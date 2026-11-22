//! Laying out and drawing one GUI's objects into canvas-space shapes.

use egui::epaint::TextShape;
use egui::text::{Fonts, LayoutJob, TextFormat};
use egui::{FontId, Shape, Stroke};
use std::time::{Duration, Instant};

use super::*;
use rustudio_core::mech;


/// Measures TextScaled against a canvas without laying out the same
/// single-line text again at every projected BillboardGui width.
#[derive(Default)]
pub(super) struct TextFitCache {
    sizes: TextMemo<(LayoutJob, FitSearch), 4096>,
    wrapped: TextMemo<(LayoutJob, egui::Vec2), 4096>,
    unwrapped: rustc_hash::FxHashMap<u64, UnwrappedProbes>,
    probe_fonts: Option<Fonts>,
    /// The probe fonts being made on another thread (live runs: parsing every
    /// face is tens of millions of instructions, after each font change).
    probe_building: Option<std::sync::mpsc::Receiver<Fonts>>,
    probe_frame: Option<u64>,
    galleys: TextMemo<Arc<egui::Galley>, 1024>,
    frame: Option<u64>,
    // Reserve time for HUD text even when world signs fill their budget.
    spent: [Duration; 2],
    screen: bool,
    budgeted: bool,
    deferred: u64,
}

#[derive(Clone, Copy)]
enum FitSearch {
    Pending { lo: f32, hi: f32, step: u8 },
    Ready(f32),
}

struct UnwrappedProbes {
    style: LayoutJob,
    sizes: rustc_hash::FxHashMap<u32, (egui::Vec2, f32)>,
}

// Replace a single entry on collision to bound cache memory and avoid
// recurring full-cache clears during a camera sweep through new canvas sizes.
struct TextMemo<T, const N: usize>(Vec<Option<(u64, T)>>);

impl<T, const N: usize> Default for TextMemo<T, N> {
    fn default() -> Self {
        Self(Vec::new())
    }
}

impl<T, const N: usize> TextMemo<T, N> {
    fn get(&self, key: u64) -> Option<&T> {
        let (stored, value) = self.0.get(key as usize % N)?.as_ref()?;
        (*stored == key).then_some(value)
    }

    fn insert(&mut self, key: u64, value: T) {
        if self.0.is_empty() {
            self.0.resize_with(N, || None);
        }
        self.0[key as usize % N] = Some((key, value));
    }
}

fn text_job_hash(job: &LayoutJob, extra: impl std::hash::Hash) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut h = rustc_hash::FxHasher::default();
    job.hash(&mut h);
    extra.hash(&mut h);
    h.finish()
}

fn single_line_ascii(text: &str) -> bool {
    text.bytes().all(|byte| byte == b' ' || byte.is_ascii_graphic())
}

/// Trial text sizes do not use the displayed atlas. Reuse the identical
/// Roblox fallback faces so an atlas reset only parses each file once.
fn probe_font_definitions(mut definitions: egui::FontDefinitions) -> egui::FontDefinitions {
    for names in definitions.families.values_mut() {
        for name in names {
            if !name.starts_with("rbx:") {
                continue;
            }
            let Some((_, index)) = name.rsplit_once("+fallback") else { continue };
            let shared = format!("rbx:probe-fallback-{index}");
            if let Some(data) = definitions.font_data.remove(name) {
                definitions.font_data.entry(shared.clone()).or_insert(data);
            }
            *name = shared;
        }
    }
    definitions
}

impl TextFitCache {
    pub(super) fn begin_frame(&mut self, ctx: &egui::Context, budgeted: bool) {
        let frame = ctx.cumulative_frame_nr();
        if self.frame == Some(frame) {
            return;
        }
        if let Some(fonts) = &self.probe_fonts {
            let options = ctx.fonts(|fonts| fonts.options().clone());
            if fonts.options() != &options {
                *self = Self::default();
            }
        }
        self.frame = Some(frame);
        self.spent = [Duration::ZERO; 2];
        self.budgeted = budgeted;
    }

    pub(super) fn set_screen(&mut self, screen: bool) {
        self.screen = screen;
    }

    pub(super) fn take_deferred(&mut self) -> u64 {
        std::mem::take(&mut self.deferred)
    }

    fn can_start_work(&mut self) -> bool {
        let limit = Duration::from_millis(if self.screen { 2 } else { 4 });
        if self.budgeted && self.spent[usize::from(self.screen)] >= limit {
            self.deferred += 1;
            return false;
        }
        true
    }

    fn finish_work(&mut self, start: Instant) {
        self.spent[usize::from(self.screen)] += start.elapsed();
    }

    fn probe(&mut self, ctx: &egui::Context, mut job: LayoutJob) -> Option<Arc<egui::Galley>> {
        sane_font_sizes(&mut job);
        let start = self.can_start_work().then(Instant::now)?;
        // TextScaled tries fractional sizes that never get painted. Keep
        // their glyphs away from the uploaded atlas and its world-GUI mips.
        if self.probe_fonts.is_none() {
            let make = |options, definitions| {
                let mut probe = Fonts::new(options, probe_font_definitions(definitions));
                // Only their sizes are read: no glyphs are rasterized.
                probe.set_measure_only(true);
                probe
            };
            if let Some(rx) = &self.probe_building {
                match rx.try_recv() {
                    Ok(fonts) => self.probe_fonts = Some(fonts),
                    Err(std::sync::mpsc::TryRecvError::Disconnected) => self.probe_building = None,
                    Err(std::sync::mpsc::TryRecvError::Empty) if self.budgeted => {
                        self.deferred += 1;
                        return None;
                    }
                    Err(std::sync::mpsc::TryRecvError::Empty) => {
                        let _p = rustudio_core::bench::scope("gui_text_probe_wait");
                        self.probe_fonts = rx.recv().ok();
                    }
                }
                if self.probe_fonts.is_some() {
                    self.probe_building = None;
                }
            } else if self.budgeted {
                // (made off the main thread; fitting waits for it like text
                // over the frame's budget does)
                let (options, definitions) = ctx.fonts(|f| (f.options().clone(), f.definitions().clone()));
                let (tx, rx) = std::sync::mpsc::channel();
                let repaint = ctx.clone();
                let spawned = std::thread::Builder::new().name("gui-probe-fonts".into()).spawn(move || {
                    let _ = tx.send(make(options, definitions));
                    repaint.request_repaint();
                });
                if spawned.is_ok() {
                    self.probe_building = Some(rx);
                    self.deferred += 1;
                    return None;
                }
            }
            if self.probe_fonts.is_none() {
                let _p = rustudio_core::bench::scope("gui_text_probe_init");
                self.probe_fonts = Some(ctx.fonts(|f| make(f.options().clone(), f.definitions().clone())));
            }
        }
        let fonts = self.probe_fonts.as_mut().expect("probe fonts");
        if self.probe_frame != self.frame || fonts.font_atlas_fill_ratio() > 0.8 {
            let _p = rustudio_core::bench::scope("gui_text_probe_pass");
            fonts.begin_pass(fonts.options().clone());
            self.probe_frame = self.frame;
        }
        let galley = fonts.with_pixels_per_point(ctx.pixels_per_point()).layout_job(job);
        self.finish_work(start);
        Some(galley)
    }

    fn layout(&mut self, ctx: &egui::Context, mut job: LayoutJob) -> Option<Arc<egui::Galley>> {
        // epaint normalizes the wrapping width before it stores the layout.
        job.wrap.max_width = job.wrap.max_width.round();
        sane_font_sizes(&mut job);
        let key = text_job_hash(&job, ());
        if let Some(galley) = self.galleys.get(key).filter(|g| *g.job == job) {
            return Some(galley.clone());
        }
        let start = self.can_start_work().then(Instant::now)?;
        let cache = job.text.len() <= 256;
        let galley = ctx.fonts_mut(|fonts| fonts.layout_job(job));
        self.finish_work(start);
        if cache {
            self.galleys.insert(key, galley.clone());
        }
        Some(galley)
    }

    /// A layout that is never deferred: AutomaticSize and other
    /// script-visible geometry (AbsoluteSize, TextBounds) can't wait for a
    /// later frame, or a ProximityPrompt's AutomaticSize X labels stay 0 wide.
    fn layout_now(&mut self, ctx: &egui::Context, job: LayoutJob) -> Arc<egui::Galley> {
        let budgeted = std::mem::replace(&mut self.budgeted, false);
        let galley = self.layout(ctx, job);
        self.budgeted = budgeted;
        galley.expect("an unbudgeted layout always runs")
    }

    pub(super) fn clear_galleys(&mut self) {
        self.galleys = Default::default();
    }

    fn size(
        &mut self,
        ctx: &egui::Context,
        bounds: egui::Vec2,
        max: f32,
        simple: bool,
        build: impl Fn(f32, bool) -> LayoutJob,
    ) -> Option<f32> {
        let _p = rustudio_core::bench::scope("gui_text_fit");
        let style = build(1.0, !simple);
        let key = text_job_hash(&style, (bounds.x.to_bits(), bounds.y.to_bits(), max.to_bits()));
        let (mut lo, mut hi, mut step) = match self.sizes.get(key).filter(|(job, _)| *job == style) {
            Some((_, FitSearch::Ready(size))) => return Some(*size),
            Some((_, FitSearch::Pending { lo, hi, step })) => (*lo, *hi, *step),
            None => (1.0, max.max(1.0), 0),
        };
        if !self.can_start_work() {
            return None;
        }

        let style_key = text_job_hash(&style, ());
        let mut probes = if simple {
            self.unwrapped.remove(&style_key).filter(|entry| entry.style == style)
                .map(|entry| entry.sizes).unwrap_or_default()
        } else {
            Default::default()
        };
        // Plain text has a uniform explicit line height. Reject sizes
        // that cannot fit even one row, allowing for physical-pixel rounding.
        let line_height = if simple && !style.text.is_empty() {
            style.sections.first().and_then(|s| s.format.line_height)
                .filter(|height| height.is_finite() && *height > 0.0)
        } else {
            None
        };
        let height_limit = bounds.y + 0.5 + ctx.pixels_per_point().recip();
        // Resume this search at the first unmeasured size next frame.
        // A probe is the smallest unit of atlas work.
        let mut search = || {
            while step < 10 && lo.floor() != hi.floor() {
                let mid = (lo + hi) * 0.5;
                if line_height.is_some_and(|height| mid * height > height_limit) {
                    hi = mid;
                    step += 1;
                    continue;
                }
                let measured = if simple {
                    let (single, advance) = match probes.entry(mid.to_bits()) {
                        std::collections::hash_map::Entry::Occupied(entry) => *entry.get(),
                        std::collections::hash_map::Entry::Vacant(entry) => {
                            let probe = self.probe(ctx, build(mid, false))?;
                            *entry.insert((probe.size(), probe.intrinsic_size().x))
                        }
                    };
                    // epaint skips the wrapping loop when the paragraph advance
                    // fits the rounded wrap width. No wrap fits a shorter height
                    // than the single row for this uniform text.
                    (advance <= bounds.x.max(1.0).round() || single.y > bounds.y + 0.5).then_some(single)
                } else {
                    None
                };
                let measured = if let Some(measured) = measured {
                    measured
                } else {
                    let mut job = build(mid, true);
                    job.wrap.max_width = job.wrap.max_width.round();
                    let key = text_job_hash(&job, ());
                    if let Some((_, measure)) = self.wrapped.get(key).filter(|(cached, _)| *cached == job) {
                        *measure
                    } else {
                        let _p = rustudio_core::bench::scope("gui_text_fit_wrapped");
                        let galley = self.probe(ctx, job)?;
                        let measure = galley.size();
                        self.wrapped.insert(key, ((*galley.job).clone(), measure));
                        measure
                    }
                };
                if measured.x <= bounds.x + 0.5 && measured.y <= bounds.y + 0.5 {
                    lo = mid;
                } else {
                    hi = mid;
                }
                step += 1;
            }
            Some(lo.floor().max(1.0))
        };
        let size = search();
        if simple {
            if self.unwrapped.len() >= 256 || probes.len() >= 1024 {
                self.unwrapped.clear();
            } else {
                self.unwrapped.insert(style_key, UnwrappedProbes { style: style.clone(), sizes: probes });
            }
        }
        self.sizes.insert(key, (style, size.map_or(FitSearch::Pending { lo, hi, step }, FitSearch::Ready)));
        size
    }
}

/// `rect` inside the UIPadding of `r` (a GuiObject or a LayerCollector:
/// Studio pads a ScreenGui's children too, e.g. Laser Tag's 20 px HUD margin).
pub(super) fn padded(d: &Document, r: Ref, rect: Rect, scale: f32) -> Rect {
    let Some(p) = child_of_class(d, r, "UIPadding") else {
        return rect;
    };
    let l = udim(d, p, "PaddingLeft");
    let rr = udim(d, p, "PaddingRight");
    let t = udim(d, p, "PaddingTop");
    let b = udim(d, p, "PaddingBottom");
    Rect::from_min_max(
        Pos2::new(
            rect.left() + l[0] * rect.width() + l[1] * scale,
            rect.top() + t[0] * rect.height() + t[1] * scale,
        ),
        Pos2::new(
            rect.right() - rr[0] * rect.width() - rr[1] * scale,
            rect.bottom() - b[0] * rect.height() - b[1] * scale,
        ),
    )
}

/// 2D affine map (turns of rotated GuiObjects).
#[derive(Clone, Copy, Debug)]
pub(super) struct Aff {
    m: [f32; 4],
    t: [f32; 2],
}

