//! The Properties panel, drawn like Studio's (checks/ui/studio/props-*.png):
//! a filter field, then category bands (25 px, (32, 34, 39), bold name and
//! a ▼ / ▶ to fold) over two-column rows (21 px with a 1-px (32, 34, 39)
//! separator; names indented 25 px; the divider at 48 % of the width).
//! Text (188, 190, 200), read-only rows (73, 77, 90). Values as Studio shows
//! them: check boxes, colour swatches with "[r, g, b]" (and the BrickColor
//! name when the colour is one), enum and BrickColor drop-downs, vectors as
//! "x, y, z" that unfold into X / Y / Z, CFrames as Position / Orientation.
//! Which rows, in which category and order: src/props_layout.rs.

use std::collections::{HashMap, HashSet};

use egui::{Color32, FontId, Pos2, Rect, Sense, Stroke, Vec2};
use glam::{Mat3, Vec3};
use rbx_dom_weak::types::{
    BrickColor, Color3, Color3uint8, ColorSequence, ColorSequenceKeypoint, CustomPhysicalProperties, Enum,
    Font as RbxFont, FontStyle, FontWeight, NumberRange, NumberSequence, NumberSequenceKeypoint, PhysicalProperties,
    Rect as RbxRect, Ref, Tags, UDim, UDim2, Variant, Vector2, Vector2int16, Vector3, Vector3int16,
};
use rbx_dom_weak::ustr;

use crate::app::Editor;
use rustudio_core::doc;
use rustudio_core::math::Xform;
use crate::props_layout::{Category, Frame, Row};

pub(crate) const ROW_H: f32 = 21.0;
pub(crate) const CAT_H: f32 = 25.0;
pub(crate) const INDENT: f32 = 25.0;
pub(crate) const DIVIDER: f32 = 0.48;
pub const TEXT: Color32 = Color32::from_rgb(188, 190, 200);
pub const READ_ONLY: Color32 = Color32::from_rgb(73, 77, 90);
pub const BAND: Color32 = Color32::from_rgb(32, 34, 39);
const BOX_OFF: Color32 = Color32::from_rgb(40, 41, 49);
pub(crate) const FONT: f32 = 12.5;

#[derive(Default)]
pub struct PropertiesState {
    pub filter: String,
    /// Folded categories (Surface starts folded, as in Studio).
    folded: HashSet<String>,
    unfolded: HashSet<String>,
    /// Unfolded vector rows ("Size", "CFrame.Position"…).
    open_rows: HashSet<String>,
    /// The cell being typed into: (row key, text).
    editing: Option<(String, String)>,
    /// True from the click that opens a text cell until `TextEdit` has focus
    /// and its full-selection range has been applied.
    select_all_pending: bool,
    /// The word a double-click selected, held until the button is released
    /// (egui would otherwise select on release and flash a caret on press).
    /// Quick presses counted toward double/triple click: frame, count, time, place.
    press_frame: u64,
    press_count: u8,
    press_time: f64,
    press_pos: Option<Pos2>,
    /// A drag selecting text: (click count, anchor start, anchor end).
    drag: Option<(u8, usize, usize)>,
    /// The edit box as last laid out: (row key, galley, text origin, rect).
    edit_layout: Option<(String, std::sync::Arc<egui::Galley>, Pos2, Rect)>,
    cache: HashMap<(String, bool), Vec<Category>>,
    /// Row labels drawn last frame, in order (the self-test reads them).
    pub drawn: Vec<String>,
    /// Studio's BrickColor palette popup, when open.
    pub brick_popup: Option<crate::color_editors::BrickPopup>,
    /// The ribbon Color button's BrickColor popup (crate::ribbon_quick).
    pub ribbon_brick: Option<crate::color_editors::BrickPopup>,
    /// Studio's Select Color dialog, when open.
    pub color_dialog: Option<crate::color_editors::ColorDialog>,
    /// The dialog's 16 custom colours (kept for the session, as Qt does).
    pub custom_colors: [Option<[u8; 3]>; 16],
    /// A slider being dragged: the property and the values before.
    pub slider_drag: Option<SliderDrag>,
    /// The ranged row last clicked: its editor stays shown (Studio shows it
    /// on the selected row; rustudio also on hover).
    pub active_row: Option<String>,
    active_for: Option<Ref>,
    /// Each drawn row's rect, by property name (the self-test reads them).
    pub row_rects: HashMap<String, Rect>,
    /// Active Instance (`Variant::Ref`) property picker: clicking an instance
    /// in Explorer or the 3D viewport assigns it to `prop` without changing selection.
    pub ref_pick: Option<RefPick>,
}

#[derive(Clone, Debug)]
pub struct RefPick {
    pub prop: String,
    pub targets: Vec<Ref>,
    pub expected_class: String,
}

pub struct SliderDrag {
    pub prop: String,
    pub originals: Vec<(Ref, Variant)>,
}

/// A number property's value as f64.
fn number_of(v: &Variant) -> Option<f64> {
    match v {
        Variant::Float32(f) => Some(*f as f64),
        Variant::Float64(f) => Some(*f),
        Variant::Int32(i) => Some(*i as f64),
        Variant::Int64(i) => Some(*i as f64),
        _ => None,
    }
}

/// `x` in the same number type as `like`.
fn number_like(like: &Variant, x: f64) -> Variant {
    match like {
        Variant::Float64(_) => Variant::Float64(x),
        Variant::Int32(_) => Variant::Int32(x.round() as i32),
        Variant::Int64(_) => Variant::Int64(x.round() as i64),
        _ => Variant::Float32(x as f32),
    }
}

/// `rgb` in the same colour type as `like` (Color3uint8 for parts, constant
/// 2-stop ColorSequence for UIGradient.Color).
fn color_variant(like: &Variant, c: [u8; 3]) -> Variant {
    let c3 = Color3::new(c[0] as f32 / 255.0, c[1] as f32 / 255.0, c[2] as f32 / 255.0);
    match like {
        Variant::Color3(_) => Variant::Color3(c3),
        Variant::ColorSequence(_) => Variant::ColorSequence(ColorSequence {
            keypoints: vec![ColorSequenceKeypoint::new(0.0, c3), ColorSequenceKeypoint::new(1.0, c3)],
        }),
        _ => Variant::Color3uint8(Color3uint8::new(c[0], c[1], c[2])),
    }
}

fn has_prop(class: &str, name: &str) -> bool {
    let db = doc::db();
    let Some(desc) = db.classes.get(class) else {
        return false;
    };
    db.superclasses_iter(desc).any(|c| c.properties.contains_key(name))
}

/// The filter field's id (Ctrl+Shift+P focuses it, as in Studio).
pub const PROPS_FILTER_ID: &str = "properties_filter";

/// The Properties panel's title: Studio's "Properties - Class "Name"".
pub fn panel_title(ed: &Editor) -> String {
    let side = ed
        .game
        .play
        .as_ref()
        .filter(|p| p.mode == rustudio_engine::runtime::RunMode::Play)
        .map(|p| if p.showing == rustudio_engine::runtime::Side::Server { " [Server]" } else { " [Client]" })
        .unwrap_or("");
    let sel = &ed.game.doc.selection;
    match sel.first().and_then(|r| ed.game.doc.dom.get_by_ref(*r)) {
        Some(i) if sel.len() > 1 => format!("Properties - {} \"{}\" (+{} more){side}", i.class, i.name, sel.len() - 1),
        Some(i) => format!("Properties - {} \"{}\"{side}", i.class, i.name),
        None => format!("Properties{side}"),
    }
}

/// Expected target class for a `Variant::Ref` property (from the Studio API dump,
/// falling back to `"Instance"`).
pub fn expected_ref_class(class: &str, prop: &str) -> String {
    if prop == "Parent" {
        return "Instance".into();
    }
    rustudio_core::api::get()
        .and_then(|api| api.prop_value_class(class, prop))
        .unwrap_or("Instance")
        .to_string()
}

/// Studio's number text: up to 3 decimals, no trailing zeros.
pub fn fmt_num(f: f64) -> String {
    let s = format!("{f:.3}");
    let s = s.trim_end_matches('0').trim_end_matches('.');
    if s == "-0" { "0".into() } else { s.to_string() }
}

fn fmt_v3(v: Vec3) -> String {
    format!("{}, {}, {}", fmt_num(v.x as f64), fmt_num(v.y as f64), fmt_num(v.z as f64))
}

/// Parses `n` comma-separated numbers; a single number fills every
/// component (typing "0" into a Vector3 gives 0, 0, 0).
fn parse_nums(s: &str, n: usize) -> Option<Vec<f32>> {
    let p: Vec<f32> = s.split(',').map(|x| x.trim().trim_matches(['[', ']', '{', '}']).trim()).filter(|x| !x.is_empty()).map(|x| x.parse().ok()).collect::<Option<_>>()?;
    match p.len() {
        1 => Some(vec![p[0]; n]),
        l if l == n => Some(p),
        _ => None,
    }
}

/// The run of non-separator characters around char index `c` (a whole
/// number like "-2840.53" in "1, -2840.53, 3").
fn word_bounds(text: &str, c: usize) -> (usize, usize) {
    let chars: Vec<char> = text.chars().collect();
    let sep = |ch: char| ch == ',' || ch.is_whitespace();
    let c = c.min(chars.len());
    let mut a = c;
    while a > 0 && !sep(chars[a - 1]) {
        a -= 1;
    }
    let mut b = c;
    while b < chars.len() && !sep(chars[b]) {
        b += 1;
    }
    (a, b)
}

fn parse_v3(s: &str) -> Option<Vec3> {
    parse_nums(s, 3).map(|p| Vec3::new(p[0], p[1], p[2]))
}

/// The palette BrickColor closest to a colour.
pub(crate) fn nearest_brick(rgb: [u8; 3]) -> BrickColor {
    (1..=1032u16)
        .filter_map(BrickColor::from_number)
        .min_by_key(|b| {
            let c = rustudio_render::scene::brick_rgb(*b);
            (0..3).map(|i| (c[i] as i32 - rgb[i] as i32).pow(2)).sum::<i32>()
        })
        .unwrap_or(BrickColor::MediumStoneGrey)
}

/// The BrickColor whose colour this exactly is (Studio adds its name).
fn brick_name(rgb: [u8; 3]) -> Option<String> {
    (1..=1032u16)
        .filter_map(BrickColor::from_number)
        .find(|b| rustudio_render::scene::brick_rgb(*b) == rgb)
        .map(|b| b.to_string())
}

fn mul(a: &Xform, b: &Xform) -> Xform {
    Xform { pos: a.pos + a.rot * b.pos, rot: a.rot * b.rot }
}

/// The rotation after setting an Attachment's Axis (or SecondaryAxis): the
/// same rule as the Lua runtime's setter (runtime.rs). Axis becomes the X
/// axis, the old Y made perpendicular to it (the old Z when parallel);
/// SecondaryAxis keeps X and takes Y perpendicular to it, ignored when
/// parallel. Z = X × Y, and a zero vector changes nothing.
fn attachment_rot_after_axis(rot: Mat3, v: Vec3, secondary: bool) -> Mat3 {
    let (x0, y0, z0) = (rot.x_axis, rot.y_axis, rot.z_axis);
    if v.length_squared() < 1e-12 {
        rot
    } else if secondary {
        let y = v - x0 * v.dot(x0);
        if y.length_squared() < 1e-10 {
            rot
        } else {
            let y = y.normalize();
            Mat3::from_cols(x0, y, x0.cross(y))
        }
    } else {
        let x = v.normalize();
        let y = y0 - x * y0.dot(x);
        let y = if y.length_squared() < 1e-10 { z0.cross(x) } else { y };
        let y = y.normalize();
        Mat3::from_cols(x, y, x.cross(y))
    }
}

fn cf_of(v: Option<Variant>) -> Option<Xform> {
    match v? {
        Variant::CFrame(cf) => Some(Xform::from_cframe(&cf)),
        Variant::OptionalCFrame(Some(cf)) => Some(Xform::from_cframe(&cf)),
        _ => None,
    }
}

/// A value's text as Studio shows it (None: drawn by its own widget).
fn value_text(doc: &doc::Document, v: &Variant) -> String {
    match v {
        Variant::String(s) => {
            if s.contains('\n') {
                s.replace("\r\n", " ").replace('\n', " ")
            } else {
                s.clone()
            }
        }
        Variant::Int32(i) => i.to_string(),
        Variant::Int64(i) => i.to_string(),
        Variant::Float32(f) => fmt_num(*f as f64),
        Variant::Float64(f) => fmt_num(*f),
        Variant::Vector3(v) => fmt_v3(rustudio_core::math::v3(*v)),
        Variant::Vector2(v) => format!("{}, {}", fmt_num(v.x as f64), fmt_num(v.y as f64)),
        Variant::Vector3int16(v) => format!("{}, {}, {}", v.x, v.y, v.z),
        Variant::Vector2int16(v) => format!("{}, {}", v.x, v.y),
        // Studio UDimComponentItem::toString ("%1, %2").
        Variant::UDim(u) => format!("{}, {}", fmt_num(u.scale as f64), u.offset),
        // Studio UDim2PropertyItem::toString ("{%1, %2},{%3, %4}").
        Variant::UDim2(u) => format!(
            "{{{}, {}}},{{{}, {}}}",
            fmt_num(u.x.scale as f64),
            u.x.offset,
            fmt_num(u.y.scale as f64),
            u.y.offset
        ),
        // Studio RectPropertyItem::toString ("%1, %2, %3, %4").
        Variant::Rect(r) => format!(
            "{}, {}, {}, {}",
            fmt_num(r.min.x as f64),
            fmt_num(r.min.y as f64),
            fmt_num(r.max.x as f64),
            fmt_num(r.max.y as f64)
        ),
        // Studio NumberRangePropertyItem (" %f , %f ").
        Variant::NumberRange(r) => format!("{}, {}", fmt_num(r.min as f64), fmt_num(r.max as f64)),
        // Studio NumberSequencePropertyItem::toString:
        // constant 2-point zero-envelope sequence shows the single number; else "<NumberSequence>".
        Variant::NumberSequence(s) => {
            if s.keypoints.len() == 2
                && (s.keypoints[0].value - s.keypoints[1].value).abs() < 1e-6
                && s.keypoints[0].envelope == 0.0
                && s.keypoints[1].envelope == 0.0
            {
                fmt_num(s.keypoints[0].value as f64)
            } else {
                "<NumberSequence>".to_string()
            }
        }
        // Studio ColorSequencePropertyItem::toString:
        // constant 2-point sequence shows "[r, g, b]"; else "<ColorSequence>".
        Variant::ColorSequence(s) => {
            if let Some(rgb) = color_seq_constant(s) {
                format!("[{}, {}, {}]", rgb[0], rgb[1], rgb[2])
            } else {
                "<ColorSequence>".to_string()
            }
        }
        Variant::Ref(r) => doc.dom.get_by_ref(*r).map(|i| i.name.clone()).unwrap_or_default(),
        Variant::ContentId(c) => c.as_str().to_string(),
        Variant::Content(c) => c.as_uri().unwrap_or_default().to_string(),
        Variant::Font(f) => font_display_name(&f.family),
        Variant::Faces(f) => format!("{f:?}"),
        Variant::Axes(a) => format!("{a:?}"),
        Variant::UniqueId(u) => u.to_string(),
        Variant::PhysicalProperties(_) => String::new(),
        Variant::CFrame(cf) => fmt_v3(rustudio_core::math::v3(cf.position)),
        other => format!("{:?}", other.ty()),
    }
}

fn color_seq_constant(s: &ColorSequence) -> Option<[u8; 3]> {
    if s.keypoints.len() == 2 {
        let a = s.keypoints[0].color;
        let b = s.keypoints[1].color;
        if (a.r - b.r).abs() < 1e-6 && (a.g - b.g).abs() < 1e-6 && (a.b - b.b).abs() < 1e-6 {
            return Some([
                (a.r.clamp(0.0, 1.0) * 255.0).round() as u8,
                (a.g.clamp(0.0, 1.0) * 255.0).round() as u8,
                (a.b.clamp(0.0, 1.0) * 255.0).round() as u8,
            ]);
        }
    }
    None
}

fn font_display_name(family: &str) -> String {
    family.rsplit('/').next().unwrap_or(family).trim_end_matches(".json").to_string()
}

/// Built-in Studio font family names (from `<studio>/content/fonts/families/*.json`,
/// with a static fallback list of Studio's 44 built-in families).
fn studio_font_families() -> &'static Vec<String> {
    static FAMILIES: std::sync::OnceLock<Vec<String>> = std::sync::OnceLock::new();
    FAMILIES.get_or_init(|| {
        let mut out = Vec::new();
        if let Some(dir) = rustudio_core::default_scripts::studio_dir() {
            if let Ok(entries) = std::fs::read_dir(dir.join("content/fonts/families")) {
                for entry in entries.flatten() {
                    let path = entry.path();
                    if path.extension().is_some_and(|e| e == "json") {
                        if let Some(stem) = path.file_stem().and_then(|s| s.to_str()) {
                            out.push(stem.to_string());
                        }
                    }
                }
            }
        }
        if out.is_empty() {
            out = [
                "AccanthisADFStd", "AmaticSC", "Arimo", "Balthazar", "Bangers", "BuilderExtended",
                "BuilderMono", "BuilderSans", "ComicNeueAngular", "Creepster", "DenkOne", "Fondamento",
                "FredokaOne", "GrenzeGotisch", "Guru", "HighwayGothic", "Inconsolata", "IndieFlower",
                "JosefinSans", "Jura", "Kalam", "LegacyArial", "LegacyArimo", "LuckiestGuy",
                "Merriweather", "Michroma", "Montserrat", "NotoSansCJKFallback", "Nunito", "Oswald",
                "PatrickHand", "PermanentMarker", "PressStart2P", "Roboto", "RobotoCondensed",
                "RobotoMono", "RomanAntique", "Sarpanch", "SourceSansPro", "SpecialElite",
                "TitilliumWeb", "Ubuntu", "Zekton",
            ]
            .into_iter()
            .map(String::from)
            .collect();
        }
        out.sort_by_key(|s| s.to_lowercase());
        out.dedup();
        out
    })
}

/// Sub-component mutation for multi-selection edits (matches Studio's per-component
/// update in `UDimComponentItem`, `RectPropertyItem`, `FontPropertyItem`).
#[derive(Clone, Debug)]
enum SubEdit {
    Vec3(usize, f32),
    Vec2(usize, f32),
    UDimScale(f32),
    UDimOffset(i32),
    UDim2X(UDim),
    UDim2Y(UDim),
    UDim2XScale(f32),
    UDim2XOffset(i32),
    UDim2YScale(f32),
    UDim2YOffset(i32),
    RectComp(usize, f32),
    RangeMin(f32),
    RangeMax(f32),
    FontFamily(String),
    FontWeight(FontWeight),
    FontStyle(FontStyle),
}

