//! Plugins over the signed-in session, the calls Studio's plugin manager,
//! Toolbox and "Publish as Plugin" make (endpoints read from the Windows
//! build's strings and its Toolbox / PluginManagement plugins, replies
//! recorded with `examples/plugin_probe.rs`):
//!
//! * the account's installed plugins (what Manage Plugins lists):
//!   `GET apis.roblox.com/studio-user-data-api/plugin-metadata` →
//!   `{pluginMetadata:[{pluginAssetId, assetVersionId, latestVersion,
//!   enabled, moderated, pluginName, autoUpdateEnabled}], cursor}`;
//! * each plugin's newest version: `GET studio-plugin-api/v1/plugins?pluginIds=a,b`
//!   → `{data:[{id, name, description, versionId, created, updated}]}`. An
//!   installed plugin has an update when its `assetVersionId` differs from
//!   `versionId` (Studio's Update button);
//! * names, creators, price: `GET toolbox-service/v1/items/details?assetIds=`;
//! * HTTP / script-injection permissions: `GET studio-user-settings/plugin-permissions/v2/plugins?permissionType=…`
//!   (pages via `nextCursor` / `&cursor=`; HTTP rows are keyed by host[:port]);
//! * the plugin file: asset delivery `v2/assetVersionId/{versionId}` (or
//!   `v2/assetId/{id}`), with the open place's context like every other
//!   asset request ([`Session::download_asset`]);
//! * Creator Store search: `GET toolbox-service/v2/assets:search?searchCategoryType=Plugin`;
//! * ownership: `GET inventory.roblox.com/v1/users/{u}/items/Asset/{id}/is-owned`;
//! * taking a free plugin (Toolbox "Get"): `POST marketplace-fiat-service/v1/product/purchase`
//!   with `productKey {productNamespace: PRODUCT_NAMESPACE_CREATOR_MARKETPLACE_ASSET,
//!   productType: PRODUCT_TYPE_PLUGIN, productTargetId}` and the expected price (0 USD);
//! * publishing (the Toolbox's asset upload over the session): `POST
//!   apis.roblox.com/assets/user-auth/v1/assets` (new) or `PATCH …/assets/{id}`
//!   (a new version of a plugin the account owns), multipart `request` JSON
//!   `{assetType: "Plugin", displayName, description, creationContext:{creator, expectedPrice:0}}`
//!   + `fileContent` (model/x-rbxm), then `GET …/operations/{id}` until done
//!   (`Data/Upload.ashx` no longer takes plugins: 307 to request-error 404).
//!
//! No call here changes the account's plugin list (plugin-metadata writes):
//! rustudio keeps its own enabled / installed state on disk.

use std::collections::HashMap;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::session::Session;
use crate::Error;

pub const APIS: &str = "https://apis.roblox.com";
pub const PLUGIN_METADATA_URL: &str = "https://apis.roblox.com/studio-user-data-api/plugin-metadata";
pub const PLUGIN_INFO_URL: &str = "https://apis.roblox.com/studio-plugin-api/v1/plugins";
pub const ITEM_DETAILS_URL: &str = "https://apis.roblox.com/toolbox-service/v1/items/details";
pub const PERMISSIONS_URL: &str = "https://apis.roblox.com/studio-user-settings/plugin-permissions/v2/plugins";
pub const SEARCH_URL: &str = "https://apis.roblox.com/toolbox-service/v2/assets:search";
pub const PURCHASE_URL: &str = "https://apis.roblox.com/marketplace-fiat-service/v1/product/purchase";
pub const UPLOAD_URL: &str = "https://data.roblox.com/Data/Upload.ashx";
/// Asset type id of a plugin (Enum.AssetType.Plugin).
pub const PLUGIN_ASSET_TYPE: u64 = 38;

/// Why a plugin call failed, in the words Manage Plugins shows.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Failure {
    SignedOut,
    Offline(String),
    NotOwned,
    Moderated,
    NotAPlugin,
    NotFound,
    Other(String),
}

