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
//! Which player scripts a place gets is `Workspace.PlayerScriptsUseInput-
//! ActionSystem` (Enum.RolloutState Default / Disabled / Enabled), read as
//! Roblox's client reads it today (`input_action_system_on`):
//!
//! - Enabled — the Input Action System layout (checked in Vinegar's Studio
//!   0.747 at Play): `StarterPlayer.PlayerModule` (ModuleScript tree with
//!   CameraModule, ControlModule, PlayerModuleInternal …, the Scripts
//!   PlayerScriptsLoader (RunContext Client) and InputSetup (RunContext
//!   Server), and the `InputContexts` folder of InputContext / InputAction /
//!   InputBinding objects the modules read their input from), and
//!   StarterPlayerScripts.`RbxCharacterSounds`. The tree is the install's
//!   own project file, `ExtraContent/scripts/PlayerScripts/PlayerScripts.rbxp`.
//!   Studio 0.748 and later reference an external `InputContexts.rbxp` subtree.
//!   `StarterPlayer.CreateDefaultPlayerModule` false (a forked module) leaves
//!   the PlayerModule out.
//! - Default and Disabled — the classic layout, the one every place without
//!   the opt-in gets in Roblox's client: the rollout's phase 1 (through
//!   2026; phase 2 — Default meaning Enabled — early 2027; devforum "Input
//!   Action System: PlayerScripts rollout", t/4678416) resolves Default to
//!   Disabled, so a live Baseplate's `Player.PlayerScripts` holds
//!   `PlayerScriptsLoader`, `PlayerModule` and `RbxCharacterSounds`, and
//!   games `require(PlayerScripts.PlayerModule):GetControls()`.
//!   StarterPlayerScripts gets `PlayerScriptsLoader` (LocalScript),
//!   `PlayerModule` (ModuleScript tree: CameraModule, ControlModule,
//!   CommonUtils — `OldPlayerScripts.rbxp`'s tree) and `RbxCharacterSounds`
//!   (LocalScript + AtomicBinding), from
//!   `ExtraContent/scripts/PlayerScripts/StarterPlayerScripts` and
//!   `StarterPlayerScriptsCommon`; the client copies them into
//!   `Player.PlayerScripts` at join (play.rs). The same layout serves an
//!   install without the project file. `mech::PLAYER_SCRIPTS_DEFAULT_IAS` is
//!   where Default flips when Roblox's phase 2 comes.
//! - Each new character gets `Health` (Script,
//!   `content/avatar/scripts/humanoidHealthRegenScript.rbxmx`) and `Animate`
//!   (LocalScript with its animation values: R15
//!   `content/avatar/scripts/humanoidAnimateR15_DEPRECATED_20260813.rbxm`,
//!   R6 `content/avatar/scripts/humanoidAnimatePlayEmote.rbxm`).
//!
//! A place's own script of the same name in the destination wins. In the
//! Input Action System layout a custom module in StarterPlayerScripts can
//! coexist with the default module under StarterPlayer. A place that forked
//! the old character `Sound` script (StarterCharacterScripts) keeps its
//! sounds and gets no RbxCharacterSounds (`own_character_sounds`).

use std::path::{Path, PathBuf};

use rbx_dom_weak::types::{Ref, Variant};
use rbx_dom_weak::{InstanceBuilder, WeakDom};

use crate::doc::Document;
use crate::rig_type::RigType;

/// The Studio install the default scripts are read from.
pub fn studio_dir() -> Option<PathBuf> {
    let has = has_player_scripts;
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
    let bundled = crate::paths::data().join("rustudio/studio");
    has(&bundled).then_some(bundled)
}

const PLAYER_SCRIPTS: &str = "ExtraContent/scripts/PlayerScripts";

pub(crate) fn has_player_scripts(root: &Path) -> bool {
    let scripts = root.join(PLAYER_SCRIPTS);
    scripts.join("PlayerScripts.rbxp").is_file()
        || (scripts.join("StarterPlayerScripts/PlayerModule.module.lua").is_file()
            && scripts.join("StarterPlayerScripts/PlayerModule.module").is_dir())
}

fn player_scripts_dir() -> Option<PathBuf> {
    studio_dir().or_else(crate::studio_content::installed_player_scripts)
}

