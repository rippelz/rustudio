//! Studio's new-place templates: the list the Start Page shows, the place
//! files behind them, and built-in copies of the three blank ones.
//!
//! Studio's Start Page (BuiltInStandalonePlugins/StartPage) lists templates
//! from develop.roblox.com `v1/gametemplates` (TemplatesPage /
//! DiscoverExperiences, `isTemplates`), in the order Roblox returns them,
//! names them by their universe and shows each universe's game icon
//! (thumbnails `v1/games/icons`). Clicking one opens its root place: the
//! place file, downloaded like any other place. Both lists are public; the
//! place download needs the login session.
//!
//! rustudio does the same: the list and icons come from Roblox when it can
//! reach it (else the list measured on 2026-11-11, below); a template's
//! place is downloaded with the session and kept in
//! `~/.cache/rustudio/templates`, else read from that cache or the place
//! cache, else, for Baseplate, Classic Baseplate and Flat Terrain, built
//! from `builtin.rs`: Studio's DataModel after opening each template,
//! property for property.

use std::collections::HashMap;
use std::path::PathBuf;

use rbx_dom_weak::types::{
    Attributes, BinaryString, CFrame, Color3, Color3uint8, ColorSequence, ColorSequenceKeypoint, Content, ContentId,
    Enum, Font, FontStyle, FontWeight, Matrix3, NumberRange, NumberSequence, NumberSequenceKeypoint, PhysicalProperties, Rect, Ref,
    SharedString, Tags, UDim, UDim2, Variant, Vector2, Vector3,
};
use rbx_dom_weak::{InstanceBuilder, WeakDom};

use rustudio_core::doc::Document;

/// Studio's template tabs (TemplatesPage: All, Blank, Gameplay, Themed).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Kind {
    Generic,
    Gameplay,
    Theme,
}

impl Kind {
    pub fn parse(s: &str) -> Option<Kind> {
        match s {
            "Generic" => Some(Kind::Generic),
            "Gameplay" => Some(Kind::Gameplay),
            "Theme" => Some(Kind::Theme),
            _ => None,
        }
    }

    /// The tab's name on Studio's Templates page (TemplatesPage.Generic …).
    pub fn label(self) -> &'static str {
        match self {
            Kind::Generic => "Blank",
            Kind::Gameplay => "Gameplay",
            Kind::Theme => "Themed",
        }
    }
}

/// One template as Studio lists it.
#[derive(Debug, Clone, PartialEq)]
pub struct TemplateInfo {
    pub universe_id: u64,
    /// The root place: what opening the template opens.
    pub place_id: u64,
    pub name: String,
    pub kind: Kind,
}

/// The place Studio's "New Experience" button opens
/// (FStringStudioDefaultNewTemplatePlaceId, 95206881: Baseplate).
pub const DEFAULT_PLACE_ID: u64 = 95206881;
pub const CLASSIC_BASEPLATE_PLACE_ID: u64 = 6560363541;
pub const FLAT_TERRAIN_PLACE_ID: u64 = 95206192;

/// How many templates Studio's Home page shows before "See All"
/// (FIntLuaStartPageHomeCarouselMaxCount: 15 in Studio's live flags).
pub const HOME_MAX: usize = 15;

/// develop `v1/gametemplates` as Roblox answered it on 2026-11-11, in its
/// order: (universe, root place, name, type).
pub const MEASURED: [(u64, u64, &str, Kind); 16] = [
    (28220420, 95206881, "Baseplate", Kind::Generic),
    (2464612126, 6560363541, "Classic Baseplate", Kind::Generic),
    (28223770, 95206192, "Flat Terrain", Kind::Generic),
    (6314775459, 18636963376, "Platformer", Kind::Gameplay),
    (6106389365, 17823019220, "Laser Tag", Kind::Gameplay),
    (10774682086, 84442400172515, "Boneheads", Kind::Gameplay),
    (6680068955, 110446511895848, "UGC Homestore", Kind::Theme),
    (6106388692, 17823017290, "FPS System", Kind::Gameplay),
    (5557500170, 16078915506, "Racing", Kind::Gameplay),
    (93412282, 203812057, "Classic Obby", Kind::Gameplay),
    (202770430, 520390648, "Village", Kind::Theme),
    (93411794, 203810088, "Castle", Kind::Theme),
    (138962641, 366130569, "Suburban", Kind::Theme),
    (107387509, 264719325, "Pirate Island", Kind::Theme),
    (142606178, 379736082, "Starting Place", Kind::Theme),
    (4594822820, 13165709401, "Modern City", Kind::Theme),
];

