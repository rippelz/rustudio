//! Built-in Studio plugin APIs (RobloxScriptSecurity):
//! - `Plugin:GetUri()`, `Plugin:GetPreinitPayload()`, `Plugin:SetReady()`,
//!   `Plugin:FinishFullLoading()`
//! - `Plugin:Invoke()`, `Plugin:OnInvoke()`, `Plugin:OnInvokeSuspendOverride()`
//! - `Plugin:GetItem()`, `Plugin:SetItem()`, `Plugin:OnSetItem()`
//! - `Plugin:GetPluginComponent(name)`: Studio's component bridge used by
//!   `PluginLoaderBuilder`, `DeveloperFramework`, `StudioFoundation` and
//!   built-in plugins (`Widgets`, `Actions`, `Panels`, `MouseTracker`,
//!   `PlaceManager`, `PlaceSessionContextBridge`, …).

use mlua::{Lua, MultiValue, Table, Value};
use rbx_dom_weak::types::Ref;

use super::*;

const BUILTIN_LUA: &str = r#"
local make_qwidget, get_uri = ...
local M = {}

local function uri_key(u)
    if type(u) == "string" then return u end
    if type(u) ~= "table" then return tostring(u) end
    return table.concat({
        tostring(u.DataModel or ""),
        tostring(u.PluginType or ""),
        tostring(u.PluginId or ""),
        tostring(u.Category or ""),
        tostring(u.ItemId or ""),
    }, "/")
end

local function make_signal()
    local be = Instance.new("BindableEvent")
    return be, be.Event
end

-- Per-plugin Invoke / Item / Component tables (weak keys on plugin userdata).
local state = setmetatable({}, { __mode = "k" })
local function pstate(plugin)
    local s = state[plugin]
    if not s then
        s = {
            invokes = {},
            items = {},
            item_cbs = {},
            components = {},
            ready = false,
        }
        state[plugin] = s
    end
    return s
end

-- Shared across plugins in the same DataModel (Widgets, Actions, Panels, Settings, Tools).
local reg_widgets = {}  -- uri_key -> entry
local reg_actions = {}  -- uri_key -> { desc = ..., be = BindableEvent, ev = ..., changed_be = ..., changed_ev = ... }
local reg_panels = {}   -- uri_key -> PluginGui
local reg_settings = {} -- uri_key -> { desc = ..., value = ..., be = ..., ev = ... }
local reg_tools = {}    -- uri_key -> { desc = ..., be = ..., ev = ... }

local function make_disconnect(fn)
    local done = false
    return {
        Connected = true,
        Disconnect = function(self)
            if done then return end
            done = true
            self.Connected = false
            fn()
        end,
        Destroy = function(self)
            self:Disconnect()
        end,
    }
end

function M.OnInvoke(plugin, key, cb)
    local s = pstate(plugin)
    s.invokes[key] = cb
    return make_disconnect(function()
        if s.invokes[key] == cb then s.invokes[key] = nil end
    end)
end

function M.Invoke(plugin, key, ...)
    local s = pstate(plugin)
    local cb = s.invokes[key]
    if type(cb) == "function" then
        return cb(...)
    end
    return nil
end

function M.GetItem(plugin, key, default_val)
    local s = pstate(plugin)
    local v = s.items[key]
    if v == nil then return default_val end
    return v
end

function M.SetItem(plugin, key, val)
    local s = pstate(plugin)
    s.items[key] = val
    local cbs = s.item_cbs[key]
    if cbs then
        for cb in pairs(cbs) do
            task.spawn(cb, val)
        end
    end
end

function M.OnSetItem(plugin, key, cb)
    local s = pstate(plugin)
    s.item_cbs[key] = s.item_cbs[key] or {}
    s.item_cbs[key][cb] = true
    return make_disconnect(function()
        if s.item_cbs[key] then s.item_cbs[key][cb] = nil end
    end)
end

