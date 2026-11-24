//! Studio GeoGen::TrussConfig (Studio), face emitter Studio.
use glam::{Vec2, Vec3};
pub fn segments(length: f32) -> usize {
    (length * 0.5 + 0.005).max(0.0) as usize
}

/// Unit-box positions and size-scaled normals, shared by equal Size/Style parts.
pub fn generate(size: Vec3, style: u32) -> (Vec<Vec3>, Vec<Vec3>, Vec<Vec2>) {
    let axis = if size.y < size.x {
        if size.z < size.x {
            0
        } else {
            2
        }
    } else if size.y <= size.z {
        2
    } else {
        1
    };
    let scale = if size.min_element() >= 0.0001 {
        size.min_element() * 0.5
    } else {
        1.
    };
    let length = size[axis] / scale;
    let count = segments(length);
    let mut p = Vec::new();
    let mut n = Vec::new();
    let mut uv = Vec::new();
    if count == 0 {
        return (p, n, uv);
    }
    let along = Vec3::AXES[axis];
    let (a, b) = match axis {
        0 => (-Vec3::Z, Vec3::Y),
        1 => (Vec3::X, Vec3::Z),
        _ => (Vec3::X, -Vec3::Y),
    };
    let start = -along * length * 0.5;
    let mut quad = |origin: Vec3, u: Vec3, w: f32, v: Vec3, h: f32| {
        let normal = u.cross(v);
        let origin = origin + u * w.min(0.) + v * h.min(0.);
        let w = w.abs();
        let h = h.abs();
        for (x, y) in [(0., 0.), (1., 0.), (1., 1.), (0., 0.), (1., 1.), (0., 1.)] {
            p.push((origin + u * (x * w) + v * (y * h)) * scale / size);
            n.push(normal * size);
            uv.push(Vec2::new(x * w, y * h));
        }
    };
    // Four oriented faces; BridgeStyle has supports on the opposing pair only.
    for (face, (origin, u, out)) in [
        (start - a - b, a, b),
        (start - a + b, -b, a),
        (start + a + b, -a, -b),
        (start + a - b, b, -a),
    ]
    .into_iter()
    .enumerate()
    {
        quad(origin, -u, -0.25, along, count as f32 * 2.);
        quad(origin, out, 0.25, along, count as f32 * 2.);
        quad(origin + u * 0.25, -out, -0.25, along, count as f32 * 2.);
        quad(origin + out * 0.25, u, 0.25, along, count as f32 * 2.);
        for i in 0..=count {
            let y = if i == 0 { 0. } else { i as f32 * 2. - 0.25 };
            let h = if i == 0 || i == count { 0.25 } else { 0.5 };
            let lo = if i == 0 { 0. } else { 0.25 };
            let hi = if i == count { 0. } else { 0.25 };
            let o = origin + along * y;
            quad(o + u * 0.25, -u, -1.5, along, h);
            quad(o + u * 0.25 + out * 0.25, u, 1.5, along, h);
            quad(o + u * lo, u, 1.75 - lo, out, 0.25);
            quad(o + u * hi + along * h, -u, -(1.75 - hi), out, 0.25);
        }
        if count > 1 && (style == 0 || (style == 1 && face % 2 == 1)) {
            for i in 0..count {
                let flip = if style == 0 {
                    i % 2 == 1
                } else if face == 1 {
                    i >= count / 2
                } else {
                    i < count / 2
                };
                let direction = if flip { -along } else { along };
                let sign = if flip { -1. } else { 1. };
                let across = sign * (direction - u) * 0.707106;
                let diagonal = (direction + u) * 0.707106;
                let o = origin
                    + along * (i as f32 * 2. + if flip { 2. } else { 0. })
                    + (direction + u) * 0.125
                    + out * 0.0625
                    - across * 0.125;
                quad(o, across, 0.25, diagonal, 2.474873);
                quad(o + out * 0.125, -across, -0.25, diagonal, 2.474873);
                quad(o, -out, -0.125, diagonal, 2.474873);
                quad(
                    o + out * 0.125 + across * 0.25,
                    out,
                    -0.125,
                    diagonal,
                    2.474873,
                );
            }
        }
    }
    (p, n, uv)
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn segment_lengths_and_styles() {
        for length in [2., 4., 8., 16.] {
            let count = segments(length);
            assert_eq!(count, (length / 2.) as usize);
            for style in 0..3 {
                let (p, n, uv) = generate(Vec3::new(2., length, 2.), style);
                let support_faces = if count <= 1 {
                    0
                } else {
                    match style {
                        0 => 4,
                        1 => 2,
                        _ => 0,
                    }
                };
                assert_eq!(p.len(), ((count + 2) * 16 + count * support_faces * 4) * 6);
                assert_eq!(p.len(), n.len());
                assert_eq!(p.len(), uv.len());
                assert!(p
                    .iter()
                    .all(|p| p.is_finite() && p.abs().max_element() < 0.501));
            }
        }
    }
}
