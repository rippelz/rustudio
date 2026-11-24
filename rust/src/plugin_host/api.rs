//! Studio's API domain: plugin — the Plugin object and what plugins make
//! (`plugin_host::api`; studio registers it in the engine at start-up,
//! `studio_hooks::install`; every `api*.rs` file here is scanned for member
//! claims by build.rs).
//!
//! This file: the dispatcher, the Plugin core (settings, activation, the
//! mouse object, user id, Unloading / Deactivation) and Selection's base
//! (Get / Set / Add / Remove / SelectionChanged on the editor's selection).
//! Hook files, one per area:
//! - api_toolbar.rs: CreateToolbar, PluginToolbar, PluginToolbarButton
//! - api_widgets.rs: CreateDockWidgetPluginGui(Async), CreateQWidgetPluginGui,
//!   PluginGui members
//! - api_menus.rs:   CreatePluginAction, CreatePluginMenu, PluginMenu
//! - api_mouse.rs:   PluginMouse's properties (MouseState, fed by input)
//! - api_studio.rs:  OpenScript, OpenWikiPage, ribbon tools, CSG, FBX
//!   import, asset / save prompts, StartDrag, SaveSelectedToRoblox
//! A member whose behaviour isn't there yet still makes the right object
//! and warns once in Output (vm::todo), never a silent no-op.

#![allow(unused_imports, dead_code)]

use mlua::{Lua, MultiValue, Table, Value};
use rbx_dom_weak::types::{Ref, Variant};

use rustudio_core::log::Level;
use rustudio_engine::ltypes::rt_err;
use crate::plugin_host::vm::{self, UiKind};
use rustudio_engine::runtime::*;

#[path = "api_toolbar.rs"]
pub mod toolbar;
#[path = "api_widgets.rs"]
pub mod widgets;
#[path = "api_menus.rs"]
pub mod menus;
#[path = "api_mouse.rs"]
pub mod mouse;
#[path = "api_studio.rs"]
pub mod studio;
#[path = "api_theme.rs"]
pub mod theme;
#[path = "api_history.rs"]
pub mod history;
#[path = "api_script_editor.rs"]
pub mod script_editor;
#[path = "api_studio_service.rs"]
pub mod studio_service;
#[path = "api_fbx_anim.rs"]
pub mod fbx_anim;
#[path = "api_text.rs"]
pub mod text;
#[path = "api_testing.rs"]
mod testing;
#[path = "api_builtin.rs"]
pub mod builtin;

/// The plugin domain's member claims (build.rs scans src/plugin_host/api*.rs).
/// The engine's member_claims.rs cannot include them (this file is above the
/// engine); the engine takes them through the Domain registered at start-up.
pub mod claims {
    include!(concat!(env!("OUT_DIR"), "/plugin_claims.rs"));
}

