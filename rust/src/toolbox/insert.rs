//! Inserting a Toolbox asset, as Studio's Toolbox does it (Toolbox_25017
//! Util/InsertAsset.lua, read as bytecode; NOTES.md):
//!
//! * the asset is downloaded and decoded off the UI thread (`prepare`,
//!   with stages for the progress line and a cancel flag);
//! * models, packages, meshes, decals, animations: the asset file's
//!   top-level instances. Sky / Atmosphere go to Lighting, a MaterialPack
//!   to MaterialService, a Tool / HopperBin asks Workspace or StarterPack,
//!   the rest goes to Workspace and is moved like `Model:MoveTo` to the
//!   insert point: the hit of the ray through the 3D view's centre
//!   (`getInsertPosition`, 2048 studs) or 64 studs in front of the camera,
//!   for a drag the hit under the mouse (snapped to the Move increment),
//!   raised until it overlaps nothing. A decal goes into the selected
//!   object (or Workspace); dropped on a part, into that part on the face
//!   under the mouse;
//! * audio: a Sound (SoundId, Name, SourceAssetId) in the first selected
//!   object or Workspace; video: a VideoFrame (Video, Size {1,0},{1,0})
//!   likewise; fonts: nothing is inserted (Studio's insertFont is empty);
//!   plugins: installed through the plugin store;
//! * one undo step named like Studio's waypoint ("After insert asset <id>"),
//!   the inserted objects selected, SourceAssetId set on them; a model with
//!   scripts asks first (`promptScriptWarningAndWait`) unless the account
//!   made it.

use glam::Vec3;
use rbx_dom_weak::types::{Ref, Variant, Vector3};
use rbx_dom_weak::WeakDom;

use super::api::{self, Failure, Http};
use super::categories::Kind;
use super::model::Asset;
use rustudio_core::math::Xform;

/// Where an insert goes.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Target {
    /// Click / Insert button / deep link: the view's insert point.
    Click,
    /// Dropped in the 3D view: the hit under the mouse (None: nothing under
    /// it, in front of the camera along the mouse ray), the part hit and its face.
    Drop { at: Option<Vec3>, ray_origin: Vec3, ray_dir: Vec3, part: Option<Ref>, face: Option<u32> },
    /// Dropped on an Explorer row: parented there.
    Parent(Ref),
}

/// A card dragged into the 3D view (edit mode). Studio inserts the asset as
/// the drag enters the view and hands the instances to the 3D view's dragger
/// while the button is still held: the active plugin's PluginMouse.DragEnter
/// when a plugin holds the mouse, else Studio's own Select / Move tools,
/// which are the same DraggerFramework (`DraggerToolComponent` connects
/// `mouse.DragEnter`). Its code, bundled in the corpus plugin Model Reflect
/// (stravant DraggerFramework 0.1.4): `DraggerToolModel:
/// _processToolboxInitiatedFreeformSelectionDrag` / `_processToolboxInitiatedFaceDrag`
/// "We didn't get an associated mouse down"; `DraggingParts` moves the
/// selection under the mouse each move from the bounding box centre
/// (`basisPoint = Vector3.new()`, "Just drag from the center of the
/// object") and on mouse up adds the waypoint "End Freeform Drag";
/// `DraggingFaceInstance` (one Decal / Texture) parents it to the part under
/// the mouse with Face = that surface and adds "Drag Face Instance"; "When
/// the user drags out of the 3D view, the face instance that was passed to
/// us will be removed".
#[derive(Debug, Clone, PartialEq)]
pub struct DragIn {
    pub id: u64,
    /// The insert job started on entering (None once applied or dropped).
    pub job: Option<u64>,
    /// What the insert made (DragEnter's instances), and its undo label.
    pub inserted: Vec<Ref>,
    pub label: String,
    /// The inserted roots and everything under them as inserted (what a
    /// drag-out takes back: `Document::retract_inserted`).
    pub made: Vec<Ref>,
    /// No plugin held the mouse on entering: rustudio's own dragger moves it.
    pub builtin: bool,
    /// The built-in dragger's live changes: property values before the drag
    /// (recorded as one step at the release).
    pub moved: Vec<(Ref, rbx_dom_weak::Ustr, Option<Variant>)>,
    /// Face drag: the Decal / Texture, its parent and Face as inserted.
    pub face: Option<(Ref, Ref, Option<Variant>)>,
    /// The pointer where the dragger last placed it.
    pub at: Option<[f32; 2]>,
}

