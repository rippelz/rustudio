//! Roblox's CoreScripts and CorePackages from Studio's data model patch,
//! `ExtraContent/models/DataModelPatch/DataModelPatch.rbxm` (also in the CDN
//! package `extracontent-models.zip` that studio_content.rs downloads).
//!
//! The patch is one binary model (version-8d7a1009c17e4ff8: 17 MB, 4082
//! Folders, 47181 ModuleScripts, 2 LocalizationTables, 5 StringValues):
//!
//! - `PatchRoot.DataModelInstances.CoreGui.RobloxGui.Modules`: the CoreGui
//!   modules (TopBar, Settings, PlayerList, Chrome, Flags…),
//! - `PatchRoot.DataModelInstances.CorePackages.{Packages, Workspace}`: the
//!   CorePackages service's contents (Roact, React, UIBlox, ExpChat…),
//! - `PatchRoot.CoreScripts`: the scripts the engine starts by name:
//!   `StarterScript`, `LoadingScript`, `Connection`, `ServerStarterScript`,
//!   `CoreScripts/<Name>` (what `ScriptContext:AddCoreScriptLocal` runs), the
//!   `ServerCoreScripts/<Name>` and the `CoreScriptLocalization` tables,
//! - `PatchRoot.PlayerScripts`: the default PlayerModule (rustudio reads the
//!   text copy in `ExtraContent/scripts/PlayerScripts` instead).
//!
//! Every script's `Source` is a ProtectedString (binary property type
//! `0x1d`) holding compiled Luau bytecode (version 9, types version 3) with
//! no header and plain opcodes; rbx_binary skips that property type, so
//! [`decode`] turns those PROP chunks into plain strings first and keeps the
//! bytecode beside the tree.
//!
//! Where things go ([`install`]) follows the patch's own layout and the names
//! Roblox's clients report: errors name `CoreGui.RobloxGui.Modules.<…>`,
//! `CorePackages.Packages._Index.<…>`,
//! `CorePackages.Workspace.Packages._Workspace.<…>` and
//! `CoreGui.RobloxGui.CoreScripts/<Name>`, while the engine-started scripts
//! report bare chunk names (`Script 'StarterScript'`, `'LoadingScript'`,
//! `'Connection'`), and CoreScripts look up `CoreGui.CoreScriptLocalization`
//! (devforum.roblox.com/t/2659679 and /t/2584693).
//! A Folder in `DataModelInstances` names the instance at its path (the
//! CoreGui and CorePackages services, the engine's RobloxGui ScreenGui:
//! StarterScript waits for `CoreGui.RobloxGui` and reads its
//! `AbsoluteSize`); what is not there yet is moved in.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use rbx_dom_weak::types::{Ref, Variant};
use rbx_dom_weak::{InstanceBuilder, WeakDom};

use crate::doc::Document;

/// The patch, relative to the Studio root.
pub const PATCH: &str = "ExtraContent/models/DataModelPatch/DataModelPatch.rbxm";

/// Compiled Luau, as Roblox ships it.
pub type Bytecode = Arc<[u8]>;

/// The patch file of the Studio install default_scripts reads, when there.
pub fn patch_path() -> Option<PathBuf> {
    crate::default_scripts::studio_dir()
        .map(|d| d.join(PATCH))
        .filter(|p| p.is_file())
}

/// The patch's tree, with every script's bytecode taken out of `Source`.
pub struct Patch {
    pub dom: WeakDom,
    pub bytecode: HashMap<Ref, Bytecode>,
}

/// What [`install`] put in a DataModel.
#[derive(Default, Clone)]
pub struct Installed {
    /// The compiled Source of every ModuleScript placed in the DataModel.
    pub modules: HashMap<Ref, Bytecode>,
    /// `PatchRoot.CoreScripts` by name: the scripts the engine starts
    /// (`StarterScript`) and `ScriptContext:AddCoreScriptLocal` runs.
    pub scripts: HashMap<String, Bytecode>,
}