pub const METHODS: &[(&str, &str)] = &[
    // Plugin core (this file)
    ("GetSetting", "Plugin"),
    ("SetSetting", "Plugin"),
    ("GetStudioUserId", "Plugin"),
    ("IsActivated", "Plugin"),
    ("IsActivatedWithExclusiveMouse", "Plugin"),
    ("Activate", "Plugin"),
    ("Deactivate", "Plugin"),
    ("GetMouse", "Plugin"),
    ("GetJoinMode", "Plugin"),
    // Selection's base (this file)
    ("Get", "Selection"),
    ("Set", "Selection"),
    ("Add", "Selection"),
    ("Remove", "Selection"),
    ("AddFocusCallback", "Selection"),
    ("SetTerrainSelectionHack", "Selection"),
    ("ClearTerrainSelectionHack", "Selection"),
    // api_toolbar.rs
    ("CreateToolbar", "Plugin"),
    ("CreateButton", "PluginToolbar"),
    ("CreatePopupButton", "PluginToolbar"),
    ("SetActive", "PluginToolbarButton"),
    ("SetDropdownActive", "PluginToolbarButton"),
    // api_widgets.rs (CreateDockWidgetPluginGui(Async) yield: LUA_METHODS)
    ("CreateQWidgetPluginGui", "Plugin"),
    ("GetRelativeMousePosition", "PluginGui"),
    // api_menus.rs
    ("CreatePluginAction", "Plugin"),
    ("CreatePluginMenu", "Plugin"),
    ("AddAction", "PluginMenu"),
    ("AddMenu", "PluginMenu"),
    ("AddNewAction", "PluginMenu"),
    ("AddSeparator", "PluginMenu"),
    ("Clear", "PluginMenu"),
    ("ShowAsync", "PluginMenu"),
    // api_theme.rs
    ("GetColor", "StudioTheme"),
    ("GetAvailableThemes", "Studio"),
    // api_studio.rs
    ("OpenScript", "Plugin"),
    ("OpenWikiPage", "Plugin"),
    ("GetSelectedRibbonTool", "Plugin"),
    ("SelectRibbonTool", "Plugin"),
    ("Union", "Plugin"),
    ("Negate", "Plugin"),
    ("Separate", "Plugin"),
    ("Intersect", "Plugin"),
    ("StartDrag", "Plugin"),
    ("SaveSelectedToRoblox", "Plugin"),
    // api_history.rs
    ("SetWaypoint", "ChangeHistoryService"),
    ("GetCanUndo", "ChangeHistoryService"),
    ("GetCanRedo", "ChangeHistoryService"),
    ("Undo", "ChangeHistoryService"),
    ("Redo", "ChangeHistoryService"),
    ("ResetWaypoints", "ChangeHistoryService"),
    ("SetEnabled", "ChangeHistoryService"),
    ("TryBeginRecording", "ChangeHistoryService"),
    ("FinishRecording", "ChangeHistoryService"),
    ("IsRecordingInProgress", "ChangeHistoryService"),
    // api_script_editor.rs
    ("FindScriptDocument", "ScriptEditorService"),
    ("GetScriptDocuments", "ScriptEditorService"),
    ("OpenScriptDocumentAsync", "ScriptEditorService"),
    ("GetEditorSource", "ScriptEditorService"),
    ("UpdateSourceAsync", "ScriptEditorService"),
    ("RegisterAutocompleteCallback", "ScriptEditorService"),
    ("RegisterScriptAnalysisCallback", "ScriptEditorService"),
    ("DeregisterAutocompleteCallback", "ScriptEditorService"),
    ("DeregisterScriptAnalysisCallback", "ScriptEditorService"),
    ("IsCommandBar", "ScriptDocument"),
    ("GetScript", "ScriptDocument"),
    ("GetLineCount", "ScriptDocument"),
    ("GetLine", "ScriptDocument"),
    ("GetText", "ScriptDocument"),
    ("GetSelection", "ScriptDocument"),
    ("GetSelectionStart", "ScriptDocument"),
    ("GetSelectionEnd", "ScriptDocument"),
    ("GetSelectedText", "ScriptDocument"),
    ("HasSelectedText", "ScriptDocument"),
    ("GetViewport", "ScriptDocument"),
    ("GetReadOnlyReasons", "ScriptDocument"),
    ("IsReadOnly", "ScriptDocument"),
    ("AddReadOnlyReasonsAsync", "ScriptDocument"),
    ("RemoveReadOnlyReasonsAsync", "ScriptDocument"),
    ("ReviewableTextEditsAsync", "ScriptDocument"),
    ("EditTextAsync", "ScriptDocument"),
    ("MultiEditTextAsync", "ScriptDocument"),
    ("ForceSetSelectionAsync", "ScriptDocument"),
    ("RequestSetSelectionAsync", "ScriptDocument"),
    // this file: Instance:GetDebugId (Plugin security)
    ("GetDebugId", "Instance"),
    // api_text.rs (TextService for every script, not only plugins)
    ("GetTextSize", "TextService"),
    ("GetTextBoundsAsync", "TextService"),
    // api_studio_service.rs
    ("GetClassIcon", "StudioService"),
    ("GizmoRaycast", "StudioService"),
    ("GetUserId", "StudioService"),
    ("HasInternalPermission", "StudioService"),
    ("GetStartupAssetId", "StudioService"),
    ("GetStartupPluginId", "StudioService"),
    ("GetBadgeConfigureUrl", "StudioService"),
    ("GetBadgeUploadUrl", "StudioService"),
    ("GetResourceByCategory", "StudioService"),
    ("GetTermsOfUseUrl", "StudioService"),
    ("CopyToClipboard", "StudioService"),
    ("IsPluginInstalled", "StudioService"),
    ("IsPluginUpToDate", "StudioService"),
    ("OpenInBrowser_DONOTUSE", "StudioService"),
    ("SetPluginEnabled", "StudioService"),
    ("ShowPublishToRoblox", "StudioService"),
    ("UninstallPlugin", "StudioService"),
    ("UpdatePluginManagement", "StudioService"),
    ("TryInstallPlugin", "StudioService"),
    ("AnimationIdSelected", "StudioService"),
    // StudioAssetService / BrowserService
    ("ShowSaveToRoblox", "StudioAssetService"),
    ("ResolveSaveInstanceToRoblox", "StudioAssetService"),
    ("PublishPackage", "StudioAssetService"),
    ("UpdatePublishedPackage", "StudioAssetService"),
    ("ConvertToPackageUpload", "StudioAssetService"),
    ("FireOnUGCSubmitCompleted", "StudioAssetService"),
    ("SerializeInstances", "StudioAssetService"),
    ("DEPRECATED_SerializeInstances", "StudioAssetService"),
    ("OpenBrowserWindow", "BrowserService"),
    ("CloseBrowserWindow", "BrowserService"),
    ("OpenNativeOverlay", "BrowserService"),
    ("SendCommand", "BrowserService"),
    ("EmitHybridEvent", "BrowserService"),
    ("ExecuteJavaScript", "BrowserService"),
    ("ReturnToJavaScript", "BrowserService"),
    ("CopyAuthCookieFromBrowserToEngine", "BrowserService"),
    ("Start", "SmoothVoxelsUpgraderService"),
    ("Cancel", "SmoothVoxelsUpgraderService"),
    ("CancelImportHeightmap", "HeightmapImporterService"),
    ("GetHeightmapPreviewAsync", "HeightmapImporterService"),
    ("ImportHeightmap", "HeightmapImporterService"),
    ("ImportHeightmapWithMaterialSlotsAsync", "HeightmapImporterService"),
    ("IsValidColormap", "HeightmapImporterService"),
    ("IsValidHeightmap", "HeightmapImporterService"),
    ("SetImportHeightmapPaused", "HeightmapImporterService"),
    ("DefineCallout", "CalloutService"),
    ("AttachCallout", "CalloutService"),
    ("DetachCalloutsByDefinitionId", "CalloutService"),
    ("ShowNotification", "ToastNotificationService"),
    ("HideNotification", "ToastNotificationService"),
    ("GetWidgetFromLabel", "StudioWidgetsService"),
    ("GetWidgetFromPluginGui", "StudioWidgetsService"),
    ("ApplyHighlight", "StudioWidgetsService"),
    ("ApplySpotlight", "StudioWidgetsService"),
    ("HideSpotlight", "StudioWidgetsService"),
    ("ApplyShadows", "StudioWidgetsService"),
    ("ApplyFillInBox", "StudioWidgetsService"),
    ("GetAppliedStyles", "StylingService"),
    ("GetStyleInfo", "StylingService"),
    ("GetStyleSheetDerivesChain", "StylingService"),
    ("GetStyleSheetInfo", "StylingService"),
    ("UpdateUnitTestOnly", "StylingService"),
    ("GetScriptFilePath", "CoreScriptSyncService"),
    ("GetBinaryContents", "File"),
    ("GetTemporaryId", "File"),
    ("ExportPlace", "PluginManagerInterface"),
    ("ExportSelection", "PluginManagerInterface"),
    ("CreatePlugin", "PluginManagerInterface"),
    // api_builtin.rs (RobloxScriptSecurity built-in plugin APIs)
    ("GetPluginComponent", "Plugin"),
    ("GetUri", "Plugin"),
    ("GetPreinitPayload", "Plugin"),
    ("FinishFullLoading", "Plugin"),
    ("SetReady", "Plugin"),
    ("OnInvoke", "Plugin"),
    ("OnInvokeSuspendOverride", "Plugin"),
    ("Invoke", "Plugin"),
    ("GetItem", "Plugin"),
    ("SetItem", "Plugin"),
    ("OnSetItem", "Plugin"),
    ("GetPluginPolicy", "PluginPolicyService"),
    ("ConvertToMockPackage", "PackageUIService"),
    ("ConvertToPackageClosedCallback", "PackageUIService"),
    ("ConvertToPackageUpload", "PackageUIService"),
    ("ConvertToPackageAsync", "PackageUIService"),
    ("GetPackageInfo", "PackageUIService"),
    ("PublishPackage", "PackageUIService"),
    ("SetPackageVersion", "PackageUIService"),
    ("ClearUploadNames", "StudioPublishService"),
    ("CloseAfterPublish", "StudioPublishService"),
    ("PublishAs", "StudioPublishService"),
    ("PublishThenTurnOnTeamCreate", "StudioPublishService"),
    ("RefreshDocumentDisplayName", "StudioPublishService"),
    ("RegisterPublishHold", "StudioPublishService"),
    ("SaveOrPublishPlaceToRobloxIsCanceled", "StudioPublishService"),
    ("SetTeamCreateOnPublishInfo", "StudioPublishService"),
    ("SetUniverseDisplayName", "StudioPublishService"),
    ("SetUploadNames", "StudioPublishService"),
    ("ShowSaveOrPublishPlaceToRoblox", "StudioPublishService"),
    // AssetImportService
    ("GetAllPresets", "AssetImportService"),
    ("GetPreset", "AssetImportService"),
    ("RemovePreset", "AssetImportService"),
    ("SavePreset", "AssetImportService"),
    ("StartSessionWithPath", "AssetImportService"),
    ("StartSingleFileWatch", "AssetImportService"),
    ("StopSingleFileWatch", "AssetImportService"),
    ("GetFilesInDirAsync", "AssetImportService"),
    ("PickFileWithPromptAsync", "AssetImportService"),
    ("PickImageFileWithPrompt", "AssetImportService"),
    ("PickMeshFileWithPrompt", "AssetImportService"),
    ("PickMultipleFilesWithPrompt", "AssetImportService"),
    ("StartSessionWithPathAsync", "AssetImportService"),
    ("UploadAssetFromContentAsync", "AssetImportService"),
    ("UploadAssetFromPathAsync", "AssetImportService"),
    ("UploadVersionedAssetFromContentAsync", "AssetImportService"),
    ("UploadVersionedAssetFromPathAsync", "AssetImportService"),
    // CollaboratorsService
    ("GetCollaboratorsList", "CollaboratorsService"),
    ("GetSelectionHighlightsEnabled", "CollaboratorsService"),
    ("MultiGetCanCollaborate", "CollaboratorsService"),
    ("RequestFlyToCollaborator", "CollaboratorsService"),
    ("ToggleSelectionHighlights", "CollaboratorsService"),
    ("ToggleTeamCreate", "CollaboratorsService"),
    // TeamCreateService
    ("CloseGameIfUserDoesntHavePerms", "TeamCreateService"),
];