impl Aff {
    pub(super) const ID: Aff = Aff {
        m: [1.0, 0.0, 0.0, 1.0],
        t: [0.0, 0.0],
    };
    pub(super) fn apply(&self, p: Pos2) -> Pos2 {
        Pos2::new(
            self.m[0] * p.x + self.m[1] * p.y + self.t[0],
            self.m[2] * p.x + self.m[3] * p.y + self.t[1],
        )
    }
    /// `self` first, then `o`.
    fn then(&self, o: &Aff) -> Aff {
        let m = [
            o.m[0] * self.m[0] + o.m[1] * self.m[2],
            o.m[0] * self.m[1] + o.m[1] * self.m[3],
            o.m[2] * self.m[0] + o.m[3] * self.m[2],
            o.m[2] * self.m[1] + o.m[3] * self.m[3],
        ];
        let t = o.apply(Pos2::new(self.t[0], self.t[1]));
        Aff { m, t: [t.x, t.y] }
    }
    fn rotate_about(c: Pos2, deg: f32) -> Aff {
        let (s, co) = deg.to_radians().sin_cos();
        Aff {
            m: [co, -s, s, co],
            t: [c.x - co * c.x + s * c.y, c.y - s * c.x - co * c.y],
        }
    }
    pub(super) fn same(&self, o: &Aff) -> bool {
        self.m == o.m && self.t == o.t
    }
    pub(super) fn is_id(&self) -> bool {
        self.m == [1.0, 0.0, 0.0, 1.0] && self.t == [0.0, 0.0]
    }
}

/// One shape in canvas space with its clip, turn and draw order.
#[derive(Clone)]
pub(super) struct Item {
    pub shape: Shape,
    pub clip: Rect,
    pub aff: Aff,
    pub z: i32,
    pub seq: usize,
    /// The object that drew it.
    pub by: Ref,
    /// Its canvas bounds and triangles (turned, clipped) once tessellated;
    /// shared with the memo it is replayed from, so a kept item is
    /// tessellated once (None: draws nothing).
    pub tess: Arc<std::sync::OnceLock<Option<(Rect, Arc<Mesh>)>>>,
}

/// A clickable / hoverable object found while drawing (canvas space).
#[derive(Clone)]
pub(super) struct LocalHit {
    pub r: Ref,
    pub rect: Rect,
    pub aff: Aff,
    pub clip: Option<Rect>,
    pub button: bool,
    pub textbox: bool,
    pub sink: bool,
    pub scrolling: bool,
    /// Draggable, or holds an enabled UIDragDetector.
    pub drag: bool,
    /// Drew something of its own (background, image, text, stroke…); an
    /// empty transparent container isn't picked by edit-mode clicks.
    pub painted: bool,
}

/// A drawn GuiObject's output from its last walk, in order: runs of its own
/// pieces (shapes, hit boxes, layout boxes, what it read) and its children,
/// each kept in its own `Memo`. Valid while the walk would reach it with the
/// same inputs (`key`) and no journal mark touched it or anything under it
/// (gui_input.rs `memo_up` drops it and every ancestor's, like Studio's
/// LayoutDirty / ViewportDirty bits going up the LayoutNode tree).
pub(super) struct Memo {
    key: [u32; 28],
    pub(super) segs: Vec<Seg>,
}

pub(super) enum Seg {
    Own {
        items: Vec<Item>,
        hits: Vec<LocalHit>,
        boxes: Vec<(Ref, GuiBox)>,
        reads: Vec<(Ref, Ref)>,
        kids: Vec<Ref>,
    },
    Child { r: Ref, parent: Rect, laid: Option<Rect>, dn: Down },
}

/// A memo being written: where its current run of own pieces starts.
pub(super) struct Rec {
    start: [usize; 5],
    segs: Vec<Seg>,
    no_cache: Option<&'static str>,
    vfs: usize,
}

/// Laying out one GUI root.
pub(super) struct Walk<'a, 'b> {
    pub gui: &'a mut Gui,
    pub ctx: &'a egui::Context,
    pub input: &'a mut DrawIn<'b>,
    pub items: Vec<Item>,
    pub hits: Vec<LocalHit>,
    pub seq: usize,
    /// Every layout written, to keep for frames this GUI isn't walked.
    pub boxes: Vec<(Ref, GuiBox)>,
    pub global_z: bool,
    /// Added to canvas positions for AbsolutePosition (ScreenGui inset).
    pub abs_offset: [f32; 2],
    /// A ScreenGui's canvas origin on screen (UIBlur samples the view there).
    pub screen: Option<Pos2>,
    /// The object being walked (none at the root): who reads what.
    pub cur: Ref,
    /// Memos being written, innermost last; and replay depth.
    pub rec: Vec<Rec>,
    pub replaying: u32,
}

/// Inherited while walking down the tree.
#[derive(Clone, Copy)]
pub(super) struct Down {
    pub aff: Aff,
    pub clip: Rect,
    pub alpha: f32,
    pub tint: [f32; 3],
    /// UIScale so far (offsets, text sizes and strokes grow with it).
    pub scale: f32,
    pub z: i32,
    /// The ancestors' Rotation summed (AbsoluteRotation adds its own).
    pub turn: f32,
}

impl Walk<'_, '_> {
    fn push(&mut self, shape: Shape, dn: &Down) {
        self.seq += 1;
        self.items.push(Item {
            shape,
            clip: dn.clip,
            aff: dn.aff,
            z: dn.z,
            seq: self.seq,
            by: self.cur,
            tess: Default::default(),
        });
    }

    /// Lays out and draws the GuiObject children of `parent` inside `content`.
    pub(super) fn children(&mut self, parent: Ref, content: Rect, dn: Down) {
        let d = self.input.doc;
        // Folders organize GUI descendants without introducing a layout box.
        // (what was read is kept for the GUI cache: gui_input.rs cache_marks)
        fn gui_children(d: &Document, parent: Ref, out: &mut Vec<Ref>, gui: &mut Gui, by: Ref) {
            gui.walk_kids.push(parent);
            for child in children(d, parent) {
                gui.walk_reads.push((child, by));
                match d.class_of(child).as_deref() {
                    Some("Folder") => gui_children(d, child, out, gui, by),
                    Some(class) if doc::is_a(class, "GuiObject") => out.push(child),
                    _ => {}
                }
            }
        }
        let mut kids = Vec::new();
        gui_children(d, parent, &mut kids, self.gui, self.cur);
        let list_layout = child_of_class(d, parent, "UIListLayout");
        let list = list_layout.or_else(|| child_of_class(d, parent, "UIPageLayout"));
        let grid = child_of_class(d, parent, "UIGridLayout");
        let mut placed: HashMap<Ref, Rect> = HashMap::new();
        if let Some(lay) = grid.or(list) {
            let mut order: Vec<Ref> = kids
                .iter()
                .copied()
                .filter(|k| boolp(d, *k, "Visible", true))
                .collect();
            // SortOrder: Name 0, Custom 1, LayoutOrder 2.
            match enum_u32(d.prop(lay, "SortOrder")).unwrap_or(2) {
                0 => order.sort_by_key(|k| name_of(d, *k)),
                2 => order.sort_by_key(|k| num(d, *k, "LayoutOrder", 0.0) as i64),
                _ => {}
            }
            let halign = enum_u32(d.prop(lay, "HorizontalAlignment")).unwrap_or(1); // Center 0, Left 1, Right 2
            let valign = enum_u32(d.prop(lay, "VerticalAlignment")).unwrap_or(1); // Center 0, Top 1, Bottom 2
            let vertical = enum_u32(d.prop(lay, "FillDirection")).unwrap_or(1) == 1;
            if Some(lay) == grid {
                let cell = udim2(d, lay, "CellSize");
                let pad = udim2(d, lay, "CellPadding");
                let cw = cell[0] * content.width() + cell[1] * dn.scale;
                let ch = cell[2] * content.height() + cell[3] * dn.scale;
                let px = pad[0] * content.width() + pad[1] * dn.scale;
                let py = pad[2] * content.height() + pad[3] * dn.scale;
                let max_cells = num(d, lay, "FillDirectionMaxCells", 0.0) as usize;
                let fit = if vertical {
                    ((content.height() + py) / (ch + py).max(1e-3)).floor()
                } else {
                    ((content.width() + px) / (cw + px).max(1e-3)).floor()
                };
                let mut per_line = (fit as usize).max(1);
                if max_cells > 0 {
                    per_line = per_line.min(max_cells);
                }
                let n = order.len();
                let lines = n.div_ceil(per_line).max(1);
                let (cols, rows) = if vertical {
                    (lines, per_line.min(n.max(1)))
                } else {
                    (per_line.min(n.max(1)), lines)
                };
                let total_w = cols as f32 * cw + cols.saturating_sub(1) as f32 * px;
                let total_h = rows as f32 * ch + rows.saturating_sub(1) as f32 * py;
                let x0 = content.left()
                    + match halign {
                        0 => (content.width() - total_w) * 0.5,
                        2 => content.width() - total_w,
                        _ => 0.0,
                    };
                let y0 = content.top()
                    + match valign {
                        0 => (content.height() - total_h) * 0.5,
                        2 => content.height() - total_h,
                        _ => 0.0,
                    };
                for (i, k) in order.iter().enumerate() {
                    let (c, r) = if vertical {
                        (i / per_line, i % per_line)
                    } else {
                        (i % per_line, i / per_line)
                    };
                    placed.insert(
                        *k,
                        Rect::from_min_size(
                            Pos2::new(x0 + c as f32 * (cw + px), y0 + r as f32 * (ch + py)),
                            egui::vec2(cw, ch),
                        ),
                    );
                }
            } else {
                let pad = udim(d, lay, "Padding");
                let gap = pad[0]
                    * if vertical {
                        content.height()
                    } else {
                        content.width()
                    }
                    + pad[1] * dn.scale;
                let mut sizes: Vec<egui::Vec2> = order
                    .iter()
                    .map(|k| self.own_size(*k, content, dn.scale, 0))
                    .collect();
                let mut total: f32 = sizes
                    .iter()
                    .map(|s| if vertical { s.y } else { s.x })
                    .sum::<f32>()
                    + gap * order.len().saturating_sub(1) as f32;
                // Fill lets each item grow by default. A UIFlexItem can
                // override how much its own item grows.
                let flex_property = if vertical { "VerticalFlex" } else { "HorizontalFlex" };
                let fill = Some(lay) == list_layout
                    && enum_u32(d.prop(lay, flex_property)) == Some(1);
                let room = if vertical { content.height() } else { content.width() } - total;
                if room > 0.0 && !order.is_empty() {
                    let ratios: Vec<f32> = order.iter().map(|k| {
                        self.gui.walk_kids.push(*k);
                        match child_of_class(d, *k, "UIFlexItem") {
                            Some(item) => {
                                self.gui.walk_reads.push((item, self.cur));
                                flex_grow(d, item)
                            }
                            None => if fill { 1.0 } else { 0.0 },
                        }
                    }).collect();
                    let sum: f32 = ratios.iter().sum();
                    if sum > 0.0 {
                        for ((k, s), r) in order.iter().zip(sizes.iter_mut()).zip(&ratios) {
                            let add = room * r / sum;
                            if vertical {
                                s.y += add;
                            } else {
                                s.x += add;
                                // (as tall as it is at its new width:
                                // Sizer::flex_heights)
                                if add > 0.0 && enum_u32(d.prop(*k, "AutomaticSize")).unwrap_or(0) & 2 != 0 {
                                    s.y = Sizer { d, m: &mut *self }.height_at_width(*k, s.x, content, dn.scale, 0);
                                }
                            }
                        }
                        total += room;
                    }
                }
                let mut at = if vertical {
                    content.top()
                        + match valign {
                            0 => (content.height() - total) * 0.5,
                            2 => content.height() - total,
                            _ => 0.0,
                        }
                } else {
                    content.left()
                        + match halign {
                            0 => (content.width() - total) * 0.5,
                            2 => content.width() - total,
                            _ => 0.0,
                        }
                };
                for (k, s) in order.iter().zip(sizes) {
                    let r = if vertical {
                        let x = content.left()
                            + match halign {
                                0 => (content.width() - s.x) * 0.5,
                                2 => content.width() - s.x,
                                _ => 0.0,
                            };
                        Rect::from_min_size(Pos2::new(x, at), s)
                    } else {
                        let y = content.top()
                            + match valign {
                                0 => (content.height() - s.y) * 0.5,
                                2 => content.height() - s.y,
                                _ => 0.0,
                            };
                        Rect::from_min_size(Pos2::new(at, y), s)
                    };
                    placed.insert(*k, r);
                    at += if vertical { s.y } else { s.x } + gap;
                }
            }
        }
        // A UIPageLayout shows its current page where the row starts, sliding
        // between pages while it tweens (crates/rustudio-engine/src/domains/gui_input.luau).
        if let Some(lay) = list.filter(|l| d.class_of(*l).is_some_and(|c| c == "UIPageLayout")) {
            let page = super::share().pages.get(&lay).copied();
            if let Some((from, to, alpha)) = page {
                // (sliding: the page moves without a document mark)
                self.gui.no_cache = Some("page sliding");
                let off = |r: Ref| placed.get(&r).map(|p| p.min - content.min);
                if let (Some(a), Some(b)) = (off(from).or(off(to)), off(to)) {
                    let o = a + (b - a) * alpha;
                    for p in placed.values_mut() {
                        *p = p.translate(-o);
                    }
                }
            }
        }
        // Sibling order: ZIndex, then child order.
        let mut sorted = kids;
        sorted.sort_by_key(|k| num(d, *k, "ZIndex", 1.0) as i32);
        for k in sorted {
            self.object_memo(k, content, placed.get(&k).copied(), dn);
        }
    }

    /// egui font size per Roblox TextSize for a family: Roblox sizes a font so
    /// a line (ascent + descent) is TextSize tall; egui sizes by the em.
    fn em_per_size(&mut self, family: &FontFamily) -> f32 {
        let key = format!("{family:?}");
        if let Some(k) = self.gui.em_ratio.get(&key) {
            return *k;
        }
        let fam = family.clone();
        let line = self
            .ctx
            .fonts_mut(|f| f.row_height(&FontId::new(100.0, fam)))
            / 100.0;
        let k = mech::GUI_FONT_EM_PER_TEXT_SIZE / line.max(0.5);
        self.gui.em_ratio.insert(key, k);
        k
    }

