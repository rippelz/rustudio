//! Placing GUIs on the screen (ScreenGui, SurfaceGui, BillboardGui), painting
//! them, and the mouse / keyboard: hover, clicks, text boxes, scrolling and
//! GuiService selection.

use egui::epaint::{TessellationOptions, Tessellator};
use egui::Shape;
use glam::Mat3;

use super::walk::{padded, Aff, Down, Walk};
use super::*;
use rustudio_core::math::Xform;
use rustudio_core::mech;
use rustudio_render::scene::{vec3_of, xform_of};

/// The GUI's debugging switches (read once).
#[derive(Clone, Copy)]
enum DebugEnv {
    /// RUSTUDIO_GUI_CACHE_WHY: say why each GUI was walked.
    Why,
    /// RUSTUDIO_GUI_CACHE_VERIFY: walk kept GUIs anyway (and once more
    /// without memos) and compare.
    Verify,
    /// RUSTUDIO_GUI_LAYOUT_VERIFY: compare the kept layout with a fresh one.
    LayoutVerify,
    /// RUSTUDIO_GUI_BLACK_BG: ScreenGuis over black (GUI-only captures).
    BlackBg,
}

fn debug_env(e: DebugEnv) -> bool {
    static ON: std::sync::OnceLock<[bool; 4]> = std::sync::OnceLock::new();
    ON.get_or_init(|| {
        ["RUSTUDIO_GUI_CACHE_WHY", "RUSTUDIO_GUI_CACHE_VERIFY", "RUSTUDIO_GUI_LAYOUT_VERIFY", "RUSTUDIO_GUI_BLACK_BG"]
            .map(|n| std::env::var_os(n).is_some())
    })[e as usize]
}

/// The LocalPlayer's PlayerGui (where play-mode ScreenGuis live).
pub(super) fn local_player_gui(d: &Document) -> Option<Ref> {
    d.find_child(d.dom.root_ref(), "Players").and_then(|p| match d.prop(p, "LocalPlayer") {
        Some(Variant::Ref(lp)) if d.exists(lp) => d.find_child(lp, "PlayerGui"),
        _ => None,
    })
}

/// A ScreenGui's output from the frame it was last walked: its triangles
/// on the screen, what can be clicked, and its Stats counts. Drawn again as
/// it is until a journal mark touches its tree or something it was drawn
/// with changes (hover, focus, selection, the font atlas, the view).
///
/// Studio does the same: each GuiBase2d's LayoutNode keeps dirty bits
/// (LayoutDirty, ViewportDirty, MarkedForViewportUpdate, ChildSort,
/// IntrinsicSize, ContentSize…; names at Studio, dumped by
/// Studio), layout runs only for dirty nodes, and the vertices sit in
/// a retained Graphics::GuiCluster per layer collector (an ISuperCluster,
/// vtable Studio) that is refilled only when marked (the frame stats
/// time UILayout, FillGuiVertices and Render2d apart, Studio).
pub(super) struct RootCache {
    key: [u32; 13],
    shapes: Vec<Shape>,
    hits: Vec<Hit>,
    /// Stats.UI2D: draw calls, triangles.
    ui2d: (u64, u64),
    vfs: Vec<Ref>,
    /// SurfaceGuis / BillboardGuis: the canvas triangles (with their
    /// bounds for the view test) and hit boxes, mapped again each frame.
    citems: Vec<(Rect, Arc<Mesh>)>,
    lhits: Vec<walk::LocalHit>,
}

/// A previously projected edit billboard. If the camera returns to this
/// exact canvas size, update its projection without revisiting the GUI tree.
struct BillboardCanvas {
    output: RootCache,
    boxes: Vec<(Ref, GuiBox)>,
    reads: Vec<(Ref, Ref)>,
    children: Vec<Ref>,
}

#[derive(Default)]
pub(super) struct BillboardArchive {
    canvases: Vec<BillboardCanvas>,
    reads: rustc_hash::FxHashSet<Ref>,
}

/// The state outside the document that cached GUIs were drawn with.
#[derive(Default)]
pub(super) struct CacheWith {
    hovered: HashSet<Ref>,
    pressed: Option<Ref>,
    focus: Option<Ref>,
    selected: Option<Ref>,
    selected_core: Option<Ref>,
    selection_visible: bool,
    atlas: [usize; 2],
    fill: f32,
    /// Font files and families egui knows: a font added rebuilds egui's
    /// atlas from scratch (glyphs move) at the next frame.
    fonts: (usize, usize),
    ppp: f32,
}

/// What a GUI root is drawn onto.
#[derive(Clone, Copy)]
enum Target {
    /// Screen: canvas (0,0) at this point.
    Screen(Pos2),
    /// A part's face: canvas corners top-left, top-right, bottom-left (world).
    Surface { tl: Vec3, tr: Vec3, bl: Vec3 },
    /// Billboard: canvas centred on this screen point (its world anchor).
    Billboard(Pos2, Vec3),
}

struct Root {
    /// Drawn over the world (AlwaysOnTop, ScreenGuis), not depth-tested.
    on_top: bool,
    /// LightInfluence / Brightness: the colour factor (gamma space).
    light: f32,
    order: i64,
    dist: f32,
    gui: Ref,
    target: Target,
    canvas: [f32; 2],
    abs_offset: [f32; 2],
}

impl Gui {
    /// Drops TextBox keyboard focus without firing FocusLost (the session
    /// it belonged to has ended: Play stopping with the chat bar focused
    /// would otherwise leave the editor's keys going to a box that's gone).
    pub fn drop_focus(&mut self) {
        self.focus = None;
        self.tb_cursor = -1;
        self.tb_sel = -1;
        self.tb_selecting = None;
        self.tb_held_key = None;
        let mut sh = super::share();
        sh.focus = None;
        sh.focus_req.clear();
        sh.focus_events.clear();
    }

    /// Keeps the list of SurfaceGuis / BillboardGuis under Workspace from the
    /// document's journal: scanning Place B's 116k instances for them was
    /// 8 ms a frame. Call before `draw`.
    pub fn sync_roots(&mut self, d: &mut Document) {
        let _p = rustudio_core::bench::scope("gui_roots");
        let marks = d.marks_for(doc::Reader::Gui);
        self.cache_marks(d, &marks);
        let ws = d.workspace();
        let layer = |d: &Document, r: Ref| {
            d.dom
                .get_by_ref(r)
                .is_some_and(|i| i.class == "SurfaceGui" || i.class == "BillboardGui")
                && ws.is_some_and(|w| d.is_ancestor(w, r))
        };
        // Adornments under the LocalPlayer's PlayerGui come from the same
        // journal (scanning that subtree every frame was 4 ms in Place E); a new
        // PlayerGui (LocalPlayer set, respawn) is scanned once.
        let pg = local_player_gui(d);
        self.messages.sync(d, &marks, pg);
        let pg_new = pg != self.adorn_pg_of || d.id != self.roots_doc;
        if pg_new || !matches!(marks, doc::Marks::Some(_)) {
            self.adorn_pg_of = pg;
            self.adorn_pg = match pg {
                Some(pg) => d
                    .subtree(pg)
                    .into_iter()
                    .filter(|r| d.class_of(*r).is_some_and(|c| super::adorn::is_adornment(&c)))
                    .collect(),
                None => HashSet::new(),
            };
            self.adorn_pg_order = None;
        }
        match marks {
            doc::Marks::Some(marks) if d.id == self.roots_doc => {
                for m in marks {
                    let doc::Mark::Tree(t) = m else { continue };
                    for r in d.subtree(t) {
                        let adorn = d.dom.get_by_ref(r).is_some_and(|i| {
                            doc::is_a(&i.class, "HandleAdornment") || doc::is_a(&i.class, "HandlesBase")
                        });
                        if adorn && ws.is_some_and(|w| d.is_ancestor(w, r)) {
                            self.adorn_roots.insert(r);
                        } else {
                            self.adorn_roots.remove(&r);
                        }
                        let in_pg = adorn && pg.is_some_and(|p| d.is_ancestor(p, r));
                        let changed = if in_pg { self.adorn_pg.insert(r) } else { self.adorn_pg.remove(&r) };
                        if changed {
                            self.adorn_pg_order = None;
                        }
                        self.surface_specs.remove(&r);
                        self.root_props.remove(&r);
                        let keep = layer(d, r);
                        let has = self.world_roots_set.contains(&r);
                        if keep && !has {
                            self.world_roots_set.insert(r);
                            self.world_roots.push(r);
                        } else if !keep && has {
                            self.world_roots_set.remove(&r);
                            self.world_roots.retain(|x| *x != r);
                            self.root_boxes.remove(&r);
                        }
                    }
                }
            }
            _ => {
                self.roots_doc = d.id;
                self.surface_specs.clear();
                self.root_props.clear();
                self.surface_geo.clear();
                self.world_roots = match ws {
                    Some(ws) => d
                        .dom
                        .descendants_of(ws)
                        .filter(|i| i.class == "SurfaceGui" || i.class == "BillboardGui")
                        .map(|i| i.referent())
                        .collect(),
                    None => Vec::new(),
                };
                self.world_roots_set = self.world_roots.iter().copied().collect();
                self.adorn_roots = match ws {
                    Some(ws) => d
                        .dom
                        .descendants_of(ws)
                        .filter(|i| doc::is_a(&i.class, "HandleAdornment") || doc::is_a(&i.class, "HandlesBase"))
                        .map(|i| i.referent())
                        .collect(),
                    None => HashSet::new(),
                };
                self.root_boxes.clear();
                self.layout.clear();
                self.layout_from.clear();
                self.layout_changed = true;
            }
        }
    }

    /// Lightweight sync for a plugin DockWidget / QWidget `Gui`
    /// (`Roots::Widget`): only invalidates its own 2D caches from journal
    /// marks without scanning CoreGui or Workspace for 3D world layers /
    /// handle adornments (which only the CoreGui overlay draws).
    pub fn sync_widget(&mut self, d: &Document, marks: &doc::Marks) {
        let _p = rustudio_core::bench::scope("gui_roots_widget");
        self.cache_marks(d, marks);
        if !matches!(marks, doc::Marks::Some(_)) || d.id != self.roots_doc {
            self.roots_doc = d.id;
            self.surface_specs.clear();
            self.root_props.clear();
            self.surface_geo.clear();
            self.root_boxes.clear();
            self.layout.clear();
            self.layout_from.clear();
            self.layout_changed = true;
        }
    }

    /// `sync_roots` for a plugin `Gui` (Roots::CoreGui / Widget): the
    /// journal's marks come from the caller (one reader for all plugin
    /// GUIs: src/plugin_host/widgets_ui.rs). Keeps the BillboardGuis /
    /// SurfaceGuis under CoreGui and the handle adornments under CoreGui or
    /// Workspace (Studio draws a plugin's handles in the edit view).
    pub fn sync_plugin(&mut self, d: &Document, marks: &doc::Marks) {
        let _p = rustudio_core::bench::scope("gui_roots_plugin");
        self.cache_marks(d, marks);
        let root = d.dom.root_ref();
        let core = d.find_child(root, "CoreGui");
        let ws = d.workspace();
        let under = |r: Ref| core.is_some_and(|c| d.is_ancestor(c, r)) || ws.is_some_and(|w| d.is_ancestor(w, r));
        let layer = |r: Ref| {
            d.dom.get_by_ref(r).is_some_and(|i| i.class == "SurfaceGui" || i.class == "BillboardGui")
                && core.is_some_and(|c| d.is_ancestor(c, r))
        };
        match marks {
            doc::Marks::Some(marks) if d.id == self.roots_doc => {
                for m in marks {
                    let doc::Mark::Tree(t) = m else { continue };
                    for r in d.subtree(*t) {
                        let adorn = d.class_of(r).is_some_and(|c| super::adorn::is_adornment(&c));
                        if adorn && under(r) {
                            self.adorn_roots.insert(r);
                        } else {
                            self.adorn_roots.remove(&r);
                        }
                        self.surface_specs.remove(&r);
                        self.root_props.remove(&r);
                        let keep = layer(r);
                        let has = self.world_roots_set.contains(&r);
                        if keep && !has {
                            self.world_roots_set.insert(r);
                            self.world_roots.push(r);
                        } else if !keep && has {
                            self.world_roots_set.remove(&r);
                            self.world_roots.retain(|x| *x != r);
                            self.root_boxes.remove(&r);
                        }
                    }
                    // (a removed subtree: its entries go)
                    if !d.exists(*t) {
                        self.adorn_roots.remove(t);
                    }
                }
                self.adorn_roots.retain(|r| d.exists(*r));
            }
            _ => {
                self.roots_doc = d.id;
                self.surface_specs.clear();
                self.root_props.clear();
                self.surface_geo.clear();
                let scan = |top: Option<Ref>| -> Vec<Ref> {
                    top.map(|t| d.dom.descendants_of(t).map(|i| i.referent()).collect()).unwrap_or_default()
                };
                let core_list = scan(core);
                self.world_roots = core_list.iter().copied().filter(|r| layer(*r)).collect();
                self.world_roots_set = self.world_roots.iter().copied().collect();
                self.adorn_roots = core_list
                    .into_iter()
                    .chain(scan(ws))
                    .filter(|r| d.class_of(*r).is_some_and(|c| super::adorn::is_adornment(&c)))
                    .collect();
                self.root_boxes.clear();
                self.layout.clear();
                self.layout_from.clear();
                self.layout_changed = true;
            }
        }
    }

    /// Drops the kept output of every ScreenGui a mark touches: a property of
    /// something its walk read, or an instance added, moved or removed where
    /// that walk listed the children (or that it read itself).
    fn cache_marks(&mut self, d: &Document, marks: &doc::Marks) {
        // A SurfaceGui's own placement properties (`SurfaceSpec`): any of
        // its properties written drops its entry; reparenting drops it in
        // `sync_roots` / `sync_plugin` (Tree marks).
        // (and a root's own RootProps the same way)
        if !self.surface_specs.is_empty() || !self.root_props.is_empty() {
            match marks {
                doc::Marks::Some(marks) if d.id == self.roots_doc => {
                    for mark in marks {
                        if let doc::Mark::Prop(r, _) = mark {
                            self.surface_specs.remove(r);
                            self.root_props.remove(r);
                        }
                    }
                }
                _ => {
                    self.surface_specs.clear();
                    self.root_props.clear();
                    self.surface_geo.clear();
                }
            }
        }
        if !self.billboard_model_centers.is_empty() {
            match marks {
                doc::Marks::Some(marks) if d.id == self.roots_doc => {
                    for mark in marks {
                        match *mark {
                            doc::Mark::Tree(_) => {
                                self.billboard_model_centers.clear();
                                break;
                            }
                            doc::Mark::Prop(r, name) if matches!(name.as_str(), "CFrame" | "Position" | "Orientation" | "Rotation") => {
                                let mut part = r;
                                while let Some(inst) = d.dom.get_by_ref(part) {
                                    self.billboard_model_centers.remove(&part);
                                    part = inst.parent();
                                }
                            }
                            _ => {}
                        }
                    }
                }
                _ => self.billboard_model_centers.clear(),
            }
        }
        let marks = match marks {
            doc::Marks::Some(m) if d.id == self.roots_doc => m,
            _ => {
                self.clear_billboard_canvases();
                self.cache.clear();
                self.memo.clear();
                return;
            }
        };
        if self.cache_owner.is_empty() && self.billboard_archive_owner.is_empty() {
            return;
        }
        let why_on = debug_env(DebugEnv::Why);
        for m in marks {
            let r = match *m {
                doc::Mark::Prop(r, _) | doc::Mark::Tree(r) => r,
            };
            self.invalidate_billboard_archive(r);
            let (read, listed) = match *m {
                doc::Mark::Prop(r, _) => (self.cache_owner.get(&r).map(|o| (o.0, o.1)), None),
                doc::Mark::Tree(r) => {
                    let parent = d.dom.get_by_ref(r).map(|i| i.parent());
                    if let Some(parent) = parent {
                        self.invalidate_billboard_archive(parent);
                    }
                    (self.cache_owner.get(&r).map(|o| (o.0, o.1)), parent.and_then(|p| self.cache_kids.get(&p).map(|g| (*g, p))))
                }
            };
            // The objects whose memos it touches: itself and up, the one that
            // read it and up (it may be gone, or read from elsewhere: a
            // ViewportFrame's camera), the parent that listed it and up.
            if let Some((g, by)) = read {
                self.memo_up(d, r, g);
                self.memo_up(d, by, g);
            }
            if let Some((g, p)) = listed {
                self.memo_up(d, p, g);
            }
            let hit = read.map(|x| x.0).or(listed.map(|x| x.0));
            if let Some(g) = hit {
                if self.cache.remove(&g).is_some() && why_on {
                    let what = match *m {
                        doc::Mark::Prop(_, n) => n.to_string(),
                        doc::Mark::Tree(_) => "<tree>".to_string(),
                    };
                    let class = d.class_of(r).map(|c| c.to_string()).unwrap_or_default();
                    self.cache_why.insert(g, format!("mark {class} {} {what}", name_of(d, r)));
                }
            }
        }
    }

    /// RUSTUDIO_GUI_CACHE_VERIFY: GUI `g` walked again (`shapes` / `hits` on
    /// the screen, `citems` / `lhits` in its canvas) against what was kept.
    #[allow(clippy::too_many_arguments)]
    fn cache_verify(
        &self,
        d: &Document,
        g: Ref,
        k: &RootCache,
        screen: bool,
        shapes: &[Shape],
        hits: &[Hit],
        citems: &[(Rect, Arc<Mesh>)],
        lhits: &[walk::LocalHit],
    ) {
        let same_mesh = |a: &Mesh, b: &Mesh| {
            a.texture_id == b.texture_id
                && a.indices == b.indices
                && a.vertices.len() == b.vertices.len()
                && a.vertices.iter().zip(&b.vertices).all(|(x, y)| x.pos == y.pos && x.uv == y.uv && x.color == y.color)
        };
        let same_shape = |a: &Shape, b: &Shape| match (a, b) {
            (Shape::Mesh(a), Shape::Mesh(b)) => same_mesh(a, b),
            _ => false,
        };
        let same_hit = |a: &Hit, b: &Hit| a.r == b.r && a.quad == b.quad && a.clip == b.clip && a.sink == b.sink;
        let same_lhit = |a: &walk::LocalHit, b: &walk::LocalHit| a.r == b.r && a.rect == b.rect && a.clip == b.clip && a.sink == b.sink;
        let (n_shape, n_hit) = if screen {
            (
                (k.shapes.len() != shapes.len()).then_some(usize::MAX).or_else(|| k.shapes.iter().zip(shapes).position(|(a, b)| !same_shape(a, b))),
                (k.hits.len() != hits.len()).then_some(usize::MAX).or_else(|| k.hits.iter().zip(hits).position(|(a, b)| !same_hit(a, b))),
            )
        } else {
            (
                (k.citems.len() != citems.len()).then_some(usize::MAX).or_else(|| k.citems.iter().zip(citems).position(|(a, b)| a.0 != b.0 || !same_mesh(&a.1, &b.1))),
                (k.lhits.len() != lhits.len()).then_some(usize::MAX).or_else(|| k.lhits.iter().zip(lhits).position(|(a, b)| !same_lhit(a, b))),
            )
        };
        let same = n_shape.is_none() && n_hit.is_none();
        rustudio_core::bench::add(if same { "gui_verify_same" } else { "gui_verify_differ" }, 0.0, 1);
        if !same {
            eprintln!(
                "[guiverify] {} differs from its kept output: first differing triangles {:?}, hit {:?} (usize::MAX: counts differ; {} / {} screen shapes, {} / {} canvas items)",
                name_of(d, g),
                n_shape,
                n_hit,
                k.shapes.len(),
                shapes.len(),
                k.citems.len(),
                citems.len(),
            );
        }
    }

