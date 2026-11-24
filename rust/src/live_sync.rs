//! Turns the editor's change journal into Team Create edits in an order the
//! server accepts.
//!
//! A plugin (or the person) can make hundreds of changes in one frame, in any
//! order: Instance.new + set Parent bottom-up, move an old script's children
//! into its replacement, Destroy the old one, parent a cloned folder tree. The
//! journal only says "this subtree changed", so each frame's marks are planned
//! as a whole against the document's final state:
//!
//! 1. Services the editor made locally (a plugin's GetService) bind to the
//!    server's listed ones (`LiveModel::unbound_service`).
//! 2. Creates: each new subtree from its topmost new instance, parents before
//!    children. An instance the server already has, found inside a new tree,
//!    is moved there instead of being created again.
//! 3. Moves of instances the server has, now that their parents exist.
//! 4. Property changes of instances the server already had.
//! 5. Deletes, deepest first: an instance removed from the place (Destroy
//!    leaves it parentless in the document; Explorer deletes put it in the
//!    undo limbo) is deleted on the server. Its subtree's referents are
//!    forgotten, so putting it back creates it anew.
//!
//! Anything that cannot be sent is reported by path, never silently dropped.

use rbx_types::{Ref, Variant};
use rustudio_core::doc::{Document, Mark};
use rustudio_net::model::LiveModel;
use rustudio_net::replicator::PropValue;
use std::collections::{HashMap, HashSet};

/// One edit for the server, in send order.
#[derive(Debug)]
pub enum LiveOp {
    Create {
        reference: Ref,
        id: u64,
        class: String,
        parent: u64,
        props: Vec<(String, PropValue)>,
    },
    Reparent { reference: Ref, id: u64, parent: u64 },
    Property { reference: Ref, id: u64, class: String, name: String, value: PropValue },
    /// A script Source edit: it goes through the draft store, not a property.
    Source { id: u64, text: String },
    Delete { reference: Ref, id: u64 },
}

#[derive(Default, Debug)]
pub struct Plan {
    pub ops: Vec<LiveOp>,
    /// Edits that cannot be sent (saving stays blocked), by instance path.
    pub unsynced: Vec<String>,
    /// Properties left out of a create (the rest of the instance is sent).
    pub skipped: Vec<String>,
}

