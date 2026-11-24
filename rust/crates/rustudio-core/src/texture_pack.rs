//! A material's texture pack: the child image ids it names (colour, normal,
//! roughness, metalness, height, emissive), read from a Studio texture pack
//! XML, and the packs registered for a MaterialVariant's own loose maps
//! (`terrain_mat.rs`, `scene.rs`). `matpack.rs` (render) loads them.

use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};

/// The child image ids a texture pack XML names.
#[derive(Debug, Default, Clone, PartialEq)]
pub struct Pack {
    pub tiling: f32,
    pub color: Option<u64>,
    pub normal: Option<u64>,
    pub roughness: Option<u64>,
    pub metalness: Option<u64>,
    pub height: Option<u64>,
    pub emissive: Option<u64>,
}

pub fn parse_pack(xml: &str) -> Result<Pack, String> {
    if !xml.contains("<roblox") || !xml.contains("texturepack_version") {
        return Err("not a texture pack".into());
    }
    let field = |tag: &str| -> Option<&str> {
        let open = xml.find(&format!("<{tag}"))?;
        let start = open + xml[open..].find('>')? + 1;
        let end = start + xml[start..].find(&format!("</{tag}>"))?;
        Some(xml[start..end].trim())
    };
    let id = |tag: &str| field(tag).and_then(|v| v.parse::<u64>().ok());
    Ok(Pack {
        tiling: field("tiling").and_then(|v| v.parse().ok()).unwrap_or(1.0),
        color: id("color"),
        normal: id("normal"),
        roughness: id("roughness"),
        metalness: id("metalness"),
        height: id("height"),
        emissive: id("emissive"),
    })
}

/// Key bit: the pack's maps built from its KTX2 representations
/// (`matpack::build_acr`).
pub const ACR_BIT: u64 = 1 << 61;

fn variant_packs() -> &'static Mutex<HashMap<u64, Pack>> {
    static PACKS: OnceLock<Mutex<HashMap<u64, Pack>>> = OnceLock::new();
    PACKS.get_or_init(Default::default)
}

/// A registered pack (see `register_terrain_variant`).
pub fn variant_pack(pack: u64) -> Option<Pack> {
    variant_packs().lock().expect("variant packs").get(&pack).cloned()
}

/// A pack id from `register_terrain_variant` (a MaterialVariant's maps).
pub fn is_variant_pack(pack: u64) -> bool {
    pack & (1u64 << 62) != 0 && pack & ACR_BIT == 0
}

/// Route authored terrain maps through the regular asynchronous pack loader.
pub fn register_terrain_variant(pack: Pack) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    [pack.color, pack.normal, pack.roughness, pack.metalness, pack.height].hash(&mut h);
    // Bit 62 marks a variant; the hash must stay below ACR_BIT (bit 61), or
    // `build` takes the variant for a Studio KTX2 pack, the KTX2 lookup 404s
    // and its fallback asks for the id without bit 61, which nothing
    // registered (Laser Tag's Walkway / MossGrass / Moss drew flat white).
    let id = (h.finish() & (ACR_BIT - 1)) | (1u64 << 62);
    variant_packs().lock().expect("variant packs").entry(id).or_insert(pack);
    id
}