impl Failure {
    pub fn describe(&self) -> String {
        match self {
            Failure::SignedOut => "You are signed out. Sign in to install or update plugins.".into(),
            Failure::Offline(e) => format!("Roblox can't be reached ({e})."),
            Failure::NotOwned => "This plugin isn't in your inventory. Get it from the Creator Store first.".into(),
            Failure::Moderated => "This plugin was moderated and can't be installed.".into(),
            Failure::NotAPlugin => "That asset isn't a plugin.".into(),
            Failure::NotFound => "No asset with that id.".into(),
            Failure::Other(e) => e.clone(),
        }
    }

    /// From an HTTP status and reply text (asset delivery / apis errors).
    pub fn from_reply(status: u16, body: &str) -> Failure {
        let lower = body.to_ascii_lowercase();
        if status == 401 {
            return Failure::SignedOut;
        }
        if status == 429 {
            return Failure::Other("Roblox is busy (too many requests); try again in a moment.".into());
        }
        if lower.contains("moderat") {
            return Failure::Moderated;
        }
        if status == 404 || lower.contains("not found") || lower.contains("does not exist") {
            return Failure::NotFound;
        }
        if status == 403 || lower.contains("not authorized") || lower.contains("permission") {
            return Failure::NotOwned;
        }
        let msg = serde_json::from_str::<Value>(body)
            .ok()
            .and_then(|v| v.pointer("/errors/0/message").and_then(|m| m.as_str()).map(str::to_string))
            .filter(|m| !m.is_empty())
            .unwrap_or_else(|| body.chars().take(160).collect());
        Failure::Other(format!("HTTP {status}: {msg}"))
    }
}

impl From<Failure> for Error {
    fn from(f: Failure) -> Error {
        Error(f.describe())
    }
}

fn http_err(e: Error) -> Failure {
    Failure::Offline(e.0)
}

/// One row of the account's installed plugins (plugin-metadata).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct CloudPlugin {
    pub plugin_asset_id: u64,
    #[serde(default)]
    pub asset_version_id: u64,
    #[serde(default)]
    pub latest_version: u64,
    #[serde(default = "yes")]
    pub enabled: bool,
    #[serde(default)]
    pub moderated: bool,
    #[serde(default)]
    pub plugin_name: String,
    #[serde(default)]
    pub auto_update_enabled: bool,
}

fn yes() -> bool {
    true
}

/// A plugin's current version (studio-plugin-api).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct PluginInfo {
    pub id: u64,
    pub name: String,
    pub description: String,
    pub version_id: u64,
    pub updated: String,
}

/// Toolbox item details: creator and price.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ItemDetails {
    pub id: u64,
    pub name: String,
    pub description: String,
    pub type_id: u64,
    pub creator_id: u64,
    pub creator_name: String,
    pub creator_type: u64,
    pub updated: String,
    /// None: not for sale through the Creator Store.
    pub is_free: Option<bool>,
}

/// One permission row (HttpService domain or ScriptInjection).
#[derive(Debug, Clone, PartialEq)]
pub struct CloudPermission {
    pub plugin_id: u64,
    pub kind: String,
    pub allowed: bool,
    pub domain: Option<String>,
}

/// A Creator Store search result.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct SearchHit {
    pub id: u64,
    pub name: String,
    pub description: String,
    pub creator_name: String,
    pub verified: bool,
    pub is_free: bool,
    pub up_percent: Option<u64>,
}

#[derive(Debug, Clone, Default)]
pub struct SearchPage {
    pub hits: Vec<SearchHit>,
    pub next: Option<String>,
}

/// Percent-encodes a query value.
pub fn enc(s: &str) -> String {
    let mut o = String::new();
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => o.push(b as char),
            _ => o.push_str(&format!("%{b:02X}")),
        }
    }
    o
}