    /// Whether `layout` changed since this was last asked (the viewport
    /// hands it to the game only then).
    pub fn take_layout_change(&mut self) -> bool {
        std::mem::take(&mut self.layout_changed)
    }

    /// Keeps what ScreenGui `g`'s walk read (`walk_reads`, `walk_kids`) for
    /// `cache_marks`, in place of what its last walk read.
    fn cache_own(&mut self, g: Ref) {
        self.own_gen = self.own_gen.wrapping_add(1);
        let gen = self.own_gen;
        let mut reads = std::mem::take(&mut self.walk_reads);
        let kids = std::mem::take(&mut self.walk_kids);
        reads.push((g, g));
        for (x, by) in &reads {
            self.cache_owner.insert(*x, (g, *by, gen));
        }
        let old = self.cache_owned.remove(&g);
        if let Some((old_reads, old_kids)) = &old {
            // What this walk no longer read (under something hidden now, or
            // gone): its entry goes, and so do the memos of it and of what
            // read it, which nothing would keep up to date while unread.
            for (x, by) in old_reads {
                if self.cache_owner.get(x).is_some_and(|o| o.0 == g && o.2 != gen) {
                    self.cache_owner.remove(x);
                    self.memo.remove(x);
                    self.memo.remove(by);
                }
            }
            for x in old_kids {
                if self.cache_kids.get(x) == Some(&g) {
                    self.cache_kids.remove(x);
                }
            }
        }
        for x in &kids {
            self.cache_kids.insert(*x, g);
        }
        self.cache_owned.insert(g, (reads, kids));
    }

    /// Scales the V of every kept font-atlas triangle by `s` (a power of
    /// two: exact). Memos whose triangles are shared elsewhere are dropped.
    fn rescale_font_uvs(&mut self, s: f32) {
        fn mesh(m: &mut Arc<Mesh>, s: f32) {
            if m.texture_id == egui::TextureId::default() {
                for v in &mut Arc::make_mut(m).vertices {
                    v.uv.y *= s;
                }
            }
        }
        fn root(c: &mut RootCache, s: f32) {
            for shape in &mut c.shapes {
                if let Shape::Mesh(m) = shape {
                    mesh(m, s);
                }
            }
            for (_, m) in &mut c.citems {
                mesh(m, s);
            }
        }
        for c in self.cache.values_mut() {
            root(c, s);
        }
        for archive in self.billboard_canvases.values_mut() {
            for canvas in &mut archive.canvases {
                root(&mut canvas.output, s);
            }
        }
        self.memo.retain(|_, memo| {
            memo.segs.iter_mut().all(|seg| {
                let walk::Seg::Own { items, .. } = seg else { return true };
                items.iter_mut().all(|it| match Arc::get_mut(&mut it.tess) {
                    Some(lock) => {
                        if let Some(Some((_, m))) = lock.get_mut() {
                            mesh(m, s);
                        }
                        true
                    }
                    None => false,
                })
            })
        });
    }

    fn clear_billboard_canvases(&mut self) {
        self.billboard_canvases.clear();
        self.billboard_archive_owner.clear();
    }

    fn invalidate_billboard_archive(&mut self, touched: Ref) {
        let Some(gui) = self.billboard_archive_owner.get(&touched).copied() else { return };
        self.drop_billboard_archive(gui);
    }

    fn drop_billboard_archive(&mut self, gui: Ref) {
        if let Some(archive) = self.billboard_canvases.remove(&gui) {
            for read in archive.reads {
                if self.billboard_archive_owner.get(&read) == Some(&gui) {
                    self.billboard_archive_owner.remove(&read);
                }
            }
        }
    }

    fn reuse_billboard_canvas(&mut self, g: Ref, key: [u32; 13]) {
        const BILLBOARD_ROOTS: usize = 128;
        const CANVASES_PER_ROOT: usize = 8;
        const VERTICES_PER_CANVAS: usize = 4096;
        if self.cache.get(&g).is_some_and(|c| c.key == key) {
            return;
        }
        let previous = self.billboard_canvases.get_mut(&g).and_then(|archive| {
            let index = archive.canvases.iter().position(|canvas| canvas.output.key == key)?;
            Some(archive.canvases.remove(index))
        });
        if self.billboard_canvases.len() < BILLBOARD_ROOTS || self.billboard_canvases.contains_key(&g) {
            let archive = self.cache.get(&g).is_some_and(|c| {
                c.vfs.is_empty() && c.citems.iter().map(|(_, mesh)| mesh.vertices.len()).sum::<usize>() <= VERTICES_PER_CANVAS
            });
            if archive {
                if let (Some(output), Some(boxes), Some((reads, children))) = (
                    self.cache.remove(&g), self.root_boxes.get(&g), self.cache_owned.get(&g),
                ) {
                    let archive = self.billboard_canvases.entry(g).or_default();
                    for r in reads.iter().map(|(r, _)| *r).chain(children.iter().copied()) {
                        if archive.reads.insert(r) {
                            self.billboard_archive_owner.insert(r, g);
                        }
                    }
                    if archive.canvases.len() == CANVASES_PER_ROOT {
                        archive.canvases.remove(0);
                    }
                    archive.canvases.push(BillboardCanvas {
                        output, boxes: boxes.clone(), reads: reads.clone(), children: children.clone(),
                    });
                }
            }
        }
        if let Some(previous) = previous {
            if self.billboard_canvases.get(&g).is_some_and(|archive| archive.canvases.is_empty()) {
                self.drop_billboard_archive(g);
            }
            for r in self.layout_from.remove(&g).unwrap_or_default() {
                if r != g { self.layout.remove(&r); }
            }
            self.layout.extend(previous.boxes.iter().copied());
            self.layout_from.insert(g, previous.boxes.iter().map(|(r, _)| *r).collect());
            self.root_boxes.insert(g, previous.boxes);
            self.layout_changed = true;
            self.walk_reads = previous.reads;
            self.walk_kids = previous.children;
            self.cache_own(g);
            self.cache.insert(g, previous.output);
            rustudio_core::bench::add("gui_billboard_canvas_hit", 0.0, 1);
        }
    }

    /// Drops the memos of `from` and every object above it, up to GUI `g`.
    fn memo_up(&mut self, d: &Document, from: Ref, g: Ref) {
        let mut c = from;
        while c.is_some() && c != g {
            self.memo.remove(&c);
            match d.dom.get_by_ref(c) {
                Some(i) => c = i.parent(),
                None => break,
            }
        }
    }

    /// The outside state cached GUIs were drawn with changed: drop those it
    /// touches (hover / press / focus / selection), or all (font atlas).
    fn cache_check(&mut self, ctx: &egui::Context, d: &Document, render_state: Option<&egui_wgpu::RenderState>) {
        let ppp = ctx.pixels_per_point();
        let (atlas, fill, fonts) = ctx.fonts(|f| {
            let defs = f.definitions();
            (f.font_image_size(), f.font_atlas_fill_ratio(), (defs.font_data.len(), defs.families.len()))
        });
        let gs = d.find_child(d.dom.root_ref(), "GuiService");
        let selected = gs.and_then(|gs| match d.prop(gs, "SelectedObject") {
            Some(Variant::Ref(r)) if d.exists(r) => Some(r),
            _ => None,
        });
        let selected_core = gs.and_then(|gs| match d.prop(gs, "SelectedCoreObject") {
            Some(Variant::Ref(r)) if d.exists(r) => Some(r),
            _ => None,
        });
        let selection_visible = super::share().selection_visible;
        let now = CacheWith {
            hovered: self.hovered.clone(),
            pressed: self.pressed.map(|p| p.0),
            focus: self.focus,
            selected,
            selected_core,
            selection_visible,
            atlas,
            fill,
            fonts,
            ppp,
        };
        let was = std::mem::replace(&mut self.cache_with, now);
        if let Some(rs) = render_state {
            // Large first-use clears can hold wgpu's submission lock while
            // the driver allocates backing pages, even on the prewarm worker.
            // Prepare the starting atlas and its first doubling. Larger
            // allocations are made by the existing upload path if needed.
            const MAX_PREWARM_TEXELS: usize = 16 * 1024 * 1024;
            let [width, height] = atlas;
            let texels = width * height;
            if texels > 0 && texels <= MAX_PREWARM_TEXELS {
                let key = rustudio_render::world_gui::mip_copy_key(width as u32, height as u32);
                egui_wgpu::prewarm::prewarm_once(&rs.device, &rs.queue, key);
            }
            if height > 0 && height < width && texels <= MAX_PREWARM_TEXELS / 2 {
                let (w, h) = (width as u32, height as u32 * 2);
                let keys = [rustudio_render::world_gui::font_atlas_key(w, h), rustudio_render::world_gui::mip_copy_key(w, h)];
                for key in keys {
                    egui_wgpu::prewarm::prewarm(&rs.device, &rs.queue, key);
                }
            }
        }
        // Memos of objects that are gone (now and then).
        self.memo_frames += 1;
        if self.memo_frames % 600 == 0 {
            self.memo.retain(|r, _| d.exists(*r));
        }
        // egui's atlas only grew taller (it doubles): the kept triangles'
        // font UVs are texel × (1 / height), so scaling their V by the
        // power-of-two ratio gives, bit for bit, what tessellating again
        // would. Everything else stays kept.
        let grew = was.atlas[0] != 0
            && atlas[0] == was.atlas[0]
            && atlas[1] > was.atlas[1]
            && (atlas[1] / was.atlas[1].max(1)).is_power_of_two()
            && atlas[1] % was.atlas[1].max(1) == 0
            && fill >= was.fill
            && fonts == was.fonts
            && ppp == was.ppp;
        if grew {
            rustudio_core::bench::add("gui_atlas_grow", 0.0, 1);
            if std::env::var_os("RUSTUDIO_ATLAS_TRACE").is_some() {
                eprintln!("[atlas] grew {:?} -> {:?}: kept GUIs rescaled", was.atlas, atlas);
            }
            self.rescale_font_uvs(was.atlas[1] as f32 / atlas[1] as f32);
        }
        if atlas != was.atlas {
            // (textures made ahead for sizes the atlas has passed go; the
            // current size's may still be waiting to be taken this frame)
            let (w, h) = (atlas[0] as u32, atlas[1] as u32);
            use rustudio_render::world_gui::{font_atlas_key, mip_copy_key};
            egui_wgpu::prewarm::drop_except(&[font_atlas_key(w, h), mip_copy_key(w, h), font_atlas_key(w, h * 2), mip_copy_key(w, h * 2)]);
        }
        // (egui's atlas grew wider, or started over: glyph UVs moved)
        if !grew && (atlas != was.atlas || fill < was.fill || fonts != was.fonts || ppp != was.ppp) {
            rustudio_core::bench::add("gui_atlas_reset", 0.0, 1);
            if std::env::var_os("RUSTUDIO_ATLAS_TRACE").is_some() {
                eprintln!("[atlas] {:?} -> {:?} fill {} -> {} fonts {:?} -> {:?}", was.atlas, atlas, was.fill, fill, was.fonts, fonts);
            }
            if fonts != was.fonts || ppp != was.ppp {
                self.text_fit = Default::default();
            } else if atlas[0] != was.atlas[0] || fill < was.fill {
                // Height growth changes normalized mesh UVs below, but not
                // the pixel coordinates stored by the galleys.
                self.text_fit.clear_galleys();
            }
            self.memo.clear();
            self.cache.clear();
            self.clear_billboard_canvases();
            return;
        }
        let now = &self.cache_with;
        let vis_changed = now.selection_visible != was.selection_visible;
        let touched: Vec<Ref> = now
            .hovered
            .symmetric_difference(&was.hovered)
            .copied()
            .chain([now.pressed, was.pressed].into_iter().flatten().filter(|_| now.pressed != was.pressed))
            .chain([now.focus, was.focus].into_iter().flatten().filter(|_| now.focus != was.focus))
            .chain([now.selected, was.selected].into_iter().flatten().filter(|_| vis_changed || now.selected != was.selected))
            .chain([now.selected_core, was.selected_core].into_iter().flatten().filter(|_| vis_changed || now.selected_core != was.selected_core))
            .collect();
        for r in touched {
            self.invalidate_billboard_archive(r);
            if let Some((g, _, _)) = self.cache_owner.get(&r).copied() {
                self.cache.remove(&g);
                self.memo_up(d, r, g);
            }
        }
        // UIPageLayout jumps / tweens mutate GuiShare::pages outside the DOM.
        let (pages_rev, page_lays) = {
            let sh = super::share();
            if sh.pages_rev != self.pages_rev {
                (sh.pages_rev, sh.pages.keys().copied().collect::<Vec<_>>())
            } else {
                (self.pages_rev, Vec::new())
            }
        };
        if pages_rev != self.pages_rev {
            self.pages_rev = pages_rev;
            let prev = std::mem::replace(&mut self.last_pages, page_lays.clone());
            for lay in page_lays.into_iter().chain(prev) {
                let target = if self.cache_owner.contains_key(&lay) {
                    Some(lay)
                } else {
                    d.dom.get_by_ref(lay).map(|i| i.parent()).filter(|p| p.is_some())
                };
                if let Some(t) = target {
                    if let Some((g, _, _)) = self.cache_owner.get(&t).copied() {
                        self.cache.remove(&g);
                        self.memo_up(d, t, g);
                    }
                }
            }
        }
    }

    fn screen_root(&self, rect: Rect, insets: Option<u32>, inset: f32) -> (Target, [f32; 2], [f32; 2]) {
        if insets == Some(3) {
            let bar = self.topbar_inset.unwrap_or_else(|| {
                crate::gui::fallback_topbar_inset(rect.width(), self.topbar_chat.unwrap_or(true))
            });
            return (
                Target::Screen(rect.min + egui::vec2(bar.min.x, bar.min.y)),
                (bar.max - bar.min).to_array(),
                [bar.min.x, bar.min.y - mech::GUI_TOP_INSET],
            );
        }
        (
            Target::Screen(Pos2::new(rect.left(), rect.top() + inset)),
            [rect.width(), rect.height() - inset],
            [0.0, inset - mech::GUI_TOP_INSET],
        )
    }

