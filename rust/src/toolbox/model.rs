//! What the Toolbox shows, and the parsers for the replies it reads
//! (checks/toolbox/ref/NOTES.md). Every parser takes any JSON value and
//! never panics: missing or mistyped fields fall back to defaults, rows
//! without an id are dropped (tests below run the recorded replies in
//! checks/toolbox/fixtures/ and a set of malformed shapes).

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// A price as the Creator Store gives it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum Price {
    #[default]
    Unknown,
    Free,
    /// US cents.
    Cents(u64),
    /// Not on sale (own creations, inventory items).
    OffSale,
}

impl Price {
    /// Studio's label: "FREE" on the Creator Store cards, "$9.99".
    pub fn label(self, store_style: bool) -> String {
        match self {
            Price::Free => if store_style { "FREE".into() } else { "Free".into() },
            Price::Cents(c) => format!("${}.{:02}", c / 100, c % 100),
            Price::OffSale | Price::Unknown => String::new(),
        }
    }
    pub fn is_free(self) -> bool {
        self == Price::Free
    }
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Creator {
    pub id: u64,
    pub name: String,
    /// 1 user, 2 group (Toolbox's numbering).
    pub kind: u8,
    pub verified: bool,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Votes {
    pub show: bool,
    pub up: u64,
    pub down: u64,
    pub percent: Option<u64>,
    pub count: u64,
    pub has_voted: bool,
    pub can_vote: bool,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct AudioInfo {
    pub duration: f64,
    pub audio_type: String,
    pub artist: String,
    pub title: String,
    pub album: String,
    pub genre: String,
}

/// One asset as a card / detail page shows it. Filled from a v2 search row
/// or an items/details row (whichever came first; `merge` adds the other).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Asset {
    pub id: u64,
    pub name: String,
    pub description: String,
    pub type_id: u32,
    pub creator: Creator,
    pub votes: Votes,
    pub price: Price,
    pub has_scripts: bool,
    pub script_count: u64,
    pub created: String,
    pub updated: String,
    pub category_path: String,
    pub sub_types: Vec<String>,
    pub endorsed: bool,
    pub triangles: Option<u64>,
    pub vertices: Option<u64>,
    /// script, meshPart, animation, decal, audio, tool.
    pub counts: Vec<(String, u64)>,
    pub audio: Option<AudioInfo>,
    /// The row came with full details (creator, votes, price).
    pub detailed: bool,
}

impl Asset {
    pub fn count(&self, k: &str) -> Option<u64> {
        self.counts.iter().find(|(n, _)| n == k).map(|(_, v)| *v)
    }
    /// Fields the other reply had and this one lacks.
    pub fn merge(&mut self, o: &Asset) {
        if self.name.is_empty() {
            self.name = o.name.clone();
        }
        if self.description.is_empty() {
            self.description = o.description.clone();
        }
        if self.type_id == 0 {
            self.type_id = o.type_id;
        }
        if self.creator.name.is_empty() {
            self.creator = o.creator.clone();
        }
        if !self.votes.show && o.votes.show {
            self.votes = o.votes.clone();
        }
        if self.price == Price::Unknown {
            self.price = o.price;
        }
        self.has_scripts |= o.has_scripts;
        if self.created.is_empty() {
            self.created = o.created.clone();
            self.updated = o.updated.clone();
        }
        if self.category_path.is_empty() {
            self.category_path = o.category_path.clone();
        }
        if self.triangles.is_none() {
            self.triangles = o.triangles;
            self.vertices = o.vertices;
        }
        if self.counts.is_empty() {
            self.counts = o.counts.clone();
        }
        if self.audio.is_none() {
            self.audio = o.audio.clone();
        }
        self.endorsed |= o.endorsed;
        self.detailed |= o.detailed;
    }
}

/// One page of a list: the assets (details may still be missing: `ids`
/// keeps the order) and how to get the next one.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Page {
    pub ids: Vec<u64>,
    pub assets: Vec<Asset>,
    pub next: Option<String>,
    pub total: Option<u64>,
    pub facets: Vec<String>,
    /// Spell check: the query Roblox suggests ("Did you mean").
    pub corrected: Option<String>,
    pub filtered_keyword: Option<String>,
}

// ---------------------------------------------------------------- helpers

fn s(v: &Value, k: &str) -> String {
    v.get(k).and_then(|x| x.as_str()).unwrap_or("").to_string()
}

/// A number that may arrive as a JSON number or a numeric string.
pub fn num(v: Option<&Value>) -> Option<u64> {
    let v = v?;
    v.as_u64()
        .or_else(|| v.as_f64().filter(|f| f.is_finite() && *f >= 0.0).map(|f| f as u64))
        .or_else(|| v.as_str().and_then(|t| t.trim().parse().ok()))
}

fn fnum(v: Option<&Value>) -> Option<f64> {
    let v = v?;
    v.as_f64().or_else(|| v.as_str().and_then(|t| t.trim().parse().ok())).filter(|f| f.is_finite())
}

fn b(v: Option<&Value>) -> bool {
    v.and_then(|x| x.as_bool()).unwrap_or(false)
}

fn arr<'a>(v: &'a Value, k: &str) -> &'a [Value] {
    v.get(k).and_then(|a| a.as_array()).map(|a| a.as_slice()).unwrap_or(&[])
}