/// What the worker made ready.
pub enum Prepared {
    Model { dom: WeakDom, scripts: usize },
    Sound,
    Video,
    Font,
    Plugin,
}

pub struct Ready {
    pub job: u64,
    pub asset: Asset,
    pub kind: Kind,
    pub target: Target,
    pub prepared: Prepared,
}

/// Progress stages shown on the insert line.
pub const STAGES: [&str; 4] = ["Getting details…", "Downloading…", "Reading the model…", "Placing…"];

/// The asset file over the session (asset delivery v2 with the place's
/// context, then the CDN location without the cookie).
pub fn download(h: &dyn Http, id: u64, cancel: &dyn Fn() -> bool) -> Result<Vec<u8>, Failure> {
    let v = api::get_json(h, &format!("https://assetdelivery.roblox.com/v2/assetId/{id}"), cancel);
    let v = match v {
        Ok(v) => v,
        Err(Failure::Forbidden(_)) => {
            return Err(Failure::Forbidden("You don't have permission to use this asset. It may be private or not shared with you.".into()))
        }
        Err(e) => return Err(e),
    };
    let Some(loc) = v.pointer("/locations/0/location").and_then(|l| l.as_str()) else {
        let msg = v.pointer("/errors/0/message").and_then(|m| m.as_str()).unwrap_or("").to_string();
        let code = v.pointer("/errors/0/code").and_then(|c| c.as_u64()).unwrap_or(0);
        return Err(match code {
            403 => Failure::Forbidden(if msg.is_empty() { String::new() } else { msg }),
            404 => Failure::NotFound,
            _ if msg.to_ascii_lowercase().contains("moderat") => Failure::Moderated,
            _ if msg.is_empty() => Failure::Other("Roblox gave no download for this asset.".into()),
            _ => Failure::Other(msg),
        });
    };
    api::get_bytes(h, loc, false, cancel)
}

/// Worker side: details (if missing), the file, the decoded model.
pub fn prepare(h: &dyn Http, job: u64, mut asset: Asset, target: Target, cancel: &dyn Fn() -> bool, stage: &dyn Fn(usize)) -> Result<Ready, Failure> {
    stage(0);
    if asset.type_id == 0 || asset.name.is_empty() {
        if let Some(d) = api::fetch_details(h, &[asset.id], cancel)?.into_iter().next() {
            asset.merge(&d);
        }
    }
    let kind = Kind::from_asset_type(asset.type_id).unwrap_or(Kind::Model);
    let kind = if kind == Kind::Model && asset.sub_types.iter().any(|s| s == "Package") { Kind::Package } else { kind };
    let prepared = match kind {
        Kind::Audio => Prepared::Sound,
        Kind::Video => Prepared::Video,
        Kind::Font => Prepared::Font,
        Kind::Plugin => Prepared::Plugin,
        _ => {
            stage(1);
            // Test seam: RUSTUDIO_TOOLBOX_INSERT_DELAY_MS holds the download
            // (cancellable) so a check can drag out while it is loading.
            if let Some(ms) = std::env::var("RUSTUDIO_TOOLBOX_INSERT_DELAY_MS").ok().and_then(|v| v.parse::<u64>().ok()) {
                let until = std::time::Instant::now() + std::time::Duration::from_millis(ms);
                while std::time::Instant::now() < until {
                    if cancel() {
                        return Err(Failure::Cancelled);
                    }
                    std::thread::sleep(std::time::Duration::from_millis(10));
                }
            }
            let bytes = download(h, asset.id, cancel)?;
            if cancel() {
                return Err(Failure::Cancelled);
            }
            stage(2);
            let dom = rustudio_core::doc::Document::decode(&bytes).map_err(|e| Failure::Other(format!("The asset file can't be read: {e}")))?;
            let scripts = dom
                .descendants()
                .filter(|i| matches!(i.class.as_str(), "Script" | "LocalScript" | "ModuleScript"))
                .count();
            Prepared::Model { dom, scripts }
        }
    };
    Ok(Ready { job, asset, kind, target, prepared })
}

/// Studio's insert waypoint name.
pub fn waypoint(id: u64, kind: Kind) -> String {
    if kind == Kind::Plugin { format!("After attempt to install plugin {id}") } else { format!("After insert asset {id}") }
}

// ------------------------------------------------------------- placement

fn is_part(class: &str) -> bool {
    rustudio_core::doc::is_a(class, "BasePart")
}

