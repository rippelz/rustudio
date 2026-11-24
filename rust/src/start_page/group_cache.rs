//! The Start Page's group lists from the last session, per account:
//! `~/.cache/rustudio/start-groups-<userId>.json` (RUSTUDIO_CACHE_DIR moves
//! it with the place cache). Shown at startup while the lists refresh, so
//! the groups don't wait on ~90 requests.

use std::path::PathBuf;

use rustudio_account::cloud::GroupInfo;
use rustudio_account::Experience;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CachedGroup {
    pub id: u64,
    pub name: String,
    pub role: String,
    /// The group's first page (only groups that had any are kept).
    pub experiences: Vec<Experience>,
    /// Roblox had more pages after it.
    #[serde(default)]
    pub more: bool,
}

impl CachedGroup {
    pub fn info(&self) -> GroupInfo {
        GroupInfo { id: self.id, name: self.name.clone(), role: self.role.clone(), can_edit: None }
    }
}

#[derive(Serialize, Deserialize)]
struct File {
    version: u32,
    groups: Vec<CachedGroup>,
}

const VERSION: u32 = 1;

fn path(uid: u64) -> PathBuf {
    let places = rustudio_account::cloud::PlaceCache::default().root;
    places
        .parent()
        .map(|p| p.to_path_buf())
        .unwrap_or_else(|| PathBuf::from("."))
        .join(format!("start-groups-{uid}.json"))
}

pub fn load(uid: u64) -> Option<Vec<CachedGroup>> {
    load_from(&path(uid))
}

fn load_from(p: &std::path::Path) -> Option<Vec<CachedGroup>> {
    let f: File = serde_json::from_slice(&std::fs::read(p).ok()?).ok()?;
    (f.version == VERSION).then_some(f.groups)
}

/// Keeps the groups whose first page loaded with experiences, in order.
/// A group whose page failed keeps its cached entry, so one refused
/// request doesn't drop it from the next startup.
pub fn save(uid: u64, groups: &[GroupInfo], pages: &[Result<(Vec<Experience>, bool), String>]) {
    save_to(&path(uid), groups, pages);
}

fn save_to(p: &std::path::Path, groups: &[GroupInfo], pages: &[Result<(Vec<Experience>, bool), String>]) {
    let old = load_from(p).unwrap_or_default();
    let keep: Vec<CachedGroup> = groups
        .iter()
        .zip(pages)
        .filter_map(|(g, r)| match r {
            Ok((rows, _)) if rows.is_empty() => None,
            Ok((rows, more)) => Some(CachedGroup {
                id: g.id,
                name: g.name.clone(),
                role: g.role.clone(),
                experiences: rows.clone(),
                more: *more,
            }),
            Err(_) => old.iter().find(|c| c.id == g.id).cloned(),
        })
        .collect();
    let Ok(text) = serde_json::to_vec(&File { version: VERSION, groups: keep }) else { return };
    if let Some(dir) = p.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    // Written whole, then renamed: a second rustudio reading it never sees half.
    let tmp = p.with_extension(format!("json.{}.tmp", std::process::id()));
    if std::fs::write(&tmp, text).is_ok() && std::fs::rename(&tmp, p).is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn exp(id: u64) -> Experience {
        Experience {
            id,
            name: format!("e{id}"),
            root_place_id: Some(id + 1),
            creator_type: rustudio_account::CreatorKind::Group,
            creator_id: 9,
            creator_name: "g".into(),
            updated: String::new(),
            team_create: None,
        }
    }

    fn group(id: u64) -> GroupInfo {
        GroupInfo { id, name: format!("g{id}"), role: "r".into(), can_edit: None }
    }

    #[test]
    fn keeps_groups_with_experiences_and_failed_ones_from_before() {
        let dir = std::env::temp_dir().join(format!("rustudio-group-cache-{}", std::process::id()));
        let p = dir.join("c.json");
        let groups = [group(1), group(2), group(3)];
        save_to(&p, &groups, &[Ok((vec![exp(10)], false)), Ok((vec![], false)), Ok((vec![exp(30)], true))]);
        let c = load_from(&p).unwrap();
        assert_eq!(c.iter().map(|g| (g.id, g.experiences.len(), g.more)).collect::<Vec<_>>(), vec![(1, 1, false), (3, 1, true)]);
        // Group 3's request fails next time: its cached page stays.
        save_to(&p, &groups, &[Ok((vec![exp(10), exp(11)], false)), Ok((vec![], false)), Err("429".into())]);
        let c = load_from(&p).unwrap();
        assert_eq!(c.iter().map(|g| (g.id, g.experiences.len())).collect::<Vec<_>>(), vec![(1, 2), (3, 1)]);
        assert_eq!(c[0].info(), group(1));
        let _ = std::fs::remove_dir_all(dir);
    }
}
