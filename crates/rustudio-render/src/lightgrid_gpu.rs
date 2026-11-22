//! Light grid texture uploads (lightgrid.rs) made ready where the bytes
//! are: the grid job writes its skylight and local channels straight into
//! one mapped staging buffer in the textures' row layout, so taking a
//! finished grid on the main thread only records copies into the next
//! frame's encoder (`Renderer::upload_lightgrid`). A `queue.write_texture`
//! of the whole grid (8 MB skylight + 17 MB local) on the main thread
//! allocated a staging buffer and copied it there: up to 17 ms a build.
//! Every write of the two textures goes through here, so copies land in
//! the order they were made.

use egui_wgpu::wgpu;

use crate::lightgrid::dims;

/// Which grid texture a region goes to.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GridTexture {
    /// Skylight and sun, two bytes a voxel (`LightGrid::data`).
    Sky,
    /// Filtered local radiance, four bytes a voxel (`LightGrid::local`).
    Local,
}

impl GridTexture {
    fn texel_bytes(self) -> usize {
        match self {
            GridTexture::Sky => 2,
            GridTexture::Local => 4,
        }
    }
}

/// Voxels `lo..=hi` (grid X, Y, Z) of one texture. `src` is the whole
/// grid's bytes (X fastest, then Z, then Y) or, `packed`, the box's own
/// bytes in the same order (`LocalPatch::data`).
pub struct GridRegion<'a> {
    pub texture: GridTexture,
    pub lo: [usize; 3],
    pub hi: [usize; 3],
    pub src: &'a [u8],
    pub packed: bool,
}

struct RegionCopy {
    texture: GridTexture,
    offset: u64,
    bytes_per_row: u32,
    lo: [usize; 3],
    hi: [usize; 3],
}

/// Regions written into a staging buffer, waiting to be copied.
pub struct GridUpload {
    buffer: Option<wgpu::Buffer>,
    copies: Vec<RegionCopy>,
}

