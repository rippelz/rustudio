//! The Toolbox's web calls over the signed-in session, as Studio's Toolbox
//! makes them (checks/toolbox/ref/NOTES.md, "Endpoints"): URL builders
//! (pure, unit-tested), an `Http` seam (the session, anonymous, or recorded
//! fixtures in tests) and fetches with Studio-like retry on 429.

use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::categories::Kind;
use super::model::{self, Asset, Page};

pub const APIS: &str = "https://apis.roblox.com";
pub const PAGE_SIZE: usize = 30;
/// Studio asks items/details for at most 30 ids a call.
pub const DETAILS_CHUNK: usize = 30;

/// Why a call failed (shown with Retry).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Failure {
    SignedOut,
    Offline(String),
    RateLimited,
    NotFound,
    Forbidden(String),
    Moderated,
    Cancelled,
    Other(String),
}

impl Failure {
    pub fn describe(&self) -> String {
        match self {
            Failure::SignedOut => "You are signed out. Sign in to use the Toolbox.".into(),
            Failure::Offline(e) => format!("Roblox can't be reached. Check your connection. ({e})"),
            Failure::RateLimited => "Roblox is busy (too many requests). Try again in a moment.".into(),
            Failure::NotFound => "This asset doesn't exist or was removed.".into(),
            Failure::Forbidden(m) => if m.is_empty() { "You don't have access to this asset.".into() } else { m.clone() },
            Failure::Moderated => "This asset was moderated and can't be used.".into(),
            Failure::Cancelled => "Cancelled.".into(),
            Failure::Other(e) => e.clone(),
        }
    }

    pub fn from_status(status: u16, body: &str) -> Failure {
        let lower = body.to_ascii_lowercase();
        match status {
            401 => Failure::SignedOut,
            429 => Failure::RateLimited,
            404 => Failure::NotFound,
            _ if lower.contains("moderat") => Failure::Moderated,
            403 => Failure::Forbidden(model::error_message(status, body)),
            _ => Failure::Other(model::error_message(status, body)),
        }
    }
}

/// The transport: GET bytes / POST text. The session, anonymous, or a
/// test's recorded replies.
pub trait Http: Send + Sync {
    fn get(&self, url: &str, cookie: bool) -> Result<(u16, Vec<u8>), String>;
    fn post(&self, url: &str, content_type: &str, body: &[u8]) -> Result<(u16, String), String>;
    /// POST / PATCH / DELETE with the CSRF dance (votes, favorites).
    fn send(&self, method: &str, url: &str, content_type: &str, body: &[u8]) -> Result<(u16, String), String> {
        if method == "POST" { self.post(url, content_type, body) } else { Err(format!("{method} is not supported here")) }
    }
    fn user_id(&self) -> u64;
    fn signed_in(&self) -> bool;
    /// Waits between retries (tests make it instant).
    fn sleep(&self, secs: f32) {
        std::thread::sleep(std::time::Duration::from_secs_f32(secs));
    }
}

/// The signed-in session (a copy that doesn't hold the account lock).
pub struct SessionHttp(pub rustudio_account::session::Session);

impl Http for SessionHttp {
    fn get(&self, url: &str, cookie: bool) -> Result<(u16, Vec<u8>), String> {
        self.0.fetch_bytes(url, cookie).map_err(|e| e.0)
    }
    fn post(&self, url: &str, ct: &str, body: &[u8]) -> Result<(u16, String), String> {
        self.0.post_csrf(url, ct, body).map_err(|e| e.0)
    }
    fn send(&self, method: &str, url: &str, ct: &str, body: &[u8]) -> Result<(u16, String), String> {
        self.0.send_csrf(method, url, ct, body).map_err(|e| e.0)
    }
    fn user_id(&self) -> u64 {
        self.0.profile.id
    }
    fn signed_in(&self) -> bool {
        true
    }
}

/// The network down (signed in, every call fails as offline):
/// `RUSTUDIO_TOOLBOX_OFFLINE=1` for the error / retry states.
pub struct OfflineHttp(pub u64);