pub fn read(path: &Path) -> Result<Patch, String> {
    let bytes = std::fs::read(path).map_err(|e| format!("{}: {e}", path.display()))?;
    decode(&bytes)
}

const SOURCE_MARK: &str = "\0rustudio-bytecode:";
const PROTECTED_STRING: u8 = 0x1d;
const STRING: u8 = 0x01;

/// Reads a binary model whose script sources are ProtectedStrings.
pub fn decode(file: &[u8]) -> Result<Patch, String> {
    let (rewritten, sources) = protected_to_marks(file)?;
    let mut dom = rbx_binary::from_reader(&rewritten[..]).map_err(|e| e.to_string())?;
    let mut bytecode = HashMap::with_capacity(sources.len());
    let key = rbx_dom_weak::ustr("Source");
    let refs: Vec<Ref> = dom.descendants().map(|i| i.referent()).collect();
    for r in refs {
        let Some(i) = dom.get_by_ref_mut(r) else { continue };
        let index = match i.properties.get(&key) {
            Some(Variant::String(s)) => s.strip_prefix(SOURCE_MARK).and_then(|n| n.parse::<usize>().ok()),
            _ => None,
        };
        if let Some(n) = index {
            i.properties.remove(&key);
            if let Some(b) = sources.get(n) {
                bytecode.insert(r, b.clone());
            }
        }
    }
    Ok(Patch { dom, bytecode })
}

struct Bytes<'a>(&'a [u8]);

impl<'a> Bytes<'a> {
    fn take(&mut self, n: usize) -> Result<&'a [u8], String> {
        if n > self.0.len() {
            return Err("binary model truncated".into());
        }
        let (a, b) = self.0.split_at(n);
        self.0 = b;
        Ok(a)
    }
    fn u32(&mut self) -> Result<u32, String> {
        Ok(u32::from_le_bytes(self.take(4)?.try_into().unwrap()))
    }
    fn string(&mut self) -> Result<&'a [u8], String> {
        let n = self.u32()? as usize;
        self.take(n)
    }
}

/// Copies the file with each `Source` PROP chunk of ProtectedStrings
/// replaced by an uncompressed chunk of plain strings `SOURCE_MARK<n>`,
/// `n` indexing the returned bytecode.
fn protected_to_marks(file: &[u8]) -> Result<(Vec<u8>, Vec<Bytecode>), String> {
    if !file.starts_with(b"<roblox!") || file.len() < 32 {
        return Err("not a binary Roblox model".into());
    }
    let mut out = Vec::with_capacity(file.len());
    out.extend_from_slice(&file[..32]);
    let mut counts: HashMap<u32, usize> = HashMap::new();
    let mut sources = Vec::new();
    let mut rest = Bytes(&file[32..]);
    loop {
        let header = rest.take(16)?;
        let name: [u8; 4] = header[..4].try_into().unwrap();
        let compressed = u32::from_le_bytes(header[4..8].try_into().unwrap()) as usize;
        let len = u32::from_le_bytes(header[8..12].try_into().unwrap()) as usize;
        let payload = rest.take(if compressed == 0 { len } else { compressed })?;
        let data = || -> Result<Vec<u8>, String> {
            if compressed == 0 {
                Ok(payload.to_vec())
            } else if payload.starts_with(&[0x28, 0xb5, 0x2f, 0xfd]) {
                zstd::bulk::decompress(payload, len).map_err(|e| e.to_string())
            } else {
                lz4_flex::block::decompress(payload, len).map_err(|e| e.to_string())
            }
        };
        let mut replaced = None;
        match &name {
            b"INST" => {
                let data = data()?;
                let mut b = Bytes(&data);
                let type_id = b.u32()?;
                b.string()?;
                b.take(1)?;
                counts.insert(type_id, b.u32()? as usize);
            }
            b"PROP" => {
                let data = data()?;
                let mut b = Bytes(&data);
                let type_id = b.u32()?;
                let prop = b.string()?;
                if prop == b"Source" && b.0.first() == Some(&PROTECTED_STRING) {
                    b.take(1)?;
                    let n = counts.get(&type_id).copied().ok_or("PROP chunk before its INST chunk")?;
                    let mut body = Vec::with_capacity(16 + n * 32);
                    body.extend_from_slice(&type_id.to_le_bytes());
                    body.extend_from_slice(&(prop.len() as u32).to_le_bytes());
                    body.extend_from_slice(prop);
                    body.push(STRING);
                    for _ in 0..n {
                        let mark = format!("{SOURCE_MARK}{}", sources.len());
                        sources.push(Bytecode::from(b.string()?));
                        body.extend_from_slice(&(mark.len() as u32).to_le_bytes());
                        body.extend_from_slice(mark.as_bytes());
                    }
                    replaced = Some(body);
                }
            }
            _ => {}
        }
        match replaced {
            Some(body) => {
                out.extend_from_slice(&name);
                out.extend_from_slice(&0u32.to_le_bytes());
                out.extend_from_slice(&(body.len() as u32).to_le_bytes());
                out.extend_from_slice(&0u32.to_le_bytes());
                out.extend_from_slice(&body);
            }
            None => {
                out.extend_from_slice(header);
                out.extend_from_slice(payload);
            }
        }
        if &name == b"END\0" {
            return Ok((out, sources));
        }
    }
}