impl SubEdit {
    fn apply(&self, old: &Variant) -> Option<Variant> {
        Some(match (self, old) {
            (SubEdit::Vec3(i, x), Variant::Vector3(v)) => {
                let mut a = [v.x, v.y, v.z];
                a[*i] = *x;
                Variant::Vector3(Vector3::new(a[0], a[1], a[2]))
            }
            (SubEdit::Vec2(i, x), Variant::Vector2(v)) => {
                let mut a = [v.x, v.y];
                a[*i] = *x;
                Variant::Vector2(Vector2::new(a[0], a[1]))
            }
            (SubEdit::UDimScale(s), Variant::UDim(u)) => Variant::UDim(UDim::new(*s, u.offset)),
            (SubEdit::UDimOffset(o), Variant::UDim(u)) => Variant::UDim(UDim::new(u.scale, *o)),
            (SubEdit::UDim2X(x), Variant::UDim2(u)) => Variant::UDim2(UDim2::new(*x, u.y)),
            (SubEdit::UDim2Y(y), Variant::UDim2(u)) => Variant::UDim2(UDim2::new(u.x, *y)),
            (SubEdit::UDim2XScale(s), Variant::UDim2(u)) => Variant::UDim2(UDim2::new(UDim::new(*s, u.x.offset), u.y)),
            (SubEdit::UDim2XOffset(o), Variant::UDim2(u)) => Variant::UDim2(UDim2::new(UDim::new(u.x.scale, *o), u.y)),
            (SubEdit::UDim2YScale(s), Variant::UDim2(u)) => Variant::UDim2(UDim2::new(u.x, UDim::new(*s, u.y.offset))),
            (SubEdit::UDim2YOffset(o), Variant::UDim2(u)) => Variant::UDim2(UDim2::new(u.x, UDim::new(u.y.scale, *o))),
            // Studio RectPropertyItem sorts (x0, x1) and (y0, y1) so min <= max.
            (SubEdit::RectComp(i, x), Variant::Rect(r)) => {
                let mut a = [r.min.x, r.min.y, r.max.x, r.max.y];
                a[*i] = *x;
                Variant::Rect(RbxRect::new(
                    Vector2::new(a[0].min(a[2]), a[1].min(a[3])),
                    Vector2::new(a[0].max(a[2]), a[1].max(a[3])),
                ))
            }
            (SubEdit::RangeMin(m), Variant::NumberRange(r)) => Variant::NumberRange(NumberRange::new(*m, r.max.max(*m))),
            (SubEdit::RangeMax(m), Variant::NumberRange(r)) => Variant::NumberRange(NumberRange::new(r.min.min(*m), *m)),
            (SubEdit::FontFamily(fam), Variant::Font(f)) => Variant::Font(RbxFont {
                family: fam.clone(),
                weight: f.weight,
                style: f.style,
                cached_face_id: None,
            }),
            (SubEdit::FontWeight(w), Variant::Font(f)) => Variant::Font(RbxFont {
                family: f.family.clone(),
                weight: *w,
                style: f.style,
                cached_face_id: None,
            }),
            (SubEdit::FontStyle(st), Variant::Font(f)) => Variant::Font(RbxFont {
                family: f.family.clone(),
                weight: f.weight,
                style: *st,
                cached_face_id: None,
            }),
            _ => return None,
        })
    }
}

/// What the panel did this frame.
enum Edit {
    Prop(String, Variant),
    SubProp(String, SubEdit),
    Frame(Frame, Option<Vec3>, Option<Vec3>),
    AddTag(String),
    RemoveTag(String),
}

struct Painter<'a> {
    ui: &'a mut egui::Ui,
    width: f32,
    x0: f32,
}

impl Painter<'_> {
    fn row(&mut self, key: &str, h: f32) -> (Rect, egui::Response) {
        let (r, resp) = self.ui.allocate_exact_size(Vec2::new(self.width, h), Sense::click());
        let _ = key;
        (r, resp)
    }
}

impl Editor {
    pub(crate) fn properties_interacting(&self) -> bool {
        self.props.editing.is_some() || self.props.slider_drag.is_some()
            || self.props.brick_popup.is_some() || self.props.color_dialog.is_some()
    }

    pub fn properties_ui(&mut self, ui: &mut egui::Ui) {
        let _p = rustudio_core::bench::scope("properties_build");
        // Studio: the filter field always, its shortcut in the hint; the
        // selection's class and name are in the panel's title.
        ui.add(
            egui::TextEdit::singleline(&mut self.props.filter)
                .id(egui::Id::new(PROPS_FILTER_ID))
                .hint_text(format!("Filter Properties ({})", crate::studio_ui::shortcut_label("Ctrl+Shift+P")))
                .desired_width(f32::INFINITY),
        );
        ui.add_space(2.0);
        self.props.drawn.clear();
        self.ui_rects.insert("props.panel", ui.max_rect());
        let sel = self.game.doc.selection.clone();
        let Some(&first) = sel.first() else {
            self.hud.property_rows = 0;
            return;
        };
        let Some(class) = self.game.doc.class_of(first).map(|c| c.to_string()) else {
            return;
        };
        if self.props.active_for != Some(first) {
            self.props.active_for = Some(first);
            self.props.active_row = None;
        }
        let can_collide = !matches!(self.game.doc.prop(first, "CanCollide"), Some(Variant::Bool(false)));
        let mut cats = self
            .props
            .cache
            .entry((class.clone(), can_collide))
            .or_insert_with(|| crate::props_layout::rows(&class, can_collide))
            .clone();
        // Multi-selection: keep only properties common to every selected class.
        if sel.len() > 1 {
            // Each distinct class once: a 1 000-part selection is a handful of classes.
            let mut classes: Vec<String> = sel.iter().filter_map(|r| self.game.doc.class_of(*r).map(|c| c.to_string())).collect();
            classes.sort_unstable();
            classes.dedup();
            // Origin / Pivot rows stay while every selected class has them
            // (a Part with a WedgePart or MeshPart keeps them).
            let frame_kind = if doc::is_a(&class, "BasePart") {
                "BasePart"
            } else if doc::is_a(&class, "PVInstance") {
                "PVInstance"
            } else {
                class.as_str()
            };
            for cat in &mut cats {
                cat.rows.retain(|row| match row {
                    Row::Prop(n, _) => {
                        matches!(n.as_str(), "Name" | "ClassName" | "Parent" | "Archivable" | "UniqueId")
                            || classes.iter().all(|c| has_prop(c, n))
                    }
                    Row::Frame(_, _) => classes.iter().all(|c| doc::is_a(c, frame_kind)),
                });
            }
        }
        let filter = self.props.filter.trim().to_lowercase();
        let mut edits: Vec<Edit> = Vec::new();
        egui::ScrollArea::vertical()
            .auto_shrink([false, false])
            .id_salt("props_scroll")
            .show(ui, |ui| {
                ui.spacing_mut().item_spacing = Vec2::ZERO;
                let width = ui.available_width();
                let x0 = ui.cursor().min.x;
                let mut p = Painter { ui, width, x0 };
                for cat in &cats {
                    let rows: Vec<&Row> = cat
                        .rows
                        .iter()
                        .filter(|r| {
                            filter.is_empty()
                                || match r {
                                    Row::Prop(n, _) => {
                                        n.to_lowercase().contains(&filter)
                                            || (n == "AudioContent"
                                                && matches!(class.as_str(), "Sound" | "AudioPlayer")
                                                && "preview".contains(&filter))
                                    }
                                    Row::Frame(l, _) => l.to_lowercase().contains(&filter),
                                }
                        })
                        .collect();
                    if rows.is_empty() {
                        continue;
                    }
                    let folded = filter.is_empty() && self.category_folded(cat);
                    if self.band(&mut p, &cat.name, folded, None) {
                        self.toggle_category(cat);
                    }
                    if folded {
                        continue;
                    }
                    for r in rows {
                        match r {
                            Row::Prop(name, ro) => self.prop_row(&mut p, &sel, first, &class, name, *ro, &mut edits),
                            Row::Frame(label, f) => self.frame_rows(&mut p, first, label, *f, &mut edits),
                        }
                    }
                }
                self.tags_section(&mut p, first, &mut edits);
                self.attributes_section(&mut p, first);
            });
        self.hud.property_rows = self.props.drawn.len();
        let ctx = ui.ctx().clone();
        self.color_editors_ui(&ctx, &mut edits);
        self.apply_prop_edits(&sel, first, edits);
    }

    fn category_folded(&self, cat: &Category) -> bool {
        if cat.collapsed {
            !self.props.unfolded.contains(&cat.name)
        } else {
            self.props.folded.contains(&cat.name)
        }
    }

    fn toggle_category(&mut self, cat: &Category) {
        let set = if cat.collapsed { &mut self.props.unfolded } else { &mut self.props.folded };
        if !set.remove(&cat.name) {
            set.insert(cat.name.clone());
        }
    }

    /// A category band; returns true when it was clicked (fold / unfold).
    /// `plus`: draw Studio's "+" at the right (Tags, Attributes) and report
    /// its click through the returned flag's second meaning via `plus_hit`.
    fn band(&mut self, p: &mut Painter, name: &str, folded: bool, plus_hit: Option<&mut bool>) -> bool {
        let (r, resp) = p.row(name, CAT_H);
        let painter = p.ui.painter();
        painter.rect_filled(r, 0.0, BAND);
        let tri = Rect::from_center_size(Pos2::new(r.min.x + 10.0, r.center().y), Vec2::splat(12.0));
        rustudio_engine::ti::paint(painter, tri, if folded { "caret-right-filled" } else { "caret-down-filled" }, TEXT);
        painter.text(
            Pos2::new(r.min.x + INDENT, r.center().y),
            egui::Align2::LEFT_CENTER,
            name,
            FontId::new(FONT, egui::FontFamily::Name("bold".into())),
            TEXT,
        );
        let mut clicked = resp.clicked();
        if let Some(hit) = plus_hit {
            let pr = Rect::from_center_size(Pos2::new(r.max.x - 14.0, r.center().y), Vec2::splat(14.0));
            rustudio_engine::ti::paint(p.ui.painter(), pr, "plus", TEXT);
            if clicked && resp.interact_pointer_pos().is_some_and(|q| pr.expand(3.0).contains(q)) {
                *hit = true;
                clicked = false;
            }
        }
        let _ = p.x0;
        clicked
    }

    /// The two-column cell frame of a row: (row rect, name right edge, value rect).
    fn cells(p: &mut Painter, key: &str) -> (Rect, Rect, egui::Response) {
        let (r, resp) = p.row(key, ROW_H + 1.0);
        let painter = p.ui.painter();
        painter.line_segment([Pos2::new(r.min.x, r.max.y - 0.5), Pos2::new(r.max.x, r.max.y - 0.5)], Stroke::new(1.0, BAND));
        let div = r.min.x + (r.width() * DIVIDER).round();
        painter.line_segment([Pos2::new(div, r.min.y), Pos2::new(div, r.max.y)], Stroke::new(1.0, BAND));
        let value = Rect::from_min_max(Pos2::new(div + 5.0, r.min.y), Pos2::new(r.max.x - 4.0, r.max.y - 1.0));
        (r, value, resp)
    }

    fn name_text(p: &mut Painter, r: Rect, depth: f32, name: &str, color: Color32, arrow: Option<bool>) {
        Self::name_text_styled(p, r, depth, name, color, arrow, false);
    }

    fn name_text_styled(p: &mut Painter, r: Rect, depth: f32, name: &str, color: Color32, arrow: Option<bool>, bold: bool) {
        // Clipped at the divider, as Studio's name column is.
        let div = r.min.x + (r.width() * DIVIDER).round();
        let clip = if depth < 0.0 { r } else { Rect::from_min_max(r.min, Pos2::new(div - 2.0, r.max.y)) };
        let painter = p.ui.painter_at(clip);
        let x = r.min.x + INDENT + depth * 18.0;
        if let Some(open) = arrow {
            let tri = Rect::from_center_size(Pos2::new(x - 15.0, r.center().y), Vec2::splat(11.0));
            rustudio_engine::ti::paint(&painter, tri, if open { "caret-down-filled" } else { "caret-right-filled" }, color);
        }
        let font = if bold {
            FontId::new(FONT, egui::FontFamily::Name("bold".into()))
        } else {
            FontId::proportional(FONT)
        };
        painter.text(Pos2::new(x, r.center().y), egui::Align2::LEFT_CENTER, name, font, color);
    }

    fn value_label(p: &mut Painter, v: Rect, text: &str, color: Color32) {
        let painter = p.ui.painter_at(v);
        painter.text(Pos2::new(v.min.x, v.center().y), egui::Align2::LEFT_CENTER, text, FontId::proportional(FONT), color);
    }

    /// An editable text cell: typing replaces the shown text, Enter or
    /// leaving commits; returns the committed text.
    ///
    /// The mouse selects as Studio's edit box does, on the press rather than
    /// egui's release: a click opens the cell with everything selected, a
    /// second click picks the number under the pointer, a third the whole
    /// text, and dragging after a double-click extends word by word.
    fn text_cell(&mut self, p: &mut Painter, key: &str, v: Rect, shown: &str, ro: bool, clicked: bool) -> Option<String> {
        text_cell_on(&mut self.props, self.game.frame_no, p, key, v, shown, ro, clicked)
    }

    fn prop_value(&self, r: Ref, class: &str, name: &str) -> Option<Variant> {
        let parent_is_root = self.game.doc.dom.get_by_ref(r).is_some_and(|i| i.parent() == self.game.doc.dom.root_ref());
        match name {
            "ClassName" => Some(Variant::String(class.to_string())),
            "Name" => self.game.doc.dom.get_by_ref(r).map(|i| Variant::String(i.name.clone())),
            "Parent" if parent_is_root => Some(Variant::String(self.game.doc.title.clone())),
            "Parent" => self.game.doc.dom.get_by_ref(r).map(|i| Variant::Ref(i.parent())),
            // A Part resizes on every face by whole studs (Studio: "Right, Top,
            // Back, Left, Bottom, Front", 1).
            "ResizeableFaces" if self.game.doc.prop(r, name).is_none() => {
                Some(Variant::String("Right, Top, Back, Left, Bottom, Front".into()))
            }
            "ResizeIncrement" if self.game.doc.prop(r, name).is_none() => Some(Variant::Int32(1)),
            // A part keeps only Color; its BrickColor is the nearest palette
            // colour (Studio shows both).
            "BrickColor" => self.game.doc.prop(r, name).or_else(|| match self.game.doc.prop(r, "Color") {
                Some(Variant::Color3uint8(c)) => Some(Variant::BrickColor(nearest_brick([c.r, c.g, c.b]))),
                _ => None,
            }),
            // A part's mass facts, read-only and worked out as Studio shows
            // them (captured RefPart 4×1.2×2 Plastic: Mass 6.72, AssemblyMass
            // inf when anchored, root itself, centre at its position).
            "Mass" | "AssemblyMass" | "AssemblyRootPart" | "AssemblyCenterOfMass" if doc::is_a(class, "BasePart") => {
                let size = match self.game.doc.prop(r, "Size") {
                    Some(Variant::Vector3(s)) => s,
                    _ => Vector3::new(4.0, 1.2, 2.0),
                };
                let density = match self.game.doc.prop(r, "CustomPhysicalProperties") {
                    Some(Variant::PhysicalProperties(PhysicalProperties::Custom(c))) => c.density(),
                    _ => rustudio_core::mech::PART_DENSITY,
                };
                let mass = size.x * size.y * size.z * density;
                let anchored = matches!(self.game.doc.prop(r, "Anchored"), Some(Variant::Bool(true)));
                Some(match name {
                    "Mass" => Variant::Float32(mass),
                    "AssemblyMass" if anchored => Variant::String("inf".into()),
                    "AssemblyMass" => Variant::Float32(mass),
                    "AssemblyRootPart" => Variant::Ref(r),
                    _ => Variant::Vector3(rustudio_core::math::rv3(rustudio_render::scene::xform_of(&self.game.doc, r).map(|x| x.pos).unwrap_or_default())),
                })
            }
            // Studio shows PhysicalProperties as a checkbox: Current is always
            // ticked (greyed), Custom is ticked when set.
            "CurrentPhysicalProperties" => Some(Variant::Bool(true)),
            // A script runs unless Disabled or Enabled = false (as Play reads it).
            "Enabled" if doc::is_a(class, "BaseScript") => Some(Variant::Bool(
                !matches!(self.game.doc.prop(r, "Disabled"), Some(Variant::Bool(true)))
                    && !matches!(self.game.doc.prop(r, "Enabled"), Some(Variant::Bool(false))),
            )),
            // GUI computed layout properties from the live GUI pass (GuiBox).
            "AbsolutePosition" if doc::is_a(class, "GuiBase2d") => {
                let pos = self.game.gui.layout.get(&r).map(|b| b.pos).unwrap_or([0.0, 0.0]);
                Some(Variant::Vector2(Vector2::new(pos[0], pos[1])))
            }
            "AbsoluteSize" if doc::is_a(class, "GuiBase2d") => {
                if let Some(b) = self.game.gui.layout.get(&r) {
                    Some(Variant::Vector2(Vector2::new(b.size[0], b.size[1])))
                } else if doc::is_a(class, "LayerCollector") {
                    let s = self.game.viewport_rect.size();
                    Some(Variant::Vector2(Vector2::new(s.x.round(), s.y.round())))
                } else {
                    Some(Variant::Vector2(Vector2::new(0.0, 0.0)))
                }
            }
            "AbsoluteRotation" if doc::is_a(class, "GuiBase2d") => {
                let rot = self.game.gui.layout.get(&r).map(|b| b.rot).unwrap_or(0.0);
                Some(Variant::Float32(rot))
            }
            "TextBounds" if doc::is_a(class, "GuiObject") => {
                let tb = self.game.gui.layout.get(&r).and_then(|b| b.text).unwrap_or([0.0, 0.0]);
                Some(Variant::Vector2(Vector2::new(tb[0], tb[1])))
            }
            "TextFits" if doc::is_a(class, "GuiObject") => {
                let fits = self.game.gui.layout.get(&r).map(|b| b.text_fits).unwrap_or(true);
                Some(Variant::Bool(fits))
            }
            "AbsoluteCanvasSize" if doc::is_a(class, "ScrollingFrame") => {
                let sz = self.game.gui.layout.get(&r).and_then(|b| b.canvas).unwrap_or([0.0, 0.0]);
                Some(Variant::Vector2(Vector2::new(sz[0], sz[1])))
            }
            "AbsoluteWindowSize" if doc::is_a(class, "ScrollingFrame") => {
                let sz = self.game.gui.layout.get(&r).and_then(|b| b.window).unwrap_or([0.0, 0.0]);
                Some(Variant::Vector2(Vector2::new(sz[0], sz[1])))
            }
            "ContentText" if matches!(class, "TextLabel" | "TextButton" | "TextBox") => {
                let text = match self.game.doc.prop(r, "Text") {
                    Some(Variant::String(s)) => s,
                    _ => String::new(),
                };
                let rich = matches!(self.game.doc.prop(r, "RichText"), Some(Variant::Bool(true)));
                Some(Variant::String(if rich { strip_rich_tags(&text) } else { text }))
            }
            // Image / ImageContent twin fallback on ImageLabel / ImageButton.
            "Image" | "ImageContent" if matches!(class, "ImageLabel" | "ImageButton") => {
                let read = |prop: &str| match self.game.doc.prop(r, prop) {
                    Some(Variant::Content(c)) => c.as_uri().unwrap_or("").to_string(),
                    Some(Variant::ContentId(c)) => c.as_str().to_string(),
                    Some(Variant::String(s)) => s,
                    _ => String::new(),
                };
                let primary = read(name);
                let uri = if !primary.is_empty() {
                    primary
                } else {
                    read(if name == "ImageContent" { "Image" } else { "ImageContent" })
                };
                if name == "ImageContent" {
                    Some(Variant::Content(rbx_dom_weak::types::Content::from(uri)))
                } else {
                    Some(Variant::ContentId(uri.into()))
                }
            }
            // Every instance has a UniqueId in Studio; one not saved yet shows
            // its session id.
            "UniqueId"
                if match self.game.doc.prop(r, name) {
                    Some(Variant::UniqueId(u)) => u.to_string().bytes().all(|c| c == b'0'),
                    other => other.is_none(),
                } =>
            {
                Some(Variant::String(format!("{r}")))
            }
            "CustomPhysicalProperties" => Some(Variant::Bool(matches!(
                self.game.doc.prop(r, name),
                Some(Variant::PhysicalProperties(PhysicalProperties::Custom(_)))
            ))),
            "AudioContent" if matches!(class, "Sound" | "AudioPlayer") => {
                let read = |prop: &str| match self.game.doc.prop(r, prop) {
                    Some(Variant::Content(c)) => c.as_uri().unwrap_or("").to_string(),
                    Some(Variant::ContentId(c)) => c.as_str().to_string(),
                    Some(Variant::String(s)) => s,
                    _ => String::new(),
                };
                let uri = read("AudioContent");
                let uri = if !uri.trim().is_empty() {
                    uri
                } else {
                    read(if class == "Sound" { "SoundId" } else { "Asset" })
                };
                Some(Variant::Content(rbx_dom_weak::types::Content::from(uri)))
            }
            // An Attachment's axes come from its CFrame: Axis is the local X
            // axis, SecondaryAxis the local Y. The World forms apply the
            // parent part's frame (and any Bones above it). Nothing stores
            // them as separate properties, so reading them from the document
            // would show zero.
            "Axis" | "SecondaryAxis" | "WorldAxis" | "WorldSecondaryAxis" if doc::is_a(class, "Attachment") => {
                let doc = &self.game.doc;
                let world = name.starts_with("World");
                let xf = if world {
                    rustudio_core::attachment::world(doc, r, false)?
                } else {
                    rustudio_core::attachment::local(doc, r, false)
                };
                let axis = if name.ends_with("SecondaryAxis") { xf.rot.y_axis } else { xf.rot.x_axis };
                Some(Variant::Vector3(rustudio_core::math::rv3(axis)))
            }
            // WorldCFrame is computed from the attachment's CFrame and its
            // parent part, so it moves with the part and is never a stored
            // property. (The row itself is a Position / Orientation frame;
            // this covers reads outside the row, such as multi-selection.)
            "WorldCFrame" if doc::is_a(class, "Attachment") => {
                rustudio_core::attachment::world(&self.game.doc, r, false).map(|xf| Variant::CFrame(xf.to_cframe()))
            }
            _ if matches!(class, "Sound" | "AudioPlayer") && self.live_sound_prop(r, name).is_some() => {
                self.live_sound_prop(r, name)
            }
            _ => self.game.doc.prop(r, name).or_else(|| typed_empty(class, name)),
        }
    }

