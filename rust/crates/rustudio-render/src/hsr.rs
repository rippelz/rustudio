//! Hidden surface removal under layered clothing, as Studio does it
//! (checks/mesh-deform/AUDIT.md (d), measured 2026-11-14 on Studio 0.747):
//!
//! - It is not occlusion: with the jacket fully transparent the arms and
//!   most of the upper torso are still not drawn. It applies in Edit, after
//!   CreateHumanoidModelFromDescription, Humanoid:AddAccessory, re-parenting
//!   the accessory or toggling its WrapLayer.
//! - It comes from data Roblox computes for each asset: every WrapLayer and
//!   WrapTarget names an HSR asset (HSRAssetId, not readable from scripts),
//!   a model holding one binary blob plus the mesh ids it was computed for.
//!   A mesh that isn't the listed one gets no HSR (the same jacket mesh
//!   bytes loaded from a local file hide nothing).
//! - The blob (`parse`): a version byte; u32 cage vertex count, u32 face
//!   count, u32 list length; one bit per *inner* cage vertex (set = not
//!   covered by this garment; the 0.744 build's strings describe it as
//!   sampled with rays per face); from version 17 a u32; per face of the
//!   render mesh (all LODs) a u16 offset and u16 count into a list of u16
//!   cage vertex indices (the cage triangles the face lies over); then per
//!   face data and a hash, not needed here.
//! - A body face is dropped when every cage vertex its list names is
//!   covered by a garment (cages pair by UV, as for fitting). Against
//!   Studio's transparent-jacket picture this gives the same body (IoU 0.94,
//!   differences a few pixels along the torso strip's edges).
//!
//! The body part is then drawn from a copy of its mesh without those faces
//! (a generated `rbxtemp://rustudio/hsr/…` mesh with the same bounds).

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex, OnceLock};

use glam::{Vec2, Vec3};
use rbx_dom_weak::types::{Ref, Variant};

use rustudio_core::doc::Document;
use crate::scene::content_string;

/// An HSR asset's blob, parsed.
pub struct Hsr {
    /// Per cage vertex: covered by this garment (empty for body parts).
    pub covered: Vec<bool>,
    /// Per render-mesh face: (offset, count) into `list`.
    pub faces: Vec<(u16, u16)>,
    pub list: Vec<u16>,
    /// The mesh ids it was computed for (render mesh first).
    pub mesh_ids: Vec<u64>,
}

fn u32_at(b: &[u8], o: usize) -> Option<u32> {
    Some(u32::from_le_bytes(b.get(o..o + 4)?.try_into().ok()?))
}

fn u16_at(b: &[u8], o: usize) -> Option<u16> {
    Some(u16::from_le_bytes(b.get(o..o + 2)?.try_into().ok()?))
}

/// The blob (see the module notes) and the mesh id list
/// ("\0\n<id>\n<id>…" then a hash).
pub fn parse(blob: &[u8], ids: &[u8]) -> Option<Hsr> {
    let ver = *blob.first()?;
    let (n1, n2, n3) = (u32_at(blob, 1)? as usize, u32_at(blob, 5)? as usize, u32_at(blob, 9)? as usize);
    let mut o = 13;
    let bits = blob.get(o..o + n1.div_ceil(8))?;
    o += n1.div_ceil(8);
    if ver >= 17 {
        o += 4;
    }
    let mut faces = Vec::with_capacity(n2);
    for k in 0..n2 {
        faces.push((u16_at(blob, o + 4 * k)?, u16_at(blob, o + 4 * k + 2)?));
    }
    o += 4 * n2;
    let mut list = Vec::with_capacity(n3);
    for k in 0..n3 {
        list.push(u16_at(blob, o + 2 * k)?);
    }
    let covered = (0..n1).map(|k| (bits[k / 8] >> (k % 8)) & 1 == 0).collect();
    let text = String::from_utf8_lossy(ids);
    let mesh_ids = text
        .split('\n')
        .filter_map(|s| {
            let d: String = s.chars().take_while(|c| c.is_ascii_digit()).collect();
            d.parse().ok()
        })
        .collect();
    Some(Hsr { covered, faces, list, mesh_ids })
}

/// The numeric asset id of a content string (rbxassetid://N, …?id=N).
pub fn asset_number(id: &str) -> Option<u64> {
    let tail = id.rsplit(|c: char| c == '=' || c == '/').next()?;
    let d: String = tail.chars().take_while(|c| c.is_ascii_digit()).collect();
    d.parse().ok()
}