/// `{currencyCode, quantity{significand, exponent}}` → a price.
pub fn price_of(p: Option<&Value>) -> Price {
    let Some(p) = p else { return Price::Unknown };
    let sig = p.pointer("/quantity/significand").and_then(|x| x.as_i64().or_else(|| x.as_str().and_then(|t| t.parse().ok())));
    let exp = p.pointer("/quantity/exponent").and_then(|x| x.as_i64()).unwrap_or(0);
    match sig {
        Some(0) => Price::Free,
        Some(s) if s > 0 => {
            // value = s * 10^exp dollars → cents
            let e = exp + 2;
            let cents = if e >= 0 { s.saturating_mul(10i64.saturating_pow(e.min(12) as u32)) } else { s / 10i64.saturating_pow((-e).min(12) as u32) };
            Price::Cents(cents.max(0) as u64)
        }
        _ => Price::Unknown,
    }
}

fn votes_of(v: Option<&Value>) -> Votes {
    let Some(v) = v else { return Votes::default() };
    Votes {
        show: b(v.get("showVotes")),
        up: num(v.get("upVotes")).unwrap_or(0),
        down: num(v.get("downVotes")).unwrap_or(0),
        percent: num(v.get("upVotePercent")),
        count: num(v.get("voteCount")).unwrap_or(0),
        has_voted: b(v.get("hasVoted")),
        can_vote: b(v.get("canVote")),
    }
}

fn counts_of(v: Option<&Value>) -> Vec<(String, u64)> {
    v.and_then(|c| c.as_object())
        .map(|o| o.iter().filter_map(|(k, x)| Some((k.clone(), num(Some(x))?))).collect())
        .unwrap_or_default()
}

fn strings(v: Option<&Value>) -> Vec<String> {
    v.and_then(|a| a.as_array()).map(|a| a.iter().filter_map(|x| x.as_str().map(str::to_string)).collect()).unwrap_or_default()
}

// ---------------------------------------------------------------- parsers

