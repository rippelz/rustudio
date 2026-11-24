//! Members only Roblox's CoreScripts call (core_scripts.rs runs them from
//! Studio's data model patch): ScriptContext:AddCoreScriptLocal, the
//! DataModel's fast-flag and engine-feature queries,
//! UserInputService:GetPlatform, GuiService:GetNotificationTypeList, and
//! MessageBusService / LocalStorageService (services_core.luau). Each is in
//! the API dump with RobloxScriptSecurity (or LocalUserSecurity: GetFast*,
//! GetEngineFeature); the runtime's security check refuses game scripts
//! before these run.
//!
//! Flag values: rustudio has none of Roblox's served flag settings, so a
//! defined flag keeps the default its CoreScript gives (`DefineFastFlag(name,
//! default)`), and a flag nothing defined reads false / 0 / "" (estimate:
//! the engine's own C++ flags are unknown here). Engine features answer
//! false: they gate engine API newer than what rustudio has, and the
//! CoreScripts take their older path without them (estimate).

use std::collections::HashMap;

use mlua::{Function, Lua, MultiValue, ObjectLike, Table, Value};
use rbx_dom_weak::types::Ref;
use rbx_dom_weak::InstanceBuilder;

use crate::runtime::*;

/// Loads services_core.luau (MessageBusService, LocalStorageService) with
/// the services chunk's helpers, plus `new_object(class)`: a parentless
/// engine-made instance (MessageBusService:Subscribe's MessageBusConnection).
pub(super) fn install(lua: &Lua, rs: &Table) -> mlua::Result<()> {
    rs.set(
        "new_object",
        lua.create_function(|lua, class: String| {
            let r = {
                let mut w = world(lua)?;
                let r = w.dm().dom.insert(Ref::none(), InstanceBuilder::new(class.as_str()).with_name(class.as_str()));
                w.orphans.insert(r);
                r
            };
            inst_value(lua, r)
        })?,
    )?;
    let m: Table = lua.load(include_str!("services_core.luau")).set_name("=rustudio.services.core").call(rs.clone())?;
    lua.set_named_registry_value("svc_core_impl", m)
}

/// The fast flags defined in this DataModel (DefineFastFlag / Int / String).
#[derive(Default)]
pub(super) struct FastFlags(HashMap<String, Fast>);

#[derive(Clone)]
enum Fast {
    Flag(bool),
    Int(i64),
    Str(String),
}

fn name_arg(args: &MultiValue, method: &str) -> mlua::Result<String> {
    match arg(args, 1) {
        Value::String(s) => Ok(s.to_str()?.to_string()),
        _ => Err(rt_err(format!("{method}: Argument 1 missing or nil"))),
    }
}

fn one(lua: &Lua, f: Fast) -> mlua::Result<MultiValue> {
    Ok(MultiValue::from_iter([match f {
        Fast::Flag(b) => Value::Boolean(b),
        Fast::Int(i) => Value::Integer(i),
        Fast::Str(s) => Value::String(lua.create_string(s)?),
    }]))
}