/// Play can proceed with the priority download or a place's own player module.
pub fn play_controls_available(doc: &Document) -> bool {
    let own = doc.find_child(doc.dom.root_ref(), "StarterPlayer").is_some_and(|sp| {
        child_named(doc, sp, "PlayerModule").is_some()
            || doc.find_child(sp, "StarterPlayerScripts")
                .is_some_and(|sps| child_named(doc, sps, "PlayerModule").is_some()
                    && child_named(doc, sps, "PlayerScriptsLoader").is_some())
    });
    own || player_scripts_dir().is_some()
}

/// A child by name (Document::find_child matches the class).
pub fn child_named(doc: &Document, parent: Ref, name: &str) -> Option<Ref> {
    let p = doc.dom.get_by_ref(parent)?;
    p.children()
        .iter()
        .copied()
        .find(|c| doc.dom.get_by_ref(*c).is_some_and(|i| i.name == name))
}

/// Whether the place brought its own character sounds, so Roblox's
/// RbxCharacterSounds stays out: a `RbxCharacterSounds` of its own in
/// StarterPlayerScripts (Roblox's rule: a same-named object there replaces
/// the default) or in StarterCharacterScripts (a game that put it there
/// meant the same, and both would play every sound twice), or a fork of the
/// old character `Sound` script in StarterCharacterScripts or its
/// StarterCharacter — "you won't get the new system if you forked the old
/// one" (Roblox staff, devforum "Sound script changes", t/373132).
pub fn own_character_sounds(doc: &Document, sp: Ref, sps: Ref) -> bool {
    if child_named(doc, sps, "RbxCharacterSounds").is_some() {
        return true;
    }
    let scs = doc.find_child(sp, "StarterCharacterScripts");
    let starter = child_named(doc, sp, "StarterCharacter");
    scs.is_some_and(|scs| ["RbxCharacterSounds", "Sound"].iter().any(|n| child_named(doc, scs, n).is_some()))
        || starter.is_some_and(|m| child_named(doc, m, "Sound").is_some())
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

/// Read the instance tree from a Studio project, including standalone subtrees.
fn project_tree(path: &Path) -> Result<serde_json::Value, String> {
    let source = std::fs::read_to_string(path).map_err(|e| format!("{}: {e}", path.display()))?;
    let mut json: serde_json::Value =
        serde_json::from_str(&source).map_err(|e| format!("{}: {e}", path.display()))?;
    json.pointer_mut("/instanceHierarchy/tree")
        .filter(|tree| tree.is_object())
        .map(std::mem::take)
        .ok_or_else(|| format!("{}: missing instanceHierarchy.tree", path.display()))
}

/// Resolve a project node's source before applying its local overrides.
fn project_node(
    name: &str,
    j: &serde_json::Value,
    base: &Path,
    includes: usize,
) -> Result<Node, String> {
    let mut node = match j.get("$path") {
        Some(p) => {
            let path = base.join(p.as_str().ok_or_else(|| format!("{name}: invalid $path"))?);
            if path.extension().is_some_and(|ext| ext == "rbxp") {
                if includes >= 32 {
                    return Err(format!("{}: too many nested project references", path.display()));
                }
                let tree = project_tree(&path)?;
                project_node(name, &tree, path.parent().unwrap_or(base), includes + 1)?
            } else {
                path_node(&path, name)
                    .ok_or_else(|| format!("{}: cannot load project node {name}", path.display()))?
            }
        }
        None => Node::new(
            name,
            if name == "StarterPlayerScripts" { "StarterPlayerScripts" } else { "Folder" },
        ),
    };
    apply_project(&mut node, j, base, includes)?;
    Ok(node)
}

fn apply_project(
    node: &mut Node,
    j: &serde_json::Value,
    base: &Path,
    includes: usize,
) -> Result<(), String> {
    let o = j.as_object().ok_or_else(|| format!("{}: expected a project node", node.name))?;
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
            Some(child) if v.get("$path").is_some() => *child = project_node(k, v, base, includes)?,
            Some(child) => apply_project(child, v, base, includes)?,
            None => node.children.push(project_node(k, v, base, includes)?),
        }
    }
    Ok(())
}