impl Http for OfflineHttp {
    fn get(&self, _url: &str, _cookie: bool) -> Result<(u16, Vec<u8>), String> {
        Err("network unreachable".into())
    }
    fn post(&self, _url: &str, _ct: &str, _body: &[u8]) -> Result<(u16, String), String> {
        Err("network unreachable".into())
    }
    fn user_id(&self) -> u64 {
        self.0
    }
    fn signed_in(&self) -> bool {
        true
    }
}

/// Signed out: only public calls (thumbnails, CDN).
pub struct AnonHttp;

impl Http for AnonHttp {
    fn get(&self, url: &str, _cookie: bool) -> Result<(u16, Vec<u8>), String> {
        anon_get(url)
    }
    fn post(&self, _url: &str, _ct: &str, _body: &[u8]) -> Result<(u16, String), String> {
        Ok((401, String::new()))
    }
    fn user_id(&self) -> u64 {
        0
    }
    fn signed_in(&self) -> bool {
        false
    }
}

/// A GET without the session (thumbnails CDN, signed-out browsing).
pub fn anon_get(url: &str) -> Result<(u16, Vec<u8>), String> {
    use std::io::Read;
    let a = rustudio_account::api::agent();
    let mut r = a.get(url).header("Accept-Encoding", "gzip").call().map_err(|e| e.to_string())?;
    let st = r.status().as_u16();
    let mut b = Vec::new();
    r.body_mut().with_config().limit(256 << 20).reader().read_to_end(&mut b).map_err(|e| e.to_string())?;
    if b.starts_with(&[0x1f, 0x8b]) {
        let mut out = Vec::new();
        flate2::read::GzDecoder::new(&b[..]).read_to_end(&mut out).map_err(|e| e.to_string())?;
        b = out;
    }
    Ok((st, b))
}

/// Retry waits after a 429 (seconds): Studio's Networking retries with a
/// growing wait; rustudio gives up after the fourth.
pub const BACKOFF: [f32; 4] = [1.0, 2.0, 4.0, 8.0];

/// GET with retries on 429 and transient 5xx; `cancel` is polled between tries.
pub fn get_bytes(h: &dyn Http, url: &str, cookie: bool, cancel: &dyn Fn() -> bool) -> Result<Vec<u8>, Failure> {
    let mut tries = 0;
    loop {
        if cancel() {
            return Err(Failure::Cancelled);
        }
        let (st, body) = h.get(url, cookie).map_err(Failure::Offline)?;
        if (st == 429 || st == 503 || st == 502) && tries < BACKOFF.len() {
            h.sleep(BACKOFF[tries]);
            tries += 1;
            continue;
        }
        if (200..300).contains(&st) {
            return Ok(body);
        }
        return Err(Failure::from_status(st, &String::from_utf8_lossy(&body)));
    }
}

pub fn get_json(h: &dyn Http, url: &str, cancel: &dyn Fn() -> bool) -> Result<Value, Failure> {
    let b = get_bytes(h, url, true, cancel)?;
    serde_json::from_slice(&b).map_err(|e| Failure::Other(format!("Unexpected reply ({e})")))
}

/// Percent-encodes a query value.
pub fn enc(s: &str) -> String {
    rustudio_account::plugins::enc(s)
}

// ------------------------------------------------------------------ lists

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, Default)]
pub enum Sort {
    #[default]
    Relevance,
    /// "Rating" in Studio's menu (`sortCategory=Ratings`).
    Ratings,
    /// Audio tiles "Today's New Music": newest first.
    Newest,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, Default)]
pub enum CreatorFilter {
    #[default]
    All,
    Roblox,
}

/// The filter sheet (and the pill dropdowns) of the Creator Store.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize, Default)]
pub struct Filters {
    pub sort: Sort,
    pub include_unverified: bool,
    pub creators: CreatorFilter,
    /// "Search by Creator": a creator name (results are filtered to it).
    pub creator_name: String,
    /// Price range in cents (None: any).
    pub min_cents: Option<u64>,
    pub max_cents: Option<u64>,
    pub free_only: bool,
    /// Contains: Scripts, MeshParts, Decals, Tools, Animations, Audio.
    pub contains: Vec<String>,
    /// Facet chips (pine, autumn...).
    pub facets: Vec<String>,
    /// Audio: length range in seconds, Music / SoundEffect, artist, album.
    pub audio_min_s: Option<u32>,
    pub audio_max_s: Option<u32>,
    pub audio_type: Option<String>,
    pub artist: String,
    pub album: String,
}