    /// Draws every GUI in view and remembers what can be clicked.
    pub fn draw(&mut self, ctx: &egui::Context, painter: &egui::Painter, mut inp: DrawIn<'_>) {
        self.vf_live.clear();
        self.hits.clear();
        self.world_batches.clear();
        self.poll_images(ctx);
        self.cache_check(ctx, inp.doc, inp.render_state);
        let verify = debug_env(DebugEnv::Verify);
        self.text_fit.begin_frame(ctx, !verify && !inp.fixed_step);
        if self.retry_missing_faces(inp.store) {
            self.memo.clear();
            self.cache.clear();
            self.clear_billboard_canvases();
        }
        if inp.roots == Roots::View {
            self.preload_faces(ctx, inp.store, inp.doc);
        }
        self.fx_frame(inp.doc);
        if (inp.playing && inp.roots == Roots::View) || inp.roots == Roots::CoreGui {
            let _a = rustudio_core::bench::scope("gui_adorn");
            self.draw_adornments(inp.doc, inp.cam, inp.rect, painter);
        }
        if inp.roots == Roots::View && inp.playing {
            self.draw_messages(ctx, painter, inp.store, inp.doc, inp.rect);
        }
        let pre = rustudio_core::bench::scope("gui_pre");
        let d = inp.doc;
        let root = d.dom.root_ref();
        let rect = inp.rect;
        let mut roots: Vec<Root> = Vec::new();

        // ScreenGuis: the LocalPlayer's PlayerGui while playing; StarterGui
        // (ShowDevelopmentGui) while editing, as Studio shows them.
        let screen_parent = if inp.roots != Roots::View {
            None
        } else if inp.playing {
            d.find_child(root, "Players")
                .and_then(|p| match d.prop(p, "LocalPlayer") {
                    Some(Variant::Ref(lp)) if d.exists(lp) => d.find_child(lp, "PlayerGui"),
                    _ => None,
                })
        } else {
            d.find_child(root, "StarterGui")
                .filter(|sg| boolp(d, *sg, "ShowDevelopmentGui", true))
        };
        let mut layered: Vec<Ref> = Vec::new();
        if let Some(pg) = screen_parent {
            for g in children(d, pg) {
                let Some(class) = d.class_of(g) else { continue };
                if !boolp(d, g, "Enabled", true) {
                    continue;
                }
                match class.as_str() {
                    "ScreenGui" => {
                        // ScreenInsets None / DeviceSafeInsets (IgnoreGuiInset, which
                        // doc.rs derives from it both ways): the whole view.
                        // TopbarSafeInsets: the top bar's free area, GuiService.TopbarInset
                        // (right of Roblox's buttons, the bar's height) — TopbarPlus's
                        // ScreenGui, whose icons sit at Y scale 0.5 of it, next to the
                        // chat button.
                        let si = enum_u32(d.prop(g, "ScreenInsets"));
                        let ignore = matches!(si, Some(0) | Some(1));
                        let inset = if ignore { 0.0 } else { mech::GUI_TOP_INSET };
                        let (target, canvas, abs_offset) = self.screen_root(rect, si, inset);
                        roots.push(Root {
                            on_top: true,
                            light: 1.0,
                            order: 1_000_000 + num(d, g, "DisplayOrder", 0.0) as i64,
                            dist: 0.0,
                            gui: g,
                            target,
                            canvas,
                            abs_offset,
                        });
                        // BillboardGuis / SurfaceGuis kept in a ScreenGui (the
                        // proximity prompts' ProximityPrompts gui) draw at
                        // their Adornee.
                        for c in children(d, g) {
                            if matches!(d.class_of(c).as_deref(), Some("BillboardGui" | "SurfaceGui"))
                                && boolp(d, c, "Enabled", true)
                                && matches!(d.prop(c, "Adornee"), Some(Variant::Ref(a)) if d.exists(a))
                            {
                                layered.push(c);
                            }
                        }
                    }
                    "SurfaceGui" | "BillboardGui" => layered.push(g),
                    _ => {}
                }
            }
        }
        // CoreGui during Play: Roblox's built-in in-experience UIs (ExperienceChat,
        // bubbleChat BillboardGuis, top-bar chat toggle) live in CoreGui, respecting
        // ScreenInsets / DisplayOrder just like PlayerGui.
        if inp.playing && inp.roots == Roots::View {
            if let Some(core) = d.find_child(root, "CoreGui") {
                let mut stack: Vec<Ref> = children(d, core);
                stack.reverse();
                while let Some(g) = stack.pop() {
                    let Some(class) = d.class_of(g) else { continue };
                    match class.as_str() {
                        "ScreenGui" => {
                            if self.mobile_play_controls
                                && d.dom.get_by_ref(g).is_some_and(|gui| gui.name == "TopBarApp")
                            {
                                continue;
                            }
                            if !boolp(d, g, "Enabled", true) {
                                continue;
                            }
                            let si = enum_u32(d.prop(g, "ScreenInsets"));
                            let ignore = matches!(si, Some(0) | Some(1));
                            let inset = if ignore { 0.0 } else { mech::GUI_TOP_INSET };
                            let (target, canvas, abs_offset) = self.screen_root(rect, si, inset);
                            roots.push(Root {
                                on_top: true,
                                light: 1.0,
                                order: 1_000_000 + num(d, g, "DisplayOrder", 0.0) as i64,
                                dist: 0.0,
                                gui: g,
                                target,
                                canvas,
                                abs_offset,
                            });
                            // Nested ScreenGuis (e.g. CoreGui.ExperienceChat.bubbleChat)
                            // and BillboardGuis / SurfaceGuis inside CoreGui ScreenGuis.
                            let mut sub: Vec<Ref> = children(d, g);
                            sub.reverse();
                            for c in sub {
                                match d.class_of(c).as_deref() {
                                    Some("BillboardGui" | "SurfaceGui") => {
                                        if boolp(d, c, "Enabled", true)
                                            && matches!(d.prop(c, "Adornee"), Some(Variant::Ref(a)) if d.exists(a))
                                        {
                                            layered.push(c);
                                        }
                                    }
                                    Some("ScreenGui" | "Folder") => stack.push(c),
                                    _ => {}
                                }
                            }
                        }
                        "BillboardGui" | "SurfaceGui" => {
                            if boolp(d, g, "Enabled", true)
                                && matches!(d.prop(g, "Adornee"), Some(Variant::Ref(a)) if d.exists(a))
                            {
                                layered.push(g);
                            }
                        }
                        "Folder" => {
                            let mut k = children(d, g);
                            k.reverse();
                            stack.extend(k);
                        }
                        _ => {}
                    }
                }
            }
        }
        match inp.roots {
            Roots::View => {}
            // A plugin's dock widget: its panel's content is the canvas, in
            // widget pixels from the panel's top-left.
            Roots::Widget(w) => {
                if d.exists(w) {
                    roots.push(Root {
                        on_top: true,
                        light: 1.0,
                        order: 0,
                        dist: 0.0,
                        gui: w,
                        target: Target::Screen(rect.min),
                        canvas: [rect.width(), rect.height()],
                        abs_offset: [0.0, 0.0],
                    });
                }
            }
            // CoreGui in the edit viewport: its ScreenGuis (directly in it or
            // in Folders) cover the whole view with no inset (F46: the
            // viewport's size, IgnoreGuiInset false).
            Roots::CoreGui => {
                if let Some(core) = d.find_child(root, "CoreGui") {
                    let mut stack: Vec<Ref> = children(d, core);
                    stack.reverse();
                    while let Some(g) = stack.pop() {
                        let Some(class) = d.class_of(g) else { continue };
                        if class == "ScreenGui" {
                            if boolp(d, g, "Enabled", true) {
                                roots.push(Root {
                                    on_top: true,
                                    light: 1.0,
                                    order: 1_000_000 + num(d, g, "DisplayOrder", 0.0) as i64,
                                    dist: 0.0,
                                    gui: g,
                                    target: Target::Screen(rect.min),
                                    canvas: [rect.width(), rect.height()],
                                    abs_offset: [0.0, 0.0],
                                });
                            }
                        } else if !doc::is_a(&class, "LayerCollector") && !doc::is_a(&class, "GuiObject") {
                            let mut k = children(d, g);
                            k.reverse();
                            stack.extend(k);
                        }
                    }
                }
            }
        }
        // (the cached SurfaceGuis' properties need the journal `sync_roots`
        // / `sync_plugin` read for this document)
        let specs_ok = d.id == self.roots_doc;
        if !specs_ok {
            self.surface_specs.clear();
            self.root_props.clear();
            self.surface_geo.clear();
        }
        let first_world = layered.len();
        layered.extend(self.world_roots.iter().copied().filter(|g| {
            !matches!(inp.roots, Roots::Widget(_)) && d.exists(*g) && match self.surface_specs.get(g) {
                Some(spec) => spec.enabled,
                None => boolp(d, *g, "Enabled", true),
            }
        }));
        drop(pre);
        let place = rustudio_core::bench::scope("gui_place");
        let view_proj = inp.cam.view_proj(rect);
        let mut billboard_light = None;
        let mut light_terms = None;
        for (i, g) in layered.into_iter().enumerate() {
            // SurfaceGuis under Workspace: their own properties from the
            // last frame unless the journal touched them, the part's
            // placement read now.
            let spec = (specs_ok && i >= first_world)
                .then(|| {
                    let fresh = self.surface_specs.get(&g).is_some_and(|s| s.current(d));
                    if !fresh {
                        let spec = SurfaceSpec::read(d, g)?;
                        self.surface_specs.insert(g, spec);
                    }
                    self.surface_specs.get(&g).copied()
                })
                .flatten();
            let placed = match spec {
                Some(spec) => {
                    let geo = self.surface_geo.entry(g).or_default();
                    spec.place_kept(d, inp.cam, geo).map(|p| (p, spec.on_top, spec.light_props, spec.z_offset))
                }
                None => place_3d(d, inp.cam, &view_proj, rect, g, &mut self.billboard_model_centers).map(|p| {
                    let on_top = boolp(d, g, "AlwaysOnTop", false);
                    let light_props = if on_top { [0.0; 2] } else { light_props(d, g) };
                    (p, on_top, light_props, num(d, g, "ZOffset", 0.0) as i64)
                }),
            };
            if let Some(((target, canvas, dist), on_top, light_props, z_offset)) = placed {
                let light = if on_top {
                    1.0
                } else {
                    world_gui_light(light_props, || {
                        let terms = *light_terms.get_or_insert_with(|| SceneLight::of(d));
                        match target {
                            Target::Surface { tl, tr, bl } => terms.on(Some((bl - tl).cross(tr - tl).normalize_or_zero())),
                            _ => *billboard_light.get_or_insert_with(|| terms.on(Some(-inp.cam.forward()))),
                        }
                    })
                };
                roots.push(Root {
                    on_top,
                    light,
                    order: if on_top { 500_000 } else { 0 } + z_offset,
                    dist,
                    gui: g,
                    target,
                    canvas,
                    abs_offset: [0.0, 0.0],
                });
            }
        }
        // Back to front: 3D GUIs (far first, AlwaysOnTop after), then ScreenGuis by DisplayOrder.
        {
            let _sort = rustudio_core::bench::scope("gui_z_sort");
            // (stably by order, then far to near: the same order as a
            // stable sort of the roots, sorting small keys instead)
            let mut keys: Vec<(i64, f32, u32)> =
                roots.iter().enumerate().map(|(i, r)| (r.order, r.dist, i as u32)).collect();
            keys.sort_unstable_by(|a, b| a.0.cmp(&b.0).then(b.1.total_cmp(&a.1)).then(a.2.cmp(&b.2)));
            let mut slots: Vec<Option<Root>> = roots.drain(..).map(Some).collect();
            roots.extend(keys.iter().map(|k| slots[k.2 as usize].take().expect("each root once")));
        }
        drop(place);

        // (No prepared discs: they only speed up small circles.)
        let atlas = ctx.fonts(|f| f.font_image_size());
        // The renderer's reverse-Z projection: the depth the world GUI pass tests.
        let view_proj_rz = inp.cam.view_proj_reverse_z(rect);
        let mut tess = Tessellator::new(
            ctx.pixels_per_point(),
            TessellationOptions::default(),
            atlas,
            Vec::new(),
        );
        let mut out: Vec<Shape> = Vec::new();
        // World GUI triangles in root order: each world root's own batches,
        // or a SurfaceGui reprojection job run after the loop (on several
        // threads when there are many vertices), appended in this order.
        let mut world_parts: Vec<WorldPart> = Vec::new();
        let mut surface_jobs: Vec<SurfaceJob> = Vec::new();
        let surface_view = SurfaceView {
            view_proj,
            view_proj_rz,
            cam_pos: inp.cam.pos,
            rect,
            ppp: ctx.pixels_per_point(),
        };
        let mut cached_roots = 0;
        let mut triangle_counts = [0, 0, 0];
        let mut drawn_roots: Vec<Ref> = Vec::with_capacity(roots.len());
        // (RUSTUDIO_GUI_CACHE_WHY: say why each GUI was walked)
        let why_on = debug_env(DebugEnv::Why);
        for root in roots {
            let g = root.gui;
            let read_props = || {
                let class = d.class_of(g).unwrap_or_default();
                let class = class.as_str();
                RootProps {
                    // ZIndexBehavior: Global 0, Sibling 1.
                    global_z: enum_u32(d.prop(g, "ZIndexBehavior")).unwrap_or(0) == 0,
                    screen_gui: class == "ScreenGui",
                    clips: doc::is_a(class, "PluginGui")
                        || boolp(d, g, "ClipsDescendants", class == "SurfaceGui"),
                }
            };
            // (kept while the journal is this document's: see cache_marks)
            let RootProps { global_z, screen_gui, clips } = if specs_ok {
                *self.root_props.entry(g).or_insert_with(read_props)
            } else {
                read_props()
            };
            let canvas_rect =
                Rect::from_min_size(Pos2::ZERO, egui::vec2(root.canvas[0], root.canvas[1]));
            let clip = if screen_gui {
                // The view, in the canvas's coordinates: a ScreenGui's content
                // is not held to its inset box (Roblox: a Frame at Y offset
                // −58 in a CoreUISafeInsets gui shows under the top bar;
                // TopbarPlus's dropdowns hang below its TopbarSafeInsets gui).
                match root.target {
                    Target::Screen(o) => Rect::from_min_max(rect.min - o.to_vec2(), rect.max - o.to_vec2()),
                    _ => canvas_rect,
                }
            } else if clips {
                canvas_rect
            } else {
                Rect::EVERYTHING
            };
            let root_box = GuiBox {
                pos: root.abs_offset,
                size: root.canvas,
                rot: 0.0,
                ..Default::default()
            };
            if self.layout.insert(g, root_box) != Some(root_box) {
                self.layout_changed = true;
            }
            drawn_roots.push(g);
            // A clipped SurfaceGui that can't colour a pixel of the view
            // (off to the side, or too far to cover a pixel centre) isn't
            // walked: scripts keep reading its last layout (its canvas
            // doesn't depend on the camera).
            if let Target::Surface { tl, tr, bl } = root.target {
                if clip != Rect::EVERYTHING && !surface_in_view(&view_proj, rect, ctx.pixels_per_point(), tl, tr, bl) {
                    // (its entries from its last walk stay in `layout`)
                    rustudio_core::bench::add("gui_unseen", 0.0, 1);
                    continue;
                }
            }
            if !inp.playing && clip != Rect::EVERYTHING {
                if let Target::Billboard(center, _) = root.target {
                    let projected = Rect::from_center_size(center, canvas_rect.size());
                    if !projected.expand(1.0 / ctx.pixels_per_point()).intersects(rect) {
                        rustudio_core::bench::add("gui_unseen", 0.0, 1);
                        continue;
                    }
                }
            }
            // A ScreenGui nothing touched since it was walked is drawn from
            // what that walk made.
            // A GUI nothing touched since it was walked is drawn from what
            // that walk made: a ScreenGui's screen triangles as they are, a
            // SurfaceGui's / BillboardGui's canvas triangles projected again.
            let screen_root = matches!(root.target, Target::Screen(_));
            let key = {
                let (ox, oy, kind) = match root.target {
                    Target::Screen(o) => (o.x, o.y, 0.0),
                    Target::Surface { .. } => (0.0, 0.0, 1.0),
                    Target::Billboard(..) => (0.0, 0.0, 2.0),
                };
                [
                    ox, oy, root.canvas[0], root.canvas[1], root.abs_offset[0], root.abs_offset[1],
                    clip.min.x, clip.min.y, clip.max.x, clip.max.y, ctx.pixels_per_point(),
                    global_z as u32 as f32, kind,
                ]
                .map(f32::to_bits)
            };
            // (RUSTUDIO_GUI_CACHE_VERIFY: walk it anyway and compare)
            if !verify && matches!(root.target, Target::Billboard(..)) {
                self.reuse_billboard_canvas(g, key);
            }
            let mut kept: Option<RootCache> = None;
            let mut reuse: Option<RootCache> = None;
            if let Some(c) = self.cache.get(&g).filter(|c| c.key == key) {
                // (its ViewportFrames' images are the renderer's, updated in
                // place: kept unless one got a new texture)
                let vfs_ok = c.vfs.iter().all(|v| {
                    let now = self.vf_tex.get(v).filter(|t| Some(&t.1) == self.vf_size.get(v)).map(|t| t.0);
                    self.vf_walked.get(v).is_some_and(|w| *w == now && now.is_some())
                });
                if vfs_ok {
                    self.vf_live.extend(c.vfs.iter().copied());
                    if let std::collections::hash_map::Entry::Vacant(entry) = self.layout_from.entry(g) {
                        if let Some(boxes) = self.root_boxes.get(&g) {
                            self.layout.extend(boxes.iter().copied());
                            entry.insert(boxes.iter().map(|(r, _)| *r).collect());
                            self.layout_changed = true;
                        }
                    }
                }
                if vfs_ok && verify {
                    kept = self.cache.remove(&g);
                } else if vfs_ok && screen_root {
                    let c = self.cache.get(&g).unwrap();
                    out.extend(c.shapes.iter().cloned());
                    self.hits.extend(c.hits.iter().cloned());
                    crate::runtime::domains::services::perf::draw(crate::runtime::domains::services::perf::UI2D, c.ui2d.0, c.ui2d.1);
                    cached_roots += 1;
                    continue;
                } else if vfs_ok {
                    reuse = self.cache.remove(&g);
                    cached_roots += 1;
                }
            }
            let (target, canvas) = (root.target, root.canvas);
            // World GUIs go to the renderer's pass (depth-tested, lit and
            // tone-mapped with the scene, as Studio's); AlwaysOnTop ones and
            // ScreenGuis to egui over the view (Studio draws AlwaysOnTop
            // colours as they are: tools/fidelity gui-depth).
            let world = !matches!(target, Target::Screen(_)) && !root.on_top;
            let light = root.light;
            // A canvas point's reverse-Z depth (SurfaceGui: its world point;
            // BillboardGui: its anchor's).
            // (and its distance from the camera, for fog).
            // (a world point's)
            let depth_at = |w: Vec3| -> (f32, f32) {
                let c = view_proj_rz * w.extend(1.0);
                let dist = (w - inp.cam.pos).length();
                (if c.w <= 1e-6 { 1.0 } else { c.z / c.w }, dist)
            };
            // A SurfaceGui canvas point's world point.
            let surface_point = |p: Pos2| -> Option<Vec3> {
                let Target::Surface { tl, tr, bl } = target else {
                    return None;
                };
                let (u, v) = (p.x / canvas[0].max(1.0), p.y / canvas[1].max(1.0));
                Some(tl + (tr - tl) * u + (bl - tl) * v)
            };
            let depth_of = |p: Pos2| -> (f32, f32) {
                let w = match target {
                    Target::Surface { .. } => surface_point(p).expect("a surface"),
                    Target::Billboard(_, at) => at,
                    Target::Screen(_) => return (1.0, 0.0),
                };
                depth_at(w)
            };
            // A world point's screen position and clip-space w.
            let project_w = |world: Vec3| -> Option<(Pos2, f32)> {
                let clip = view_proj * world.extend(1.0);
                if clip.w <= 1e-4 {
                    return None;
                }
                let ndc = clip.truncate() / clip.w;
                Some((
                    Pos2::new(
                        rect.min.x + (ndc.x + 1.0) * 0.5 * rect.width(),
                        rect.min.y + (1.0 - ndc.y) * 0.5 * rect.height(),
                    ),
                    clip.w,
                ))
            };
            let project = |world: Vec3| project_w(world).map(|(p, _)| p);
            // SurfaceGuis: a canvas point's screen position and w.
            let surface = |p: Pos2| -> Option<(Pos2, f32)> { project_w(surface_point(p)?) };
            let billboard_origin = match target {
                Target::Billboard(center, _) => center - egui::vec2(canvas[0] * 0.5, canvas[1] * 0.5),
                _ => Pos2::ZERO,
            };
            let map = |p: Pos2| -> Option<Pos2> {
                match target {
                    Target::Screen(o) => Some(o + p.to_vec2()),
                    Target::Billboard(..) => Some(billboard_origin + p.to_vec2()),
                    Target::Surface { .. } => project(surface_point(p)?),
                }
            };
            let perspective = matches!(target, Target::Surface { .. });
            let ppp = ctx.pixels_per_point();
            // Whether an item can colour any pixel of the view, in two
            // halves: its bounds (turned, clipped) in the canvas, kept with
            // its triangles (None: nothing to draw)...
            let canvas_bounds = |it: &walk::Item| -> Option<Rect> {
                // (+ the tessellator's feathering, 1 / ppp canvas units)
                let b = it.shape.visual_bounding_rect();
                if !b.is_positive() {
                    return None;
                }
                let b = b.expand(1.0 / ppp);
                let c = [b.left_top(), b.right_top(), b.right_bottom(), b.left_bottom()]
                    .map(|p| it.aff.apply(p));
                let mut b = Rect::from_points(&c);
                if it.clip != Rect::EVERYTHING {
                    b = b.intersect(it.clip);
                    if !b.is_positive() {
                        return None;
                    }
                }
                Some(b)
            };
            // ... and, each frame, those bounds on screen must overlap the
            // view and hold a pixel centre (the GUI pass has no MSAA, so a
            // triangle with none draws nothing). Unknown (behind the
            // camera) counts as yes.
            let on_screen = |b: Rect| -> bool {
                let mut s = Rect::NOTHING;
                for p in [b.left_top(), b.right_top(), b.right_bottom(), b.left_bottom()] {
                    match map(p) {
                        Some(q) => s.extend_with(q),
                        None => return true,
                    }
                }
                let s = s.intersect(rect);
                // A pixel centre (i + 0.5) / ppp inside on both axes.
                s.is_positive() && contains_pixel_center(s.min.x, s.max.x, ppp)
                    && contains_pixel_center(s.min.y, s.max.y, ppp)
            };
            let (out_from, hits_from) = (out.len(), self.hits.len());
            // The walk and the canvas triangles (or the kept ones).
            self.text_fit.set_screen(screen_root);
            let (citems, lhits) = match reuse.as_mut() {
                Some(c) => (std::mem::take(&mut c.citems), std::mem::take(&mut c.lhits)),
                None => {
                    self.cache.remove(&g);
                    self.no_cache = None;
                    self.walk_vfs.clear();
                    self.walk_reads.clear();
                    self.walk_kids.clear();
                    // Its old entries make way for what this walk writes.
                    for r in self.layout_from.remove(&g).unwrap_or_default() {
                        if r != g {
                            self.layout.remove(&r);
                        }
                    }
                    let walk_scope = rustudio_core::bench::scope("gui_walk");
                    let mut w = Walk {
                        gui: self,
                        ctx,
                        input: &mut inp,
                        items: Vec::new(),
                        hits: Vec::new(),
                        seq: 0,
                        boxes: Vec::new(),
                        global_z,
                        abs_offset: root.abs_offset,
                        screen: match root.target {
                            Target::Screen(p) => Some(p),
                            _ => None,
                        },
                        cur: Ref::none(),
                        rec: Vec::new(),
                        replaying: 0,
                        table_cells: HashMap::new(),
                    };
                    // A UIScale under the GUI itself scales offsets and text, not
                    // the canvas (Studio: RoundResultsGui at 1.5 keeps its
                    // full-screen Background, labels 1.5x, still centred).
                    let root_scale = root_ui_scale(d, g);
                    w.children(
                        g,
                        padded(d, g, canvas_rect, root_scale),
                        Down {
                            aff: Aff::ID,
                            clip,
                            alpha: 1.0,
                            tint: [1.0, 1.0, 1.0],
                            scale: root_scale,
                            z: 0,
                            turn: 0.0,
                        },
                    );
                    drop(walk_scope);
                    let _mesh_scope = rustudio_core::bench::scope("gui_mesh");
                    let mut items = std::mem::take(&mut w.items);
                    let hits = std::mem::take(&mut w.hits);
                    let boxes = std::mem::take(&mut w.boxes);
                    if verify {
                        // The same walk without memos must give the same.
                        let saved = (self.no_cache, self.walk_reads.len(), self.walk_kids.len(), self.walk_vfs.len());
                        self.memo_off = true;
                        let mut w = Walk {
                            gui: self,
                            ctx,
                            input: &mut inp,
                            items: Vec::new(),
                            hits: Vec::new(),
                            seq: 0,
                            boxes: Vec::new(),
                            global_z,
                            abs_offset: root.abs_offset,
                            screen: match root.target {
                                Target::Screen(p) => Some(p),
                                _ => None,
                            },
                            cur: Ref::none(),
                            rec: Vec::new(),
                            replaying: 0,
                            table_cells: HashMap::new(),
                        };
                        w.children(g, padded(d, g, canvas_rect, root_ui_scale(d, g)), Down { aff: Aff::ID, clip, alpha: 1.0, tint: [1.0, 1.0, 1.0], scale: root_ui_scale(d, g), z: 0, turn: 0.0 });
                        let (items2, hits2, boxes2) = (std::mem::take(&mut w.items), std::mem::take(&mut w.hits), std::mem::take(&mut w.boxes));
                        self.memo_off = false;
                        self.no_cache = saved.0;
                        self.walk_reads.truncate(saved.1);
                        self.walk_kids.truncate(saved.2);
                        self.walk_vfs.truncate(saved.3);
                        let tess_of = |tess: &mut Tessellator, it: &walk::Item| {
                            let mut m = Mesh::default();
                            tess.tessellate_shape(it.shape.clone(), &mut m);
                            (m.vertices.iter().map(|v| (v.pos, v.uv, v.color)).collect::<Vec<_>>(), m.indices, m.texture_id)
                        };
                        let mut bad: Option<String> = None;
                        if items.len() != items2.len() {
                            bad = Some(format!("{} / {} items", items.len(), items2.len()));
                        } else {
                            for (i, (a, b)) in items.iter().zip(&items2).enumerate() {
                                if a.z != b.z || a.seq != b.seq || a.clip != b.clip || !a.aff.same(&b.aff) || tess_of(&mut tess, a) != tess_of(&mut tess, b) {
                                    let path = |r: Ref| {
                                        let mut n = Vec::new();
                                        let mut c = r;
                                        while let Some(i) = d.dom.get_by_ref(c) {
                                            if c == g || n.len() > 8 {
                                                break;
                                            }
                                            n.push(i.name.clone());
                                            c = i.parent();
                                        }
                                        n.reverse();
                                        n.join(".")
                                    };
                                    let (ta, tb) = (tess_of(&mut tess, a), tess_of(&mut tess, b));
                                    bad = Some(format!(
                                        "item {i} by {} / {}: z {} {} seq {} {} clip {:?} {:?} verts {:?} / {:?}",
                                        path(a.by), path(b.by), a.z, b.z, a.seq, b.seq, a.clip, b.clip,
                                        &ta.0[..ta.0.len().min(4)], &tb.0[..tb.0.len().min(4)]
                                    ));
                                    break;
                                }
                            }
                        }
                        let same_hit = |a: &walk::LocalHit, b: &walk::LocalHit| a.r == b.r && a.rect == b.rect && a.clip == b.clip && a.sink == b.sink && a.button == b.button && a.painted == b.painted && a.aff.same(&b.aff);
                        if bad.is_none() && (hits.len() != hits2.len() || !hits.iter().zip(&hits2).all(|(a, b)| same_hit(a, b))) {
                            bad = Some(format!("hits {} / {}", hits.len(), hits2.len()));
                        }
                        if bad.is_none() && boxes != boxes2 {
                            bad = Some(format!("boxes {} / {}", boxes.len(), boxes2.len()));
                        }
                        rustudio_core::bench::add(if bad.is_none() { "gui_verify_memo_same" } else { "gui_verify_memo_differ" }, 0.0, 1);
                        if let Some(b) = bad {
                            eprintln!("[guiverify] {}: memo walk differs from a full walk: {b}", name_of(d, g));
                        }
                    }
                    self.layout_from.insert(g, boxes.iter().map(|b| b.0).collect());
                    if self.root_boxes.get(&g) != Some(&boxes) {
                        self.layout_changed = true;
                    }
                    self.root_boxes.insert(g, boxes);
                    // (what it read: for the marks that drop kept output)
                    self.cache_own(g);
                    if global_z {
                        items.sort_by_key(|i| (i.z, i.seq));
                    }
                    let _tess = rustudio_core::bench::scope("gui_tessellate");
                    let mut citems = Vec::with_capacity(items.len());
                    for it in &items {
                        let kept = it.tess.get_or_init(|| {
                            let b = canvas_bounds(it)?;
                            let mut mesh = Mesh::default();
                            tess.tessellate_shape(it.shape.clone(), &mut mesh);
                            if mesh.is_empty() {
                                return None;
                            }
                            if !it.aff.is_id() {
                                for v in &mut mesh.vertices {
                                    v.pos = it.aff.apply(v.pos);
                                }
                            }
                            if it.clip != Rect::EVERYTHING {
                                mesh = clip_mesh(mesh, it.clip);
                            }
                            Some((b, Arc::new(mesh)))
                        });
                        let Some((b, mesh)) = kept else { continue };
                        // (a ScreenGui's view test needs no camera: done now)
                        if screen_root && !on_screen(*b) {
                            continue;
                        }
                        citems.push((*b, mesh.clone()));
                    }
                    (citems, hits)
                }
            };
            let project_scope = (!citems.is_empty()).then(|| rustudio_core::bench::scope("gui_project_mesh"));
            let mut ui2d = (0u64, 0u64);
            let mut last_texture = None;
            let mut n_in = 0;
            let mut n_out = 0;
            let billboard_depth = match target {
                Target::Billboard(..) if world => Some(depth_of(Pos2::ZERO)),
                _ => None,
            };
            let mut projected: Vec<Pos2> = Vec::new();
            let mut world_part: Vec<rustudio_render::world_gui::GuiBatch> = Vec::new();
            // (world SurfaceGuis: reprojected by `SurfaceView::project`
            // after the loop, the same arithmetic)
            let surface_items: &[(Rect, Arc<Mesh>)] = if let (true, true, Target::Surface { tl, tr, bl }) = (perspective, world, target) {
                world_parts.push(WorldPart::Job(surface_jobs.len()));
                surface_jobs.push(SurfaceJob { tl, tr, bl, canvas, light, items: citems.clone() });
                &[]
            } else {
                &citems
            };
            for (bounds, retained) in surface_items {
                if !screen_root && !on_screen(*bounds) {
                    continue;
                }
                n_in += retained.indices.len() as u64 / 3;
                let subdivided;
                // (the vertices' screen positions when subdivide already
                // projected them for its test)
                projected.clear();
                let mesh = if perspective {
                    subdivided = subdivide(retained, mech::GUI_SURFACE_AFFINE_ERR_PX, rect, &surface, &mut projected);
                    subdivided.as_ref()
                } else {
                    retained.as_ref()
                };
                let projected = (projected.len() == mesh.vertices.len()).then_some(projected.as_slice());
                n_out += mesh.indices.len() as u64 / 3;
                if mesh.is_empty() {
                    continue;
                }
                if world {
                    let width = rect.width().max(1.0);
                    let height = rect.height().max(1.0);
                    let vertex = |v: &Vertex, p: Pos2, (depth, dist)| rustudio_render::world_gui::GuiVertex {
                        ndc: [
                            (p.x - rect.min.x) / width * 2.0 - 1.0,
                            1.0 - (p.y - rect.min.y) / height * 2.0,
                        ],
                        depth,
                        uv: [v.uv.x, v.uv.y],
                        color: v.color.to_array(),
                        light: [light; 3],
                        dist,
                        alpha_mul: 1.0,
                        ..Default::default()
                    };
                    if let Some(depth) = billboard_depth {
                        // All triangles share the billboard's translation and depth.
                        let verts = mesh.vertices.iter().map(|v| {
                            vertex(v, billboard_origin + v.pos.to_vec2(), depth)
                        });
                        rustudio_render::world_gui::push(&mut world_part, mesh.texture_id, verts, &mesh.indices);
                    } else {
                        let verts: Option<Vec<_>> = mesh.vertices.iter().enumerate().map(|(i, v)| {
                            let p = match projected {
                                Some(p) => p[i],
                                None => map(v.pos)?,
                            };
                            Some(vertex(v, p, depth_of(v.pos)))
                        }).collect();
                        if let Some(verts) = verts {
                            rustudio_render::world_gui::push(&mut world_part, mesh.texture_id, verts, &mesh.indices);
                        }
                    }
                } else {
                    // Screen roots are already clipped to the canvas. Retain one
                    // mesh per consecutive texture run, preserving alpha order.
                    if let Target::Screen(origin) = target {
                        if out.len() > out_from {
                            if let Some(Shape::Mesh(last)) = out.last_mut() {
                                if last.texture_id == mesh.texture_id {
                                    let last = Arc::make_mut(last);
                                    let offset = last.vertices.len() as u32;
                                    last.indices.extend(mesh.indices.iter().map(|i| offset + i));
                                    last.vertices.extend(mesh.vertices.iter().map(|vertex| Vertex {
                                        pos: origin + vertex.pos.to_vec2(),
                                        ..*vertex
                                    }));
                                    ui2d.1 += (mesh.indices.len() / 3) as u64;
                                    continue;
                                }
                            }
                        }
                    }
                    let mut mesh = mesh.clone();
                    if !mesh.vertices.iter_mut().all(|v| {
                        if let Some(p) = map(v.pos) {
                            v.pos = p;
                            true
                        } else {
                            false
                        }
                    }) {
                        continue;
                    }
                    ui2d.1 += (mesh.indices.len() / 3) as u64;
                    ui2d.0 += (last_texture.replace(mesh.texture_id) != Some(mesh.texture_id)) as u64;
                    out.push(Shape::Mesh(mesh.into()));
                }
            }
            if ui2d.0 > 0 {
                crate::runtime::domains::services::perf::draw(crate::runtime::domains::services::perf::UI2D, ui2d.0, ui2d.1);
            }
            if !world_part.is_empty() {
                world_parts.push(WorldPart::Batches(world_part));
            }
            triangle_counts[perspective as usize] += n_in;
            triangle_counts[2] += n_out;
            drop(project_scope);
            let hit_scope = (!lhits.is_empty()).then(|| rustudio_core::bench::scope("gui_project_hits"));
            for h in &lhits {
                let corners = [
                    h.rect.left_top(),
                    h.rect.right_top(),
                    h.rect.right_bottom(),
                    h.rect.left_bottom(),
                ]
                .map(|p| h.aff.apply(p));
                let project_quad = |[a, b, c, d]: [Pos2; 4]| Some([map(a)?, map(b)?, map(c)?, map(d)?]);
                let Some(quad) = project_quad(corners) else { continue };
                let clip = h.clip.and_then(|c| {
                    project_quad([c.left_top(), c.right_top(), c.right_bottom(), c.left_bottom()])
                });
                self.hits.push(Hit {
                    r: h.r,
                    quad,
                    clip,
                    button: h.button,
                    textbox: h.textbox,
                    sink: h.sink,
                    scrolling: h.scrolling,
                    page_layout: h.page_layout,
                    drag: h.drag,
                    painted: h.painted,
                });
            }
            drop(hit_scope);
            if let Some(mut c) = reuse {
                // (drawn from what was kept: keep it)
                c.citems = citems;
                c.lhits = lhits;
                self.cache.insert(g, c);
                continue;
            }
            if let Some(k) = kept.take() {
                self.cache_verify(d, g, &k, screen_root, &out[out_from..], &self.hits[hits_from..], &citems, &lhits);
            }
            if self.no_cache.is_none() {
                let (shapes, hits) = if screen_root {
                    (out[out_from..].to_vec(), self.hits[hits_from..].to_vec())
                } else {
                    (Vec::new(), Vec::new())
                };
                let (citems, lhits) = if screen_root { (Vec::new(), Vec::new()) } else { (citems, lhits) };
                self.cache.insert(
                    g,
                    RootCache {
                        key,
                        shapes,
                        hits,
                        ui2d,
                        vfs: std::mem::take(&mut self.walk_vfs),
                        citems,
                        lhits,
                    },
                );
            } else {
                rustudio_core::bench::add("gui_uncached", 0.0, 1);
            }
            if why_on {
                eprintln!(
                    "[guiwhy] {} walked: {}",
                    name_of(d, g),
                    match (self.no_cache, self.cache_why.remove(&g)) {
                        (Some(v), _) => format!("kept out: {v}"),
                        (_, Some(w)) => w,
                        _ => "new or view changed".to_string(),
                    }
                );
            }
        }
        // The world SurfaceGuis' triangles, then every world root's in order.
        {
            let _p = (!surface_jobs.is_empty()).then(|| rustudio_core::bench::scope("gui_project_surfaces"));
            let mut done = surface_view.project_all(&surface_jobs);
            for o in &done {
                triangle_counts[1] += o.n_in;
                triangle_counts[2] += o.n_out;
            }
            for part in world_parts {
                let batches = match part {
                    WorldPart::Batches(b) => b,
                    WorldPart::Job(i) => std::mem::take(&mut done[i].batches),
                };
                for b in batches {
                    let Some(texture) = b.texture else { continue };
                    rustudio_render::world_gui::push(&mut self.world_batches, texture, b.vertices, &b.indices);
                }
            }
        }
        // (RUSTUDIO_GUI_BATCH_HASH: print a hash of the frame's world GUI
        // triangles, to compare builds frame by frame without the 3D view)
        static BATCH_HASH: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
        if *BATCH_HASH.get_or_init(|| std::env::var_os("RUSTUDIO_GUI_BATCH_HASH").is_some()) {
            use std::hash::{Hash, Hasher};
            let mut h = std::collections::hash_map::DefaultHasher::new();
            for b in &self.world_batches {
                b.texture.hash(&mut h);
                for v in &b.vertices {
                    for f in v.ndc.iter().chain([&v.depth]).chain(&v.uv).chain(&v.light).chain([&v.dist]) {
                        f.to_bits().hash(&mut h);
                    }
                    v.color.hash(&mut h);
                }
                b.indices.hash(&mut h);
            }
            eprintln!("[guihash] {} {}", self.world_batches.iter().map(|b| b.indices.len()).sum::<usize>(), h.finish());
        }
        let deferred = self.text_fit.take_deferred();
        if deferred > 0 {
            rustudio_core::bench::add("gui_text_deferred", 0.0, deferred);
            ctx.request_repaint();
        }
        rustudio_core::bench::add("gui_cached", 0.0, cached_roots);
        for (name, count) in ["gui_tris_flat", "gui_tris_surface_in", "gui_tris_out"].into_iter().zip(triangle_counts) {
            rustudio_core::bench::add(name, 0.0, count);
        }
        // GUIs drawn last frame and not now: their entries go.
        let was = std::mem::take(&mut self.layout_roots);
        let now: rustc_hash::FxHashSet<Ref> = drawn_roots.iter().copied().collect();
        let mut gone: rustc_hash::FxHashSet<Ref> = Default::default();
        for g in was.iter().filter(|g| !now.contains(*g) && gone.insert(**g)) {
            for r in self.layout_from.remove(g).unwrap_or_default() {
                self.layout.remove(&r);
            }
            self.layout.remove(g);
            self.layout_changed = true;
        }
        if debug_env(DebugEnv::Verify) || debug_env(DebugEnv::LayoutVerify) {
            // The layout as a fresh frame would write it.
            let mut fresh: HashMap<Ref, GuiBox> = HashMap::new();
            for g in &drawn_roots {
                for (r, b) in self.root_boxes.get(g).into_iter().flatten() {
                    fresh.insert(*r, *b);
                }
            }
            for g in &drawn_roots {
                if let Some(b) = self.layout.get(g) {
                    fresh.insert(*g, *b);
                }
            }
            let same = fresh == self.layout;
            rustudio_core::bench::add(if same { "gui_verify_layout_same" } else { "gui_verify_layout_differ" }, 0.0, 1);
            if !same {
                eprintln!("[guiverify] layout: {} kept, {} fresh", self.layout.len(), fresh.len());
            }
        }
        self.layout_roots = drawn_roots;
        rustudio_core::bench::add("gui_memo_hit", 0.0, std::mem::take(&mut self.memo_hits));
        rustudio_core::bench::add("gui_memo_walk", 0.0, std::mem::take(&mut self.memo_walks));
        self.drawn = out.len();
        if rustudio_core::profiler::active() {
            use rustudio_core::profiler::{counter, Cat};
            counter("GUI items drawn", Cat::Gui, out.len() as f64);
            counter("GUI layers", Cat::Gui, self.layout_roots.len() as f64);
        }
        let _paint = rustudio_core::bench::scope("gui_paint");
        let clipped = painter.with_clip_rect(rect);
        // (RUSTUDIO_GUI_BLACK_BG: ScreenGuis over black, to compare GUI
        // captures without the 3D view's frame-to-frame differences)
        if debug_env(DebugEnv::BlackBg) {
            clipped.rect_filled(rect, 0.0, Color32::BLACK);
        }
        clipped.extend(out);
    }

