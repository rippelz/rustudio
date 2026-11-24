//! Engine API domain: players_chat (Player, Players, StarterPlayer, Teams,
//! the legacy Chat service, TextChatService and its whole TextChat stack,
//! LocalizationService / LocalizationTable / Translator, PlayerViewService).
//! See crates/rustudio-engine/src/domains/mod.rs for the hook contract and
//! checks/parity/players_chat.md for what was measured in Studio.
//!
//! Studio-mode behaviour, where Roblox's backend can't be used for real:
//! - Bans (Players:BanAsync / UnbanAsync) validate their config exactly as
//!   Studio does and then skip the request, printing Studio's "will succeed
//!   on production game servers" line; GetBanHistoryAsync prints its line
//!   and fails as in a test environment. Nothing is ever sent to Roblox.
//! - Groups / friends / user lookups go through the signed-in login session
//!   (players_chat_web.rs), read-only, yielding like Studio.
//! - Chat isn't filtered (Studio has no filter); chat never leaves the
//!   machine: SendAsync crosses from the client runtime to the server one
//!   inside the Play session (play_game.rs) and back.
//! - Legacy DataPersistence (Player:SaveX / LoadX) warns it's deprecated
//!   and keeps nothing, as Studio.
#![allow(unused_imports, unused_variables)]

use std::sync::Mutex;

use mlua::{Function, Lua, MultiValue, Table, Value};
use rbx_dom_weak::types::{Ref, Variant};

use crate::runtime::*;

#[path = "players_chat_web.rs"]
mod web;
#[path = "players_chat_loc.rs"]
pub mod loc;

const LUAU: &str = include_str!("players_chat.luau");
const LUAU_UI: &str = include_str!("players_chat_ui.luau");

pub const METHODS: &[(&str, &str)] = &[
    // Player (Rust: no yield)
    ("AddReplicationFocus", "Player"),
    ("RemoveReplicationFocus", "Player"),
    ("ClearCachedAvatarAppearance", "Player"),
    ("GetCameraState", "Player"),
    ("GetData", "Player"),
    ("GetJoinData", "Player"),
    ("IsVerified", "Player"),
    ("PromptAgeCheck", "Player"),
    ("SetSuperSafeChat", "Player"),
    ("LoadCharacterAppearance", "Player"),
    ("LoadBoolean", "Player"),
    ("LoadNumber", "Player"),
    ("LoadString", "Player"),
    ("LoadInstance", "Player"),
    ("SaveBoolean", "Player"),
    ("SaveNumber", "Player"),
    ("SaveString", "Player"),
    ("SaveInstance", "Player"),
    ("loadBoolean", "Player"),
    ("loadNumber", "Player"),
    ("loadString", "Player"),
    ("loadInstance", "Player"),
    ("saveBoolean", "Player"),
    ("saveNumber", "Player"),
    ("saveString", "Player"),
    ("saveInstance", "Player"),
    ("WaitForDataReady", "Player"),
    ("waitForDataReady", "Player"),
    // Players
    ("GetPlayerFromCharacter", "Players"),
    ("getPlayerFromCharacter", "Players"),
    ("playerFromCharacter", "Players"),
    ("GetUserThumbnailAsync", "Players"),
    ("GetCharacterAppearanceAsync", "Players"),
    ("GetCharacterAppearanceInfoAsync", "Players"),
    ("GetProfileConfigurationFromUserIdAsync", "Players"),
    ("GetHumanoidDescriptionFromUserIdAsync", "Players"),
    ("GetHumanoidDescriptionFromOutfitIdAsync", "Players"),
    ("CreateHumanoidModelFromDescriptionAsync", "Players"),
    ("CreateHumanoidModelFromUserIdAsync", "Players"),
    // Teams
    ("RebalanceTeams", "Teams"),
    // PlayerViewService
    ("GetDeviceCameraCFrame", "PlayerViewService"),
    // LocalizationTable / Translator / LocalizationService (players_chat_loc.rs)
    ("GetContents", "LocalizationTable"),
    ("SetContents", "LocalizationTable"),
    ("GetEntries", "LocalizationTable"),
    ("SetEntries", "LocalizationTable"),
    ("GetString", "LocalizationTable"),
    ("GetTranslator", "LocalizationTable"),
    ("SetEntry", "LocalizationTable"),
    ("SetEntryValue", "LocalizationTable"),
    ("SetEntryKey", "LocalizationTable"),
    ("SetEntrySource", "LocalizationTable"),
    ("SetEntryContext", "LocalizationTable"),
    ("SetEntryExample", "LocalizationTable"),
    ("RemoveEntry", "LocalizationTable"),
    ("RemoveEntryValue", "LocalizationTable"),
    ("RemoveKey", "LocalizationTable"),
    ("RemoveTargetLocale", "LocalizationTable"),
    ("FormatByKey", "Translator"),
    ("Translate", "Translator"),
    ("RobloxOnlyTranslate", "Translator"),
    ("GetTableEntries", "LocalizationService"),
    ("GetCorescriptLocalizations", "LocalizationService"),
    ("GetIsLoadingInternalTranslations", "LocalizationService"),
    ("SetRobloxLocaleId", "LocalizationService"),
    ("IsLoadingInternalTranslationsSettingChanged", "LocalizationService"),
    ("StartTextScraper", "LocalizationService"),
    ("StopTextScraper", "LocalizationService"),
    // Player / Players CoreScript members
    ("GetFriendStatus", "Player"),
    ("HasBlockedPlayer", "Player"),
    ("GetBlockListInitialized", "Player"),
    ("SetBlockListInitialized", "Player"),
    ("AddToBlockList", "Player"),
    ("UpdatePlayerBlocked", "Player"),
    ("RequestFriendship", "Player"),
    ("RevokeFriendship", "Player"),
    ("GetUnder13", "Player"),
    ("GetGlobalUserId", "Player"),
    ("GetCanManageAsync", "Player"),
    ("GetSeatRequested", "Player"),
    ("GetToolRequested", "Player"),
    ("NotifyAgeCheckPassed", "Player"),
    ("SetChatTranslationSettingsLocaleId", "Player"),
    ("SetExperienceSettingsLocaleId", "Player"),
    ("SetHasRobloxSubscription", "Player"),
    ("SetMembershipType", "Player"),
    ("SetModerationAccessKey", "Player"),
    ("Chat", "Players"),
    ("TeamChat", "Players"),
    ("WhisperChat", "Players"),
    ("ReportAbuse", "Players"),
    ("ReportAbuseV3", "Players"),
    ("ReportAvatarAbuse", "Players"),
    ("ReportChatAbuse", "Players"),
    ("SetChatStyle", "Players"),
    ("SetLocalPlayerInfo", "Players"),
    // TextChatService CoreScript members
    ("ChatIsLoaded", "TextChatService"),
    ("GetTextChannelWindows", "TextChatService"),
    ("HasAllocatedUniverseChatContext", "TextChatService"),
    ("IsProtectedChatEnabled", "TextChatService"),
    ("SendEnableChatButtonClicked", "TextChatService"),
    ("SendEnableChatButtonShown", "TextChatService"),
    ("SendExpChatLoadSuccess", "TextChatService"),
    ("SendExpChatMessageClientRendered", "TextChatService"),
    ("SendExpChatWindowScroll", "TextChatService"),
    ("SendExpChatWindowStatusChange", "TextChatService"),
    ("SendTextChatCommandClientSent", "TextChatService"),
    ("setModerationModeEnabled", "TextChatService"),
    ("CanUsersWhisperAsync", "TextChatService"),
    ("GetChatableUserCountAsync", "TextChatService"),
    ("GetPresetsAsync", "TextChatService"),
    ("OnUserChatSettingUpdateAsync", "TextChatService"),
    ("SetAbsolutePosition", "ChannelTabsConfiguration"),
    ("SetAbsoluteSize", "ChannelTabsConfiguration"),
    // Legacy Chat CoreScript members
    ("ChatLocal", "Chat"),
    ("GetShouldUseLuaChat", "Chat"),
    ("ReconcileCommunicationAccess", "Chat"),
    ("RequestModerationModeEnabled", "Chat"),
];

/// Written in Luau (players_chat.luau) because they yield or run callbacks.
pub const LUA_METHODS: &[(&str, &str)] = &[
    ("Kick", "Player"),
    ("LoadCharacter", "Player"),
    ("LoadCharacterAsync", "Player"),
    ("LoadCharacterBlocking", "Player"),
    ("LoadCharacterWithHumanoidDescription", "Player"),
    ("LoadCharacterWithHumanoidDescriptionAsync", "Player"),
    ("LoadCharacterWithAvatarRulesAsync", "Player"),
    ("RequestStreamAroundAsync", "Player"),
    ("IsInGroup", "Player"),
    ("IsInGroupAsync", "Player"),
    ("GetRankInGroup", "Player"),
    ("GetRankInGroupAsync", "Player"),
    ("GetRoleInGroup", "Player"),
    ("GetRoleInGroupAsync", "Player"),
    ("IsFriendsWith", "Player"),
    ("IsFriendsWithAsync", "Player"),
    ("IsBestFriendsWith", "Player"),
    ("GetFriendsOnline", "Player"),
    ("GetFriendsOnlineAsync", "Player"),
    ("GetFriendsInServerAsync", "Player"),
    ("GetFriendsInUniverseAsync", "Player"),
    ("GetFriendsWhoPlayedAsync", "Player"),
    ("GetUserIdFromNameAsync", "Players"),
    ("GetNameFromUserIdAsync", "Players"),
    ("GetFriendsAsync", "Players"),
    ("BanAsync", "Players"),
    ("UnbanAsync", "Players"),
    ("GetBanHistoryAsync", "Players"),
    ("RegisterChatCallback", "Chat"),
    ("InvokeChatCallback", "Chat"),
    ("SetBubbleChatSettings", "Chat"),
    ("CanUserChatAsync", "Chat"),
    ("CanUsersChatAsync", "Chat"),
    ("FilterStringAsync", "Chat"),
    ("FilterStringForBroadcast", "Chat"),
    ("FilterStringForPlayerAsync", "Chat"),
    ("Chat", "Chat"),
    ("CanUserChatAsync", "TextChatService"),
    ("CanUsersChatAsync", "TextChatService"),
    ("CanUsersDirectChatAsync", "TextChatService"),
    ("GetChatGroupsAsync", "TextChatService"),
    ("DisplayBubble", "TextChatService"),
    ("SendAsync", "TextChannel"),
    ("SendInternalAsync", "TextChannel"),
    ("SendDictatedSpeechAsync", "TextChannel"),
    ("SendPresetAsync", "TextChannel"),
    ("DisplaySystemMessage", "TextChannel"),
    ("AddUserAsync", "TextChannel"),
    ("SetDirectChatRequester", "TextChannel"),
    ("DeriveNewMessageProperties", "ChatWindowConfiguration"),
    ("GetTranslatorForPlayerAsync", "LocalizationService"),
    ("GetTranslatorForPlayer", "LocalizationService"),
    ("GetTranslatorForLocaleAsync", "LocalizationService"),
    ("GetCountryRegionForPlayerAsync", "LocalizationService"),
];

