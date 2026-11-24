//! Web API helpers for the Asset Manager: universe places, user/group
//! inventories and creations, single-asset lookup, and file upload.

use std::collections::{HashMap, HashSet};
use std::path::Path;

use rustudio_account::cloud::{CloudApi, CloudPlace, Endpoints};
use rustudio_account::session::Session;

use super::{AssetEntry, AssetKind, ImportItem, ImportStatus};
use crate::toolbox::api::{self as tb_api, Failure, Filters, Http, ListReq, Source};

/// Fetches all places belonging to `universe_id`.
pub fn fetch_places(session: &Session, universe_id: u64) -> Result<Vec<CloudPlace>, String> {
    let api = CloudApi { session: session.downloader(), ep: Endpoints::default() };
    api.places(universe_id).map_err(|e| e.0)
}

/// Fetches the first page of a user's or group's creations + inventory across
/// the core Asset Manager asset kinds.
pub fn fetch_inventory_first_page(
    h: &dyn Http,
    group_id: Option<u64>,
    cancel: &dyn Fn() -> bool,
) -> (Vec<AssetEntry>, HashMap<(AssetKind, bool), Option<String>>, Option<Failure>) {
    let mut items = Vec::new();
    let mut seen = HashSet::new();
    let mut cursors = HashMap::new();
    let mut first_err: Option<Failure> = None;

    for &kind in &AssetKind::INVENTORY_KINDS {
        let Some(tb_kind) = kind.to_toolbox_kind() else { continue };
        for is_creation in [true, false] {
            if cancel() {
                return (items, cursors, Some(Failure::Cancelled));
            }
            let source = if is_creation {
                Source::Creations { group: group_id }
            } else {
                Source::Inventory { group: group_id }
            };
            let req = ListReq {
                source,
                kind: tb_kind,
                query: String::new(),
                category_path: None,
                sub_type: None,
                filters: Filters::default(),
            };
            match tb_api::fetch_page(h, &req, None, cancel) {
                Ok(page) => {
                    cursors.insert((kind, is_creation), page.next);
                    for a in &page.assets {
                        if seen.insert(a.id) {
                            items.push(AssetEntry::from_toolbox_asset(a, kind, is_creation));
                        }
                    }
                }
                Err(Failure::SignedOut) => return (items, cursors, Some(Failure::SignedOut)),
                Err(Failure::Cancelled) => return (items, cursors, Some(Failure::Cancelled)),
                Err(e) => {
                    if first_err.is_none() {
                        first_err = Some(e);
                    }
                }
            }
        }
    }

    let err = if items.is_empty() { first_err } else { None };
    (items, cursors, err)
}

/// Fetches the next page for any `(kind, is_creation)` pair that still has a cursor.
pub fn fetch_inventory_next_page(
    h: &dyn Http,
    group_id: Option<u64>,
    prev_cursors: &HashMap<(AssetKind, bool), Option<String>>,
    cancel: &dyn Fn() -> bool,
) -> (Vec<AssetEntry>, HashMap<(AssetKind, bool), Option<String>>, Option<Failure>) {
    let mut items = Vec::new();
    let mut seen = HashSet::new();
    let mut next_cursors = prev_cursors.clone();
    let mut first_err = None;

    for (&(kind, is_creation), cursor) in prev_cursors {
        let Some(cur) = cursor.as_deref() else { continue };
        let Some(tb_kind) = kind.to_toolbox_kind() else { continue };
        if cancel() {
            return (items, next_cursors, Some(Failure::Cancelled));
        }
        let source = if is_creation {
            Source::Creations { group: group_id }
        } else {
            Source::Inventory { group: group_id }
        };
        let req = ListReq {
            source,
            kind: tb_kind,
            query: String::new(),
            category_path: None,
            sub_type: None,
            filters: Filters::default(),
        };
        match tb_api::fetch_page(h, &req, Some(cur), cancel) {
            Ok(page) => {
                next_cursors.insert((kind, is_creation), page.next);
                for a in &page.assets {
                    if seen.insert(a.id) {
                        items.push(AssetEntry::from_toolbox_asset(a, kind, is_creation));
                    }
                }
            }
            Err(e) => {
                next_cursors.insert((kind, is_creation), None);
                if first_err.is_none() {
                    first_err = Some(e);
                }
            }
        }
    }

    let err = if items.is_empty() { first_err } else { None };
    (items, next_cursors, err)
}

/// Looks up a single asset by numeric ID.
pub fn lookup_asset(h: &dyn Http, id: u64) -> Result<Option<AssetEntry>, Failure> {
    let list = tb_api::fetch_details(h, &[id], &|| false)?;
    Ok(list.into_iter().next().map(|a| {
        let kind = AssetKind::from_type_id(a.type_id);
        AssetEntry::from_toolbox_asset(&a, kind, false)
    }))
}

/// Inspects a local file path and prepares an `ImportItem` with Roblox asset
/// type and MIME type.
pub fn classify_import_file(path: &Path) -> Option<ImportItem> {
    let ext = path.extension()?.to_str()?.to_ascii_lowercase();
    let (asset_type, kind, mime): (&'static str, AssetKind, &'static str) = match ext.as_str() {
        "png" => ("Decal", AssetKind::Image, "image/png"),
        "jpg" | "jpeg" => ("Decal", AssetKind::Image, "image/jpeg"),
        "bmp" => ("Decal", AssetKind::Image, "image/bmp"),
        "tga" => ("Decal", AssetKind::Image, "image/tga"),
        "mp3" => ("Audio", AssetKind::Audio, "audio/mpeg"),
        "ogg" => ("Audio", AssetKind::Audio, "audio/ogg"),
        "wav" => ("Audio", AssetKind::Audio, "audio/wav"),
        "flac" => ("Audio", AssetKind::Audio, "audio/flac"),
        "rbxm" => ("Model", AssetKind::Model, "model/x-rbxm"),
        "rbxmx" => ("Model", AssetKind::Model, "model/x-rbxmx"),
        "fbx" => ("Model", AssetKind::MeshPart, "model/fbx"),
        "obj" => ("Model", AssetKind::MeshPart, "model/obj"),
        "gltf" => ("Model", AssetKind::MeshPart, "model/gltf+json"),
        "glb" => ("Model", AssetKind::MeshPart, "model/gltf-binary"),
        _ => return None,
    };
    let stem = path
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("Asset")
        .to_string();
    let size_bytes = std::fs::metadata(path).map(|m| m.len()).unwrap_or(0);
    Some(ImportItem {
        path: path.to_path_buf(),
        display_name: stem,
        description: String::new(),
        asset_type,
        kind,
        mime,
        size_bytes,
        status: ImportStatus::Pending,
    })
}

/// Uploads one `ImportItem` to Roblox using the signed-in session.
pub fn upload_import_item(
    session: &Session,
    item: &ImportItem,
    group_id: Option<u64>,
) -> Result<u64, String> {
    let bytes = std::fs::read(&item.path).map_err(|e| format!("Could not read file: {e}"))?;
    let filename = item
        .path
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or("asset");
    let api = CloudApi { session: session.downloader(), ep: Endpoints::default() };
    api.upload_asset(
        item.asset_type,
        &item.display_name,
        &item.description,
        group_id,
        filename,
        item.mime,
        &bytes,
    )
    .map_err(|e| e.0)
}