/// GET with the session; a 429 (rate limited) is retried after 2, 4 and 8 s.
fn get_json(s: &Session, url: &str) -> std::result::Result<Value, Failure> {
    let mut wait = 2;
    let (st, t) = loop {
        let (st, t) = s.get(url).map_err(http_err)?;
        if st != 429 || wait > 8 {
            break (st, t);
        }
        std::thread::sleep(std::time::Duration::from_secs(wait));
        wait *= 2;
    };
    if st != 200 {
        return Err(Failure::from_reply(st, &t));
    }
    serde_json::from_str(&t).map_err(|e| Failure::Other(format!("{url}: {e}")))
}

/// The account's installed plugins. One reply holds them all: its
/// `cursor` is a change token (asking with it returns the same list), not
/// a next page.
pub fn installed(s: &Session) -> std::result::Result<Vec<CloudPlugin>, Failure> {
    let v = get_json(s, PLUGIN_METADATA_URL)?;
    let mut seen = std::collections::HashSet::new();
    Ok(parse_installed(&v).into_iter().filter(|p| seen.insert(p.plugin_asset_id)).collect())
}

pub fn parse_installed(v: &Value) -> Vec<CloudPlugin> {
    v.get("pluginMetadata")
        .and_then(|m| m.as_array())
        .into_iter()
        .flatten()
        .filter_map(|p| serde_json::from_value(p.clone()).ok())
        .collect()
}

/// Current versions of plugins, 50 ids a request.
pub fn plugin_info(s: &Session, ids: &[u64]) -> std::result::Result<HashMap<u64, PluginInfo>, Failure> {
    let mut out = HashMap::new();
    for chunk in ids.chunks(50) {
        let list = chunk.iter().map(u64::to_string).collect::<Vec<_>>().join(",");
        let v = get_json(s, &format!("{PLUGIN_INFO_URL}?pluginIds={list}"))?;
        for p in parse_plugin_info(&v) {
            out.insert(p.id, p);
        }
    }
    Ok(out)
}

pub fn parse_plugin_info(v: &Value) -> Vec<PluginInfo> {
    v.get("data")
        .and_then(|d| d.as_array())
        .into_iter()
        .flatten()
        .filter_map(|p| {
            Some(PluginInfo {
                id: p.get("id")?.as_u64()?,
                name: p.get("name").and_then(|n| n.as_str()).unwrap_or("").to_string(),
                description: p.get("description").and_then(|n| n.as_str()).unwrap_or("").to_string(),
                version_id: p.get("versionId").and_then(|n| n.as_u64()).unwrap_or(0),
                updated: p.get("updated").and_then(|n| n.as_str()).unwrap_or("").to_string(),
            })
        })
        .collect()
}

/// Toolbox details (name, creator, type, price), 50 ids a request.
pub fn item_details(s: &Session, ids: &[u64]) -> std::result::Result<HashMap<u64, ItemDetails>, Failure> {
    let mut out = HashMap::new();
    for chunk in ids.chunks(50) {
        let list = chunk.iter().map(u64::to_string).collect::<Vec<_>>().join(",");
        let v = get_json(s, &format!("{ITEM_DETAILS_URL}?assetIds={list}"))?;
        for d in parse_item_details(&v) {
            out.insert(d.id, d);
        }
    }
    Ok(out)
}

pub fn parse_item_details(v: &Value) -> Vec<ItemDetails> {
    v.get("data")
        .and_then(|d| d.as_array())
        .into_iter()
        .flatten()
        .filter_map(|e| {
            let a = e.get("asset")?;
            let c = e.get("creator");
            let s = |v: Option<&Value>, k: &str| v.and_then(|v| v.get(k)).and_then(|x| x.as_str()).unwrap_or("").to_string();
            let n = |v: Option<&Value>, k: &str| v.and_then(|v| v.get(k)).and_then(|x| x.as_u64()).unwrap_or(0);
            Some(ItemDetails {
                id: a.get("id")?.as_u64()?,
                name: s(Some(a), "name"),
                description: s(Some(a), "description"),
                type_id: n(Some(a), "typeId"),
                creator_id: n(c, "id"),
                creator_name: s(c, "name"),
                creator_type: n(c, "type"),
                updated: s(Some(a), "updatedUtc"),
                is_free: e.pointer("/fiatProduct/isFree").and_then(|f| f.as_bool()),
            })
        })
        .collect()
}