pub const EVENTS: &[(&str, &str)] = &[
    ("Chatted", "Chat"),
    ("BubbleChatSettingsChanged", "Chat"),
    ("PlayerChatAvailabilityStatusChanged", "Chat"),
    ("ReconcileCommunicationAccessCompleted", "Chat"),
    ("TimeoutChatAttempt", "Chat"),
    ("SendingMessage", "TextChatService"),
    ("MessageReceived", "TextChatService"),
    ("BubbleDisplayed", "TextChatService"),
    ("MessageReceived", "TextChannel"),
    ("Triggered", "TextChatCommand"),
    // Never fired in a Studio Play test (no membership / subscription
    // changes happen there), as in Studio.
    ("PlayerMembershipChanged", "Players"),
    // (Studio didn't fire it on a respawn in its Play test either.)
    ("EmotesLoaded", "Player"),
    ("UserSubscriptionStatusChanged", "Players"),
    ("AutoTranslateWillRun", "LocalizationService"),
    ("FriendStatusChanged", "Player"),
    ("BlockListChanged", "Player"),
    ("CloudEditSelectionChanged", "Player"),
    ("InstancePinned", "Player"),
    ("InstanceUnpinned", "Player"),
    ("SimulationRadiusChanged", "Player"),
    ("StreamingPinComplete", "Player"),
    ("FriendRequestEvent", "Players"),
    ("PlayerChatted", "Players"),
    ("PlayerConnecting", "Players"),
    ("PlayerDisconnecting", "Players"),
    ("PlayerRejoining", "Players"),
    ("PromptAgeCheckRequested", "Players"),
    // TextChatService CoreScript events
    ("ChatActionReceived", "TextChatService"),
    ("CoreMessageReceived", "TextChatService"),
    ("ExpChatFeatureValueChanged", "TextChatService"),
    ("OnIncomingMessageEvent", "TextChatService"),
    ("SendingUniverseChatMessage", "TextChatService"),
    ("TextChannelWindowAdded", "TextChatService"),
    ("TextChannelWindowRemoved", "TextChatService"),
    ("UniverseChatChannelAllocated", "TextChatService"),
    ("UniverseChatMessageReceived", "TextChatService"),
    ("UserMessageIntentSent", "TextChatService"),
];

/// Player:Kick requests (message), for play_game.rs to disconnect the player.
static KICKS: Mutex<Vec<String>> = Mutex::new(Vec::new());

pub fn take_kick() -> Option<String> {
    KICKS.lock().ok()?.drain(..).next()
}

fn one(v: Value) -> mlua::Result<MultiValue> {
    Ok(MultiValue::from_vec(vec![v]))
}

fn none() -> mlua::Result<MultiValue> {
    Ok(MultiValue::new())
}

fn enum_val(lua: &Lua, e: &str, item: &str) -> mlua::Result<Value> {
    lua.load(format!("return Enum.{e}.{item}")).eval()
}

fn color(lua: &Lua, r: f32, g: f32, b: f32) -> mlua::Result<Value> {
    variant_to_lua(lua, &Variant::Color3(rbx_dom_weak::types::Color3::new(r, g, b)), None)
}

fn vec2(lua: &Lua, v: [f32; 2]) -> mlua::Result<Value> {
    Ok(Value::UserData(lua.create_userdata(LVector2(glam::Vec2::new(v[0], v[1])))?))
}

fn json_to_lua(lua: &Lua, v: &serde_json::Value) -> mlua::Result<Value> {
    use serde_json::Value as J;
    Ok(match v {
        J::Null => Value::Nil,
        J::Bool(b) => Value::Boolean(*b),
        J::Number(n) => Value::Number(n.as_f64().unwrap_or(0.0)),
        J::String(s) => Value::String(lua.create_string(s)?),
        J::Array(a) => {
            let t = lua.create_table()?;
            for x in a {
                t.raw_push(json_to_lua(lua, x)?)?;
            }
            Value::Table(t)
        }
        J::Object(o) => {
            let t = lua.create_table()?;
            for (k, x) in o {
                t.raw_set(k.as_str(), json_to_lua(lua, x)?)?;
            }
            Value::Table(t)
        }
    })
}

/// A property the domain answers: what's stored, else Studio's default.
fn stored_or(lua: &Lua, r: Ref, key: &str, enum_name: Option<&str>, dflt: impl FnOnce() -> mlua::Result<Value>) -> mlua::Result<Option<Value>> {
    let v = world(lua)?.d().prop(r, key);
    match v {
        Some(v) => Ok(Some(variant_to_lua(lua, &v, enum_name)?)),
        None => dflt().map(Some),
    }
}

/// The value players_chat.luau keeps for a TextChat object (`P[inst][key]`).
fn lua_prop(lua: &Lua, r: Ref, key: &str) -> mlua::Result<Option<Value>> {
    let get: Function = lua.globals().get("__rs_pc_get")?;
    let (found, v): (bool, Value) = get.call((inst_value(lua, r)?, key))?;
    Ok(found.then_some(v))
}

fn side_name(lua: &Lua) -> mlua::Result<&'static str> {
    let w = world(lua)?;
    Ok(if w.mode == RunMode::Run {
        "Run"
    } else {
        w.side.label()
    })
}

fn deprecated_persistence(lua: &Lua) -> mlua::Result<()> {
    world(lua)?.output.push((
        rustudio_core::log::Level::Warn,
        "DataPersistence is deprecated, please use DataStoreService instead.".into(),
    ));
    Ok(())
}

fn character_of(lua: &Lua, player: Ref) -> mlua::Result<Option<Ref>> {
    Ok(match world(lua)?.d().prop(player, "Character") {
        Some(Variant::Ref(c)) if !c.is_none() => Some(c),
        _ => None,
    })
}

pub fn call(lua: &Lua, name: &str, this: Ref, args: &MultiValue) -> Option<mlua::Result<MultiValue>> {
    if let Some(r) = loc::call(lua, name, this, args) {
        return Some(r);
    }
    if !METHODS.iter().any(|(m, _)| *m == name) {
        return None;
    }
    let class = inst_meta(lua, this).map(|m| m.0).unwrap_or_default();
    if !METHODS.iter().any(|(m, b)| *m == name && rustudio_core::doc::is_a(&class, b)) {
        return None;
    }
    Some(call_inner(lua, name, this, args))
}