/// `toolbox-service/v2/assets:search`.
pub fn parse_v2_search(v: &Value) -> Page {
    let mut page = Page::default();
    for e in arr(v, "creatorStoreAssets") {
        let Some(a) = e.get("asset") else { continue };
        let Some(id) = num(a.get("id")).filter(|i| *i > 0) else { continue };
        let c = e.get("creator");
        let creator = Creator {
            id: c.and_then(|c| num(c.get("userId")).or_else(|| num(c.get("groupId")))).unwrap_or(0),
            name: c.map(|c| s(c, "name")).unwrap_or_default(),
            kind: if c.and_then(|c| c.get("groupId")).is_some() { 2 } else { 1 },
            verified: b(c.and_then(|c| c.get("verified"))),
        };
        let audio = if a.get("durationSeconds").is_some() || a.get("audioType").is_some() || a.get("artist").is_some() {
            Some(AudioInfo {
                duration: fnum(a.get("durationSeconds")).unwrap_or(0.0),
                audio_type: s(a, "audioType"),
                artist: s(a, "artist"),
                title: s(a, "title"),
                album: s(a, "album"),
                genre: s(a, "genre"),
            })
        } else {
            None
        };
        let asset = Asset {
            id,
            name: s(a, "name"),
            description: s(a, "description"),
            type_id: num(a.get("assetTypeId")).unwrap_or(0) as u32,
            creator,
            votes: votes_of(e.get("voting")),
            price: price_of(e.pointer("/creatorStoreProduct/purchasePrice")),
            has_scripts: b(a.get("hasScripts")),
            script_count: num(a.get("scriptCount")).unwrap_or(0),
            created: s(a, "createTime"),
            updated: s(a, "updateTime"),
            category_path: s(a, "categoryPath"),
            sub_types: strings(a.get("subTypes")),
            endorsed: b(a.get("isEndorsed")),
            triangles: a.pointer("/objectMeshSummary/triangles").and_then(|x| num(Some(x))),
            vertices: a.pointer("/objectMeshSummary/vertices").and_then(|x| num(Some(x))),
            counts: counts_of(a.get("instanceCounts")),
            audio,
            detailed: true,
        };
        page.ids.push(id);
        page.assets.push(asset);
    }
    page.next = v.get("nextPageToken").and_then(|x| x.as_str()).filter(|x| !x.is_empty()).map(str::to_string);
    page.total = num(v.get("totalResults"));
    page.facets = v.pointer("/queryFacets/availableFacets").map(|f| strings(Some(f))).unwrap_or_default();
    page.filtered_keyword = v.get("filteredKeyword").and_then(|x| x.as_str()).map(str::to_string);
    page.corrected = corrected(v);
    page
}

fn corrected(v: &Value) -> Option<String> {
    let sc = v.get("spellCheckerResult")?;
    let state = num(sc.get("correctionState")).unwrap_or(0);
    let q = sc.get("correctedQuery").and_then(|x| x.as_str()).filter(|x| !x.is_empty())?;
    (state != 0).then(|| q.to_string())
}

/// The v1 lists (marketplace, inventory, recent, home sections,
/// creations): ids (and names for creations) in order.
pub fn parse_id_list(v: &Value) -> Page {
    let mut page = Page::default();
    for e in arr(v, "data") {
        let Some(id) = num(e.get("id")).filter(|i| *i > 0) else { continue };
        page.ids.push(id);
        if let Some(n) = e.get("name").and_then(|x| x.as_str()) {
            page.assets.push(Asset { id, name: n.to_string(), ..Default::default() });
        }
    }
    page.next = v.get("nextPageCursor").and_then(|x| x.as_str()).filter(|x| !x.is_empty()).map(str::to_string);
    page.total = num(v.get("totalResults"));
    page.facets = v.pointer("/queryFacets/availableFacets").map(|f| strings(Some(f))).unwrap_or_default();
    page.filtered_keyword = v.get("filteredKeyword").and_then(|x| x.as_str()).map(str::to_string);
    page.corrected = corrected(v);
    page
}

