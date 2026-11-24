//! A Play publication carries the camera and the changes needed by the
//! viewport's private document. The viewport keeps its normal journal readers
//! and does not borrow either of the simulation worker's documents.

use rbx_dom_weak::{
    types::{Ref, Variant},
    AHashMap, Instance, PropMap, Ustr,
};
use rustc_hash::FxHashSet;
use rustudio_core::doc::{Document, Mark, Marks, Reader};

use crate::{camera::Camera, effects::SnapshotRequests};

#[derive(Default)]
pub struct SnapshotSource {
    serial: u64,
    document_id: Option<u64>,
}

/// The main thread's cursor. Keep it alongside the displayed document.
#[derive(Default)]
pub struct SnapshotReceiver {
    // Source document, publication serial, and local presentation document.
    applied: Option<(u64, u64, u64)>,
}

/// A delta's predecessor was not applied. Request a replacement from the
/// source and keep drawing the last complete publication in the meantime.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MissingSnapshot;

pub struct RenderSnapshot {
    serial: u64,
    source_document_id: u64,
    camera: Camera,
    body: Body,
    effects: SnapshotRequests,
}

/// When Play shows the client, map the server's scripted effects to it.
pub type ServerEffects<'a> = (u64, &'a dyn Fn(Ref) -> Option<Ref>);

enum Body {
    Replace(Box<Document>),
    Delta(Delta),
}

#[derive(Default)]
struct Delta {
    marks: Vec<Mark>,
    tree: AHashMap<Ref, Instance>,
    deleted: Vec<Ref>,
    properties: Vec<PropertyEdit>,
}

enum PropertyEdit {
    Value(Ref, Ustr, Option<Variant>),
    Map(Ref, PropMap),
    Name(Ref, String),
}

impl SnapshotSource {
    /// Makes the next capture self-contained, for example after transport
    /// discards a delta.
    pub fn reset(&mut self) {
        self.document_id = None;
    }

    /// Call after gameplay and camera have finished publishing this tick.
    /// Move the snapshot across the channel; deltas require ordered delivery.
    pub fn capture(&mut self, displayed: &mut Document, camera: &Camera) -> RenderSnapshot {
        self.capture_with_server(displayed, camera, None)
    }

    /// Also publishes scripted Emit, Clear and beam offsets from the server
    /// when the displayed document is the client.
    pub fn capture_with_server(
        &mut self,
        displayed: &mut Document,
        camera: &Camera,
        server: Option<ServerEffects<'_>>,
    ) -> RenderSnapshot {
        let _scope = rustudio_core::bench::scope("render_snapshot_capture");
        let marks = displayed.marks_for(Reader::RenderSnapshot);
        let prior_id = self.document_id.replace(displayed.id);
        let body = match marks {
            Marks::Some(marks) if prior_id == Some(displayed.id) => {
                Body::Delta(Delta::capture(displayed, marks))
            }
            _ => {
                // The mirror's undo limbo does not use a gameplay referent.
                let mut mirror = rbx_dom_weak::with_scratch_refs(|| {
                    Document::from_play_dom(displayed.dom.clone(), displayed.title.clone())
                });
                mirror.kind = displayed.kind;
                mirror.path.clone_from(&displayed.path);
                mirror.cloud.clone_from(&displayed.cloud);
                Body::Replace(Box::new(mirror))
            }
        };
        self.serial += 1;
        RenderSnapshot {
            serial: self.serial,
            source_document_id: displayed.id,
            camera: camera.clone(),
            body,
            effects: SnapshotRequests::capture(displayed.id, server),
        }
    }
}

impl RenderSnapshot {
    pub fn serial(&self) -> u64 {
        self.serial
    }

    pub fn source_document_id(&self) -> u64 {
        self.source_document_id
    }

    pub fn replaces_document(&self) -> bool {
        matches!(self.body, Body::Replace(_))
    }

