//! What the character builder lists: saved outfits, outfit and user avatar
//! descriptions, player search, friends and avatar thumbnails, from Roblox's
//! documented web APIs over the signed-in session (read-only).

use crate::api::agent;
use crate::session::{session_agent, Session};
use crate::{Error, Result};

/// A person the builder can show (search result, friend).
#[derive(Debug, Clone)]
pub struct Person {
    pub id: u64,
    pub name: String,
    pub display: String,
}

/// An outfit: one of Studio's preset outfits or one the person saved.
#[derive(Debug, Clone)]
pub struct Outfit {
    pub id: u64,
    pub name: String,
}

fn json(s: &Session, url: &str) -> Result<serde_json::Value> {
    let (status, text) = s.get(url)?;
    if status != 200 {
        return Err(Error(format!("{url} returned HTTP {status}")));
    }
    Ok(serde_json::from_str(&text)?)
}

/// An outfit's description (`avatar.roblox.com/v1/outfits/{id}/details`):
/// assets, bodyColors (BrickColor ids), `scale` and playerAvatarType; the
/// same shape `avatar::parse_avatar` reads once `scale` is renamed.
pub fn outfit_details(s: &Session, id: u64) -> Result<serde_json::Value> {
    let mut v = json(s, &format!("https://avatar.roblox.com/v1/outfits/{id}/details"))?;
    if let Some(scale) = v.get("scale").cloned() {
        v["scales"] = scale;
    }
    Ok(v)
}

/// Anyone's current avatar (`avatar.roblox.com/v1/users/{id}/avatar`).
pub fn user_avatar(s: &Session, user_id: u64) -> Result<serde_json::Value> {
    json(s, &format!("https://avatar.roblox.com/v1/users/{user_id}/avatar"))
}

/// The person's saved avatar outfits (first 50).
pub fn outfits(s: &Session, user_id: u64) -> Result<Vec<Outfit>> {
    let v = json(
        s,
        &format!("https://avatar.roblox.com/v2/avatar/users/{user_id}/outfits?page=1&itemsPerPage=50&isEditable=true"),
    )?;
    Ok(v.get("data")
        .and_then(|d| d.as_array())
        .into_iter()
        .flatten()
        .filter(|o| o.get("outfitType").and_then(|t| t.as_str()).is_none_or(|t| t == "Avatar"))
        .filter_map(|o| {
            Some(Outfit {
                id: o.get("id")?.as_u64()?,
                name: o.get("name").and_then(|n| n.as_str()).unwrap_or("").to_string(),
            })
        })
        .collect())
}

fn people(v: &serde_json::Value) -> Vec<Person> {
    v.get("data")
        .and_then(|d| d.as_array())
        .into_iter()
        .flatten()
        .filter_map(|p| {
            Some(Person {
                id: p.get("id")?.as_u64()?,
                name: p.get("name").and_then(|n| n.as_str()).unwrap_or("").to_string(),
                display: p.get("displayName").and_then(|n| n.as_str()).unwrap_or("").to_string(),
            })
        })
        .collect()
}

/// Players matching `query`: an exact username or user id first, then
/// Roblox's keyword search.
pub fn search(s: &Session, query: &str) -> Result<Vec<Person>> {
    let q = query.trim();
    let mut out: Vec<Person> = Vec::new();
    if let Ok(id) = q.parse::<u64>() {
        out.extend(names(&[id]).unwrap_or_default());
    }
    if let Ok(id) = crate::avatar::user_id_for(q) {
        out.extend(names(&[id]).unwrap_or_default());
    }
    let enc: String = q
        .bytes()
        .map(|b| match b {
            b'a'..=b'z' | b'A'..=b'Z' | b'0'..=b'9' | b'_' | b'-' | b'.' => (b as char).to_string(),
            _ => format!("%{b:02X}"),
        })
        .collect();
    if q.len() >= 3 {
        if let Ok(v) = json(s, &format!("https://users.roblox.com/v1/users/search?keyword={enc}&limit=25")) {
            out.extend(people(&v));
        }
    }
    let mut seen = std::collections::HashSet::new();
    out.retain(|p| seen.insert(p.id));
    Ok(out)
}

/// Names for user ids (`POST users.roblox.com/v1/users`, public).
pub fn names(ids: &[u64]) -> Result<Vec<Person>> {
    if ids.is_empty() {
        return Ok(Vec::new());
    }
    let mut out = Vec::new();
    for chunk in ids.chunks(100) {
        let body = serde_json::json!({ "userIds": chunk, "excludeBannedUsers": true });
        let mut r = agent().post("https://users.roblox.com/v1/users").send_json(&body)?;
        let text = r.body_mut().read_to_string()?;
        let v: serde_json::Value = serde_json::from_str(&text)?;
        out.extend(people(&v));
    }
    // In the order asked.
    out.sort_by_key(|p| ids.iter().position(|i| *i == p.id).unwrap_or(usize::MAX));
    Ok(out)
}

