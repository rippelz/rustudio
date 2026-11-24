//! Compares a place rustudio builds with a DataModel Studio saved, property
//! by property (the template checks; RUSTUDIO_TEMPLATE_DUMP runs it on a
//! Studio dump: see `studio_template_diff`).

use std::collections::HashMap;

use rbx_dom_weak::types::{Ref, Variant};

use rustudio_core::doc::Document;

/// Per-instance ids Studio makes fresh on every load or save.
const VOLATILE: &[&str] = &["UniqueId", "HistoryId", "SourceAssetId", "Capabilities", "DefinesCapabilities"];

fn f(x: f64) -> String {
    let s = format!("{:.4}", x);
    let s = s.trim_end_matches('0').trim_end_matches('.').to_string();
    if s == "-0" { "0".into() } else { s }
}

/// A comparable spelling of a value: floats to 4 places, both colour types
/// as 0–1 floats, both enum types as their number, both content types as
/// their URI.
pub fn norm(doc: &Document, v: &Variant) -> String {
    match v {
        Variant::Float32(x) => f(*x as f64),
        Variant::Float64(x) => f(*x),
        Variant::Int32(x) => x.to_string(),
        Variant::Int64(x) => x.to_string(),
        Variant::Bool(b) => b.to_string(),
        Variant::String(s) => format!("{s:?}"),
        Variant::Enum(e) => format!("enum {}", e.to_u32()),
        Variant::EnumItem(e) => format!("enum {}", e.value),
        Variant::Color3(c) => format!("rgb {} {} {}", f((c.r as f64 * 255.0).round()), f((c.g as f64 * 255.0).round()), f((c.b as f64 * 255.0).round())),
        Variant::Color3uint8(c) => format!("rgb {} {} {}", c.r, c.g, c.b),
        Variant::Vector3(p) => format!("v3 {} {} {}", f(p.x as f64), f(p.y as f64), f(p.z as f64)),
        Variant::Vector2(p) => format!("v2 {} {}", f(p.x as f64), f(p.y as f64)),
        Variant::CFrame(c) => {
            let o = c.orientation;
            let m = [o.x.x, o.x.y, o.x.z, o.y.x, o.y.y, o.y.z, o.z.x, o.z.y, o.z.z];
            format!(
                "cf {} {} {} | {}",
                f(c.position.x as f64),
                f(c.position.y as f64),
                f(c.position.z as f64),
                m.iter().map(|x| f(*x as f64)).collect::<Vec<_>>().join(" ")
            )
        }
        Variant::OptionalCFrame(None) => "cf none".into(),
        Variant::OptionalCFrame(Some(c)) => norm(doc, &Variant::CFrame(*c)),
        Variant::ContentId(c) => format!("content {:?}", c.as_str()),
        Variant::Content(c) => match c.as_uri() {
            Some(u) => format!("content {u:?}"),
            None if c.as_object().is_some() => "content object".into(),
            None => "content \"\"".into(),
        },
        Variant::BinaryString(b) => {
            let b: &[u8] = b.as_ref();
            format!("bytes {} {:016x}", b.len(), fnv(b))
        }
        Variant::SharedString(s) => format!("shared {} {:016x}", s.data().len(), fnv(s.data())),
        Variant::Ref(r) => match doc.dom.get_by_ref(*r) {
            Some(i) => format!("ref {}", i.name),
            None => "ref null".into(),
        },
        Variant::Tags(t) => format!("tags {:?}", t.iter().collect::<Vec<_>>()),
        Variant::Attributes(a) => format!("attrs {}", a.len()),
        Variant::UDim(u) => format!("udim {} {}", f(u.scale as f64), u.offset),
        Variant::UDim2(u) => format!("udim2 {} {} {} {}", f(u.x.scale as f64), u.x.offset, f(u.y.scale as f64), u.y.offset),
        Variant::NumberSequence(s) => format!(
            "nseq {}",
            s.keypoints.iter().map(|k| format!("{} {} {}", f(k.time as f64), f(k.value as f64), f(k.envelope as f64))).collect::<Vec<_>>().join(", ")
        ),
        Variant::ColorSequence(s) => format!(
            "cseq {}",
            s.keypoints
                .iter()
                .map(|k| format!("{} {} {} {}", f(k.time as f64), f(k.color.r as f64), f(k.color.g as f64), f(k.color.b as f64)))
                .collect::<Vec<_>>()
                .join(", ")
        ),
        Variant::PhysicalProperties(p) => format!("{p:?}"),
        Variant::Font(ft) => format!("font {:?} {:?} {:?}", ft.family, ft.weight, ft.style),
        Variant::Rect(r) => format!("rect {} {} {} {}", f(r.min.x as f64), f(r.min.y as f64), f(r.max.x as f64), f(r.max.y as f64)),
        Variant::MaterialColors(m) => {
            let b = m.encode();
            format!("bytes {} {:016x}", b.len(), fnv(&b))
        }
        other => format!("{other:?}"),
    }
}