/// `toolbox-service/v1/items/details`.
pub fn parse_details(v: &Value) -> Vec<Asset> {
    let mut out = Vec::new();
    for e in arr(v, "data") {
        let Some(a) = e.get("asset") else { continue };
        let Some(id) = num(a.get("id")).filter(|i| *i > 0) else { continue };
        let c = e.get("creator");
        let ad = a.get("audioDetails");
        let dur = fnum(a.get("duration")).unwrap_or(0.0);
        let type_id = num(a.get("typeId")).unwrap_or(0) as u32;
        let audio = if ad.is_some() || type_id == 3 {
            let ad = ad.cloned().unwrap_or(Value::Null);
            Some(AudioInfo {
                duration: dur,
                audio_type: s(&ad, "audioType"),
                artist: s(&ad, "artist"),
                title: s(&ad, "title"),
                album: s(&ad, "musicAlbum"),
                genre: s(&ad, "musicGenre"),
            })
        } else {
            None
        };
        let fp = e.get("fiatProduct");
        let mut price = price_of(fp.and_then(|f| f.get("purchasePrice")));
        if fp.and_then(|f| f.get("isFree")).and_then(|x| x.as_bool()) == Some(true) {
            price = Price::Free;
        } else if fp.is_some() && fp.and_then(|f| f.get("purchasable")).and_then(|x| x.as_bool()) == Some(false) {
            price = Price::OffSale;
        }
        out.push(Asset {
            id,
            name: s(a, "name"),
            description: s(a, "description"),
            type_id,
            creator: Creator {
                id: c.and_then(|c| num(c.get("id"))).unwrap_or(0),
                name: c.map(|c| s(c, "name")).unwrap_or_default(),
                kind: c.and_then(|c| num(c.get("type"))).unwrap_or(1) as u8,
                verified: b(c.and_then(|c| c.get("isVerifiedCreator"))),
            },
            votes: votes_of(e.get("voting")),
            price,
            has_scripts: b(a.get("hasScripts")),
            script_count: a.pointer("/modelTechnicalDetails/instanceCounts/script").and_then(|x| num(Some(x))).unwrap_or(0),
            created: s(a, "createdUtc"),
            updated: s(a, "updatedUtc"),
            category_path: s(a, "categoryPath"),
            sub_types: strings(a.get("assetSubTypes")),
            endorsed: b(a.get("isEndorsed")),
            triangles: a.pointer("/modelTechnicalDetails/objectMeshSummary/triangles").and_then(|x| num(Some(x))),
            vertices: a.pointer("/modelTechnicalDetails/objectMeshSummary/vertices").and_then(|x| num(Some(x))),
            counts: counts_of(a.pointer("/modelTechnicalDetails/instanceCounts")),
            audio,
            detailed: true,
        });
    }
    out
}

/// `develop.roblox.com/v1/assets` (the fallback for asset types items/details
/// refuses, e.g. Animations): name, type, creator id, dates.
pub fn parse_develop(v: &Value) -> Vec<Asset> {
    arr(v, "data")
        .iter()
        .filter_map(|e| {
            let id = num(e.get("id")).filter(|i| *i > 0)?;
            let c = e.get("creator");
            Some(Asset {
                id,
                name: s(e, "name"),
                description: s(e, "description"),
                type_id: num(e.get("typeId")).unwrap_or(0) as u32,
                creator: Creator { id: c.and_then(|c| num(c.get("targetId"))).unwrap_or(0), name: String::new(), kind: c.and_then(|c| num(c.get("typeId"))).unwrap_or(1) as u8, verified: false },
                created: s(e, "created"),
                updated: s(e, "updated"),
                price: Price::Free,
                ..Default::default()
            })
        })
        .collect()
}

/// `thumbnails.roblox.com/v1/assets`: id → (state, url).
pub fn parse_thumbnails(v: &Value) -> Vec<(u64, String, Option<String>)> {
    arr(v, "data")
        .iter()
        .filter_map(|e| {
            let id = num(e.get("targetId"))?;
            let url = e.get("imageUrl").and_then(|x| x.as_str()).filter(|u| u.starts_with("https://")).map(str::to_string);
            Some((id, s(e, "state"), url))
        })
        .collect()
}

/// `autocomplete-studio/v2/suggest` → queries in order.
pub fn parse_autocomplete(v: &Value) -> Vec<String> {
    arr(v, "Data").iter().filter_map(|e| e.get("Query").and_then(|q| q.as_str()).map(str::to_string)).filter(|q| !q.trim().is_empty()).collect()
}

/// `creator-home-api/v1/groups` → (id, name).
pub fn parse_groups(v: &Value) -> Vec<(u64, String)> {
    arr(v, "groups").iter().filter_map(|g| Some((num(g.get("id"))?, s(g, "name")))).collect()
}

