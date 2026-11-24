//! What File Sync changes in the open place: the project tree (src/project.rs
//! reads it) matched to the place's instances by name and class, compared,
//! and turned into steps — create, set a property, delete — each with the
//! file it came from, for the preview, the log and the change count.
//!
//! - **Scope.** At attach the whole tree is compared; after that only the
//!   nodes whose files changed on disk (a change to a folder covers what is
//!   inside it). So a file saved changes only its own instance, whatever
//!   else differs between the place and the files ("Place wins" leaves
//!   those differences until their files change).
//! - **Creates** (`Opts::create_missing`, Live Hydrate at attach): a node the
//!   place lacks is built with its subtree; one the place has with another
//!   class is replaced (counted as a replace in the preview).
//! - **Deletes**: an instance that came from a file (`known`) whose file is
//!   gone; and, unless unknowns are kept (the setting, or the node's
//!   `$ignoreUnknownInstances` / keepUnknowns), instances under a mapped
//!   folder that no file describes. Services and the DataModel's children
//!   that aren't mapped folders are never touched.
//! - **Only Code Mode**: only scripts and instances with scripts inside.
//! - Model files (.rbxm / .rbxmx) are not compared at attach (their
//!   instances are kept); when the file changes the instance is rebuilt.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use rbx_dom_weak::types::{Ref, Variant};
use rbx_dom_weak::ustr;
use rustudio_core::doc::Document;

use crate::mcp::project_apply::{clone_snap, count, create};
use crate::mcp::tree;
use crate::project::{Origin, Snap};

/// Which nodes are compared.
#[derive(Clone, Debug, PartialEq)]
pub enum Scope {
    /// The whole tree (attach, the project file changed, "Sync all now").
    All,
    /// Only what these changed paths (files or folders) feed.
    Paths(HashSet<PathBuf>),
}

impl Scope {
    pub fn merge(&mut self, other: Scope) {
        match (&mut *self, other) {
            (Scope::All, _) => {}
            (_, Scope::All) => *self = Scope::All,
            (Scope::Paths(a), Scope::Paths(b)) => a.extend(b),
        }
    }

    fn touches(&self, o: Option<&Origin>) -> bool {
        match self {
            Scope::All => true,
            Scope::Paths(changed) => o.is_some_and(|o| origin_paths(o).iter().any(|p| changed.iter().any(|c| p.starts_with(c)))),
        }
    }
}

/// The paths an origin is read from.
fn origin_paths(o: &Origin) -> Vec<&Path> {
    match o {
        Origin::Node { project, .. } => vec![project.as_path()],
        Origin::File { file, data, .. } | Origin::Model { file, data, .. } => vec![file.as_path(), data.as_path()],
        Origin::Dir { dir, init, data } => {
            let mut v = vec![dir.as_path(), data.as_path()];
            if let Some((f, _)) = init {
                v.push(f.as_path());
            }
            v
        }
        Origin::InModel { file } => vec![file.as_path()],
    }
}

/// The file a node is read from (for the log), if any.
pub fn origin_file(o: Option<&Origin>) -> Option<PathBuf> {
    match o? {
        Origin::Node { project, .. } => Some(project.clone()),
        Origin::File { file, .. } | Origin::Model { file, .. } | Origin::InModel { file } => Some(file.clone()),
        Origin::Dir { dir, init, .. } => Some(init.as_ref().map(|(f, _)| f.clone()).unwrap_or_else(|| dir.clone())),
    }
}

#[derive(Clone, Copy, Debug)]
pub struct Opts {
    /// Build instances for nodes the place lacks.
    pub create_missing: bool,
    /// Leave instances no file describes alone.
    pub keep_unknowns: bool,
    /// Only scripts and instances with scripts inside.
    pub code_only: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Why {
    /// It came from a file that is gone.
    FileGone,
    /// No file describes it (a mapped folder without Keep Unknowns).
    Unknown,
}

pub enum Step {
    Create { parent: Ref, snap: Box<Snap>, replace: Option<Ref>, file: Option<PathBuf> },
    Set { r: Ref, prop: String, value: Variant, file: Option<PathBuf> },
    Delete { r: Ref, why: Why, file: Option<PathBuf> },
}

#[derive(Default)]
pub struct Plan {
    pub steps: Vec<Step>,
    /// Things left alone and why (missing with Live Hydrate off, a model
    /// not compared, an instance that can't be replaced).
    pub notes: Vec<String>,
}

impl Plan {
    pub fn is_empty(&self) -> bool {
        self.steps.is_empty()
    }