fn fnv(b: &[u8]) -> u64 {
    b.iter().fold(0xcbf29ce484222325u64, |h, x| (h ^ *x as u64).wrapping_mul(0x100000001b3))
}

/// Every instance under `root` by its path of `Class:Name` steps (a
/// repeated step gets `#n`).
pub fn paths(doc: &Document) -> Vec<(String, Ref)> {
    let mut out = Vec::new();
    fn walk(doc: &Document, r: Ref, path: &str, out: &mut Vec<(String, Ref)>) {
        let Some(i) = doc.dom.get_by_ref(r) else { return };
        let mut seen: HashMap<String, usize> = HashMap::new();
        for c in i.children() {
            let Some(ci) = doc.dom.get_by_ref(*c) else { continue };
            let step = format!("{}:{}", ci.class, ci.name);
            let n = seen.entry(step.clone()).or_default();
            let step = if *n == 0 { step } else { format!("{step}#{n}") };
            *n += 1;
            let p = if path.is_empty() { step } else { format!("{path}/{step}") };
            out.push((p.clone(), *c));
            walk(doc, *c, &p, out);
        }
    }
    walk(doc, doc.dom.root_ref(), "", &mut out);
    out
}

/// One property that differs: (path, property, Studio's value, ours).
pub type Diff = (String, String, String, String);

/// What differs between Studio's DataModel and ours, for the instances
/// under the paths `only` starts (all when empty): instances one side lacks
/// (property "<instance>") and properties whose values differ. Also counts
/// the properties compared.
pub fn diff(studio: &Document, ours: &Document, only: &[&str]) -> (Vec<Diff>, usize) {
    let keep = |p: &str| only.is_empty() || only.iter().any(|o| p == *o || p.starts_with(&format!("{o}/")));
    let sp = paths(studio);
    let op: HashMap<String, Ref> = paths(ours).into_iter().collect();
    let sset: HashMap<&str, Ref> = sp.iter().map(|(p, r)| (p.as_str(), *r)).collect();
    let mut out = Vec::new();
    let mut compared = 0;
    for (p, sr) in &sp {
        if !keep(p) {
            continue;
        }
        let Some(&or) = op.get(p) else {
            out.push((p.clone(), "<instance>".into(), "present".into(), "missing".into()));
            continue;
        };
        let si = studio.dom.get_by_ref(*sr).unwrap();
        let mut names: Vec<_> = si.properties.keys().map(|k| k.to_string()).collect();
        names.sort();
        for name in names {
            if VOLATILE.contains(&name.as_str()) {
                continue;
            }
            let svar = &si.properties[&rbx_dom_weak::ustr(&name)];
            let ovar = ours.prop(or, &name);
            compared += 1;
            if name == "SmoothGrid" {
                if let Some(d) = voxel_diff(svar, ovar.as_ref()) {
                    out.push((p.clone(), name, "voxels".into(), d));
                }
                continue;
            }
            let sv = norm(studio, svar);
            let ov = ovar.map(|v| norm(ours, &v)).unwrap_or_else(|| "<none>".into());
            if !same(&sv, &ov) {
                out.push((p.clone(), name, sv, ov));
            }
        }
    }
    let mut extra: Vec<_> = op.keys().filter(|p| keep(p) && !sset.contains_key(p.as_str())).cloned().collect();
    extra.sort();
    for p in extra {
        out.push((p, "<instance>".into(), "missing".into(), "present".into()));
    }
    (out, compared)
}

/// Values that are the same thing written two ways: an unset reference or
/// empty blob, tag list or attribute set and a missing property.
fn same(a: &str, b: &str) -> bool {
    let empty = |x: &str| matches!(x, "<none>" | "ref null" | "tags []" | "attrs 0") || x.starts_with("bytes 0 ");
    a == b || (empty(a) && empty(b))
}