impl Filters {
    /// Something other than the defaults is set (Studio's filter button
    /// shows a dot).
    pub fn active(&self) -> bool {
        *self != Filters::default()
    }
}

/// Where a list comes from.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Source {
    /// Creator Store search / category results.
    Store,
    /// A home swimlane (`section` of the Models home).
    Home(String),
    Inventory { group: Option<u64> },
    Recent,
    Creations { group: Option<u64> },
    /// "More From <creator>" on the detail page.
    Creator { id: u64, group: bool },
}

/// Everything that decides a list's contents: its cache key.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct ListReq {
    pub source: Source,
    pub kind: Kind,
    pub query: String,
    pub category_path: Option<String>,
    pub sub_type: Option<String>,
    pub filters: Filters,
}

impl ListReq {
    pub fn store(kind: Kind, query: &str) -> ListReq {
        ListReq { source: Source::Store, kind, query: query.to_string(), category_path: None, sub_type: None, filters: Filters::default() }
    }
}

/// The first or next page's URL. `user_id` for the account routes.
pub fn list_url(r: &ListReq, user_id: u64, cursor: Option<&str>) -> String {
    let tb = format!("{APIS}/toolbox-service");
    let q = enc(r.query.trim());
    let f = &r.filters;
    match &r.source {
        Source::Store | Source::Creator { .. } if r.kind.v2().is_some() => {
            let mut u = format!("{tb}/v2/assets:search?searchCategoryType={}&query={q}&maxPageSize={PAGE_SIZE}", r.kind.v2().unwrap_or("Model"));
            match f.sort {
                Sort::Relevance => {}
                Sort::Ratings => u.push_str("&sortCategory=Ratings"),
                Sort::Newest => u.push_str("&sortCategory=CreateTime&sortDirection=Descending"),
            }
            u.push_str(if f.include_unverified { "&includeOnlyVerifiedCreators=false" } else { "&includeOnlyVerifiedCreators=true" });
            if let Some(p) = &r.category_path {
                u.push_str(&format!("&categoryPath={}", enc(p)));
            }
            if let Some(s) = &r.sub_type {
                u.push_str(&format!("&assetSubTypes={}", enc(s)));
            }
            for fc in &f.facets {
                u.push_str(&format!("&facets={}", enc(fc)));
            }
            if f.free_only {
                u.push_str("&maxPriceCents=0");
            } else {
                if let Some(m) = f.min_cents.filter(|m| *m > 0) {
                    u.push_str(&format!("&minPriceCents={m}"));
                }
                if let Some(m) = f.max_cents {
                    u.push_str(&format!("&maxPriceCents={m}"));
                }
            }
            for c in &f.contains {
                u.push_str(&format!("&includedInstanceTypes={}", enc(c)));
            }
            match &r.source {
                Source::Creator { id, group } => u.push_str(&format!("&{}={id}", if *group { "groupId" } else { "userId" })),
                _ => match f.creators {
                    CreatorFilter::Roblox => u.push_str("&userId=1"),
                    CreatorFilter::All if !f.creator_name.trim().is_empty() => u.push_str(&format!("&creatorName={}", enc(f.creator_name.trim()))),
                    CreatorFilter::All => {}
                },
            }
            if r.kind == Kind::Audio {
                if let Some(m) = f.audio_min_s {
                    u.push_str(&format!("&audioMinDurationSeconds={m}"));
                }
                if let Some(m) = f.audio_max_s {
                    u.push_str(&format!("&audioMaxDurationSeconds={m}"));
                }
                if let Some(t) = &f.audio_type {
                    u.push_str(&format!("&audioTypes={}", enc(t)));
                }
                if !f.artist.trim().is_empty() {
                    u.push_str(&format!("&artist={}", enc(f.artist.trim())));
                }
                if !f.album.trim().is_empty() {
                    u.push_str(&format!("&album={}", enc(f.album.trim())));
                }
            }
            if let Some(c) = cursor {
                u.push_str(&format!("&pageToken={}", enc(c)));
            }
            u
        }
        Source::Store | Source::Creator { .. } => {
            // Animations (and anything else the v2 search refuses): the v1 marketplace route.
            let mut u = format!("{tb}/v1/marketplace/{}?keyword={q}&limit={PAGE_SIZE}&sortType={}&includeOnlyVerifiedCreators={}",
                r.kind.asset_type(), if f.sort == Sort::Ratings { "Ratings" } else { "Relevance" }, !f.include_unverified);
            if let Source::Creator { id, group } = &r.source {
                u.push_str(&format!("&creatorTargetId={id}&creatorType={}", if *group { 2 } else { 1 }));
            }
            push_cursor(&mut u, cursor);
            u
        }
        Source::Home(section) => {
            let mut u = format!("{tb}/v1/home/{}/section/{}/assets?limit={PAGE_SIZE}", r.kind.asset_type(), enc(section));
            push_cursor(&mut u, cursor);
            u
        }
        Source::Inventory { group } => {
            let owner = match group {
                Some(g) => format!("group/{g}"),
                None => format!("user/{user_id}"),
            };
            let mut u = format!("{tb}/v1/inventory/{owner}/{}?keyword={q}&sortType=Relevance&limit={PAGE_SIZE}&includeOnlyVerifiedCreators=true&querySource=0&tags=&uiSortIntent=Relevance&excludeGroupCreations=true&originalUserQuery={q}&originalCorrection=", r.kind.api());
            push_cursor(&mut u, cursor);
            u
        }
        Source::Recent => {
            let mut u = format!("{tb}/v1/recent/user/{user_id}/{}?keyword={q}&sortType=Relevance&limit={PAGE_SIZE}", r.kind.api());
            push_cursor(&mut u, cursor);
            u
        }
        Source::Creations { group } => {
            let owner = match group {
                Some(g) => format!("group/{g}"),
                None => format!("user/{user_id}"),
            };
            let mut u = format!("{tb}/v1/creations/{owner}/{}?limit=25&separateModelsAndPackages=true", r.kind.creations_name());
            push_cursor(&mut u, cursor);
            u
        }
    }
}

