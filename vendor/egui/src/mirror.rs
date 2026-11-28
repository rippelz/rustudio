//! rustudio patch: draws the whole window mirrored left to right (rustudio's
//! Luau Panic Mode). The app mirrors pointer input itself, so clicks land on
//! what is drawn under them. Paint callbacks (3D views) move to their
//! mirrored place but draw their own content unmirrored.

use std::sync::atomic::{AtomicBool, Ordering};

use epaint::{ClippedPrimitive, Primitive, Rect};

static MIRRORED: AtomicBool = AtomicBool::new(false);

/// Turns mirroring on or off.
pub fn set(on: bool) {
    MIRRORED.store(on, Ordering::Relaxed);
}

/// Whether the window is drawn mirrored.
pub fn is_on() -> bool {
    MIRRORED.load(Ordering::Relaxed)
}

/// `x` reflected across the middle of `screen`.
pub fn x(screen: Rect, x: f32) -> f32 {
    screen.min.x + screen.max.x - x
}

fn rect(screen: Rect, r: Rect) -> Rect {
    Rect::from_x_y_ranges(x(screen, r.max.x)..=x(screen, r.min.x), r.y_range())
}

/// The primitives reflected across the middle of `screen` (no back-face
/// culling in egui's renderers, so the reversed winding still draws).
pub(crate) fn apply(screen: Rect, primitives: &mut [ClippedPrimitive]) {
    for p in primitives {
        p.clip_rect = rect(screen, p.clip_rect);
        match &mut p.primitive {
            Primitive::Mesh(mesh) => {
                for v in &mut mesh.vertices {
                    v.pos.x = x(screen, v.pos.x);
                }
            }
            Primitive::Callback(callback) => callback.rect = rect(screen, callback.rect),
        }
    }
}