/// Events this domain (or the host) fires.
pub const EVENTS: &[(&str, &str)] = &[
    // vm::unload
    ("Unloading", "Plugin"),
    // Activate / Deactivate here
    ("Deactivation", "Plugin"),
    // Set / Add / Remove here, the editor's selection (edit_sync.rs)
    ("SelectionChanged", "Selection"),
    ("SelectionChangedThisFrame", "Selection"),
    ("Status", "SmoothVoxelsUpgraderService"),
    ("ColormapHasUnknownPixels", "HeightmapImporterService"),
    ("ProgressUpdate", "HeightmapImporterService"),
    // the harness's --action plugin-button:… (PluginHost::trigger)
    ("Click", "PluginToolbarButton"),
    ("DropdownClick", "PluginToolbarButton"),
    // the harness's --action plugin-action:…
    ("Triggered", "PluginAction"),
    // settings().Studio.Theme written (services.rs)
    ("ThemeChanged", "Studio"),
    // api_history.rs (queued on the document, fired by the host)
    ("OnUndo", "ChangeHistoryService"),
    ("OnRedo", "ChangeHistoryService"),
    ("OnRecordingStarted", "ChangeHistoryService"),
    ("OnRecordingFinished", "ChangeHistoryService"),
    // api_script_editor.rs (script_docs.rs events, fired by the host)
    ("TextDocumentDidOpen", "ScriptEditorService"),
    ("TextDocumentDidClose", "ScriptEditorService"),
    ("TextDocumentDidChange", "ScriptEditorService"),
    ("SelectionChanged", "ScriptDocument"),
    ("ViewportChanged", "ScriptDocument"),
    // api_widgets.rs / widgets_ui.rs: the widget's panel focus and a
    // plugin:StartDrag drag over it
    ("WindowFocused", "PluginGui"),
    ("WindowFocusReleased", "PluginGui"),
    ("PluginDragEntered", "PluginGui"),
    ("PluginDragMoved", "PluginGui"),
    ("PluginDragLeft", "PluginGui"),
    ("PluginDragDropped", "PluginGui"),
    // widgets_ui.rs: Toolbox instances dragged into the 3D view while the
    // plugin's mouse is active (api-walk.txt: "connect ok" in Studio)
    ("DragEnter", "PluginMouse"),
    // Built-in plugin DataModel lifecycle & drag events
    ("Ready", "Plugin"),
    ("ViewportDragDropped", "Plugin"),
    ("ViewportDragEntered", "Plugin"),
    ("ViewportDragLeft", "Plugin"),
    ("DataModelSessionStarted", "MultipleDocumentInterfaceInstance"),
    ("DataModelSessionEnded", "MultipleDocumentInterfaceInstance"),
    ("CurrentDataModelTypeAboutToChange", "DataModelSession"),
    ("CurrentDataModelTypeChanged", "DataModelSession"),
    // MouseService viewport hover events (widgets_ui.rs)
    ("MouseEnterStudioViewport", "MouseService"),
    ("MouseLeaveStudioViewport", "MouseService"),
    // Built-in plugin service open/publish signals
    ("OnOpenGameSettings", "StudioService"),
    ("OnImportFromRoblox", "StudioService"),
    ("OnOpenManagePackagePlugin", "StudioService"),
    ("OnPluginInstalledFromToolbox", "StudioService"),
    ("OnPluginInstalledFromWeb", "StudioService"),
    ("OnPublishAsPlugin", "StudioService"),
    ("OnSaveToRoblox", "StudioService"),
    ("PromptTransformPluginCheckEnable", "StudioService"),
    ("SaveLocallyAsComplete", "StudioService"),
    ("OnSaveToRoblox", "StudioAssetService"),
    ("OnConvertToPackageResult", "StudioAssetService"),
    ("OnPromptSaveInstanceToRobloxAsync", "StudioAssetService"),
    ("OnPublishPackageResult", "StudioAssetService"),
    ("OnUGCSubmitCompleted", "StudioAssetService"),
    ("AuthCookieCopiedToEngine", "BrowserService"),
    ("BrowserWindowClosed", "BrowserService"),
    ("BrowserWindowWillNavigate", "BrowserService"),
    ("JavaScriptCallback", "BrowserService"),
    ("OnOpenConvertToPackagePlugin", "PackageUIService"),
    ("OnConvertToPackageResult", "PackageUIService"),
    ("OnSaveOrPublishPlaceToRoblox", "StudioPublishService"),
    ("GamePublishFinished", "StudioPublishService"),
    ("GamePublishCancelled", "StudioPublishService"),
    ("GameNameUpdated", "StudioPublishService"),
    ("OnPublishAttempt", "StudioPublishService"),
    ("SingleFileChanged", "AssetImportService"),
    ("StartSingleMeshImport", "AssetImportService"),
    ("CollaboratorIdleUpdate", "CollaboratorsService"),
    ("CollaboratorInstanceCreatedSignal", "CollaboratorsService"),
    ("CollaboratorInstanceDestroyedSignal", "CollaboratorsService"),
    ("CollaboratorStatusUpdatedSignal", "CollaboratorsService"),
    ("MultiGetCanCollaborateRetrieved", "CollaboratorsService"),
    ("ServerMultiGetCanCollaborateRequested", "CollaboratorsService"),
    ("ToggleSelectionHighlightsSignal", "CollaboratorsService"),
];