/// Studio's current player scripts: what PlayerScripts.rbxp puts under
/// StarterPlayer (PlayerModule, StarterPlayerScripts' children).
fn project_starter_player(path: &Path, base: &Path) -> Result<Node, String> {
    let tree = project_tree(path)?;
    let starter = tree.get("StarterPlayer")
        .ok_or_else(|| format!("{}: missing StarterPlayer", path.display()))?;
    let sp = project_node("StarterPlayer", starter, base, 0)?;
    let has_loader = sp.children.iter().any(|module| {
        module.name == "PlayerModule" && module.class == "ModuleScript"
            && module.children.iter().any(|loader| {
                loader.name == "PlayerScriptsLoader" && loader.class == "Script"
            })
    });
    if !has_loader {
        return Err(format!("{}: missing PlayerModule or PlayerScriptsLoader", path.display()));
    }
    Ok(sp)
}

/// Workspace.PlayerScriptsUseInputActionSystem (Enum.RolloutState Default
/// 0, Disabled 1, Enabled 2) asks for the Input Action System player
/// scripts: Enabled, or Default once Roblox's rollout makes Default mean
/// that (`mech::PLAYER_SCRIPTS_DEFAULT_IAS`; see the module docs).
fn input_action_system_on(doc: &Document) -> bool {
    let state = doc
        .workspace()
        .and_then(|ws| crate::doc::enum_u32(doc.prop(ws, "PlayerScriptsUseInputActionSystem")))
        .unwrap_or(0);
    match state {
        2 => true,
        1 => false,
        _ => crate::mech::PLAYER_SCRIPTS_DEFAULT_IAS,
    }
}

/// Installs the default player scripts into the StarterPlayer tree. Returns
/// the names inserted.
pub fn insert_player_scripts(doc: &mut Document) -> Result<Vec<&'static str>, String> {
    let Some(root_dir) = player_scripts_dir() else {
        return Ok(insert_own_loader(doc));
    };
    insert_player_scripts_from(doc, &root_dir)
}

/// Roblox's PlayerScriptsLoader in rustudio's words: the client puts this
/// LocalScript next to PlayerModule, and all it does is require the module
/// once, which starts its CameraModule and ControlModule. Used when Studio's
/// content (the real loader) isn't at hand but a PlayerModule is: a place
/// with its own fork of the module in StarterPlayerScripts got no loader at
/// all without the content, nothing required the fork, and its camera and
/// controls never ran while the engine's keyboard stand-in moved the character.
pub const PLAYER_SCRIPTS_LOADER: &str = "\
-- rustudio: stands in for Roblox's PlayerScriptsLoader, which the client\n\
-- adds beside PlayerModule. Requiring the module once starts its camera\n\
-- and control modules.\n\
local playerModule = script.Parent:WaitForChild(\"PlayerModule\")\n\
require(playerModule)\n";

/// Without Studio's content: the loader for a place's own PlayerModule in
/// StarterPlayerScripts (the names Roblox's client checks), when none is
/// there. A place without a module gets nothing (the stand-in controls).
fn insert_own_loader(doc: &mut Document) -> Vec<&'static str> {
    let root = doc.dom.root_ref();
    let Some(sps) = doc
        .find_child(root, "StarterPlayer")
        .and_then(|sp| doc.find_child(sp, "StarterPlayerScripts"))
    else {
        return Vec::new();
    };
    if child_named(doc, sps, "PlayerModule").is_none() || child_named(doc, sps, "PlayerScriptsLoader").is_some() {
        return Vec::new();
    }
    let r = doc.dom.insert(sps, script("LocalScript", "PlayerScriptsLoader", PLAYER_SCRIPTS_LOADER.to_string()));
    doc.mark_tree(r);
    vec!["PlayerScriptsLoader"]
}