fn push_cursor(u: &mut String, cursor: Option<&str>) {
    if let Some(c) = cursor {
        u.push_str(&format!("&cursor={}", enc(c)));
    }
}

pub fn details_url(ids: &[u64]) -> String {
    let list = ids.iter().map(u64::to_string).collect::<Vec<_>>().join("%2C");
    format!("{APIS}/toolbox-service/v1/items/details?assetIds={list}")
}

pub fn autocomplete_url(kind: Kind, prefix: &str, limit: usize) -> String {
    format!("{APIS}/autocomplete-studio/v2/suggest?cat={}&prefix={}&limit={limit}", kind.autocomplete(), enc(prefix))
}

pub fn thumbnails_url(ids: &[u64], size: u32) -> String {
    let list = ids.iter().map(u64::to_string).collect::<Vec<_>>().join(",");
    format!("https://thumbnails.roblox.com/v1/assets?assetIds={list}&returnPolicy=PlaceHolder&size={size}x{size}&format=Png&isCircular=false")
}

pub const GROUPS_URL: &str = "https://apis.roblox.com/creator-home-api/v1/groups?surface=StudioToolboxCreations";

/// One page: the list call, then items/details for the rows that came
/// without details (v1 routes), keeping the list's order.
pub fn fetch_page(h: &dyn Http, r: &ListReq, cursor: Option<&str>, cancel: &dyn Fn() -> bool) -> Result<Page, Failure> {
    if !h.signed_in() {
        return Err(Failure::SignedOut);
    }
    let url = list_url(r, h.user_id(), cursor);
    let v = get_json(h, &url, cancel)?;
    let v2 = url.contains("/v2/assets:search");
    if v.get("errors").is_some() && v.get("creatorStoreAssets").is_none() && v.get("data").is_none() {
        return Err(Failure::Other(model::error_message(400, &v.to_string())));
    }
    let mut page = if v2 { model::parse_v2_search(&v) } else { model::parse_id_list(&v) };
    let missing: Vec<u64> = page.ids.iter().copied().filter(|id| !page.assets.iter().any(|a| a.id == *id && a.detailed)).collect();
    if !missing.is_empty() {
        let details = fetch_details(h, &missing, cancel)?;
        let named: Vec<Asset> = std::mem::take(&mut page.assets);
        page.assets = page
            .ids
            .iter()
            .map(|id| {
                let mut a = details.iter().find(|d| d.id == *id).cloned().unwrap_or_else(|| Asset { id: *id, ..Default::default() });
                if let Some(n) = named.iter().find(|n| n.id == *id) {
                    a.merge(n);
                }
                a
            })
            .collect();
    }
    Ok(page)
}