pub fn measured() -> Vec<TemplateInfo> {
    MEASURED
        .iter()
        .map(|&(universe_id, place_id, name, kind)| TemplateInfo { universe_id, place_id, name: name.into(), kind })
        .collect()
}

/// The list from a `v1/gametemplates` reply (`data[].gameTemplateType`,
/// `data[].universe.{id, rootPlaceId, name}`), in its order.
pub fn parse_list(text: &str) -> Result<Vec<TemplateInfo>, String> {
    let v: serde_json::Value = serde_json::from_str(text).map_err(|e| format!("gametemplates: {e}"))?;
    let rows = v.get("data").and_then(|d| d.as_array()).ok_or("gametemplates: no data")?;
    let out: Vec<TemplateInfo> = rows
        .iter()
        .filter_map(|r| {
            let u = r.get("universe")?;
            Some(TemplateInfo {
                universe_id: u.get("id")?.as_u64()?,
                place_id: u.get("rootPlaceId")?.as_u64()?,
                name: u.get("name")?.as_str()?.to_string(),
                kind: Kind::parse(r.get("gameTemplateType")?.as_str()?).unwrap_or(Kind::Theme),
            })
        })
        .collect();
    if out.is_empty() {
        return Err("gametemplates: empty list".into());
    }
    Ok(out)
}

fn get(agent: &ureq::Agent, url: &str) -> Result<Vec<u8>, String> {
    use std::io::Read;
    let mut r = agent.get(url).call().map_err(|e| e.to_string())?;
    let status = r.status().as_u16();
    let mut bytes = Vec::new();
    r.body_mut()
        .with_config()
        .limit(16 << 20)
        .reader()
        .read_to_end(&mut bytes)
        .map_err(|e| e.to_string())?;
    if status != 200 {
        return Err(format!("{url}: HTTP {status}"));
    }
    Ok(bytes)
}

/// The template list from Roblox (public; no sign-in needed).
pub fn fetch_list(develop: &str) -> Result<Vec<TemplateInfo>, String> {
    let agent = rustudio_account::session::session_agent();
    let bytes = get(&agent, &format!("{develop}/v1/gametemplates"))?;
    parse_list(&String::from_utf8_lossy(&bytes))
}

/// Each universe's game icon (PNG bytes), as Studio's tiles show them.
pub fn fetch_icons(thumbnails: &str, universes: &[u64]) -> Vec<(u64, Vec<u8>)> {
    let agent = rustudio_account::session::session_agent();
    let mut out = Vec::new();
    for chunk in universes.chunks(50) {
        let ids: Vec<String> = chunk.iter().map(|i| i.to_string()).collect();
        let url = format!(
            "{thumbnails}/v1/games/icons?universeIds={}&size=256x256&format=Png&isCircular=false",
            ids.join(",")
        );
        let Ok(bytes) = get(&agent, &url) else { continue };
        let Ok(v) = serde_json::from_slice::<serde_json::Value>(&bytes) else { continue };
        for t in v.get("data").and_then(|d| d.as_array()).into_iter().flatten() {
            let (Some(id), Some(url)) = (t.get("targetId").and_then(|x| x.as_u64()), t.get("imageUrl").and_then(|x| x.as_str())) else {
                continue;
            };
            if t.get("state").and_then(|s| s.as_str()) != Some("Completed") {
                continue;
            }
            if let Ok(png) = get(&agent, url) {
                out.push((id, png));
            }
        }
    }
    out
}