    /// The size of a text object's text, wrapped at `wrap` (AutomaticSize).
    fn measure_text(&mut self, r: Ref, wrap: f32, scale: f32) -> egui::Vec2 {
        let d = self.input.doc;
        let mut text = text_prop(d, r, "Text");
        if text.is_empty() && d.class_of(r).is_some_and(|c| c == "TextBox") {
            text = text_prop(d, r, "PlaceholderText");
        }
        if boolp(d, r, "RichText", false) {
            text = rich_runs(&text).into_iter().map(|run| run.text).collect();
        }
        if text.is_empty() {
            return egui::Vec2::ZERO;
        }
        let font = font_of(d, r);
        let family = self.gui.family(self.ctx, self.input.store, &font);
        let text = super::ligatures::apply(&family, text);
        let size = num(d, r, "TextSize", 14.0) * scale * legacy_text_scale(&font);
        let k = self.em_per_size(&family);
        let mut f = TextFormat::simple(FontId::new(size * k, family), Color32::WHITE);
        f.line_height = Some(size * num(d, r, "LineHeight", 1.0));
        let mut job = LayoutJob::default();
        job.append(&text, 0.0, f);
        job.wrap.max_width = wrap.max(1.0);
        job.round_output_to_gui = false;
        let g = self.gui.text_fit.layout_now(self.ctx, job);
        egui::vec2(g.rect.width(), g.rect.height().max(size))
    }

    fn content_size(&mut self, r: Ref, size: egui::Vec2, scale: f32, auto: u32, depth: usize) -> egui::Vec2 {
        let d = self.input.doc;
        Sizer { d, m: self }.content_size(r, size, scale, auto, depth)
    }

    fn own_size(&mut self, r: Ref, parent: Rect, scale: f32, depth: usize) -> egui::Vec2 {
        let d = self.input.doc;
        Sizer { d, m: self }.own_size(r, parent, scale, depth)
    }

    fn own_size_signed(&mut self, r: Ref, parent: Rect, scale: f32, depth: usize) -> egui::Vec2 {
        let d = self.input.doc;
        Sizer { d, m: self }.own_size_signed(r, parent, scale, depth)
    }
}

/// Measures a text object's text for AutomaticSize (`Walk::measure_text`
/// in the GUI walk, `FaceText` for a script's read between walks).
pub(super) trait AutoText {
    fn measure_text(&mut self, d: &Document, r: Ref, wrap: f32, scale: f32) -> egui::Vec2;
}

impl AutoText for &mut Walk<'_, '_> {
    fn measure_text(&mut self, _: &Document, r: Ref, wrap: f32, scale: f32) -> egui::Vec2 {
        Walk::measure_text(self, r, wrap, scale)
    }
}

/// The AutomaticSize / Size rules, shared by the GUI walk and by
/// AbsoluteSize reads of objects not laid out yet (`gui::auto_grow_now`).
pub(super) struct Sizer<'d, M> {
    pub d: &'d Document,
    pub m: M,
}

impl<M: AutoText> Sizer<'_, M> {
    /// What an AutomaticSize object must hold: its text, or its children
    /// (laid out by a UIListLayout, or by their own positions), plus padding.
    pub(super) fn content_size(
        &mut self,
        r: Ref,
        size: egui::Vec2,
        scale: f32,
        auto: u32,
        depth: usize,
    ) -> egui::Vec2 {
        self.content_size_in(r, size, scale, auto, depth, f32::INFINITY)
    }

    /// `content_size`, a TextWrapped text growing on X wrapping at `room`
    /// (the width its parent leaves it).
    pub(super) fn content_size_in(
        &mut self,
        r: Ref,
        size: egui::Vec2,
        scale: f32,
        auto: u32,
        depth: usize,
        room: f32,
    ) -> egui::Vec2 {
        let d = self.d;
        let pad = |name: &str, along: f32| {
            let u = child_of_class(d, r, "UIPadding")
                .map(|p| udim(d, p, name))
                .unwrap_or([0.0, 0.0]);
            u[0] * along + u[1] * scale
        };
        let (pl, pr, pt, pb) = (
            pad("PaddingLeft", size.x),
            pad("PaddingRight", size.x),
            pad("PaddingTop", size.y),
            pad("PaddingBottom", size.y),
        );
        let inner = Rect::from_min_size(
            Pos2::ZERO,
            egui::vec2((size.x - pl - pr).max(0.0), (size.y - pt - pb).max(0.0)),
        );
        // Growing on X, its children measure with the room it can grow
        // into, not its own (often 0) width: Roblox's Help tab nests
        // AutomaticSize XY frames 0 wide around wrapped "Up arrow", which
        // wrapped a letter per line.
        let room_inner = if auto & 1 != 0 && room.is_finite() {
            Rect::from_min_size(Pos2::ZERO, egui::vec2(inner.width().max(room - pl - pr), inner.height()))
        } else {
            inner
        };
        let mut ext = egui::Vec2::ZERO;
        if d.class_of(r)
            .is_some_and(|c| matches!(c.as_str(), "TextLabel" | "TextButton" | "TextBox"))
        {
            // A wrapped text growing on X grows up to the room its parent
            // leaves, then wraps (Studio: Argon's error message, an
            // AutomaticSize XY TextWrapped label in a ScrollingFrame, wraps in
            // three lines in its 302 px window,
            // checks/plugins/ref/corpus/screens/argon-load-popup.jpg).
            let wrap = if !boolp(d, r, "TextWrapped", false) {
                f32::INFINITY
            } else if auto & 1 != 0 {
                (room - pl - pr).max(inner.width())
            } else {
                inner.width()
            };
            ext = self.m.measure_text(d, r, wrap, scale);
        }
        let kids: Vec<Ref> = children(d, r)
            .into_iter()
            .filter(|c| {
                d.class_of(*c).is_some_and(|k| doc::is_a(&k, "GuiObject"))
                    && boolp(d, *c, "Visible", true)
            })
            .collect();
        if let Some(lay) = child_of_class(d, r, "UIListLayout") {
            let vertical = enum_u32(d.prop(lay, "FillDirection")).unwrap_or(1) == 1;
            let p = udim(d, lay, "Padding");
            let gap =
                p[0] * if vertical {
                    inner.height()
                } else {
                    inner.width()
                } + p[1] * scale;
            let mut sizes: Vec<egui::Vec2> =
                kids.iter().map(|k| self.auto_child_size(*k, inner, room_inner, scale, auto, depth + 1)).collect();
            // A row that isn't growing on X fills its width as `children`
            // lays it out: the items that flex wider are as tall as they
            // are at their new width.
            if !vertical && auto & 1 == 0 && depth < 8 {
                let used = sizes.iter().map(|s| s.x).sum::<f32>() + gap * kids.len().saturating_sub(1) as f32;
                let fill = enum_u32(d.prop(lay, "HorizontalFlex")) == Some(1);
                self.flex_heights(&kids, &mut sizes, inner, inner.width() - used, fill, scale, depth + 1);
            }
            let mut along = 0.0f32;
            let mut across = 0.0f32;
            for (i, s) in sizes.iter().enumerate() {
                along += if vertical { s.y } else { s.x } + if i > 0 { gap } else { 0.0 };
                across = across.max(if vertical { s.x } else { s.y });
            }
            let e = if vertical {
                egui::vec2(across, along)
            } else {
                egui::vec2(along, across)
            };
            ext = ext.max(e);
        } else if let Some(lay) = child_of_class(d, r, "UIGridLayout") {
            // A grid holds its cells (CellSize, not the children's Size), in
            // as many lines as the fill axis fits (as `children` places them).
            let vertical = enum_u32(d.prop(lay, "FillDirection")).unwrap_or(1) == 1;
            let cell = udim2(d, lay, "CellSize");
            let cp = udim2(d, lay, "CellPadding");
            let cw = cell[0] * inner.width() + cell[1] * scale;
            let ch = cell[2] * inner.height() + cell[3] * scale;
            let px = cp[0] * inner.width() + cp[1] * scale;
            let py = cp[2] * inner.height() + cp[3] * scale;
            let n = kids.len();
            if n > 0 {
                let fits = |room: f32, c: f32, p: f32| -> usize {
                    if room.is_finite() && room > 0.0 {
                        (((room + p) / (c + p).max(1e-3)).floor() as usize).max(1)
                    } else {
                        n
                    }
                };
                // The fill axis is the one AutomaticSize doesn't grow.
                let mut per_line = if vertical {
                    if auto & 2 != 0 { n } else { fits(inner.height(), ch, py) }
                } else if auto & 1 != 0 {
                    n
                } else {
                    fits(inner.width(), cw, px)
                };
                let max_cells = num(d, lay, "FillDirectionMaxCells", 0.0) as usize;
                if max_cells > 0 {
                    per_line = per_line.min(max_cells);
                }
                let lines = n.div_ceil(per_line);
                let (cols, rows) = if vertical {
                    (lines, per_line.min(n))
                } else {
                    (per_line.min(n), lines)
                };
                ext = ext.max(egui::vec2(
                    cols as f32 * cw + cols.saturating_sub(1) as f32 * px,
                    rows as f32 * ch + rows.saturating_sub(1) as f32 * py,
                ));
            }
        } else {
            for k in kids {
                let s = self.auto_child_size(k, inner, room_inner, scale, auto, depth + 1);
                let p = udim2(d, k, "Position");
                let anchor = vec2(d, k, "AnchorPoint", [0.0, 0.0]);
                let at = egui::vec2(
                    p[0] * inner.width() + p[1] * scale,
                    p[2] * inner.height() + p[3] * scale,
                );
                ext = ext.max(at + egui::vec2(s.x * (1.0 - anchor[0]), s.y * (1.0 - anchor[1])));
            }
        }
        egui::vec2(ext.x + pl + pr, ext.y + pt + pb)
    }

    /// A child's size as its AutomaticSize parent (growing on `auto`'s axes)
    /// measures it: a child sized by Scale on a growing axis would size
    /// itself from the size being worked out, so it counts with what it
    /// holds instead (its own content, the same way down), as Studio sizes
    /// Archimedes' window (an AutomaticSize Y button whose content sits in a
    /// `UDim2.new(1, 0, 1, -20)` frame: Studio shows the whole panel,
    /// checks/plugins/ref/corpus/screens/archimedes-click-main.jpg).
    /// `room_inner`: `inner` as wide as the parent can grow (children sized
    /// by Scale on X still take `inner`).
    pub(super) fn auto_child_size(
        &mut self,
        k: Ref,
        inner: Rect,
        room_inner: Rect,
        scale: f32,
        auto: u32,
        depth: usize,
    ) -> egui::Vec2 {
        let base = if udim2(self.d, k, "Size")[0] != 0.0 { inner } else { room_inner };
        let mut s = self.own_size(k, base, scale, depth);
        if depth >= 8 {
            return s;
        }
        let sz = udim2(self.d, k, "Size");
        let grows = |axis: u32, sc: f32| auto & axis != 0 && sc != 0.0;
        let (gx, gy) = (grows(1, sz[0]), grows(2, sz[2]));
        if gx || gy {
            let axes = (gx as u32) | ((gy as u32) << 1);
            let c = self.content_size(k, s, scale, axes, depth);
            if gx {
                s.x = s.x.max(c.x);
            }
            if gy {
                s.y = s.y.max(c.y);
            }
        }
        s
    }

    /// A horizontal row's items grown by `room` (UIFlexItem FlexMode Grow,
    /// or every item when the layout's HorizontalFlex is Fill), and those
    /// growing on Y made as tall as their content is at the new width (an
    /// item sized 0 wide measured first at 0: ExpChat's TextContainer wrapped
    /// the typed text a word per line, the chat input 182 px tall).
    #[allow(clippy::too_many_arguments)]
    pub(super) fn flex_heights(
        &mut self,
        kids: &[Ref],
        sizes: &mut [egui::Vec2],
        parent: Rect,
        room: f32,
        fill: bool,
        scale: f32,
        depth: usize,
    ) {
        let d = self.d;
        if !room.is_finite() || room <= 0.0 || kids.is_empty() {
            return;
        }
        let ratios: Vec<f32> = kids
            .iter()
            .map(|k| match child_of_class(d, *k, "UIFlexItem") {
                Some(item) => flex_grow(d, item),
                None => if fill { 1.0 } else { 0.0 },
            })
            .collect();
        let sum: f32 = ratios.iter().sum();
        if sum <= 0.0 {
            return;
        }
        for ((k, s), r) in kids.iter().zip(sizes.iter_mut()).zip(&ratios) {
            if *r > 0.0 {
                s.x += room * r / sum;
                if enum_u32(d.prop(*k, "AutomaticSize")).unwrap_or(0) & 2 != 0 {
                    s.y = self.height_at_width(*k, s.x, parent, scale, depth);
                }
            }
        }
    }

    /// An AutomaticSize Y object's height when it is `width` wide.
    pub(super) fn height_at_width(&mut self, r: Ref, width: f32, parent: Rect, scale: f32, depth: usize) -> f32 {
        let d = self.d;
        let s = udim2(d, r, "Size");
        let base = s[2] * parent.height() + s[3] * scale;
        let c = self.content_size_in(r, egui::vec2(width, base), scale, 2, depth, width);
        let mut y = base.max(c.y);
        if let Some(c) = child_of_class(d, r, "UISizeConstraint") {
            let min = vec2(d, c, "MinSize", [0.0, 0.0]);
            let max = vec2(d, c, "MaxSize", [f32::INFINITY, f32::INFINITY]);
            y = y.clamp(min[1], max[1].max(min[1]));
        }
        y
    }

    /// A GuiObject's size from Size, AutomaticSize and its constraints (and UIScale).
    pub(super) fn own_size(&mut self, r: Ref, parent: Rect, scale: f32, depth: usize) -> egui::Vec2 {
        self.own_size_signed(r, parent, scale, depth).max(egui::Vec2::ZERO)
    }

    /// `own_size` before negative sizes are clamped: Studio reports those
    /// (AbsoluteSize -1, -1 for a Size of -1 px offset, the box placed with
    /// it; Emitter2D's jitter fix makes them for particles near their end).
    pub(super) fn own_size_signed(&mut self, r: Ref, parent: Rect, scale: f32, depth: usize) -> egui::Vec2 {
        let d = self.d;
        let s = udim2(d, r, "Size");
        // SizeConstraint (RelativeXY 0, RelativeXX 1, RelativeYY 2): the
        // Size's scales both take the parent's width, or both its height
        // (DataStore Editor's square check boxes: 0.6 of the row's height).
        let (sw, sh) = match enum_u32(d.prop(r, "SizeConstraint")) {
            Some(1) => (parent.width(), parent.width()),
            Some(2) => (parent.height(), parent.height()),
            _ => (parent.width(), parent.height()),
        };
        let mut size = egui::vec2(s[0] * sw + s[1] * scale, s[2] * sh + s[3] * scale);
        // AutomaticSize: None 0, X 1, Y 2, XY 3.
        let auto = enum_u32(d.prop(r, "AutomaticSize")).unwrap_or(0);
        if auto != 0 && depth < 8 {
            let px = udim2(d, r, "Position");
            let room = parent.width() - (px[0] * parent.width() + px[1] * scale);
            let c = self.content_size_in(r, size, scale, auto, depth, room);
            if auto & 1 != 0 {
                size.x = size.x.max(c.x);
            }
            if auto == 3 {
                // Growing on both axes, the content is laid out again at the
                // width it settled on (within a UISizeConstraint's MaxSize):
                // children sized by Scale on X and wrapped text take that
                // width, and the height is what they need there. (Measured
                // at the old width, ExpChat's translation notice, a
                // 300 px-max XY frame of two wrapped labels, wrapped a
                // character per line and filled its 400 px MaxSize height.)
                if let Some(c) = child_of_class(d, r, "UISizeConstraint") {
                    let min = vec2(d, c, "MinSize", [0.0, 0.0]);
                    let max = vec2(d, c, "MaxSize", [f32::INFINITY, f32::INFINITY]);
                    size.x = size.x.clamp(min[0], max[0].max(min[0]));
                }
                let c = self.content_size_in(r, size, scale, auto, depth, room);
                size.y = size.y.max(c.y);
            } else if auto & 2 != 0 {
                size.y = size.y.max(c.y);
            }
        }
        if let Some(c) = child_of_class(d, r, "UISizeConstraint") {
            let min = vec2(d, c, "MinSize", [0.0, 0.0]);
            let max = vec2(d, c, "MaxSize", [f32::INFINITY, f32::INFINITY]);
            size.x = size.x.clamp(min[0], max[0].max(min[0]));
            size.y = size.y.clamp(min[1], max[1].max(min[1]));
        }
        if let Some(c) = child_of_class(d, r, "UIAspectRatioConstraint") {
            let ratio = num(d, c, "AspectRatio", 1.0).max(1e-3);
            // AspectType: FitWithinMaxSize 0, ScaleWithParentSize 1; DominantAxis: Width 0, Height 1.
            if enum_u32(d.prop(c, "AspectType")) == Some(1) {
                if enum_u32(d.prop(c, "DominantAxis")) == Some(1) {
                    size.x = size.y * ratio;
                } else {
                    size.y = size.x / ratio;
                }
            } else if size.x / size.y.max(1e-3) > ratio {
                size.x = size.y * ratio;
            } else {
                size.y = size.x / ratio;
            }
        }
        if let Some(sc) = child_of_class(d, r, "UIScale") {
            size *= num(d, sc, "Scale", 1.0);
        }
        size
    }
}