/// The account's plugin permissions (all plugins): Studio asks once per
/// permission type (`permissionType=HttpService`, `ScriptInjection`) and
/// follows `nextCursor` (`&cursor=`) page by page.
pub fn permissions(s: &Session) -> std::result::Result<Vec<CloudPermission>, Failure> {
    let mut out = Vec::new();
    for kind in ["HttpService", "ScriptInjection"] {
        let mut cursor: Option<String> = None;
        for _ in 0..200 {
            let mut url = format!("{PERMISSIONS_URL}?permissionType={kind}");
            if let Some(c) = &cursor {
                url.push_str(&format!("&cursor={}", enc(c)));
            }
            let v = get_json(s, &url)?;
            out.extend(parse_permissions(&v));
            match v.get("nextCursor").and_then(|c| c.as_str()) {
                Some(c) if !c.is_empty() && Some(c) != cursor.as_deref() => cursor = Some(c.to_string()),
                _ => break,
            }
        }
    }
    Ok(out)
}

pub fn parse_permissions(v: &Value) -> Vec<CloudPermission> {
    let mut out = Vec::new();
    for p in v.get("data").and_then(|d| d.as_array()).into_iter().flatten() {
        let Some(id) = p.get("pluginId").and_then(|i| i.as_u64()) else { continue };
        for r in p.get("permissions").and_then(|d| d.as_array()).into_iter().flatten() {
            out.push(CloudPermission {
                plugin_id: id,
                kind: r.get("type").and_then(|t| t.as_str()).unwrap_or("").to_string(),
                allowed: r.get("isAllowed").and_then(|t| t.as_bool()).unwrap_or(false),
                domain: r.get("domain").and_then(|t| t.as_str()).map(str::to_string),
            });
        }
    }
    out
}

/// A plugin's file: a given version (asset version id) or the newest. The
/// reply's asset type must be Plugin.
pub fn download(s: &Session, asset_id: u64, version_id: Option<u64>) -> std::result::Result<Vec<u8>, Failure> {
    let url = match version_id {
        Some(v) if v != 0 => format!("https://assetdelivery.roblox.com/v2/assetVersionId/{v}"),
        _ => format!("https://assetdelivery.roblox.com/v2/assetId/{asset_id}"),
    };
    let mut wait = 2;
    let (st, body) = loop {
        let (st, body) = s.fetch_bytes(&url, true).map_err(http_err)?;
        if st != 429 || wait > 4 {
            break (st, body);
        }
        std::thread::sleep(std::time::Duration::from_secs(wait));
        wait *= 2;
    };
    // A specific older version the v2 route keeps refusing (429): asset
    // delivery v1 `?assetversionid=` answers 302 to the file.
    if st != 200 {
        if let Some(v) = version_id.filter(|v| *v != 0) {
            let (st1, loc, _) = s
                .get_with_header(&format!("https://assetdelivery.roblox.com/v1/asset/?assetversionid={v}"), "location")
                .map_err(http_err)?;
            if let (300..=399, Some(loc)) = (st1, loc) {
                let (st2, bytes) = s.fetch_bytes(&loc, false).map_err(http_err)?;
                if (200..300).contains(&st2) {
                    return Ok(bytes);
                }
            }
        }
    }
    let text = String::from_utf8_lossy(&body).to_string();
    let v: Value = serde_json::from_str(&text).map_err(|_| Failure::from_reply(st, &text))?;
    if let Some(t) = v.get("assetTypeId").and_then(|t| t.as_u64()) {
        if t != PLUGIN_ASSET_TYPE {
            return Err(Failure::NotAPlugin);
        }
    }
    let Some(loc) = v.pointer("/locations/0/location").and_then(|l| l.as_str()) else {
        let msg = v.pointer("/errors/0/message").and_then(|m| m.as_str()).unwrap_or("");
        let code = v.pointer("/errors/0/code").and_then(|m| m.as_u64()).unwrap_or(st as u64) as u16;
        return Err(Failure::from_reply(if st == 200 { code } else { st }, msg));
    };
    let (st, bytes) = s.fetch_bytes(loc, false).map_err(http_err)?;
    if !(200..300).contains(&st) {
        return Err(Failure::Other(format!("plugin {asset_id}: download returned HTTP {st}")));
    }
    Ok(bytes)
}

