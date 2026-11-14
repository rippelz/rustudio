//! Smooth terrain: decodes `Terrain.SmoothGrid` into sparse voxels and builds
//! smooth per-material surface meshes (render) plus a merged solid mesh
//! (collision).
//!
//! Format notes, from public community documentation only (rbx-dom's
//! `docs/smooth-grid.md` in pull request 444, zeux.io's 2017 "Voxel terrain:
//! storage" article, and the independent write-ups that checked it):
//!
//! * byte 0: version (only 1 is known), byte 1: log2 of the chunk edge (5 =>
//!   32^3 chunks; 0..=8 accepted).
//! * then chunks until the end of the blob, no count. Each chunk starts with
//!   its offset from the previous chunk (from 0,0,0 for the first), in chunk
//!   units: three big-endian `i32`s (X, Y, Z) whose bytes are interleaved
//!   (X0 Y0 Z0 X1 Y1 Z1 ...; byte 0 = most significant).
//! * voxels in the chunk follow as runs, X fastest, then Z, then Y. A run is a
//!   flag byte (bits 0-5 material index, bit 6 = occupancy byte follows,
//!   bit 7 = count byte follows), then the occupancy byte, then the count byte.
//!   A count byte `c > 0` means `c + 1` voxels; `c == 0` means one voxel
//!   followed by a shoreline water-occupancy byte. No occupancy byte means
//!   full. Occupancy byte `b` decodes to `(b + 1) / 256`; Air is always empty.
//! * voxel `v` covers studs `4v .. 4v + 4` (centre at `4v + 2`); chunk `c`
//!   starts at voxel `32c` (stud `128c`).
#![allow(dead_code)]

#[path = "grass_pack.rs"]
pub(crate) mod grass_pack;

use std::collections::HashMap;

/// Studs per voxel edge.
pub const VOXEL: f32 = 4.0;
/// SmoothGrid material index of Water.
pub const WATER: u8 = 1;

const CHUNK_SHIFT: u32 = 5;
const CS: usize = 1 << CHUNK_SHIFT;
const CV: usize = CS * CS * CS;
const ISO: f32 = 0.5;
/// Largest accepted |voxel coordinate| (keeps all arithmetic inside i32).
const COORD_LIMIT: i64 = 1 << 29;

/// SmoothGrid material slots, in index order (a subset of Enum.Material).
const MATERIALS: [&str; 23] = [
    "Air",
    "Water",
    "Grass",
    "Slate",
    "Concrete",
    "Brick",
    "Sand",
    "WoodPlanks",
    "Rock",
    "Glacier",
    "Snow",
    "Sandstone",
    "Mud",
    "Basalt",
    "Ground",
    "CrackedLava",
    "Asphalt",
    "Cobblestone",
    "Ice",
    "LeafyGrass",
    "Salt",
    "Limestone",
    "Pavement",
];

/// Enum.Material name for a SmoothGrid material index ("Unknown" past the table).
pub fn material_name(m: u8) -> &'static str {
    MATERIALS.get(m as usize).copied().unwrap_or("Unknown")
}

/// SmoothGrid index for an Enum.Material name.
pub fn material_index(name: &str) -> Option<u8> {
    MATERIALS.iter().position(|n| *n == name).map(|i| i as u8)
}

/// Studio's default terrain colours: the `base_color` of each material in
/// its PlatformContent/pc/terrain/materials.json (read once, read only).
/// They are what Studio's Terrain:GetMaterialColor returns in a new place
/// (checked in an offline Studio for Grass, Rock, Sand, Snow, Mud, Slate and Brick).
fn studio_base_colors() -> Option<&'static std::collections::HashMap<String, [u8; 3]>> {
    use std::sync::OnceLock;
    static COLORS: OnceLock<Option<std::collections::HashMap<String, [u8; 3]>>> = OnceLock::new();
    COLORS
        .get_or_init(|| {
            let path = crate::default_scripts::studio_dir()?
                .join("PlatformContent/pc/terrain/materials.json");
            let json: serde_json::Value =
                serde_json::from_slice(&std::fs::read(path).ok()?).ok()?;
            let mut out = std::collections::HashMap::new();
            for m in json.get("materials")?.as_array()? {
                let (Some(name), Some(c)) = (
                    m.get("name").and_then(|n| n.as_str()),
                    m.get("base_color").and_then(|c| c.as_array()),
                ) else {
                    continue;
                };
                let ch = |i: usize| c.get(i).and_then(|v| v.as_u64()).unwrap_or(0).min(255) as u8;
                out.insert(name.to_string(), [ch(0), ch(1), ch(2)]);
            }
            Some(out)
        })
        .as_ref()
}

/// Flat colour per material: Studio's default (`studio_base_colors`) when
/// its install is found, else our own estimate picked by eye. A place's
/// `Terrain.MaterialColors` blob (23 RGB triples in this same slot order,
/// first two reserved) overrides them.
pub fn material_color(m: u8) -> [u8; 3] {
    if let Some(c) = studio_base_colors().and_then(|t| t.get(material_name(m))) {
        return *c;
    }
    match m {
        0 => [0, 0, 0],
        1 => [44, 112, 158],   // Water
        2 => [98, 138, 60],    // Grass
        3 => [64, 68, 72],     // Slate
        4 => [128, 126, 121],  // Concrete
        5 => [138, 74, 54],    // Brick
        6 => [198, 182, 132],  // Sand
        7 => [146, 108, 70],   // WoodPlanks
        8 => [104, 100, 94],   // Rock
        9 => [152, 198, 216],  // Glacier
        10 => [232, 237, 242], // Snow
        11 => [176, 122, 84],  // Sandstone
        12 => [82, 64, 48],    // Mud
        13 => [46, 43, 45],    // Basalt
        14 => [102, 84, 60],   // Ground
        15 => [196, 92, 44],   // CrackedLava
        16 => [72, 72, 74],    // Asphalt
        17 => [116, 110, 102], // Cobblestone
        18 => [168, 204, 232], // Ice
        19 => [112, 148, 64],  // LeafyGrass
        20 => [208, 204, 198], // Salt
        21 => [204, 194, 166], // Limestone
        22 => [148, 148, 143], // Pavement
        _ => [200, 0, 200],
    }
}

pub fn is_water(m: u8) -> bool {
    m == WATER
}

/// Occupancy byte -> fraction (byte 255 = full, byte 0 = 1/256, not empty).
#[inline]
pub fn occupancy_from_byte(b: u8) -> f32 {
    (b as f32 + 1.0) / 256.0
}

/// The occupancy byte Studio stores for a share (0..1): round(share × 255),
/// read back as (byte + 1) / 256; a share that rounds to 0 leaves the voxel
/// Air (None). Measured with ReadVoxels after FillBlock / FillBall in the
/// offline Studio (round 21, tools/fidelity/terrain_probe.luau): 0.25 reads
/// 0.254, 0.5 reads 0.504, 0.9727 stays 0.9727, and the ball's faintest rim
/// isn't written.
pub fn occupancy_to_byte(share: f32) -> Option<u8> {
    let b = (share.clamp(0.0, 1.0) * crate::mech::TERRAIN_OCCUPANCY_SCALE).round() as i32;
    (b > 0).then_some(b.min(255) as u8)
}

/// Centre of voxel `v` in world studs.
pub fn voxel_center(v: [i32; 3]) -> [f32; 3] {
    v.map(|c| (c as f32 + 0.5) * VOXEL)
}

#[inline]
fn lidx(x: usize, y: usize, z: usize) -> usize {
    x + CS * (z + CS * y)
}

#[inline]
fn chunk_key(v: [i32; 3]) -> [i32; 3] {
    v.map(|c| c >> CHUNK_SHIFT)
}

#[inline]
fn local_index(v: [i32; 3]) -> usize {
    let m = (CS - 1) as i32;
    lidx(
        (v[0] & m) as usize,
        (v[1] & m) as usize,
        (v[2] & m) as usize,
    )
}

struct Chunk {
    mat: Box<[u8]>,
    /// Raw occupancy byte (meaningless where mat == 0).
    occ: Box<[u8]>,
    /// Shoreline water byte + 1 (0 = none); only allocated if the chunk has any.
    water: Option<Box<[u16]>>,
}

impl Chunk {
    fn new() -> Self {
        Chunk {
            mat: vec![0; CV].into_boxed_slice(),
            occ: vec![0; CV].into_boxed_slice(),
            water: None,
        }
    }

    fn write(&mut self, i: usize, n: usize, mat: u8, occ: u8, water: u16) {
        self.mat[i..i + n].fill(mat);
        self.occ[i..i + n].fill(occ);
        if water != 0 || self.water.is_some() {
            let w = self
                .water
                .get_or_insert_with(|| vec![0; CV].into_boxed_slice());
            w[i..i + n].fill(water);
        }
    }
}

/// Sparse voxel grid, stored as 32^3 chunks keyed by chunk coordinate.
/// `Voxels::get` with the last chunk remembered (see `Voxels::sampler`).
pub struct Sampler<'a> {
    v: &'a Voxels,
    last: std::cell::Cell<Option<([i32; 3], Option<&'a Chunk>)>>,
}

impl Sampler<'_> {
    pub fn get(&self, x: i32, y: i32, z: i32) -> (u8, f32) {
        let v = [x, y, z];
        let k = chunk_key(v);
        let c = match self.last.get() {
            Some((lk, c)) if lk == k => c,
            _ => {
                let c = self.v.chunks.get(&k);
                self.last.set(Some((k, c)));
                c
            }
        };
        match c {
            None => (0, 0.0),
            Some(c) => {
                let i = local_index(v);
                match c.mat[i] {
                    0 => (0, 0.0),
                    m => (m, occupancy_from_byte(c.occ[i])),
                }
            }
        }
    }
}

pub struct Voxels {
    chunks: HashMap<[i32; 3], Chunk>,
    count: usize,
}

impl std::fmt::Debug for Voxels {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Voxels")
            .field("chunks", &self.chunks.len())
            .field("count", &self.count)
            .finish()
    }
}

struct Reader<'a> {
    b: &'a [u8],
    pos: usize,
}

impl<'a> Reader<'a> {
    fn byte(&mut self) -> Result<u8, String> {
        let v = *self
            .b
            .get(self.pos)
            .ok_or_else(|| format!("SmoothGrid: truncated at byte {}", self.pos))?;
        self.pos += 1;
        Ok(v)
    }

    fn take(&mut self, n: usize) -> Result<&'a [u8], String> {
        let s = self
            .b
            .get(self.pos..self.pos + n)
            .ok_or_else(|| format!("SmoothGrid: truncated at byte {}", self.pos))?;
        self.pos += n;
        Ok(s)
    }
}

impl Voxels {
    pub fn empty() -> Self {
        Voxels {
            chunks: HashMap::new(),
            count: 0,
        }
    }

    /// Decodes a SmoothGrid blob. An empty blob is an empty grid.
    pub fn decode(bytes: &[u8]) -> Result<Voxels, String> {
        let mut out = Voxels::empty();
        if bytes.is_empty() {
            return Ok(out);
        }
        if bytes.len() < 2 {
            return Err("SmoothGrid: truncated header".into());
        }
        let version = bytes[0];
        if version != 1 {
            return Err(format!(
                "SmoothGrid: unsupported version {version} (only version 1 is known)"
            ));
        }
        let exp = bytes[1] as u32;
        if exp > 8 {
            return Err(format!(
                "SmoothGrid: chunk size exponent {exp} out of range"
            ));
        }
        let edge = 1usize << exp;
        let cells = edge * edge * edge;
        let mut r = Reader { b: bytes, pos: 2 };
        let mut cur = [0i32; 3];
        while r.pos < bytes.len() {
            let off = r.take(12)?;
            let mut d = [0u32; 3];
            for k in 0..4 {
                for a in 0..3 {
                    d[a] = (d[a] << 8) | off[k * 3 + a] as u32;
                }
            }
            for a in 0..3 {
                cur[a] = cur[a].wrapping_add(d[a] as i32);
            }
            let origin = cur.map(|c| c as i64 * edge as i64);
            if origin.iter().any(|o| o.abs() + edge as i64 > COORD_LIMIT) {
                return Err(format!(
                    "SmoothGrid: chunk {cur:?} is outside the supported range"
                ));
            }
            let origin = origin.map(|o| o as i32);
            let mut i = 0usize;
            while i < cells {
                let at = r.pos;
                let flag = r.byte()?;
                let mat = flag & 0x3f;
                if mat as usize >= MATERIALS.len() {
                    return Err(format!(
                        "SmoothGrid: unknown material index {mat} at byte {at}"
                    ));
                }
                let occ = if flag & 0x40 != 0 { r.byte()? } else { 255 };
                let mut n = 1usize;
                let mut water = 0u16;
                if flag & 0x80 != 0 {
                    let c = r.byte()?;
                    if c == 0 {
                        water = r.byte()? as u16 + 1;
                    } else {
                        n = c as usize + 1;
                    }
                }
                if i + n > cells {
                    return Err(format!(
                        "SmoothGrid: run at byte {at} runs past the end of its chunk"
                    ));
                }
                if mat != 0 {
                    // Shoreline water only exists beside a partial solid.
                    if mat == WATER || occ == 255 {
                        water = 0;
                    }
                    if exp == CHUNK_SHIFT {
                        out.chunk_mut(cur).write(i, n, mat, occ, water);
                    } else {
                        for j in i..i + n {
                            let l = [j % edge, j / (edge * edge), (j / edge) % edge];
                            let v = [
                                origin[0] + l[0] as i32,
                                origin[1] + l[1] as i32,
                                origin[2] + l[2] as i32,
                            ];
                            out.chunk_mut(chunk_key(v))
                                .write(local_index(v), 1, mat, occ, water);
                        }
                    }
                }
                i += n;
            }
        }
        out.finish();
        Ok(out)
    }

    fn chunk_mut(&mut self, key: [i32; 3]) -> &mut Chunk {
        self.chunks.entry(key).or_insert_with(Chunk::new)
    }

    fn finish(&mut self) {
        self.chunks.retain(|_, c| c.mat.iter().any(|&m| m != 0));
        self.count = self
            .chunks
            .values()
            .map(|c| c.mat.iter().filter(|&&m| m != 0).count())
            .sum();
    }

    /// Writes one voxel: SmoothGrid material index and occupancy (0..1);
    /// material 0 (Air) clears it.
    pub fn set(&mut self, v: [i32; 3], mat: u8, occupancy: f32) {
        self.set_many([(v, mat, occupancy)]);
    }

    /// Writes many voxels (Terrain:WriteVoxels): (voxel, material, occupancy),
    /// as `set`, with one recount at the end.
    pub fn set_many(&mut self, voxels: impl IntoIterator<Item = ([i32; 3], u8, f32)>) {
        for (v, mat, occupancy) in voxels {
            // No occupancy is Air whatever the material.
            let (mat, occ) = match occupancy_to_byte(occupancy) {
                Some(b) if mat != 0 => (mat, b),
                _ => (0, 0),
            };
            self.chunk_mut(chunk_key(v)).write(local_index(v), 1, mat, occ, 0);
        }
        self.finish();
    }

    /// Writes voxels as `set_many`, keeping the count as it goes (no full
    /// recount): the Terrain Editor's brush, every frame on big places.
    pub fn edit(&mut self, voxels: impl IntoIterator<Item = ([i32; 3], u8, f32)>) {
        let mut touched: std::collections::HashSet<[i32; 3]> = std::collections::HashSet::new();
        for (v, mat, occupancy) in voxels {
            let (mat, occ) = match occupancy_to_byte(occupancy) {
                Some(b) if mat != 0 => (mat, b),
                _ => (0, 0),
            };
            let (old, _) = self.get(v[0], v[1], v[2]);
            if old == 0 && mat == 0 {
                continue;
            }
            let key = chunk_key(v);
            self.chunk_mut(key).write(local_index(v), 1, mat, occ, 0);
            self.count = self.count + usize::from(mat != 0) - usize::from(old != 0);
            touched.insert(key);
        }
        for k in touched {
            if self.chunks.get(&k).is_some_and(|c| c.mat.iter().all(|&m| m == 0)) {
                self.chunks.remove(&k);
            }
        }
    }

    /// Number of non-Air voxels (Water included).
    pub fn count(&self) -> usize {
        self.count
    }

    /// Number of stored (non-empty) 32^3 chunks.
    pub fn chunk_count(&self) -> usize {
        self.chunks.len()
    }

    /// Material and solid occupancy (0..1) of a voxel; Air is `(0, 0.0)`.
    pub fn get(&self, x: i32, y: i32, z: i32) -> (u8, f32) {
        let v = [x, y, z];
        match self.chunks.get(&chunk_key(v)) {
            None => (0, 0.0),
            Some(c) => {
                let i = local_index(v);
                match c.mat[i] {
                    0 => (0, 0.0),
                    m => (m, occupancy_from_byte(c.occ[i])),
                }
            }
        }
    }

    /// Whether a world point is in water (a Water voxel, or shoreline water
    /// in a solid one).
    pub fn water_at(&self, p: [f32; 3]) -> bool {
        let v = p.map(|x| (x / VOXEL).floor() as i32);
        let Some(c) = self.chunks.get(&chunk_key(v)) else {
            return false;
        };
        let i = local_index(v);
        c.mat[i] == WATER || c.water.as_ref().is_some_and(|w| w[i] != 0)
    }

    /// Whether two grids hold the same solid (non-water) voxels, which is all
    /// collision is made of: a water fill (Place B's start) leaves it as it was.
    pub fn same_solid(&self, other: &Voxels) -> bool {
        let solid = |c: Option<&Chunk>, i: usize| match c {
            Some(c) if c.mat[i] != 0 && c.mat[i] != WATER => (c.mat[i], c.occ[i]),
            _ => (0, 0),
        };
        let keys: std::collections::HashSet<&[i32; 3]> =
            self.chunks.keys().chain(other.chunks.keys()).collect();
        keys.into_iter().all(|k| {
            let (a, b) = (self.chunks.get(k), other.chunks.get(k));
            (0..CV).all(|i| solid(a, i) == solid(b, i))
        })
    }