fn insert_player_scripts_from(doc: &mut Document, root_dir: &Path) -> Result<Vec<&'static str>, String> {
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
    // The Input Action System layout: StarterPlayer.PlayerModule (see the
    // module docs). CreateDefaultPlayerModule false: the place forked it.
    let create_default = doc.prop(sp, "CreateDefaultPlayerModule") != Some(Variant::Bool(false));
    if input_action_system_on(doc) {
        let project = base.join("PlayerScripts.rbxp");
        let present = project.try_exists().map_err(|e| format!("{}: {e}", project.display()))?;
        if present {
            let tree = project_starter_player(&project, &base)?;
            for child in tree.children {
                if child.name == "StarterPlayerScripts" {
                    for c in child.children {
                        if c.name == "RbxCharacterSounds" && own_character_sounds(doc, sp, sps) {
                            continue;
                        }
                        if child_named(doc, sps, &c.name).is_none() {
                            added.push(if c.name == "RbxCharacterSounds" { "RbxCharacterSounds" } else { "StarterPlayerScripts" });
                            let r = doc.dom.insert(sps, c.builder());
                            doc.mark_tree(r);
                        }
                    }
                } else if child_named(doc, sp, &child.name).is_none()
                    && (create_default || child.name != "PlayerModule")
                {
                    added.push(if child.name == "PlayerModule" { "PlayerModule" } else { "StarterPlayer" });
                    let r = doc.dom.insert(sp, child.builder());
                    doc.mark_tree(r);
                }
            }
            return Ok(added);
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
    add(doc, "PlayerModule", player_module(root_dir));
    // Studio's loader file missing (a partial content folder) with a
    // module to load: rustudio's own loader.
    if child_named(doc, sps, "PlayerModule").is_some() {
        add(doc, "PlayerScriptsLoader", Some(script("LocalScript", "PlayerScriptsLoader", PLAYER_SCRIPTS_LOADER.to_string())));
    }
    let sounds = read(&base.join("StarterPlayerScriptsCommon/RbxCharacterSounds.lua")).map(|s| {
        let mut b = script("LocalScript", "RbxCharacterSounds", s);
        if let Some(ab) =
            read(&base.join("StarterPlayerScriptsCommon/RbxCharacterSounds/AtomicBinding.lua"))
        {
            b = b.with_child(script("ModuleScript", "AtomicBinding", ab));
        }
        b
    });
    if !own_character_sounds(doc, sp, sps) {
        add(doc, "RbxCharacterSounds", sounds);
    }
    Ok(added)
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

    /// A place whose Workspace.PlayerScriptsUseInputActionSystem is `state`
    /// (Enum.RolloutState Default 0, Disabled 1, Enabled 2).
    fn place_with_flag(state: u32) -> Document {
        let mut d = Document::empty();
        let root = d.dom.root_ref();
        d.dom.insert(
            root,
            InstanceBuilder::new("Workspace").with_name("Workspace").with_property(
                "PlayerScriptsUseInputActionSystem",
                rbx_dom_weak::types::Enum::from_u32(state),
            ),
        );
        d
    }

    /// An install with both layouts: the Input Action System project
    /// (PlayerScripts.rbxp + its InputContexts.rbxp) and the classic folders.
    fn install_with_both_layouts(root: &Path) {
        let base = root.join(PLAYER_SCRIPTS);
        let w = |p: PathBuf, s: &str| {
            std::fs::create_dir_all(p.parent().unwrap()).unwrap();
            std::fs::write(p, s).unwrap();
        };
        w(base.join("StarterPlayer/PlayerModule/init.lua"), "return {}");
        w(base.join("StarterPlayer/PlayerModule/PlayerScriptsLoader.client.lua"), "return");
        w(base.join("StarterPlayerScripts/PlayerScriptsLoader.lua"), "-- loader");
        w(base.join("StarterPlayerScripts/PlayerModule.module.lua"), "return {}");
        w(base.join("StarterPlayerScripts/PlayerModule.module/CameraModule.lua"), "return {}");
        w(base.join("StarterPlayerScripts/PlayerModule.module/ControlModule.lua"), "return {}");
        w(base.join("StarterPlayerScriptsCommon/RbxCharacterSounds.lua"), "-- sounds");
        w(base.join("StarterPlayerScriptsCommon/RbxCharacterSounds/AtomicBinding.lua"), "return {}");
        w(base.join("PlayerScripts.rbxp"), r#"{
            "instanceHierarchy": {"tree": {"StarterPlayer": {
                "$path": "StarterPlayer",
                "StarterPlayerScripts": {"RbxCharacterSounds": {"$className": "LocalScript",
                    "$path": "StarterPlayerScriptsCommon/RbxCharacterSounds.lua",
                    "AtomicBinding": {"$path": "StarterPlayerScriptsCommon/RbxCharacterSounds/AtomicBinding.lua"}}},
                "PlayerModule": {"InputContexts": {"$path": "InputContexts.rbxp"}}
            }}}
        }"#);
        w(base.join("InputContexts.rbxp"), r#"{
            "instanceHierarchy": {"tree": {"$className": "Folder",
                "CharacterContext": {"$className": "InputContext",
                    "Jump": {"$className": "InputAction",
                        "Keyboard": {"$className": "InputBinding", "$properties": {"KeyCode": "Space"}}}}
            }}
        }"#);
    }

    /// No Studio content: a place's own PlayerModule in StarterPlayerScripts
    /// still gets a PlayerScriptsLoader, rustudio's, so the fork runs; a place
    /// without a module, or with its own loader, gets nothing; a second call
    /// adds nothing.
    #[test]
    fn own_player_module_gets_a_loader_without_studio_content() {
        let mut d = Document::new_place();
        assert!(insert_own_loader(&mut d).is_empty());
        let sp = d.find_child(d.dom.root_ref(), "StarterPlayer").unwrap();
        let sps = d.find_child(sp, "StarterPlayerScripts").unwrap();
        d.dom.insert(sps, InstanceBuilder::new("ModuleScript").with_name("PlayerModule"));
        assert_eq!(insert_own_loader(&mut d), vec!["PlayerScriptsLoader"]);
        let loader = child_named(&d, sps, "PlayerScriptsLoader").unwrap();
        assert_eq!(d.class_of(loader).as_deref(), Some("LocalScript"));
        assert!(matches!(d.prop(loader, "Source"), Some(Variant::String(s)) if s.contains("require(playerModule)")));
        assert!(insert_own_loader(&mut d).is_empty());
        let mut own = Document::new_place();
        let sp = own.find_child(own.dom.root_ref(), "StarterPlayer").unwrap();
        let sps = own.find_child(sp, "StarterPlayerScripts").unwrap();
        own.dom.insert(sps, InstanceBuilder::new("ModuleScript").with_name("PlayerModule"));
        own.dom.insert(sps, InstanceBuilder::new("LocalScript").with_name("PlayerScriptsLoader"));
        assert!(insert_own_loader(&mut own).is_empty());
    }

    /// Which layout a place gets is Workspace.PlayerScriptsUseInputActionSystem:
    /// Default (a new place, or no Workspace at all) and Disabled are the
    /// classic PlayerScriptsLoader + PlayerModule + RbxCharacterSounds in
    /// StarterPlayerScripts, as Roblox's client gives every place without the
    /// opt-in (rollout phase 1); Enabled is StarterPlayer.PlayerModule, left
    /// out when CreateDefaultPlayerModule is false.
    #[test]
    fn player_scripts_layout_follows_the_rollout_flag() {
        let unique = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos();
        let root = std::env::temp_dir().join(format!("rustudio-player-layout-{}-{unique}", std::process::id()));
        install_with_both_layouts(&root);
        let classic = ["PlayerScriptsLoader", "PlayerModule", "RbxCharacterSounds"];
        let check = |mut d: Document, expect: &[&str], classic_tree: bool| {
            let added = insert_player_scripts_from(&mut d, &root).unwrap();
            assert_eq!(added, expect);
            let sp = d.find_child(d.dom.root_ref(), "StarterPlayer").unwrap();
            let sps = d.find_child(sp, "StarterPlayerScripts").unwrap();
            let n = |p: Ref, name: &str| child_named(&d, p, name);
            let class = |r: Ref| d.class_of(r).map(|c| c.to_string()).unwrap_or_default();
            assert_eq!(n(sps, "PlayerScriptsLoader").is_some(), classic_tree);
            assert_eq!(n(sp, "PlayerModule").is_some(), !classic_tree && expect.contains(&"PlayerModule"));
            if classic_tree && expect.contains(&"PlayerModule") {
                let pm = n(sps, "PlayerModule").unwrap();
                assert_eq!(class(pm), "ModuleScript");
                assert!(n(pm, "CameraModule").is_some() && n(pm, "ControlModule").is_some());
                assert_eq!(class(n(sps, "PlayerScriptsLoader").unwrap()), "LocalScript");
            }
            if !classic_tree && expect.contains(&"PlayerModule") {
                let pm = n(sp, "PlayerModule").unwrap();
                assert_eq!(class(n(pm, "PlayerScriptsLoader").unwrap()), "Script");
                assert!(n(n(pm, "InputContexts").unwrap(), "CharacterContext").is_some());
            }
            let sounds = n(sps, "RbxCharacterSounds").unwrap();
            assert!(n(sounds, "AtomicBinding").is_some());
            // A second call adds nothing.
            assert!(insert_player_scripts_from(&mut d, &root).unwrap().is_empty());
        };
        check(Document::empty(), &classic, true);
        check(place_with_flag(0), &classic, true);
        check(place_with_flag(1), &classic, true);
        check(place_with_flag(2), &["PlayerModule", "RbxCharacterSounds"], false);
        // Enabled, the default module turned off: the place brings its own.
        let mut d = place_with_flag(2);
        let root_ref = d.dom.root_ref();
        d.dom.insert(
            root_ref,
            InstanceBuilder::new("StarterPlayer")
                .with_name("StarterPlayer")
                .with_property("CreateDefaultPlayerModule", false),
        );
        check(d, &["RbxCharacterSounds"], false);
        std::fs::remove_dir_all(root).unwrap();
    }

    /// A place with character sounds of its own gets no RbxCharacterSounds,
    /// in either layout: its own RbxCharacterSounds in StarterPlayerScripts
    /// or StarterCharacterScripts, or a fork of the old character `Sound`
    /// script in StarterCharacterScripts or its StarterCharacter (Roblox:
    /// "you won't get the new system if you forked the old one").
    #[test]
    fn own_character_sounds_keep_roblox_s_out() {
        let unique = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos();
        let root = std::env::temp_dir().join(format!("rustudio-own-sounds-{}-{unique}", std::process::id()));
        install_with_both_layouts(&root);
        let place = |flag: u32, holder: &str, name: &str| {
            let mut d = place_with_flag(flag);
            let root_ref = d.dom.root_ref();
            let folder = |n: &str| {
                let class = if n == "StarterCharacter" { "Model" } else { n };
                let mut b = InstanceBuilder::new(class).with_name(n);
                if n == holder {
                    let script_class = if name == "Sound" { "Script" } else { "LocalScript" };
                    b = b.with_child(InstanceBuilder::new(script_class).with_name(name));
                }
                b
            };
            d.dom.insert(
                root_ref,
                InstanceBuilder::new("StarterPlayer")
                    .with_name("StarterPlayer")
                    .with_child(folder("StarterPlayerScripts"))
                    .with_child(folder("StarterCharacterScripts"))
                    .with_child(folder("StarterCharacter")),
            );
            d
        };
        let roblox_sounds = |d: &Document| {
            let sp = d.find_child(d.dom.root_ref(), "StarterPlayer").unwrap();
            let sps = d.find_child(sp, "StarterPlayerScripts").unwrap();
            child_named(d, sps, "RbxCharacterSounds").filter(|r| child_named(d, *r, "AtomicBinding").is_some())
        };
        for flag in [0, 2] {
            for (holder, name) in [
                ("StarterPlayerScripts", "RbxCharacterSounds"),
                ("StarterCharacterScripts", "RbxCharacterSounds"),
                ("StarterCharacterScripts", "Sound"),
                ("StarterCharacter", "Sound"),
            ] {
                let mut d = place(flag, holder, name);
                let added = insert_player_scripts_from(&mut d, &root).unwrap();
                assert!(!added.contains(&"RbxCharacterSounds"), "{flag} {holder}.{name}: {added:?}");
                assert!(roblox_sounds(&d).is_none(), "{flag} {holder}.{name}");
            }
            // Without any of its own: Roblox's.
            let mut d = place(flag, "StarterCharacterScripts", "Health");
            let added = insert_player_scripts_from(&mut d, &root).unwrap();
            assert!(added.contains(&"RbxCharacterSounds"), "{flag}: {added:?}");
            assert!(roblox_sounds(&d).is_some(), "{flag}");
        }
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn custom_player_module_coexists_with_the_input_action_system() {
        let unique = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let root = std::env::temp_dir().join(format!("rustudio-custom-player-{unique}"));
        let base = root.join(PLAYER_SCRIPTS);
        let module = base.join("StarterPlayer/PlayerModule");
        std::fs::create_dir_all(&module).unwrap();
        std::fs::write(module.join("init.lua"), "return 'default'").unwrap();
        std::fs::write(module.join("PlayerScriptsLoader.client.lua"), "return").unwrap();
        std::fs::write(
            base.join("PlayerScripts.rbxp"),
            r#"{"instanceHierarchy":{"tree":{"StarterPlayer":{"$path":"StarterPlayer"}}}}"#,
        )
        .unwrap();
        std::fs::create_dir(base.join("StarterPlayerScripts")).unwrap();
        std::fs::write(
            base.join("StarterPlayerScripts/PlayerScriptsLoader.lua"),
            "return",
        )
        .unwrap();

        let mut doc = Document::new_place();
        let starter = doc.find_child(doc.dom.root_ref(), "StarterPlayer").unwrap();
        let scripts = doc.find_child(starter, "StarterPlayerScripts").unwrap();
        let custom = doc.dom.insert(
            scripts,
            script("ModuleScript", "PlayerModule", "return 'custom'".into()),
        );

        let mut legacy = doc.clone_for_play("legacy".into());
        let workspace = legacy.workspace().unwrap();
        legacy.dom.get_by_ref_mut(workspace).unwrap().properties.insert(
            "PlayerScriptsUseInputActionSystem".into(),
            Variant::Enum(rbx_dom_weak::types::Enum::from_u32(1)),
        );
        let modern_ws = doc.workspace().unwrap();
        doc.dom.get_by_ref_mut(modern_ws).unwrap().properties.insert(
            "PlayerScriptsUseInputActionSystem".into(),
            Variant::Enum(rbx_dom_weak::types::Enum::from_u32(2)),
        );
        let legacy_added = insert_player_scripts_from(&mut legacy, &root);
        let modern_added = insert_player_scripts_from(&mut doc, &root);
        std::fs::remove_dir_all(root).unwrap();

        assert_eq!(legacy_added.unwrap(), ["PlayerScriptsLoader"]);
        let legacy_starter = legacy.find_child(legacy.dom.root_ref(), "StarterPlayer").unwrap();
        assert!(child_named(&legacy, legacy_starter, "PlayerModule").is_none());
        assert_eq!(modern_added.unwrap(), ["PlayerModule"]);
        assert_eq!(child_named(&doc, scripts, "PlayerModule"), Some(custom));
        assert!(child_named(&doc, scripts, "PlayerScriptsLoader").is_none());
        let default_module = child_named(&doc, starter, "PlayerModule").unwrap();
        assert!(child_named(&doc, default_module, "PlayerScriptsLoader").is_some());
    }

    #[test]
    fn project_imports_input_contexts_with_properties_and_rejects_missing_sources() {
        let unique = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos();
        let root = std::env::temp_dir().join(format!("rustudio-player-project-{}-{unique}", std::process::id()));
        let base = root.join(PLAYER_SCRIPTS);
        let module = base.join("StarterPlayer/PlayerModule");
        std::fs::create_dir_all(&module).unwrap();
        std::fs::write(module.join("init.lua"), "return {}").unwrap();
        std::fs::write(module.join("PlayerScriptsLoader.client.lua"), "return").unwrap();
        std::fs::write(base.join("PlayerScripts.rbxp"), r#"{
            "instanceHierarchy": {"tree": {"StarterPlayer": {
                "$path": "StarterPlayer",
                "PlayerModule": {
                    "InputContexts": {"$path": "input/InputContexts.rbxp",
                        "CharacterContext": {"$properties": {"Priority": 250}}}
                }
            }}}
        }"#).unwrap();

        let mut doc = place_with_flag(2);
        let err = insert_player_scripts_from(&mut doc, &root).unwrap_err();
        let sp = doc.find_child(doc.dom.root_ref(), "StarterPlayer").unwrap();
        assert!(err.contains("InputContexts.rbxp"));
        assert!(child_named(&doc, sp, "PlayerModule").is_none());

        let input = base.join("input");
        std::fs::create_dir(&input).unwrap();
        std::fs::write(input.join("InputContexts.rbxp"), r#"{
            "instanceHierarchy": {"tree": {"$className": "Folder",
                "CharacterContext": {"$className": "InputContext",
                    "$properties": {"Priority": 150, "Sink": true},
                    "Move": {"$className": "InputAction",
                        "$properties": {"Type": "Direction2D", "Enabled": false},
                        "Keyboard": {"$className": "InputBinding",
                            "$properties": {"Up": "W"}},
                        "Gamepad": {"$className": "InputBinding",
                            "$properties": {"KeyCode": "Thumbstick1", "Vector2Scale": [2, -3]}}
                    }
                }
            }}
        }"#).unwrap();
        let result = insert_player_scripts_from(&mut doc, &root);
        std::fs::remove_dir_all(root).unwrap();
        assert_eq!(result.unwrap(), vec!["PlayerModule"]);
        let child = |r, name| child_named(&doc, r, name).unwrap();
        let pm = child(sp, "PlayerModule");
        let ctx = child(child(pm, "InputContexts"), "CharacterContext");
        assert_eq!(doc.prop(ctx, "Priority"), Some(Variant::Int32(250)));
        assert_eq!(doc.prop(ctx, "Sink"), Some(Variant::Bool(true)));
        let action = child(ctx, "Move");
        assert_eq!(doc.prop(action, "Enabled"), Some(Variant::Bool(false)));
        let enum_of = |r, prop| crate::doc::enum_u32(doc.prop(r, prop));
        let key = |name| crate::doc::enum_item_value("KeyCode", name);
        assert_eq!(enum_of(child(action, "Keyboard"), "Up"), key("W"));
        let gamepad = child(action, "Gamepad");
        assert_eq!(enum_of(gamepad, "KeyCode"), key("Thumbstick1"));
        assert_eq!(doc.prop(gamepad, "Vector2Scale"), Some(Variant::Vector2(
            rbx_dom_weak::types::Vector2::new(2.0, -3.0),
        )));
    }

    #[test]
    fn default_scripts_from_studio_folder() {
        let Some(_) = studio_dir() else { return }; // no Studio install on this machine
        fn len(d: &Document, r: Ref) -> usize {
            match d.prop(r, "Source") {
                Some(Variant::String(s)) => s.len(),
                _ => 0,
            }
        }
        // A new place (PlayerScriptsUseInputActionSystem Default): the
        // classic PlayerScripts, what Roblox's client gives it today.
        let mut d = Document::empty();
        let added = insert_player_scripts(&mut d).unwrap();
        assert_eq!(added, vec!["PlayerScriptsLoader", "PlayerModule", "RbxCharacterSounds"]);
        {
            let root = d.dom.root_ref();
            let sp = d.find_child(root, "StarterPlayer").unwrap();
            let sps = d.find_child(sp, "StarterPlayerScripts").unwrap();
            let n = |p: Ref, name: &str| child_named(&d, p, name);
            let class = |r: Ref| d.class_of(r).map(|c| c.to_string()).unwrap_or_default();
            assert!(n(sp, "PlayerModule").is_none());
            assert_eq!(class(n(sps, "PlayerScriptsLoader").unwrap()), "LocalScript");
            let pm = n(sps, "PlayerModule").unwrap();
            assert_eq!(class(pm), "ModuleScript");
            let cam = n(pm, "CameraModule").unwrap();
            assert!(n(cam, "ZoomController").and_then(|z| n(z, "Popper")).is_some());
            assert!(n(cam, "VRCameraTeleportDetector.spec").is_none());
            assert_eq!(class(n(pm, "ControlModule").unwrap()), "ModuleScript");
            // (A Folder in this layout: OldPlayerScripts.rbxp's CommonUtils.)
            let utils = n(pm, "CommonUtils").unwrap();
            assert_eq!(class(utils), "Folder");
            assert_eq!(class(n(utils, "CharacterUtil").unwrap()), "ModuleScript");
            assert!(n(pm, "InputContexts").is_none() && n(pm, "PlayerScriptsLoader").is_none());
            assert!(n(n(sps, "RbxCharacterSounds").unwrap(), "AtomicBinding").is_some());
            assert!(len(&d, pm) > 100 && len(&d, cam) > 20_000);
            assert!(insert_player_scripts(&mut d).unwrap().is_empty());
        }
        // The flag Enabled: the Input Action System layout.
        let mut d = place_with_flag(2);
        let added = insert_player_scripts(&mut d).unwrap();
        assert_eq!(added, vec!["PlayerModule", "RbxCharacterSounds"]);
        let root = d.dom.root_ref();
        let sp = d.find_child(root, "StarterPlayer").unwrap();
        let sps = d.find_child(sp, "StarterPlayerScripts").unwrap();
        let n = |p: Ref, name: &str| child_named(&d, p, name);
        let class = |r: Ref| d.class_of(r).map(|c| c.to_string()).unwrap_or_default();
        let enum_of = |r: Ref, k: &str| crate::doc::enum_u32(d.prop(r, k));
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
        let key = |name: &str| crate::doc::enum_item_value("KeyCode", name);
        assert_eq!(enum_of(kb, "Up"), key("W"));
        assert_eq!(enum_of(kb, "Left"), key("A"));
        assert!(len(&d, pm) > 100 && len(&d, cam) > 20_000);
        // A second call keeps what is there.
        assert!(insert_player_scripts(&mut d).unwrap().is_empty());
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