    /// Pointer over the viewport: hover, clicks and the wheel. Queues events;
    /// returns true when the GUI took the input (no 3D picking or clicks).
    /// Some GUI object has the pointer over it (MouseEnter without a Leave yet).
    pub fn has_hover(&self) -> bool {
        !self.hovered.is_empty()
    }

    /// Pointer is currently over a GUI object that sinks clicks (`Active`, button, or TextBox).
    pub fn pointer_over_sink(&self) -> bool {
        self.last_over_sink
    }

    /// Checks the game GUI at the finger's position before routing it to the camera.
    pub fn sinks_pointer_at(&self, p: Pos2) -> bool {
        self.hits.iter().rev().any(|hit| {
            hit.sink && point_in_quad(&hit.quad, p) && hit.clip.is_none_or(|clip| point_in_quad(&clip, p))
        })
    }

    /// The last frame's mouse wheel was consumed by a GUI object.
    pub fn wheel_sunk(&self) -> bool {
        self.last_wheel_sunk
    }

    /// Whether a mouse wheel at screen point `p` lands on a GUI object that
    /// consumes scroll / pointer input (an Active/sink object, ScrollingFrame, or UIPageLayout).
    pub fn sinks_wheel_at(&self, p: Pos2) -> bool {
        self.hits.iter().rev().any(|h| {
            (h.sink || h.scrolling || h.page_layout.is_some())
                && point_in_quad(&h.quad, p)
                && h.clip.is_none_or(|c| point_in_quad(&c, p))
        })
    }