    /// (creates, sets, deletes); a create that replaces counts as both.
    pub fn counts(&self) -> (usize, usize, usize) {
        let (mut c, mut s, mut d) = (0, 0, 0);
        for st in &self.steps {
            match st {
                Step::Create { replace, .. } => {
                    c += 1;
                    d += replace.is_some() as usize;
                }
                Step::Set { .. } => s += 1,
                Step::Delete { .. } => d += 1,
            }
        }
        (c, s, d)
    }

    /// Scripts whose Source this plan sets.
    pub fn sources(&self) -> usize {
        self.steps.iter().filter(|s| matches!(s, Step::Set { prop, .. } if prop == "Source")).count()
    }
}

pub fn is_script(class: &str) -> bool {
    rustudio_core::doc::is_a(class, "LuaSourceContainer")
}

/// Whether a snapshot is or holds a script.
fn snap_has_code(s: &Snap) -> bool {
    if is_script(&s.class) {
        return true;
    }
    if let Some((d, kids)) = &s.dom {
        return kids.iter().any(|k| d.descendants_of(*k).any(|i| is_script(i.class.as_str())));
    }
    s.children.iter().any(snap_has_code)
}

/// Whether an instance is or holds a script.
pub fn has_code(doc: &Document, r: Ref) -> bool {
    doc.dom.descendants_of(r).any(|i| is_script(i.class.as_str()))
}

/// A copy of a snapshot without the parts that hold no code.
fn code_only_copy(s: &Snap) -> Snap {
    let mut c = clone_snap(s);
    fn prune(s: &mut Snap) {
        s.children.retain(snap_has_code);
        for c in &mut s.children {
            prune(c);
        }
    }
    prune(&mut c);
    c
}

/// How a project node's children pair with an instance's children: each
/// node's instance (same name and class, first unused), the same-named one
/// of another class it would replace, and the instance's children left over.
struct Pairing {
    same: Vec<Option<Ref>>,
    other: Vec<Option<Ref>>,
    left: Vec<Ref>,
}

fn pair(doc: &Document, r: Ref, nodes: &[Snap]) -> Pairing {
    let kids: Vec<Ref> = doc.dom.get_by_ref(r).map(|i| i.children().to_vec()).unwrap_or_default();
    let mut used: HashSet<Ref> = HashSet::new();
    let mut same = Vec::with_capacity(nodes.len());
    for c in nodes {
        let k = kids.iter().copied().find(|k| !used.contains(k) && doc.dom.get_by_ref(*k).is_some_and(|i| i.name == c.name && i.class == c.class.as_str()));
        if let Some(k) = k {
            used.insert(k);
        }
        same.push(k);
    }
    let mut other = Vec::with_capacity(nodes.len());
    for (i, c) in nodes.iter().enumerate() {
        let k = if same[i].is_some() {
            None
        } else {
            kids.iter().copied().find(|k| !used.contains(k) && doc.dom.get_by_ref(*k).is_some_and(|i| i.name == c.name))
        };
        if let Some(k) = k {
            used.insert(k);
        }
        other.push(k);
    }
    let left = kids.into_iter().filter(|k| !used.contains(k)).collect();
    Pairing { same, other, left }
}

/// Which instance each project node is (where the place has it): the File
/// Sync map, which write-back and "came from a file" deletes go by.
pub fn match_map(doc: &Document, tree: &Snap) -> HashMap<Ref, Origin> {
    let mut map = HashMap::new();
    fn walk(doc: &Document, s: &Snap, r: Ref, map: &mut HashMap<Ref, Origin>) {
        if let Some(o) = &s.origin {
            map.insert(r, o.clone());
        }
        if s.dom.is_some() {
            if let Some(file) = s.origin.as_ref().and_then(|o| o.key()) {
                for d in doc.dom.descendants_of(r).skip(1) {
                    map.insert(d.referent(), Origin::InModel { file: file.to_path_buf() });
                }
            }
            return;
        }
        let p = pair(doc, r, &s.children);
        for (c, k) in s.children.iter().zip(p.same) {
            if let Some(k) = k {
                walk(doc, c, k, map);
            }
        }
    }
    walk(doc, tree, doc.dom.root_ref(), &mut map);
    map
}

/// Compares the tree with the place within `scope`. `known` is the last
/// map (instances that came from files).
pub fn plan(doc: &Document, tree: &Snap, scope: &Scope, known: &HashMap<Ref, Origin>, opts: Opts) -> Plan {
    let mut p = Plan::default();
    walk(doc, tree, doc.dom.root_ref(), false, scope, known, opts, &mut p);
    p
}

#[allow(clippy::too_many_arguments)]
fn walk(doc: &Document, s: &Snap, r: Ref, parent_in: bool, scope: &Scope, known: &HashMap<Ref, Origin>, opts: Opts, p: &mut Plan) {
    let Some(inst) = doc.dom.get_by_ref(r) else { return };
    let here = parent_in || scope.touches(s.origin.as_ref());
    let file = origin_file(s.origin.as_ref());
    if here && (!opts.code_only || is_script(inst.class.as_str())) {
        for (k, v) in &s.props {
            if doc.prop(r, k).as_ref() != Some(v) {
                p.steps.push(Step::Set { r, prop: k.clone(), value: v.clone(), file: file.clone() });
            }
        }
    }
    let pairing = pair(doc, r, &s.children);
    for (i, c) in s.children.iter().enumerate() {
        let c_in = here || scope.touches(c.origin.as_ref());
        let c_file = origin_file(c.origin.as_ref());
        if opts.code_only && !snap_has_code(c) {
            continue;
        }
        match pairing.same[i] {
            Some(k) if c.dom.is_some() => {
                // A model file: rebuilt when it changed, else kept as it is.
                if c_in && matches!(scope, Scope::Paths(_)) {
                    p.steps.push(Step::Create { parent: r, snap: Box::new(clone_snap(c)), replace: Some(k), file: c_file });
                }
            }
            Some(k) => walk(doc, c, k, here, scope, known, opts, p),
            None => {
                if !c_in {
                    continue;
                }
                if !opts.create_missing {
                    p.notes.push(format!("{}: not in the place (Live Hydrate is off)", child_path(doc, r, &c.name)));
                    continue;
                }
                let replace = pairing.other[i];
                if let Some(k) = replace {
                    if !doc.can_edit_structure(k) {
                        p.notes.push(format!("{}: the place has a {} there that can't be replaced", tree::path(doc, k), doc.class_of(k).map(|c| c.to_string()).unwrap_or_default()));
                        continue;
                    }
                }
                let snap = if opts.code_only { code_only_copy(c) } else { clone_snap(c) };
                p.steps.push(Step::Create { parent: r, snap: Box::new(snap), replace, file: c_file });
            }
        }
    }
    // The instance's children no node describes.
    let keeps = opts.keep_unknowns || s.keep_unknowns || !matches!(s.origin, Some(Origin::Dir { .. }));
    for k in pairing.left {
        if !doc.can_edit_structure(k) || (opts.code_only && !has_code(doc, k)) {
            continue;
        }
        let came_from = known.get(&k).filter(|o| o.key().is_some());
        let gone = came_from.is_some_and(|o| here || scope.touches(Some(o)));
        if gone {
            p.steps.push(Step::Delete { r: k, why: Why::FileGone, file: origin_file(came_from) });
        } else if here && !keeps && came_from.is_none() {
            p.steps.push(Step::Delete { r: k, why: Why::Unknown, file: None });
        }
    }
}

fn child_path(doc: &Document, parent: Ref, name: &str) -> String {
    let pp = tree::path(doc, parent);
    if pp == "game" { name.to_string() } else { format!("{pp}.{name}") }
}

/// One line of the preview / log for a step, with the path as it is now.
pub struct Line {
    pub kind: &'static str,
    pub path: String,
    pub class: String,
    pub prop: Option<String>,
    pub file: Option<PathBuf>,
    /// (old, new) for a text property (Source…), else short values.
    pub old: Option<String>,
    pub new: Option<String>,
    pub instances: usize,
}

fn text_of(v: &Variant) -> String {
    match v {
        Variant::String(s) => s.clone(),
        other => format!("{other:?}"),
    }
}

pub fn lines(doc: &Document, plan: &Plan) -> Vec<Line> {
    plan.steps
        .iter()
        .map(|s| match s {
            Step::Create { parent, snap, replace, file } => Line {
                kind: if replace.is_some() { "replace" } else { "create" },
                path: child_path(doc, *parent, &snap.name),
                class: snap.class.clone(),
                prop: None,
                file: file.clone(),
                old: None,
                new: snap.props.iter().find(|(k, _)| k == "Source").map(|(_, v)| text_of(v)),
                instances: count(snap),
            },
            Step::Set { r, prop, value, file } => Line {
                kind: "set",
                path: tree::path(doc, *r),
                class: doc.class_of(*r).map(|c| c.to_string()).unwrap_or_default(),
                prop: Some(prop.clone()),
                file: file.clone(),
                old: Some(doc.prop(*r, prop).map(|v| text_of(&v)).unwrap_or_default()),
                new: Some(text_of(value)),
                instances: 1,
            },
            Step::Delete { r, why, file } => Line {
                kind: match why {
                    Why::FileGone => "delete",
                    Why::Unknown => "delete unknown",
                },
                path: tree::path(doc, *r),
                class: doc.class_of(*r).map(|c| c.to_string()).unwrap_or_default(),
                prop: None,
                file: file.clone(),
                old: None,
                new: None,
                instances: doc.dom.descendants_of(*r).count(),
            },
        })
        .collect()
}

/// What applying did, for the log: (step line, outcome).
pub struct Applied {
    pub path: String,
    pub what: String,
    pub file: Option<PathBuf>,
    /// The instance, for scripts whose Source was set (live commits).
    pub source_set: Option<Ref>,
    pub error: Option<String>,
}

/// Applies a plan as document edits (undoable, journaled: a live Team
/// Create document sends them).
pub fn apply(doc: &mut Document, plan: Plan) -> Vec<Applied> {
    let mut out = Vec::new();
    let mut sets: Vec<(Ref, rbx_dom_weak::Ustr, Variant)> = Vec::new();
    for step in plan.steps {
        match step {
            Step::Set { r, prop, value, file } => {
                let source = (prop == "Source").then_some(r);
                let what = match &value {
                    Variant::String(s) if prop == "Source" => format!("Source ({} lines)", s.lines().count()),
                    _ => format!("set {prop}"),
                };
                out.push(Applied { path: tree::path(doc, r), what, file, source_set: source, error: None });
                sets.push((r, ustr(&prop), value));
            }
            Step::Create { parent, snap, replace, file } => {
                let path = child_path(doc, parent, &snap.name);
                if let Some(old) = replace {
                    doc.remove(&[old]);
                }
                let n = count(&snap);
                let what = match (replace.is_some(), n) {
                    (true, n) => format!("replaced ({n} instance(s))"),
                    (false, 1) => format!("created {}", snap.class),
                    (false, n) => format!("created {} with {} inside", snap.class, n - 1),
                };
                let error = create(doc, parent, &snap).err();
                out.push(Applied { path, what, file, source_set: None, error });
            }
            Step::Delete { r, why, file } => {
                let path = tree::path(doc, r);
                let n = doc.dom.descendants_of(r).count();
                let gone = doc.remove(&[r]);
                let what = format!("deleted ({n} instance(s){})", if why == Why::Unknown { ", no file describes it" } else { ", its file is gone" });
                out.push(Applied { path, what, file, source_set: None, error: (gone == 0).then(|| "it couldn't be deleted".into()) });
            }
        }
    }
    if !sets.is_empty() {
        doc.set_props("File Sync", sets, None);
    }
    out
}

/// Where a queued create goes: an instance the place has, or one an earlier
/// item of the same queue made.
#[derive(Clone, Copy, Debug)]
pub enum Target {
    Ref(Ref),
    Made(usize),
}

/// One item of a live apply sent in batches (Team Create dropped the
/// session on big single-frame applies): a create without its children
/// (they follow as their own items, parents first), a set or a delete.
pub enum Queued {
    Create { parent: Target, slot: usize, snap: Box<Snap>, replace: Option<Ref>, file: Option<PathBuf>, top: bool },
    Set { r: Ref, prop: String, value: Variant, file: Option<PathBuf> },
    Delete { r: Ref, why: Why, file: Option<PathBuf> },
}

/// A plan cut into batch items; `made` holds what each create made.
pub struct LiveQueue {
    pub items: std::collections::VecDeque<Queued>,
    pub made: Vec<Option<Ref>>,
    /// Instances in the whole queue, and sent so far.
    pub total: usize,
    pub sent: usize,
    pub last: Option<std::time::Instant>,
}

impl LiveQueue {
    pub fn new(plan: Plan) -> LiveQueue {
        let mut q = LiveQueue { items: Default::default(), made: Vec::new(), total: 0, sent: 0, last: None };
        // Sets first (the scripts already there), creates, deletes last.
        let mut later = Vec::new();
        for step in plan.steps {
            match step {
                Step::Set { r, prop, value, file } => {
                    q.total += 1;
                    q.items.push_back(Queued::Set { r, prop, value, file });
                }
                Step::Create { parent, snap, replace, file } => q.push_create(Target::Ref(parent), *snap, replace, file, true),
                Step::Delete { r, why, file } => later.push(Queued::Delete { r, why, file }),
            }
        }
        q.total += later.len();
        q.items.extend(later);
        q
    }

