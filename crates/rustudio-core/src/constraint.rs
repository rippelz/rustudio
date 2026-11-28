//! Studio's Model > Constraint menu, natively: join two selected parts (or
//! two attachments) with one of Roblox's mechanical constraints.
//!
//! Studio's ConstraintTool makes an Attachment in each part at the joint and
//! puts the constraint beside the parts with Attachment0 / Attachment1 set.
//! With two parts selected it inserts at once; the attachments meet at the
//! closest points of the two parts, so the joint starts where they touch
//! (or halfway across the gap), and both share one world frame so creating
//! the constraint does not move them.

use glam::Vec3;
use rbx_dom_weak::types::{Ref, Variant};
use rbx_dom_weak::InstanceBuilder;

use crate::doc::{self, Document};
use crate::math::Xform;

/// The closest point to `p` on the oriented box `xf` × `size`.
fn closest_on_box(xf: &Xform, size: Vec3, p: Vec3) -> Vec3 {
    let inv = xf.inverse();
    let local = inv.apply(p);
    let h = size * 0.5;
    xf.apply(Vec3::new(
        local.x.clamp(-h.x, h.x),
        local.y.clamp(-h.y, h.y),
        local.z.clamp(-h.z, h.z),
    ))
}

/// `world` as a frame local to `part`.
fn to_local(part: &Xform, world: &Xform) -> Xform {
    let inv = part.inverse();
    Xform {
        pos: inv.apply(world.pos),
        rot: inv.rot * world.rot,
    }
}

fn is_attachment(doc: &Document, r: Ref) -> bool {
    doc.class_of(r).is_some_and(|c| doc::is_a(&c, "Attachment"))
}

fn is_part(doc: &Document, r: Ref) -> bool {
    doc.class_of(r).is_some_and(|c| doc::is_a(&c, "BasePart"))
}

fn parent_of(doc: &Document, r: Ref) -> Ref {
    doc.dom
        .get_by_ref(r)
        .map(|i| i.parent())
        .filter(|p| p.is_some())
        .unwrap_or_else(|| doc.default_parent())
}

/// Inserts `class` between `a` and `b`, either two parts or two attachments.
/// Returns the constraint; `None` when the pair cannot be joined.
///
/// Two parts get an Attachment in each, at a common world frame at their
/// closest points. Two attachments (or Bones) are used as they are. The
/// constraint goes beside the first part, as Studio inserts it. The whole
/// creation is one undo step.
pub fn insert_between(doc: &mut Document, class: &str, a: Ref, b: Ref) -> Option<Ref> {
    if a == b || !doc::is_a(class, "Constraint") || !doc.exists(a) || !doc.exists(b) {
        return None;
    }
    let (a0, a1, parent, made): (Ref, Ref, Ref, Vec<(Ref, Ref)>) =
        if is_attachment(doc, a) && is_attachment(doc, b) {
            let part = crate::body::attachment_part(doc, a)?;
            (a, b, parent_of(doc, part), Vec::new())
        } else if is_part(doc, a) && is_part(doc, b) {
            let xf0 = doc::xform_of(doc, a)?;
            let xf1 = doc::xform_of(doc, b)?;
            let size0 = doc::vec3_of(doc.prop(a, "Size"))?;
            let size1 = doc::vec3_of(doc.prop(b, "Size"))?;
            let p0 = closest_on_box(&xf0, size0, xf1.pos);
            let p1 = closest_on_box(&xf1, size1, xf0.pos);
            let joint = Xform {
                pos: (p0 + p1) * 0.5,
                rot: xf0.rot,
            };
            let attach = |doc: &mut Document, part: Ref, local: Xform| {
                doc.dom.insert(
                    part,
                    InstanceBuilder::new("Attachment")
                        .with_name(doc::default_instance_name("Attachment"))
                        .with_property("CFrame", Variant::CFrame(local.to_cframe())),
                )
            };
            let a0 = attach(doc, a, to_local(&xf0, &joint));
            let a1 = attach(doc, b, to_local(&xf1, &joint));
            (a0, a1, parent_of(doc, a), vec![(a0, a), (a1, b)])
        } else {
            return None;
        };
    let c = doc.dom.insert(
        parent,
        InstanceBuilder::new(class)
            .with_name(doc::default_instance_name(class))
            .with_property("Attachment0", Variant::Ref(a0))
            .with_property("Attachment1", Variant::Ref(a1)),
    );
    let mut created = made;
    created.push((c, parent));
    doc.record_drag(&format!("Insert {class}"), Vec::new(), created);
    Some(c)
}

#[cfg(test)]
mod tests {
    use super::*;
    use rbx_dom_weak::types::CFrame;