    fn prop_row(&mut self, p: &mut Painter, sel: &[Ref], first: Ref, class: &str, name: &str, ro_api: bool, edits: &mut Vec<Edit>) {
        let parent_is_root = self.game.doc.dom.get_by_ref(first).is_some_and(|i| i.parent() == self.game.doc.dom.root_ref());
        let value = self.prop_value(first, class, name);
        let all_vals: Vec<Option<Variant>> = if sel.len() > 1 {
            sel.iter()
                .map(|&r| {
                    let c = self.game.doc.class_of(r).map(|s| s.to_string()).unwrap_or_default();
                    self.prop_value(r, &c, name)
                })
                .collect()
        } else {
            Vec::new()
        };
        let mixed = sel.len() > 1
            && all_vals
                .iter()
                .skip(1)
                .any(|v| !opt_variant_eq(value.as_ref(), v.as_ref()));
        let ro = ro_api
            || matches!(name, "ClassName" | "UniqueId" | "IsInSandbox")
            || (name == "Parent" && parent_is_root);
        let color = if ro { READ_ONLY } else { TEXT };
        let bold = value
            .as_ref()
            .is_some_and(|v| mixed || is_non_default(class, name, v, ro));
        let vec_like = matches!(
            value,
            Some(Variant::Vector3(_))
                | Some(Variant::Vector2(_))
                | Some(Variant::UDim(_))
                | Some(Variant::UDim2(_))
                | Some(Variant::Rect(_))
                | Some(Variant::NumberRange(_))
                | Some(Variant::Font(_))
        );
        let audio_content_row = name == "AudioContent" && matches!(class, "Sound" | "AudioPlayer");
        let audio_folded = self.props.open_rows.contains("fold:AudioContent");
        let open = self.props.open_rows.contains(name);
        let (r, v, resp) = Self::cells(p, name);
        self.props.row_rects.insert(name.to_string(), r);
        if self.props.active_row.as_deref() == Some(name) {
            // Studio's selected row: the name cell in the selection blue.
            let div = r.min.x + (r.width() * DIVIDER).round();
            p.ui.painter().rect_filled(Rect::from_min_max(r.min, Pos2::new(div, r.max.y - 1.0)), 0.0, Color32::from_rgb(19, 35, 93));
        }
        let custom_set = name == "CustomPhysicalProperties" && matches!(value, Some(Variant::Bool(true)));
        let arrow = if audio_content_row {
            Some(!audio_folded)
        } else {
            vec_like
                .then_some(open)
                .or((matches!(value, Some(Variant::Faces(_))) || matches!(name, "ResizeableFaces" | "CurrentPhysicalProperties") || custom_set).then_some(false))
        };
        Self::name_text_styled(p, r, 0.0, name, color, arrow, bold);
        self.props.drawn.push(name.to_string());
        let on_name = resp.interact_pointer_pos().is_some_and(|q| q.x < v.min.x);
        if resp.clicked() && on_name {
            if audio_content_row {
                if !self.props.open_rows.remove("fold:AudioContent") {
                    self.props.open_rows.insert("fold:AudioContent".to_string());
                }
            } else if vec_like {
                if !self.props.open_rows.remove(name) {
                    self.props.open_rows.insert(name.to_string());
                }
            }
        }
        let clicked_value = resp.clicked() && !on_name;
        let Some(value) = value else {
            return;
        };
        match &value {
            Variant::Bool(b) => {
                let br = Rect::from_min_size(Pos2::new(v.min.x + 2.0, v.center().y - 7.0), Vec2::splat(14.0));
                let painter = p.ui.painter();
                if mixed {
                    // Studio tri-state indeterminate checkbox when multi-selected values differ.
                    painter.rect_filled(br, 2.0, if ro { READ_ONLY } else { Color32::from_rgb(130, 134, 148) });
                    let dash = Rect::from_center_size(br.center(), Vec2::new(8.0, 2.0));
                    painter.rect_filled(dash, 0.5, Color32::from_rgb(25, 26, 31));
                } else if *b {
                    painter.rect_filled(br, 2.0, if ro { READ_ONLY } else { Color32::from_rgb(230, 231, 233) });
                    rustudio_engine::ti::paint(painter, br.shrink(1.0), "check", Color32::from_rgb(25, 26, 31));
                } else {
                    painter.rect_filled(br, 2.0, BOX_OFF);
                }
                if clicked_value && !ro {
                    let next = if mixed { true } else { !*b };
                    edits.push(Edit::Prop(name.to_string(), Variant::Bool(next)));
                }
            }
            Variant::Color3(_) | Variant::Color3uint8(_) | Variant::BrickColor(_) => {
                let (rgb, label) = match &value {
                    Variant::Color3(c) => {
                        let rgb = [(c.r * 255.0).round() as u8, (c.g * 255.0).round() as u8, (c.b * 255.0).round() as u8];
                        (rgb, None)
                    }
                    Variant::Color3uint8(c) => ([c.r, c.g, c.b], None),
                    Variant::BrickColor(b) => (rustudio_render::scene::brick_rgb(*b), Some(b.to_string())),
                    _ => unreachable!(),
                };
                let sw = Rect::from_min_size(Pos2::new(v.min.x + 2.0, v.center().y - 6.0), Vec2::splat(12.0));
                p.ui.painter().rect_filled(sw, 1.0, if mixed { BOX_OFF } else { Color32::from_rgb(rgb[0], rgb[1], rgb[2]) });
                p.ui.painter().rect_stroke(sw, 1.0, Stroke::new(1.0, BAND), egui::StrokeKind::Inside);
                let text = if mixed {
                    String::new()
                } else {
                    match label {
                        Some(n) => n,
                        None => {
                            let base = format!("[{}, {}, {}]", rgb[0], rgb[1], rgb[2]);
                            // A part's Color names its BrickColor when it is one (captured).
                            match brick_name(rgb).filter(|_| name == "Color") {
                                Some(n) => format!("{base} ({n})"),
                                None => base,
                            }
                        }
                    }
                };
                let tv = Rect::from_min_max(Pos2::new(sw.max.x + 6.0, v.min.y), v.max);
                self.ui_rects.insert(if matches!(value, Variant::BrickColor(_)) { "props.brickcolor" } else { "props.color" }, v);
                if ro {
                    Self::value_label(p, tv, &text, color);
                } else if matches!(value, Variant::BrickColor(_)) {
                    // Studio's palette popup (crate::color_editors).
                    Self::value_label(p, tv, &text, color);
                    if clicked_value {
                        self.props.brick_popup = Some(crate::color_editors::BrickPopup {
                            prop: name.to_string(),
                            origin: v.left_bottom() + Vec2::new(0.0, 1.0),
                            opened_frame: self.game.frame_no,
                            hovered: None,
                        });
                    }
                } else {
                    // The swatch opens Studio's Select Color dialog; the text
                    // edits "r, g, b" (as Studio's row editor does).
                    let on_swatch = resp.interact_pointer_pos().is_some_and(|q| q.x < tv.min.x);
                    if clicked_value && on_swatch {
                        self.open_color_dialog(name, rgb);
                    }
                    let plain = if mixed { String::new() } else { format!("{}, {}, {}", rgb[0], rgb[1], rgb[2]) };
                    let editing = self.props.editing.as_ref().is_some_and(|(k, _)| k == name);
                    if !editing {
                        Self::value_label(p, tv, &text, color);
                    }
                    if let Some(t) = self.text_cell(p, name, if editing { tv } else { Rect::NOTHING }, &plain, false, clicked_value && !on_swatch) {
                        if let Some(n) = parse_nums(&t, 3) {
                            let c = [n[0].clamp(0.0, 255.0) as u8, n[1].clamp(0.0, 255.0) as u8, n[2].clamp(0.0, 255.0) as u8];
                            edits.push(Edit::Prop(name.to_string(), color_variant(&value, c)));
                        }
                    }
                }
            }
            Variant::ColorSequence(seq) => {
                // Studio ColorSequencePropertyItem: swatch/gradient + "[r, g, b]" or "<ColorSequence>".
                let sw = Rect::from_min_size(Pos2::new(v.min.x + 2.0, v.center().y - 6.0), Vec2::new(24.0, 12.0));
                if mixed || seq.keypoints.is_empty() {
                    p.ui.painter().rect_filled(sw, 1.0, BOX_OFF);
                } else if seq.keypoints.len() == 1 {
                    let c = seq.keypoints[0].color;
                    let c32 = Color32::from_rgb((c.r * 255.0).round() as u8, (c.g * 255.0).round() as u8, (c.b * 255.0).round() as u8);
                    p.ui.painter().rect_filled(sw, 1.0, c32);
                } else {
                    // Paint gradient preview strip across the swatch.
                    let mut mesh = egui::Mesh::default();
                    for w in seq.keypoints.windows(2) {
                        let (a, b) = (&w[0], &w[1]);
                        let x0 = sw.min.x + a.time.clamp(0.0, 1.0) * sw.width();
                        let x1 = sw.min.x + b.time.clamp(0.0, 1.0) * sw.width();
                        let ca = Color32::from_rgb((a.color.r * 255.0).round() as u8, (a.color.g * 255.0).round() as u8, (a.color.b * 255.0).round() as u8);
                        let cb = Color32::from_rgb((b.color.r * 255.0).round() as u8, (b.color.g * 255.0).round() as u8, (b.color.b * 255.0).round() as u8);
                        let base = mesh.vertices.len() as u32;
                        mesh.vertices.push(egui::epaint::Vertex { pos: Pos2::new(x0, sw.min.y), uv: egui::epaint::WHITE_UV, color: ca });
                        mesh.vertices.push(egui::epaint::Vertex { pos: Pos2::new(x1, sw.min.y), uv: egui::epaint::WHITE_UV, color: cb });
                        mesh.vertices.push(egui::epaint::Vertex { pos: Pos2::new(x1, sw.max.y), uv: egui::epaint::WHITE_UV, color: cb });
                        mesh.vertices.push(egui::epaint::Vertex { pos: Pos2::new(x0, sw.max.y), uv: egui::epaint::WHITE_UV, color: ca });
                        mesh.add_triangle(base, base + 1, base + 2);
                        mesh.add_triangle(base, base + 2, base + 3);
                    }
                    p.ui.painter().add(egui::Shape::mesh(mesh));
                }
                p.ui.painter().rect_stroke(sw, 1.0, Stroke::new(1.0, BAND), egui::StrokeKind::Inside);
                let tv = Rect::from_min_max(Pos2::new(sw.max.x + 6.0, v.min.y), v.max);
                let text = if mixed { String::new() } else { value_text(&self.game.doc, &value) };
                if ro {
                    Self::value_label(p, tv, &text, color);
                } else {
                    let rgb = color_seq_constant(seq).unwrap_or_else(|| {
                        seq.keypoints.first().map(|k| [
                            (k.color.r * 255.0).round() as u8,
                            (k.color.g * 255.0).round() as u8,
                            (k.color.b * 255.0).round() as u8,
                        ]).unwrap_or([255, 255, 255])
                    });
                    let on_swatch = resp.interact_pointer_pos().is_some_and(|q| q.x < tv.min.x);
                    if clicked_value && on_swatch {
                        self.open_color_dialog(name, rgb);
                    }
                    let plain = if mixed {
                        String::new()
                    } else if let Some(c) = color_seq_constant(seq) {
                        format!("{}, {}, {}", c[0], c[1], c[2])
                    } else {
                        text.clone()
                    };
                    let editing = self.props.editing.as_ref().is_some_and(|(k, _)| k == name);
                    if !editing {
                        Self::value_label(p, tv, &text, color);
                    }
                    if let Some(t) = self.text_cell(p, name, if editing { tv } else { Rect::NOTHING }, &plain, false, clicked_value && !on_swatch) {
                        if let Some(nv) = parse_like(&value, &t) {
                            edits.push(Edit::Prop(name.to_string(), nv));
                        }
                    }
                }
            }
            Variant::Enum(_) | Variant::EnumItem(_) => {
                let (ty, cur) = match &value {
                    Variant::Enum(e) => (enum_type_of(class, name), e.to_u32()),
                    Variant::EnumItem(e) => (Some(e.ty.clone()), e.value),
                    _ => unreachable!(),
                };
                let items: Vec<(String, u32)> = ty
                    .as_deref()
                    .and_then(|t| doc::db().enums.get(t))
                    .map(|e| {
                        let mut v: Vec<(String, u32)> = e.items.iter().map(|(k, v)| (k.to_string(), *v)).collect();
                        v.sort_by_key(|(_, n)| *n);
                        v
                    })
                    .unwrap_or_default();
                let cur_name = if mixed {
                    String::new()
                } else {
                    items.iter().find(|(_, n)| *n == cur).map(|(k, _)| k.clone()).unwrap_or_else(|| cur.to_string())
                };
                Self::value_label(p, v, &cur_name, color);
                if !ro {
                    let id = egui::Id::new(("enum_popup", name));
                    if clicked_value {
                        egui::Popup::open_id(p.ui.ctx(), id);
                    }
                    let mut picked = None;
                    let just_opened = clicked_value;
                    // Studio's enum list: as wide as the value cell, compact
                    // rows, scrollbar on the right edge, ~12 rows tall.
                    const ITEM_H: f32 = 19.0;
                    let width = v.width().max(120.0);
                    egui::Popup::new(id, p.ui.ctx().clone(), egui::PopupAnchor::Position(v.left_bottom()), p.ui.layer_id())
                        .kind(egui::PopupKind::Menu)
                        .open_memory(None)
                        .close_behavior(egui::PopupCloseBehavior::CloseOnClick)
                        .width(width)
                        .frame(egui::Frame::popup(p.ui.style()).inner_margin(egui::Margin::same(2)).corner_radius(0))
                        .show(|ui| {
                            ui.set_width(width - 4.0);
                            ui.spacing_mut().item_spacing = egui::vec2(0.0, 0.0);
                            egui::ScrollArea::vertical()
                                .max_height(ITEM_H * 12.0)
                                .auto_shrink([false, true])
                                .show(ui, |ui| {
                                    for (k, n) in &items {
                                        let (r, resp) = ui.allocate_exact_size(egui::vec2(ui.available_width(), ITEM_H), Sense::click());
                                        let is_sel = !mixed && *n == cur;
                                        if is_sel || resp.hovered() {
                                            let bg = if is_sel { Color32::from_rgb(19, 35, 93) } else { Color32::from_rgb(45, 48, 58) };
                                            ui.painter().rect_filled(r, 0.0, bg);
                                        }
                                        ui.painter().text(Pos2::new(r.min.x + 6.0, r.center().y), egui::Align2::LEFT_CENTER, k, FontId::proportional(FONT), TEXT);
                                        if is_sel && just_opened {
                                            resp.scroll_to_me(Some(egui::Align::Center));
                                        }
                                        if resp.clicked() {
                                            picked = Some(*n);
                                        }
                                    }
                                });
                        });
                    if let Some(n) = picked.filter(|n| mixed || *n != cur) {
                        edits.push(Edit::Prop(name.to_string(), Variant::Enum(Enum::from_u32(n))));
                    }
                }
            }
            Variant::Vector3(v3) => {
                let g = rustudio_core::math::v3(*v3);
                let shown = if mixed { String::new() } else { fmt_v3(g) };
                if let Some(t) = self.text_cell(p, name, v, &shown, ro, clicked_value) {
                    if let Some(n) = parse_v3(&t) {
                        edits.push(Edit::Prop(name.to_string(), Variant::Vector3(rustudio_core::math::rv3(n))));
                    }
                }
                if open {
                    for (i, axis) in ["X", "Y", "Z"].iter().enumerate() {
                        let key = format!("{name}.{axis}");
                        let (r, vv, resp) = Self::cells(p, &key);
                        Self::name_text(p, r, 1.0, axis, color, None);
                        let comp_mixed = sel.len() > 1 && all_vals.iter().skip(1).any(|o| match o {
                            Some(Variant::Vector3(other)) => ([other.x, other.y, other.z][i] - g[i]).abs() > 1e-5,
                            _ => true,
                        });
                        let comp_shown = if comp_mixed { String::new() } else { fmt_num(g[i] as f64) };
                        let on_sub_val = resp.interact_pointer_pos().is_some_and(|q| q.x >= vv.min.x);
                        if let Some(t) = self.text_cell(p, &key, vv, &comp_shown, ro, resp.clicked() && on_sub_val && !ro) {
                            if let Ok(x) = t.trim().parse::<f32>() {
                                edits.push(Edit::SubProp(name.to_string(), SubEdit::Vec3(i, x)));
                            }
                        }
                    }
                }
            }
            // Studio Vector2PropertyItem: "x, y", opening to X and Y.
            Variant::Vector2(v2) => {
                let comps = [v2.x, v2.y];
                let shown = if mixed { String::new() } else { value_text(&self.game.doc, &value) };
                if let Some(t) = self.text_cell(p, name, v, &shown, ro, clicked_value && !ro) {
                    if let Some(nv) = parse_like(&value, &t) {
                        edits.push(Edit::Prop(name.to_string(), nv));
                    }
                }
                if open {
                    for (i, axis) in ["X", "Y"].iter().enumerate() {
                        let key = format!("{name}.{axis}");
                        let (r, vv, resp) = Self::cells(p, &key);
                        Self::name_text(p, r, 1.0, axis, color, None);
                        let comp_mixed = sel.len() > 1 && all_vals.iter().skip(1).any(|o| match o {
                            Some(Variant::Vector2(other)) => ([other.x, other.y][i] - comps[i]).abs() > 1e-5,
                            _ => true,
                        });
                        let comp_shown = if comp_mixed { String::new() } else { fmt_num(comps[i] as f64) };
                        let on_sub_val = resp.interact_pointer_pos().is_some_and(|q| q.x >= vv.min.x);
                        if let Some(t) = self.text_cell(p, &key, vv, &comp_shown, ro, resp.clicked() && on_sub_val && !ro) {
                            if let Ok(x) = t.trim().parse::<f32>() {
                                edits.push(Edit::SubProp(name.to_string(), SubEdit::Vec2(i, x)));
                            }
                        }
                    }
                }
            }
            // Studio's UDim2 row: "{xs, xo},{ys, yo}", opening to X and Y,
            // each opening to Scale and Offset.
            Variant::UDim2(u) => {
                let u = *u;
                let shown = if mixed { String::new() } else { value_text(&self.game.doc, &value) };
                if let Some(t) = self.text_cell(p, name, v, &shown, ro, clicked_value && !ro) {
                    if let Some(nv) = parse_like(&value, &t) {
                        edits.push(Edit::Prop(name.to_string(), nv));
                    }
                }
                if open {
                    for (is_y, axis, part) in [(false, "X", u.x), (true, "Y", u.y)] {
                        let key = format!("{name}.{axis}");
                        let axis_vals: Vec<UDim> = all_vals.iter().filter_map(|o| match o {
                            Some(Variant::UDim2(u2)) => Some(if is_y { u2.y } else { u2.x }),
                            _ => None,
                        }).collect();
                        if let Some(sub) = self.udim_rows(p, &key, axis, 1.0, part, &axis_vals, ro, color) {
                            let se = match (is_y, sub) {
                                (false, UDimSubEdit::Whole(n)) => SubEdit::UDim2X(n),
                                (true, UDimSubEdit::Whole(n)) => SubEdit::UDim2Y(n),
                                (false, UDimSubEdit::Scale(s)) => SubEdit::UDim2XScale(s),
                                (true, UDimSubEdit::Scale(s)) => SubEdit::UDim2YScale(s),
                                (false, UDimSubEdit::Offset(o)) => SubEdit::UDim2XOffset(o),
                                (true, UDimSubEdit::Offset(o)) => SubEdit::UDim2YOffset(o),
                            };
                            edits.push(Edit::SubProp(name.to_string(), se));
                        }
                    }
                }
            }
            Variant::UDim(u) => {
                let u = *u;
                let shown = if mixed { String::new() } else { value_text(&self.game.doc, &value) };
                if let Some(t) = self.text_cell(p, name, v, &shown, ro, clicked_value && !ro) {
                    if let Some(nv) = parse_like(&value, &t) {
                        edits.push(Edit::Prop(name.to_string(), nv));
                    }
                }
                if open {
                    let u_vals: Vec<UDim> = all_vals.iter().filter_map(|o| match o {
                        Some(Variant::UDim(u1)) => Some(*u1),
                        _ => None,
                    }).collect();
                    if let Some(sub) = self.udim_parts(p, name, 1.0, u, &u_vals, ro, color) {
                        match sub {
                            UDimSubEdit::Scale(s) => edits.push(Edit::SubProp(name.to_string(), SubEdit::UDimScale(s))),
                            UDimSubEdit::Offset(o) => edits.push(Edit::SubProp(name.to_string(), SubEdit::UDimOffset(o))),
                            UDimSubEdit::Whole(n) => edits.push(Edit::Prop(name.to_string(), Variant::UDim(n))),
                        }
                    }
                }
            }
            // Studio RectPropertyItem: "x0, y0, x1, y1", opening to X0, Y0, X1, Y1.
            Variant::Rect(rc) => {
                let comps = [rc.min.x, rc.min.y, rc.max.x, rc.max.y];
                let shown = if mixed { String::new() } else { value_text(&self.game.doc, &value) };
                let has_9slice_btn = name == "SliceCenter" && !ro && v.width() > 36.0;
                let (text_v, btn_clicked) = if has_9slice_btn {
                    let btn_r = Rect::from_min_max(
                        Pos2::new(v.max.x - 20.0, v.min.y + 1.0),
                        Pos2::new(v.max.x - 1.0, v.max.y - 1.0),
                    );
                    let resp = p.ui.interact(btn_r, egui::Id::new(("slice_editor_btn", name)), Sense::click());
                    let bg = if resp.hovered() {
                        Color32::from_rgb(66, 70, 82)
                    } else {
                        Color32::from_rgb(48, 51, 60)
                    };
                    p.ui.painter().rect_filled(btn_r, 2.0, bg);
                    p.ui.painter().text(
                        btn_r.center(),
                        egui::Align2::CENTER_CENTER,
                        "…",
                        FontId::proportional(FONT),
                        TEXT,
                    );
                    let clicked = resp.on_hover_text("Open 9-Slice Editor").clicked();
                    (Rect::from_min_max(v.min, Pos2::new(btn_r.min.x - 2.0, v.max.y)), clicked)
                } else {
                    (v, false)
                };
                if btn_clicked {
                    if self.action_state("nine_slice_editor").1 != Some(true) {
                        self.trigger_capability("nine_slice_editor");
                    }
                }
                if let Some(t) = self.text_cell(p, name, text_v, &shown, ro, clicked_value && !btn_clicked && !ro) {
                    if let Some(nv) = parse_like(&value, &t) {
                        edits.push(Edit::Prop(name.to_string(), nv));
                    }
                }
                if open {
                    for (i, label) in ["X0", "Y0", "X1", "Y1"].iter().enumerate() {
                        let key = format!("{name}.{label}");
                        let (r, vv, resp) = Self::cells(p, &key);
                        Self::name_text(p, r, 1.0, label, color, None);
                        let comp_mixed = sel.len() > 1 && all_vals.iter().skip(1).any(|o| match o {
                            Some(Variant::Rect(other)) => ([other.min.x, other.min.y, other.max.x, other.max.y][i] - comps[i]).abs() > 1e-5,
                            _ => true,
                        });
                        let comp_shown = if comp_mixed { String::new() } else { fmt_num(comps[i] as f64) };
                        let on_sub_val = resp.interact_pointer_pos().is_some_and(|q| q.x >= vv.min.x);
                        if let Some(t) = self.text_cell(p, &key, vv, &comp_shown, ro, resp.clicked() && on_sub_val && !ro) {
                            if let Ok(x) = t.trim().parse::<f32>() {
                                edits.push(Edit::SubProp(name.to_string(), SubEdit::RectComp(i, x)));
                            }
                        }
                    }
                }
            }
            // Studio NumberRangePropertyItem: "min, max", opening to Min and Max.
            Variant::NumberRange(nr) => {
                let nr = *nr;
                let shown = if mixed { String::new() } else { value_text(&self.game.doc, &value) };
                if let Some(t) = self.text_cell(p, name, v, &shown, ro, clicked_value && !ro) {
                    if let Some(nv) = parse_like(&value, &t) {
                        edits.push(Edit::Prop(name.to_string(), nv));
                    }
                }
                if open {
                    for (is_max, label, cur_v) in [(false, "Min", nr.min), (true, "Max", nr.max)] {
                        let key = format!("{name}.{label}");
                        let (r, vv, resp) = Self::cells(p, &key);
                        Self::name_text(p, r, 1.0, label, color, None);
                        let comp_mixed = sel.len() > 1 && all_vals.iter().skip(1).any(|o| match o {
                            Some(Variant::NumberRange(other)) => ((if is_max { other.max } else { other.min }) - cur_v).abs() > 1e-5,
                            _ => true,
                        });
                        let comp_shown = if comp_mixed { String::new() } else { fmt_num(cur_v as f64) };
                        let on_sub_val = resp.interact_pointer_pos().is_some_and(|q| q.x >= vv.min.x);
                        if let Some(t) = self.text_cell(p, &key, vv, &comp_shown, ro, resp.clicked() && on_sub_val && !ro) {
                            if let Ok(x) = t.trim().parse::<f32>() {
                                let se = if is_max { SubEdit::RangeMax(x) } else { SubEdit::RangeMin(x) };
                                edits.push(Edit::SubProp(name.to_string(), se));
                            }
                        }
                    }
                }
            }
            // Studio FontPropertyItem: family picker, opening to Weight and Style.
            Variant::Font(f) => {
                let f = f.clone();
                let fam_mixed = sel.len() > 1 && all_vals.iter().skip(1).any(|o| match o {
                    Some(Variant::Font(other)) => other.family != f.family,
                    _ => true,
                });
                let cur_fam = font_display_name(&f.family);
                Self::value_label(p, v, if fam_mixed { "" } else { &cur_fam }, color);
                if !ro {
                    let id = egui::Id::new(("font_family_popup", name));
                    if clicked_value {
                        egui::Popup::open_id(p.ui.ctx(), id);
                    }
                    let families = studio_font_families();
                    let mut picked: Option<String> = None;
                    const ITEM_H: f32 = 19.0;
                    let width = v.width().max(150.0);
                    egui::Popup::new(id, p.ui.ctx().clone(), egui::PopupAnchor::Position(v.left_bottom()), p.ui.layer_id())
                        .kind(egui::PopupKind::Menu)
                        .open_memory(None)
                        .close_behavior(egui::PopupCloseBehavior::CloseOnClick)
                        .width(width)
                        .frame(egui::Frame::popup(p.ui.style()).inner_margin(egui::Margin::same(2)).corner_radius(0))
                        .show(|ui| {
                            ui.set_width(width - 4.0);
                            ui.spacing_mut().item_spacing = egui::vec2(0.0, 0.0);
                            egui::ScrollArea::vertical()
                                .max_height(ITEM_H * 12.0)
                                .auto_shrink([false, true])
                                .show(ui, |ui| {
                                    for fam in families {
                                        let (r, resp) = ui.allocate_exact_size(egui::vec2(ui.available_width(), ITEM_H), Sense::click());
                                        let is_sel = !fam_mixed && *fam == cur_fam;
                                        if is_sel || resp.hovered() {
                                            let bg = if is_sel { Color32::from_rgb(19, 35, 93) } else { Color32::from_rgb(45, 48, 58) };
                                            ui.painter().rect_filled(r, 0.0, bg);
                                        }
                                        ui.painter().text(Pos2::new(r.min.x + 6.0, r.center().y), egui::Align2::LEFT_CENTER, fam, FontId::proportional(FONT), TEXT);
                                        if is_sel && clicked_value {
                                            resp.scroll_to_me(Some(egui::Align::Center));
                                        }
                                        if resp.clicked() {
                                            picked = Some(fam.clone());
                                        }
                                    }
                                });
                        });
                    if let Some(fam) = picked {
                        let uri = format!("rbxasset://fonts/families/{fam}.json");
                        if fam_mixed || uri != f.family {
                            edits.push(Edit::SubProp(name.to_string(), SubEdit::FontFamily(uri)));
                        }
                    }
                }
                if open {
                    // Weight sub-row
                    let w_key = format!("{name}.Weight");
                    let (wr, wv, wresp) = Self::cells(p, &w_key);
                    Self::name_text(p, wr, 1.0, "Weight", color, None);
                    let w_mixed = sel.len() > 1 && all_vals.iter().skip(1).any(|o| match o {
                        Some(Variant::Font(other)) => other.weight != f.weight,
                        _ => true,
                    });
                    const WEIGHTS: &[(&str, FontWeight)] = &[
                        ("Thin", FontWeight::Thin),
                        ("ExtraLight", FontWeight::ExtraLight),
                        ("Light", FontWeight::Light),
                        ("Regular", FontWeight::Regular),
                        ("Medium", FontWeight::Medium),
                        ("SemiBold", FontWeight::SemiBold),
                        ("Bold", FontWeight::Bold),
                        ("ExtraBold", FontWeight::ExtraBold),
                        ("Heavy", FontWeight::Heavy),
                    ];
                    let w_label = if w_mixed { "" } else { WEIGHTS.iter().find(|(_, w)| *w == f.weight).map(|(l, _)| *l).unwrap_or("Regular") };
                    Self::value_label(p, wv, w_label, color);
                    let on_w_val = wresp.interact_pointer_pos().is_some_and(|q| q.x >= wv.min.x);
                    if !ro {
                        let wid = egui::Id::new(("font_weight_popup", name));
                        if wresp.clicked() && on_w_val {
                            egui::Popup::open_id(p.ui.ctx(), wid);
                        }
                        let mut picked_w = None;
                        const ITEM_H: f32 = 19.0;
                        let width = wv.width().max(110.0);
                        egui::Popup::new(wid, p.ui.ctx().clone(), egui::PopupAnchor::Position(wv.left_bottom()), p.ui.layer_id())
                            .kind(egui::PopupKind::Menu)
                            .open_memory(None)
                            .close_behavior(egui::PopupCloseBehavior::CloseOnClick)
                            .width(width)
                            .frame(egui::Frame::popup(p.ui.style()).inner_margin(egui::Margin::same(2)).corner_radius(0))
                            .show(|ui| {
                                ui.set_width(width - 4.0);
                                ui.spacing_mut().item_spacing = egui::vec2(0.0, 0.0);
                                for (lbl, w) in WEIGHTS {
                                    let (r, resp) = ui.allocate_exact_size(egui::vec2(ui.available_width(), ITEM_H), Sense::click());
                                    let is_sel = !w_mixed && *w == f.weight;
                                    if is_sel || resp.hovered() {
                                        let bg = if is_sel { Color32::from_rgb(19, 35, 93) } else { Color32::from_rgb(45, 48, 58) };
                                        ui.painter().rect_filled(r, 0.0, bg);
                                    }
                                    ui.painter().text(Pos2::new(r.min.x + 6.0, r.center().y), egui::Align2::LEFT_CENTER, *lbl, FontId::proportional(FONT), TEXT);
                                    if resp.clicked() {
                                        picked_w = Some(*w);
                                    }
                                }
                            });
                        if let Some(w) = picked_w.filter(|w| w_mixed || *w != f.weight) {
                            edits.push(Edit::SubProp(name.to_string(), SubEdit::FontWeight(w)));
                        }
                    }

                    // Style sub-row
                    let s_key = format!("{name}.Style");
                    let (sr, sv, sresp) = Self::cells(p, &s_key);
                    Self::name_text(p, sr, 1.0, "Style", color, None);
                    let s_mixed = sel.len() > 1 && all_vals.iter().skip(1).any(|o| match o {
                        Some(Variant::Font(other)) => other.style != f.style,
                        _ => true,
                    });
                    const STYLES: &[(&str, FontStyle)] = &[("Normal", FontStyle::Normal), ("Italic", FontStyle::Italic)];
                    let s_label = if s_mixed { "" } else { STYLES.iter().find(|(_, s)| *s == f.style).map(|(l, _)| *l).unwrap_or("Normal") };
                    Self::value_label(p, sv, s_label, color);
                    let on_s_val = sresp.interact_pointer_pos().is_some_and(|q| q.x >= sv.min.x);
                    if !ro {
                        let sid = egui::Id::new(("font_style_popup", name));
                        if sresp.clicked() && on_s_val {
                            egui::Popup::open_id(p.ui.ctx(), sid);
                        }
                        let mut picked_s = None;
                        const ITEM_H: f32 = 19.0;
                        let width = sv.width().max(100.0);
                        egui::Popup::new(sid, p.ui.ctx().clone(), egui::PopupAnchor::Position(sv.left_bottom()), p.ui.layer_id())
                            .kind(egui::PopupKind::Menu)
                            .open_memory(None)
                            .close_behavior(egui::PopupCloseBehavior::CloseOnClick)
                            .width(width)
                            .frame(egui::Frame::popup(p.ui.style()).inner_margin(egui::Margin::same(2)).corner_radius(0))
                            .show(|ui| {
                                ui.set_width(width - 4.0);
                                ui.spacing_mut().item_spacing = egui::vec2(0.0, 0.0);
                                for (lbl, st) in STYLES {
                                    let (r, resp) = ui.allocate_exact_size(egui::vec2(ui.available_width(), ITEM_H), Sense::click());
                                    let is_sel = !s_mixed && *st == f.style;
                                    if is_sel || resp.hovered() {
                                        let bg = if is_sel { Color32::from_rgb(19, 35, 93) } else { Color32::from_rgb(45, 48, 58) };
                                        ui.painter().rect_filled(r, 0.0, bg);
                                    }
                                    ui.painter().text(Pos2::new(r.min.x + 6.0, r.center().y), egui::Align2::LEFT_CENTER, *lbl, FontId::proportional(FONT), TEXT);
                                    if resp.clicked() {
                                        picked_s = Some(*st);
                                    }
                                }
                            });
                        if let Some(st) = picked_s.filter(|st| s_mixed || *st != f.style) {
                            edits.push(Edit::SubProp(name.to_string(), SubEdit::FontStyle(st)));
                        }
                    }
                }
            }
            Variant::Tags(_) | Variant::Attributes(_) => {}
            Variant::Ref(cur) => {
                let picking_this = self.props.ref_pick.as_ref().is_some_and(|pk| pk.prop == name);
                if picking_this {
                    p.ui.painter().rect_filled(v, 2.0, Color32::from_rgb(26, 56, 118));
                    p.ui.painter().rect_stroke(v, 2.0, egui::Stroke::new(1.0, Color32::from_rgb(90, 155, 255)), egui::StrokeKind::Inside);
                }
                let can_clear = !ro && name != "Parent" && (mixed || cur.is_some());
                let (label_v, clear_clicked) = if can_clear && v.width() > 36.0 {
                    let btn_r = Rect::from_min_max(
                        Pos2::new(v.max.x - 18.0, v.min.y + 1.0),
                        Pos2::new(v.max.x - 1.0, v.max.y - 1.0),
                    );
                    let cresp = p.ui.interact(btn_r, egui::Id::new(("ref_clear", name)), Sense::click());
                    let fg = if cresp.hovered() { Color32::WHITE } else { READ_ONLY };
                    p.ui.painter().text(
                        btn_r.center(),
                        egui::Align2::CENTER_CENTER,
                        "×",
                        FontId::proportional(FONT),
                        fg,
                    );
                    let clicked = cresp.on_hover_text("Clear reference (set to nil)").clicked();
                    (Rect::from_min_max(v.min, Pos2::new(btn_r.min.x - 2.0, v.max.y)), clicked)
                } else {
                    (v, false)
                };
                if clear_clicked {
                    if picking_this {
                        self.props.ref_pick = None;
                    }
                    edits.push(Edit::Prop(name.to_string(), Variant::Ref(Ref::none())));
                } else {
                    let expected = expected_ref_class(class, name);
                    let label = if picking_this {
                        format!("Pick {expected}…")
                    } else if mixed {
                        String::new()
                    } else {
                        value_text(&self.game.doc, &value)
                    };
                    let text_col = if picking_this {
                        Color32::from_rgb(180, 215, 255)
                    } else {
                        color
                    };
                    Self::value_label(p, label_v, &label, text_col);
                    if !ro && clicked_value {
                        if picking_this {
                            self.props.ref_pick = None;
                        } else {
                            self.props.ref_pick = Some(RefPick {
                                prop: name.to_string(),
                                targets: sel.to_vec(),
                                expected_class: expected,
                            });
                        }
                    }
                    if picking_this {
                        if p.ui.input(|i| i.key_pressed(egui::Key::Escape)) {
                            self.props.ref_pick = None;
                        } else if name != "Parent"
                            && p.ui.input(|i| i.key_pressed(egui::Key::Backspace) || i.key_pressed(egui::Key::Delete))
                        {
                            self.props.ref_pick = None;
                            edits.push(Edit::Prop(name.to_string(), Variant::Ref(Ref::none())));
                        }
                    }
                }
            }
            other if !ro && number_of(other).is_some() && crate::prop_slider::spec_for(class, name).is_some() => {
                let spec = crate::prop_slider::spec_for(class, name).unwrap_or(crate::prop_slider::SliderSpec { min: 0.0, max: 1.0, ticks: 20.0, square: false });
                self.slider_cell(p, name, other, mixed, spec, v, &resp, clicked_value, edits);
            }
            other => {
                // For String properties, preserve full text (including newlines) when editing.
                let display = if mixed { String::new() } else { value_text(&self.game.doc, other) };
                let edit_initial = if mixed {
                    String::new()
                } else if let Variant::String(s) = other {
                    s.clone()
                } else {
                    display.clone()
                };
                let editable = !ro
                    && matches!(
                        other,
                        Variant::String(_)
                            | Variant::Int32(_)
                            | Variant::Int64(_)
                            | Variant::Float32(_)
                            | Variant::Float64(_)
                            | Variant::Vector2int16(_)
                            | Variant::Vector3int16(_)
                            | Variant::ContentId(_)
                            | Variant::Content(_)
                            | Variant::NumberSequence(_)
                    );
                let editing = self.props.editing.as_ref().is_some_and(|(k, _)| k == name);
                if !editing && matches!(other, Variant::String(_)) {
                    Self::value_label(p, v, &display, if ro { READ_ONLY } else { TEXT });
                    if let Some(t) = self.text_cell(p, name, Rect::NOTHING, &edit_initial, ro, clicked_value && editable) {
                        if let Some(nv) = parse_like(other, &t) {
                            edits.push(Edit::Prop(name.to_string(), nv));
                        }
                    }
                } else if let Some(t) = self.text_cell(p, name, v, &edit_initial, ro, clicked_value && editable) {
                    if let Some(nv) = parse_like(other, &t) {
                        edits.push(Edit::Prop(name.to_string(), nv));
                    }
                }
            }
        }
        if audio_content_row && !audio_folded && self.game.doc.selection.len() == 1 {
            self.audio_preview_row(p, first);
        }
    }