    fn push_create(&mut self, parent: Target, mut snap: Snap, replace: Option<Ref>, file: Option<PathBuf>, top: bool) {
        let slot = self.made.len();
        self.made.push(None);
        // A model file goes whole; a folder's children follow it.
        let children = if snap.dom.is_some() { Vec::new() } else { std::mem::take(&mut snap.children) };
        self.total += count(&snap);
        self.items.push_back(Queued::Create { parent, slot, snap: Box::new(snap), replace, file, top });
        for c in children {
            let f = origin_file(c.origin.as_ref());
            self.push_create(Target::Made(slot), c, None, f, false);
        }
    }

    pub fn is_done(&self) -> bool {
        self.items.is_empty()
    }

    /// Applies items until about `budget` instances went (at least one
    /// item). Returns what was applied, for the log (creates inside a new
    /// folder are folded into its line).
    pub fn apply_batch(&mut self, doc: &mut Document, budget: usize) -> Vec<Applied> {
        let mut out = Vec::new();
        let mut used = 0;
        let mut sets: Vec<(Ref, rbx_dom_weak::Ustr, Variant)> = Vec::new();
        while let Some(item) = self.items.front() {
            let weight = match item {
                Queued::Create { snap, .. } => count(snap),
                _ => 1,
            };
            if used > 0 && used + weight > budget.max(1) {
                break;
            }
            used += weight;
            self.sent += weight;
            match self.items.pop_front().unwrap() {
                Queued::Set { r, prop, value, file } => {
                    let source = (prop == "Source").then_some(r);
                    let what = match &value {
                        Variant::String(s) if prop == "Source" => format!("Source ({} lines)", s.lines().count()),
                        _ => format!("set {prop}"),
                    };
                    out.push(Applied { path: tree::path(doc, r), what, file, source_set: source, error: None });
                    sets.push((r, ustr(&prop), value));
                }
                Queued::Delete { r, why, file } => {
                    let path = tree::path(doc, r);
                    let n = doc.dom.descendants_of(r).count();
                    let gone = doc.remove(&[r]);
                    let what = format!("deleted ({n} instance(s){})", if why == Why::Unknown { ", no file describes it" } else { ", its file is gone" });
                    out.push(Applied { path, what, file, source_set: None, error: (gone == 0).then(|| "it couldn't be deleted".into()) });
                }
                Queued::Create { parent, slot, snap, replace, file, top } => {
                    let parent = match parent {
                        Target::Ref(r) => Some(r),
                        Target::Made(i) => self.made.get(i).copied().flatten(),
                    };
                    let Some(parent) = parent.filter(|p| doc.dom.get_by_ref(*p).is_some()) else {
                        out.push(Applied { path: snap.name.clone(), what: "not created".into(), file, source_set: None, error: Some("its parent wasn't created".into()) });
                        continue;
                    };
                    let path = child_path(doc, parent, &snap.name);
                    if let Some(old) = replace {
                        doc.remove(&[old]);
                    }
                    match create(doc, parent, &snap) {
                        Ok(r) => {
                            self.made[slot] = Some(r);
                            if top {
                                out.push(Applied { path, what: format!("created {}", snap.class), file, source_set: None, error: None });
                            }
                        }
                        Err(e) => out.push(Applied { path, what: "not created".into(), file, source_set: None, error: Some(e) }),
                    }
                }
            }
        }
        if !sets.is_empty() {
            doc.set_props("File Sync", sets, None);
        }
        self.last = Some(std::time::Instant::now());
        out
    }
}

/// One mapped path of the project and where it lands in the place.
#[derive(Clone, Debug, PartialEq)]
pub struct MapRow {
    /// "ServerScriptService.Game".
    pub place: String,
    /// "src/server/" (a folder ends in /) or "src/x.client.luau".
    pub disk: String,
    /// The place has it.
    pub has: bool,
    /// The class the files make.
    pub class: String,
    /// Files under it (a folder), else 1.
    pub files: usize,
}

/// The project's mapped paths and where they land in the place.
pub fn mappings(doc: &Document, tree: &Snap, root: &Path) -> Vec<MapRow> {
    let mut out = Vec::new();
    fn files(s: &Snap) -> usize {
        let own = matches!(s.origin, Some(Origin::File { .. }) | Some(Origin::Model { .. }) | Some(Origin::Dir { init: Some(_), .. })) as usize;
        own + s.children.iter().map(files).sum::<usize>()
    }
    fn walk(doc: &Document, s: &Snap, r: Option<Ref>, path: &str, root: &Path, out: &mut Vec<MapRow>) {
        let disk = match &s.origin {
            // Shown with `/` on every OS, like the project file's paths.
            Some(Origin::Dir { dir, .. }) => Some(format!("{}/", dir.strip_prefix(root).unwrap_or(dir).display()).replace('\\', "/")),
            Some(Origin::File { file, .. }) | Some(Origin::Model { file, .. }) => Some(file.strip_prefix(root).unwrap_or(file).display().to_string().replace('\\', "/")),
            _ => None,
        };
        if let Some(d) = disk {
            out.push(MapRow { place: path.to_string(), disk: d, has: r.is_some(), class: s.class.clone(), files: files(s).max(1) });
            return;
        }
        let p = r.map(|r| pair(doc, r, &s.children));
        for (i, c) in s.children.iter().enumerate() {
            let k = p.as_ref().and_then(|p| p.same[i]);
            let cp = if path.is_empty() { c.name.clone() } else { format!("{path}.{}", c.name) };
            walk(doc, c, k, &cp, root, out);
        }
    }
    walk(doc, tree, Some(doc.dom.root_ref()), "", root, &mut out);
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use rbx_dom_weak::InstanceBuilder;

    struct Fixture {
        dir: PathBuf,
        doc: Document,
    }

    /// A project mapping ServerScriptService.Game to src/server and a place
    /// that has Game with Main (old source), Stale (no file) and a Part.
    fn fixture(tag: &str) -> Fixture {
        let dir = std::env::temp_dir().join(format!("rustudio-filesync-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("src/server/Lib")).unwrap();
        std::fs::write(
            dir.join("default.project.json"),
            r#"{"name": "t", "tree": {"$className": "DataModel", "ServerScriptService": {"$className": "ServerScriptService", "Game": {"$path": "src/server"}}}}"#,
        )
        .unwrap();
        std::fs::write(dir.join("src/server/Main.server.luau"), "print('new')\n").unwrap();
        std::fs::write(dir.join("src/server/Lib/init.luau"), "return {}\n").unwrap();
        std::fs::write(dir.join("src/server/Other.luau"), "return 1\n").unwrap();
        let mut doc = Document::empty();
        doc.ensure_services();
        let sss = doc.find_child(doc.dom.root_ref(), "ServerScriptService").unwrap();
        let game = doc.dom.insert(sss, InstanceBuilder::new("Folder").with_name("Game"));
        doc.dom.insert(game, InstanceBuilder::new("Script").with_name("Main").with_property("Source", "print('old')\n"));
        doc.dom.insert(game, InstanceBuilder::new("ModuleScript").with_name("Other").with_property("Source", "return 0\n"));
        doc.dom.insert(game, InstanceBuilder::new("ModuleScript").with_name("Stale"));
        doc.dom.insert(game, InstanceBuilder::new("Part").with_name("Prop"));
        Fixture { dir, doc }
    }

    fn read(dir: &Path) -> Snap {
        crate::mcp::project_apply::read(&dir.join("default.project.json")).unwrap().0
    }

    fn kinds(doc: &Document, p: &Plan) -> Vec<String> {
        lines(doc, p).iter().map(|l| format!("{} {}{}", l.kind, l.path, l.prop.as_ref().map(|p| format!(".{p}")).unwrap_or_default())).collect()
    }

    const ALL_IN: Opts = Opts { create_missing: true, keep_unknowns: false, code_only: false };

    #[test]
    fn attach_plans_the_whole_tree_with_the_safety_options() {
        let f = fixture("attach");
        let tree = read(&f.dir);
        let none = HashMap::new();
        let k = kinds(&f.doc, &plan(&f.doc, &tree, &Scope::All, &none, ALL_IN));
        assert_eq!(
            k,
            [
                "create ServerScriptService.Game.Lib",
                "set ServerScriptService.Game.Main.Source",
                "set ServerScriptService.Game.Other.Source",
                "delete unknown ServerScriptService.Game.Stale",
                "delete unknown ServerScriptService.Game.Prop",
            ]
        );
        // Keep Unknowns: nothing deleted; Live Hydrate off: nothing created.
        let keep = Opts { keep_unknowns: true, create_missing: false, ..ALL_IN };
        let p = plan(&f.doc, &tree, &Scope::All, &none, keep);
        assert_eq!(kinds(&f.doc, &p), ["set ServerScriptService.Game.Main.Source", "set ServerScriptService.Game.Other.Source"]);
        assert!(p.notes.iter().any(|n| n.contains("Game.Lib") && n.contains("Live Hydrate")), "{:?}", p.notes);
        // Only Code Mode: the Part (no code) is left alone.
        let code = Opts { code_only: true, ..ALL_IN };
        let k = kinds(&f.doc, &plan(&f.doc, &tree, &Scope::All, &none, code));
        assert!(k.contains(&"delete unknown ServerScriptService.Game.Stale".to_string()) && !k.iter().any(|l| l.contains("Prop")), "{k:?}");
        let _ = std::fs::remove_dir_all(&f.dir);
    }

    #[test]
    fn a_changed_file_touches_only_its_instance() {
        let mut f = fixture("scope");
        let tree = read(&f.dir);
        // Place wins at attach: nothing applied, only the map made.
        let map = match_map(&f.doc, &tree);
        let main_file = std::fs::canonicalize(f.dir.join("src/server/Main.server.luau")).unwrap();
        let scope = Scope::Paths([main_file.clone()].into());
        let p = plan(&f.doc, &tree, &scope, &map, ALL_IN);
        assert_eq!(kinds(&f.doc, &p), ["set ServerScriptService.Game.Main.Source"], "Other differs too but its file didn't change");
        assert_eq!(p.steps.iter().filter_map(|s| if let Step::Set { file, .. } = s { file.clone() } else { None }).next(), Some(main_file));
        let out = apply(&mut f.doc, p);
        assert!(out[0].source_set.is_some() && out[0].error.is_none());
        let main = tree::resolve(&f.doc, "ServerScriptService.Game.Main").unwrap();
        assert_eq!(f.doc.prop(main, "Source"), Some(Variant::String("print('new')\n".into())));

        // A new file and a removed file (one that came from a file).
        let root = std::fs::canonicalize(&f.dir).unwrap();
        std::fs::write(root.join("src/server/New.client.luau"), "print(2)\n").unwrap();
        std::fs::remove_file(root.join("src/server/Other.luau")).unwrap();
        let tree = read(&f.dir);
        let scope = Scope::Paths([root.join("src/server/New.client.luau"), root.join("src/server/Other.luau")].into());
        let p = plan(&f.doc, &tree, &scope, &map, ALL_IN);
        assert_eq!(kinds(&f.doc, &p), ["create ServerScriptService.Game.New", "delete ServerScriptService.Game.Other"]);
        // Stale (no file, unknown) and Prop are outside the scope: kept.
        apply(&mut f.doc, p);
        assert!(tree::resolve(&f.doc, "ServerScriptService.Game.Stale").is_ok());
        assert_eq!(f.doc.class_of(tree::resolve(&f.doc, "ServerScriptService.Game.New").unwrap()).unwrap().as_str(), "LocalScript");
        let _ = std::fs::remove_dir_all(&f.dir);
    }

    #[test]
    fn a_removed_file_deletes_even_with_keep_unknowns() {
        let f = fixture("gone");
        let tree = read(&f.dir);
        let map = match_map(&f.doc, &tree);
        let root = std::fs::canonicalize(&f.dir).unwrap();
        std::fs::remove_file(root.join("src/server/Other.luau")).unwrap();
        let tree = read(&f.dir);
        let keep = Opts { keep_unknowns: true, ..ALL_IN };
        let p = plan(&f.doc, &tree, &Scope::Paths([root.join("src/server/Other.luau")].into()), &map, keep);
        assert_eq!(kinds(&f.doc, &p), ["delete ServerScriptService.Game.Other"]);
        assert_eq!(p.counts(), (0, 0, 1));
        let _ = std::fs::remove_dir_all(&f.dir);
    }

    #[test]
    fn applied_twice_leaves_nothing_and_maps_every_file() {
        let mut f = fixture("again");
        let tree = read(&f.dir);
        let p = plan(&f.doc, &tree, &Scope::All, &HashMap::new(), ALL_IN);
        assert_eq!(p.counts(), (1, 2, 2));
        apply(&mut f.doc, p);
        let again = plan(&f.doc, &tree, &Scope::All, &HashMap::new(), ALL_IN);
        assert!(again.is_empty(), "{:?}", kinds(&f.doc, &again));
        let map = match_map(&f.doc, &tree);
        let lib = tree::resolve(&f.doc, "ServerScriptService.Game.Lib").unwrap();
        assert!(matches!(map.get(&lib), Some(Origin::Dir { .. })));
        let m = mappings(&f.doc, &tree, &std::fs::canonicalize(&f.dir).unwrap());
        assert_eq!(m.len(), 1);
        assert_eq!((m[0].place.as_str(), m[0].disk.as_str(), m[0].has, m[0].class.as_str(), m[0].files), ("ServerScriptService.Game", "src/server/", true, "Folder", 3));
        let _ = std::fs::remove_dir_all(&f.dir);
    }

    /// A live apply cut into batches builds the same place as one apply:
    /// parents before children, each batch within its budget.
    #[test]
    fn a_live_queue_builds_the_same_place_in_batches() {
        let mut f = fixture("queue");
        let root = std::fs::canonicalize(&f.dir).unwrap();
        for i in 0..12 {
            std::fs::create_dir_all(root.join(format!("src/server/Lib/Sub{}", i % 3))).unwrap();
            std::fs::write(root.join(format!("src/server/Lib/Sub{}/M{i}.luau", i % 3)), format!("return {i}\n")).unwrap();
        }
        let tree = read(&f.dir);
        let p = plan(&f.doc, &tree, &Scope::All, &HashMap::new(), ALL_IN);
        let mut q = LiveQueue::new(p);
        let total = q.total;
        assert!(total > 16, "{total}");
        let mut batches = 0;
        while !q.is_done() {
            let before = q.sent;
            let out = q.apply_batch(&mut f.doc, 5);
            assert!(q.sent - before <= 5, "batch of {}", q.sent - before);
            assert!(out.iter().all(|a| a.error.is_none()), "{:?}", out.iter().map(|a| &a.error).collect::<Vec<_>>());
            batches += 1;
        }
        assert_eq!(q.sent, total);
        assert!(batches >= total / 5);
        // Nothing left to do: the queue built what one apply would.
        let again = plan(&f.doc, &tree, &Scope::All, &HashMap::new(), ALL_IN);
        assert!(again.is_empty(), "{:?}", kinds(&f.doc, &again));
        assert!(tree::resolve(&f.doc, "ServerScriptService.Game.Lib.Sub2.M11").is_ok());
        let _ = std::fs::remove_dir_all(&f.dir);
    }
}