/// Creator Store plugin search (Toolbox's Plugins tab).
pub fn search(s: &Session, query: &str, page: Option<&str>) -> std::result::Result<SearchPage, Failure> {
    let mut url = format!("{SEARCH_URL}?searchCategoryType=Plugin&maxPageSize=30&query={}", enc(query));
    if let Some(p) = page {
        url.push_str(&format!("&pageToken={}", enc(p)));
    }
    Ok(parse_search(&get_json(s, &url)?))
}

pub fn parse_search(v: &Value) -> SearchPage {
    let hits = v
        .get("creatorStoreAssets")
        .and_then(|a| a.as_array())
        .into_iter()
        .flatten()
        .filter_map(|e| {
            let a = e.get("asset")?;
            let q = e.pointer("/creatorStoreProduct/purchasePrice/quantity/significand").and_then(|x| x.as_i64());
            Some(SearchHit {
                id: a.get("id")?.as_u64()?,
                name: a.get("name").and_then(|x| x.as_str()).unwrap_or("").to_string(),
                description: a.get("description").and_then(|x| x.as_str()).unwrap_or("").to_string(),
                creator_name: e.pointer("/creator/name").and_then(|x| x.as_str()).unwrap_or("").to_string(),
                verified: e.pointer("/creator/verified").and_then(|x| x.as_bool()).unwrap_or(false),
                is_free: q.unwrap_or(0) == 0,
                up_percent: e.pointer("/voting/upVotePercent").and_then(|x| x.as_u64()),
            })
        })
        .collect();
    let next = v.get("nextPageToken").and_then(|x| x.as_str()).filter(|x| !x.is_empty()).map(str::to_string);
    SearchPage { hits, next }
}

/// Whether the account owns an asset.
pub fn owns(s: &Session, user_id: u64, asset_id: u64) -> std::result::Result<bool, Failure> {
    let (st, t) = s
        .get(&format!("https://inventory.roblox.com/v1/users/{user_id}/items/Asset/{asset_id}/is-owned"))
        .map_err(http_err)?;
    match st {
        200 => Ok(t.trim() == "true"),
        _ => Err(Failure::from_reply(st, &t)),
    }
}

/// What taking a free plugin did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Acquired {
    Now,
    AlreadyOwned,
}

/// The purchase request Toolbox sends for a free Creator Store plugin.
pub fn purchase_body(asset_id: u64) -> Value {
    serde_json::json!({
        "expectedPrice": {"currencyCode": "USD", "quantity": {"significand": 0, "exponent": 0}},
        "productKey": {
            "productNamespace": "PRODUCT_NAMESPACE_CREATOR_MARKETPLACE_ASSET",
            "productType": "PRODUCT_TYPE_PLUGIN",
            "productTargetId": asset_id.to_string(),
        }
    })
}