/// One home-configuration category (the category tree).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct HomeCategory {
    pub name: String,
    pub display: String,
    pub path: String,
    pub keyword: Option<String>,
    pub children: Vec<HomeCategory>,
}

/// `toolbox-service/v1/home/<type>/configuration`: the swimlane sections
/// (name, display name) and the category tree.
pub fn parse_home_config(v: &Value) -> (Vec<(String, String)>, Option<HomeCategory>) {
    let mut sections = Vec::new();
    let mut tree = None;
    for sct in arr(v, "sections") {
        let name = s(sct, "name");
        if let Some(sub) = sct.get("subcategory") {
            tree = Some(home_cat(sub, 0));
        } else if !name.is_empty() {
            sections.push((name, s(sct, "displayName")));
        }
    }
    (sections, tree)
}

fn home_cat(v: &Value, depth: u32) -> HomeCategory {
    let mut children: Vec<(u64, HomeCategory)> = Vec::new();
    if depth < 6 {
        if let Some(o) = v.get("children").and_then(|c| c.as_object()) {
            for c in o.values() {
                if b(c.get("hidden")) {
                    continue;
                }
                children.push((num(c.get("index")).unwrap_or(999), home_cat(c, depth + 1)));
            }
        }
    }
    children.sort_by_key(|c| c.0);
    HomeCategory {
        name: s(v, "name"),
        display: s(v, "displayName"),
        path: s(v, "categoryPath"),
        keyword: v.pointer("/queryParams/keyword").and_then(|k| k.as_str()).map(str::to_string),
        children: children.into_iter().map(|c| c.1).collect(),
    }
}

/// An error message from any reply shape Roblox uses: `{errors:[{message}]}`,
/// `{errors:{field:[msg]}}`, `{message}`, or plain text.
pub fn error_message(status: u16, body: &str) -> String {
    let v: Value = serde_json::from_str(body).unwrap_or(Value::Null);
    let msg = v
        .pointer("/errors/0/message")
        .and_then(|m| m.as_str())
        .map(str::to_string)
        .or_else(|| {
            v.get("errors").and_then(|e| e.as_object()).and_then(|o| {
                o.iter().next().map(|(k, m)| format!("{k}: {}", m.get(0).and_then(|x| x.as_str()).unwrap_or("")))
            })
        })
        .or_else(|| v.get("message").and_then(|m| m.as_str()).map(str::to_string))
        .filter(|m| !m.trim().is_empty())
        .unwrap_or_else(|| body.chars().filter(|c| !c.is_control()).take(120).collect());
    match status {
        401 => "You are signed out.".into(),
        429 => "Roblox is busy (too many requests). Retrying…".into(),
        _ if msg.is_empty() => format!("HTTP {status}"),
        _ => format!("HTTP {status}: {msg}"),
    }
}

/// "4K", "1.2M", Studio's short vote counts.
pub fn short_count(n: u64) -> String {
    match n {
        0..=999 => n.to_string(),
        1000..=999_999 => {
            let k = n as f64 / 1000.0;
            if k < 10.0 && (k * 10.0).round() % 10.0 != 0.0 { format!("{:.1}K", k) } else { format!("{}K", k.round() as u64) }
        }
        _ => {
            let m = n as f64 / 1_000_000.0;
            if m < 10.0 && (m * 10.0).round() % 10.0 != 0.0 { format!("{:.1}M", m) } else { format!("{}M", m.round() as u64) }
        }
    }
}

/// "2019-06-03T…" → "Jun 3, 2019".
pub fn short_date(iso: &str) -> String {
    let p: Vec<&str> = iso.get(..10).unwrap_or("").split('-').collect();
    const M: [&str; 12] = ["Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec"];
    match (p.first().and_then(|y| y.parse::<u32>().ok()), p.get(1).and_then(|m| m.parse::<usize>().ok()), p.get(2).and_then(|d| d.parse::<u32>().ok())) {
        (Some(y), Some(m @ 1..=12), Some(d)) => format!("{} {d}, {y}", M[m - 1]),
        _ => iso.to_string(),
    }
}