/// Plans one frame's journal marks. `allocate` hands out new instance
/// referents in this session's namespace.
pub fn plan(
    doc: &Document,
    model: &mut LiveModel,
    marks: &[Mark],
    allocate: &mut dyn FnMut() -> Result<u64, String>,
) -> Plan {
    let mut out = Plan::default();
    bind_services(doc, model);
    let root = doc.dom.root_ref();
    let mut trees = Vec::new();
    let mut seen = HashSet::new();
    for mark in marks {
        if let Mark::Tree(r) = *mark {
            if seen.insert(r) && !is_local_live_instance(doc, model, r) {
                trees.push(r);
            }
        }
    }
    let mut create_roots = Vec::new();
    let mut moves = Vec::new();
    let mut deletes = Vec::new();
    for &r in &trees {
        if !in_place(doc, r) {
            if model.server_id(r).is_some() {
                deletes.push(r);
            } else if model.lost_referent(r) {
                out.unsynced.push(format!(
                    "{}: removed here, but it is still on the Team Create server: another instance has its server referent, so it can't be deleted by it",
                    path(doc, r)
                ));
            }
        } else if model.server_id(r).is_some() {
            moves.push(r);
        } else {
            // The topmost new ancestor: the whole new tree goes from there.
            // Never above a DataModel child: services aren't NewObjects, so
            // one the server doesn't list leaves its new tree reported below.
            let mut top = r;
            loop {
                let parent = doc.dom.get_by_ref(top).map_or(Ref::none(), |i| i.parent());
                let grandparent = doc.dom.get_by_ref(parent).map_or(Ref::none(), |i| i.parent());
                if parent == root || grandparent == root || parent.is_none() || model.server_id(parent).is_some() {
                    break;
                }
                top = parent;
            }
            if doc.dom.get_by_ref(top).is_some_and(|i| i.parent() == root) {
                out.unsynced.push(format!("{}: a service the Team Create server does not have", path(doc, top)));
                continue;
            }
            if !create_roots.contains(&top) {
                create_roots.push(top);
            }
        }
    }
    let mut created = HashSet::new();
    // Reference properties of new instances, sent once every create is out.
    let mut deferred: Vec<(Ref, String)> = Vec::new();
    for top in create_roots {
        let Some(parent) = doc.dom.get_by_ref(top).and_then(|i| model.server_id(i.parent())) else {
            out.unsynced.push(format!(
                "{}: its parent is not on the Team Create server (a service it does not have?)",
                path(doc, top)
            ));
            continue;
        };
        let mut queue = std::collections::VecDeque::from([(top, parent)]);
        while let Some((r, parent)) = queue.pop_front() {
            let Some(inst) = doc.dom.get_by_ref(r) else { continue };
            if r != top && model.server_id(r).is_some() {
                // Moved into the new tree (a replaced script's old children).
                moves.push(r);
                continue;
            }
            // A referent the server already uses would take it from that
            // instance (the session's allocator should never hand one out).
            let id = loop {
                match allocate() {
                    Ok(id) if model.reference(id).is_some() => {
                        log::warn!("Team Create: skipped referent {id:#x}, which an instance already has");
                    }
                    other => break other,
                }
            };
            let id = match id {
                Ok(id) => id,
                Err(error) => {
                    out.unsynced.push(format!("{}: {error}", path(doc, r)));
                    continue;
                }
            };
            model.bind_local(id, r);
            created.insert(r);
            let props = create_props(doc, model, r, &mut out.skipped, &mut deferred);
            out.ops.push(LiveOp::Create { reference: r, id, class: inst.class.to_string(), parent, props });
            queue.extend(inst.children().iter().map(|&c| (c, id)));
        }
    }
    let mut moved = HashSet::new();
    for r in moves {
        if !moved.insert(r) {
            continue;
        }
        let (Some(id), Some(inst)) = (model.server_id(r), doc.dom.get_by_ref(r)) else { continue };
        match model.server_id(inst.parent()) {
            Some(parent) => out.ops.push(LiveOp::Reparent { reference: r, id, parent }),
            None => out.unsynced.push(format!("{}: moved under an instance that is not on the server", path(doc, r))),
        }
    }
    // A NewObject carrying any Ref-typed property (Motor6D.Part0,
    // ObjectValue.Value, even a nil one) resets the session (0x104), while
    // the same value sent as a property change after the create is applied
    // (live probes on a test place, 2026-11-21). Sent here, after every
    // create, they can also name instances created later in the same tree
    // (a Model's PrimaryPart).
    for (r, name) in deferred {
        let (Some(id), Some(inst)) = (model.server_id(r), doc.dom.get_by_ref(r)) else { continue };
        match doc.prop(r, &name).as_ref().and_then(|v| live_value(v, model)) {
            Some(value) => out.ops.push(LiveOp::Property { reference: r, id, class: inst.class.to_string(), name, value }),
            None => out.unsynced.push(format!("{}.{name}: refers to an instance that is not on the Team Create server", path(doc, r))),
        }
    }
    let mut props_seen = HashSet::new();
    for mark in marks {
        let Mark::Prop(r, name) = *mark else { continue };
        if created.contains(&r) || !props_seen.insert((r, name)) || !in_place(doc, r) {
            continue;
        }
        let (Some(id), Some(inst)) = (model.server_id(r), doc.dom.get_by_ref(r)) else { continue };
        if is_local_live_instance(doc, model, r) {
            continue;
        }
        // Workspace.CurrentCamera naming this client's own camera.
        if name.as_str() == "CurrentCamera" && local_camera(doc, model).is_some_and(|c| doc.prop(r, "CurrentCamera") == Some(Variant::Ref(c))) {
            continue;
        }
        // Not replicated (the server's schema has only TimeOfDay, its twin,
        // which is marked with it): sending it would fail Lighting's edits.
        if name.as_str() == "ClockTime" && inst.class.as_str() == "Lighting" {
            continue;
        }
        if server_owned(inst.class.as_str(), name.as_str()) {
            continue;
        }
        if name.as_str() == "Source" {
            if let Some(Variant::String(text)) = doc.prop(r, "Source") {
                out.ops.push(LiveOp::Source { id, text });
            }
            continue;
        }
        let value = if name.as_str() == "Name" { Some(Variant::String(inst.name.clone())) } else { doc.prop(r, name.as_str()) };
        match value.as_ref().and_then(|v| live_value(v, model)) {
            Some(value) => out.ops.push(LiveOp::Property {
                reference: r,
                id,
                class: inst.class.to_string(),
                name: name.to_string(),
                value,
            }),
            None => out.unsynced.push(format!("{}.{name}: Team Create cannot synchronize this value yet", path(doc, r))),
        }
    }
    // Deepest first, so a child is never named after its server parent went.
    let depth = |r: Ref| std::iter::successors(doc.dom.get_by_ref(r), |i| doc.dom.get_by_ref(i.parent())).count();
    deletes.sort_by_key(|&r| std::cmp::Reverse(depth(r)));
    for r in deletes {
        let Some(id) = model.server_id(r) else { continue };
        out.ops.push(LiveOp::Delete { reference: r, id });
        for d in doc.subtree(r) {
            model.unbind(d);
        }
    }
    out
}

/// A service the editor made itself (GetService) that the server lists.
fn bind_services(doc: &Document, model: &mut LiveModel) {
    let unbound: Vec<_> = doc.dom.root().children().iter().copied().filter(|&c| model.server_id(c).is_none()).collect();
    for c in unbound {
        let Some(class) = doc.dom.get_by_ref(c).map(|i| i.class.to_string()) else { continue };
        if let Some(id) = model.unbound_service(&class) {
            log::info!("Team Create: bound the local {class} to the server's (referent {id:#x})");
            model.bind_local(id, c);
        }
    }
}

/// Whether `r` is in the DataModel (not parentless, not removed).
fn in_place(doc: &Document, r: Ref) -> bool {
    let root = doc.dom.root_ref();
    let mut current = r;
    while let Some(inst) = doc.dom.get_by_ref(current) {
        if current == root {
            return true;
        }
        current = inst.parent();
    }
    false
}

/// What a NewObject carries, as Studio and the server send it: the Name and
/// every property that differs from the class default. A default omitted is
/// the server's own default, and a default sent can be refused: the server
/// dropped the session ("Error while receiving data") on the LocalScripts a
/// plugin made with RunContext = Legacy and Disabled = false set explicitly,
/// while ModuleScripts (no such properties) went through. A value without a
/// wire form is left out and reported; the rest of the instance is created.
fn create_props(doc: &Document, model: &LiveModel, r: Ref, skipped: &mut Vec<String>, deferred: &mut Vec<(Ref, String)>) -> Vec<(String, PropValue)> {
    let Some(inst) = doc.dom.get_by_ref(r) else { return Vec::new() };
    let mut props = vec![("Name".to_string(), PropValue::String(inst.name.clone()))];
    let mut names: Vec<_> = inst.properties.iter().collect();
    names.sort_by_key(|(name, _)| name.as_str());
    for (name, value) in names {
        if server_owned(inst.class.as_str(), name.as_str()) {
            continue;
        }
        let always = matches!(name.as_str(), "Source" | "ScriptGuid");
        // The server's default is the constructor's (Studio's measured
        // values where the reflection database is wrong: a Part's Smooth
        // top, the database's default, has to be sent, or it is Studs).
        if !always && rustudio_core::doc::class_default(inst.class.as_str(), name.as_str()).as_ref() == Some(value) {
            continue;
        }
        if matches!(value, Variant::Ref(_)) {
            deferred.push((r, name.to_string()));
            continue;
        }
        match live_value(value, model) {
            Some(value) => props.push((name.to_string(), value)),
            None => skipped.push(format!("{}.{name}: its value has no Team Create form yet", path(doc, r))),
        }
    }
    props
}