impl Walk<'_, '_> {
    /// What a memo depends on besides the document: where and how the walk
    /// reaches the object.
    fn memo_key(&self, parent: Rect, laid: Option<Rect>, dn: &Down) -> [u32; 28] {
        let l = laid.unwrap_or(Rect::NOTHING);
        [
            parent.min.x, parent.min.y, parent.max.x, parent.max.y,
            laid.is_some() as u32 as f32, l.min.x, l.min.y, l.max.x, l.max.y,
            dn.aff.m[0], dn.aff.m[1], dn.aff.m[2], dn.aff.m[3], dn.aff.t[0], dn.aff.t[1],
            dn.clip.min.x, dn.clip.min.y, dn.clip.max.x, dn.clip.max.y,
            dn.alpha, dn.tint[0], dn.tint[1], dn.tint[2], dn.scale, dn.z as f32,
            self.global_z as u32 as f32, self.abs_offset[0], self.abs_offset[1],
        ]
        .map(f32::to_bits)
    }

    fn lens(&self) -> [usize; 5] {
        [self.items.len(), self.hits.len(), self.boxes.len(), self.gui.walk_reads.len(), self.gui.walk_kids.len()]
    }

    /// Ends the innermost memo's current run of own pieces.
    fn close_own(&mut self) {
        let lens = self.lens();
        let Some(top) = self.rec.last_mut() else { return };
        if lens != top.start {
            let s = top.start;
            top.segs.push(Seg::Own {
                items: self.items[s[0]..].to_vec(),
                hits: self.hits[s[1]..].to_vec(),
                boxes: self.boxes[s[2]..].to_vec(),
                reads: self.gui.walk_reads[s[3]..].to_vec(),
                kids: self.gui.walk_kids[s[4]..].to_vec(),
            });
        }
        top.start = lens;
    }

    /// A child GuiObject: from its memo when it has a valid one, else walked
    /// (and its memo written, unless something it drew changes by itself).
    pub(super) fn object_memo(&mut self, k: Ref, parent: Rect, laid: Option<Rect>, dn: Down) {
        if self.replaying == 0 {
            self.close_own();
            if let Some(top) = self.rec.last_mut() {
                top.segs.push(Seg::Child { r: k, parent, laid, dn });
            }
        }
        let key = self.memo_key(parent, laid, &dn);
        if !self.gui.memo_off && self.gui.memo.get(&k).is_some_and(|m| m.key == key) {
            let m = self.gui.memo.remove(&k).unwrap();
            self.replaying += 1;
            for s in &m.segs {
                match s {
                    Seg::Own { items, hits, boxes, reads, kids } => {
                        for it in items {
                            self.seq += 1;
                            let mut it = it.clone();
                            it.seq = self.seq;
                            self.items.push(it);
                        }
                        self.hits.extend(hits.iter().cloned());
                        for (r, b) in boxes {
                            self.gui.layout.insert(*r, *b);
                            self.boxes.push((*r, *b));
                        }
                        self.gui.walk_reads.extend_from_slice(reads);
                        self.gui.walk_kids.extend_from_slice(kids);
                    }
                    Seg::Child { r, parent, laid, dn } => self.object_memo(*r, *parent, *laid, *dn),
                }
            }
            self.replaying -= 1;
            self.gui.memo.insert(k, m);
            self.gui.memo_hits += 1;
        } else {
            let replaying = std::mem::replace(&mut self.replaying, 0);
            let lens = self.lens();
            self.rec.push(Rec { start: lens, segs: Vec::new(), no_cache: self.gui.no_cache.take(), vfs: self.gui.walk_vfs.len() });
            let cur = std::mem::replace(&mut self.cur, k);
            self.object(k, parent, laid, dn);
            self.cur = cur;
            self.close_own();
            let rec = self.rec.pop().unwrap();
            if self.gui.no_cache.is_none() && self.gui.walk_vfs.len() == rec.vfs {
                self.gui.memo.insert(k, Memo { key, segs: rec.segs });
            } else {
                self.gui.memo.remove(&k);
            }
            self.gui.no_cache = rec.no_cache.or(self.gui.no_cache);
            self.replaying = replaying;
            self.gui.memo_walks += 1;
        }
        if self.replaying == 0 {
            let lens = self.lens();
            if let Some(top) = self.rec.last_mut() {
                top.start = lens;
            }
        }
    }

    fn object(&mut self, r: Ref, parent: Rect, laid: Option<Rect>, dn: Down) {
        let d = self.input.doc;
        let Some(class) = d.class_of(r).map(|c| c.to_string()) else {
            return;
        };
        if !boolp(d, r, "Visible", true) {
            return;
        }
        // Drawn: its modifiers (and theirs: a UIStroke's UIGradient) are read.
        self.gui.walk_kids.push(r);
        for c in children(d, r) {
            self.gui.walk_reads.push((c, r));
            if !d.class_of(c).is_some_and(|k| doc::is_a(&k, "GuiObject")) {
                for x in d.subtree(c) {
                    self.gui.walk_reads.push((x, r));
                    self.gui.walk_kids.push(x);
                }
            }
        }
        let signed = self.own_size_signed(r, parent, dn.scale, 0);
        let size = signed.max(egui::Vec2::ZERO);
        let rect = match laid {
            // Layouts own both position and size (UIGridLayout overrides Size).
            Some(l) => l,
            None => {
                let p = udim2(d, r, "Position");
                let anchor = vec2(d, r, "AnchorPoint", [0.0, 0.0]);
                let at = Pos2::new(
                    parent.left() + p[0] * parent.width() + p[1] * dn.scale,
                    parent.top() + p[2] * parent.height() + p[3] * dn.scale,
                );
                Rect::from_min_size(
                    Pos2::new(at.x - anchor[0] * size.x, at.y - anchor[1] * size.y),
                    size,
                )
            }
        };
        let rot = num(d, r, "Rotation", 0.0);
        let mut me = dn;
        me.scale *= child_of_class(d, r, "UIScale")
            .map(|s| num(d, s, "Scale", 1.0))
            .unwrap_or(1.0);
        if self.global_z {
            me.z = num(d, r, "ZIndex", 1.0) as i32;
        }
        if rot.abs() > 1e-3 {
            me.aff = Aff::rotate_about(rect.center(), rot).then(&dn.aff);
        }
        me.turn += rot;
        if class == "CanvasGroup" {
            me.alpha *= 1.0 - num(d, r, "GroupTransparency", 0.0);
            let c = color(d, r, "GroupColor3", [1.0, 1.0, 1.0]);
            me.tint = [me.tint[0] * c[0], me.tint[1] * c[1], me.tint[2] * c[2]];
        }
        // Studio's box under turned ancestors: its centre turned with them,
        // the size unturned; AbsoluteRotation is every Rotation summed.
        // (a negative size is reported, and places the box, as in Studio)
        let (abs_min, abs_size) = match laid {
            Some(_) => (rect.min, rect.size()),
            None => {
                let anchor = vec2(d, r, "AnchorPoint", [0.0, 0.0]);
                let shift = egui::vec2(
                    anchor[0] * (size.x - signed.x),
                    anchor[1] * (size.y - signed.y),
                );
                (rect.min + shift, signed)
            }
        };
        let centre = dn.aff.apply(abs_min + abs_size * 0.5);
        let mut gb = GuiBox {
            pos: [
                centre.x - abs_size.x * 0.5 + self.abs_offset[0],
                centre.y - abs_size.y * 0.5 + self.abs_offset[1],
            ],
            size: [abs_size.x, abs_size.y],
            rot: dn.turn + rot,
            text: None,
            canvas: None,
            canvas_fit: None,
        };

        let items_before = self.items.len();
        // UIBlur: what's behind, blurred, under the object's own background.
        self.blur(r, rect, &me);
        // Background (buttons darken while hovered / pressed: AutoButtonColor).
        let mut corner = child_of_class(d, r, "UICorner").map(|c| {
            let u = udim(d, c, "CornerRadius");
            u[0] * rect.width().min(rect.height()) + u[1] * me.scale
        });
        // Frame.Style RobloxSquare (2) / RobloxRound (3): Studio's
        // blackBkg_square/_round.png, black at 179/255 with an 8 px
        // corner, in place of the frame's own colour and border.
        let frame_style = if class == "Frame" {
            rustudio_render::scene::enum_u32(d.prop(r, "Style")).unwrap_or(0)
        } else {
            0
        };
        let styled = matches!(frame_style, 2 | 3);
        if frame_style == 3 {
            corner = Some(8.0 * me.scale);
        }
        let gradient = child_of_class(d, r, "UIGradient");
        let is_button = doc::is_a(&class, "GuiButton");
        let mut bg_t = num(d, r, "BackgroundTransparency", 0.0);
        let mut bg = color(d, r, "BackgroundColor3", [1.0, 1.0, 1.0]);
        if styled {
            bg_t = 1.0 - 179.0 / 255.0;
            bg = [0.0, 0.0, 0.0];
        }
        if is_button && boolp(d, r, "AutoButtonColor", true) {
            let k = if self.gui.pressed.is_some_and(|(p, _)| p == r) {
                mech::GUI_BUTTON_PRESS_DARKEN
            } else if self.gui.hovered.contains(&r) {
                mech::GUI_BUTTON_HOVER_DARKEN
            } else {
                0.0
            };
            bg = [bg[0] * (1.0 - k), bg[1] * (1.0 - k), bg[2] * (1.0 - k)];
        }
        // Frame.Style ChatBlue (1) / ChatGreen (4) / ChatRed (5): Studio's
        // dialog_*.png 9-sliced over the box (gui_fx.rs), not the colour.
        let chat_style = matches!(frame_style, 1 | 4 | 5);
        if chat_style {
            self.chat_frame(frame_style, rect, &me);
            bg_t = 1.0;
        }
        // A ViewportFrame's background is inside its image (vf_render.rs).
        if class == "ViewportFrame" {
            bg_t = 1.0;
        }
        if bg_t < 1.0 && rect.width() > 0.0 && rect.height() > 0.0 {
            let a = (1.0 - bg_t) * me.alpha;
            let tint = me.tint;
            let pts = rounded(rect, corner.unwrap_or(0.0));
            let mesh = match gradient {
                Some(g) => {
                    let gr = Grad::new(d, g, rect);
                    let f = move |p: Pos2| {
                        let (c, t) = gr.at(p);
                        rgba(
                            [
                                bg[0] * c[0] * tint[0],
                                bg[1] * c[1] * tint[1],
                                bg[2] * c[2] * tint[2],
                            ],
                            a * (1.0 - t),
                        )
                    };
                    if corner.is_some() {
                        fill_mesh(&pts, &f)
                    } else {
                        quad_mesh(
                            rect,
                            Rect::from_min_max(egui::epaint::WHITE_UV, egui::epaint::WHITE_UV),
                            egui::TextureId::default(),
                            &f,
                            8,
                        )
                    }
                }
                None => {
                    let c = rgba([bg[0] * tint[0], bg[1] * tint[1], bg[2] * tint[2]], a);
                    fill_mesh(&pts, &|_| c)
                }
            };
            self.push(Shape::Mesh(mesh.into()), &me);
            // BorderSizePixel draws outside the box (BorderMode Outline), not with a UICorner.
            let border = num(d, r, "BorderSizePixel", 0.0) * me.scale;
            if border > 0.0 && corner.is_none() && !styled && !chat_style {
                let bc = rgba(color(d, r, "BorderColor3", [0.0, 0.0, 0.0]), a);
                let o = rect.expand(border);
                for s in [
                    Rect::from_min_max(o.min, Pos2::new(o.max.x, rect.min.y)),
                    Rect::from_min_max(Pos2::new(o.min.x, rect.max.y), o.max),
                    Rect::from_min_max(
                        Pos2::new(o.min.x, rect.min.y),
                        Pos2::new(rect.min.x, rect.max.y),
                    ),
                    Rect::from_min_max(
                        Pos2::new(rect.max.x, rect.min.y),
                        Pos2::new(o.max.x, rect.max.y),
                    ),
                ] {
                    self.push(
                        Shape::Mesh(
                            fill_mesh(
                                &[
                                    s.left_top(),
                                    s.right_top(),
                                    s.right_bottom(),
                                    s.left_bottom(),
                                ],
                                &|_| bc,
                            )
                            .into(),
                        ),
                        &me,
                    );
                }
            }
        }
        let text_class = matches!(class.as_str(), "TextLabel" | "TextButton" | "TextBox");
        // UIStroke: around the box (Border), or around the text (Contextual, on text objects).
        let stroke = child_of_class(d, r, "UIStroke");
        let stroke_on_text = stroke.is_some_and(|s| {
            text_class && enum_u32(d.prop(s, "ApplyStrokeMode")).unwrap_or(0) == 0
        });
        if let Some(s) = stroke.filter(|_| !stroke_on_text) {
            let th = num(d, s, "Thickness", 1.0) * me.scale;
            let st = num(d, s, "Transparency", 0.0);
            if th > 0.0 && st < 1.0 {
                let c = rgba(color(d, s, "Color", [0.0, 0.0, 0.0]), (1.0 - st) * me.alpha);
                let outer = rounded(
                    rect.expand(th * 0.5),
                    corner.map(|c| c + th * 0.5).unwrap_or(0.0),
                );
                // A UIGradient under the UIStroke colours the stroke.
                match child_of_class(d, s, "UIGradient") {
                    Some(g) => {
                        let gl = GradLut::new(d, g, rect);
                        let ps = egui::epaint::PathStroke::new_uv(th, move |_, p| gl.tint(p, c));
                        self.push(Shape::closed_line(outer, ps), &me);
                    }
                    None => self.push(Shape::closed_line(outer, Stroke::new(th, c)), &me),
                }
            }
        }
        if matches!(class.as_str(), "ImageLabel" | "ImageButton") {
            self.image(r, rect, &me, gradient, corner);
        }
        // VideoFrame / VideoDisplay: the video's current frame (media domain).
        if matches!(class.as_str(), "VideoFrame" | "VideoDisplay") {
            self.image(r, rect, &me, gradient, corner);
        }
        if class == "ViewportFrame" {
            self.viewport_frame(r, rect, &me);
        }
        if text_class {
            // Text is laid out inside the object's UIPadding (Studio: Laser
            // Tag's right-aligned timer sits 16 px in from the box's right).
            let text_rect = padded(d, r, rect, me.scale);
            // A UIGradient on a text object colours its text over the
            // object's box (its Contextual UIStroke keeps its own colour,
            // or its own UIGradient's).
            let fill_grad = gradient.map(|g| GradLut::new(d, g, rect));
            gb.text = self.text(r, &class, text_rect, &me, stroke.filter(|_| stroke_on_text), fill_grad, rect);
        }
        // ParticleEmitter2D children, over the object, under its children.
        self.particles(r, rect, &me);
        // GuiService.SelectedObject (keyboard / gamepad selection).
        if self.selected_object() == Some(r) {
            let [cr, cg, cb] = mech::GUI_SELECTION_RGB;
            self.push(
                Shape::closed_line(
                    rounded(rect.expand(3.0), corner.unwrap_or(0.0) + 3.0),
                    Stroke::new(2.0, Color32::from_rgb(cr, cg, cb)),
                ),
                &me,
            );
        }

        let textbox = class == "TextBox";
        let scrolling = class == "ScrollingFrame";
        self.hits.push(LocalHit {
            r,
            rect,
            aff: me.aff,
            clip: (dn.clip != Rect::EVERYTHING).then_some(dn.clip),
            button: is_button,
            textbox,
            sink: is_button || textbox || boolp(d, r, "Active", false),
            scrolling,
            drag: boolp(d, r, "Draggable", false)
                || child_of_class(d, r, "UIDragDetector").is_some_and(|dd| boolp(d, dd, "Enabled", true)),
            painted: self.items.len() > items_before,
        });
        self.gui.layout.insert(r, gb);
        self.boxes.push((r, gb));

        // Children: inside the padding; clipped by ClipsDescendants (not when turned).
        let mut content = padded(d, r, rect, me.scale);
        // Chat-styled frames hold their children 17 px in (Studio: a
        // child at (-11, -8) of a ChatBlue frame lands at (+6, +9)).
        if chat_style {
            content = content.shrink(17.0 * me.scale);
        }
        if (boolp(d, r, "ClipsDescendants", false) || scrolling) && me.aff.is_id() {
            me.clip = me.clip.intersect(rect);
        }
        if scrolling {
            let cs = udim2(d, r, "CanvasSize");
            let cp = vec2(d, r, "CanvasPosition", [0.0, 0.0]);
            let mut canvas = egui::vec2(
                cs[0] * content.width() + cs[1] * me.scale,
                cs[2] * content.height() + cs[3] * me.scale,
            );
            // AutomaticCanvasSize (None 0, X 1, Y 2, XY 3) grows the canvas
            // to hold the children (Place A's admin panel: CanvasSize 0).
            let acs = enum_u32(d.prop(r, "AutomaticCanvasSize")).unwrap_or(0);
            // The least canvas: the content area, and what an automatic axis holds.
            let mut floor = content.size();
            if acs != 0 {
                let held = self.content_size(r, rect.size(), me.scale, acs, 0)
                    - (rect.size() - content.size());
                if acs & 1 != 0 {
                    canvas.x = canvas.x.max(held.x);
                    floor.x = floor.x.max(held.x);
                }
                if acs & 2 != 0 {
                    canvas.y = canvas.y.max(held.y);
                    floor.y = floor.y.max(held.y);
                }
            }
            let canvas = canvas.max(content.size());
            // Recorded with the padding around it (the scroll range is this
            // less the frame's size).
            let pad = rect.size() - content.size();
            let whole = canvas + pad;
            let fit = crate::gui::CanvasFit { pad: [pad.x, pad.y], floor: [floor.x, floor.y], scale: me.scale };
            if let Some(b) = self.gui.layout.get_mut(&r) {
                b.canvas = Some([whole.x, whole.y]);
                b.canvas_fit = Some(fit);
            }
            if let Some((_, b)) = self.boxes.iter_mut().rev().find(|(k, _)| *k == r) {
                b.canvas = Some([whole.x, whole.y]);
                b.canvas_fit = Some(fit);
            }
            self.children(
                r,
                Rect::from_min_size(
                    Pos2::new(content.left() - cp[0], content.top() - cp[1]),
                    canvas,
                ),
                me,
            );
            let th = num(d, r, "ScrollBarThickness", 12.0) * me.scale;
            let st = num(d, r, "ScrollBarImageTransparency", 0.0);
            if th > 0.0 && st < 1.0 && canvas.y > content.height() + 0.5 {
                self.scroll_bar(r, rect, content, canvas.y, cp[1], th, st, &me);
            }
        } else {
            self.children(r, content, me);
        }
    }

