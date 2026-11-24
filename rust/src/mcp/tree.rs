//! Instance paths and the Explorer tree for the MCP tools, read straight
//! from the document (no Lua): fast enough to page a 36k-instance place.
//!
//! A path is GetFullName's: names from a DataModel child down, joined by
//! dots ("Workspace.Map.Door"); "game" (or a leading "game.") is the
//! DataModel. A name GetFullName couldn't round-trip (dots, brackets,
//! quotes, empty) is written `["a.b"]` (JSON string quoting). Same-named
//! siblings resolve to the first, as FindFirstChild; every listed instance
//! also has an id (`#` + its referent) that names it exactly.

use rbx_dom_weak::types::{Ref, Variant};
use rustudio_core::doc::Document;
use serde_json::{json, Value as J};

fn needs_quotes(name: &str) -> bool {
    name.is_empty() || name.contains(['.', '[', ']', '"']) || name.trim() != name
}

/// The instance's path (see the module doc).
pub fn path(doc: &Document, r: Ref) -> String {
    let root = doc.dom.root_ref();
    if r == root {
        return "game".into();
    }
    let mut names = Vec::new();
    let mut cur = r;
    while let Some(i) = doc.dom.get_by_ref(cur) {
        if cur == root {
            break;
        }
        names.push(i.name.as_str());
        cur = i.parent();
    }
    // Not under the DataModel (deleted, or a plugin's own): mark it.
    let detached = cur != root;
    let mut out = String::new();
    for name in names.iter().rev() {
        if needs_quotes(name) {
            out.push('[');
            out.push_str(&J::String(name.to_string()).to_string());
            out.push(']');
        } else {
            if !out.is_empty() {
                out.push('.');
            }
            out.push_str(name);
        }
    }
    if detached {
        format!("(detached) {out}")
    } else {
        out
    }
}

pub fn id(r: Ref) -> String {
    format!("#{r}")
}

/// Splits a path into names.
pub fn split(path: &str) -> Result<Vec<String>, String> {
    let mut out = Vec::new();
    let mut rest = path.trim();
    while !rest.is_empty() {
        if let Some(after) = rest.strip_prefix('[') {
            // ["quoted"]: a JSON string, then ].
            let mut de = serde_json::Deserializer::from_str(after).into_iter::<String>();
            let name = match de.next() {
                Some(Ok(n)) => n,
                _ => return Err(format!("{path:?}: a [\"…\"] segment needs a quoted name")),
            };
            let used = de.byte_offset();
            let tail = after[used..].trim_start();
            rest = tail.strip_prefix(']').ok_or_else(|| format!("{path:?}: missing ] after a quoted name"))?;
            out.push(name);
            rest = rest.strip_prefix('.').unwrap_or(rest);
        } else {
            let end = rest.find(['.', '[']).unwrap_or(rest.len());
            let name = &rest[..end];
            if name.is_empty() {
                return Err(format!("{path:?}: an empty name (two dots?)"));
            }
            out.push(name.to_string());
            rest = &rest[end..];
            rest = rest.strip_prefix('.').unwrap_or(rest);
        }
    }
    Ok(out)
}

/// The instance a path or `#id` names.
pub fn resolve(doc: &Document, path: &str) -> Result<Ref, String> {
    let p = path.trim();
    if let Some(hex) = p.strip_prefix('#') {
        let r: Ref = hex.parse().map_err(|_| format!("{p:?} isn't an id"))?;
        return if doc.dom.get_by_ref(r).is_some() { Ok(r) } else { Err(format!("No instance has id {p} (deleted?)")) };
    }
    let mut names = split(p)?;
    if names.first().is_some_and(|n| n == "game") {
        names.remove(0);
    }
    let mut cur = doc.dom.root_ref();
    for (k, name) in names.iter().enumerate() {
        let inst = doc.dom.get_by_ref(cur).ok_or("the DataModel is missing")?;
        let found = inst.children().iter().copied().find(|c| doc.dom.get_by_ref(*c).is_some_and(|i| i.name == *name));
        match found {
            Some(c) => cur = c,
            None => {
                let mut kids: Vec<String> = inst.children().iter().filter_map(|c| doc.dom.get_by_ref(*c)).map(|i| i.name.clone()).collect();
                let more = kids.len().saturating_sub(25);
                kids.truncate(25);
                let at = if k == 0 { "game".to_string() } else { path_of_names(&names[..k]) };
                return Err(format!(
                    "{at} has no child named {name:?} (its children: {}{})",
                    if kids.is_empty() { "none".into() } else { kids.join(", ") },
                    if more > 0 { format!(", … {more} more") } else { String::new() }
                ));
            }
        }
    }
    Ok(cur)
}