local function make_component(plugin, name)
    if name == "FTFUtil" then
        -- Real Studio only exposes FTFUtil inside automated Feature Test Framework runs.
        error("PluginComponent 'FTFUtil' is not available", 0)
    end
    if name == "Widgets" then
        return {
            RegisterAsync = function(_, list)
                if type(list) == "table" then
                    if list.Uri ~= nil then
                        reg_widgets[uri_key(list.Uri)] = list
                    else
                        for _, entry in ipairs(list) do
                            if type(entry) == "table" and entry.Uri ~= nil then
                                reg_widgets[uri_key(entry.Uri)] = entry
                            end
                        end
                    end
                end
            end,
            DeregisterAsync = function(_, list)
                if type(list) == "table" then
                    if list.Category ~= nil or list.ItemId ~= nil then
                        reg_widgets[uri_key(list)] = nil
                    else
                        for _, u in ipairs(list) do
                            reg_widgets[uri_key(u)] = nil
                        end
                    end
                end
            end,
            GetAsync = function(_, uris)
                local out = {}
                if type(uris) == "table" then
                    for _, u in ipairs(uris) do
                        local e = reg_widgets[uri_key(u)]
                        if e then table.insert(out, e) end
                    end
                end
                return out
            end,
            GetMatchedWidgetsAsync = function(_, uri)
                local e = reg_widgets[uri_key(uri)]
                if e then
                    return { e }
                end
                if type(uri) == "table" and uri.PluginId == "Ribbon" then
                    return {
                        {
                            Uri = uri,
                            Exists = true,
                            Visible = true,
                            Position = Vector2.new(0, 0),
                            Size = Vector2.new(48, 50),
                        },
                    }
                end
                return {}
            end,
            FocusAsync = function() end,
        }
    end
    if name == "Actions" then
        local function default_action_desc(uri, desc)
            local d = {
                Uri = uri,
                Enabled = true,
                Visible = true,
                Checked = false,
                Exists = true,
                Text = "",
                Tooltip = "",
                Shortcuts = {},
            }
            if type(desc) == "table" then
                for k, v in pairs(desc) do
                    d[k] = v
                end
            end
            return d
        end
        local function ensure_action(uri, desc)
            local k = uri_key(uri)
            local a = reg_actions[k]
            if not a then
                local be, ev = make_signal()
                local cbe, cev = make_signal()
                a = {
                    desc = default_action_desc(uri, desc),
                    be = be,
                    ev = ev,
                    changed_be = cbe,
                    changed_ev = cev,
                }
                reg_actions[k] = a
            elseif type(desc) == "table" then
                for dk, dv in pairs(desc) do
                    a.desc[dk] = dv
                end
            end
            return a
        end
        return {
            -- Studio's Actions:CreateAsync returns a 1-element array { activatedSignal }
            -- (PluginLoaderBuilder / AssetImporter index [1]:Connect(...)).
            CreateAsync = function(_, desc, _add_to_toolbar)
                local uri = type(desc) == "table" and desc.Uri or desc
                local a = ensure_action(uri, desc)
                return { a.ev }
            end,
            BindToActivatedAsync = function(_, uri)
                local a = ensure_action(uri, nil)
                return a.ev
            end,
            MultiBindToActivatedAsync = function(_, uris)
                local out = {}
                if type(uris) == "table" then
                    for _, u in ipairs(uris) do
                        table.insert(out, ensure_action(u, nil).ev)
                    end
                end
                return out
            end,
            BindToChangedAsync = function(_, uri)
                local a = ensure_action(uri, nil)
                return a.changed_ev
            end,
            MultiBindToChangedAsync = function(_, uris)
                local out = {}
                if type(uris) == "table" then
                    for _, u in ipairs(uris) do
                        table.insert(out, ensure_action(u, nil).changed_ev)
                    end
                end
                return out
            end,
            ActivateAsync = function(_, uri)
                local a = ensure_action(uri, nil)
                a.be:Fire()
            end,
            UpdateAsync = function(_, desc)
                local function apply_one(d)
                    local uri = type(d) == "table" and d.Uri or d
                    local a = ensure_action(uri, d)
                    a.changed_be:Fire(a.desc)
                end
                if type(desc) == "table" and desc.Uri == nil and type(desc[1]) == "table" then
                    for _, d in ipairs(desc) do
                        apply_one(d)
                    end
                else
                    apply_one(desc)
                end
            end,
            SetEnabledAsync = function(_, uri, en)
                local a = ensure_action(uri, nil)
                a.desc.Enabled = en == true
                a.changed_be:Fire(a.desc)
            end,
            SetCheckedAsync = function(_, uri, ch)
                local a = ensure_action(uri, nil)
                a.desc.Checked = ch == true
                a.changed_be:Fire(a.desc)
            end,
            SetVisibleAsync = function(_, uri, vis)
                local a = ensure_action(uri, nil)
                a.desc.Visible = vis == true
                a.changed_be:Fire(a.desc)
            end,
            GetAsync = function(_, uris)
                local out = {}
                if type(uris) == "table" then
                    for _, u in ipairs(uris) do
                        local a = ensure_action(u, nil)
                        table.insert(out, a.desc)
                    end
                end
                return out
            end,
            ListAsync = function(_)
                local out = {}
                for _, a in pairs(reg_actions) do
                    table.insert(out, a.desc)
                end
                return out
            end,
            IsContextManagedAsync = function(_, _uri)
                return false
            end,
            RegisterAsync = function() end,
        }
    end
    if name == "Settings" then
        local function ensure_setting(uri, desc)
            local k = uri_key(uri)
            local s = reg_settings[k]
            if not s then
                local be, ev = make_signal()
                s = { desc = type(desc) == "table" and desc or { Uri = uri }, value = nil, be = be, ev = ev }
                if type(desc) == "table" and desc.Value ~= nil then
                    s.value = desc.Value
                end
                reg_settings[k] = s
            elseif type(desc) == "table" then
                for dk, dv in pairs(desc) do s.desc[dk] = dv end
                if desc.Value ~= nil then s.value = desc.Value end
            end
            return s
        end
        return {
            CreateAsync = function(_, desc)
                local uri = type(desc) == "table" and desc.Uri or desc
                local s = ensure_setting(uri, desc)
                return s.ev
            end,
            BindAsync = function(_, uri)
                return ensure_setting(uri, nil).ev
            end,
            BindToChangedAsync = function(_, uri)
                return ensure_setting(uri, nil).ev
            end,
            MultiBindAsync = function(_, uris)
                local out = {}
                if type(uris) == "table" then
                    for _, u in ipairs(uris) do
                        table.insert(out, ensure_setting(u, nil).ev)
                    end
                end
                return out
            end,
            GetAsync = function(_, uri)
                return ensure_setting(uri, nil).value
            end,
            SetBoolAsync = function(_, uri, v)
                local s = ensure_setting(uri, nil)
                s.value = v == true
                s.be:Fire(s.value)
            end,
            SetNumberAsync = function(_, uri, v)
                local s = ensure_setting(uri, nil)
                s.value = tonumber(v) or 0
                s.be:Fire(s.value)
            end,
            SetStringAsync = function(_, uri, v)
                local s = ensure_setting(uri, nil)
                s.value = tostring(v or "")
                s.be:Fire(s.value)
            end,
            UpdateAsync = function(_, desc)
                local uri = type(desc) == "table" and desc.Uri or desc
                local s = ensure_setting(uri, desc)
                s.be:Fire(s.value)
            end,
            DeleteAsync = function(_, uri)
                reg_settings[uri_key(uri)] = nil
            end,
            ListAsync = function(_)
                local out = {}
                for _, s in pairs(reg_settings) do table.insert(out, s.desc) end
                return out
            end,
        }
    end
    if name == "Tools" then
        local function ensure_tool(uri, desc)
            local k = uri_key(uri)
            local t = reg_tools[k]
            if not t then
                local be, ev = make_signal()
                t = { desc = type(desc) == "table" and desc or { Uri = uri }, be = be, ev = ev }
                reg_tools[k] = t
            elseif type(desc) == "table" then
                for dk, dv in pairs(desc) do t.desc[dk] = dv end
            end
            return t
        end
        return {
            CreateAsync = function(_, desc)
                local uri = type(desc) == "table" and desc.Uri or desc
                return ensure_tool(uri, desc).ev
            end,
            BindAsync = function(_, uri)
                return ensure_tool(uri, nil).ev
            end,
            GetAsync = function(_, uris)
                local out = {}
                if type(uris) == "table" then
                    for _, u in ipairs(uris) do table.insert(out, ensure_tool(u, nil).desc) end
                end
                return out
            end,
            ListAsync = function(_)
                local out = {}
                for _, t in pairs(reg_tools) do table.insert(out, t.desc) end
                return out
            end,
        }
    end
    if name == "Panels" then
        local next_id = 0
        return {
            CreateAsync = function(_, uri, config)
                local k = uri_key(uri)
                local existing = reg_panels[k]
                if existing and existing.Parent ~= nil then
                    return existing
                end
                next_id += 1
                local id = (type(uri) == "table" and uri.ItemId and uri.ItemId ~= "" and uri.ItemId)
                    or ("Panel_" .. tostring(next_id))
                local gui = make_qwidget(plugin, k .. "_" .. id)
                if type(config) == "table" then
                    if type(config.Title) == "string" then gui.Title = config.Title end
                    if config.InitialEnabled ~= nil then gui.Enabled = config.InitialEnabled == true end
                end
                reg_panels[k] = gui
                return gui
            end,
            SetAttachmentAsync = function() end,
            SetSizeAsync = function() end,
            ShowIfAttachedAsync = function(_, uri)
                local g = reg_panels[uri_key(uri)]
                if g then g.Enabled = true end
            end,
            HideAsync = function(_, uri)
                local g = reg_panels[uri_key(uri)]
                if g then g.Enabled = false end
            end,
            FocusAsync = function() end,
            DestroyAsync = function(_, uri)
                local k = uri_key(uri)
                local g = reg_panels[k]
                reg_panels[k] = nil
                if g then pcall(function() g:Destroy() end) end
            end,
            GetPositionAsync = function() return Vector2.new(0, 0) end,
            GetSizeAsync = function(_, uri)
                local g = reg_panels[uri_key(uri)]
                return g and g.AbsoluteSize or Vector2.new(0, 0)
            end,
            ReportInteractionAsync = function() end,
        }
    end
    if name == "MouseTracker" then
        return {
            RegisterMouseTrackingRelativeToWidgetAsync = function(_, _uri)
                local f = Instance.new("Frame")
                f:SetAttribute("RBX_MousePosition", Vector2.new(0, 0))
                return f
            end,
            UnregisterMouseTrackingAsync = function() end,
        }
    end
    if name == "PlaceManager" then
        local _, shown = make_signal()
        local _, closed = make_signal()
        return {
            PlaceDocPanelShown = shown,
            PlaceDocPanelClosed = closed,
        }
    end
    if name == "PlaceSessionContextBridge" then
        return {
            GetPlaceIsPersistedToCloudAsync = function() return false end,
            GetPlaceFilePathAsync = function() return "" end,
            GetPlaceIdAsync = function() return 0 end,
            GetUniverseIdAsync = function() return 0 end,
            SavePlaceAsync = function() return false end,
            SaveToFileAsync = function() return false end,
            ReplacePlaceAsync = function() end,
        }
    end
    if name == "WebBrowserManager" then
        return {
            IsAvailableAsync = function() return false end,
            ShowWebBrowserWidgetAsync = function() end,
            HideWebBrowserWidgetAsync = function() end,
            ReloadWebBrowserWidgetAsync = function() end,
            ClearWebBrowserWidgetFocusAsync = function() end,
        }
    end
    -- Fallback component stub for other optional Studio plugin components.
    return setmetatable({}, {
        __index = function(_, k)
            if type(k) == "string" and k:sub(-5) == "Async" then
                return function() return nil end
            end
            return function() return nil end
        end,
    })