    /// A drag from elsewhere (Explorer instances) let go at `p` over this
    /// plugin GUI: nothing here was pressed, so only the top object that
    /// takes clicks hears MouseButton1Up (a button) and InputEnded
    /// (MouseButton1); no Click or Activated (Studio 0.747,
    /// checks/plugins/ref/captures/xdrag.txt S1, S4).
    pub fn drop_release(&mut self, p: Pos2, rect: Rect) {
        let Some(t) = self
            .hits
            .iter()
            .rev()
            .find(|h| h.sink && point_in_quad(&h.quad, p) && h.clip.is_none_or(|c| point_in_quad(&c, p)))
            .cloned()
        else {
            return;
        };
        let inset = self.input_inset.unwrap_or(mech::GUI_TOP_INSET);
        let (x, y) = (p.x - rect.left(), p.y - rect.top() - inset);
        if t.button {
            self.events.push(GuiEvent::Fire(t.r, "MouseButton1Up", vec![GuiArg::Num(x as f64), GuiArg::Num(y as f64)]));
        }
        self.events.push(GuiEvent::Fire(t.r, "InputEnded", vec![GuiArg::Input { button: 1, began: false, x, y }]));
    }

    pub fn input(&mut self, ctx: &egui::Context, resp: &egui::Response, rect: Rect) -> bool {
        let pointer = self.pointer_override.or(resp.hover_pos()).filter(|p| rect.contains(*p));
        let under: Vec<Hit> = match pointer {
            Some(p) => self
                .hits
                .iter()
                .filter(|h| {
                    point_in_quad(&h.quad, p) && h.clip.is_none_or(|c| point_in_quad(&c, p))
                })
                .cloned()
                .collect(),
            None => Vec::new(),
        };
        let top = under.iter().rev().find(|h| h.sink).cloned();
        self.last_over_sink = top.is_some();
        self.last_wheel_sunk = false;
        // GUI coordinates: from the view's top-left, below the top bar inset (as InputObject.Position).
        let inset = self.input_inset.unwrap_or(mech::GUI_TOP_INSET);
        // Plugin GUIs (dock widgets, CoreGui in the edit view) follow Studio's
        // edit-mode order (checks/plugins/widgets/captures): the pointer coming
        // in is an InputBegan (MouseMovement), MouseButton1Down comes before
        // InputBegan, MouseButton1Click before MouseButton1Up, Activated's
        // click count is 0.
        let plugin = self.input_inset.is_some();
        let gui_xy = |p: Pos2| (p.x - rect.left(), p.y - rect.top() - inset);
        let now: HashSet<Ref> = under.iter().map(|h| h.r).collect();
        let moved = ctx.input(|i| i.pointer.delta() != egui::Vec2::ZERO);
        if let Some(p) = pointer {
            let (x, y) = gui_xy(p);
            for h in &under {
                if !self.hovered.contains(&h.r) {
                    self.events.push(GuiEvent::Fire(
                        h.r,
                        "MouseEnter",
                        vec![GuiArg::Num(x as f64), GuiArg::Num(y as f64)],
                    ));
                    self.events.push(GuiEvent::Fire(
                        h.r,
                        "InputBegan",
                        vec![GuiArg::Input { button: 0, began: true, x, y }],
                    ));
                    self.events.push(GuiEvent::GuiStateChanged(h.r));
                } else if moved {
                    self.events.push(GuiEvent::Fire(
                        h.r,
                        "MouseMoved",
                        vec![GuiArg::Num(x as f64), GuiArg::Num(y as f64)],
                    ));
                    self.events.push(GuiEvent::Fire(
                        h.r,
                        "InputChanged",
                        vec![GuiArg::Input { button: 0, began: false, x, y }],
                    ));
                }
            }
        }
        let (lx, ly) = pointer.map(gui_xy).unwrap_or((0.0, 0.0));
        if moved {
            if let (Some(d), Some(p)) = (self.dragging, pointer) {
                let (x, y) = gui_xy(p);
                super::share().drags.push((d, 1, x, y));
            }
            if let (Some(tb), Some(p)) = (self.tb_selecting, pointer) {
                if self.focus == Some(tb) && ctx.input(|i| i.pointer.primary_down()) {
                    if let Some(pos) = self.textbox_pos_from_screen(tb, p) {
                        if let Some(layout) = self.tb_layouts.get(&tb) {
                            let mut st = textbox::TextBoxState {
                                text: layout.text.clone(),
                                cursor: self.tb_cursor,
                                selection_start: self.tb_sel,
                                focused: true,
                                editable: true,
                                multi_line: false,
                                clear_text_on_focus: false,
                            };
                            let out = st.apply(textbox::EditAction::DragTo { pos });
                            if out.changed_cursor || out.changed_selection {
                                self.tb_cursor = st.cursor;
                                self.tb_sel = st.selection_start;
                                self.tb_blink = ui_time(ctx);
                                let mut sh = super::share();
                                sh.tb_cursor.insert(tb, st.cursor);
                                sh.tb_sel.insert(tb, st.selection_start);
                                drop(sh);
                                self.events.push(GuiEvent::TextBoxUpdate {
                                    r: tb,
                                    text: None,
                                    cursor: out.changed_cursor.then_some(st.cursor),
                                    selection: out.changed_selection.then_some(st.selection_start),
                                });
                            }
                        }
                    }
                }
            }
        }
        for r in self.hovered.difference(&now) {
            self.events.push(GuiEvent::Fire(
                *r,
                "MouseLeave",
                vec![GuiArg::Num(lx as f64), GuiArg::Num(ly as f64)],
            ));
            self.events.push(GuiEvent::Fire(
                *r,
                "InputEnded",
                vec![GuiArg::Input { button: 0, began: false, x: lx, y: ly }],
            ));
            self.events.push(GuiEvent::GuiStateChanged(*r));
        }
        self.hovered = now;
        {
            let mut sh = super::share();
            sh.hovered = self.hovered.clone();
            sh.buttons = ctx.input(|i| {
                [
                    i.pointer.primary_down(),
                    i.pointer.secondary_down(),
                    i.pointer.middle_down(),
                ]
            });
        }

        let mut took = top.is_some() && (resp.clicked() || resp.secondary_clicked());
        let presses: Vec<(egui::PointerButton, bool)> = ctx.input(|i| {
            i.events
                .iter()
                .filter_map(|e| match e {
                    egui::Event::PointerButton {
                        button, pressed, ..
                    } => Some((*button, *pressed)),
                    _ => None,
                })
                .collect()
        });
        let had_press = !presses.is_empty();
        for (button, pressed) in presses {
            let b = match button {
                egui::PointerButton::Primary => 1u8,
                egui::PointerButton::Secondary => 2,
                _ => continue,
            };
            let Some(p) = pointer else { continue };
            {
                let mut sh = super::share();
                sh.mouse_edges.push((b, pressed, top.is_some()));
                if sh.mouse_edges.len() > 64 {
                    sh.mouse_edges.remove(0);
                }
            }
            let (x, y) = gui_xy(p);
            let input = GuiArg::Input {
                button: b,
                began: pressed,
                x,
                y,
            };
            if pressed {
                // Clicking anywhere else ends text editing.
                if let Some(f) = self
                    .focus
                    .filter(|f| top.as_ref().is_none_or(|t| t.r != *f))
                {
                    // The scripts fire FocusLost (crates/rustudio-engine/src/domains/gui_input.rs).
                    let mut sh = super::share();
                    sh.focus_events.push((f, false, false, Some((b, x, y))));
                    sh.focus = None;
                    sh.tb_cursor.insert(f, -1);
                    sh.tb_sel.insert(f, -1);
                    self.focus = None;
                    self.tb_cursor = -1;
                    self.tb_sel = -1;
                    self.tb_selecting = None;
                    self.tb_held_key = None;
                }
                let began_at = self.events.len();
                for h in &under {
                    self.events
                        .push(GuiEvent::Fire(h.r, "InputBegan", vec![input.clone()]));
                }
                // A left press starts dragging the top-most draggable object
                // that nothing in front of it took.
                if b == 1 {
                    let top_i = under.iter().rposition(|h| h.sink);
                    let drag = under
                        .iter()
                        .enumerate()
                        .rev()
                        .find(|(i, h)| h.drag && top_i.is_none_or(|t| *i >= t))
                        .map(|(_, h)| h.r);
                    if let Some(d) = drag {
                        self.dragging = Some(d);
                        super::share().drags.push((d, 0, x, y));
                        took = true;
                    }
                }
                if let Some(t) = &top {
                    took = true;
                    self.pressed = Some((t.r, b));
                    super::share().pressed = Some(t.r);
                    self.events.push(GuiEvent::GuiStateChanged(t.r));
                    if t.button {
                        let ev = GuiEvent::Fire(
                            t.r,
                            if b == 1 {
                                "MouseButton1Down"
                            } else {
                                "MouseButton2Down"
                            },
                            vec![GuiArg::Num(x as f64), GuiArg::Num(y as f64)],
                        );
                        if plugin {
                            self.events.insert(began_at, ev);
                        } else {
                            self.events.push(ev);
                        }
                    }
                    if t.textbox && b == 1 {
                        let now_s = ui_time(ctx);
                        let click_pos = self.textbox_pos_from_screen(t.r, p);
                        let is_dbl = self.tb_last_click.is_some_and(|(lr, lt, lp)| {
                            lr == t.r && (now_s - lt) <= 0.4 && (lp - p).length() <= 6.0
                        });
                        self.tb_last_click = if is_dbl { None } else { Some((t.r, now_s, p)) };
                        self.tb_blink = now_s;
                        if self.focus != Some(t.r) {
                            self.focus = Some(t.r);
                            let pos = click_pos.unwrap_or(1);
                            self.tb_cursor = pos as i32;
                            self.tb_sel = -1;
                            self.tb_selecting = Some(t.r);
                            self.tb_held_key = None;
                            let mut sh = super::share();
                            sh.tb_focus_click = click_pos.map(|cp| (t.r, cp));
                            sh.focus_events.push((t.r, true, false, Some((b, x, y))));
                            sh.focus = Some(t.r);
                            sh.tb_cursor.insert(t.r, self.tb_cursor);
                            sh.tb_sel.insert(t.r, -1);
                        } else if let (Some(pos), Some(layout)) = (click_pos, self.tb_layouts.get(&t.r)) {
                            let shift = ctx.input(|i| i.modifiers.shift);
                            let mut st = textbox::TextBoxState {
                                text: layout.text.clone(),
                                cursor: self.tb_cursor,
                                selection_start: self.tb_sel,
                                focused: true,
                                editable: true,
                                multi_line: false,
                                clear_text_on_focus: false,
                            };
                            let action = if is_dbl {
                                textbox::EditAction::DoubleClickAt { pos }
                            } else {
                                self.tb_selecting = Some(t.r);
                                textbox::EditAction::ClickAt { pos, shift }
                            };
                            let out = st.apply(action);
                            if out.changed_cursor || out.changed_selection {
                                self.tb_cursor = st.cursor;
                                self.tb_sel = st.selection_start;
                                let mut sh = super::share();
                                sh.tb_cursor.insert(t.r, st.cursor);
                                sh.tb_sel.insert(t.r, st.selection_start);
                                drop(sh);
                                self.events.push(GuiEvent::TextBoxUpdate {
                                    r: t.r,
                                    text: None,
                                    cursor: out.changed_cursor.then_some(st.cursor),
                                    selection: out.changed_selection.then_some(st.selection_start),
                                });
                            }
                        }
                    }
                }
            } else {
                if b == 1 {
                    self.tb_selecting = None;
                    if let Some(d) = self.dragging.take() {
                        super::share().drags.push((d, 2, x, y));
                    }
                }
                for h in &under {
                    self.events
                        .push(GuiEvent::Fire(h.r, "InputEnded", vec![input.clone()]));
                }
                if let Some((pr, pb)) = self.pressed.take() {
                    super::share().pressed = None;
                    self.events.push(GuiEvent::GuiStateChanged(pr));
                    if pb != b {
                        continue;
                    }
                    took = true;
                    if self.hits.iter().any(|h| h.r == pr && h.button) {
                        let up_at = self.events.len();
                        self.events.push(GuiEvent::Fire(
                            pr,
                            if b == 1 {
                                "MouseButton1Up"
                            } else {
                                "MouseButton2Up"
                            },
                            vec![GuiArg::Num(x as f64), GuiArg::Num(y as f64)],
                        ));
                        if plugin && top.as_ref().is_some_and(|t| t.r == pr) {
                            // Click, Up, Activated (count 0)
                            let up = self.events.remove(up_at);
                            self.events.push(GuiEvent::Fire(pr, if b == 1 { "MouseButton1Click" } else { "MouseButton2Click" }, vec![]));
                            self.events.push(up);
                            if b == 1 {
                                self.events.push(GuiEvent::Fire(pr, "Activated", vec![input.clone(), GuiArg::Num(0.0)]));
                            }
                            self.last_click = Some(pr);
                        } else if top.as_ref().is_some_and(|t| t.r == pr) {
                            self.events.push(GuiEvent::Fire(
                                pr,
                                if b == 1 {
                                    "MouseButton1Click"
                                } else {
                                    "MouseButton2Click"
                                },
                                vec![],
                            ));
                            if b == 1 {
                                self.events.push(GuiEvent::Fire(
                                    pr,
                                    "Activated",
                                    vec![input.clone(), GuiArg::Num(1.0)],
                                ));
                            }
                            self.last_click = Some(pr);
                        }
                    }
                }
            }
        }
        // The wheel over a ScrollingFrame scrolls it: ScrollWheelDeltaAmount
        // (100) pixels per notch.
        let wheel: f32 = ctx.input(|i| {
            i.raw
                .events
                .iter()
                .filter_map(|e| match e {
                    egui::Event::MouseWheel { unit, delta, .. } => Some(
                        mech::GUI_SCROLL_PX_PER_NOTCH
                            * match unit {
                                egui::MouseWheelUnit::Line => delta.y,
                                egui::MouseWheelUnit::Point => {
                                    delta.y / mech::WHEEL_POINTS_PER_NOTCH
                                }
                                egui::MouseWheelUnit::Page => delta.y * 3.0,
                            },
                    ),
                    _ => None,
                })
                .sum()
        });
        if moved || had_press || wheel != 0.0 {
            // Mouse activity hides the keyboard/gamepad selection cursor (matching Roblox).
            super::share().selection_visible = false;
        }
        if wheel != 0.0 && resp.hovered() {
            // Every object under the pointer hears the notch (MouseWheelForward
            // / Backward and InputChanged with a MouseWheel InputObject).
            if let Some(p) = pointer {
                let (x, y) = gui_xy(p);
                let fwd = wheel > 0.0;
                for h in &under {
                    self.events.push(GuiEvent::Fire(
                        h.r,
                        if fwd { "MouseWheelForward" } else { "MouseWheelBackward" },
                        vec![GuiArg::Num(x as f64), GuiArg::Num(y as f64)],
                    ));
                    self.events.push(GuiEvent::Fire(
                        h.r,
                        "InputChanged",
                        vec![GuiArg::Input { button: if fwd { 3 } else { 4 }, began: false, x, y }],
                    ));
                }
            }
            if let Some(sf) = under.iter().rev().find(|h| h.scrolling) {
                self.events.push(GuiEvent::Scroll(sf.r, [0.0, -wheel]));
                took = true;
                self.last_wheel_sunk = true;
            } else if let Some(pl) = under.iter().rev().find_map(|h| h.page_layout) {
                super::share().page_scrolls.push((pl, if wheel > 0.0 { -1 } else { 1 }));
                took = true;
                self.last_wheel_sunk = true;
            } else if top.is_some() {
                took = true;
                self.last_wheel_sunk = true;
            }
        }
        took
    }

