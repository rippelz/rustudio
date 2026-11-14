//! Roblox's default player and character scripts, inserted at Play the way
//! Studio does it (checked in an offline Studio: the client's PlayerScripts and the
//! character after Play start, source lengths matched file by file).
//!
//! Nothing is copied into rustudio: the scripts are read at Play start from
//! the Studio install (read only), found as `RUSTUDIO_STUDIO_DIR`, else an
//! up-to-date Studio on this machine (Vinegar's newest version folder: it
//! updates itself, so its scripts are the ones Studio runs today), else the
//! folder the default content link (`~/.local/share/rustudio/content`) points
//! into, else the copy rustudio downloaded from Roblox's CDN
//! (`~/.local/share/rustudio/studio`, see studio_content.rs), else
//! `~/.local/share/rustudio/studio`.
//!
//! - Studio's current layout (checked in Vinegar's Studio 0.747 at Play:
//!   checks/humanoid-physics/feel/studio-0.747-starterplayer-tree.txt), used
//!   unless Workspace.PlayerScriptsUseInputActionSystem is Disabled:
//!   `StarterPlayer.PlayerModule` (ModuleScript tree with CameraModule,
//!   ControlModule, PlayerModuleInternal …, the Scripts PlayerScriptsLoader
//!   (RunContext Client) and InputSetup (RunContext Server), and the
//!   `InputContexts` folder of InputContext / InputAction / InputBinding
//!   objects the modules read their input from), and
//!   StarterPlayerScripts.`RbxCharacterSounds`. The tree is the install's
//!   own project file, `ExtraContent/scripts/PlayerScripts/PlayerScripts.rbxp`.
//! - The older layout (that property Disabled, an install without the
//!   project file, or a place with its own PlayerModule / PlayerScriptsLoader
//!   in StarterPlayerScripts): StarterPlayerScripts gets `PlayerScriptsLoader`
//!   (LocalScript), `PlayerModule` (ModuleScript tree: CameraModule,
//!   ControlModule, CommonUtils) and `RbxCharacterSounds` (LocalScript +
//!   AtomicBinding), from
//!   `ExtraContent/scripts/PlayerScripts/StarterPlayerScripts` and
//!   `StarterPlayerScriptsCommon`.
//! - Each new character gets `Health` (Script,
//!   `content/avatar/scripts/humanoidHealthRegenScript.rbxmx`) and `Animate`
//!   (LocalScript with its animation values: R15
//!   `content/avatar/scripts/humanoidAnimateR15_DEPRECATED_20260813.rbxm`,
//!   R6 `content/avatar/scripts/humanoidAnimatePlayEmote.rbxm`).
//!
//! A place's own script of the same name (in StarterPlayerScripts or
//! StarterCharacterScripts) wins, as in Studio.

use std::path::{Path, PathBuf};

use rbx_dom_weak::types::{Ref, Variant};
use rbx_dom_weak::{InstanceBuilder, WeakDom};

use crate::doc::Document;
use crate::rig::RigType;

/// The Studio install the default scripts are read from.
pub fn studio_dir() -> Option<PathBuf> {
    let has = |p: &Path| p.join("ExtraContent/scripts/PlayerScripts").is_dir();
    if let Ok(p) = std::env::var("RUSTUDIO_STUDIO_DIR") {
        let p = PathBuf::from(p);
        return has(&p).then_some(p);
    }
    // Vinegar's Studio (kept current by Vinegar): its newest version folder.
    if let Some(home) = std::env::var_os("HOME") {
        let versions = PathBuf::from(home).join(".var/app/org.vinegarhq.Vinegar/data/vinegar/versions");
        if let Ok(rd) = std::fs::read_dir(&versions) {
            let newest = rd
                .filter_map(|e| e.ok().map(|e| e.path()))
                .filter(|p| has(p))
                .max_by_key(|p| std::fs::metadata(p).and_then(|m| m.modified()).ok());
            if let Some(p) = newest {
                return Some(p);
            }
        }
    }
    // The person's content link usually points at Studio's own content/.
    if let Ok(target) = std::fs::canonicalize(crate::paths::data().join("rustudio/content")) {
        if let Some(root) = target.parent() {
            if has(root) {
                return Some(root.to_path_buf());
            }
        }
    }
    // Roblox Studio installed on this Mac (RobloxStudio.app).
    if let Some(d) = crate::paths::installed_studio() {
        return Some(d);
    }
    // Studio's content downloaded by rustudio itself (studio_content.rs).
    if let Some(d) = crate::studio_content::installed() {
        if has(&d) {
            return Some(d);
        }
    }
    let fallback = crate::paths::data().join("rustudio/studio");
    has(&fallback).then_some(fallback)
}