/// items/details in chunks of 30. Missing ids (removed / private assets)
/// simply aren't in the reply.
pub fn fetch_details(h: &dyn Http, ids: &[u64], cancel: &dyn Fn() -> bool) -> Result<Vec<Asset>, Failure> {
    let mut out = Vec::new();
    for chunk in ids.chunks(DETAILS_CHUNK) {
        match get_json(h, &details_url(chunk), cancel) {
            Ok(v) => out.extend(model::parse_details(&v)),
            // items/details refuses some types (Animations: 404): develop's batch.
            Err(Failure::NotFound) | Err(Failure::Other(_)) => {
                let list = chunk.iter().map(u64::to_string).collect::<Vec<_>>().join(",");
                if let Ok(v) = get_json(h, &format!("https://develop.roblox.com/v1/assets?assetIds={list}"), cancel) {
                    out.extend(model::parse_develop(&v));
                }
            }
            Err(e) => return Err(e),
        }
    }
    Ok(out)
}

pub fn fetch_suggestions(h: &dyn Http, kind: Kind, prefix: &str) -> Vec<String> {
    if prefix.trim().is_empty() || !h.signed_in() {
        return Vec::new();
    }
    get_json(h, &autocomplete_url(kind, prefix.trim(), 8), &|| false).map(|v| model::parse_autocomplete(&v)).unwrap_or_default()
}

pub fn fetch_groups(h: &dyn Http) -> Result<Vec<(u64, String)>, Failure> {
    Ok(model::parse_groups(&get_json(h, GROUPS_URL, &|| false)?))
}

pub fn owns(h: &dyn Http, asset_id: u64) -> Result<bool, Failure> {
    let url = format!("https://inventory.roblox.com/v1/users/{}/items/Asset/{asset_id}/is-owned", h.user_id());
    let b = get_bytes(h, &url, true, &|| false)?;
    Ok(String::from_utf8_lossy(&b).trim() == "true")
}

pub fn favorite_count(h: &dyn Http, asset_id: u64) -> Option<u64> {
    let b = get_bytes(h, &format!("https://catalog.roblox.com/v1/favorites/assets/{asset_id}/count"), true, &|| false).ok()?;
    String::from_utf8_lossy(&b).trim().parse().ok()
}

pub fn is_favorited(h: &dyn Http, asset_id: u64) -> Option<bool> {
    let url = format!("https://catalog.roblox.com/v1/favorites/users/{}/assets/{asset_id}/favorite", h.user_id());
    let b = get_bytes(h, &url, true, &|| false).ok()?;
    let t = String::from_utf8_lossy(&b);
    Some(t.trim() != "null" && !t.trim().is_empty())
}

/// The account-changing requests Studio's buttons send. rustudio builds
/// them exactly; the owner's rules keep vote / favorite / report from
/// being sent live by tests (`RUSTUDIO_TOOLBOX_DRY_RUN=1` logs instead).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Mutation {
    pub method: &'static str,
    pub url: String,
    pub body: String,
}

pub fn vote_request(asset_id: u64, up: Option<bool>) -> Mutation {
    match up {
        Some(v) => Mutation { method: "POST", url: format!("{APIS}/voting-api/vote/asset/{asset_id}?vote={v}"), body: String::new() },
        None => Mutation { method: "DELETE", url: format!("{APIS}/voting-api/vote/asset/{asset_id}"), body: String::new() },
    }
}

pub fn favorite_request(user_id: u64, asset_id: u64, on: bool) -> Mutation {
    Mutation {
        method: if on { "POST" } else { "DELETE" },
        url: format!("https://catalog.roblox.com/v1/favorites/users/{user_id}/assets/{asset_id}/favorite"),
        body: String::new(),
    }
}

