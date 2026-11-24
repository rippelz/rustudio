//! `apply_project`: push a Rojo / Argon project into the open place.
//!
//! The project is read with the editor's own project reader (src/project.rs:
//! the same file kinds, `$properties`, meta / model JSON, ignore globs and
//! init files the Project Folder support uses), then compared with the
//! place node by node, by name and class:
//!
//! - a node the place doesn't have (or has with another class) is created
//!   with its subtree (the other-class instance is replaced);
//! - a node the place has gets the properties the project sets that differ
//!   (script Source included); model files (.rbxm / .rbxmx) are only
//!   created, their contents are not compared;
//! - with `delete_extra`, place children the project's folders don't have
//!   are deleted, only under nodes that don't keep unknown instances (Rojo's
//!   `$ignoreUnknownInstances`; folders from `$path` default to not keeping
//!   them).
//!
//! The plan is made first and reported; nothing changes in a dry run.

use std::path::{Path, PathBuf};

use rbx_dom_weak::types::{Ref, Variant};
use rbx_dom_weak::{ustr, InstanceBuilder};
use rustudio_core::doc::Document;
use serde_json::{json, Value as J};

use super::tree;
use crate::project::{self, Snap};

pub enum Step {
    Create { parent: Ref, snap: Box<Snap>, replace: Option<Ref> },
    Set { r: Ref, prop: String, value: Variant },
    Delete { r: Ref },
}

#[derive(Default)]
pub struct Plan {
    pub steps: Vec<Step>,
    pub notes: Vec<String>,
}

/// The project file for `p`: itself, or the folder's default.project.json
/// (else its only *.project.json).
pub fn project_file(p: &Path) -> Result<PathBuf, String> {
    if p.is_dir() {
        let files = project::project_files(p);
        return match files.iter().find(|f| project::name_of(f) == "default.project.json").or(files.first()) {
            Some(f) if files.len() == 1 || project::name_of(f) == "default.project.json" => Ok(f.clone()),
            Some(_) => Err(format!("{} has several project files ({}); name one", p.display(), files.iter().map(|f| project::name_of(f)).collect::<Vec<_>>().join(", "))),
            None => Err(format!("{} has no *.project.json", p.display())),
        };
    }
    if p.is_file() {
        return Ok(p.to_path_buf());
    }
    Err(format!("{} doesn't exist", p.display()))
}

/// Reads a project into its snapshot tree (the DataModel node) and the
/// reader's warnings.
pub fn read(file: &Path) -> Result<(Snap, Vec<String>), String> {
    let file = std::fs::canonicalize(file).map_err(|e| format!("{}: {e}", file.display()))?;
    let root = file.parent().unwrap_or(Path::new("/")).to_path_buf();
    let text = std::fs::read_to_string(&file).map_err(|e| format!("{}: {e}", file.display()))?;
    let pj: J = serde_json::from_str(&text).map_err(|e| format!("{}: {e}", file.display()))?;
    let ignore: Vec<glob::Pattern> = ["globIgnorePaths", "ignoreGlobs"]
        .iter()
        .filter_map(|k| pj.get(*k).and_then(|v| v.as_array()))
        .flatten()
        .filter_map(|g| g.as_str().and_then(|g| glob::Pattern::new(g).ok()))
        .collect();
    // The project's own settings (init file names, …) while it is read; the
    // open project's come back after.
    let before = crate::project_config::active();
    let (config, mut warnings) = crate::project_config::ProjectConfig::load(&file, &pj);
    crate::project_config::set_active(Some(std::sync::Arc::new(config)));
    let mut reader = project::Reader::new(&root, &ignore);
    let tree = reader.project_file(&file);
    warnings.extend(std::mem::take(&mut reader.warnings));
    crate::project_config::set_active(before);
    Ok((tree.ok_or("the project file has no tree")?, warnings))
}

/// Compares the project with the document.
pub fn plan(doc: &Document, tree: &Snap, delete_extra: bool) -> Plan {
    let mut p = Plan::default();
    walk(doc, tree, doc.dom.root_ref(), delete_extra, &mut p);
    p
}