/// A new script (not yet on the server) whose ScriptGuid another script in
/// the place already has gets a fresh one before it is sent, as Studio gives
/// copies their own (not a server rule: the test-place resets of
/// 2026-11-21 were re-sent shared strings, see writer::SharedSent). Paste,
/// duplicate and insert renew them already; this covers Clone() from
/// plugins, MCP and the command bar.
pub fn renew_shared_script_guids(doc: &mut Document, model: &LiveModel, marks: &[Mark]) {
    let guid_of = |doc: &Document, r: Ref| match doc.dom.get_by_ref(r).and_then(|i| i.properties.get(&"ScriptGuid".into())) {
        Some(Variant::String(g)) if !g.is_empty() => Some(g.clone()),
        _ => None,
    };
    let mut fresh = Vec::new();
    let mut seen = HashSet::new();
    for mark in marks {
        let Mark::Tree(r) = *mark else { continue };
        for d in doc.subtree(r) {
            if seen.insert(d) && model.server_id(d).is_none() && guid_of(doc, d).is_some() {
                fresh.push(d);
            }
        }
    }
    if fresh.is_empty() {
        return;
    }
    let mut owners: HashMap<String, Vec<Ref>> = HashMap::new();
    for d in doc.dom.descendants() {
        if let Some(Variant::String(g)) = d.properties.get(&"ScriptGuid".into()) {
            if !g.is_empty() {
                owners.entry(g.clone()).or_default().push(d.referent());
            }
        }
    }
    for r in fresh {
        let Some(guid) = guid_of(doc, r) else { continue };
        let others = owners.get(&guid).map_or(0, |v| v.iter().filter(|&&o| o != r).count());
        if others == 0 {
            continue;
        }
        let new = rustudio_core::doc::new_script_guid();
        log::info!("Team Create: {} shared ScriptGuid {guid} with another script; sent as {new}", path(doc, r));
        if let Some(list) = owners.get_mut(&guid) {
            list.retain(|&o| o != r);
        }
        if let Some(i) = doc.dom.get_by_ref_mut(r) {
            i.properties.insert("ScriptGuid".into(), Variant::String(new));
        }
    }
}

/// State the server streams to clients and never takes back: an instance
/// copied from the live DataModel keeps it, and sending it in a NewObject
/// reset a test place's session (0x104, 2026-11-21: a pasted R6 rig whose
/// Parts carried NetworkOwnerV3 and whose containers carried
/// numExpectedDirectChildren). Covers the runtime-only properties
/// (not saved, not scriptable) and the ones the reflection DB doesn't list
/// but the stream carries.
pub fn server_owned(class: &str, name: &str) -> bool {
    use rbx_reflection::{PropertyKind, PropertySerialization, PropertyTag};
    if matches!(name, "numExpectedDirectChildren" | "NetworkOwnerV3" | "NetworkOwnerV2" | "NetworkIsSleeping") {
        return true;
    }
    let db = rustudio_core::doc::db();
    let Some(class) = db.classes.get(class) else { return false };
    db.superclasses_iter(class).find_map(|c| c.properties.get(name)).is_some_and(|p| {
        p.tags.contains(&PropertyTag::NotScriptable)
            && matches!(p.kind, PropertyKind::Canonical { serialization: PropertySerialization::DoesNotSerialize })
    })
}

/// `game.Workspace.Model.Part` (or `(removed) Part`).
pub fn path(doc: &Document, r: Ref) -> String {
    let root = doc.dom.root_ref();
    let mut names = Vec::new();
    let mut current = r;
    while let Some(inst) = doc.dom.get_by_ref(current) {
        if current == root {
            return names.into_iter().rev().collect::<Vec<_>>().join(".");
        }
        names.push(inst.name.clone());
        current = inst.parent();
    }
    let name = names.into_iter().rev().collect::<Vec<_>>().join(".");
    if name.is_empty() { format!("(removed instance {r:?})") } else { format!("(removed) {name}") }
}

/// Studio may create services locally when a plugin calls GetService.
/// Their presence in the schema does not make them valid NewObject edits.
/// Keep session-only services and detached plugin trees local.
/// Workspace.CurrentCamera when it is a camera the server doesn't have
/// (made locally on join, as Studio makes its own).
pub fn local_camera(doc: &Document, model: &LiveModel) -> Option<Ref> {
    let ws = doc.workspace()?;
    match doc.prop(ws, "CurrentCamera") {
        Some(Variant::Ref(c)) if doc.exists(c) && model.server_id(c).is_none() && doc.class_of(c).is_some_and(|k| k == "Camera") => Some(c),
        _ => None,
    }
}

pub fn is_local_live_instance(doc: &Document, model: &LiveModel, reference: Ref) -> bool {
    use rbx_reflection::ClassTag;

    // Studio's edit camera (Workspace.CurrentCamera) is each client's own:
    // the server never sends one, and it is never sent.
    if let Some(cam) = local_camera(doc, model) {
        if reference == cam || doc.is_ancestor(cam, reference) {
            return true;
        }
    }

    let mut current = reference;
    while let Some(instance) = doc.dom.get_by_ref(current) {
        if instance.parent().is_none() {
            return instance.class.as_str() == "Plugin";
        }
        if instance.parent() == doc.dom.root_ref() {
            let class = instance.class.as_str();
            if rustudio_core::doc::UNSAVED_SERVICES.contains(&class) {
                return true;
            }
            return model.server_id(current).is_none()
                && rustudio_core::doc::db().classes.get(class).is_some_and(|descriptor| {
                    descriptor.tags.contains(&ClassTag::Service)
                        && (current == reference || descriptor.tags.contains(&ClassTag::NotReplicated))
                });
        }
        current = instance.parent();
    }
    false
}