/// A mesh's vertex uvs in file order (versions 2–5; None for others).
pub fn raw_uvs(bytes: &[u8]) -> Option<Vec<Vec2>> {
    let nl = bytes.iter().position(|b| *b == b'\n')?;
    let ver = std::str::from_utf8(bytes.get(8..nl)?).ok()?.trim().to_string();
    let b = &bytes[nl + 1..];
    let (start, n, stride) = if ver.starts_with('2') {
        (u16_at(b, 0)? as usize, u32_at(b, 4)? as usize, *b.get(2)? as usize)
    } else if ver.starts_with('3') {
        (u16_at(b, 0)? as usize, u32_at(b, 8)? as usize, *b.get(2)? as usize)
    } else if ver.starts_with('4') || ver.starts_with('5') {
        (u16_at(b, 0)? as usize, u32_at(b, 4)? as usize, 40)
    } else {
        return None;
    };
    (0..n)
        .map(|i| {
            let r = start + i * stride;
            let f = |k: usize| u32_at(b, r + k).map(f32::from_bits);
            Some(Vec2::new(f(24)?, f(28)?))
        })
        .collect()
}

fn uv_key(uv: Vec2) -> (i64, i64) {
    ((uv.x * 1e4).round() as i64, (uv.y * 1e4).round() as i64)
}

/// An HSR asset by id: downloaded (public), its HiddenSurfaceRemovalAsset's
/// blob parsed.
fn load(id: u64) -> Result<Arc<Hsr>, String> {
    static CACHE: OnceLock<Mutex<HashMap<u64, Arc<Hsr>>>> = OnceLock::new();
    if let Some(h) = CACHE.get_or_init(Default::default).lock().ok().and_then(|c| c.get(&id).cloned()) {
        return Ok(h);
    }
    let bytes = rustudio_core::assets::public_cached(id)?;
    let dom = Document::decode(&bytes)?;
    let inst = dom
        .descendants()
        .find(|i| i.class == "HiddenSurfaceRemovalAsset")
        .ok_or_else(|| format!("HSR {id}: no HiddenSurfaceRemovalAsset"))?;
    let blob = |k: &str| -> Vec<u8> {
        match inst.properties.iter().find(|(n, _)| n.as_str() == k).map(|(_, v)| v) {
            Some(Variant::BinaryString(b)) => AsRef::<[u8]>::as_ref(b).to_vec(),
            Some(Variant::String(s)) => s.as_bytes().to_vec(),
            _ => Vec::new(),
        }
    };
    let h = Arc::new(parse(&blob("HSRData"), &blob("HSRMeshIdData")).ok_or_else(|| format!("HSR {id}: unreadable data"))?);
    if let Ok(mut c) = CACHE.get_or_init(Default::default).lock() {
        c.insert(id, h.clone());
    }
    Ok(h)
}

/// One garment's part in a body part's HSR.
#[derive(Clone)]
struct Garment {
    hsr: u64,
    mesh: Option<u64>,
    inner: String,
}

#[derive(Clone)]
enum State {
    Pending,
    Ready(Option<String>),
}

fn states() -> &'static Mutex<HashMap<String, State>> {
    static S: OnceLock<Mutex<HashMap<String, State>>> = OnceLock::new();
    S.get_or_init(Default::default)
}

pub enum Look {
    /// Nothing hidden (no garment, no HSR data, or it doesn't apply).
    None,
    Pending,
    /// Draw this mesh id instead.
    Ready(String),
}

fn child_of_class(doc: &Document, parent: Ref, class: &str) -> Option<Ref> {
    doc.dom
        .get_by_ref(parent)?
        .children()
        .iter()
        .copied()
        .find(|c| doc.class_of(*c).is_some_and(|k| k == class))
}

fn content(doc: &Document, r: Ref, keys: &[&str]) -> Option<String> {
    keys.iter().find_map(|k| content_string(doc.prop(r, k))).filter(|s| !s.is_empty())
}