pub const LUA_METHODS: &[(&str, &str)] = &[
    ("ExecutePlayModeAsync", "StudioTestService"),
    ("ExecuteRunModeAsync", "StudioTestService"),
    // api_script_editor.rs (yields a frame: DidClose fires first)
    ("CloseAsync", "ScriptDocument"),
    // api_studio_service.rs (dialogs: yield until answered)
    ("PromptImportFile", "StudioService"),
    ("PromptImportFileAsync", "StudioService"),
    ("PromptImportFiles", "StudioService"),
    ("PromptImportFilesAsync", "StudioService"),
    ("ImportFbxAnimation", "Plugin"),
    ("ImportFbxAnimationAsync", "Plugin"),
    ("ImportFbxRig", "Plugin"),
    ("ImportFbxRigAsync", "Plugin"),
    ("PromptForExistingAssetId", "Plugin"),
    ("PromptForExistingAssetIdAsync", "Plugin"),
    ("PromptSaveSelection", "Plugin"),
    ("PromptSaveSelectionAsync", "Plugin"),
    // api_widgets.rs: made at once, return on the next frame (Studio yields)
    ("CreateDockWidgetPluginGui", "Plugin"),
    ("CreateDockWidgetPluginGuiAsync", "Plugin"),
];

/// The domain the engine holds (its slot in every table is last, where
/// `plugin::` sat when this was an engine module). `studio_hooks::install`
/// registers it once, before any runtime is made.
pub static DOMAIN: rustudio_engine::runtime::domains::Domain = rustudio_engine::runtime::domains::Domain {
    name: "plugin",
    methods: METHODS,
    lua_methods: LUA_METHODS,
    events: EVENTS,
    claims: claims::CLAIMS,
    claim_pairs: claims::CLAIMS_PAIRS,
    call_claims: claims::CALL_CLAIMS,
    call_claim_pairs: claims::CALL_CLAIMS_PAIRS,
    newindex_claims: claims::NEWINDEX_CLAIMS,
    newindex_claim_pairs: claims::NEWINDEX_CLAIMS_PAIRS,
    prop_get,
    newindex,
    call,
    write_acts,
    install,
    step,
    hold_source_write: crate::plugin_host::permissions::hold_source_write,
    script_env: vm::script_env,
    running_asset_id: vm::running_asset_id,
    plugin_manager: studio_service::plugin_manager,
    check_http: crate::plugin_host::permissions::check_http,
    running_plugin: crate::plugin_host::permissions::running_plugin,
    http_decision: crate::plugin_host::permissions::http_decision,
    theme_get: theme::studio_theme,
    theme_set: theme::set_theme,
    theme_note: theme::note_theme,
    active_script: crate::plugin_host::script_docs::active,
};

/// Registers the plugin domain in the engine (idempotent; call before the
/// first `Runtime::new`).
pub fn install_domain() {
    rustudio_engine::runtime::domains::register(&DOMAIN);
}

fn mine(class: &str) -> bool {
    matches!(
        class,
        "Plugin" | "Selection" | "PluginToolbar" | "PluginToolbarButton" | "PluginMenu" | "PluginMouse"
            | "PluginAction" | "StudioTheme" | "ChangeHistoryService" | "ScriptEditorService" | "ScriptDocument"
            | "StudioService" | "File" | "PluginManagerInterface" | "PluginManager" | "TextService" | "Studio"
            | "MultipleDocumentInterfaceInstance" | "DataModelSession"
            | "SmoothVoxelsUpgraderService" | "HeightmapImporterService"
            | "CalloutService" | "ToastNotificationService" | "StudioWidgetsService"
            | "StylingService" | "CoreScriptSyncService"
            | "PluginPolicyService" | "PackageUIService" | "StudioPublishService"
            | "AssetImportService" | "CollaboratorsService" | "TeamCreateService"
            | "StudioAssetService" | "BrowserService"
    ) || rustudio_core::doc::is_a(class, "PluginGui")
}

