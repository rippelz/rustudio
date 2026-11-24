-- rustudio engine domain gui_input (src/domains/gui_input.rs): GUI objects,
-- input services, the input action system, styles, haptics / VR / gamepad,
-- proximity prompts. Studio's behaviour measured in an offline Studio
-- (checks/parity/gui_input.md). Loaded once per runtime after the library.
local G = ...
local fire, log = G.fire, G.log
-- The current DataModel. The native helper sends changed roots since the
-- last refresh, including when the editor replaces the whole document.
local game = game
local workspace = workspace
local function refresh_game()
    local ok, g, w = pcall(G.world_roots)
    if ok then
        if g then game = g end
        if w then workspace = w end
    end
end

local methods, get, set = {}, {}, {}
local M = { methods = methods, get = get, set = set }
local LM = __rs_lua_methods -- library methods this domain takes over

-- A service, or nil when the DataModel's copy is gone (GetService can
-- hand back a destroyed one on a live client).
local function svc(name)
    local ok, s = pcall(game.GetService, game, name)
    if ok and s and pcall(function() return s.Parent end) then return s end
    return game:FindFirstChildOfClass(name)
end
local function changed(inst, prop)
    fire(inst, "Changed", prop)
    fire(inst, "Changed:" .. prop)
end
local function enum(e, name) return Enum[e][name] end
local function f32(x) return Vector3.new(x, 0, 0).X end
local function isa(v, c) return typeof(v) == "Instance" and v:IsA(c) end
local function is_enum(v, e)
    if typeof(v) ~= "EnumItem" then return false end
    local ok, item = pcall(function() return Enum[e][v.Name] end)
    return ok and item == v
end
local function argerr(n, fn, want, got)
    error(string.format("invalid argument #%d to '%s' (%s expected, got %s)", n, fn, want, typeof(got)), 0)
end
local function local_player()
    local ok, p = pcall(function() return svc("Players").LocalPlayer end)
    return ok and p or nil
end
local KEY_NONE
for _, k in Enum.KeyCode:GetEnumItems() do
    if k.Value == 0 then KEY_NONE = k break end
end
local function input_object(fields)
    local t = { KeyCode = KEY_NONE, UserInputState = Enum.UserInputState.None, UserInputType = Enum.UserInputType.None,
        Position = Vector3.zero, Delta = Vector3.zero }
    for k, v in fields do t[k] = v end
    return G.new_input_object(t)
end
-- Visible all the way up and in an enabled LayerCollector.
local function shown(o)
    local cur = o
    while cur do
        if cur:IsA("GuiObject") then
            if not cur.Visible then return false end
        elseif cur:IsA("LayerCollector") then
            return cur.Enabled
        elseif not cur:IsA("Folder") then
            return false
        end
        cur = cur.Parent
    end
    return false
end

-------------------------------------------------------- descendant tracking
-- Instances this domain keeps up with (StyleLinks, input contexts, prompts,
-- haptic effects), found once and then through DescendantAdded.
local tracked = { StyleLink = {}, InputContext = {}, ProximityPrompt = {}, HapticEffect = {}, Adorn = {} }
local tracking = false
local styles_dirty = true
local style_recheck_frames = 0
__rs_styles_dirty = function()
    styles_dirty = true
end
local prompt_order = 0
local function track(i)
    local class = i.ClassName
    local set_ = tracked[class]
    if class == "ProximityPrompt" then
        if not set_[i] then
            prompt_order += 1
            set_[i] = prompt_order
        end
    elseif set_ then
        set_[i] = true
        if class == "StyleLink" then styles_dirty = true end
    end
    if i:IsA("HandleAdornment") or i:IsA("HandlesBase") then tracked.Adorn[i] = true end
end
local function start_tracking()
    if tracking then return end
    tracking = true
    local classes = G.is_edit()
        and { "StyleLink", "HandleAdornment", "HandlesBase" }
        or { "StyleLink", "InputContext", "ProximityPrompt", "HapticEffect", "HandleAdornment", "HandlesBase" }
    for _, d in G.find_classes(classes) do track(d) end
    -- Natively filtered DescendantAdded / Removing: no Lua call per instance.
    local added, removing = G.class_signals(classes)
    added:Connect(track)
    removing:Connect(function(i)
        local cn = i.ClassName
        local set_ = tracked[cn]
        if set_ then
            set_[i] = nil
            if cn == "StyleLink" then styles_dirty = true end
        end
        tracked.Adorn[i] = nil
    end)
end

------------------------------------------------------------------ GuiService
local gs_state = {
    paused_notification = true, emotes_open = false, inspect_enabled = true, inspect_open = false,
    nav = true, groups = {},
    global_inset = { 0, 58, 0, 0 },
    open_menus = {},
    selected_core = nil,
    center_dialogs = {},
}
get["GuiService.CoreGuiNavigationEnabled"] = function() return gs_state.nav end
set["GuiService.CoreGuiNavigationEnabled"] = function(inst, _, v)
    if type(v) ~= "boolean" then error("Unable to assign property CoreGuiNavigationEnabled. bool expected, got " .. typeof(v), 0) end
    if gs_state.nav ~= v then gs_state.nav = v changed(inst, "CoreGuiNavigationEnabled") end
    return true
end
get["GuiService.PreferredTextSize"] = function()
    return (__rs_user_game_settings and __rs_user_game_settings.PreferredTextSize) or Enum.PreferredTextSize.Medium
end
get["GuiService.PreferredTransparency"] = function()
    local v = __rs_user_game_settings and __rs_user_game_settings.PreferredTransparency
    return type(v) == "number" and v or 1
end
get["GuiService.ReducedMotionEnabled"] = function()
    return (__rs_user_game_settings and __rs_user_game_settings.ReducedMotion) == true
end
get["GuiService.ViewportDisplaySize"] = function() return Enum.DisplaySize.Medium end
get["GuiService.IsWindows"] = function() return true end
get["GuiService.CoreGuiFolder"] = function() return svc("CoreGui") end
get["GuiService.CoreEffectFolder"] = function() return svc("CoreGui") end
get["GuiService.DisplayScalingMode"] = function()
    local ok, v = pcall(function() return Enum.DisplayScalingMode.Auto end)
    return ok and v or nil
end
get["GuiService.ViewportSizeInMM"] = function() return Vector2.new(400, 225) end
get["GuiService.SelectedCoreObject"] = function() return gs_state.selected_core end
get["GuiService.SendCoreUiNotification"] = function() return gs_state.send_core_ui_notif end
set["GuiService.SendCoreUiNotification"] = function(_, _, v)
    if v ~= nil and type(v) ~= "function" then
        error("Unable to assign callback SendCoreUiNotification. Function expected, got " .. typeof(v), 0)
    end
    gs_state.send_core_ui_notif = v
    return true
end
set["GuiService.SelectedCoreObject"] = function(gs, _, v)
    if v ~= nil and not isa(v, "GuiObject") then
        error("Unable to assign property SelectedCoreObject. Object expected, got " .. typeof(v), 0)
    end
    local old = gs_state.selected_core
    if old == v then return true end
    if old and old.Parent then fire(old, "SelectionLost") end
    if v then fire(v, "SelectionGained") end
    gs_state.selected_core = v
    G.set_selected_core(gs, v)
    changed(gs, "SelectedCoreObject")
    return true
end

methods["GuiService.GetResolutionScale"] = function() return 1 end
methods["GuiService.GetSafeZoneOffsets"] = function() return { top = 0, bottom = 0, left = 0, right = 0 } end
methods["GuiService.GetHardwareSafeViewport"] = function()
    local w, h = G.view()
    return Vector2.new(w, h)
end
local topbar_seen
methods["GuiService.SetTopbarInset"] = function(gs, rect)
    if typeof(rect) ~= "Rect" then error("Unable to cast value to Rect", 0) end
    if G.set_topbar_inset(rect) then
        topbar_seen = G.topbar_inset()
        changed(gs, "TopbarInset")
    end
end
methods["GuiService.SetGlobalGuiInset"] = function(_, x1, y1, x2, y2)
    gs_state.global_inset = { tonumber(x1) or 0, tonumber(y1) or 0, tonumber(x2) or 0, tonumber(y2) or 0 }
end
local UNSUPPORTED_SETTINGS_ROWS = {
    ["Output DeviceFrame"] = true,
    ["HapticsFrame"] = true,
    ["In-game friends chat notificationsFrame"] = true,
}
local disabled_settings_rows = setmetatable({}, { __mode = "k" })
local settings_watch_connected = setmetatable({}, { __mode = "k" })
local function disable_settings_row(row)
    if not row or disabled_settings_rows[row] then return end
    disabled_settings_rows[row] = true
    local function dim(o)
        if not o then return end
        if o:IsA("GuiObject") then
            pcall(function() o.Interactable = false end)
            pcall(function() o.Active = false end)
            pcall(function() o.Selectable = false end)
            if o:IsA("TextLabel") or o:IsA("TextButton") then
                pcall(function() o.TextTransparency = math.max(o.TextTransparency, 0.55) end)
            elseif o:IsA("ImageLabel") or o:IsA("ImageButton") then
                pcall(function()
                    if o.Image ~= "" then
                        o.ImageTransparency = math.max(o.ImageTransparency, 0.72)
                    end
                end)
            end
        elseif o:IsA("UIStroke") then
            pcall(function() o.Transparency = math.max(o.Transparency, 0.65) end)
        end
    end
    dim(row)
    for _, d in row:GetDescendants() do
        dim(d)
    end
end
local function disable_unsupported_settings_rows()
    local cg = svc("CoreGui")
    if not cg then return end
    local rg = cg:FindFirstChild("RobloxGui")
    if not rg then return end
    local shield = rg:FindFirstChild("SettingsClippingShield")
    if not shield then return end
    local function scan(root)
        for _, d in root:GetDescendants() do
            if UNSUPPORTED_SETTINGS_ROWS[d.Name] and d:IsA("GuiObject") then
                disable_settings_row(d)
            end
        end
    end
    scan(shield)
    if not settings_watch_connected[shield] then
        settings_watch_connected[shield] = true
        shield.DescendantAdded:Connect(function(d)
            if UNSUPPORTED_SETTINGS_ROWS[d.Name] and d:IsA("GuiObject") then
                task.defer(disable_settings_row, d)
            end
        end)
    end
end
local function sync_core_menu()
    G.set_core_menu_open(next(gs_state.open_menus) ~= nil or gs_state.inspect_open)
end
methods["GuiService.SetMenuIsOpen"] = function(gs, open, menuName)
    local key = tostring(menuName or "default")
    local was = next(gs_state.open_menus) ~= nil or gs_state.inspect_open or G.menu_open()
    gs_state.open_menus[key] = (open == true) and true or nil
    sync_core_menu()
    if open == true then
        task.defer(disable_unsupported_settings_rows)
    end
    local now = next(gs_state.open_menus) ~= nil or gs_state.inspect_open or G.menu_open()
    if was ~= now then
        fire(gs, now and "MenuOpened" or "MenuClosed")
    end
end
methods["GuiService.GetErrorMessage"] = function() return "" end
methods["GuiService.GetErrorCode"] = function() return Enum.ConnectionError.OK end
methods["GuiService.GetErrorType"] = function() return Enum.ConnectionError.OK end
methods["GuiService.BroadcastNotification"] = function() end
methods["GuiService.IsMemoryTrackerEnabled"] = function() return false end
methods["GuiService.GetAutoUIScaleHundredths"] = function() return 100 end
methods["GuiService.GetEffectiveUIScaleHundredths"] = function() return 100 end
methods["GuiService.GetRawScreenScale"] = function() return 1 end
methods["GuiService.GetScreenResolution"] = function()
    local w, h = G.view()
    return Vector2.new(w, h)
end
methods["GuiService.GetBrickCount"] = function() return 0 end
methods["GuiService.GetUiMessage"] = function() return "" end
methods["GuiService.GetErrorDetails"] = function() return {} end
methods["GuiService.GetGuiIsVisible"] = function() return true end
methods["GuiService.CloseStatsBasedOnInputString"] = function() return false end
methods["GuiService.ShowStatsBasedOnInputString"] = function() return false end
for _, n in {
    "ClearError", "ForceTenFootInterface", "AddKey", "RemoveKey", "AddSpecialKey", "RemoveSpecialKey",
    "SetHardwareSafeAreaInsets", "SetPurchasePromptIsShown", "SetSafeZoneOffsets", "SetUIScaleMultiplier",
    "SetUiMessage", "ToggleGuiIsVisibleForCaptures", "ToggleGuiIsVisibleIfAllowed",
    "OnNotificationDisplayed", "OnNotificationInteraction", "OpenBrowserWindow", "OpenNativeOverlay",
    "SendUIOcclusionMetricsForQueryRegion", "InspectPlayerFromUserIdWithCtx",
} do
    methods["GuiService." .. n] = function() end
end
methods["GuiService.ToggleFullscreen"] = function()
    local ok, ugs = pcall(function() return UserSettings():GetService("UserGameSettings") end)
    if ok and ugs then
        pcall(function() ugs.Fullscreen = not ugs.Fullscreen end)
    end
end
-- VRService CoreScript members
methods["VRService.IsVRAppBuild"] = function() return false end
methods["VRService.IsMaquettes"] = function() return false end
get["VRService.VRDeviceAvailable"] = function() return false end
get["VRService.VRDeviceName"] = function() return "" end
get["VRService.VRSessionState"] = function()
    local ok, v = pcall(function() return Enum.VRSessionState.Undefined end)
    return ok and v or nil
end
get["VRService.DidPointerHit"] = function() return false end
get["VRService.LaserDistance"] = function() return 0 end
get["VRService.PointerHitCFrame"] = function() return CFrame.new() end
get["VRService.QuestDisplayRefreshRate"] = function() return 0 end
get["VRService.QuestASWState"] = function() return false end
get["VRService.LaserPointer"] = function()
    local ok, v = pcall(function() return Enum.VRLaserPointerMode.Pointer end)
    return ok and v or nil
end
get["VRService.PresentationMode"] = function()
    local ok, v = pcall(function() return Enum.VRPresentationMode.Stereo end)
    return ok and v or nil