// ------------------------------------------------------------- place files

/// Where a template's place came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Source {
    Downloaded(u64),
    Cached(u64),
    BuiltIn,
}

/// `~/.cache/rustudio/templates/<placeId>/v<N>.rbxl` (RUSTUDIO_CACHE_DIR
/// moves it with the place cache).
pub fn cache_dir() -> PathBuf {
    let places = rustudio_account::cloud::PlaceCache::default().root;
    places.parent().map(|p| p.join("templates")).unwrap_or_else(|| PathBuf::from("templates"))
}

fn newest(dir: &std::path::Path) -> Option<(u64, PathBuf)> {
    std::fs::read_dir(dir)
        .ok()?
        .flatten()
        .filter_map(|e| {
            let p = e.path();
            let n = p.file_name()?.to_str()?.to_string();
            let v: u64 = n.strip_suffix(".rbxl").or_else(|| n.strip_suffix(".rbxlx"))?.strip_prefix('v')?.parse().ok()?;
            Some((v, p))
        })
        .max_by_key(|(v, _)| *v)
}

/// A cached copy of the template's place: rustudio's template cache, else
/// the place cache (a template opened by its place ID).
pub fn cached(place_id: u64) -> Option<(u64, Vec<u8>)> {
    let dirs = [
        cache_dir().join(place_id.to_string()),
        rustudio_account::cloud::PlaceCache::default().dir(place_id),
    ];
    dirs.iter().find_map(|d| {
        let (v, p) = newest(d)?;
        Some((v, std::fs::read(p).ok()?))
    })
}

fn store(place_id: u64, version: u64, bytes: &[u8]) {
    let dir = cache_dir().join(place_id.to_string());
    if std::fs::create_dir_all(&dir).is_err() {
        return;
    }
    let xml = bytes.starts_with(b"<roblox") && !bytes.starts_with(b"<roblox!");
    if std::fs::write(dir.join(format!("v{version}.{}", if xml { "rbxlx" } else { "rbxl" })), bytes).is_ok() {
        // Only the newest is kept.
        for e in std::fs::read_dir(&dir).into_iter().flatten().flatten() {
            let n = e.file_name().to_string_lossy().into_owned();
            if n.starts_with('v') && !n.starts_with(&format!("v{version}.")) {
                let _ = std::fs::remove_file(e.path());
            }
        }
    }
}

/// The template's place file: the newest version from Roblox (unless the
/// cache holds it already), else a cached copy. Err says why neither was
/// possible.
pub fn fetch_place(api: Option<&rustudio_account::CloudApi>, place_id: u64) -> Result<(Vec<u8>, Source), String> {
    let cache = cached(place_id);
    let why = match api {
        None => "not signed in to Roblox".to_string(),
        Some(api) => {
            let have = cache.as_ref().map(|(v, _)| *v);
            match api.latest_version(place_id) {
                Ok(latest) if Some(latest) == have => {
                    return Ok((cache.unwrap().1, Source::Cached(latest)));
                }
                Ok(_) => match api.download(place_id, None) {
                    Ok((bytes, v)) => {
                        store(place_id, v, &bytes);
                        return Ok((bytes, Source::Downloaded(v)));
                    }
                    Err(e) => e.0,
                },
                Err(e) => e.0,
            }
        }
    };
    match cache {
        Some((v, bytes)) => Ok((bytes, Source::Cached(v))),
        None => Err(why),
    }
}

// ---------------------------------------------------------- built-in places