fn call_inner(lua: &Lua, name: &str, this: Ref, args: &MultiValue) -> mlua::Result<MultiValue> {
    match name {
        "AddReplicationFocus" | "RemoveReplicationFocus" => {
            let part = opt_inst(&arg(args, 1));
            let is_part = part.is_some_and(|p| {
                world(lua)
                    .ok()
                    .and_then(|w| w.d().class_of(p).map(|c| rustudio_core::doc::is_a(&c, "BasePart")))
                    .unwrap_or(false)
            });
            let streaming = {
                let w = world(lua)?;
                let d = w.d();
                d.workspace().and_then(|ws| d.prop(ws, "StreamingEnabled"))
            };
            if world(lua)?.side == Side::Client && !is_part {
                return Err(rt_err("Invalid replication focus"));
            }
            if !matches!(streaming, Some(Variant::Bool(true))) {
                return Err(rt_err(format!("Workspace.StreamingEnabled is false, cannot {name}")));
            }
            if !is_part {
                return Err(rt_err("Invalid replication focus"));
            }
            // One machine: every part is already streamed in.
            none()
        }
        "ClearCachedAvatarAppearance" | "PromptAgeCheck" => none(),
        "SetSuperSafeChat" => none(),
        "GetCameraState" => Err(rt_err("GetCameraState requires Server Authority to be enabled")),
        "GetData" => Err(rt_err("Player:GetData() is not yet enabled!")),
        // A Play joined without a teleport has no data; after a play-testing
        // teleport: SourcePlaceId, SourceGameId, TeleportData, Members (play_teleport.rs).
        "GetJoinData" => match crate::play_teleport::arrival() {
            Some(a) => one(crate::runtime::ext::from_json(lua, &a.join_data())?),
            None => one(Value::Table(lua.create_table()?)),
        },
        "IsVerified" => {
            if world(lua)?.side == Side::Client {
                return Err(rt_err("isVerified() can only be called by the backend server"));
            }
            one(Value::Boolean(false))
        }
        "LoadCharacterAppearance" => {
            // Puts the asset on the character (Accessory / clothing / body
            // colors), as Roblox's appearance loader does.
            let Some(item) = opt_inst(&arg(args, 1)) else {
                return Err(rt_err("Argument 1 missing or nil"));
            };
            if let Some(ch) = character_of(lua, this)? {
                let set: Function = lua.load("return function(i, c) i.Parent = c end").eval()?;
                set.call::<()>((inst_value(lua, item)?, inst_value(lua, ch)?))?;
            }
            none()
        }
        "LoadBoolean" | "loadBoolean" => {
            deprecated_persistence(lua)?;
            one(Value::Boolean(false))
        }
        "LoadNumber" | "loadNumber" => {
            deprecated_persistence(lua)?;
            one(Value::Number(0.0))
        }
        "LoadString" | "loadString" => {
            deprecated_persistence(lua)?;
            one(Value::String(lua.create_string("")?))
        }
        "LoadInstance" | "loadInstance" => {
            deprecated_persistence(lua)?;
            one(Value::Nil)
        }
        "SaveBoolean" | "SaveNumber" | "SaveString" | "SaveInstance" | "saveBoolean" | "saveNumber"
        | "saveString" | "saveInstance" => {
            deprecated_persistence(lua)?;
            none()
        }
        "WaitForDataReady" | "waitForDataReady" => one(Value::Boolean(true)),
        "GetIsLoadingInternalTranslations" | "HasBlockedPlayer" | "GetUnder13" => one(Value::Boolean(false)),
        "GetBlockListInitialized" | "GetCanManageAsync" => one(Value::Boolean(true)),
        "GetFriendStatus" => one(enum_val(lua, "FriendStatus", "NotFriend")?),
        "GetGlobalUserId" => {
            let uid = match world(lua)?.d().prop(this, "UserId") {
                Some(Variant::Int64(n)) => n,
                Some(Variant::Int32(n)) => n as i64,
                _ => 0,
            };
            one(Value::Integer(uid))
        }
        "GetSeatRequested" | "GetToolRequested" => one(Value::Nil),
        "SetRobloxLocaleId"
        | "IsLoadingInternalTranslationsSettingChanged"
        | "StartTextScraper"
        | "StopTextScraper"
        | "SetBlockListInitialized"
        | "AddToBlockList"
        | "UpdatePlayerBlocked"
        | "RequestFriendship"
        | "RevokeFriendship"
        | "NotifyAgeCheckPassed"
        | "SetChatTranslationSettingsLocaleId"
        | "SetExperienceSettingsLocaleId"
        | "SetHasRobloxSubscription"
        | "SetMembershipType"
        | "SetModerationAccessKey"
        | "TeamChat"
        | "WhisperChat"
        | "ReportAbuse"
        | "ReportAbuseV3"
        | "ReportAvatarAbuse"
        | "ReportChatAbuse"
        | "SetChatStyle"
        | "SetLocalPlayerInfo" => none(),
        "Chat" => none(),
        "ChatIsLoaded"
        | "SendEnableChatButtonClicked"
        | "SendEnableChatButtonShown"
        | "SendExpChatLoadSuccess"
        | "SendExpChatMessageClientRendered"
        | "SendExpChatWindowScroll"
        | "SendExpChatWindowStatusChange"
        | "SendTextChatCommandClientSent"
        | "ChatLocal"
        | "ReconcileCommunicationAccess"
        | "RequestModerationModeEnabled" => none(),
        "GetShouldUseLuaChat" => one(Value::Boolean(true)),
        "SetAbsolutePosition" | "SetAbsoluteSize" => {
            let v = opt_ud::<LVector2>(&arg(args, 1)).map(|u| u.0).unwrap_or(glam::Vec2::ZERO);
            let k = if name == "SetAbsolutePosition" { "AbsolutePosition" } else { "AbsoluteSize" };
            if let Ok(f) = lua.globals().get::<Function>("__rs_pc_set_layout") {
                let _ = f.call::<()>(("ChannelTabsConfiguration", k, v.x as f64, v.y as f64));
            }
            none()
        }
        "GetTextChannelWindows" | "GetPresetsAsync" => one(Value::Table(lua.create_table()?)),
        "HasAllocatedUniverseChatContext" | "IsProtectedChatEnabled" | "setModerationModeEnabled" => {
            one(Value::Boolean(false))
        }
        "CanUsersWhisperAsync" | "OnUserChatSettingUpdateAsync" => one(Value::Boolean(true)),
        "GetChatableUserCountAsync" => {
            let n = {
                let w = world(lua)?;
                let d = w.d();
                d.find_child(d.dom.root_ref(), "Players")
                    .and_then(|p| d.dom.get_by_ref(p))
                    .map(|i| i.children().iter().filter(|c| d.class_of(**c).as_deref() == Some("Player")).count())
                    .unwrap_or(1)
            };
            one(Value::Integer(n as i64))
        }
        "GetPlayerFromCharacter" | "getPlayerFromCharacter" | "playerFromCharacter" => {
            let found = opt_inst(&arg(args, 1)).and_then(|ch| {
                let w = world(lua).ok()?;
                let d = w.d();
                let kids = d.dom.get_by_ref(this)?.children().to_vec();
                kids.into_iter().find(|p| {
                    d.class_of(*p).is_some_and(|c| c == "Player")
                        && matches!(d.prop(*p, "Character"), Some(Variant::Ref(c)) if c == ch)
                })
            });
            one(opt_ref_value(lua, found)?)
        }
        "GetUserThumbnailAsync" => {
            let id = match arg(args, 1) {
                Value::Number(n) => n as i64,
                Value::Integer(n) => n,
                _ => return Err(rt_err("Argument 1 missing or nil")),
            };
            let kind = opt_ud::<LEnumItem>(&arg(args, 2)).ok_or_else(|| rt_err("Argument 2 missing or nil"))?;
            let size = opt_ud::<LEnumItem>(&arg(args, 3)).ok_or_else(|| rt_err("Argument 3 missing or nil"))?;
            let t = match kind.name.as_str() {
                "HeadShot" => "AvatarHeadShot",
                "AvatarBust" => "AvatarBust",
                "AvatarThumbnail" => "Avatar",
                other => other,
            };
            let (w, h) = size
                .name
                .strip_prefix("Size")
                .and_then(|s| s.split_once('x'))
                .unwrap_or(("420", "420"));
            // Studio answers with the rbxthumb URL and isReady = false
            // (it never waits for the thumbnail service).
            let url = format!("rbxthumb://type={t}&id={id}&w={w}&h={h}");
            Ok(MultiValue::from_vec(vec![
                Value::String(lua.create_string(url)?),
                Value::Boolean(false),
            ]))
        }
        "GetHumanoidDescriptionFromUserIdAsync"
        | "GetHumanoidDescriptionFromOutfitIdAsync"
        | "CreateHumanoidModelFromDescriptionAsync"
        | "CreateHumanoidModelFromUserIdAsync" => {
            let base = name.trim_end_matches("Async");
            crate::runtime::ext::call(lua, base, this, args).unwrap_or_else(|| Err(rt_err(format!("{name} is not available"))))
        }
        "GetCharacterAppearanceAsync" => {
            // The worn assets as a Model (Roblox's CharacterAppearance), built
            // from the session's own avatar for the signed-in user.
            let m = crate::runtime::ext::create_any(lua, "Model")?;
            one(inst_value(lua, m)?)
        }
        "GetCharacterAppearanceInfoAsync" => {
            let uid = match arg(args, 1) {
                Value::Number(n) => n as i64,
                Value::Integer(n) => n,
                _ => return Err(rt_err("Argument 1 missing or nil")),
            };
            let look = world(lua)?.avatar.clone().filter(|l| l.user_id as i64 == uid);
            let t = lua.create_table()?;
            let assets = lua.create_table()?;
            let bc = lua.create_table()?;
            let scales = lua.create_table()?;
            match look {
                Some(l) => {
                    for (k, c) in [("headColorId", l.colors.head), ("torsoColorId", l.colors.torso), ("leftArmColorId", l.colors.left_arm), ("rightArmColorId", l.colors.right_arm), ("leftLegColorId", l.colors.left_leg), ("rightLegColorId", l.colors.right_leg)] {
                        bc.set(k, nearest_brick(c) as u16 as f64)?;
                    }
                    scales.set("height", l.scales.height)?;
                    scales.set("width", l.scales.width)?;
                    scales.set("depth", l.scales.depth)?;
                    scales.set("head", l.scales.head)?;
                    scales.set("bodyType", l.scales.body_type)?;
                    scales.set("proportion", l.scales.proportion)?;
                    t.set("playerAvatarType", if l.r15 { "R15" } else { "R6" })?;
                }
                None => {
                    t.set("playerAvatarType", "R15")?;
                }
            }
            t.set("assets", assets)?;
            t.set("bodyColors", bc)?;
            t.set("scales", scales)?;
            t.set("emotes", lua.create_table()?)?;
            t.set("defaultShirtApplied", false)?;
            t.set("defaultPantsApplied", false)?;
            one(Value::Table(t))
        }
        "GetProfileConfigurationFromUserIdAsync" => {
            let t = lua.create_table()?;
            one(Value::Table(t))
        }
        "RebalanceTeams" => {
            rebalance_teams(lua, this)?;
            none()
        }
        "GetDeviceCameraCFrame" => {
            if opt_inst(&arg(args, 1)).is_none() {
                return Err(rt_err("No player specified"));
            }
            // No head-tracking device: the identity CFrame, as Studio.
            let cf: Value = lua.load("return CFrame.new()").eval()?;
            one(cf)
        }
        _ => none(),
    }
}