    /// A UDim sub-row (`X` / `Y` of a UDim2) that opens to Scale and Offset.
    #[allow(clippy::too_many_arguments)]
    fn udim_rows(&mut self, p: &mut Painter, key: &str, label: &str, depth: f32, u: UDim, all: &[UDim], ro: bool, color: Color32) -> Option<UDimSubEdit> {
        let open = self.props.open_rows.contains(key);
        let (r, vv, resp) = Self::cells(p, key);
        Self::name_text(p, r, depth, label, color, Some(open));
        let on_name = resp.interact_pointer_pos().is_some_and(|q| q.x < vv.min.x);
        if resp.clicked() && on_name && !self.props.open_rows.remove(key) {
            self.props.open_rows.insert(key.to_string());
        }
        let u_mixed = all.len() > 1 && all.iter().skip(1).any(|o| (o.scale - u.scale).abs() > 1e-5 || o.offset != u.offset);
        let shown = if u_mixed { String::new() } else { value_text(&self.game.doc, &Variant::UDim(u)) };
        let mut out = None;
        if let Some(t) = self.text_cell(p, key, vv, &shown, ro, resp.clicked() && !on_name && !ro) {
            if let Some(Variant::UDim(n)) = parse_like(&Variant::UDim(u), &t) {
                out = Some(UDimSubEdit::Whole(n));
            }
        }
        if open {
            out = self.udim_parts(p, key, depth + 1.0, u, all, ro, color).or(out);
        }
        out
    }