/// A property value in the built-in tables (`builtin.rs`).
#[derive(Debug, Clone, Copy)]
pub enum V {
    B(bool),
    F(f32),
    D(f64),
    I(i32),
    L(i64),
    S(&'static str),
    E(u32),
    C3(f32, f32, f32),
    C8(u8, u8, u8),
    V3(f32, f32, f32),
    V2(f32, f32),
    /// Position, then the rotation matrix row by row.
    Cf([f32; 12]),
    /// An OptionalCFrame that is set.
    OCf([f32; 12]),
    NoCf,
    Id(&'static str),
    Uri(&'static str),
    NoContent,
    Bin(&'static [u8]),
    Shared(&'static [u8]),
    Ud(f32, i32),
    Ud2(f32, i32, f32, i32),
    NSeq(&'static [[f32; 3]]),
    CSeq(&'static [[f32; 4]]),
    Font(&'static str, u16, u8),
    Rect(f32, f32, f32, f32),
    Range(f32, f32),
    DefaultPhysics,
    /// The instance at this path.
    Ref(&'static str),
    NoRef,
    Attrs(&'static [(&'static str, V)]),
    Tags(&'static [&'static str]),
    /// The class default (a patch takes the property away).
    Default,
}

/// One instance: its path of names from the DataModel (a name siblings
/// share is written Name@Class), class, and the properties its class
/// defaults don't give.
pub struct Inst {
    pub path: &'static str,
    pub class: &'static str,
    pub props: &'static [(&'static str, V)],
}

/// Another template as changes to Baseplate's table.
pub struct Patch {
    /// Subtrees it doesn't have.
    pub remove: &'static [&'static str],
    pub add: &'static [Inst],
    /// (path, property, value).
    pub set: &'static [(&'static str, &'static str, V)],
}

fn cframe(m: &[f32; 12]) -> CFrame {
    CFrame::new(
        Vector3::new(m[0], m[1], m[2]),
        Matrix3::new(Vector3::new(m[3], m[4], m[5]), Vector3::new(m[6], m[7], m[8]), Vector3::new(m[9], m[10], m[11])),
    )
}

/// The value as a Variant (None: leave the property to the class default).
fn variant(v: &V, refs: &HashMap<&str, Ref>) -> Option<Variant> {
    Some(match *v {
        V::B(b) => Variant::Bool(b),
        V::F(x) => Variant::Float32(x),
        V::D(x) => Variant::Float64(x),
        V::I(x) => Variant::Int32(x),
        V::L(x) => Variant::Int64(x),
        V::S(s) => Variant::String(s.into()),
        V::E(e) => Variant::Enum(Enum::from_u32(e)),
        V::C3(r, g, b) => Variant::Color3(Color3::new(r, g, b)),
        V::C8(r, g, b) => Variant::Color3uint8(Color3uint8::new(r, g, b)),
        V::V3(x, y, z) => Variant::Vector3(Vector3::new(x, y, z)),
        V::V2(x, y) => Variant::Vector2(Vector2::new(x, y)),
        V::Cf(ref m) => Variant::CFrame(cframe(m)),
        V::OCf(ref m) => Variant::OptionalCFrame(Some(cframe(m))),
        V::NoCf => Variant::OptionalCFrame(None),
        V::Id(s) => Variant::ContentId(ContentId::from(s)),
        V::Uri(s) => Variant::Content(Content::from_uri(s)),
        V::NoContent => Variant::Content(Content::default()),
        V::Bin(b) => Variant::BinaryString(BinaryString::from(b.to_vec())),
        V::Shared(b) => Variant::SharedString(SharedString::new(b.to_vec())),
        V::Ud(s, o) => Variant::UDim(UDim::new(s, o)),
        V::Ud2(xs, xo, ys, yo) => Variant::UDim2(UDim2::new(UDim::new(xs, xo), UDim::new(ys, yo))),
        V::NSeq(k) => Variant::NumberSequence(NumberSequence {
            keypoints: k.iter().map(|k| NumberSequenceKeypoint::new(k[0], k[1], k[2])).collect(),
        }),
        V::CSeq(k) => Variant::ColorSequence(ColorSequence {
            keypoints: k.iter().map(|k| ColorSequenceKeypoint::new(k[0], Color3::new(k[1], k[2], k[3]))).collect(),
        }),
        V::Font(family, weight, style) => Variant::Font(Font::new(
            family,
            FontWeight::from_u16(weight).unwrap_or_default(),
            FontStyle::from_u8(style).unwrap_or_default(),
        )),
        V::Rect(a, b, c, d) => Variant::Rect(Rect::new(Vector2::new(a, b), Vector2::new(c, d))),
        V::Range(a, b) => Variant::NumberRange(NumberRange::new(a, b)),
        V::DefaultPhysics => Variant::PhysicalProperties(PhysicalProperties::Default),
        V::Ref(p) => Variant::Ref(refs.get(p).copied().unwrap_or_else(Ref::none)),
        V::NoRef => Variant::Ref(Ref::none()),
        V::Attrs(items) => {
            let mut a = Attributes::default();
            for (k, v) in items {
                if let Some(v) = variant(v, refs) {
                    a.insert((*k).to_string(), v);
                }
            }
            Variant::Attributes(a)
        }
        V::Tags(t) => Variant::Tags(Tags::from(t.iter().map(|s| s.to_string()).collect::<Vec<_>>())),
        V::Default => return None,
    })
}

/// The DataModel a table (plus a patch) describes.
pub fn build_dom(base: &[Inst], patch: Option<&Patch>) -> WeakDom {
    let gone = |p: &str| {
        patch.is_some_and(|pt| pt.remove.iter().any(|r| p == *r || p.starts_with(&format!("{r}/"))))
    };
    let insts: Vec<&Inst> = base
        .iter()
        .filter(|i| !gone(i.path))
        .chain(patch.map(|p| p.add.iter()).into_iter().flatten())
        .collect();
    let mut dom = WeakDom::new(InstanceBuilder::new("DataModel").with_name("Game"));
    let mut refs: HashMap<&str, Ref> = HashMap::new();
    for i in &insts {
        let (parent, step) = match i.path.rsplit_once('/') {
            Some((p, n)) => (refs.get(p).copied().unwrap_or_else(|| dom.root_ref()), n),
            None => (dom.root_ref(), i.path),
        };
        // Name@Class: a name its siblings share.
        let name = step.split_once('@').map_or(step, |(n, _)| n);
        let r = dom.insert(parent, InstanceBuilder::new(i.class).with_name(name));
        refs.insert(i.path, r);
    }
    let mut props: Vec<(&str, &str, &V)> = insts.iter().flat_map(|i| i.props.iter().map(move |(k, v)| (i.path, *k, v))).collect();
    if let Some(p) = patch {
        props.extend(p.set.iter().map(|(path, k, v)| (*path, *k, v)));
    }
    for (path, k, v) in props {
        let Some(&r) = refs.get(path) else { continue };
        let value = variant(v, &refs);
        let Some(inst) = dom.get_by_ref_mut(r) else { continue };
        let key = rbx_dom_weak::ustr(k);
        match value {
            Some(x) => {
                if k == "Name" {
                    if let Variant::String(n) = &x {
                        inst.name = n.clone();
                    }
                }
                inst.properties.insert(key, x);
            }
            None => {
                inst.properties.remove(&key);
            }
        }
    }
    dom
}

/// Flat Terrain's ground as Studio has it: solid Grass, voxels x and z
/// −256…255, y −14…1 (2048 × 64 × 2048 studs, top at y = 8).
pub const FLAT_TERRAIN_VOXELS: ([i32; 3], [i32; 3]) = ([-256, -14, -256], [255, 1, 255]);

fn flat_terrain_grid() -> Vec<u8> {
    let mut v = rustudio_core::terrain::Voxels::empty();
    let grass = rustudio_core::terrain::material_index("Grass").unwrap_or(2);
    let (lo, hi) = FLAT_TERRAIN_VOXELS;
    let cells = (lo[0]..=hi[0]).flat_map(move |x| {
        (lo[1]..=hi[1]).flat_map(move |y| (lo[2]..=hi[2]).map(move |z| ([x, y, z], (grass, 255u8, 0u16))))
    });
    v.set_raw_many(cells);
    v.encode()
}

/// A built-in copy of one of Studio's blank templates, by root place ID.
pub fn builtin(place_id: u64) -> Option<WeakDom> {
    use super::builtin::{BASEPLATE, CLASSIC_BASEPLATE, FLAT_TERRAIN};
    Some(match place_id {
        DEFAULT_PLACE_ID => build_dom(BASEPLATE, None),
        CLASSIC_BASEPLATE_PLACE_ID => build_dom(BASEPLATE, Some(&CLASSIC_BASEPLATE)),
        FLAT_TERRAIN_PLACE_ID => {
            let mut dom = build_dom(BASEPLATE, Some(&FLAT_TERRAIN));
            let terrain = dom
                .descendants()
                .find(|i| i.class == "Terrain")
                .map(|i| i.referent());
            if let Some(i) = terrain.and_then(|t| dom.get_by_ref_mut(t)) {
                i.properties
                    .insert(rbx_dom_weak::ustr("SmoothGrid"), Variant::BinaryString(BinaryString::from(flat_terrain_grid())));
            }
            dom
        }
        _ => return None,
    })
}

const MIGRATED: &str = "RBX_LightingTechnologyUnifiedMigration";
const ORIGINAL_TECHNOLOGY: &str = "RBX_OriginalTechnologyOnFileLoad";

/// What Studio does to Lighting when it opens a place file (seen in all 16
/// templates): one not yet marked RBX_LightingTechnologyUnifiedMigration
/// takes its LightingStyle from Technology (Baseplate, Classic Baseplate
/// and Flat Terrain save ShadowMap with LightingStyle Realistic and open
/// Soft) and gets the mark; RBX_OriginalTechnologyOnFileLoad records the
/// Technology it was opened with.
pub fn migrate_lighting(doc: &mut Document) {
    let Some(l) = doc.find_child(doc.dom.root_ref(), "Lighting") else { return };
    let tech = rustudio_render::scene::enum_u32(doc.prop(l, "Technology")).unwrap_or(rustudio_core::mech::TECHNOLOGY_SOFT[1]);
    let Some(i) = doc.dom.get_by_ref_mut(l) else { return };
    let key = rbx_dom_weak::ustr("Attributes");
    let mut attrs = match i.properties.get(&key) {
        Some(Variant::Attributes(a)) => a.clone(),
        _ => Attributes::default(),
    };
    if attrs.get(MIGRATED).is_none() {
        let soft = rustudio_core::mech::TECHNOLOGY_SOFT.contains(&tech);
        i.properties.insert(rbx_dom_weak::ustr("LightingStyle"), Variant::Enum(Enum::from_u32(u32::from(soft))));
        attrs.insert(MIGRATED.into(), Variant::Bool(true));
    }
    if attrs.get(ORIGINAL_TECHNOLOGY).is_none() {
        attrs.insert(ORIGINAL_TECHNOLOGY.into(), Variant::Int32(tech as i32));
    }
    i.properties.insert(key, Variant::Attributes(attrs));
}

/// Studio's default new place (File > New, Ctrl+N, New Experience): the
/// Baseplate template, built in, so it opens at once and offline.
pub fn baseplate() -> Document {
    let info = measured().into_iter().find(|t| t.place_id == DEFAULT_PLACE_ID).expect("Baseplate is measured");
    new_place(&info, None).expect("Baseplate is built in")
}

/// The new place for a template: its place file when there is one, else
/// the built-in copy. Unsaved and named after the template, as Studio
/// opens it.
pub fn new_place(info: &TemplateInfo, file: Option<&[u8]>) -> Result<Document, String> {
    let doc = match file {
        Some(bytes) => {
            let mut d = Document::from_place_bytes(bytes, info.name.clone())?;
            migrate_lighting(&mut d);
            d
        }
        None => match builtin(info.place_id) {
            Some(dom) => Document::from_template_dom(dom, info.name.clone()),
            None => return Err(format!("the {} template isn't built in", info.name)),
        },
    };
    Ok(doc)
}