pub fn call(lua: &Lua, name: &str, this: Ref, args: &MultiValue) -> Option<mlua::Result<MultiValue>> {
    if name == "GetDebugId" {
        return Some(debug_id(lua, this, args));
    }
    let class = match inst_class(lua, this) {
        Ok((c, _)) => c.to_string(),
        Err(_) => return None,
    };
    if !mine(&class) {
        return None;
    }
    if let Some(r) = core_call(lua, &class, name, this, args) {
        return Some(r);
    }
    if let Some(r) = builtin::call(lua, &class, name, this, args) {
        return Some(r);
    }
    if let Some(r) = toolbar::call(lua, &class, name, this, args) {
        return Some(r);
    }
    if let Some(r) = widgets::call(lua, &class, name, this, args) {
        return Some(r);
    }
    if let Some(r) = menus::call(lua, &class, name, this, args) {
        return Some(r);
    }
    if let Some(r) = theme::call(lua, &class, name, this, args) {
        return Some(r);
    }
    if let Some(r) = history::call(lua, &class, name, this, args) {
        return Some(r);
    }
    if let Some(r) = script_editor::call(lua, &class, name, this, args) {
        return Some(r);
    }
    if let Some(r) = studio_service::call(lua, &class, name, this, args) {
        return Some(r);
    }
    if let Some(r) = text::call(lua, &class, name, this, args) {
        return Some(r);
    }
    if class == "Studio" && name == "GetAvailableThemes" {
        return Some(theme::available_themes(lua).and_then(ret1));
    }
    studio::call(lua, &class, name, this, args)
}

pub fn prop_get(lua: &Lua, r: Ref, class: &str, key: &str) -> mlua::Result<Option<Value>> {
    if !mine(class) {
        return Ok(None);
    }
    if let Some(v) = core_prop_get(lua, r, class, key)? {
        return Ok(Some(v));
    }
    if let Some(v) = mouse::prop_get(lua, r, class, key)? {
        return Ok(Some(v));
    }
    if let Some(v) = toolbar::prop_get(lua, r, class, key)? {
        return Ok(Some(v));
    }
    if let Some(v) = widgets::prop_get(lua, r, class, key)? {
        return Ok(Some(v));
    }
    if let Some(v) = studio_service::prop_get(lua, r, class, key)? {
        return Ok(Some(v));
    }
    if let Some(v) = builtin::prop_get(lua, r, class, key)? {
        return Ok(Some(v));
    }
    menus::prop_get(lua, r, class, key)
}

pub fn newindex(lua: &Lua, r: Ref, class: &str, key: &str, value: &Value) -> mlua::Result<bool> {
    if !mine(class) {
        return Ok(false);
    }
    if core_newindex(lua, r, class, key, value)? {
        return Ok(true);
    }
    if toolbar::newindex(lua, r, class, key, value)? {
        return Ok(true);
    }
    widgets::newindex(lua, r, class, key, value)
}

pub fn write_acts(_class: &str, _key: &str) -> bool {
    false
}

pub fn install(lua: &Lua) -> mlua::Result<()> {
    vm::install(lua);
    menus::install(lua)?;
    script_editor::install(lua)?;
    studio_service::install(lua)?;
    widgets::install(lua)?;
    builtin::install(lua)?;
    testing::install(lua)
}

pub fn step(_lua: &Lua, _dt: f64) -> mlua::Result<()> {
    Ok(())
}

/// Instance:GetDebugId(scopeLength = 4): Studio's shape (checks/plugins/
/// services/captures/history.txt `e.debugid*`): "<scope>_<id>", the scope
/// the DataModel's id in hex cut to `scopeLength` characters (the edit
/// DataModel is 2: "2_xxxxxx", 8 characters), the id six digits, stable for
/// the instance; scopeLength 0 → the id alone.
fn debug_id(lua: &Lua, this: Ref, args: &MultiValue) -> mlua::Result<MultiValue> {
    #[derive(Default)]
    struct Ids(std::collections::HashMap<Ref, u64>, u64);
    let scope_len = match argv(args, 1) {
        Value::Nil => 4,
        Value::Integer(n) => n.max(0) as usize,
        Value::Number(n) => n.max(0.0) as usize,
        v => return Err(rt_err(format!("Unable to cast {} to int", v.type_name()))),
    };
    if lua.app_data_ref::<Ids>().is_none() {
        lua.set_app_data(Ids::default());
    }
    let id = {
        let mut ids = lua.app_data_mut::<Ids>().expect("ids");
        match ids.0.get(&this) {
            Some(i) => *i,
            None => {
                ids.1 += 1;
                let i = 100_000 + ids.1 * 7 % 900_000;
                ids.0.insert(this, i);
                i
            }
        }
    };
    let dm = match world(lua)?.mode {
        RunMode::Edit => 2u32,
        _ => 3,
    };
    let scope: String = format!("{dm:x}").chars().take(scope_len).collect();
    let s = if scope.is_empty() { format!("{id}") } else { format!("{scope}_{id}") };
    ret1(lstr(lua, &s)?)
}

// ------------------------------------------------------------- helpers

pub(crate) fn ret1(v: Value) -> mlua::Result<MultiValue> {
    Ok(MultiValue::from_iter([v]))
}

pub(crate) fn ret0() -> mlua::Result<MultiValue> {
    Ok(MultiValue::new())
}

pub(crate) fn lstr(lua: &Lua, s: &str) -> mlua::Result<Value> {
    Ok(Value::String(lua.create_string(s)?))
}

pub(crate) fn argv(args: &MultiValue, i: usize) -> Value {
    args.get(i).cloned().unwrap_or(Value::Nil)
}

/// A string argument as Studio's bridge reads one (numbers convert).
pub(crate) fn arg_string(args: &MultiValue, i: usize) -> mlua::Result<String> {
    match argv(args, i) {
        Value::String(s) => Ok(s.to_string_lossy()),
        Value::Integer(n) => Ok(n.to_string()),
        Value::Number(n) => Ok(if n.fract() == 0.0 && n.abs() < 1e15 { format!("{}", n as i64) } else { n.to_string() }),
        Value::Nil => Err(rt_err(format!("Argument {i} missing or nil"))),
        v => Err(rt_err(format!("Unable to cast {} to string", lua_type_name(&v)))),
    }
}