/// Teams:RebalanceTeams: players of AutoAssignable teams spread so the
/// counts differ by at most one.
fn rebalance_teams(lua: &Lua, teams: Ref) -> mlua::Result<()> {
    let f: Function = lua
        .load(
            r#"return function(teams)
    local list = {}
    for _, t in teams:GetChildren() do
        if t:IsA("Team") and t.AutoAssignable then table.insert(list, t) end
    end
    if #list == 0 then return end
    local players = {}
    for _, p in game:GetService("Players"):GetPlayers() do
        if p.Team and table.find(list, p.Team) then table.insert(players, p) end
    end
    for i, p in players do
        local t = list[(i - 1) % #list + 1]
        if p.Team ~= t then p.Team = t end
    end
end"#,
        )
        .eval()?;
    f.call::<()>(inst_value(lua, teams)?)
}

/// Studio's values for the Chat / TextChat objects' plain properties (measured
/// in Studio Edit + Play; the generated stubs answer zeros), used until set.
fn studio_default(lua: &Lua, class: &str, key: &str) -> mlua::Result<Option<Value>> {
    let rgb = |r: f32, g: f32, b: f32| color(lua, r / 255.0, g / 255.0, b / 255.0);
    let builder_med = || -> mlua::Result<Value> {
        lua.load(r#"return Font.new("rbxasset://fonts/families/BuilderSans.json", Enum.FontWeight.Medium)"#).eval()
    };
    let builder_bold = || -> mlua::Result<Value> {
        lua.load(r#"return Font.new("rbxasset://fonts/families/BuilderSans.json", Enum.FontWeight.Bold)"#).eval()
    };
    let gotham_med = || -> mlua::Result<Value> {
        lua.load(r#"return Font.new("rbxasset://fonts/families/GothamSSm.json", Enum.FontWeight.Medium)"#).eval()
    };
    let vec3_zero = || -> mlua::Result<Value> {
        variant_to_lua(lua, &Variant::Vector3(rbx_dom_weak::types::Vector3::new(0.0, 0.0, 0.0)), None)
    };
    let s = |v: &str| Ok(Value::String(lua.create_string(v)?));
    let n = |v: f64| Ok(Value::Number(v));
    let b = |v: bool| Ok(Value::Boolean(v));
    let v: mlua::Result<Value> = match (class, key) {
        // Legacy Chat
        ("Chat", "BubbleChatEnabled") => b(false),
        ("Chat", "LoadDefaultChat") => b(true),
        ("Chat", "IsAutoMigrated") => b(false),
        ("Chat", "ModerationMode") => s(""),
        // Player CoreScript chat defaults (Studio overrides rbx_reflection defaults)
        ("Player", "ChatAvailabilityStatus") => s("AllUsers"),
        ("Player", "ChatMode") => enum_val(lua, "ChatMode", "TextAndMenu"),
        ("Player", "ThirdPartyTextChatRestrictionStatus") => enum_val(lua, "ChatRestrictionStatus", "NotRestricted"),
        ("Player", "UnfilteredChat") => b(false),
        ("Player", "VoiceChatVolume") => n(1.0),
        // TextChatService translation defaults
        ("TextChatService", "ChatTranslationEnabled") => b(true),
        ("TextChatService", "ChatTranslationToggleEnabled") => b(false),
        ("TextChatService", "ChatTranslationFTUXShown") => b(true),
        // BubbleChatConfiguration (measured in Studio)
        ("BubbleChatConfiguration", "AdorneeName") => s("HumanoidRootPart"),
        ("BubbleChatConfiguration", "BackgroundColor3") => rgb(250.0, 250.0, 250.0),
        ("BubbleChatConfiguration", "BackgroundTransparency") => n(0.1),
        ("BubbleChatConfiguration", "BubbleDuration") => n(15.0),
        ("BubbleChatConfiguration", "BubblesSpacing") => n(6.0),
        ("BubbleChatConfiguration", "Enabled") => b(true),
        ("BubbleChatConfiguration", "Font") => enum_val(lua, "Font", "BuilderSansMedium"),
        ("BubbleChatConfiguration", "FontFace") => builder_med(),
        ("BubbleChatConfiguration", "LocalPlayerStudsOffset") => vec3_zero(),
        ("BubbleChatConfiguration", "MaxBubbles") => n(3.0),
        ("BubbleChatConfiguration", "MaxDistance") => n(100.0),
        ("BubbleChatConfiguration", "MinimizeDistance") => n(40.0),
        ("BubbleChatConfiguration", "TailVisible") => b(true),
        ("BubbleChatConfiguration", "TextColor3") => rgb(57.0, 59.0, 61.0),
        ("BubbleChatConfiguration", "TextSize") => n(20.0),
        ("BubbleChatConfiguration", "VerticalStudsOffset") => n(0.0),
        // ChatWindowConfiguration (measured in Studio)
        ("ChatWindowConfiguration", "BackgroundColor3") => rgb(25.0, 27.0, 29.0),
        ("ChatWindowConfiguration", "BackgroundTransparency") => n(0.3),
        ("ChatWindowConfiguration", "Enabled") => b(true),
        ("ChatWindowConfiguration", "FontFace") => builder_med(),
        ("ChatWindowConfiguration", "HeightScale" | "WidthScale") => n(1.0),
        ("ChatWindowConfiguration", "HorizontalAlignment") => enum_val(lua, "HorizontalAlignment", "Left"),
        ("ChatWindowConfiguration", "TextChannelDisplayMode") => enum_val(lua, "TextChannelDisplayMode", "AllTextChannels"),
        ("ChatWindowConfiguration", "TextColor3") => rgb(255.0, 255.0, 255.0),
        ("ChatWindowConfiguration", "TextSize") => n(18.0),
        ("ChatWindowConfiguration", "TextStrokeColor3") => rgb(0.0, 0.0, 0.0),
        ("ChatWindowConfiguration", "TextStrokeTransparency") => n(0.5),
        ("ChatWindowConfiguration", "VerticalAlignment") => enum_val(lua, "VerticalAlignment", "Top"),
        // ChatInputBarConfiguration (measured in Studio)
        ("ChatInputBarConfiguration", "AutocompleteEnabled") => b(true),
        ("ChatInputBarConfiguration", "BackgroundColor3") => rgb(25.0, 27.0, 29.0),
        ("ChatInputBarConfiguration", "BackgroundTransparency") => n(0.2),
        ("ChatInputBarConfiguration", "Enabled") => b(true),
        ("ChatInputBarConfiguration", "FontFace") => builder_med(),
        ("ChatInputBarConfiguration", "KeyboardKeyCode") => enum_val(lua, "KeyCode", "Slash"),
        ("ChatInputBarConfiguration", "PlaceholderColor3") => rgb(178.0, 178.0, 178.0),
        ("ChatInputBarConfiguration", "TextColor3") => rgb(255.0, 255.0, 255.0),
        ("ChatInputBarConfiguration", "TextSize") => n(18.0),
        ("ChatInputBarConfiguration", "TextStrokeColor3") => rgb(0.0, 0.0, 0.0),
        ("ChatInputBarConfiguration", "TextStrokeTransparency") => n(0.5),
        // ChannelTabsConfiguration (measured in Studio)
        ("ChannelTabsConfiguration", "BackgroundColor3") => rgb(25.0, 27.0, 29.0),
        ("ChannelTabsConfiguration", "BackgroundTransparency") => n(0.0),
        ("ChannelTabsConfiguration", "Enabled") => b(false),
        ("ChannelTabsConfiguration", "FontFace") => builder_bold(),
        ("ChannelTabsConfiguration", "HoverBackgroundColor3") => rgb(125.0, 125.0, 125.0),
        ("ChannelTabsConfiguration", "SelectedTabTextColor3") => rgb(255.0, 255.0, 255.0),
        ("ChannelTabsConfiguration", "TextColor3") => rgb(175.0, 175.0, 175.0),
        ("ChannelTabsConfiguration", "TextSize") => n(18.0),
        ("ChannelTabsConfiguration", "TextStrokeColor3") => rgb(0.0, 0.0, 0.0),
        ("ChannelTabsConfiguration", "TextStrokeTransparency") => n(1.0),
        // TextChatCommand / TextSource / TextChannel / TextChatService
        ("TextChatCommand", "Enabled" | "AutocompleteVisible") => b(true),
        ("TextChatCommand", "PrimaryAlias" | "SecondaryAlias") => s(""),
        ("TextSource", "CanSend") => b(true),
        ("TextSource", "UserId") => n(0.0),
        ("TextChannel", "AddPlayersOnJoin") => b(false),
        ("TextChatService", "CreateDefaultCommands" | "CreateDefaultTextChannels") => b(true),
        ("TextChatService", "HasSeenDeprecationDialog" | "IsLegacyChatDisabled") => b(false),
        ("TextChatService", "PlatformIntegratedChat") => enum_val(lua, "RolloutState", "Disabled"),
        ("TextChatService", "ChatVersion") => enum_val(lua, "ChatVersion", "LegacyChatService"),
        // Player CoreScript properties
        ("Player", "AppearanceDidLoad") => b(true),
        ("Player", "Guest" | "Teleported" | "TeleportedIn" | "VREnabled") => b(false),
        ("Player", "OsPlatform" | "PlatformName") => s("Windows"),
        ("Player", "VRDevice") => s(""),
        ("LocalizationService", "ForcePlayModeGameLocaleId" | "ForcePlayModeRobloxLocaleId" | "RobloxForcePlayModeGameLocaleId" | "RobloxForcePlayModeRobloxLocaleId") => s(""),
        ("LocalizationService", "IsTextScraperRunning") => b(false),
        // MessageProperties defaults (measured in Studio)
        ("TextChatMessageProperties" | "BubbleChatMessageProperties" | "ChatWindowMessageProperties", "PrefixText" | "Translation" | "Text") => s(""),
        ("BubbleChatMessageProperties", "BackgroundColor3") => rgb(250.0, 250.0, 250.0),
        ("BubbleChatMessageProperties", "BackgroundTransparency") => n(0.1),
        ("BubbleChatMessageProperties", "FontFace") => gotham_med(),
        ("BubbleChatMessageProperties", "TailVisible") => b(true),
        ("BubbleChatMessageProperties", "TextColor3") => rgb(57.0, 59.0, 61.0),
        ("BubbleChatMessageProperties", "TextSize") => n(16.0),
        ("ChatWindowMessageProperties", "FontFace") => builder_med(),
        ("ChatWindowMessageProperties", "TextColor3") => rgb(255.0, 255.0, 255.0),
        ("ChatWindowMessageProperties", "TextSize") => n(18.0),
        ("ChatWindowMessageProperties", "TextStrokeColor3") => rgb(0.0, 0.0, 0.0),
        ("ChatWindowMessageProperties", "TextStrokeTransparency") => n(0.5),
        _ => return Ok(None),
    };
    v.map(Some)
}

pub fn prop_get(lua: &Lua, r: Ref, class: &str, key: &str) -> mlua::Result<Option<Value>> {
    if let Some(v) = loc::prop_get(lua, r, class, key)? {
        return Ok(Some(v));
    }
    let explicitly_stored = world(lua)?
        .d()
        .dom
        .get_by_ref(r)
        .is_some_and(|i| i.properties.contains_key(&rbx_dom_weak::ustr(key)));
    if !explicitly_stored {
        if let Some(v) = studio_default(lua, class, key)? {
            return Ok(Some(v));
        }
    }
    match (class, key) {
        // Play's local player carries the account's (play.rs create_player);
        // a Player without them reads as Studio's offline answer.
        ("Player", "MembershipType") => stored_or(lua, r, key, Some("MembershipType"), || enum_val(lua, "MembershipType", "None")),
        ("Player", "AccountAge") => stored_or(lua, r, key, None, || Ok(Value::Number(0.0))),
        ("Player", "CharacterAppearanceId") => stored_or(lua, r, key, None, || {
            Ok(match world(lua)?.d().prop(r, "UserId") {
                Some(Variant::Int64(u)) => Value::Number(u as f64),
                _ => Value::Number(0.0),
            })
        }),
        ("Player", "AgeChecked") => stored_or(lua, r, key, Some("AgeCheckStatus"), || enum_val(lua, "AgeCheckStatus", "Unchecked")),
        ("Player", "FrustumStreaming") => stored_or(lua, r, key, Some("FrustumStreamingMode"), || enum_val(lua, "FrustumStreamingMode", "Default")),
        ("Player", "DeviceTrustLevel") => {
            // Hidden; Studio's own build doesn't even list it. The first
            // (unknown) level.
            // Without the enum (an older Studio, like the one measured) the
            // member doesn't exist there either: Studio's error.
            let v: Value = lua
                .load("local ok, e = pcall(function() return Enum.PlayerDeviceTrustLevel end) return ok and e and e:GetEnumItems()[1] or nil")
                .eval()?;
            if v.is_nil() {
                let full = world(lua)?.full_name(r);
                return Err(rt_err(format!("DeviceTrustLevel is not a valid member of Player \"{full}\"")));
            }
            Ok(Some(v))
        }
        // The User value of a Studio test player (tostring as Studio prints it).
        ("Player", "User") => Ok(Some(Value::String(lua.create_string("U1.AQAAAAAAAAAAAAAAAAAAAAA")?))),
        ("Player", "ChatAvailabilityStatus") => stored_or(lua, r, key, None, || Ok(Value::String(lua.create_string("AllUsers")?))),
        ("Player", "ChatMode") => stored_or(lua, r, key, Some("ChatMode"), || enum_val(lua, "ChatMode", "TextAndMenu")),
        ("Player", "ThirdPartyTextChatRestrictionStatus") => stored_or(lua, r, key, Some("ChatRestrictionStatus"), || enum_val(lua, "ChatRestrictionStatus", "NotRestricted")),
        ("Player", "UnfilteredChat") => stored_or(lua, r, key, None, || Ok(Value::Boolean(false))),
        ("Player", "VoiceChatVolume") => stored_or(lua, r, key, None, || Ok(Value::Number(1.0))),
        ("Players", "BubbleChat") => stored_or(lua, r, key, None, || Ok(Value::Boolean(false))),
        ("Players", "ClassicChat") => stored_or(lua, r, key, None, || Ok(Value::Boolean(true))),
        // A local / unpublished place's server size in Studio.
        ("Players", "MaxPlayers") => stored_or(lua, r, key, None, || Ok(Value::Number(12.0))),
        ("Players", "PreferredPlayers") => stored_or(lua, r, key, None, || Ok(Value::Number(0.0))),
        ("StarterPlayer", "LoadCharacterLayeredClothing ") | ("StarterPlayer", "LoadCharacterLayeredClothing") => {
            stored_or(lua, r, "LoadCharacterLayeredClothing", Some("LoadCharacterLayeredClothing"), || {
                enum_val(lua, "LoadCharacterLayeredClothing", "Default")
            })
        }
        ("TextChatService", "ChatTranslationEnabled") => stored_or(lua, r, key, None, || Ok(Value::Boolean(true))),
        ("TextChatService", "ChatTranslationToggleEnabled") => stored_or(lua, r, key, None, || Ok(Value::Boolean(false))),
        ("TextChatService", "ChatTranslationFTUXShown") => stored_or(lua, r, key, None, || Ok(Value::Boolean(true))),
        ("Team", "ChildOrder") => {
            let idx = {
                let w = world(lua)?;
                let d = w.d();
                let stored_i = match d.prop(r, "ChildOrder") {
                    Some(Variant::Int32(n)) => Some(n as i64),
                    Some(Variant::Int64(n)) => Some(n),
                    _ => None,
                };
                if let Some(n) = stored_i {
                    n
                } else {
                    let parent = d.dom.get_by_ref(r).map(|i| i.parent()).unwrap_or(Ref::none());
                    d.dom
                        .get_by_ref(parent)
                        .and_then(|p| {
                            p.children()
                                .iter()
                                .filter(|&&c| d.dom.get_by_ref(c).is_some_and(|i| i.class == "Team"))
                                .position(|&c| c == r)
                        })
                        .unwrap_or(0) as i64
                }
            };
            Ok(Some(Value::Integer(idx)))
        }
        ("TextChannel", "DirectChatRequester") => Ok(Some(lua_prop(lua, r, key)?.unwrap_or(Value::Nil))),
        ("TextChannel", "IsDefaultTextChannel") => {
            let nm = world(lua)?.d().dom.get_by_ref(r).map(|i| i.name.clone()).unwrap_or_default();
            Ok(Some(Value::Boolean(nm == "RBXGeneral" || nm == "RBXSystem" || nm.starts_with("RBXTeam"))))
        }
        ("TextChatMessage", _) => {
            let mapped = match key {
                "PrefixTextInternal" => "PrefixText",
                "TextInternal" | "OriginalText" | "RewrittenText" => "Text",
                "TranslationInternal" | "RewrittenTranslation" => "Translation",
                _ => key,
            };
            if let Some(v) = lua_prop(lua, r, mapped)? {
                return Ok(Some(v));
            }
            let v = match key {
                "MessageId" | "Metadata" | "PrefixText" | "Text" | "Translation"
                | "PrefixTextInternal" | "TextInternal" | "TranslationInternal"
                | "OriginalText" | "RewrittenText" | "RewrittenTranslation"
                | "ChatActionType" | "PresetChatVersion" | "PresetId" => Value::String(lua.create_string("")?),
                "ForModeration" | "IsHiddenMessage" | "IsHistorical" | "WasRewritten" => Value::Boolean(false),
                "Status" => enum_val(lua, "TextChatMessageStatus", "Unknown")?,
                "Timestamp" => lua.load("return DateTime.fromUnixTimestamp(0)").eval()?,
                "TextChannel" | "TextSource" | "BubbleChatMessageProperties" | "ChatWindowMessageProperties" => Value::Nil,
                _ => return Ok(None),
            };
            Ok(Some(v))
        }
        ("TextChatMessageProperties" | "BubbleChatMessageProperties" | "ChatWindowMessageProperties", "PrefixText" | "Translation" | "Text") => {
            stored_or(lua, r, key, None, || Ok(Value::String(lua.create_string("")?)))
        }
        ("ChatWindowMessageProperties", "PrefixTextProperties") => {
            let v = match world(lua)?.d().prop(r, key) {
                Some(Variant::Ref(x)) if !x.is_none() => Some(x),
                _ => None,
            };
            Ok(Some(opt_ref_value(lua, v)?))
        }
        ("ChatInputBarConfiguration" | "ChatWindowConfiguration" | "ChannelTabsConfiguration", "AbsolutePosition" | "AbsoluteSize" | "AbsolutePositionWrite" | "AbsoluteSizeWrite") => {
            if world(lua)?.side != Side::Client {
                return Ok(Some(vec2(lua, [0.0, 0.0])?));
            }
            if class == "ChannelTabsConfiguration" {
                let enabled = match world(lua)?.d().prop(r, "Enabled") {
                    Some(Variant::Bool(b)) => b,
                    _ => false,
                };
                if !enabled {
                    return Ok(Some(vec2(lua, [0.0, 0.0])?));
                }
            }
            let rk = key.trim_end_matches("Write");
            let (x, y): (f32, f32) = lua
                .globals()
                .get::<Function>("__rs_pc_layout")
                .and_then(|f| f.call((class, rk)))
                .unwrap_or((0.0, 0.0));
            Ok(Some(vec2(lua, [x, y])?))
        }
        ("ChatInputBarConfiguration", "IsFocused" | "IsFocusedWrite") => {
            if world(lua)?.side != Side::Client {
                return Ok(Some(Value::Boolean(false)));
            }
            let f: bool = lua
                .globals()
                .get::<Function>("__rs_pc_layout")
                .and_then(|f| f.call((class, "IsFocused")))
                .unwrap_or(false);
            Ok(Some(Value::Boolean(f)))
        }
        // The default chat's TextBox is in CoreGui: scripts get nil unless a custom one was set.
        ("ChatInputBarConfiguration", "TextBox" | "TargetTextChannel") => {
            let v = match world(lua)?.d().prop(r, key) {
                Some(Variant::Ref(x)) if !x.is_none() => Some(x),
                _ => None,
            };
            Ok(Some(opt_ref_value(lua, v)?))
        }
        _ => Ok(None),
    }
}

pub fn newindex(lua: &Lua, r: Ref, class: &str, key: &str, value: &Value) -> mlua::Result<bool> {
    if loc::newindex(lua, r, class, key, value)? {
        return Ok(true);
    }
    match (class, key) {
        ("Player", "Team") => {
            let new_team = match value {
                Value::Nil => Ref::none(),
                v => {
                    let tr = opt_inst(v).ok_or_else(|| {
                        rt_err(format!("Unable to assign property Team. Object expected, got {}", v.type_name()))
                    })?;
                    let is_team = world(lua)?.d().class_of(tr).is_some_and(|c| c == "Team");
                    if !is_team {
                        return Err(rt_err("Unable to assign property Team. Team expected"));
                    }
                    tr
                }
            };
            let (old_team, team_color) = {
                let w = world(lua)?;
                let d = w.d();
                let old = match d.prop(r, "Team") {
                    Some(Variant::Ref(t)) if d.exists(t) => t,
                    _ => Ref::none(),
                };
                let color = if new_team.is_some() {
                    team_brick_color(d, new_team)
                } else {
                    None
                };
                (old, color)
            };
            store_prop(lua, r, "Team", Variant::Ref(new_team))?;
            if new_team.is_some() {
                store_prop(lua, r, "Neutral", Variant::Bool(false))?;
                if let Some(bc) = team_color {
                    store_prop(lua, r, "TeamColor", Variant::BrickColor(bc))?;
                }
            } else {
                store_prop(lua, r, "Neutral", Variant::Bool(true))?;
            }
            fire_team_membership(lua, r, old_team, new_team)?;
            Ok(true)
        }
        ("Player", "TeamColor") => {
            let bc = opt_ud::<crate::ltypes::LBrickColor>(value)
                .ok_or_else(|| {
                    rt_err(format!(
                        "Unable to assign property TeamColor. BrickColor expected, got {}",
                        value.type_name()
                    ))
                })?
                .0;
            let (old_team, neutral, matched) = {
                let w = world(lua)?;
                let d = w.d();
                let old = match d.prop(r, "Team") {
                    Some(Variant::Ref(t)) if d.exists(t) => t,
                    _ => Ref::none(),
                };
                let neutral = matches!(d.prop(r, "Neutral"), Some(Variant::Bool(true)));
                let matched = if neutral {
                    Ref::none()
                } else {
                    find_team_by_color(d, bc).unwrap_or(Ref::none())
                };
                (old, neutral, matched)
            };
            store_prop(lua, r, "TeamColor", Variant::BrickColor(bc))?;
            if !neutral && matched != old_team {
                store_prop(lua, r, "Team", Variant::Ref(matched))?;
                fire_team_membership(lua, r, old_team, matched)?;
            }
            Ok(true)
        }
        ("Player", "Neutral") => {
            let on = !matches!(value, Value::Nil | Value::Boolean(false));
            let (old_team, matched) = {
                let w = world(lua)?;
                let d = w.d();
                let old = match d.prop(r, "Team") {
                    Some(Variant::Ref(t)) if d.exists(t) => t,
                    _ => Ref::none(),
                };
                let matched = if on {
                    Ref::none()
                } else {
                    team_brick_color(d, r)
                        .and_then(|bc| find_team_by_color(d, bc))
                        .unwrap_or(Ref::none())
                };
                (old, matched)
            };
            store_prop(lua, r, "Neutral", Variant::Bool(on))?;
            if matched != old_team {
                store_prop(lua, r, "Team", Variant::Ref(matched))?;
                fire_team_membership(lua, r, old_team, matched)?;
            }
            Ok(true)
        }
        ("TextChannel" | "TextChatService", "OnIncomingMessage")
        | ("TextChannel", "ShouldDeliverCallback")
        | ("TextChatService", "OnBubbleAdded" | "OnChatWindowAdded") => {
            set_callback(lua, r, key, value.clone())?;
            Ok(true)
        }
        ("ChatInputBarConfiguration" | "ChatWindowConfiguration", "AbsolutePositionWrite" | "AbsoluteSizeWrite") => {
            let v = opt_ud::<LVector2>(value).map(|u| u.0).unwrap_or(glam::Vec2::ZERO);
            let k = key.trim_end_matches("Write");
            if let Ok(f) = lua.globals().get::<Function>("__rs_pc_set_layout") {
                let _ = f.call::<()>((class, k, v.x as f64, v.y as f64));
            }
            Ok(true)
        }
        ("ChatInputBarConfiguration", "IsFocusedWrite") => {
            let b = matches!(value, Value::Boolean(true));
            if let Ok(f) = lua.globals().get::<Function>("__rs_pc_set_layout") {
                let _ = f.call::<()>((class, "IsFocused", b, 0.0));
            }
            Ok(true)
        }
        ("TextChannel", "DirectChatRequester")
        | ("TextSource", "UserId")
        | ("ChatInputBarConfiguration", "IsFocused" | "AbsolutePosition" | "AbsoluteSize")
        | ("ChatWindowConfiguration", "AbsolutePosition" | "AbsoluteSize")
        | ("ChannelTabsConfiguration", "AbsolutePosition" | "AbsoluteSize") => {
            Err(rt_err(format!("Unable to assign property {key}. Property is read only")))
        }
        ("BubbleChatConfiguration", "Font") => {
            // Sync legacy Enum.Font writes to FontFace as Studio does.
            if opt_ud::<LEnumItem>(value).is_some() {
                let _ = lua
                    .load("local i, e = ... pcall(function() i.FontFace = Font.fromEnum(e) end)")
                    .call::<()>((inst_value(lua, r)?, value.clone()));
            }
            Ok(false)
        }
        ("TextChatMessage", _) => {
            let set: Function = lua.globals().get("__rs_pc_set")?;
            set.call::<()>((inst_value(lua, r)?, key, value.clone()))?;
            Ok(true)
        }
        _ => Ok(false),
    }
}

pub fn write_acts(class: &str, key: &str) -> bool {
    matches!(
        (class, key),
        ("Player", "Team" | "TeamColor" | "Neutral")
            | ("TextChannel" | "TextChatService", "OnIncomingMessage")
            | ("TextChannel", "ShouldDeliverCallback")
            | ("TextChatService", "OnBubbleAdded" | "OnChatWindowAdded")
            | ("BubbleChatConfiguration", "Font")
            | ("TextChatMessage", _)
            | ("LocalizationTable", "Root")
            | ("ChatInputBarConfiguration" | "ChatWindowConfiguration", "AbsolutePositionWrite" | "AbsoluteSizeWrite")
            | ("ChatInputBarConfiguration", "IsFocusedWrite")
    )
}

fn team_brick_color(d: &rustudio_core::doc::Document, r: Ref) -> Option<rbx_dom_weak::types::BrickColor> {
    match d.prop(r, "TeamColor") {
        Some(Variant::BrickColor(bc)) => Some(bc),
        Some(Variant::Int32(n)) => rbx_dom_weak::types::BrickColor::from_number(n as u16),
        None => Some(rbx_dom_weak::types::BrickColor::MediumStoneGrey),
        _ => None,
    }
}

fn find_team_by_color(d: &rustudio_core::doc::Document, bc: rbx_dom_weak::types::BrickColor) -> Option<Ref> {
    let teams = d.find_child(d.dom.root_ref(), "Teams")?;
    let inst = d.dom.get_by_ref(teams)?;
    inst.children().iter().copied().find(|c| {
        d.class_of(*c).is_some_and(|k| k == "Team") && team_brick_color(d, *c) == Some(bc)
    })
}

fn fire_team_membership(lua: &Lua, player: Ref, old_team: Ref, new_team: Ref) -> mlua::Result<()> {
    if old_team == new_team {
        return Ok(());
    }
    let pv = inst_value(lua, player)?;
    let mut w = world(lua)?;
    if old_team.is_some() {
        w.fire(old_team, "PlayerRemoved", vec![pv.clone()]);
    }
    if new_team.is_some() {
        w.fire(new_team, "PlayerAdded", vec![pv]);
    }
    Ok(())
}

pub fn install(lua: &Lua) -> mlua::Result<()> {
    let r = lua.create_table()?;
    r.set("stream_around", lua.create_function(|lua, (player, position): (Value, Value)| {
        use std::sync::{Arc, atomic::{AtomicBool, Ordering}};
        let player = opt_inst(&player).ok_or_else(|| rt_err("Player expected"))?;
        let position = opt_ud::<LVector3>(&position)
            .ok_or_else(|| rt_err("Unable to cast value to Vector3"))?.0;
        let mut w = world(lua)?;
        let complete = Arc::new(AtomicBool::new(!w.has_peer));
        if w.has_peer {
            w.stream_requests.push(crate::replicate::StreamRequest {
                player, position, complete: complete.clone(),
            });
        }
        drop(w);
        lua.create_function(move |_, ()| Ok(complete.load(Ordering::Relaxed)))
    })?)?;
    r.set(
        "fire",
        lua.create_function(|lua, args: MultiValue| {
            let mut it = args.into_iter();
            let inst = it.next().and_then(|v| opt_inst(&v)).ok_or_else(|| rt_err("fire: instance expected"))?;
            let name = match it.next() {
                Some(Value::String(s)) => s.to_str()?.to_string(),
                _ => return Err(rt_err("fire: event name expected")),
            };
            world(lua)?.fire(inst, &name, it.collect());
            flush_immediate(lua);
            Ok(())
        })?,
    )?;
    for (fn_name, core_only) in [("fire_game", false), ("fire_core", true)] {
        r.set(
            fn_name,
            lua.create_function(move |lua, args: MultiValue| {
                let mut it = args.into_iter();
                let inst = it.next().and_then(|v| opt_inst(&v)).ok_or_else(|| rt_err("fire: instance expected"))?;
                let name = match it.next() {
                    Some(Value::String(s)) => s.to_str()?.to_string(),
                    _ => return Err(rt_err("fire: event name expected")),
                };
                let rest: Vec<Value> = it.collect();
                fire_filtered(lua, inst, &name, &rest, core_only);
                Ok(())
            })?,
        )?;
    }
    r.set(
        "create",
        lua.create_function(|lua, class: String| {
            let r = crate::runtime::ext::create_any(lua, &class)?;
            inst_value(lua, r)
        })?,
    )?;
    r.set("side", lua.create_function(|lua, ()| side_name(lua))?)?;
    r.set(
        "load_character",
        lua.create_function(|lua, p: Value| {
            if let Some(p) = opt_inst(&p) {
                world(lua)?.load_character.push(p);
            }
            Ok(())
        })?,
    )?;
    r.set(
        "kick",
        lua.create_function(|lua, (p, msg): (Value, String)| {
            if let Ok(mut k) = KICKS.lock() {
                k.push(msg.clone());
            }
            world(lua)?.output.push((
                rustudio_core::log::Level::Info,
                if msg.is_empty() {
                    "You have been kicked by this experience or its moderators.".to_string()
                } else {
                    format!("You have been kicked by this experience or its moderators. Moderation message:  {msg}")
                },
            ));
            Ok(())
        })?,
    )?;
    r.set(
        "pages",
        lua.create_function(|lua, (inst, rows, size): (Value, Table, usize)| {
            let Some(r) = opt_inst(&inst) else { return Ok(()) };
            let mut pages = Vec::new();
            let mut cur = lua.create_table()?;
            for v in rows.sequence_values::<Value>() {
                cur.raw_push(v?)?;
                if cur.raw_len() >= size.max(1) {
                    pages.push(std::mem::replace(&mut cur, lua.create_table()?));
                }
            }
            if cur.raw_len() > 0 || pages.is_empty() {
                pages.push(cur);
            }
            super::pages::register_list(lua, r, pages, None)
        })?,
    )?;
    r.set(
        "web",
        lua.create_function(|lua, (kind, a, b): (String, Value, Value)| {
            let int = |v: &Value| match v {
                Value::Number(n) => *n as i64,
                Value::Integer(n) => *n,
                _ => 0,
            };
            let text = |v: &Value| match v {
                Value::String(s) => s.to_string_lossy().to_string(),
                _ => String::new(),
            };
            let (key, job) = match kind.as_str() {
                "groups" => (format!("groups:{}", int(&a)), web::groups(int(&a))),
                "friend" => (format!("friend:{}:{}", int(&a), int(&b)), web::friend_status(int(&a), int(&b))),
                "friends_online" => (format!("online:{}", int(&a)), web::friends_online(int(&a))),
                "friends" => (format!("friends:{}", int(&a)), web::friends(int(&a))),
                "id_by_name" => (format!("id:{}", text(&a).to_lowercase()), web::user_id_from_name(text(&a))),
                "name_by_id" => (format!("name:{}", int(&a)), web::name_from_user_id(int(&a))),
                _ => return Err(rt_err(format!("unknown lookup {kind}"))),
            };
            Ok(match web::poll(&key, job) {
                web::Poll::Ready(v) => (0, json_to_lua(lua, &v)?),
                web::Poll::Failed(e) => (1, Value::String(lua.create_string(e)?)),
                web::Poll::Pending => (2, Value::Nil),
            })
        })?,
    )?;
    r.set(
        "core_gui",
        lua.create_function(|lua, ()| {
            let cg = {
                let mut w = world(lua)?;
                let d = w.dm();
                let root = d.dom.root_ref();
                match d.find_child(root, "CoreGui") {
                    Some(r) => r,
                    None => d.insert(root, rbx_dom_weak::InstanceBuilder::new("CoreGui").with_name("CoreGui")),
                }
            };
            inst_value(lua, cg)
        })?,
    )?;
    r.set(
        "core_gui_enabled",
        lua.create_function(|lua, name: String| {
            Ok(!world(lua)?.core_gui_off.contains(&name))
        })?,
    )?;
    r.set(
        "is_focused",
        lua.create_function(|_, inst: Value| {
            Ok(opt_inst(&inst).is_some_and(|r| crate::gui::share().focus == Some(r)))
        })?,
    )?;
    r.set(
        "gui_box",
        lua.create_function(|lua, inst: Value| {
            if let Some(r) = opt_inst(&inst) {
                if let Some(b) = world(lua)?.input.gui_layout.get(&r).copied() {
                    return Ok((b.pos[0], b.pos[1], b.size[0], b.size[1]));
                }
            }
            Ok((0.0f32, 0.0f32, 0.0f32, 0.0f32))
        })?,
    )?;
    r.set(
        "viewport_size",
        lua.create_function(|lua, ()| {
            let s = world(lua)?.input.view_size;
            Ok((s[0], s[1]))
        })?,
    )?;
    r.set(
        "core_scripts_active",
        lua.create_function(|lua, ()| Ok(world(lua).is_ok_and(|w| !w.core_scripts.scripts.is_empty())))?,
    )?;
    let r_ui = r.clone();
    r.set(
        "load_ui",
        lua.create_function(move |lua, api: Value| {
            lua.load(LUAU_UI)
                .set_name("=rustudio.players_chat_ui")
                .call::<Value>((r_ui.clone(), api))
        })?,
    )?;
    let methods: Table = lua.globals().get("__rs_lua_methods")?;
    lua.load(LUAU)
        .set_name("=rustudio.players_chat")
        .call::<()>((methods.clone(), r))?;
    // LocalizationService's translators (yield in Roblox: one frame here).
    let loc_fn = lua.create_function(|lua, (scope, locale): (Value, String)| {
        loc::new_translator(lua, None, &locale)
    })?;
    lua.globals().set("__rs_pc_translator", loc_fn)?;
    lua.load(
        r#"
local M = __rs_lua_methods
local function player_locale(p)
    local ok, l = pcall(function() return p.LocaleId end)
    return ok and l or "en-us"
end
M.GetTranslatorForPlayerAsync = function(self, player)
    if typeof(player) ~= "Instance" or not player:IsA("Player") then error("Unable to cast value to Object", 0) end
    task.wait()
    -- (Studio, measured: an unpublished place has no cloud tables.)
    if game.GameId == 0 then error("Publishing the game is required to use GetTranslatorForPlayerAsync API.", 0) end
    return __rs_pc_translator(nil, player_locale(player))
end
M.GetTranslatorForPlayer = function(self, player)
    if typeof(player) ~= "Instance" or not player:IsA("Player") then error("Unable to cast value to Object", 0) end
    return __rs_pc_translator(nil, player_locale(player))
end
M.GetTranslatorForLocaleAsync = function(self, locale)
    if type(locale) ~= "string" then error("Argument 1 missing or nil", 0) end
    task.wait()
    return __rs_pc_translator(nil, locale)
end
-- The player's country from their locale (Studio's test player: US).
M.GetCountryRegionForPlayerAsync = function(self, player)
    if typeof(player) ~= "Instance" or not player:IsA("Player") then error("Unable to cast value to Object", 0) end
    task.wait()
    local l = player_locale(player)
    local region = l:match("%-(%a%a)$")
    return region and region:upper() or "US"
end
"#,
    )
    .set_name("=rustudio.players_chat.loc")
    .exec()?;
    Ok(())
}

pub fn step(lua: &Lua, dt: f64) -> mlua::Result<()> {
    // No chat session in the edit DataModel (the plugin host's VM): the
    // channels, commands and TextSources are made by Play.
    if world(lua).map_or(true, |w| w.mode == RunMode::Edit) {
        return Ok(());
    }
    let f: Option<Function> = lua.globals().get("__rs_pc_step").ok();
    if let Some(f) = f {
        if let Err(e) = f.call::<()>(dt) {
            world(lua)?.output.push((rustudio_core::log::Level::Error, crate::runtime::error_message(&e)));
        }
    }
    Ok(())
}

// ------------------------------------------------- play session (play_game.rs)

/// The signed-in account's identity fields for Play's local player.
#[derive(Clone, Copy, Debug, Default)]
pub struct Identity {
    pub account_age: i32,
    pub verified: bool,
    pub premium: bool,
}

/// Fetches `uid`'s identity through the login session (waits up to `max`;
/// kept for the process). None when signed out or unreachable.
pub fn account_identity(uid: i64, max: std::time::Duration) -> Option<Identity> {
    let t0 = std::time::Instant::now();
    loop {
        match web::poll(&format!("identity:{uid}"), web::identity(uid)) {
            web::Poll::Ready(v) => {
                return Some(Identity {
                    account_age: v["AccountAge"].as_i64().unwrap_or(0) as i32,
                    verified: v["HasVerifiedBadge"].as_bool().unwrap_or(false),
                    premium: v["Premium"].as_bool().unwrap_or(false),
                })
            }
            web::Poll::Failed(_) => return None,
            web::Poll::Pending if t0.elapsed() < max => std::thread::sleep(std::time::Duration::from_millis(20)),
            web::Poll::Pending => return None,
        }
    }
}

/// A line or command the local player sent (TextChannel:SendAsync, whisper,
/// or legacy chat), to carry to the server runtime.
pub struct Outgoing {
    pub kind: String,
    pub channel: Option<Ref>,
    pub text: String,
    pub raw_text: String,
    pub metadata: String,
    pub id: String,
    pub target_uid: i64,
}

/// The client's queued SendAsync / chat messages.
pub fn take_outgoing(lua: &Lua) -> Vec<Outgoing> {
    let Ok(t) = lua.globals().get::<Table>("__rs_pc_outbox") else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for v in t.clone().sequence_values::<Table>().flatten() {
        let kind: String = v.get("kind").unwrap_or_else(|_| "msg".to_string());
        let channel = v.get::<Value>("channel").ok().and_then(|c| opt_inst(&c));
        let text: String = v.get("text").unwrap_or_default();
        let raw_text: String = v.get("raw_text").unwrap_or_else(|_| text.clone());
        out.push(Outgoing {
            kind,
            channel,
            text,
            raw_text,
            metadata: v.get("metadata").unwrap_or_default(),
            id: v.get("id").unwrap_or_default(),
            target_uid: v.get("target_uid").unwrap_or(0),
        });
    }
    let _ = t.clear();
    out
}

/// The server runtime receives a message: Player.Chatted, commands,
/// ShouldDeliverCallback. Returns the user ids it's delivered to.
pub fn server_receive(lua: &Lua, player: Ref, channel: Option<Ref>, m: &Outgoing) -> Vec<i64> {
    let f: Function = match lua.globals().get("__rs_pc_server_receive") {
        Ok(f) => f,
        Err(_) => return Vec::new(),
    };
    let res = (|| -> mlua::Result<Vec<i64>> {
        let ch_val = match channel {
            Some(c) => inst_value(lua, c)?,
            None => Value::Nil,
        };
        let ids: Table = f.call((
            inst_value(lua, player)?,
            ch_val,
            m.kind.as_str(),
            m.text.as_str(),
            m.raw_text.as_str(),
            m.metadata.as_str(),
            m.id.as_str(),
            m.target_uid,
        ))?;
        Ok(ids.sequence_values::<i64>().flatten().collect())
    })();
    match res {
        Ok(v) => v,
        Err(e) => {
            if let Ok(mut w) = world(lua) {
                w.output.push((rustudio_core::log::Level::Error, crate::runtime::error_message(&e)));
            }
            Vec::new()
        }
    }
}

/// The client runtime receives a delivered message (MessageReceived, the
/// CoreGui window line, and the BubbleChat BillboardGui).
pub fn client_receive(lua: &Lua, channel: Option<Ref>, sender: i64, m: &Outgoing) {
    let Ok(f) = lua.globals().get::<Function>("__rs_pc_client_receive") else {
        return;
    };
    let r = (|| -> mlua::Result<()> {
        let ch_val = match channel {
            Some(c) => inst_value(lua, c)?,
            None => Value::Nil,
        };
        f.call::<()>((
            ch_val,
            sender,
            m.kind.as_str(),
            m.text.as_str(),
            m.raw_text.as_str(),
            m.metadata.as_str(),
            m.id.as_str(),
        ))
    })();
    if let Err(e) = r {
        if let Ok(mut w) = world(lua) {
            w.output.push((rustudio_core::log::Level::Error, crate::runtime::error_message(&e)));
        }
    }
}

/// Programmatic chat input (e.g. emote wheel "/e wave"): goes through
/// TextChatService's target channel or legacy chat.
pub fn chat_input(lua: &Lua, text: &str) -> bool {
    let Ok(f) = lua.globals().get::<Function>("__rs_pc_chat_input") else {
        return false;
    };
    f.call::<bool>(text).unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::*;
    use rustudio_core::doc::Document;

    #[test]
    fn text_chat_defaults_and_callbacks_match_studio() {
        let mut doc = Document::new_place();
        doc.ensure_services();
        let rt = Runtime::new().expect("runtime");
        rt.enter(World::new(doc, RunMode::Play));
        rt.bind_globals().expect("globals");
        // Step server setup once to create configs, default channels & commands
        step(&rt.lua, 1.0 / 60.0).expect("step");
        let res: String = rt
            .lua
            .load(
                r#"
                local T = game:GetService("TextChatService")
                local C = game:GetService("Chat")
                assert(T.ChatVersion == Enum.ChatVersion.TextChatService, "ChatVersion")
                assert(T.CreateDefaultTextChannels == true, "CreateDefaultTextChannels")
                assert(T.CreateDefaultCommands == true, "CreateDefaultCommands")
                assert(T.ChatTranslationEnabled == true, "ChatTranslationEnabled")
                assert(C.LoadDefaultChat == true, "LoadDefaultChat")
                assert(C.BubbleChatEnabled == false, "BubbleChatEnabled")

                local cwc = T:FindFirstChildOfClass("ChatWindowConfiguration")
                local cib = T:FindFirstChildOfClass("ChatInputBarConfiguration")
                local ctc = T:FindFirstChildOfClass("ChannelTabsConfiguration")
                local bcc = T:FindFirstChildOfClass("BubbleChatConfiguration")
                assert(cwc and cib and ctc and bcc, "all 4 configs")
                assert(cwc.TextSize == 18 and cwc.FontFace.Family:find("BuilderSans", 1, true), "cwc defaults")
                assert(cib.TextSize == 18 and cib.KeyboardKeyCode == Enum.KeyCode.Slash, "cib defaults")
                assert(ctc.Enabled == false and ctc.BackgroundTransparency == 0 and ctc.TextStrokeTransparency == 1, "ctc defaults")
                assert(bcc.TextSize == 20 and bcc.MaxBubbles == 3 and bcc.MinimizeDistance == 40 and bcc.MaxDistance == 100, "bcc defaults")
                assert(bcc:FindFirstChildOfClass("UIGradient") and bcc:FindFirstChildOfClass("ImageLabel")
                    and bcc:FindFirstChildOfClass("UICorner") and bcc:FindFirstChildOfClass("UIPadding"), "bcc children")

                -- ChatWindowMessageProperties is NotCreatable; DeriveNewMessageProperties copies CWC
                assert(not pcall(function() return Instance.new("ChatWindowMessageProperties") end), "cwmp not creatable")
                local derived = cwc:DeriveNewMessageProperties()
                assert(derived.TextSize == 18 and derived.PrefixTextProperties == nil, "derived")
                local bmp = Instance.new("BubbleChatMessageProperties")
                assert(bmp.TextSize == 16 and bmp.TailVisible == true, "bmp defaults")

                -- Read-only guards
                assert(not pcall(function() cib.IsFocused = true end), "IsFocused ro")
                assert(not pcall(function() cwc.AbsoluteSize = Vector2.zero end), "AbsoluteSize ro")

                -- Default channels & 9 default commands
                local tcf = T:FindFirstChild("TextChannels")
                local cmdf = T:FindFirstChild("TextChatCommands")
                assert(tcf and tcf:FindFirstChild("RBXGeneral") and tcf:FindFirstChild("RBXSystem"), "default channels")
                assert(cmdf and #cmdf:GetChildren() == 9, "9 default commands")

                -- Legacy Chat callbacks & SetBubbleChatSettings validation
                assert(not pcall(function() C:SetBubbleChatSettings(123) end), "bad bcs")
                C:SetBubbleChatSettings({ MaxBubbles = 5, BubbleDuration = 10 })
                assert(bcc.MaxBubbles == 5 and bcc.BubbleDuration == 10, "bcs synced")
                C:RegisterChatCallback(Enum.ChatCallbackType.OnServerReceivingMessage, function(x) return x + 1 end)
                assert(C:InvokeChatCallback(Enum.ChatCallbackType.OnServerReceivingMessage, 9) == 10, "chat cb")
                return "ok"
                "#,
            )
            .eval()
            .expect("luau chat unit test");
        assert_eq!(res, "ok");
    }

    #[test]
    fn player_team_and_team_color_sync() {
        let mut doc = Document::new_place();
        doc.ensure_services();
        let teams = doc.find_child(doc.dom.root_ref(), "Teams").unwrap();
        let players = doc.find_child(doc.dom.root_ref(), "Players").unwrap();
        let red_bc = rbx_dom_weak::types::BrickColor::ReallyRed;
        let blu_bc = rbx_dom_weak::types::BrickColor::ReallyBlue;
        let spec_bc = rbx_dom_weak::types::BrickColor::White;
        doc.dom.insert(
            teams,
            rbx_dom_weak::InstanceBuilder::new("Team")
                .with_name("RED")
                .with_property("TeamColor", Variant::BrickColor(red_bc)),
        );
        doc.dom.insert(
            teams,
            rbx_dom_weak::InstanceBuilder::new("Team")
                .with_name("BLU")
                .with_property("TeamColor", Variant::BrickColor(blu_bc)),
        );
        let spec = doc.dom.insert(
            teams,
            rbx_dom_weak::InstanceBuilder::new("Team")
                .with_name("Spectator")
                .with_property("TeamColor", Variant::BrickColor(spec_bc)),
        );
        doc.dom.insert(
            players,
            rbx_dom_weak::InstanceBuilder::new("Player")
                .with_name("P1")
                .with_property("Team", Variant::Ref(spec))
                .with_property("TeamColor", Variant::BrickColor(spec_bc))
                .with_property("Neutral", Variant::Bool(false)),
        );
        let rt = Runtime::new().expect("runtime");
        rt.enter(World::new(doc, RunMode::Play));
        rt.bind_globals().expect("globals");
        let res: String = rt
            .lua
            .load(
                r#"
                local P = game:GetService("Players"):FindFirstChild("P1")
                local T = game:GetService("Teams")
                local log = {}
                T.RED.PlayerAdded:Connect(function(p) table.insert(log, "+RED:" .. p.Name) end)
                T.RED.PlayerRemoved:Connect(function(p) table.insert(log, "-RED:" .. p.Name) end)
                T.Spectator.PlayerRemoved:Connect(function(p) table.insert(log, "-Spec:" .. p.Name) end)
                local teamChanged = 0
                P:GetPropertyChangedSignal("Team"):Connect(function() teamChanged += 1 end)

                -- Setting TeamColor while not Neutral updates Team and fires Team signals
                P.TeamColor = BrickColor.new(1, 0, 0)
                assert(P.Team == T.RED, "TeamColor -> Team RED")
                assert(#T.RED:GetPlayers() == 1 and T.RED:GetPlayers()[1] == P, "RED GetPlayers")
                assert(teamChanged == 1, "Team signal fired")

                -- Setting Team updates TeamColor and Neutral
                P.Team = T.BLU
                assert(P.TeamColor == T.BLU.TeamColor and P.Neutral == false, "Team -> TeamColor BLU")
                assert(#T.RED:GetPlayers() == 0 and #T.BLU:GetPlayers() == 1, "BLU GetPlayers")

                -- Setting Neutral = true clears Team; setting Neutral = false restores Team from TeamColor
                P.Neutral = true
                assert(P.Team == nil, "Neutral true clears Team")
                P.TeamColor = BrickColor.new(1, 0, 0)
                assert(P.Team == nil, "TeamColor while Neutral stays nil")
                P.Neutral = false
                assert(P.Team == T.RED, "Neutral false resolves Team from TeamColor")

                -- Setting Team = nil sets Neutral = true
                P.Team = nil
                assert(P.Neutral == true and P.Team == nil, "Team nil sets Neutral")
                return table.concat(log, ",")
                "#,
            )
            .eval()
            .expect("team sync test");
        assert!(res.contains("-Spec:P1") && res.contains("+RED:P1") && res.contains("-RED:P1"), "got {res}");
    }
}