fn studio_fast_flag(flag: &str) -> Option<bool> {
    match flag {
        // Measured live in Studio 0.747/0.748 (StudioAppSettings.json + StudioIxpSettings.json):
        // Chrome Unibar TopBar, ChatInputBarV2, BubbleChatConfiguration children, channel tabs,
        // SettingsHub Gallery tab, People grid view, and blue Resume button with focus nav.
        "EnableInGameMenuChrome"
        | "AddTopBarScrim"
        | "ExpChatNewInputBar4"
        | "SetUpBubbleChatConfigurationChildProperties"
        | "ExpChatAlignSystemMessageStyling"
        | "ExpChatChannelTabSizingUpdate2"
        | "ExpChatChannelTabRefactor2"
        | "ExpChatFixChannelTabsMidExperienceToggle"
        | "ExpChatUseChannelTabsStore6"
        // ChatInputBarV2: round custom send button, pill corners and truncation
        | "ExpChatInputBarV2CustomSendButton"
        | "ExpChatInputBarV2CornerAndTruncationFix"
        | "ExpChatPresetButtonOldInputBarV2"
        | "ExpChatPresetButtonGlobalTabFix"
        // SettingsHub: Gallery tab (Captures -> Gallery)
        | "IGMChangeCapturesToGallery"
        | "IGMChangeGalleryHeaderIcon"
        | "UpdateGalleryIcon"
        | "GalleryStopGap"
        | "EnableCapturesGalleryRetrievalCoreScript_v3"
        | "EnableCapturesGalleryRetrieval2"
        | "EnableSettingsHubUIDelegateRollout"
        | "EnableSystemScrimInSettingsHub"
        // SettingsHub: People tab grid/card layout
        | "RefactorPeoplePage9"
        | "AddPeoplePageCardLayout4"
        | "EnableSettingsHubCreateReactPage2"
        | "EnablePeoplePageButtonOnMount"
        | "NewPeoplePageIcons5"
        | "RenderPeoplePageOnTabSwitch"
        | "PeoplePageCardMenuUseVisibleProperty"
        | "FixPeoplePageCardTooltip"
        | "PeoplePagePostponeInitialRender"
        | "PeoplePageRemoveSkeletons"
        | "PeoplePageRemoveUseMemoElements"
        | "PeopleCardsEnableVirtualizedGrid"
        // SettingsHub: MenuButtons (blue Resume + focus navigation)
        | "RelocateMobileMenuButtons4"
        | "AddTraversalHistoryReactMenuButtons"
        | "MenuButtonsMountWithIEM"
        | "MenuButtonsCheckVisibilityBeforeMount"
        | "MenuButtonsCheckVisibilityBeforeMount2"
        | "MenuButtonsUseKeyImages"
        | "MenuButtonsDisconnectGamepadConnected"
        | "MenuButtonsReplaceUseEffect"
        | "MenuButtonsSkipAnimation"
        | "MenuButtonsIncreaseIconSize"
        | "RefactorMenuConfirmationButtons5"
        | "IEMFocusNavSupportNewButtons2"
        | "IEMFocusNavToButtons"
        | "IEMFocusNavPeoplePageToButtons"
        | "IEMTabFocusNav"
        // Built-in TerrainEditor: MaterialGrid forwards AutomaticSize/ScrollingDirection to its Grid pane.
        | "MaterialGridExtraProps2"
        // Studio 0.748 built-in plugin lazy startup flags (StudioAppSettings.json):
        // defer mounting React/Rodux trees until first user interaction via PluginLoaderBuilder.
        | "ImprovePluginSpeedPivotEditor"
        | "ImprovePluginSpeed_AssetManager"
        | "ImprovePluginSpeed_TagEditor"
        | "ImprovePluginSpeed_ManageCollaborators"
        | "FoundationLazyOverlayLoading"
        // Studio 0.748 GameSettings flags (StudioAppSettings.json + live Studio 0.748):
        // GameSettingsExperienceGuidelines is asserted by BrowserUtils.OpenExperienceQuestionnaire
        // (Additional Settings / CreatorHub page) and BasicInfoPage.Components.Guidelines.
        | "GameSettingsExperienceGuidelines"
        | "PruneGameSettings"
        | "RemoveGameSettingsMonetizationPage"
        | "RemoveGameSettingsPermissionsBanner"
        | "RemoveQuestionnaireFromGameSettings"
        | "EnableRestartServersRedirectInGameSettings"
        | "GameSettingsAddCreatorHubLinkToFooter"
        | "GameSettingsGameToExperience"
        | "GameSettingsEditableApiRemoveIdVerification"
        | "GameSettingsRenameOptions"
        | "EnableDeveloperAdsStatsApiMonetizationPageFix" => Some(true),
        "MenuButtonsUseGreyResumeButton"
        | "PeoplePageEnablePersonSignalStore" => Some(false),
        _ => None,
    }
}

fn studio_fast_int(flag: &str) -> Option<i64> {
    match flag {
        // Measured live in Studio 0.748 (StudioAppSettings.json):
        "RelocateMobileMenuButtonsVariant" => Some(2),
        // CoreScripts React work slice. Roblox ships 15 ms (live client and
        // Studio settings), but our Luau runs that much work slower, and at
        // 144 Hz a 15 ms slice drops frames when menus mount. Use React's
        // upstream 5 ms slice: same UI, the work spreads over more frames.
        "ReactSchedulerYieldInterval2" => Some(5),
        // Built-in Toolbox & TerrainEditor FastInt defaults (measured live in Studio 0.748:
        // returning 0 breaks Toolbox Essential section limit=0 / modulo-by-zero insert yielding,
        // and SmoothTerrainMaxCppRegion < selection volume forces TerrainEditor RegionTransform
        // out of LiveEdit into simpleMove, which pastes without clearing the source region):
        "ToolboxHomeViewInitialPageSize" => Some(30),
        "ToolboxAutocompleteDropdownSize" => Some(6),
        "ToolboxYieldInsertPerInstanceModulus" => Some(500),
        "ToolboxAssetDependencyGrantEventTimeout" => Some(5),
        "ToolboxMaxManualRetryAttempts" => Some(3),
        "SmoothTerrainMaxCppRegion" => Some(67_108_864),
        "SmoothTerrainMaxLuaRegion" => Some(4_194_304),
        "TerrainEditorMaxImportVolumeMegaVoxels" => Some(256),
        _ => None,
    }
}