end

function M.GetPluginComponent(plugin, name)
    local s = pstate(plugin)
    if s.components[name] == nil then
        s.components[name] = make_component(plugin, name)
    end
    return s.components[name]
end

return M
"#;

pub fn install(lua: &Lua) -> mlua::Result<()> {
    let make_qwidget = lua.create_function(|lua, (plugin, id): (Value, String)| {
        let this = opt_inst(&plugin).ok_or_else(|| rt_err("Plugin expected"))?;
        let r = super::widgets::make_panel_widget(lua, this, id)?;
        inst_value(lua, r)
    })?;
    let get_uri = lua.create_function(|lua, plugin: Value| {
        let this = opt_inst(&plugin).ok_or_else(|| rt_err("Plugin expected"))?;
        plugin_uri(lua, this)
    })?;
    let m: Table = lua
        .load(BUILTIN_LUA)
        .set_name("=rustudio.plugin_builtin")
        .call((make_qwidget, get_uri))?;
    lua.set_named_registry_value("rs_plugin_builtin", m)
}

fn plugin_uri(lua: &Lua, this: Ref) -> mlua::Result<Value> {
    let (dm_name, plugin_type, plugin_id) = {
        let dm = {
            let w = world(lua)?;
            match (w.mode, w.side) {
                (RunMode::Edit, _) => "Edit",
                (_, Side::Client) => "PlayClient",
                (_, Side::Server) => "PlayServer",
            }
        };
        let (ptype, pid) = vm::vm(lua)
            .and_then(|v| {
                v.by_root(this).map(|p| {
                    if p.is_builtin {
                        let stem = p
                            .name
                            .strip_prefix("builtin_")
                            .unwrap_or(&p.name)
                            .strip_suffix(".rbxm")
                            .unwrap_or(&p.name);
                        ("BuiltIn".to_string(), stem.to_string())
                    } else if p.name.starts_with("cloud_") {
                        ("Cloud".to_string(), p.name.clone())
                    } else {
                        ("Local".to_string(), p.name.clone())
                    }
                })
            })
            .unwrap_or_else(|| ("Local".to_string(), String::new()));
        (dm, ptype, pid)
    };
    let t = lua.create_table()?;
    t.set("DataModel", dm_name)?;
    t.set("PluginType", plugin_type)?;
    t.set("PluginId", plugin_id)?;
    t.set("Category", "Plugins")?;
    t.set("ItemId", "")?;
    Ok(Value::Table(t))
}