/// What a body MeshPart (`part`, mesh `mesh_id`) is drawn with under its
/// character's layered clothing.
pub fn body_mesh(doc: &Document, part: Ref, mesh_id: &str) -> Look {
    if std::env::var_os("RUSTUDIO_NO_HSR").is_some() {
        return Look::None;
    }
    let Some(wt) = child_of_class(doc, part, "WrapTarget") else { return Look::None };
    let Some(body_hsr) = content(doc, wt, &["HSRContent", "HSRAssetId"]).and_then(|s| asset_number(&s)) else { return Look::None };
    let Some(cage) = content(doc, wt, &["CageMeshContent", "CageMeshId"]) else { return Look::None };
    let Some(model) = doc.dom.get_by_ref(part).map(|i| i.parent()) else { return Look::None };
    let mut garments = Vec::new();
    for c in doc.dom.get_by_ref(model).map(|m| m.children().to_vec()).unwrap_or_default() {
        if !doc.class_of(c).is_some_and(|k| rustudio_core::doc::is_a(&k, "Accoutrement")) {
            continue;
        }
        let Some(h) = rustudio_core::default_scripts::child_named(doc, c, "Handle") else { continue };
        if !crate::wrap::is_layered_handle(doc, h) {
            continue;
        }
        let Some(w) = child_of_class(doc, h, "WrapLayer") else { continue };
        let (Some(hsr), Some(inner)) = (
            content(doc, w, &["HSRContent", "HSRAssetId"]).and_then(|s| asset_number(&s)),
            content(doc, w, &["ReferenceMeshContent", "ReferenceMeshId"]),
        ) else {
            continue;
        };
        let mesh = content(doc, h, &["MeshContent", "MeshId"]).and_then(|s| asset_number(&s));
        garments.push(Garment { hsr, mesh, inner });
    }
    if garments.is_empty() {
        return Look::None;
    }
    let key = format!(
        "{mesh_id}|{body_hsr}|{cage}|{}",
        garments.iter().map(|g| format!("{}:{:?}:{}", g.hsr, g.mesh, g.inner)).collect::<Vec<_>>().join(",")
    );
    let mut s = states().lock().unwrap_or_else(|e| e.into_inner());
    match s.get(&key) {
        Some(State::Ready(Some(id))) => return Look::Ready(id.clone()),
        Some(State::Ready(None)) => return Look::None,
        Some(State::Pending) => return Look::Pending,
        None => {}
    }
    s.insert(key.clone(), State::Pending);
    drop(s);
    let mesh_id = mesh_id.to_string();
    std::thread::Builder::new()
        .name("hsr".into())
        .spawn(move || {
            let out = match hidden_mesh(&mesh_id, body_hsr, &cage, &garments) {
                Ok(v) => v,
                Err(e) => {
                    if std::env::var_os("RUSTUDIO_HSR_TRACE").is_some() {
                        eprintln!("hsr: {e}");
                    }
                    None
                }
            };
            states().lock().unwrap_or_else(|e| e.into_inner()).insert(key, State::Ready(out));
            crate::wrap::bump_generation();
        })
        .ok();
    Look::Pending
}

/// The faces of a body mesh its garments hide (indices of the render
/// mesh's faces), or None when the HSR data doesn't apply.
pub fn hidden_faces(body: &Hsr, mesh: u64, body_cage_uvs: &[Vec2], covered: &HashSet<(i64, i64)>) -> Option<Vec<bool>> {
    if body.mesh_ids.first() != Some(&mesh) {
        return None;
    }
    Some(
        body.faces
            .iter()
            .map(|(off, cnt)| {
                let (off, cnt) = (*off as usize, *cnt as usize);
                cnt > 0
                    && body.list.get(off..off + cnt).is_some_and(|l| {
                        l.iter().all(|v| body_cage_uvs.get(*v as usize).is_some_and(|uv| covered.contains(&uv_key(*uv))))
                    })
            })
            .collect(),
    )
}

