//! The game's own animation rules for Play characters: the published
//! experience's Game Settings (game-start-info: Standard animations and
//! per-slot animation overrides) and a place file's AvatarSettings
//! animation rules (AnimationPacksMode, …AnimationOverride package ids,
//! AnimationClipsMode CustomClips with Custom…AnimationId clips).

use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};

use rbx_dom_weak::types::Variant;

use crate::assets::SharedClient;
use crate::doc::Document;

/// Enum.AssetType names of animation packages by id (game-start-info
/// overrides name types by number).
fn anim_kind(type_id: u64) -> Option<&'static str> {
    Some(match type_id {
        48 => "ClimbAnimation",
        50 => "FallAnimation",
        51 => "IdleAnimation",
        52 => "JumpAnimation",
        53 => "RunAnimation",
        54 => "SwimAnimation",
        55 => "WalkAnimation",
        78 => "MoodAnimation",
        _ => return None,
    })
}

/// What the game says about animations.
#[derive(Debug, Clone, Default)]
pub struct GameAnimRules {
    /// Players keep their own animation packs (Player Choice).
    pub player_packs: bool,
    /// The published game's per-slot package overrides (Game Settings →
    /// Avatar asset overrides): (AssetType name, package asset id; 0 = the
    /// default animation).
    pub overrides: Vec<(String, u64)>,
    /// The place's animation overrides: Animate slot → animation ids (idle
    /// may have several).
    pub clips: Vec<(&'static str, Vec<u64>)>,
}

fn web_cache() -> &'static Mutex<HashMap<u64, Option<serde_json::Value>>> {
    static C: OnceLock<Mutex<HashMap<u64, Option<serde_json::Value>>>> = OnceLock::new();
    C.get_or_init(Default::default)
}

fn package_cache() -> &'static Mutex<HashMap<u64, Option<Vec<u8>>>> {
    static C: OnceLock<Mutex<HashMap<u64, Option<Vec<u8>>>>> = OnceLock::new();
    C.get_or_init(Default::default)
}

/// The published experience's rules (cached per universe for the run).
fn published(universe: u64) -> Option<serde_json::Value> {
    if let Some(v) = web_cache().lock().ok()?.get(&universe) {
        return v.clone();
    }
    let v = rustudio_account::characters::game_start_info(universe).ok();
    web_cache().lock().ok()?.insert(universe, v.clone());
    v
}

fn int(doc: &Document, r: rbx_dom_weak::types::Ref, k: &str) -> i64 {
    match doc.prop(r, k) {
        Some(Variant::Int64(v)) => v,
        Some(Variant::Int32(v)) => v as i64,
        Some(Variant::Float64(v)) => v as i64,
        Some(Variant::Float32(v)) => v as i64,
        _ => 0,
    }
}

fn flag(doc: &Document, r: rbx_dom_weak::types::Ref, k: &str) -> bool {
    matches!(doc.prop(r, k), Some(Variant::Bool(true)))
}