/// Terrain voxels compared cell by cell (material, occupancy byte, water):
/// None when they agree, else a summary of where they don't.
fn voxel_diff(studio: &Variant, ours: Option<&Variant>) -> Option<String> {
    use rustudio_core::terrain::Voxels;
    let bytes = |v: Option<&Variant>| match v {
        Some(Variant::BinaryString(b)) => Some(AsRef::<[u8]>::as_ref(b).to_vec()),
        _ => None,
    };
    let a = Voxels::decode(&bytes(Some(studio)).unwrap_or_default()).ok()?;
    let b = Voxels::decode(&bytes(ours).unwrap_or_default()).unwrap_or_else(|_| Voxels::empty());
    let (ab, bb) = (a.bounds(), b.bounds());
    let lo_hi = match (ab, bb) {
        (None, None) => return None,
        (Some(x), None) | (None, Some(x)) => x,
        (Some(x), Some(y)) => ([0, 1, 2].map(|i| x.0[i].min(y.0[i])), [0, 1, 2].map(|i| x.1[i].max(y.1[i]))),
    };
    let (mut bad, mut first) = (0usize, None);
    for x in lo_hi.0[0]..=lo_hi.1[0] {
        for y in lo_hi.0[1]..=lo_hi.1[1] {
            for z in lo_hi.0[2]..=lo_hi.1[2] {
                let (p, q) = (a.get_raw([x, y, z]), b.get_raw([x, y, z]));
                if p != q {
                    bad += 1;
                    first.get_or_insert(([x, y, z], p, q));
                }
            }
        }
    }
    (bad > 0).then(|| {
        format!(
            "{bad} cells differ (studio {} cells {:?}, ours {} cells {:?}; first {:?})",
            a.count(),
            ab,
            b.count(),
            bb,
            first
        )
    })
}

/// Per layer of a terrain blob: each (material, occupancy byte, water)
/// and how many cells have it (for working out how a template's terrain
/// was made).
pub fn voxel_layers(bytes: &[u8]) -> Vec<(i32, Vec<((u8, u8, u16), usize)>)> {
    let v = rustudio_core::terrain::Voxels::decode(bytes).unwrap();
    let Some((lo, hi)) = v.bounds() else { return Vec::new() };
    (lo[1]..=hi[1])
        .map(|y| {
            let mut m: HashMap<(u8, u8, u16), usize> = HashMap::new();
            for x in lo[0]..=hi[0] {
                for z in lo[2]..=hi[2] {
                    *m.entry(v.get_raw([x, y, z])).or_default() += 1;
                }
            }
            let mut m: Vec<_> = m.into_iter().collect();
            m.sort();
            (y, m)
        })
        .collect()
}

// ------------------------------------------------- built-in table generator