    /// Applies a publication to the main-thread mirror and camera together.
    pub fn apply(
        self,
        receiver: &mut SnapshotReceiver,
        doc: &mut Document,
        camera: &mut Camera,
    ) -> Result<(), MissingSnapshot> {
        let _scope = rustudio_core::bench::scope("render_snapshot_apply");
        match self.body {
            Body::Replace(mirror) => *doc = *mirror,
            Body::Delta(delta) => {
                if receiver.applied != Some((self.source_document_id, self.serial - 1, doc.id)) {
                    return Err(MissingSnapshot);
                }
                delta.apply(doc);
            }
        }
        self.effects.publish(doc.id);
        *camera = self.camera;
        receiver.applied = Some((self.source_document_id, self.serial, doc.id));
        Ok(())
    }
}

impl Delta {
    fn capture(doc: &Document, marks: Vec<Mark>) -> Self {
        let mut delta = Self::default();
        let mut tree_roots = FxHashSet::default();
        let mut prop_keys = FxHashSet::default();
        let mut copy_map = FxHashSet::default();
        let mut pending = Vec::new();
        for mark in marks {
            match mark {
                Mark::Tree(r) if tree_roots.insert(r) => {
                    delta.marks.push(mark);
                    if let Some(node) = doc.dom.get_by_ref(r) {
                        pending.push(r);
                        // Capture order from the actual target parent. An
                        // unchanged sibling can still move in the child list.
                        if let Some(parent) = doc.dom.get_by_ref(node.parent()) {
                            delta
                                .tree
                                .entry(parent.referent())
                                .or_insert_with(|| parent.clone());
                        }
                    } else {
                        delta.deleted.push(r);
                    }
                }
                Mark::Prop(r, key) if prop_keys.insert((r, key)) => {
                    delta.marks.push(mark);
                    if !matches!(key.as_str(), "CFrame" | "Transform" | "Name") {
                        copy_map.insert(r);
                    }
                }
                _ => {}
            }
        }

        let mut visited = FxHashSet::default();
        while let Some(r) = pending.pop() {
            if visited.insert(r) {
                if let Some(node) = doc.dom.get_by_ref(r) {
                    delta.tree.entry(r).or_insert_with(|| node.clone());
                    pending.extend_from_slice(node.children());
                }
            }
        }

        for &mark in &delta.marks {
            let Mark::Prop(r, key) = mark else { continue };
            if delta.tree.contains_key(&r) {
                continue;
            }
            let Some(node) = doc.dom.get_by_ref(r) else {
                continue;
            };
            if key == "Name" {
                delta
                    .properties
                    .push(PropertyEdit::Name(r, node.name.clone()));
            } else if copy_map.remove(&r) {
                // Script setters can update XML aliases or Content twins
                // while journaling only the public property name.
                delta
                    .properties
                    .push(PropertyEdit::Map(r, node.properties.clone()));
            } else if matches!(key.as_str(), "CFrame" | "Transform") {
                delta.properties.push(PropertyEdit::Value(
                    r,
                    key,
                    node.properties.get(&key).cloned(),
                ));
            }
        }
        delta
    }