pub fn live_value(value: &Variant, model: &LiveModel) -> Option<PropValue> {
    use rbx_types::Variant as V;
    use rustudio_net::replicator::PropValue as P;
    Some(match value {
        V::Bool(v) => P::Bool(*v), V::Int32(v) => P::Int(*v as i64), V::Int64(v) => P::Int(*v),
        V::Float32(v) => P::Float(*v as f64), V::Float64(v) => P::Float(*v),
        V::Enum(v) => P::Enum(v.to_u32()), V::String(v) => P::String(v.clone()),
        V::BinaryString(v)=>P::BinaryString(v.clone().into_vec()),
        V::Attributes(v)=>{let mut bytes=Vec::new();v.to_writer(&mut bytes).ok()?;P::BinaryString(bytes)},
        V::SharedString(v) => P::SharedString(v.data().to_vec()),
        V::Color3(v) => P::Color3([v.r, v.g, v.b]), V::Color3uint8(v) => P::Color3uint8([v.r, v.g, v.b]),
        V::Vector2(v) => P::Vector2([v.x, v.y]), V::Vector3(v) => P::Vector3([v.x, v.y, v.z]),
        V::CFrame(v) => P::CFrame { pos: [v.position.x, v.position.y, v.position.z], rot: [v.orientation.x.x, v.orientation.x.y, v.orientation.x.z, v.orientation.y.x, v.orientation.y.y, v.orientation.y.z, v.orientation.z.x, v.orientation.z.y, v.orientation.z.z] },
        V::Ref(v) if v.is_none() => P::Ref(None), V::Ref(v) => P::Ref(Some(model.server_id(*v)?)),
        other => rustudio_net::model::typed_value(other)?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use rbx_dom_weak::{InstanceBuilder, WeakDom};
    use rustudio_net::model::LiveModel;

    #[test]
    fn live_journal_excludes_local_services_and_keeps_place_edits() {
        let mut dom = WeakDom::new(InstanceBuilder::new("DataModel"));
        let root = dom.root_ref();
        let run = dom.insert(root, InstanceBuilder::new("RunService"));
        let widgets = dom.insert(root, InstanceBuilder::new("PluginGuiService"));
        let widget = dom.insert(widgets, InstanceBuilder::new("Folder"));
        let plugin = dom.insert(Ref::none(), InstanceBuilder::new("Plugin"));
        let plugin_script = dom.insert(plugin, InstanceBuilder::new("Script"));
        let workspace = dom.insert(root, InstanceBuilder::new("Workspace"));
        let part = dom.insert(workspace, InstanceBuilder::new("Part"));
        let doc = Document::from_play_dom(dom, "Team Create".into());
        let mut model = LiveModel::new();
        model.bind_local(123, workspace);

        for reference in [run, widgets, widget, plugin, plugin_script] {
            assert!(is_local_live_instance(&doc, &model, reference));
        }
        for reference in [workspace, part] {
            assert!(!is_local_live_instance(&doc, &model, reference));
        }
    }

    /// A test place's reset: a rig copied from the live DataModel kept the
    /// server's streaming state. Creates carry the place's own values only.
    #[test]
    fn creates_leave_out_server_owned_state() {
        let (mut live, [server, ..]) = Live::new();
        live.start();
        let part = live.new_inst(
            InstanceBuilder::new("Part")
                .with_name("Head")
                .with_property("Size", rbx_types::Vector3::new(2.0, 1.0, 1.0))
                .with_property("NetworkOwnerV3", 2i32)
                .with_property("numExpectedDirectChildren", 6i32),
        );
        live.set_parent(part, server);
        let plan = live.plan();
        let Some(LiveOp::Create { props, .. }) = plan.ops.first() else { panic!("{:?}", kinds(&plan)) };
        let names: Vec<_> = props.iter().map(|(n, _)| n.as_str()).collect();
        assert!(names.contains(&"Size"), "{names:?}");
        assert!(!names.contains(&"NetworkOwnerV3") && !names.contains(&"numExpectedDirectChildren"), "{names:?}");
        assert!(plan.skipped.is_empty(), "{:?}", plan.skipped);
        assert!(!server_owned("Part", "Size") && !server_owned("Script", "Source"));
    }

    /// Two copies of one script made by Clone() go out with different
    /// ScriptGuids; one keeps the original.
    #[test]
    fn copies_sharing_a_script_guid_get_fresh_ones() {
        let (mut live, [server, ..]) = Live::new();
        live.start();
        let guid = "{FC4009F0-1F59-4E83-8D70-0C5E9E5357A7}";
        let a = live.new_inst(InstanceBuilder::new("Script").with_name("Respawn").with_property("ScriptGuid", guid));
        let b = live.new_inst(InstanceBuilder::new("Script").with_name("Respawn").with_property("ScriptGuid", guid));
        live.set_parent(a, server);
        live.set_parent(b, server);
        let rustudio_core::doc::Marks::Some(marks) = live.doc.marks_for(rustudio_core::doc::Reader::TeamCreate) else { panic!() };
        renew_shared_script_guids(&mut live.doc, &live.model, &marks);
        let g = |r| live.doc.prop(r, "ScriptGuid");
        assert_ne!(g(a), g(b));
        assert!(g(a) == Some(Variant::String(guid.into())) || g(b) == Some(Variant::String(guid.into())));
        let Some(Variant::String(other)) = [g(a), g(b)].into_iter().flatten().find(|v| *v != Variant::String(guid.into())) else { panic!() };
        assert!(other.len() == 38 && other.starts_with('{') && other.as_bytes()[15] == b'4', "{other}");
    }

    /// A test place's rig: references never ride in a create (any Ref in a
    /// NewObject resets the session); they follow every create, so a
    /// Model's PrimaryPart can name a child created after it.
    #[test]
    fn references_follow_the_creates() {
        let (mut live, [server, ..]) = Live::new();
        live.start();
        let rig = live.new_inst(InstanceBuilder::new("Model").with_name("Rig"));
        let root = live.doc.dom.insert(rig, InstanceBuilder::new("Part").with_name("HumanoidRootPart"));
        let torso = live.doc.dom.insert(rig, InstanceBuilder::new("Part").with_name("Torso"));
        let joint = live.doc.dom.insert(root, InstanceBuilder::new("Motor6D").with_name("RootJoint"));
        live.doc.dom.get_by_ref_mut(rig).unwrap().properties.insert("PrimaryPart".into(), Variant::Ref(root));
        let j = live.doc.dom.get_by_ref_mut(joint).unwrap();
        j.properties.insert("Part0".into(), Variant::Ref(root));
        j.properties.insert("Part1".into(), Variant::Ref(torso));
        live.set_parent(rig, server);
        let plan = live.plan();
        assert!(plan.unsynced.is_empty(), "{:?}", plan.unsynced);
        let creates = plan.ops.iter().take_while(|op| matches!(op, LiveOp::Create { .. })).count();
        assert_eq!(creates, 4, "{:?}", kinds(&plan));
        for op in &plan.ops[..creates] {
            let LiveOp::Create { props, .. } = op else { unreachable!() };
            assert!(props.iter().all(|(_, v)| !matches!(v, PropValue::Ref(_))), "{props:?}");
        }
        let id = |r| live.model.server_id(r).unwrap();
        let mut refs: Vec<(String, u64, Option<u64>)> = plan.ops[creates..].iter().map(|op| match op {
            LiveOp::Property { id, name, value: PropValue::Ref(target), .. } => (name.clone(), *id, *target),
            other => panic!("{other:?}"),
        }).collect();
        refs.sort();
        assert_eq!(refs, vec![
            ("Part0".into(), id(joint), Some(id(root))),
            ("Part1".into(), id(joint), Some(id(torso))),
            ("PrimaryPart".into(), id(rig), Some(id(root))),
        ]);
    }

    struct Live {
        doc: Document,
        model: LiveModel,
        next: u64,
    }

    impl Live {
        /// game.ServerScriptService.Server.{A (with child Config), B} on the server.
        fn new() -> (Self, [Ref; 4]) {
            let mut dom = WeakDom::new(InstanceBuilder::new("DataModel"));
            let root = dom.root_ref();
            let sss = dom.insert(root, InstanceBuilder::new("ServerScriptService").with_name("ServerScriptService"));
            let server = dom.insert(sss, InstanceBuilder::new("Folder").with_name("Server"));
            let a = dom.insert(server, InstanceBuilder::new("ModuleScript").with_name("A").with_property("Source", "return 1"));
            let config = dom.insert(a, InstanceBuilder::new("Configuration").with_name("Config"));
            let doc = Document::from_play_dom(dom, "Team Create".into());
            let mut model = LiveModel::new();
            for (id, r) in [(0xd, root), (0x10, sss), (0x11, server), (0x12, a), (0x13, config)] {
                model.bind_local(id, r);
            }
            (Live { doc, model, next: (8 << 32) | 1 }, [server, a, config, root])
        }

        fn plan(&mut self) -> Plan {
            let rustudio_core::doc::Marks::Some(marks) = self.doc.marks_for(rustudio_core::doc::Reader::TeamCreate) else {
                panic!("the journal overflowed");
            };
            let next = &mut self.next;
            plan(&self.doc, &mut self.model, &marks, &mut || {
                *next += 1;
                Ok(*next)
            })
        }

        fn start(&mut self) {
            let _ = self.doc.marks_for(rustudio_core::doc::Reader::TeamCreate);
        }

        fn new_inst(&mut self, builder: InstanceBuilder) -> Ref {
            let r = self.doc.dom.insert(Ref::none(), builder);
            self.doc.mark_tree(r);
            r
        }

        fn set_parent(&mut self, r: Ref, parent: Ref) {
            self.doc.dom.transfer_within(r, parent);
            self.doc.mark_tree(r);
        }
    }

    fn kinds(plan: &Plan) -> Vec<String> {
        plan.ops
            .iter()
            .map(|op| match op {
                LiveOp::Create { props, .. } => format!(
                    "create {}",
                    props.iter().find(|(n, _)| n == "Name").map_or("?".into(), |(_, v)| format!("{v:?}"))
                ),
                LiveOp::Reparent { id, parent, .. } => format!("move {id:#x} -> {parent:#x}"),
                LiveOp::Property { name, .. } => format!("prop {name}"),
                LiveOp::Source { id, .. } => format!("source {id:#x}"),
                LiveOp::Delete { id, .. } => format!("delete {id:#x}"),
            })
            .collect()
    }

    /// The Place A push: a script is replaced in one frame by a new one of the
    /// same name (the old one's children moved into it), the old one
    /// Destroyed (left parentless in the document). The new script is
    /// created, the child moved into it, and only then the old one deleted.
    #[test]
    fn a_script_replaced_in_one_frame_is_created_moved_into_and_deleted() {
        let (mut live, [server, a, config, _]) = Live::new();
        live.start();
        let n = live.new_inst(InstanceBuilder::new("ModuleScript").with_name("A").with_property("Source", "return 2"));
        live.set_parent(config, n);
        live.set_parent(n, server);
        live.set_parent(a, Ref::none()); // Destroy
        let plan = live.plan();
        assert!(plan.unsynced.is_empty(), "{:?}", plan.unsynced);
        let new_id = live.model.server_id(n).unwrap();
        assert_eq!(
            kinds(&plan),
            [r#"create String("A")"#.to_string(), format!("move 0x13 -> {new_id:#x}"), "delete 0x12".into()]
        );
        assert!(matches!(&plan.ops[0], LiveOp::Create { parent: 0x11, props, .. }
            if props.iter().any(|(n, v)| n == "Source" && *v == PropValue::String("return 2".into()))));
        assert_eq!(live.model.server_id(a), None, "the old script's referent is forgotten");
        assert_eq!(live.model.server_id(config), Some(0x13), "the moved child keeps its own");
    }

    /// Instance.new, children parented bottom-up, the root last: one create
    /// tree from the root, parents first, whatever order the marks came in.
    #[test]
    fn a_tree_built_bottom_up_is_created_top_down() {
        let (mut live, [server, ..]) = Live::new();
        live.start();
        let folder = live.new_inst(InstanceBuilder::new("Folder").with_name("Kit"));
        let sub = live.new_inst(InstanceBuilder::new("Folder").with_name("Sub"));
        let leaf = live.new_inst(InstanceBuilder::new("ModuleScript").with_name("Leaf"));
        live.set_parent(leaf, sub);
        live.set_parent(sub, folder);
        live.set_parent(folder, server);
        let plan = live.plan();
        assert!(plan.unsynced.is_empty(), "{:?}", plan.unsynced);
        assert_eq!(kinds(&plan), [r#"create String("Kit")"#, r#"create String("Sub")"#, r#"create String("Leaf")"#]);
        let id = |r| live.model.server_id(r).unwrap();
        let parents: Vec<u64> = plan.ops.iter().map(|op| match op { LiveOp::Create { parent, .. } => *parent, _ => 0 }).collect();
        assert_eq!(parents, [0x11, id(folder), id(sub)]);
    }

    /// Defaults are not sent (the server's NewObject omits them too); a value
    /// with no wire form drops only itself.
    #[test]
    fn creates_carry_only_non_default_properties() {
        let (mut live, [server, ..]) = Live::new();
        live.start();
        let local = live.new_inst(
            InstanceBuilder::new("LocalScript")
                .with_name("Hud")
                .with_property("Source", "print(1)")
                .with_property("Disabled", false)
                .with_property("RunContext", rbx_types::Enum::from_u32(0)),
        );
        live.set_parent(local, server);
        let plan = live.plan();
        let LiveOp::Create { props, .. } = &plan.ops[0] else { panic!("{plan:?}") };
        let names: Vec<_> = props.iter().map(|(n, _)| n.as_str()).collect();
        assert_eq!(names, ["Name", "Source"]);
        let off = live.new_inst(InstanceBuilder::new("LocalScript").with_name("Off").with_property("Disabled", true));
        live.set_parent(off, server);
        let plan = live.plan();
        let LiveOp::Create { props, .. } = &plan.ops[0] else { panic!("{plan:?}") };
        assert!(props.iter().any(|(n, v)| n == "Disabled" && *v == PropValue::Bool(true)));
    }

    /// Parent = nil and Explorer deletes (to the undo limbo) both delete;
    /// a child is deleted before its parent; putting it back recreates it.
    #[test]
    fn removed_instances_are_deleted_deepest_first_and_recreated_on_return() {
        let (mut live, [server, a, config, _]) = Live::new();
        live.start();
        live.set_parent(config, Ref::none());
        live.set_parent(a, Ref::none());
        let plan = live.plan();
        assert_eq!(kinds(&plan), ["delete 0x13", "delete 0x12"]);
        live.set_parent(a, server);
        let plan = live.plan();
        assert_eq!(kinds(&plan), [r#"create String("A")"#]);
        assert!(live.model.server_id(a).is_some_and(|id| id != 0x12));
    }

    /// A new tree under ReplicatedFirst, which the editor made itself: it
    /// binds to the server's listed service and the tree is created there.
    /// (A join makes every listed service in the model's own tree, which
    /// becomes the document: this is a service listed with no instance.)
    #[test]
    fn a_local_service_the_server_lists_is_bound() {
        let (mut live, _) = Live::new();
        live.model.apply(rustudio_net::replicator::RepOp::Services(vec![(0x2110, "ReplicatedFirst".into())]));
        let made = live.model.reference(0x2110).expect("the model makes the listed service");
        live.model.unbind(made);
        let root = live.doc.dom.root_ref();
        let rf = live.doc.dom.insert(root, InstanceBuilder::new("ReplicatedFirst").with_name("ReplicatedFirst"));
        live.start();
        let first = live.new_inst(InstanceBuilder::new("Folder").with_name("First"));
        live.set_parent(first, rf);
        let plan = live.plan();
        assert!(plan.unsynced.is_empty(), "{:?}", plan.unsynced);
        assert!(matches!(&plan.ops[..], [LiveOp::Create { parent: 0x2110, .. }]), "{plan:?}");
    }

    /// A tree under a local-only parent is reported, not sent.
    #[test]
    fn a_tree_under_a_parent_the_server_lacks_is_reported() {
        let (mut live, _) = Live::new();
        let root = live.doc.dom.root_ref();
        let rf = live.doc.dom.insert(root, InstanceBuilder::new("ReplicatedFirst").with_name("ReplicatedFirst"));
        live.start();
        let first = live.new_inst(InstanceBuilder::new("Folder").with_name("First"));
        live.set_parent(first, rf);
        let plan = live.plan();
        assert!(plan.ops.is_empty());
        assert_eq!(plan.unsynced.len(), 1);
        assert!(plan.unsynced[0].starts_with("ReplicatedFirst.First"), "{:?}", plan.unsynced);
    }

    /// Place A's LocalMapAssets: a new script was given the folder's referent (a
    /// reused scope), so the folder lost it and its Destroy() went nowhere.
    /// Now the allocator skips taken referents, and a removed instance that
    /// lost its referent is reported instead of silently kept.
    #[test]
    fn taken_referents_are_skipped_and_lost_instances_reported() {
        let (mut live, [server, a, _, _]) = Live::new();
        live.start();
        // The session's allocator hands out 0x12, A's referent, first.
        let mut ids = vec![0x40u64, 0x12];
        let n = live.new_inst(InstanceBuilder::new("ModuleScript").with_name("New"));
        live.set_parent(n, server);
        let rustudio_core::doc::Marks::Some(marks) = live.doc.marks_for(rustudio_core::doc::Reader::TeamCreate) else { panic!() };
        let plan = plan(&live.doc, &mut live.model, &marks, &mut || Ok(ids.pop().unwrap()));
        assert!(matches!(&plan.ops[..], [LiveOp::Create { id: 0x40, .. }]), "{plan:?}");
        assert_eq!(live.model.server_id(a), Some(0x12), "A keeps its referent");

        // An instance that did lose its referent (a build before this fix).
        live.model.bind_local(0x12, n);
        assert!(live.model.lost_referent(a));
        live.set_parent(a, Ref::none());
        let plan = live.plan();
        assert!(plan.ops.is_empty(), "{plan:?}");
        assert_eq!(plan.unsynced.len(), 1);
        assert!(plan.unsynced[0].contains("still on the Team Create server"), "{:?}", plan.unsynced);
    }
}

#[cfg(test)]
mod studio_parity_tests {
    use super::*;

    /// One table for Names the wire leaves out, in both crates.
    #[test]
    fn default_name_tables_agree() {
        for class in rustudio_core::doc::db().classes.keys() {
            assert_eq!(rustudio_net::model::default_instance_name(class), rustudio_core::doc::default_instance_name(class), "{class}");
        }
    }

    /// The camera made on join (Studio's own) and CurrentCamera naming it are
    /// never sent to the server.
    #[test]
    fn the_local_camera_is_never_sent() {
        let mut doc = Document::empty();
        let root = doc.dom.root_ref();
        let ws = doc.dom.insert(root, rbx_dom_weak::InstanceBuilder::new("Workspace").with_name("Workspace"));
        let mut model = LiveModel::new();
        model.bind_local(13, root);
        model.bind_local(14, ws);
        let cam = doc.ensure_current_camera().unwrap();
        assert!(local_camera(&doc, &model) == Some(cam));
        let marks = vec![Mark::Tree(cam), Mark::Prop(ws, rbx_dom_weak::ustr("CurrentCamera")), Mark::Prop(cam, rbx_dom_weak::ustr("CFrame"))];
        let mut next = 100u64;
        let p = plan(&doc, &mut model, &marks, &mut || { next += 1; Ok(next) });
        assert!(p.ops.is_empty(), "{:?}", p.ops);
        assert!(p.unsynced.is_empty(), "{:?}", p.unsynced);
    }

    /// Offline repro of a live push: builds the project items named in
    /// RUSTUDIO_TC_REPRO_ITEMS (comma-separated paths) from the project at
    /// RUSTUDIO_TC_REPRO_PROJECT the way File Sync creates them, plans them,
    /// encodes them against the schema in the captured join stream at
    /// RUSTUDIO_TC_REPRO_STREAM and decodes each frame back. Prints every
    /// create: class, wire properties (name:type), bytes. Sends nothing.
    /// Offline: the creates one subtree of a place file (RUSTUDIO_TC_REPRO_RBXL,
    /// the first instance whose child is named RUSTUDIO_TC_REPRO_CHILD, or
    /// whose path is RUSTUDIO_TC_REPRO_PATH) turns into, encoded against the
    /// captured join stream's schema (RUSTUDIO_TC_REPRO_STREAM) and decoded
    /// back. Sends nothing.
    #[test]
    #[ignore = "needs a place file and a captured stream (env)"]
    fn repro_subtree_push_offline() {
        use rustudio_net::replicator::{Decoder, RepOp};
        let (Ok(file), Ok(stream)) = (std::env::var("RUSTUDIO_TC_REPRO_RBXL"), std::env::var("RUSTUDIO_TC_REPRO_STREAM")) else {
            return;
        };
        let data = std::fs::read(stream).unwrap();
        let mut dec = Decoder::new();
        let mut p = 7;
        while p + 4 <= data.len() && dec.schema.is_none() {
            let len = (u32::from_be_bytes(data[p..p + 4].try_into().unwrap()) & 0x7fff_ffff) as usize;
            let f = &data[p + 4..p + 4 + len];
            p += 4 + len;
            if !f.is_empty() {
                dec.decode_frame(f[0], &f[1..]);
            }
        }
        let schema = dec.schema.clone().unwrap();
        let mut doc = Document::open(std::path::Path::new(&file)).unwrap();
        let all = doc.subtree(doc.dom.root_ref());
        let top = match (std::env::var("RUSTUDIO_TC_REPRO_PATH"), std::env::var("RUSTUDIO_TC_REPRO_CHILD")) {
            (Ok(want), _) => all.iter().copied().find(|&r| path(&doc, r) == want),
            (_, Ok(child)) => all.iter().copied().find(|&r| {
                doc.dom.get_by_ref(r).unwrap().children().iter().any(|&c| doc.dom.get_by_ref(c).unwrap().name == child)
            }),
            _ => None,
        }
        .expect("no such instance");
        println!("subtree {} ({} instances)", path(&doc, top), doc.subtree(top).len());
        let inside: HashSet<Ref> = doc.subtree(top).into_iter().collect();
        let mut model = LiveModel::new();
        let mut next = 0x100u64;
        for r in all {
            if !inside.contains(&r) {
                next += 1;
                model.bind_local(next, r);
            }
        }
        let _ = doc.marks_for(rustudio_core::doc::Reader::TeamCreate);
        doc.mark_tree(top);
        let rustudio_core::doc::Marks::Some(marks) = doc.marks_for(rustudio_core::doc::Reader::TeamCreate) else { panic!() };
        let mut next = (7u64 << 32) | 1;
        let plan = plan(&doc, &mut model, &marks, &mut || {
            next += 1;
            Ok(next)
        });
        for line in &plan.skipped {
            println!("SKIPPED {line}");
        }
        for line in &plan.unsynced {
            println!("UNSYNCED {line}");
        }
        for op in plan.ops {
            let (reference, class, frame, skipped) = match op {
                LiveOp::Create { reference, id, class, parent, props } => {
                    let (frame, skipped) = rustudio_net::writer::create_partial(&schema, id, &class, parent, &props).unwrap();
                    (reference, class, frame, skipped)
                }
                other => {
                    println!("OTHER {other:?}");
                    continue;
                }
            };
            let mut d = Decoder::new();
            d.schema = Some(schema.clone());
            let ops = d.decode_frame(frame[0], &frame[1..]);
            let wire: Vec<String> = match ops.first() {
                Some(RepOp::NewInstance { props, .. }) => props.iter().map(|(n, v)| format!("{n}={}", format!("{v:?}").chars().take(60).collect::<String>())).collect(),
                _ => vec![format!("DECODE FAILED {:?}", d.errors)],
            };
            println!("{} {class} {} bytes; wire: {}; writer skipped: {skipped:?}", path(&doc, reference), frame.len(), wire.join(" | "));
        }
    }

    #[test]
    #[ignore = "needs a project and a captured stream (env)"]
    fn repro_live_push_offline() {
        use rbx_dom_weak::{InstanceBuilder, WeakDom};
        use rustudio_net::replicator::{Decoder, RepOp};
        let (Ok(project), Ok(stream), Ok(items)) = (
            std::env::var("RUSTUDIO_TC_REPRO_PROJECT"),
            std::env::var("RUSTUDIO_TC_REPRO_STREAM"),
            std::env::var("RUSTUDIO_TC_REPRO_ITEMS"),
        ) else {
            return;
        };
        let data = std::fs::read(stream).unwrap();
        let mut dec = Decoder::new();
        let mut p = 7;
        while p + 4 <= data.len() && dec.schema.is_none() {
            let len = (u32::from_be_bytes(data[p..p + 4].try_into().unwrap()) & 0x7fff_ffff) as usize;
            let f = &data[p + 4..p + 4 + len];
            p += 4 + len;
            if !f.is_empty() {
                dec.decode_frame(f[0], &f[1..]);
            }
        }
        let schema = dec.schema.clone().unwrap();
        let (tree, _) = crate::mcp::project_apply::read(std::path::Path::new(&project)).unwrap();
        let mut dom = WeakDom::new(InstanceBuilder::new("DataModel"));
        let root = dom.root_ref();
        let mut doc = Document::from_play_dom(std::mem::replace(&mut dom, WeakDom::new(InstanceBuilder::new("DataModel"))), "repro".into());
        let mut model = LiveModel::new();
        model.bind_local(0xd, doc.dom.root_ref());
        let _ = root;
        let mut next = (7u64 << 32) | 1;
        for item in items.split(',') {
            let names: Vec<&str> = item.split('.').collect();
            let mut snap = &tree;
            let mut parent = doc.dom.root_ref();
            for (depth, name) in names.iter().enumerate() {
                snap = snap.children.iter().find(|c| c.name == *name).unwrap_or_else(|| panic!("{item}: no {name}"));
                if depth + 1 == names.len() {
                    break;
                }
                let existing = doc.dom.get_by_ref(parent).unwrap().children().iter().copied().find(|c| doc.dom.get_by_ref(*c).unwrap().name == *name);
                parent = existing.unwrap_or_else(|| {
                    let r = doc.dom.insert(parent, InstanceBuilder::new(snap.class.clone()).with_name(*name));
                    next += 1;
                    model.bind_local(0x1000 + next, r);
                    r
                });
            }
            let _ = doc.marks_for(rustudio_core::doc::Reader::TeamCreate);
            crate::mcp::project_apply::create(&mut doc, parent, snap).unwrap();
            let rustudio_core::doc::Marks::Some(marks) = doc.marks_for(rustudio_core::doc::Reader::TeamCreate) else { panic!() };
            let plan = plan(&doc, &mut model, &marks, &mut || {
                next += 1;
                Ok(next)
            });
            for line in &plan.skipped {
                println!("SKIPPED {line}");
            }
            for op in plan.ops {
                let LiveOp::Create { reference, id, class, parent, props } = op else { continue };
                let (frame, skipped) = rustudio_net::writer::create_partial(&schema, id, &class, parent, &props).unwrap();
                let mut d = Decoder::new();
                d.schema = Some(schema.clone());
                let ops = d.decode_frame(frame[0], &frame[1..]);
                let wire: Vec<String> = match ops.first() {
                    Some(RepOp::NewInstance { props, .. }) => props.iter().map(|(n, v)| format!("{n}={}", format!("{v:?}").chars().take(48).collect::<String>())).collect(),
                    _ => vec![format!("DECODE FAILED {:?}", d.errors)],
                };
                println!("{} {class} {} bytes; wire: {}; writer skipped: {skipped:?}", path(&doc, reference), frame.len(), wire.join(" | "));
            }
        }
    }
}
