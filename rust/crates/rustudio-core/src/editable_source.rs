//! The EditableImage / EditableMesh store, which lives in the runtime (the
//! engine, runtime/editable.rs) but is read from below it: the document's
//! `prop`, the asset and texture caches and the GUI caches ask here.
//!
//! The engine installs the store once, at start-up (`Runtime::new` calls
//! `editable::install`). With no store installed nothing is live: an
//! EditableImage or EditableMesh id resolves to nothing and draws nothing.

use glam::Vec3;
use rbx_dom_weak::types::Ref;

use crate::texture::Rgba;
use crate::vertex::Vertex;

pub const IMG_PREFIX: &str = "rbxtemp://rustudio/editable/img/";

/// What the runtime keeps for EditableImages and EditableMeshes.
pub trait EditableSource: Sync {
    /// `rbxtemp://rustudio/editable/img/<n>@<gen>` for an EditableImage (or
    /// the mesh form); `user` is the instance and property showing it.
    fn content_uri(&self, obj: Ref, user: Option<(Ref, &str)>) -> Option<String>;
    /// An editable image id's pixels and current generation.
    fn image_pixels(&self, id: &str) -> Option<(u64, Rgba)>;
    /// Whether `id` names a live EditableMesh copy.
    fn is_live(&self, id: &str) -> bool;
    /// That copy's vertices and the extent of its bounds.
    fn live_build(&self, id: &str) -> Option<(Vec<Vertex>, Vec3)>;
}

static SOURCE: std::sync::OnceLock<&'static dyn EditableSource> = std::sync::OnceLock::new();

/// The engine's store, installed once at start-up.
pub fn install(source: &'static dyn EditableSource) {
    let _ = SOURCE.set(source);
}

fn source() -> Option<&'static dyn EditableSource> {
    SOURCE.get().copied()
}

pub fn content_uri(obj: Ref, user: Option<(Ref, &str)>) -> Option<String> {
    source()?.content_uri(obj, user)
}

pub fn image_pixels(id: &str) -> Option<(u64, Rgba)> {
    source()?.image_pixels(id)
}

pub fn is_live(id: &str) -> bool {
    source().is_some_and(|s| s.is_live(id))
}

pub fn live_build(id: &str) -> Option<(Vec<Vertex>, Vec3)> {
    source()?.live_build(id)
}

/// For a texture cache: `rbxtemp://rustudio/editable/img/<n>@<gen>` → the
/// key without the generation and the generation.
pub fn image_key(id: &str) -> Option<(&str, u64)> {
    let rest = id.strip_prefix(IMG_PREFIX)?;
    let at = rest.find('@')?;
    let gen = rest[at + 1..].parse().ok()?;
    Some((&id[..IMG_PREFIX.len() + at], gen))
}

/// A cache key with every editable image generation taken out (composites
/// name several ids), or None when it names none.
pub fn strip_generations(key: &str) -> Option<String> {
    if !key.contains(IMG_PREFIX) {
        return None;
    }
    let mut out = String::with_capacity(key.len());
    let mut rest = key;
    while let Some(i) = rest.find(IMG_PREFIX) {
        let after = i + IMG_PREFIX.len();
        out.push_str(&rest[..after]);
        let tail = &rest[after..];
        let digits = tail.bytes().take_while(u8::is_ascii_digit).count();
        out.push_str(&tail[..digits]);
        let mut t = &tail[digits..];
        if let Some(g) = t.strip_prefix('@') {
            let gd = g.bytes().take_while(u8::is_ascii_digit).count();
            t = &g[gd..];
        }
        rest = t;
    }
    out.push_str(rest);
    Some(out)
}