/// The Rust spelling of a value for `templates::V`, or None for one the
/// tables don't need (UniqueId and the like).
fn lit(v: &Variant, path_of: &dyn Fn(Ref) -> Option<String>) -> Option<String> {
    let fl = |x: f32| {
        let s = format!("{x:?}");
        if s.contains('.') || s.contains('e') || s.contains("inf") || s.contains("NaN") { s } else { format!("{s}.0") }
    };
    let bytes = |b: &[u8]| {
        let mut s = String::from("b\"");
        for c in b {
            match *c {
                b'"' => s.push_str("\\\""),
                b'\\' => s.push_str("\\\\"),
                0x20..=0x7e => s.push(*c as char),
                _ => s.push_str(&format!("\\x{c:02x}")),
            }
        }
        s.push('"');
        s
    };
    let cf = |c: &rbx_dom_weak::types::CFrame| {
        let o = c.orientation;
        let m = [c.position.x, c.position.y, c.position.z, o.x.x, o.x.y, o.x.z, o.y.x, o.y.y, o.y.z, o.z.x, o.z.y, o.z.z];
        format!("[{}]", m.iter().map(|x| fl(*x)).collect::<Vec<_>>().join(", "))
    };
    Some(match v {
        Variant::Bool(b) => format!("B({b})"),
        Variant::Float32(x) => format!("F({})", fl(*x)),
        Variant::Float64(x) => format!("D({x:?})"),
        Variant::Int32(x) => format!("I({x})"),
        Variant::Int64(x) => format!("L({x})"),
        Variant::String(s) => format!("S({s:?})"),
        Variant::Enum(e) => format!("E({})", e.to_u32()),
        Variant::Color3(c) => format!("C3({}, {}, {})", fl(c.r), fl(c.g), fl(c.b)),
        Variant::Color3uint8(c) => format!("C8({}, {}, {})", c.r, c.g, c.b),
        Variant::Vector3(p) => format!("V3({}, {}, {})", fl(p.x), fl(p.y), fl(p.z)),
        Variant::Vector2(p) => format!("V2({}, {})", fl(p.x), fl(p.y)),
        Variant::CFrame(c) => format!("Cf({})", cf(c)),
        Variant::OptionalCFrame(None) => "NoCf".into(),
        Variant::OptionalCFrame(Some(c)) => format!("OCf({})", cf(c)),
        Variant::ContentId(c) => format!("Id({:?})", c.as_str()),
        Variant::Content(c) => match c.as_uri() {
            Some(u) => format!("Uri({u:?})"),
            None => "NoContent".into(),
        },
        Variant::BinaryString(b) => format!("Bin({})", bytes(b.as_ref())),
        Variant::SharedString(s) => format!("Shared({})", bytes(s.data())),
        Variant::UDim(u) => format!("Ud({}, {})", fl(u.scale), u.offset),
        Variant::UDim2(u) => format!("Ud2({}, {}, {}, {})", fl(u.x.scale), u.x.offset, fl(u.y.scale), u.y.offset),
        Variant::NumberSequence(s) => format!(
            "NSeq(&[{}])",
            s.keypoints.iter().map(|k| format!("[{}, {}, {}]", fl(k.time), fl(k.value), fl(k.envelope))).collect::<Vec<_>>().join(", ")
        ),
        Variant::ColorSequence(s) => format!(
            "CSeq(&[{}])",
            s.keypoints
                .iter()
                .map(|k| format!("[{}, {}, {}, {}]", fl(k.time), fl(k.color.r), fl(k.color.g), fl(k.color.b)))
                .collect::<Vec<_>>()
                .join(", ")
        ),
        Variant::Font(ft) => format!("Font({:?}, {}, {})", ft.family, ft.weight.as_u16(), ft.style.as_u8()),
        Variant::Rect(r) => format!("Rect({}, {}, {}, {})", fl(r.min.x), fl(r.min.y), fl(r.max.x), fl(r.max.y)),
        Variant::NumberRange(r) => format!("Range({}, {})", fl(r.min), fl(r.max)),
        Variant::PhysicalProperties(rbx_dom_weak::types::PhysicalProperties::Default) => "DefaultPhysics".into(),
        Variant::Ref(r) => match path_of(*r) {
            Some(p) => format!("Ref({p:?})"),
            None => "NoRef".into(),
        },
        Variant::Attributes(a) => {
            let mut items = Vec::new();
            for (k, v) in a.iter() {
                items.push(format!("({k:?}, {})", lit(v, path_of)?));
            }
            format!("Attrs(&[{}])", items.join(", "))
        }
        Variant::Tags(t) => format!("Tags(&[{}])", t.iter().map(|s| format!("{s:?}")).collect::<Vec<_>>().join(", ")),
        Variant::MaterialColors(m) => format!("Bin({})", bytes(&m.encode())),
        _ => return None,
    })
}

/// The properties of a Studio instance that rustudio's class defaults
/// don't already give, as (name, literal), sorted by name.
fn table_props(studio: &Document, r: Ref, scratch: &mut Document, path_of: &dyn Fn(Ref) -> Option<String>) -> Vec<(String, String)> {
    let i = studio.dom.get_by_ref(r).unwrap();
    let root = scratch.dom.root_ref();
    let fresh = scratch.insert(root, rbx_dom_weak::InstanceBuilder::new(i.class));
    let mut out = Vec::new();
    let mut names: Vec<_> = i.properties.keys().map(|k| k.to_string()).collect();
    names.sort();
    for name in names {
        if VOLATILE.contains(&name.as_str()) || name == "SmoothGrid" || name == "PhysicsGrid" {
            continue;
        }
        let v = &i.properties[&rbx_dom_weak::ustr(&name)];
        let sv = norm(studio, v);
        let dv = scratch.prop(fresh, &name).map(|d| norm(scratch, &d)).unwrap_or_else(|| "<none>".into());
        if name != "Name" && same(&sv, &dv) {
            continue;
        }
        match lit(v, path_of) {
            Some(l) => out.push((name, l)),
            None => eprintln!("generator: {} . {name}: no literal for {v:?}", i.class),
        }
    }
    scratch.dom.destroy(fresh);
    out
}