fn studio_engine_feature(feature: &str) -> bool {
    matches!(
        feature,
        // Measured live in Studio 0.747/0.748
        "InGameChromeSignalAPI"
            | "EnablePerBubbleCustomization"
            | "EnableDisplayBubble"
            | "EnableChatInputBarConfigurationAutocompleteEnabled"
            | "EnableBubbleChatFromChatService"
            | "EnableTCSChatTranslation"
            | "DeviceSafeInsetsSupportLuaApp"
            | "GuiHidingApiSupport"
            | "CoreAttributes"
            | "AvatarChatServiceEnabled"
            | "AddPeoplePageLayoutSettings"
            | "CoreScriptGalleryRetrieval"
    )
}

pub(super) fn get_global_flag(lua: &Lua, method: &str, flag: &str) -> mlua::Result<MultiValue> {
    let defined = super::state(lua)?.fast_flags.0.get(flag).cloned();
    if method == "GetFFlag" {
        let b = match defined {
            Some(Fast::Flag(b)) => b,
            _ => studio_fast_flag(flag).unwrap_or(false),
        };
        return Ok(MultiValue::from_iter([Value::Boolean(b)]));
    }
    let s = match defined {
        Some(Fast::Str(s)) => s,
        Some(Fast::Int(i)) => i.to_string(),
        Some(Fast::Flag(b)) => b.to_string(),
        None => if let Some(i) = studio_fast_int(flag) {
            i.to_string()
        } else {
            match flag {
                "InGameGlobalGuiInset" => "36".to_string(),
                "FriendRequestNotificationThrottle" => "5".to_string(),
                "NewDevConsoleMaxGraphCount" => "60".to_string(),
                "NewDevConsoleMaxLogCount" | "NewDevConsoleMaxHttpCount" => "500".to_string(),
                "StudioSearchWithoutInsertionTimeSeconds" => "60".to_string(),
                "FileMaxSizeBytes" => "20971520".to_string(),
                _ => String::new(),
            }
        },
    };
    Ok(MultiValue::from_iter([Value::String(lua.create_string(s)?)]))
}

