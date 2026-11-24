//! The part-mesh vertex: what the mesh loader, the asset cache and the
//! EditableMesh store hand to the renderer, so it lives below all of them.

use bytemuck::{Pod, Zeroable};

#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable, Debug, PartialEq)]
pub struct Vertex {
    pub pos: [f32; 3],
    pub normal: [f32; 3],
    pub uv: [f32; 2],
    pub projection: [f32; 4],
    /// Per-vertex colour (sRGB 0..1); alpha 0 = use the part's Color.
    pub color: [f32; 4],
}