    /// The `Scale` and `Offset` rows under a UDim.
    #[allow(clippy::too_many_arguments)]
    fn udim_parts(&mut self, p: &mut Painter, key: &str, depth: f32, u: UDim, all: &[UDim], ro: bool, color: Color32) -> Option<UDimSubEdit> {
        let mut out = None;
        for (i, label) in ["Scale", "Offset"].iter().enumerate() {
            let k = format!("{key}.{label}");
            let (r, vv, resp) = Self::cells(p, &k);
            Self::name_text(p, r, depth, label, color, None);
            let comp_mixed = all.len() > 1 && all.iter().skip(1).any(|o| {
                if i == 0 { (o.scale - u.scale).abs() > 1e-5 } else { o.offset != u.offset }
            });
            let shown = if comp_mixed {
                String::new()
            } else if i == 0 {
                fmt_num(u.scale as f64)
            } else {
                u.offset.to_string()
            };
            let on_val = resp.interact_pointer_pos().is_some_and(|q| q.x >= vv.min.x);
            if let Some(t) = self.text_cell(p, &k, vv, &shown, ro, resp.clicked() && on_val && !ro) {
                if let Ok(x) = t.trim().parse::<f64>() {
                    out = Some(if i == 0 { UDimSubEdit::Scale(x as f32) } else { UDimSubEdit::Offset(x.round() as i32) });
                }
            }
        }
        out
    }

    /// Studio 0.748's indented `Preview` child row under `Asset -> AudioContent`
    /// for `Sound` and `AudioPlayer`: green Play / yellow Pause toggle button +
    /// interactive horizontal scrubber track (`0 .. TimeLength`).
    fn audio_preview_row(&mut self, p: &mut Painter, r: Ref) {
        let (row_rect, v, _) = Self::cells(p, "AudioContent.Preview");
        self.props.row_rects.insert("Preview".to_string(), row_rect);
        Self::name_text(p, row_rect, 1.0, "Preview", TEXT, None);
        self.props.drawn.push("Preview".to_string());

        let edit = self.game.play.is_none();
        let (playing, pos, len) = self.game.edit_audio.preview_state(r);
        let btn = Rect::from_min_size(Pos2::new(v.min.x + 1.0, v.center().y - 9.0), Vec2::splat(18.0));
        let slider = Rect::from_min_max(Pos2::new(btn.max.x + 6.0, v.min.y + 1.0), Pos2::new(v.max.x, v.max.y - 1.0));
        self.ui_rects.insert("props.audio_preview_btn", btn);
        self.ui_rects.insert("props.audio_preview_slider", slider);

        let btn_resp = p.ui.interact(
            btn,
            p.ui.id().with(("audio_preview_btn", r)),
            if edit { Sense::click() } else { Sense::hover() },
        );
        let painter = p.ui.painter().clone();
        if edit && btn_resp.hovered() {
            painter.rect_filled(btn, 2.0, Color32::from_rgb(45, 48, 58));
        }
        if playing {
            // Studio 0.748 Pause icon: two yellow vertical bars with dark outline.
            let yellow = Color32::from_rgb(235, 200, 60);
            let edge = Stroke::new(1.0, Color32::from_rgb(32, 34, 39));
            let c = btn.center();
            let left_bar = Rect::from_min_size(Pos2::new(c.x - 4.5, c.y - 5.0), Vec2::new(3.5, 10.0));
            let right_bar = Rect::from_min_size(Pos2::new(c.x + 1.0, c.y - 5.0), Vec2::new(3.5, 10.0));
            for bar in [left_bar, right_bar] {
                painter.rect_filled(bar, 0.5, yellow);
                painter.rect_stroke(bar, 0.5, edge, egui::StrokeKind::Outside);
            }
        } else {
            // Studio 0.748 Play icon: green right-pointing triangle.
            let green = if edit && len > 0.0 {
                Color32::from_rgb(56, 198, 92)
            } else {
                Color32::from_rgb(65, 118, 78)
            };
            let c = btn.center();
            painter.add(egui::Shape::convex_polygon(
                vec![
                    Pos2::new(c.x - 4.0, c.y - 5.0),
                    Pos2::new(c.x + 5.0, c.y),
                    Pos2::new(c.x - 4.0, c.y + 5.0),
                ],
                green,
                Stroke::new(1.0, Color32::from_rgb(32, 34, 39)),
            ));
        }
        if edit && btn_resp.clicked() {
            self.game.edit_audio_preview_toggle(r);
        }

        if slider.width() > 16.0 {
            let frac = if len > 0.0 { (pos / len).clamp(0.0, 1.0) } else { 0.0 };
            crate::prop_slider::paint_slider(&painter, slider, frac);
            let sresp = p.ui.interact(
                slider,
                p.ui.id().with(("audio_preview_slider", r)),
                if edit && len > 0.0 { Sense::click_and_drag() } else { Sense::hover() },
            );
            if edit && len > 0.0 && (sresp.clicked() || sresp.dragged() || sresp.drag_started()) {
                if let Some(q) = sresp.interact_pointer_pos() {
                    let seek = crate::prop_slider::fraction_at(slider, q.x) * len;
                    self.game.edit_audio_preview_seek(r, seek);
                }
            }
        }
        if playing {
            p.ui.ctx().request_repaint();
        }
    }

    /// A ranged number (crate::prop_slider): the value as text, and, while
    /// the row is hovered or picked, Studio's spin box and slider. Dragging
    /// the slider updates the selection live and is one undo step.
    #[allow(clippy::too_many_arguments)]
    fn slider_cell(
        &mut self,
        p: &mut Painter,
        name: &str,
        value: &Variant,
        mixed: bool,
        spec: crate::prop_slider::SliderSpec,
        v: Rect,
        resp: &egui::Response,
        clicked_value: bool,
        edits: &mut Vec<Edit>,
    ) {
        use crate::prop_slider as ps;
        let cur = number_of(value).unwrap_or(0.0);
        let dragging = self.props.slider_drag.as_ref().is_some_and(|d| d.prop == name);
        if clicked_value {
            self.props.active_row = Some(name.to_string());
        }
        let editing = self.props.editing.as_ref().is_some_and(|(k, _)| k == name);
        // The pointer over the row (a press on the slider takes the row's
        // own hover away, so ask the pointer), a drag, typing, or picked.
        let over = p.ui.ctx().pointer_latest_pos().is_some_and(|q| resp.rect.contains(q));
        let held = p.ui.ctx().is_being_dragged(p.ui.id().with(("slider", name)));
        let shown = over || held || dragging || editing || self.props.active_row.as_deref() == Some(name);
        let text_val = if mixed { String::new() } else { fmt_num(cur) };
        if !shown {
            Self::value_label(p, v, &text_val, TEXT);
            return;
        }
        // Studio's editor fills the value cell from the divider.
        let cell = Rect::from_min_max(Pos2::new(v.min.x - 4.0, v.min.y + 1.0), Pos2::new(v.max.x + 4.0, v.max.y));
        let spin = Rect::from_min_size(cell.min, Vec2::new(ps::SPIN_W.min(cell.width() * 0.5), cell.height()));
        let slider = Rect::from_min_max(Pos2::new(spin.max.x + 2.0, cell.min.y), cell.max);
        self.ui_rects.insert("props.slider", slider);
        self.ui_rects.insert("props.spin", spin);
        let painter = p.ui.painter().clone();
        painter.rect_filled(cell, 0.0, ps::SPIN_BG);
        let arrows = Rect::from_min_max(Pos2::new(spin.max.x - 12.0, spin.min.y), spin.max);
        let up = Rect::from_min_max(arrows.min, Pos2::new(arrows.max.x, arrows.center().y));
        let down = Rect::from_min_max(Pos2::new(arrows.min.x, arrows.center().y), arrows.max);
        rustudio_engine::ti::paint(&painter, Rect::from_center_size(up.center() + Vec2::new(0.0, 1.5), Vec2::splat(9.0)), "chevron-up", TEXT);
        rustudio_engine::ti::paint(&painter, Rect::from_center_size(down.center() - Vec2::new(0.0, 1.5), Vec2::splat(9.0)), "chevron-down", TEXT);
        let text_rect = Rect::from_min_max(spin.min + Vec2::new(4.0, 0.0), Pos2::new(arrows.min.x - 2.0, spin.max.y));
        let on_text = resp.interact_pointer_pos().is_some_and(|q| text_rect.contains(q));
        if let Some(t) = self.text_cell(p, name, text_rect, &text_val, false, clicked_value && on_text) {
            if let Some(nv) = parse_like(value, &t) {
                edits.push(Edit::Prop(name.to_string(), nv));
            }
        }
        // Arrows: one step (the slider's tick) up or down.
        for (rect, dir, key) in [(up, 1.0, "up"), (down, -1.0, "down")] {
            if p.ui.interact(rect, p.ui.id().with(("spin", name, key)), Sense::click()).clicked() {
                edits.push(Edit::Prop(name.to_string(), number_like(value, spec.nudge(cur, dir))));
                self.props.active_row = Some(name.to_string());
            }
        }
        ps::paint_slider(&painter, slider, spec.fraction(cur));
        let sresp = p.ui.interact(slider, p.ui.id().with(("slider", name)), Sense::click_and_drag());
        if sresp.drag_started() {
            let originals = self.game
                .doc
                .selection
                .iter()
                .filter_map(|r| self.game.doc.prop(*r, name).map(|v| (*r, v)))
                .collect();
            self.props.slider_drag = Some(SliderDrag { prop: name.to_string(), originals });
            self.props.active_row = Some(name.to_string());
        }
        if let Some(q) = sresp.interact_pointer_pos().filter(|_| sresp.dragged() || sresp.drag_started()) {
            let nv = spec.value_at(ps::fraction_at(slider, q.x));
            if (nv - cur).abs() > 1e-9 {
                let refs: Vec<Ref> = self.props.slider_drag.as_ref().map(|d| d.originals.iter().map(|(r, _)| *r).collect()).unwrap_or_default();
                for r in refs {
                    self.game.doc.set_prop_live(r, ustr(name), number_like(value, nv));
                }
            }
        }
        if sresp.drag_stopped() {
            if let Some(d) = self.props.slider_drag.take() {
                let changes: Vec<_> = d
                    .originals
                    .into_iter()
                    .filter_map(|(r, old)| {
                        let new = self.game.doc.dom.get_by_ref(r)?.properties.get(&ustr(name)).cloned()?;
                        (new != old).then(|| (r, ustr(name), Some(old), new))
                    })
                    .collect();
                if !changes.is_empty() {
                    self.game.doc.record_props(&format!("Set {name}"), changes);
                }
            }
        } else if sresp.clicked() {
            if let Some(q) = sresp.interact_pointer_pos() {
                edits.push(Edit::Prop(name.to_string(), number_like(value, spec.value_at(ps::fraction_at(slider, q.x)))));
            }
        }
    }

    /// Opens Studio's Select Color dialog for `prop` on the selection.
    pub fn open_color_dialog(&mut self, prop: &str, rgb: [u8; 3]) {
        let originals = self.game.doc.selection.iter().filter_map(|r| self.game.doc.prop(*r, prop).map(|v| (*r, v))).collect();
        self.props.color_dialog = Some(crate::color_editors::ColorDialog::new(prop, originals, rgb, self.game.frame_no));
    }

    /// The BrickColor popup and the Select Color dialog, when open.
    fn color_editors_ui(&mut self, ctx: &egui::Context, edits: &mut Vec<Edit>) {
        use crate::color_editors::{brick_popup_ui, color_dialog_ui, BrickPick, DialogOut};
        if let Some(mut pop) = self.props.brick_popup.take() {
            let first = self.game.doc.selection.first().copied();
            let current = first.and_then(|r| match self.game.doc.prop(r, &pop.prop) {
                Some(Variant::BrickColor(b)) => Some(b),
                _ => match self.game.doc.prop(r, "Color") {
                    Some(Variant::Color3uint8(c)) => Some(nearest_brick([c.r, c.g, c.b])),
                    _ => None,
                },
            });
            self.ui_rects.insert("brick.popup", Rect::from_min_size(crate::color_editors::popup_origin(ctx, &pop), crate::color_editors::POPUP_SIZE));
            match brick_popup_ui(ctx, &mut pop, current, self.game.frame_no) {
                BrickPick::None => self.props.brick_popup = Some(pop),
                BrickPick::Close => {}
                BrickPick::Picked(b) => edits.push(Edit::Prop(pop.prop.clone(), Variant::BrickColor(b))),
                BrickPick::MoreColors => {
                    // Studio's More Colors opens the colour dialog on Color.
                    let rgb = current.map(rustudio_render::scene::brick_rgb).unwrap_or([163, 162, 165]);
                    let prop = if first.and_then(|r| self.game.doc.class_of(r)).is_some_and(|c| doc::is_a(&c, "BasePart")) { "Color" } else { pop.prop.as_str() };
                    let prop = prop.to_string();
                    self.open_color_dialog(&prop, rgb);
                }
            }
        }
        if let Some(mut d) = self.props.color_dialog.take() {
            self.ui_rects.insert("color.dialog", Rect::from_min_size(crate::color_editors::dialog_origin(ctx), crate::color_editors::DIALOG_SIZE));
            let mut custom = self.props.custom_colors;
            let out = color_dialog_ui(ctx, &mut d, &mut custom, self.game.frame_no);
            self.props.custom_colors = custom;
            match out {
                DialogOut::None => self.props.color_dialog = Some(d),
                DialogOut::Changed(c) => {
                    // Live, as Studio: the part changes while the dialog is open.
                    for (r, old) in &d.originals {
                        self.game.doc.set_prop_live(*r, ustr(&d.prop), color_variant(old, c));
                    }
                    self.props.color_dialog = Some(d);
                }
                DialogOut::Ok => {
                    // Every live change of the dialog becomes one undo step.
                    let changes: Vec<_> = d
                        .originals
                        .iter()
                        .map(|(r, old)| (*r, ustr(&d.prop), Some(old.clone()), color_variant(old, d.rgb)))
                        .collect();
                    for (r, n, _, v) in &changes {
                        self.game.doc.set_prop_live(*r, *n, v.clone());
                    }
                    self.game.doc.record_props(&format!("Set {}", d.prop), changes);
                }
                DialogOut::Cancel => {
                    for (r, old) in &d.originals {
                        self.game.doc.set_prop_live(*r, ustr(&d.prop), old.clone());
                    }
                }
            }
        }
    }