fn path_of_names(names: &[String]) -> String {
    names.join(".")
}

pub struct TreeQuery<'a> {
    pub depth: usize,
    pub class: Option<&'a str>,
    pub name: Option<String>,
    pub offset: usize,
    pub limit: usize,
}

/// Instances under `root` (not itself), depth-first in Explorer order,
/// filtered and paged: { total, items, next_offset }.
pub fn list(doc: &Document, root: Ref, q: &TreeQuery) -> J {
    let filtered = q.class.is_some() || q.name.is_some();
    let depth = if filtered && q.depth == 0 { 64 } else { q.depth.max(1) };
    let name = q.name.as_ref().map(|n| n.to_lowercase());
    let mut total = 0usize;
    let mut items = Vec::new();
    let mut stack: Vec<(Ref, usize)> = doc
        .dom
        .get_by_ref(root)
        .map(|i| i.children().iter().rev().map(|c| (*c, 1)).collect())
        .unwrap_or_default();
    while let Some((r, d)) = stack.pop() {
        let Some(inst) = doc.dom.get_by_ref(r) else { continue };
        let class_ok = q.class.is_none_or(|c| rustudio_core::doc::is_a(inst.class.as_str(), c));
        let name_ok = name.as_ref().is_none_or(|n| inst.name.to_lowercase().contains(n));
        if class_ok && name_ok {
            if total >= q.offset && items.len() < q.limit {
                items.push(json!({
                    "path": path(doc, r),
                    "class": inst.class.as_str(),
                    "id": id(r),
                    "children": inst.children().len(),
                }));
            }
            total += 1;
        }
        if d < depth {
            for c in inst.children().iter().rev() {
                stack.push((*c, d + 1));
            }
        }
    }
    let next = q.offset + items.len();
    json!({
        "root": path(doc, root),
        "total": total,
        "items": items,
        "next_offset": if next < total { J::from(next) } else { J::Null },
    })
}

/// A property value as JSON (project-file form), else its debug text.
pub fn value_json(class: &str, prop: &str, v: &Variant) -> J {
    rustudio_core::project_values::to_json(class, prop, v).unwrap_or_else(|| {
        let mut s = format!("{v:?}");
        if s.len() > 400 {
            s.truncate(400);
            s.push('…');
        }
        J::String(s)
    })
}