pub fn insert_tracking_request(asset_id: u64) -> Mutation {
    Mutation { method: "POST", url: format!("{APIS}/toolbox-service/v1/insert/asset/{asset_id}"), body: String::new() }
}

pub fn report_url(asset_id: u64) -> String {
    format!("https://www.roblox.com/abusereport/asset?id={asset_id}")
}

pub fn store_url(asset_id: u64, kind: Kind) -> String {
    let seg = match kind {
        Kind::Model | Kind::Package | Kind::Animation => "models",
        Kind::Plugin => "plugins",
        Kind::Audio => "audio",
        Kind::Font => "fonts",
        Kind::Decal => "decals",
        Kind::MeshPart => "meshparts",
        Kind::Video => "videos",
    };
    format!("https://create.roblox.com/store/asset/{asset_id}?assetType={seg}")
}

/// The purchase request for a free asset (Toolbox "Get"); refused for
/// anything with a price (rustudio never spends Robux or money).
pub fn acquire_body(asset_id: u64, kind: Kind) -> Option<Value> {
    let pt = kind.product_type()?;
    Some(serde_json::json!({
        "expectedPrice": {"currencyCode": "USD", "quantity": {"significand": 0, "exponent": 0}},
        "productKey": {
            "productNamespace": "PRODUCT_NAMESPACE_CREATOR_MARKETPLACE_ASSET",
            "productType": pt,
            "productTargetId": asset_id.to_string(),
        }
    }))
}

/// Takes a free asset into the account (only when the details say it is
/// free and the account doesn't own it).
pub fn acquire_free(h: &dyn Http, asset: &Asset) -> Result<bool, Failure> {
    let kind = Kind::from_asset_type(asset.type_id).unwrap_or(Kind::Model);
    if !asset.price.is_free() {
        return Err(Failure::Other("This asset isn't free; get it on the Creator Store website.".into()));
    }
    if owns(h, asset.id).unwrap_or(false) {
        return Ok(false);
    }
    let body = acquire_body(asset.id, kind).ok_or_else(|| Failure::Other("This asset type can't be taken from the Toolbox.".into()))?;
    let (st, t) = h
        .post(rustudio_account::plugins::PURCHASE_URL, "application/json", body.to_string().as_bytes())
        .map_err(Failure::Offline)?;
    let v: Value = serde_json::from_str(&t).unwrap_or(Value::Null);
    match v.get("purchaseTransactionStatus").and_then(|x| x.as_str()).unwrap_or("") {
        "PURCHASE_TRANSACTION_STATUS_SUCCESS" => Ok(true),
        "PURCHASE_TRANSACTION_STATUS_ALREADY_OWNED" => Ok(false),
        s if !s.is_empty() => Err(Failure::Other(format!("Roblox refused: {s}"))),
        _ if (200..300).contains(&st) && v.get("purchased").and_then(|p| p.as_bool()) == Some(true) => Ok(true),
        _ => Err(Failure::from_status(st, &t)),
    }
}

#[cfg(test)]
pub mod tests {
    use super::*;
    use std::collections::HashMap;
    use std::sync::Mutex;

    /// Replies by URL substring; counts calls; can fail the first N calls with 429.
    #[derive(Default)]
    pub struct Mock {
        pub routes: Vec<(String, u16, Vec<u8>)>,
        pub calls: Mutex<Vec<String>>,
        pub throttle: Mutex<u32>,
        pub slept: Mutex<f32>,
    }

    impl Mock {
        pub fn route(mut self, pat: &str, fixture: &str) -> Self {
            let p = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("checks/toolbox/fixtures").join(fixture);
            self.routes.push((pat.to_string(), 200, std::fs::read(p).unwrap()));
            self
        }
        pub fn raw(mut self, pat: &str, status: u16, body: &str) -> Self {
            self.routes.push((pat.to_string(), status, body.as_bytes().to_vec()));
            self
        }
    }