    /// A CFrame row with Position and Orientation under it (Studio's Transform / Pivot rows).
    fn frame_rows(&mut self, p: &mut Painter, first: Ref, label: &str, f: Frame, edits: &mut Vec<Edit>) {
        let xf = self.frame_value(first, f).unwrap_or(Xform { pos: Vec3::ZERO, rot: glam::Mat3::IDENTITY });
        let (r, _, resp) = Self::cells(p, label);
        let folded = self.props.open_rows.contains(&format!("fold:{label}"));
        Self::name_text(p, r, 0.0, label, TEXT, Some(!folded));
        self.props.drawn.push(label.to_string());
        if resp.clicked() {
            let k = format!("fold:{label}");
            if !self.props.open_rows.remove(&k) {
                self.props.open_rows.insert(k);
            }
        }
        if folded {
            return;
        }
        for (part, value) in [("Position", xf.pos), ("Orientation", xf.orientation_deg())] {
            let key = format!("{label}.{part}");
            let (r, v, resp) = Self::cells(p, &key);
            Self::name_text(p, r, 1.0, part, TEXT, Some(false));
            let on_value = resp.interact_pointer_pos().is_some_and(|q| q.x >= v.min.x);
            if let Some(t) = self.text_cell(p, &key, v, &fmt_v3(value), false, resp.clicked() && on_value) {
                if let Some(n) = parse_v3(&t) {
                    let (pos, ori) = if part == "Position" { (Some(n), None) } else { (None, Some(n)) };
                    edits.push(Edit::Frame(f, pos, ori));
                }
            }
        }
    }

    fn frame_value(&self, r: Ref, f: Frame) -> Option<Xform> {
        let offset = cf_of(self.game.doc.prop(r, "PivotOffset")).unwrap_or(Xform { pos: Vec3::ZERO, rot: glam::Mat3::IDENTITY });
        match f {
            Frame::PartCFrame => cf_of(self.game.doc.prop(r, "CFrame")),
            Frame::PartOrigin => cf_of(self.game.doc.prop(r, "CFrame")).map(|c| mul(&c, &offset)),
            Frame::PivotOffset => Some(offset),
            Frame::ModelOrigin | Frame::WorldPivot => cf_of(self.game.doc.prop(r, "WorldPivotData")).or_else(|| {
                let pp = match self.game.doc.prop(r, "PrimaryPart") {
                    Some(Variant::Ref(p)) if p.is_some() => p,
                    _ => self.selected_parts().first().copied()?,
                };
                cf_of(self.game.doc.prop(pp, "CFrame"))
            }),
            // An Attachment's CFrame defaults to identity, as Roblox's does.
            Frame::AttachmentCFrame => Some(cf_of(self.game.doc.prop(r, "CFrame")).unwrap_or(Xform { pos: Vec3::ZERO, rot: Mat3::IDENTITY })),
            Frame::AttachmentWorldCFrame => rustudio_core::attachment::world(&self.game.doc, r, false),
        }
    }

    fn tags_section(&mut self, p: &mut Painter, first: Ref, edits: &mut Vec<Edit>) {
        let mut plus = false;
        self.band(p, "Tags", false, Some(&mut plus));
        let tags: Vec<String> = match self.game.doc.prop(first, "Tags") {
            Some(Variant::Tags(t)) => t.iter().map(|s| s.to_string()).collect(),
            _ => Vec::new(),
        };
        if plus {
            self.props.editing = Some(("tag:new".into(), String::new()));
        }
        if self.props.editing.as_ref().is_some_and(|(k, _)| k == "tag:new") {
            let (_, v, _) = Self::cells(p, "tag:new");
            if let Some(t) = self.text_cell(p, "tag:new", v, "", false, false) {
                if !t.trim().is_empty() {
                    edits.push(Edit::AddTag(t.trim().to_string()));
                }
            }
        }
        if tags.is_empty() {
            let (r, _, _) = Self::cells(p, "tags:none");
            Self::name_text(p, r, -0.5, "No Tags have been added yet", READ_ONLY.gamma_multiply(1.6), None);
        }
        for t in tags {
            let (r, v, resp) = Self::cells(p, &format!("tag:{t}"));
            Self::name_text(p, r, 0.0, &t, TEXT, None);
            let x = Rect::from_center_size(Pos2::new(v.max.x - 8.0, v.center().y), Vec2::splat(12.0));
            rustudio_engine::ti::paint(p.ui.painter(), x, "x", TEXT);
            if resp.clicked() && resp.interact_pointer_pos().is_some_and(|q| x.expand(3.0).contains(q)) {
                edits.push(Edit::RemoveTag(t));
            }
        }
    }

    fn attributes_section(&mut self, p: &mut Painter, first: Ref) {
        let mut plus = false;
        self.band(p, "Attributes", false, Some(&mut plus));
        let attrs: Vec<(String, String)> = match self.game.doc.prop(first, "Attributes") {
            Some(Variant::Attributes(a)) => {
                let mut v: Vec<(String, String)> = a
                    .iter()
                    .filter(|(k, _)| !k.starts_with("RBX_"))
                    .map(|(k, v)| (k.clone(), value_text(&self.game.doc, v)))
                    .collect();
                v.sort();
                v
            }
            _ => Vec::new(),
        };
        if attrs.is_empty() {
            let (r, _, _) = Self::cells(p, "attrs:none");
            Self::name_text(p, r, -0.5, "No Attribute has been added yet", READ_ONLY.gamma_multiply(1.6), None);
        }
        for (k, v) in attrs {
            let (r, vr, _) = Self::cells(p, &format!("attr:{k}"));
            Self::name_text(p, r, 0.0, &k, TEXT, None);
            Self::value_label(p, vr, &v, TEXT);
        }
    }

    /// Whether `candidate` can be assigned to the currently active `Variant::Ref` picker.
    pub fn ref_pick_accepts(&self, candidate: Ref) -> bool {
        let Some(pick) = self.props.ref_pick.as_ref() else {
            return false;
        };
        let Some(class) = self.game.doc.class_of(candidate) else {
            return false;
        };
        if !doc::is_a(&class, &pick.expected_class) {
            return false;
        }
        if pick.prop == "Parent" {
            // Reject reparenting an instance into itself or any of its descendants.
            if pick.targets.iter().any(|&t| t == candidate || self.game.doc.is_ancestor(t, candidate)) {
                return false;
            }
        }
        true
    }

    /// Completes the active `Variant::Ref` picker with `picked`, keeping selection intact.
    pub fn commit_ref_pick(&mut self, picked: Ref) -> bool {
        if !self.ref_pick_accepts(picked) {
            return false;
        }
        let Some(pick) = self.props.ref_pick.take() else {
            return false;
        };
        if pick.prop == "Parent" {
            let n = self.game.doc.reparent(&pick.targets, picked);
            if n > 0 {
                self.explorer.expanded.insert(picked);
            }
            return n > 0;
        }
        let changes: Vec<_> = pick
            .targets
            .iter()
            .copied()
            .filter(|r| self.game.doc.class_of(*r).is_some_and(|c| has_prop(&c, &pick.prop)))
            .map(|r| (r, ustr(&pick.prop), Variant::Ref(picked)))
            .collect();
        if !changes.is_empty() {
            self.game.doc.set_props(&format!("Set {}", pick.prop), changes, None);
            true
        } else {
            false
        }
    }

    fn apply_prop_edits(&mut self, sel: &[Ref], first: Ref, edits: Vec<Edit>) {
        for e in edits {
            match e {
                Edit::Prop(name, Variant::Ref(target)) if name == "Parent" => {
                    if target.is_some()
                        && !sel.iter().any(|&t| t == target || self.game.doc.is_ancestor(t, target))
                    {
                        self.game.doc.reparent(sel, target);
                    }
                }
                Edit::Prop(name, v) if name == "Name" => {
                    if let Variant::String(n) = v {
                        let n = n.trim().to_string();
                        if !n.is_empty() {
                            for r in sel {
                                self.game.doc.rename(*r, &n);
                            }
                        }
                    }
                }
                // A part's BrickColor sets its Color (the colour it is drawn with).
                Edit::Prop(name, Variant::BrickColor(b)) if name == "BrickColor" && self.game.doc.class_of(first).is_some_and(|c| doc::is_a(&c, "BasePart")) => {
                    let rgb = rustudio_render::scene::brick_rgb(b);
                    let changes = sel
                        .iter()
                        .copied()
                        .filter(|r| self.game.doc.class_of(*r).is_some_and(|c| doc::is_a(&c, "BasePart")))
                        .map(|r| (r, ustr("Color"), Variant::Color3uint8(Color3uint8::new(rgb[0], rgb[1], rgb[2]))))
                        .collect();
                    self.game.doc.set_props("Set BrickColor", changes, None);
                }
                // A script's Enabled box sets Enabled and clears an old Disabled.
                Edit::Prop(name, Variant::Bool(on)) if name == "Enabled" && self.game.doc.class_of(first).is_some_and(|c| doc::is_a(&c, "BaseScript")) => {
                    let changes = sel
                        .iter()
                        .copied()
                        .filter(|r| self.game.doc.class_of(*r).is_some_and(|c| doc::is_a(&c, "BaseScript")))
                        .flat_map(|r| [(r, ustr("Enabled"), Variant::Bool(on)), (r, ustr("Disabled"), Variant::Bool(!on))])
                        .collect();
                    self.game.doc.set_props("Set Enabled", changes, None);
                }
                // A WeldConstraint's Enabled box keeps `State` bit 0 in sync so
                // saving and Team Create replication persist the enabled flag.
                Edit::Prop(name, Variant::Bool(on)) if name == "Enabled" && self.game.doc.class_of(first).as_deref() == Some("WeldConstraint") => {
                    let changes = sel
                        .iter()
                        .copied()
                        .filter(|r| self.game.doc.class_of(*r).as_deref() == Some("WeldConstraint"))
                        .flat_map(|r| {
                            let prev = match self.game.doc.prop(r, "State") {
                                Some(Variant::Int32(s)) => s,
                                _ => 3,
                            };
                            let next = if on { prev | 1 } else { prev & !1 };
                            [(r, ustr("Enabled"), Variant::Bool(on)), (r, ustr("State"), Variant::Int32(next))]
                        })
                        .collect();
                    self.game.doc.set_props("Set Enabled", changes, None);
                }
                // Ticking CustomPhysicalProperties starts from Plastic's values
                // (density 0.7, friction 0.3, elasticity 0.5, weights 1).
                Edit::Prop(name, Variant::Bool(on)) if name == "CustomPhysicalProperties" => {
                    let v = Variant::PhysicalProperties(if on {
                        PhysicalProperties::Custom(CustomPhysicalProperties::new(0.7, 0.3, 0.5, 1.0, 1.0, 1.0))
                    } else {
                        PhysicalProperties::Default
                    });
                    let changes = sel
                        .iter()
                        .copied()
                        .filter(|r| self.game.doc.class_of(*r).is_some_and(|c| doc::is_a(&c, "BasePart")))
                        .map(|r| (r, ustr("CustomPhysicalProperties"), v.clone()))
                        .collect();
                    self.game.doc.set_props("Set CustomPhysicalProperties", changes, None);
                }
                // Editing AudioContent on Sound / AudioPlayer keeps legacy
                // SoundId / Asset in sync (Roblox treats the pair as one property).
                Edit::Prop(name, Variant::Content(c))
                    if name == "AudioContent"
                        && self.game.doc.class_of(first).is_some_and(|cl| matches!(cl.as_str(), "Sound" | "AudioPlayer")) =>
                {
                    let uri = c.as_uri().unwrap_or("").to_string();
                    let mut changes = Vec::new();
                    for &r in sel {
                        match self.game.doc.class_of(r).as_deref() {
                            Some("Sound") => {
                                changes.push((r, ustr("AudioContent"), Variant::Content(c.clone())));
                                changes.push((r, ustr("SoundId"), Variant::ContentId(uri.clone().into())));
                            }
                            Some("AudioPlayer") => {
                                changes.push((r, ustr("AudioContent"), Variant::Content(c.clone())));
                                changes.push((r, ustr("Asset"), Variant::ContentId(uri.clone().into())));
                            }
                            _ => {}
                        }
                    }
                    self.game.doc.set_props("Set AudioContent", changes, None);
                }
                // Editing Image / ImageContent on ImageLabel / ImageButton keeps the twin in sync.
                Edit::Prop(name, v)
                    if matches!(name.as_str(), "Image" | "ImageContent")
                        && self.game.doc.class_of(first).is_some_and(|cl| matches!(cl.as_str(), "ImageLabel" | "ImageButton")) =>
                {
                    let uri = match &v {
                        Variant::Content(c) => c.as_uri().unwrap_or("").to_string(),
                        Variant::ContentId(c) => c.as_str().to_string(),
                        _ => String::new(),
                    };
                    let mut changes = Vec::new();
                    for &r in sel {
                        if self.game.doc.class_of(r).is_some_and(|cl| matches!(cl.as_str(), "ImageLabel" | "ImageButton")) {
                            changes.push((r, ustr("Image"), Variant::ContentId(uri.clone().into())));
                            changes.push((r, ustr("ImageContent"), Variant::Content(rbx_dom_weak::types::Content::from(uri.clone()))));
                        }
                    }
                    self.game.doc.set_props(&format!("Set {name}"), changes, None);
                }
                // An Attachment's axes are not stored: setting one turns the
                // attachment's CFrame (attachment_axis_change).
                Edit::Prop(name, Variant::Vector3(v))
                    if matches!(name.as_str(), "Axis" | "SecondaryAxis" | "WorldAxis" | "WorldSecondaryAxis")
                        && self.game.doc.class_of(first).is_some_and(|c| doc::is_a(&c, "Attachment")) =>
                {
                    let v = rustudio_core::math::v3(v);
                    let changes: Vec<_> = sel
                        .iter()
                        .filter_map(|&r| self.attachment_axis_change(r, &name, v))
                        .collect();
                    if !changes.is_empty() {
                        self.game.doc.set_props(&format!("Set {name}"), changes, None);
                    }
                }
                Edit::Prop(name, v) => {
                    let targets: Vec<Ref> = sel
                        .iter()
                        .copied()
                        .filter(|r| self.game.doc.class_of(*r).is_some_and(|c| has_prop(&c, &name)))
                        .collect();
                    let changes = targets.iter().map(|r| (*r, ustr(&name), v.clone())).collect();
                    self.game.doc.set_props(&format!("Set {name}"), changes, Some(format!("prop:{name}:{targets:?}")));
                }
                Edit::SubProp(name, sub) => {
                    // A per-component edit (X / Y / Z under the row) of an
                    // Attachment axis turns the CFrame too, not a stored prop.
                    let attachment_axis = matches!(name.as_str(), "Axis" | "SecondaryAxis" | "WorldAxis" | "WorldSecondaryAxis")
                        && self.game.doc.class_of(first).is_some_and(|c| doc::is_a(&c, "Attachment"));
                    if attachment_axis {
                        let changes: Vec<_> = sel
                            .iter()
                            .filter_map(|&r| {
                                let c = self.game.doc.class_of(r)?;
                                if !doc::is_a(&c, "Attachment") {
                                    return None;
                                }
                                let Variant::Vector3(nv) = self.prop_value(r, &c, &name).and_then(|old| sub.apply(&old))? else {
                                    return None;
                                };
                                self.attachment_axis_change(r, &name, rustudio_core::math::v3(nv))
                            })
                            .collect();
                        if !changes.is_empty() {
                            self.game.doc.set_props(&format!("Set {name}"), changes, None);
                        }
                        continue;
                    }
                    let mut changes = Vec::new();
                    for &r in sel {
                        let Some(c) = self.game.doc.class_of(r).map(|s| s.to_string()) else { continue };
                        if !has_prop(&c, &name) {
                            continue;
                        }
                        if let Some(old) = self.prop_value(r, &c, &name) {
                            if let Some(nv) = sub.apply(&old) {
                                changes.push((r, ustr(&name), nv));
                            }
                        }
                    }
                    if !changes.is_empty() {
                        self.game.doc.set_props(&format!("Set {name}"), changes, None);
                    }
                }
                Edit::Frame(f, pos, ori) => self.set_frame(first, f, pos, ori),
                Edit::AddTag(t) => self.edit_tags(sel, |tags| {
                    if !tags.contains(&t) {
                        tags.push(t.clone());
                    }
                }),
                Edit::RemoveTag(t) => self.edit_tags(sel, |tags| tags.retain(|x| *x != t)),
            }
        }
    }

    fn edit_tags(&mut self, sel: &[Ref], f: impl Fn(&mut Vec<String>)) {
        let changes = sel
            .iter()
            .map(|r| {
                let mut tags: Vec<String> = match self.game.doc.prop(*r, "Tags") {
                    Some(Variant::Tags(t)) => t.iter().map(|s| s.to_string()).collect(),
                    _ => Vec::new(),
                };
                f(&mut tags);
                (*r, ustr("Tags"), Variant::Tags(Tags::from(tags)))
            })
            .collect();
        self.game.doc.set_props("Set Tags", changes, None);
    }

    /// Writes a Transform / Pivot row: CFrame directly; Origin by moving
    /// the part (or model) so its pivot lands there; the pivot offsets alone.
    fn set_frame(&mut self, r: Ref, f: Frame, pos: Option<Vec3>, ori: Option<Vec3>) {
        let Some(cur) = self.frame_value(r, f) else {
            return;
        };
        let new = Xform::from_orientation_deg(pos.unwrap_or(cur.pos), ori.unwrap_or_else(|| cur.orientation_deg()));
        let cf = |x: &Xform| Variant::CFrame(x.to_cframe());
        match f {
            Frame::PartCFrame => self.game.doc.set_props("Set CFrame", vec![(r, ustr("CFrame"), cf(&new))], None),
            Frame::PivotOffset => self.game.doc.set_props("Set PivotOffset", vec![(r, ustr("PivotOffset"), cf(&new))], None),
            Frame::PartOrigin => {
                let offset = cf_of(self.game.doc.prop(r, "PivotOffset")).unwrap_or(Xform { pos: Vec3::ZERO, rot: glam::Mat3::IDENTITY });
                let part = mul(&new, &offset.inverse());
                self.game.doc.set_props("Set Origin", vec![(r, ustr("CFrame"), cf(&part))], None);
            }
            Frame::WorldPivot => self.game.doc.set_props(
                "Set WorldPivot",
                vec![(r, ustr("WorldPivotData"), Variant::OptionalCFrame(Some(new.to_cframe())))],
                None,
            ),
            Frame::ModelOrigin => {
                // Move every part of the model by new × old⁻¹.
                let delta = mul(&new, &cur.inverse());
                let mut changes: Vec<(Ref, rbx_dom_weak::Ustr, Variant)> = self.game
                    .doc
                    .dom
                    .descendants_of(r)
                    .map(|i| i.referent())
                    .filter_map(|p| cf_of(self.game.doc.prop(p, "CFrame")).filter(|_| self.game.scene.get(p).is_some()).map(|c| (p, ustr("CFrame"), cf(&mul(&delta, &c)))))
                    .collect();
                changes.push((r, ustr("WorldPivotData"), Variant::OptionalCFrame(Some(new.to_cframe()))));
                self.game.doc.set_props("Set Origin", changes, None);
            }
            Frame::AttachmentCFrame => self.game.doc.set_props("Set CFrame", vec![(r, ustr("CFrame"), cf(&new))], None),
            Frame::AttachmentWorldCFrame => {
                // WorldCFrame is the parent part's frame (with the Bones above
                // it) × the stored local CFrame; write the local part.
                let parent = rustudio_core::attachment::parent(&self.game.doc, r, true)
                    .unwrap_or(Xform { pos: Vec3::ZERO, rot: Mat3::IDENTITY });
                let local = mul(&parent.inverse(), &new);
                self.game.doc.set_props("Set WorldCFrame", vec![(r, ustr("CFrame"), cf(&local))], None);
            }
        }
    }