    /// A reader for many nearby `get`s (ray marches): it keeps the last
    /// chunk it looked up, so neighbouring voxels cost no hash lookup.
    pub fn sampler(&self) -> Sampler<'_> {
        Sampler {
            v: self,
            last: std::cell::Cell::new(None),
        }
    }

    /// Whether `p` is under the water as it is drawn: the surface-nets level
    /// of the column (a top water voxel of occupancy o reaches o voxels above
    /// its centre) less WATER_RAY_DROP, where Studio draws it. The camera's
    /// underwater fog uses this; a whole-voxel test turned it on up to two
    /// studs above the visible water (Pirate Island, voxels 62 % full).
    pub fn under_drawn_water(&self, p: [f32; 3]) -> bool {
        let y = p[1] + WATER_RAY_DROP;
        let v = [p[0], y, p[2]].map(|x| (x / VOXEL).floor() as i32);
        let level = |vy: i32| (vy as f32 + 0.5) * VOXEL + self.water(v[0], vy, v[2]) * VOXEL;
        if self.water(v[0], v[1], v[2]) > 0.0 {
            self.water(v[0], v[1] + 1, v[2]) > 0.0 || y < level(v[1])
        } else {
            self.water(v[0], v[1] - 1, v[2]) > 0.0 && y < level(v[1] - 1)
        }
    }

    /// Water occupancy (0..1): the occupancy of a Water voxel, or the shoreline
    /// water stored beside a partial solid.
    pub fn water(&self, x: i32, y: i32, z: i32) -> f32 {
        let v = [x, y, z];
        let Some(c) = self.chunks.get(&chunk_key(v)) else {
            return 0.0;
        };
        let i = local_index(v);
        if c.mat[i] == WATER {
            return occupancy_from_byte(c.occ[i]);
        }
        match &c.water {
            Some(w) if w[i] != 0 => w[i] as f32 / 256.0,
            _ => 0.0,
        }
    }

    /// Whether the 32³ storage chunk holding voxel `v` exists (it may still
    /// be all Air).
    pub fn chunk_has_voxels(&self, v: [i32; 3]) -> bool {
        self.chunks.contains_key(&chunk_key(v))
    }

    /// Whether chunk `key` or any chunk beside it (the ones its surface is
    /// made from) is stored: if not, the chunk has no surface at all.
    pub fn chunk_near(&self, key: [i32; 3]) -> bool {
        (-1..=1).any(|dx| {
            (-1..=1).any(|dy| (-1..=1).any(|dz| self.chunks.contains_key(&[key[0] + dx, key[1] + dy, key[2] + dz])))
        })
    }

    /// The lowest and highest voxel row any stored chunk covers (cheap: from
    /// the chunk keys, so it can be a little wider than the voxels).
    pub fn y_range(&self) -> Option<(i32, i32)> {
        let lo = self.chunks.keys().map(|k| k[1]).min()?;
        let hi = self.chunks.keys().map(|k| k[1]).max()?;
        Some((lo << CHUNK_SHIFT, (hi << CHUNK_SHIFT) + CS as i32 - 1))
    }

    /// Inclusive min/max voxel coordinates of non-Air voxels.
    pub fn bounds(&self) -> Option<([i32; 3], [i32; 3])> {
        let mut lo = [i32::MAX; 3];
        let mut hi = [i32::MIN; 3];
        for (k, c) in &self.chunks {
            let base = k.map(|c| c << CHUNK_SHIFT);
            for y in 0..CS {
                for z in 0..CS {
                    let s = lidx(0, y, z);
                    let row = &c.mat[s..s + CS];
                    let Some(x0) = row.iter().position(|&m| m != 0) else {
                        continue;
                    };
                    let x1 = row.iter().rposition(|&m| m != 0).unwrap_or(x0);
                    let a = [base[0] + x0 as i32, base[1] + y as i32, base[2] + z as i32];
                    let b = [base[0] + x1 as i32, a[1], a[2]];
                    for i in 0..3 {
                        lo[i] = lo[i].min(a[i]);
                        hi[i] = hi[i].max(b[i]);
                    }
                }
            }
        }
        (lo[0] <= hi[0]).then_some((lo, hi))
    }

    /// World-space (studs) box enclosing every non-Air voxel.
    pub fn world_bounds(&self) -> Option<([f32; 3], [f32; 3])> {
        self.bounds().map(|(lo, hi)| {
            (
                lo.map(|c| c as f32 * VOXEL),
                hi.map(|c| (c + 1) as f32 * VOXEL),
            )
        })
    }

    /// Finds Workspace.Terrain and decodes its SmoothGrid. `None` when there is
    /// no Terrain or it has no SmoothGrid property.
    pub fn from_dom(dom: &rbx_dom_weak::WeakDom) -> Option<Result<Voxels, String>> {
        let ws = dom
            .root()
            .children()
            .iter()
            .filter_map(|r| dom.get_by_ref(*r))
            .find(|i| i.class.as_str() == "Workspace")?;
        let terrain = ws
            .children()
            .iter()
            .filter_map(|r| dom.get_by_ref(*r))
            .find(|i| i.class.as_str() == "Terrain")?;
        match terrain.properties.get(&rbx_dom_weak::ustr("SmoothGrid"))? {
            rbx_types::Variant::BinaryString(b) => Some(Voxels::decode(b.as_ref())),
            rbx_types::Variant::SharedString(s) => Some(Voxels::decode(s.data())),
            other => Some(Err(format!(
                "SmoothGrid has unexpected type {:?}",
                other.ty()
            ))),
        }
    }
}

// ---------------------------------------------------------------- editing

impl Voxels {
    /// The SmoothGrid blob `decode` reads: version 1, 32³ chunks, each a
    /// 12-byte offset from the previous chunk (four bytes per axis,
    /// interleaved, most significant first) then runs of
    /// `flag (material | 0x40 occupancy byte | 0x80 count byte) [occ] [n - 1]`,
    /// a count byte of 0 carrying one voxel's shoreline water instead.
    pub fn encode(&self) -> Vec<u8> {
        let mut out = vec![1u8, CHUNK_SHIFT as u8];
        let mut keys: Vec<[i32; 3]> = self.chunks.keys().copied().collect();
        keys.sort();
        let mut prev = [0i32; 3];
        for key in keys {
            let c = &self.chunks[&key];
            let d = [0, 1, 2].map(|a| key[a].wrapping_sub(prev[a]) as u32);
            prev = key;
            for k in 0..4 {
                for v in d {
                    out.push((v >> (24 - 8 * k)) as u8);
                }
            }
            let water = |i: usize| c.water.as_ref().map_or(0, |w| w[i]);
            let mut i = 0usize;
            while i < CV {
                let (mat, occ) = (c.mat[i], if c.mat[i] == 0 { 255 } else { c.occ[i] });
                let w = if mat != 0 && mat != WATER && occ != 255 { water(i) } else { 0 };
                let mut flag = mat;
                if occ != 255 {
                    flag |= 0x40;
                }
                if w != 0 {
                    out.extend([flag | 0x80]);
                    if occ != 255 {
                        out.push(occ);
                    }
                    out.extend([0, (w - 1) as u8]);
                    i += 1;
                    continue;
                }
                let mut n = 1usize;
                while i + n < CV
                    && n < 256
                    && c.mat[i + n] == mat
                    && (mat == 0 || c.occ[i + n] == occ)
                    && (mat == 0 || mat == WATER || occ == 255 || water(i + n) == 0)
                {
                    n += 1;
                }
                out.push(if n > 1 { flag | 0x80 } else { flag });
                if occ != 255 {
                    out.push(occ);
                }
                if n > 1 {
                    out.push((n - 1) as u8);
                }
                i += n;
            }
        }
        out
    }

    /// Terrain:Fill*: every voxel near `lo..hi` (world studs) takes the
    /// share of it inside the shape, from `inside` (signed distance in studs
    /// from the voxel's centre to the shape's surface, negative inside) as
    /// 0.5 − d / VOXEL. A material takes the voxel where it reaches it
    /// (occupancy the larger of the two); Air carves it (1 − share at most).
    pub fn fill(&mut self, lo: [f32; 3], hi: [f32; 3], mat: u8, inside: impl Fn([f32; 3]) -> f32) {
        let a = [0, 1, 2].map(|k| ((lo[k] / VOXEL).floor() as i32) - 1);
        let b = [0, 1, 2].map(|k| ((hi[k] / VOXEL).ceil() as i32) + 1);
        for y in a[1]..=b[1] {
            for z in a[2]..=b[2] {
                for x in a[0]..=b[0] {
                    let share = (0.5 - inside(voxel_center([x, y, z])) / VOXEL).clamp(0.0, 1.0);
                    if occupancy_to_byte(share).is_none() {
                        continue;
                    }
                    let v = [x, y, z];
                    let (old, occ) = self.get(x, y, z);
                    let (m, o) = if mat == 0 {
                        let left = occ.min(1.0 - share);
                        if old == 0 || left < 1.0 / 256.0 {
                            (0, 0.0)
                        } else {
                            (old, left)
                        }
                    } else {
                        (mat, if old == 0 { share } else { occ.max(share) })
                    };
                    self.write_one(v, m, o);
                }
            }
        }
        self.finish();
    }

    /// Terrain:ReplaceMaterial: `from` becomes `to` inside the voxel box.
    pub fn replace(&mut self, a: [i32; 3], b: [i32; 3], from: u8, to: u8) {
        for y in a[1]..b[1] {
            for z in a[2]..b[2] {
                for x in a[0]..b[0] {
                    let (m, o) = self.get(x, y, z);
                    if m == from && m != 0 {
                        self.write_one([x, y, z], to, if to == 0 { 0.0 } else { o });
                    }
                }
            }
        }
        self.finish();
    }

    /// Terrain:Clear.
    pub fn clear(&mut self) {
        self.chunks.clear();
        self.count = 0;
    }

    fn write_one(&mut self, v: [i32; 3], mat: u8, occupancy: f32) {
        let (mat, occ) = match occupancy_to_byte(occupancy) {
            Some(b) if mat != 0 => (mat, b),
            _ => (0, 0),
        };
        self.chunk_mut(chunk_key(v)).write(local_index(v), 1, mat, occ, 0);
    }
}

// ------------------------------------------------- raw cells (round 25)

impl Voxels {
    /// A voxel's stored bytes: material, occupancy byte (255 = full) and
    /// shoreline water (byte + 1, 0 = none); Air is (0, 0, 0). Region copies
    /// (terrain_region.rs) move these as they are, so copy and paste don't
    /// round occupancies through `occupancy_to_byte`.
    pub fn get_raw(&self, v: [i32; 3]) -> (u8, u8, u16) {
        match self.chunks.get(&chunk_key(v)) {
            None => (0, 0, 0),
            Some(c) => {
                let i = local_index(v);
                match c.mat[i] {
                    0 => (0, 0, 0),
                    m => (m, c.occ[i], c.water.as_ref().map_or(0, |w| w[i])),
                }
            }
        }
    }

    /// Writes stored bytes as `get_raw` reads them (material 0 is Air),
    /// with one recount at the end.
    pub fn set_raw_many(&mut self, cells: impl IntoIterator<Item = ([i32; 3], (u8, u8, u16))>) {
        for (v, (mat, occ, water)) in cells {
            let (occ, water) = if mat == 0 { (0, 0) } else { (occ, water) };
            let key = chunk_key(v);
            if mat == 0 && !self.chunks.contains_key(&key) {
                continue;
            }
            self.chunk_mut(key).write(local_index(v), 1, mat, occ, water);
        }
        self.finish();
    }
}

// ---------------------------------------------------------------- meshing

pub struct MaterialMesh {
    pub material: u8,
    pub positions: Vec<[f32; 3]>,
    pub normals: Vec<[f32; 3]>,
    /// Per vertex: its cell's random byte (Studio's detiling offset; the
    /// water shader blends three offset samples by it, as SmoothWater does).
    pub rand: Vec<u8>,
    pub indices: Vec<u32>,
}

/// Padded sample grid per chunk: local voxels -1..=33.
const P: usize = CS + 3;
const PP: usize = P * P;
/// Cells per axis: cell min-corners at local -1..=31.
const NC: usize = CS + 1;
const STRIDE: [usize; 3] = [1, P, PP];
const NO_VERT: u32 = u32::MAX;
const NO_MAT: u8 = u8::MAX;

const CUBE_EDGES: [(usize, usize); 12] = [
    (0, 1),
    (2, 3),
    (4, 5),
    (6, 7),
    (0, 2),
    (1, 3),
    (4, 6),
    (5, 7),
    (0, 4),
    (1, 5),
    (2, 6),
    (3, 7),
];

#[inline]
fn pidx(x: usize, y: usize, z: usize) -> usize {
    x + P * y + PP * z
}

#[inline]
fn cidx(c: [usize; 3]) -> usize {
    c[0] + NC * (c[1] + NC * c[2])
}

/// Sample fields for one chunk plus a one/two voxel apron.
struct Grid {
    solid: Vec<f32>,
    water: Vec<f32>,
    water_kind: Vec<u8>,
    mat: Vec<u8>,
    occ: [f32; 256],
}

#[derive(Default)]
struct Builder {
    positions: Vec<[f32; 3]>,
    normals: Vec<[f32; 3]>,
    rand: Vec<u8>,
    indices: Vec<u32>,
}

struct LocalVert {
    pos: [f32; 3],
    nrm: [f32; 3],
    mat: u8,
    idx: u32,
}

struct State {
    /// Per field (0 solid, 1 water): cell -> local vertex id.
    cell: [Vec<u32>; 2],
    local: Vec<LocalVert>,
    /// (local vertex, material) for vertices shared by several materials.
    overflow: HashMap<(u32, u8), u32>,
    /// Vertices of chunk-border cells, welded across chunks.
    seam: HashMap<(u8, u8, [i32; 3]), u32>,
    builders: Vec<Builder>,
}

fn copy_range(d: i32) -> (usize, usize, usize) {
    // (source start, padded destination start, length)
    match d {
        -1 => (CS - 1, 0, 1),
        0 => (0, 1, CS),
        _ => (0, CS + 1, 2),
    }
}

impl Grid {
    fn new() -> Self {
        let mut occ = [0.0; 256];
        for (b, o) in occ.iter_mut().enumerate() {
            *o = occupancy_from_byte(b as u8);
        }
        Grid {
            solid: vec![0.0; P * P * P],
            water: vec![0.0; P * P * P],
            water_kind: vec![0; P * P * P],
            mat: vec![0; P * P * P],
            occ,
        }
    }