/// The person's friends, with names.
pub fn friends(s: &Session, user_id: u64) -> Result<Vec<Person>> {
    let v = json(s, &format!("https://friends.roblox.com/v1/users/{user_id}/friends"))?;
    let ids: Vec<u64> = people(&v).into_iter().map(|p| p.id).collect();
    let mut list = names(&ids)?;
    list.sort_by_key(|p| p.display.to_lowercase());
    Ok(list)
}

/// Which avatar thumbnail.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Thumb {
    /// A user's full-body avatar.
    User,
    /// An outfit's full body.
    Outfit,
    /// A catalog asset (emotes, accessories).
    Asset,
}

/// Full-body thumbnails (150x150 PNG) for up to 100 ids. Ids whose
/// thumbnail isn't ready (state Pending / Blocked) are left out.
pub fn thumbnails(kind: Thumb, ids: &[u64]) -> Result<Vec<(u64, Vec<u8>)>> {
    if ids.is_empty() {
        return Ok(Vec::new());
    }
    let agent = session_agent();
    let mut out = Vec::new();
    for chunk in ids.chunks(100) {
        let list: Vec<String> = chunk.iter().map(|i| i.to_string()).collect();
        let url = match kind {
            Thumb::User => format!(
                "https://thumbnails.roblox.com/v1/users/avatar?userIds={}&size=150x150&format=Png&isCircular=false",
                list.join(",")
            ),
            Thumb::Outfit => format!(
                "https://thumbnails.roblox.com/v1/users/outfits?userOutfitIds={}&size=150x150&format=Png&isCircular=false",
                list.join(",")
            ),
            Thumb::Asset => format!(
                "https://thumbnails.roblox.com/v1/assets?assetIds={}&size=150x150&format=Png&isCircular=false",
                list.join(",")
            ),
        };
        let text = agent.get(&url).call()?.body_mut().read_to_string()?;
        let v: serde_json::Value = serde_json::from_str(&text)?;
        let urls: Vec<(u64, String)> = v
            .get("data")
            .and_then(|d| d.as_array())
            .into_iter()
            .flatten()
            .filter_map(|t| {
                Some((
                    t.get("targetId")?.as_u64()?,
                    t.get("imageUrl")?.as_str().filter(|u| !u.is_empty())?.to_string(),
                ))
            })
            .collect();
        // The images in parallel (8 at a time).
        let got = std::sync::Mutex::new(Vec::new());
        std::thread::scope(|sc| {
            for part in urls.chunks(urls.len().div_ceil(8).max(1)) {
                let (agent, got) = (&agent, &got);
                sc.spawn(move || {
                    for (id, img) in part {
                        if let Ok(mut r) = agent.get(img.as_str()).call() {
                            if r.status().as_u16() == 200 {
                                if let Ok(bytes) = r.body_mut().with_config().limit(4 << 20).read_to_vec() {
                                    got.lock().unwrap().push((*id, bytes));
                                }
                            }
                        }
                    }
                });
            }
        });
        out.extend(got.into_inner().unwrap());
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn outfit_scale_reads_as_scales() {
        let mut v: serde_json::Value = serde_json::from_str(
            r#"{"assets":[{"id":1,"assetType":{"id":27,"name":"Torso"}}],
                "bodyColors":{"headColorId":5},"scale":{"height":1.05},"playerAvatarType":"R6"}"#,
        )
        .unwrap();
        if let Some(scale) = v.get("scale").cloned() {
            v["scales"] = scale;
        }
        let a = crate::avatar::parse_avatar(1, &v);
        assert!(!a.r15);
        assert!((a.height - 1.05).abs() < 1e-6);
        assert_eq!(a.assets[0].type_name, "Torso");
    }
}

/// A game's avatar rules as the engine reads them at join
/// (`avatar.roblox.com/v1/game-start-info?universeId=…`):
/// allowCustomAnimations ("True" / "False" = Game Settings' Player Choice /
/// Standard) and universeAvatarAssetOverrides (asset type, asset id,
/// isPlayerChoice).
pub fn game_start_info(universe_id: u64) -> Result<serde_json::Value> {
    let mut r = agent()
        .get(&format!("https://avatar.roblox.com/v1/game-start-info?universeId={universe_id}"))
        .call()?;
    let text = r.body_mut().read_to_string()?;
    Ok(serde_json::from_str(&text)?)
}