fn hidden_mesh(mesh_id: &str, body_hsr: u64, cage: &str, garments: &[Garment]) -> Result<Option<String>, String> {
    let mesh_no = asset_number(mesh_id).ok_or("no mesh id")?;
    let body = load(body_hsr)?;
    let cage_uvs = raw_uvs(&rustudio_core::assets::content_blocking(cage)?).ok_or("cage format")?;
    let mut covered = HashSet::new();
    for g in garments {
        let Ok(h) = load(g.hsr) else { continue };
        // The data is for the garment's own mesh only.
        if g.mesh.is_none() || h.mesh_ids.first() != g.mesh.as_ref() {
            continue;
        }
        let Some(uvs) = rustudio_core::assets::content_blocking(&g.inner).ok().and_then(|b| raw_uvs(&b)) else { continue };
        for (k, c) in h.covered.iter().enumerate() {
            if *c {
                if let Some(uv) = uvs.get(k) {
                    covered.insert(uv_key(*uv));
                }
            }
        }
    }
    let Some(hide) = hidden_faces(&body, mesh_no, &cage_uvs, &covered) else { return Ok(None) };
    if !hide.iter().any(|h| *h) {
        return Ok(None);
    }
    let bytes = rustudio_core::assets::content_blocking(mesh_id)?;
    let m = rustudio_core::mesh::parse(&bytes)?;
    if m.skin.is_some() {
        // (skinned body meshes keep every face: the skinning pass reads
        // the mesh by its own id)
        return Ok(None);
    }
    let tris = m.positions.len() / 3;
    let keep: Vec<usize> = (0..tris).filter(|t| !hide.get(*t).copied().unwrap_or(false)).collect();
    // A copy without the hidden faces, plus a zero-area triangle across the
    // original bounds so it stretches to the part the same way.
    let n = keep.len() * 3 + 3;
    let mut b = Vec::with_capacity(32 + n * 36 + (n / 3) * 12);
    b.extend_from_slice(b"version 2.00\n");
    b.extend_from_slice(&12u16.to_le_bytes());
    b.push(36);
    b.push(12);
    b.extend_from_slice(&(n as u32).to_le_bytes());
    b.extend_from_slice(&((n / 3) as u32).to_le_bytes());
    let mut vert = |p: Vec3, nn: Vec3, uv: Vec2| {
        for f in [p.x, p.y, p.z, nn.x, nn.y, nn.z, uv.x, uv.y, 0.0] {
            b.extend_from_slice(&f.to_le_bytes());
        }
    };
    for t in &keep {
        for k in 0..3 {
            let i = t * 3 + k;
            vert(m.positions[i], m.normals.get(i).copied().unwrap_or(Vec3::Y), m.uvs.get(i).copied().unwrap_or_default());
        }
    }
    vert(m.min, Vec3::Y, Vec2::ZERO);
    vert(m.max, Vec3::Y, Vec2::ZERO);
    vert(m.max, Vec3::Y, Vec2::ZERO);
    for t in 0..n / 3 {
        for k in 0..3 {
            b.extend_from_slice(&((t * 3 + k) as u32).to_le_bytes());
        }
    }
    let hash = blake3::hash(&b).to_hex().to_string();
    let id = format!("rbxtemp://rustudio/hsr/{}", &hash[..32]);
    rustudio_core::assets::register_generated(&id, b);
    Ok(Some(id))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_both_versions() {
        // version 16: 9 cage vertices (2 bytes of bits), 2 faces, 3 list entries
        let mut b = vec![16u8];
        for v in [9u32, 2, 3] {
            b.extend_from_slice(&v.to_le_bytes());
        }
        b.extend_from_slice(&[0b1111_1110, 0b1]);
        for (o, c) in [(0u16, 2u16), (2, 1)] {
            b.extend_from_slice(&o.to_le_bytes());
            b.extend_from_slice(&c.to_le_bytes());
        }
        for v in [4u16, 5, 6] {
            b.extend_from_slice(&v.to_le_bytes());
        }
        let h = parse(&b, b"\0\n111\n222\n333abc").unwrap();
        assert_eq!(h.covered[0], true);
        assert_eq!(h.covered[1], false);
        assert_eq!(h.covered[8], false);
        assert_eq!(h.faces, vec![(0, 2), (2, 1)]);
        assert_eq!(h.list, vec![4, 5, 6]);
        assert_eq!(h.mesh_ids, vec![111, 222, 333]);
        // version 18: no bits, a u32 before the table
        let mut b = vec![18u8];
        for v in [0u32, 1, 2] {
            b.extend_from_slice(&v.to_le_bytes());
        }
        b.extend_from_slice(&1u32.to_le_bytes());
        b.extend_from_slice(&0u16.to_le_bytes());
        b.extend_from_slice(&2u16.to_le_bytes());
        b.extend_from_slice(&7u16.to_le_bytes());
        b.extend_from_slice(&8u16.to_le_bytes());
        let h = parse(&b, b"\0\n9").unwrap();
        assert!(h.covered.is_empty());
        assert_eq!(h.list, vec![7, 8]);
    }

    #[test]
    fn a_face_goes_when_all_its_cage_vertices_are_covered() {
        let body = Hsr { covered: vec![], faces: vec![(0, 2), (2, 2), (4, 0)], list: vec![0, 1, 1, 2], mesh_ids: vec![5] };
        let uvs = [Vec2::new(0.1, 0.1), Vec2::new(0.2, 0.2), Vec2::new(0.3, 0.3)];
        let covered: HashSet<_> = [uv_key(uvs[0]), uv_key(uvs[1])].into_iter().collect();
        assert_eq!(hidden_faces(&body, 5, &uvs, &covered), Some(vec![true, false, false]));
        // another mesh than the data was made for: no HSR
        assert_eq!(hidden_faces(&body, 6, &uvs, &covered), None);
    }
}

#[cfg(test)]
mod probe {
    /// `HSR_PROBE=file.rbxm cargo test hsr::probe -- --ignored --nocapture`
    #[test]
    #[ignore]
    fn wrap_props() {
        let Ok(p) = std::env::var("HSR_PROBE") else { return };
        let dom = rustudio_core::doc::Document::decode(&std::fs::read(p).unwrap()).unwrap();
        for i in dom.descendants() {
            if i.class == "WrapTarget" || i.class == "WrapLayer" {
                let mut k: Vec<String> = i.properties.iter().map(|(n, v)| format!("{n}={v:?}")).collect();
                k.sort();
                println!("{} {}: {}", i.class, i.name, k.join(" | ").chars().take(900).collect::<String>());
                break;
            }
        }
    }
}