fn named(dom: &WeakDom, parent: Ref, name: &str) -> Option<Ref> {
    dom.get_by_ref(parent)?
        .children()
        .iter()
        .copied()
        .find(|c| dom.get_by_ref(*c).is_some_and(|i| i.name == name))
}

/// Puts the patch in a client DataModel: `DataModelInstances` into the
/// services they name (CoreGui gets its `RobloxGui` ScreenGui first, as the
/// engine makes it), the `CoreScriptLocalization` tables under CoreGui, and
/// `PatchRoot.CoreScripts` aside by name.
pub fn install(doc: &mut Document, mut patch: Patch) -> Result<Installed, String> {
    let patch_root = named(&patch.dom, patch.dom.root_ref(), "PatchRoot").ok_or("the patch has no PatchRoot")?;
    // (A patch without DataModelInstances: its root's children stand for
    // the services.)
    let instances = named(&patch.dom, patch_root, "DataModelInstances").unwrap_or(patch_root);
    let root = doc.dom.root_ref();
    let service = |doc: &mut Document, class: &str| {
        doc.find_child(root, class)
            .unwrap_or_else(|| doc.dom.insert(root, InstanceBuilder::new(class).with_name(class)))
    };
    let core_gui = service(doc, "CoreGui");
    if named(&doc.dom, core_gui, "RobloxGui").is_none() {
        // ZIndexBehavior Sibling: the settings menu's Leave / Respawn /
        // Resume row (React-built at ZIndex 1 inside its ZIndex 2
        // MenuContainer) draws over the menu's background in the real
        // client; Global, Instance.new's default, drew it underneath.
        doc.dom.insert(
            core_gui,
            InstanceBuilder::new("ScreenGui")
                .with_name("RobloxGui")
                .with_property("ResetOnSpawn", false)
                .with_property("ZIndexBehavior", Variant::Enum(rbx_dom_weak::types::Enum::from_u32(1))),
        );
    }
    let tops: Vec<Ref> = patch.dom.get_by_ref(instances).map(|i| i.children().to_vec()).unwrap_or_default();
    let mut touched = vec![core_gui];
    for top in tops {
        let name = patch.dom.get_by_ref(top).map(|i| i.name.clone()).unwrap_or_default();
        if !crate::doc::is_service(&name) {
            return Err(format!("DataModelInstances.{name} is not a service"));
        }
        let target = service(doc, &name);
        merge(&mut patch.dom, top, doc, target);
        touched.push(target);
    }
    let mut installed = Installed::default();
    if let Some(scripts) = named(&patch.dom, patch_root, "CoreScripts") {
        let kids = patch.dom.get_by_ref(scripts).map(|i| i.children().to_vec()).unwrap_or_default();
        for k in kids {
            let Some(i) = patch.dom.get_by_ref(k) else { continue };
            if i.class == "LocalizationTable" {
                patch.dom.transfer(k, &mut doc.dom, core_gui);
            } else if let Some(b) = patch.bytecode.remove(&k) {
                installed.scripts.insert(i.name.clone(), b);
            }
        }
    }
    installed.modules = patch.bytecode.into_iter().filter(|(r, _)| doc.dom.get_by_ref(*r).is_some()).collect();
    for r in touched {
        doc.mark_tree(r);
    }
    Ok(installed)
}

