//! Laying out and drawing one GUI's objects into canvas-space shapes.

use egui::epaint::TextShape;
use egui::text::{Fonts, LayoutJob, TextFormat};
use egui::{FontId, Shape, Stroke};
use std::time::{Duration, Instant};

use super::*;
use rustudio_core::mech;

fn safe_font_size(size: f32) -> f32 {
    if size.is_finite() {
        size.max(mech::GUI_MIN_FONT_SIZE)
    } else {
        mech::GUI_MIN_FONT_SIZE
    }
}

fn constrain_font_size(size: f32, min: f32, max: f32) -> f32 {
    let min = safe_font_size(min);
    size.clamp(min, safe_font_size(max).max(min))
}

/// Measures TextScaled against a canvas without laying out the same
/// single-line text again at every projected BillboardGui width.
#[derive(Default)]
pub(super) struct TextFitCache {
    sizes: TextMemo<(LayoutJob, FitSearch), 4096>,
    wrapped: TextMemo<(LayoutJob, egui::Vec2, bool), 4096>,
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

/// Whether epaint had to split a single word across rows because it exceeded
/// `max_width` (Roblox `TextScaled` shrinks the font size rather than breaking
/// a word mid-word).
pub(super) fn has_mid_word_break(galley: &egui::Galley) -> bool {
    galley.rows.windows(2).any(|w| {
        if w[0].ends_with_newline {
            return false;
        }
        let last = w[0].row.glyphs.last().map(|g| g.chr);
        let first = w[1].row.glyphs.first().map(|g| g.chr);
        match (last, first) {
            (Some(a), Some(b)) => !a.is_whitespace() && !b.is_whitespace() && !matches!(a, '-' | '/'),
            _ => false,
        }
    })
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

    fn probe(&mut self, ctx: &egui::Context, job: LayoutJob) -> Option<Arc<egui::Galley>> {
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
            None => (1.0, max.floor().max(1.0) + 1.0, 0),
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
            while step < 10 && lo + 1.0 < hi {
                let mid = ((lo + hi) * 0.5).floor();
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
                    (advance <= bounds.x.max(1.0).round() || single.y > bounds.y + 0.5).then_some((single, false))
                } else {
                    None
                };
                let (measured, mid_break) = if let Some(measured) = measured {
                    measured
                } else {
                    let mut job = build(mid, true);
                    job.wrap.max_width = job.wrap.max_width.round();
                    let key = text_job_hash(&job, ());
                    if let Some((_, measure, broken)) = self.wrapped.get(key).filter(|(cached, _, _)| *cached == job) {
                        (*measure, *broken)
                    } else {
                        let _p = rustudio_core::bench::scope("gui_text_fit_wrapped");
                        let galley = self.probe(ctx, job)?;
                        let measure = galley.size();
                        let broken = has_mid_word_break(&galley);
                        self.wrapped.insert(key, ((*galley.job).clone(), measure, broken));
                        (measure, broken)
                    }
                };
                if !mid_break && measured.x <= bounds.x + 0.5 && measured.y <= bounds.y + 0.5 {
                    lo = mid;
                } else {
                    hi = mid;
                }
                step += 1;
            }
            Some(lo.clamp(1.0, max.max(1.0)))
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
pub(crate) fn padded(d: &Document, r: Ref, rect: Rect, scale: f32) -> Rect {
    let Some(p) = child_of_class(d, r, "UIPadding") else {
        return rect;
    };
    let l = udim(d, p, "PaddingLeft");
    let rr = udim(d, p, "PaddingRight");
    let t = udim(d, p, "PaddingTop");
    let b = udim(d, p, "PaddingBottom");
    Rect::from_two_pos(
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

/// Roblox multiline text height: `LineHeight` scales the spacing between lines
/// only, not the final line's height (`(rows - 1) * size * line_height + size`).
pub(super) fn text_rows_height(rows: usize, size: f32, line_height: f32) -> f32 {
    if rows <= 1 {
        size
    } else {
        (rows - 1) as f32 * size * line_height + size
    }
}

/// Applies `UISizeConstraint` and `UIAspectRatioConstraint` children of a
/// `UIGridLayout` to its computed cell size `(cw, ch)`.
fn apply_cell_constraints(d: &Document, lay: Ref, mut cw: f32, mut ch: f32, scale: f32) -> (f32, f32) {
    if let Some(c) = child_of_class(d, lay, "UISizeConstraint") {
        let min = vec2(d, c, "MinSize", [0.0, 0.0]);
        let max = vec2(d, c, "MaxSize", [f32::INFINITY, f32::INFINITY]);
        if min[0] <= max[0] {
            cw = cw.clamp(min[0] * scale, max[0] * scale);
        }
        if min[1] <= max[1] {
            ch = ch.clamp(min[1] * scale, max[1] * scale);
        }
    }
    if let Some(c) = child_of_class(d, lay, "UIAspectRatioConstraint") {
        let ratio = num(d, c, "AspectRatio", 1.0).max(1e-3);
        if enum_u32(d.prop(c, "AspectType")) == Some(1) {
            if enum_u32(d.prop(c, "DominantAxis")) == Some(1) {
                cw = ch * ratio;
            } else {
                ch = cw / ratio;
            }
        } else if cw / ch.max(1e-3) > ratio {
            cw = ch * ratio;
        } else {
            ch = cw / ratio;
        }
    }
    (cw, ch)
}

/// Signed slot offset of page `i` relative to `cur` in a `Circular` `UIPageLayout`
/// of `n` pages. Moving forward (`dir > 0`) keeps `n/2` positive (`0,1,2,-1` for
/// `n=4`); initial or backward (`dir <= 0`) wraps `(n+1)/2` negative (`0,1,-2,-1`).
fn wrap_page_slot(i: usize, cur: usize, n: usize, dir: i8) -> i32 {
    if n == 0 {
        return 0;
    }
    let d = ((i as i32 - cur as i32).rem_euclid(n as i32)) as usize;
    let wrap_neg = if dir > 0 {
        d > n / 2
    } else {
        d >= (n + 1) / 2
    };
    if wrap_neg {
        d as i32 - n as i32
    } else {
        d as i32
    }
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
    pub page_layout: Option<Ref>,
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
    /// Pre-computed cell rects from a parent `UITableLayout`, consumed when
    /// walking each row/column strip's direct children.
    pub table_cells: HashMap<Ref, Rect>,
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
    /// rustudio extension (not Studio): Roblox's chat input bar only takes
    /// clicks on its one-line TextBox, a thin strip inside the pill's
    /// padding. A press anywhere on the pill (ExperienceChat's InputGroup)
    /// focuses that TextBox here. The send button is drawn later, so its own
    /// hit stays on top.
    fn chat_pill_hit(&mut self, d: &Document, tb: Ref) {
        let parent = |r: Ref| d.dom.get_by_ref(r).map(|i| i.parent()).filter(|p| p.is_some());
        let named = |r: Ref, n: &str| name_of(d, r) == n;
        let Some(tbc) = parent(tb).filter(|r| named(*r, "TextBoxContainer")) else { return };
        let Some(tc) = parent(tbc).filter(|r| named(*r, "TextContainer")) else { return };
        let Some(group) = parent(tc).filter(|r| named(*r, "InputGroup")) else { return };
        let mut a = Some(group);
        let mut in_chat = false;
        while let Some(r) = a {
            if named(r, "ExperienceChat") {
                in_chat = true;
                break;
            }
            a = parent(r);
        }
        if !in_chat {
            return;
        }
        let Some(pill) = self.hits.iter().rev().find(|h| h.r == group).cloned() else { return };
        self.hits.push(LocalHit { r: tb, button: false, textbox: true, sink: true, drag: false, painted: false, ..pill });
    }

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
        let placed = self.place_children(parent, content, dn.scale, false);
        // Sibling order: ZIndex, then child order.
        let mut sorted = kids;
        sorted.sort_by_key(|k| num(d, *k, "ZIndex", 1.0) as i32);
        for k in sorted {
            self.object_memo(k, content, placed.get(&k).copied(), dn);
        }
    }

    /// Computes layout slots and their content extent without visiting or painting the slots.
    pub(super) fn place_children(&mut self, parent: Ref, content: Rect, scale: f32, reading: bool) -> HashMap<Ref, Rect> {
        let d = self.input.doc;
        // UIGridStyleLayouts lay out only direct visible GuiObject children of
        // `parent` (not GuiObjects nested inside a Folder child, though those
        // are still rendered via `kids`).
        let direct_kids: Vec<Ref> = children(d, parent)
            .into_iter()
            .filter(|k| {
                d.class_of(*k).is_some_and(|c| doc::is_a(&c, "GuiObject"))
                    && boolp(d, *k, "Visible", true)
            })
            .collect();
        let list_layout = child_of_class(d, parent, "UIListLayout");
        let page_layout = child_of_class(d, parent, "UIPageLayout");
        let grid_layout = child_of_class(d, parent, "UIGridLayout");
        let table_layout = child_of_class(d, parent, "UITableLayout");
        let mut placed: HashMap<Ref, Rect> = HashMap::new();
        let mut from_table = false;
        if !self.table_cells.is_empty() {
            for k in &direct_kids {
                if let Some(rc) = self.table_cells.remove(k) {
                    placed.insert(*k, rc);
                    from_table = true;
                }
            }
        }
        if !from_table {
        if let Some(lay) = grid_layout.or(table_layout).or(page_layout).or(list_layout) {
            let mut order: Vec<Ref> = direct_kids;
            // SortOrder: Name 0, Custom 1, LayoutOrder 2.
            let sort_order = enum_u32(d.prop(lay, "SortOrder")).unwrap_or(2);
            match sort_order {
                0 => order.sort_by_key(|k| name_of(d, *k)),
                2 => order.sort_by_key(|k| num(d, *k, "LayoutOrder", 0.0) as i64),
                _ => {}
            }
            let halign = enum_u32(d.prop(lay, "HorizontalAlignment")).unwrap_or(1); // Center 0, Left 1, Right 2
            let valign = enum_u32(d.prop(lay, "VerticalAlignment")).unwrap_or(1); // Center 0, Top 1, Bottom 2
            let default_dir = if Some(lay) == page_layout { 0 } else { 1 };
            let vertical = enum_u32(d.prop(lay, "FillDirection")).unwrap_or(default_dir) == 1;
            if Some(lay) == grid_layout {
                let cell = udim2(d, lay, "CellSize");
                let pad = udim2(d, lay, "CellPadding");
                let (cw, ch) = apply_cell_constraints(
                    d,
                    lay,
                    cell[0] * content.width() + cell[1] * scale,
                    cell[2] * content.height() + cell[3] * scale,
                    scale,
                );
                let px = pad[0] * content.width() + pad[1] * scale;
                let py = pad[2] * content.height() + pad[3] * scale;
                let max_cells = num(d, lay, "FillDirectionMaxCells", 0.0) as usize;
                let start_corner = enum_u32(d.prop(lay, "StartCorner")).unwrap_or(0); // TL 0, TR 1, BL 2, BR 3
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
                let flip_x = matches!(start_corner, 1 | 3);
                let flip_y = matches!(start_corner, 2 | 3);
                for (i, k) in order.iter().enumerate() {
                    let (mut c, mut r) = if vertical {
                        (i / per_line, i % per_line)
                    } else {
                        (i % per_line, i / per_line)
                    };
                    if flip_x {
                        c = cols - 1 - c;
                    }
                    if flip_y {
                        r = rows - 1 - r;
                    }
                    placed.insert(
                        *k,
                        Rect::from_min_size(
                            Pos2::new(x0 + c as f32 * (cw + px), y0 + r as f32 * (ch + py)),
                            egui::vec2(cw, ch),
                        ),
                    );
                }
                let lay_box = GuiBox {
                    size: if n > 0 { [total_w, total_h] } else { [0.0, 0.0] },
                    canvas: Some(if n > 0 { [cols as f32, rows as f32] } else { [0.0, 0.0] }),
                    window: Some([cw / scale.max(1e-6), ch / scale.max(1e-6)]),
                    ..Default::default()
                };
                self.gui.layout.insert(lay, lay_box);
                self.boxes.push((lay, lay_box));
            } else if Some(lay) == table_layout {
                // MajorAxis: RowMajor 0, ColumnMajor 1.
                // FillEmptySpaceRows fills along the major (strip) axis;
                // FillEmptySpaceColumns fills along the minor (within-strip) axis.
                let major_col = enum_u32(d.prop(lay, "MajorAxis")).unwrap_or(0) == 1;
                let fill_cols = boolp(d, lay, "FillEmptySpaceColumns", false);
                let fill_rows = boolp(d, lay, "FillEmptySpaceRows", false);
                let pad = udim2(d, lay, "Padding");
                let pad_x = pad[0] * content.width() + pad[1] * scale;
                let pad_y = pad[2] * content.height() + pad[3] * scale;
                let mut strip_cells: Vec<(Ref, Vec<(Ref, egui::Vec2)>)> = Vec::with_capacity(order.len());
                let mut max_minor = 0usize;
                for &strip in &order {
                    self.gui.walk_kids.push(strip);
                    let mut cells: Vec<Ref> = children(d, strip)
                        .into_iter()
                        .filter(|c| {
                            d.class_of(*c).is_some_and(|k| doc::is_a(&k, "GuiObject"))
                                && boolp(d, *c, "Visible", true)
                        })
                        .collect();
                    match sort_order {
                        0 => cells.sort_by_key(|k| name_of(d, *k)),
                        2 => cells.sort_by_key(|k| num(d, *k, "LayoutOrder", 0.0) as i64),
                        _ => {}
                    }
                    let measured: Vec<(Ref, egui::Vec2)> = cells
                        .into_iter()
                        .map(|c| {
                            self.gui.walk_reads.push((c, self.cur));
                            (c, self.own_size(c, content, scale, 0))
                        })
                        .collect();
                    max_minor = max_minor.max(measured.len());
                    strip_cells.push((strip, measured));
                }
                let n_strips = strip_cells.len();
                let (mut col_w, mut row_h) = if major_col {
                    let mut cw = vec![0.0f32; n_strips];
                    let mut rh = vec![0.0f32; max_minor];
                    for (si, (_, cells)) in strip_cells.iter().enumerate() {
                        for (ci, (_, sz)) in cells.iter().enumerate() {
                            cw[si] = cw[si].max(sz.x);
                            rh[ci] = rh[ci].max(sz.y);
                        }
                    }
                    if fill_rows && n_strips > 0 {
                        let avail = content.width() - pad_x * n_strips.saturating_sub(1) as f32;
                        let sum: f32 = cw.iter().sum();
                        if sum > 0.0 && sum < avail {
                            let f = avail / sum;
                            for w in &mut cw { *w *= f; }
                        }
                    }
                    if fill_cols && max_minor > 0 {
                        let avail = content.height() - pad_y * max_minor.saturating_sub(1) as f32;
                        let sum: f32 = rh.iter().sum();
                        if sum > 0.0 && sum < avail {
                            let f = avail / sum;
                            for h in &mut rh { *h *= f; }
                        }
                    }
                    (cw, rh)
                } else {
                    let mut cw = vec![0.0f32; max_minor];
                    let mut rh = vec![0.0f32; n_strips];
                    for (si, (_, cells)) in strip_cells.iter().enumerate() {
                        for (ci, (_, sz)) in cells.iter().enumerate() {
                            cw[ci] = cw[ci].max(sz.x);
                            rh[si] = rh[si].max(sz.y);
                        }
                    }
                    if fill_cols && max_minor > 0 {
                        let avail = content.width() - pad_x * max_minor.saturating_sub(1) as f32;
                        let sum: f32 = cw.iter().sum();
                        if sum > 0.0 && sum < avail {
                            let f = avail / sum;
                            for w in &mut cw { *w *= f; }
                        }
                    }
                    if fill_rows && n_strips > 0 {
                        let avail = content.height() - pad_y * n_strips.saturating_sub(1) as f32;
                        let sum: f32 = rh.iter().sum();
                        if sum > 0.0 && sum < avail {
                            let f = avail / sum;
                            for h in &mut rh { *h *= f; }
                        }
                    }
                    (cw, rh)
                };
                let _ = (&mut col_w, &mut row_h);
                let total_w = col_w.iter().sum::<f32>() + pad_x * col_w.len().saturating_sub(1) as f32;
                let total_h = row_h.iter().sum::<f32>() + pad_y * row_h.len().saturating_sub(1) as f32;
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
                let mut col_x = Vec::with_capacity(col_w.len());
                let mut cx = x0;
                for &w in &col_w {
                    col_x.push(cx);
                    cx += w + pad_x;
                }
                let mut row_y = Vec::with_capacity(row_h.len());
                let mut ry = y0;
                for &h in &row_h {
                    row_y.push(ry);
                    ry += h + pad_y;
                }
                for (si, (strip, cells)) in strip_cells.iter().enumerate() {
                    let strip_rect = if major_col {
                        Rect::from_min_size(Pos2::new(col_x[si], y0), egui::vec2(col_w[si], total_h))
                    } else {
                        Rect::from_min_size(Pos2::new(x0, row_y[si]), egui::vec2(total_w, row_h[si]))
                    };
                    placed.insert(*strip, strip_rect);
                    if self.replaying == 0 {
                        self.gui.memo.remove(strip);
                    }
                    for (ci, (cell, _)) in cells.iter().enumerate() {
                        let cell_rect = if major_col {
                            Rect::from_min_size(Pos2::new(col_x[si], row_y[ci]), egui::vec2(col_w[si], row_h[ci]))
                        } else {
                            Rect::from_min_size(Pos2::new(col_x[ci], row_y[si]), egui::vec2(col_w[ci], row_h[si]))
                        };
                        self.table_cells.insert(*cell, cell_rect);
                    }
                }
                let lay_box = GuiBox {
                    size: if n_strips > 0 { [total_w, total_h] } else { [0.0, 0.0] },
                    ..Default::default()
                };
                self.gui.layout.insert(lay, lay_box);
                self.boxes.push((lay, lay_box));
            } else if Some(lay) == page_layout {
                let pad = udim(d, lay, "Padding");
                let gap = pad[0]
                    * if vertical {
                        content.height()
                    } else {
                        content.width()
                    }
                    + pad[1] * scale;
                let circular = boolp(d, lay, "Circular", false);
                let n = order.len();
                let (slot_w, slot_h) = (content.width(), content.height());
                let slot_main = if vertical { slot_h } else { slot_w };
                let stride = slot_main + gap;
                let page_state = super::share().pages.get(&lay).copied();
                let (from_idx, to_idx, alpha, dir) = match page_state {
                    Some((from, to, a, dir)) if n > 0 => {
                        let ti = order.iter().position(|&r| r == to).unwrap_or(0);
                        let fi = order.iter().position(|&r| r == from).unwrap_or(ti);
                        if a < 1.0 && fi != ti {
                            self.gui.no_cache = Some("page sliding");
                        }
                        (fi, ti, a, dir)
                    }
                    _ => (0, 0, 1.0, 0),
                };
                let cur_pos = if from_idx == to_idx || alpha >= 1.0 {
                    to_idx as f32
                } else if circular && n > 0 {
                    let step = if dir > 0 {
                        ((to_idx + n - from_idx) % n) as i32
                    } else if dir < 0 {
                        -(((from_idx + n - to_idx) % n) as i32)
                    } else {
                        wrap_page_slot(to_idx, from_idx, n, 0)
                    };
                    from_idx as f32 + step as f32 * alpha
                } else {
                    from_idx as f32 + (to_idx as f32 - from_idx as f32) * alpha
                };
                for (i, k) in order.iter().enumerate() {
                    let slot_off = if circular && n > 0 {
                        if alpha >= 1.0 || from_idx == to_idx {
                            wrap_page_slot(i, to_idx, n, dir) as f32
                        } else {
                            let mut delta = (i as f32 - cur_pos).rem_euclid(n as f32);
                            if dir > 0 {
                                if delta > (n / 2) as f32 {
                                    delta -= n as f32;
                                }
                            } else if delta >= ((n + 1) / 2) as f32 {
                                delta -= n as f32;
                            }
                            delta
                        }
                    } else {
                        i as f32 - cur_pos
                    };
                    let s = self.layout_item_size(*k, content, scale, reading);
                    let (sx, sy) = if vertical {
                        (content.left(), content.top() + slot_off * stride)
                    } else {
                        (content.left() + slot_off * stride, content.top())
                    };
                    let ax = match halign {
                        0 => (slot_w - s.x) * 0.5,
                        2 => slot_w - s.x,
                        _ => 0.0,
                    };
                    let ay = match valign {
                        0 => (slot_h - s.y) * 0.5,
                        2 => slot_h - s.y,
                        _ => 0.0,
                    };
                    placed.insert(*k, Rect::from_min_size(Pos2::new(sx + ax, sy + ay), s));
                }
                let acs = if n == 0 {
                    [0.0, 0.0]
                } else {
                    let total_main = n as f32 * slot_main + (n - 1) as f32 * gap;
                    if vertical {
                        [slot_w, total_main]
                    } else {
                        [total_main, slot_h]
                    }
                };
                let lay_box = GuiBox { size: acs, ..Default::default() };
                self.gui.layout.insert(lay, lay_box);
                self.boxes.push((lay, lay_box));
            } else {
                let pad = udim(d, lay, "Padding");
                let gap = pad[0]
                    * if vertical {
                        content.height()
                    } else {
                        content.width()
                    }
                    + pad[1] * scale;
                // The flex settings (list-flex-layouts): Wraps breaks the
                // list into lines at the fill axis' extent; HorizontalFlex /
                // VerticalFlex (UIFlexAlignment: None 0, Fill 1, SpaceAround
                // 2, SpaceBetween 3, SpaceEvenly 4) grow/shrink the items or
                // spread them along the fill axis, and spread the lines across
                // it; ItemLineAlignment (Automatic 0, Start 1, Center 2, End 3,
                // Stretch 4) sets the items within their line.
                let is_list = Some(lay) == list_layout;
                let wraps = is_list && boolp(d, lay, "Wraps", false);
                let hflex = if is_list { enum_u32(d.prop(lay, "HorizontalFlex")).unwrap_or(0) } else { 0 };
                let vflex = if is_list { enum_u32(d.prop(lay, "VerticalFlex")).unwrap_or(0) } else { 0 };
                let (main_flex, cross_flex) = if vertical { (vflex, hflex) } else { (hflex, vflex) };
                // Center 0, Left / Top 1, Right / Bottom 2 on each axis.
                let (main_align, cross_align) = if vertical { (valign, halign) } else { (halign, valign) };
                let line_align = match if is_list { enum_u32(d.prop(lay, "ItemLineAlignment")).unwrap_or(0) } else { 0 } {
                    // Automatic: the alignment across the fill axis — and
                    // Fill across stretches the items to their line.
                    0 if cross_flex == 1 => 4,
                    0 => match cross_align {
                        0 => 2,
                        2 => 3,
                        _ => 1,
                    },
                    a => a,
                };
                let (room_main, room_cross) = if vertical {
                    (content.height(), content.width())
                } else {
                    (content.width(), content.height())
                };
                let mut sizes: Vec<egui::Vec2> = order
                    .iter()
                    .map(|k| self.layout_item_size(*k, content, scale, reading))
                    .collect();
                // Per-item flex metadata: (grow_ratio, shrink_ratio, item_line_align, min_main, max_main).
                let flex_items: Vec<(f32, f32, u32, f32, f32)> = order.iter().map(|k| {
                    self.gui.walk_kids.push(*k);
                    let (gr, sr, ila) = match child_of_class(d, *k, "UIFlexItem") {
                        Some(item) => {
                            self.gui.walk_reads.push((item, self.cur));
                            let (g, s) = flex_ratios(d, item);
                            let a = enum_u32(d.prop(item, "ItemLineAlignment")).unwrap_or(0);
                            (g, s, a)
                        }
                        None => if main_flex == 1 { (1.0, 1.0, 0) } else { (0.0, 0.0, 0) },
                    };
                    let own_scale = child_of_class(d, *k, "UIScale")
                        .map(|s| num(d, s, "Scale", 1.0))
                        .unwrap_or(1.0);
                    let sc_factor = scale * own_scale;
                    let (mn, mx) = match child_of_class(d, *k, "UISizeConstraint") {
                        Some(sc) => {
                            self.gui.walk_reads.push((sc, self.cur));
                            let min_v = vec2(d, sc, "MinSize", [0.0, 0.0]);
                            let max_v = vec2(d, sc, "MaxSize", [f32::INFINITY, f32::INFINITY]);
                            let axis = usize::from(vertical);
                            if min_v[axis] <= max_v[axis] {
                                ((min_v[axis] * sc_factor).max(0.0), max_v[axis] * sc_factor)
                            } else {
                                (0.0, f32::INFINITY)
                            }
                        }
                        None => (0.0, f32::INFINITY),
                    };
                    (gr, sr, ila, mn, mx)
                }).collect();
                let lines = list_lines(&sizes, vertical, wraps, room_main, gap);
                // Across the fill axis: the lines, their gaps and the room
                // left, shared by the lines (Fill), spread between them, or
                // put before / after the block (the alignment).
                let n_lines = lines.len() as f32;
                let lines_sum: f32 = lines.iter().map(|l| l.across).sum();
                let cross_gaps = gap * lines.len().saturating_sub(1) as f32;
                let cross_total = lines_sum + cross_gaps;
                let cross_room = room_cross - cross_total;
                let cross_raw = room_cross - lines_sum;
                let (mut cross_at, cross_gap, line_grow) = match cross_flex {
                    1 if cross_room > 0.0 && n_lines > 0.0 => (0.0, gap, cross_room / n_lines),
                    1 => (0.0, gap, 0.0),
                    2 if cross_room > 0.0 && n_lines > 0.0 => {
                        let each = cross_raw / n_lines;
                        if each >= gap { (each * 0.5, each, 0.0) } else { (cross_room * 0.5, gap, 0.0) }
                    }
                    2 => (cross_room * 0.5, gap, 0.0),
                    3 if cross_room > 0.0 && lines.len() > 1 => (0.0, cross_raw / (n_lines - 1.0), 0.0),
                    3 => (0.0, gap, 0.0),
                    4 if cross_room > 0.0 && n_lines > 0.0 => {
                        let each = cross_raw / (n_lines + 1.0);
                        if each >= gap { (each, each, 0.0) } else { (cross_room * 0.5, gap, 0.0) }
                    }
                    4 => (cross_room * 0.5, gap, 0.0),
                    _ => (
                        match cross_align {
                            0 => cross_room * 0.5,
                            2 => cross_room,
                            _ => 0.0,
                        },
                        gap,
                        0.0,
                    ),
                };
                let mut acs_main = 0.0f32;
                let mut acs_cross = 0.0f32;
                for (li, line) in lines.iter().enumerate() {
                    let line_across = line.across + line_grow;
                    if li > 0 {
                        acs_cross += gap;
                    }
                    acs_cross += line_across;
                    let idx = line.items.clone();
                    let line_len = idx.len();
                    let count = line_len as f32;
                    let line_gaps = gap * line_len.saturating_sub(1) as f32;
                    let mut main_sizes: Vec<f32> = idx
                        .clone()
                        .map(|i| if vertical { sizes[i].y } else { sizes[i].x })
                        .collect();
                    let base_main = main_sizes.clone();
                    let room = room_main - line.along;
                    let grow_sum: f32 = idx.clone().map(|i| flex_items[i].0).sum();
                    let shrink_sum: f32 = idx.clone().map(|i| flex_items[i].1).sum();
                    if room > 0.0 && grow_sum > 0.0 {
                        let mut frozen = vec![false; line_len];
                        for _ in 0..line_len {
                            let cur_sum: f32 = main_sizes.iter().sum();
                            let rem = room_main - line_gaps - cur_sum;
                            if rem <= 1e-4 {
                                break;
                            }
                            let gsum: f32 = (0..line_len)
                                .filter(|&j| !frozen[j])
                                .map(|j| flex_items[idx.start + j].0)
                                .sum();
                            if gsum <= 0.0 {
                                break;
                            }
                            let mut any_clamped = false;
                            for j in 0..line_len {
                                if frozen[j] {
                                    continue;
                                }
                                let (gr, _, _, _, mx) = flex_items[idx.start + j];
                                if gr > 0.0 {
                                    let want = main_sizes[j] + rem * gr / gsum;
                                    if want > mx {
                                        main_sizes[j] = mx;
                                        frozen[j] = true;
                                        any_clamped = true;
                                    }
                                }
                            }
                            if !any_clamped {
                                for j in 0..line_len {
                                    if frozen[j] {
                                        continue;
                                    }
                                    let gr = flex_items[idx.start + j].0;
                                    if gr > 0.0 {
                                        main_sizes[j] += rem * gr / gsum;
                                    }
                                }
                                break;
                            }
                        }
                    } else if room < 0.0 && shrink_sum > 0.0 {
                        let mut frozen = vec![false; line_len];
                        for _ in 0..line_len {
                            let cur_sum: f32 = main_sizes.iter().sum();
                            let overflow = cur_sum + line_gaps - room_main;
                            if overflow <= 1e-4 {
                                break;
                            }
                            let wsum: f32 = (0..line_len)
                                .filter(|&j| !frozen[j])
                                .map(|j| base_main[j] * flex_items[idx.start + j].1)
                                .sum();
                            if wsum <= 0.0 {
                                break;
                            }
                            let mut any_clamped = false;
                            for j in 0..line_len {
                                if frozen[j] {
                                    continue;
                                }
                                let (_, sr, _, mn, _) = flex_items[idx.start + j];
                                let w = base_main[j] * sr;
                                if w > 0.0 {
                                    let want = main_sizes[j] - overflow * w / wsum;
                                    if want < mn {
                                        main_sizes[j] = mn;
                                        frozen[j] = true;
                                        any_clamped = true;
                                    }
                                }
                            }
                            if !any_clamped {
                                for j in 0..line_len {
                                    if frozen[j] {
                                        continue;
                                    }
                                    let w = base_main[j] * flex_items[idx.start + j].1;
                                    if w > 0.0 {
                                        main_sizes[j] -= overflow * w / wsum;
                                    }
                                }
                                break;
                            }
                        }
                    }
                    for (j, i) in idx.clone().enumerate() {
                        if vertical {
                            sizes[i].y = main_sizes[j];
                        } else {
                            sizes[i].x = main_sizes[j];
                        }
                    }
                    let final_items_sum: f32 = main_sizes.iter().sum();
                    let final_along = final_items_sum + line_gaps;
                    acs_main = acs_main.max(final_along);
                    let final_room = room_main - final_along;
                    let raw_free = room_main - final_items_sum;
                    let (main_at, main_gap) = match main_flex {
                        1 => (0.0, gap),
                        2 if final_room > 0.0 && count > 0.0 => {
                            let each = raw_free / count;
                            if each >= gap { (each * 0.5, each) } else { (final_room * 0.5, gap) }
                        }
                        2 => (final_room * 0.5, gap),
                        3 if final_room > 0.0 && count > 1.0 => (0.0, raw_free / (count - 1.0)),
                        3 => (0.0, gap),
                        4 if final_room > 0.0 && count > 0.0 => {
                            let each = raw_free / (count + 1.0);
                            if each >= gap { (each, each) } else { (final_room * 0.5, gap) }
                        }
                        4 => (final_room * 0.5, gap),
                        _ => (
                            match main_align {
                                0 => final_room * 0.5,
                                2 => final_room,
                                _ => 0.0,
                            },
                            gap,
                        ),
                    };
                    let mut at = main_at;
                    for i in idx {
                        let s = sizes[i];
                        let (along, across) = if vertical { (s.y, s.x) } else { (s.x, s.y) };
                        let item_la = match flex_items[i].2 {
                            0 => line_align,
                            a => a,
                        };
                        let (off, size) = match item_la {
                            4 => (0.0, line_across),
                            2 => ((line_across - across) * 0.5, across),
                            3 => (line_across - across, across),
                            _ => (0.0, across),
                        };
                        let r = if vertical {
                            Rect::from_min_size(
                                Pos2::new(content.left() + cross_at + off, content.top() + at),
                                egui::vec2(size, along),
                            )
                        } else {
                            Rect::from_min_size(
                                Pos2::new(content.left() + at, content.top() + cross_at + off),
                                egui::vec2(along, size),
                            )
                        };
                        placed.insert(order[i], r);
                        at += along + main_gap;
                    }
                    cross_at += line_across + cross_gap;
                }
                let acs = if lines.is_empty() {
                    [0.0, 0.0]
                } else if vertical {
                    [acs_cross, acs_main]
                } else {
                    [acs_main, acs_cross]
                };
                let lay_box = GuiBox { size: acs, ..Default::default() };
                self.gui.layout.insert(lay, lay_box);
                self.boxes.push((lay, lay_box));
            }
        }
        }
        placed
    }

    fn layout_item_size(&mut self, r: Ref, parent: Rect, scale: f32, reading: bool) -> egui::Vec2 {
        if reading {
            Sizer { d: self.input.doc, m: super::FaceText }.own_size(r, parent, scale, 0)
        } else {
            self.own_size(r, parent, scale, 0)
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
        let rich = boolp(d, r, "RichText", false);
        let font = font_of(d, r);
        let legacy_scale = legacy_text_scale(&font);
        let mut size = num(d, r, "TextSize", 14.0) * scale * legacy_scale;
        if let Some(c) = child_of_class(d, r, "UITextSizeConstraint") {
            size = constrain_font_size(
                size,
                num(d, c, "MinTextSize", 1.0) * scale,
                num(d, c, "MaxTextSize", 100.0) * scale,
            );
        }
        let size = safe_font_size(size);
        if text.is_empty() {
            return egui::vec2(0.0, size);
        }
        let family = self.gui.family(self.ctx, self.input.store, &font);
        let k = self.em_per_size(&family);
        note_text_face(self.ctx, &font, &family, k);
        let lh = num(d, r, "LineHeight", 1.0);
        let mut job = LayoutJob::default();
        let mut has_custom_size = false;
        if rich {
            let runs = rich_runs(&text);
            if runs.iter().all(|run| run.text.is_empty()) {
                return egui::vec2(0.0, size);
            }
            let bold_face = font.1 >= 600;
            let italic_face = font.2;
            let bold_family = if !bold_face && runs.iter().any(|r| r.bold && !r.italic) {
                let bf = (font.0.clone(), 700, font.2);
                let fam = self.gui.family(self.ctx, self.input.store, &bf);
                let em = self.em_per_size(&fam);
                note_text_face(self.ctx, &bf, &fam, em);
                Some((fam, em))
            } else {
                None
            };
            let italic_family = if !italic_face && runs.iter().any(|r| r.italic && !r.bold) {
                let itf = (font.0.clone(), font.1, true);
                let fam = self.gui.family(self.ctx, self.input.store, &itf);
                let em = self.em_per_size(&fam);
                note_text_face(self.ctx, &itf, &fam, em);
                Some((fam, em))
            } else {
                None
            };
            let bold_italic_family = if !(bold_face && italic_face) && runs.iter().any(|r| r.bold && r.italic) {
                let bif = (font.0.clone(), 700, true);
                let fam = self.gui.family(self.ctx, self.input.store, &bif);
                let em = self.em_per_size(&fam);
                note_text_face(self.ctx, &bif, &fam, em);
                Some((fam, em))
            } else {
                None
            };
            for run in &runs {
                if run.size.is_some() {
                    has_custom_size = true;
                }
                let rs = safe_font_size(run.size.map(|s| s * scale * legacy_scale).unwrap_or(size));
                let (fam, em) = match (run.bold, run.italic) {
                    (true, true) => bold_italic_family
                        .as_ref()
                        .or(bold_family.as_ref())
                        .or(italic_family.as_ref())
                        .cloned()
                        .unwrap_or_else(|| (family.clone(), k)),
                    (true, false) => bold_family.clone().unwrap_or_else(|| (family.clone(), k)),
                    (false, true) => italic_family.clone().unwrap_or_else(|| (family.clone(), k)),
                    (false, false) => (family.clone(), k),
                };
                let shaped = apply_font_ligatures(&fam, &run.text);
                let mut f = TextFormat::simple(FontId::new(safe_font_size(rs * em), fam), Color32::WHITE);
                f.studio_text_size = Some(rs);
                f.line_height = Some(rs * lh);
                job.append(shaped.as_ref(), 0.0, f);
            }
        } else {
            let shaped = apply_font_ligatures(&family, &text);
            let mut f = TextFormat::simple(FontId::new(safe_font_size(size * k), family), Color32::WHITE);
            f.studio_text_size = Some(size);
            f.line_height = Some(size * lh);
            job.append(shaped.as_ref(), 0.0, f);
        }
        job.wrap.max_width = wrap.max(1.0);
        job.keep_trailing_whitespace = true;
        job.round_output_to_gui = false;
        let g = self.gui.text_fit.layout_now(self.ctx, job);
        let h = if has_custom_size && (lh - 1.0).abs() < 1e-4 {
            g.rect.height().ceil()
        } else {
            text_rows_height(g.rows.len(), size, lh)
        };
        egui::vec2(g.rect.width().ceil(), h)
    }

    fn content_size(&mut self, r: Ref, size: egui::Vec2, scale: f32, auto: u32, depth: usize) -> egui::Vec2 {
        let d = self.input.doc;
        Sizer { d, m: self }.content_size(r, size, scale, auto, depth)
    }

    fn content_size_in(&mut self, r: Ref, size: egui::Vec2, scale: f32, auto: u32, depth: usize, room: f32) -> egui::Vec2 {
        let d = self.input.doc;
        Sizer { d, m: self }.content_size_in(r, size, scale, auto, depth, room)
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
        let mut ext = egui::Vec2::ZERO;
        if d.class_of(r)
            .is_some_and(|c| matches!(c.as_str(), "TextLabel" | "TextButton" | "TextBox"))
        {
            // A wrapped text growing on X grows up to the room its parent
            // leaves, then wraps (Studio: Argon's error message, an
            // AutomaticSize XY TextWrapped label in a ScrollingFrame, wraps in
            // three lines in its 302 px window,
            // checks/plugins/ref/corpus/screens/argon-load-popup.jpg).
            let sz = udim2(d, r, "Size");
            let wrap = if !boolp(d, r, "TextWrapped", false) {
                f32::INFINITY
            } else if auto & 1 != 0 {
                if sz[0] > 0.0 && inner.width() > 0.0 {
                    let avail = (room - pl - pr).max(0.0);
                    if avail.is_finite() { inner.width().min(avail) } else { inner.width() }
                } else {
                    (room - pl - pr).max(inner.width())
                }
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
            let sizes: Vec<egui::Vec2> = kids
                .iter()
                .map(|k| self.auto_child_size(*k, inner, scale, auto, depth + 1))
                .collect();
            // A wrapping list breaks at the fill axis' extent — unless that
            // axis is the one growing (nothing to wrap at: one line).
            let main_auto = if vertical { auto & 2 != 0 } else { auto & 1 != 0 };
            let room = if vertical { inner.height() } else { inner.width() };
            let lines = list_lines(&sizes, vertical, boolp(d, lay, "Wraps", false) && !main_auto, room, gap);
            let along = lines.iter().map(|l| l.along).fold(0.0f32, f32::max);
            let across = lines.iter().map(|l| l.across).sum::<f32>() + gap * lines.len().saturating_sub(1) as f32;
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
            let (cw, ch) = apply_cell_constraints(
                d,
                lay,
                cell[0] * inner.width() + cell[1] * scale,
                cell[2] * inner.height() + cell[3] * scale,
                scale,
            );
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
        } else if let Some(lay) = child_of_class(d, r, "UITableLayout") {
            let major_col = enum_u32(d.prop(lay, "MajorAxis")).unwrap_or(0) == 1;
            let sort_order = enum_u32(d.prop(lay, "SortOrder")).unwrap_or(2);
            let pad = udim2(d, lay, "Padding");
            let pad_x = pad[0] * inner.width() + pad[1] * scale;
            let pad_y = pad[2] * inner.height() + pad[3] * scale;
            let mut strips = kids;
            match sort_order {
                0 => strips.sort_by_key(|k| name_of(d, *k)),
                2 => strips.sort_by_key(|k| num(d, *k, "LayoutOrder", 0.0) as i64),
                _ => {}
            }
            let mut max_minor = 0usize;
            let mut rows_cells: Vec<Vec<egui::Vec2>> = Vec::with_capacity(strips.len());
            for strip in &strips {
                let mut cells: Vec<Ref> = children(d, *strip)
                    .into_iter()
                    .filter(|c| {
                        d.class_of(*c).is_some_and(|k| doc::is_a(&k, "GuiObject"))
                            && boolp(d, *c, "Visible", true)
                    })
                    .collect();
                match sort_order {
                    0 => cells.sort_by_key(|k| name_of(d, *k)),
                    2 => cells.sort_by_key(|k| num(d, *k, "LayoutOrder", 0.0) as i64),
                    _ => {}
                }
                let szs: Vec<egui::Vec2> = cells
                    .iter()
                    .map(|c| self.auto_child_size(*c, inner, scale, auto, depth + 1))
                    .collect();
                max_minor = max_minor.max(szs.len());
                rows_cells.push(szs);
            }
            let (col_w, row_h) = if major_col {
                let mut cw = vec![0.0f32; rows_cells.len()];
                let mut rh = vec![0.0f32; max_minor];
                for (si, cells) in rows_cells.iter().enumerate() {
                    for (ci, sz) in cells.iter().enumerate() {
                        cw[si] = cw[si].max(sz.x);
                        rh[ci] = rh[ci].max(sz.y);
                    }
                }
                (cw, rh)
            } else {
                let mut cw = vec![0.0f32; max_minor];
                let mut rh = vec![0.0f32; rows_cells.len()];
                for (si, cells) in rows_cells.iter().enumerate() {
                    for (ci, sz) in cells.iter().enumerate() {
                        cw[ci] = cw[ci].max(sz.x);
                        rh[si] = rh[si].max(sz.y);
                    }
                }
                (cw, rh)
            };
            if !rows_cells.is_empty() {
                ext = ext.max(egui::vec2(
                    col_w.iter().sum::<f32>() + pad_x * col_w.len().saturating_sub(1) as f32,
                    row_h.iter().sum::<f32>() + pad_y * row_h.len().saturating_sub(1) as f32,
                ));
            }
        } else {
            for k in kids {
                let s = self.auto_child_size(k, inner, scale, auto, depth + 1);
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
    pub(super) fn auto_child_size(&mut self, k: Ref, inner: Rect, scale: f32, auto: u32, depth: usize) -> egui::Vec2 {
        let mut s = self.own_size_in_auto(k, inner, scale, depth, auto).max(egui::Vec2::ZERO);
        if depth >= 24 {
            return s;
        }
        let sz = udim2(self.d, k, "Size");
        let grows = |axis: u32, sc: f32| auto & axis != 0 && sc != 0.0;
        let (gx, gy) = (grows(1, sz[0]), grows(2, sz[2]));
        if gx || gy {
            let axes = (gx as u32) | ((gy as u32) << 1);
            let c = self.content_size_in(k, s, scale, axes, depth, inner.width());
            if gx {
                s.x = s.x.max(c.x);
            }
            if gy {
                s.y = s.y.max(c.y);
            }
        }
        s
    }

    /// A GuiObject's size from Size, AutomaticSize and its constraints (and UIScale).
    pub(super) fn own_size(&mut self, r: Ref, parent: Rect, scale: f32, depth: usize) -> egui::Vec2 {
        self.own_size_signed(r, parent, scale, depth).max(egui::Vec2::ZERO)
    }

    /// `own_size` before negative sizes are clamped: Studio reports those
    /// (AbsoluteSize -1, -1 for a Size of -1 px offset, the box placed with
    /// it; Emitter2D's jitter fix makes them for particles near their end).
    pub(super) fn own_size_signed(&mut self, r: Ref, parent: Rect, scale: f32, depth: usize) -> egui::Vec2 {
        self.own_size_in_auto(r, parent, scale, depth, 0)
    }

    fn own_size_in_auto(&mut self, r: Ref, parent: Rect, scale: f32, depth: usize, parent_auto: u32) -> egui::Vec2 {
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
        // Evaluation order verified against Studio:
        // Base Size (with SizeConstraint) -> UISizeConstraint -> UIAspectRatioConstraint
        // -> AutomaticSize -> own UIScale.
        if let Some(c) = child_of_class(d, r, "UISizeConstraint") {
            let min = vec2(d, c, "MinSize", [0.0, 0.0]);
            let max = vec2(d, c, "MaxSize", [f32::INFINITY, f32::INFINITY]);
            if min[0] <= max[0] {
                size.x = size.x.clamp(min[0] * scale, max[0] * scale);
            }
            if min[1] <= max[1] {
                size.y = size.y.clamp(min[1] * scale, max[1] * scale);
            }
        }
        if let Some(c) = child_of_class(d, r, "UIAspectRatioConstraint") {
            let ratio = num(d, c, "AspectRatio", 1.0).max(1e-3);
            // AspectType: FitWithinMaxSize 0, ScaleWithParentSize 1; DominantAxis: Width 0, Height 1.
            // When measuring inside an AutomaticSize parent, a Scale on the parent's
            // auto-growing axis isn't constrained yet (e.g. a 1:1 square thumbnail
            // with Size {1,0},{1,0} in an AutomaticSize.Y card), so derive that axis
            // from the known fixed axis instead of collapsing to 0.
            let auto_gx = parent_auto & 1 != 0 && s[0] != 0.0;
            let auto_gy = parent_auto & 2 != 0 && s[2] != 0.0;
            if enum_u32(d.prop(c, "AspectType")) == Some(1) {
                if enum_u32(d.prop(c, "DominantAxis")) == Some(1) {
                    size.x = size.y * ratio;
                } else {
                    size.y = size.x / ratio;
                }
                if parent.width() > 0.0 && parent_auto & 1 == 0 && size.x > parent.width() {
                    size.x = parent.width();
                    size.y = size.x / ratio;
                }
                if parent.height() > 0.0 && parent_auto & 2 == 0 && size.y > parent.height() {
                    size.y = parent.height();
                    size.x = size.y * ratio;
                }
            } else if auto_gy && !auto_gx {
                size.y = size.x / ratio;
            } else if auto_gx && !auto_gy {
                size.x = size.y * ratio;
            } else if size.x / size.y.max(1e-3) > ratio {
                size.x = size.y * ratio;
            } else {
                size.y = size.x / ratio;
            }
        }
        // AutomaticSize: None 0, X 1, Y 2, XY 3.
        let auto = enum_u32(d.prop(r, "AutomaticSize")).unwrap_or(0);
        if auto != 0 && depth < 24 {
            let px = udim2(d, r, "Position");
            let anchor = vec2(d, r, "AnchorPoint", [0.0, 0.0]);
            let pos_x = px[0] * parent.width() + px[1] * scale;
            let room = if anchor[0] > 0.5 {
                pos_x / anchor[0]
            } else {
                (parent.width() - pos_x) / (1.0 - anchor[0]).max(1e-3)
            }
            .min(parent.width())
            .max(0.0);
            let c = self.content_size_in(r, size, scale, auto, depth, room);
            if auto & 1 != 0 {
                size.x = size.x.max(c.x);
            }
            if auto & 2 != 0 {
                size.y = size.y.max(c.y);
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
            ..Default::default()
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
        // UIShadow: the box's drop shadow, under its background.
        self.shadow(r, rect, corner.unwrap_or(0.0), &me);
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
        let text_class = matches!(class.as_str(), "TextLabel" | "TextButton" | "TextBox");
        // UIStroke: around the box (Border), or around the text (Contextual, on text objects).
        let stroke = child_of_class(d, r, "UIStroke");
        let stroke_on_text = stroke.is_some_and(|s| {
            text_class && enum_u32(d.prop(s, "ApplyStrokeMode")).unwrap_or(0) == 0
        });
        let border_stroke = stroke.filter(|_| !stroke_on_text).and_then(|s| {
            let th = num(d, s, "Thickness", 1.0) * me.scale;
            let st = num(d, s, "Transparency", 0.0);
            (th > 0.0 && st < 1.0).then_some((s, th, st))
        });
        let feather = 1.0 / self.ctx.pixels_per_point().max(1e-3);
        let has_corner = corner.is_some_and(|c| c >= 0.5);
        if bg_t < 1.0 && rect.width() > 0.0 && rect.height() > 0.0 {
            let a = (1.0 - bg_t) * me.alpha;
            let tint = me.tint;
            // When an opaque outer UIStroke borders the fill, extend the fill under the stroke's
            // inner AA edge by half a pixel so the backdrop never bleeds through the seam.
            let seam_overlap = match border_stroke {
                Some((_, th, st)) if st <= 0.01 && (has_corner || !me.aff.is_id()) => {
                    (0.5 * feather).min(th * 0.5)
                }
                _ => 0.0,
            };
            let fill_rect = if seam_overlap > 0.0 { rect.expand(seam_overlap) } else { rect };
            let fill_r = corner.map(|c| {
                let base = c.min(rect.width() * 0.5).min(rect.height() * 0.5).max(0.0);
                if base >= 0.5 { base + seam_overlap } else { 0.0 }
            }).unwrap_or(0.0);
            let fill_feather = if has_corner || !me.aff.is_id() { feather } else { 0.0 };
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
                    if has_corner {
                        fill_rounded_aa_mesh(
                            fill_rect,
                            fill_r,
                            fill_feather,
                            None,
                            egui::TextureId::default(),
                            &f,
                            true,
                        )
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
                    fill_rounded_aa_mesh(
                        fill_rect,
                        fill_r,
                        fill_feather,
                        None,
                        egui::TextureId::default(),
                        &|_| c,
                        false,
                    )
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
        if let Some((s, th, st)) = border_stroke {
            let c = rgba(color(d, s, "Color", [0.0, 0.0, 0.0]), (1.0 - st) * me.alpha);
            let stroke_feather = if has_corner || !me.aff.is_id() || th < 1.0 { feather } else { 0.0 };
            let mesh = match child_of_class(d, s, "UIGradient") {
                Some(g) => {
                    let gl = GradLut::new(d, g, rect);
                    stroke_rounded_aa_mesh(rect, corner, th, stroke_feather, &|p| gl.tint(p, c))
                }
                None => stroke_rounded_aa_mesh(rect, corner, th, stroke_feather, &|_| c),
            };
            self.push(Shape::Mesh(mesh.into()), &me);
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
            let (tb, fits) = self.text(r, &class, text_rect, &me, stroke.filter(|_| stroke_on_text), fill_grad, rect);
            gb.text = tb;
            gb.text_fits = fits;
        }
        // ParticleEmitter2D children, over the object, under its children.
        self.particles(r, rect, &me);
        // GuiService.SelectedCoreObject / SelectedObject (keyboard / gamepad selection).
        if let Some((sel, is_core)) = self.selected_object() {
            if sel == r {
                self.draw_selection_cursor(r, is_core, rect, corner, feather, &me);
            }
        }

        let interactable = boolp(d, r, "Interactable", true);
        let textbox = interactable && class == "TextBox";
        let scrolling = class == "ScrollingFrame";
        let scrolling_enabled = interactable && scrolling && boolp(d, r, "ScrollingEnabled", true);
        let page_layout = interactable
            .then(|| {
                child_of_class(d, r, "UIPageLayout")
                    .filter(|pl| boolp(d, *pl, "ScrollWheelInputEnabled", true))
            })
            .flatten();
        let active = boolp(d, r, "Active", false);
        if !self.input.playing || interactable || active {
            self.hits.push(LocalHit {
                r,
                rect,
                aff: me.aff,
                clip: (dn.clip != Rect::EVERYTHING).then_some(dn.clip),
                button: interactable && is_button,
                textbox,
                sink: (interactable && (is_button || textbox)) || active,
                scrolling: scrolling_enabled,
                page_layout,
                drag: interactable
                    && (boolp(d, r, "Draggable", false)
                        || child_of_class(d, r, "UIDragDetector")
                            .is_some_and(|dd| boolp(d, dd, "Enabled", true))),
                painted: self.items.len() > items_before,
            });
        }
        if textbox {
            self.chat_pill_hit(d, r);
        }
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
            // CanvasSize.Scale multiplies the ScrollingFrame's parent content size.
            let cs = match d.prop(r, "CanvasSize") {
                Some(Variant::UDim2(u)) => [u.x.scale, u.x.offset as f32, u.y.scale, u.y.offset as f32],
                _ => [0.0, 0.0, 2.0, 0.0],
            };
            let mut raw = egui::vec2(
                cs[0] * parent.width() + cs[1] * me.scale,
                cs[2] * parent.height() + cs[3] * me.scale,
            );
            // AutomaticCanvasSize (None 0, X 1, Y 2, XY 3) grows the canvas to hold
            // its children + UIPadding.
            let acs = enum_u32(d.prop(r, "AutomaticCanvasSize")).unwrap_or(0);
            let held = if acs != 0 {
                self.content_size_in(r, rect.size(), me.scale, acs, 0, rect.width())
            } else {
                egui::Vec2::ZERO
            };
            if acs & 1 != 0 {
                raw.x = raw.x.max(held.x);
            }
            if acs & 2 != 0 {
                raw.y = raw.y.max(held.y);
            }
            let th = num(d, r, "ScrollBarThickness", 12.0) * me.scale;
            // Studio reserves a 1 px separator beside a non-zero scrollbar.
            let gutter = if th > 0.0 { th + 1.0 * me.scale } else { 0.0 };
            // ScrollingDirection: X 1, Y 2, XY 4 (default).
            let sd = enum_u32(d.prop(r, "ScrollingDirection")).unwrap_or(4);
            let can_x = sd != 2;
            let can_y = sd != 1;
            let show_v = can_y && raw.y > rect.height() + 0.5;
            let show_h = can_x && raw.x > rect.width() + 0.5;
            // ScrollBarInset: None 0, ScrollBar 1, Always 2.
            let vi = enum_u32(d.prop(r, "VerticalScrollBarInset")).unwrap_or(0);
            let hi = enum_u32(d.prop(r, "HorizontalScrollBarInset")).unwrap_or(0);
            let vpos = enum_u32(d.prop(r, "VerticalScrollBarPosition")).unwrap_or(0); // Right 0, Left 1
            let inset_v = if vi == 2 || (vi == 1 && show_v) { gutter } else { 0.0 };
            let inset_h = if hi == 2 || (hi == 1 && show_h) { gutter } else { 0.0 };
            let auto_x = if acs & 1 != 0 { held.x + if inset_v > 0.0 { th } else { 0.0 } } else { 0.0 };
            let auto_y = if acs & 2 != 0 { held.y + if inset_h > 0.0 { th } else { 0.0 } } else { 0.0 };
            if acs & 1 != 0 {
                raw.x = raw.x.max(auto_x);
            }
            if acs & 2 != 0 {
                raw.y = raw.y.max(auto_y);
            }
            let floor = egui::vec2(
                (rect.width() - inset_v).max(0.0).max(auto_x),
                (rect.height() - inset_h).max(0.0).max(auto_y),
            );
            let canvas = raw.max(floor);
            // AbsoluteWindowSize subtracts the gutter whenever a scrollbar shows,
            // independent of VerticalScrollBarInset / HorizontalScrollBarInset.
            let window = egui::vec2(
                (rect.width() - if show_v { gutter } else { 0.0 }).max(0.0),
                (rect.height() - if show_h { gutter } else { 0.0 }).max(0.0),
            );
            let max_scroll = (canvas - window).max(egui::Vec2::ZERO);
            let cp = vec2(d, r, "CanvasPosition", [0.0, 0.0]);
            let cp = [cp[0].clamp(0.0, max_scroll.x), cp[1].clamp(0.0, max_scroll.y)];
            let fit = crate::gui::CanvasFit {
                basis: [parent.width(), parent.height()],
                floor: [floor.x, floor.y],
                scale: me.scale,
            };
            if let Some(b) = self.gui.layout.get_mut(&r) {
                b.canvas = Some([canvas.x, canvas.y]);
                b.window = Some([window.x, window.y]);
                b.canvas_fit = Some(fit);
            }
            if let Some((_, b)) = self.boxes.iter_mut().rev().find(|(k, _)| *k == r) {
                b.canvas = Some([canvas.x, canvas.y]);
                b.window = Some([window.x, window.y]);
                b.canvas_fit = Some(fit);
            }
            let origin = Pos2::new(
                rect.left() + if vpos == 1 { inset_v } else { 0.0 } - cp[0],
                rect.top() - cp[1],
            );
            let canvas_rect = Rect::from_min_size(origin, canvas);
            // UIPadding pads inside the canvas (not added on top of CanvasSize).
            let mut inner_canvas = padded(d, r, canvas_rect, me.scale);
            if chat_style {
                inner_canvas = inner_canvas.shrink(17.0 * me.scale);
            }
            self.children(r, inner_canvas, me);
            let st = num(d, r, "ScrollBarImageTransparency", 0.0);
            if th > 0.0 && st < 1.0 {
                if show_v {
                    let bar_x = if vpos == 1 { rect.left() } else { rect.right() - th };
                    let track = Rect::from_min_size(Pos2::new(bar_x, rect.top()), egui::vec2(th, window.y));
                    self.scroll_bar(r, track, canvas.y, cp[1], th, st, &me, true);
                }
                if show_h {
                    let bar_x = rect.left() + if vpos == 1 && show_v { gutter } else { 0.0 };
                    let track = Rect::from_min_size(Pos2::new(bar_x, rect.bottom() - th), egui::vec2(window.x, th));
                    self.scroll_bar(r, track, canvas.x, cp[0], th, st, &me, false);
                }
            }
        } else {
            self.children(r, content, me);
        }
    }

    /// A ScrollingFrame's vertical or horizontal thumb as Studio draws it.
    #[allow(clippy::too_many_arguments)]
    fn scroll_bar(&mut self, r: Ref, track: Rect, canvas_len: f32, cp: f32, th: f32, st: f32, me: &Down, vertical: bool) {
        let d = self.input.doc;
        let view = if vertical { track.height() } else { track.width() };
        if view <= 0.0 || canvas_len <= view {
            return;
        }
        let len = (view * view / canvas_len).max(2.0 * th).min(view);
        let travel = (canvas_len - view).max(1e-3);
        let off = (cp / travel).clamp(0.0, 1.0) * (view - len);
        let bar = if vertical {
            Rect::from_min_size(Pos2::new(track.left(), track.top() + off), egui::vec2(th, len))
        } else {
            Rect::from_min_size(Pos2::new(track.left() + off, track.top()), egui::vec2(len, th))
        };
        let ic = color(d, r, "ScrollBarImageColor3", [1.0, 1.0, 1.0]);
        let tint = me.tint;
        let c = rgba([ic[0] * tint[0], ic[1] * tint[1], ic[2] * tint[2]], (1.0 - st) * me.alpha);
        let image = |prop: &str, dflt: &str| {
            let has = d.dom.get_by_ref(r).is_some_and(|i| i.properties.contains_key(&rbx_dom_weak::ustr(prop)));
            if has { text_prop(d, r, prop) } else { dflt.to_string() }
        };
        let parts = if vertical {
            [
                (image("TopImage", "rbxasset://textures/ui/Scroll/scroll-top.png"), Rect::from_min_size(bar.min, egui::vec2(th, th))),
                (
                    image("MidImage", "rbxasset://textures/ui/Scroll/scroll-middle.png"),
                    Rect::from_min_max(Pos2::new(bar.left(), bar.top() + th), Pos2::new(bar.right(), bar.bottom() - th)),
                ),
                (
                    image("BottomImage", "rbxasset://textures/ui/Scroll/scroll-bottom.png"),
                    Rect::from_min_max(Pos2::new(bar.left(), bar.bottom() - th), bar.max),
                ),
            ]
        } else {
            [
                (image("TopImage", "rbxasset://textures/ui/Scroll/scroll-top.png"), Rect::from_min_size(bar.min, egui::vec2(th, th))),
                (
                    image("MidImage", "rbxasset://textures/ui/Scroll/scroll-middle.png"),
                    Rect::from_min_max(Pos2::new(bar.left() + th, bar.top()), Pos2::new(bar.right() - th, bar.bottom())),
                ),
                (
                    image("BottomImage", "rbxasset://textures/ui/Scroll/scroll-bottom.png"),
                    Rect::from_min_max(Pos2::new(bar.right() - th, bar.top()), bar.max),
                ),
            ]
        };
        if parts.iter().all(|(id, _)| id.is_empty()) {
            return;
        }
        let mut drew = false;
        for (id, dst) in parts {
            if id.is_empty() || dst.width() <= 0.0 || dst.height() <= 0.0 {
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

    fn selected_object(&self) -> Option<(Ref, bool)> {
        if !super::share().selection_visible {
            return None;
        }
        let d = self.input.doc;
        let gs = d.find_child(d.dom.root_ref(), "GuiService")?;
        if let Some(Variant::Ref(r)) = d.prop(gs, "SelectedCoreObject") {
            if d.exists(r) {
                return Some((r, true));
            }
        }
        match d.prop(gs, "SelectedObject") {
            Some(Variant::Ref(r)) if d.exists(r) => Some((r, false)),
            _ => None,
        }
    }

    /// Draws the keyboard / gamepad selection cursor around `r`, honoring
    /// `GuiObject.SelectionImageObject` (e.g. CoreScripts' transparent
    /// `noSelectionObject` suppresses the outline; a custom `UIStroke` cursor
    /// styles it).
    fn draw_selection_cursor(
        &mut self,
        r: Ref,
        is_core: bool,
        rect: Rect,
        corner: Option<f32>,
        feather: f32,
        me: &Down,
    ) {
        let d = self.input.doc;
        if let Some(Variant::Ref(img)) = d.prop(r, "SelectionImageObject") {
            if d.exists(img) {
                let stroke = child_of_class(d, img, "UIStroke")
                    .or_else(|| {
                        d.dom.get_by_ref(img)?.children().iter().copied().find_map(|c| {
                            if d.class_of(c).is_some_and(|k| doc::is_a(&k, "GuiObject"))
                                && boolp(d, c, "Visible", true)
                            {
                                child_of_class(d, c, "UIStroke")
                            } else {
                                None
                            }
                        })
                    })
                    .filter(|s| {
                        boolp(d, *s, "Enabled", true)
                            && num(d, *s, "Transparency", 0.0) < 1.0
                            && num(d, *s, "Thickness", 1.0) > 0.0
                    });
                if let Some(s) = stroke {
                    let th = num(d, s, "Thickness", 2.0) * me.scale;
                    let st = num(d, s, "Transparency", 0.0).clamp(0.0, 1.0);
                    let c = rgba(color(d, s, "Color", [1.0, 1.0, 1.0]), (1.0 - st) * me.alpha);
                    let expand = th;
                    let sel_rect = rect.expand(expand);
                    let sel_corner = corner.map(|rad| rad + expand);
                    self.push(
                        Shape::Mesh(stroke_rounded_aa_mesh(sel_rect, sel_corner, th, feather, &|_| c).into()),
                        me,
                    );
                    return;
                }
                let bg_vis = num(d, img, "BackgroundTransparency", 0.0) < 1.0;
                let img_vis = d
                    .class_of(img)
                    .is_some_and(|k| matches!(k.as_str(), "ImageLabel" | "ImageButton"))
                    && !text_prop(d, img, "Image").is_empty()
                    && num(d, img, "ImageTransparency", 0.0) < 1.0;
                let child_vis = d.dom.get_by_ref(img).is_some_and(|inst| {
                    inst.children().iter().any(|&c| {
                        d.class_of(c).is_some_and(|k| doc::is_a(&k, "GuiObject"))
                            && boolp(d, c, "Visible", true)
                    })
                });
                if !bg_vis && !img_vis && !child_vis {
                    // Explicit transparent placeholder (e.g. SettingsHub's noSelectionObject).
                    return;
                }
            }
        }
        let (c, th, expand) = if is_core {
            (Color32::WHITE, 2.5 * me.scale, 2.5 * me.scale)
        } else {
            let [cr, cg, cb] = mech::GUI_SELECTION_RGB;
            (Color32::from_rgb(cr, cg, cb), 2.0, 2.0)
        };
        let sel_rect = rect.expand(expand);
        let sel_corner = corner.map(|rad| rad + expand);
        self.push(
            Shape::Mesh(stroke_rounded_aa_mesh(sel_rect, sel_corner, th, feather, &|_| c).into()),
            me,
        );
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
        let feather = 1.0 / self.ctx.pixels_per_point().max(1e-3);
        let quad = |dst: Rect, uv: Rect| match corner.filter(|c| *c >= 0.5) {
            Some(c) => rounded_quad_mesh(dst, uv, tex, &col, c, feather),
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
    /// truncation, alignment, rich text and outline. Returns `(TextBounds, TextFits)`.
    fn text(
        &mut self,
        r: Ref,
        class: &str,
        rect: Rect,
        me: &Down,
        ui_stroke: Option<Ref>,
        fill_grad: Option<GradLut>,
        box_rect: Rect,
    ) -> (Option<[f32; 2]>, bool) {
        let d = self.input.doc;
        let _text = rustudio_core::bench::scope("gui_text");
        let raw_text = text_prop(d, r, "Text");
        let text_col = color(d, r, "TextColor3", [0.0, 0.0, 0.0]);
        let mut col = text_col;
        let tt = num(d, r, "TextTransparency", 0.0);
        let focused = self.gui.focus == Some(r);
        let is_textbox = class == "TextBox";
        let is_placeholder = is_textbox && raw_text.is_empty();
        // Roblox shows PlaceholderText whenever Text is empty, even while focused.
        let mut text = if is_placeholder {
            col = color(d, r, "PlaceholderColor3", [0.5, 0.5, 0.5]);
            text_prop(d, r, "PlaceholderText")
        } else {
            raw_text.clone()
        };
        let max_vis = num(d, r, "MaxVisibleGraphemes", -1.0) as i32;
        if !is_placeholder && max_vis >= 0 {
            text = textbox::limit_visible_graphemes(&text, max_vis).to_string();
        }
        let font = font_of(d, r);
        let family = self.gui.family(self.ctx, self.input.store, &font);
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
        let mut runs = if rich {
            rich_runs(&text)
        } else {
            vec![Run {
                text: apply_font_ligatures(&family, &text).into_owned(),
                bold: false,
                italic: font.2,
                underline: false,
                strike: false,
                color: None,
                size: None,
                transparency: None,
                stroke_transparency: None,
            }]
        };
        if rich {
            for run in &mut runs {
                if let Cow::Owned(s) = apply_font_ligatures(&family, &run.text) {
                    run.text = s;
                }
            }
        }
        // RichText <b>, <i>, <b><i>: load the family's bold / italic / bold-italic faces.
        let italic_face = font.2;
        let base_is_italic_file = matches!(&family, FontFamily::Name(n) if n.contains("Italic") || n.contains("Oblique"));
        let bold_family = if rich && !bold_face && runs.iter().any(|r| r.bold && !r.italic) {
            let bf = (font.0.clone(), 700, font.2);
            let fam = self.gui.family(self.ctx, self.input.store, &bf);
            let em = self.em_per_size(&fam);
            note_text_face(self.ctx, &bf, &fam, em);
            Some((fam, em))
        } else {
            None
        };
        let italic_family = if rich && !italic_face && runs.iter().any(|r| r.italic && !r.bold) {
            let itf = (font.0.clone(), font.1, true);
            let fam = self.gui.family(self.ctx, self.input.store, &itf);
            let em = self.em_per_size(&fam);
            note_text_face(self.ctx, &itf, &fam, em);
            Some((fam, em))
        } else {
            None
        };
        let bold_italic_family = if rich && !(bold_face && italic_face) && runs.iter().any(|r| r.bold && r.italic) {
            let bif = (font.0.clone(), 700, true);
            let fam = self.gui.family(self.ctx, self.input.store, &bif);
            let em = self.em_per_size(&fam);
            note_text_face(self.ctx, &bif, &fam, em);
            Some((fam, em))
        } else {
            None
        };
        let legacy_scale = legacy_text_scale(&font);
        let build = |size: f32, wrap: bool| -> LayoutJob {
            let mut job = LayoutJob::default();
            for run in &runs {
                let rs = safe_font_size(run.size.map(|s| s * ui_scale * legacy_scale).unwrap_or(size));
                let c = run.color.unwrap_or(col);
                let color = rgba(
                    [c[0] * tint[0], c[1] * tint[1], c[2] * tint[2]],
                    (1.0 - run.transparency.unwrap_or(tt)) * alpha,
                );
                let (fam, em, real_italic, has_bold) = match (run.bold, run.italic) {
                    (true, true) => match &bold_italic_family {
                        Some((bf, be)) => (bf.clone(), *be, *bf != family, true),
                        None => match (&bold_family, &italic_family) {
                            (Some((bf, be)), _) => (bf.clone(), *be, base_is_italic_file, true),
                            (_, Some((itf, ite))) => (itf.clone(), *ite, *itf != family, bold_face),
                            _ => (family.clone(), em_per_size, base_is_italic_file, bold_face),
                        },
                    },
                    (true, false) => match &bold_family {
                        Some((bf, be)) => (bf.clone(), *be, base_is_italic_file, true),
                        None => (family.clone(), em_per_size, base_is_italic_file, bold_face),
                    },
                    (false, true) => match &italic_family {
                        Some((itf, ite)) => (itf.clone(), *ite, *itf != family, bold_face),
                        None => (family.clone(), em_per_size, base_is_italic_file, bold_face),
                    },
                    (false, false) => (family.clone(), em_per_size, base_is_italic_file, bold_face),
                };
                let mut f = TextFormat::simple(FontId::new(safe_font_size(rs * em), fam), color);
                f.studio_text_size = Some(rs);
                f.line_height = Some(rs * line_height);
                f.italics = run.italic && !real_italic;
                if run.underline {
                    f.underline = Stroke::new((rs / 14.0).max(1.0), color);
                }
                if run.strike {
                    f.strikethrough = Stroke::new((rs / 14.0).max(1.0), color);
                }
                if run.bold && !has_bold {
                    // Faux bold: egui has no synthetic bold, so a little extra spacing.
                    f.extra_letter_spacing = rs * 0.04;
                }
                job.append(&run.text, 0.0, f);
            }
            job.wrap.max_width = if wrap || truncate {
                rect.width().max(1.0) + if truncate && !wrap { 0.5 } else { 0.0 }
            } else {
                f32::INFINITY
            };
            if truncate {
                if !wrap {
                    job.wrap.max_rows = 1;
                    job.wrap.break_anywhere = true;
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
        let mut size = num(d, r, "TextSize", 14.0) * me.scale * legacy_text_scale(&font);
        if !scaled {
            if let Some(c) = tsc {
                size = constrain_font_size(
                    size,
                    num(d, c, "MinTextSize", 1.0) * me.scale,
                    num(d, c, "MaxTextSize", 100.0) * me.scale,
                );
            }
        }
        size = safe_font_size(size);
        if text.is_empty() && !focused {
            return (Some([0.0, size]), size <= rect.height() + 0.5);
        }
        let simple = !rich && !truncate && single_line_ascii(&text);
        if scaled {
            let max = tsc
                .map(|c| num(d, c, "MaxTextSize", 100.0))
                .unwrap_or(100.0)
                .min(100.0)
                * me.scale.max(1.0);
            let Some(fitted) = self.gui.text_fit.size(ctx, rect.size(), max, simple, &build) else {
                self.gui.no_cache = Some("text deferred");
                return (None, false);
            };
            size = fitted;
            if let Some(c) = tsc {
                size = size.max(num(d, c, "MinTextSize", 1.0) * me.scale);
            }
        }
        size = safe_font_size(size);
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
            return (None, false);
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
        // Sync script cursor/selection writes so drawing reflects live CursorPosition/SelectionStart.
        let (tb_cur, tb_sel) = if focused {
            let sh = super::share();
            let c = sh.tb_cursor.get(&r).copied().unwrap_or(self.gui.tb_cursor);
            let s = sh.tb_sel.get(&r).copied().unwrap_or(self.gui.tb_sel);
            (c, s)
        } else {
            (-1, -1)
        };
        let char_idx_for_byte = |b: i32| -> usize {
            if b < 1 || is_placeholder {
                return 0;
            }
            let clamped = textbox::clamp_byte_pos(&text, (b - 1) as usize);
            text[..clamped].chars().count()
        };
        // Caret position in galley-local coordinates (for horizontal scrolling & drawing).
        let caret_local_rect = if focused {
            if is_placeholder || text.is_empty() {
                let cx = 0.0;
                Rect::from_min_max(Pos2::new(cx, 0.0), Pos2::new(cx, size * line_height))
            } else {
                let cc = egui::text::CCursor::new(char_idx_for_byte(tb_cur));
                galley.pos_from_cursor(cc)
            }
        } else {
            Rect::NOTHING
        };
        // Horizontal scrolling for non-wrapped, non-scaled TextBoxes when text overflows.
        let mut scroll_x = 0.0f32;
        let mut text_down = *me;
        if is_textbox && !wrapped && !scaled && rect.width() > 0.0 {
            let view_w = rect.width();
            let content_w = galley.rect.width();
            if focused && !is_placeholder && content_w > view_w {
                let mut sx = self.gui.tb_scroll.get(&r).copied().unwrap_or(0.0);
                let caret_rel_x = (x + caret_local_rect.min.x) - rect.left();
                let margin = 2.0f32.min(view_w * 0.25);
                if caret_rel_x - sx > view_w - margin {
                    sx = caret_rel_x - (view_w - margin);
                } else if caret_rel_x - sx < 0.0 {
                    sx = caret_rel_x;
                }
                let max_scroll = ((x + galley.rect.right()) - rect.left() - (view_w - margin)).max(0.0);
                sx = sx.clamp(0.0, max_scroll);
                self.gui.tb_scroll.insert(r, sx);
                scroll_x = sx;
            } else {
                self.gui.tb_scroll.remove(&r);
            }
            // Clip overflowing TextBox text/selection/caret to the TextBox bounds.
            if me.aff.is_id() && (focused || scroll_x > 0.0 || boolp(d, r, "ClipsDescendants", false)) {
                text_down.clip = me.clip.intersect(rect);
            }
        }
        let pos = Pos2::new(x - scroll_x, y);
        if is_textbox {
            self.gui.tb_layouts.insert(
                r,
                TextBoxLayout {
                    galley: galley.clone(),
                    text_origin: pos,
                    screen_origin: self.screen,
                    aff: me.aff,
                    is_placeholder,
                    text: raw_text.clone(),
                },
            );
        }
        // Selection highlight (drawn BEHIND text, Roblox cornflower blue rgb(107, 161, 249)).
        if focused && !is_placeholder && tb_sel >= 1 && tb_cur >= 1 && tb_sel != tb_cur {
            let lo_b = textbox::clamp_byte_pos(&text, (tb_cur.min(tb_sel) - 1) as usize);
            let hi_b = textbox::clamp_byte_pos(&text, (tb_cur.max(tb_sel) - 1) as usize);
            if lo_b < hi_b {
                let lo_c = text[..lo_b].chars().count();
                let hi_c = text[..hi_b].chars().count();
                let lc_lo = galley.layout_from_cursor(egui::text::CCursor::new(lo_c));
                let lc_hi = galley.layout_from_cursor(egui::text::CCursor::new(hi_c));
                let sel_col = Color32::from_rgba_unmultiplied(
                    107,
                    161,
                    249,
                    (255.0 * me.alpha).round().clamp(0.0, 255.0) as u8,
                );
                for row_idx in lc_lo.row..=lc_hi.row.min(galley.rows.len().saturating_sub(1)) {
                    let row = &galley.rows[row_idx];
                    let x0 = if row_idx == lc_lo.row {
                        row.pos.x + row.x_offset(lc_lo.column)
                    } else {
                        row.rect().left()
                    };
                    let x1 = if row_idx == lc_hi.row {
                        row.pos.x + row.x_offset(lc_hi.column)
                    } else {
                        row.rect().right().max(row.rect().left() + size * 0.35)
                    };
                    if x1 > x0 {
                        let srect = Rect::from_min_max(
                            Pos2::new(pos.x + x0, pos.y + row.min_y()),
                            Pos2::new(pos.x + x1, pos.y + row.max_y()),
                        );
                        self.push(Shape::rect_filled(srect, 0.0, sel_col), &text_down);
                    }
                }
            }
        }
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
            let skip_transparent = rich
                && runs
                    .iter()
                    .any(|r| r.transparency.is_some_and(|t| t >= 1.0) || r.stroke_transparency.is_some_and(|t| t >= 1.0));
            let has_color_glyphs = galley
                .rows
                .iter()
                .any(|pr| !pr.row.visuals.color_glyph_vertex_starts.is_empty());
            let tinted = if outline_grad.is_some() || skip_transparent || has_color_glyphs {
                Some(recolor_galley(&galley, pos, Some(Color32::TRANSPARENT), |p, orig| {
                    if skip_transparent && orig.a() == 0 {
                        Color32::TRANSPARENT
                    } else if let Some(gl) = &outline_grad {
                        gl.tint(p, c)
                    } else {
                        c
                    }
                }))
            } else {
                None
            };
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
                self.push(Shape::Text(ts), &text_down);
            }
        }
        let fill = match &fill_grad {
            Some(gl) => recolor_galley(&galley, pos, None, |p, c| gl.tint(p, c)),
            None => galley.clone(),
        };
        self.push(Shape::Text(TextShape::new(pos, fill, rgba(col, (1.0 - tt) * me.alpha))), &text_down);
        // Blinking caret in the focused TextBox (1 physical pixel wide, TextColor3,
        // solid for 0.53s after any input so it stays visible while moving/typing).
        if focused {
            self.gui.no_cache = Some("focused textbox");
            let now_s = ui_time(self.ctx);
            let elapsed = (now_s - self.gui.tb_blink).max(0.0);
            let blink_on = ((elapsed / 0.53) as u64) % 2 == 0;
            if blink_on {
                let ppp = self.ctx.pixels_per_point().max(1.0);
                let caret_w = 1.0 / ppp;
                let cx = ((pos.x + caret_local_rect.min.x) * ppp).round() / ppp;
                let (cy0, cy1) = if caret_local_rect.height() > 1.0 {
                    (pos.y + caret_local_rect.min.y, pos.y + caret_local_rect.max.y)
                } else {
                    (pos.y, pos.y + size * line_height)
                };
                let caret_rect = Rect::from_min_max(Pos2::new(cx, cy0), Pos2::new(cx + caret_w, cy1));
                self.push(
                    Shape::rect_filled(caret_rect, 0.0, rgba(text_col, (1.0 - tt) * me.alpha)),
                    &text_down,
                );
            }
        }
        let full_w = galley.rect.width().ceil();
        let has_custom_size = rich && runs.iter().any(|r| r.size.is_some());
        let full_h = if has_custom_size && (line_height - 1.0).abs() < 1e-4 {
            galley.rect.height().ceil()
        } else {
            text_rows_height(galley.rows.len(), size, line_height)
        };
        let fits = full_w <= rect.width() + 0.5 && full_h <= rect.height() + 0.5;
        let (tb_w, tb_h) = if wrapped && !scaled && galley.rows.len() > 1 && full_h > rect.height() + 0.5 {
            // Studio's TextBounds on overflowing wrapped text measures only the
            // rows that fit vertically inside the box while TextFits is false.
            let mut fit_rows = 0usize;
            for k in 1..=galley.rows.len() {
                if text_rows_height(k, size, line_height) <= rect.height() + 0.5 {
                    fit_rows = k;
                } else {
                    break;
                }
            }
            if fit_rows > 0 {
                let vis_w = galley.rows[..fit_rows]
                    .iter()
                    .map(|pr| pr.rect().width())
                    .fold(0.0f32, f32::max)
                    .ceil();
                (vis_w, text_rows_height(fit_rows, size, line_height))
            } else {
                (full_w, full_h)
            }
        } else {
            (full_w, full_h)
        };
        (Some([tb_w, tb_h]), fits)
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

/// UIFlexItem (grow, shrink) ratios:
/// None (0) -> (0, 0), Grow (1) -> (1, 0), Shrink (2) -> (0, 1),
/// Fill (3) -> (1, 1), Custom (4) -> (GrowRatio, ShrinkRatio).
fn flex_ratios(d: &Document, f: Ref) -> (f32, f32) {
    match enum_u32(d.prop(f, "FlexMode")).unwrap_or(0) {
        1 => (1.0, 0.0),
        2 => (0.0, 1.0),
        3 => (1.0, 1.0),
        4 => (
            num(d, f, "GrowRatio", 0.0).max(0.0),
            num(d, f, "ShrinkRatio", 0.0).max(0.0),
        ),
        _ => (0.0, 0.0),
    }
}

/// A line of a UIListLayout: which of the (sorted) items it holds, and
/// their extent along the fill axis (gaps included) and across it (the
/// largest).
pub(super) struct ListLine {
    pub items: std::ops::Range<usize>,
    pub along: f32,
    pub across: f32,
}

/// The lines of a UIListLayout over `sizes` (the items in layout order):
/// one, unless it `wraps` and they overflow `room` along the fill axis —
/// then each line takes the items that fit, at least one (UIListLayout.Wraps).
pub(super) fn list_lines(sizes: &[egui::Vec2], vertical: bool, wraps: bool, room: f32, gap: f32) -> Vec<ListLine> {
    let along = |s: &egui::Vec2| if vertical { s.y } else { s.x };
    let across = |s: &egui::Vec2| if vertical { s.x } else { s.y };
    let mut lines = Vec::new();
    let mut start = 0;
    let (mut a, mut c) = (0.0f32, 0.0f32);
    for (i, s) in sizes.iter().enumerate() {
        let add = along(s) + if i > start { gap } else { 0.0 };
        if wraps && i > start && a + add > room + 1e-3 {
            lines.push(ListLine { items: start..i, along: a, across: c });
            start = i;
            a = along(s);
            c = across(s);
        } else {
            a += add;
            c = c.max(across(s));
        }
    }
    if !sizes.is_empty() {
        lines.push(ListLine { items: start..sizes.len(), along: a, across: c });
    }
    lines
}

#[cfg(test)]
mod tests {
    #[test]
    fn font_sizes_are_valid_after_scaling() {
        for size in [0.0, f32::NAN, f32::INFINITY, f32::from_bits(1)] {
            assert_eq!(safe_font_size(size), mech::GUI_MIN_FONT_SIZE);
        }
        assert_eq!(safe_font_size(14.0), 14.0);
        assert_eq!(safe_font_size(constrain_font_size(f32::NAN, 0.0, 0.0)), mech::GUI_MIN_FONT_SIZE);
    }

    use super::*;

    #[test]
    fn text_fit_matches_wrapped_probes() {
        let ctx = egui::Context::default();
        let mut output = ctx.run_ui(egui::RawInput::default(), |_| {
            let mut cache = TextFitCache::default();
            for text in ["Temple Guardian", "longwordwithoutbreaks", "é W", "title\nsecond line", "Lv. 100", "jjjj W"] {
                for width in [6.0, 39.4, 70.3, 70.8, 198.0] {
                    for height in [3.5, 11.3, 31.7, 49.0] {
                        for max in [20.0f32, 100.0] {
                            let build = |size: f32, wrap: bool| {
                                let mut job = LayoutJob::simple(
                                    text.into(), FontId::proportional(size), egui::Color32::WHITE,
                                    if wrap { width } else { f32::INFINITY },
                                );
                                job.sections[0].format.line_height = Some(size);
                                job.round_output_to_gui = false;
                                job
                            };
                            let mut lo = 1i32;
                            let mut hi = (max.floor().max(1.0) as i32) + 1;
                            while lo + 1 < hi {
                                let mid = (lo + hi) / 2;
                                let g = ctx.fonts_mut(|fonts| fonts.layout_job(build(mid as f32, true)));
                                let actual = g.size();
                                if !has_mid_word_break(&g) && actual.x <= width + 0.5 && actual.y <= height + 0.5 {
                                    lo = mid;
                                } else {
                                    hi = mid;
                                }
                            }
                            let fitted = cache.size(&ctx, egui::vec2(width, height), max, single_line_ascii(text), build);
                            assert_eq!(fitted, Some(lo as f32), "{text:?} {width}×{height} max {max}");
                        }
                    }
                }
            }
        });
        output.textures_delta.clear();
    }

    #[test]
    fn text_scaled_does_not_split_single_word_mid_word() {
        let ctx = egui::Context::default();
        let mut output = ctx.run_ui(egui::RawInput::default(), |_| {
            let mut cache = TextFitCache::default();
            for word in ["Random", "Spectate"] {
                let width = 70.0;
                let height = 180.0;
                let build = |size: f32, wrap: bool| {
                    let mut job = LayoutJob::simple(
                        word.into(), FontId::proportional(size), egui::Color32::WHITE,
                        if wrap { width } else { f32::INFINITY },
                    );
                    job.sections[0].format.line_height = Some(size);
                    job.round_output_to_gui = false;
                    job
                };
                let fitted = cache.size(&ctx, egui::vec2(width, height), 100.0, true, build).unwrap();
                let g = ctx.fonts_mut(|fonts| fonts.layout_job(build(fitted, true)));
                assert_eq!(g.rows.len(), 1, "{word} at size {fitted} wrapped into {} rows", g.rows.len());
                assert!(!has_mid_word_break(&g), "{word} broke mid-word");
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
            let mut walk = Walk { gui: &mut gui, ctx: ui.ctx(), input: &mut input, items: Vec::new(), hits: Vec::new(), seq: 0, boxes: Vec::new(), global_z: false, abs_offset: [0.0; 2], screen: None, cur: Ref::none(), rec: Vec::new(), replaying: 0, table_cells: HashMap::new() };
            walk.children(parent, rect, Down { aff: Aff::ID, clip: rect, alpha: 1.0, tint: [1.0; 3], scale: 1.0, z: 0, turn: 0.0 });
            let reply_box = &walk.boxes.iter().find(|(r, _)| *r == reply).unwrap().1;
            assert!((reply_box.size[0] - rect.width()).abs() < 0.01);
            assert!((reply_box.pos[0] - rect.left()).abs() < 0.01);
            assert!((reply_box.size[1] - 28.0).abs() < 0.01);
        });
        out.textures_delta.clear();
    }

    /// UIListLayout.Wraps: a horizontal list breaks into lines at its
    /// parent's width (five 40 px tiles, 5 px apart, in 100 px: two, two,
    /// one), the lines stacked by the same Padding; HorizontalFlex
    /// SpaceBetween spreads each line's items to its ends and
    /// ItemLineAlignment Stretch gives a short item its line's height. An
    /// AutomaticSize Y holder of such a list is as tall as its lines.
    #[test]
    fn list_wraps_and_flexes() {
        let mut doc = Document::new_place();
        let root = doc.dom.root_ref();
        let tile = |doc: &mut Document, parent: Ref, n: i32, h: i32| {
            doc.dom.insert(parent, InstanceBuilder::new("Frame").with_property("LayoutOrder", n)
                .with_property("Size", UDim2::new(UDim::new(0.0, 40), UDim::new(0.0, h))))
        };
        let wrap = doc.dom.insert(root, InstanceBuilder::new("Frame"));
        doc.dom.insert(wrap, InstanceBuilder::new("UIListLayout")
            .with_property("FillDirection", Enum::from_u32(0))
            .with_property("Padding", UDim::new(0.0, 5))
            .with_property("Wraps", true));
        let tiles: Vec<Ref> = (0..5).map(|n| tile(&mut doc, wrap, n, 20)).collect();
        let spread = doc.dom.insert(root, InstanceBuilder::new("Frame"));
        doc.dom.insert(spread, InstanceBuilder::new("UIListLayout")
            .with_property("FillDirection", Enum::from_u32(0))
            .with_property("Padding", UDim::new(0.0, 5))
            .with_property("Wraps", true)
            .with_property("HorizontalFlex", Enum::from_u32(3))
            .with_property("ItemLineAlignment", Enum::from_u32(4)));
        let a = tile(&mut doc, spread, 0, 20);
        let b = tile(&mut doc, spread, 1, 10);
        let c = tile(&mut doc, spread, 2, 20);
        let page = doc.dom.insert(root, InstanceBuilder::new("Frame"));
        let holder = doc.dom.insert(page, InstanceBuilder::new("Frame")
            .with_property("Size", UDim2::new(UDim::new(0.0, 100), UDim::new(0.0, 0)))
            .with_property("AutomaticSize", Enum::from_u32(2)));
        doc.dom.insert(holder, InstanceBuilder::new("UIListLayout")
            .with_property("FillDirection", Enum::from_u32(0))
            .with_property("Padding", UDim::new(0.0, 5))
            .with_property("Wraps", true));
        for n in 0..5 {
            tile(&mut doc, holder, n, 20);
        }
        let ctx = egui::Context::default();
        let mut gui = Gui::default();
        let cam = Camera::default();
        let mut store = AssetStore::default();
        let rect = Rect::from_min_size(Pos2::ZERO, egui::vec2(100.0, 200.0));
        let mut input = DrawIn { render_state: None, doc: &doc, cam: &cam, rect, store: &mut store, doc_dir: None, playing: false, fixed_step: true, roots: Default::default() };
        let mut out = ctx.run_ui(Default::default(), |ui| {
            let ctx = ui.ctx();
            let mut walk = Walk { gui: &mut gui, ctx, input: &mut input, items: Vec::new(), hits: Vec::new(), seq: 0, boxes: Vec::new(), global_z: false, abs_offset: [0.0; 2], screen: None, cur: Ref::none(), rec: Vec::new(), replaying: 0, table_cells: HashMap::new() };
            let dn = Down { aff: Aff::ID, clip: rect, alpha: 1.0, tint: [1.0; 3], scale: 1.0, z: 0, turn: 0.0 };
            walk.children(wrap, rect, dn);
            walk.children(spread, rect, dn);
            walk.children(page, rect, dn);
            let b_ = |r: Ref| walk.boxes.iter().find(|(k, _)| *k == r).unwrap().1.clone();
            let at: Vec<[f32; 2]> = tiles.iter().map(|t| b_(*t).pos).collect();
            assert_eq!(at, vec![[0.0, 0.0], [45.0, 0.0], [0.0, 25.0], [45.0, 25.0], [0.0, 50.0]], "wrapped lines");
            // SpaceBetween: a (0) and b (60) at the line's ends, 15 px of
            // room spread into the gap; c alone on the second line, at the
            // start. Stretch: b is 20 tall like its line.
            assert!((b_(a).pos[0] - 0.0).abs() < 0.01 && (b_(b).pos[0] - 60.0).abs() < 0.01, "{:?} {:?}", b_(a).pos, b_(b).pos);
            assert!((b_(c).pos[0] - 0.0).abs() < 0.01 && (b_(c).pos[1] - 25.0).abs() < 0.01, "{:?}", b_(c).pos);
            assert!((b_(b).size[1] - 20.0).abs() < 0.01, "stretched {:?}", b_(b).size);
            let h = b_(holder);
            assert!((h.size[0] - 100.0).abs() < 0.01 && (h.size[1] - 70.0).abs() < 0.01, "AutomaticSize Y holds the three lines: {:?}", h.size);
        });
        out.textures_delta.clear();
    }

    /// UIShadow: a drop shadow of the box — soft over BlurRadius (10 px:
    /// from 5 px inside the box's edge to 5 px out), in its Color at
    /// 1 - Transparency — drawn under the box's background.
    #[test]
    fn ui_shadow_under_the_box() {
        use rbx_dom_weak::types::Color3;
        let mut doc = Document::new_place();
        let root = doc.dom.root_ref();
        let page = doc.dom.insert(root, InstanceBuilder::new("Frame"));
        let frame = doc.dom.insert(page, InstanceBuilder::new("Frame")
            .with_property("Position", UDim2::new(UDim::new(0.0, 50), UDim::new(0.0, 50)))
            .with_property("Size", UDim2::new(UDim::new(0.0, 100), UDim::new(0.0, 50))));
        doc.dom.insert(frame, InstanceBuilder::new("UIShadow")
            .with_property("BlurRadius", UDim::new(0.0, 10))
            .with_property("Color", Color3::new(1.0, 0.0, 0.0))
            .with_property("Transparency", 0.5f32));
        let ctx = egui::Context::default();
        let mut gui = Gui::default();
        let cam = Camera::default();
        let mut store = AssetStore::default();
        let rect = Rect::from_min_size(Pos2::ZERO, egui::vec2(200.0, 200.0));
        let mut input = DrawIn { render_state: None, doc: &doc, cam: &cam, rect, store: &mut store, doc_dir: None, playing: false, fixed_step: true, roots: Default::default() };
        let mut out = ctx.run_ui(Default::default(), |ui| {
            let ctx = ui.ctx();
            let mut walk = Walk { gui: &mut gui, ctx, input: &mut input, items: Vec::new(), hits: Vec::new(), seq: 0, boxes: Vec::new(), global_z: false, abs_offset: [0.0; 2], screen: None, cur: Ref::none(), rec: Vec::new(), replaying: 0, table_cells: HashMap::new() };
            walk.children(page, rect, Down { aff: Aff::ID, clip: rect, alpha: 1.0, tint: [1.0; 3], scale: 1.0, z: 0, turn: 0.0 });
            let bounds = |it: &Item| it.shape.visual_bounding_rect();
            let shadow = walk.items.iter().find(|it| bounds(it).min.x < 46.0).expect("a shadow item");
            let b = bounds(shadow);
            assert!((b.min.x - 45.0).abs() < 0.5 && (b.max.x - 155.0).abs() < 0.5 && (b.min.y - 45.0).abs() < 0.5 && (b.max.y - 105.0).abs() < 0.5, "{b:?}");
            let bg = walk.items.iter().find(|it| (bounds(it).min.x - 50.0).abs() < 0.01 && (bounds(it).max.x - 150.0).abs() < 0.01).expect("the background");
            assert!(shadow.seq < bg.seq, "the shadow is under the box");
            let Shape::Mesh(m) = &shadow.shape else { panic!("a mesh") };
            assert!(m.vertices.iter().all(|v| v.color.g() == 0 && v.color.b() == 0), "red");
            let a = m.vertices.iter().map(|v| v.color.a()).max().unwrap();
            assert!((a as i32 - 128).abs() <= 2, "half transparent at the fullest: {a}");
            assert!(m.vertices.iter().any(|v| v.color.a() == 0), "faded out at the edge");
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
            let mut walk = Walk { gui: &mut gui, ctx, input: &mut input, items: Vec::new(), hits: Vec::new(), seq: 0, boxes: Vec::new(), global_z: false, abs_offset: [0.0; 2], screen: None, cur: Ref::none(), rec: Vec::new(), replaying: 0, table_cells: HashMap::new() };
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
            let mut walk = Walk { gui: &mut gui, ctx, input: &mut input, items: Vec::new(), hits: Vec::new(), seq: 0, boxes: Vec::new(), global_z: false, abs_offset: [0.0; 2], screen: None, cur: Ref::none(), rec: Vec::new(), replaying: 0, table_cells: HashMap::new() };
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
            let mut walk = Walk { gui: &mut gui, ctx, input: &mut input, items: Vec::new(), hits: Vec::new(), seq: 0, boxes: Vec::new(), global_z: false, abs_offset: [0.0; 2], screen: None, cur: Ref::none(), rec: Vec::new(), replaying: 0, table_cells: HashMap::new() };
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
            let mut walk = Walk { gui: &mut gui, ctx, input: &mut input, items: Vec::new(), hits: Vec::new(), seq: 0, boxes: Vec::new(), global_z: false, abs_offset: [0.0; 2], screen: None, cur: Ref::none(), rec: Vec::new(), replaying: 0, table_cells: HashMap::new() };
            walk.children(parent, rect, Down { aff: Aff::ID, clip: rect, alpha: 1.0, tint: [1.0; 3], scale: 1.0, z: 0, turn: 0.0 });
            let b = walk.boxes.iter().find(|(k, _)| *k == sf).unwrap().1;
            let laid = b.canvas.unwrap();
            assert_eq!(b.canvas_for([0.0, 0.0, 0.0, 400.0]), Some(laid), "same CanvasSize");
            assert!((laid[1] - 400.0).abs() < 0.01, "UIPadding is inside the canvas: {laid:?}");
            let small = b.canvas_for([0.0, 0.0, 0.0, 160.0]).unwrap();
            assert!((small[1] - 160.0).abs() < 0.01, "shrunk: {small:?}");
            let tiny = b.canvas_for([0.0, 0.0, 0.0, 10.0]).unwrap();
            assert!((tiny[1] - 100.0).abs() < 0.01, "never below the frame: {tiny:?}");
            let a = walk.boxes.iter().find(|(k, _)| *k == auto).unwrap().1;
            let held = a.canvas_for([0.0, 0.0, 0.0, 0.0]).unwrap();
            assert!((held[1] - 250.0).abs() < 0.01 && Some(held) == a.canvas, "automatic Y holds its child: {held:?}");
        });
        out.textures_delta.clear();
    }

    #[test]
    fn table_layout_row_and_col_major_fill_empty() {
        let mut doc = Document::new_place();
        let root = doc.dom.root_ref();
        let make_table = |doc: &mut Document, major_axis: u32, fc: bool, fr: bool| {
            let tbl = doc.dom.insert(root, InstanceBuilder::new("Frame"));
            let lay = doc.dom.insert(tbl, InstanceBuilder::new("UITableLayout")
                .with_property("MajorAxis", Enum::from_u32(major_axis))
                .with_property("FillEmptySpaceColumns", fc)
                .with_property("FillEmptySpaceRows", fr)
                .with_property("Padding", UDim2::new(UDim::new(0.0, 4), UDim::new(0.0, 6)))
                .with_property("SortOrder", Enum::from_u32(2)));
            let r1 = doc.dom.insert(tbl, InstanceBuilder::new("Frame").with_property("LayoutOrder", 1i32));
            let c11 = doc.dom.insert(r1, InstanceBuilder::new("Frame").with_property("LayoutOrder", 1i32)
                .with_property("Size", UDim2::new(UDim::new(0.0, 50), UDim::new(0.0, 35))));
            let c12 = doc.dom.insert(r1, InstanceBuilder::new("Frame").with_property("LayoutOrder", 2i32)
                .with_property("Size", UDim2::new(UDim::new(0.0, 60), UDim::new(0.0, 20))));
            let r2 = doc.dom.insert(tbl, InstanceBuilder::new("Frame").with_property("LayoutOrder", 2i32));
            let c21 = doc.dom.insert(r2, InstanceBuilder::new("Frame").with_property("LayoutOrder", 1i32)
                .with_property("Size", UDim2::new(UDim::new(0.0, 30), UDim::new(0.0, 40))));
            (tbl, lay, r1, c11, c12, r2, c21)
        };
        let (t_row, l_row, _, rc11, rc12, _, rc21) = make_table(&mut doc, 0, true, false);
        let (t_col, l_col, _, cc11, _, cc2, cc21) = make_table(&mut doc, 1, false, true);
        let ctx = egui::Context::default();
        let mut gui = Gui::default();
        let cam = Camera::default();
        let mut store = AssetStore::default();
        let rect = Rect::from_min_size(Pos2::ZERO, egui::vec2(220.0, 140.0));
        let mut input = DrawIn { render_state: None, doc: &doc, cam: &cam, rect, store: &mut store, doc_dir: None, playing: false, fixed_step: true, roots: Default::default() };
        let mut out = ctx.run_ui(Default::default(), |ui| {
            let ctx = ui.ctx();
            let mut walk = Walk { gui: &mut gui, ctx, input: &mut input, items: Vec::new(), hits: Vec::new(), seq: 0, boxes: Vec::new(), global_z: false, abs_offset: [0.0; 2], screen: None, cur: Ref::none(), rec: Vec::new(), replaying: 0, table_cells: HashMap::new() };
            let dn = Down { aff: Aff::ID, clip: rect, alpha: 1.0, tint: [1.0; 3], scale: 1.0, z: 0, turn: 0.0 };
            walk.children(t_row, rect, dn);
            walk.children(t_col, rect, dn);
            let b = |r: Ref| walk.boxes.iter().find(|(k, _)| *k == r).unwrap().1;
            // RowMajor FC=true: avail_w = 220 - 4 = 216, col_w = [50, 60] scaled by 216/110.
            assert!((b(l_row).size[0] - 220.0).abs() < 0.01 && (b(l_row).size[1] - 81.0).abs() < 0.01);
            assert!((b(rc11).size[0] - 50.0 * 216.0 / 110.0).abs() < 0.02);
            assert!((b(rc12).size[0] - 60.0 * 216.0 / 110.0).abs() < 0.02);
            assert!((b(rc21).size[1] - 40.0).abs() < 0.01);
            // ColumnMajor FR=true scales the major (strip/column) axis: [60, 30] -> avail 216.
            assert!((b(l_col).size[0] - 220.0).abs() < 0.01);
            assert!((b(cc11).size[0] - 60.0 * 216.0 / 90.0).abs() < 0.02);
            assert!((b(cc2).size[0] - 30.0 * 216.0 / 90.0).abs() < 0.02);
            assert!((b(cc21).size[0] - 30.0 * 216.0 / 90.0).abs() < 0.02);
        });
        out.textures_delta.clear();
    }

    #[test]
    fn grid_start_corners_and_constraints() {
        let mut doc = Document::new_place();
        let root = doc.dom.root_ref();
        let parent = doc.dom.insert(root, InstanceBuilder::new("Frame"));
        let lay = doc.dom.insert(parent, InstanceBuilder::new("UIGridLayout")
            .with_property("CellSize", UDim2::new(UDim::new(0.4, 0), UDim::new(0.8, 0)))
            .with_property("CellPadding", UDim2::new(UDim::new(0.0, 5), UDim::new(0.0, 5)))
            .with_property("StartCorner", Enum::from_u32(3)) // BottomRight
            .with_property("SortOrder", Enum::from_u32(2)));
        doc.dom.insert(lay, InstanceBuilder::new("UIAspectRatioConstraint")
            .with_property("AspectRatio", 2.0f32)
            .with_property("AspectType", Enum::from_u32(0)));
        let i1 = doc.dom.insert(parent, InstanceBuilder::new("Frame").with_property("LayoutOrder", 1i32));
        let i2 = doc.dom.insert(parent, InstanceBuilder::new("Frame").with_property("LayoutOrder", 2i32));
        let i3 = doc.dom.insert(parent, InstanceBuilder::new("Frame").with_property("LayoutOrder", 3i32));
        let ctx = egui::Context::default();
        let mut gui = Gui::default();
        let cam = Camera::default();
        let mut store = AssetStore::default();
        let rect = Rect::from_min_size(Pos2::ZERO, egui::vec2(200.0, 140.0));
        let mut input = DrawIn { render_state: None, doc: &doc, cam: &cam, rect, store: &mut store, doc_dir: None, playing: false, fixed_step: true, roots: Default::default() };
        let mut out = ctx.run_ui(Default::default(), |ui| {
            let ctx = ui.ctx();
            let mut walk = Walk { gui: &mut gui, ctx, input: &mut input, items: Vec::new(), hits: Vec::new(), seq: 0, boxes: Vec::new(), global_z: false, abs_offset: [0.0; 2], screen: None, cur: Ref::none(), rec: Vec::new(), replaying: 0, table_cells: HashMap::new() };
            walk.children(parent, rect, Down { aff: Aff::ID, clip: rect, alpha: 1.0, tint: [1.0; 3], scale: 1.0, z: 0, turn: 0.0 });
            let b = |r: Ref| walk.boxes.iter().find(|(k, _)| *k == r).unwrap().1;
            // Cell 80x112 aspect-fit 2:1 -> 80x40; 2x2 grid; BottomRight puts i1 at (85,45), i2 at (0,45), i3 at (85,0).
            assert_eq!(b(lay).window, Some([80.0, 40.0]));
            assert_eq!(b(lay).canvas, Some([2.0, 2.0]));
            assert!((b(i1).pos[0] - 85.0).abs() < 0.01 && (b(i1).pos[1] - 45.0).abs() < 0.01);
            assert!((b(i2).pos[0] - 0.0).abs() < 0.01 && (b(i2).pos[1] - 45.0).abs() < 0.01);
            assert!((b(i3).pos[0] - 85.0).abs() < 0.01 && (b(i3).pos[1] - 0.0).abs() < 0.01);
        });
        out.textures_delta.clear();
    }

    #[test]
    fn constraints_order_and_padding_overflow() {
        let mut doc = Document::new_place();
        let root = doc.dom.root_ref();
        let page = doc.dom.insert(root, InstanceBuilder::new("Frame"));
        // MinSize > MaxSize ignored.
        let min_gt_max = doc.dom.insert(page, InstanceBuilder::new("Frame")
            .with_property("Size", UDim2::new(UDim::new(0.0, 100), UDim::new(0.0, 100))));
        doc.dom.insert(min_gt_max, InstanceBuilder::new("UISizeConstraint")
            .with_property("MinSize", Vector2::new(120.0, 90.0))
            .with_property("MaxSize", Vector2::new(60.0, 50.0)));
        // ScaleWithParentSize clamped to parent (200x120) preserving 0.5 ratio -> 60x120.
        let scale_ar = doc.dom.insert(page, InstanceBuilder::new("Frame")
            .with_property("Size", UDim2::new(UDim::new(0.0, 100), UDim::new(0.0, 100))));
        doc.dom.insert(scale_ar, InstanceBuilder::new("UIAspectRatioConstraint")
            .with_property("AspectRatio", 0.5f32)
            .with_property("AspectType", Enum::from_u32(1))
            .with_property("DominantAxis", Enum::from_u32(0)));
        // AutomaticSize runs after UISizeConstraint: grows to child 150x80 despite MaxSize 90x50.
        let auto_sc = doc.dom.insert(page, InstanceBuilder::new("Frame")
            .with_property("AutomaticSize", Enum::from_u32(3)));
        doc.dom.insert(auto_sc, InstanceBuilder::new("UISizeConstraint")
            .with_property("MinSize", Vector2::new(40.0, 30.0))
            .with_property("MaxSize", Vector2::new(90.0, 50.0)));
        doc.dom.insert(auto_sc, InstanceBuilder::new("Frame")
            .with_property("Size", UDim2::new(UDim::new(0.0, 150), UDim::new(0.0, 80))));
        // UIPadding overflow flips rect cleanly: 100x60 with L=70,R=70,T=40,B=40 -> pos(30,20) size(40,20).
        let pad_box = doc.dom.insert(page, InstanceBuilder::new("Frame")
            .with_property("Size", UDim2::new(UDim::new(0.0, 100), UDim::new(0.0, 60))));
        doc.dom.insert(pad_box, InstanceBuilder::new("UIPadding")
            .with_property("PaddingLeft", UDim::new(0.0, 70))
            .with_property("PaddingRight", UDim::new(0.0, 70))
            .with_property("PaddingTop", UDim::new(0.0, 40))
            .with_property("PaddingBottom", UDim::new(0.0, 40)));
        let inside = doc.dom.insert(pad_box, InstanceBuilder::new("Frame")
            .with_property("Size", UDim2::new(UDim::new(1.0, 0), UDim::new(1.0, 0))));
        let ctx = egui::Context::default();
        let mut gui = Gui::default();
        let cam = Camera::default();
        let mut store = AssetStore::default();
        let rect = Rect::from_min_size(Pos2::ZERO, egui::vec2(200.0, 120.0));
        let mut input = DrawIn { render_state: None, doc: &doc, cam: &cam, rect, store: &mut store, doc_dir: None, playing: false, fixed_step: true, roots: Default::default() };
        let mut out = ctx.run_ui(Default::default(), |ui| {
            let ctx = ui.ctx();
            let mut walk = Walk { gui: &mut gui, ctx, input: &mut input, items: Vec::new(), hits: Vec::new(), seq: 0, boxes: Vec::new(), global_z: false, abs_offset: [0.0; 2], screen: None, cur: Ref::none(), rec: Vec::new(), replaying: 0, table_cells: HashMap::new() };
            walk.children(page, rect, Down { aff: Aff::ID, clip: rect, alpha: 1.0, tint: [1.0; 3], scale: 1.0, z: 0, turn: 0.0 });
            let b = |r: Ref| walk.boxes.iter().find(|(k, _)| *k == r).unwrap().1;
            assert_eq!(b(min_gt_max).size, [100.0, 100.0]);
            assert!((b(scale_ar).size[0] - 60.0).abs() < 0.01 && (b(scale_ar).size[1] - 120.0).abs() < 0.01);
            assert_eq!(b(auto_sc).size, [150.0, 80.0]);
            assert_eq!(b(inside).pos, [30.0, 20.0]);
            assert_eq!(b(inside).size, [40.0, 20.0]);
        });
        out.textures_delta.clear();
    }

    #[test]
    fn scrolling_frame_insets_gutter_and_direction() {
        let mut doc = Document::new_place();
        let root = doc.dom.root_ref();
        let parent = doc.dom.insert(root, InstanceBuilder::new("Frame"));
        let sf = doc.dom.insert(parent, InstanceBuilder::new("ScrollingFrame")
            .with_property("Size", UDim2::new(UDim::new(0.0, 180), UDim::new(0.0, 100)))
            .with_property("CanvasSize", UDim2::new(UDim::new(0.0, 0), UDim::new(0.0, 300)))
            .with_property("ScrollBarThickness", 16i32)
            .with_property("VerticalScrollBarInset", Enum::from_u32(2))
            .with_property("VerticalScrollBarPosition", Enum::from_u32(1)));
        let child = doc.dom.insert(sf, InstanceBuilder::new("Frame")
            .with_property("Size", UDim2::new(UDim::new(1.0, 0), UDim::new(0.5, 0))));
        let ctx = egui::Context::default();
        let mut gui = Gui::default();
        let cam = Camera::default();
        let mut store = AssetStore::default();
        let rect = Rect::from_min_size(Pos2::ZERO, egui::vec2(200.0, 120.0));
        let mut input = DrawIn { render_state: None, doc: &doc, cam: &cam, rect, store: &mut store, doc_dir: None, playing: false, fixed_step: true, roots: Default::default() };
        let mut out = ctx.run_ui(Default::default(), |ui| {
            let ctx = ui.ctx();
            let mut walk = Walk { gui: &mut gui, ctx, input: &mut input, items: Vec::new(), hits: Vec::new(), seq: 0, boxes: Vec::new(), global_z: false, abs_offset: [0.0; 2], screen: None, cur: Ref::none(), rec: Vec::new(), replaying: 0, table_cells: HashMap::new() };
            walk.children(parent, rect, Down { aff: Aff::ID, clip: rect, alpha: 1.0, tint: [1.0; 3], scale: 1.0, z: 0, turn: 0.0 });
            let b = |r: Ref| walk.boxes.iter().find(|(k, _)| *k == r).unwrap().1;
            // gutter = 16 + 1 = 17 -> acs = (163, 300), aws = (163, 100), child shifted right by 17.
            assert_eq!(b(sf).canvas, Some([163.0, 300.0]));
            assert_eq!(b(sf).window, Some([163.0, 100.0]));
            assert_eq!(b(child).pos, [17.0, 0.0]);
            assert_eq!(b(child).size, [163.0, 150.0]);
        });
        out.textures_delta.clear();
    }

    #[test]
    fn page_layout_slots_and_circular_wrap() {
        let mut doc = Document::new_place();
        let root = doc.dom.root_ref();
        let parent = doc.dom.insert(root, InstanceBuilder::new("Frame"));
        let lay = doc.dom.insert(parent, InstanceBuilder::new("UIPageLayout")
            .with_property("Padding", UDim::new(0.0, 10))
            .with_property("Circular", true)
            .with_property("SortOrder", Enum::from_u32(2)));
        let p: Vec<Ref> = (1..=4).map(|i| {
            doc.dom.insert(parent, InstanceBuilder::new("Frame")
                .with_property("LayoutOrder", i)
                .with_property("Size", UDim2::new(UDim::new(1.0, 0), UDim::new(1.0, 0))))
        }).collect();
        let ctx = egui::Context::default();
        let mut gui = Gui::default();
        let cam = Camera::default();
        let mut store = AssetStore::default();
        let rect = Rect::from_min_size(Pos2::ZERO, egui::vec2(200.0, 120.0));
        let mut input = DrawIn { render_state: None, doc: &doc, cam: &cam, rect, store: &mut store, doc_dir: None, playing: false, fixed_step: true, roots: Default::default() };
        let mut out = ctx.run_ui(Default::default(), |ui| {
            let ctx = ui.ctx();
            let mut walk = Walk { gui: &mut gui, ctx, input: &mut input, items: Vec::new(), hits: Vec::new(), seq: 0, boxes: Vec::new(), global_z: false, abs_offset: [0.0; 2], screen: None, cur: Ref::none(), rec: Vec::new(), replaying: 0, table_cells: HashMap::new() };
            walk.children(parent, rect, Down { aff: Aff::ID, clip: rect, alpha: 1.0, tint: [1.0; 3], scale: 1.0, z: 0, turn: 0.0 });
            let b = |r: Ref| walk.boxes.iter().find(|(k, _)| *k == r).unwrap().1;
            // Default Horizontal, stride = 210, acs = (4*200 + 3*10, 120) = (830, 120).
            // Circular initial (dir=0): slots [0, +1, -2, -1].
            assert_eq!(b(lay).size, [830.0, 120.0]);
            assert_eq!(b(p[0]).pos[0], 0.0);
            assert_eq!(b(p[1]).pos[0], 210.0);
            assert_eq!(b(p[2]).pos[0], -420.0);
            assert_eq!(b(p[3]).pos[0], -210.0);
        });
        out.textures_delta.clear();
    }

    #[test]
    fn list_flex_shrink_and_size_constraint_clamp() {
        let mut doc = Document::new_place();
        let root = doc.dom.root_ref();
        let parent = doc.dom.insert(root, InstanceBuilder::new("Frame"));
        doc.dom.insert(parent, InstanceBuilder::new("UIListLayout")
            .with_property("FillDirection", Enum::from_u32(0))
            .with_property("SortOrder", Enum::from_u32(2)));
        let a = doc.dom.insert(parent, InstanceBuilder::new("Frame").with_property("LayoutOrder", 1i32)
            .with_property("Size", UDim2::new(UDim::new(0.0, 100), UDim::new(0.0, 20))));
        doc.dom.insert(a, InstanceBuilder::new("UIFlexItem").with_property("FlexMode", Enum::from_u32(2)));
        doc.dom.insert(a, InstanceBuilder::new("UISizeConstraint")
            .with_property("MinSize", Vector2::new(80.0, 0.0))
            .with_property("MaxSize", Vector2::new(200.0, 200.0)));
        let b_ref = doc.dom.insert(parent, InstanceBuilder::new("Frame").with_property("LayoutOrder", 2i32)
            .with_property("Size", UDim2::new(UDim::new(0.0, 100), UDim::new(0.0, 20))));
        doc.dom.insert(b_ref, InstanceBuilder::new("UIFlexItem").with_property("FlexMode", Enum::from_u32(2)));
        let ctx = egui::Context::default();
        let mut gui = Gui::default();
        let cam = Camera::default();
        let mut store = AssetStore::default();
        let rect = Rect::from_min_size(Pos2::ZERO, egui::vec2(140.0, 40.0));
        let mut input = DrawIn { render_state: None, doc: &doc, cam: &cam, rect, store: &mut store, doc_dir: None, playing: false, fixed_step: true, roots: Default::default() };
        let mut out = ctx.run_ui(Default::default(), |ui| {
            let ctx = ui.ctx();
            let mut walk = Walk { gui: &mut gui, ctx, input: &mut input, items: Vec::new(), hits: Vec::new(), seq: 0, boxes: Vec::new(), global_z: false, abs_offset: [0.0; 2], screen: None, cur: Ref::none(), rec: Vec::new(), replaying: 0, table_cells: HashMap::new() };
            walk.children(parent, rect, Down { aff: Aff::ID, clip: rect, alpha: 1.0, tint: [1.0; 3], scale: 1.0, z: 0, turn: 0.0 });
            let bx = |r: Ref| walk.boxes.iter().find(|(k, _)| *k == r).unwrap().1;
            // Overflow = 60: `a` hits MinSize 80 (shrinks 20), `b` absorbs remaining 40 -> 60.
            assert!((bx(a).size[0] - 80.0).abs() < 0.01);
            assert!((bx(b_ref).size[0] - 60.0).abs() < 0.01);
        });
        out.textures_delta.clear();
    }

}