fn walk(doc: &Document, s: &Snap, r: Ref, delete_extra: bool, p: &mut Plan) {
    let Some(inst) = doc.dom.get_by_ref(r) else { return };
    for (k, v) in &s.props {
        if doc.prop(r, k).as_ref() != Some(v) {
            p.steps.push(Step::Set { r, prop: k.clone(), value: v.clone() });
        }
    }
    let mut used: Vec<Ref> = Vec::new();
    for c in &s.children {
        let same_name: Vec<Ref> = inst
            .children()
            .iter()
            .copied()
            .filter(|k| !used.contains(k) && doc.dom.get_by_ref(*k).is_some_and(|i| i.name == c.name))
            .collect();
        let same_class = same_name.iter().copied().find(|k| doc.dom.get_by_ref(*k).is_some_and(|i| i.class == c.class.as_str()));
        match same_class {
            Some(k) => {
                used.push(k);
                if c.dom.is_some() {
                    continue;
                }
                walk(doc, c, k, delete_extra, p);
            }
            None => {
                let replace = same_name.first().copied().filter(|k| doc.can_edit_structure(*k));
                if let Some(k) = replace {
                    used.push(k);
                } else if !same_name.is_empty() {
                    p.notes.push(format!("{}: the place has a {} there that can't be replaced", tree::path(doc, same_name[0]), doc.class_of(same_name[0]).map(|c| c.to_string()).unwrap_or_default()));
                    continue;
                }
                p.steps.push(Step::Create { parent: r, snap: Box::new(clone_snap(c)), replace });
            }
        }
    }
    if delete_extra && !s.keep_unknowns {
        for k in inst.children() {
            if !used.contains(k) && doc.can_edit_structure(*k) {
                p.steps.push(Step::Delete { r: *k });
            }
        }
    }
}

pub(crate) fn clone_snap(s: &Snap) -> Snap {
    Snap {
        name: s.name.clone(),
        class: s.class.clone(),
        props: s.props.clone(),
        children: s.children.iter().map(clone_snap).collect(),
        origin: s.origin.clone(),
        keep_unknowns: s.keep_unknowns,
        dom: s.dom.as_ref().map(|(d, kids)| {
            let mut copy = rbx_dom_weak::WeakDom::new(InstanceBuilder::new("Folder"));
            let new = d.clone_multiple_into_external(kids, &mut copy);
            (copy, new)
        }),
    }
}

pub(crate) fn count(s: &Snap) -> usize {
    1 + s.children.iter().map(count).sum::<usize>() + s.dom.as_ref().map_or(0, |(d, kids)| kids.iter().map(|k| d.descendants_of(*k).count()).sum::<usize>())
}

fn short(v: &Variant, class: &str, prop: &str) -> J {
    if let Variant::String(s) = v {
        if s.len() > 80 || s.contains('\n') {
            return json!(format!("<{} lines, {} bytes>", s.lines().count(), s.len()));
        }
    }
    tree::value_json(class, prop, v)
}

/// The plan as JSON (paths as they are now).
pub fn report(doc: &Document, p: &Plan) -> J {
    let mut creates = Vec::new();
    let mut sets = Vec::new();
    let mut deletes = Vec::new();
    for s in &p.steps {
        match s {
            Step::Create { parent, snap, replace } => {
                let parent_path = tree::path(doc, *parent);
                let path = if parent_path == "game" { snap.name.clone() } else { format!("{parent_path}.{}", snap.name) };
                creates.push(json!({ "path": path, "class": snap.class, "instances": count(snap), "replaces": replace.map(|r| doc.class_of(r).map(|c| c.to_string()).unwrap_or_default()) }));
            }
            Step::Set { r, prop, value } => {
                let class = doc.class_of(*r).map(|c| c.to_string()).unwrap_or_default();
                let old = doc.prop(*r, prop).map(|o| short(&o, &class, prop)).unwrap_or(J::Null);
                sets.push(json!({ "path": tree::path(doc, *r), "property": prop, "from": old, "to": short(value, &class, prop) }));
            }
            Step::Delete { r } => deletes.push(json!({ "path": tree::path(doc, *r), "class": doc.class_of(*r).map(|c| c.to_string()) })),
        }
    }
    json!({ "create": creates, "set": sets, "delete": deletes, "notes": p.notes })
}

/// Builds a snapshot's instance (and subtree) under `parent`.
pub(crate) fn create(doc: &mut Document, parent: Ref, s: &Snap) -> Result<Ref, String> {
    if let Some((dom, kids)) = &s.dom {
        let mut bytes = Vec::new();
        rbx_binary::to_writer(&mut bytes, dom, kids).map_err(|e| format!("{}: {e}", s.name))?;
        let r = if kids.len() == 1 {
            let refs = doc.paste(&bytes, parent, "Apply project");
            let r = *refs.first().ok_or_else(|| format!("{}: its model file didn't paste", s.name))?;
            doc.rename(r, &s.name);
            r
        } else {
            let f = doc.insert(parent, InstanceBuilder::new("Folder").with_name(s.name.clone()));
            doc.paste(&bytes, f, "Apply project");
            f
        };
        if !s.props.is_empty() {
            doc.set_props("Apply project", s.props.iter().map(|(k, v)| (r, ustr(k), v.clone())).collect(), None);
        }
        return Ok(r);
    }
    fn builder(s: &Snap) -> InstanceBuilder {
        let mut b = project::builder(s);
        for c in &s.children {
            if c.dom.is_none() {
                b = b.with_child(builder(c));
            }
        }
        b
    }
    let r = doc.insert(parent, builder(s));
    // Model files inside a new folder: pasted after it.
    fn models(doc: &mut Document, r: Ref, s: &Snap) -> Result<(), String> {
        let kids: Vec<Ref> = doc.dom.get_by_ref(r).map(|i| i.children().to_vec()).unwrap_or_default();
        let mut plain = kids.into_iter();
        for c in &s.children {
            if c.dom.is_some() {
                create(doc, r, c)?;
            } else if let Some(k) = plain.next() {
                models(doc, k, c)?;
            }
        }
        Ok(())
    }
    models(doc, r, s)?;
    Ok(r)
}