const PLAYER_SCRIPTS: &str = "ExtraContent/scripts/PlayerScripts";

/// A child by name (Document::find_child matches the class).
pub fn child_named(doc: &Document, parent: Ref, name: &str) -> Option<Ref> {
    let p = doc.dom.get_by_ref(parent)?;
    p.children()
        .iter()
        .copied()
        .find(|c| doc.dom.get_by_ref(*c).is_some_and(|i| i.name == name))
}

fn read(path: &Path) -> Option<String> {
    std::fs::read_to_string(path).ok()
}

fn script(class: &str, name: &str, source: String) -> InstanceBuilder {
    InstanceBuilder::new(class)
        .with_name(name)
        .with_property("Source", Variant::String(source))
}

/// A Rojo-style folder of `.lua` files: `X.lua` next to a folder `X` is a
/// ModuleScript with the folder's contents as children; a folder alone is a
/// Folder. `*.spec.lua` test files are left out (Studio leaves them out too).
fn module_children(dir: &Path) -> Vec<InstanceBuilder> {
    let Ok(rd) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut entries: Vec<PathBuf> = rd.filter_map(|e| e.ok().map(|e| e.path())).collect();
    entries.sort();
    let mut out = Vec::new();
    for p in &entries {
        let Some(file) = p.file_name().and_then(|f| f.to_str()) else {
            continue;
        };
        if p.is_file() {
            let Some(stem) = file.strip_suffix(".lua") else {
                continue;
            };
            if stem.ends_with(".spec") {
                continue;
            }
            let Some(src) = read(p) else { continue };
            let mut b = script("ModuleScript", stem, src);
            let sub = dir.join(stem);
            if sub.is_dir() {
                b = b.with_children(module_children(&sub));
            }
            out.push(b);
        } else if p.is_dir() && !dir.join(format!("{file}.lua")).is_file() {
            out.push(
                InstanceBuilder::new("Folder")
                    .with_name(file)
                    .with_children(module_children(p)),
            );
        }
    }
    out
}

/// The PlayerModule tree (StarterPlayerScripts/PlayerModule.module*).
fn player_module(root: &Path) -> Option<InstanceBuilder> {
    let base = root.join(PLAYER_SCRIPTS).join("StarterPlayerScripts");
    let src = read(&base.join("PlayerModule.module.lua"))?;
    Some(
        script("ModuleScript", "PlayerModule", src)
            .with_children(module_children(&base.join("PlayerModule.module"))),
    )
}

/// An instance of the install's PlayerScripts project (PlayerScripts.rbxp).
struct Node {
    name: String,
    class: String,
    props: Vec<(String, Variant)>,
    children: Vec<Node>,
}

impl Node {
    fn new(name: &str, class: &str) -> Node {
        Node { name: name.to_string(), class: class.to_string(), props: Vec::new(), children: Vec::new() }
    }

    fn set(&mut self, k: &str, v: Variant) {
        self.props.retain(|(n, _)| n != k);
        self.props.push((k.to_string(), v));
    }

    fn builder(self) -> InstanceBuilder {
        let mut b = InstanceBuilder::new(self.class.as_str()).with_name(self.name.as_str());
        for (k, v) in self.props {
            b = b.with_property(k.as_str(), v);
        }
        b.with_children(self.children.into_iter().map(Node::builder))
    }
}

/// A script file as Studio's project makes it (the live tree, see the module
/// docs): `X.client.lua` a Script with RunContext Client, `X.server.lua` a
/// Script with RunContext Server, `X.lua` a ModuleScript. (stem, class, RunContext)
fn script_kind(file: &str) -> Option<(&str, &'static str, Option<u32>)> {
    let stem = file.strip_suffix(".lua").or_else(|| file.strip_suffix(".luau"))?;
    if stem.ends_with(".spec") {
        return None;
    }
    if let Some(s) = stem.strip_suffix(".client") {
        Some((s, "Script", Some(2)))
    } else if let Some(s) = stem.strip_suffix(".server") {
        Some((s, "Script", Some(1)))
    } else {
        Some((stem, "ModuleScript", None))
    }
}