end
methods["GuiService.AddCenterDialog"] = function(_, dialog, dialogType, showFn, hideFn)
    local list = gs_state.center_dialogs
    local prev = list[#list]
    if prev and prev.hide then pcall(prev.hide) end
    table.insert(list, { dialog = dialog, kind = dialogType, show = showFn, hide = hideFn })
    if showFn then pcall(showFn) end
end
methods["GuiService.RemoveCenterDialog"] = function(_, dialog)
    local list = gs_state.center_dialogs
    for i = #list, 1, -1 do
        if list[i].dialog == dialog then
            local removed = table.remove(list, i)
            if removed.hide then pcall(removed.hide) end
            if i == #list + 1 and list[#list] and list[#list].show then
                pcall(list[#list].show)
            end
            break
        end
    end
end

get["GuiObject.Transparency"] = function(o)
    return o.BackgroundTransparency
end
set["GuiObject.Transparency"] = function(o, _, v)
    local n = tonumber(v)
    if n == nil then error("Unable to assign property Transparency. number expected, got " .. typeof(v), 0) end
    o.BackgroundTransparency = n
    if o:IsA("TextLabel") or o:IsA("TextButton") or o:IsA("TextBox") then
        o.TextTransparency = n
    end
    return true
end

get["GuiService.TopbarInset"] = G.topbar_inset
local function step_topbar()
    local rect = G.topbar_inset()
    if rect == topbar_seen then return end
    local first = topbar_seen == nil
    topbar_seen = rect
    if first then return end
    local gs = svc("GuiService")
    if gs then changed(gs, "TopbarInset") end
end

methods["GuiService.AddSelectionParent"] = function(_, name, parent)
    gs_state.groups[tostring(name)] = { parent }
end
methods["GuiService.AddSelectionTuple"] = function(_, name, ...)
    gs_state.groups[tostring(name)] = { ... }
end
methods["GuiService.RemoveSelectionGroup"] = function(_, name) gs_state.groups[tostring(name)] = nil end
methods["GuiService.SendNotification"] = function() error("GuiService:SendNotification is not yet enabled!", 0) end
methods["GuiService.DismissNotification"] = function() error("GuiService:DismissNotification is not yet enabled!", 0) end
methods["GuiService.GetGameplayPausedNotificationEnabled"] = function() return gs_state.paused_notification end
methods["GuiService.SetGameplayPausedNotificationEnabled"] = function(_, on) gs_state.paused_notification = on == true end
methods["GuiService.SetEmotesMenuOpen"] = function(self, open)
    local v = open == true
    if gs_state.emotes_open ~= v then
        gs_state.emotes_open = v
        fire(self, "EmotesMenuOpenChanged", v)
    end
end
LM.GetEmotesMenuOpen = function() return gs_state.emotes_open end
methods["GuiService.SetInspectMenuEnabled"] = function(_, on) gs_state.inspect_enabled = on == true end
LM.GetInspectMenuEnabled = function() return gs_state.inspect_enabled end
-- The inspect menu (drawn by game_menu.rs). Studio opens it whatever
-- SetInspectMenuEnabled says (that only hides the player list's button):
-- MenuOpened and MenuIsOpen at the call, MenuClosed when it closes. It
-- lists the avatar's worn items with their prices (read-only lookups).
local inspect_seq = 0
local function close_inspect()
    if not gs_state.inspect_open then return end
    gs_state.inspect_open = false
    sync_core_menu()
    inspect_seq += 1
    G.inspect_set(nil)
    fire(svc("GuiService"), "MenuClosed")
end
local function open_inspect(title_fn, desc_fn)
    local was = gs_state.inspect_open
    gs_state.inspect_open = true
    sync_core_menu()
    inspect_seq += 1
    local seq = inspect_seq
    G.inspect_set({ title = "", robux = 0, items = {}, loading = true })
    if not was then fire(svc("GuiService"), "MenuOpened") end
    task.spawn(function()
        local title = select(2, pcall(title_fn)) or ""
        if seq ~= inspect_seq then return end
        G.inspect_set({ title = tostring(title) .. "'s Avatar", robux = 0, items = {}, loading = true })
        local ok, desc = pcall(desc_fn)
        local ids = {}
        if ok and type(desc) == "table" then
            for _, id in desc do table.insert(ids, id) end
        elseif ok and typeof(desc) == "Instance" then
            local function add(id) id = tonumber(id) if id and id > 0 and not table.find(ids, id) then table.insert(ids, id) end end
            for _, a in desc:GetAccessories(true) do add(a.AssetId) end
            for _, p in { "Shirt", "Pants", "GraphicTShirt", "Face", "Head", "Torso", "LeftArm", "RightArm", "LeftLeg", "RightLeg",
                "IdleAnimation", "WalkAnimation", "RunAnimation", "JumpAnimation", "FallAnimation", "ClimbAnimation", "SwimAnimation" } do
                add(desc[p])
            end
        end
        local items, total = {}, 0
        local mps = svc("MarketplaceService")
        for _, id in ids do
            if seq ~= inspect_seq then return end
            local okp, info = pcall(function() return mps:GetProductInfoAsync(id) end)
            if okp and type(info) == "table" then
                local price = info.IsForSale and tonumber(info.PriceInRobux) or nil
                if price then total += price end
                table.insert(items, { name = tostring(info.Name), price = price })
            end
        end
        if seq ~= inspect_seq then return end
        G.inspect_set({ title = tostring(title) .. "'s Avatar", robux = total, items = items, loading = false })
    end)
end
methods["GuiService.InspectPlayerFromUserId"] = function(_, id)
    id = tonumber(id)
    if id == nil or id <= 0 then error("GuiService:InspectPlayerFromUserId - Got invalid userId.", 0) end
    local players = svc("Players")
    open_inspect(function()
        local p = players:GetPlayerByUserId(id)
        if p then return p.DisplayName end
        return players:GetNameFromUserIdAsync(id)
    end, function()
        -- The worn assets, as the avatar API lists them.
        local job = G.web_get("https://avatar.roblox.com/v1/users/" .. math.floor(id) .. "/avatar")
        local status, text
        repeat
            task.wait()
            status, text = G.textgen_poll(job)
        until status ~= nil
        if status ~= 200 then error(text) end
        local data = svc("HttpService"):JSONDecode(text)
        local ids = {}
        for _, a in data.assets or {} do table.insert(ids, a.id) end
        return ids
    end)
end
methods["GuiService.InspectPlayerFromHumanoidDescription"] = function(_, desc, name)
    if not isa(desc, "HumanoidDescription") then error("Unable to cast value to Object", 0) end
    open_inspect(function() return tostring(name) end, function() return desc end)
end
methods["GuiService.CloseInspectMenu"] = function() close_inspect() end
get["GuiService.MenuIsOpen"] = function() return next(gs_state.open_menus) ~= nil or gs_state.inspect_open or G.menu_open() end
local function step_inspect()
    if G.inspect_closed() then close_inspect() end
end

-- GetInsetArea: the screen less what the given insets keep clear, in GUI
-- coordinates (Studio's Play: the 58 px top bar is above y = 0).
methods["GuiService.GetInsetArea"] = function(_, insets)
    if not is_enum(insets, "ScreenInsets") then
        error("Unable to cast " .. typeof(insets) .. " to token", 0)
    end
    local w, h = G.view()
    local inset = (G.is_client() and not G.is_edit()) and 58 or 0
    if insets == Enum.ScreenInsets.CoreUISafeInsets then
        return Rect.new(0, 0, w, h - inset)
    end
    -- TopbarSafeInsets: the bar's free area (TopbarInset), above y = 0.
    if insets == Enum.ScreenInsets.TopbarSafeInsets and inset > 0 then
        local bar = G.topbar_inset()
        return Rect.new(bar.Min.X, bar.Min.Y - inset, bar.Max.X, bar.Max.Y - inset)
    end
    return Rect.new(0, -inset, w, h - inset)
end

local function selectable(o)
    return isa(o, "GuiObject") and o.Selectable and shown(o) and o.AbsoluteSize.X > 0 and o.AbsoluteSize.Y > 0
end
-- The first selectable object under `parent`, top-most then left-most.
local function first_selectable(parent)
    local best, by, bx
    for _, d in parent:GetDescendants() do
        if selectable(d) then
            local p = d.AbsolutePosition
            if not best or p.Y < by or (p.Y == by and p.X < bx) then
                best, by, bx = d, p.Y, p.X
            end
        end
    end
    return best
end
methods["GuiService.Select"] = function(self, parent)
    if typeof(parent) ~= "Instance" or not parent:FindFirstAncestorWhichIsA("BasePlayerGui") then
        error("Calling GuiService:Select() on an instance that is not a descendant of a BasePlayerGui", 0)
    end
    local pick = first_selectable(parent)
    if pick then self.SelectedObject = pick end
end

-- SelectedObject: SelectionLost on the old one, SelectionGained on the new,
-- the property's Changed, then SelectionChanged(amISelected, previous, new)
-- up through every GuiBase2d holding either (Studio's order). A selection
-- that can't be selected warns and moves to a selectable descendant.
local selected = nil
set["GuiService.SelectedObject"] = function(gs, _, v)
    if v ~= nil and not isa(v, "GuiObject") then
        error("Unable to assign property SelectedObject. Object expected, got " .. typeof(v), 0)
    end
    local old = gs.SelectedObject
    if old == v then return true end
    local invalid = v ~= nil and not v.Selectable
    if invalid then log("warn", "Setting GuiService.SelectedObject to invalid GuiObject") end
    if old and old.Parent then fire(old, "SelectionLost") end
    if v then fire(v, "SelectionGained") end
    __rs_set_raw(gs, "SelectedObject", v)
    selected = v
    local done = {}
    local function up(from)
        local cur = from
        while cur and cur ~= game do
            if cur:IsA("GuiBase2d") and not done[cur] then
                done[cur] = true
                fire(cur, "SelectionChanged", cur == v, old, v)
            end
            cur = cur.Parent
        end
    end
    if v then up(v) end
    if old then up(old) end
    if invalid then
        local pick = first_selectable(v)
        if pick then gs.SelectedObject = pick else gs.SelectedObject = nil end
    end
    return true
end

------------------------------------------------------------ UserInputService
get["UserInputService.OnScreenKeyboardPosition"] = function() return Vector2.zero end
get["UserInputService.OnScreenKeyboardSize"] = function() return Vector2.zero end
get["UserInputService.UserHeadCFrame"] = function() return CFrame.new() end
methods["UserInputService.GetUserCFrame"] = function(_, t)
    if typeof(t) ~= "EnumItem" then error("Unable to cast " .. typeof(t) .. " to token", 0) end
    return CFrame.new()
end
methods["UserInputService.RecenterUserHeadCFrame"] = function() end
-- No accelerometer or gyroscope (Studio's warnings, an empty InputObject).
methods["UserInputService.GetDeviceAcceleration"] = function()
    log("warn", "Trying to get acceleration data on a device without a accelerometer.")
    return input_object({ UserInputType = Enum.UserInputType.Accelerometer, UserInputState = Enum.UserInputState.Change })
end
LM.GetDeviceGravity = function()
    log("warn", "Trying to get gravity data on a device without a accelerometer.")
    return input_object({ UserInputType = Enum.UserInputType.Accelerometer, UserInputState = Enum.UserInputState.Change })
end
LM.GetDeviceRotation = function()
    log("warn", "Trying to get rotation data on a device without a gyroscope.")
    return input_object({ UserInputType = Enum.UserInputType.Gyro, UserInputState = Enum.UserInputState.Change }), CFrame.new()
end
methods["UserInputService.GetMouseButtonsPressed"] = function()
    local b1, b2, b3 = G.buttons()
    local _, _, mx, my = G.view()
    local out = {}
    for i, down in { b1, b2, b3 } do
        if down then
            table.insert(out, input_object({ UserInputType = Enum.UserInputType["MouseButton" .. i],
                UserInputState = Enum.UserInputState.Begin, Position = Vector3.new(mx, my - 58, 0) }))
        end
    end
    return out
end
local nav_pads = {}
methods["UserInputService.SetNavigationGamepad"] = function(_, pad, on)
    if typeof(pad) ~= "EnumItem" then error("Unable to cast " .. typeof(pad) .. " to token", 0) end
    nav_pads[pad] = on == true or nil
end
LM.IsNavigationGamepad = function(_, pad) return nav_pads[pad] == true end
LM.GetNavigationGamepads = function()
    local out = {}
    for p in nav_pads do table.insert(out, p) end
    table.sort(out, function(a, b) return a.Value < b.Value end)
    return out
end

-- VirtualInput: Studio's scriptable input source (typeof "Object"). Its
-- sends go through the same path as the player's input.
local function send(state, uit, key, x, y, z, dx, dy)
    if __rs_input_event then
        task.spawn(__rs_input_event, state, uit, key, x or 0, y or 0, z or 0, dx or 0, dy or 0, false)
    end
end
local vi_methods = {}
function vi_methods:SendKey(down, key)
    if typeof(key) ~= "EnumItem" then error("Unable to cast " .. typeof(key) .. " to token", 0) end
    send(down and "Begin" or "End", "Keyboard", key.Name)
end
function vi_methods:SendMouseButton(pos, button, down)
    if typeof(pos) ~= "Vector2" then error("Unable to cast " .. typeof(pos) .. " to Vector2", 0) end
    if typeof(button) ~= "EnumItem" then error("Unable to cast " .. typeof(button) .. " to token", 0) end
    send(down and "Begin" or "End", button.Name, "Unknown", pos.X, pos.Y)
end
function vi_methods:SendMouseDelta(d)
    if svc("UserInputService").MouseBehavior == Enum.MouseBehavior.Default then
        error("VirtualInput::SendMouseDelta: cursor is not locked.", 0)
    end
    send("Change", "MouseMovement", "Unknown", 0, 0, 0, d.X, d.Y)
end
function vi_methods:SendMousePosition(pos)
    if typeof(pos) ~= "Vector2" then error("Unable to cast " .. typeof(pos) .. " to Vector2", 0) end
    send("Change", "MouseMovement", "Unknown", pos.X, pos.Y)
end
function vi_methods:SendPointerAction(pos, action)
    action = type(action) == "table" and action or {}
    fire(svc("UserInputService"), "PointerAction", tonumber(action.Wheel) or 0,
        typeof(action.Pan) == "Vector2" and action.Pan or Vector2.zero, tonumber(action.Pinch) or 0, false)
end
local focused -- the TextBox with the keyboard (forward)
function vi_methods:SendTextInput(text)
    if focused and focused.Parent then
        focused.Text = focused.Text .. tostring(text)
    end
end
function vi_methods:IsA(c) return c == "VirtualInput" or c == "Object" end
for _, n in { "SendKey", "SendMouseButton", "SendMouseDelta", "SendMousePosition", "SendPointerAction", "SendTextInput" } do
    methods["VirtualInput." .. n] = vi_methods[n]
end
methods["UserInputService.CreateVirtualInput"] = function()
    return G.make_object("VirtualInput", vi_methods)
end

--------------------------------------------------------- ContextActionService
methods["ContextActionService.BindActionToInputTypes"] = function(self, name, fn, touch, ...)
    return self:BindAction(name, fn, touch, ...)
end
methods["ContextActionService.BindCoreAction"] = function(self, name, fn, touch, ...)
    return LM.BindCoreAction(self, name, fn, touch, ...)
end
methods["ContextActionService.BindCoreActionAtPriority"] = function(self, name, fn, touch, priority, ...)
    return LM.BindCoreActionAtPriority(self, name, fn, touch, priority, ...)
end
methods["ContextActionService.UnbindCoreAction"] = function(self, name)
    return LM.UnbindCoreAction(self, name)
end
methods["ContextActionService.GetBoundCoreActionInfo"] = function(self, name)
    return LM.GetBoundCoreActionInfo(self, name)
end
methods["ContextActionService.GetAllBoundCoreActionInfo"] = function(self)
    return LM.GetAllBoundCoreActionInfo(self)
end
local equipped_tool = nil
methods["ContextActionService.GetCurrentLocalToolIcon"] = function()
    if equipped_tool and equipped_tool.Parent then return equipped_tool.TextureId end
    return ""
end
local function step_tools()
    local p = local_player()
    local char = p and p.Character
    local tool = char and char:FindFirstChildOfClass("Tool")
    if tool ~= equipped_tool then
        local cas = svc("ContextActionService")
        if equipped_tool then fire(cas, "LocalToolUnequipped", equipped_tool) end
        equipped_tool = tool
        if tool then fire(cas, "LocalToolEquipped", tool) end
    end
end

------------------------------------------------------------ GuiObject state
local function compute_gui_state(o)
    if not o.Interactable then return Enum.GuiState.NonInteractable end
    local hovered, pressed = G.hover_state(o)
    if pressed then return Enum.GuiState.Press end
    if hovered then return Enum.GuiState.Hover end
    return Enum.GuiState.Idle
end
local last_gui_state = setmetatable({}, { __mode = "k" })
local function check_gui_state(o)
    if not o or not pcall(function() return o.Parent end) then return end
    local now = compute_gui_state(o)
    if last_gui_state[o] ~= now then
        last_gui_state[o] = now
        if next(tracked.StyleLink) ~= nil then
            style_recheck_frames = 4
        end
        changed(o, "GuiState")
    end
end
M.check_gui_state = check_gui_state
get["GuiObject.GuiState"] = function(o)
    local now = compute_gui_state(o)
    last_gui_state[o] = now
    return now
end
set["GuiObject.Interactable"] = function(o, _, v)
    if type(v) ~= "boolean" then
        error("Unable to assign property Interactable. bool expected, got " .. typeof(v), 0)
    end
    __rs_set_raw(o, "Interactable", v)
    check_gui_state(o)
    return true
end
local sinks = setmetatable({}, { __mode = "k" })
get["GuiObject.InputSink"] = function(o) return sinks[o] or Enum.InputSink.None end
set["GuiObject.InputSink"] = function(o, _, v)
    if typeof(v) == "number" or typeof(v) == "string" then
        for _, e in Enum.InputSink:GetEnumItems() do
            if e.Value == v or e.Name == v then v = e end
        end
    end
    if not is_enum(v, "InputSink") then
        error("Unable to assign property InputSink. EnumItem expected, got " .. typeof(v), 0)
    end
    if (sinks[o] or Enum.InputSink.None) ~= v then
        sinks[o] = v
        changed(o, "InputSink")
    end
    return true
end

------------------------------------------------- GetGuiObjectsAtPosition
-- Every GuiObject drawn at (x, y) in GUI coordinates (AbsolutePosition's),
-- top-most first: ScreenGuis by DisplayOrder, ZIndexBehavior's order inside
-- each, clipped by ClipsDescendants and ScrollingFrames. The walk is Rust
-- (gui_input.rs objects_at): a big PlayerGui made it a long frame in Luau.
local function objects_at(guis, x, y)
    return G.objects_at(guis, x, y)
end
local function screen_guis(pg)
    local list, idx = {}, {}
    for i, c in pg:GetChildren() do
        if c:IsA("ScreenGui") and c.Enabled then
            table.insert(list, c)
            idx[c] = i
        end
    end
    table.sort(list, function(a, b)
        if a.DisplayOrder ~= b.DisplayOrder then return a.DisplayOrder > b.DisplayOrder end
        return idx[a] > idx[b]
    end)
    return list
end
local function check_xy(x, y)
    if tonumber(x) == nil then error("Unable to cast " .. typeof(x) .. " to int", 0) end
    if tonumber(y) == nil then error("Unable to cast " .. typeof(y) .. " to int", 0) end
    return math.floor(tonumber(x)), math.floor(tonumber(y))
end
methods["BasePlayerGui.GetGuiObjectsAtPosition"] = function(self, x, y)
    x, y = check_xy(x, y)
    return objects_at(screen_guis(self), x, y)
end
-- A ScreenGui's own answer: Studio gives an empty list for the PlayerGui's
-- ScreenGuis (measured); other LayerCollectors answer for themselves.
methods["LayerCollector.GetGuiObjectsAtPosition"] = function(self, x, y)
    x, y = check_xy(x, y)
    if self:IsA("ScreenGui") and self:FindFirstAncestorWhichIsA("BasePlayerGui") then return {} end
    if not self.Enabled then return {} end
    return objects_at({ self }, x, y)
end
methods["LayerCollector.GetLayoutNodeTree"] = function(self)
    local function node(o)
        local t = { Instance = o, Children = {} }
        for _, c in o:GetChildren() do
            if c:IsA("GuiObject") then table.insert(t.Children, node(c)) end
        end
        return t
    end
    return node(self)
end

----------------------------------------------------------------- PlayerGui
local topbar = {}
get["PlayerGui.CurrentScreenOrientation"] = function() return Enum.ScreenOrientation.LandscapeLeft end
methods["PlayerGui.GetTopbarTransparency"] = function(self) return topbar[self] or 0.5 end
methods["PlayerGui.SetTopbarTransparency"] = function(self, v)
    v = tonumber(v)
    if v == nil then error("Unable to cast value to float", 0) end
    v = f32(v)
    if topbar[self] ~= v then
        topbar[self] = v
        fire(self, "TopbarTransparencyChangedSignal", v)
    end
end
get["CoreGui.Version"] = function() return 0 end

--------------------------------------------------------------- Text objects
local function content(o)
    if o.RichText then return G.content_text(o.Text) end
    return o.Text
end
for _, c in { "TextLabel", "TextButton", "TextBox" } do
    get[c .. ".ContentText"] = content
    get[c .. ".OpenTypeFeaturesError"] = function() return "" end
    -- Fits when its full text layout fit inside its padded box last frame.
    get[c .. ".TextFits"] = function(o)
        local x, _, _, _, _, _, tw, _, _, _, fits = G.layout(o)
        if x == nil or tw < 0 then return false end
        return fits == true
    end
end
get["TextLabel.LocalizedText"] = function(o) return o.Text end
get["TextButton.LocalizedText"] = function(o) return o.Text end
get["ImageLabel.IsLoaded"] = function(o) return G.image_loaded(o.Image) end
get["ImageButton.IsLoaded"] = function(o) return G.image_loaded(o.Image) end

------------------------------------------------------------------ TextBox
-- CaptureFocus: ClearTextOnFocus empties it (only when TextEditable), the
-- cursor goes to the end (or clicked glyph), Focused then UserInputService.TextBoxFocused.
-- ReleaseFocus: cursor and selection to -1, FocusLost(submitted, input) then
-- TextBoxFocusReleased.
local cursor, selstart = setmetatable({}, { __mode = "k" }), setmetatable({}, { __mode = "k" })
local last_text = nil
local function set_cursor(box, v)
    if (cursor[box] or 1) == v then return end
    cursor[box] = v
    G.sync_tb_sel(box, v, nil)
    changed(box, "CursorPosition")
end
local function set_sel(box, v)
    if (selstart[box] or -1) == v then return end
    selstart[box] = v
    G.sync_tb_sel(box, nil, v)
    changed(box, "SelectionStart")
end
get["TextBox.CursorPosition"] = function(box)
    local v = cursor[box]
    if v == nil then return 1 end
    return v
end
get["TextBox.SelectionStart"] = function(box)
    local v = selstart[box]
    if v == nil then return -1 end
    return v
end
local function int_arg(prop, v)
    v = tonumber(v)
    if v == nil then error("Unable to assign property " .. prop .. ". int expected", 0) end
    return math.floor(v)
end
set["TextBox.CursorPosition"] = function(box, _, v)
    v = int_arg("CursorPosition", v)
    local n = #box.Text
    if focused == box then v = math.clamp(v, 1, n + 1) else v = math.clamp(v, -1, n + 1) end
    set_cursor(box, v)
    return true
end
set["TextBox.SelectionStart"] = function(box, _, v)
    v = int_arg("SelectionStart", v)
    local n = #box.Text
    set_sel(box, math.clamp(v, -1, n + 1))
    return true
end
-- A TextBox in a plugin's dock widget: UserInputService doesn't hear of it
-- (Studio 0.747: checks/plugins/widgets/captures/studio-widgets.txt).
-- (Play's plugin copies too: their widgets are plugin GUIs as well)
local function in_widget(box)
    return box:FindFirstAncestorWhichIsA("PluginGui") ~= nil
end
local function gain(box, click_pos)
    local cleared = false
    if box.ClearTextOnFocus and box.TextEditable and box.Text ~= "" then
        box.Text = ""
        cleared = true
    end
    focused = box
    last_text = box.Text
    local n = #last_text
    local target = (not cleared and click_pos ~= nil) and math.clamp(click_pos, 1, n + 1) or (n + 1)
    local old_c = cursor[box]
    if old_c == nil then old_c = 1 end
    local old_s = selstart[box]
    if old_s == nil then old_s = -1 end
    cursor[box] = target
    selstart[box] = -1
    G.sync_tb_sel(box, target, -1)
    if old_c ~= target then changed(box, "CursorPosition") end
    if old_s ~= -1 then changed(box, "SelectionStart") end
    check_gui_state(box)
    fire(box, "Focused")
    if not in_widget(box) then fire(svc("UserInputService"), "TextBoxFocused", box) end
end
local function lose(box, enter, input)
    if focused ~= box then return end
    focused = nil
    local old_c = cursor[box]
    if old_c == nil then old_c = 1 end
    local old_s = selstart[box]
    if old_s == nil then old_s = -1 end
    cursor[box] = -1
    selstart[box] = -1
    G.sync_tb_sel(box, -1, -1)
    if old_c ~= -1 then changed(box, "CursorPosition") end
    if old_s ~= -1 then changed(box, "SelectionStart") end
    check_gui_state(box)
    fire(box, "FocusLost", enter == true, input)
    if not in_widget(box) then fire(svc("UserInputService"), "TextBoxFocusReleased", box) end
end
methods["TextBox.CaptureFocus"] = function(box)
    if focused == box or not G.is_client() then return end
    if not box:IsDescendantOf(game) or not box:FindFirstAncestorWhichIsA("LayerCollector") then return end
    if focused then
        G.set_focus(nil)
        lose(focused, false, nil)
    end
    G.set_focus(box)
    gain(box, nil)
end
methods["TextBox.ReleaseFocus"] = function(box, submitted)
    if focused ~= box then return end
    G.set_focus(nil)
    lose(box, submitted, nil)
end
methods["TextBox.IsFocused"] = function(box) return focused == box end
-- (moved here from runtime_ext.rs, which answered nil)
methods["UserInputService.GetFocusedTextBox"] = function() return focused end
local function step_focus()
    for _, e in G.focus_events() do
        if e.gained then
            if focused and focused ~= e.box then lose(focused, false, nil) end
            if focused ~= e.box then
                local click_pos = G.take_focus_click(e.box)
                gain(e.box, click_pos)
            end
        else
            local io
            if e.button == 0 then
                io = input_object({ UserInputType = Enum.UserInputType.Keyboard, UserInputState = Enum.UserInputState.Begin,
                    KeyCode = e.enter and Enum.KeyCode.Return or Enum.KeyCode.Escape })
            elseif e.button then
                io = input_object({ UserInputType = Enum.UserInputType["MouseButton" .. e.button],
                    UserInputState = Enum.UserInputState.Begin, Position = Vector3.new(e.x, e.y, 0) })
            end
            lose(e.box, e.enter, io)
        end
    end
    if focused then
        if not focused:IsDescendantOf(game) then
            local box = focused
            G.set_focus(nil)
            lose(box, false, nil)
        elseif focused.Text ~= last_text then
            -- Script mutated Text while focused: clamp cursor/selection to new length
            -- without jumping CursorPosition to end (matches Studio 12a/12b).
            last_text = focused.Text
            local max_pos = #last_text + 1
            local c = cursor[focused] or 1
            if c > max_pos then set_cursor(focused, max_pos) end
            local s = selstart[focused] or -1
            if s > max_pos then set_sel(focused, max_pos) end
        end
    end
end

-- Applies a single UI-side TextBox edit/navigation commit in Studio's exact order:
-- all fields updated first, then CursorPosition -> SelectionStart -> Text signals.
function M.apply_textbox_edit(box, new_text, new_cur, new_sel)
    local old_c = cursor[box]
    if old_c == nil then old_c = 1 end
    local old_s = selstart[box]
    if old_s == nil then old_s = -1 end
    local text_changed = false
    if new_text ~= nil and box.Text ~= new_text then
        G.set_text_silent(box, new_text)
        if focused == box then last_text = new_text end
        text_changed = true
    end
    local cur_changed = false
    if new_cur ~= nil and old_c ~= new_cur then
        cursor[box] = new_cur
        cur_changed = true
    end
    local sel_changed = false
    if new_sel ~= nil and old_s ~= new_sel then
        selstart[box] = new_sel
        sel_changed = true
    end
    if cur_changed then changed(box, "CursorPosition") end
    if sel_changed then changed(box, "SelectionStart") end
    if text_changed then changed(box, "Text") end
end

------------------------------------------------------------- ScrollingFrame
-- The wheel scrolls in whole steps here: no fling to slow down.
methods["ScrollingFrame.GetScrollVelocity"] = function() return Vector2.zero end
methods["ScrollingFrame.ResetScrollVelocity"] = function() end

-------------------------------------------------------------- UIGridLayout
local function grid_cells(g)
    local parent = g.Parent
    if not isa(parent, "GuiBase2d") then return Vector2.zero, Vector2.new(g.CellSize.X.Offset, g.CellSize.Y.Offset) end
    local s = parent.AbsoluteSize
    local cs, cp = g.CellSize, g.CellPadding
    local cw, ch = cs.X.Scale * s.X + cs.X.Offset, cs.Y.Scale * s.Y + cs.Y.Offset
    local px, py = cp.X.Scale * s.X + cp.X.Offset, cp.Y.Scale * s.Y + cp.Y.Offset
    local n = 0
    for _, c in parent:GetChildren() do
        if c:IsA("GuiObject") and c.Visible then n += 1 end
    end
    if n == 0 then return Vector2.zero, Vector2.new(cw, ch) end
    local vertical = g.FillDirection == Enum.FillDirection.Vertical
    local fit = vertical and math.floor((s.Y + py) / math.max(ch + py, 1e-3)) or math.floor((s.X + px) / math.max(cw + px, 1e-3))
    local per = math.max(fit, 1)
    if g.FillDirectionMaxCells > 0 then per = math.min(per, g.FillDirectionMaxCells) end
    local lines = math.ceil(n / per)
    local along = math.min(per, n)
    if vertical then return Vector2.new(lines, along), Vector2.new(cw, ch) end
    return Vector2.new(along, lines), Vector2.new(cw, ch)
end
get["UIGridLayout.AbsoluteCellCount"] = function(g)
    local x, _, _, _, cx, cy = G.layout(g)
    if x ~= nil and cx >= 0 then return Vector2.new(cx, cy) end
    return (grid_cells(g))
end
get["UIGridLayout.AbsoluteCellSize"] = function(g)
    local x, _, _, _, _, _, _, _, wx, wy = G.layout(g)
    if x ~= nil and wx >= 0 then return Vector2.new(wx, wy) end
    return select(2, grid_cells(g))
end
-- Deprecated: the layout already re-applies itself every frame; a custom
-- sort function is what SortOrder Custom used to call.
methods["UIGridStyleLayout.ApplyLayout"] = function() end
local sort_fns = setmetatable({}, { __mode = "k" })
methods["UIGridStyleLayout.SetCustomSortFunction"] = function(self, fn)
    if fn ~= nil and type(fn) ~= "function" then error("Unable to cast value to function", 0) end
    sort_fns[self] = fn
end

-------------------------------------------------------------- UIPageLayout
-- Pages are the parent's visible GuiObjects in SortOrder. Jumps set
-- CurrentPage at once; when Animated the view tweens over TweenTime and
-- Stopped(page) fires at its end (not when not animated). JumpToIndex is
-- 0-based and wraps (Studio: 10 of 3 pages is the second).
local pl_state = setmetatable({}, { __mode = "k" })
local function pages_of(pl)
    local parent = pl.Parent
    local list = {}
    if not parent then return list end
    for _, c in parent:GetChildren() do
        if c:IsA("GuiObject") and c.Visible then table.insert(list, c) end
    end
    local idx = {}
    for i, c in list do idx[c] = i end
    if pl.SortOrder == Enum.SortOrder.Name then
        table.sort(list, function(a, b) return a.Name < b.Name end)
    elseif pl.SortOrder == Enum.SortOrder.LayoutOrder then
        table.sort(list, function(a, b)
            if a.LayoutOrder ~= b.LayoutOrder then return a.LayoutOrder < b.LayoutOrder end
            return idx[a] < idx[b]
        end)
    end
    return list
end
local function current_page(pl)
    local st = pl_state[pl]
    local pages = pages_of(pl)
    if st and st.page and table.find(pages, st.page) then return st.page, pages end
    return pages[1], pages
end
get["UIPageLayout.CurrentPage"] = function(pl) return (current_page(pl)) end
local tweening = {}
local function go(pl, page, dir)
    local cur, pages = current_page(pl)
    if page == nil or page == cur then return end
    dir = dir or 0
    local st = pl_state[pl] or {}
    pl_state[pl] = st
    st.page = page
    changed(pl, "CurrentPage")
    if pl.Animated and pl.TweenTime > 0 then
        tweening[pl] = { from = cur, to = page, t = 0, dir = dir, entered = false }
        G.set_page(pl, cur, page, 0, dir)
    else
        tweening[pl] = nil
        G.set_page(pl, page, page, 1, dir)
        -- Studio fires PageLeave(cur) and PageEnter(page) in layout order of
        -- the pages when Animated is false, and does not fire Stopped.
        for _, p in pages do
            if p == cur then
                fire(pl, "PageLeave", cur)
            elseif p == page then
                fire(pl, "PageEnter", page)
            end
        end
    end
end
local function step_pages(dt)
    for _, ev in G.page_scrolls() do
        local pl = ev.layout
        if pl and pl.Parent and pl.ScrollWheelInputEnabled then
            if ev.dir > 0 then
                methods["UIPageLayout.Next"](pl)
            elseif ev.dir < 0 then
                methods["UIPageLayout.Previous"](pl)
            end
        end
    end
    for pl, tw in tweening do
        tw.t += dt
        local dur = math.max(pl.TweenTime, 1e-3)
        local a = math.min(tw.t / dur, 1)
        if not tw.entered and a > 0 then
            tw.entered = true
            fire(pl, "PageEnter", tw.to)
        end
        local ok, alpha = pcall(function() return svc("TweenService"):GetValue(a, pl.EasingStyle, pl.EasingDirection) end)
        G.set_page(pl, tw.from, tw.to, ok and alpha or a, tw.dir)
        if a >= 1 then
            tweening[pl] = nil
            fire(pl, "Stopped", tw.to)
            fire(pl, "PageLeave", tw.from)
        end
    end
end
methods["UIPageLayout.JumpTo"] = function(pl, page)
    if not isa(page, "GuiObject") then
        error((typeof(page) == "Instance" and page.ClassName or typeof(page)) .. " passed to PageLayout JumpTo is not a GuiObject", 0)
    end
    local _, pages = current_page(pl)
    if table.find(pages, page) then go(pl, page, 0) end
end
methods["UIPageLayout.JumpToIndex"] = function(pl, i)
    i = tonumber(i)
    if i == nil then error("Unable to cast value to int", 0) end
    local _, pages = current_page(pl)
    if #pages == 0 then return end
    go(pl, pages[(math.floor(i) % #pages) + 1], 0)
end
local function shift(pl, d)
    local cur, pages = current_page(pl)
    if not cur then return end
    local i = table.find(pages, cur) + d
    if i < 1 or i > #pages then
        if not pl.Circular then return end
        i = (i - 1) % #pages + 1
    end
    go(pl, pages[i], d)
end
methods["UIPageLayout.Next"] = function(pl) shift(pl, 1) end
methods["UIPageLayout.Previous"] = function(pl) shift(pl, -1) end

------------------------------------------------------------ UIDragDetector
local dd_constraints = setmetatable({}, { __mode = "k" })
local dd_style = setmetatable({}, { __mode = "k" })
methods["UIDragDetector.AddConstraintFunction"] = function(self, priority, fn)
    if tonumber(priority) == nil then error("Unable to cast value to int", 0) end
    if type(fn) ~= "function" then error("Unable to cast value to function", 0) end
    -- A real RBXScriptConnection, its Disconnect removing the function.
    local holder = Instance.new("BindableEvent")
    local conn = holder.Event:Connect(fn)
    local list = dd_constraints[self] or {}
    dd_constraints[self] = list
    table.insert(list, { priority = tonumber(priority), fn = fn, conn = conn })
    table.sort(list, function(a, b) return a.priority > b.priority end)
    return conn
end
methods["UIDragDetector.SetDragStyleFunction"] = function(self, fn)
    if fn ~= nil and type(fn) ~= "function" then error("Unable to cast value to function", 0) end
    dd_style[self] = fn
end
methods["UIDragDetector.GetReferencePosition"] = function(self)
    local ref = self.ReferenceUIInstance
    if isa(ref, "GuiObject") then return ref.Position end
    local p = self.Parent
    if isa(p, "GuiObject") then return p.Position end
    return UDim2.new()
end
methods["UIDragDetector.GetReferenceRotation"] = function(self)
    local ref = self.ReferenceUIInstance
    if isa(ref, "GuiObject") then return ref.Rotation end
    local p = self.Parent
    if isa(p, "GuiObject") then return p.Rotation end
    return 0
end

-- Dragging (the viewport reports presses / moves / releases on objects
-- that are Draggable or hold an enabled UIDragDetector). Draggable:
-- DragBegin(Position at the start), the object follows the mouse,
-- DragStopped(x, y). UIDragDetector: DragStart / DragContinue / DragEnd
-- (input position), DragUDim2 / DragRotation, its DragStyle (TranslatePlane,
-- TranslateLine along DragAxis, Rotate, Scriptable through the style
-- function), constraint functions by priority, ResponseStyle (Offset /
-- Scale move the object; Custom* leave it to scripts) and BoundingUI.
local drag = nil
local function detector_of(o)
    for _, c in o:GetChildren() do
        if c:IsA("UIDragDetector") and c.Enabled then return c end
    end
    return nil
end
local function run_constraints(dd, v)
    for _, e in dd_constraints[dd] or {} do
        if e.conn.Connected then
            local ok, r = pcall(e.fn, v)
            if ok and r ~= nil then v = r end
        end
    end
    return v
end
local function drag_step(kind, x, y)
    if kind == 0 then
        local o = drag.obj
        drag.start_pos = o.Position
        drag.start_rot = o.Rotation
        drag.x0, drag.y0 = x, y
        local dd = detector_of(o)
        drag.dd = dd
        if dd then
            fire(dd, "DragStart", Vector2.new(x, y))
        elseif o.Draggable then
            fire(o, "DragBegin", o.Position)
        end
        return
    end
    local o, dd = drag.obj, drag.dd
    if not o.Parent then return end
    local dx, dy = x - drag.x0, y - drag.y0
    if dd then
        local style = dd.DragStyle
        local du, rot = UDim2.fromOffset(dx, dy), 0
        if style == Enum.UIDragDetectorDragStyle.TranslateLine then
            local axis = dd.DragAxis
            local m = axis.Magnitude > 0 and axis.Unit or Vector2.xAxis
            local along = dx * m.X + dy * m.Y
            du = UDim2.fromOffset(m.X * along, m.Y * along)
        elseif style == Enum.UIDragDetectorDragStyle.Rotate then
            local c = o.AbsolutePosition + o.AbsoluteSize / 2
            local a0 = math.atan2(drag.y0 - c.Y, drag.x0 - c.X)
            local a1 = math.atan2(y - c.Y, x - c.X)
            rot = math.deg(a1 - a0)
            du = UDim2.new()
        elseif style == Enum.UIDragDetectorDragStyle.Scriptable then
            local fn = dd_style[dd]
            local ok, r = pcall(function() return fn and fn(Vector2.new(x, y)) end)
            if ok and typeof(r) == "UDim2" then du = r - drag.start_pos
            elseif ok and type(r) == "number" then rot = r - drag.start_rot du = UDim2.new() end
        end
        if style == Enum.UIDragDetectorDragStyle.Rotate or (style == Enum.UIDragDetectorDragStyle.Scriptable and rot ~= 0) then
            rot = run_constraints(dd, rot)
            pcall(function() dd.DragRotation = rot end)
        else
            if dd.ResponseStyle == Enum.UIDragDetectorResponseStyle.Scale or dd.ResponseStyle == Enum.UIDragDetectorResponseStyle.CustomScale then
                local ps = o.Parent:IsA("GuiBase2d") and o.Parent.AbsoluteSize or Vector2.one
                du = UDim2.fromScale(du.X.Offset / math.max(ps.X, 1), du.Y.Offset / math.max(ps.Y, 1))
            end
            du = run_constraints(dd, du)
            pcall(function() dd.DragUDim2 = du end)
        end
        local rs = dd.ResponseStyle
        if rs == Enum.UIDragDetectorResponseStyle.Offset or rs == Enum.UIDragDetectorResponseStyle.Scale then
            if style == Enum.UIDragDetectorDragStyle.Rotate then
                o.Rotation = drag.start_rot + rot
            else
                local np = drag.start_pos + du
                local b = dd.BoundingUI
                if b and b:IsA("GuiObject") then
                    -- Keep the object inside the bounding object.
                    local bp, bs, os = b.AbsolutePosition, b.AbsoluteSize, o.AbsoluteSize
                    local cur = o.AbsolutePosition
                    local want = cur + Vector2.new((np.X.Offset - o.Position.X.Offset), (np.Y.Offset - o.Position.Y.Offset))
                    local cx = math.clamp(want.X, bp.X, bp.X + bs.X - os.X) - want.X
                    local cy = math.clamp(want.Y, bp.Y, bp.Y + bs.Y - os.Y) - want.Y
                    np = np + UDim2.fromOffset(cx, cy)
                end
                o.Position = np
            end
        end
        fire(dd, kind == 2 and "DragEnd" or "DragContinue", Vector2.new(x, y))
    elseif o.Draggable then
        if kind == 1 then
            o.Position = drag.start_pos + UDim2.fromOffset(dx, dy)
        else
            fire(o, "DragStopped", x, y)
        end
    end
end
local function step_drags()
    for _, e in G.drags() do
        if e.kind == 0 then drag = { obj = e.obj } end
        if drag and drag.obj == e.obj then
            local ok, err = pcall(drag_step, e.kind, e.x, e.y)
            if not ok then log("error", tostring(err)) end
            if e.kind == 2 then drag = nil end
        end
    end
    if drag and not G.buttons() then
        local _, _, mx, my = G.view()
        pcall(drag_step, 2, mx, my - 58)
        drag = nil
    end
end

------------------------------------------------------- 3D handle adornments
-- The mouse ray against HandleAdornments (their shape at Adornee * CFrame,
-- SizeRelativeOffset), Handles (a sphere per face at half size + 2 studs
-- out) and ArcHandles (two spheres per axis on a ring of radius |half
-- size| + 2: Z's on ±Y, Y's on ±X, X's on ±Z). The nearest one under the
-- mouse is hovered (parts in front hide those not AlwaysOnTop). Studio's
-- order: MouseEnter, MouseButton1Down, MouseDrag while held (Handles:
-- studs along the face's axis since the press; ArcHandles: angle in
-- radians and radius change on the ring's plane), MouseButton1Up on the
-- pressed one, MouseLeave; hovering keeps changing while dragging.
local HANDLE_R = 0.55
local function adornee_cf(a)
    local ad = a.Adornee
    if isa(ad, "BasePart") then return ad.CFrame, ad.Size end
    if isa(ad, "Model") then
        local ok, cf, size = pcall(function() return ad:GetBoundingBox() end)
        if ok then return cf, size end
    end
    return nil
end
local function ray_sphere(o, d, c, r)
    local oc = o - c
    local b = oc:Dot(d)
    local cc = oc:Dot(oc) - r * r
    local h = b * b - cc
    if h < 0 then return nil end
    local t = -b - math.sqrt(h)
    if t < 0 then t = -b + math.sqrt(h) end
    return t >= 0 and t or nil
end
local function ray_box(o, d, cf, size)
    local lo = cf:PointToObjectSpace(o)
    local ld = cf:VectorToObjectSpace(d)
    local h = size / 2
    local tmin, tmax = -math.huge, math.huge
    for _, ax in { "X", "Y", "Z" } do
        local oo, dd, hh = lo[ax], ld[ax], h[ax]
        if math.abs(dd) < 1e-9 then
            if oo < -hh or oo > hh then return nil end
        else
            local t1, t2 = (-hh - oo) / dd, (hh - oo) / dd
            if t1 > t2 then t1, t2 = t2, t1 end
            tmin, tmax = math.max(tmin, t1), math.min(tmax, t2)
            if tmin > tmax then return nil end
        end
    end
    if tmax < 0 then return nil end
    return math.max(tmin, 0)
end
local FACES = { Right = Vector3.xAxis, Left = -Vector3.xAxis, Top = Vector3.yAxis, Bottom = -Vector3.yAxis, Back = Vector3.zAxis, Front = -Vector3.zAxis }
local ARC_SPOTS = { X = { Vector3.zAxis, -Vector3.zAxis }, Y = { Vector3.xAxis, -Vector3.xAxis }, Z = { Vector3.yAxis, -Vector3.yAxis } }
local AXIS_VEC = { X = Vector3.xAxis, Y = Vector3.yAxis, Z = Vector3.zAxis }
-- Distance along the ray to `a` and which part of it (face / axis name).
local function adorn_hit(a, o, d)
    if not a.Visible then return nil end
    local cf, size = adornee_cf(a)
    if not cf then return nil end
    if a:IsA("Handles") then
        local best, which
        for name, n in FACES do
            if a.Faces[name] then
                local c = cf:PointToWorldSpace(n * (math.abs(n:Dot(size)) / 2 + 2))
                local t = ray_sphere(o, d, c, HANDLE_R)
                if t and (not best or t < best) then best, which = t, name end
            end
        end
        return best, which
    elseif a:IsA("ArcHandles") then
        local radius = (size / 2).Magnitude + 2
        local best, which
        for name, spots in ARC_SPOTS do
            if a.Axes[name] then
                for _, n in spots do
                    local t = ray_sphere(o, d, cf:PointToWorldSpace(n * radius), HANDLE_R)
                    if t and (not best or t < best) then best, which = t, name end
                end
            end
        end
        return best, which
    end
    local base = cf * CFrame.new(size * a.SizeRelativeOffset / 2) * a.CFrame
    if a:IsA("SphereHandleAdornment") then return ray_sphere(o, d, base.Position, a.Radius) end
    if a:IsA("BoxHandleAdornment") or a:IsA("ImageHandleAdornment") then
        local s = a.Size
        if a:IsA("ImageHandleAdornment") then s = Vector3.new(s.X, s.Y, 0.05) end
        return ray_box(o, d, base, s)
    end
    if a:IsA("CylinderHandleAdornment") or a:IsA("ConeHandleAdornment") then
        return ray_box(o, d, base, Vector3.new(a.Radius * 2, a.Radius * 2, a.Height))
    end
    if a:IsA("PyramidHandleAdornment") then
        return ray_box(o, d, base * CFrame.new(0, 0, -a.Height / 2), Vector3.new(a.Size, a.Size, a.Height))
    end
    if a:IsA("LineHandleAdornment") then
        return ray_box(o, d, base * CFrame.new(0, 0, -a.Length / 2), Vector3.new(0.2, 0.2, a.Length))
    end
    return nil
end
local hover_a, hover_part = nil, nil
local press = nil
local function enum_of(a, part)
    if part == nil then return nil end
    if a:IsA("Handles") then return Enum.NormalId[part] end
    return Enum.Axis[part]
end
local function adorn_fire(a, ev, part, ...)
    if a:IsA("HandleAdornment") then fire(a, ev) else fire(a, ev, enum_of(a, part), ...) end
end
-- Where the ray passes the axis line through p along n: studs along n.
local function along_axis(o, d, p, n)
    local w = o - p
    local b = d:Dot(n)
    local denom = 1 - b * b
    if denom < 1e-6 then return nil end
    return (w:Dot(n) - b * w:Dot(d)) / denom
end
local function ring_point(o, d, c, n)
    local dn = d:Dot(n)
    if math.abs(dn) < 1e-6 then return nil end
    local t = (c - o):Dot(n) / dn
    if t < 0 then return nil end
    return o + d * t
end
local function step_adorns()
    if next(tracked.Adorn) == nil then
        if hover_a or press then G.set_adorn_hot(false) end
        hover_a = nil press = nil
        return
    end
    local o, d = G.mouse_ray()
    if d.Magnitude < 0.5 then return end
    local best, ba, bp
    for a in tracked.Adorn do
        if a.Parent then
            local ok, t, part = pcall(adorn_hit, a, o, d)
            if ok and t and (not best or t < best) then
                local hidden = false
                if not (a:IsA("HandlesBase") or a.AlwaysOnTop) then
                    local params = RaycastParams.new()
                    params.FilterType = Enum.RaycastFilterType.Exclude
                    local ex = {}
                    local p = local_player()
                    if p and p.Character then table.insert(ex, p.Character) end
                    if a.Adornee then table.insert(ex, a.Adornee) end
                    params.FilterDescendantsInstances = ex
                    local r = workspace:Raycast(o, d * t, params)
                    hidden = r ~= nil and r.Distance < t - 0.01
                end
                if not hidden then best, ba, bp = t, a, part end
            end
        end
    end
    if ba ~= hover_a or bp ~= hover_part then
        if hover_a and hover_a.Parent then adorn_fire(hover_a, "MouseLeave", hover_part) end
        hover_a, hover_part = ba, bp
        if ba then adorn_fire(ba, "MouseEnter", bp) end
    end
    local function begin()
        local a, part = hover_a, hover_part
        press = { a = a, part = part }
        local cf, size = adornee_cf(a)
        if cf and a:IsA("Handles") then
            local n = cf:VectorToWorldSpace(FACES[part])
            press.p, press.n = cf.Position, n
            press.t0 = along_axis(o, d, cf.Position, n)
        elseif cf and a:IsA("ArcHandles") then
            local n = cf:VectorToWorldSpace(AXIS_VEC[part])
            press.c, press.n = cf.Position, n
            press.radius = (size / 2).Magnitude + 2
            local q = ring_point(o, d, cf.Position, n)
            press.v0 = q and (q - cf.Position)
        end
        adorn_fire(a, "MouseButton1Down", part)
    end
    local function finish()
        if press.a.Parent then adorn_fire(press.a, "MouseButton1Up", press.part) end
        press = nil
    end
    for _, e in G.mouse_edges() do
        if e.button == 1 then
            if e.down and not e.gui and hover_a and not press then begin()
            elseif not e.down and press then finish() end
        end
    end
    if press and G.buttons() then
        local a = press.a
        if a.Parent and press.n then
            if press.t0 then
                local t = along_axis(o, d, press.p, press.n)
                if t and t ~= press.last then
                    press.last = t
                    adorn_fire(a, "MouseDrag", press.part, t - press.t0)
                end
            elseif press.v0 then
                local q = ring_point(o, d, press.c, press.n)
                if q then
                    local v = q - press.c
                    local key = v.X .. "," .. v.Y .. "," .. v.Z
                    if key ~= press.last then
                        press.last = key
                        local ang = math.atan2(press.v0:Cross(v):Dot(press.n), press.v0:Dot(v))
                        adorn_fire(a, "MouseDrag", press.part, ang, v.Magnitude - press.radius)
                    end
                end
            end
        end
    elseif press and not G.buttons() then
        finish()
    end
    local held = (press and press.a) or hover_a
    G.set_adorn_hot(hover_a ~= nil or press ~= nil, held ~= nil and held:IsA("HandlesBase"))
end

-------------------------------------------------------------------- Styles
-- StyleRule properties / transitions, StyleSheet derives, StyleQuery
-- conditions, and the engine applying them: every StyleLink's sheet styles
-- the descendants of the link's parent (rules by Priority, later wins;
-- "$token" values are the sheet's attributes; a property no rule sets any
-- more gets back the value it had).
-- Strong tables (pruned on tree mutations in prune()): inst_value interns
-- userdata in a weak-value table, so __mode = "k" would let GC wipe a live
-- StyleRule's properties as soon as Lua drops its local reference!
local rule_props = {}
local rule_trans = {}
local sheet_derives = {}
local query_conds = {}
local pseudo_kids = {} -- parent_inst → { [ClassName] = child_inst }
local pseudo_prefixes = {} -- alt → prefix step array (never mutate alt!)
local function props_of(r)
    local t = rule_props[r]
    if not t then t = {} rule_props[r] = t end
    return t
end
local function copy(t)
    local o = {}
    for k, v in t do o[k] = v end
    return o
end
methods["StyleRule.GetProperties"] = function(r) return copy(props_of(r)) end
methods["StyleRule.GetProperty"] = function(r, name) return props_of(r)[tostring(name)] end
methods["StyleRule.SetProperty"] = function(r, name, v)
    if type(name) ~= "string" then error("Unable to cast value to std::string", 0) end
    props_of(r)[name] = v
    styles_dirty = true
end
methods["StyleRule.SetProperties"] = function(r, t)
    if type(t) ~= "table" then error("Unable to cast value to Dictionary", 0) end
    local p = {}
    for k, v in t do p[tostring(k)] = v end
    rule_props[r] = p
    styles_dirty = true
end
local function trans_of(r)
    local t = rule_trans[r]
    if not t then t = {} rule_trans[r] = t end
    return t
end
local function check_tween(v)
    if v == nil or typeof(v) == "TweenInfo" then return true end
    local tn = ({ number = "double", string = "string", boolean = "bool" })[type(v)] or typeof(v)
    log("warn", "Type `" .. tn .. "` is not supported as a StyleRule property transition")
    return false
end
methods["StyleRule.GetDefaultPropertyTransition"] = function(r) return trans_of(r)["*"] end
methods["StyleRule.SetDefaultPropertyTransition"] = function(r, v)
    if check_tween(v) then trans_of(r)["*"] = v end
end
methods["StyleRule.GetPropertyTransitions"] = function(r) return copy(trans_of(r)) end
methods["StyleRule.SetPropertyTransition"] = function(r, name, v)
    if check_tween(v) then trans_of(r)[tostring(name)] = v end
end
methods["StyleRule.SetPropertyTransitions"] = function(r, t)
    if type(t) ~= "table" then error("Unable to cast value to Dictionary", 0) end
    local o = {}
    for k, v in t do
        if check_tween(v) then o[tostring(k)] = v end
    end
    rule_trans[r] = o
end
get["StyleRule.SelectorError"] = function() return "" end

local function rules_of(base)
    local list, idx = {}, {}
    for i, c in base:GetChildren() do
        if c:IsA("StyleRule") then table.insert(list, c) idx[c] = i end
    end
    table.sort(list, function(a, b)
        if a.Priority ~= b.Priority then return a.Priority < b.Priority end
        return idx[a] < idx[b]
    end)
    return list
end
methods["StyleBase.GetStyleRules"] = function(base) return rules_of(base) end
methods["StyleBase.InsertStyleRule"] = function(base, rule, priority)
    if not isa(rule, "StyleRule") then error("Unable to cast value to StyleRule", 0) end
    if priority ~= nil then rule.Priority = tonumber(priority) or 0 end
    rule.Parent = base
    styles_dirty = true
    fire(base, "StyleRulesChanged")
end
methods["StyleBase.SetStyleRules"] = function(base, rules)
    if type(rules) ~= "table" then error("Unable to cast value to Objects", 0) end
    for i, r in rules do
        if not isa(r, "StyleRule") then error("Expected StyleRule at index " .. (i - 1) .. ", got " .. (typeof(r) == "Instance" and r.ClassName or typeof(r)), 0) end
    end
    local existing = rules_of(base)
    if #existing > 0 then log("warn", "Unparenting existing styles in SetStyles().") end
    for _, r in existing do r.Parent = nil end
    for _, r in rules do r.Parent = base end
    styles_dirty = true
    fire(base, "StyleRulesChanged")
end
methods["StyleSheet.GetDerives"] = function(s) return copy(sheet_derives[s] or {}) end
methods["StyleSheet.SetDerives"] = function(s, list)
    if type(list) ~= "table" then error("Unable to cast value to Objects", 0) end
    for i, d in list do
        if not isa(d, "StyleSheet") then
            error("Expected StyleSheet at index " .. (i - 1) .. ", got " .. (typeof(d) == "Instance" and d.ClassName or typeof(d)), 0)
        end
    end
    sheet_derives[s] = copy(list)
    styles_dirty = true
end
local function conds_of(q)
    local t = query_conds[q]
    if not t then t = {} query_conds[q] = t end
    return t
end
methods["StyleQuery.GetCondition"] = function(q, name) return conds_of(q)[tostring(name)] end
methods["StyleQuery.GetConditions"] = function(q) return copy(conds_of(q)) end
methods["StyleQuery.SetCondition"] = function(q, name, v)
    conds_of(q)[tostring(name)] = v
    styles_dirty = true
end
methods["StyleQuery.SetConditions"] = function(q, t)
    if type(t) ~= "table" then error("Unable to cast value to Dictionary", 0) end
    local o = {}
    for k, v in t do o[tostring(k)] = v end
    query_conds[q] = o
    styles_dirty = true
end
-- Active when it is in a styled tree and its size conditions hold for the
-- screen (MinSize / MaxSize / AspectRatioRange).
local function query_active(q, screen)
    if not screen then return false end
    local any = false
    for k, v in conds_of(q) do
        any = true
        if k == "MinSize" and typeof(v) == "Vector2" then
            if screen.X < v.X or screen.Y < v.Y then return false end
        elseif k == "MaxSize" and typeof(v) == "Vector2" then
            if screen.X > v.X or screen.Y > v.Y then return false end
        elseif k == "AspectRatioRange" and typeof(v) == "NumberRange" then
            local ar = screen.X / math.max(screen.Y, 1)
            if ar < v.Min or ar > v.Max then return false end
        elseif k == "ReducedMotionEnabled" then
            if v ~= (get["GuiService.ReducedMotionEnabled"]() == true) then return false end
        end
    end
    return any
end
get["StyleQuery.IsActive"] = function(q)
    local sheet = q:FindFirstAncestorWhichIsA("StyleSheet")
    if not sheet or not G.is_client() then return false end
    local w, h = G.view()
    return query_active(q, Vector2.new(w, h - 58))
end

-- Selectors: "A, B" lists of compounds joined by descendant (space) or
-- child (">") steps; a compound is ClassName, .tag, #Name and :state
-- (GuiState: hover, press, focus, nonInteractable), in any combination.
local selector_cache = {}
local function parse_selector(s)
    if selector_cache[s] then return selector_cache[s] end
    local alts = {}
    for alt in string.gmatch(s, "[^,]+") do
        local steps = {}
        local rel = "desc"
        alt = alt:gsub("::", " :: "):gsub(">", " > ")
        for tok in string.gmatch(alt, "%S+") do
            if tok == ">" then
                rel = "child"
            elseif tok == "::" then
                rel = "pseudo"
            elseif tok == "&" then
                rel = "self"
            else
                local c = { rel = rel, tags = {}, states = {} }
                local rest = tok
                if rest:sub(1, 1) == "&" then c.rel = "self" rest = rest:sub(2) end
                c.class = rest:match("^([%a_][%w_]*)")
                for kind, name in string.gmatch(rest, "([%.#:])([%w_%-]+)") do
                    if kind == "." then table.insert(c.tags, name)
                    elseif kind == "#" then c.name = name
                    else table.insert(c.states, name:lower()) end
                end
                table.insert(steps, c)
                rel = "desc"
            end
        end
        if #steps > 0 then table.insert(alts, steps) end
    end
    selector_cache[s] = alts
    return alts
end
-- Per-restyle caches of native reads (ClassName, Name, tags, Parent) for the
-- styleable nodes, so matching a sheet against a big CoreGui tree does table
-- lookups instead of tens of thousands of IsA / HasTag / Parent calls.
-- `sx` is nil outside step_styles.
local NO_TOKEN = {}
local sx = nil -- { cls = {}, name = {}, tags = {}, par = {}, inroot = {} }
local isa_cache = {} -- ClassName → { class → bool } (IsA depends only on the class)
local function isa_fast(o, cls)
    local cn = sx and sx.cls[o]
    if not cn then return o:IsA(cls) end
    local m = isa_cache[cn]
    if not m then m = {} isa_cache[cn] = m end
    local v = m[cls]
    if v == nil then v = o:IsA(cls) m[cls] = v end
    return v
end
local function parent_fast(o)
    if not sx then return o.Parent end
    local p = sx.par[o]
    if p == nil then p = o.Parent or false sx.par[o] = p end
    return p or nil
end
local function compound_matches(c, o)
    if c.class and not isa_fast(o, c.class) then return false end
    local ts = sx and sx.tags[o]
    if c.name then
        local nm = if ts then sx.name[o] else o.Name
        if nm ~= c.name then return false end
    end
    if ts then
        for _, t in c.tags do
            if not ts[t] then return false end
        end
    else
        for _, t in c.tags do
            if not o:HasTag(t) then return false end
        end
    end
    if #c.states > 0 then
        if not isa_fast(o, "GuiObject") then return false end
        local st = get["GuiObject.GuiState"](o).Name:lower()
        for _, s in c.states do
            if s == "focus" then
                if focused ~= o and selected ~= o and gs_state.selected_core ~= o then return false end
            elseif s ~= st then
                return false
            end
        end
    end
    return true
end
-- Does `steps` (right-most last) match o, with every step inside `root`?
-- `scope`: the instances a nested rule's parent matched (nil: the root).
local function steps_match(steps, o, root, scopes)
    local root_parent = parent_fast(root)
    local inroot = sx and sx.inroot
    local function within(p)
        if p == root or (inroot and inroot[p]) then return true end
        return p:IsDescendantOf(root)
    end
    local function from(i, node)
        local c = steps[i]
        if not compound_matches(c, node) then return false end
        if i == 1 then
            if scopes then
                if c.rel == "self" then return scopes[node] == true end
                local p = parent_fast(node)
                if c.rel == "child" then return p ~= nil and scopes[p] == true end
                while p and p ~= root_parent do
                    if scopes[p] then return true end
                    p = parent_fast(p)
                end
                return false
            end
            if c.rel == "child" then return parent_fast(node) == root end
            return within(node)
        end
        local p = parent_fast(node)
        if c.rel == "child" then return p ~= nil and from(i - 1, p) end
        while p and within(p) do
            if from(i - 1, p) then return true end
            p = parent_fast(p)
        end
        return false
    end
    return from(#steps, o)
end
local styled = {} -- inst → { prop → original value }
local style_targets = {} -- inst → { prop → value } last applied
local style_overridden = setmetatable({}, { __mode = "k" }) -- inst → { prop → last script override }
local step_styles_defaults = {} -- ClassName → { _inst, prop → { default } }
local function resolve_token(v, sheets)
    if type(v) == "string" and v:sub(1, 1) == "$" then
        local name = v:sub(2)
        for _, s in sheets do
            local a = s:GetAttribute(name)
            if a ~= nil then return a end
        end
        return nil
    end
    return v
end
local function collect_rules(sheet, out, sheets_out, seen)
    if seen[sheet] then return end
    seen[sheet] = true
    table.insert(sheets_out, sheet)
    for _, d in sheet_derives[sheet] or {} do collect_rules(d, out, sheets_out, seen) end
    for _, d in sheet:GetChildren() do
        if d:IsA("StyleDerive") and d.StyleSheet then collect_rules(d.StyleSheet, out, sheets_out, seen) end
    end
    local function add(base, parent_rule, depth)
        for _, r in rules_of(base) do
            table.insert(out, { rule = r, parent = parent_rule, depth = depth })
            add(r, r, depth + 1)
        end
        for _, c in base:GetChildren() do
            if c:IsA("StyleQuery") then
                add(c, parent_rule, depth)
            end
        end
    end
    add(sheet, nil, 0)
end
local last_style_rev, last_style_w, last_style_h, last_hover_sig = -1, -1, -1, -1
local styles_need_hover = false
local watched_sheets = {}
local function hidden_plugin_gui(inst)
    if not G.is_edit() then return false end
    local cur = inst
    while cur do
        if cur:IsA("PluginGui") then return not cur.Enabled end
        cur = cur.Parent
    end
    return false
end
local function style_watch_roots(links)
    local roots = {}
    for link in links do
        table.insert(roots, link)
        if link.Parent and not hidden_plugin_gui(link) then
            table.insert(roots, link.Parent)
        end
    end
    for sheet in watched_sheets do table.insert(roots, sheet) end
    return roots
end
local function step_styles(style_rev, w, h, hover_sig)
    local links = tracked.StyleLink
    if next(links) == nil and next(styled) == nil then
        if next(watched_sheets) then
            table.clear(watched_sheets)
            G.style_watch_reset({})
        end
        styles_dirty = false
        return
    end
    if style_recheck_frames > 0 then
        style_recheck_frames -= 1
        for inst, props in style_targets do
            if not pcall(function() return inst.Parent end) then continue end
            for k, last_val in props do
                local ok_c, cur = pcall(function() return inst[k] end)
                if ok_c then
                    local ok_eq, same = pcall(function() return cur == last_val end)
                    if not (ok_eq and same) then
                        styles_dirty = true
                        break
                    end
                end
            end
            if styles_dirty then break end
        end
        if not styles_dirty then
            for inst, props in style_overridden do
                if not pcall(function() return inst.Parent end) then continue end
                for k, ov_val in props do
                    local ok_c, cur = pcall(function() return inst[k] end)
                    if ok_c then
                        local ok_eq, same = pcall(function() return cur == ov_val end)
                        if not (ok_eq and same) then
                            styles_dirty = true
                            break
                        end
                    end
                end
                if styles_dirty then break end
            end
        end
    end
    local need = styles_dirty
        or w ~= last_style_w
        or h ~= last_style_h
        or (styles_need_hover and hover_sig ~= last_hover_sig)
    if not need then
        if style_rev == last_style_rev then return end
        if not G.style_watch_changed() then
            last_style_rev = style_rev
            return
        end
    end
    styles_dirty = false
    last_style_rev, last_style_w, last_style_h, last_hover_sig = style_rev, w, h, hover_sig
    styles_need_hover = false
    local want = {} -- inst → { prop → { value, transition } }
    table.clear(watched_sheets)
    local pseudo_wanted = {} -- parent → { [cls] = true }
    local pseudo_mutated = false
    local function get_or_make_pseudo(parent, cls)
        local map = pseudo_kids[parent]
        local pk = map and map[cls]
        if pk then
            local ok_p, pp = pcall(function() return pk.Parent end)
            if ok_p and pp == parent then
                pseudo_wanted[parent] = pseudo_wanted[parent] or {}
                pseudo_wanted[parent][cls] = true
                return pk
            end
            map[cls] = nil
        end
        local existing = parent:FindFirstChildOfClass(cls)
        if existing then return existing end
        local ok_n, inst = pcall(Instance.new, cls)
        if not ok_n or not inst then return nil end
        pcall(function()
            inst.Name = "__rs_pseudo_" .. cls
            inst.Archivable = false
            inst.Parent = parent
        end)
        pseudo_kids[parent] = pseudo_kids[parent] or {}
        pseudo_kids[parent][cls] = inst
        pseudo_wanted[parent] = pseudo_wanted[parent] or {}
        pseudo_wanted[parent][cls] = true
        pseudo_mutated = true
        return inst
    end
    local screen = Vector2.new(w, h - 58)
    sx = { cls = {}, name = {}, tags = {}, par = {}, inroot = nil }
    -- Rules per sheet, read once per restyle (CoreScripts link one sheet
    -- from ~20 places).
    local sheet_memo = {}
    local function sheet_rules(sheet)
        local mm = sheet_memo[sheet]
        if mm then return mm end
        local rules, sheets = {}, {}
        G.profile_step(3, collect_rules, sheet, rules, sheets, {})
        local anchored = false
        for i, e in rules do
            local r = e.rule
            e.seq = i
            e.sel = r.Selector or ""
            e.query = r:FindFirstAncestorWhichIsA("StyleQuery")
            e.priority = r.Priority
            if e.parent == nil and e.sel ~= "" then
                for _, alt in parse_selector(e.sel) do
                    if alt[1].rel ~= "desc" then anchored = true end
                end
            end
        end
        mm = { rules = rules, sheets = sheets, anchored = anchored }
        sheet_memo[sheet] = mm
        return mm
    end
    -- A link is redundant when another link to the same sheet styles the
    -- same parent or an ancestor: that pass matches this subtree too and
    -- sets the same values (unless a top-level selector is anchored to the
    -- root with ">" or "&").
    local active_links = {}
    for link in links do
        local sheet, root = link.StyleSheet, link.Parent
        if sheet and root and not hidden_plugin_gui(link) and link:IsDescendantOf(game) then
            table.insert(active_links, { link = link, sheet = sheet, root = root })
        end
    end
    for i, a in active_links do
        if sheet_rules(a.sheet).anchored then continue end
        for j, b in active_links do
            if i ~= j and b.sheet == a.sheet then
                if (b.root == a.root and j < i) or (b.root ~= a.root and a.root:IsDescendantOf(b.root)) then
                    a.skip = true
                    break
                end
            end
        end
    end
    for _, al in active_links do
        local sheet, root = al.sheet, al.root
        local mm = sheet_rules(sheet)
        for _, item in mm.sheets do watched_sheets[item] = true end
        if not al.skip then
            local rules, sheets = mm.rules, mm.sheets
            local nodes, n_cls, n_name, n_tags = G.style_index(root)
            local by_tag, by_name, by_cls = {}, {}, {}
            local inroot = {}
            sx.inroot = inroot
            G.profile_step(4, function()
            local s_cls, s_name, s_tags = sx.cls, sx.name, sx.tags
            for i, n in nodes do
                inroot[n] = true
                local cn, nm = n_cls[i], n_name[i]
                s_cls[n] = cn
                s_name[n] = nm
                s_tags[n] = n_tags[i]
                local ln = by_name[nm]
                if ln then table.insert(ln, n) else by_name[nm] = { n } end
                local lc = by_cls[cn]
                if lc then table.insert(lc, n) else by_cls[cn] = { n } end
                for tg in s_tags[n] do
                    local lt = by_tag[tg]
                    if lt then table.insert(lt, n) else by_tag[tg] = { n } end
                end
            end
            end)
            -- Candidates for a compound: by #Name, else the rarest .tag,
            -- else every node whose class IsA the compound's class.
            local cls_cands = {}
            local function cands_for(st)
                if st.name then return by_name[st.name] end
                if st.tags[1] then
                    local best
                    for _, tg in st.tags do
                        local lt = by_tag[tg]
                        if not lt then return nil end
                        if not best or #lt < #best then best = lt end
                    end
                    return best
                end
                if st.class then
                    local got = cls_cands[st.class]
                    if not got then
                        got = {}
                        for _, lc in by_cls do
                            if isa_fast(lc[1], st.class) then
                                for _, n in lc do table.insert(got, n) end
                            end
                        end
                        cls_cands[st.class] = got
                    end
                    return got
                end
                return nodes
            end
            -- Matches per rule (nested rules match inside their parent's).
            local matched = {}
            local order = table.clone(rules)
            G.profile_step(5, function()
            for _, e in order do
                local r = e.rule
                local active = true
                local q = e.query
                if q and not query_active(q, screen) then active = false end
                local m = {}
                local selector = active and e.sel
                if selector and selector ~= "" then
                    local scopes = e.parent and matched[e.parent] or nil
                    if e.parent == nil or (scopes and next(scopes) ~= nil) then
                        for _, alt in parse_selector(selector) do
                            for _, st in alt do
                                if #st.states > 0 then styles_need_hover = true end
                            end
                            local last = alt[#alt]
                            if last.rel == "pseudo" and last.class then
                                if #alt == 1 then
                                    if scopes then
                                        for n in scopes do
                                            if isa_fast(n, "GuiBase2d") then
                                                local pk = get_or_make_pseudo(n, last.class)
                                                if pk then m[pk] = true end
                                            end
                                        end
                                    end
                                else
                                    local prefix = pseudo_prefixes[alt]
                                    if not prefix then
                                        prefix = {}
                                        for pi = 1, #alt - 1 do prefix[pi] = alt[pi] end
                                        pseudo_prefixes[alt] = prefix
                                    end
                                    local cands = cands_for(prefix[#prefix])
                                    if cands then
                                        for _, n in cands do
                                            if isa_fast(n, "GuiBase2d") and steps_match(prefix, n, root, scopes) then
                                                local pk = get_or_make_pseudo(n, last.class)
                                                if pk then m[pk] = true end
                                            end
                                        end
                                    end
                                end
                            else
                                local cands = cands_for(last)
                                if cands then
                                    for _, n in cands do
                                        if not m[n] and steps_match(alt, n, root, scopes) then m[n] = true end
                                    end
                                end
                            end
                        end
                    end
                end
                matched[r] = m
            end
            end)
            table.sort(order, function(a, b)
                if a.priority ~= b.priority then return a.priority < b.priority end
                return a.seq < b.seq
            end)
            local tok = mm.tok
            if not tok then tok = {} mm.tok = tok end
            G.profile_step(6, function()
            for _, e in order do
                local r = e.rule
                local props = rule_props[r]
                if props and next(props) then
                    local tr = rule_trans[r] or {}
                    for n in matched[r] do
                        local t = want[n] or {}
                        want[n] = t
                        for k, v in props do
                            local val = v
                            if type(v) == "string" and string.sub(v, 1, 1) == "$" then
                                val = tok[v]
                                if val == nil then
                                    val = resolve_token(v, sheets)
                                    if val == nil then val = NO_TOKEN end
                                    tok[v] = val
                                end
                                if val == NO_TOKEN then val = nil end
                            end
                            if val ~= nil then t[k] = { val, tr[k] or tr["*"] } end
                        end
                    end
                end
            end
            end)
        end
    end
    sx = nil
    -- Cache per-class defaults so StyleRules never overwrite explicitly set
    -- instance properties (measured in Studio 0.747: direct property writes
    -- always take precedence over StyleRule properties).
    local class_defaults = step_styles_defaults
    local function is_default_prop(inst, k, cur)
        local cn = inst.ClassName
        local cd = class_defaults[cn]
        if cd == nil then
            local ok_n, dummy = pcall(Instance.new, cn)
            cd = ok_n and { _inst = dummy } or false
            class_defaults[cn] = cd
        end
        if not cd then return false end
        local dv = cd[k]
        if dv == nil then
            local ok_d, v = pcall(function() return cd._inst[k] end)
            dv = ok_d and { v } or false
            cd[k] = dv
        end
        if not dv then return false end
        local ok_eq, eq = pcall(function() return cur == dv[1] end)
        return ok_eq and eq
    end
    -- Apply, remembering originals; restore what nothing sets any more.
    for inst, props in want do
        local orig = styled[inst] or {}
        styled[inst] = orig
        local last = style_targets[inst] or {}
        style_targets[inst] = last
        for k, e in props do
            local ok_c, cur = pcall(function() return inst[k] end)
            if ok_c then
                if orig[k] == nil then
                    -- Only style properties still at their class default.
                    if is_default_prop(inst, k, cur) then
                        orig[k] = { cur }
                        if style_overridden[inst] then style_overridden[inst][k] = nil end
                    else
                        local ov = style_overridden[inst] or {}
                        style_overridden[inst] = ov
                        ov[k] = cur
                    end
                elseif last[k] ~= nil then
                    -- If a script explicitly wrote a different value, yield to it;
                    -- if it reset the property back to its class default (e.g. React
                    -- clearing a hover prop), re-apply the StyleSheet value.
                    local ok_eq, same = pcall(function() return cur == last[k] end)
                    if not (ok_eq and same) then
                        if is_default_prop(inst, k, cur) then
                            orig[k] = { cur }
                            last[k] = nil
                            if style_overridden[inst] then style_overridden[inst][k] = nil end
                        else
                            orig[k] = nil
                            last[k] = nil
                            local ov = style_overridden[inst] or {}
                            style_overridden[inst] = ov
                            ov[k] = cur
                        end
                    end
                end
            end
            if orig[k] and last[k] ~= e[1] then
                last[k] = e[1]
                if e[2] then
                    pcall(function() svc("TweenService"):Create(inst, e[2], { [k] = e[1] }):Play() end)
                else
                    pcall(function() inst[k] = e[1] end)
                end
            end
        end
    end
    for inst, orig in styled do
        local props = want[inst] or {}
        for k, o in orig do
            if props[k] == nil then
                if inst.Parent then pcall(function() inst[k] = o[1] end) end
                orig[k] = nil
                if style_targets[inst] then style_targets[inst][k] = nil end
            end
        end
        if next(orig) == nil then
            styled[inst] = nil
            style_targets[inst] = nil
        end
    end
    -- Clean up pseudo-children no longer matched by any ::ClassName rule.
    for parent, map in pseudo_kids do
        local keep = pseudo_wanted[parent]
        for cls, pk in map do
            if not (keep and keep[cls]) then
                pcall(function() pk:Destroy() end)
                map[cls] = nil
                styled[pk] = nil
                style_targets[pk] = nil
                pseudo_mutated = true
            end
        end
        if next(map) == nil then pseudo_kids[parent] = nil end
    end
    local _, new_srev = G.style_state()
    if pseudo_mutated then
        last_style_rev = new_srev
    elseif new_srev ~= last_style_rev then
        styles_dirty = true
    end
    G.style_watch_reset(style_watch_roots(links))
end

---------------------------------------------------------- ParticleEmitter2D
-- Emit(n) adds n particles now, Clear() removes them (drawn by
-- src/gui_fx.rs, which also runs Rate).
methods["ParticleEmitter2D.Emit"] = function(self, n)
    n = n == nil and 16 or tonumber(n)
    if n == nil then error("Unable to cast value to int", 0) end
    if n >= 1 then G.p2d(self, math.floor(n)) end
end
methods["ParticleEmitter2D.Clear"] = function(self) G.p2d(self, 0) end

-------------------------------------------------------- Input action system
-- InputContext (Enabled, Priority, Sink) > InputAction (Type, Enabled) >
-- InputBinding (KeyCode or Up/Down/Left/Right/Forward/Backward, modifiers,
-- Scale). An action's state starts at its type's zero; Bool actions fire
-- Pressed / Released, every change fires StateChanged(state).
local action_state = setmetatable({}, { __mode = "k" })
local action_type = setmetatable({}, { __mode = "k" })
local function zero_of(t)
    if t == Enum.InputActionType.Bool then return false end
    if t == Enum.InputActionType.Direction1D then return 0 end
    if t == Enum.InputActionType.Direction2D then return Vector2.zero end
    if t == Enum.InputActionType.Direction3D then return Vector3.zero end
    if t == Enum.InputActionType.ViewportPosition then return Vector2.new(-1, -1) end
    return false
end
local function state_of(a)
    local s = action_state[a]
    if s == nil then return zero_of(a.Type) end
    return s
end
local function action_live(a)
    local ctx = a.Parent
    return a.Enabled and isa(ctx, "InputContext") and ctx.Enabled and a:IsDescendantOf(game)
end
local function set_state(a, v)
    local old = state_of(a)
    if old == v then return end
    action_state[a] = v
    if a.Type == Enum.InputActionType.Bool then
        fire(a, v and "Pressed" or "Released")
    end
    fire(a, "StateChanged", v)
end
methods["InputAction.GetState"] = function(a)
    if not action_live(a) then return zero_of(a.Type) end
    return state_of(a)
end
methods["InputAction.Fire"] = function(a, v)
    local t = a.Type
    if t == Enum.InputActionType.Bool and type(v) ~= "boolean" then
        error("Action:Fire() on InputActionType.Bool should be called with a bool state", 0)
    end
    if not action_live(a) then return end
    local ok = (t == Enum.InputActionType.Bool and type(v) == "boolean")
        or (t == Enum.InputActionType.Direction1D and type(v) == "number")
        or ((t == Enum.InputActionType.Direction2D or t == Enum.InputActionType.ViewportPosition) and typeof(v) == "Vector2")
        or (t == Enum.InputActionType.Direction3D and typeof(v) == "Vector3")
    if ok then set_state(a, v) end
end
methods["InputBinding.Fire"] = function(b, v)
    if b.Type ~= Enum.InputBindingType.Scriptable then
        error("InputBinding:Fire() can only be called when Type is Scriptable", 0)
    end
    local a = b.Parent
    if isa(a, "InputAction") then methods["InputAction.Fire"](a, v) end
end
-- Type changes reset the state to the new type's zero (Released first when
-- a Bool action takes over from a non-zero state).
set["InputAction.Type"] = function(a, _, v)
    local before_t = a.Type
    local before = state_of(a)
    if typeof(v) ~= "EnumItem" then return false end
    if v == before_t then return false end
    __rs_set_raw(a, "Type", v)
    action_state[a] = nil
    local z = zero_of(v)
    if before ~= z then
        if v == Enum.InputActionType.Bool and before ~= zero_of(before_t) then fire(a, "Released") end
        fire(a, "StateChanged", z)
    end
    return true
end
local function reset(a)
    local z = zero_of(a.Type)
    if state_of(a) ~= z then set_state(a, z) end
end
set["InputAction.Enabled"] = function(a, _, v)
    if v == false then reset(a) end
    return false
end
set["InputContext.Enabled"] = function(ctx, _, v)
    if v == false then
        for _, a in ctx:GetChildren() do
            if a:IsA("InputAction") then reset(a) end
        end
    end
    return false
end
local last_device = "Keyboard"
local function binding_device(b)
    local k = b.KeyCode
    if k ~= KEY_NONE and k.Name:find("^Button") or k.Name:find("^Thumbstick") or k.Name:find("^DPad") then return "Gamepad" end
    return "Keyboard"
end
get["InputAction.PreferredBinding"] = function(a)
    local first
    for _, b in a:GetChildren() do
        if b:IsA("InputBinding") then
            first = first or b
            if binding_device(b) == last_device then return b end
        end
    end
    return first
end
get["InputActionLabel.ResolvedText"] = function() return "" end
get["InputActionLabel.ResolvedImageContent"] = function() return Content.none end
-- Held keys / buttons per binding, for the action's value.
local held = {}
local pad_values = {}
local sticks = {}
local function mods_ok(b)
    local uis = svc("UserInputService")
    for _, m in { b.PrimaryModifier, b.SecondaryModifier } do
        if m ~= KEY_NONE and not uis:IsKeyDown(m) then return false end
    end
    return true
end
local function binding_value(a, b)
    local t = a.Type
    local s = b.Scale
    local function h(k) return k ~= KEY_NONE and held[k] == true end
    if t == Enum.InputActionType.Bool then
        return h(b.KeyCode) and mods_ok(b)
    elseif t == Enum.InputActionType.Direction1D then
        local v = 0
        if h(b.KeyCode) then v = 1 end
        if h(b.Up) then v += 1 end
        if h(b.Down) then v -= 1 end
        return v * s
    elseif t == Enum.InputActionType.Direction2D then
        local v = Vector2.new((h(b.Right) and 1 or 0) - (h(b.Left) and 1 or 0), (h(b.Up) and 1 or 0) - (h(b.Down) and 1 or 0))
        local stick = sticks[b.KeyCode]
        if stick and stick.Magnitude > b.Deadzone.Min then
            local range = b.Deadzone.Max - b.Deadzone.Min
            local magnitude = if range > 0 then math.min(1, (stick.Magnitude - b.Deadzone.Min) / range) else 1
            v += stick.Unit * magnitude ^ b.ResponseCurve
        end
        if b.ClampMagnitudeToOne and v.Magnitude > 1 then v = v.Unit end
        return v * b.Vector2Scale * s
    elseif t == Enum.InputActionType.Direction3D then
        local v = Vector3.new((h(b.Right) and 1 or 0) - (h(b.Left) and 1 or 0), (h(b.Up) and 1 or 0) - (h(b.Down) and 1 or 0),
            (h(b.Backward) and 1 or 0) - (h(b.Forward) and 1 or 0))
        if b.ClampMagnitudeToOne and v.Magnitude > 1 then v = v.Unit end
        return v * b.Vector3Scale * s
    end
    return nil
end
local function binding_uses(b, key)
    if b.Type == Enum.InputBindingType.Scriptable then return false end
    for _, k in { b.KeyCode, b.Up, b.Down, b.Left, b.Right, b.Forward, b.Backward } do
        if k == key then return true end
    end
    return false
end
-- This frame's mouse motion and wheel input, added to held keys and sticks.
local analog = setmetatable({}, { __mode = "k" })
local function keys_value(a)
    local t = a.Type
    local value = zero_of(t)
    if t == Enum.InputActionType.ViewportPosition then return value end
    for _, b in a:GetChildren() do
        if b:IsA("InputBinding") and b.Type ~= Enum.InputBindingType.Scriptable then
            local v = binding_value(a, b)
            if t == Enum.InputActionType.Bool then
                value = value or v
            elseif v ~= nil then
                value += v
            end
        end
    end
    return value
end
-- Re-evaluate an action on a relevant input change, keeping other held bindings.
local function actions_input(key, down)
    held[key] = down or nil
    local ctxs = {}
    for c in tracked.InputContext do
        if c.Enabled and c:IsDescendantOf(game) then table.insert(ctxs, c) end
    end
    table.sort(ctxs, function(a, b) return a.Priority > b.Priority end)
    for _, ctx in ctxs do
        local took = false
        for _, a in ctx:GetChildren() do
            if a:IsA("InputAction") and a.Enabled and a.Type ~= Enum.InputActionType.ViewportPosition then
                for _, b in a:GetChildren() do
                    if b:IsA("InputBinding") and binding_uses(b, key) then
                        local value = keys_value(a)
                        if analog[a] ~= nil then value += analog[a] end
                        set_state(a, value)
                        took = true
                        break
                    end
                end
            end
        end
        if took and ctx.Sink then return true end
    end
    return false
end

local function gamepad_input(state, pad, key, x, y)
    local kc = Enum.KeyCode[key]
    if not kc then return end
    local values = pad_values[pad]
    if not values then
        values = {}
        pad_values[pad] = values
    end
    if key == "Thumbstick1" or key == "Thumbstick2" then
        local stick = Vector2.new(x, y)
        values[kc] = if stick == Vector2.zero then nil else stick
        local strongest = Vector2.zero
        for _, other in pad_values do
            if other[kc] and other[kc].Magnitude > strongest.Magnitude then strongest = other[kc] end
        end
        sticks[kc] = strongest
        actions_input(kc, false)
    elseif state == "Begin" or state == "End" then
        values[kc] = state == "Begin" or nil
        local down = false
        for _, other in pad_values do
            if other[kc] then down = true; break end
        end
        actions_input(kc, down)
    end
end
-- The analog key codes (measured in Studio 0.747 with a real mouse,
-- checks/humanoid-physics/feel/traces/studio-ias.log): a binding on
-- MouseDelta gives its action, for the frame the mouse moved in, the raw
-- motion in pixels × Scale × Vector2Scale ÷ that frame's dt (a rate: the
-- PlayerModule multiplies by dt again), and only while the mouse is locked
-- (the motion UserInputService:GetMouseDelta reports); MouseWheel gives
-- 0.5 a notch × Scale ÷ dt; the next frame the action is back at what its
-- keys hold. MousePosition gives a ViewportPosition action the pointer.
local frame_dt = 1 / 60
-- A ViewportPosition action's value is in viewport coordinates (the top
-- of the window is y = 0): the PlayerModule's DynamicThumbstick adds
-- GuiService:GetInsetArea(Enum.ScreenInsets.None).Min to it to get the
-- GUI-coordinate position it compares with AbsolutePositions, and that
-- Min is (0, -58) in Studio's Play (GetInsetArea above).
local function viewport_pos(x, y)
    local inset = (G.is_client() and not G.is_edit()) and 58 or 0
    return Vector2.new(x, y + inset)
end
local function analog_input(kind, x, y)
    local key = Enum.KeyCode[kind]
    for c in tracked.InputContext do
        if c.Enabled and c:IsDescendantOf(game) then
            for _, a in c:GetChildren() do
                if a:IsA("InputAction") and a.Enabled then
                    for _, b in a:GetChildren() do
                        if b:IsA("InputBinding") and b.Type ~= Enum.InputBindingType.Scriptable and b.KeyCode == key then
                            local t = a.Type
                            if kind == "MousePosition" then
                                if t == Enum.InputActionType.ViewportPosition then set_state(a, viewport_pos(x, y)) end
                            elseif t == Enum.InputActionType.Direction2D then
                                analog[a] = (analog[a] or Vector2.zero) + Vector2.new(x, y) * b.Vector2Scale * b.Scale / frame_dt
                                set_state(a, keys_value(a) + analog[a])
                            elseif t == Enum.InputActionType.Direction1D then
                                analog[a] = (analog[a] or 0) + x * b.Scale / frame_dt
                                set_state(a, keys_value(a) + analog[a])
                            end
                        end
                    end
                end
            end
        end
    end
end
-- A new frame of input: last frame's analog input is over.
local function analog_frame(dt)
    if type(dt) == "number" and dt > 0 then frame_dt = dt end
    for a in analog do
        analog[a] = nil
        if a.Parent ~= nil then set_state(a, keys_value(a)) end
    end
end
local mouse_keys = { MouseButton1 = "MouseLeftButton", MouseButton2 = "MouseRightButton", MouseButton3 = "MouseMiddleButton" }

------------------------------------------------------- Mouse / UIS extras
-- The legacy Mouse's KeyDown / KeyUp (lower-case character), Move,
-- WheelForward / WheelBackward and Idle; UserInputService.JumpRequest.
local mouse_moved = false
local prompt_key
local function on_input(state, uit, key, x, y, z, gpe, dx, dy)
    local mouse = G.mouse()
    if uit == "Keyboard" then
        local kc = Enum.KeyCode[key]
        if kc and not gpe then
            local down = state == "Begin"
            if state == "Begin" or state == "End" then
                actions_input(kc, down)
                if prompt_key then prompt_key(kc, down) end
            end
        end
        if mouse and not gpe and (state == "Begin" or state == "End") then
            local kc2 = Enum.KeyCode[key]
            local ch = kc2 and kc2.Value < 256 and string.char(kc2.Value):lower() or key:lower()
            fire(mouse, state == "Begin" and "KeyDown" or "KeyUp", ch)
        end
        if key == "Space" and state == "Begin" and not gpe then
            fire(svc("UserInputService"), "JumpRequest")
        end
        last_device = "Keyboard"
    elseif mouse_keys[uit] then
        if not gpe and (state == "Begin" or state == "End") then
            actions_input(Enum.KeyCode[mouse_keys[uit]], state == "Begin")
        end
        last_device = "Keyboard"
    elseif uit == "MouseMovement" then
        mouse_moved = true
        if mouse then fire(mouse, "Move") end
        if not gpe then
            analog_input("MousePosition", x, y)
            if (dx or 0) ~= 0 or (dy or 0) ~= 0 then analog_input("MouseDelta", dx, dy) end
        end
    elseif uit == "MouseWheel" then
        if mouse then fire(mouse, (z or 0) > 0 and "WheelForward" or "WheelBackward") end
        -- (z is already the engine's 0.5 a notch: mech WHEEL_INPUT_PER_NOTCH.)
        if not gpe and (z or 0) ~= 0 then analog_input("MouseWheel", z, 0) end
    elseif uit:find("^Gamepad") then
        if not gpe then gamepad_input(state, uit, key, x, y) end
        last_device = "Gamepad"
    end
end
do
    local inner = __rs_input_event
    if inner then
        __rs_input_event = function(state, uit, key, x, y, z, dx, dy, gpe)
            local ok, err = pcall(on_input, state, uit, key, x, y, z, gpe, dx, dy)
            if not ok then log("error", "gui_input: " .. tostring(err)) end
            return inner(state, uit, key, x, y, z, dx, dy, gpe)
        end
    end
    local frame_end = __rs_input_frame_end
    __rs_input_frame_end = function(dt)
        local ok, err = pcall(analog_frame, dt)
        if not ok then log("error", "gui_input: " .. tostring(err)) end
        if frame_end then return frame_end(dt) end
    end
end
local function step_mouse()
    local mouse = G.mouse()
    if mouse and not mouse_moved then fire(mouse, "Idle") end
    mouse_moved = false
end

-------------------------------------------------------- Gamepad / haptics
methods["GamepadService.EnableGamepadCursor"] = function(self, obj)
    if obj ~= nil and typeof(obj) ~= "Instance" then error("Unable to cast value to Object", 0) end
    -- No cursor without a gamepad (Studio: GamepadCursorEnabled stays false).
    if G.gamepads() > 0 then
        __rs_set_raw(self, "GamepadCursorEnabled", true)
        if isa(obj, "GuiObject") then svc("GuiService").SelectedObject = obj end
    end
end
methods["GamepadService.DisableGamepadCursor"] = function(self)
    if self.GamepadCursorEnabled then __rs_set_raw(self, "GamepadCursorEnabled", false) end
end
local function pad_index(t)
    if typeof(t) ~= "EnumItem" then return nil end
    local n = t.Name:match("^Gamepad(%d)$")
    return n and tonumber(n)
end
local function pad_connected(t)
    local i = pad_index(t)
    return i ~= nil and i >= 1 and i <= 8 and bit32.btest(G.gamepads(), bit32.lshift(1, i - 1))
end
methods["HapticService.IsVibrationSupported"] = function(_, t)
    if typeof(t) ~= "EnumItem" then error("Unable to cast value to token", 0) end
    return pad_connected(t)
end
methods["HapticService.IsMotorSupported"] = function(_, t, motor)
    if typeof(t) ~= "EnumItem" or typeof(motor) ~= "EnumItem" then error("Unable to cast value to token", 0) end
    return pad_connected(t) and (motor == Enum.VibrationMotor.Large or motor == Enum.VibrationMotor.Small)
end
local motors = {}
methods["HapticService.SetMotor"] = function(_, t, motor, ...)
    if typeof(t) ~= "EnumItem" or typeof(motor) ~= "EnumItem" then error("Unable to cast value to token", 0) end
    if select("#", ...) == 0 then
        log("error", "Error in HapticService:SetMotor no values found for vibration.")
        return
    end
    motors[t.Name .. "/" .. motor.Name] = table.pack(...)
end
methods["HapticService.GetMotor"] = function(_, t, motor)
    if typeof(t) ~= "EnumItem" or typeof(motor) ~= "EnumItem" then error("Unable to cast value to token", 0) end
    local v = motors[t.Name .. "/" .. motor.Name]
    if not v then return end
    return table.unpack(v, 1, v.n)
end
-- HapticEffect: in the DataModel, a play ends after its type's length
-- (Studio: UIHover 35 ms, UIClick 55, UINotification 0.4 s, collision 0.3,
-- explosion 0.8, Custom at once); Looped plays until Stop.
local effect_len = { UIHover = 0.035, UIClick = 0.055, UINotification = 0.4, GameplayCollision = 0.3, GameplayExplosion = 0.8, Custom = 0 }
local playing_fx = {}
local fx_keys = setmetatable({}, { __mode = "k" })
methods["HapticEffect.Play"] = function(fx)
    if not fx:IsDescendantOf(game) then return end
    if fx.Looped then playing_fx[fx] = math.huge return end
    playing_fx[fx] = G.time() + (effect_len[fx.Type.Name] or 0)
end
methods["HapticEffect.Stop"] = function(fx) playing_fx[fx] = nil end
methods["HapticEffect.SetWaveformKeys"] = function(fx, keys)
    if fx.Type ~= Enum.HapticEffectType.Custom then error("Cannot modify non-custom haptic effect!", 0) end
    if type(keys) ~= "table" then error("Variant cast failed", 0) end
    for _, k in keys do
        if typeof(k) ~= "FloatCurveKey" then error("Variant cast failed", 0) end
    end
    fx_keys[fx] = copy(keys)
end
local function step_fx()
    local now = G.time()
    for fx, at in playing_fx do
        if now >= at then
            playing_fx[fx] = nil
            if fx.Parent then fire(fx, "Ended") end
        end
    end
end

------------------------------------------------------------------- VRService
local touchpad = {}
methods["VRService.GetTouchpadMode"] = function(_, pad)
    if typeof(pad) ~= "EnumItem" then error("Unable to cast value to token", 0) end
    return touchpad[pad] or Enum.VRTouchpadMode.VirtualThumbstick
end
methods["VRService.SetTouchpadMode"] = function(vr, pad, mode)
    if typeof(pad) ~= "EnumItem" or typeof(mode) ~= "EnumItem" then error("Unable to cast value to token", 0) end
    if (touchpad[pad] or Enum.VRTouchpadMode.VirtualThumbstick) ~= mode then
        touchpad[pad] = mode
        fire(vr, "TouchpadModeChanged", pad, mode)
    end
end
local gui_input_cf = Enum.UserCFrame.Head
get["VRService.GuiInputUserCFrame"] = function() return gui_input_cf end
set["VRService.GuiInputUserCFrame"] = function(vr, _, v)
    if not is_enum(v, "UserCFrame") then
        error("Unable to assign property GuiInputUserCFrame. EnumItem expected, got " .. typeof(v), 0)
    end
    if v ~= gui_input_cf then gui_input_cf = v changed(vr, "GuiInputUserCFrame") end
    return true
end

------------------------------------------------------------ Dialog
local dialog_use = setmetatable({}, { __mode = "k" })
get["Dialog.InUse"] = function(d) return dialog_use[d] == true end
set["Dialog.InUse"] = function(d, _, v)
    v = v == true
    if (dialog_use[d] == true) ~= v then dialog_use[d] = v changed(d, "InUse") end
    return true
end
local dialog_players = setmetatable({}, { __mode = "k" })
methods["Dialog.GetCurrentPlayers"] = function(d)
    local out = {}
    for p in dialog_players[d] or {} do
        if p.Parent then table.insert(out, p) end
    end
    return out
end

------------------------------------------------------- BillboardGui distance
get["BillboardGui.CurrentDistance"] = function(b)
    if not G.is_client() then return 0 end
    local x, y, z, ok = G.camera()
    if not ok then return 0 end
    local a = b.Adornee or b.Parent
    local pos
    if isa(a, "BasePart") then pos = a.Position
    elseif isa(a, "Attachment") then pos = a.WorldPosition
    elseif isa(a, "Model") then
        local okp, piv = pcall(function() return a:GetPivot() end)
        pos = okp and piv.Position or nil
    end
    if not pos then return 0 end
    return f32((pos - Vector3.new(x, y, z)).Magnitude)
end

LM.CaptureSnapshotAsync = function(self)
    if typeof(self) ~= "Instance" or not self:IsA("ViewportFrame") then
        error("Expected ':' not '.' calling member function CaptureSnapshotAsync", 0)
    end
    local id = G.vf_snap_start(self)
    local res
    for _ = 1, 180 do
        task.wait()
        res = G.vf_snap_poll(id)
        if res ~= nil then break end
    end
    return res or ""
end

------------------------------------------------------- TextGenerator
-- Studio generates through the place's universe (/user/cloud/v2/
-- universes/{id}:generateText) with the user's session; it must be in the
-- DataModel first.
LM.GenerateTextAsync = function(self, request)
    if typeof(self) ~= "Instance" or not self:IsA("TextGenerator") then
        error("Expected ':' not '.' calling member function GenerateTextAsync", 0)
    end
    if not self:IsDescendantOf(game) then error("TextGenerator must be parented into the DataModel.", 0) end
    if type(request) ~= "table" then error("Unable to cast value to Dictionary", 0) end
    local up = request.UserPrompt
    if type(up) ~= "string" then error("UserPrompt should be a string.", 0) end
    local body = { userPrompt = up, systemPrompt = self.SystemPrompt, temperature = self.Temperature }
    if request.ContextToken ~= nil then
        if type(request.ContextToken) ~= "string" then error("ContextToken should be a string.", 0) end
        body.contextToken = request.ContextToken
    end
    if request.MaxTokens ~= nil then
        if type(request.MaxTokens) ~= "number" then
            error("MaxTokens should be a number. Non-integral values will be rounded to the nearest integer.", 0)
        end
        local n = math.floor(request.MaxTokens + 0.5)
        if n < 1 then error("The value of MaxTokens must be at least 1.", 0) end
        body.maxTokens = n
    end
    if request.JsonSchema ~= nil then
        if type(request.JsonSchema) ~= "string" then error("JsonSchema should be a string.", 0) end
        body.responseFormatType = "JSON_SCHEMA"
        body.jsonSchemaStr = request.JsonSchema
    end
    local http = svc("HttpService")
    local id = G.textgen_start(game.GameId, http:JSONEncode(body))
    local status, text
    repeat
        task.wait()
        status, text = G.textgen_poll(id)
    until status ~= nil
    if status == 0 then error("Error while making HTTP request: " .. tostring(text), 0) end
    if status < 200 or status >= 300 then
        error("Error while making HTTP request: " .. G.http_error(status), 0)
    end
    local ok, r = pcall(function() return http:JSONDecode(text) end)
    if not ok or type(r) ~= "table" then error("Cannot parse response.", 0) end
    if type(r.generatedText) ~= "string" then error("Response does not include generated text.", 0) end
    if type(r.contextToken) ~= "string" then error("Response does not include a context token.", 0) end
    if r.model == nil then error("Response does not include the model.", 0) end
    return { GeneratedText = r.generatedText, ContextToken = r.contextToken, Model = r.model }
end

---------------------------------------------------------- ProximityPrompt
-- The client shows each enabled prompt in range (MaxActivationDistance from
-- the character, line of sight when asked), by Exclusivity; its key holds
-- it. Hold: PromptButtonHoldBegan, then at HoldDuration PromptButtonHoldEnded
-- and Triggered, TriggerEnded at release (an early release: HoldEnded only).
-- Each fires on the prompt, then ProximityPromptService (with the prompt),
-- on the client and the server (sent as an engine message).
local shown_prompts = {}
local holds = {}
local function prompt_pos(p)
    local a = p.Parent
    if isa(a, "BasePart") then return a.Position end
    if isa(a, "Attachment") then return a.WorldPosition end
    if isa(a, "Model") then
        local ok, piv = pcall(function() return a:GetPivot() end)
        return ok and piv.Position or nil
    end
    return nil
end
local function both(p, ev, svc_ev, player)
    fire(p, ev, player)
    local pps = svc("ProximityPromptService")
    if pps then fire(pps, svc_ev, p, player) end
end
local function send_server(p, kind)
    if G.send then G.send(p, kind) end
end
local function hold_begin(p)
    local player = local_player()
    if not player or holds[p] then return end
    if p.HoldDuration > 0 then
        holds[p] = { t0 = G.time(), done = false }
        both(p, "PromptButtonHoldBegan", "PromptButtonHoldBegan", player)
        send_server(p, "PromptButtonHoldBegan")
    else
        holds[p] = { t0 = G.time(), done = true }
        both(p, "Triggered", "PromptTriggered", player)
        send_server(p, "Triggered")
    end
end
local function hold_end(p)
    local h = holds[p]
    if not h then return end
    holds[p] = nil
    local player = local_player()
    if h.done then
        both(p, "TriggerEnded", "PromptTriggerEnded", player)
        send_server(p, "TriggerEnded")
    else
        both(p, "PromptButtonHoldEnded", "PromptButtonHoldEnded", player)
        send_server(p, "PromptButtonHoldEnded")
    end
end
methods["ProximityPrompt.InputHoldBegin"] = function(p)
    if G.is_client() then hold_begin(p) end
end
methods["ProximityPrompt.InputHoldEnd"] = function(p)
    if G.is_client() then hold_end(p) end
end
prompt_key = function(kc, down)
    for p in shown_prompts do
        if p.KeyboardKeyCode == kc then
            if down then hold_begin(p) else hold_end(p) end
        end
    end
end
-- Legacy Dialogs: Studio's MainBotChatScript2 with stand-ins for what only
-- CoreGui has. Measured (offline Studio): InUse goes true on the client then the
-- server (GetCurrentPlayers then lists the player), DialogChoiceSelected
-- (player, choice) fires on the client only, InUse false at the end.
local dialog_set = {}
local dialog_added, dialog_removed
local function dialog_track(i)
    if i:IsA("Dialog") and not dialog_set[i] then
        dialog_set[i] = true
        dialog_added:Fire(i)
    end
end
local dialog_ui_started = false
local function start_dialog_ui()
    if dialog_ui_started then return end
    local p = local_player()
    local pg = p and p:FindFirstChildOfClass("PlayerGui")
    if not pg then return end
    dialog_ui_started = true
    dialog_added, dialog_removed = Instance.new("BindableEvent"), Instance.new("BindableEvent")
    -- RobloxGui stand-in: Studio's has ControlFrame.BottomLeftControl at
    -- (0, 0, 1, -46), 130 × 46.
    local rg = Instance.new("ScreenGui")
    rg.Name = "RobloxGui"
    rg.ResetOnSpawn = false
    rg.DisplayOrder = 100000
    local cf = Instance.new("Frame")
    cf.Name = "ControlFrame"
    cf.Size = UDim2.fromScale(1, 1)
    cf.BackgroundTransparency = 1
    cf.Parent = rg
    local bl = Instance.new("Frame")
    bl.Name = "BottomLeftControl"
    bl.Position = UDim2.new(0, 0, 1, -46)
    bl.Size = UDim2.fromOffset(130, 46)
    bl.BackgroundTransparency = 1
    bl.Parent = cf
    local holder = Instance.new("ScreenGui")
    holder.Name = "DialogBubbles"
    holder.ResetOnSpawn = false
    holder.Parent = pg
    rg.Parent = pg
    local rrs = {}
    local remote = {}
    function remote:FireServer(dialog, on)
        if G.send then G.send(dialog, "SetDialogInUse", on == true) end
    end
    function rrs:WaitForChild() return remote end
    for _, d in G.find_classes({ "Dialog" }) do dialog_track(d) end
    local dialog_added_sig, dialog_removing_sig = G.class_signals({ "Dialog" })
    dialog_added_sig:Connect(dialog_track)
    dialog_removing_sig:Connect(function(i)
        if dialog_set[i] then dialog_set[i] = nil dialog_removed:Fire(i) end
    end)
    local D = {
        coregui = holder,
        robloxgui = rg,
        rrs = rrs,
        analytics = { TrackEvent = function() end },
        tenfoot = { IsEnabled = function() return false end },
        utility = { IsSmallTouchScreen = function() return false end },
        -- GameTranslator: the text as written (no localization table here).
        translator = { TranslateAndRegister = function(_, label, _, text) label.Text = text end },
        signal = function(dialog, player, choice)
            fire(dialog, "DialogChoiceSelected", player, choice)
        end,
        chat_local = function(...) svc("Chat"):Chat(...) end,
        item_added = dialog_added.Event,
        item_removed = dialog_removed.Event,
        dialogs = function()
            local out = {}
            for d in dialog_set do table.insert(out, d) end
            return out
        end,
        -- GuiService:GetClosestDialogToPosition: the nearest dialog on a part.
        closest = function(_, pos)
            local best, bd
            for d in dialog_set do
                local part = d.Parent
                if part and part:IsA("BasePart") and d:IsDescendantOf(workspace) then
                    local dist = (part.Position - pos).Magnitude
                    if not bd or dist < bd then best, bd = d, dist end
                end
            end
            return best
        end,
    }
    local ok, f = pcall(G.dialog_ui)
    if ok and f then
        task.spawn(f, D)
    elseif not ok then
        log("warn", "Dialog UI: " .. tostring(f))
    end
end

local prompt_ui_started = false
local function preferred_prompt(candidate, current)
    return not current or candidate.alignment > current.alignment
        or (candidate.alignment == current.alignment and candidate.order < current.order)
end
local function prompt_clear(prompt, character, origin, direction)
    local params = RaycastParams.new()
    params.FilterType = Enum.RaycastFilterType.Exclude
    local excluded = {character}
    local holder = prompt.Parent
    if isa(holder, "Attachment") then holder = holder.Parent end
    if holder then table.insert(excluded, holder) end
    while true do
        params.FilterDescendantsInstances = excluded
        local hit = workspace:Raycast(origin, direction, params)
        if not hit then return true end
        local part = hit.Instance
        if not isa(part, "BasePart") or isa(part, "Terrain") or part.Transparency <= 0.95 then
            return false
        end
        table.insert(excluded, part)
    end
end
local function step_prompts()
    if not dialog_ui_started then start_dialog_ui() end
    if not prompt_ui_started and local_player() then
        prompt_ui_started = true
        local ok, f = pcall(G.prompt_ui)
        if ok and f then
            task.spawn(f)
        elseif not ok then
            log("warn", "ProximityPrompt UI: " .. tostring(f))
        end
    end
    local now = G.time()
    for p, h in holds do
        if not h.done and now - h.t0 >= p.HoldDuration then
            h.done = true
            local player = local_player()
            both(p, "PromptButtonHoldEnded", "PromptButtonHoldEnded", player)
            send_server(p, "PromptButtonHoldEnded")
            both(p, "Triggered", "PromptTriggered", player)
            send_server(p, "Triggered")
        end
    end
    local player = local_player()
    local char = player and player.Character
    local root = char and (char:FindFirstChild("HumanoidRootPart") or char.PrimaryPart)
    local want = {}
    local camera = workspace.CurrentCamera
    if root and camera then
        local origin = root.Position
        local view = camera.CFrame
        local best_key, best_all = {}, nil
        for p, order in tracked.ProximityPrompt do
            if p.Enabled and p:IsDescendantOf(workspace) then
                local pos = prompt_pos(p)
                if pos and (pos - origin).Magnitude <= p.MaxActivationDistance then
                    -- UIOffset only positions the drawn widget. Eligibility
                    -- and priority use the projected world attachment.
                    local _, on_screen = camera:WorldToViewportPoint(pos)
                    local sightline = pos - view.Position
                    if on_screen and (not p.RequiresLineOfSight or prompt_clear(p, char, view.Position, sightline)) then
                        local ex = p.Exclusivity
                        local c = { p = p, order = order, alignment = view.LookVector:Dot(sightline.Unit) }
                        if ex == Enum.ProximityPromptExclusivity.AlwaysShow then
                            want[p] = true
                        elseif ex == Enum.ProximityPromptExclusivity.OneGlobally then
                            if preferred_prompt(c, best_all) then best_all = c end
                        else
                            local k = p.KeyboardKeyCode
                            if preferred_prompt(c, best_key[k]) then best_key[k] = c end
                        end
                    end
                end
            end
        end
        for _, c in best_key do want[c.p] = true end
        if best_all then want[best_all.p] = true end
    end
    local pps = svc("ProximityPromptService")
    if pps and not pps.Enabled then want = {} end
    for p in shown_prompts do
        if not want[p] then
            shown_prompts[p] = nil
            if holds[p] then hold_end(p) end
            if pps then fire(pps, "PromptHidden", p) end
            fire(p, "PromptHidden")
        end
    end
    for p in want do
        if not shown_prompts[p] then
            shown_prompts[p] = true
            if pps then fire(pps, "PromptShown", p, Enum.ProximityPromptInputType.Keyboard) end
            fire(p, "PromptShown", Enum.ProximityPromptInputType.Keyboard)
        end
    end
end
-- The server hears the client's prompt input (gui_input.rs engine_message).
function M.remote_in(p, player, kind, ...)
    if kind == "SetDialogInUse" and typeof(p) == "Instance" and p:IsA("Dialog") then
        local on = ...
        local set_ = dialog_players[p] or {}
        dialog_players[p] = set_
        set_[player] = on and true or nil
        p.InUse = on == true
        return
    end
    if typeof(p) ~= "Instance" or not p:IsA("ProximityPrompt") then return end
    local svc_ev = ({ Triggered = "PromptTriggered", TriggerEnded = "PromptTriggerEnded",
        PromptButtonHoldBegan = "PromptButtonHoldBegan", PromptButtonHoldEnded = "PromptButtonHoldEnded" })[kind]
    if svc_ev then both(p, kind, svc_ev, player) end
end

---------------------------------------------------------------------- step
-- Drops tracked instances that were destroyed (no Parent read possible).
local function prune()
    for _, set_ in tracked do
        for i in set_ do
            if not pcall(function() return i.Parent end) then set_[i] = nil end
        end
    end
    for i in shown_prompts do
        if not pcall(function() return i.Parent end) then shown_prompts[i] = nil holds[i] = nil end
    end
    for d in dialog_set do
        if not pcall(function() return d.Parent end) then dialog_set[d] = nil end
    end
    if hover_a and not pcall(function() return hover_a.Parent end) then hover_a = nil end
    if press and not pcall(function() return press.a.Parent end) then press = nil end
    if focused and not pcall(function() return focused.Parent end) then focused = nil G.set_focus(nil) end
    if equipped_tool and not pcall(function() return equipped_tool.Parent end) then equipped_tool = nil end
    for fx in playing_fx do
        if not pcall(function() return fx.Parent end) then playing_fx[fx] = nil end
    end
    for pl in tweening do
        if not pcall(function() return pl.Parent end) then tweening[pl] = nil end
    end
    for inst in styled do
        if not pcall(function() return inst.Parent end) then styled[inst] = nil style_targets[inst] = nil end
    end
    for _, tbl in { rule_props, rule_trans, sheet_derives, query_conds, pseudo_kids } do
        for k in tbl do
            if not pcall(function() return k.Parent end) then tbl[k] = nil end
        end
    end
end

local last_prune_rev = -1
local function step_all(dt)
    refresh_game()
    local tree_rev, style_rev, vw, vh, hover_sig = G.style_state()
    if tree_rev ~= last_prune_rev then
        last_prune_rev = tree_rev
        G.profile_step(1, prune)
    end
    step_fx()
    if not G.is_client() then return end
    local edit = G.is_edit()
    if not edit then step_inspect() end
    start_tracking()
    if not edit then step_topbar() end
    step_focus()
    if not edit then step_tools() end
    step_pages(dt)
    step_drags()
    step_adorns()
    step_mouse()
    if not edit then step_prompts() end
    G.profile_step(2, step_styles, style_rev, vw, vh, hover_sig)
    return styles_dirty
        or next(playing_fx) ~= nil
        or next(tweening) ~= nil
        or active_drag ~= nil
        or hover_a ~= nil
        or press ~= nil
        or mouse_moved
        or focused ~= nil
        or next(tracked.Adorn) ~= nil
end

-- The Play server's plugin widgets (plugin_host/widgets_ui.rs, GUI
-- channel 2): their TextBoxes gain and lose focus, as a client's do in
-- step_all (the server has no game GUI of its own to step).
__rs_plugin_gui_step = function()
    if G.is_client() then return end
    local ok, err = pcall(step_focus)
    if not ok then log("error", "gui_input: " .. tostring(err)) end
end

-- Focus changes queued by this frame's presses, fired before the frame's GUI
-- events (a press elsewhere ends typing first: a TextBox's FocusLost comes
-- before the pressed button's Activated, as Studio; Argon reads its port box
-- on FocusLost and connects on Activated).
function M.focus_now()
    if not G.is_client() then return end
    local ok, err = xpcall(step_focus, debug.traceback)
    if not ok then log("error", "gui_input focus: " .. tostring(err):gsub("\n", " | ")) end
end

function M.step(dt)
    local ok, res = xpcall(step_all, debug.traceback, dt)
    if not ok then
        log("error", "gui_input step: " .. tostring(res):gsub("\n", " | "))
        return true
    end
    return res == true
end

return M