    impl Http for Mock {
        fn get(&self, url: &str, _c: bool) -> Result<(u16, Vec<u8>), String> {
            self.calls.lock().unwrap().push(url.to_string());
            {
                let mut t = self.throttle.lock().unwrap();
                if *t > 0 {
                    *t -= 1;
                    return Ok((429, b"{\"errors\":[{\"code\":0,\"message\":\"TooManyRequests\"}]}".to_vec()));
                }
            }
            for (p, st, b) in &self.routes {
                if url.contains(p.as_str()) {
                    return Ok((*st, b.clone()));
                }
            }
            Err("no route (offline)".into())
        }
        fn post(&self, url: &str, _ct: &str, _b: &[u8]) -> Result<(u16, String), String> {
            self.calls.lock().unwrap().push(format!("POST {url}"));
            Ok((200, "{\"purchaseTransactionStatus\":\"PURCHASE_TRANSACTION_STATUS_SUCCESS\"}".into()))
        }
        fn user_id(&self) -> u64 {
            1000001
        }
        fn signed_in(&self) -> bool {
            true
        }
        fn sleep(&self, s: f32) {
            *self.slept.lock().unwrap() += s;
        }
    }

    #[test]
    fn urls() {
        let mut r = ListReq::store(Kind::Model, "tree house");
        assert_eq!(list_url(&r, 1, None), "https://apis.roblox.com/toolbox-service/v2/assets:search?searchCategoryType=Model&query=tree%20house&maxPageSize=30&includeOnlyVerifiedCreators=true");
        r.filters.sort = Sort::Ratings;
        r.filters.include_unverified = true;
        r.filters.facets = vec!["pine".into()];
        r.filters.free_only = true;
        r.category_path = Some("3d__nature".into());
        let u = list_url(&r, 1, Some("abc="));
        assert!(u.contains("&sortCategory=Ratings&includeOnlyVerifiedCreators=false&categoryPath=3d__nature&facets=pine&maxPriceCents=0&pageToken=abc%3D"), "{u}");
        let a = ListReq::store(Kind::Animation, "walk");
        assert!(list_url(&a, 1, None).starts_with("https://apis.roblox.com/toolbox-service/v1/marketplace/24?keyword=walk&limit=30"));
        let inv = ListReq { source: Source::Inventory { group: None }, ..ListReq::store(Kind::Audio, "") };
        assert!(list_url(&inv, 77, Some("c")).starts_with("https://apis.roblox.com/toolbox-service/v1/inventory/user/77/audio?keyword=&sortType=Relevance&limit=30"));
        assert!(list_url(&inv, 77, Some("c")).ends_with("&cursor=c"));
        let g = ListReq { source: Source::Creations { group: Some(5) }, ..ListReq::store(Kind::MeshPart, "") };
        assert_eq!(list_url(&g, 77, None), "https://apis.roblox.com/toolbox-service/v1/creations/group/5/MeshPart?limit=25&separateModelsAndPackages=true");
        let rec = ListReq { source: Source::Recent, ..ListReq::store(Kind::Model, "") };
        assert!(list_url(&rec, 77, None).starts_with("https://apis.roblox.com/toolbox-service/v1/recent/user/77/model?"));
        let home = ListReq { source: Source::Home("trending".into()), ..ListReq::store(Kind::Model, "") };
        assert_eq!(list_url(&home, 0, None), "https://apis.roblox.com/toolbox-service/v1/home/10/section/trending/assets?limit=30");
        assert_eq!(details_url(&[1, 2]), "https://apis.roblox.com/toolbox-service/v1/items/details?assetIds=1%2C2");
        assert_eq!(autocomplete_url(Kind::Decal, "gr", 8), "https://apis.roblox.com/autocomplete-studio/v2/suggest?cat=image&prefix=gr&limit=8");
        let mut au = ListReq::store(Kind::Audio, "rain");
        au.filters.audio_max_s = Some(10);
        au.filters.audio_type = Some("SoundEffect".into());
        assert!(list_url(&au, 0, None).ends_with("&audioMaxDurationSeconds=10&audioTypes=SoundEffect"));
        assert_eq!(vote_request(5, Some(true)).url, "https://apis.roblox.com/voting-api/vote/asset/5?vote=true");
        assert_eq!(vote_request(5, None).method, "DELETE");
        assert_eq!(favorite_request(7, 5, false).method, "DELETE");
        assert_eq!(acquire_body(5, Kind::Audio).unwrap().pointer("/productKey/productType").unwrap(), "PRODUCT_TYPE_AUDIO");
        assert!(acquire_body(5, Kind::Animation).is_none());
    }