    fn part(doc: &mut Document, parent: Ref, name: &str, x: f32) -> Ref {
        doc.insert(
            parent,
            doc::new_part(name, 1, Vec3::new(x, 0.0, 0.0), Vec3::splat(4.0), [128; 3]),
        )
    }

    #[test]
    fn two_parts_get_meeting_attachments_in_one_undo_step() {
        let mut doc = Document::new_place();
        let ws = doc.workspace().unwrap();
        let a = part(&mut doc, ws, "A", 0.0);
        let b = part(&mut doc, ws, "B", 6.0);
        let undo = doc.undo_len();
        let c = insert_between(&mut doc, "HingeConstraint", a, b).expect("hinge");
        assert_eq!(doc.class_of(c).as_deref(), Some("HingeConstraint"));
        assert_eq!(doc.dom.get_by_ref(c).unwrap().parent(), ws);
        assert_eq!(doc.undo_len(), undo + 1, "one undo step");
        let a0 = match doc.prop(c, "Attachment0") {
            Some(Variant::Ref(r)) => r,
            v => panic!("Attachment0: {v:?}"),
        };
        let a1 = match doc.prop(c, "Attachment1") {
            Some(Variant::Ref(r)) => r,
            v => panic!("Attachment1: {v:?}"),
        };
        assert_eq!(doc.dom.get_by_ref(a0).unwrap().parent(), a);
        assert_eq!(doc.dom.get_by_ref(a1).unwrap().parent(), b);
        // The faces touch at x = 2 and x = 4; the attachments meet halfway.
        let w0 = crate::attachment::world(&doc, a0, true).unwrap();
        let w1 = crate::attachment::world(&doc, a1, true).unwrap();
        assert!((w0.pos - w1.pos).length() < 1e-4, "{:?} vs {:?}", w0.pos, w1.pos);
        assert!((w0.pos - Vec3::new(3.0, 0.0, 0.0)).length() < 1e-4, "{:?}", w0.pos);
        // Undo removes the attachments too; redo brings the three back.
        doc.undo();
        assert!(doc.dom.get_by_ref(c).is_none());
        assert!(doc.dom.get_by_ref(a0).is_none());
        assert!(doc.dom.get_by_ref(a1).is_none());
        doc.redo();
        assert!(doc.dom.get_by_ref(c).is_some());
        let a0b = match doc.prop(c, "Attachment0") {
            Some(Variant::Ref(r)) => r,
            v => panic!("Attachment0 after redo: {v:?}"),
        };
        assert!(doc.dom.get_by_ref(a0b).is_some());
    }

    #[test]
    fn selected_attachments_are_joined_as_they_are() {
        let mut doc = Document::new_place();
        let ws = doc.workspace().unwrap();
        let a = part(&mut doc, ws, "A", 0.0);
        let b = part(&mut doc, ws, "B", 10.0);
        let attach = |doc: &mut Document, parent: Ref, x: f32| {
            doc.insert(
                parent,
                InstanceBuilder::new("Attachment")
                    .with_name("Attachment")
                    .with_property(
                        "CFrame",
                        Variant::CFrame(Xform { pos: Vec3::new(x, 0.0, 0.0), rot: glam::Mat3::IDENTITY }.to_cframe()),
                    ),
            )
        };
        let a0 = attach(&mut doc, a, 2.0);
        let a1 = attach(&mut doc, b, -2.0);
        let attachments = doc.count_class("Attachment");
        let undo = doc.undo_len();
        let c = insert_between(&mut doc, "RigidConstraint", a0, a1).expect("rigid");
        assert_eq!(doc.count_class("Attachment"), attachments, "no new attachments");
        assert_eq!(doc.undo_len(), undo + 1, "one undo step");
        assert!(matches!(doc.prop(c, "Attachment0"), Some(Variant::Ref(r)) if r == a0));
        assert!(matches!(doc.prop(c, "Attachment1"), Some(Variant::Ref(r)) if r == a1));
    }

    #[test]
    fn a_part_and_an_attachment_are_not_a_joint() {
        let mut doc = Document::new_place();
        let ws = doc.workspace().unwrap();
        let a = part(&mut doc, ws, "A", 0.0);
        let b = part(&mut doc, ws, "B", 6.0);
        let a0 = doc.insert(
            a,
            InstanceBuilder::new("Attachment")
                .with_property("CFrame", Variant::CFrame(CFrame::identity())),
        );
        assert!(insert_between(&mut doc, "HingeConstraint", a, a0).is_none());
        // WeldConstraint is not a Constraint child (its own ribbon button).
        assert!(insert_between(&mut doc, "WeldConstraint", a, b).is_none());
    }
}