    /// Maps a screen pointer position `p` into a 1-based UTF-8 byte offset inside `tb`'s text.
    fn textbox_pos_from_screen(&self, tb: Ref, p: Pos2) -> Option<usize> {
        let layout = self.tb_layouts.get(&tb)?;
        if layout.is_placeholder || layout.text.is_empty() {
            return Some(1);
        }
        let origin = layout.screen_origin?;
        let canvas_p = Pos2::new(p.x - origin.x, p.y - origin.y);
        // (For unrotated ScreenGui TextBoxes, aff is identity; for rotated, invert 2x2)
        let local_p = if layout.aff.is_id() {
            canvas_p
        } else {
            canvas_p
        };
        let rel = local_p - layout.text_origin;
        let cc = layout.galley.cursor_from_pos(rel);
        let char_idx: usize = cc.index.into();
        let byte_pos = layout
            .text
            .char_indices()
            .nth(char_idx)
            .map(|(b, _)| b)
            .unwrap_or(layout.text.len());
        Some(byte_pos + 1)
    }

    /// Editing the focused TextBox: full caret movement, selection, clipboard,
    /// word jumps/deletes, MultiLine, TextEditable, key repeat, and Enter/Escape.
    /// True while a TextBox has the keyboard.
    pub fn keyboard(&mut self, ctx: &egui::Context, doc: &Document) -> bool {
        // A focus a script takes from a key's InputBegan (the chat's "/"
        // hotkey calling CaptureFocus) doesn't receive that key's character,
        // as in Roblox. The script runs on the Play worker and the character
        // can come a frame or two after the key (IME commit), so remember the
        // last key's character and drop it once if it arrives soon after.
        const WINDOW: std::time::Duration = std::time::Duration::from_millis(250);
        let now = rustudio_core::sim_clock::now();
        let now_s = ui_time(ctx);
        ctx.input(|i| {
            for e in &i.raw.events {
                if let egui::Event::Key { key, pressed: true, repeat: false, modifiers, .. } = e {
                    if !modifiers.ctrl && !modifiers.command && !modifiers.alt {
                        let sym = key.symbol_or_name();
                        self.last_key_char = (sym.len() == 1 && sym.as_bytes()[0].is_ascii_graphic()).then(|| (sym.to_lowercase(), now));
                    }
                }
            }
        });
        // Scripts' TextBox:CaptureFocus / ReleaseFocus (they fired the events).
        let (focus_reqs, script_sels) = {
            let mut sh = super::share();
            (
                std::mem::take(&mut sh.focus_req),
                std::mem::take(&mut sh.tb_script_sel),
            )
        };
        for req in focus_reqs {
            if req.is_some() && req != self.focus {
                self.swallow = self.last_key_char.take().filter(|(_, at)| now - *at < WINDOW);
                self.tb_blink = now_s;
                self.tb_held_key = None;
                if let Some(r) = req {
                    let sh = super::share();
                    let text_len = text_prop(doc, r, "Text").len() as i32;
                    self.tb_cursor = sh.tb_cursor.get(&r).copied().unwrap_or(text_len + 1);
                    self.tb_sel = sh.tb_sel.get(&r).copied().unwrap_or(-1);
                }
            } else if req.is_none() {
                self.tb_cursor = -1;
                self.tb_sel = -1;
                self.tb_selecting = None;
                self.tb_held_key = None;
            }
            self.focus = req;
        }
        for (r, cur, sel) in script_sels {
            if self.focus == Some(r) {
                if let Some(c) = cur {
                    self.tb_cursor = c;
                    self.tb_blink = now_s;
                }
                if let Some(s) = sel {
                    self.tb_sel = s;
                }
            }
        }
        if self.swallow.as_ref().is_some_and(|(_, at)| now - *at >= WINDOW) {
            self.swallow = None;
        }
        let Some(f) = self.focus else { return false };
        if !doc.exists(f) {
            self.focus = None;
            self.tb_cursor = -1;
            self.tb_sel = -1;
            self.tb_selecting = None;
            self.tb_held_key = None;
            let mut sh = super::share();
            sh.focus = None;
            sh.tb_cursor.insert(f, -1);
            sh.tb_sel.insert(f, -1);
            return false;
        }
        // Keep live cursor valid against the document's current Text.
        let cur_text = text_prop(doc, f, "Text");
        let editable = boolp(doc, f, "TextEditable", true);
        let multi_line = boolp(doc, f, "MultiLine", false);
        let clear_on_focus = boolp(doc, f, "ClearTextOnFocus", true);
        if self.tb_cursor < 1 {
            self.tb_cursor = (cur_text.len() + 1) as i32;
        } else if (self.tb_cursor - 1) as usize > cur_text.len() {
            self.tb_cursor = (cur_text.len() + 1) as i32;
        }
        if self.tb_sel > (cur_text.len() + 1) as i32 {
            self.tb_sel = (cur_text.len() + 1) as i32;
        }
        let mut state = textbox::TextBoxState {
            text: cur_text,
            cursor: self.tb_cursor,
            selection_start: self.tb_sel,
            focused: true,
            editable,
            multi_line,
            clear_text_on_focus: clear_on_focus,
        };

        // Collect raw events (nothing else in the editor may have used them up).
        let raw_events: Vec<egui::Event> = ctx.input(|i| i.raw.events.clone());
        let saw_copy_ev = raw_events.iter().any(|e| matches!(e, egui::Event::Copy));
        let saw_cut_ev = raw_events.iter().any(|e| matches!(e, egui::Event::Cut));
        let saw_paste_ev = raw_events.iter().any(|e| matches!(e, egui::Event::Paste(_)));

        let key_to_action = |key: egui::Key, mods: egui::Modifiers, repeat: bool, multi: bool, clip: &str| -> Option<textbox::EditAction> {
            let cmd = mods.ctrl || mods.command;
            let word = cmd || (cfg!(target_os = "macos") && mods.alt);
            let shift = mods.shift;
            match key {
                egui::Key::ArrowLeft => Some(textbox::EditAction::Left { shift, word }),
                egui::Key::ArrowRight => Some(textbox::EditAction::Right { shift, word }),
                egui::Key::ArrowUp => Some(textbox::EditAction::Up { shift }),
                egui::Key::ArrowDown => Some(textbox::EditAction::Down { shift }),
                egui::Key::Home => Some(textbox::EditAction::Home { shift }),
                egui::Key::End => Some(textbox::EditAction::End { shift }),
                egui::Key::Backspace => Some(textbox::EditAction::Backspace { word }),
                egui::Key::Delete => Some(textbox::EditAction::Delete { word }),
                egui::Key::Enter => (!repeat || multi).then_some(textbox::EditAction::Enter),
                egui::Key::Escape => (!repeat).then_some(textbox::EditAction::Escape),
                egui::Key::A if cmd && !mods.alt => Some(textbox::EditAction::SelectAll),
                egui::Key::C if cmd && !mods.alt && !shift && !saw_copy_ev => Some(textbox::EditAction::Copy),
                egui::Key::X if cmd && !mods.alt && !shift && !saw_cut_ev => Some(textbox::EditAction::Cut),
                egui::Key::V if cmd && !mods.alt && !shift && !saw_paste_ev => {
                    Some(textbox::EditAction::Paste(clip.to_string()))
                }
                _ => None,
            }
        };
        let is_repeatable = |key: egui::Key| {
            matches!(
                key,
                egui::Key::ArrowLeft
                    | egui::Key::ArrowRight
                    | egui::Key::ArrowUp
                    | egui::Key::ArrowDown
                    | egui::Key::Backspace
                    | egui::Key::Delete
            )
        };

        let mut actions: Vec<textbox::EditAction> = Vec::new();
        for e in &raw_events {
            match e {
                egui::Event::Copy => actions.push(textbox::EditAction::Copy),
                egui::Event::Cut => actions.push(textbox::EditAction::Cut),
                egui::Event::Paste(s) => actions.push(textbox::EditAction::Paste(s.clone())),
                egui::Event::Text(t) => {
                    let mut s = t.as_str();
                    if let Some((c, _)) = self.swallow.as_ref() {
                        if let Some(rest) = s.strip_prefix(c.as_str()) {
                            s = rest;
                            self.swallow = None;
                        } else if !s.is_empty() {
                            self.swallow = None;
                        }
                    }
                    // Drop ASCII control characters (Ctrl+A/C/X/V on some platforms send \x01/\x03/\x18/\x16).
                    let clean: String = s
                        .chars()
                        .filter(|c| !c.is_control() || *c == '\t' || (*c == '\n' && multi_line))
                        .collect();
                    if !clean.is_empty() {
                        actions.push(textbox::EditAction::InsertText(clean));
                    }
                }
                egui::Event::Key { key, pressed: true, repeat, modifiers, .. } => {
                    if *repeat {
                        if let Some(held) = self.tb_held_key.as_mut() {
                            held.4 = true;
                        }
                    } else if is_repeatable(*key) {
                        self.tb_held_key = Some((*key, *modifiers, now_s, now_s, false));
                    }
                    if let Some(act) = key_to_action(*key, *modifiers, *repeat, multi_line, &self.tb_clipboard) {
                        actions.push(act);
                    }
                }
                egui::Event::Key { key, pressed: false, .. } => {
                    if self.tb_held_key.is_some_and(|(hk, ..)| hk == *key) {
                        self.tb_held_key = None;
                    }
                }
                _ => {}
            }
        }

        // Synthetic key repeat for held navigation/delete keys when the platform/harness
        // does not emit OS `repeat: true` events (400ms initial delay, 33ms repeat interval).
        if let Some((hk, hmods, pressed_at, last_rep, saw_os_rep)) = self.tb_held_key {
            if !saw_os_rep && (now_s - pressed_at) >= 0.40 && (now_s - last_rep) >= 0.033 {
                self.tb_held_key = Some((hk, hmods, pressed_at, now_s, false));
                if let Some(act) = key_to_action(hk, hmods, true, multi_line, &self.tb_clipboard) {
                    actions.push(act);
                }
            }
        }

        let mut lost_focus: Option<bool> = None;
        for act in actions {
            let out = state.apply(act);
            if let Some(copied) = out.copied {
                self.tb_clipboard = copied.clone();
                ctx.copy_text(copied);
            }
            if let Some(enter) = out.lost_focus {
                lost_focus = Some(enter);
                break;
            }
            if out.changed_cursor || out.changed_selection || out.changed_text {
                self.tb_cursor = state.cursor;
                self.tb_sel = state.selection_start;
                self.tb_blink = now_s;
                {
                    let mut sh = super::share();
                    sh.tb_cursor.insert(f, state.cursor);
                    sh.tb_sel.insert(f, state.selection_start);
                }
                self.events.push(GuiEvent::TextBoxUpdate {
                    r: f,
                    text: out.changed_text.then(|| state.text.clone()),
                    cursor: out.changed_cursor.then_some(state.cursor),
                    selection: out.changed_selection.then_some(state.selection_start),
                });
            }
        }

        if let Some(enter) = lost_focus {
            // Enter / Esc: the scripts fire FocusLost with the key's InputObject.
            let mut sh = super::share();
            sh.focus_events.push((f, false, enter, Some((0, 0.0, 0.0))));
            sh.focus = None;
            sh.tb_cursor.insert(f, -1);
            sh.tb_sel.insert(f, -1);
            self.focus = None;
            self.tb_cursor = -1;
            self.tb_sel = -1;
            self.tb_selecting = None;
            self.tb_held_key = None;
        }
        true
    }

    /// Keyboard navigation of GuiService.SelectedCoreObject / SelectedObject:
    /// `\` toggles UI navigation mode (when `UiNavigationKeyBindEnabled` is on),
    /// arrows move to the NextSelection* object or the nearest Selectable one
    /// that way; Enter activates it.
    pub fn navigate(&mut self, ctx: &egui::Context, doc: &Document) {
        if self.focus.is_some() || ctx.egui_wants_keyboard_input() {
            return;
        }
        let Some(gs) = doc.find_child(doc.dom.root_ref(), "GuiService") else {
            return;
        };
        let (dir, enter, toggle_nav) = ctx.input(|i| {
            use egui::Key::*;
            let d = if i.key_pressed(ArrowUp) {
                Some((0.0, -1.0, "NextSelectionUp"))
            } else if i.key_pressed(ArrowDown) {
                Some((0.0, 1.0, "NextSelectionDown"))
            } else if i.key_pressed(ArrowLeft) {
                Some((-1.0, 0.0, "NextSelectionLeft"))
            } else if i.key_pressed(ArrowRight) {
                Some((1.0, 0.0, "NextSelectionRight"))
            } else {
                None
            };
            (d, i.key_pressed(Enter), i.key_pressed(Backslash))
        });
        let selected_core = match doc.prop(gs, "SelectedCoreObject") {
            Some(Variant::Ref(r)) if doc.exists(r) => Some(r),
            _ => None,
        };
        let selected_game = match doc.prop(gs, "SelectedObject") {
            Some(Variant::Ref(r)) if doc.exists(r) => Some(r),
            _ => None,
        };
        let is_selectable = |h: &Hit| -> bool {
            if !boolp(doc, h.r, "Interactable", true) {
                return false;
            }
            match doc.prop(h.r, "Selectable") {
                Some(Variant::Bool(s)) => s,
                _ => h.button || h.textbox,
            }
        };
        if toggle_nav {
            let mut sh = super::share();
            if !sh.ui_nav_toggle_disabled {
                let core_menu = sh.core_menu_open;
                if core_menu {
                    let next = !sh.selection_visible;
                    sh.selection_visible = next;
                    drop(sh);
                    if next && selected_core.is_none() {
                        if let Some(h) = self.hits.iter().find(|h| is_selectable(h)) {
                            self.events.push(GuiEvent::SelectCore(Some(h.r)));
                        }
                    }
                } else if selected_game.is_some() {
                    sh.selection_visible = false;
                    drop(sh);
                    self.events.push(GuiEvent::Select(None));
                } else if let Some(h) = self.hits.iter().find(|h| is_selectable(h)) {
                    sh.selection_visible = true;
                    drop(sh);
                    self.events.push(GuiEvent::Select(Some(h.r)));
                }
            }
            return;
        }
        let core_menu = super::share().core_menu_open;
        let (sel, is_core) = if let Some(r) = selected_core.filter(|_| core_menu) {
            (r, true)
        } else if let Some(r) = selected_game {
            (r, false)
        } else if let Some(r) = selected_core.filter(|_| super::share().selection_visible) {
            (r, true)
        } else {
            return;
        };
        if enter
            && (!is_core || super::share().selection_visible)
            && doc
                .class_of(sel)
                .is_some_and(|c| doc::is_a(&c, "GuiButton"))
        {
            self.events
                .push(GuiEvent::Fire(sel, "MouseButton1Click", vec![]));
            self.events.push(GuiEvent::Fire(
                sel,
                "Activated",
                vec![
                    GuiArg::Input {
                        button: 0,
                        began: false,
                        x: 0.0,
                        y: 0.0,
                    },
                    GuiArg::Num(1.0),
                ],
            ));
        }
        let Some((dx, dy, prop)) = dir else { return };
        super::share().selection_visible = true;
        let emit_select = |events: &mut Vec<GuiEvent>, target: Ref| {
            if is_core {
                events.push(GuiEvent::SelectCore(Some(target)));
            } else {
                events.push(GuiEvent::Select(Some(target)));
            }
        };
        if let Some(Variant::Ref(n)) = doc.prop(sel, prop) {
            if doc.exists(n) {
                emit_select(&mut self.events, n);
                return;
            }
        }
        let Some(from) = self.layout.get(&sel).copied() else {
            return;
        };
        let c0 = (
            from.pos[0] + from.size[0] * 0.5,
            from.pos[1] + from.size[1] * 0.5,
        );
        let mut best: Option<(f32, Ref)> = None;
        for h in &self.hits {
            if h.r == sel {
                continue;
            }
            let Some(b) = self.layout.get(&h.r) else {
                continue;
            };
            if !is_selectable(h) {
                continue;
            }
            let c = (
                b.pos[0] + b.size[0] * 0.5 - c0.0,
                b.pos[1] + b.size[1] * 0.5 - c0.1,
            );
            let along = c.0 * dx + c.1 * dy;
            if along <= 1.0 {
                continue;
            }
            let cost = along + (c.0 * dy - c.1 * dx).abs() * 2.0;
            if best.is_none_or(|(bc, _)| cost < bc) {
                best = Some((cost, h.r));
            }
        }
        if let Some((_, r)) = best {
            emit_select(&mut self.events, r);
        }
    }

    /// The topmost GuiObject under a screen point (Studio selects it when
    /// clicked while editing). Empty transparent containers (a full-screen
    /// BackgroundTransparency=1 Frame holding a layout) are clicked through.
    pub fn object_at(&self, p: Pos2) -> Option<Ref> {
        self.hits
            .iter()
            .rev()
            .find(|h| {
                h.painted
                    && point_in_quad(&h.quad, p)
                    && h.clip.is_none_or(|c| point_in_quad(&c, p))
            })
            .map(|h| h.r)
    }

    /// Screen centre of a drawn GuiObject (the self-test clicks there).
    pub fn screen_center(&self, r: Ref) -> Option<Pos2> {
        let h = self.hits.iter().find(|h| h.r == r)?;
        Some((h.quad.iter().fold(egui::Vec2::ZERO, |a, p| a + p.to_vec2()) / 4.0).to_pos2())
    }
}

/// Where a SurfaceGui / BillboardGui is drawn, its canvas size and distance.
/// Whether a SurfaceGui canvas (corners top-left, top-right, bottom-left)
/// can colour a pixel of the view: on screen it must overlap the view and
/// hold a pixel centre. Behind the camera in part counts as yes.
fn surface_in_view(view_proj: &glam::Mat4, rect: Rect, ppp: f32, tl: Vec3, tr: Vec3, bl: Vec3) -> bool {
    let mut s = Rect::NOTHING;
    for p in [tl, tr, bl, tr + bl - tl] {
        let c = *view_proj * p.extend(1.0);
        if c.w <= 1e-4 {
            return true;
        }
        let ndc = c.truncate() / c.w;
        s.extend_with(Pos2::new(
            rect.min.x + (ndc.x + 1.0) * 0.5 * rect.width(),
            rect.min.y + (1.0 - ndc.y) * 0.5 * rect.height(),
        ));
    }
    // (+ a pixel for the tessellator's feathering)
    let s = s.expand(1.0 / ppp).intersect(rect);
    s.is_positive() && contains_pixel_center(s.min.x, s.max.x, ppp)
        && contains_pixel_center(s.min.y, s.max.y, ppp)
}

fn contains_pixel_center(lo: f32, hi: f32, ppp: f32) -> bool {
    let lo = lo * ppp - 0.5;
    let hi = hi * ppp - 0.5;
    hi - lo > 1.0 || hi.floor() >= lo.ceil()
}