/// Applies a plan; returns the number of instances created, properties
/// set and instances deleted.
pub fn apply(doc: &mut Document, p: Plan) -> Result<(usize, usize, usize), String> {
    let (mut made, mut set, mut gone) = (0, 0, 0);
    let mut sets: Vec<(Ref, rbx_dom_weak::Ustr, Variant)> = Vec::new();
    for step in p.steps {
        match step {
            Step::Set { r, prop, value } => {
                sets.push((r, ustr(&prop), value));
                set += 1;
            }
            Step::Create { parent, snap, replace } => {
                if let Some(old) = replace {
                    doc.remove(&[old]);
                    gone += 1;
                }
                create(doc, parent, &snap)?;
                made += count(&snap);
            }
            Step::Delete { r } => {
                gone += doc.remove(&[r]);
            }
        }
    }
    if !sets.is_empty() {
        doc.set_props("Apply project", sets, None);
    }
    Ok((made, set, gone))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn project_dir() -> PathBuf {
        let dir = std::env::temp_dir().join(format!("rustudio-mcp-project-{}-{:?}", std::process::id(), std::thread::current().id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("src/server/Lib")).unwrap();
        std::fs::write(
            dir.join("default.project.json"),
            r#"{"name": "t", "tree": {"$className": "DataModel", "ServerScriptService": {"$className": "ServerScriptService", "Game": {"$path": "src/server"}}}}"#,
        )
        .unwrap();
        std::fs::write(dir.join("src/server/Main.server.luau"), "print('new')\n").unwrap();
        std::fs::write(dir.join("src/server/Lib/init.luau"), "return {}\n").unwrap();
        std::fs::write(dir.join("src/server/Added.luau"), "return 1\n").unwrap();
        dir
    }

    #[test]
    fn a_project_is_planned_and_applied() {
        let dir = project_dir();
        let mut doc = Document::empty();
        doc.ensure_services();
        let sss = doc.find_child(doc.dom.root_ref(), "ServerScriptService").unwrap();
        let game = doc.dom.insert(sss, InstanceBuilder::new("Folder").with_name("Game"));
        doc.dom.insert(game, InstanceBuilder::new("Script").with_name("Main").with_property("Source", "print('old')\n"));
        doc.dom.insert(game, InstanceBuilder::new("Script").with_name("Stale"));
        let file = project_file(&dir).unwrap();
        let (tree, warnings) = read(&file).unwrap();
        assert!(warnings.is_empty(), "{warnings:?}");
        let p = plan(&doc, &tree, false);
        let r = report(&doc, &p);
        assert_eq!(r["set"].as_array().unwrap().len(), 1, "{r}");
        assert_eq!(r["set"][0]["path"], "ServerScriptService.Game.Main");
        assert_eq!(r["set"][0]["property"], "Source");
        let created: Vec<&str> = r["create"].as_array().unwrap().iter().map(|c| c["path"].as_str().unwrap()).collect();
        assert!(created.contains(&"ServerScriptService.Game.Lib") && created.contains(&"ServerScriptService.Game.Added"), "{r}");
        assert!(r["delete"].as_array().unwrap().is_empty());
        // With delete_extra the stale script goes too.
        let p2 = plan(&doc, &tree, true);
        assert_eq!(report(&doc, &p2)["delete"][0]["path"], "ServerScriptService.Game.Stale");
        let (made, set, gone) = apply(&mut doc, p2).unwrap();
        assert_eq!((made, set, gone), (2, 1, 1));
        let main = tree::resolve(&doc, "ServerScriptService.Game.Main").unwrap();
        assert_eq!(doc.prop(main, "Source"), Some(Variant::String("print('new')\n".into())));
        assert_eq!(doc.class_of(tree::resolve(&doc, "ServerScriptService.Game.Lib").unwrap()).unwrap().as_str(), "ModuleScript");
        assert!(tree::resolve(&doc, "ServerScriptService.Game.Stale").is_err());
        // Applied again: nothing left to do.
        let again = plan(&doc, &tree, true);
        assert!(again.steps.is_empty(), "{}", report(&doc, &again));
        let _ = std::fs::remove_dir_all(dir);
    }
}