pub fn prop_get(lua: &Lua, _r: Ref, class: &str, key: &str) -> mlua::Result<Option<Value>> {
    match (class, key) {
        ("StudioPublishService", "PublishLocked") => Ok(Some(Value::Boolean(false))),
        ("StudioService", "InstalledPluginData" | "Secrets") => Ok(Some(lstr(lua, "")?)),
        ("StudioService", "ShowWeldDetails") => Ok(Some(Value::Boolean(false))),
        _ => Ok(None),
    }
}

fn plugin_policy(lua: &Lua, plugin_name: &str) -> mlua::Result<Value> {
    let t = lua.create_table()?;
    match plugin_name {
        "Toolbox" => {
            t.set("ShowRobloxCreatedAssets", false)?;
            t.set("DisableRatings", false)?;
            t.set("DisableMarketplaceAndRecents", false)?;
            t.set("HideNonRelevanceSorts", false)?;
            t.set("HideVerifiedCreatorBadges", false)?;
            t.set("DisableAutocomplete", false)?;
            t.set("MaxAudioLength", 420)?;
            t.set("HomeViewEnabledAssetTypes", "Model;Audio;MeshPart;Decal;Plugin")?;
            t.set("MarketplaceDisabledCategories", "")?;
            t.set("MarketplaceShouldUsePluginCreatorWhitelist", false)?;
            t.set("Enabled", true)?;
            t.set("SocialMediaReferencesAllowed", true)?;
            t.set("RenameFriendsToConnections", false)?;
        }
        "PlayerEmulator" => {
            t.set("SocialMediaReferencesAllowed", true)?;
        }
        "GameSettings" | "PublishPlaceAs" => {
            t.set("OptInLocations", lua.create_table()?)?;
            t.set("SocialMediaReferencesAllowed", true)?;
        }
        _ => {}
    }
    Ok(Value::Table(t))
}