/// Every instance as (slash path of names, class, props) in tree order.
pub fn table(studio: &Document) -> Vec<(String, String, Vec<(String, String)>)> {
    let mut by_ref: HashMap<Ref, String> = HashMap::new();
    let mut order = Vec::new();
    fn walk(doc: &Document, r: Ref, path: &str, by_ref: &mut HashMap<Ref, String>, order: &mut Vec<Ref>) {
        let kids = doc.dom.get_by_ref(r).unwrap().children().to_vec();
        let mut count: HashMap<&str, usize> = HashMap::new();
        for c in &kids {
            *count.entry(doc.dom.get_by_ref(*c).unwrap().name.as_str()).or_default() += 1;
        }
        let mut seen = std::collections::HashSet::new();
        for c in kids.iter().copied() {
            let i = doc.dom.get_by_ref(c).unwrap();
            // Names shared by siblings (three services are all "Instance")
            // take their class: Name@Class.
            let step = if count[i.name.as_str()] > 1 { format!("{}@{}", i.name, i.class) } else { i.name.clone() };
            assert!(seen.insert(step.clone()), "two children {step} under {path}");
            let p = if path.is_empty() { step } else { format!("{path}/{step}") };
            by_ref.insert(c, p.clone());
            order.push(c);
            walk(doc, c, &p, by_ref, order);
        }
    }
    walk(studio, studio.dom.root_ref(), "", &mut by_ref, &mut order);
    let path_of = |r: Ref| by_ref.get(&r).cloned();
    let mut scratch = Document::new_place();
    order
        .iter()
        .map(|r| {
            let class = studio.dom.get_by_ref(*r).unwrap().class.to_string();
            (by_ref[r].clone(), class, table_props(studio, *r, &mut scratch, &path_of))
        })
        .collect()
}

fn props_src(props: &[(String, String)]) -> String {
    props.iter().map(|(k, v)| format!("        ({k:?}, {v}),\n")).collect()
}

/// Rust source for the built-in templates: Baseplate whole, the others as
/// patches on it.
pub fn generate(base: &Document, others: &[(&str, &Document)]) -> String {
    let bt = table(base);
    let mut s = String::new();
    s.push_str("//! Studio's generic templates as rustudio builds them offline: generated\n");
    s.push_str("//! by `start_page::tests::generate_builtin_templates` from the DataModels\n");
    s.push_str("//! Studio holds after opening each template place (File >\n");
    s.push_str("//! Save to File As). Only the values rustudio's class defaults don't give\n");
    s.push_str("//! are listed. Regenerate rather than edit.\n\n");
    s.push_str("use super::templates::{Inst, Patch, V::*};\n\n");
    s.push_str("pub(super) static BASEPLATE: &[Inst] = &[\n");
    for (p, c, props) in &bt {
        s.push_str(&format!("    Inst {{ path: {p:?}, class: {c:?}, props: &[\n{}    ] }},\n", props_src(props)));
    }
    s.push_str("];\n");
    let bmap: HashMap<&str, (&str, &Vec<(String, String)>)> = bt.iter().map(|(p, c, v)| (p.as_str(), (c.as_str(), v))).collect();
    for (name, doc) in others {
        let ot = table(doc);
        let omap: HashMap<&str, &Vec<(String, String)>> = ot.iter().map(|(p, _, v)| (p.as_str(), v)).collect();
        let remove: Vec<&str> = bt.iter().map(|(p, _, _)| p.as_str()).filter(|p| !omap.contains_key(p)).collect();
        // Only the top of each removed subtree.
        let remove: Vec<&str> = remove.iter().copied().filter(|p| !remove.iter().any(|q| p.starts_with(&format!("{q}/")))).collect();
        let mut add = String::new();
        let mut set = String::new();
        for (p, c, props) in &ot {
            match bmap.get(p.as_str()) {
                None => add.push_str(&format!("        Inst {{ path: {p:?}, class: {c:?}, props: &[\n{}        ] }},\n", props_src(props).replace("\n        (", "\n            (").replacen("        (", "            (", 1))),
                Some((_, bprops)) => {
                    let bm: HashMap<&str, &str> = bprops.iter().map(|(k, v)| (k.as_str(), v.as_str())).collect();
                    let om: HashMap<&str, &str> = props.iter().map(|(k, v)| (k.as_str(), v.as_str())).collect();
                    for (k, v) in props {
                        if bm.get(k.as_str()) != Some(&v.as_str()) {
                            set.push_str(&format!("        ({p:?}, {k:?}, {v}),\n"));
                        }
                    }
                    for (k, _) in bprops.iter() {
                        if !om.contains_key(k.as_str()) {
                            set.push_str(&format!("        ({p:?}, {k:?}, Default),\n"));
                        }
                    }
                }
            }
        }
        s.push_str(&format!(
            "\npub(super) static {name}: Patch = Patch {{\n    remove: &[{}],\n    add: &[\n{add}    ],\n    set: &[\n{set}    ],\n}};\n",
            remove.iter().map(|p| format!("{p:?}")).collect::<Vec<_>>().join(", ")
        ));
    }
    s
}