    /// The `CFrame` change that sets `Attachment.Axis` / `SecondaryAxis` (or
    /// their World versions), the same rule as the Lua runtime's setter
    /// (crates/rustudio-engine/src/runtime.rs): Axis becomes the X axis and
    /// the old Y is made perpendicular to it (Z = X × Y); SecondaryAxis
    /// keeps X and takes Y perpendicular to it. A zero vector changes
    /// nothing.
    fn attachment_axis_change(&self, r: Ref, name: &str, v: Vec3) -> Option<(Ref, rbx_dom_weak::Ustr, Variant)> {
        let doc = &self.game.doc;
        if !doc.class_of(r).is_some_and(|c| doc::is_a(&c, "Attachment")) {
            return None;
        }
        let local_cf = cf_of(doc.prop(r, "CFrame")).unwrap_or(Xform { pos: Vec3::ZERO, rot: Mat3::IDENTITY });
        let parent = rustudio_core::attachment::parent(doc, r, true)
            .unwrap_or(Xform { pos: Vec3::ZERO, rot: Mat3::IDENTITY });
        let v = if name.starts_with("World") { parent.rot.transpose() * v } else { v };
        let rot = attachment_rot_after_axis(local_cf.rot, v, name.ends_with("SecondaryAxis"));
        Some((r, ustr("CFrame"), Variant::CFrame(Xform { pos: local_cf.pos, rot }.to_cframe())))
    }
}

#[allow(clippy::too_many_arguments)]
fn text_cell_on(
    props: &mut PropertiesState,
    frame_no: u64,
    p: &mut Painter,
    key: &str,
    v: Rect,
    shown: &str,
    ro: bool,
    clicked: bool,
) -> Option<String> {
    use egui::text::{CCursor, CCursorRange};
    let eid = egui::Id::new(("prop_edit", key));
    let ctx = p.ui.ctx().clone();
    let (pressed, down, pos, now, shift) =
        p.ui.input(|i| (i.pointer.primary_pressed(), i.pointer.primary_down(), i.pointer.interact_pos(), i.time, i.modifiers.shift));
    // Count quick presses once a frame (1 = caret, 2 = word, 3 = all).
    if pressed && props.press_frame != frame_no {
        props.press_frame = frame_no;
        let near = pos.zip(props.press_pos).is_some_and(|(a, b)| a.distance(b) < 6.0);
        props.press_count = if near && now - props.press_time < 0.4 { props.press_count % 3 + 1 } else { 1 };
        props.press_time = now;
        props.press_pos = pos;
    }
    let editing = props.editing.as_ref().is_some_and(|(k, _)| k == key);
    if !editing {
        Editor::value_label(p, v, shown, if ro { READ_ONLY } else { TEXT });
        if clicked && !ro {
            props.editing = Some((key.to_string(), shown.to_string()));
            props.select_all_pending = true;
            let mut st = egui::text_edit::TextEditState::default();
            st.cursor.set_char_range(Some(CCursorRange::two(CCursor::new(0), CCursor::new(shown.chars().count()))));
            st.store(&ctx, eid);
            // Request focus immediately so on the next frame `has_focus(eid)`
            // (and `owns_ime_events(eid)`) is already true — egui 0.36
            // collapses any non-empty cursor selection when `!owns_ime_events`.
            ctx.memory_mut(|m| m.request_focus(eid));
            props.drag = None;
            props.edit_layout = None;
            ctx.request_repaint();
        }
        return None;
    }
    let mut text = props.editing.as_ref().map(|(_, t)| t.clone()).unwrap_or_default();
    let n = text.chars().count();
    if props.select_all_pending {
        let mut st = egui::text_edit::TextEditState::load(&ctx, eid).unwrap_or_default();
        st.cursor.set_char_range(Some(CCursorRange::two(CCursor::new(0), CCursor::new(n))));
        st.store(&ctx, eid);
        if ctx.memory(|m| m.has_focus(eid)) {
            props.select_all_pending = false;
        }
    }
    // Selection from the pointer, worked out against last frame's layout
    // so this frame already paints it.
    let mut sel: Option<(usize, usize)> = None;
    if let Some((_, galley, gpos, rect)) = props.edit_layout.clone().filter(|l| l.0 == key) {
        let char_at = |q: Pos2| usize::from(galley.cursor_from_pos(q - gpos).index).min(n);
        match (pos, props.drag) {
            (Some(q), _) if pressed && rect.contains(q) => {
                let c = char_at(q);
                let (a, b) = match props.press_count {
                    1 if shift => {
                        let anchor = egui::text_edit::TextEditState::load(&ctx, eid)
                            .and_then(|s| s.cursor.char_range())
                            .map_or(c, |r| usize::from(r.secondary.index));
                        (anchor, c)
                    }
                    1 => (c, c),
                    2 => word_bounds(&text, c),
                    _ => (0, n),
                };
                props.drag = Some((props.press_count, a, b));
                sel = Some((a, b));
            }
            (Some(q), Some((mode, a, b))) if down && !pressed => {
                let c = char_at(q);
                sel = Some(match mode {
                    1 => (a, c),
                    2 => {
                        let (wa, wb) = word_bounds(&text, c);
                        if wa < a { (b, wa) } else { (a, wb.max(b)) }
                    }
                    _ => (0, n),
                });
            }
            _ => {}
        }
    }
    if !down {
        props.drag = None;
    }
    if let Some((a, b)) = sel {
        let mut st = egui::text_edit::TextEditState::load(&ctx, eid).unwrap_or_default();
        st.cursor.set_char_range(Some(CCursorRange::two(CCursor::new(a), CCursor::new(b))));
        st.store(&ctx, eid);
    }
    let out = p
        .ui
        .scope_builder(egui::UiBuilder::new().max_rect(v).layout(egui::Layout::centered_and_justified(egui::Direction::TopDown)), |ui| {
            egui::TextEdit::singleline(&mut text).id(eid).frame(egui::Frame::NONE).font(FontId::proportional(FONT)).show(ui)
        })
        .inner;
    let resp = out.response.response.clone();
    // Sits over the edit box so egui's own click handling never runs.
    p.ui.interact(resp.rect, eid.with("mouse"), egui::Sense::CLICK | egui::Sense::DRAG).on_hover_cursor(egui::CursorIcon::Text);
    props.edit_layout = Some((key.to_string(), out.galley.clone(), out.galley_pos, resp.rect));
    resp.request_focus();
    let (enter, esc) = p.ui.input(|i| (i.key_pressed(egui::Key::Enter), i.key_pressed(egui::Key::Escape)));
    if esc {
        props.editing = None;
        props.select_all_pending = false;
        return None;
    }
    if enter || resp.lost_focus() {
        props.editing = None;
        props.select_all_pending = false;
        return Some(text);
    }
    props.editing = Some((key.to_string(), text));
    None
}

enum UDimSubEdit {
    Whole(UDim),
    Scale(f32),
    Offset(i32),
}

fn strip_rich_tags(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut in_tag = false;
    for ch in s.chars() {
        match ch {
            '<' => in_tag = true,
            '>' => in_tag = false,
            _ if !in_tag => out.push(ch),
            _ => {}
        }
    }
    out
}

fn opt_variant_eq(a: Option<&Variant>, b: Option<&Variant>) -> bool {
    match (a, b) {
        (Some(x), Some(y)) => variant_eq(x, y),
        (None, None) => true,
        _ => false,
    }
}

fn variant_eq(a: &Variant, b: &Variant) -> bool {
    const EPS: f32 = 1e-5;
    match (a, b) {
        (Variant::Bool(x), Variant::Bool(y)) => x == y,
        (Variant::Int32(x), Variant::Int32(y)) => x == y,
        (Variant::Int64(x), Variant::Int64(y)) => x == y,
        (Variant::Float32(x), Variant::Float32(y)) => (x - y).abs() < EPS,
        (Variant::Float64(x), Variant::Float64(y)) => (x - y).abs() < 1e-5,
        (Variant::String(x), Variant::String(y)) => x == y,
        (Variant::Vector2(x), Variant::Vector2(y)) => (x.x - y.x).abs() < EPS && (x.y - y.y).abs() < EPS,
        (Variant::Vector3(x), Variant::Vector3(y)) => (x.x - y.x).abs() < EPS && (x.y - y.y).abs() < EPS && (x.z - y.z).abs() < EPS,
        (Variant::UDim(x), Variant::UDim(y)) => (x.scale - y.scale).abs() < EPS && x.offset == y.offset,
        (Variant::UDim2(x), Variant::UDim2(y)) => {
            (x.x.scale - y.x.scale).abs() < EPS && x.x.offset == y.x.offset && (x.y.scale - y.y.scale).abs() < EPS && x.y.offset == y.y.offset
        }
        (Variant::Rect(x), Variant::Rect(y)) => {
            (x.min.x - y.min.x).abs() < EPS && (x.min.y - y.min.y).abs() < EPS && (x.max.x - y.max.x).abs() < EPS && (x.max.y - y.max.y).abs() < EPS
        }
        (Variant::NumberRange(x), Variant::NumberRange(y)) => (x.min - y.min).abs() < EPS && (x.max - y.max).abs() < EPS,
        (Variant::Color3(x), Variant::Color3(y)) => (x.r - y.r).abs() < 0.002 && (x.g - y.g).abs() < 0.002 && (x.b - y.b).abs() < 0.002,
        (Variant::Color3uint8(x), Variant::Color3uint8(y)) => x == y,
        (Variant::Color3(x), Variant::Color3uint8(y)) | (Variant::Color3uint8(y), Variant::Color3(x)) => {
            ((x.r * 255.0).round() as u8) == y.r && ((x.g * 255.0).round() as u8) == y.g && ((x.b * 255.0).round() as u8) == y.b
        }
        (Variant::BrickColor(x), Variant::BrickColor(y)) => x == y,
        (Variant::Enum(x), Variant::Enum(y)) => x.to_u32() == y.to_u32(),
        (Variant::Enum(x), Variant::EnumItem(y)) | (Variant::EnumItem(y), Variant::Enum(x)) => x.to_u32() == y.value,
        (Variant::EnumItem(x), Variant::EnumItem(y)) => x.value == y.value,
        (Variant::ContentId(x), Variant::ContentId(y)) => x.as_str() == y.as_str(),
        (Variant::Content(x), Variant::Content(y)) => x.as_uri() == y.as_uri(),
        (Variant::Font(x), Variant::Font(y)) => x.family == y.family && x.weight == y.weight && x.style == y.style,
        (Variant::Ref(x), Variant::Ref(y)) => x == y,
        _ => a == b,
    }
}

/// Whether an editable property differs from its class default in the
/// reflection database (Studio bolds modified property names).
fn is_non_default(class: &str, name: &str, value: &Variant, ro: bool) -> bool {
    if ro || matches!(name, "ClassName" | "Parent" | "UniqueId" | "IsInSandbox") {
        return false;
    }
    if name == "Name" {
        return matches!(value, Variant::String(s) if s != class);
    }
    let Some(def) = doc::class_default(class, name) else {
        return false;
    };
    !variant_eq(value, &def)
}

/// A property with no stored value and no default in the reflection
/// database (IgnoreGuiInset, AbsoluteSize…): the empty value of its type.
fn typed_empty(class: &str, prop: &str) -> Option<Variant> {
    use rbx_dom_weak::types::VariantType as T;
    let db = doc::db();
    let desc = db.classes.get(class)?;
    let dt = db.superclasses_iter(desc).find_map(|c| Some(&c.properties.get(prop)?.data_type))?;
    match dt {
        rbx_reflection::DataType::Enum(_) => Some(Variant::Enum(Enum::from_u32(0))),
        rbx_reflection::DataType::Value(ty) => Some(match ty {
            T::Bool => Variant::Bool(false),
            T::Float32 => Variant::Float32(0.0),
            T::Float64 => Variant::Float64(0.0),
            T::Int32 => Variant::Int32(0),
            T::Int64 => Variant::Int64(0),
            T::String => Variant::String(String::new()),
            T::Vector2 => Variant::Vector2(Vector2::new(0.0, 0.0)),
            T::Vector3 => Variant::Vector3(rbx_dom_weak::types::Vector3::new(0.0, 0.0, 0.0)),
            T::UDim => Variant::UDim(UDim::new(0.0, 0)),
            T::UDim2 => Variant::UDim2(UDim2::new(UDim::new(0.0, 0), UDim::new(0.0, 0))),
            T::Rect => Variant::Rect(RbxRect::new(Vector2::new(0.0, 0.0), Vector2::new(0.0, 0.0))),
            T::NumberRange => Variant::NumberRange(NumberRange::new(0.0, 0.0)),
            T::NumberSequence => Variant::NumberSequence(NumberSequence {
                keypoints: vec![NumberSequenceKeypoint::new(0.0, 0.0, 0.0), NumberSequenceKeypoint::new(1.0, 0.0, 0.0)],
            }),
            T::ColorSequence => Variant::ColorSequence(ColorSequence {
                keypoints: vec![
                    ColorSequenceKeypoint::new(0.0, Color3::new(1.0, 1.0, 1.0)),
                    ColorSequenceKeypoint::new(1.0, Color3::new(1.0, 1.0, 1.0)),
                ],
            }),
            T::Color3 => Variant::Color3(Color3::new(1.0, 1.0, 1.0)),
            T::Font => Variant::Font(RbxFont::regular("rbxasset://fonts/families/SourceSansPro.json")),
            T::Ref => Variant::Ref(Ref::none()),
            T::Content => Variant::Content(rbx_dom_weak::types::Content::from(String::new())),
            T::ContentId => Variant::ContentId(String::new().into()),
            _ => return None,
        }),
        _ => None,
    }
}

/// Studio's single-number UDim heuristic: `|v| <= 1.0` or containing `'.'` is treated as Scale;
/// integer values with `|v| > 1.0` (e.g. `"100"`, `"-20"`) are Offset.
fn parse_udim_single(tok: &str) -> Option<UDim> {
    let tok = tok.trim().trim_matches(['{', '}', '[', ']']).trim();
    let v: f32 = tok.parse().ok()?;
    if v.abs() <= 1.0 || tok.contains('.') {
        Some(UDim::new(v, 0))
    } else {
        Some(UDim::new(0.0, v.round() as i32))
    }
}

fn normalize_asset_uri(t: &str) -> String {
    let t = t.trim();
    if !t.is_empty() && t.chars().all(|c| c.is_ascii_digit()) {
        format!("rbxassetid://{t}")
    } else {
        t.to_string()
    }
}

/// The enum type of `class.prop` from the reflection database.
fn enum_type_of(class: &str, prop: &str) -> Option<String> {
    let db = doc::db();
    let desc = db.classes.get(class)?;
    db.superclasses_iter(desc).find_map(|c| match &c.properties.get(prop)?.data_type {
        rbx_reflection::DataType::Enum(e) => Some(e.to_string()),
        _ => None,
    })
}