fn script_node(name: &str, class: &str, run_context: Option<u32>, source: String) -> Node {
    let mut n = Node::new(name, class);
    n.set("Source", Variant::String(source));
    if let Some(rc) = run_context {
        n.set("RunContext", Variant::Enum(rbx_dom_weak::types::Enum::from_u32(rc)));
    }
    n
}

/// A file or folder of the project as an instance: a folder with an
/// `init*.lua` is that script with the rest as children, else a Folder.
fn path_node(p: &Path, name: &str) -> Option<Node> {
    if p.is_file() {
        let (_, class, rc) = script_kind(p.file_name()?.to_str()?)?;
        return Some(script_node(name, class, rc, read(p)?));
    }
    let rd = std::fs::read_dir(p).ok()?;
    let mut entries: Vec<PathBuf> = rd.filter_map(|e| e.ok().map(|e| e.path())).collect();
    entries.sort();
    let init = entries.iter().find_map(|e| {
        let f = e.file_name()?.to_str()?;
        let (stem, class, rc) = script_kind(f)?;
        (e.is_file() && stem == "init").then(|| (e.clone(), class, rc))
    });
    let mut node = match &init {
        Some((file, class, rc)) => script_node(name, class, *rc, read(file)?),
        None => Node::new(name, "Folder"),
    };
    for e in &entries {
        if init.as_ref().is_some_and(|(f, _, _)| f == e) {
            continue;
        }
        let Some(f) = e.file_name().and_then(|f| f.to_str()) else { continue };
        let child = if e.is_dir() {
            path_node(e, f)
        } else {
            script_kind(f).and_then(|(stem, _, _)| path_node(e, stem))
        };
        node.children.extend(child);
    }
    Some(node)
}

/// A project node laid over what its `$path` gave (or a new instance):
/// `$className`, `$properties`, and its children by name.
fn project_node(name: &str, j: &serde_json::Value, base: &Path) -> Option<Node> {
    let mut node = match j.get("$path").and_then(|p| p.as_str()) {
        Some(p) => path_node(&base.join(p), name)?,
        None => Node::new(name, if name == "StarterPlayerScripts" { "StarterPlayerScripts" } else { "Folder" }),
    };
    apply_project(&mut node, j, base);
    Some(node)
}

fn apply_project(node: &mut Node, j: &serde_json::Value, base: &Path) {
    let Some(o) = j.as_object() else { return };
    if let Some(c) = o.get("$className").and_then(|c| c.as_str()) {
        node.class = c.to_string();
    }
    if let Some(props) = o.get("$properties").and_then(|p| p.as_object()) {
        for (k, v) in props {
            if let Ok((name, val)) = crate::project_values::resolve(&node.class, k, v) {
                node.set(&name, val);
            }
        }
    }
    for (k, v) in o {
        if k.starts_with('$') {
            continue;
        }
        match node.children.iter_mut().find(|c| c.name == *k) {
            Some(child) => apply_project(child, v, base),
            None => node.children.extend(project_node(k, v, base)),
        }
    }
}

/// Studio's current player scripts: what PlayerScripts.rbxp puts under
/// StarterPlayer (PlayerModule, StarterPlayerScripts' children).
fn project_starter_player(root: &Path) -> Option<Node> {
    let base = root.join(PLAYER_SCRIPTS);
    let json: serde_json::Value = serde_json::from_str(&read(&base.join("PlayerScripts.rbxp"))?).ok()?;
    let tree = json.get("instanceHierarchy")?.get("tree")?;
    let sp = project_node("StarterPlayer", tree.get("StarterPlayer")?, &base)?;
    sp.children.iter().any(|c| c.name == "PlayerModule").then_some(sp)
}

/// Workspace.PlayerScriptsUseInputActionSystem is Disabled (RolloutState 1).
fn input_action_system_off(doc: &Document) -> bool {
    doc.workspace()
        .is_some_and(|ws| crate::scene::enum_u32(doc.prop(ws, "PlayerScriptsUseInputActionSystem")) == Some(1))
}