pub(super) fn call(lua: &Lua, owner: &str, name: &str, args: &MultiValue) -> mlua::Result<MultiValue> {
    if matches!(
        owner,
        "MessageBusService"
            | "MessageBusConnection"
            | "LocalStorageService"
            | "StarterGui"
            | "RunService"
            | "IXPService"
            | "MemStorageService"
            | "MemStorageConnection"
            | "RbxAnalyticsService"
            | "SessionService"
            | "ExperienceService"
            | "PolicyService"
            | "SafetyService"
            | "PermissionsService"
            | "UserGameSettings"
            | "ThirdPartyUserService"
            | "NotificationService"
            | "AdService"
            | "AchievementService"
            | "Animator"
            | "CaptureService"
            | "HttpService"
            | "HttpRequest"
            | "AvatarEditorService"
            | "GenericChallengeService"
            | "AvatarChatService"
            | "VideoCaptureService"
            | "CoreGui"
            | "EventIngestService"
            | "SocialService"
            | "GroupService"
            | "AvatarCreationService"
            | "MarketplaceService"
            | "PlayerScripts"
            | "TelemetryService"
            | "AudioFocusService"
            | "SoundService"
            | "MaterialService"
    ) {
        let m: Table = lua.named_registry_value("svc_core_impl")?;
        if let Ok(f) = m.get::<Function>(format!("{owner}.{name}")) {
            return f.call(args.clone());
        }
    }
    match name {
        "GetTextSize" => {
            let text = match arg(args, 1) {
                Value::String(s) => s.to_str()?.to_string(),
                _ => return Err(rt_err("GetTextSize: Argument 1 missing or nil")),
            };
            let size = match arg(args, 2) {
                Value::Integer(n) => n as f32,
                Value::Number(n) => n as f32,
                _ => return Err(rt_err("GetTextSize: Argument 2 missing or nil")),
            };
            let font_name = match arg(args, 3) {
                Value::UserData(u) => u.get::<String>("Name").unwrap_or_else(|_| "SourceSans".into()),
                Value::Table(t) => t.get::<String>("Name").unwrap_or_else(|_| "SourceSans".into()),
                Value::String(s) => s.to_str().map(|x| x.to_string()).unwrap_or_else(|_| "SourceSans".into()),
                _ => "SourceSans".into(),
            };
            let frame_w = match arg(args, 4) {
                Value::UserData(u) => u.get::<f64>("X").unwrap_or(0.0) as f32,
                Value::Table(t) => t.get::<f64>("X").unwrap_or(0.0) as f32,
                _ => 0.0,
            };
            let fk = crate::gui::legacy_face_key(&font_name);
            let [w, h] = crate::gui::measure_text_string(&text, size, &fk, frame_w, false);
            let v2_new: Function = lua.globals().get::<Table>("Vector2")?.get("new")?;
            Ok(MultiValue::from_iter([v2_new.call::<Value>((w as f64, h as f64))?]))
        }
        "GetTextBoundsAsync" => {
            let p = opt_inst(&arg(args, 1))
                .ok_or_else(|| rt_err("GetTextBoundsAsync: GetTextBoundsParams expected"))?;
            let (text, size, fk, width, rich) = {
                let w = world(lua)?;
                let d = w.d();
                let text = match d.prop(p, "Text") {
                    Some(rbx_dom_weak::types::Variant::String(s)) => s.clone(),
                    _ => String::new(),
                };
                let size = match d.prop(p, "Size") {
                    Some(rbx_dom_weak::types::Variant::Float32(n)) => n,
                    Some(rbx_dom_weak::types::Variant::Float64(n)) => n as f32,
                    Some(rbx_dom_weak::types::Variant::Int32(n)) => n as f32,
                    _ => 16.0,
                };
                let width = match d.prop(p, "Width") {
                    Some(rbx_dom_weak::types::Variant::Float32(n)) => n,
                    Some(rbx_dom_weak::types::Variant::Float64(n)) => n as f32,
                    Some(rbx_dom_weak::types::Variant::Int32(n)) => n as f32,
                    _ => 0.0,
                };
                let rich = matches!(d.prop(p, "RichText"), Some(rbx_dom_weak::types::Variant::Bool(true)));
                let fk = match d.prop(p, "Font") {
                    Some(rbx_dom_weak::types::Variant::Font(f)) => {
                        let wt = match f.weight {
                            rbx_dom_weak::types::FontWeight::Thin => 100,
                            rbx_dom_weak::types::FontWeight::ExtraLight => 200,
                            rbx_dom_weak::types::FontWeight::Light => 300,
                            rbx_dom_weak::types::FontWeight::Regular => 400,
                            rbx_dom_weak::types::FontWeight::Medium => 500,
                            rbx_dom_weak::types::FontWeight::SemiBold => 600,
                            rbx_dom_weak::types::FontWeight::Bold => 700,
                            rbx_dom_weak::types::FontWeight::ExtraBold => 800,
                            rbx_dom_weak::types::FontWeight::Heavy => 900,
                            _ => 400,
                        };
                        let italic = matches!(f.style, rbx_dom_weak::types::FontStyle::Italic);
                        crate::gui::normalize_face_key(&f.family, wt, italic)
                    }
                    _ => crate::gui::legacy_face_key("SourceSans"),
                };
                (text, size, fk, width, rich)
            };
            let [w, h] = crate::gui::measure_text_string(&text, size, &fk, width, rich);
            let v2_new: Function = lua.globals().get::<Table>("Vector2")?.get("new")?;
            Ok(MultiValue::from_iter([v2_new.call::<Value>((w as f64, h as f64))?]))
        }
        "GetTextSizeOffsetAsync" => Ok(MultiValue::from_iter([Value::Number(0.0)])),
        "OpenScreenshotsFolder" => Ok(MultiValue::new()),
        "GetMaxQualityLevel" => Ok(MultiValue::from_iter([Value::Integer(21)])),
        "AddCoreScriptLocal" => {
            let script = name_arg(args, name)?;
            let parent = opt_inst(&arg(args, 2)).unwrap_or(Ref::none());
            if start_core_script(lua, &script, parent)?.is_none() {
                return Err(rt_err(format!("AddCoreScriptLocal: no CoreScript named '{script}'")));
            }
            Ok(MultiValue::new())
        }
        "LoadLocalAsset" => {
            let uri = name_arg(args, name)?;
            let path = rustudio_core::assets::resolve(&uri, None)
                .ok_or_else(|| rt_err(format!("LoadLocalAsset: could not resolve '{uri}'")))?;
            let bytes = std::fs::read(&path).map_err(|e| rt_err(format!("LoadLocalAsset {uri}: {e}")))?;
            let mut dom = rustudio_core::doc::Document::decode(&bytes)
                .map_err(|e| rt_err(format!("LoadLocalAsset {uri}: {e}")))?;
            let first = dom
                .root()
                .children()
                .first()
                .copied()
                .ok_or_else(|| rt_err(format!("LoadLocalAsset {uri}: empty model")))?;
            let (r, holder) = {
                let mut w = world(lua)?;
                let holder = w.dm().dom.insert(Ref::none(), InstanceBuilder::new("Folder").with_name("_tmp_local_asset"));
                dom.transfer(first, &mut w.dm().dom, holder);
                w.orphans.insert(first);
                (first, holder)
            };
            set_parent(lua, r, Ref::none())?;
            if let Ok(mut w) = world(lua) {
                w.dm().dom.destroy(holder);
            }
            Ok(MultiValue::from_iter([inst_value(lua, r)?]))
        }
        "GetLocalFileContents" => {
            let uri = name_arg(args, name)?;
            let path = rustudio_core::assets::resolve(&uri, None)
                .ok_or_else(|| rt_err(format!("GetLocalFileContents: could not resolve '{uri}'")))?;
            let bytes = std::fs::read(&path).map_err(|e| rt_err(format!("GetLocalFileContents {uri}: {e}")))?;
            Ok(MultiValue::from_iter([Value::String(lua.create_string(bytes)?)]))
        }
        "DefineFastFlag" | "DefineFastInt" | "DefineFastString" => {
            let flag = name_arg(args, name)?;
            let default = match (name, arg(args, 2)) {
                ("DefineFastFlag", Value::Boolean(b)) => Fast::Flag(studio_fast_flag(&flag).unwrap_or(b)),
                ("DefineFastInt", Value::Integer(i)) => Fast::Int(studio_fast_int(&flag).unwrap_or(i)),
                ("DefineFastInt", Value::Number(n)) => Fast::Int(studio_fast_int(&flag).unwrap_or(n as i64)),
                ("DefineFastString", Value::String(s)) => Fast::Str(s.to_str()?.to_string()),
                _ => return Err(rt_err(format!("{name}: Argument 2 missing or nil"))),
            };
            let v = super::state(lua)?.fast_flags.0.entry(flag).or_insert(default).clone();
            one(lua, v)
        }
        "GetFastFlag" | "GetFastInt" | "GetFastString" => {
            let flag = name_arg(args, name)?;
            let defined = super::state(lua)?.fast_flags.0.get(&flag).cloned();
            let v = match (name, defined) {
                ("GetFastFlag", Some(v @ Fast::Flag(_)))
                | ("GetFastInt", Some(v @ Fast::Int(_)))
                | ("GetFastString", Some(v @ Fast::Str(_))) => v,
                ("GetFastFlag", _) => Fast::Flag(studio_fast_flag(&flag).unwrap_or(false)),
                ("GetFastInt", _) => Fast::Int(studio_fast_int(&flag).unwrap_or(0)),
                _ => Fast::Str(String::new()),
            };
            one(lua, v)
        }
        // A flag's served version: rustudio serves none.
        "SetFlagVersion" => {
            name_arg(args, name)?;
            Ok(MultiValue::new())
        }
        // No Roblox play session behind a local Play (estimate: empty).
        "GetPlaySessionId" => Ok(MultiValue::from_iter([Value::String(lua.create_string("")?)])),
        // Studio's client answers the platform Studio runs on; rustudio
        // answers Windows off macOS, the platform whose Studio it follows
        // elsewhere (estimate).
        "GetPlatform" => {
            let platform = if cfg!(target_os = "macos") { 1 } else { 0 };
            Ok(MultiValue::from_iter([super::enum_val(lua, "Platform", platform)?]))
        }
        // The app's notification types by name. Only the two Roblox's own
        // test double (Modules/TestHelpers/MockGuiService.lua) lists are known.
        "GetNotificationTypeList" => {
            let t = lua.create_table()?;
            t.set("NATIVE_EXIT", 35)?;
            t.set("RESTART_APP", 41)?;
            Ok(MultiValue::from_iter([Value::Table(t)]))
        }
        // "GetEngineFeature"
        _ => {
            let feat = name_arg(args, name)?;
            Ok(MultiValue::from_iter([Value::Boolean(studio_engine_feature(&feat))]))
        }
    }
}