/// Takes a free plugin into the account's inventory (Toolbox "Get"); a
/// plugin with a price is refused here (rustudio sends no paid purchase).
pub fn acquire_free(s: &Session, asset_id: u64) -> std::result::Result<Acquired, Failure> {
    let d = item_details(s, &[asset_id])?;
    let Some(item) = d.get(&asset_id) else { return Err(Failure::NotFound) };
    if item.type_id != PLUGIN_ASSET_TYPE {
        return Err(Failure::NotAPlugin);
    }
    if item.is_free != Some(true) {
        return Err(Failure::Other("This plugin isn't free; buy it on the Creator Store website.".into()));
    }
    let body = purchase_body(asset_id).to_string();
    let (st, t) = s.post_csrf(PURCHASE_URL, "application/json", body.as_bytes()).map_err(http_err)?;
    let v: Value = serde_json::from_str(&t).unwrap_or(Value::Null);
    let status = v.get("purchaseTransactionStatus").and_then(|x| x.as_str()).unwrap_or("");
    match (st, status) {
        (_, "PURCHASE_TRANSACTION_STATUS_SUCCESS") => Ok(Acquired::Now),
        (_, "PURCHASE_TRANSACTION_STATUS_ALREADY_OWNED") => Ok(Acquired::AlreadyOwned),
        (200, _) if v.get("purchased").and_then(|p| p.as_bool()) == Some(true) => Ok(Acquired::Now),
        (_, s2) if !s2.is_empty() => Err(Failure::Other(format!("Purchase refused: {s2}"))),
        _ => Err(Failure::from_reply(st, &t)),
    }
}

/// "Publish as Plugin": a new plugin asset or a new version of one.
#[derive(Debug, Clone, PartialEq)]
pub struct PublishRequest {
    /// Some: overwrite this plugin (one the account owns).
    pub overwrite: Option<u64>,
    pub name: String,
    pub description: String,
    /// Group to publish under (None: the signed-in user).
    pub group_id: Option<u64>,
}

pub const USER_AUTH_ASSETS_URL: &str = "https://apis.roblox.com/assets/user-auth/v1/assets";
pub const USER_AUTH_OPERATIONS_URL: &str = "https://apis.roblox.com/assets/user-auth/v1/operations";
impl PublishRequest {
    /// The method and URL: POST …/assets (new) or PATCH …/assets/{id}.
    pub fn target(&self) -> (&'static str, String) {
        match self.overwrite {
            Some(id) => ("PATCH", format!("{USER_AUTH_ASSETS_URL}/{id}")),
            None => ("POST", USER_AUTH_ASSETS_URL.to_string()),
        }
    }

    /// The `request` part: Studio's asset upload request for a Plugin, kept
    /// off sale (expectedPrice 0, no Creator Store listing).
    pub fn request_json(&self, user_id: u64) -> Value {
        let creator = match self.group_id {
            Some(g) => serde_json::json!({"groupId": g.to_string()}),
            None => serde_json::json!({"userId": user_id.to_string()}),
        };
        let mut v = serde_json::json!({
            "assetType": "Plugin",
            "displayName": self.name,
            "description": self.description,
            "creationContext": {"creator": creator, "expectedPrice": 0},
        });
        if let Some(id) = self.overwrite {
            v["assetId"] = serde_json::json!(id.to_string());
        }
        v
    }

    /// multipart/form-data body: `request` (JSON) and `fileContent` (the model).
    pub fn body(&self, user_id: u64, rbxm: &[u8]) -> (String, Vec<u8>) {
        crate::asset_upload::multipart(&self.request_json(user_id), "asset.rbxm", "model/x-rbxm", rbxm)
    }
}

/// Uploads `rbxm` (a binary model of the plugin's instances) and waits for
/// Roblox to finish. Returns the plugin's asset id (the new one, or `overwrite`).
pub fn publish(s: &Session, req: &PublishRequest, rbxm: &[u8]) -> std::result::Result<u64, Failure> {
    if req.name.trim().is_empty() {
        return Err(Failure::Other("A plugin needs a name.".into()));
    }
    let (method, url) = req.target();
    let (ct, body) = req.body(s.profile.id, rbxm);
    let (st, t) = s.send_csrf(method, &url, &ct, &body).map_err(http_err)?;
    if !(200..300).contains(&st) {
        return Err(Failure::from_reply(st, &t));
    }
    let op = crate::asset_upload::wait_for_operation(
        &t,
        USER_AUTH_OPERATIONS_URL,
        |url| {
            let (status, body) = s.get(url).map_err(http_err)?;
            if status != 200 {
                return Err(Failure::from_reply(status, &body));
            }
            Ok(body)
        },
        Failure::Other,
    )?;
    let id = op
        .pointer("/response/assetId")
        .and_then(|v| v.as_str().and_then(|s| s.parse().ok()).or_else(|| v.as_u64()))
        .or(req.overwrite);
    id.ok_or_else(|| Failure::Other(format!("upload finished without an asset id: {}", op.to_string().chars().take(200).collect::<String>())))
}