/// Puts the default player scripts into StarterPlayer.StarterPlayerScripts
/// (made if missing) unless the place has its own of the same name. Returns
/// the names inserted.
pub fn insert_player_scripts(doc: &mut Document) -> Vec<&'static str> {
    let Some(root_dir) = studio_dir() else {
        return Vec::new();
    };
    let root = doc.dom.root_ref();
    let sp = doc.find_child(root, "StarterPlayer").unwrap_or_else(|| {
        doc.dom.insert(
            root,
            InstanceBuilder::new("StarterPlayer").with_name("StarterPlayer"),
        )
    });
    let sps = doc
        .find_child(sp, "StarterPlayerScripts")
        .unwrap_or_else(|| {
            doc.dom.insert(
                sp,
                InstanceBuilder::new("StarterPlayerScripts").with_name("StarterPlayerScripts"),
            )
        });
    let base = root_dir.join(PLAYER_SCRIPTS);
    let mut added = Vec::new();
    // Studio's current layout: StarterPlayer.PlayerModule (see the module docs).
    let own_old = ["PlayerModule", "PlayerScriptsLoader"].iter().any(|n| child_named(doc, sps, n).is_some());
    if !own_old && !input_action_system_off(doc) {
        if let Some(tree) = project_starter_player(&root_dir) {
            for child in tree.children {
                if child.name == "StarterPlayerScripts" {
                    for c in child.children {
                        if child_named(doc, sps, &c.name).is_none() {
                            added.push(if c.name == "RbxCharacterSounds" { "RbxCharacterSounds" } else { "StarterPlayerScripts" });
                            let r = doc.dom.insert(sps, c.builder());
                            doc.mark_tree(r);
                        }
                    }
                } else if child_named(doc, sp, &child.name).is_none() {
                    added.push(if child.name == "PlayerModule" { "PlayerModule" } else { "StarterPlayer" });
                    let r = doc.dom.insert(sp, child.builder());
                    doc.mark_tree(r);
                }
            }
            return added;
        }
    }
    let mut add = |doc: &mut Document, name: &'static str, b: Option<InstanceBuilder>| {
        if child_named(doc, sps, name).is_some() {
            return;
        }
        if let Some(b) = b {
            let r = doc.dom.insert(sps, b);
            doc.mark_tree(r);
            added.push(name);
        }
    };
    add(
        doc,
        "PlayerScriptsLoader",
        read(&base.join("StarterPlayerScripts/PlayerScriptsLoader.lua"))
            .map(|s| script("LocalScript", "PlayerScriptsLoader", s)),
    );
    add(doc, "PlayerModule", player_module(&root_dir));
    let sounds = read(&base.join("StarterPlayerScriptsCommon/RbxCharacterSounds.lua")).map(|s| {
        let mut b = script("LocalScript", "RbxCharacterSounds", s);
        if let Some(ab) =
            read(&base.join("StarterPlayerScriptsCommon/RbxCharacterSounds/AtomicBinding.lua"))
        {
            b = b.with_child(script("ModuleScript", "AtomicBinding", ab));
        }
        b
    });
    add(doc, "RbxCharacterSounds", sounds);
    added
}

/// Reads a model file (binary or XML) into its own DOM.
fn read_model(path: &Path) -> Option<WeakDom> {
    Document::decode(&std::fs::read(path).ok()?).ok()
}

/// Moves the model file's top-level instance named `name` into `parent`.
fn insert_model(doc: &mut Document, path: &Path, name: &str, parent: Ref) -> Option<Ref> {
    let mut tmp = read_model(path)?;
    let top = tmp
        .root()
        .children()
        .iter()
        .copied()
        .find(|r| tmp.get_by_ref(*r).is_some_and(|i| i.name == name))?;
    tmp.transfer(top, &mut doc.dom, parent);
    doc.mark_tree(top);
    Some(top)
}