pub fn call(lua: &Lua, class: &str, name: &str, this: Ref, args: &MultiValue) -> Option<mlua::Result<MultiValue>> {
    if class == "PluginPolicyService" && name == "GetPluginPolicy" {
        let plugin_name = match arg_string(args, 1) {
            Ok(s) => s,
            Err(e) => return Some(Err(e)),
        };
        return Some(plugin_policy(lua, &plugin_name).and_then(ret1));
    }
    if class == "PackageUIService" {
        let r = match name {
            "GetPackageInfo" => lua.create_table().and_then(|t| ret1(Value::Table(t))),
            "ConvertToPackageAsync" | "SetPackageVersion" => ret1(Value::Nil),
            "ConvertToMockPackage"
            | "ConvertToPackageClosedCallback"
            | "ConvertToPackageUpload"
            | "PublishPackage" => ret0(),
            _ => return None,
        };
        return Some(r);
    }
    if class == "StudioPublishService" {
        let r = match name {
            "RegisterPublishHold" => (|| {
                let conn = lua.create_table()?;
                conn.set("Connected", true)?;
                conn.set(
                    "Disconnect",
                    lua.create_function(|_, c: Table| {
                        c.set("Connected", false)?;
                        Ok(())
                    })?,
                )?;
                ret1(Value::Table(conn))
            })(),
            "ShowSaveOrPublishPlaceToRoblox" => (|| {
                let show_select = argv(args, 1);
                let is_publish = argv(args, 2);
                let close_mode = argv(args, 3);
                world(lua)?.fire(
                    this,
                    "OnSaveOrPublishPlaceToRoblox",
                    vec![show_select, is_publish, close_mode],
                );
                ret0()
            })(),
            "ClearUploadNames"
            | "CloseAfterPublish"
            | "PublishAs"
            | "PublishThenTurnOnTeamCreate"
            | "RefreshDocumentDisplayName"
            | "SaveOrPublishPlaceToRobloxIsCanceled"
            | "SetTeamCreateOnPublishInfo"
            | "SetUniverseDisplayName"
            | "SetUploadNames" => ret0(),
            _ => return None,
        };
        return Some(r);
    }
    if class == "AssetImportService" {
        let r = match name {
            "GetAllPresets" | "GetPreset" | "GetFilesInDirAsync" | "PickMultipleFilesWithPrompt" => {
                lua.create_table().and_then(|t| ret1(Value::Table(t)))
            }
            "SavePreset" => ret1(Value::Boolean(true)),
            "PickFileWithPromptAsync" | "PickImageFileWithPrompt" | "PickMeshFileWithPrompt" => {
                lstr(lua, "").and_then(ret1)
            }
            "StartSessionWithPath" | "StartSessionWithPathAsync" => ret1(Value::Nil),
            "UploadAssetFromContentAsync"
            | "UploadAssetFromPathAsync"
            | "UploadVersionedAssetFromContentAsync"
            | "UploadVersionedAssetFromPathAsync" => {
                Ok(MultiValue::from_iter([Value::Boolean(false), Value::Nil]))
            }
            "RemovePreset" | "StartSingleFileWatch" | "StopSingleFileWatch" => ret0(),
            _ => return None,
        };
        return Some(r);
    }
    if class == "TeamCreateService" {
        let r = match name {
            "CloseGameIfUserDoesntHavePerms" => ret0(),
            _ => return None,
        };
        return Some(r);
    }
    if class == "CollaboratorsService" {
        #[derive(Default)]
        struct CollabHighlights(bool);
        if lua.app_data_ref::<CollabHighlights>().is_none() {
            lua.set_app_data(CollabHighlights(true));
        }
        let r = match name {
            "GetCollaboratorsList" => (|| {
                let kids: Vec<Ref> = {
                    let w = world(lua)?;
                    let d = w.d();
                    d.dom
                        .get_by_ref(this)
                        .map(|i| {
                            i.children()
                                .iter()
                                .copied()
                                .filter(|c| d.class_of(*c).is_some_and(|cl| cl == "Collaborator"))
                                .collect()
                        })
                        .unwrap_or_default()
                };
                list_value(lua, &kids).and_then(ret1)
            })(),
            "GetSelectionHighlightsEnabled" => {
                let on = lua.app_data_ref::<CollabHighlights>().map(|c| c.0).unwrap_or(true);
                ret1(Value::Boolean(on))
            }
            "ToggleSelectionHighlights" => (|| {
                let cur = lua.app_data_ref::<CollabHighlights>().map(|c| c.0).unwrap_or(true);
                let on = match argv(args, 1) {
                    Value::Boolean(b) => b,
                    _ => !cur,
                };
                lua.set_app_data(CollabHighlights(on));
                world(lua)?.fire(this, "ToggleSelectionHighlightsSignal", vec![Value::Boolean(on)]);
                ret0()
            })(),
            "MultiGetCanCollaborate" | "RequestFlyToCollaborator" | "ToggleTeamCreate" => ret0(),
            _ => return None,
        };
        return Some(r);
    }
    if class == "StudioService" {
        let r = match name {
            "GetStartupAssetId" | "GetStartupPluginId" | "GetBadgeConfigureUrl" | "GetBadgeUploadUrl" | "GetTermsOfUseUrl" => {
                lstr(lua, "").and_then(ret1)
            }
            "GetResourceByCategory" => lua.create_table().and_then(|t| ret1(Value::Table(t))),
            "IsPluginInstalled" => ret1(Value::Boolean(false)),
            "IsPluginUpToDate" => ret1(Value::Boolean(true)),
            "CopyToClipboard"
            | "OpenInBrowser_DONOTUSE"
            | "SetPluginEnabled"
            | "ShowPublishToRoblox"
            | "UninstallPlugin"
            | "UpdatePluginManagement"
            | "TryInstallPlugin"
            | "AnimationIdSelected" => ret0(),
            _ => return None,
        };
        return Some(r);
    }
    if class == "StudioAssetService" {
        let r = match name {
            "SerializeInstances" | "DEPRECATED_SerializeInstances" => lstr(lua, "").and_then(ret1),
            "ShowSaveToRoblox"
            | "ResolveSaveInstanceToRoblox"
            | "PublishPackage"
            | "UpdatePublishedPackage"
            | "ConvertToPackageUpload"
            | "FireOnUGCSubmitCompleted" => ret0(),
            _ => return None,
        };
        return Some(r);
    }
    if class == "BrowserService" {
        let r = match name {
            "OpenBrowserWindow"
            | "CloseBrowserWindow"
            | "OpenNativeOverlay"
            | "SendCommand"
            | "EmitHybridEvent"
            | "ExecuteJavaScript"
            | "ReturnToJavaScript"
            | "CopyAuthCookieFromBrowserToEngine" => ret0(),
            _ => return None,
        };
        return Some(r);
    }
    if class != "Plugin" {
        return None;
    }
    let r = match name {
        "GetUri" => plugin_uri(lua, this).and_then(ret1),
        "GetPreinitPayload" => ret1(Value::Nil),
        "FinishFullLoading" => ret0(),
        "SetReady" => (|| {
            world(lua)?.fire(this, "Ready", vec![]);
            ret0()
        })(),
        "GetPluginComponent" => (|| {
            let comp = arg_string(args, 1)?;
            let m: Table = lua.named_registry_value("rs_plugin_builtin")?;
            let f: mlua::Function = m.get("GetPluginComponent")?;
            let v: Value = f.call((inst_value(lua, this)?, comp))?;
            ret1(v)
        })(),
        "OnInvoke" | "OnInvokeSuspendOverride" => (|| {
            let key = arg_string(args, 1)?;
            let cb = argv(args, 2);
            let m: Table = lua.named_registry_value("rs_plugin_builtin")?;
            let f: mlua::Function = m.get("OnInvoke")?;
            let v: Value = f.call((inst_value(lua, this)?, key, cb))?;
            ret1(v)
        })(),
        "Invoke" => (|| {
            let key = arg_string(args, 1)?;
            let m: Table = lua.named_registry_value("rs_plugin_builtin")?;
            let f: mlua::Function = m.get("Invoke")?;
            let mut call_args = MultiValue::new();
            call_args.push_back(inst_value(lua, this)?);
            call_args.push_back(lstr(lua, &key)?);
            for a in args.iter().skip(2) {
                call_args.push_back(a.clone());
            }
            f.call::<MultiValue>(call_args)
        })(),
        "GetItem" => (|| {
            let key = arg_string(args, 1)?;
            let dflt = argv(args, 2);
            let m: Table = lua.named_registry_value("rs_plugin_builtin")?;
            let f: mlua::Function = m.get("GetItem")?;
            let v: Value = f.call((inst_value(lua, this)?, key, dflt))?;
            ret1(v)
        })(),
        "SetItem" => (|| {
            let key = arg_string(args, 1)?;
            let val = argv(args, 2);
            let m: Table = lua.named_registry_value("rs_plugin_builtin")?;
            let f: mlua::Function = m.get("SetItem")?;
            f.call::<()>((inst_value(lua, this)?, key, val))?;
            ret0()
        })(),
        "OnSetItem" => (|| {
            let key = arg_string(args, 1)?;
            let cb = argv(args, 2);
            let m: Table = lua.named_registry_value("rs_plugin_builtin")?;
            let f: mlua::Function = m.get("OnSetItem")?;
            let v: Value = f.call((inst_value(lua, this)?, key, cb))?;
            ret1(v)
        })(),
        _ => return None,
    };
    Some(r)
}