    /// A ScrollingFrame's vertical thumb as Studio draws it: TopImage,
    /// MidImage and BottomImage (default the content's
    /// textures/ui/Scroll/scroll-{top,middle,bottom}.png: white at half
    /// alpha, rounded ends) tinted ScrollBarImageColor3, at the frame's right
    /// edge, ScrollBarThickness wide. The ends are thickness-square images;
    /// the thumb is the view's share of the canvas long and never shorter
    /// than its two ends (Studio's WPixel capture: a 60 px view of a 300 px
    /// canvas draws a 16 px thumb whose round caps meet at its middle,
    /// checks/plugins/widgets/captures/studio-wleft.png).
    #[allow(clippy::too_many_arguments)]
    fn scroll_bar(&mut self, r: Ref, rect: Rect, content: Rect, canvas_h: f32, cp_y: f32, th: f32, st: f32, me: &Down) {
        let d = self.input.doc;
        let h = content.height();
        let len = (h * h / canvas_h).max(2.0 * th).min(h);
        let travel = (canvas_h - h).max(1e-3);
        let top = content.top() + (cp_y / travel).clamp(0.0, 1.0) * (h - len);
        let bar = Rect::from_min_size(Pos2::new(rect.right() - th, top), egui::vec2(th, len));
        let ic = color(d, r, "ScrollBarImageColor3", [1.0, 1.0, 1.0]);
        let tint = me.tint;
        let c = rgba([ic[0] * tint[0], ic[1] * tint[1], ic[2] * tint[2]], (1.0 - st) * me.alpha);
        let image = |prop: &str, dflt: &str| {
            let has = d.dom.get_by_ref(r).is_some_and(|i| i.properties.contains_key(&rbx_dom_weak::ustr(prop)));
            if has { text_prop(d, r, prop) } else { dflt.to_string() }
        };
        let parts = [
            (image("TopImage", "rbxasset://textures/ui/Scroll/scroll-top.png"), Rect::from_min_size(bar.min, egui::vec2(th, th))),
            (
                image("MidImage", "rbxasset://textures/ui/Scroll/scroll-middle.png"),
                Rect::from_min_max(Pos2::new(bar.left(), bar.top() + th), Pos2::new(bar.right(), bar.bottom() - th)),
            ),
            (
                image("BottomImage", "rbxasset://textures/ui/Scroll/scroll-bottom.png"),
                Rect::from_min_max(Pos2::new(bar.left(), bar.bottom() - th), bar.max),
            ),
        ];
        if parts.iter().all(|(id, _)| id.is_empty()) {
            return;
        }
        let mut drew = false;
        for (id, dst) in parts {
            if id.is_empty() || dst.height() <= 0.0 {
                continue;
            }
            let Some((tex, _)) = self.gui.texture(self.ctx, self.input.store, self.input.doc_dir, &id) else {
                continue;
            };
            drew = true;
            let full = Rect::from_min_max(Pos2::ZERO, Pos2::new(1.0, 1.0));
            self.push(Shape::Mesh(quad_mesh(dst, full, tex, &|_| c, 1).into()), me);
        }
        if !drew {
            // No content folder: a plain bar.
            let pts = [bar.left_top(), bar.right_top(), bar.right_bottom(), bar.left_bottom()];
            self.push(Shape::Mesh(fill_mesh(&pts, &|_| c).into()), me);
        }
    }

    fn selected_object(&self) -> Option<Ref> {
        let d = self.input.doc;
        let gs = d.find_child(d.dom.root_ref(), "GuiService")?;
        match d.prop(gs, "SelectedObject") {
            Some(Variant::Ref(r)) if d.exists(r) => Some(r),
            _ => None,
        }
    }