/// Moves `src`'s children under `dst`: a Folder whose name is already a
/// child there stands for that child and merges into it.
fn merge(patch: &mut WeakDom, src: Ref, doc: &mut Document, dst: Ref) {
    let kids = patch.get_by_ref(src).map(|i| i.children().to_vec()).unwrap_or_default();
    for k in kids {
        let Some(i) = patch.get_by_ref(k) else { continue };
        match (i.class.as_str(), named(&doc.dom, dst, &i.name)) {
            ("Folder", Some(existing)) => merge(patch, k, doc, existing),
            _ => patch.transfer(k, &mut doc.dom, dst),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn path_of(d: &Document, mut r: Ref) -> String {
        let mut parts = Vec::new();
        while r != d.dom.root_ref() {
            let Some(i) = d.dom.get_by_ref(r) else { break };
            parts.push(i.name.clone());
            r = i.parent();
        }
        parts.reverse();
        parts.join(".")
    }

    /// A small model with ProtectedString sources, in the patch's layout,
    /// written the way Roblox writes it (rbx_binary, then the Source chunk
    /// switched to type 0x1d with raw bytecode).
    fn tiny_patch() -> Vec<u8> {
        let mut dom = WeakDom::new(InstanceBuilder::new("DataModel"));
        let root = dom.root_ref();
        let pr = dom.insert(root, InstanceBuilder::new("Folder").with_name("PatchRoot"));
        let dmi = dom.insert(pr, InstanceBuilder::new("Folder").with_name("DataModelInstances"));
        let cg = dom.insert(dmi, InstanceBuilder::new("Folder").with_name("CoreGui"));
        let rg = dom.insert(cg, InstanceBuilder::new("Folder").with_name("RobloxGui"));
        let modules = dom.insert(rg, InstanceBuilder::new("Folder").with_name("Modules"));
        let module = |name: &str, n: u8| InstanceBuilder::new("ModuleScript").with_name(name).with_property("Source", format!("#{n}"));
        dom.insert(modules, module("TopBar", 1));
        let cp = dom.insert(dmi, InstanceBuilder::new("Folder").with_name("CorePackages"));
        let pk = dom.insert(cp, InstanceBuilder::new("Folder").with_name("Packages"));
        dom.insert(pk, module("Roact", 2));
        let cs = dom.insert(pr, InstanceBuilder::new("Folder").with_name("CoreScripts"));
        dom.insert(cs, module("StarterScript", 3));
        dom.insert(cs, module("CoreScripts/ExperienceChatMain", 4));
        dom.insert(cs, InstanceBuilder::new("LocalizationTable").with_name("CoreScriptLocalization"));
        let tops: Vec<Ref> = dom.root().children().to_vec();
        let mut bin = Vec::new();
        rbx_binary::to_writer(&mut bin, &dom, &tops).unwrap();
        // Uncompress every chunk, then make the Source chunk ProtectedString
        // with bytecode [9, 3, n, 0xff] (not UTF-8).
        let mut out = bin[..32].to_vec();
        let mut rest = &bin[32..];
        loop {
            let name: [u8; 4] = rest[..4].try_into().unwrap();
            let c = u32::from_le_bytes(rest[4..8].try_into().unwrap()) as usize;
            let len = u32::from_le_bytes(rest[8..12].try_into().unwrap()) as usize;
            let payload = &rest[16..16 + if c == 0 { len } else { c }];
            rest = &rest[16 + payload.len()..];
            let mut data = match c {
                0 => payload.to_vec(),
                _ if payload.starts_with(&[0x28, 0xb5, 0x2f, 0xfd]) => zstd::bulk::decompress(payload, len).unwrap(),
                _ => lz4_flex::block::decompress(payload, len).unwrap(),
            };
            if &name == b"PROP" && data[8..14] == *b"Source" {
                let mut b = data[..14].to_vec();
                b.push(PROTECTED_STRING);
                let mut s = Bytes(&data[15..]);
                while !s.0.is_empty() {
                    let text = s.string().unwrap();
                    let n = text[1] - b'0';
                    b.extend_from_slice(&4u32.to_le_bytes());
                    b.extend_from_slice(&[9, 3, n, 0xff]);
                }
                data = b;
            }
            out.extend_from_slice(&name);
            out.extend_from_slice(&0u32.to_le_bytes());
            out.extend_from_slice(&(data.len() as u32).to_le_bytes());
            out.extend_from_slice(&0u32.to_le_bytes());
            out.extend_from_slice(&data);
            if &name == b"END\0" {
                return out;
            }
        }
    }

    #[test]
    fn installs_the_patch_where_roblox_runs_it() {
        let patch = decode(&tiny_patch()).unwrap();
        assert_eq!(patch.bytecode.len(), 4);
        let mut d = Document::empty();
        let installed = install(&mut d, patch).unwrap();
        let root = d.dom.root_ref();
        let cg = d.find_child(root, "CoreGui").unwrap();
        let rg = crate::default_scripts::child_named(&d, cg, "RobloxGui").unwrap();
        assert_eq!(d.class_of(rg).map(|c| c.to_string()).as_deref(), Some("ScreenGui"));
        assert!(crate::default_scripts::child_named(&d, cg, "CoreScriptLocalization").is_some());
        let cp = d.find_child(root, "CorePackages").unwrap();
        let mut modules: Vec<(String, u8)> = installed
            .modules
            .iter()
            .map(|(r, b)| (path_of(&d, *r), b[2]))
            .collect();
        modules.sort();
        assert_eq!(
            modules,
            vec![("CoreGui.RobloxGui.Modules.TopBar".into(), 1), ("CorePackages.Packages.Roact".into(), 2)]
        );
        assert!(d.dom.get_by_ref(cp).is_some());
        // The script sources are out of the tree.
        let source = rbx_dom_weak::ustr("Source");
        assert!(installed.modules.keys().all(|r| !d.dom.get_by_ref(*r).unwrap().properties.contains_key(&source)));
        let mut scripts: Vec<(&str, u8)> = installed.scripts.iter().map(|(k, b)| (k.as_str(), b[2])).collect();
        scripts.sort();
        assert_eq!(scripts, vec![("CoreScripts/ExperienceChatMain", 4), ("StarterScript", 3)]);
    }

    #[test]
    fn studio_patch_has_the_core_scripts() {
        let Some(path) = patch_path() else { return }; // no Studio install on this machine
        let patch = read(&path).unwrap();
        let mut d = Document::empty();
        let installed = install(&mut d, patch).unwrap();
        // Luau bytecode version 3..=14, what the vendored Luau loads.
        for b in installed.scripts.values().chain(installed.modules.values()) {
            assert!((3..=14).contains(&b[0]), "bytecode version {}", b[0]);
        }
        assert!(installed.scripts.contains_key("StarterScript"));
        assert!(installed.scripts.contains_key("CoreScripts/ExperienceChatMain"));
        let root = d.dom.root_ref();
        let cg = d.find_child(root, "CoreGui").unwrap();
        let rg = crate::default_scripts::child_named(&d, cg, "RobloxGui").unwrap();
        let modules = crate::default_scripts::child_named(&d, rg, "Modules").unwrap();
        assert!(crate::default_scripts::child_named(&d, modules, "TopBar").is_some());
        let cp = d.find_child(root, "CorePackages").unwrap();
        for name in ["Packages", "Workspace"] {
            assert!(crate::default_scripts::child_named(&d, cp, name).is_some(), "CorePackages.{name}");
        }
        assert!(installed.modules.len() > 10_000);
    }
}