impl GridUpload {
    /// Lays `regions` out in one mapped buffer (rows padded to wgpu's copy
    /// alignment) and fills it on the calling thread. Regions outside the
    /// grid or whose bytes don't match their box are left out.
    pub fn new(device: &wgpu::Device, regions: &[GridRegion<'_>]) -> GridUpload {
        let [nx, ny, nz] = dims();
        let mut copies = Vec::with_capacity(regions.len());
        let mut used = Vec::with_capacity(regions.len());
        let mut size = 0u64;
        for r in regions {
            let bpp = r.texture.texel_bytes();
            if (0..3).any(|k| r.lo[k] > r.hi[k] || r.hi[k] >= [nx, ny, nz][k]) {
                continue;
            }
            let [w, d, h] = [0, 1, 2].map(|k| r.hi[k] - r.lo[k] + 1);
            let want = if r.packed { w * h * d * bpp } else { nx * ny * nz * bpp };
            if r.src.len() != want {
                continue;
            }
            let row = w * bpp;
            let bytes_per_row = row.next_multiple_of(wgpu::COPY_BYTES_PER_ROW_ALIGNMENT as usize);
            copies.push(RegionCopy { texture: r.texture, offset: size, bytes_per_row: bytes_per_row as u32, lo: r.lo, hi: r.hi });
            used.push(r);
            size += (bytes_per_row * h * d) as u64;
        }
        if copies.is_empty() {
            return GridUpload { buffer: None, copies };
        }
        let buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("light grid upload"),
            size,
            usage: wgpu::BufferUsages::COPY_SRC | wgpu::BufferUsages::MAP_WRITE,
            mapped_at_creation: true,
        });
        {
            let Ok(mut view) = buffer.get_mapped_range_mut(..) else {
                return GridUpload { buffer: None, copies: Vec::new() };
            };
            for (c, r) in copies.iter().zip(used) {
                let bpp = r.texture.texel_bytes();
                let [w, d, h] = [0, 1, 2].map(|k| r.hi[k] - r.lo[k] + 1);
                let row = w * bpp;
                let bpr = c.bytes_per_row as usize;
                let at = c.offset as usize;
                if !r.packed && w == nx && h == nz && bpr == row {
                    // (whole layers: one run of the grid's bytes)
                    let from = r.lo[1] * nz * nx * bpp;
                    view.slice(at..at + d * h * row).copy_from_slice(&r.src[from..from + d * h * row]);
                    continue;
                }
                for y in 0..d {
                    for z in 0..h {
                        let dst = at + (y * h + z) * bpr;
                        let src = if r.packed {
                            (y * h + z) * row
                        } else {
                            (((r.lo[1] + y) * nz + r.lo[2] + z) * nx + r.lo[0]) * bpp
                        };
                        view.slice(dst..dst + row).copy_from_slice(&r.src[src..src + row]);
                    }
                }
            }
        }
        buffer.unmap();
        GridUpload { buffer: Some(buffer), copies }
    }

    /// A build's upload (lightgrid::build_grid): from scratch, both whole
    /// channels; built on the last one, the skylight's changed box and the
    /// local patches. The bytes the GPU then has are let go from `build`.
    pub fn of_build(device: &wgpu::Device, build: &mut crate::lightgrid::GridBuild) -> GridUpload {
        let [nx, ny, nz] = dims();
        let full = ([0; 3], [nx - 1, ny - 1, nz - 1]);
        let mut regions = Vec::with_capacity(2 + build.patches.len());
        let sky = if build.incremental { build.changed } else { Some(full) };
        if let Some((lo, hi)) = sky {
            regions.push(GridRegion { texture: GridTexture::Sky, lo, hi, src: &build.grid.data, packed: false });
        }
        if !build.incremental {
            regions.push(GridRegion { texture: GridTexture::Local, lo: full.0, hi: full.1, src: &build.grid.local, packed: false });
        }
        regions.extend(build.patches.iter().map(|p| GridRegion { texture: GridTexture::Local, lo: p.lo, hi: p.hi, src: &p.data, packed: true }));
        let up = GridUpload::new(device, &regions);
        build.grid.local = Vec::new();
        build.patches = Vec::new();
        up
    }

    /// Re-lit boxes of the local channel (LocalLights::update).
    pub fn of_patches(device: &wgpu::Device, patches: &[crate::lightgrid::LocalPatch]) -> GridUpload {
        let regions: Vec<_> = patches.iter().map(|p| GridRegion { texture: GridTexture::Local, lo: p.lo, hi: p.hi, src: &p.data, packed: true }).collect();
        GridUpload::new(device, &regions)
    }

    pub fn is_empty(&self) -> bool {
        self.buffer.is_none()
    }

    /// Records the copies into `encoder` (the texture is X, Z, Y: rows of
    /// X along Z, layers along Y).
    pub(crate) fn encode(&self, encoder: &mut wgpu::CommandEncoder, sky: &wgpu::Texture, local: &wgpu::Texture) {
        let Some(buffer) = &self.buffer else { return };
        for c in &self.copies {
            let [w, d, h] = [0, 1, 2].map(|k| (c.hi[k] - c.lo[k] + 1) as u32);
            encoder.copy_buffer_to_texture(
                wgpu::TexelCopyBufferInfo {
                    buffer,
                    layout: wgpu::TexelCopyBufferLayout { offset: c.offset, bytes_per_row: Some(c.bytes_per_row), rows_per_image: Some(h) },
                },
                wgpu::TexelCopyTextureInfo {
                    texture: if c.texture == GridTexture::Sky { sky } else { local },
                    mip_level: 0,
                    origin: wgpu::Origin3d { x: c.lo[0] as u32, y: c.lo[2] as u32, z: c.lo[1] as u32 },
                    aspect: wgpu::TextureAspect::All,
                },
                wgpu::Extent3d { width: w, height: h, depth_or_array_layers: d },
            );
        }
    }
}