/// Adds Health and Animate to a new character (after StarterCharacterScripts
/// were copied in, so a place's own Health / Animate stays). Returns the names added.
pub fn insert_character_scripts(
    doc: &mut Document,
    character: Ref,
    rig: RigType,
) -> Vec<&'static str> {
    let Some(root_dir) = studio_dir() else {
        return Vec::new();
    };
    let avatar = root_dir.join("content/avatar/scripts");
    let mut added = Vec::new();
    if child_named(doc, character, "Health").is_none()
        && insert_model(
            doc,
            &avatar.join("humanoidHealthRegenScript.rbxmx"),
            "Health",
            character,
        )
        .is_some()
    {
        added.push("Health");
    }
    let animate = match rig {
        RigType::R15 => "humanoidAnimateR15_DEPRECATED_20260813.rbxm",
        RigType::R6 => "humanoidAnimatePlayEmote.rbxm",
    };
    if child_named(doc, character, "Animate").is_none()
        && insert_model(doc, &avatar.join(animate), "Animate", character).is_some()
    {
        added.push("Animate");
    }
    added
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_scripts_from_studio_folder() {
        let Some(_) = studio_dir() else { return }; // no Studio install on this machine
        let mut d = Document::empty();
        let added = insert_player_scripts(&mut d);
        assert_eq!(added, vec!["PlayerModule", "RbxCharacterSounds"]);
        let root = d.dom.root_ref();
        let sp = d.find_child(root, "StarterPlayer").unwrap();
        let sps = d.find_child(sp, "StarterPlayerScripts").unwrap();
        let n = |p: Ref, name: &str| child_named(&d, p, name);
        let class = |r: Ref| d.class_of(r).map(|c| c.to_string()).unwrap_or_default();
        let enum_of = |r: Ref, k: &str| crate::scene::enum_u32(d.prop(r, k));
        // Studio's live tree at Play (0.747): StarterPlayer.PlayerModule, the
        // sounds script alone in StarterPlayerScripts.
        assert!(n(sps, "PlayerModule").is_none() && n(sps, "PlayerScriptsLoader").is_none());
        let sounds = n(sps, "RbxCharacterSounds").unwrap();
        assert_eq!(class(sounds), "LocalScript");
        assert!(n(sounds, "AtomicBinding").is_some());
        let pm = n(sp, "PlayerModule").unwrap();
        assert_eq!(class(pm), "ModuleScript");
        let cam = n(pm, "CameraModule").unwrap();
        assert_eq!(class(cam), "ModuleScript");
        assert!(n(cam, "ZoomController").and_then(|z| n(z, "Popper")).is_some());
        assert!(n(cam, "VRCameraTeleportDetector.spec").is_none());
        assert_eq!(class(n(pm, "CommonUtils").unwrap()), "ModuleScript");
        let loader = n(pm, "PlayerScriptsLoader").unwrap();
        assert_eq!((class(loader).as_str(), enum_of(loader, "RunContext")), ("Script", Some(2)));
        let setup = n(pm, "InputSetup").unwrap();
        assert_eq!((class(setup).as_str(), enum_of(setup, "RunContext")), ("Script", Some(1)));
        let auth = n(n(pm, "ServerAuthority").unwrap(), "ServerAuthorityClient").unwrap();
        assert_eq!(d.prop(auth, "Enabled"), Some(Variant::Bool(false)));
        let ctx = n(n(pm, "InputContexts").unwrap(), "CharacterContext").unwrap();
        assert_eq!(class(ctx), "InputContext");
        let mv = n(ctx, "MoveAction").unwrap();
        assert_eq!(class(mv), "InputAction");
        let kb = n(mv, "KeyboardPrimaryBinding").unwrap();
        assert_eq!(class(kb), "InputBinding");
        let key = |name: &str| crate::ltypes::LEnumItem::from_name("KeyCode", name).map(|e| e.value);
        assert_eq!(enum_of(kb, "Up"), key("W"));
        assert_eq!(enum_of(kb, "Left"), key("A"));
        fn len(d: &Document, r: Ref) -> usize {
            match d.prop(r, "Source") {
                Some(Variant::String(s)) => s.len(),
                _ => 0,
            }
        }
        assert!(len(&d, pm) > 100 && len(&d, cam) > 20_000);
        // A second call keeps what is there.
        assert!(insert_player_scripts(&mut d).is_empty());
        let ch = d
            .dom
            .insert(root, InstanceBuilder::new("Model").with_name("Player"));
        assert_eq!(
            insert_character_scripts(&mut d, ch, RigType::R15),
            vec!["Health", "Animate"]
        );
        let anim = child_named(&d, ch, "Animate").unwrap();
        assert_eq!(len(&d, anim), 28802);
        assert!(child_named(&d, anim, "walk").is_some());
    }
}
