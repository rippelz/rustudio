//! Shared attachment frames. Ancestor Bone.Transform affects descendants.
//! A Bone's own Transform appears only in its transformed frame.
use glam::{Mat3, Vec3};
use rbx_dom_weak::types::{Ref, Variant};
use crate::{doc::{self, Document}, math::Xform};

fn identity() -> Xform { Xform { pos: Vec3::ZERO, rot: Mat3::IDENTITY } }
fn frame(d: &Document, r: Ref, key: &str) -> Xform {
    match d.prop(r, key) { Some(Variant::CFrame(c)) => Xform::from_cframe(&c), _ => identity() }
}
pub fn compose(a: Xform, b: Xform) -> Xform {
    Xform { pos: a.apply(b.pos), rot: a.rot * b.rot }
}
pub fn local(d: &Document, r: Ref, transformed: bool) -> Xform {
    let cf = frame(d, r, "CFrame");
    if transformed && d.class_of(r).is_some_and(|c| c == "Bone") {
        compose(cf, frame(d, r, "Transform"))
    } else { cf }
}
pub fn parent(d: &Document, r: Ref, transformed: bool) -> Option<Xform> {
    let mut chain = Vec::new();
    let mut cur = d.dom.get_by_ref(r)?.parent();
    // (a parent cycle can't be built through Parent; the bound stands in
    // for the visited set, which was an allocation per read)
    let mut steps = 0u32;
    while let Some(i) = d.dom.get_by_ref(cur) {
        steps += 1;
        if steps > 4096 { return None; }
        if doc::is_a(&i.class, "Attachment") {
            chain.push(cur);
            cur = i.parent();
        } else if doc::is_a(&i.class, "BasePart") {
            let mut result = if i.class != "Terrain" && doc::is_a(&i.class, "BasePart") {
                frame(d, cur, "CFrame")
            } else { identity() };
            for a in chain.into_iter().rev() { result = compose(result, local(d, a, transformed)); }
            return Some(result);
        } else {
            cur = i.parent();
        }
    }
    let mut result = identity();
    for a in chain.into_iter().rev() { result = compose(result, local(d, a, transformed)); }
    Some(result)
}
pub fn world(d: &Document, r: Ref, transformed: bool) -> Option<Xform> {
    Some(compose(parent(d, r, true)?, local(d, r, transformed)))
}

#[cfg(test)]
#[path = "selftest/attachment.rs"]
mod tests;