    /// ImageLabel / ImageButton with ImageRect*, ScaleType and UIGradient.
    fn image(&mut self, r: Ref, rect: Rect, me: &Down, gradient: Option<Ref>, corner: Option<f32>) {
        let d = self.input.doc;
        let video = matches!(d.class_of(r).as_deref(), Some("VideoFrame" | "VideoDisplay"));
        let id = if video {
            self.gui.no_cache = Some("video");
            crate::runtime::domains::media::video::surface_uri(r).unwrap_or_default()
        } else {
            text_prop(d, r, "Image")
        };
        let it = num(d, r, if video { "VideoTransparency" } else { "ImageTransparency" }, 0.0);
        if id.is_empty() || it >= 1.0 {
            return;
        }
        let Some((tex, [tw, th])) =
            self.gui
                .texture(self.ctx, self.input.store, self.input.doc_dir, &id)
        else {
            return;
        };
        let ic = color(d, r, if video { "VideoColor3" } else { "ImageColor3" }, [1.0, 1.0, 1.0]);
        let (tw, th) = (tw as f32, th as f32);
        let off = vec2(d, r, if video { "VideoRectOffset" } else { "ImageRectOffset" }, [0.0, 0.0]);
        let sz = vec2(d, r, if video { "VideoRectSize" } else { "ImageRectSize" }, [0.0, 0.0]);
        let src = if sz[0] > 0.0 && sz[1] > 0.0 {
            Rect::from_min_size(Pos2::new(off[0], off[1]), egui::vec2(sz[0], sz[1]))
        } else {
            Rect::from_min_size(Pos2::ZERO, egui::vec2(tw, th))
        };
        let uv_of = |s: Rect| {
            Rect::from_min_max(
                Pos2::new(s.min.x / tw, s.min.y / th),
                Pos2::new(s.max.x / tw, s.max.y / th),
            )
        };
        let alpha = (1.0 - it) * me.alpha;
        let tint = me.tint;
        let grad = gradient.map(|g| Grad::new(d, g, rect));
        let col = move |p: Pos2| match &grad {
            Some(g) => {
                let (c, t) = g.at(p);
                rgba(
                    [
                        ic[0] * c[0] * tint[0],
                        ic[1] * c[1] * tint[1],
                        ic[2] * c[2] * tint[2],
                    ],
                    alpha * (1.0 - t),
                )
            }
            None => rgba([ic[0] * tint[0], ic[1] * tint[1], ic[2] * tint[2]], alpha),
        };
        let steps = if gradient.is_some() { 8 } else { 1 };
        let mut meshes: Vec<Mesh> = Vec::new();
        // ScaleType: Stretch 0, Slice 1, Tile 2, Fit 3, Crop 4.
        let scale_type = enum_u32(d.prop(r, "ScaleType")).unwrap_or(0);
        // Minified Stretch / Fit / Crop images sample a smaller copy (gui.rs `mip`).
        let tex = if matches!(scale_type, 0 | 3 | 4) && rect.width() > 0.0 && rect.height() > 0.0 {
            let ppp = self.ctx.pixels_per_point();
            let m = (src.width() / (rect.width() * ppp)).min(src.height() / (rect.height() * ppp));
            self.gui.mip(&id, tex, m)
        } else {
            tex
        };
        // UICorner rounds the image too (Stretch, Fit, Crop).
        let quad = |dst: Rect, uv: Rect| match corner.filter(|c| *c >= 0.5) {
            Some(c) => rounded_quad_mesh(dst, uv, tex, &col, c),
            None => quad_mesh(dst, uv, tex, &col, steps),
        };
        match scale_type {
            1 => {
                let sc = match d.prop(r, "SliceCenter") {
                    Some(Variant::Rect(rc)) => Rect::from_min_max(
                        Pos2::new(rc.min.x, rc.min.y),
                        Pos2::new(rc.max.x, rc.max.y),
                    ),
                    _ => Rect::from_min_size(Pos2::ZERO, src.size()),
                };
                let k = num(d, r, "SliceScale", 1.0) * me.scale;
                let xs = [
                    src.left(),
                    src.left() + sc.left(),
                    src.left() + sc.right(),
                    src.right(),
                ];
                let ys = [
                    src.top(),
                    src.top() + sc.top(),
                    src.top() + sc.bottom(),
                    src.bottom(),
                ];
                // A target smaller than its corners shrinks them in
                // proportion (each axis on its own); the middle gets nothing.
                let fit = |a: f32, b: f32, room: f32| {
                    let (a, b) = (a.max(0.0), b.max(0.0));
                    let f = if a + b > room && a + b > 0.0 { room.max(0.0) / (a + b) } else { 1.0 };
                    (a * f, b * f)
                };
                let (dl, dr) = fit(sc.left() * k, (src.width() - sc.right()) * k, rect.width());
                let (dt, db) = fit(sc.top() * k, (src.height() - sc.bottom()) * k, rect.height());
                let dx = [
                    rect.left(),
                    rect.left() + dl,
                    rect.right() - dr,
                    rect.right(),
                ];
                let dy = [
                    rect.top(),
                    rect.top() + dt,
                    rect.bottom() - db,
                    rect.bottom(),
                ];
                for j in 0..3 {
                    for i in 0..3 {
                        let dst = Rect::from_min_max(
                            Pos2::new(dx[i], dy[j]),
                            Pos2::new(dx[i + 1], dy[j + 1]),
                        );
                        if dst.width() > 0.0 && dst.height() > 0.0 {
                            meshes.push(quad_mesh(
                                dst,
                                uv_of(Rect::from_min_max(
                                    Pos2::new(xs[i], ys[j]),
                                    Pos2::new(xs[i + 1], ys[j + 1]),
                                )),
                                tex,
                                &col,
                                steps,
                            ));
                        }
                    }
                }
            }
            2 => {
                let ts = udim2(d, r, "TileSize");
                let tsz = egui::vec2(
                    (ts[0] * rect.width() + ts[1] * me.scale).max(1.0),
                    (ts[2] * rect.height() + ts[3] * me.scale).max(1.0),
                );
                let (nx, ny) = (
                    (rect.width() / tsz.x).ceil() as usize,
                    (rect.height() / tsz.y).ceil() as usize,
                );
                if nx * ny <= 4096 {
                    for j in 0..ny {
                        for i in 0..nx {
                            let dst = Rect::from_min_size(
                                Pos2::new(
                                    rect.left() + i as f32 * tsz.x,
                                    rect.top() + j as f32 * tsz.y,
                                ),
                                tsz,
                            );
                            let vis = dst.intersect(rect);
                            let s = Rect::from_min_max(
                                Pos2::new(
                                    src.left() + (vis.left() - dst.left()) / tsz.x * src.width(),
                                    src.top() + (vis.top() - dst.top()) / tsz.y * src.height(),
                                ),
                                Pos2::new(
                                    src.left() + (vis.right() - dst.left()) / tsz.x * src.width(),
                                    src.top() + (vis.bottom() - dst.top()) / tsz.y * src.height(),
                                ),
                            );
                            meshes.push(quad_mesh(vis, uv_of(s), tex, &col, steps));
                        }
                    }
                }
            }
            3 => {
                let k = (rect.width() / src.width()).min(rect.height() / src.height());
                meshes.push(quad(Rect::from_center_size(rect.center(), src.size() * k), uv_of(src)));
            }
            4 => {
                let k = (rect.width() / src.width()).max(rect.height() / src.height());
                let shown = Rect::from_center_size(
                    src.center(),
                    egui::vec2(rect.width() / k, rect.height() / k),
                );
                meshes.push(quad(rect, uv_of(shown)));
            }
            _ => meshes.push(quad(rect, uv_of(src))),
        }
        for m in meshes {
            self.push(Shape::Mesh(m.into()), me);
        }
    }

    /// Text with its font, size (TextScaled, UITextSizeConstraint), wrapping,
    /// truncation, alignment, rich text and outline. Returns TextBounds.
    fn text(
        &mut self,
        r: Ref,
        class: &str,
        rect: Rect,
        me: &Down,
        ui_stroke: Option<Ref>,
        fill_grad: Option<GradLut>,
        box_rect: Rect,
    ) -> Option<[f32; 2]> {
        let d = self.input.doc;
        let _text = rustudio_core::bench::scope("gui_text");
        let mut text = text_prop(d, r, "Text");
        let mut col = color(d, r, "TextColor3", [0.0, 0.0, 0.0]);
        let tt = num(d, r, "TextTransparency", 0.0);
        let focused = self.gui.focus == Some(r);
        if class == "TextBox" && text.is_empty() && !focused {
            text = text_prop(d, r, "PlaceholderText");
            col = color(d, r, "PlaceholderColor3", [0.7, 0.7, 0.7]);
        }
        let font = font_of(d, r);
        let family = self.gui.family(self.ctx, self.input.store, &font);
        let text = super::ligatures::apply(&family, text);
        let bold_face = font.1 >= 600;
        let rich = boolp(d, r, "RichText", false);
        let wrapped = boolp(d, r, "TextWrapped", false);
        let scaled = boolp(d, r, "TextScaled", false);
        let line_height = num(d, r, "LineHeight", 1.0);
        let tsc = child_of_class(d, r, "UITextSizeConstraint");
        let truncate = enum_u32(d.prop(r, "TextTruncate")).unwrap_or(0) != 0;
        // TextXAlignment: Left 0, Right 1, Center 2. TextYAlignment: Top 0, Center 1, Bottom 2.
        let xa = enum_u32(d.prop(r, "TextXAlignment")).unwrap_or(2);
        let ya = enum_u32(d.prop(r, "TextYAlignment")).unwrap_or(1);
        let tint = me.tint;
        let alpha = me.alpha;
        let ui_scale = me.scale;
        // Roblox sizes a font so a line (ascent + descent) is TextSize tall;
        // egui sizes by the em. This family's line height per em, measured once.
        let em_per_size = self.em_per_size(&family);
        note_text_face(self.ctx, &font, &family, em_per_size);
        // RichText <b>: the family's bold face (Studio draws SourceSans <b>
        // with SourceSansPro 700: checks/plugins/widgets sheet probe-wpixel).
        let bold_family = if rich && !bold_face && text.contains("<b") {
            let bf = (font.0.clone(), 700, font.2);
            let fam = self.gui.family(self.ctx, self.input.store, &bf);
            let em = self.em_per_size(&fam);
            Some((fam, em))
        } else {
            None
        };
        let runs = if rich {
            rich_runs(&text)
        } else {
            vec![Run {
                text: text.clone(),
                bold: false,
                italic: font.2,
                underline: false,
                strike: false,
                color: None,
                size: None,
                transparency: None,
            }]
        };
        let build = |size: f32, wrap: bool| -> LayoutJob {
            let mut job = LayoutJob::default();
            for run in &runs {
                let rs = run.size.map(|s| s * ui_scale).unwrap_or(size);
                let c = run.color.unwrap_or(col);
                let color = rgba(
                    [c[0] * tint[0], c[1] * tint[1], c[2] * tint[2]],
                    (1.0 - run.transparency.unwrap_or(tt)) * alpha,
                );
                let (fam, em) = match (&bold_family, run.bold) {
                    (Some((bf, be)), true) => (bf.clone(), *be),
                    _ => (family.clone(), em_per_size),
                };
                let mut f = TextFormat::simple(FontId::new(rs * em, fam), color);
                f.line_height = Some(rs * line_height);
                f.italics = run.italic;
                if run.underline {
                    f.underline = Stroke::new((rs / 14.0).max(1.0), color);
                }
                if run.strike {
                    f.strikethrough = Stroke::new((rs / 14.0).max(1.0), color);
                }
                if run.bold && !bold_face && bold_family.is_none() {
                    // Faux bold: egui has no synthetic bold, so a little extra spacing.
                    f.extra_letter_spacing = rs * 0.04;
                }
                super::color_glyphs::append(&mut job, &run.text, f);
            }
            job.wrap.max_width = if wrap || truncate {
                rect.width().max(1.0)
            } else {
                f32::INFINITY
            };
            if truncate {
                if !wrap {
                    job.wrap.max_rows = 1;
                }
                job.wrap.overflow_character = Some('…');
            }
            job.halign = match xa {
                0 => egui::Align::LEFT,
                1 => egui::Align::RIGHT,
                _ => egui::Align::Center,
            };
            // A right or centred "1 " keeps its space's width (Studio:
            // InCommand's line numbers, ref/corpus/screens/incommand-click-display.jpg).
            job.keep_trailing_whitespace = true;
            job.round_output_to_gui = false;
            job
        };
        let ctx = self.ctx;
        if text.is_empty() && !focused {
            return Some([0.0, 0.0]);
        }
        let mut size = num(d, r, "TextSize", 14.0) * me.scale * legacy_text_scale(&font);
        let simple = !rich && !truncate && single_line_ascii(&text);
        if scaled {
            let max = tsc
                .map(|c| num(d, c, "MaxTextSize", 100.0))
                .unwrap_or(100.0)
                .min(100.0)
                * me.scale.max(1.0);
            let Some(fitted) = self.gui.text_fit.size(ctx, rect.size(), max, simple, &build) else {
                self.gui.no_cache = Some("text deferred");
                return None;
            };
            size = fitted;
            if let Some(c) = tsc {
                size = size.max(num(d, c, "MinTextSize", 1.0) * me.scale);
            }
        } else if let Some(c) = tsc {
            size = size.clamp(
                num(d, c, "MinTextSize", 1.0) * me.scale,
                num(d, c, "MaxTextSize", 100.0) * me.scale,
            );
        }
        let galley = {
            let _p = rustudio_core::bench::scope("gui_text_layout");
            if scaled && simple {
                self.gui.text_fit.layout(ctx, build(size, false)).and_then(|single| {
                    if single.size().x + 1.0 <= rect.width().max(1.0) {
                        Some(single)
                    } else {
                        self.gui.text_fit.layout(ctx, build(size, true))
                    }
                })
            } else {
                self.gui.text_fit.layout(ctx, build(size, wrapped || scaled))
            }
        };
        let Some(galley) = galley else {
            self.gui.no_cache = Some("text deferred");
            return None;
        };
        let gs = galley.size();
        // The galley's rows are aligned around x by its halign.
        let x = match xa {
            0 => rect.left(),
            1 => rect.right(),
            _ => rect.center().x,
        };
        let y = match ya {
            0 => rect.top(),
            2 => rect.bottom() - gs.y,
            _ => rect.center().y - gs.y * 0.5,
        };
        let pos = Pos2::new(x, y);
        let mut outline: Option<(f32, Color32)> = None;
        let mut outline_grad: Option<GradLut> = None;
        let stroke_t = num(d, r, "TextStrokeTransparency", 1.0);
        if stroke_t < 1.0 {
            outline = Some((
                me.scale.max(1.0),
                rgba(
                    color(d, r, "TextStrokeColor3", [0.0, 0.0, 0.0]),
                    (1.0 - stroke_t) * (1.0 - tt) * me.alpha,
                ),
            ));
        }
        if let Some(s) = ui_stroke {
            let th = num(d, s, "Thickness", 1.0) * me.scale;
            let st = num(d, s, "Transparency", 0.0);
            if th > 0.0 && st < 1.0 {
                outline = Some((
                    th,
                    rgba(
                        color(d, s, "Color", [0.0, 0.0, 0.0]),
                        (1.0 - st) * (1.0 - tt) * me.alpha,
                    ),
                ));
                outline_grad = child_of_class(d, s, "UIGradient").map(|g| GradLut::new(d, g, box_rect));
            }
        }
        if let Some((th, c)) = outline {
            // Roblox strokes the outline once at its transparency; the eight
            // copies here overlap, so each takes a share of the alpha.
            let a = c.a() as f32 / 255.0;
            let per = 1.0 - (1.0 - a).powf(1.0 / mech::GUI_TEXT_STROKE_OVERLAP);
            let c = if a > 0.0 { c.gamma_multiply(per / a) } else { c };
            // A rich text run's own transparency fades its outline too
            // (ExpChat's message body holds the name again as an invisible
            // run to make room for it: it drew a black outline of the name).
            let fill_a = (1.0 - tt) * me.alpha;
            let fades = rich && fill_a > 0.0 && runs.iter().any(|run| run.transparency.is_some());
            let tinted = (outline_grad.is_some() || fades).then(|| {
                recolor_galley(&galley, pos, |p, v| {
                    let base = outline_grad.as_ref().map_or(c, |gl| gl.tint(p, c));
                    if fades {
                        base.gamma_multiply((v.a() as f32 / 255.0 / fill_a).min(1.0))
                    } else {
                        base
                    }
                })
            });
            for k in 0..8 {
                let a = std::f32::consts::TAU * k as f32 / 8.0;
                let at = pos + egui::vec2(a.cos(), a.sin()) * th;
                let ts = match &tinted {
                    Some(g) => TextShape::new(at, g.clone(), c),
                    None => {
                        let mut ts = TextShape::new(at, galley.clone(), c);
                        ts.override_text_color = Some(c);
                        ts
                    }
                };
                self.push(Shape::Text(ts), me);
            }
        }
        let fill = match &fill_grad {
            Some(gl) => recolor_galley(&galley, pos, |p, c| gl.tint(p, c)),
            None => galley.clone(),
        };
        self.push(Shape::Text(TextShape::new(pos, fill, rgba(col, (1.0 - tt) * me.alpha))), me);
        // Caret in the focused TextBox (blinking).
        if focused { self.gui.no_cache = Some("focused textbox"); }
        if focused && (self.ctx.input(|i| i.time) * 2.0) as i64 % 2 == 0 {
            let cx = pos.x + galley.rect.right() + 1.0;
            self.push(
                Shape::line_segment(
                    [
                        Pos2::new(cx, rect.center().y - size * 0.5),
                        Pos2::new(cx, rect.center().y + size * 0.5),
                    ],
                    Stroke::new(1.0, rgba(col, me.alpha)),
                ),
                me,
            );
        }
        Some([galley.rect.width(), galley.rect.height()])
    }