    fn apply(mut self, doc: &mut Document) {
        if !self.tree.is_empty() || !self.deleted.is_empty() {
            let removed = doc.dom.apply_snapshot_tree(self.tree, &self.deleted);
            if !removed.is_empty() {
                let roots: FxHashSet<_> = self.deleted.into_iter().collect();
                // A destroyed subtree is gone from the mirror by the time
                // its journal readers run. They need each removed referent.
                self.marks.extend(
                    removed
                        .into_iter()
                        .filter(|r| !roots.contains(r))
                        .map(Mark::Tree),
                );
            }
        }
        for edit in self.properties {
            match edit {
                PropertyEdit::Value(r, key, value) => {
                    if let Some(i) = doc.dom.get_by_ref_mut(r) {
                        if let Some(value) = value {
                            i.properties.insert(key, value);
                        } else {
                            i.properties.remove(&key);
                        }
                    }
                }
                PropertyEdit::Map(r, props) => {
                    if let Some(i) = doc.dom.get_by_ref_mut(r) {
                        i.properties = props;
                    }
                }
                PropertyEdit::Name(r, name) => {
                    if let Some(i) = doc.dom.get_by_ref_mut(r) {
                        i.name = name;
                    }
                }
            }
        }
        for mark in self.marks {
            match mark {
                Mark::Tree(r) => doc.mark_tree(r),
                Mark::Prop(r, key) => doc.mark_prop(r, key),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use glam::{Mat3, Vec3};
    use rbx_dom_weak::{types::Content, ustr, InstanceBuilder};
    use rustudio_core::math::Xform;

    use super::*;
    use crate::scene::Scene;

    #[test]
    fn publishes_camera_and_coalesced_draw_changes_without_live_access() {
        fn assert_send<T: Send>() {}
        assert_send::<RenderSnapshot>();

        let mut live = Document::new_place();
        let workspace = live.find_child(live.dom.root_ref(), "Workspace").unwrap();
        let host = live.dom.insert(workspace, InstanceBuilder::new("Part"));
        let light = live.dom.insert(host, InstanceBuilder::new("PointLight"));
        let mesh = live.dom.insert(
            workspace,
            InstanceBuilder::new("MeshPart").with_property("TextureID", "old"),
        );
        let mut source = SnapshotSource::default();
        let mut receiver = SnapshotReceiver::default();
        let mut mirror = Document::empty();
        let mut camera = Camera::default();
        let mut scene = Scene::default();
        source
            .capture(&mut live, &camera)
            .apply(&mut receiver, &mut mirror, &mut camera)
            .unwrap();
        assert_ne!(mirror.id, live.id);
        scene.sync(&mut mirror);
        scene.take_changes();

        let cframe = ustr("CFrame");
        let at = Vec3::new(3.0, 4.0, 5.0);
        for pos in [Vec3::Z, at] {
            live.set_prop_live(
                host,
                cframe,
                Variant::CFrame(
                    Xform {
                        pos,
                        rot: Mat3::IDENTITY,
                    }
                    .to_cframe(),
                ),
            );
        }
        live.set_prop_live(light, ustr("Brightness"), Variant::Float32(3.0));
        let props = &mut live.dom.get_by_ref_mut(mesh).unwrap().properties;
        props.remove(&ustr("TextureID"));
        props.insert(
            ustr("TextureContent"),
            Variant::Content(Content::from_uri("new")),
        );
        live.mark_prop(mesh, ustr("TextureContent"));
        let next_cam = Camera {
            pos: at,
            ..Camera::default()
        };
        let delta = source.capture(&mut live, &next_cam);
        assert!(!delta.replaces_document());
        assert_eq!(delta.source_document_id(), live.id);
        assert_eq!(delta.serial(), 2);

        // The worker can already begin modifying the next simulation frame.
        live.set_prop_live(
            host,
            cframe,
            Variant::CFrame(
                Xform {
                    pos: Vec3::ZERO,
                    rot: Mat3::IDENTITY,
                }
                .to_cframe(),
            ),
        );
        delta
            .apply(&mut receiver, &mut mirror, &mut camera)
            .unwrap();
        scene.sync(&mut mirror);
        assert_eq!(scene.take_changes().moved, vec![host]);
        assert_eq!(scene.get(host).unwrap().xf.pos, at);
        assert_eq!(camera.pos, at);
        assert_eq!(
            mirror.prop(light, "Brightness"),
            Some(Variant::Float32(3.0))
        );
        assert!(mirror
            .dom
            .get_by_ref(mesh)
            .unwrap()
            .properties
            .get(&ustr("TextureID"))
            .is_none());
        assert_eq!(
            mirror.prop(mesh, "TextureContent"),
            Some(Variant::Content(Content::from_uri("new")))
        );
    }

    #[test]
    fn preserves_tree_order_and_survivors_across_removal_and_reparenting() {
        let mut live = Document::new_place();
        let workspace = live.find_child(live.dom.root_ref(), "Workspace").unwrap();
        let a = live.dom.insert(workspace, InstanceBuilder::new("Folder"));
        let b = live.dom.insert(workspace, InstanceBuilder::new("Folder"));
        let c = live.dom.insert(workspace, InstanceBuilder::new("Folder"));
        let survivor = live.dom.insert(a, InstanceBuilder::new("Part"));
        let dead_child = live.dom.insert(a, InstanceBuilder::new("Part"));

        let mut source = SnapshotSource::default();
        let mut receiver = SnapshotReceiver::default();
        let mut mirror = Document::empty();
        let mut camera = Camera::default();
        let mut scene = Scene::default();
        source
            .capture(&mut live, &camera)
            .apply(&mut receiver, &mut mirror, &mut camera)
            .unwrap();
        scene.sync(&mut mirror);
        scene.take_changes();

        live.dom.transfer_within(survivor, b);
        live.mark_tree(survivor);
        live.dom.destroy(a);
        live.mark_tree(a);
        // Moving a sibling out and back changes its order even if its final
        // parent matches the state in the last publication.
        live.dom.transfer_within(b, Ref::none());
        live.dom.transfer_within(b, workspace);
        live.mark_tree(b);
        let new = live.dom.insert(
            c,
            InstanceBuilder::new("Model").with_child(InstanceBuilder::new("Part")),
        );
        live.mark_tree(new);
        let detached = live.dom.insert(Ref::none(), InstanceBuilder::new("Camera"));
        live.mark_tree(detached);
        live.rename(c, "Preview");
        source
            .capture(&mut live, &camera)
            .apply(&mut receiver, &mut mirror, &mut camera)
            .unwrap();

        assert_eq!(
            mirror.dom.get_by_ref(workspace).unwrap().children(),
            live.dom.get_by_ref(workspace).unwrap().children()
        );
        assert_eq!(mirror.dom.get_by_ref(b).unwrap().children(), &[survivor]);
        assert_eq!(mirror.dom.get_by_ref(survivor).unwrap().parent(), b);
        assert_eq!(
            mirror.dom.get_by_ref(new).unwrap().children(),
            live.dom.get_by_ref(new).unwrap().children()
        );
        assert!(mirror.dom.get_by_ref(detached).unwrap().parent().is_none());
        assert_eq!(mirror.dom.get_by_ref(c).unwrap().name, "Preview");
        assert!(mirror.dom.get_by_ref(a).is_none());
        assert!(mirror.dom.get_by_ref(dead_child).is_none());
        scene.sync(&mut mirror);
        assert_eq!(scene.take_changes().removed, vec![dead_child]);
        assert!(scene.get(survivor).is_some());
    }

    #[test]
    fn rejects_missing_deltas_and_recovers_from_a_keyframe() {
        let mut live = Document::new_place();
        let mut source = SnapshotSource::default();
        let mut receiver = SnapshotReceiver::default();
        let mut mirror = Document::empty();
        let mut camera = Camera::default();
        source
            .capture(&mut live, &camera)
            .apply(&mut receiver, &mut mirror, &mut camera)
            .unwrap();
        let first_mirror_id = mirror.id;
        let view = Camera {
            pos: Vec3::ONE,
            ..Camera::default()
        };
        let _dropped = source.capture(&mut live, &view);
        assert_eq!(
            source
                .capture(&mut live, &view)
                .apply(&mut receiver, &mut mirror, &mut camera),
            Err(MissingSnapshot)
        );
        assert_ne!(camera.pos, view.pos);
        assert_eq!(mirror.id, first_mirror_id);
        source.reset();
        let full = source.capture(&mut live, &view);
        assert!(full.replaces_document());
        full.apply(&mut receiver, &mut mirror, &mut camera).unwrap();
        assert_ne!(mirror.id, first_mirror_id);
        assert_eq!(camera.pos, view.pos);
        live.mark_all();
        assert!(source.capture(&mut live, &view).replaces_document());
        assert!(source
            .capture(&mut Document::new_place(), &view)
            .replaces_document());
    }
}