/// Parses typed text back into a value of the same kind.
fn parse_like(old: &Variant, raw: &str) -> Option<Variant> {
    // Preserve leading/trailing spaces and newlines for String properties.
    if matches!(old, Variant::String(_)) {
        return Some(Variant::String(raw.to_string()));
    }
    let t = raw.trim();
    Some(match old {
        Variant::Int32(_) => Variant::Int32(t.parse::<f64>().ok()?.round() as i32),
        Variant::Int64(_) => Variant::Int64(t.parse::<f64>().ok()?.round() as i64),
        Variant::Float32(_) => Variant::Float32(t.parse().ok()?),
        Variant::Float64(_) => Variant::Float64(t.parse().ok()?),
        Variant::Vector2(_) => {
            let p = parse_nums(t, 2)?;
            Variant::Vector2(Vector2::new(p[0], p[1]))
        }
        Variant::Vector2int16(_) => {
            let p = parse_nums(t, 2)?;
            Variant::Vector2int16(Vector2int16::new(p[0].round() as i16, p[1].round() as i16))
        }
        Variant::Vector3int16(_) => {
            let p = parse_nums(t, 3)?;
            Variant::Vector3int16(Vector3int16::new(p[0].round() as i16, p[1].round() as i16, p[2].round() as i16))
        }
        // Studio UDimComponentItem::parse:
        // 1 number -> Scale if |v| <= 1 or contains '.', else Offset;
        // 2 numbers -> "scale, offset".
        Variant::UDim(_) => {
            let toks: Vec<&str> = t.split(',').map(|x| x.trim()).filter(|x| !x.is_empty()).collect();
            match toks.len() {
                1 => Variant::UDim(parse_udim_single(toks[0])?),
                2 => {
                    let s: f32 = toks[0].trim_matches(['{', '}', '[', ']']).trim().parse().ok()?;
                    let o: f32 = toks[1].trim_matches(['{', '}', '[', ']']).trim().parse().ok()?;
                    Variant::UDim(UDim::new(s, o.round() as i32))
                }
                _ => return None,
            }
        }
        // Studio UDim2PropertyItem::parse:
        // 1 number -> same UDim (Scale if |v|<=1 or '.', else Offset) on X and Y;
        // 2 numbers starting with '{' -> single UDim {s, o} applied to X and Y;
        // 2 numbers without '{' -> X and Y each parsed by the single-number rule
        //   (so "0.5, 10" gives {0.5, 0},{0, 10}; "100, 200" gives {0, 100},{0, 200});
        // 4 numbers -> "{xs, xo},{ys, yo}" (braces optional).
        Variant::UDim2(_) => {
            let toks: Vec<&str> = t.split(',').map(|x| x.trim()).filter(|x| !x.is_empty()).collect();
            match toks.len() {
                1 => {
                    let u = parse_udim_single(toks[0])?;
                    Variant::UDim2(UDim2::new(u, u))
                }
                2 if t.starts_with('{') => {
                    let s: f32 = toks[0].trim_matches(['{', '}', '[', ']']).trim().parse().ok()?;
                    let o: f32 = toks[1].trim_matches(['{', '}', '[', ']']).trim().parse().ok()?;
                    let u = UDim::new(s, o.round() as i32);
                    Variant::UDim2(UDim2::new(u, u))
                }
                2 => Variant::UDim2(UDim2::new(parse_udim_single(toks[0])?, parse_udim_single(toks[1])?)),
                4 => {
                    let p = parse_nums(t, 4)?;
                    Variant::UDim2(UDim2::new(UDim::new(p[0], p[1].round() as i32), UDim::new(p[2], p[3].round() as i32)))
                }
                _ => return None,
            }
        }
        // Studio RectPropertyItem::parse:
        // 1 number v -> (0, 0, v, v); 2 numbers x, y -> (0, 0, x, y);
        // 4 numbers -> (x0, y0, x1, y1), normalized so min <= max.
        Variant::Rect(_) => {
            let toks: Vec<&str> = t.split(',').map(|x| x.trim()).filter(|x| !x.is_empty()).collect();
            let (x0, y0, x1, y1) = match toks.len() {
                1 => {
                    let v: f32 = toks[0].trim_matches(['{', '}', '[', ']']).trim().parse().ok()?;
                    (0.0, 0.0, v, v)
                }
                2 => {
                    let x: f32 = toks[0].trim_matches(['{', '}', '[', ']']).trim().parse().ok()?;
                    let y: f32 = toks[1].trim_matches(['{', '}', '[', ']']).trim().parse().ok()?;
                    (0.0, 0.0, x, y)
                }
                4 => {
                    let p = parse_nums(t, 4)?;
                    (p[0], p[1], p[2], p[3])
                }
                _ => return None,
            };
            Variant::Rect(RbxRect::new(
                Vector2::new(x0.min(x1), y0.min(y1)),
                Vector2::new(x0.max(x1), y0.max(y1)),
            ))
        }
        // Studio NumberRangePropertyItem:
        // "min, max" (or "min max" / "(min, max)") or single number "v" -> (v, v).
        Variant::NumberRange(_) => {
            let trimmed = t.trim().trim_matches(|c| c == '(' || c == ')');
            let nums: Vec<f32> = trimmed
                .split(|c: char| c == ',' || c.is_whitespace())
                .filter(|x| !x.is_empty())
                .map(|x| x.parse::<f32>().ok())
                .collect::<Option<_>>()?;
            match nums.len() {
                1 => Variant::NumberRange(NumberRange::new(nums[0], nums[0])),
                2 => Variant::NumberRange(NumberRange::new(nums[0].min(nums[1]), nums[0].max(nums[1]))),
                _ => return None,
            }
        }
        // NumberSequence: single number -> constant sequence; two numbers -> linear ramp 0..1.
        Variant::NumberSequence(_) => {
            let nums: Vec<f32> = t
                .split(|c: char| c == ',' || c.is_whitespace())
                .filter(|x| !x.is_empty())
                .map(|x| x.parse::<f32>().ok())
                .collect::<Option<_>>()?;
            match nums.len() {
                1 => Variant::NumberSequence(NumberSequence {
                    keypoints: vec![
                        NumberSequenceKeypoint::new(0.0, nums[0], 0.0),
                        NumberSequenceKeypoint::new(1.0, nums[0], 0.0),
                    ],
                }),
                2 => Variant::NumberSequence(NumberSequence {
                    keypoints: vec![
                        NumberSequenceKeypoint::new(0.0, nums[0], 0.0),
                        NumberSequenceKeypoint::new(1.0, nums[1], 0.0),
                    ],
                }),
                _ => return None,
            }
        }
        // ColorSequence: "r, g, b" -> constant 2-stop color sequence.
        Variant::ColorSequence(_) => {
            let n = parse_nums(t, 3)?;
            let c = [n[0].clamp(0.0, 255.0) as u8, n[1].clamp(0.0, 255.0) as u8, n[2].clamp(0.0, 255.0) as u8];
            color_variant(old, c)
        }
        Variant::ContentId(_) => Variant::ContentId(normalize_asset_uri(t).into()),
        Variant::Content(_) => Variant::Content(rbx_dom_weak::types::Content::from(normalize_asset_uri(t))),
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    /// Attachment Axis / SecondaryAxis edits turn the frame the same way the
    /// Lua runtime does (runtime.rs): Axis = new X, Y kept perpendicular; a
    /// SecondaryAxis parallel to X is ignored; Z = X × Y.
    #[test]
    fn attachment_axis_edits_turn_the_frame() {
        use glam::{Mat3, Vec3};
        let id = Mat3::IDENTITY;
        let x = Vec3::X;
        let y = Vec3::Y;

        // Axis to +Y: X = +Y, the old Y is parallel so Z (0,0,1) × X gives
        // the new Y = (-1, 0, 0), and Z = X × Y = (0, 0, 1).
        let r = super::attachment_rot_after_axis(id, y, false);
        assert!((r.x_axis - y).length() < 1e-5);
        assert!((r.y_axis - Vec3::new(-1.0, 0.0, 0.0)).length() < 1e-5);
        assert!((r.z_axis - Vec3::new(0.0, 0.0, 1.0)).length() < 1e-5);

        // Axis (-1, 0, 0): X flips, Y stays +Y, Z flips (still right-handed).
        let r = super::attachment_rot_after_axis(id, -x, false);
        assert!((r.x_axis + x).length() < 1e-5);
        assert!((r.y_axis - y).length() < 1e-5);
        assert!((r.z_axis + Vec3::Z).length() < 1e-5);

        // SecondaryAxis (-1, 0, 0) is parallel to X: no change.
        let r = super::attachment_rot_after_axis(id, -x, true);
        assert!((r.x_axis - x).length() < 1e-5);
        assert!((r.y_axis - y).length() < 1e-5);

        // SecondaryAxis (0, 0, 1): Y becomes +Z, Z = X × Y = -Y.
        let r = super::attachment_rot_after_axis(id, Vec3::Z, true);
        assert!((r.y_axis - Vec3::Z).length() < 1e-5);
        assert!((r.z_axis + y).length() < 1e-5);

        // A zero vector changes nothing.
        assert_eq!(super::attachment_rot_after_axis(id, Vec3::ZERO, false), id);
        assert_eq!(super::attachment_rot_after_axis(id, Vec3::ZERO, true), id);

        // A rotated starting frame: the new X is the vector, Y perpendicular.
        let start = Mat3::from_rotation_y(0.7);
        let v = Vec3::new(0.3, 1.0, -0.2).normalize();
        let r = super::attachment_rot_after_axis(start, v, false);
        assert!((r.x_axis - v).length() < 1e-5);
        assert!(r.y_axis.dot(v).abs() < 1e-5);
        assert!((r.x_axis.cross(r.y_axis) - r.z_axis).length() < 1e-5);
    }

    #[test]
    fn single_number_fills_vector() {
        assert_eq!(super::parse_nums("0", 3), Some(vec![0.0; 3]));
        assert_eq!(super::parse_nums(" 2.5 ", 2), Some(vec![2.5; 2]));
        assert_eq!(super::parse_nums("1, 2, 3", 3), Some(vec![1.0, 2.0, 3.0]));
        assert_eq!(super::parse_nums("1, 2", 3), None);
        assert_eq!(super::parse_nums("1, x, 3", 3), None);
    }

    #[test]
    fn udim2_round_trips_studio_text() {
        use rbx_dom_weak::types::{UDim, UDim2, Variant};
        let old = Variant::UDim2(UDim2::new(UDim::new(0.0, 0), UDim::new(0.0, 0)));
        let Some(Variant::UDim2(u)) = super::parse_like(&old, "{0.5, 10},{1, -20}") else { panic!() };
        assert_eq!((u.x.scale, u.x.offset, u.y.scale, u.y.offset), (0.5, 10, 1.0, -20));
        assert_eq!(super::value_text(&rustudio_core::doc::Document::new_place(), &Variant::UDim2(u)), "{0.5, 10},{1, -20}");
        let Some(Variant::UDim2(u)) = super::parse_like(&old, "{0.5, 10}, {1, -20}") else { panic!() };
        assert_eq!((u.x.scale, u.x.offset, u.y.scale, u.y.offset), (0.5, 10, 1.0, -20));
        let Some(Variant::UDim2(u)) = super::parse_like(&old, "0.25,0,0.75,4") else { panic!() };
        assert_eq!((u.x.scale, u.y.scale, u.y.offset), (0.25, 0.75, 4));
        let Some(Variant::UDim2(u)) = super::parse_like(&old, "0.5") else { panic!() };
        assert_eq!((u.x.scale, u.x.offset, u.y.scale, u.y.offset), (0.5, 0, 0.5, 0));
        let Some(Variant::UDim2(u)) = super::parse_like(&old, "1") else { panic!() };
        assert_eq!((u.x.scale, u.x.offset, u.y.scale, u.y.offset), (1.0, 0, 1.0, 0));
        // Studio: integer with |v| > 1 is Offset, float with '.' is Scale.
        let Some(Variant::UDim2(u)) = super::parse_like(&old, "100") else { panic!() };
        assert_eq!((u.x.scale, u.x.offset, u.y.scale, u.y.offset), (0.0, 100, 0.0, 100));
        let Some(Variant::UDim2(u)) = super::parse_like(&old, "2.5") else { panic!() };
        assert_eq!((u.x.scale, u.x.offset, u.y.scale, u.y.offset), (2.5, 0, 2.5, 0));
        let Some(Variant::UDim2(u)) = super::parse_like(&old, "0.5, 10") else { panic!() };
        assert_eq!((u.x.scale, u.x.offset, u.y.scale, u.y.offset), (0.5, 0, 0.0, 10));
        let Some(Variant::UDim2(u)) = super::parse_like(&old, "100, 200") else { panic!() };
        assert_eq!((u.x.scale, u.x.offset, u.y.scale, u.y.offset), (0.0, 100, 0.0, 200));
        let Some(Variant::UDim2(u)) = super::parse_like(&old, "{0.5, 10}") else { panic!() };
        assert_eq!((u.x.scale, u.x.offset, u.y.scale, u.y.offset), (0.5, 10, 0.5, 10));
        let Some(Variant::UDim2(u)) = super::parse_like(&old, "0.25, 0.75") else { panic!() };
        assert_eq!((u.x.scale, u.x.offset, u.y.scale, u.y.offset), (0.25, 0, 0.75, 0));
        assert!(super::parse_like(&old, "1, 2, 3").is_none());
        let u_old = Variant::UDim(UDim::new(0.0, 0));
        let Some(Variant::UDim(u)) = super::parse_like(&u_old, "0.5") else { panic!() };
        assert_eq!((u.scale, u.offset), (0.5, 0));
        let Some(Variant::UDim(u)) = super::parse_like(&u_old, "100") else { panic!() };
        assert_eq!((u.scale, u.offset), (0.0, 100));
        let Some(Variant::UDim(u)) = super::parse_like(&u_old, "0, 8") else { panic!() };
        assert_eq!(u.offset, 8);
    }

    #[test]
    fn gui_property_parsers_and_subedits_match_studio() {
        use rbx_dom_weak::types::{
            Color3, ColorSequence, ColorSequenceKeypoint, Font as RbxFont, FontStyle, FontWeight, NumberRange,
            NumberSequence, NumberSequenceKeypoint, Rect as RbxRect, UDim, UDim2, Variant, Vector2,
        };
        let doc = rustudio_core::doc::Document::new_place();

        // Rect 
        let r_old = Variant::Rect(RbxRect::new(Vector2::new(0.0, 0.0), Vector2::new(0.0, 0.0)));
        let Some(Variant::Rect(r)) = super::parse_like(&r_old, "10") else { panic!() };
        assert_eq!((r.min.x, r.min.y, r.max.x, r.max.y), (0.0, 0.0, 10.0, 10.0));
        let Some(Variant::Rect(r)) = super::parse_like(&r_old, "10, 20") else { panic!() };
        assert_eq!((r.min.x, r.min.y, r.max.x, r.max.y), (0.0, 0.0, 10.0, 20.0));
        let Some(Variant::Rect(r)) = super::parse_like(&r_old, "20, 10, 5, 30") else { panic!() };
        assert_eq!((r.min.x, r.min.y, r.max.x, r.max.y), (5.0, 10.0, 20.0, 30.0));
        assert_eq!(super::value_text(&doc, &Variant::Rect(r)), "5, 10, 20, 30");

        // NumberRange 
        let nr_old = Variant::NumberRange(NumberRange::new(0.0, 0.0));
        let Some(Variant::NumberRange(nr)) = super::parse_like(&nr_old, "5") else { panic!() };
        assert_eq!((nr.min, nr.max), (5.0, 5.0));
        let Some(Variant::NumberRange(nr)) = super::parse_like(&nr_old, "10, 3") else { panic!() };
        assert_eq!((nr.min, nr.max), (3.0, 10.0));
        assert_eq!(super::value_text(&doc, &Variant::NumberRange(nr)), "3, 10");
        let Some(Variant::NumberRange(nr)) = super::parse_like(&nr_old, "(2, 8)") else { panic!() };
        assert_eq!((nr.min, nr.max), (2.0, 8.0));

        // NumberSequence 
        let ns_old = Variant::NumberSequence(NumberSequence { keypoints: vec![] });
        let Some(ns) = super::parse_like(&ns_old, "0.5") else { panic!() };
        assert_eq!(super::value_text(&doc, &ns), "0.5");
        let ramp = Variant::NumberSequence(NumberSequence {
            keypoints: vec![NumberSequenceKeypoint::new(0.0, 0.0, 0.0), NumberSequenceKeypoint::new(1.0, 1.0, 0.0)],
        });
        assert_eq!(super::value_text(&doc, &ramp), "<NumberSequence>");

        // ColorSequence 
        let cs_old = Variant::ColorSequence(ColorSequence { keypoints: vec![] });
        let Some(cs) = super::parse_like(&cs_old, "255, 0, 128") else { panic!() };
        assert_eq!(super::value_text(&doc, &cs), "[255, 0, 128]");
        let grad = Variant::ColorSequence(ColorSequence {
            keypoints: vec![
                ColorSequenceKeypoint::new(0.0, Color3::new(1.0, 0.0, 0.0)),
                ColorSequenceKeypoint::new(1.0, Color3::new(0.0, 0.0, 1.0)),
            ],
        });
        assert_eq!(super::value_text(&doc, &grad), "<ColorSequence>");

        // String preserves leading/trailing whitespace and newlines
        let s_old = Variant::String(String::new());
        assert_eq!(super::parse_like(&s_old, "  hello\nworld "), Some(Variant::String("  hello\nworld ".into())));

        // Content / ContentId auto-prefixes bare numeric asset IDs
        let cid_old = Variant::ContentId(String::new().into());
        assert_eq!(super::parse_like(&cid_old, "123456"), Some(Variant::ContentId("rbxassetid://123456".into())));
        assert_eq!(super::parse_like(&cid_old, ""), Some(Variant::ContentId("".into())));

        // Multi-selection SubEdit preserves untouched components
        let u2 = Variant::UDim2(UDim2::new(UDim::new(0.5, 10), UDim::new(1.0, -20)));
        let Some(Variant::UDim2(u2_next)) = super::SubEdit::UDim2XOffset(42).apply(&u2) else { panic!() };
        assert_eq!((u2_next.x.scale, u2_next.x.offset, u2_next.y.scale, u2_next.y.offset), (0.5, 42, 1.0, -20));

        // Font SubEdit
        let f = Variant::Font(RbxFont::regular("rbxasset://fonts/families/SourceSansPro.json"));
        let Some(Variant::Font(f2)) = super::SubEdit::FontWeight(FontWeight::Bold).apply(&f) else { panic!() };
        assert_eq!(f2.weight, FontWeight::Bold);
        assert_eq!(f2.style, FontStyle::Normal);

        // Non-default bold detection
        assert!(!super::is_non_default("Frame", "BackgroundTransparency", &Variant::Float32(0.0), false));
        assert!(super::is_non_default("Frame", "BackgroundTransparency", &Variant::Float32(0.5), false));
        assert!(!super::is_non_default("Frame", "AbsoluteSize", &Variant::Vector2(Vector2::new(100.0, 100.0)), true));
    }

    /// Regression test: clicking a value cell (e.g. Position) must focus the
    /// TextEdit with ALL text selected on the very next frame so typing
    /// immediately replaces the entire value, matching Studio.
    #[test]
    fn text_cell_selects_all_on_click() {
        let mut props = super::PropertiesState::default();
        let ctx = egui::Context::default();
        let key = "Position";
        let initial = "{0, 100},{0, 200}";
        let eid = egui::Id::new(("prop_edit", key));
        let cell_rect = egui::Rect::from_min_size(egui::pos2(120.0, 40.0), egui::vec2(180.0, 20.0));

        // Frame 1: user clicks the value cell.
        ctx.run_ui(egui::RawInput::default(), |ui| {
            let mut p = super::Painter { ui, width: 300.0, x0: 0.0 };
            let out = super::text_cell_on(&mut props, 1, &mut p, key, cell_rect, initial, false, true);
            assert!(out.is_none());
        }).textures_delta.clear();

        // Frame 2: TextEdit renders — must already have focus AND select-all range!
        ctx.run_ui(egui::RawInput::default(), |ui| {
            let mut p = super::Painter { ui, width: 300.0, x0: 0.0 };
            let out = super::text_cell_on(&mut props, 2, &mut p, key, cell_rect, initial, false, false);
            assert!(out.is_none());
        }).textures_delta.clear();
        assert!(ctx.memory(|m| m.has_focus(eid)), "TextEdit must be focused after clicking cell");
        let range = egui::text_edit::TextEditState::load(&ctx, eid)
            .and_then(|s| s.cursor.char_range())
            .expect("TextEdit must have a cursor range");
        let sorted = range.as_sorted_char_range();
        assert_eq!(
            (usize::from(sorted.start), usize::from(sorted.end)),
            (0, initial.chars().count()),
            "all text must be selected on open"
        );

        // Frame 3: typing "1" + Enter replaces the entire selection instead of appending.
        let mut input = egui::RawInput::default();
        input.events.push(egui::Event::Text("1".into()));
        input.events.push(egui::Event::Key {
            key: egui::Key::Enter,
            physical_key: None,
            pressed: true,
            repeat: false,
            modifiers: egui::Modifiers::NONE,
        });
        let mut committed = None;
        ctx.run_ui(input, |ui| {
            let mut p = super::Painter { ui, width: 300.0, x0: 0.0 };
            committed = super::text_cell_on(&mut props, 3, &mut p, key, cell_rect, initial, false, false);
        }).textures_delta.clear();
        assert_eq!(committed.as_deref(), Some("1"));
    }

    #[test]
    fn numbers_like_studio() {
        assert_eq!(super::fmt_num(0.5), "0.5");
        assert_eq!(super::fmt_num(41.7330001), "41.733");
        assert_eq!(super::fmt_num(100000.0), "100000");
        assert_eq!(super::fmt_num(-0.0001), "0");
        assert_eq!(super::fmt_v3(glam::Vec3::new(4.0, 1.2, 2.0)), "4, 1.2, 2");
    }

    #[test]
    fn expected_ref_class_reads_api_dump() {
        assert_eq!(super::expected_ref_class("Part", "Parent"), "Instance");
        if rustudio_core::api::get().is_some() {
            assert_eq!(super::expected_ref_class("WeldConstraint", "Part0"), "BasePart");
            assert_eq!(super::expected_ref_class("Beam", "Attachment0"), "Attachment");
            assert_eq!(super::expected_ref_class("Model", "PrimaryPart"), "BasePart");
        }
    }
}