    /// A ViewportFrame: the renderer draws its parts (vf_render.rs, through
    /// the world's own scene and part shader) into a texture, composited
    /// here as one image tinted by ImageColor3 / ImageTransparency. The
    /// walk only records the render size; the camera and lighting are read
    /// each frame (`Gui::vf_requests`), so a moving camera or part doesn't
    /// walk the GUI again.
    ///
    /// Studio 0.747 (checks/viewport-frame-render/studio): the background is
    /// part of the image (the parts replace it; BackgroundTransparency is
    /// its alpha), ImageColor3 tints and ImageTransparency fades the whole
    /// image; a ViewportFrame in a SurfaceGui / BillboardGui under
    /// Workspace draws nothing at all (one in PlayerGui / StarterGui with
    /// an Adornee does).
    fn viewport_frame(&mut self, r: Ref, rect: Rect, me: &Down) {
        let d = self.input.doc;
        if d.workspace().is_some_and(|w| d.is_ancestor(w, r)) {
            return;
        }
        let it = num(d, r, "ImageTransparency", 0.0);
        let ic = color(d, r, "ImageColor3", [1.0, 1.0, 1.0]);
        // (no CurrentCamera: the background alone)
        if !matches!(d.prop(r, "CurrentCamera"), Some(Variant::Ref(c)) if d.exists(c)) {
            let bt = num(d, r, "BackgroundTransparency", 0.0);
            let bg = color(d, r, "BackgroundColor3", [1.0, 1.0, 1.0]);
            if bt < 1.0 {
                let c = rgba(std::array::from_fn(|k| bg[k] * ic[k] * me.tint[k]), (1.0 - bt) * (1.0 - it) * me.alpha);
                self.push(Shape::rect_filled(rect, 0.0, c), me);
            }
            return;
        }
        let size = [(rect.width() * self.ctx.pixels_per_point()).ceil().clamp(1.0, 4096.0) as u32,
                    (rect.height() * self.ctx.pixels_per_point()).ceil().clamp(1.0, 4096.0) as u32];
        self.gui.walk_vfs.push(r);
        self.gui.vf_live.insert(r);
        self.gui.vf_size.insert(r, size);
        let tex = self.gui.vf_tex.get(&r).filter(|t| t.1 == size).map(|t| t.0);
        self.gui.vf_walked.insert(r, tex);
        let Some(id) = tex else {
            // (not drawn at this size yet: walked again next frame)
            self.gui.no_cache = Some("viewport frame target");
            return;
        };
        let mut m = Mesh::with_texture(id);
        m.add_rect_with_uv(rect, Rect::from_min_max(Pos2::ZERO, Pos2::new(1.0, 1.0)), rgba(std::array::from_fn(|k| ic[k]*me.tint[k]), (1.0-it)*me.alpha));
        let mut inside = *me;
        inside.clip = me.clip.intersect(rect);
        self.push(Shape::Mesh(m.into()), &inside);
    }
}