/// An attribute's value as JSON: the plain shape for the common types
/// (numbers, strings, booleans, [x, y, z], [r, g, b]), else its type and
/// debug text.
pub fn attribute_json(v: &Variant) -> J {
    match v {
        Variant::String(s) => json!(s),
        Variant::Bool(b) => json!(b),
        Variant::Float64(n) => json!(n),
        Variant::Float32(n) => json!(n),
        Variant::Int32(n) => json!(n),
        Variant::Int64(n) => json!(n),
        Variant::Vector3(p) => json!([p.x, p.y, p.z]),
        Variant::Vector2(p) => json!([p.x, p.y]),
        Variant::Color3(c) => json!([c.r, c.g, c.b]),
        Variant::UDim(u) => json!([u.scale, u.offset]),
        Variant::UDim2(u) => json!([[u.x.scale, u.x.offset], [u.y.scale, u.y.offset]]),
        Variant::NumberRange(r) => json!([r.min, r.max]),
        Variant::BrickColor(b) => json!(b.to_string()),
        Variant::CFrame(c) => json!([c.position.x, c.position.y, c.position.z, c.orientation.x.x, c.orientation.x.y, c.orientation.x.z, c.orientation.y.x, c.orientation.y.y, c.orientation.y.z, c.orientation.z.x, c.orientation.z.y, c.orientation.z.z]),
        other => json!({ format!("{:?}", other.ty()): format!("{other:?}") }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rbx_dom_weak::InstanceBuilder;

    fn doc() -> (Document, Ref, Ref) {
        let mut d = Document::empty();
        d.ensure_services();
        let ws = d.workspace().unwrap();
        let model = d.dom.insert(ws, InstanceBuilder::new("Model").with_name("Map"));
        let door = d.dom.insert(model, InstanceBuilder::new("Part").with_name("Door"));
        d.dom.insert(model, InstanceBuilder::new("Part").with_name("a.b"));
        d.dom.insert(model, InstanceBuilder::new("Script").with_name("Open"));
        (d, model, door)
    }

    #[test]
    fn paths_round_trip() {
        let (d, model, door) = doc();
        assert_eq!(path(&d, door), "Workspace.Map.Door");
        assert_eq!(resolve(&d, "Workspace.Map.Door").unwrap(), door);
        assert_eq!(resolve(&d, "game.Workspace.Map").unwrap(), model);
        assert_eq!(resolve(&d, "game").unwrap(), d.dom.root_ref());
        let odd = d.dom.get_by_ref(model).unwrap().children()[1];
        assert_eq!(path(&d, odd), r#"Workspace.Map["a.b"]"#);
        assert_eq!(resolve(&d, r#"Workspace.Map["a.b"]"#).unwrap(), odd);
        assert_eq!(resolve(&d, &id(door)).unwrap(), door);
        let e = resolve(&d, "Workspace.Map.Window").unwrap_err();
        assert!(e.contains("Workspace.Map has no child named \"Window\"") && e.contains("Door"), "{e}");
        assert_eq!(split(r#"A["x]y"].B"#).unwrap(), vec!["A", "x]y", "B"]);
        assert!(split("A..B").is_err());
    }

    #[test]
    fn attributes_read_as_plain_json() {
        use rbx_dom_weak::types::{Color3, Vector3};
        assert_eq!(attribute_json(&Variant::Float64(100.0)), json!(100.0));
        assert_eq!(attribute_json(&Variant::String("red".into())), json!("red"));
        assert_eq!(attribute_json(&Variant::Vector3(Vector3::new(1.0, 2.0, 3.0))), json!([1.0, 2.0, 3.0]));
        assert_eq!(attribute_json(&Variant::Color3(Color3::new(1.0, 0.0, 0.5))), json!([1.0, 0.0, 0.5]));
    }

    #[test]
    fn listing_filters_and_pages() {
        let (d, _, _) = doc();
        let ws = d.workspace().unwrap();
        let q = |class: Option<&'static str>, offset, limit| TreeQuery { depth: 0, class, name: None, offset, limit };
        let all = list(&d, ws, &TreeQuery { depth: 1, ..q(None, 0, 100) });
        assert!(all["items"].as_array().unwrap().iter().any(|i| i["path"] == "Workspace.Map"));
        assert!(!all["items"].as_array().unwrap().iter().any(|i| i["path"] == "Workspace.Map.Door"));
        let parts = list(&d, ws, &q(Some("Part"), 0, 1));
        assert_eq!(parts["total"], 2);
        assert_eq!(parts["items"].as_array().unwrap().len(), 1);
        assert_eq!(parts["next_offset"], 1);
        let rest = list(&d, ws, &q(Some("Part"), 1, 10));
        assert_eq!(rest["items"][0]["path"], r#"Workspace.Map["a.b"]"#);
        assert!(rest["next_offset"].is_null());
        let scripts = list(&d, d.dom.root_ref(), &TreeQuery { name: Some("OPE".into()), ..q(Some("LuaSourceContainer"), 0, 10) });
        assert_eq!(scripts["items"][0]["path"], "Workspace.Map.Open");
    }
}