/// The rules for the open place: the place file's AvatarSettings first
/// (it's what this copy of the game will publish), then the published
/// experience's Game Settings for a place opened from Roblox.
pub fn rules(doc: &Document) -> GameAnimRules {
    let mut out = GameAnimRules {
        player_packs: crate::avatar::place_allows_animation_packs(doc),
        ..Default::default()
    };
    let root = doc.dom.root_ref();
    let local = doc.find_child(root, "AvatarSettings").and_then(|s| {
        doc.dom
            .descendants_of(s)
            .find(|i| i.class == "AvatarAnimationRules")
            .map(|i| i.referent())
    });
    if let Some(r) = local {
        // Current files: …AnimationOverride animation ids per slot (0 = the
        // player's / default). Older files: AnimationClipsMode CustomClips
        // with Custom…AnimationEnabled + Custom…AnimationId, which the API
        // dump marks deprecated in favour of the overrides.
        let legacy = crate::scene::enum_u32(doc.prop(r, "AnimationClipsMode")).unwrap_or(0) == 1;
        let clip = |now: &str, old: &str| -> Option<u64> {
            let id = int(doc, r, &format!("{now}AnimationOverride"));
            if id > 0 {
                return Some(id as u64);
            }
            let id = int(doc, r, &format!("Custom{old}AnimationId"));
            (legacy && flag(doc, r, &format!("Custom{old}AnimationEnabled")) && id > 0).then_some(id as u64)
        };
        let mut idles: Vec<u64> = [("Idle", "Idle"), ("IdleAlt", "IdleAlt1")].iter().filter_map(|(n, o)| clip(n, o)).collect();
        if legacy {
            let id = int(doc, r, "CustomIdleAlt2AnimationId");
            if flag(doc, r, "CustomIdleAlt2AnimationEnabled") && id > 0 {
                idles.push(id as u64);
            }
        }
        if !idles.is_empty() {
            out.clips.push(("idle", idles));
        }
        for (name, slot) in [
            ("Walk", "walk"),
            ("Run", "run"),
            ("Jump", "jump"),
            ("Fall", "fall"),
            ("Climb", "climb"),
            ("Swim", "swim"),
            ("SwimIdle", "swimidle"),
            ("Mood", "mood"),
        ] {
            if let Some(id) = clip(name, name) {
                out.clips.push((slot, vec![id]));
            }
        }
    }
    if let Some(v) = doc.cloud.as_ref().and_then(|c| published(c.universe_id)) {
        if v.get("allowCustomAnimations").and_then(|a| a.as_str()).is_some_and(|a| a.eq_ignore_ascii_case("false")) {
            out.player_packs = false;
        }
        for o in v.get("universeAvatarAssetOverrides").and_then(|a| a.as_array()).into_iter().flatten() {
            let choice = o.get("isPlayerChoice").and_then(|b| b.as_bool()).unwrap_or(true);
            let Some(kind) = o.get("assetTypeID").and_then(|t| t.as_u64()).and_then(anim_kind) else {
                continue;
            };
            if choice {
                continue;
            }
            let id = o.get("assetID").and_then(|i| i.as_u64()).unwrap_or(0);
            out.overrides.push((kind.to_string(), id));
        }
    }
    out
}

/// An animation package's .rbxm (cached; None when it can't be had).
pub fn package(client: Option<&SharedClient>, id: u64) -> Option<Vec<u8>> {
    if let Some(v) = package_cache().lock().ok()?.get(&id) {
        return v.clone();
    }
    let bytes = client.and_then(|c| {
        let guard = c.lock().ok()?;
        guard.as_ref()?.download_asset(id, None).ok()
    });
    let bytes = bytes.or_else(|| rustudio_account::public_asset(id).ok());
    package_cache().lock().ok()?.insert(id, bytes.clone());
    bytes
}

#[cfg(test)]
mod tests {
    use super::*;
    use rbx_dom_weak::{InstanceBuilder, WeakDom};

    fn place(rules: InstanceBuilder) -> Document {
        let dom = WeakDom::new(InstanceBuilder::new("DataModel").with_child(
            InstanceBuilder::new("AvatarSettings").with_name("AvatarSettings").with_child(
                InstanceBuilder::new("AvatarRules").with_name("DefaultAvatarRules").with_child(rules),
            ),
        ));
        let mut bytes = Vec::new();
        rbx_binary::to_writer(&mut bytes, &dom, dom.root().children()).unwrap();
        let mut tmp = Document::decode(&bytes).unwrap();
        let mut d = Document::empty();
        let root = d.dom.root_ref();
        for c in tmp.root().children().to_vec() {
            tmp.transfer(c, &mut d.dom, root);
        }
        d
    }

    #[test]
    fn overrides_and_legacy_clips() {
        let d = place(
            InstanceBuilder::new("AvatarAnimationRules")
                .with_name("AvatarAnimationRules")
                .with_property("IdleAnimationOverride", 11i64)
                .with_property("IdleAltAnimationOverride", 12i64)
                .with_property("RunAnimationOverride", 13i64)
                .with_property("AnimationPacksMode", rbx_dom_weak::types::Enum::from_u32(1)),
        );
        let r = rules(&d);
        assert!(!r.player_packs);
        assert_eq!(r.clips, vec![("idle", vec![11, 12]), ("run", vec![13])]);
        let d = place(
            InstanceBuilder::new("AvatarAnimationRules")
                .with_name("AvatarAnimationRules")
                .with_property("AnimationClipsMode", rbx_dom_weak::types::Enum::from_u32(1))
                .with_property("CustomWalkAnimationEnabled", true)
                .with_property("CustomWalkAnimationId", 21i64)
                .with_property("CustomJumpAnimationId", 22i64),
        );
        let r = rules(&d);
        assert!(r.player_packs);
        assert_eq!(r.clips, vec![("walk", vec![21])]);
    }
}