/// World AABB of the BaseParts under `roots` in `dom`.
pub fn aabb(dom: &WeakDom, roots: &[Ref]) -> Option<(Vec3, Vec3)> {
    let mut lo = Vec3::splat(f32::INFINITY);
    let mut hi = Vec3::splat(f32::NEG_INFINITY);
    let mut any = false;
    for &r in roots {
        let mut stack = vec![r];
        while let Some(x) = stack.pop() {
            let Some(i) = dom.get_by_ref(x) else { continue };
            stack.extend_from_slice(i.children());
            if !is_part(i.class.as_str()) {
                continue;
            }
            let (Some(Variant::CFrame(cf)), Some(Variant::Vector3(sz))) = (i.properties.get(&"CFrame".into()), i.properties.get(&"Size".into())) else { continue };
            let xf = Xform::from_cframe(cf);
            let half = Vec3::new(sz.x, sz.y, sz.z) * 0.5;
            let ext = xf.rot.x_axis.abs() * half.x + xf.rot.y_axis.abs() * half.y + xf.rot.z_axis.abs() * half.z;
            if !ext.is_finite() || !xf.pos.is_finite() {
                continue;
            }
            lo = lo.min(xf.pos - ext);
            hi = hi.max(xf.pos + ext);
            any = true;
        }
    }
    any.then_some((lo, hi))
}

/// Moves everything under `roots` by `d` (BasePart CFrames, Model
/// WorldPivots; attachments and bones are relative and follow).
pub fn translate(dom: &mut WeakDom, roots: &[Ref], d: Vec3) {
    if d == Vec3::ZERO || !d.is_finite() {
        return;
    }
    let mut stack: Vec<Ref> = roots.to_vec();
    while let Some(x) = stack.pop() {
        let Some(i) = dom.get_by_ref_mut(x) else { continue };
        stack.extend_from_slice(i.children());
        for key in ["CFrame", "WorldPivotData", "WorldPivot"] {
            match i.properties.get_mut(&key.into()) {
                Some(Variant::CFrame(cf)) if key == "CFrame" && is_part(i.class.as_str()) || key != "CFrame" => {
                    cf.position = Vector3::new(cf.position.x + d.x, cf.position.y + d.y, cf.position.z + d.z);
                }
                Some(Variant::OptionalCFrame(Some(cf))) => {
                    cf.position = Vector3::new(cf.position.x + d.x, cf.position.y + d.y, cf.position.z + d.z);
                }
                _ => {}
            }
        }
    }
}

/// Model:MoveTo: the box's centre to `to`, then raised until it overlaps
/// none of `world` (AABBs of the parts already there). Returns the shift.
pub fn move_to(lo: Vec3, hi: Vec3, to: Vec3, world: &[(Vec3, Vec3)]) -> Vec3 {
    let c = (lo + hi) * 0.5;
    let mut d = to - c;
    let half = (hi - lo) * 0.5;
    for _ in 0..64 {
        let (blo, bhi) = (to - half + Vec3::new(0.0, d.y - (to.y - c.y), 0.0), to + half + Vec3::new(0.0, d.y - (to.y - c.y), 0.0));
        let eps = 1e-3;
        let mut top = None::<f32>;
        for (wl, wh) in world {
            let overlap = blo.x < wh.x - eps && bhi.x > wl.x + eps && blo.y < wh.y - eps && bhi.y > wl.y + eps && blo.z < wh.z - eps && bhi.z > wl.z + eps;
            if overlap {
                top = Some(top.map_or(wh.y, |t: f32| t.max(wh.y)));
            }
        }
        match top {
            Some(t) => d.y += t - blo.y,
            None => break,
        }
    }
    d
}

/// Snaps a horizontal position to the Move increment (drag inserts).
pub fn snap(p: Vec3, inc: f32) -> Vec3 {
    if !(inc > 0.0) {
        return p;
    }
    Vec3::new((p.x / inc).round() * inc, p.y, (p.z / inc).round() * inc)
}

/// The face (Enum.NormalId value: Right 0, Top 1, Back 2, Left 3, Bottom 4,
/// Front 5) of a box hit at world point `p`.
pub fn face_of(xf: &Xform, size: Vec3, p: Vec3) -> u32 {
    let l = xf.inverse().apply(p);
    let n = l / (size * 0.5).max(Vec3::splat(1e-4));
    let a = n.abs();
    if a.x >= a.y && a.x >= a.z {
        if n.x > 0.0 { 0 } else { 3 }
    } else if a.y >= a.z {
        if n.y > 0.0 { 1 } else { 4 }
    } else if n.z > 0.0 {
        2
    } else {
        5
    }
}