    #[test]
    fn pages_v2_and_v1_with_details_in_order() {
        let m = Mock::default().route("v2/assets:search", "v2_search_model_tree.json");
        let p = fetch_page(&m, &ListReq::store(Kind::Model, "tree"), None, &|| false).unwrap();
        assert_eq!(p.assets.len(), 30);
        assert_eq!(m.calls.lock().unwrap().len(), 1, "v2 needs no details call");

        let m = Mock::default().route("/inventory/", "v1_inventory_model.json").route("items/details", "items_details.json");
        let r = ListReq { source: Source::Inventory { group: None }, ..ListReq::store(Kind::Model, "") };
        let p = fetch_page(&m, &r, None, &|| false).unwrap();
        assert_eq!(p.ids.len(), 30);
        assert_eq!(p.assets.iter().map(|a| a.id).collect::<Vec<_>>(), p.ids, "order kept, unknown ids kept as bare rows");
        assert_eq!(m.calls.lock().unwrap().iter().filter(|c| c.contains("items/details")).count(), 1);
        assert_eq!(p.next.as_deref(), Some("CURSOR2"));

        let m = Mock::default().route("/creations/", "v1_creations_model.json").raw("items/details", 200, "{\"data\":[]}");
        let r = ListReq { source: Source::Creations { group: None }, ..ListReq::store(Kind::Model, "") };
        let p = fetch_page(&m, &r, None, &|| false).unwrap();
        assert_eq!(p.assets[0].name, "Creation 1", "creations keep their names without details");
    }

    #[test]
    fn retries_429_then_gives_up() {
        let m = Mock::default().route("v2/assets:search", "v2_search_model_tree.json");
        *m.throttle.lock().unwrap() = 2;
        assert!(fetch_page(&m, &ListReq::store(Kind::Model, "tree"), None, &|| false).is_ok());
        assert_eq!(*m.slept.lock().unwrap(), 3.0);
        let m = Mock::default().route("v2/assets:search", "v2_search_model_tree.json");
        *m.throttle.lock().unwrap() = 99;
        assert_eq!(fetch_page(&m, &ListReq::store(Kind::Model, "tree"), None, &|| false), Err(Failure::RateLimited));
        assert_eq!(*m.slept.lock().unwrap(), 15.0);
    }

    #[test]
    fn failures_map() {
        let m = Mock::default();
        assert!(matches!(fetch_page(&m, &ListReq::store(Kind::Model, "x"), None, &|| false), Err(Failure::Offline(_))));
        let m = Mock::default().raw("search", 401, "");
        assert_eq!(fetch_page(&m, &ListReq::store(Kind::Model, "x"), None, &|| false), Err(Failure::SignedOut));
        let m = Mock::default().raw("search", 200, "<html>not json");
        assert!(matches!(fetch_page(&m, &ListReq::store(Kind::Model, "x"), None, &|| false), Err(Failure::Other(_))));
        let m = Mock::default().route("search", "v2_search_error_animation.json");
        assert!(matches!(fetch_page(&m, &ListReq::store(Kind::Model, "x"), None, &|| false), Err(Failure::Other(_))));
        assert_eq!(fetch_page(&Mock::default(), &ListReq::store(Kind::Model, "x"), None, &|| true), Err(Failure::Cancelled));
        assert_eq!(Failure::from_status(400, "Asset is moderated"), Failure::Moderated);
        let _ = HashMap::<u8, u8>::new();
    }

    #[test]
    fn acquire_only_free() {
        let m = Mock::default().raw("is-owned", 200, "false");
        let mut a = Asset { id: 5, type_id: 10, price: model::Price::Cents(99), ..Default::default() };
        assert!(acquire_free(&m, &a).is_err());
        assert!(!m.calls.lock().unwrap().iter().any(|c| c.starts_with("POST")), "nothing sent for a paid asset");
        a.price = model::Price::Free;
        assert_eq!(acquire_free(&m, &a), Ok(true));
        let m = Mock::default().raw("is-owned", 200, "true");
        assert_eq!(acquire_free(&m, &a), Ok(false));
        assert!(!m.calls.lock().unwrap().iter().any(|c| c.starts_with("POST")));
    }
}