fn place_3d(
    d: &Document,
    cam: &Camera,
    view_proj: &glam::Mat4,
    rect: Rect,
    g: Ref,
    model_centers: &mut HashMap<Ref, Option<Vec3>>,
) -> Option<(Target, [f32; 2], f32)> {
    let class = d.class_of(g)?;
    let adornee = match d.prop(g, "Adornee") {
        Some(Variant::Ref(a)) if d.exists(a) => Some(a),
        _ => None,
    };
    let host = adornee.unwrap_or(d.dom.get_by_ref(g)?.parent());
    let host_class = d.class_of(host)?;
    if class == "SurfaceGui" {
        if !doc::is_a(&host_class, "BasePart") {
            return None;
        }
        let spec = SurfaceSpec::read(d, g)?;
        return spec.place(d, cam);
    }
    // BillboardGui: over a part, attachment or model.
    let part_position = |r| match d.prop(r, "CFrame") {
        Some(Variant::CFrame(cf)) => Some(Vec3::new(cf.position.x, cf.position.y, cf.position.z)),
        _ => None,
    };
    let base = if doc::is_a(&host_class, "BasePart") {
        part_position(host)?
    } else if host_class == "Attachment" {
        let pxf = xform_of(d, d.dom.get_by_ref(host)?.parent())?;
        let cf = match d.prop(host, "CFrame") {
            Some(Variant::CFrame(c)) => Xform::from_cframe(&c),
            _ => Xform {
                pos: Vec3::ZERO,
                rot: Mat3::IDENTITY,
            },
        };
        pxf.apply(cf.pos)
    } else if doc::is_a(&host_class, "Model") {
        (*model_centers.entry(host).or_insert_with(|| {
            let (sum, count) = d.dom.descendants_of(host)
                .filter(|i| doc::is_a(&i.class, "BasePart"))
                .filter_map(|i| part_position(i.referent()))
                .fold((Vec3::ZERO, 0u32), |(sum, count), pos| (sum + pos, count + 1));
            (count > 0).then(|| sum / count as f32)
        }))?
    } else {
        return None;
    };
    let world_off = vec3_of(d.prop(g, "StudsOffsetWorldSpace")).unwrap_or(Vec3::ZERO);
    let cam_off = vec3_of(d.prop(g, "StudsOffset")).unwrap_or(Vec3::ZERO);
    let p = base + world_off + cam.right() * cam_off.x + cam.up() * cam_off.y
        - cam.forward() * cam_off.z;
    let dist = (p - cam.pos).length();
    let max = num(d, g, "MaxDistance", 0.0);
    if max > 0.0 && max.is_finite() && dist > max {
        return None;
    }
    let clip = *view_proj * p.extend(1.0);
    if clip.w <= 1e-4 {
        return None;
    }
    let ndc = clip.truncate() / clip.w;
    let c = Pos2::new(
        rect.min.x + (ndc.x + 1.0) * 0.5 * rect.width(),
        rect.min.y + (1.0 - ndc.y) * 0.5 * rect.height(),
    );
    let s = udim2(d, g, "Size");
    // Size: scale in studs, offset in pixels.
    // (by view-space depth, not distance: Studio's 230 px at 10 studs
    // off-axis is depth's 229.76, distance's would be 229.46)
    let depth = (p - cam.pos).dot(cam.forward());
    let px_per_stud =
        rect.height() / (2.0 * depth.max(0.01) * (cam.fov_deg.to_radians() * 0.5).tan());
    // Whole pixels, rounded, as Studio's AbsoluteSize (Studio 2026-11-16, a
    // 4 x 0.75-stud tag: 80x15 at 30 studs, 7.5 → 8, 2.25 → 2, 0.9 → 1;
    // descendants lay out in the rounded box, and sub-pixel motion changes
    // nothing — the test place's 200 NPC name tags re-walked every frame on
    // ~1e-6 stud jitter).
    let canvas = [(s[0] * px_per_stud + s[1]).round(), (s[2] * px_per_stud + s[3]).round()];
    if canvas[0] <= 0.0 || canvas[1] <= 0.0 || canvas[0] > 8192.0 || canvas[1] > 8192.0 {
        return None;
    }
    // SizeOffset shifts the billboard center in fractions of its rendered size
    // (+X right, +Y up in Roblox's screen-space offset convention).
    let size_off = vec2(d, g, "SizeOffset", [0.0, 0.0]);
    let c = Pos2::new(c.x + size_off[0] * canvas[0], c.y - size_off[1] * canvas[1]);
    Some((Target::Billboard(c, p), canvas, dist))
}

/// A SurfaceGui / BillboardGui's light (round 23, linear): Brightness where
/// LightInfluence is 0, the light on its face where it is 1, between in
/// between. The light: Ambient, OutdoorAmbient and the sun on the face
/// (0.5 × Lighting.Brightness × N·L), unshadowed and without local lights
/// (an estimate). The world GUI pass then tone-maps it with the scene.
/// (tools/fidelity gui-depth: Studio shows LightInfluence 0 colours through
/// its tone curve: red 255 → 224, Brightness 2 grey 0.4 → 148.)
fn world_gui_light([li, bright]: [f32; 2], scene_light: impl FnOnce() -> f32) -> f32 {
    if li <= 0.0 {
        return bright;
    }
    bright * (1.0 - li) + scene_light() * li
}

/// A world GUI's LightInfluence (0..1) and Brightness (≥ 0).
fn light_props(d: &Document, g: Ref) -> [f32; 2] {
    [num(d, g, "LightInfluence", 1.0).clamp(0.0, 1.0), num(d, g, "Brightness", 1.0).max(0.0)]
}

/// What a SurfaceGui's placement reads from itself and which part it is on
/// (`place_3d`). Kept from frame to frame (Gui::surface_specs, dropped by
/// the journal): a game's ~970 world SurfaceGuis read ~15 properties each a
/// frame for this. The part's CFrame and Size are read each frame.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct SurfaceSpec {
    enabled: bool,
    host: Ref,
    /// The Adornee as set, and whether it existed (else the parent hosts).
    adornee: Option<(Ref, bool)>,
    face: u32,
    max_distance: f32,
    /// SizingMode PixelsPerStud: the PixelsPerStud; FixedSize: None.
    pixels_per_stud: Option<f32>,
    canvas_size: [f32; 2],
    on_top: bool,
    light_props: [f32; 2],
    z_offset: i64,
}

impl SurfaceSpec {
    /// None: not a SurfaceGui on a part (it draws nowhere).
    fn read(d: &Document, g: Ref) -> Option<SurfaceSpec> {
        if d.class_of(g)? != "SurfaceGui" {
            return None;
        }
        let adornee = match d.prop(g, "Adornee") {
            Some(Variant::Ref(a)) if !a.is_none() => Some((a, d.exists(a))),
            _ => None,
        };
        let host = match adornee {
            Some((a, true)) => a,
            _ => d.dom.get_by_ref(g)?.parent(),
        };
        if !doc::is_a(&d.class_of(host)?, "BasePart") {
            return None;
        }
        let on_top = boolp(d, g, "AlwaysOnTop", false);
        Some(SurfaceSpec {
            enabled: boolp(d, g, "Enabled", true),
            host,
            adornee,
            face: enum_u32(d.prop(g, "Face")).unwrap_or(5),
            max_distance: num(d, g, "MaxDistance", 0.0),
            pixels_per_stud: (enum_u32(d.prop(g, "SizingMode")).unwrap_or(0) == 1)
                .then(|| num(d, g, "PixelsPerStud", 50.0)),
            canvas_size: vec2(d, g, "CanvasSize", [800.0, 600.0]),
            on_top,
            light_props: if on_top { [0.0; 2] } else { light_props(d, g) },
            z_offset: num(d, g, "ZOffset", 0.0) as i64,
        })
    }

    /// Still what `read` would give: the journal drops it on any write to
    /// the GUI or a reparent; an Adornee appearing or going is checked here.
    fn current(&self, d: &Document) -> bool {
        self.adornee.is_none_or(|(a, existed)| d.exists(a) == existed) && d.exists(self.host)
    }

    /// Where it is drawn this frame: its canvas corners on the part's face,
    /// canvas size and distance; None when the face looks away or it is
    /// beyond MaxDistance.
    fn place(&self, d: &Document, cam: &Camera) -> Option<(Target, [f32; 2], f32)> {
        let host = self.host;
        let xf = xform_of(d, host)?;
        let h = vec3_of(d.prop(host, "Size")).unwrap_or(Vec3::ONE) * 0.5;
        // Face (NormalId): Right 0, Top 1, Back 2, Left 3, Bottom 4, Front 5 → (normal, canvas right, canvas up).
        let (n, right, up) = match self.face {
            0 => (Vec3::X, -Vec3::Z, Vec3::Y),
            1 => (Vec3::Y, Vec3::X, -Vec3::Z),
            2 => (Vec3::Z, Vec3::X, Vec3::Y),
            3 => (-Vec3::X, Vec3::Z, Vec3::Y),
            4 => (-Vec3::Y, Vec3::X, Vec3::Z),
            _ => (-Vec3::Z, -Vec3::X, Vec3::Y),
        };
        let (wr, hu) = ((right * h).length(), (up * h).length());
        let centre = n * (n * h).length() + n * mech::GUI_SURFACE_LIFT;
        let mid = xf.apply(centre);
        // Only seen from in front of the face.
        if (xf.rot * n).dot(cam.pos - mid) <= 0.0 {
            return None;
        }
        let max = self.max_distance;
        let dist = (mid - cam.pos).length();
        if max > 0.0 && dist > max {
            return None;
        }
        // SizingMode: FixedSize 0 (CanvasSize), PixelsPerStud 1.
        let canvas = match self.pixels_per_stud {
            Some(pps) => [wr * 2.0 * pps, hu * 2.0 * pps],
            None => self.canvas_size,
        };
        let tl = xf.apply(centre - right * wr + up * hu);
        let tr = xf.apply(centre + right * wr + up * hu);
        let bl = xf.apply(centre - right * wr - up * hu);
        Some((Target::Surface { tl, tr, bl }, canvas, dist))
    }

    /// `place`, from `geo` while the host part's CFrame and Size are the
    /// same bit for bit as when it was worked out (only the camera tests
    /// run again; the same arithmetic, so the same result).
    fn place_kept(&self, d: &Document, cam: &Camera, geo: &mut Option<SurfaceGeo>) -> Option<(Target, [f32; 2], f32)> {
        thread_local! {
            static KEYS: (rbx_dom_weak::Ustr, rbx_dom_weak::Ustr) =
                (rbx_dom_weak::ustr("CFrame"), rbx_dom_weak::ustr("Size"));
        }
        let (kc, ks) = KEYS.with(|k| *k);
        let inst = d.dom.get_by_ref(self.host)?;
        let (Some(Variant::CFrame(cf)), Some(Variant::Vector3(size))) = (inst.properties.get(&kc), inst.properties.get(&ks)) else {
            *geo = None;
            return self.place(d, cam);
        };
        let o = &cf.orientation;
        let key = SurfaceGeoKey {
            cframe: [
                cf.position.x, cf.position.y, cf.position.z,
                o.x.x, o.x.y, o.x.z, o.y.x, o.y.y, o.y.z, o.z.x, o.z.y, o.z.z,
            ].map(f32::to_bits),
            size: [size.x, size.y, size.z].map(f32::to_bits),
            spec: *self,
        };
        if geo.as_ref().is_none_or(|g| g.key != key) {
            *geo = SurfaceGeo::new(self, d, key);
        }
        let g = geo.as_ref()?;
        // Only seen from in front of the face.
        if g.facing.dot(cam.pos - g.mid) <= 0.0 {
            return None;
        }
        let max = self.max_distance;
        let dist = (g.mid - cam.pos).length();
        if max > 0.0 && dist > max {
            return None;
        }
        Some((Target::Surface { tl: g.tl, tr: g.tr, bl: g.bl }, g.canvas, dist))
    }
}

/// A world root's place in the frame's world GUI triangles (`Gui::draw`).
enum WorldPart {
    Batches(Vec<rustudio_render::world_gui::GuiBatch>),
    Job(usize),
}

/// A world SurfaceGui's kept canvas triangles to project this frame.
struct SurfaceJob {
    tl: Vec3,
    tr: Vec3,
    bl: Vec3,
    canvas: [f32; 2],
    light: f32,
    items: Vec<(Rect, Arc<Mesh>)>,
}

#[derive(Default)]
struct SurfaceOut {
    batches: Vec<rustudio_render::world_gui::GuiBatch>,
    n_in: u64,
    n_out: u64,
}

/// What projecting a world SurfaceGui reads from the frame (the same
/// arithmetic `Gui::draw`'s closures use for every other root).
struct SurfaceView {
    view_proj: glam::Mat4,
    view_proj_rz: glam::Mat4,
    cam_pos: Vec3,
    rect: Rect,
    ppp: f32,
}

impl SurfaceView {
    /// A canvas point's world point.
    fn point(&self, j: &SurfaceJob, p: Pos2) -> Vec3 {
        let (u, v) = (p.x / j.canvas[0].max(1.0), p.y / j.canvas[1].max(1.0));
        j.tl + (j.tr - j.tl) * u + (j.bl - j.tl) * v
    }

    /// A world point's screen position and clip-space w.
    fn project_w(&self, world: Vec3) -> Option<(Pos2, f32)> {
        let rect = self.rect;
        let clip = self.view_proj * world.extend(1.0);
        if clip.w <= 1e-4 {
            return None;
        }
        let ndc = clip.truncate() / clip.w;
        Some((
            Pos2::new(
                rect.min.x + (ndc.x + 1.0) * 0.5 * rect.width(),
                rect.min.y + (1.0 - ndc.y) * 0.5 * rect.height(),
            ),
            clip.w,
        ))
    }

    /// A world point's reverse-Z depth and distance from the camera.
    fn depth_at(&self, w: Vec3) -> (f32, f32) {
        let c = self.view_proj_rz * w.extend(1.0);
        let dist = (w - self.cam_pos).length();
        (if c.w <= 1e-6 { 1.0 } else { c.z / c.w }, dist)
    }

    /// Whether canvas bounds `b` can colour a pixel of the view (unknown,
    /// behind the camera, counts as yes).
    fn on_screen(&self, j: &SurfaceJob, b: Rect) -> bool {
        let mut s = Rect::NOTHING;
        for p in [b.left_top(), b.right_top(), b.right_bottom(), b.left_bottom()] {
            match self.project_w(self.point(j, p)) {
                Some((q, _)) => s.extend_with(q),
                None => return true,
            }
        }
        let s = s.intersect(self.rect);
        s.is_positive() && contains_pixel_center(s.min.x, s.max.x, self.ppp)
            && contains_pixel_center(s.min.y, s.max.y, self.ppp)
    }

    /// One SurfaceGui's triangles in the view (split where perspective
    /// would bend them: `subdivide_known`), as world GUI vertices.
    fn project(&self, j: &SurfaceJob) -> SurfaceOut {
        let mut out = SurfaceOut::default();
        let rect = self.rect;
        let mut known: Vec<(Pos2, Vec3)> = Vec::new();
        let mut seen: Vec<Option<(Pos2, Vec3)>> = Vec::new();
        let seen_at = |p: Pos2| -> Option<(Pos2, f32, Vec3)> {
            let w = self.point(j, p);
            let (p, cw) = self.project_w(w)?;
            Some((p, cw, w))
        };
        let width = rect.width().max(1.0);
        let height = rect.height().max(1.0);
        let vertex = |v: &Vertex, p: Pos2, (depth, dist): (f32, f32)| rustudio_render::world_gui::GuiVertex {
            ndc: [
                (p.x - rect.min.x) / width * 2.0 - 1.0,
                1.0 - (p.y - rect.min.y) / height * 2.0,
            ],
            depth,
            uv: [v.uv.x, v.uv.y],
            color: v.color.to_array(),
            light: [j.light; 3],
            dist,
            alpha_mul: 1.0,
            ..Default::default()
        };
        for (bounds, retained) in &j.items {
            if !self.on_screen(j, *bounds) {
                continue;
            }
            out.n_in += retained.indices.len() as u64 / 3;
            known.clear();
            let (mut b, mut wlo, mut whi) = (Rect::NOTHING, f32::INFINITY, 0.0f32);
            let all = retained.vertices.iter().all(|v| {
                let w = self.point(j, v.pos);
                let Some((p, cw)) = self.project_w(w) else { return false };
                known.push((p, w));
                b.extend_with(p);
                wlo = wlo.min(cw);
                whi = whi.max(cw);
                true
            });
            let split = if all {
                subdivide_known(retained.as_ref(), mech::GUI_SURFACE_AFFINE_ERR_PX, rect, seen_at, b, wlo, whi, &mut seen)
            } else {
                // (behind the camera in part: drawn as nothing)
                None
            };
            let mesh = split.as_ref().unwrap_or(retained.as_ref());
            out.n_out += mesh.indices.len() as u64 / 3;
            if mesh.is_empty() || !all {
                continue;
            }
            match &split {
                None => {
                    let verts = mesh.vertices.iter().zip(&known).map(|(v, (p, w))| vertex(v, *p, self.depth_at(*w)));
                    rustudio_render::world_gui::push(&mut out.batches, mesh.texture_id, verts, &mesh.indices);
                }
                // (split: each new vertex as `subdivide_known` projected it;
                // one behind the camera drops the mesh)
                Some(mesh) => {
                    if seen.iter().all(Option::is_some) {
                        let verts = mesh.vertices.iter().zip(&seen).map(|(v, s)| {
                            let (p, w) = s.expect("all seen");
                            vertex(v, p, self.depth_at(w))
                        });
                        rustudio_render::world_gui::push(&mut out.batches, mesh.texture_id, verts, &mesh.indices);
                    }
                }
            }
        }
        out
    }