/// UIFlexItem grow ratio: FlexMode Grow (1) and Fill (3)
/// grow by 1, Custom (4) by its GrowRatio; None (0) and Shrink (2) don't.
fn flex_grow(d: &Document, f: Ref) -> f32 {
    match enum_u32(d.prop(f, "FlexMode")).unwrap_or(0) {
        1 | 3 => 1.0,
        4 => num(d, f, "GrowRatio", 0.0).max(0.0),
        _ => 0.0,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn text_fit_matches_wrapped_probes() {
        let ctx = egui::Context::default();
        let mut output = ctx.run_ui(egui::RawInput::default(), |_| {
            let mut cache = TextFitCache::default();
            for text in ["Temple Guardian", "longwordwithoutbreaks", "é W", "title\nsecond line", "Lv. 100", "jjjj W"] {
                for width in [6.0, 39.4, 70.3, 70.8, 198.0] {
                    for height in [3.5, 11.3, 31.7, 49.0] {
                        for max in [20.0, 100.0] {
                            let build = |size: f32, wrap: bool| {
                                let mut job = LayoutJob::simple(
                                    text.into(), FontId::proportional(size), egui::Color32::WHITE,
                                    if wrap { width } else { f32::INFINITY },
                                );
                                job.sections[0].format.line_height = Some(size);
                                job.round_output_to_gui = false;
                                job
                            };
                            let mut lo = 1.0f32;
                            let mut hi = max;
                            for _ in 0..10 {
                                let mid = (lo + hi) * 0.5;
                                let actual = ctx.fonts_mut(|fonts| fonts.layout_job(build(mid, true))).size();
                                if actual.x <= width + 0.5 && actual.y <= height + 0.5 {
                                    lo = mid;
                                } else {
                                    hi = mid;
                                }
                            }
                            let fitted = cache.size(&ctx, egui::vec2(width, height), max, single_line_ascii(text), build);
                            assert_eq!(fitted, Some(lo.floor()), "{text:?} {width}×{height} max {max}");
                        }
                    }
                }
            }
        });
        output.textures_delta.clear();
    }

    #[test]
    fn measure_only_probes_lay_out_like_painted_text() {
        let ctx = egui::Context::default();
        ctx.run_ui(egui::RawInput::default(), |_| {}).textures_delta.clear();
        let new = |measure: bool| {
            let mut f = ctx.fonts(|f| Fonts::new(f.options().clone(), probe_font_definitions(f.definitions().clone())));
            f.set_measure_only(measure);
            f
        };
        let (mut painted, mut measured) = (new(false), new(true));
        // (fallback faces: two CJK ideographs and an emoji, made at run time)
        let fallback: String = [0x6f22, 0x5b57, 0x20, 0x1f642].into_iter().filter_map(char::from_u32).collect();
        let mixed = format!("é W {fallback}");
        for text in ["CAUTION\nWET FLOOR", "U", "Temple Guardian", mixed.as_str(), "longwordwithoutbreaks", "jjjj W", "Lv. 100"] {
            for size in [1.0, 7.3, 13.77, 31.5, 64.01, 99.9] {
                for (wrap, rows, ppp) in [(f32::INFINITY, usize::MAX, 1.0), (57.0, usize::MAX, 1.25), (57.0, 1, 1.0), (140.0, 2, 2.0)] {
                    for italics in [false, true] {
                        let mut job = LayoutJob::simple(text.into(), FontId::proportional(size), Color32::WHITE, wrap);
                        job.sections[0].format.line_height = Some(size * 1.1);
                        job.sections[0].format.italics = italics;
                        job.sections[0].format.extra_letter_spacing = size * 0.04;
                        job.wrap.max_rows = rows;
                        job.wrap.overflow_character = (rows != usize::MAX).then_some('…');
                        job.round_output_to_gui = false;
                        let a = painted.with_pixels_per_point(ppp).layout_job(job.clone());
                        let b = measured.with_pixels_per_point(ppp).layout_job(job);
                        let what = format!("{text:?} {size} {wrap} {rows} {ppp}");
                        assert_eq!(a.size(), b.size(), "{what}");
                        assert_eq!(a.intrinsic_size(), b.intrinsic_size(), "{what}");
                        assert_eq!(a.rows.len(), b.rows.len(), "{what}");
                        for (ra, rb) in a.rows.iter().zip(&b.rows) {
                            assert_eq!(ra.pos, rb.pos, "{what}");
                            assert_eq!(ra.size, rb.size, "{what}");
                            let pa: Vec<_> = ra.glyphs.iter().map(|g| (g.chr, g.pos, g.advance_width)).collect();
                            let pb: Vec<_> = rb.glyphs.iter().map(|g| (g.chr, g.pos, g.advance_width)).collect();
                            assert_eq!(pa, pb, "{what}");
                        }
                    }
                }
            }
        }
        assert!(measured.font_atlas_fill_ratio() < painted.font_atlas_fill_ratio());
    }

    /// The fonts keep text shaped across wrap widths (vendored epaint's
    /// ShapedCache) and shape plans per face: laying the same text out again
    /// at other widths, sizes and pixel densities gives what fresh fonts do.
    #[test]
    fn text_laid_out_again_at_new_widths_matches_fresh_fonts() {
        let ctx = egui::Context::default();
        ctx.run_ui(egui::RawInput::default(), |_| {}).textures_delta.clear();
        let new = || ctx.fonts(|f| Fonts::new(f.options().clone(), probe_font_definitions(f.definitions().clone())));
        let mut warm = new();
        let fallback: String = [0x6f22, 0x5b57, 0x20, 0x1f642].into_iter().filter_map(char::from_u32).collect();
        let mixed = format!("é W {fallback}");
        for pass in 0..3 {
            for text in ["CAUTION\nWET FLOOR", "Temple Guardian Lv. 5", mixed.as_str(), "longwordwithoutbreaks", "a b c d e f g"] {
                for size in [7.3, 13.77, 31.5] {
                    for ppp in [1.0, 1.25] {
                        for wrap in [300.0, 57.0, 140.0, 33.0, f32::INFINITY, 58.0] {
                            let mut job = LayoutJob::simple(text.into(), FontId::proportional(size), Color32::WHITE, wrap);
                            job.sections[0].format.line_height = Some(size * 1.1);
                            job.halign = egui::Align::Center;
                            job.round_output_to_gui = false;
                            let a = warm.with_pixels_per_point(ppp).layout_job(job.clone());
                            let b = new().with_pixels_per_point(ppp).layout_job(job);
                            let what = format!("pass {pass} {text:?} {size} {wrap} {ppp}");
                            assert_eq!(a.size(), b.size(), "{what}");
                            assert_eq!(a.intrinsic_size(), b.intrinsic_size(), "{what}");
                            assert_eq!(a.rows.len(), b.rows.len(), "{what}");
                            for (ra, rb) in a.rows.iter().zip(&b.rows) {
                                assert_eq!((ra.pos, ra.size), (rb.pos, rb.size), "{what}");
                                let ga: Vec<_> = ra.glyphs.iter().map(|g| (g.chr, g.pos, g.advance_width, g.line_height, g.font_ascent)).collect();
                                let gb: Vec<_> = rb.glyphs.iter().map(|g| (g.chr, g.pos, g.advance_width, g.line_height, g.font_ascent)).collect();
                                assert_eq!(ga, gb, "{what}");
                            }
                        }
                    }
                }
            }
            // (the caches drop entries a while after their last use)
            warm.begin_pass(warm.options().clone());
        }
    }

    #[test]
    fn text_budget_defers_misses_and_resumes_next_frame() {
        let ctx = egui::Context::default();
        let mut cache = TextFitCache::default();
        let job = |text: &str| {
            LayoutJob::simple(text.into(), FontId::proportional(20.0), Color32::WHITE, 100.0)
        };
        let mut output = ctx.run_ui(egui::RawInput::default(), |_| {
            cache.begin_frame(&ctx, true);
            assert!(cache.layout(&ctx, job("cached")).is_some());
            cache.spent[0] = Duration::from_millis(4);
            assert!(cache.layout(&ctx, job("cached")).is_some());
            assert!(cache.layout(&ctx, job("world")).is_none());
            cache.begin_frame(&ctx, true);
            assert!(cache.layout(&ctx, job("world")).is_none());
            cache.set_screen(true);
            assert!(cache.layout(&ctx, job("HUD")).is_some());
            assert_eq!(cache.take_deferred(), 2);
        });
        output.textures_delta.clear();
        let mut output = ctx.run_ui(egui::RawInput::default(), |_| {
            cache.begin_frame(&ctx, true);
            cache.set_screen(false);
            assert!(cache.layout(&ctx, job("world")).is_some());
        });
        output.textures_delta.clear();
    }

    #[test]
    fn rotate_about_centre() {
        let a = Aff::rotate_about(Pos2::new(10.0, 10.0), 90.0);
        let p = a.apply(Pos2::new(20.0, 10.0));
        assert!(
            (p.x - 10.0).abs() < 1e-4 && (p.y - 20.0).abs() < 1e-4,
            "{p:?}"
        );
    }
}

#[cfg(test)]
mod layout_tests {
    use super::*;
    use rbx_dom_weak::{InstanceBuilder, types::{UDim, UDim2, Enum, Vector2}};

    #[test]
    fn list_fill_expands_a_single_visible_item() {
        let mut doc = Document::new_place();
        let root = doc.dom.root_ref();
        let parent = doc.dom.insert(root, InstanceBuilder::new("Frame"));
        doc.dom.insert(parent, InstanceBuilder::new("UIListLayout")
            .with_property("FillDirection", Enum::from_u32(0))
            .with_property("HorizontalAlignment", Enum::from_u32(2))
            .with_property("HorizontalFlex", Enum::from_u32(1))
            .with_property("Padding", UDim::new(0.0, 8)));
        let size = UDim2::new(UDim::new(0.5, 0), UDim::new(0.0, 28));
        let reply = doc.dom.insert(parent, InstanceBuilder::new("TextButton")
            .with_property("Size", size)
            .with_property("AnchorPoint", Vector2::new(0.5, 0.0)));
        doc.dom.insert(parent, InstanceBuilder::new("TextButton")
            .with_property("Size", size)
            .with_property("Visible", false));

        let ctx = egui::Context::default();
        let mut gui = Gui::default();
        let cam = Camera::default();
        let mut store = AssetStore::default();
        let rect = Rect::from_min_size(Pos2::new(20.0, 0.0), egui::vec2(309.7, 30.0));
        let mut input = DrawIn { render_state: None, doc: &doc, cam: &cam, rect, store: &mut store, doc_dir: None, playing: false, fixed_step: true, roots: Default::default() };
        let mut out = ctx.run_ui(Default::default(), |ui| {
            let mut walk = Walk { gui: &mut gui, ctx: ui.ctx(), input: &mut input, items: Vec::new(), hits: Vec::new(), seq: 0, boxes: Vec::new(), global_z: false, abs_offset: [0.0; 2], screen: None, cur: Ref::none(), rec: Vec::new(), replaying: 0 };
            walk.children(parent, rect, Down { aff: Aff::ID, clip: rect, alpha: 1.0, tint: [1.0; 3], scale: 1.0, z: 0, turn: 0.0 });
            let reply_box = &walk.boxes.iter().find(|(r, _)| *r == reply).unwrap().1;
            assert!((reply_box.size[0] - rect.width()).abs() < 0.01);
            assert!((reply_box.pos[0] - rect.left()).abs() < 0.01);
            assert!((reply_box.size[1] - 28.0).abs() < 0.01);
        });
        out.textures_delta.clear();
    }

    #[test]
    fn grid_overrides_authored_tab_width() {
        let mut doc = Document::new_place();
        let root = doc.dom.root_ref();
        let parent = doc.dom.insert(root, InstanceBuilder::new("Frame"));
        let folder = doc.dom.insert(parent, InstanceBuilder::new("Folder"));
        let nested = doc.dom.insert(folder, InstanceBuilder::new("Folder"));
        let control = doc.dom.insert(nested, InstanceBuilder::new("TextButton").with_name("Travel").with_property("LayoutOrder", 100i32));
        let mut tabs = Vec::new();
        for name in ["Sound", "Titles"] {
            tabs.push(doc.dom.insert(parent, InstanceBuilder::new("Frame").with_name(name)
                .with_property("Size", UDim2::new(UDim::new(0.326, 0), UDim::new(1.0, 0)))));
        }
        doc.dom.insert(parent, InstanceBuilder::new("UIGridLayout")
            .with_property("CellSize", UDim2::new(UDim::new(0.49, 0), UDim::new(1.0, 0)))
            .with_property("CellPadding", UDim2::new(UDim::new(0.01, 0), UDim::new(0.0, 0)))
            .with_property("FillDirection", Enum::from_u32(1))
            .with_property("HorizontalAlignment", Enum::from_u32(0)));
        let ctx = egui::Context::default();
        let mut gui = Gui::default();
        let cam = Camera::default();
        let mut store = AssetStore::default();
        let rect = Rect::from_min_size(Pos2::ZERO, egui::vec2(1000.0, 40.0));
        let mut input = DrawIn { render_state: None, doc: &doc, cam: &cam, rect, store: &mut store, doc_dir: None, playing: false, fixed_step: true, roots: Default::default() };
        let mut out = ctx.run_ui(Default::default(), |ui| {
            let ctx = ui.ctx();
            let mut walk = Walk { gui: &mut gui, ctx, input: &mut input, items: Vec::new(), hits: Vec::new(), seq: 0, boxes: Vec::new(), global_z: false, abs_offset: [0.0; 2], screen: None, cur: Ref::none(), rec: Vec::new(), replaying: 0 };
            walk.children(parent, rect, Down { aff: Aff::ID, clip: rect, alpha: 1.0, tint: [1.0; 3], scale: 1.0, z: 0, turn: 0.0 });
            assert!(walk.boxes.iter().any(|(r, _)| *r == control), "nested Folder control was skipped");
            let boxes: Vec<_> = tabs.iter().map(|r| &walk.boxes.iter().find(|(k, _)| k == r).unwrap().1).collect();
            assert!((boxes[0].size[0] - 490.0).abs() < 0.01);
            assert!((boxes[1].pos[0] - boxes[0].pos[0] - 500.0).abs() < 0.01);
        });
        // (No renderer here: epaint asserts undelivered font-atlas updates.)
        out.textures_delta.clear();
    }

    /// UIFlexItem in a horizontal UIListLayout: a Grow child takes the room
    /// the others leave (Model Reflect's header: a 16 px button, a 28 px
    /// logo and a Size (0, 28) title that grows); Custom grows by GrowRatio.
    /// A wrapped AutomaticSize X label grows up to its parent's width and
    /// wraps there (Argon's error message).
    #[test]
    fn flex_grow_and_wrapped_automatic_x() {
        let mut doc = Document::new_place();
        let root = doc.dom.root_ref();
        let parent = doc.dom.insert(root, InstanceBuilder::new("Frame"));
        doc.dom.insert(parent, InstanceBuilder::new("UIListLayout").with_property("FillDirection", Enum::from_u32(0)));
        let fixed = doc.dom.insert(parent, InstanceBuilder::new("Frame").with_property("LayoutOrder", 0i32)
            .with_property("Size", UDim2::new(UDim::new(0.0, 40), UDim::new(0.0, 28))));
        let grow = doc.dom.insert(parent, InstanceBuilder::new("Frame").with_property("LayoutOrder", 1i32)
            .with_property("Size", UDim2::new(UDim::new(0.0, 0), UDim::new(0.0, 28))));
        doc.dom.insert(grow, InstanceBuilder::new("UIFlexItem").with_property("FlexMode", Enum::from_u32(1)));
        let custom = doc.dom.insert(parent, InstanceBuilder::new("Frame").with_property("LayoutOrder", 2i32)
            .with_property("Size", UDim2::new(UDim::new(0.0, 10), UDim::new(0.0, 28))));
        doc.dom.insert(custom, InstanceBuilder::new("UIFlexItem").with_property("FlexMode", Enum::from_u32(4)).with_property("GrowRatio", 3.0f32));
        let holder = doc.dom.insert(root, InstanceBuilder::new("Frame"));
        let label = doc.dom.insert(holder, InstanceBuilder::new("TextLabel")
            .with_property("Text", "word ".repeat(60))
            .with_property("TextWrapped", true)
            .with_property("AutomaticSize", Enum::from_u32(3)));
        let ctx = egui::Context::default();
        let mut gui = Gui::default();
        let cam = Camera::default();
        let mut store = AssetStore::default();
        let rect = Rect::from_min_size(Pos2::ZERO, egui::vec2(250.0, 30.0));
        let mut input = DrawIn { render_state: None, doc: &doc, cam: &cam, rect, store: &mut store, doc_dir: None, playing: false, fixed_step: true, roots: Default::default() };
        let mut out = ctx.run_ui(Default::default(), |ui| {
            let ctx = ui.ctx();
            let mut walk = Walk { gui: &mut gui, ctx, input: &mut input, items: Vec::new(), hits: Vec::new(), seq: 0, boxes: Vec::new(), global_z: false, abs_offset: [0.0; 2], screen: None, cur: Ref::none(), rec: Vec::new(), replaying: 0 };
            let dn = Down { aff: Aff::ID, clip: rect, alpha: 1.0, tint: [1.0; 3], scale: 1.0, z: 0, turn: 0.0 };
            walk.children(parent, rect, dn);
            walk.children(holder, rect, dn);
            let b = |r: Ref| walk.boxes.iter().find(|(k, _)| *k == r).unwrap().1.clone();
            // room 250 - 40 - 0 - 10 = 200: Grow 1 part (50), Custom 3 parts (150)
            assert!((b(fixed).size[0] - 40.0).abs() < 0.01);
            assert!((b(grow).size[0] - 50.0).abs() < 0.01 && (b(grow).pos[0] - 40.0).abs() < 0.01, "{:?}", b(grow).size);
            assert!((b(custom).size[0] - 160.0).abs() < 0.01 && (b(custom).pos[0] - 90.0).abs() < 0.01, "{:?}", b(custom).size);
            let l = b(label);
            assert!(l.size[0] <= 250.0 && l.size[0] > 200.0 && l.size[1] > 40.0, "wrapped at the parent's width: {:?}", l.size);
        });
        out.textures_delta.clear();
    }

    /// SizeConstraint RelativeYY / RelativeXX: both scales take the parent's
    /// height / width (DataStore Editor's check box: a 0.6-of-the-row square).
    #[test]
    fn size_constraint_relative_axes() {
        let mut doc = Document::new_place();
        let root = doc.dom.root_ref();
        let parent = doc.dom.insert(root, InstanceBuilder::new("Frame"));
        let yy = doc.dom.insert(parent, InstanceBuilder::new("Frame")
            .with_property("Size", UDim2::new(UDim::new(0.6, 0), UDim::new(1.0, 0)))
            .with_property("SizeConstraint", Enum::from_u32(2)));
        let xx = doc.dom.insert(parent, InstanceBuilder::new("Frame")
            .with_property("Size", UDim2::new(UDim::new(0.5, 0), UDim::new(0.25, 0)))
            .with_property("SizeConstraint", Enum::from_u32(1)));
        let ctx = egui::Context::default();
        let mut gui = Gui::default();
        let cam = Camera::default();
        let mut store = AssetStore::default();
        let rect = Rect::from_min_size(Pos2::ZERO, egui::vec2(200.0, 30.0));
        let mut input = DrawIn { render_state: None, doc: &doc, cam: &cam, rect, store: &mut store, doc_dir: None, playing: false, fixed_step: true, roots: Default::default() };
        let mut out = ctx.run_ui(Default::default(), |ui| {
            let ctx = ui.ctx();
            let mut walk = Walk { gui: &mut gui, ctx, input: &mut input, items: Vec::new(), hits: Vec::new(), seq: 0, boxes: Vec::new(), global_z: false, abs_offset: [0.0; 2], screen: None, cur: Ref::none(), rec: Vec::new(), replaying: 0 };
            walk.children(parent, rect, Down { aff: Aff::ID, clip: rect, alpha: 1.0, tint: [1.0; 3], scale: 1.0, z: 0, turn: 0.0 });
            let size = |r: Ref| walk.boxes.iter().find(|(k, _)| *k == r).unwrap().1.size;
            let a = size(yy);
            assert!((a[0] - 18.0).abs() < 0.01 && (a[1] - 30.0).abs() < 0.01, "RelativeYY {a:?}");
            let b = size(xx);
            assert!((b[0] - 100.0).abs() < 0.01 && (b[1] - 50.0).abs() < 0.01, "RelativeXX {b:?}");
        });
        out.textures_delta.clear();
    }

    /// A ScrollingFrame's recorded canvas can be worked out again for a new
    /// CanvasSize (a glide re-reads its range each frame, before the next
    /// layout): the same CanvasSize gives the laid-out canvas, padding
    /// included; a smaller one shrinks it, never below the content area;
    /// an AutomaticCanvasSize axis keeps what its children hold.
    #[test]
    fn scrolling_frame_canvas_for_new_canvas_size() {
        let mut doc = Document::new_place();
        let root = doc.dom.root_ref();
        let parent = doc.dom.insert(root, InstanceBuilder::new("Frame"));
        let sf = doc.dom.insert(parent, InstanceBuilder::new("ScrollingFrame")
            .with_property("Size", UDim2::new(UDim::new(0.0, 100), UDim::new(0.0, 100)))
            .with_property("CanvasSize", UDim2::new(UDim::new(0.0, 0), UDim::new(0.0, 400))));
        doc.dom.insert(sf, InstanceBuilder::new("UIPadding")
            .with_property("PaddingTop", UDim::new(0.0, 5))
            .with_property("PaddingBottom", UDim::new(0.0, 5)));
        let auto = doc.dom.insert(parent, InstanceBuilder::new("ScrollingFrame")
            .with_property("Size", UDim2::new(UDim::new(0.0, 100), UDim::new(0.0, 100)))
            .with_property("CanvasSize", UDim2::new(UDim::new(0.0, 0), UDim::new(0.0, 0)))
            .with_property("AutomaticCanvasSize", Enum::from_u32(2)));
        doc.dom.insert(auto, InstanceBuilder::new("Frame")
            .with_property("Size", UDim2::new(UDim::new(0.0, 50), UDim::new(0.0, 250))));
        let ctx = egui::Context::default();
        let mut gui = Gui::default();
        let cam = Camera::default();
        let mut store = AssetStore::default();
        let rect = Rect::from_min_size(Pos2::ZERO, egui::vec2(300.0, 300.0));
        let mut input = DrawIn { render_state: None, doc: &doc, cam: &cam, rect, store: &mut store, doc_dir: None, playing: false, fixed_step: true, roots: Default::default() };
        let mut out = ctx.run_ui(Default::default(), |ui| {
            let ctx = ui.ctx();
            let mut walk = Walk { gui: &mut gui, ctx, input: &mut input, items: Vec::new(), hits: Vec::new(), seq: 0, boxes: Vec::new(), global_z: false, abs_offset: [0.0; 2], screen: None, cur: Ref::none(), rec: Vec::new(), replaying: 0 };
            walk.children(parent, rect, Down { aff: Aff::ID, clip: rect, alpha: 1.0, tint: [1.0; 3], scale: 1.0, z: 0, turn: 0.0 });
            let b = walk.boxes.iter().find(|(k, _)| *k == sf).unwrap().1;
            let laid = b.canvas.unwrap();
            assert_eq!(b.canvas_for([0.0, 0.0, 0.0, 400.0]), Some(laid), "same CanvasSize");
            assert!((laid[1] - 410.0).abs() < 0.01, "400 + the padding: {laid:?}");
            let small = b.canvas_for([0.0, 0.0, 0.0, 160.0]).unwrap();
            assert!((small[1] - 170.0).abs() < 0.01, "shrunk: {small:?}");
            let tiny = b.canvas_for([0.0, 0.0, 0.0, 10.0]).unwrap();
            assert!((tiny[1] - 100.0).abs() < 0.01, "never below the frame: {tiny:?}");
            let a = walk.boxes.iter().find(|(k, _)| *k == auto).unwrap().1;
            let held = a.canvas_for([0.0, 0.0, 0.0, 0.0]).unwrap();
            assert!((held[1] - 250.0).abs() < 0.01 && Some(held) == a.canvas, "automatic Y holds its child: {held:?}");
        });
        out.textures_delta.clear();
    }

}

/// A TextSize of 0 (or a TextScaled label in a box with no height, or a NaN
/// from a 0/0 scale) reaches here as a 0 or non-finite font size, which
/// epaint's glyph cache rejects (debug assertion "Bad px_scale_factor: 0",
/// a crash in gui_walk layout). Roblox draws such text as nothing; a tiny
/// size draws nothing visible and keeps the layout's geometry (estimate).
fn sane_font_sizes(job: &mut LayoutJob) {
    for s in &mut job.sections {
        let size = &mut s.format.font_id.size;
        // (A tiny positive size, e.g. TextScaled in a box a hair tall,
        // scales to a 0 px factor in epaint, which refuses it too.)
        if !(size.is_finite() && *size >= rustudio_core::mech::GUI_MIN_FONT_SIZE) {
            *size = rustudio_core::mech::GUI_MIN_FONT_SIZE;
        }
    }
}