/// An optional string argument (its default when nil).
pub(crate) fn arg_string_or(args: &MultiValue, i: usize, dflt: &str) -> mlua::Result<String> {
    match argv(args, i) {
        Value::Nil => Ok(dflt.to_string()),
        _ => arg_string(args, i),
    }
}

pub(crate) fn arg_bool(args: &MultiValue, i: usize) -> mlua::Result<bool> {
    match argv(args, i) {
        Value::Boolean(b) => Ok(b),
        Value::Nil => Err(rt_err(format!("Argument {i} missing or nil"))),
        // Studio's bridge reads any other value as true (Lua truthiness).
        _ => Ok(true),
    }
}

pub(crate) fn arg_bool_or(args: &MultiValue, i: usize, dflt: bool) -> bool {
    match argv(args, i) {
        Value::Nil => dflt,
        Value::Boolean(b) => b,
        _ => true,
    }
}

/// An `Instances` argument: an array of Instances (measured in Studio:
/// `Unable to cast value to Object` for a non-Instance entry, `Expected
/// table argument, got userdata` for an Instance in place of the table).
pub(crate) fn arg_instances(args: &MultiValue, i: usize) -> mlua::Result<Vec<Ref>> {
    match argv(args, i) {
        Value::Table(t) => {
            let mut out = Vec::new();
            for v in t.sequence_values::<Value>() {
                match opt_inst(&v?) {
                    Some(r) => {
                        if !out.contains(&r) {
                            out.push(r)
                        }
                    }
                    None => return Err(rt_err("Unable to cast value to Object")),
                }
            }
            Ok(out)
        }
        Value::Nil => Err(rt_err(format!("Argument {i} missing or nil"))),
        v => Err(rt_err(format!("Expected table argument, got {}", v.type_name()))),
    }
}

/// Studio checks a call's required arguments from the last one back: a
/// call with none names the last required parameter (measured in Studio:
/// CreatePluginAction() → `Argument 3 missing or nil`).
pub(crate) fn require_args(args: &MultiValue, n: usize) -> mlua::Result<()> {
    for i in (1..=n).rev() {
        if matches!(argv(args, i), Value::Nil) {
            return Err(rt_err(format!("Argument {i} missing or nil")));
        }
    }
    Ok(())
}

fn lua_type_name(v: &Value) -> &'static str {
    match v {
        Value::Boolean(_) => "bool",
        Value::Table(_) => "table",
        Value::Function(_) => "function",
        Value::UserData(_) => "userdata",
        Value::Thread(_) => "thread",
        _ => v.type_name(),
    }
}

pub(crate) fn enum_value(lua: &Lua, en: &str, name: &str) -> mlua::Result<Value> {
    match rustudio_engine::ltypes::LEnumItem::from_name(en, name) {
        Some(item) => rustudio_engine::ltypes::enum_value(lua, item),
        None => Ok(Value::Nil),
    }
}

pub(crate) fn list_value(lua: &Lua, refs: &[Ref]) -> mlua::Result<Value> {
    let t = lua.create_table_with_capacity(refs.len(), 0)?;
    for (i, r) in refs.iter().enumerate() {
        t.raw_set(i + 1, inst_value(lua, *r)?)?;
    }
    Ok(Value::Table(t))
}

/// The plugin a Plugin-class `this` is (its root), checked.
pub(crate) fn plugin_root(lua: &Lua, this: Ref) -> mlua::Result<Ref> {
    match vm::vm(lua) {
        Some(v) if v.by_root(this).is_some() => Ok(this),
        _ => Err(rt_err("This Plugin is not loaded")),
    }
}

fn studio_dm_type(lua: &Lua) -> mlua::Result<Value> {
    let name = {
        let w = world(lua)?;
        match (w.mode, w.side) {
            (RunMode::Edit, _) => "Edit",
            (_, Side::Client) => "PlayClient",
            (_, Side::Server) => "PlayServer",
        }
    };
    enum_value(lua, "StudioDataModelType", name)
}

fn core_prop_get(lua: &Lua, r: Ref, class: &str, key: &str) -> mlua::Result<Option<Value>> {
    match (class, key) {
        ("Plugin", "HostDataModelType") => Ok(Some(studio_dm_type(lua)?)),
        ("Plugin", "HostDataModelTypeIsCurrent") => Ok(Some(Value::Boolean(true))),
        ("Plugin", "IsDebuggable") => Ok(Some(Value::Boolean(false))),
        ("Plugin", "UsesAssetInsertionDrag") => Ok(Some(Value::Boolean(false))),
        ("Plugin", "MultipleDocumentInterfaceInstance") => {
            let child = {
                let w = world(lua)?;
                let d = w.d();
                d.dom.get_by_ref(r).and_then(|i| {
                    i.children()
                        .iter()
                        .copied()
                        .find(|c| d.class_of(*c).as_deref() == Some("MultipleDocumentInterfaceInstance"))
                })
            };
            Ok(Some(match child {
                Some(c) => inst_value(lua, c)?,
                None => Value::Nil,
            }))
        }
        ("MultipleDocumentInterfaceInstance", "FocusedDataModelSession") => {
            let child = {
                let w = world(lua)?;
                let d = w.d();
                d.dom.get_by_ref(r).and_then(|i| {
                    i.children()
                        .iter()
                        .copied()
                        .find(|c| d.class_of(*c).as_deref() == Some("DataModelSession"))
                })
            };
            Ok(Some(match child {
                Some(c) => inst_value(lua, c)?,
                None => Value::Nil,
            }))
        }
        ("DataModelSession", "CurrentDataModelType") => Ok(Some(studio_dm_type(lua)?)),
        ("DataModelSession", "SessionId") => Ok(Some(Value::Integer(1))),
        ("Selection", "ActiveInstance") => {
            let active = {
                let w = world(lua)?;
                let d = w.d();
                let root = d.dom.root_ref();
                let override_ref = lua
                    .app_data_ref::<SelectionExtras>()
                    .and_then(|s| s.active)
                    .filter(|r| d.is_ancestor(root, *r) && d.selection.contains(r));
                override_ref.or_else(|| {
                    d.selection.iter().rev().copied().find(|r| d.is_ancestor(root, *r))
                })
            };
            Ok(Some(match active {
                Some(a) => inst_value(lua, a)?,
                None => Value::Nil,
            }))
        }
        ("Selection", "ShowActiveInstanceHighlight") => {
            let on = lua.app_data_ref::<SelectionExtras>().is_some_and(|s| s.show_active_highlight);
            Ok(Some(Value::Boolean(on)))
        }
        ("Selection", "SelectionBoxThickness" | "SelectionLineThickness") => Ok(Some(Value::Number(0.02))),
        ("Selection", "RenderMode") => Ok(Some(enum_value(lua, "SelectionRenderMode", "BoundingBoxes")?)),
        _ => Ok(None),
    }
}