    /// Every job's triangles, in job order: on this thread, or split into
    /// runs of about equal vertex counts over a few threads when the frame
    /// has many (each job's output is the same either way).
    fn project_all(&self, jobs: &[SurfaceJob]) -> Vec<SurfaceOut> {
        // (RUSTUDIO_GUI_THREAD_VERTS: another share per thread, for tests)
        static PER_THREAD: std::sync::OnceLock<usize> = std::sync::OnceLock::new();
        let per_thread = *PER_THREAD.get_or_init(|| {
            std::env::var("RUSTUDIO_GUI_THREAD_VERTS").ok().and_then(|v| v.parse().ok()).filter(|v| *v > 0).unwrap_or(8192)
        });
        let verts: Vec<usize> = jobs.iter().map(|j| j.items.iter().map(|(_, m)| m.vertices.len()).sum()).collect();
        let total: usize = verts.iter().sum();
        let threads = std::thread::available_parallelism().map_or(1, |n| n.get()).saturating_sub(1).clamp(1, 4);
        let parts = (total / per_thread).clamp(1, threads);
        if parts == 1 || std::env::var_os("RUSTUDIO_GUI_ONE_THREAD").is_some() {
            return jobs.iter().map(|j| self.project(j)).collect();
        }
        // Contiguous runs of jobs, cut where the running vertex count passes
        // each share.
        let mut cuts = vec![0];
        let mut acc = 0;
        for (i, v) in verts.iter().enumerate() {
            acc += v;
            if acc * parts >= total * cuts.len() && cuts.len() < parts && i + 1 < jobs.len() {
                cuts.push(i + 1);
            }
        }
        cuts.push(jobs.len());
        let runs: Vec<&[SurfaceJob]> = cuts.windows(2).map(|w| &jobs[w[0]..w[1]]).collect();
        std::thread::scope(|scope| {
            let handles: Vec<_> = runs[1..]
                .iter()
                .map(|run| scope.spawn(move || run.iter().map(|j| self.project(j)).collect::<Vec<_>>()))
                .collect();
            let mut out: Vec<SurfaceOut> = runs[0].iter().map(|j| self.project(j)).collect();
            for h in handles {
                out.extend(h.join().expect("GUI projection thread"));
            }
            out
        })
    }
}

/// A drawn root's own properties the draw reads (gui.rs `root_props`).
#[derive(Clone, Copy)]
pub(crate) struct RootProps {
    global_z: bool,
    screen_gui: bool,
    clips: bool,
}

#[derive(Clone, Copy, PartialEq)]
struct SurfaceGeoKey {
    cframe: [u32; 12],
    size: [u32; 3],
    spec: SurfaceSpec,
}

/// What `SurfaceSpec::place` works out from the part alone (gui.rs
/// `surface_geo`): the face's outward normal and middle, the canvas
/// corners and size.
#[derive(Clone, Copy)]
pub(crate) struct SurfaceGeo {
    key: SurfaceGeoKey,
    facing: Vec3,
    mid: Vec3,
    tl: Vec3,
    tr: Vec3,
    bl: Vec3,
    canvas: [f32; 2],
}

impl SurfaceGeo {
    /// `SurfaceSpec::place`'s camera-free part, line for line.
    fn new(spec: &SurfaceSpec, d: &Document, key: SurfaceGeoKey) -> Option<SurfaceGeo> {
        let host = spec.host;
        let xf = xform_of(d, host)?;
        let h = vec3_of(d.prop(host, "Size")).unwrap_or(Vec3::ONE) * 0.5;
        let (n, right, up) = match spec.face {
            0 => (Vec3::X, -Vec3::Z, Vec3::Y),
            1 => (Vec3::Y, Vec3::X, -Vec3::Z),
            2 => (Vec3::Z, Vec3::X, Vec3::Y),
            3 => (-Vec3::X, Vec3::Z, Vec3::Y),
            4 => (-Vec3::Y, Vec3::X, Vec3::Z),
            _ => (-Vec3::Z, -Vec3::X, Vec3::Y),
        };
        let (wr, hu) = ((right * h).length(), (up * h).length());
        let centre = n * (n * h).length() + n * mech::GUI_SURFACE_LIFT;
        let mid = xf.apply(centre);
        let canvas = match spec.pixels_per_stud {
            Some(pps) => [wr * 2.0 * pps, hu * 2.0 * pps],
            None => spec.canvas_size,
        };
        Some(SurfaceGeo {
            key,
            facing: xf.rot * n,
            mid,
            tl: xf.apply(centre - right * wr + up * hu),
            tr: xf.apply(centre + right * wr + up * hu),
            bl: xf.apply(centre - right * wr - up * hu),
            canvas,
        })
    }
}

/// The light `world_gui_light` mixes in: Ambient, OutdoorAmbient and the sun
/// on a face with this normal, or, with no normal, the whole sun (Studio's
/// particle and trail shaders light without N·L, round 25). The Lighting
/// terms are read once for a frame's world GUIs.
#[derive(Clone, Copy)]
struct SceneLight {
    ambient: f32,
    /// The sun's irradiance (0 below the horizon) and its direction.
    sun: Option<(f32, Vec3)>,
}

impl SceneLight {
    fn of(d: &Document) -> SceneLight {
        let root = d.dom.root_ref();
        let lighting = d.find_child(root, "Lighting");
        let get = |k: &str| lighting.and_then(|l| d.prop(l, k));
        let rgb = |k: &str, dflt: [u8; 3]| -> [u8; 3] {
            match get(k) {
                Some(Variant::Color3(c)) => [c.r, c.g, c.b].map(|x| (x.clamp(0.0, 1.0) * 255.0) as u8),
                Some(Variant::Color3uint8(c)) => [c.r, c.g, c.b],
                _ => dflt,
            }
        };
        let luma = |v: Vec3| v.x * 0.299 + v.y * 0.587 + v.z * 0.114;
        let amb = rustudio_render::lighting::sq(rgb("Ambient", mech::AMBIENT));
        let out = rustudio_render::lighting::sq(rgb("OutdoorAmbient", mech::OUTDOOR_AMBIENT));
        let ambient = luma(amb) + (luma(out) - luma(amb)).max(0.0);
        let clock = rustudio_render::lighting::clock_hours(get("ClockTime"), get("TimeOfDay"));
        let lat = f32_of(get("GeographicLatitude")).unwrap_or(mech::GEOGRAPHIC_LATITUDE);
        let sun = rustudio_render::lighting::sun_direction(clock, lat);
        let brightness = f32_of(get("Brightness")).unwrap_or(mech::LIGHT_BRIGHTNESS);
        SceneLight {
            ambient,
            sun: (sun.y > 0.0).then(|| (mech::SUN_IRRADIANCE_PER_BRIGHTNESS * brightness, sun)),
        }
    }

    fn on(&self, normal: Option<Vec3>) -> f32 {
        let sun_light = match self.sun {
            Some((irradiance, sun)) => irradiance * normal.map_or(1.0, |n| n.dot(sun).max(0.0)),
            None => 0.0,
        };
        (self.ambient + sun_light).max(0.0)
    }
}


/// The Scale of a UIScale directly under a LayerCollector (1 without one).
fn root_ui_scale(d: &Document, g: Ref) -> f32 {
    child_of_class(d, g, "UIScale").map_or(1.0, |s| num(d, s, "Scale", 1.0))
}


#[cfg(test)]
mod surface_spec_tests {
    use super::*;
    use rbx_dom_weak::types::{CFrame, Matrix3, Vector3};
    use rbx_dom_weak::{ustr, InstanceBuilder};

    #[test]
    fn surface_specs_follow_the_journal_and_place_like_before() {
        let mut d = Document::empty();
        let root = d.dom.root_ref();
        let ws = d.insert(root, InstanceBuilder::new("Workspace"));
        let at = |x: f32| CFrame::new(Vector3::new(x, 0.0, 0.0), Matrix3::identity());
        let part = d.insert(ws, InstanceBuilder::new("Part")
            .with_property("Size", Vector3::new(4.0, 2.0, 1.0))
            .with_property("CFrame", at(0.0)));
        let other = d.insert(ws, InstanceBuilder::new("Part")
            .with_property("Size", Vector3::new(8.0, 2.0, 1.0))
            .with_property("CFrame", at(20.0)));
        let sg = d.insert(part, InstanceBuilder::new("SurfaceGui"));
        let mut gui = Gui::default();
        gui.sync_roots(&mut d);
        assert!(gui.world_roots.contains(&sg));
        let mut cam = Camera::default();
        // In front of the Front face (−Z).
        cam.pos = Vec3::new(0.0, 0.0, -10.0);
        let spec = SurfaceSpec::read(&d, sg).unwrap();
        let Some((Target::Surface { tl, tr, bl }, canvas, dist)) = spec.place(&d, &cam) else { panic!() };
        assert_eq!(canvas, [800.0, 600.0]);
        assert!((dist - (10.0 - 0.5 - mech::GUI_SURFACE_LIFT)).abs() < 1e-4);
        assert!((tl - Vec3::new(2.0, 1.0, -0.5 - mech::GUI_SURFACE_LIFT)).length() < 1e-4, "{tl}");
        assert!((tr - Vec3::new(-2.0, 1.0, -0.5 - mech::GUI_SURFACE_LIFT)).length() < 1e-4, "{tr}");
        assert!((bl - Vec3::new(2.0, -1.0, -0.5 - mech::GUI_SURFACE_LIFT)).length() < 1e-4, "{bl}");
        // From behind the face it is not drawn.
        cam.pos = Vec3::new(0.0, 0.0, 10.0);
        assert!(spec.place(&d, &cam).is_none());
        cam.pos = Vec3::new(0.0, 0.0, -10.0);

        // A property write drops it; so does moving it to another part.
        gui.surface_specs.insert(sg, spec);
        d.set_prop_live(sg, ustr("PixelsPerStud"), Variant::Float32(10.0));
        gui.sync_roots(&mut d);
        assert!(!gui.surface_specs.contains_key(&sg));
        gui.surface_specs.insert(sg, spec);
        // The part's own CFrame and Size are read each frame.
        d.set_prop_live(part, ustr("CFrame"), Variant::CFrame(at(1.0)));
        gui.sync_roots(&mut d);
        assert_eq!(gui.surface_specs.get(&sg), Some(&spec));
        let moved = spec.place(&d, &cam).unwrap();
        assert!(matches!(moved.0, Target::Surface { tl, .. } if (tl.x - 3.0).abs() < 1e-4));
        // `place_kept` gives `place`'s result bit for bit, its kept corners
        // following the part's CFrame and Size.
        let bits = |p: Option<(Target, [f32; 2], f32)>| p.map(|(t, c, dist)| {
            let Target::Surface { tl, tr, bl } = t else { panic!() };
            ([tl, tr, bl].map(|v| v.to_array().map(f32::to_bits)), c.map(f32::to_bits), dist.to_bits())
        });
        let mut geo = None;
        let turned = Xform { pos: Vec3::new(1.5, -2.25, 0.3), rot: Mat3::from_euler(glam::EulerRot::XYZ, 0.3, -1.1, 0.7) }.to_cframe();
        for (cf, size, eye) in [
            (at(1.0), Vector3::new(4.0, 2.0, 1.0), Vec3::new(0.0, 0.0, -10.0)),
            (at(1.0), Vector3::new(4.0, 2.0, 1.0), Vec3::new(0.3, 1.7, -7.1)),
            (at(1.0), Vector3::new(4.0, 2.0, 1.0), Vec3::new(0.0, 0.0, 10.0)),
            (turned, Vector3::new(4.0, 2.0, 1.0), Vec3::new(0.3, 1.7, -7.1)),
            (turned, Vector3::new(3.0, 5.5, 0.2), Vec3::new(-6.0, 2.0, -3.0)),
            (turned, Vector3::new(3.0, 5.5, 0.2), Vec3::new(6.0, -2.0, 3.0)),
        ] {
            d.set_prop_live(part, ustr("CFrame"), Variant::CFrame(cf));
            d.set_prop_live(part, ustr("Size"), Variant::Vector3(size));
            cam.pos = eye;
            assert_eq!(bits(spec.place_kept(&d, &cam, &mut geo)), bits(spec.place(&d, &cam)), "{eye}");
        }
        cam.pos = Vec3::new(0.0, 0.0, -10.0);
        d.set_prop_live(part, ustr("CFrame"), Variant::CFrame(at(1.0)));
        d.set_prop_live(part, ustr("Size"), Variant::Vector3(Vector3::new(4.0, 2.0, 1.0)));
        d.reparent(&[sg], other);
        gui.sync_roots(&mut d);
        assert!(!gui.surface_specs.contains_key(&sg));
        let spec = SurfaceSpec::read(&d, sg).unwrap();
        assert_eq!(spec.host, other);
        // An Adornee that goes away hands the GUI back to its parent.
        gui.surface_specs.insert(sg, spec);
        d.set_prop_live(sg, ustr("Adornee"), Variant::Ref(part));
        gui.sync_roots(&mut d);
        let spec = SurfaceSpec::read(&d, sg).unwrap();
        assert_eq!(spec.host, part);
        assert!(spec.current(&d));
        d.dom.destroy(part);
        assert!(!spec.current(&d));
        assert_eq!(SurfaceSpec::read(&d, sg).map(|s| s.host), Some(other));
    }
}

#[cfg(test)]
mod surface_job_tests {
    use super::*;

    /// Splitting the frame's SurfaceGui jobs over threads returns, job for
    /// job and bit for bit, what projecting them in order on one thread does.
    #[test]
    fn threaded_surface_projection_matches_one_thread() {
        let mut cam = Camera::default();
        cam.pos = Vec3::new(3.0, 2.0, -12.0);
        let rect = Rect::from_min_size(Pos2::new(10.0, 20.0), egui::vec2(1280.0, 720.0));
        let view = SurfaceView {
            view_proj: cam.view_proj(rect),
            view_proj_rz: cam.view_proj_reverse_z(rect),
            cam_pos: cam.pos,
            rect,
            ppp: 1.0,
        };
        let mut seed = 7u32;
        let mut rnd = move || {
            seed = seed.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
            (seed >> 8) as f32 / (1u32 << 24) as f32
        };
        let jobs: Vec<SurfaceJob> = (0..60)
            .map(|k| {
                let o = cam.pos + cam.forward() * (6.0 + rnd() * 30.0) + cam.right() * (rnd() * 30.0 - 15.0) + cam.up() * (rnd() * 10.0 - 5.0);
                let items = (0..1 + k % 5)
                    .map(|_| {
                        let mut m = Mesh::default();
                        for q in 0..400 {
                            let (x, y) = (rnd() * 760.0, rnd() * 560.0);
                            let e = if q % 50 == 0 { 400.0 } else { 30.0 };
                            let base = m.vertices.len() as u32;
                            for (dx, dy) in [(0.0, 0.0), (e, 0.0), (e, e), (0.0, e)] {
                                m.vertices.push(Vertex {
                                    pos: Pos2::new(x + dx, y + dy),
                                    uv: Pos2::new(rnd(), rnd()),
                                    color: Color32::from_rgba_premultiplied(q as u8, 200, 30, 255),
                                });
                            }
                            m.indices.extend([base, base + 1, base + 2, base, base + 2, base + 3]);
                        }
                        (Rect::from_min_size(Pos2::ZERO, egui::vec2(800.0, 600.0)), Arc::new(m))
                    })
                    .collect();
                SurfaceJob {
                    tl: o,
                    tr: o + cam.right() * 8.0 + cam.forward() * (6.0 * rnd() - 3.0),
                    bl: o - cam.up() * 6.0 + cam.forward() * (30.0 * rnd()),
                    canvas: [800.0, 600.0],
                    light: 0.5 + rnd(),
                    items,
                }
            })
            .collect();
        let one: Vec<SurfaceOut> = jobs.iter().map(|j| view.project(j)).collect();
        let many = view.project_all(&jobs);
        assert_eq!(one.len(), many.len());
        let bytes = |o: &SurfaceOut| {
            o.batches
                .iter()
                .map(|b| (b.texture, format!("{:?}", b.vertices), b.indices.clone()))
                .collect::<Vec<_>>()
        };
        let mut drawn = 0;
        for (a, b) in one.iter().zip(&many) {
            assert_eq!((a.n_in, a.n_out), (b.n_in, b.n_out));
            assert_eq!(bytes(a), bytes(b));
            drawn += a.batches.iter().map(|b| b.indices.len()).sum::<usize>();
        }
        assert!(drawn > 100_000, "{drawn}");
        assert!(one.iter().any(|o| o.n_out > o.n_in), "some meshes split");
    }
}

#[cfg(test)]
mod billboard_canvas_tests {
    use super::*;

    #[test]
    fn archived_canvas_only_drops_for_a_relevant_property_mark() {
        let d = Document::empty();
        let root = Ref::new();
        let other_root = Ref::new();
        let label = Ref::new();
        let mut gui = Gui {
            roots_doc: d.id,
            ..Gui::default()
        };
        let output = || RootCache {
            key: [24; 13],
            shapes: Vec::new(),
            hits: Vec::new(),
            ui2d: (0, 0),
            vfs: Vec::new(),
            citems: Vec::new(),
            lhits: Vec::new(),
        };
        gui.cache.insert(root, output());
        gui.root_boxes.insert(root, Vec::new());
        gui.cache_owned.insert(root, (vec![(label, label)], vec![root]));
        gui.reuse_billboard_canvas(root, [36; 13]);
        gui.cache.insert(other_root, output());
        gui.root_boxes.insert(other_root, Vec::new());
        gui.cache_owned.insert(other_root, (vec![(other_root, other_root)], vec![other_root]));
        gui.reuse_billboard_canvas(other_root, [36; 13]);

        gui.cache_marks(&d, &doc::Marks::Some(vec![
            doc::Mark::Prop(Ref::new(), rbx_dom_weak::ustr("CFrame")),
        ]));
        assert!(gui.billboard_canvases.contains_key(&root));

        gui.cache_marks(&d, &doc::Marks::Some(vec![
            doc::Mark::Prop(label, rbx_dom_weak::ustr("Text")),
        ]));
        assert!(!gui.billboard_canvases.contains_key(&root));
        assert!(!gui.billboard_archive_owner.contains_key(&label));
        assert!(gui.billboard_canvases.contains_key(&other_root));
    }

    #[test]
    fn returning_to_a_canvas_restores_the_layout_and_dependencies() {
        let root = Ref::new();
        let child = Ref::new();
        let make_output = |key| RootCache {
            key, shapes: Vec::new(), hits: Vec::new(), ui2d: (0, 0),
            vfs: Vec::new(), citems: Vec::new(), lhits: Vec::new(),
        };
        let mut gui = Gui::default();
        let small_key = [24; 13];
        let large_key = [36; 13];
        let small = GuiBox { size: [24.0, 24.0], ..GuiBox::default() };
        let large = GuiBox { size: [36.0, 36.0], ..GuiBox::default() };
        gui.cache.insert(root, make_output(small_key));
        gui.root_boxes.insert(root, vec![(child, small)]);
        gui.cache_owned.insert(root, (vec![(child, child)], vec![root]));
        gui.reuse_billboard_canvas(root, large_key);

        gui.cache.insert(root, make_output(large_key));
        gui.layout.insert(child, large);
        gui.layout_from.insert(root, vec![child]);
        gui.root_boxes.insert(root, vec![(child, large)]);
        gui.reuse_billboard_canvas(root, small_key);

        assert_eq!(gui.cache[&root].key, small_key);
        assert_eq!(gui.layout[&child], small);
        assert_eq!(gui.cache_owner[&child].0, root);
    }
}