    /// Fills the padded grid for chunk `key`; returns (any solid, all solid, any water).
    fn fill(&mut self, v: &Voxels, key: [i32; 3]) -> (bool, bool, bool) {
        self.solid.fill(0.0);
        self.water.fill(0.0);
        self.mat.fill(0);
        for dz in -1..=1 {
            for dy in -1..=1 {
                for dx in -1..=1 {
                    let Some(ch) = v.chunks.get(&[key[0] + dx, key[1] + dy, key[2] + dz]) else {
                        continue;
                    };
                    let (rx, ry, rz) = (copy_range(dx), copy_range(dy), copy_range(dz));
                    for z in 0..rz.2 {
                        for y in 0..ry.2 {
                            let s = lidx(rx.0, ry.0 + y, rz.0 + z);
                            let d = pidx(rx.1, ry.1 + y, rz.1 + z);
                            for x in 0..rx.2 {
                                let m = ch.mat[s + x];
                                if m == 0 {
                                    continue;
                                }
                                let o = self.occ[ch.occ[s + x] as usize];
                                self.mat[d + x] = m;
                                if m == WATER {
                                    // Water as solid (round 21): any water
                                    // voxel is inside, its occupancy only
                                    // moves the surface, so part-filled
                                    // voxels in a lake make no inner faces.
                                    self.water[d + x] = 0.5 + 0.5 * o;
                                } else {
                                    // Any occupied voxel is inside; its
                                    // occupancy moves the surface (an edge to
                                    // air crosses at o / (1 + o) of the way):
                                    // Studio draws voxels under 0.5 too
                                    // (terrain_probe.luau, round 21).
                                    self.solid[d + x] = 0.5 + 0.5 * o;
                                    if let Some(w) = &ch.water {
                                        if w[s + x] != 0 {
                                            self.water[d + x] = 0.5 + 0.5 * w[s + x] as f32 / 256.0;
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }
        // Native water preprocessing: air/liquid/solid are distinct
        // classes. One simultaneous horizontal ring continues liquid into banks;
        // liquid immediately above a bank continues down with full occupancy.
        // Sample an extra apron so these rules agree at storage chunk seams.
        self.water_kind.fill(0);
        if v.has_water_near(key) {
            const Q: usize = P + 4;
            let qi = |x: usize, y: usize, z: usize| x + Q * (y + Q * z);
            let mut raw = vec![(0u8, 0.0f32, false); Q * Q * Q];
            for z in 0..Q { for y in 0..Q { for x in 0..Q {
                let w = [x,y,z].map(|c| c as i32 - 3);
                let w = [0,1,2].map(|a| (key[a] << CHUNK_SHIFT) + w[a]);
                let (m,o) = v.get(w[0],w[1],w[2]);
                let liquid = v.water(w[0],w[1],w[2]);
                raw[qi(x,y,z)] = if liquid > 0.0 {
                    (1, liquid, m != WATER && o > 1.0/256.0)
                } else if m != 0 && m != WATER { (2,o,true) }
                else { (0,0.0,false) };
            }}}
            let mut continued = raw.clone();
            for z in 1..Q-1 { for y in 1..Q-1 { for x in 1..Q-1 {
                let i = qi(x,y,z);
                if raw[i].0 == 0 || (raw[i].0 == 1 && !raw[i].2) { continue; }
                if raw[qi(x,y+1,z)].0 == 1 {
                    continued[i] = (1,1.0,raw[i].2);
                } else {
                    let mut o = 0.0f32;
                    for dz in -1i32..=1 { for dx in -1i32..=1 {
                        if dx == 0 && dz == 0 { continue; }
                        let n = raw[qi((x as i32+dx) as usize,y,(z as i32+dz) as usize)];
                        if n.0 == 1 { o = o.max(n.1); }
                    }}
                    if o > 0.0 { continued[i] = (1,o,raw[i].2); }
                }
            }}}
            for z in 0..P { for y in 0..P { for x in 0..P {
                let (xx,yy,zz) = (x+2,y+2,z+2);
                let (mut kind,mut o,_) = continued[qi(xx,yy,zz)];
                // The native second pass opens exposed remaining solid samples
                // to air, retaining only fully occupied enclosed bank samples.
                if kind == 2 && (o < 1.0 ||
                    continued[qi(xx-1,yy,zz)].0 == 0 || continued[qi(xx+1,yy,zz)].0 == 0 ||
                    continued[qi(xx,yy-1,zz)].0 != 2 || continued[qi(xx,yy+1,zz)].0 == 0 ||
                    continued[qi(xx,yy,zz-1)].0 == 0 || continued[qi(xx,yy,zz+1)].0 == 0) {
                    kind = 0; o = 0.0;
                }
                let i = pidx(x,y,z);
                self.water_kind[i] = kind;
                self.water[i] = if kind != 0 { 0.5 + 0.5*o } else { 0.0 };
            }}}
        }
        let (mut any_s, mut all_s, mut any_w) = (false, true, false);
        for (s, w) in self.solid.iter().zip(&self.water) {
            let i = *s >= ISO;
            any_s |= i;
            all_s &= i;
            any_w |= *w >= ISO;
        }
        (any_s, all_s, any_w)
    }

    /// Surface-nets vertex for cell `c` (min corner at padded sample `c`):
    /// average of the iso crossings on its edges, normal from the density
    /// gradient. Returns world-space position and normal.
    fn cell_vertex(&self, field: usize, c: [usize; 3], key: [i32; 3]) -> ([f32; 3], [f32; 3]) {
        let f = if field == 0 { &self.solid } else { &self.water };
        let base = pidx(c[0], c[1], c[2]);
        let mut d = [0f32; 8];
        for (i, di) in d.iter_mut().enumerate() {
            let sample = base + (i & 1) + ((i >> 1) & 1) * P + ((i >> 2) & 1) * PP;
            *di = f[sample];
        }
        let kinds = std::array::from_fn::<_, 8, _>(|i| {
            if field == 0 { (d[i] >= ISO) as u8 } else {
                self.water_kind[base + (i & 1) + ((i >> 1) & 1)*P + ((i >> 2) & 1)*PP]
            }
        });
        let mut sum = [0f32; 3];
        let mut n = 0u32;
        for &(a, b) in &CUBE_EDGES {
            let (da, db) = (d[a], d[b]);
            let crossing = if field == 0 { kinds[a] != kinds[b] }
                else { matches!((kinds[a], kinds[b]), (0,1) | (1,0)) };
            if crossing {
                // Solid: the surface crosses an edge from an occupied voxel
                // to air at its occupancy's share of the way (solid samples
                // hold 0.5 + 0.5 × occupancy). Studio's lone voxels top out
                // at 3.33 / 2.67 / 2.33 / 2.14 for occupancy 1 / .5 / .25 /
                // .1 and a full slab's top lies half a voxel above it
                // (terrain_probe.luau, round 21): exactly this.
                // Water samples are kept the same way.
                let t = if kinds[a] > kinds[b] {
                    ((da - 0.5) * 2.0).clamp(0.0, 1.0)
                } else {
                    1.0 - ((db - 0.5) * 2.0).clamp(0.0, 1.0)
                };
                for (k, s) in sum.iter_mut().enumerate() {
                    let ca = ((a >> k) & 1) as f32;
                    let cb = ((b >> k) & 1) as f32;
                    *s += ca + (cb - ca) * t;
                }
                n += 1;
            }
        }
        let mut fp = if n > 0 {
            sum.map(|s| s / n as f32)
        } else {
            [0.5; 3]
        };
        let nrm = gradient_normal(&d, fp)
            .or_else(|| gradient_normal(&d, [0.5; 3]))
            .unwrap_or([0.0, 1.0, 0.0]);
        // The material's shape (Studio's deform Studio, one mode per
        // material): cubify pulls the vertex to the cell centre, quantize
        // snaps it (half away from zero), shift moves it by ±shift with
        // bytes 0 / 1 / 2 of the cell's hash (whose low byte is also the
        // texture random); then clamped to the cell.
        let gc = [0, 1, 2].map(|k| (key[k] << CHUNK_SHIFT) + c[k] as i32 - 1);
        if field == 0 {
            let shape = shape_of(self.cell_material(c));
            if shape.cubify {
                fp = [0.5; 3];
            } else if shape.quantize > 0.0 {
                fp = fp.map(|f| ((f - 0.5) / shape.quantize).round() * shape.quantize + 0.5);
            } else if shape.shift > 0.0 {
                let h = studio_cell_hash(gc);
                for (k, f) in fp.iter_mut().enumerate() {
                    let b = ((h >> (8 * k)) & 0xff) as f32;
                    *f += (2.0 * b / 255.0 - 1.0) * shape.shift;
                }
            }
            fp = fp.map(|f| f.clamp(0.0, 1.0));
        }
        let mut pos = [0f32; 3];
        for k in 0..3 {
            let voxel = ((key[k] as i64) << CHUNK_SHIFT) + c[k] as i64 - 1;
            pos[k] = ((voxel as f64 + fp[k] as f64 + 0.5) * VOXEL as f64) as f32;
        }
        (pos, nrm)
    }
}

/// How a material bends the surface (Studio's terrain table: shift,
/// quantize in voxels; cubify; hard / hardsoft = faceted shading).
#[derive(Clone, Copy, Debug, Default)]
pub struct Shape {
    pub shift: f32,
    pub quantize: f32,
    pub cubify: bool,
    pub faceted: bool,
    /// Studio's "hardsoft": faceted, but a corner picks its projection axis
    /// from its vertex normal while that is within 0.75 of the face's.
    pub hardsoft: bool,
}

static SHAPE_2022: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// Which table the mesher's material shapes come from
/// (MaterialService.Use2022Materials of the place being meshed).
pub fn set_use2022(on: bool) {
    SHAPE_2022.store(on, std::sync::atomic::Ordering::Relaxed);
}

/// The table set_use2022 last picked.
pub fn use2022() -> bool {
    SHAPE_2022.load(std::sync::atomic::Ordering::Relaxed)
}

fn shape_of(m: u8) -> Shape {
    let use2022 = SHAPE_2022.load(std::sync::atomic::Ordering::Relaxed);
    crate::terrain_mat::material(m, use2022).map(|t| t.shape).unwrap_or_default()
}

/// Outward normal (minus the trilinear density gradient) at `f` in the cell.
fn gradient_normal(d: &[f32; 8], f: [f32; 3]) -> Option<[f32; 3]> {
    let [fx, fy, fz] = f;
    let (gx_, gy_, gz_) = (1.0 - fx, 1.0 - fy, 1.0 - fz);
    let gx = gy_ * gz_ * (d[1] - d[0])
        + fy * gz_ * (d[3] - d[2])
        + gy_ * fz * (d[5] - d[4])
        + fy * fz * (d[7] - d[6]);
    let gy = gx_ * gz_ * (d[2] - d[0])
        + fx * gz_ * (d[3] - d[1])
        + gx_ * fz * (d[6] - d[4])
        + fx * fz * (d[7] - d[5]);
    let gz = gx_ * gy_ * (d[4] - d[0])
        + fx * gy_ * (d[5] - d[1])
        + gx_ * fy * (d[6] - d[2])
        + fx * fy * (d[7] - d[3]);
    let len = (gx * gx + gy * gy + gz * gz).sqrt();
    (len > 1e-6).then(|| [-gx / len, -gy / len, -gz / len])
}

impl State {
    fn new() -> Self {
        State {
            cell: [vec![NO_VERT; NC * NC * NC], vec![NO_VERT; NC * NC * NC]],
            local: Vec::new(),
            overflow: HashMap::new(),
            seam: HashMap::new(),
            builders: (0..64).map(|_| Builder::default()).collect(),
        }
    }

    fn reset_chunk(&mut self) {
        self.cell[0].fill(NO_VERT);
        self.cell[1].fill(NO_VERT);
        self.local.clear();
        self.overflow.clear();
    }

    /// Index of cell `c`'s vertex in material `mat`'s mesh, creating it on demand.
    fn index(&mut self, g: &Grid, key: [i32; 3], field: usize, c: [usize; 3], mat: u8) -> u32 {
        let ci = cidx(c);
        let mut lv = self.cell[field][ci];
        if lv == NO_VERT {
            let (pos, nrm) = g.cell_vertex(field, c, key);
            lv = self.local.len() as u32;
            self.local.push(LocalVert {
                pos,
                nrm,
                mat: NO_MAT,
                idx: 0,
            });
            self.cell[field][ci] = lv;
        }
        let l = &self.local[lv as usize];
        if l.mat == mat {
            return l.idx;
        }
        let (pos, nrm, first_free) = (l.pos, l.nrm, l.mat == NO_MAT);
        let rand = cell_rand([0, 1, 2].map(|k| (key[k] << CHUNK_SHIFT) + c[k] as i32 - 1));
        let builders = &mut self.builders;
        let mut push = || {
            let b = &mut builders[mat as usize];
            b.positions.push(pos);
            b.normals.push(nrm);
            b.rand.push(rand);
            (b.positions.len() - 1) as u32
        };
        let idx = if c.iter().any(|&k| k == 0 || k == NC - 1) {
            let gc = [0, 1, 2].map(|k| (key[k] << CHUNK_SHIFT) + c[k] as i32 - 1);
            *self.seam.entry((field as u8, mat, gc)).or_insert_with(push)
        } else if first_free {
            push()
        } else {
            *self.overflow.entry((lv, mat)).or_insert_with(push)
        };
        if first_free {
            let l = &mut self.local[lv as usize];
            l.mat = mat;
            l.idx = idx;
        }
        idx
    }

    /// Emits the quads for every sign-changing edge whose low sample lies in
    /// this chunk (padded 1..=32 on each axis).
    fn edges(&mut self, g: &Grid, key: [i32; 3], field: usize) {
        let f = if field == 0 { &g.solid } else { &g.water };
        for z in 1..=CS {
            for y in 1..=CS {
                for x in 1..=CS {
                    let p = pidx(x, y, z);
                    let density = |i: usize| f[i];
                    let a_in = density(p) >= ISO;
                    for axis in 0..3 {
                        let q = p + STRIDE[axis];
                        if field == 1 && !matches!((g.water_kind[p], g.water_kind[q]), (0,1) | (1,0)) {
                            continue;
                        }
                        if (density(q) >= ISO) == a_in {
                            continue;
                        }
                        let inside = if a_in { p } else { q };
                        let mat = if field == 1 {
                            // Bank overlap is already encoded in the water field.
                            WATER
                        } else {
                            g.mat[inside]
                        };
                        let (u, v) = ((axis + 1) % 3, (axis + 2) % 3);
                        let c00 = [x, y, z];
                        let mut c10 = c00;
                        c10[u] -= 1;
                        let mut c01 = c00;
                        c01[v] -= 1;
                        let mut c11 = c10;
                        c11[v] -= 1;
                        let ids = [
                            self.index(g, key, field, c00, mat),
                            self.index(g, key, field, c10, mat),
                            self.index(g, key, field, c11, mat),
                            self.index(g, key, field, c01, mat),
                        ];
                        // c00 -> c10 -> c11 -> c01 winds counter-clockwise about +axis.
                        let q4 = if a_in {
                            ids
                        } else {
                            [ids[0], ids[3], ids[2], ids[1]]
                        };
                        // Studio's diagonal (as chunk_triangles): q1–q3 when
                        // q1 and q3 share a cell material q0 or q2 also has.
                        let flip = field == 1 || {
                            let cm = [c00, c10, c11, c01].map(|c| g.cell_material(c));
                            let m = if a_in { cm } else { [cm[0], cm[3], cm[2], cm[1]] };
                            m[1] == m[3] && (m[1] == m[2] || m[1] == m[0])
                        };
                        let b = &mut self.builders[mat as usize];
                        if !flip {
                            b.indices
                                .extend_from_slice(&[q4[0], q4[1], q4[2], q4[0], q4[2], q4[3]]);
                        } else {
                            b.indices
                                .extend_from_slice(&[q4[0], q4[1], q4[3], q4[1], q4[2], q4[3]]);
                        }
                    }
                }
            }
        }
    }
}


/// Smooth surface (surface nets over occupancy, iso 0.5, samples at voxel
/// centres) in world studs, one mesh per material. Water is its own mesh
/// (material 1), overlapping the bank for depth clipping. Vertices are
/// welded per material, including across chunk borders. Triangles wind
/// counter-clockwise seen from outside.
pub fn build_meshes(v: &Voxels) -> Vec<MaterialMesh> {
    build_fields(v, [true, true])
}

/// The water surface alone (the renderer draws solid terrain by chunk).
pub fn build_water_meshes(v: &Voxels) -> Vec<MaterialMesh> {
    let mut meshes = build_fields(v, [false, true]);
    // Native water vertex producer Studio adds -0.5 voxel after placement.
    // Solid terrain remains at its own net surface elevation.
    for mesh in &mut meshes {
        for p in &mut mesh.positions { p[1] -= WATER_RAY_DROP; }
    }
    meshes
}

fn build_fields(v: &Voxels, fields: [bool; 2]) -> Vec<MaterialMesh> {
    if v.chunks.is_empty() {
        return Vec::new();
    }
    // A chunk owns edges whose low end is inside it, so chunks just below a
    // stored chunk on each axis can own surface too.
    let mut keys: Vec<[i32; 3]> = Vec::with_capacity(v.chunks.len() * 4);
    for k in v.chunks.keys() {
        keys.push(*k);
        for a in 0..3 {
            let mut n = *k;
            n[a] -= 1;
            keys.push(n);
        }
    }
    keys.sort_unstable();
    keys.dedup();

    let mut g = Grid::new();
    let mut st = State::new();
    for key in keys {
        let (any_s, all_s, any_w) = g.fill(v, key);
        if !any_w && (!any_s || all_s) {
            continue;
        }
        st.reset_chunk();
        if fields[0] && any_s && !all_s {
            st.edges(&g, key, 0);
        }
        if fields[1] && any_w {
            st.edges(&g, key, 1);
        }
    }
    st.builders
        .into_iter()
        .enumerate()
        .filter(|(_, b)| !b.indices.is_empty())
        .map(|(m, b)| MaterialMesh {
            material: m as u8,
            positions: b.positions,
            normals: b.normals,
            rand: b.rand,
            indices: b.indices,
        })
        .collect()
}

/// Studio's water surface as rays and shapecasts meet it: the water mesh
/// moved down by half a voxel (a 16-tall water FillBlock with its top at 0
/// is hit at y = 0 from above and at y = -20 from below, where the solid
/// mesh of the same block lies at +2 / -18; water_probe.luau 2026-11-11).
pub const WATER_RAY_DROP: f32 = VOXEL * 0.5;

/// One chunk's water surface for rays (world studs, counter-clockwise seen
/// from outside, `WATER_RAY_DROP` applied): the edges this chunk owns, as
/// `build_water_meshes` makes them.
pub fn chunk_water_triangles(v: &Voxels, key: [i32; 3]) -> Vec<[[f32; 3]; 3]> {
    let mut g = Grid::new();
    let (_, _, any_w) = g.fill(v, key);
    if !any_w {
        return Vec::new();
    }
    let mut st = State::new();
    st.reset_chunk();
    st.edges(&g, key, 1);
    let b = &st.builders[WATER as usize];
    b.indices
        .chunks_exact(3)
        .map(|t| {
            [t[0], t[1], t[2]].map(|i| {
                let p = b.positions[i as usize];
                [p[0], p[1] - WATER_RAY_DROP, p[2]]
            })
        })
        .collect()
}

// ------------------------------------------------- render chunks (round 21)

/// A terrain vertex as Studio's terrain shader takes it (SmoothCluster VS):
/// every triangle carries its three corners' materials, projection axes and
/// random detiling, and each vertex says which corner it is (the GPU turns
/// that into barycentric weights). Vertices aren't shared.
#[repr(C)]
#[derive(Clone, Copy, Debug, bytemuck::Pod, bytemuck::Zeroable)]
pub struct TerrainVertex {
    pub pos: [f32; 3],
    /// Normal × 127; w = which corner (0, 1, 2).
    pub normal: [i8; 4],
    /// The three corners' palette entries (material × 3 + face).
    pub palette: [u8; 4],
    /// The three corners' projection axes (0..17).
    pub axis: [u8; 4],
    /// The three corners' random bytes (detiling rotation / offset).
    pub rand: [u8; 4],
    /// Native decoded POSITION.xz before chunk translation.
    pub wind_local: [f32; 4],
}

/// Palette entry of a material's face (Top 0, Side 1, Bottom 2).
pub fn palette(material: u8, face: u8) -> u8 {
    material * 3 + face
}

/// Studio's 18 projection axes (its CB2 axisU / axisV tables, 0.7 for the
/// diagonals, exactly as stored): 0 +X, 1 −X, 2 +Z, 3 −Z, 4–7 the XZ
/// diagonals, 8 up, 9–12 tilted up (+X, −X, +Z, −Z), 13 down, 14–17 tilted
/// down. Texture coordinate = (P·U, P·V) × tiling (Studio 0.744:
/// tables at .rdata Studio, filled by Studio).
pub const AXIS_U: [[f32; 3]; 18] = [
    [0.0, 0.0, -1.0], [0.0, 0.0, 1.0], [1.0, 0.0, 0.0], [-1.0, 0.0, 0.0],
    [0.7, 0.0, -0.7], [-0.7, 0.0, -0.7], [0.7, 0.0, 0.7], [-0.7, 0.0, 0.7],
    [-1.0, 0.0, 0.0], [-0.7, 0.7, 0.0], [-0.7, -0.7, 0.0], [-1.0, 0.0, 0.0], [-1.0, 0.0, 0.0],
    [-1.0, 0.0, 0.0], [-0.7, -0.7, 0.0], [-0.7, 0.7, 0.0], [-1.0, 0.0, 0.0], [-1.0, 0.0, 0.0],
];
pub const AXIS_V: [[f32; 3]; 18] = [
    [0.0, -1.0, 0.0], [0.0, -1.0, 0.0], [0.0, -1.0, 0.0], [0.0, -1.0, 0.0],
    [0.0, -1.0, 0.0], [0.0, -1.0, 0.0], [0.0, -1.0, 0.0], [0.0, -1.0, 0.0],
    [0.0, 0.0, -1.0], [0.0, 0.0, -1.0], [0.0, 0.0, -1.0], [0.0, 0.7, -0.7], [0.0, -0.7, -0.7],
    [0.0, 0.0, 1.0], [0.0, 0.0, 1.0], [0.0, 0.0, 1.0], [0.0, 0.7, 0.7], [0.0, -0.7, 0.7],
];

/// Studio's projection axis for a corner normal (Studio): bands on the
/// normal's Y (up above 0.9, tilted up above 0.4, side down to −0.6, tilted
/// down to −0.8, else down); on the side ±X / ±Z unless the other component
/// is at least half, then a diagonal. Cube-mapped materials (Brick,
/// WoodPlanks) take the dominant axis only.
pub fn studio_axis(n: [f32; 3], cube: bool) -> u8 {
    let (ax, az, y) = (n[0].abs(), n[2].abs(), n[1]);
    let neg = |v: f32| (v < 0.0) as u8;
    if cube {
        if ax < y && az < y {
            8
        } else if y >= -ax || y >= -az {
            if ax <= az { 2 + neg(n[2]) } else { neg(n[0]) }
        } else {
            13
        }
    } else if y > 0.9 {
        8
    } else if y > 0.4 {
        if ax <= az { 11 + neg(n[2]) } else { 9 + neg(n[0]) }
    } else if y >= -0.8 {
        if y < -0.6 {
            if ax <= az { 16 + neg(n[2]) } else { 14 + neg(n[0]) }
        } else if ax <= 2.0 * az {
            if az <= 2.0 * ax { 4 + 2 * neg(n[0]) + neg(n[2]) } else { 2 + neg(n[2]) }
        } else {
            neg(n[0])
        }
    } else {
        13
    }
}

/// Which face texture a corner on `axis` shows: side for 0–7, top for
/// 8–12, bottom for 13–17 (no separate threshold in Studio).
pub fn face_of_axis(axis: u8) -> u8 {
    match axis {
        0..=7 => 1,
        8..=12 => 0,
        _ => 2,
    }
}

/// Studio's per-cell random (Studio): boost hash_combine of the cell's
/// global voxel index (its lower-corner sample), then Thomas Wang's 32-bit
/// hash. A corner's byte is the low byte of its own cell's hash.
pub fn studio_cell_hash(gc: [i32; 3]) -> u32 {
    let (x, y) = (gc[0] as i64 as u64, gc[1] as i64 as u64);
    let s1 = 0x9e37_79b9u64.wrapping_add(x);
    let s2 = s1 ^ y.wrapping_add(0x9e37_79b9).wrapping_add(s1 << 6).wrapping_add(s1 >> 2);
    let mut h = (gc[2] as u32)
        .wrapping_add(0x9e37_79b9)
        .wrapping_add((s2 as u32) << 6)
        .wrapping_add((s2 >> 2) as u32)
        ^ (s2 as u32);
    h = h.wrapping_add(!(h << 15));
    h ^= h >> 10;
    h = h.wrapping_mul(9);
    h ^= h >> 6;
    h = h.wrapping_add(!(h << 11));
    h ^ (h >> 16)
}

fn cell_rand(gc: [i32; 3]) -> u8 {
    studio_cell_hash(gc) as u8
}

impl Grid {
    /// The material a cell's vertex shows: the one with the most occupancy
    /// among the cell's eight voxels, the lower SmoothGrid slot on a tie
    /// (round 21, against an offline Studio: its 4-material checker keeps
    /// Grass and Sand and shows Rock and Snow as small patches, and a lone
    /// Snow voxel on a Basalt slab stays a bump on Basalt).
    /// Decoration writer Studio: compact occupied solid slots, then
    /// reduce adjacent pairs. A global per-material sum changes ownership.
    fn decoration_material(&self, c: [usize; 3]) -> u8 {
        let base = pidx(c[0], c[1], c[2]);
        let offsets = [0, 1, 1 + P, P, PP, PP + 1, PP + 1 + P, PP + P];
        let raw = offsets.map(|o| {
            let p = base + o;
            (self.mat[p], ((self.solid[p] * 2.0 - 1.0) * 256.0 - 1.0).round().clamp(0.0, 255.0) as u8)
        });
        decoration_owner(raw)
    }

    fn cell_material(&self, c: [usize; 3]) -> u8 {

        let base = pidx(c[0], c[1], c[2]);
        let mut sums: [(u8, f32); 8] = [(0, 0.0); 8];
        let mut n = 0;
        for i in 0..8 {
            let p = base + (i & 1) + ((i >> 1) & 1) * P + ((i >> 2) & 1) * PP;
            let m = self.mat[p];
            if m == 0 || m == WATER {
                continue;
            }
            let o = self.solid[p];
            match sums[..n].iter_mut().find(|s| s.0 == m) {
                Some(s) => s.1 += o,
                None => {
                    sums[n] = (m, o);
                    n += 1;
                }
            }
        }
        let mut best = (0u8, -1.0f32);
        for &(m, o) in &sums[..n] {
            if o > best.1 + 1e-6 || ((o - best.1).abs() <= 1e-6 && m < best.0) {
                best = (m, o);
            }
        }
        best.0
    }
}

/// Native packed occupancy stores weight minus one for material > 1.
fn decoration_owner(raw: [(u8, u8); 8]) -> u8 {
    let mut slots = [(0u32, 0u8); 8];
    let mut n = 0;
    for (material, occupancy) in raw {
        if material > 1 {
            slots[n] = (occupancy as u32 + 1, material);
            n += 1;
        }
    }
    let mut width = 8;
    while width > 1 {
        for i in 0..width / 2 {
            let (a, b) = (slots[2 * i], slots[2 * i + 1]);
            slots[i] = if a.1 == b.1 { (a.0 + b.0, a.1) }
                else if a.0 > b.0 || (a.0 == b.0 && a.1 < b.1) { a }
                else { b };
        }
        width /= 2;
    }
    slots[0].1
}

/// The solid surface owned by chunk `key` as terrain-shader triangles (the
/// same cell vertices and quads as `build_meshes`, so the drawn shape is the
/// collision shape). `cube(material)` says which materials map as a cube.
pub fn chunk_triangles(v: &Voxels, key: [i32; 3], cube: &dyn Fn(u8) -> bool) -> Vec<TerrainVertex> {
    chunk_triangles_impl(v, key, cube, false)
}

fn chunk_triangles_impl(v: &Voxels, key: [i32; 3], cube: &dyn Fn(u8) -> bool, grass_domains: bool) -> Vec<TerrainVertex> {
    let mut g = Grid::new();
    let (any_s, all_s, _) = g.fill(v, key);
    if !any_s || all_s {
        return Vec::new();
    }
    struct Corner {
        pos: [f32; 3],
        nrm: [f32; 3],
        mat: u8,
        gc: [i32; 3],
        c: [usize; 3],
    }
    let mut cells: HashMap<usize, Corner> = HashMap::new();
    let mut quads: Vec<[usize; 4]> = Vec::new();
    {
        let mut corner = |c: [usize; 3]| -> usize {
            let ci = cidx(c);
            cells.entry(ci).or_insert_with(|| {
                let (pos, nrm) = g.cell_vertex(0, c, key);
                let gc = [0, 1, 2].map(|k| (key[k] << CHUNK_SHIFT) + c[k] as i32 - 1);
                Corner { pos, nrm, mat: if grass_domains { g.decoration_material(c) } else { g.cell_material(c) }, gc, c }
            });
            ci
        };
        for outer in 1..=CS {
            for middle in 1..=CS {
                for x in 1..=CS {
                    let (y, z) = if grass_domains { (outer, middle) } else { (middle, outer) };
                    let p = pidx(x, y, z);
                    let a_in = g.solid[p] >= ISO;
                    for axis in 0..3 {
                        let q = p + STRIDE[axis];
                        if (g.solid[q] >= ISO) == a_in {
                            continue;
                        }
                        let (u, w) = ((axis + 1) % 3, (axis + 2) % 3);
                        let c00 = [x, y, z];
                        let mut c10 = c00;
                        c10[u] -= 1;
                        let mut c01 = c00;
                        c01[w] -= 1;
                        let mut c11 = c10;
                        c11[w] -= 1;
                        let ids = [corner(c00), corner(c10), corner(c11), corner(c01)];
                        quads.push(if a_in { ids } else { [ids[0], ids[3], ids[2], ids[1]] });
                    }
                }
            }
        }
    }
    // Studio's quad emitter (Studio) picks the diagonal by material,
    // never by length: q1–q3 when q1 and q3 share a material that q0 or q2
    // also has (so every flat one-material quad), else q0–q2. Studio's Y
    // quads list q1 / q3 the other way round; the rule is symmetric in them.
    let tris: Vec<[usize; 3]> = quads.iter().flat_map(|q4| {
        let m = |i: usize| cells[&q4[i]].mat;
        if m(1) == m(3) && (m(1) == m(2) || m(1) == m(0)) {
            [[q4[0], q4[1], q4[3]], [q4[3], q4[1], q4[2]]]
        } else {
            // Decoration preserves the native alternate diagonal's cyclic
            // starts. Rotating either triangle changes its material sampler
            // and placement seed, even with identical triangle geometry.
            if grass_domains { [[q4[3], q4[0], q4[2]], [q4[2], q4[0], q4[1]]] }
            else { [[q4[0], q4[1], q4[2]], [q4[0], q4[2], q4[3]]] }
        }
    }).collect();
    // The second q1–q3 triangle starts at q3: native index table
    // Studio row3 = [0,1,3,3,1,2], confirmed by both voxel fixtures.
    // Vertex normals: the area-weighted sum of the touching
    // triangles' normals, from the final (deformed) positions. Cells on the
    // chunk's rim also touch the neighbour's triangles; they keep the
    // field's gradient normal, which both chunks agree on.
    let mut sums: HashMap<usize, glam::Vec3> = HashMap::new();
    for t in &tris {
        let p = t.map(|i| glam::Vec3::from(cells[&i].pos));
        let n = (p[1] - p[0]).cross(p[2] - p[0]);
        for i in t {
            *sums.entry(*i).or_default() += n;
        }
    }
    for (i, n) in sums {
        let k = cells.get_mut(&i).expect("cell");
        let inner = k.c.iter().all(|&v| v >= 1 && v < CS);
        if inner && n.length_squared() > 1e-12 {
            k.nrm = n.normalize().to_array();
        }
    }
    let mut out = Vec::with_capacity(quads.len() * 6);
    let mut tri = |t: [usize; 3], quad_hi: [f32; 3]| {
        let c = t.map(|i| &cells[&i]);
        // Native decoration batches use the owning voxel region, including
        // apron vertices that cross its boundary. A displaced first vertex
        // cannot select that region (paired slope fixture crosses Y=0).
        let domain_origin: [f32; 3] = std::array::from_fn(|axis| {
            let owner = c.iter().map(|k| k.gc[axis]).max().unwrap();
            ((owner >> 4) * 64 + 32) as f32
        });
        // Cells with no solid sample of their own take a neighbour's material.
        let material = |k: &Corner| k.mat;
        let fallback = c.iter().map(|k| material(k)).find(|&m| m != 0).unwrap_or(2);
        let mats = c.map(|k| if material(k) == 0 { fallback } else { material(k) });
        // Hard materials (Studio's "hard" / "hardsoft": Rock, Basalt, Brick…)
        // shade each triangle flat; the rest smooth.
        let (a, b, d) = (glam::Vec3::from(c[0].pos), glam::Vec3::from(c[1].pos), glam::Vec3::from(c[2].pos));
        let face = (b - a).cross(d - a).normalize_or_zero();
        // The normal a corner picks its axis (and so its face texture) from
        // (Studio): soft the vertex normal, hard the face's, hardsoft the
        // vertex normal while within 0.75 of the face's.
        let axis_normal = |i: usize| {
            let sh = shape_of(mats[i]);
            let vn = glam::Vec3::from(c[i].nrm);
            if face == glam::Vec3::ZERO || !sh.faceted || (sh.hardsoft && face.dot(vn) > 0.75) {
                c[i].nrm
            } else {
                face.to_array()
            }
        };
        let ax = [0, 1, 2].map(|i| studio_axis(axis_normal(i), cube(mats[i])));
        let pal = [0, 1, 2].map(|i| palette(mats[i], face_of_axis(ax[i])));
        let rnd = c.map(|k| cell_rand(k.gc));
        for (i, k) in c.iter().enumerate() {
            let q = |f: f32| (f.clamp(-1.0, 1.0) * 127.0).round() as i8;
            let n = if shape_of(mats[i]).faceted && face != glam::Vec3::ZERO {
                face.to_array()
            } else {
                k.nrm
            };
            out.push(TerrainVertex {
                pos: k.pos,
                normal: [q(n[0]), q(n[1]), q(n[2]), i as i8],
                palette: [pal[0], pal[1], pal[2], 0],
                axis: [ax[0], ax[1], ax[2], 0],
                rand: [rnd[0], rnd[1], rnd[2], 0],
                wind_local: if grass_domains { [domain_origin[0], domain_origin[1], domain_origin[2], 0.0] } else { [0.0; 4] },
            });
        }
        if grass_domains {
            // Native 16-voxel decoration regions retain the last owning
            // voxel's triangles in the adjacent region's apron as well.
            // The slope readback contains 32 such domains with distinct seeds.
            // Touching is not crossing: native X/Z fixtures exclude quads
            // whose maximum lies exactly on the neighboring region boundary.
            let owner: [i32; 3] = std::array::from_fn(|axis| c.iter().map(|k| k.gc[axis]).max().unwrap());
            let original: [TerrainVertex; 3] = out[out.len()-3..].try_into().unwrap();
            for mask in 1..8 {
                if (0..3).any(|axis| mask & (1 << axis) != 0 && (owner[axis].rem_euclid(16) != 15 || quad_hi[axis] <= domain_origin[axis] + 32.0)) { continue; }
                for mut vertex in original {
                    for axis in 0..3 { if mask & (1 << axis) != 0 { vertex.wind_local[axis] += 64.0; } }
                    out.push(vertex);
                }
            }
        }
    };
    for pair in tris.chunks_exact(2) {
        let quad_hi = std::array::from_fn(|axis| pair.iter().flatten().map(|i| cells[i].pos[axis]).fold(f32::NEG_INFINITY, f32::max));
        for t in pair { tri(*t, quad_hi); }
    }
    out
}

/// Terrain.Decoration grass for chunk `key` (round 21, after Studio's Grass
/// VS): blades standing on the chunk's Grass top faces, `length` = the
/// Terrain's GrassLength. Each blade is two triangles (both sides) of
/// terrain vertices flagged in `palette[3]` (1 = base, 2 = tip); a tip's
/// `rand[3]` holds the native displacement × 32 (root .1, tip axis+.1),
/// so the shared color/depth vertex shader sinks both with distance.
pub fn grass_blades(v: &Voxels, key: [i32; 3], length: f32) -> Vec<TerrainVertex> {
    grass_blades_density(v, key, length, 1.0)
}

/// Real blades retain their native allocated slot even when filler copies are omitted.
#[derive(Default)]
pub struct GrassGeometry {
    pub vertices: Vec<TerrainVertex>,
    pub batches: [GrassBatch; 2],
    pub cells: Vec<GrassCell>,
}
#[derive(Default)]
pub struct GrassBatch {
    pub allocated: usize,
    /// (native generated ordinal, first source vertex); each blade has six vertices.
    pub blades: Vec<(usize, usize)>,
}

pub struct GrassCell {
    pub lo: glam::Vec3,
    pub hi: glam::Vec3,
    pub near: bool,
    pub batch: GrassBatch,
}

pub fn grass_blades_density(v: &Voxels, key: [i32; 3], length: f32, density: f32) -> Vec<TerrainVertex> {
    grass_geometry_density(v, key, length, density).vertices
}

pub fn grass_geometry_density(v: &Voxels, key: [i32; 3], length: f32, density: f32) -> GrassGeometry {
    // Optional retained RenderDoc diagnostic: captured post-VS positions,
    // with zero encoded displacement, bypass generation only when requested.
    if let Ok(path) = std::env::var("RUSTUDIO_CAPTURED_GRASS") {
        if key != [0, 0, 0] { return GrassGeometry::default(); }
        return GrassGeometry { vertices: std::fs::read(path).expect("captured grass diagnostic")
            .chunks_exact(28)
            .map(|bytes| {
                // Retained captures use the original 28-byte terrain layout.
                let mut vertex: TerrainVertex = bytemuck::Zeroable::zeroed();
                bytemuck::bytes_of_mut(&mut vertex)[..28].copy_from_slice(bytes);
                vertex
            }).collect(), ..Default::default() };
    }

    let export = std::env::var_os("RUSTUDIO_COVERAGE_EXPORT");
    let mut provenance = std::collections::HashMap::new();
    let mut export_domains = Vec::new();
    let tris = chunk_triangles_impl(v, key, &|_| false, true);
    let grass_top = palette(2, 0);
    let mut out = Vec::new();
    let mut batches: [GrassBatch; 2] = Default::default();
    let mut domains = 0;
    let mut domain_cells = Vec::new();
    let mut blade_domains: [Vec<usize>; 2] = Default::default();
    let mut fillers: [Vec<(usize, usize, usize)>; 2] = Default::default();

    let scale = (crate::mech::GRASS_LENGTH_SCALE_BASE + length * crate::mech::GRASS_LENGTH_SCALE_SLOPE).max(0.0);
    for (ti, t) in tris.chunks_exact(3).enumerate() {
        if t[0].palette[..3].iter().all(|&p| p / 3 != grass_top / 3) {
            continue;
        }
        let (a, b, c) = (glam::Vec3::from(t[0].pos), glam::Vec3::from(t[1].pos), glam::Vec3::from(t[2].pos));
        let n = (b - a).cross(c - a);
        let area = n.length() * 0.5;
        // Decoration producer Studio compares normalized up dot against
        // f32(0.7) at Studio; terrain texture top threshold is separate.
        if area < 1e-4 || n.y / (2.0 * area) < 0.7 {
            continue;
        }
        let first_origin = glam::Vec3::new(t[0].wind_local[0], t[0].wind_local[1], t[0].wind_local[2]);
        let local_first = (a - first_origin).to_array();
        // Retain the existing shade stream independently of material sampling.
        let mut seed = (ti as u32).wrapping_mul(0x9e37_79b9) ^ (key[0] as u32).wrapping_mul(0x85eb_ca6b)
                ^ (key[1] as u32).wrapping_mul(0xc2b2_ae35) ^ (key[2] as u32).wrapping_mul(0x27d4_eb2f);
        if seed == 0 { seed = 0x6d2b_79f5; }
        let mut rnd = || { seed ^= seed << 13; seed ^= seed >> 17; seed ^= seed << 5; (seed & 0x00ff_ffff) as f32 / 16_777_216.0 };
        let _ = (rnd(), rnd());
        let indicators = std::array::from_fn(|i| {
            if t[0].palette[i] / 3 == grass_top / 3 { 1.0 } else { 0.0 }
        });
        let weight = crate::grass_stream::material_weight(
            [a.to_array(), b.to_array(), c.to_array()], indicators);
        // Native retains three zero-size records at zero weight. Omit those
        // degenerate triangles instead of normalizing a zero blade plane.
        domain_cells.push(first_origin.to_array().map(|x| (x / 64.0).floor() as i32));
        let domain = domains;
        if export.is_some() { export_domains.push(serde_json::json!({"domain":domain,"triangle":ti,"vertices":[a.to_array(),b.to_array(),c.to_array()],"region":domain_cells[domain],"weight":weight,"area":area})); }
        domains += 1;
        // Terrain queues both batches; the near batch
        // has its own 16-point domain and .85 geometry scale.
        for (samples, size, near) in [
            (crate::mech::GRASS_SAMPLE_POINTS.as_slice(), 1.0, false),
            (crate::mech::GRASS_NEAR_SAMPLE_POINTS.as_slice(), 0.85, true),
        ] {
            let mut stream = crate::grass_stream::Stream::new(local_first);
            let allocated = crate::grass_stream::sample_count(length, density, near);
            let slots = stream.shuffled_slots(allocated);
            let batch = &mut batches[usize::from(near)];
            batch.allocated += allocated;
            if weight == 0.0 { continue; }
            let count = ((allocated as f32 * weight).floor() as usize).max(3).min(allocated);
            let blade_scale = (scale * size) * weight;
            for (sample_index, sample) in samples.iter().take(count).enumerate() {
                batch.blades.push((slots[sample_index], out.len()));
                blade_domains[usize::from(near)].push(domain);
                let (u, w) = crate::grass_stream::fold(weight * sample[0] * 0.5 + 0.5, weight * sample[1] * 0.5 + 0.5);
                let p = a + (b - a) * u + (c - a) * w;
                if export.is_some() { provenance.insert(out.len(), (domain, p.to_array())); }
                let origin = first_origin;
                let p = p - origin;
                let (height, width, template) = stream.blade();
                let h = blade_scale * height;
                let (base_axis, tip_axis) = grass_template(template);
                let half_width = blade_scale * width;
                // Blade orientation from one of 1024 templates (grass_template).
                let side = base_axis * half_width;
                let tip = p + tip_axis * h;
                let shade = (rnd() * 255.0) as u8;
                // Captured native POSITION.w: roots=51/512, tips=axis length+.1.
                // Both batches use the same emitter; vertical
                // height leaves regular tips above sloping terrain at full fade.
                let encoded = grass_pack::emit(p.to_array(), base_axis.to_array(), tip_axis.to_array(), half_width, h);
                // Each side receives light according to its actual blade plane.
                let face = (side * 2.0).cross(tip - p).normalize();
                let vert = |index: usize, corner: i8, flag: u8, sign: f32| {
                    let pos = glam::Vec3::from_array(encoded[index][..3].try_into().unwrap()) + origin;
                    TerrainVertex {
                    pos: pos.to_array(),
                    wind_local: [pos.x - (key[0] * 128) as f32, pos.z - (key[2] * 128) as f32, scale, encoded[index][3]],
                    normal: [(face.x * sign * 127.0).round() as i8, (face.y * sign * 127.0).round() as i8, (face.z * sign * 127.0).round() as i8, corner],
                    palette: [grass_top, grass_top, grass_top, flag],
                    axis: [8, 8, 8, u8::from(near)],
                    rand: [shade, shade, shade, 0],
                }};
                out.extend_from_slice(&[vert(0, 0, 1, 1.0), vert(1, 1, 1, 1.0), vert(2, 2, 2, 1.0)]);
                out.extend_from_slice(&[vert(1, 0, 1, -1.0), vert(0, 1, 1, -1.0), vert(2, 2, 2, -1.0)]);
            }
            if count > 0 {
                for &slot in &slots[count..] {
                    fillers[usize::from(near)].push((domain, slot, out.len()-6));
                }
            }
        }
    }
    for (batch, ids) in batches.iter_mut().zip(blade_domains) {
        for ((slot, _), domain) in batch.blades.iter_mut().zip(ids) {
            *slot = *slot * domains + domain;
        }
        batch.blades.sort_unstable_by_key(|blade| blade.0);
    }
    let mut cells = Vec::new();
    let keys: std::collections::BTreeSet<_> = domain_cells.iter().copied().collect();
    for key in keys {
        let ids: Vec<_> = domain_cells.iter().enumerate().filter_map(|(i,k)| (*k == key).then_some(i)).collect();
        // Producer Studio..Studio shuffles each region's packed domains
        // after traversal, seeded by the integer region hash.
        let seed = (key[0] as u32).wrapping_mul(0x466f45d)
            ^ (key[1] as u32).wrapping_mul(0x127409f)
            ^ (key[2] as u32).wrapping_mul(0x4f9ffb7);
        let order = crate::grass_stream::Stream::from_seed(seed as u64).shuffled_slots(ids.len());
        let mut rank = vec![0; ids.len()];
        for (ordinal, original) in order.into_iter().enumerate() { rank[original] = ordinal; }
        for (near, batch) in batches.iter().enumerate() {
            let mut selected = GrassBatch { allocated: if domains == 0 {0} else {batch.allocated / domains * ids.len()}, blades: Vec::new() };
            for &(ordinal, first) in &batch.blades {
                if let Ok(domain) = ids.binary_search(&(ordinal % domains)) {
                    selected.blades.push((ordinal / domains * ids.len() + rank[domain], first));
                }
            }
            // Native copies the last generated blade into unused shuffled
            // slots. Preserve these in the resident upload: the original can
            // lie beyond the selected prefix while its filler lies inside.
            for &(domain, slot, first) in &fillers[near] {
                if let Ok(local) = ids.binary_search(&domain) {
                    selected.blades.push((slot * ids.len() + rank[local], first));
                }
            }
            selected.blades.sort_unstable_by_key(|blade| blade.0);
            // Native draw caller receives the owning 64-stud cell expanded by
            // four studs, independently of occupied domains and residency.
            let origin = glam::Vec3::from_array(key.map(|v| v as f32 * 64.0));
            let lo = origin - glam::Vec3::splat(4.0);
            let hi = origin + glam::Vec3::splat(68.0);
            cells.push(GrassCell {lo, hi, near: near != 0, batch: selected});
        }
    }
    if let Some(dir) = export {
        use std::io::Write;
        let path = std::path::Path::new(&dir).join(format!("chunk_{}_{}_{}.jsonl",key[0],key[1],key[2]));
        let mut f = std::io::BufWriter::new(std::fs::File::create(path).unwrap());
        for d in export_domains { writeln!(f,"{}",serde_json::json!({"kind":"domain","storage":key,"data":d})).unwrap(); }
        for cell in &cells {
            let camera=glam::Vec3::new(-12.0,155.0,210.0);
            let distance=camera.distance(camera.clamp(cell.lo,cell.hi));
            let factor=(2.0/(1.0+distance.min(crate::mech::GRASS_RADIUS)/crate::mech::GRASS_RADIUS)-1.0).clamp(0.0,1.0);
            let group=if cell.near {3} else {1};
            let limit=((cell.batch.allocated/group) as f32*factor).trunc() as usize*group;
            for &(ordinal,first) in &cell.batch.blades {
                let (domain,root)=provenance[&first];
                // Identify real slots by their pre-region shuffled slot, independent of domain permutation.
                let slot=ordinal/(cell.batch.allocated / crate::grass_stream::sample_count(length,density,cell.near)).max(1);
                let filler=!batches[usize::from(cell.near)].blades.iter().any(|&(o,f)| f==first && o/domains==slot);
                writeln!(f,"{}",serde_json::json!({"kind":"blade","storage":key,"domain":domain,"root":root,"region":domain_cells[domain],"near":cell.near,"ordinal":ordinal,"allocated":cell.batch.allocated,"limit":limit,"selected":ordinal<limit,"filler":filler})).unwrap();
            }
        }
    }
    GrassGeometry { vertices: out, batches, cells }
}

/// Whether chunk `key` has Grass on its top faces (decoration candidates).
pub fn chunk_has_grass_top(verts: &[TerrainVertex]) -> bool {
    let g = palette(2, 0);
    verts.chunks_exact(3).any(|t| t[0].palette[..3].iter().any(|&p| p / 3 == g / 3))
}

/// The chunks a render must re-mesh after `old` became `new`: those whose
/// voxels changed and their neighbours (a chunk's surface reads one voxel
/// past its border), or every one when there was no old grid.
/// The render chunks to re-mesh after an edit inside the voxel box
/// `lo..=hi`: the chunks it touches (one voxel out) and their neighbours.
pub fn dirty_box(lo: [i32; 3], hi: [i32; 3]) -> Vec<[i32; 3]> {
    let a = chunk_key(lo.map(|c| c - 1));
    let b = chunk_key(hi.map(|c| c + 1));
    let mut v = Vec::new();
    for z in a[2] - 1..=b[2] + 1 {
        for y in a[1] - 1..=b[1] + 1 {
            for x in a[0] - 1..=b[0] + 1 {
                v.push([x, y, z]);
            }
        }
    }
    v
}

pub fn dirty_chunks(old: Option<&Voxels>, new: &Voxels) -> Vec<[i32; 3]> {
    let mut changed: Vec<[i32; 3]> = Vec::new();
    match old {
        None => changed.extend(new.chunks.keys().copied()),
        Some(o) => {
            for (k, c) in &new.chunks {
                match o.chunks.get(k) {
                    Some(oc) if oc.mat == c.mat && oc.occ == c.occ => {}
                    _ => changed.push(*k),
                }
            }
            changed.extend(o.chunks.keys().filter(|k| !new.chunks.contains_key(*k)).copied());
        }
    }
    let mut out: std::collections::HashSet<[i32; 3]> = std::collections::HashSet::new();
    for k in changed {
        for dz in -1..=1 {
            for dy in -1..=1 {
                for dx in -1..=1 {
                    out.insert([k[0] + dx, k[1] + dy, k[2] + dz]);
                }
            }
        }
    }
    let mut v: Vec<[i32; 3]> = out.into_iter().collect();
    v.sort_unstable();
    v
}

impl Voxels {
    /// Whether chunk `key` or a chunk beside it holds any water (its surface
    /// depends on the solid voxels around it).
    pub fn has_water_near(&self, key: [i32; 3]) -> bool {
        for dz in -1..=1 {
            for dy in -1..=1 {
                for dx in -1..=1 {
                    if let Some(c) = self.chunks.get(&[key[0] + dx, key[1] + dy, key[2] + dz]) {
                        if c.mat.contains(&WATER) || c.water.is_some() {
                            return true;
                        }
                    }
                }
            }
        }
        false
    }
}

/// Which SmoothGrid materials the grid holds.
pub fn materials_present(v: &Voxels) -> [bool; 23] {
    let mut out = [false; 23];
    for c in v.chunks.values() {
        let mut seen = [false; 256];
        for &m in c.mat.iter() {
            seen[m as usize] = true;
        }
        for (i, o) in out.iter_mut().enumerate() {
            *o |= seen[i];
        }
    }
    out
}

/// Whether the water in two grids differs (so its mesh must be rebuilt).
pub fn water_changed(old: Option<&Voxels>, new: &Voxels) -> bool {
    let Some(o) = old else { return true };
    // Native liquid extraction also depends on the solid bank samples.
    if !o.same_solid(new) { return true; }
    let water = |c: &Chunk| -> Vec<u16> {
        c.mat
            .iter()
            .zip(c.occ.iter())
            .enumerate()
            .map(|(i, (&m, &b))| {
                if m == WATER {
                    b as u16 + 1
                } else {
                    c.water.as_ref().map_or(0, |w| w[i])
                }
            })
            .collect()
    };
    let keys: std::collections::HashSet<&[i32; 3]> = o.chunks.keys().chain(new.chunks.keys()).collect();
    keys.into_iter().any(|k| {
        let a = o.chunks.get(k).map(water).unwrap_or_default();
        let b = new.chunks.get(k).map(water).unwrap_or_default();
        let empty = |v: &Vec<u16>| v.iter().all(|&x| x == 0);
        if a.is_empty() || b.is_empty() {
            !(empty(&a) && empty(&b))
        } else {
            a != b
        }
    })
}

/// Collision geometry: every non-water mesh concatenated into one triangle soup
/// (vertices are not welded across materials).
pub fn collision_mesh(meshes: &[MaterialMesh]) -> (Vec<[f32; 3]>, Vec<u32>) {
    let mut pos = Vec::new();
    let mut idx = Vec::new();
    for m in meshes.iter().filter(|m| !is_water(m.material)) {
        let base = pos.len() as u32;
        pos.extend_from_slice(&m.positions);
        idx.extend(m.indices.iter().map(|i| i + base));
    }
    (pos, idx)
}

#[cfg(test)]
mod tests {
    #[test]
    #[ignore = "requires checks/water-full-re native GPU evidence"]
    fn water_matches_native_gpu_positions() {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("checks/water-full-re");
        let rows: Vec<(i32,i32,i32,String,f32,f32)> = serde_json::from_slice(
            &std::fs::read(root.join("slope-voxels.json")).unwrap()).unwrap();
        let mut v = Voxels::empty();
        for (x,y,z,name,solid,liquid) in rows {
            let mat = if solid > 0.0 { material_index(&name).unwrap() } else { WATER };
            let occ = ((if solid > 0.0 {solid} else {liquid})*255.0).round() as u8;
            let water = if solid > 0.0 && liquid > 0.0 { (liquid*255.0).round() as u16+1 } else {0};
            let p = [x,y,z];
            v.chunk_mut(chunk_key(p)).write(local_index(p),1,mat,occ,water);
        }
        let meshes = build_water_meshes(&v);
        let ours: Vec<_> = meshes.iter().flat_map(|m| m.positions.iter().copied()).collect();
        let triangles: Vec<[[f32;3];3]> = meshes.iter().flat_map(|m| m.indices.chunks_exact(3).map(|t| [m.positions[t[0] as usize],m.positions[t[1] as usize],m.positions[t[2] as usize]])).collect();
        std::fs::write(root.join("rustudio-water-triangles.json"),serde_json::to_vec(&triangles).unwrap()).unwrap();
        std::fs::write(root.join("rustudio-water-positions.json"),serde_json::to_vec(&ours).unwrap()).unwrap();
        let mut errors = Vec::new();
        for event in [499, 502, 505, 508, 511, 514, 517, 520] {
            let native: Vec<[f32;3]> = serde_json::from_slice(&std::fs::read(root.join(format!("positions-{event}.json"))).unwrap()).unwrap();
            for p in native {
                let d = ours.iter().map(|q| (0..3).map(|k| (p[k]-q[k]).powi(2)).sum::<f32>()).fold(f32::INFINITY,f32::min).sqrt();
                errors.push(d);
            }
        }
        errors.sort_by(f32::total_cmp);
        let report = format!("samples={} ours={} median={} p95={} maximum={}\n",errors.len(),ours.len(),errors[errors.len()/2],errors[errors.len()*95/100],errors.last().unwrap());
        std::fs::write(root.join("geometry-comparison.txt"),&report).unwrap();
        assert!(*errors.last().unwrap() < 0.12,"{report}");
    }

    #[test]
    fn water_land_interface_has_no_internal_curtains() {
        let mut v = Voxels::empty();
        for x in -4..=4 { for y in -4..=0 { for z in -4..=4 {
            v.set([x,y,z], if x <= 0 { WATER } else { 2 }, 1.0);
        }}}
        let meshes = build_water_meshes(&v);
        let water = &meshes[0];
        let mut shoreline = 0;
        for tri in water.indices.chunks_exact(3) {
            let p = [tri[0],tri[1],tri[2]].map(|i| water.positions[i as usize]);
            let center = [0,1,2].map(|k| (p[0][k]+p[1][k]+p[2][k])/3.0);
            if center[0] > 0.0 && center[0] < 5.0 && center[2].abs() < 8.0 && center[1] > -12.0 {
                assert!(p.iter().all(|p| (p[1]-4.0).abs()<1e-5), "internal wall: {p:?}");
                shoreline += 1;
            }
        }
        assert!(shoreline > 0, "top surface must reach the bank");
    }

    #[test]
    fn water_bank_changes_invalidate_water_mesh() {
        let mut before = Voxels::empty();
        before.set([0,0,0],WATER,1.0);
        let mut after = Voxels::empty();
        after.set([0,0,0],WATER,1.0);
        after.set([1,0,0],2,0.5);
        assert!(water_changed(Some(&before),&after));
    }

    #[test]
    fn mixed_liquid_occupancy_changes_invalidate_water_mesh() {
        let mut before = Voxels::empty();
        let mut after = Voxels::empty();
        before.chunk_mut([0,0,0]).write(0,1,2,127,64);
        after.chunk_mut([0,0,0]).write(0,1,2,127,192);
        assert!(before.same_solid(&after));
        assert!(water_changed(Some(&before),&after));
    }

    #[test]
    fn water_mixed_banks_agree_across_all_chunk_axes() {
        let mut v = Voxels::empty();
        for z in -3i32..=36 { for y in -3i32..=36 { for x in -3i32..=36 {
            if y > 33 { continue; }
            let bank = x + z > 31;
            let occ = if bank { 0.25 + (y.rem_euclid(4) as f32)*0.25 } else { 1.0 };
            v.set([x,y,z],if bank {2} else {WATER},occ);
            if bank && y == 32 {
                let p = [x,y,z];
                let c = v.chunk_mut(chunk_key(p));
                c.write(local_index(p),1,2,127,192);
            }
        }}}
        let mut a = Grid::new(); a.fill(&v,[0,0,0]);
        for axis in 0..3 {
            let mut key = [0,0,0];key[axis] = 1;
            let mut b = Grid::new();b.fill(&v,key);
            for u in 1..=CS { for w in 1..=CS {
                let mut p = [0;3];p[axis]=32;p[(axis+1)%3]=u;p[(axis+2)%3]=w;
                let mut q=p;q[axis]=0;
                let (i,j)=(pidx(p[0],p[1],p[2]),pidx(q[0],q[1],q[2]));
                assert_eq!((a.water_kind[i],a.water[i]),(b.water_kind[j],b.water[j]),"axis {axis}, {p:?}");
            }}
        }
    }

    #[test]
    fn water_bank_extension_agrees_across_chunk_seam() {
        let mut v = Voxels::empty();
        for x in 28..=35 { for z in -2..=2 { for y in -2..=0 {
            v.set([x,y,z], if x <= 30 { WATER } else { 2 }, 1.0);
        }}}
        let mut a = Grid::new();
        let mut b = Grid::new();
        a.fill(&v, [0,0,0]);
        b.fill(&v, [1,0,0]);
        assert_eq!(a.water_kind[pidx(32,1,1)], 1);
        assert_eq!(a.water[pidx(32,1,1)], b.water[pidx(0,1,1)]);
        assert_eq!(a.cell_vertex(1,[32,1,1],[0,0,0]).0,
                   b.cell_vertex(1,[0,1,1],[1,0,0]).0);
    }

    #[test]
    fn water_surface_keeps_its_level_beside_a_higher_bank() {
        let mut v = Voxels::empty();
        for x in -3..=3 { for z in -3..=3 { for y in -3..=2 {
            if x > 0 { v.set([x,y,z], 2, 1.0); }
            else if y <= 0 { v.set([x,y,z], WATER, 1.0); }
        }}}
        let mut g = Grid::new();
        g.fill(&v, [0,0,0]);
        let (p, _) = g.cell_vertex(1, [1,1,1], [0,0,0]);
        assert!((p[1]-6.0).abs() < 1e-6, "bank must not lift water: {p:?}");
        assert!(g.water[pidx(2,1,1)] >= ISO, "water must overlap the occupied bank");
    }

    #[test]
    fn water_shoreline_continues_across_solid_samples() {
        let mut voxels = Voxels::empty();
        // A level lake meets a level bank at x=1, including a chunk seam.
        for x in -2..=3 { for z in -2..=3 {
            voxels.set([x, 0, z], if x <= 0 { WATER } else { 2 }, 1.0);
        }}
        let mut grid = Grid::new();
        grid.fill(&voxels, [0, 0, 0]);
        let (water, _) = grid.cell_vertex(1, [1, 1, 1], [0, 0, 0]);
        let (land, _) = grid.cell_vertex(0, [2, 1, 1], [0, 0, 0]);
        assert!((water[1] - land[1]).abs() < 1e-6,
            "shoreline must not retract down toward the solid bank: {water:?} {land:?}");
        assert!(water.iter().all(|v| v.is_finite()));
    }
    use super::*;
    use std::collections::BTreeMap;

    #[test]
    fn grass_generation_is_distributed_repeatable_and_scaled() {
        let mut v = Voxels::empty();
        for x in -1..=3 {
            for z in -1..=3 {
                v.set([x, 0, z], 2, 1.0);
            }
        }
        let blades = grass_blades(&v, [0, 0, 0], 0.7);
        assert!(!blades.is_empty());
        assert_eq!(bytemuck::cast_slice::<_, u8>(&blades),
            bytemuck::cast_slice::<_, u8>(&grass_blades(&v, [0, 0, 0], 0.7)));
        let taller = grass_blades(&v, [0, 0, 0], 0.6999);
        assert_eq!(blades.len(), taller.len());
        // Native template allocation is per triangle, including small faces.
        let surface = chunk_triangles(&v, [0, 0, 0], &|_| false);
        let eligible_area: f32 = surface.chunks_exact(3).filter_map(|t| {
            let a = glam::Vec3::from(t[0].pos);
            let n = (glam::Vec3::from(t[1].pos) - a).cross(glam::Vec3::from(t[2].pos) - a);
            (n.y / n.length() >= 0.7).then_some(n.length() * 0.5)
        }).sum();
        let count = blades.len() / 6;
        assert!(eligible_area > 0.0);
        let eligible = surface.chunks_exact(3).filter(|t| {
            let a = glam::Vec3::from(t[0].pos);
            let n = (glam::Vec3::from(t[1].pos)-a).cross(glam::Vec3::from(t[2].pos)-a);
            n.y / n.length() >= 0.7
        }).count();
        // Four domains have Q14=16383: one fewer real blade in each batch.
        assert_eq!(eligible, 32);
        assert_eq!(count, eligible * (13 + 16) - 8);
        let near_count = blades.chunks_exact(6).filter(|b| b[0].axis[3] == 1).count();
        assert_eq!(near_count, eligible * 16 - 4);
        assert_eq!(count - near_count, eligible * 13 - 4);
        for b in blades.chunks_exact(6) {
            assert!(b.iter().all(|v| v.axis[3] == b[0].axis[3]));
        }
        let scale = 0.45 + 0.825 * 0.7;
        let mut roots = std::collections::HashSet::new();
        for (blade, tall) in blades.chunks_exact(6).zip(taller.chunks_exact(6)) {
            assert_eq!(blade[0].palette[3], 1);
            assert_eq!(blade[2].palette[3], 2);
            assert_eq!(blade[0].wind_local[3], 51.0 / 512.0);
            let root_mid = (glam::Vec3::from(blade[0].pos) + glam::Vec3::from(blade[1].pos)) * 0.5;
            let axis_length = glam::Vec3::from(blade[2].pos).distance(root_mid);
            assert!((blade[2].wind_local[3] - axis_length - 0.1).abs() <= 4.0 / 512.0 + 1e-5);
            let root = (glam::Vec3::from(blade[0].pos) + glam::Vec3::from(blade[1].pos)) * 0.5;
            roots.insert(root.to_array().map(f32::to_bits));
            let expected = (glam::Vec3::from(blade[1].pos) - glam::Vec3::from(blade[0].pos))
                .cross(glam::Vec3::from(blade[2].pos) - root).normalize();
            let normal = glam::Vec3::new(blade[0].normal[0] as f32, blade[0].normal[1] as f32, blade[0].normal[2] as f32) / 127.0;
            assert!(normal.dot(expected) > 0.99, "blade normal follows its face");
            for axis in 0..3 { assert_eq!(blade[0].normal[axis], -blade[3].normal[axis]); }
            let height = blade[2].pos[1] - root.y;
            assert!((0.85 * 1.65 * scale * 40.0_f32.to_radians().cos() - 1e-5..=3.8 * scale + 1e-5).contains(&height));
            let width = glam::Vec3::from(blade[1].pos).distance(glam::Vec3::from(blade[0].pos));
            assert!((0.85 * 0.2 * scale - 2.0 * 3.0_f32.sqrt() / 512.0..=0.6 * scale + 2.0 * 3.0_f32.sqrt() / 512.0).contains(&width));
            assert!((tall[2].pos[1] - root.y - ((0.45 + 0.825 * 0.6999) / (0.45 + 0.825 * 0.7)) * height).abs() < 8.0 / 512.0);
        }
        assert!(roots.len() > blades.len() / 12, "blades must not pile up at one root");
        if std::env::var_os("RUSTUDIO_GRASS_MEASURE").is_some() {
            for batch in 0..=1 {
                let mut ranges = [[f32::INFINITY, f32::NEG_INFINITY]; 3];
                for b in blades.chunks_exact(6).filter(|b| b[0].axis[3] == batch) {
                    let a = glam::Vec3::from(b[0].pos);
                    let c = glam::Vec3::from(b[1].pos);
                    let root = (a + c) * 0.5;
                    for (i, v) in [root.y, a.distance(c), b[2].pos[1] - root.y].into_iter().enumerate() {
                        ranges[i][0] = ranges[i][0].min(v);
                        ranges[i][1] = ranges[i][1].max(v);
                    }
                }
                println!("batch={batch} root_y/width/vertical_height={ranges:?}");
            }
        }

    }

    #[test]
    fn mixed_grass_faces_remain_decoration_candidates() {
        let mut v = Voxels::empty();
        for x in -2..=5 { for z in -2..=5 {
            v.set([x, 0, z], if x < 2 { 2 } else { 8 }, 1.0);
        }}
        let surface = chunk_triangles(&v, [0, 0, 0], &|_| false);
        let mixed: Vec<_> = surface.chunks_exact(3).filter(|t|
            t[0].palette[..3].iter().any(|&p| p / 3 == 2) &&
            t[0].palette[..3].iter().any(|&p| p / 3 != 2)).collect();
        assert!(!mixed.is_empty());
        for t in mixed { assert!(chunk_has_grass_top(t)); }
        let blades = grass_blades(&v, [0, 0, 0], 0.7);
        assert!(blades.iter().all(|v| v.pos.iter().all(|x| x.is_finite())));
        // Mixed weights shrink geometry, rather than rejecting the whole face.
        assert!(blades.chunks_exact(6).any(|b|
            glam::Vec3::from(b[0].pos).distance(glam::Vec3::from(b[1].pos)) < 0.2 * (0.45+0.825*0.7)));
    }

    #[test]
    fn decoration_ownership_matches_accepted_native_source_cells() {
        let rows: serde_json::Value = serde_json::from_str(include_str!(
            "../tests/fixtures/grass/decoration-regression.json")).unwrap();
        for row in rows.as_array().unwrap() {
            let raw = std::array::from_fn(|i| {
                let r = &row["raw"][i];
                (r[0].as_u64().unwrap() as u8, r[1].as_u64().unwrap() as u8)
            });
            assert_eq!(decoration_owner(raw), row["native"].as_u64().unwrap() as u8,
                "source cell {:?}", row["cell"]);
            let mut grid = Grid::new();
            for (offset, (material, occupancy)) in
                [0, 1, 1 + P, P, PP, PP + 1, PP + 1 + P, PP + P].into_iter().zip(raw) {
                let p = pidx(1, 1, 1) + offset;
                grid.mat[p] = material;
                grid.solid[p] = if material > 1 { 0.5 + 0.5 * occupancy_from_byte(occupancy) } else { 0.0 };
            }
            assert_eq!(grid.decoration_material([1, 1, 1]), row["native"].as_u64().unwrap() as u8);
        }
    }

    #[test]
    fn corner_material_sums_occupancy_and_breaks_ties_by_slot() {
        let mut g = Grid::new();
        let p = pidx(1, 1, 1);
        g.mat[p] = 10;
        g.solid[p] = 1.0;
        g.mat[p + 1] = 2;
        g.solid[p + 1] = 0.6;
        g.mat[p + P] = 2;
        g.solid[p + P] = 0.6;
        assert_eq!(g.cell_material([1, 1, 1]), 2);
        g.solid[p + P] = 0.4;
        assert_eq!(g.cell_material([1, 1, 1]), 2);
        g.solid[p + P] = 0.3;
        assert_eq!(g.cell_material([1, 1, 1]), 10);
        g.mat[p] = WATER;
        assert_eq!(g.cell_material([1, 1, 1]), 2);
    }

    #[test]
    fn fill_encode_round_trip() {
        let mut v = Voxels::empty();
        // A 64 × 8 × 64 slab of Grass whose top is at y = 2 (half a voxel up).
        v.fill([-32.0, -6.0, -32.0], [32.0, 2.0, 32.0], 2, |c| {
            let q = [c[0].abs() - 32.0, (c[1] + 2.0).abs() - 4.0, c[2].abs() - 32.0];
            let out = q.map(|x| x.max(0.0));
            (out[0] * out[0] + out[1] * out[1] + out[2] * out[2]).sqrt()
                + q[0].max(q[1]).max(q[2]).min(0.0)
        });
        // A ball of Sand, and a water voxel, and an Air carve.
        v.fill([36.0, -4.0, -4.0], [44.0, 4.0, 4.0], 6, |c| {
            ((c[0] - 40.0).powi(2) + c[1].powi(2) + c[2].powi(2)).sqrt() - 4.0
        });
        v.set([20, 5, 20], WATER, 0.5);
        v.fill([-4.0, -4.0, -4.0], [4.0, 4.0, 4.0], 0, |c| {
            (c[0].powi(2) + (c[1] + 2.0).powi(2) + c[2].powi(2)).sqrt() - 3.0
        });
        assert_eq!(v.get(5, -1, 5), (2, 1.0), "inside the slab: full Grass");
        let (m, top) = v.get(5, 0, 5);
        assert_eq!(m, 2);
        assert!((top - 0.5).abs() < 0.02, "the slab's top voxel is half full: {top}");
        assert_eq!(v.get(5, 1, 5).0, 0, "above the slab: Air");
        assert_eq!(v.get(10, 0, 0).0, 6, "the ball is Sand");
        assert!(v.get(0, -1, 0).1 < 1.0, "the Air ball carved the slab");
        let back = Voxels::decode(&v.encode()).expect("decode");
        assert_eq!(back.count(), v.count());
        for y in -3..3 {
            for z in -10..12 {
                for x in -10..12 {
                    assert_eq!(back.get(x, y, z), v.get(x, y, z), "voxel {x} {y} {z}");
                }
            }
        }
        assert_eq!(back.get(20, 5, 20).0, WATER);
        let mut c = back;
        c.clear();
        assert_eq!(c.count(), 0);
        assert_eq!(c.encode(), vec![1, CHUNK_SHIFT as u8]);
    }

    /// (material, occupancy byte, shoreline water byte)
    type Cell = (u8, u8, Option<u8>);

    /// Test-only encoder following the documented layout: chunks sorted by
    /// X, Y, Z; interleaved big-endian offsets; runs of up to 256.
    fn encode(cells: &HashMap<[i32; 3], Cell>) -> Vec<u8> {
        let mut chunks: BTreeMap<[i32; 3], Vec<Cell>> = BTreeMap::new();
        for (p, c) in cells {
            chunks
                .entry(chunk_key(*p))
                .or_insert_with(|| vec![(0, 255, None); CV])[local_index(*p)] = *c;
        }
        let norm = |c: Cell| -> Cell {
            match c {
                (0, _, _) => (0, 255, None),
                (m, o, _) if m == WATER || o == 255 => (m, o, None),
                c => c,
            }
        };
        let mut out = vec![1u8, 5];
        let mut prev = [0i32; 3];
        for (k, data) in chunks {
            let delta = [0, 1, 2].map(|a| k[a].wrapping_sub(prev[a]).to_be_bytes());
            for b in 0..4 {
                for d in &delta {
                    out.push(d[b]);
                }
            }
            prev = k;
            let mut i = 0;
            while i < CV {
                let (m, o, w) = norm(data[i]);
                let occ_flag = if o != 255 { 0x40 } else { 0 };
                if let Some(w) = w {
                    out.push(m | occ_flag | 0x80);
                    if o != 255 {
                        out.push(o);
                    }
                    out.extend_from_slice(&[0, w]);
                    i += 1;
                    continue;
                }
                let mut n = 1;
                while i + n < CV && n < 256 && norm(data[i + n]) == (m, o, w) {
                    n += 1;
                }
                out.push(m | occ_flag | if n > 1 { 0x80 } else { 0 });
                if o != 255 {
                    out.push(o);
                }
                if n > 1 {
                    out.push((n - 1) as u8);
                }
                i += n;
            }
        }
        out
    }

    fn base64(s: &str) -> Vec<u8> {
        let val = |c: u8| match c {
            b'A'..=b'Z' => c - b'A',
            b'a'..=b'z' => c - b'a' + 26,
            b'0'..=b'9' => c - b'0' + 52,
            b'+' => 62,
            _ => 63,
        };
        let mut out = Vec::new();
        let (mut acc, mut bits) = (0u32, 0);
        for c in s.bytes().filter(|&c| c != b'=') {
            acc = (acc << 6) | val(c) as u32;
            bits += 6;
            if bits >= 8 {
                bits -= 8;
                out.push((acc >> bits) as u8);
            }
        }
        out
    }

    /// Published test vector from rbx-dom PR 444 (two chunks at (0,0,0) and
    /// (1,0,0), each with Water at local (0,0,0) and Pavement at (1,0,0)).
    const RBXDOM_VECTOR: &str = "AQUAAAAAAAAAAAAAAAABFoD/gP+A/4D/gP+A/4D/gP+A/4D/gP+A/4D/gP+A/4D/gP+A/4D/gP+A/4D/gP+A/4D/gP+A/4D/gP+A/4D/gP+A/4D/gP+A/4D/gP+A/4D/gP+A/4D/gP+A/4D/gP+A/4D/gP+A/4D/gP+A/4D/gP+A/4D/gP+A/4D/gP+A/4D/gP+A/4D/gP+A/4D/gP+A/4D/gP+A/4D/gP+A/4D/gP+A/4D/gP+A/4D/gP+A/4D/gP+A/4D/gP+A/4D/gP+A/4D/gP+A/4D/gP+A/4D/gP+A/4D/gP+A/4D/gP+A/4D/gP+A/4D/gP+A/4D/gP+A/4D/gP+A/4D/gP+A/4D/gP0AAAAAAAAAAAABAAABFoD/gP+A/4D/gP+A/4D/gP+A/4D/gP+A/4D/gP+A/4D/gP+A/4D/gP+A/4D/gP+A/4D/gP+A/4D/gP+A/4D/gP+A/4D/gP+A/4D/gP+A/4D/gP+A/4D/gP+A/4D/gP+A/4D/gP+A/4D/gP+A/4D/gP+A/4D/gP+A/4D/gP+A/4D/gP+A/4D/gP+A/4D/gP+A/4D/gP+A/4D/gP+A/4D/gP+A/4D/gP+A/4D/gP+A/4D/gP+A/4D/gP+A/4D/gP+A/4D/gP+A/4D/gP+A/4D/gP+A/4D/gP+A/4D/gP+A/4D/gP+A/4D/gP+A/4D/gP+A/4D/gP+A/4D/gP+A/4D/gP0=";

    #[test]
    fn terrain_decodes_published_vector() {
        let bytes = base64(RBXDOM_VECTOR);
        let v = Voxels::decode(&bytes).unwrap();
        assert_eq!(v.count(), 4);
        assert_eq!(v.chunk_count(), 2);
        assert_eq!(v.get(0, 0, 0), (WATER, 1.0));
        assert_eq!(v.get(1, 0, 0), (22, 1.0));
        assert_eq!(v.get(32, 0, 0), (WATER, 1.0));
        assert_eq!(v.get(33, 0, 0), (22, 1.0));
        assert_eq!(v.get(2, 0, 0), (0, 0.0));
        assert_eq!(material_name(v.get(33, 0, 0).0), "Pavement");
        assert_eq!(v.bounds(), Some(([0, 0, 0], [33, 0, 0])));
        // Our test encoder must reproduce it byte for byte.
        let cells: HashMap<[i32; 3], Cell> = [
            ([0, 0, 0], (WATER, 255, None)),
            ([1, 0, 0], (22, 255, None)),
            ([32, 0, 0], (WATER, 255, None)),
            ([33, 0, 0], (22, 255, None)),
        ]
        .into_iter()
        .collect();
        assert_eq!(encode(&cells), bytes);
    }

    #[test]
    fn terrain_roundtrip() {
        let mut cells: HashMap<[i32; 3], Cell> = HashMap::new();
        cells.insert([0, 0, 0], (2, 255, None));
        cells.insert([-1, -1, -1], (8, 127, None));
        cells.insert([-33, 5, 40], (10, 0, None));
        cells.insert([5, -70, 3], (3, 100, Some(255)));
        cells.insert([6, -70, 3], (WATER, 200, None));
        for x in 100..400 {
            cells.insert([x, 7, -2], (14, 255, None)); // long run crossing chunks
        }
        let v = Voxels::decode(&encode(&cells)).unwrap();
        assert_eq!(v.count(), cells.len());
        for (p, (m, o, _)) in &cells {
            let (gm, go) = v.get(p[0], p[1], p[2]);
            assert_eq!(gm, *m, "material at {p:?}");
            assert!(
                (go - occupancy_from_byte(*o)).abs() < 1e-6,
                "occupancy at {p:?}"
            );
        }
        assert!((v.get(-1, -1, -1).1 - 0.5).abs() < 1e-6);
        assert!((v.get(-33, 5, 40).1 - 1.0 / 256.0).abs() < 1e-6);
        assert!((v.water(5, -70, 3) - 1.0).abs() < 1e-6);
        assert!((v.water(6, -70, 3) - occupancy_from_byte(200)).abs() < 1e-6);
        assert_eq!(v.water(0, 0, 0), 0.0);
        assert_eq!(v.get(1, 0, 0), (0, 0.0));
        assert_eq!(v.bounds(), Some(([-33, -70, -2], [399, 7, 40])));
        assert_eq!(v.world_bounds().unwrap().1, [1600.0, 32.0, 164.0]);
        assert_eq!(voxel_center([0, 0, -1]), [2.0, 2.0, -2.0]);
    }

    #[test]
    fn terrain_rejects_bad_input() {
        assert_eq!(Voxels::decode(&[]).unwrap().count(), 0);
        assert!(Voxels::decode(&[1]).is_err());
        assert!(Voxels::decode(&[2, 5]).unwrap_err().contains("version"));
        assert!(Voxels::decode(&[1, 9]).is_err());
        let mut b = vec![1, 5];
        b.extend_from_slice(&[0; 12]);
        assert!(Voxels::decode(&b).unwrap_err().contains("truncated"));
        let mut over = b.clone();
        for _ in 0..127 {
            over.extend_from_slice(&[0x80, 255]);
        }
        over.extend_from_slice(&[0, 0x80, 255]); // 32513 + 256 > 32768
        assert!(Voxels::decode(&over).unwrap_err().contains("past the end"));
        let mut unk = b.clone();
        unk.push(63);
        assert!(Voxels::decode(&unk)
            .unwrap_err()
            .contains("unknown material"));
        // Header + one empty chunk is a valid, empty grid.
        let mut air = b.clone();
        for _ in 0..128 {
            air.extend_from_slice(&[0x80, 255]);
        }
        let v = Voxels::decode(&air).unwrap();
        assert_eq!(
            (v.count(), v.bounds().is_none(), build_meshes(&v).len()),
            (0, true, 0)
        );
    }

    #[test]
    fn terrain_decodes_other_chunk_sizes() {
        // Exponent 1 (2^3 chunks): chunk (-1,0,3) => voxels x -2..-1, z 6..7.
        let mut b = vec![1, 1];
        let d = [
            (-1i32).to_be_bytes(),
            0i32.to_be_bytes(),
            3i32.to_be_bytes(),
        ];
        for k in 0..4 {
            for a in &d {
                b.push(a[k]);
            }
        }
        // index 1 = (x=1,z=0,y=0) Rock; index 6 = (x=0,z=1,y=1) Snow half.
        b.extend_from_slice(&[0, 8, 0x80, 3, 0x40 | 10, 127, 0]);
        let v = Voxels::decode(&b).unwrap();
        assert_eq!(v.count(), 2);
        assert_eq!(v.get(-1, 0, 6).0, 8);
        assert_eq!(v.get(-2, 1, 7).0, 10);
    }

    #[test]
    fn terrain_material_names() {
        assert_eq!(material_name(0), "Air");
        assert_eq!(material_name(1), "Water");
        assert_eq!(material_name(2), "Grass");
        assert_eq!(material_name(3), "Slate");
        assert_eq!(material_name(8), "Rock");
        assert_eq!(material_name(22), "Pavement");
        assert_eq!(material_name(23), "Unknown");
        assert_eq!(material_index("LeafyGrass"), Some(19));
        assert!(is_water(1) && !is_water(2));
        for m in 0..23u8 {
            assert_eq!(material_index(material_name(m)), Some(m));
        }
    }

    fn block(min: [i32; 3], max: [i32; 3], c: Cell) -> HashMap<[i32; 3], Cell> {
        let mut cells = HashMap::new();
        for x in min[0]..=max[0] {
            for y in min[1]..=max[1] {
                for z in min[2]..=max[2] {
                    cells.insert([x, y, z], c);
                }
            }
        }
        cells
    }

    fn sub(a: [f32; 3], b: [f32; 3]) -> [f32; 3] {
        [a[0] - b[0], a[1] - b[1], a[2] - b[2]]
    }
    fn cross(a: [f32; 3], b: [f32; 3]) -> [f32; 3] {
        [
            a[1] * b[2] - a[2] * b[1],
            a[2] * b[0] - a[0] * b[2],
            a[0] * b[1] - a[1] * b[0],
        ]
    }
    fn dot(a: [f32; 3], b: [f32; 3]) -> f32 {
        a[0] * b[0] + a[1] * b[1] + a[2] * b[2]
    }

    /// Closed, consistently wound, outward-facing surface around `center`.
    fn check_closed(m: &MaterialMesh, center: [f32; 3]) {
        assert_eq!(m.indices.len() % 3, 0);
        let mut directed: HashMap<(u32, u32), u32> = HashMap::new();
        for t in m.indices.chunks(3) {
            for k in 0..3 {
                *directed.entry((t[k], t[(k + 1) % 3])).or_default() += 1;
            }
            let p = [0, 1, 2].map(|k| m.positions[t[k] as usize]);
            let n = cross(sub(p[1], p[0]), sub(p[2], p[0]));
            let c = [0, 1, 2].map(|k| (p[0][k] + p[1][k] + p[2][k]) / 3.0);
            assert!(dot(n, sub(c, center)) > 0.0, "triangle faces inward");
        }
        for (&(a, b), &n) in &directed {
            assert_eq!(n, 1, "edge used twice in the same direction");
            assert_eq!(directed.get(&(b, a)), Some(&1), "open edge");
        }
        for (p, n) in m.positions.iter().zip(&m.normals) {
            assert!(dot(*n, sub(*p, center)) > 0.0, "normal points inward");
            assert!((dot(*n, *n) - 1.0).abs() < 1e-4);
        }
        // Euler characteristic of a sphere.
        let (v, e, f) = (
            m.positions.len() as i64,
            directed.len() as i64 / 2,
            m.indices.len() as i64 / 3,
        );
        assert_eq!(v - e + f, 2);
    }

    #[test]
    fn terrain_block_meshes_closed() {
        let v = Voxels::decode(&encode(&block([0, 0, 0], [2, 2, 2], (2, 255, None)))).unwrap();
        assert_eq!(v.count(), 27);
        let meshes = build_meshes(&v);
        assert_eq!(meshes.len(), 1);
        let m = &meshes[0];
        assert_eq!(m.material, 2);
        check_closed(m, [6.0, 6.0, 6.0]);
        // Flat faces half a voxel outside the full voxels (0..12 studs),
        // as Studio's slab tops: -2..14.
        for k in 0..3 {
            let lo = m.positions.iter().map(|p| p[k]).fold(f32::MAX, f32::min);
            let hi = m.positions.iter().map(|p| p[k]).fold(f32::MIN, f32::max);
            assert!(
                (lo + 2.0).abs() < 1e-4 && (hi - 14.0).abs() < 1e-4,
                "axis {k}: {lo}..{hi}"
            );
        }
        let (cp, ci) = collision_mesh(&meshes);
        assert_eq!((cp.len(), ci.len()), (m.positions.len(), m.indices.len()));
    }

    #[test]
    fn terrain_block_across_chunk_corner_is_welded() {
        // 4x4x4 block straddling the corner shared by 8 chunks. Grass: no
        // deform (Rock's Studio shift, ±0.3 voxel, can fold so small a block).
        let v = Voxels::decode(&encode(&block([-2, -2, -2], [1, 1, 1], (2, 255, None)))).unwrap();
        assert_eq!(v.chunk_count(), 8);
        let meshes = build_meshes(&v);
        assert_eq!(meshes.len(), 1);
        check_closed(&meshes[0], [0.0, 0.0, 0.0]);
    }

    #[test]
    fn studio_cell_hash_matches_the_exe() {
        // Vectors from Studio 0.744's mesher (Studio).
        for (gc, h) in [
            ([0, 0, 0], 0xbbd5_f441u32),
            ([1, 0, 0], 0x8bda_f8ed),
            ([0, 1, 0], 0xca9d_309c),
            ([0, 0, 1], 0x122c_1148),
            ([-1, -1, -1], 0x70fb_a62c),
            ([10, -3, 7], 0x93b6_6c34),
            ([-100, 25, 300], 0x8169_a389),
        ] {
            assert_eq!(studio_cell_hash(gc), h, "{gc:?}");
        }
        assert_eq!(cell_rand([0, 0, 0]), 65);
        assert_eq!(cell_rand([1, 0, 0]), 237);
    }

    #[test]
    fn studio_axis_bands_and_faces() {
        let h = 0.7071;
        for (n, cube, axis) in [
            ([0.0, 1.0, 0.0], false, 8),
            ([0.3, 0.95, 0.0], false, 8),
            ([0.8, 0.6, 0.0], false, 9),
            ([-0.8, 0.6, 0.0], false, 10),
            ([0.0, 0.6, 0.8], false, 11),
            ([0.0, 0.6, -0.8], false, 12),
            ([1.0, 0.0, 0.0], false, 0),
            ([-1.0, 0.0, 0.0], false, 1),
            ([0.0, 0.0, 1.0], false, 2),
            ([0.0, 0.0, -1.0], false, 3),
            ([h, 0.0, h], false, 4),
            ([h, 0.0, -h], false, 5),
            ([-h, 0.0, h], false, 6),
            ([-h, 0.0, -h], false, 7),
            ([0.9, 0.0, 0.3], false, 0),
            ([0.7, -0.7, 0.0], false, 14),
            ([0.0, -0.7, -0.7], false, 17),
            ([0.0, -1.0, 0.0], false, 13),
            ([0.6, 0.8, 0.0], true, 8),
            ([h, 0.0, h], true, 2),
            ([0.9, 0.3, 0.1], true, 0),
            ([0.1, -0.9, 0.1], true, 13),
        ] {
            assert_eq!(studio_axis(n, cube), axis, "{n:?} cube {cube}");
        }
        assert_eq!((0..18).map(face_of_axis).collect::<Vec<_>>(),
            [1, 1, 1, 1, 1, 1, 1, 1, 0, 0, 0, 0, 0, 2, 2, 2, 2, 2]);
        // The U / V tables are unit length but for 0.7 on the diagonals.
        for i in 0..18 {
            for t in [AXIS_U[i], AXIS_V[i]] {
                let l = (t[0] * t[0] + t[1] * t[1] + t[2] * t[2]).sqrt();
                assert!((l - 1.0).abs() < 1e-6 || (l - 0.98995).abs() < 1e-4, "{i}: {l}");
            }
        }
    }

    #[test]
    fn flat_ground_splits_like_studio_and_carries_each_cells_byte() {
        // A one-material slab: every quad splits along q1–q3 (Studio's quad
        // emitter), every top corner is on axis 8 and shows its top texture,
        // and corner k's random byte is the hash of its own cell.
        let v = Voxels::decode(&encode(&block([0, 0, 0], [5, 1, 5], (2, 255, None)))).unwrap();
        let tris = chunk_triangles(&v, [0, 0, 0], &|_| false);
        let mut tops = 0;
        for t in tris.chunks_exact(3) {
            if t.iter().any(|v| v.normal[1] < 120) {
                continue;
            }
            tops += 1;
            for v in t {
                let corner = v.normal[3] as usize;
                assert_eq!(v.axis[corner], 8);
                assert_eq!(v.palette[corner], palette(2, 0));
                // The slab's top lies on its cell's upper face (crossing at
                // (255 + 1) / 256): the cell is the one below that boundary.
                let gc = [0, 1, 2].map(|k| (v.pos[k] / VOXEL - 0.5 - 1e-3).floor() as i32);
                assert_eq!(v.rand[corner], cell_rand(gc), "{:?}", v.pos);
            }
            // The two triangles of a q1–q3 split share the diagonal that runs
            // from (x−1, z) to (x, z−1) on Studio's +Y quad: the corners of a
            // triangle never lie on one row and one column both.
            let p = t.iter().map(|v| (v.pos[0], v.pos[2])).collect::<Vec<_>>();
            let same_x = p.iter().filter(|q| (q.0 - p[0].0).abs() < 1e-3).count();
            let same_z = p.iter().filter(|q| (q.1 - p[0].1).abs() < 1e-3).count();
            assert!(same_x <= 2 && same_z <= 2);
        }
        assert!(tops > 0);
    }

    #[test]
    fn deform_is_one_mode_per_material() {
        let s = |m: u8| crate::terrain_mat::material(m, false).unwrap().shape;
        let rock = s(8);
        assert!(rock.shift > 0.29 && rock.quantize == 0.0 && !rock.cubify && rock.hardsoft);
        let brick = s(5);
        assert!(brick.cubify && brick.shift == 0.0 && brick.quantize == 0.0 && !brick.hardsoft);
        let cobble = s(17);
        assert!(cobble.quantize > 0.14 && cobble.shift == 0.0 && cobble.hardsoft);
        assert!(!s(2).cubify && s(2).shift == 0.0 && s(2).quantize == 0.0);
    }

    #[test]
    fn terrain_two_materials_and_water() {
        let mut cells = block([0, 0, 0], [1, 1, 1], (2, 255, None));
        cells.extend(block([2, 0, 0], [3, 1, 1], (8, 255, None)));
        cells.extend(block([0, 0, 5], [1, 1, 6], (WATER, 255, None)));
        // Water touching solid: its closed surface extends beneath the bank.
        cells.extend(block([4, 0, 0], [4, 1, 1], (WATER, 255, None)));
        let v = Voxels::decode(&encode(&cells)).unwrap();
        let meshes = build_meshes(&v);
        let mats: Vec<u8> = meshes.iter().map(|m| m.material).collect();
        assert_eq!(mats, vec![WATER, 2, 8]);
        let water = &meshes[0];
        assert!(is_water(water.material));
        // Native bank preprocessing permits overlap beneath the rock.
        // The separate pool keeps its own bounds.
        assert!(water
            .positions
            .iter()
            .all(|p| p[0] >= 6.0 - 1e-4 || p[2] >= 18.0 - 1e-4));
        assert!(water.positions.iter().any(|p| p[0] < 14.0 && p[2] < 18.0),
            "touching water must cover the bank intersection");
        let (pos, idx) = collision_mesh(&meshes);
        assert!(!pos.is_empty() && idx.len() % 3 == 0);
    }

    /// Run with: cargo test --release --bin rustudio terrain_perf -- --ignored --nocapture
    #[test]
    #[ignore]
    fn terrain_perf() {
        let mut cells = HashMap::new();
        for x in -256..256 {
            for z in -256..256 {
                let h = 8 + ((x as f32 * 0.05).sin() * 6.0 + (z as f32 * 0.07).cos() * 6.0) as i32;
                for y in -4..h {
                    let top = y == h - 1;
                    cells.insert(
                        [x, y, z],
                        (if top { 2 } else { 8 }, if top { 160 } else { 255 }, None),
                    );
                }
            }
        }
        let bytes = encode(&cells);
        let t = std::time::Instant::now();
        let v = Voxels::decode(&bytes).unwrap();
        let td = t.elapsed();
        let t = std::time::Instant::now();
        let meshes = build_meshes(&v);
        let tm = t.elapsed();
        let tris: usize = meshes.iter().map(|m| m.indices.len() / 3).sum();
        println!(
            "{} voxels, {} bytes: decode {td:?}, mesh {tm:?}, {tris} triangles",
            v.count(),
            bytes.len()
        );
    }
}

#[test]
#[ignore = "reads local evidence under checks/"]
fn grass_native_domain_fixtures(){
 use crate::terrain;
 let base=std::path::Path::new("checks/grass-full-re/domain-fixtures");
 for name in ["flat","slope"] {
 let d=std::fs::read(base.join(format!("{name}-voxels.bin"))).unwrap();let mut v=terrain::Voxels::empty();
 for r in d.chunks_exact(20){let i=|o|i32::from_le_bytes(r[o..o+4].try_into().unwrap());let occ=f32::from_le_bytes(r[16..20].try_into().unwrap());v.set_raw_many([([i(0),i(4),i(8)],(if i(12)==1280 {2}else{0},(occ*256.0-1.0).max(0.0) as u8,0))]);}
 let mut tris=Vec::new();let mut grass=Vec::new();for y in -1..=0 {for x in -1..=0 {for z in -1..=0 {let key=[x,y,z];tris.extend(terrain::chunk_triangles(&v,key,&|_|false));grass.extend(terrain::grass_blades(&v,key,0.7));}}}
 std::fs::write(base.join(format!("{name}-rust-tris.bin")),bytemuck::cast_slice(&tris)).unwrap();std::fs::write(base.join(format!("{name}-rust-grass.bin")),bytemuck::cast_slice(&grass)).unwrap();println!("{name} {} tris {} grass vertices stride {}",tris.len()/3,grass.len(),std::mem::size_of::<terrain::TerrainVertex>());
 assert_eq!(grass.len()/6, if name=="flat" {3694} else {6226});
 let captures: &[(usize,[f32;3],bool)] = if name=="flat" { &[(6,[32.,32.,32.],false),(7,[32.,32.,32.],true)] } else { &[(8,[32.,32.,32.],false),(9,[32.,32.,32.],true),(10,[32.,-32.,32.],true),(11,[32.,-32.,32.],false)] };
 // Compare source to native as well: native allocation contains filler copies.
 for blade in grass.chunks_exact(6) {
  let near = blade[0].axis[3] != 0;
  assert!(captures.iter().filter(|c|c.2==near).any(|&(capture,origin,_)| {
   let native=std::fs::read(base.join(format!("output-{capture}-0.bin"))).unwrap();
   native.chunks_exact(36).any(|n|(0..3).all(|v|(0..3).all(|axis| {
    let p=i16::from_le_bytes(n[v*12+axis*2..v*12+axis*2+2].try_into().unwrap()) as f32/512.0+origin[axis];
    (blade[v].pos[axis]-p).abs()<=2.0/512.0
   })))
  }),"{name}: source blade without native counterpart");
 }
 for &(capture,origin,near) in captures {
  let native=std::fs::read(base.join(format!("output-{capture}-0.bin"))).unwrap();
  for (ordinal,blade) in native.chunks_exact(36).enumerate() {
   let positions: [[f32;3];3] = std::array::from_fn(|v|std::array::from_fn(|axis|i16::from_le_bytes(blade[v*12+axis*2..v*12+axis*2+2].try_into().unwrap()) as f32/512.0+origin[axis]));
   assert!(grass.chunks_exact(6).filter(|v| (v[0].axis[3]!=0)==near).any(|v| (0..3).all(|corner|(0..3).all(|axis|(v[corner].pos[axis]-positions[corner][axis]).abs()<=2.0/512.0))),"{name} capture {capture} blade {ordinal}");
  }
 }

 }
}

#[test]
#[ignore = "retained mixed-material native ownership investigation"]
fn grass_mixed_material_ownership_probe() {
 let base_path=std::env::var("MIXED_PROBE_DIR").unwrap_or_else(|_| "checks/grass-full-re/mixed-material".into());
 let base=std::path::Path::new(&base_path);
 for name in ["sand","flat","slope","transition"] {
  let d=std::fs::read(base.join(format!("{name}-voxels.bin"))).unwrap();
  let mut v=Voxels::empty();
  for r in d.chunks_exact(20) {
   let i=|o|i32::from_le_bytes(r[o..o+4].try_into().unwrap());
   let occ=f32::from_le_bytes(r[16..20].try_into().unwrap());
   let mat=match i(12) {1280=>2,1296=>6,1792|0=>0,m=>panic!("unmapped material {m}")};
   v.set_raw_many([([i(0),i(4),i(8)],(mat,(occ*256.0-1.0).max(0.0) as u8,0))]);
  }
  let mut rows=Vec::new();
  for ky in -1..=0 {for kx in -1..=0 {for kz in -1..=0 {
   let key=[kx,ky,kz];let mut g=Grid::new();g.fill(&v,key);
   for z in 0..P-1 {for y in 0..P-1 {for x in 0..P-1 {
    let c=[x,y,z];let (pos,_)=g.cell_vertex(0,c,key);
    let gc=[0,1,2].map(|a|(key[a]<<CHUNK_SHIFT)+c[a] as i32-1);
    rows.push(serde_json::json!({"pos":pos,"gc":gc,"mat":g.cell_material(c)}));
   }}}
  }}}
  std::fs::write(base.join(format!("{name}-source-cells.json")),serde_json::to_vec(&rows).unwrap()).unwrap();
 }
}

#[test]
#[ignore = "reads local evidence under checks/"]
fn grass_atomic_mixed_boundary_export() {
 let base=std::path::Path::new("checks/grass-full-re/mixed-atomic");

 for name in ["slope","slope-plus","slope-minus"] {
  let d=std::fs::read(base.join(format!("{name}-packed.bin"))).unwrap();
  let mut vox=Voxels::empty();
  vox.set_raw_many(d.chunks_exact(16).map(|r| {
   let c=std::array::from_fn(|a|i32::from_le_bytes(r[a*4..a*4+4].try_into().unwrap()));
   (c,(r[12],r[13],0))
  }));
  let mut domains=Vec::new();
  for y in -1..=0 {for x in -1..=0 {for z in -1..=0 {
   for t in chunk_triangles_impl(&vox,[x,y,z],&|_|false,true).chunks_exact(3) {
    let a=glam::Vec3::from(t[0].pos);let b=glam::Vec3::from(t[1].pos);let c=glam::Vec3::from(t[2].pos);let n=(b-a).cross(c-a);
    if n.length()<0.0002 || n.y/n.length()<0.7 || t[0].palette[..3].iter().all(|p|p/3!=2) {continue;}
    let xyz=[t[0].pos,t[1].pos,t[2].pos];let indicators=std::array::from_fn(|i|if t[0].palette[i]/3==2 {1.0}else{0.0});
    let q14=(crate::grass_stream::material_weight(xyz,indicators)*16384.0) as i32;
    domains.push(serde_json::json!({"xyz":xyz,"q14":q14}));
   }
  }}}
  std::fs::write(base.join(format!("{name}-domains.json")),serde_json::to_vec(&domains).unwrap()).unwrap();
  let mut blades=Vec::new();
  for y in -1..=0 {for x in -1..=0 {for z in -1..=0 {blades.extend(grass_blades(&vox,[x,y,z],0.7));}}}
  std::fs::write(base.join(format!("{name}-rust-grass.bin")),bytemuck::cast_slice(&blades)).unwrap();
  let start=match name {"slope"=>0,"slope-plus"=>4,_=>8};
  let mut native_blades=Vec::new();
  for capture in start..start+4 {
   let origin=[32.0,if matches!(capture,0|1|6|7|10|11) {-32.0}else{32.0},32.0];
   let input=std::fs::read(base.join(format!("input-{capture}.bin"))).unwrap();
   for r in input.chunks_exact(28) {
    let f=|i:usize|i16::from_le_bytes(r[i*2..i*2+2].try_into().unwrap()) as f32/512.0;
    let xyz: [[f32;3];3]=std::array::from_fn(|v|std::array::from_fn(|a|origin[a]+f(a)+if v==0 {0.0}else{f(v*3+a)}));
    let q=u16::from_le_bytes(r[18..20].try_into().unwrap());
    assert!(domains.iter().any(|d| d["q14"].as_u64()==Some(q as u64) && (0..3).all(|v|(0..3).all(|a|(d["xyz"][v][a].as_f64().unwrap() as f32-xyz[v][a]).abs()<=2.0/512.0))),"{name} capture {capture}: native Q14/domain missing {xyz:?} {q}");
   }
   let meta:serde_json::Value=serde_json::from_slice(&std::fs::read(base.join(format!("output-{capture}.json"))).unwrap()).unwrap();
   let object=meta["object"].as_str().unwrap();let sample_count=u32::from_le_bytes(std::array::from_fn(|a|u8::from_str_radix(&object[(76+a)*2..(77+a)*2],16).unwrap()));
   let near=sample_count==16;
   let raw=std::fs::read(base.join(format!("output-{capture}-0.bin"))).unwrap();
   for r in raw.chunks_exact(36) {
    let xyz: [[f32;3];3]=std::array::from_fn(|v|std::array::from_fn(|a|origin[a]+i16::from_le_bytes(r[v*12+a*2..v*12+a*2+2].try_into().unwrap()) as f32/512.0));
    native_blades.push((xyz,near));
   }
  }
  let same=|r:&[TerrainVertex],n:&([[f32;3];3],bool)| (r[0].axis[3]!=0)==n.1 && (0..3).all(|v|(0..3).all(|a|(r[v].pos[a]-n.0[v][a]).abs()<=2.0/512.0));
  for n in &native_blades { assert!(blades.chunks_exact(6).any(|r|same(r,n)),"{name}: native blade missing"); }
  for r in blades.chunks_exact(6) { assert!(native_blades.iter().any(|n|same(r,n)),"{name}: Rust blade missing"); }
  assert_eq!(blades.len()/6,match name {"slope"=>3388,"slope-plus"=>3384,_=>2920});
  println!("atomic {name}: {} blades, stride {}",blades.len()/6,std::mem::size_of::<TerrainVertex>());
 }
}

#[test]
fn grass_nondefault_generation_counts() {
    let mut vox=Voxels::empty();
    for x in -1..=3 {for z in -1..=3 {vox.set([x,0,z],2,1.0);}}
    // 32 native domains, four with Q14=16383; minimum-three acceptance
    // suppresses the regular-batch deficit when allocation is three.
    for (length,density,expected) in [
        (0.1,1.0,32*(26+32)-8), (0.6999,1.0,32*(13+16)-8),
        (0.7,1.0,32*(13+16)-8), (0.7001,1.0,32*(12+15)-8),
        (1.0,1.0,32*(9+11)-8), (0.7,0.0,0),
        (0.7,0.25,32*(3+4)-4), (0.7,0.5,32*(6+8)-8),
    ] {
        assert_eq!(grass_blades_density(&vox,[0,0,0],length,density).len()/6,expected,
            "length {length} density {density}");
    }
}

#[cfg(test)]
mod grass_ordinal_tests {
    use super::*;
    #[test]
    fn allocated_ordinals_survive_filler_omission() {
        let mut vox = Voxels::empty();
        for x in -1..=3 { for z in -1..=3 { vox.set([x,0,z],2,1.0); } }
        for length in [0.1, 0.7, 1.0] {
            let geometry = grass_geometry_density(&vox, [0,0,0], length, 1.0);
            let mut covered = vec![false; geometry.vertices.len()/6];
            for (near, batch) in geometry.batches.iter().enumerate() {
                assert!(batch.allocated >= batch.blades.len());
                assert!(batch.blades.windows(2).all(|p| p[0].0 < p[1].0));
                for &(ordinal, first) in &batch.blades {
                    assert!(ordinal < batch.allocated);
                    assert_eq!(first % 6, 0);
                    assert_eq!(geometry.vertices[first].axis[3] as usize, near);
                    assert!(!covered[first/6]);
                    covered[first/6] = true;
                }
            }
            let mut cell_covered = vec![false; geometry.vertices.len()/6];
            for cell in &geometry.cells {
                assert!(cell.batch.blades.windows(2).all(|p|p[0].0 < p[1].0));
                for &(ordinal, first) in &cell.batch.blades {
                    assert!(ordinal < cell.batch.allocated);
                    cell_covered[first/6] = true;
                    assert_eq!(geometry.vertices[first].axis[3] != 0, cell.near);
                }
            }
            assert_eq!(covered, cell_covered);
            assert!(covered.into_iter().all(|v| v));
        }
    }
}

#[test]
#[ignore = "reads local evidence under checks/"]
fn native_producer_domain_order() {
    let base = std::path::Path::new("checks/grass-full-re/domain-fixtures");
    for (name, captures, mixed) in [("flat", vec![(8, [0,0,0])],false), ("slope", vec![(10,[0,0,0]), (12,[0,-1,0])],false),
        ("slope",vec![(0,[0,-1,0]),(2,[0,0,0])],true),
        ("slope-plus",vec![(4,[0,0,0]),(6,[0,-1,0])],true),
        ("slope-minus",vec![(8,[0,0,0]),(10,[0,-1,0])],true)] {
        let base = if mixed { std::path::Path::new("checks/grass-full-re/mixed-atomic") } else { base };
        let mut voxels = Voxels::empty();
        if mixed {
            let data=std::fs::read(base.join(format!("{name}-packed.bin"))).unwrap();
            voxels.set_raw_many(data.chunks_exact(16).map(|r|(std::array::from_fn(|a|i32::from_le_bytes(r[a*4..a*4+4].try_into().unwrap())),(r[12],r[13],0))));
        } else { for r in std::fs::read(base.join(format!("{name}-voxels.bin"))).unwrap().chunks_exact(20) {
            let i = |o| i32::from_le_bytes(r[o..o+4].try_into().unwrap());
            let occ = f32::from_le_bytes(r[16..20].try_into().unwrap());
            voxels.set_raw_many([([i(0),i(4),i(8)],(if i(12)==1280 {2}else{0},(occ*256.0-1.0).max(0.0) as u8,0))]);
        }
        }
        for (capture, region) in captures {
            let mut domains = Vec::new();
            for y in -1..=0 { for z in -1..=0 { for x in -1..=0 {
                let tris = chunk_triangles_impl(&voxels,[x,y,z],&|_|false,true);
                for t in tris.chunks_exact(3) {
                    let n = (glam::Vec3::from(t[1].pos)-glam::Vec3::from(t[0].pos)).cross(glam::Vec3::from(t[2].pos)-glam::Vec3::from(t[0].pos));
                    if n.length()<0.0002 || n.y/n.length()<0.7 || t[0].palette[..3].iter().all(|p|p/3!=2) {continue;}
                    if std::array::from_fn::<_,3,_>(|a|(t[0].wind_local[a]/64.0).floor() as i32)!=region {continue;}
                    domains.push(t.iter().map(|v|v.pos).collect::<Vec<_>>());
                }
            }}}
            let seed=(region[0] as u32).wrapping_mul(0x466f45d) ^ (region[1] as u32).wrapping_mul(0x127409f) ^ (region[2] as u32).wrapping_mul(0x4f9ffb7);
            let order=crate::grass_stream::Stream::from_seed(seed as u64).shuffled_slots(domains.len());
            let native=std::fs::read(base.join(format!("input-{capture}.bin"))).unwrap();
            assert_eq!(domains.len(),native.len()/28);
            for (ordinal,r) in native.chunks_exact(28).enumerate() {
                let word=|i|i16::from_le_bytes(r[i*2..i*2+2].try_into().unwrap()) as f32/512.0;
                for v in 0..3 {for a in 0..3 {
                    let expected=word(a)+if v==0 {0.0}else{word(v*3+a)}+region[a] as f32*64.0+32.0;
                    assert!((domains[order[ordinal]][v][a]-expected).abs()<=2.0/512.0,"{name} mixed={mixed} region {region:?} ordinal {ordinal}: source {:?}, expected {expected}",domains[order[ordinal]]);
                }}
            }
        }
    }
}

#[test]
#[ignore = "explicit same-input X/Z boundary source inventory"]
fn square_boundary_source_inventory() {
 let base=std::path::Path::new("checks/grass-square-boundaries/runtime");
 for name in ["flat","slope"] {
  let mut vox=Voxels::empty();
  for r in std::fs::read(base.join(format!("{name}-voxels.bin"))).unwrap().chunks_exact(20) {
   let i=|o|i32::from_le_bytes(r[o..o+4].try_into().unwrap());
   let occ=f32::from_le_bytes(r[16..20].try_into().unwrap());
   vox.set_raw_many([([i(0),i(4),i(8)],(if i(12)==1280 {2}else{0},(occ*256.0-1.0).max(0.0) as u8,0))]);
  }
  let mut rows=Vec::new();
  for key in dirty_chunks(None,&vox) {
   for t in chunk_triangles_impl(&vox,key,&|_|false,true).chunks_exact(3) {
    let n=(glam::Vec3::from(t[1].pos)-glam::Vec3::from(t[0].pos)).cross(glam::Vec3::from(t[2].pos)-glam::Vec3::from(t[0].pos));
    if n.length()<0.0002 || n.y/n.length()<0.7 || t[0].palette[..3].iter().all(|p|p/3!=2) {continue;}
    let region=std::array::from_fn::<_,3,_>(|a|(t[0].wind_local[a]/64.0).floor() as i32);
    rows.push(serde_json::json!({"region":region,"xyz":[t[0].pos,t[1].pos,t[2].pos],"palette":t[0].palette,"storage":key}));
   }
  }
  // Compare per-region multisets, retaining duplicate native assignments.
  let native: Vec<serde_json::Value> = serde_json::from_slice(&std::fs::read(base.join(format!("{name}-native-source.json"))).unwrap()).unwrap();
  let inventory = |data: &[serde_json::Value]| {
   let mut result = std::collections::BTreeMap::new();
   for row in data {
    let region: [i32; 3] = std::array::from_fn(|a| row["region"][a].as_i64().unwrap() as i32);
    let mut triangle: [[i32; 3]; 3] = std::array::from_fn(|v| std::array::from_fn(|a| (row["xyz"][v][a].as_f64().unwrap()*512.0).round() as i32));
    triangle.sort();
    *result.entry((region, triangle)).or_insert(0usize) += 1;
   }
   result
  };
  assert_eq!(inventory(&rows), inventory(&native), "native region ownership: {name}");
  std::fs::write(base.join(format!("{name}-rust-source.json")),serde_json::to_vec(&rows).unwrap()).unwrap();
 }
}

#[test]
#[ignore = "explicit CPU production Village coverage export"]
fn village_coverage_export() {
 let path=format!("{}/.cache/rustudio/templates/520390648/v63.rbxl",std::env::var("HOME").unwrap());
 let dom=rbx_binary::from_reader(std::fs::File::open(&path).unwrap()).unwrap();
 let v=Voxels::from_dom(&dom).unwrap().unwrap();
 let terrain=dom.root().children().iter().filter_map(|r|dom.get_by_ref(*r)).find(|i|i.class=="Workspace").unwrap().children().iter().filter_map(|r|dom.get_by_ref(*r)).find(|i|i.class=="Terrain").unwrap();
 let length=match terrain.properties.get(&rbx_dom_weak::ustr("GrassLength")) {Some(rbx_types::Variant::Float32(x))=>*x,_=>crate::mech::GRASS_LENGTH_DEFAULT};
 let mut keys=dirty_chunks(None,&v); keys.sort();
 for key in keys { let lo=glam::Vec3::from_array(key.map(|x|x as f32*128.0)); let cam=glam::Vec3::new(-12.0,155.0,210.0); if cam.distance(cam.clamp(lo,lo+glam::Vec3::splat(128.0)))<=crate::mech::GRASS_RADIUS {grass_geometry_density(&v,key,length,1.0);} }
}


/// Blade orientation `i` of 1024: a horizontal base axis at a random heading
/// and a unit tip axis leaning 10°–40° off vertical across it.
fn grass_template(i: usize) -> (glam::Vec3, glam::Vec3) {
    let mut z = (i as u64).wrapping_add(0x9e37_79b9_7f4a_7c15);
    let mut next = || {
        z = z.wrapping_add(0x9e37_79b9_7f4a_7c15);
        let mut x = z;
        x = (x ^ (x >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
        x = (x ^ (x >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
        ((x ^ (x >> 31)) >> 40) as f32 / (1u64 << 24) as f32
    };
    let heading = next() * std::f32::consts::TAU;
    let base = glam::Vec3::new(heading.cos(), 0.0, heading.sin());
    let lean = (10.0 + 30.0 * next()).to_radians() * if next() < 0.5 { -1.0 } else { 1.0 };
    let across = glam::Vec3::new(-heading.sin(), 0.0, heading.cos());
    (base, glam::Vec3::Y * lean.cos() + across * lean.sin())
}