#[derive(Default)]
struct SelectionExtras {
    active: Option<Ref>,
    show_active_highlight: bool,
}

fn core_newindex(lua: &Lua, r: Ref, class: &str, key: &str, value: &Value) -> mlua::Result<bool> {
    match (class, key) {
        ("Selection", "ActiveInstance") => {
            let target = opt_inst(value);
            if lua.app_data_ref::<SelectionExtras>().is_none() {
                lua.set_app_data(SelectionExtras::default());
            }
            if let Some(mut s) = lua.app_data_mut::<SelectionExtras>() {
                s.active = target;
            }
            world(lua)?.fire(r, "Changed", vec![lstr(lua, "ActiveInstance")?]);
            Ok(true)
        }
        ("Selection", "ShowActiveInstanceHighlight") => {
            let on = match value {
                Value::Boolean(b) => *b,
                Value::Nil => false,
                _ => true,
            };
            if lua.app_data_ref::<SelectionExtras>().is_none() {
                lua.set_app_data(SelectionExtras::default());
            }
            if let Some(mut s) = lua.app_data_mut::<SelectionExtras>() {
                s.show_active_highlight = on;
            }
            Ok(true)
        }
        _ => Ok(false),
    }
}

// ------------------------------------------------------------ Plugin core

fn core_call(lua: &Lua, class: &str, name: &str, this: Ref, args: &MultiValue) -> Option<mlua::Result<MultiValue>> {
    let r = match (class, name) {
        ("Plugin", "GetSetting") => (|| {
            let key = arg_string(args, 1)?.replace('.', "_");
            let sk = settings_key(lua, this)?;
            match crate::plugin_host::settings::get(&sk, &key) {
                Some(j) => ret1(crate::plugin_host::settings::from_json(lua, &j)?),
                None => ret1(Value::Nil),
            }
        })(),
        ("Plugin", "SetSetting") => (|| {
            let key = arg_string(args, 1)?.replace('.', "_");
            let sk = settings_key(lua, this)?;
            let v = crate::plugin_host::settings::to_json(lua, &argv(args, 2), 0);
            if let Err(e) = crate::plugin_host::settings::set(&sk, &key, v) {
                world(lua)?.output.push((Level::Warn, format!("Plugin settings could not be saved: {e}")));
            }
            ret0()
        })(),
        ("Plugin", "GetStudioUserId") => ret1(Value::Number(crate::plugin_host::settings::user_id() as f64)),
        ("Plugin", "IsActivated") => (|| {
            let v = vm::vm(lua).and_then(|v| v.by_root(this).map(|p| p.activated)).unwrap_or(false);
            ret1(Value::Boolean(v))
        })(),
        ("Plugin", "IsActivatedWithExclusiveMouse") => (|| {
            let v = vm::vm(lua)
                .and_then(|v| v.by_root(this).map(|p| p.activated && p.exclusive_mouse))
                .unwrap_or(false);
            ret1(Value::Boolean(v))
        })(),
        ("Plugin", "Activate") => (|| {
            let exclusive = arg_bool(args, 1)?;
            activate(lua, this, exclusive)?;
            ret0()
        })(),
        ("Plugin", "Deactivate") => (|| {
            deactivate(lua, this)?;
            ret0()
        })(),
        ("Plugin", "GetMouse") => (|| {
            let r = mouse::get_mouse(lua, this)?;
            ret1(inst_value(lua, r)?)
        })(),
        // Studio's toolbar setting (Model > Constraints > Join Surfaces):
        // Always when joining is on, else None.
        ("Plugin", "GetJoinMode") => (|| {
            let join = rustudio_engine::runtime::domains::testing::studio::editor_join_surfaces();
            ret1(enum_value(lua, "JointCreationMode", if join { "All" } else { "None" })?)
        })(),
        ("Selection", "Get") => (|| {
            let sel: Vec<Ref> = {
                let w = world(lua)?;
                let d = w.d();
                // Only what is in the DataModel: a selected instance a script
                // destroyed (or unparented) is gone from Get at once (Studio:
                // 0 right after Destroy).
                let root = d.dom.root_ref();
                d.selection.iter().copied().filter(|r| d.is_ancestor(root, *r)).collect()
            };
            ret1(list_value(lua, &sel)?)
        })(),
        ("Selection", "Set") => (|| {
            let refs = arg_instances(args, 1)?;
            let given = !refs.is_empty();
            set_selection(lua, this, SelFire::Set(given), |_| refs)?;
            ret0()
        })(),
        ("Selection", "Add") => (|| {
            let refs = arg_instances(args, 1)?;
            set_selection(lua, this, SelFire::Changed, |mut cur| {
                for r in refs {
                    if !cur.contains(&r) {
                        cur.push(r);
                    }
                }
                cur
            })?;
            ret0()
        })(),
        ("Selection", "Remove") => (|| {
            let refs = arg_instances(args, 1)?;
            let given = !refs.is_empty();
            set_selection(lua, this, SelFire::Given(given), |cur| cur.into_iter().filter(|r| !refs.contains(r)).collect())?;
            ret0()
        })(),
        ("Selection", "AddFocusCallback") => (|| {
            let conn = lua.create_table()?;
            conn.set("Connected", true)?;
            conn.set("Disconnect", lua.create_function(|_, ()| Ok(()))?)?;
            ret1(Value::Table(conn))
        })(),
        ("Selection", "SetTerrainSelectionHack" | "ClearTerrainSelectionHack") => ret0(),
        ("StudioService", "GetUserId") => ret1(Value::Number(crate::plugin_host::settings::user_id() as f64)),
        ("StudioService", "HasInternalPermission") => ret1(Value::Boolean(false)),
        ("SmoothVoxelsUpgraderService", "Start" | "Cancel") => ret0(),
        ("HeightmapImporterService", _) => return builtin::heightmap_call(lua, this, name, args),
        ("CalloutService", "DefineCallout" | "AttachCallout" | "DetachCalloutsByDefinitionId") => ret0(),
        ("ToastNotificationService", "ShowNotification" | "HideNotification") => ret0(),
        (
            "StudioWidgetsService",
            "ApplyHighlight" | "ApplySpotlight" | "HideSpotlight" | "ApplyShadows" | "ApplyFillInBox",
        ) => ret0(),
        ("StudioWidgetsService", "GetWidgetFromLabel" | "GetWidgetFromPluginGui") => ret1(Value::Nil),
        ("StylingService", "GetAppliedStyles" | "GetStyleSheetDerivesChain") => (|| {
            ret1(Value::Table(lua.create_table()?))
        })(),
        ("StylingService", "GetStyleInfo" | "GetStyleSheetInfo") => (|| {
            ret1(Value::Table(lua.create_table()?))
        })(),
        ("StylingService", "UpdateUnitTestOnly") => ret0(),
        ("CoreScriptSyncService", "GetScriptFilePath") => (|| ret1(lstr(lua, "")?))(),
        _ => return None,
    };
    Some(r)
}