/// An installed plugin has an update: Studio compares the installed asset
/// version id with the plugin's current one.
pub fn has_update(installed_version: u64, current_version: u64) -> bool {
    current_version != 0 && installed_version != current_version
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_recorded_replies() {
        let v: Value = serde_json::from_str(r#"{"pluginMetadata":[{"pluginAssetId":2268520847,"assetVersionId":12672462458357,"latestVersion":0,"enabled":true,"moderated":false,"pluginName":"Brushtool 2.1","autoUpdateEnabled":false},{"pluginAssetId":8290961982,"assetVersionId":29068666629,"latestVersion":29068666629,"enabled":false,"moderated":false,"pluginName":"Curves [Splines]","autoUpdateEnabled":false}],"cursor":""}"#).unwrap();
        let p = parse_installed(&v);
        assert_eq!(p.len(), 2);
        assert_eq!(p[1].plugin_asset_id, 8290961982);
        assert!(!p[1].enabled);
        let v: Value = serde_json::from_str(r#"{"data":[{"id":10035824259,"name":"Time Tracker","description":"x","commentsEnabled":false,"versionId":14475959943,"created":"2022-06-26T20:25:28.99Z","updated":"2022-10-29T19:20:14.087Z"}]}"#).unwrap();
        assert_eq!(parse_plugin_info(&v)[0].version_id, 14475959943);
        let v: Value = serde_json::from_str(r#"{"data":[{"pluginId":4596373254,"permissions":[{"id":1,"type":"ScriptInjection","isAllowed":true,"domain":null},{"id":2,"type":"HttpService","isAllowed":false,"domain":"example.com"}]}]}"#).unwrap();
        let perms = parse_permissions(&v);
        assert_eq!(perms.len(), 2);
        assert_eq!(perms[1].domain.as_deref(), Some("example.com"));
        assert!(has_update(1, 2) && !has_update(2, 2) && !has_update(2, 0));
    }

    #[test]
    fn publish_url_and_purchase_body() {
        let r = PublishRequest { overwrite: None, name: "rustudio publish test".into(), description: "a & b".into(), group_id: None };
        assert_eq!(r.target(), ("POST", USER_AUTH_ASSETS_URL.to_string()));
        let j = r.request_json(7);
        assert_eq!(j["assetType"], "Plugin");
        assert_eq!(j.pointer("/creationContext/creator/userId").unwrap(), "7");
        let (ct, body) = r.body(7, b"<roblox!xyz");
        assert!(ct.starts_with("multipart/form-data; boundary="));
        let text = String::from_utf8_lossy(&body);
        assert!(text.contains("name=\"fileContent\"; filename=\"asset.rbxm\"") && text.contains("<roblox!xyz"));
        let o = PublishRequest { overwrite: Some(99), ..r };
        assert_eq!(o.target().0, "PATCH");
        assert_eq!(o.request_json(7)["assetId"], "99");
        let b = purchase_body(5);
        assert_eq!(b.pointer("/productKey/productType").unwrap(), "PRODUCT_TYPE_PLUGIN");
        assert_eq!(b.pointer("/productKey/productTargetId").unwrap(), "5");
    }

    #[test]
    fn failures() {
        assert_eq!(Failure::from_reply(401, ""), Failure::SignedOut);
        assert_eq!(Failure::from_reply(403, r#"{"errors":[{"code":0,"message":"User is not authorized to access Asset."}]}"#), Failure::NotOwned);
        assert_eq!(Failure::from_reply(409, "Asset is moderated"), Failure::Moderated);
    }
}