/// Decodes an image asset referenced by ContentId (`rbxtemp://`, `rbxassetid://`,
/// `rbxasset://`, or file path) into RGBA8 pixels + dimensions.
fn load_heightmap_image(id: &str) -> Result<rustudio_core::texture::Rgba, String> {
    if id.is_empty() {
        return Err("Empty asset id".into());
    }
    let bytes = if let Some(g) = rustudio_core::assets::generated(id) {
        g
    } else if let Ok(b) = rustudio_core::assets::content_blocking(id) {
        b
    } else if let Ok(b) = std::fs::read(id) {
        std::sync::Arc::new(b)
    } else {
        return Err(format!("Could not load image {id}"));
    };
    rustudio_core::texture::decode(&bytes).map_err(|e| format!("Invalid image {id}: {e}"))
}

/// `HeightmapImporterService` methods used by Studio's built-in TerrainEditor:
/// `IsValidHeightmap`, `IsValidColormap`, `GetHeightmapPreviewAsync`,
/// `ImportHeightmap`, `ImportHeightmapWithMaterialSlotsAsync`,
/// `CancelImportHeightmap`, `SetImportHeightmapPaused`.
pub(super) fn heightmap_call(
    lua: &Lua,
    this: Ref,
    name: &str,
    args: &MultiValue,
) -> Option<mlua::Result<MultiValue>> {
    let r = match name {
        "CancelImportHeightmap" | "SetImportHeightmapPaused" => ret0(),
        "IsValidHeightmap" => (|| {
            let id = arg_string(args, 1).unwrap_or_default();
            match load_heightmap_image(&id) {
                Ok(img) => Ok(MultiValue::from_iter([
                    Value::Boolean(true),
                    lstr(lua, "")?,
                    Value::Integer(img.width as i64),
                    Value::Integer(img.height as i64),
                    Value::Integer(4),
                    Value::Integer(1),
                ])),
                Err(e) => Ok(MultiValue::from_iter([
                    Value::Boolean(false),
                    lstr(lua, &e)?,
                    Value::Integer(0),
                    Value::Integer(0),
                    Value::Integer(0),
                    Value::Integer(0),
                ])),
            }
        })(),
        "IsValidColormap" => (|| {
            let id = arg_string(args, 1).unwrap_or_default();
            match load_heightmap_image(&id) {
                Ok(img) => Ok(MultiValue::from_iter([
                    Value::Boolean(true),
                    lstr(lua, "")?,
                    Value::Integer(img.width as i64),
                    Value::Integer(img.height as i64),
                    Value::Integer(4),
                ])),
                Err(e) => Ok(MultiValue::from_iter([
                    Value::Boolean(false),
                    lstr(lua, &e)?,
                    Value::Integer(0),
                    Value::Integer(0),
                    Value::Integer(0),
                ])),
            }
        })(),
        "GetHeightmapPreviewAsync" => (|| {
            let id = arg_string(args, 1).unwrap_or_default();
            let _ = load_heightmap_image(&id).map_err(rt_err)?;
            Ok(MultiValue::from_iter([lstr(lua, &id)?, Value::Boolean(false)]))
        })(),
        "ImportHeightmap" | "ImportHeightmapWithMaterialSlotsAsync" => (|| {
            let rg = opt_ud::<rustudio_engine::ltypes::LRegion3>(&argv(args, 1))
                .ok_or_else(|| rt_err(format!("HeightmapImporterService:{name}: Region3 expected")))?;
            let hm_id = arg_string(args, 2).unwrap_or_default();
            let cm_id = arg_string(args, 3).unwrap_or_default();
            let grass = rustudio_core::terrain::material_index("Grass").unwrap_or(2);
            let default_mat = if name == "ImportHeightmap" {
                opt_ud::<rustudio_engine::ltypes::LEnumItem>(&argv(args, 4))
                    .and_then(|e| rustudio_core::terrain::material_index(&e.name))
                    .unwrap_or(grass)
            } else {
                match argv(args, 4) {
                    Value::Integer(n) => (n as u8).max(1),
                    Value::Number(n) => (n as u8).max(1),
                    _ => grass,
                }
            };
            let hm_img = load_heightmap_image(&hm_id).map_err(rt_err)?;
            let cm_img = if cm_id.is_empty() { None } else { load_heightmap_image(&cm_id).ok() };
            let lo = (rg.min / rustudio_core::terrain::VOXEL).round().as_ivec3().to_array();
            let hi = (rg.max / rustudio_core::terrain::VOXEL).round().as_ivec3().to_array();
            let size = [0, 1, 2].map(|k| (hi[k] - lo[k]).max(1));

            let terrain_ref = {
                let w = world(lua)?;
                let d = w.d();
                let ws = d.workspace().ok_or_else(|| rt_err("Workspace not found"))?;
                d.dom
                    .get_by_ref(ws)
                    .and_then(|i| {
                        i.children()
                            .iter()
                            .copied()
                            .find(|c| d.class_of(*c).is_some_and(|cl| cl == "Terrain"))
                    })
                    .ok_or_else(|| rt_err("Workspace.Terrain not found"))?
            };
            let mut voxels = {
                let mut w = world(lua)?;
                if let Some((r, v)) = w.terrain_edit.take() {
                    if r == terrain_ref {
                        v
                    } else {
                        w.terrain_edit = Some((r, v));
                        match w.d().prop(terrain_ref, "SmoothGrid") {
                            Some(rbx_dom_weak::types::Variant::BinaryString(b)) => {
                                rustudio_core::terrain::Voxels::decode(AsRef::<[u8]>::as_ref(&b))
                                    .unwrap_or_else(|_| rustudio_core::terrain::Voxels::empty())
                            }
                            _ => rustudio_core::terrain::Voxels::empty(),
                        }
                    }
                } else {
                    match w.d().prop(terrain_ref, "SmoothGrid") {
                        Some(rbx_dom_weak::types::Variant::BinaryString(b)) => {
                            rustudio_core::terrain::Voxels::decode(AsRef::<[u8]>::as_ref(&b))
                                .unwrap_or_else(|_| rustudio_core::terrain::Voxels::empty())
                        }
                        _ => rustudio_core::terrain::Voxels::empty(),
                    }
                }
            };
            let hm = crate::terrain_gen::Image { w: hm_img.width, h: hm_img.height, rgba: &hm_img.pixels };
            let cm = cm_img.as_ref().map(|c| crate::terrain_gen::Image { w: c.width, h: c.height, rgba: &c.pixels });
            crate::terrain_gen::import(&mut voxels, lo, size, &hm, cm.as_ref(), default_mat);
            let blob = voxels.encode();
            {
                let mut w = world(lua)?;
                if let Some(i) = w.dm().dom.get_by_ref_mut(terrain_ref) {
                    i.properties.insert(
                        rbx_dom_weak::ustr("SmoothGrid"),
                        rbx_dom_weak::types::Variant::BinaryString(blob.into()),
                    );
                }
                w.dm().mark_prop(terrain_ref, rbx_dom_weak::ustr("SmoothGrid"));
                w.fire(this, "ProgressUpdate", vec![Value::Number(1.0), lstr(lua, "Import")?]);
            }
            ret0()
        })(),
        _ => return None,
    };
    Some(r)
}