/// "m:ss" for audio.
pub fn duration_label(secs: f64) -> String {
    let s = secs.max(0.0).round() as u64;
    format!("{}:{:02}", s / 60, s % 60)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture(name: &str) -> Value {
        let p = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("checks/toolbox/fixtures").join(name);
        serde_json::from_str(&std::fs::read_to_string(&p).unwrap_or_else(|e| panic!("{}: {e}", p.display()))).unwrap()
    }

    #[test]
    fn v2_search_tree_matches_studio_order() {
        let p = parse_v2_search(&fixture("v2_search_model_tree.json"));
        let names: Vec<&str> = p.assets.iter().take(4).map(|a| a.name.as_str()).collect();
        assert_eq!(names, ["[NEW!] Dead Forest Tree Model", "Tree", "Realistic Trees", "Tree House"]);
        assert_eq!(p.total, Some(1000));
        assert!(p.next.is_some());
        assert!(p.facets.contains(&"pine".to_string()));
        let t = &p.assets[1];
        assert_eq!(t.creator.name, "SheriffTaco");
        assert!(t.creator.verified);
        assert_eq!(t.price, Price::Free);
        assert_eq!(t.votes.percent, Some(95));
        assert_eq!(t.count("meshPart"), Some(36));
        assert_eq!(t.category_path, "3d__nature");
    }

    #[test]
    fn v2_other_categories() {
        let a = parse_v2_search(&fixture("v2_search_audio_rain.json"));
        let au = a.assets[0].audio.as_ref().unwrap();
        assert_eq!(au.artist, "Parry Gripp");
        assert_eq!(au.duration, 95.0);
        for f in ["v2_search_plugin_building.json", "v2_search_decal_grass.json", "v2_search_meshpart_rock.json", "v2_search_font_round.json"] {
            let p = parse_v2_search(&fixture(f));
            assert!(!p.assets.is_empty(), "{f}");
            assert!(p.assets.iter().all(|a| a.id > 0 && a.type_id > 0), "{f}");
        }
        // errors as an object of field lists
        let e = fixture("v2_search_error_animation.json");
        assert!(parse_v2_search(&e).assets.is_empty());
        assert!(error_message(400, &e.to_string()).contains("searchCategoryType"));
    }

    #[test]
    fn v1_lists_and_details() {
        let p = parse_id_list(&fixture("v1_models_tree.json"));
        assert_eq!(p.ids.len(), 10);
        assert!(p.next.is_some());
        let inv = parse_id_list(&fixture("v1_inventory_model.json"));
        assert_eq!(inv.ids.len(), 30);
        let rec = parse_id_list(&fixture("v1_recent_model.json"));
        assert!(rec.next.is_none() && !rec.ids.is_empty());
        let cr = parse_id_list(&fixture("v1_creations_model.json"));
        assert_eq!(cr.assets.len(), cr.ids.len());
        assert_eq!(cr.assets[0].name, "Creation 1");
        assert!(parse_id_list(&fixture("v1_marketplace_video_empty.json")).ids.is_empty());
        let d = parse_details(&fixture("items_details.json"));
        let tree = d.iter().find(|a| a.id == 580221169).unwrap();
        assert_eq!(tree.price, Price::Free);
        assert_eq!(tree.triangles, Some(13080));
        let tacos = d.iter().find(|a| a.id == 142376088).unwrap();
        assert_eq!(tacos.audio.as_ref().unwrap().duration, 95.0);
        assert_eq!(tacos.type_id, 3);
    }

    #[test]
    fn small_replies() {
        let t = parse_thumbnails(&fixture("thumbnails_assets.json"));
        assert!(t.iter().any(|(id, st, u)| *id == 580221169 && st == "Completed" && u.is_some()));
        assert_eq!(parse_autocomplete(&fixture("autocomplete_tre.json"))[0], "tree");
        assert!(!parse_groups(&fixture("creator_home_groups.json")).is_empty());
        let (sections, tree) = parse_home_config(&fixture("home_configuration_model.json"));
        assert!(sections.iter().any(|s| s.0 == "trending"));
        let tree = tree.unwrap();
        let d3 = &tree.children[0];
        assert_eq!(d3.display, "3D Assets");
        assert_eq!(d3.children[0].display, "Characters");
        assert_eq!(d3.children[0].path, "3d__characters");
        assert_eq!(parse_id_list(&fixture("home_section_trending.json")).ids.len(), 10);
        let d = parse_develop(&fixture("develop_assets_animation.json"));
        assert_eq!((d[0].name.as_str(), d[0].type_id, d[0].creator.kind), ("WalkLeftAnim", 24, 2));
        assert_eq!(error_message(500, &fixture("error_empty_message.json").to_string()), "HTTP 500: {\"errors\":[{\"code\":0,\"message\":\"\"}]}");
    }

    #[test]
    fn malformed_shapes_never_panic() {
        let shapes = [
            "null", "1", "\"x\"", "[]", "{}", "{\"data\":null}", "{\"data\":{}}", "{\"data\":[1,null,\"a\",{}]}",
            "{\"data\":[{\"id\":\"12\"},{\"id\":-3},{\"id\":1e400}]}", "{\"creatorStoreAssets\":[{\"asset\":{}},{\"asset\":{\"id\":5,\"name\":7}}]}",
            "{\"creatorStoreAssets\":{}}", "{\"data\":[{\"asset\":{\"id\":9},\"creator\":5,\"voting\":[],\"fiatProduct\":\"x\"}]}",
            "{\"sections\":[{\"subcategory\":{\"children\":{\"a\":{\"children\":7}}}}]}", "{\"Data\":[{\"Query\":5}]}",
            "{\"nextPageToken\":5,\"totalResults\":\"7\",\"queryFacets\":{\"availableFacets\":[1,\"a\"]}}",
            "{\"creatorStoreAssets\":[{\"asset\":{\"id\":1},\"creatorStoreProduct\":{\"purchasePrice\":{\"quantity\":{\"significand\":999999999999999999,\"exponent\":99}}}}]}",
        ];
        for t in shapes {
            let v: Value = serde_json::from_str(t).unwrap_or(Value::Null);
            let _ = parse_v2_search(&v);
            let _ = parse_id_list(&v);
            let _ = parse_details(&v);
            let _ = parse_thumbnails(&v);
            let _ = parse_autocomplete(&v);
            let _ = parse_groups(&v);
            let _ = parse_develop(&v);
            let _ = parse_home_config(&v);
            let _ = error_message(400, t);
        }
        let v: Value = serde_json::from_str("{\"data\":[{\"id\":\"12\"}]}").unwrap();
        assert_eq!(parse_id_list(&v).ids, vec![12]);
        let v: Value = serde_json::from_str("{\"data\":[{\"asset\":{\"id\":9},\"creator\":5}]}").unwrap();
        assert_eq!(parse_details(&v)[0].id, 9);
    }

    #[test]
    fn labels() {
        assert_eq!(short_count(4000), "4K");
        assert_eq!(short_count(7000), "7K");
        assert_eq!(short_count(1260), "1.3K");
        assert_eq!(short_count(600), "600");
        assert_eq!(short_count(2_500_000), "2.5M");
        assert_eq!(short_date("2019-06-03T12:00:00Z"), "Jun 3, 2019");
        assert_eq!(short_date("junk"), "junk");
        assert_eq!(duration_label(95.0), "1:35");
        let p: Value = serde_json::from_str("{\"currencyCode\":\"USD\",\"quantity\":{\"significand\":999,\"exponent\":-2}}").unwrap();
        assert_eq!(price_of(Some(&p)), Price::Cents(999));
        assert_eq!(Price::Cents(999).label(true), "$9.99");
        let p: Value = serde_json::from_str("{\"quantity\":{\"significand\":5,\"exponent\":0}}").unwrap();
        assert_eq!(price_of(Some(&p)), Price::Cents(500));
        let priced = parse_v2_search(&fixture("v2_search_model_priced.json"));
        assert_eq!(priced.assets[0].price, Price::Cents(299));
        assert!(priced.assets.iter().all(|a| matches!(a.price, Price::Cents(_))));
    }
}