/// Where a top-level instance of an inserted model goes (placeAssetInCorrectService).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Home {
    World,
    Lighting,
    MaterialService,
    /// Tool / HopperBin: Studio asks.
    AskToolHome,
}

pub fn home_of(class: &str, sub_types: &[String]) -> Home {
    if rustudio_core::doc::is_a(class, "Sky") || rustudio_core::doc::is_a(class, "Atmosphere") || class == "Sky" || class == "Atmosphere" {
        Home::Lighting
    } else if sub_types.iter().any(|s| s == "MaterialPack") {
        Home::MaterialService
    } else if class == "Tool" || class == "HopperBin" {
        Home::AskToolHome
    } else {
        Home::World
    }
}

/// Encodes `refs` of `dom` as binary model bytes (for Document::paste_groups).
pub fn encode(dom: &WeakDom, refs: &[Ref]) -> Vec<u8> {
    let mut out = Vec::new();
    let _ = rbx_binary::to_writer(&mut out, dom, refs);
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use rbx_dom_weak::types::CFrame;
    use rbx_dom_weak::InstanceBuilder;

    fn part(dom: &mut WeakDom, parent: Ref, pos: Vec3, size: Vec3) -> Ref {
        dom.insert(
            parent,
            InstanceBuilder::new("Part")
                .with_property("CFrame", CFrame::new(Vector3::new(pos.x, pos.y, pos.z), rbx_dom_weak::types::Matrix3::identity()))
                .with_property("Size", Vector3::new(size.x, size.y, size.z)),
        )
    }

    #[test]
    fn move_to_sits_on_the_floor() {
        let mut dom = WeakDom::new(InstanceBuilder::new("DataModel"));
        let root = dom.root_ref();
        let m = dom.insert(root, InstanceBuilder::new("Model"));
        part(&mut dom, m, Vec3::new(100.0, 50.0, 0.0), Vec3::new(4.0, 2.0, 4.0));
        part(&mut dom, m, Vec3::new(100.0, 53.0, 0.0), Vec3::new(2.0, 4.0, 2.0));
        let (lo, hi) = aabb(&dom, &[m]).unwrap();
        assert_eq!((lo, hi), (Vec3::new(98.0, 49.0, -2.0), Vec3::new(102.0, 55.0, 2.0)));
        // baseplate top at y = 0, hit at (0, 0, 0)
        let base = [(Vec3::new(-256.0, -8.0, -256.0), Vec3::new(256.0, 0.0, 256.0))];
        let d = move_to(lo, hi, Vec3::ZERO, &base);
        translate(&mut dom, &[m], d);
        let (lo2, hi2) = aabb(&dom, &[m]).unwrap();
        assert!((lo2.y - 0.0).abs() < 1e-4, "{lo2:?}");
        assert!((lo2.x + hi2.x).abs() < 1e-4 && (lo2.z + hi2.z).abs() < 1e-4);
        // nothing under it: centre at the point
        let d = move_to(lo2, hi2, Vec3::new(5.0, 10.0, 5.0), &[]);
        assert_eq!(((lo2 + hi2) * 0.5 + d), Vec3::new(5.0, 10.0, 5.0));
    }

    #[test]
    fn faces_and_homes() {
        let xf = Xform { pos: Vec3::ZERO, rot: glam::Mat3::IDENTITY };
        let s = Vec3::new(4.0, 2.0, 6.0);
        assert_eq!(face_of(&xf, s, Vec3::new(0.0, 1.0, 0.0)), 1);
        assert_eq!(face_of(&xf, s, Vec3::new(-2.0, 0.0, 0.0)), 3);
        assert_eq!(face_of(&xf, s, Vec3::new(0.0, 0.0, -3.0)), 5);
        assert_eq!(home_of("Sky", &[]), Home::Lighting);
        assert_eq!(home_of("Tool", &[]), Home::AskToolHome);
        assert_eq!(home_of("Model", &["MaterialPack".into()]), Home::MaterialService);
        assert_eq!(home_of("Model", &[]), Home::World);
        assert_eq!(snap(Vec3::new(1.4, 2.2, -0.6), 1.0), Vec3::new(1.0, 2.2, -1.0));
        assert_eq!(waypoint(5, Kind::Model), "After insert asset 5");
    }
}