fn settings_key(lua: &Lua, this: Ref) -> mlua::Result<String> {
    vm::vm(lua)
        .and_then(|v| v.by_root(this).map(|p| p.settings_key.clone()))
        .ok_or_else(|| rt_err("This Plugin is not loaded"))
}

/// When a Selection call fires SelectionChanged (measured, checks/plugins/
/// services/captures/misc.txt `sel.*`): Set fires unless both the
/// selection and the list are empty (Set to the same list fires; Set of a
/// parentless instance from nothing fires though nothing is selected); Add
/// only when something was added; Remove whenever the list isn't empty
/// (removing what isn't selected fires).
enum SelFire {
    Set(bool),
    Changed,
    Given(bool),
}

/// Selection:Set / Add / Remove: the editor's own selection changes (the
/// Explorer and viewport show it at once) and SelectionChanged fires.
fn set_selection(lua: &Lua, sel: Ref, rule: SelFire, f: impl FnOnce(Vec<Ref>) -> Vec<Ref>) -> mlua::Result<()> {
    let mut w = world(lua)?;
    let cur: Vec<Ref> = {
        let d = w.d();
        let root = d.dom.root_ref();
        d.selection.iter().copied().filter(|r| d.is_ancestor(root, *r)).collect()
    };
    let mut next = f(cur.clone());
    {
        let d = w.d();
        // Instances outside the DataModel can't be selected.
        let root = d.dom.root_ref();
        next.retain(|r| *r != root && d.is_ancestor(root, *r));
    }
    let fires = match rule {
        SelFire::Set(given) => given || !cur.is_empty(),
        SelFire::Changed => next != cur,
        SelFire::Given(given) => given,
    };
    if next != cur {
        w.dm().selection = next;
    }
    if fires {
        // One SelectionChanged per call that changed it, after the caller
        // yields (measured in Studio: 0 right after Set, all of them after a
        // task.wait). The edit VM's host fires them at the end of the frame
        // (PluginHost::tick); a Play copy has no host: at once.
        if w.mode == RunMode::Edit {
            drop(w);
            if let Some(mut v) = vm::vm(lua) {
                v.selection_fires += 1;
            }
        } else {
            w.fire(sel, "SelectionChanged", vec![]);
            w.fire(sel, "SelectionChangedThisFrame", vec![]);
        }
    }
    Ok(())
}

/// plugin:Activate(exclusiveMouse): this plugin becomes the active one;
/// the one active before gets Deactivation (one at a time, as in Studio).
/// Activate again while active changes nothing (measured in Studio:
/// Activate(false) after Activate(true) left the exclusive mouse on).
pub(crate) fn activate(lua: &Lua, this: Ref, exclusive: bool) -> mlua::Result<()> {
    let prev = {
        let mut v = vm::vm(lua).ok_or_else(|| rt_err("This Plugin is not loaded"))?;
        let id = v.by_root(this).map(|p| p.id).ok_or_else(|| rt_err("This Plugin is not loaded"))?;
        if v.active == Some(id) {
            return Ok(());
        }
        let prev = v.active.and_then(|a| v.by_id(a).map(|p| p.root));
        for p in v.plugins.iter_mut() {
            if p.root == this {
                p.activated = true;
                p.exclusive_mouse = exclusive;
            } else {
                p.activated = false;
                p.exclusive_mouse = false;
            }
        }
        v.active = Some(id);
        v.activation_seq += 1;
        prev
    };
    if let Some(p) = prev {
        world(lua)?.fire(p, "Deactivation", vec![]);
    }
    Ok(())
}

/// plugin:Deactivate(): Deactivation fires if it was active.
pub(crate) fn deactivate(lua: &Lua, this: Ref) -> mlua::Result<()> {
    let was = {
        let mut v = vm::vm(lua).ok_or_else(|| rt_err("This Plugin is not loaded"))?;
        let Some(p) = v.by_root_mut(this) else { return Err(rt_err("This Plugin is not loaded")) };
        let was = p.activated;
        p.activated = false;
        p.exclusive_mouse = false;
        let id = p.id;
        if v.active == Some(id) {
            v.active = None;
            v.activation_seq += 1;
        }
        was
    };
    if was {
        world(lua)?.fire(this, "Deactivation", vec![]);
    }
    Ok(())
}
