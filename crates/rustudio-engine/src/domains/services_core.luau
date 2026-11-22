-- Engine API domain "services": the CoreScript-only services written in Luau
-- (services_core.rs routes their calls here). Roblox's apps run these in the
-- engine; rustudio keeps them inside the one DataModel:
-- - MessageBusService: the app's publish / subscribe bus. Message ids read
--   "<domain>.<name>", protocol ones "<protocol>.<method>.Request" /
--   ".Response" (the form Roblox's own MessageBus errors print:
--   "unsubscribing from a message id not subscribed to:
--   PermissionsProtocol.HasPermissions.Response", devforum.roblox.com/t/2659679).
--   Nothing outside the DataModel answers: `Call` returns nil and requests
--   without a handler get no response.
-- - LocalStorageService / AppStorageService: string items, kept for the Play
--   session (Roblox keeps them on disk); loaded from the start.
local rs = ...
local M = {}

local function fail(msg) error(msg, 0) end
local function str_arg(v, i)
	if type(v) ~= "string" then fail(string.format("Argument %d missing or nil", i)) end
	return v
end

------------------------------------------------------------ MessageBusService
local subscribers = {} -- mid -> { [connection] = { fn = callback, once = bool } }
local last = {} -- mid -> last published params
local owner = {} -- connection -> mid
local handlers = {} -- "<protocol>.<method>" -> request handler

local function deliver(mid, params)
	local list = subscribers[mid]
	if not list then return end
	for conn, sub in pairs(list) do
		if sub.once then
			list[conn] = nil
			owner[conn] = nil
		end
		task.spawn(sub.fn, params)
	end
end

local function subscribe(mid, fn, once, sticky)
	if type(fn) ~= "function" then fail("Argument 2 missing or nil") end
	local conn = rs.new_object("MessageBusConnection")
	subscribers[mid] = subscribers[mid] or {}
	subscribers[mid][conn] = { fn = fn, once = once == true }
	owner[conn] = mid
	if sticky and last[mid] ~= nil then
		deliver(mid, last[mid])
	end
	return conn
end

local function request_id(protocol, method) return protocol .. "." .. method .. ".Request" end
local function response_id(protocol, method) return protocol .. "." .. method .. ".Response" end

M["MessageBusService.GetMessageId"] = function(self, domain, name)
	return str_arg(domain, 1) .. "." .. str_arg(name, 2)
end
M["MessageBusService.GetProtocolMethodRequestMessageId"] = function(self, protocol, method)
	return request_id(str_arg(protocol, 1), str_arg(method, 2))
end
M["MessageBusService.GetProtocolMethodResponseMessageId"] = function(self, protocol, method)
	return response_id(str_arg(protocol, 1), str_arg(method, 2))
end
M["MessageBusService.Subscribe"] = function(self, mid, fn, once, sticky)
	return subscribe(str_arg(mid, 1), fn, once, sticky)
end
M["MessageBusService.SubscribeToProtocolMethodRequest"] = function(self, protocol, method, fn, once, sticky)
	return subscribe(request_id(str_arg(protocol, 1), str_arg(method, 2)), fn, once, sticky)
end
M["MessageBusService.SubscribeToProtocolMethodResponse"] = function(self, protocol, method, fn, once, sticky)
	return subscribe(response_id(str_arg(protocol, 1), str_arg(method, 2)), fn, once, sticky)
end
M["MessageBusService.Publish"] = function(self, mid, params)
	str_arg(mid, 1)
	last[mid] = params
	deliver(mid, params)
end
M["MessageBusService.GetLast"] = function(self, mid)
	return last[str_arg(mid, 1)]
end
M["MessageBusService.Call"] = function(self, key)
	str_arg(key, 1)
	return nil
end
M["MessageBusService.PublishProtocolMethodRequest"] = function(self, protocol, method, message)
	str_arg(protocol, 1)
	str_arg(method, 2)
	local mid = request_id(protocol, method)
	last[mid] = message
	deliver(mid, message)
	local handler = handlers[protocol .. "." .. method]
	if handler then
		task.spawn(function()
			local response = handler(message)
			M["MessageBusService.PublishProtocolMethodResponse"](self, protocol, method, response, 0)
		end)
	end
end
M["MessageBusService.PublishProtocolMethodResponse"] = function(self, protocol, method, message, code)
	local mid = response_id(str_arg(protocol, 1), str_arg(method, 2))
	last[mid] = message
	deliver(mid, message)
end
M["MessageBusService.SetRequestHandler"] = function(self, protocol, method, fn)
	handlers[str_arg(protocol, 1) .. "." .. str_arg(method, 2)] = fn
end
M["MessageBusService.MakeRequest"] = function(self, protocol, method, message, callback)
	str_arg(protocol, 1)
	str_arg(method, 2)
	if type(callback) == "function" then
		subscribe(response_id(protocol, method), callback, true, false)
	end
	M["MessageBusService.PublishProtocolMethodRequest"](self, protocol, method, message)
end
M["MessageBusConnection.Disconnect"] = function(self)
	local mid = owner[self]
	if mid and subscribers[mid] then subscribers[mid][self] = nil end
	owner[self] = nil
end

------------------------------------------------------------ LocalStorageService
local stores = {} -- service -> { key -> value }

local function items(self)
	stores[self] = stores[self] or {}
	return stores[self]
end

M["LocalStorageService.GetItem"] = function(self, key)
	return items(self)[str_arg(key, 1)] or ""
end
M["LocalStorageService.SetItem"] = function(self, key, value)
	str_arg(key, 1)
	value = tostring(value)
	items(self)[key] = value
	rs.fire(self, "ItemWasSet", key, value)
end
M["LocalStorageService.Flush"] = function(self) end
M["LocalStorageService.WhenLoaded"] = function(self, fn)
	if type(fn) ~= "function" then fail("Argument 1 missing or nil") end
	task.spawn(fn)
end

------------------------------------------------------------ StarterGui
-- SetCore / GetCore run what the CoreScripts registered (RobloxScriptSecurity
-- RegisterSetCore / RegisterGetCore). Once any is registered, an unknown
-- name fails with Roblox's message ("SetCore: SendNotification has not been
-- registered by the CoreScripts"); with no CoreScripts running they keep
-- rustudio's old answers (nothing / nil).
-- A registered function runs at its registrant's identity (a game
-- LocalScript's SetCore reaches a CoreScript's handler with the CoreScript's
-- capabilities, as in Roblox), on its own thread; its error comes back as
-- "Error calling SetCore <name>: <message>".
local set_core, get_core = {}, {}
M["StarterGui.RegisterSetCore"] = function(self, name, fn)
	if type(fn) ~= "function" then fail("Argument 2 missing or nil") end
	set_core[str_arg(name, 1)] = { fn = fn, ident = rs.identity() }
end
M["StarterGui.RegisterGetCore"] = function(self, name, fn)
	if type(fn) ~= "function" then fail("Argument 2 missing or nil") end
	get_core[str_arg(name, 1)] = { fn = fn, ident = rs.identity() }
end
local function run_as(kind, name, h, ...)
	local res = table.pack(rs.call_as(h.ident, h.fn, ...))
	if not res[1] then fail(string.format("Error calling %s %s: %s", kind, name, tostring(res[2]))) end
	return table.unpack(res, 2, res.n)
end
M["StarterGui.SetCore"] = function(self, name, ...)
	local h = set_core[str_arg(name, 1)]
	if h then
		run_as("SetCore", name, h, ...)
	elseif next(set_core) ~= nil then
		fail(string.format("SetCore: %s has not been registered by the CoreScripts", name))
	end
end
M["StarterGui.GetCore"] = function(self, name)
	local h = get_core[str_arg(name, 1)]
	if h then return run_as("GetCore", name, h) end
	if next(get_core) ~= nil then
		fail(string.format("GetCore: %s has not been registered by the CoreScripts", name))
	end
	return nil
end

------------------------------------------------------------ RunService
-- The client build the CoreScripts report in telemetry and version checks.
-- rustudio is no Roblox build: empty strings (estimate), no Team Create
-- viewer or team test, and the Roblox GUI focus kept as set.
local gui_focused = false
M["RunService.GetRobloxVersion"] = function() return "" end
M["RunService.GetCoreScriptVersion"] = function() return "" end
M["RunService.GetRobloxClientChannel"] = function() return "" end
M["RunService.IsTeamCreateViewer"] = function() return false end
M["RunService.IsTeamTest"] = function() return false end
M["RunService.GetRobloxGuiFocused"] = function() return gui_focused end
M["RunService.SetRobloxGuiFocused"] = function(self, on)
	on = on == true
	if on ~= gui_focused then
		gui_focused = on
		rs.fire(self, "RobloxGuiFocusedChanged", on)
	end
end

------------------------------------------------------------ UserInputService
-- A desktop at the top device level (estimate: rustudio runs on desktops).
M["UserInputService.GetDeviceType"] = function() return Enum.DeviceType.Desktop end
M["UserInputService.GetDeviceLevel"] = function() return Enum.DeviceLevel.High end

------------------------------------------------------------ Player
-- The block list the CoreScripts keep on the local player (the web block
-- list isn't fetched: it starts empty, estimate), and friend status
-- (no friends web call here: NotFriend, estimate).
local blocked, block_ready = {}, false
M["Player.GetBlockListInitialized"] = function() return block_ready end
M["Player.SetBlockListInitialized"] = function(self)
	block_ready = true
	rs.fire(self, "BlockListChanged")
end
M["Player.AddToBlockList"] = function(self, ids)
	if type(ids) ~= "table" then fail("Argument 1 missing or nil") end
	for _, id in ids do blocked[tonumber(id)] = true end
	rs.fire(self, "BlockListChanged")
end
M["Player.UpdatePlayerBlocked"] = function(self, id, on)
	blocked[tonumber(id)] = if on then true else nil
	rs.fire(self, "BlockListChanged")
end
M["Player.HasBlockedPlayer"] = function(self, id) return blocked[tonumber(id)] == true end
M["Player.GetFriendStatus"] = function(self, other)
	if other == self then return Enum.FriendStatus.Unknown end
	return Enum.FriendStatus.NotFriend
end
M["Player.GetUnder13"] = function() return false end

------------------------------------------------------------ RbxAnalyticsService
-- Roblox's telemetry: a Play session has no analytics backend, so the
-- reports go nowhere and the ids are empty (stubs, as for AnalyticsService).
for _, n in { "AddGlobalPointsField", "AddGlobalPointsTag", "RemoveGlobalPointsField", "RemoveGlobalPointsTag",
	"ReleaseRBXEventStream", "ReportCounter", "ReportInfluxSeries", "ReportStats", "ReportToDiagByCountryCode",
	"SendEventDeferred", "SendEventImmediately", "SetRBXEvent", "SetRBXEventStream", "TrackEvent", "TrackEventWithArgs",
	"UpdateHeartbeatObject" } do
	M["RbxAnalyticsService." .. n] = function() end
end
for _, n in { "GetClientId", "GetSessionId", "GetPlaySessionId" } do
	M["RbxAnalyticsService." .. n] = function() return "" end
end

------------------------------------------------------------ IXPService
-- Roblox's experiment layers. No experiment backend in a Play session: every
-- layer reads as loaded with no variables, so callers take their defaults
-- (estimate; the real service fetches assignments for the user).
local ixp_layers = { User = {}, Creator = {}, BrowserTracker = {} }
for kind, layers in ixp_layers do
	M["IXPService.Get" .. kind .. "LayerLoadingStatus"] = function() return Enum.IXPLoadingStatus.Initialized end
	M["IXPService.Get" .. kind .. "LayerVariables"] = function(self, name) str_arg(name, 1) return {} end
	M["IXPService.Get" .. kind .. "StatusForLayer"] = function(self, name)
		str_arg(name, 1)
		return Enum.IXPLoadingStatus.Initialized
	end
	M["IXPService.Log" .. kind .. "LayerExposure"] = function(self, name) str_arg(name, 1) end
	if kind ~= "BrowserTracker" then
		M["IXPService.Register" .. kind .. "Layers"] = function(self, list)
			if type(list) == "table" then
				for _, name in list do layers[tostring(name)] = true end
			end
		end
		M["IXPService.GetRegistered" .. kind .. "LayersToStatus"] = function()
			local out = {}
			for name in layers do out[name] = Enum.IXPLoadingStatus.Initialized end
			return out
		end
		M["IXPService.Initialize" .. kind .. "Layers"] = function(self)
			rs.fire(self, "On" .. kind .. "LayerLoadingStatusChanged", Enum.IXPLoadingStatus.Initialized)
		end
		M["IXPService.Clear" .. kind .. "Layers"] = function() table.clear(layers) end
	end
end
M["IXPService.LogFlagLinkedUserLayerExposure"] = function(self, name) str_arg(name, 1) end

------------------------------------------------------------ MemStorageService
-- The app's in-memory key / value store with change callbacks: Bind(key, fn)
-- runs fn(value) on each SetItem / Fire of that key; BindAndFire also runs it
-- now with the current value.
local mem, mem_binds, mem_owner = {}, {}, {}
local function mem_fire(key, value)
	for conn, fn in mem_binds[key] or {} do task.spawn(fn, value) end
end
local function mem_bind(key, fn)
	if type(fn) ~= "function" then fail("Argument 2 missing or nil") end
	local conn = rs.new_object("MemStorageConnection")
	mem_binds[key] = mem_binds[key] or {}
	mem_binds[key][conn] = fn
	mem_owner[conn] = key
	return conn
end
M["MemStorageService.GetItem"] = function(self, key, default)
	local v = mem[str_arg(key, 1)]
	if v == nil then return default end
	return v
end
M["MemStorageService.SetItem"] = function(self, key, value)
	str_arg(key, 1)
	mem[key] = tostring(value)
	mem_fire(key, mem[key])
end
M["MemStorageService.HasItem"] = function(self, key) return mem[str_arg(key, 1)] ~= nil end
M["MemStorageService.RemoveItem"] = function(self, key)
	local had = mem[str_arg(key, 1)] ~= nil
	mem[key] = nil
	return had
end
M["MemStorageService.Fire"] = function(self, key, value) mem_fire(str_arg(key, 1), value) end
M["MemStorageService.Bind"] = function(self, key, fn) return mem_bind(str_arg(key, 1), fn) end
M["MemStorageService.BindAndFire"] = function(self, key, fn)
	local conn = mem_bind(str_arg(key, 1), fn)
	task.spawn(fn, mem[key])
	return conn
end
M["MemStorageService.Call"] = function(self, key) str_arg(key, 1) return nil end
M["MemStorageConnection.Disconnect"] = function(self)
	local key = mem_owner[self]
	if key and mem_binds[key] then mem_binds[key][self] = nil end
	mem_owner[self] = nil
end

------------------------------------------------------------ NotificationService
-- Roblox's real-time push channel: a Play session isn't connected to it, so
-- subscriptions take nothing and its events never fire (stubs).
for _, n in { "ActionEnabled", "ActionTaken", "SubscribeToRccEventNamespace", "SubscribeToTopic",
	"UnsubscribeFromTopic", "SwitchedToAppShellFeature" } do
	M["NotificationService." .. n] = function() end
end

------------------------------------------------------------ ThirdPartyUserService
-- Console / third-party sign-in: a desktop Play session has none.
M["ThirdPartyUserService.IsSingleSignOnSupported"] = function() return false end
M["ThirdPartyUserService.IsAccountSwitchingSupported"] = function() return false end
M["ThirdPartyUserService.IsChatRestrictionSupported"] = function() return false end
M["ThirdPartyUserService.HaveActiveUser"] = function() return false end
M["ThirdPartyUserService.GetUserPlatformName"] = function() return "" end
M["ThirdPartyUserService.ShowAccountPicker"] = function() end

------------------------------------------------------------ AdService
-- No ad network in a Play session: no ad teleport, nothing to report, no
-- ads loaded (stubs).
-- (source universe, destination universe): 0, 0 for no ad teleport, as
-- BackButtonController compares them with numbers.
M["AdService.GetAdTeleportInfo"] = function() return 0, 0 end
M["AdService.GetReportAdInfo"] = function() return {} end
M["AdService.GetUniversalAppAdsEligibility"] = function() return {} end
M["AdService.IsAdLoaded"] = function() return false end
M["AdService.SetAdGuiInteractivityHandlerInitialized"] = function() end
M["AdService.HideEudsaDisclosure"] = function() end

------------------------------------------------------------ VRService / AvatarChatService
-- No VR app build, and none of the avatar-chat (face tracking, voice)
-- features in a Play session: the CoreScripts skip them (estimates).
M["VRService.IsVRAppBuild"] = function() return false end
M["AvatarChatService.deviceMeetsRequirementsForFeature"] = function() return false end
M["AvatarChatService.IsEnabled"] = function() return false end
M["AvatarChatService.IsPlaceEnabled"] = function() return false end
M["AvatarChatService.IsUniverseEnabled"] = function() return false end
M["AvatarChatService.EnableVoice"] = function() return false end
M["AvatarChatService.PollClientFeatures"] = function() return 0 end
M["AvatarChatService.PollServerFeatures"] = function() return 0 end
M["AvatarChatService.GetClientFeaturesAsync"] = function() return 0 end
M["AvatarChatService.GetServerFeaturesAsync"] = function() return 0 end

------------------------------------------------------------ the rest
-- Platform achievements (consoles): none here.
M["AchievementService.IsAvailable"] = function() return false end
M["AchievementService.HasAchieved"] = function() return false end
M["AchievementService.GrantAchievement"] = function() return false end
-- The CoreScripts' own view of an Animator's playing tracks: the same list
-- games get.
M["Animator.GetPlayingAnimationTracksCoreScript"] = function(self) return self:GetPlayingAnimationTracks() end
-- Event ingestion (telemetry): no backend in a Play session (stubs).
for _, n in { "SendEventDeferred", "SendEventImmediately", "SetRBXEvent", "SetRBXEventStream" } do
	M["EventIngestService." .. n] = function() end
end

------------------------------------------------------------ HttpService:RequestInternal
-- The CoreScripts' web requests (an HttpRequest; Start(callback) answers
-- callback(success, { StatusCode, Body, ... })). Roblox's web APIs answer
-- as the signed-in player through rustudio's login session; any other host
-- fails as a connection error. GET and POST only.
local requests = {}
M["HttpService.RequestInternal"] = function(self, options)
	if type(options) ~= "table" then fail("Argument 1 missing or nil") end
	local req = rs.new_object("HttpRequest")
	requests[req] = options
	return req
end
M["HttpRequest.Start"] = function(self, callback)
	local o = requests[self]
	requests[self] = nil
	if not o then fail("HttpRequest already started") end
	task.spawn(function()
		local id = rs.web_start(string.upper(tostring(o.Method or "GET")), tostring(o.Url), o.Body)
		if not id then
			if type(callback) == "function" then callback(false, { StatusCode = 0, Body = "", StatusMessage = "HttpError: ConnectFail" }) end
			return
		end
		local status, body
		repeat
			task.wait()
			status, body = rs.web_poll(id)
		until status ~= nil
		if type(callback) ~= "function" then return end
		if status == 0 then
			callback(false, { StatusCode = 0, Body = body, StatusMessage = body })
		else
			callback(true, { StatusCode = status, Body = body, Headers = {}, StatusMessage = tostring(status) })
		end
	end)
end
M["HttpRequest.Cancel"] = function(self) requests[self] = nil end

------------------------------------------------------------ CaptureService
-- Screenshots and videos the app saved: none in a Play session, and no
-- video capture (stubs; CaptureScreenshot and friends live in assets_cloud).
M["CaptureService.RetrieveCaptures"] = function() return {} end
M["CaptureService.CanCaptureVideo"] = function() return false end
M["CaptureService.IsCapturingVideo"] = function() return false end
for _, n in { "OnCaptureBegan", "OnCaptureEnded", "OnCaptureShared", "OnCaptureObjectShared", "OnVideoCaptureShared",
	"OnSavePromptFinished", "OnSharePromptFinished", "OnCapturePermissionsPromptFinished", "DeleteCapture",
	"SaveCaptureToExternalStorage", "SaveScreenshotCapture", "StopVideoCaptureInternal" } do
	M["CaptureService." .. n] = function() end
end

------------------------------------------------------------ LinkingService
-- Deep links into the app: a Play session gets none. The CoreScripts'
-- registered url patterns are kept (IsUrlRegistered); nothing is pending
-- and no url arrives (OnLuaUrl never fires).
local lua_urls = {}
M["LinkingService.RegisterLuaUrl"] = function(self, url) lua_urls[str_arg(url, 1)] = true end
M["LinkingService.IsUrlRegistered"] = function(self, url) return lua_urls[str_arg(url, 1)] == true end
M["LinkingService.GetLastLuaUrl"] = function() return nil end
M["LinkingService.GetAndClearLastPendingUrl"] = function() return {} end
M["LinkingService.StartLuaUrlDelivery"] = function() return nil end
M["LinkingService.StopLuaUrlDelivery"] = function() end
M["LinkingService.DetectUrl"] = function() end
M["LinkingService.DetectDlic"] = function() end
M["LinkingService.CreateInternalDlic"] = function() return nil end
M["LinkingService.OpenUrl"] = function() return false end
M["LinkingService.SupportsSwitchToSettingsApp"] = function() return false end
M["LinkingService.SwitchToSettingsApp"] = function() end
-- The user agent the client's web requests send: rustudio's own (its
-- RequestInternal goes through rustudio's session).
M["HttpService.GetUserAgent"] = function() return rs.user_agent end

------------------------------------------------------------ AudioFocusService
-- Which registered audio context (voice, the app's media) has the focus:
-- RequestFocus grants it to the highest priority asking, AcquireFocus takes
-- it. -1 while none has it (estimate).
local audio_contexts, audio_focus, audio_priority = {}, -1, -math.huge
M["AudioFocusService.RegisterContextIdFromLua"] = function(self, id)
	if not table.find(audio_contexts, id) then
		table.insert(audio_contexts, id)
		rs.fire(self, "OnContextRegistered", id)
	end
end
M["AudioFocusService.GetRegisteredContexts"] = function() return table.clone(audio_contexts) end
M["AudioFocusService.GetFocusedContextId"] = function() return audio_focus end
M["AudioFocusService.RequestFocus"] = function(self, id, priority)
	priority = tonumber(priority) or 0
	if audio_focus == id or priority >= audio_priority then
		audio_focus, audio_priority = id, priority
		return true
	end
	return false
end
M["AudioFocusService.AcquireFocus"] = function(self, id)
	audio_focus = id
	return true
end

------------------------------------------------------------ SessionService
-- The app's session tree for telemetry: sessions by id with a parent, a tag,
-- metadata and a creation time, and the contexts that hold focus. A Play
-- client starts with one root session.
local sessions = {}
local root_sid
local function now_ms() return math.floor(os.time() * 1000) end
-- (made on first use: no DataModel yet when this chunk loads)
local function root()
	if not root_sid then
		root_sid = game:GetService("HttpService"):GenerateGUID(false)
		sessions[root_sid] = { tag = "Root", meta = {}, created = now_ms() }
	end
	return root_sid
end
local focused_contexts = {}
M["SessionService.GetRootSID"] = function() return root() end
M["SessionService.SessionExists"] = function(self, sid)
	root()
	return sessions[str_arg(sid, 1)] ~= nil
end
M["SessionService.SetSession"] = function(self, parent, child, tag)
	sessions[str_arg(child, 2)] = { parent = parent, tag = tostring(tag or ""), meta = {}, created = now_ms() }
end
M["SessionService.ReplaceSession"] = function(self, sid, tag)
	local s = sessions[str_arg(sid, 1)]
	if s then s.tag = tostring(tag or "") end
end
M["SessionService.RemoveSession"] = function(self, sid)
	sid = str_arg(sid, 1)
	if sid == root_sid then return end
	sessions[sid] = nil
	for k, s in sessions do
		if s.parent == sid then sessions[k] = nil end
	end
end
M["SessionService.GetSessionTag"] = function(self, sid)
	local s = sessions[str_arg(sid, 1)]
	return s and s.tag or ""
end
M["SessionService.GetCreatedTimestampUtcMs"] = function(self, sid)
	local s = sessions[str_arg(sid, 1)]
	return s and s.created or 0
end
M["SessionService.SetMetadata"] = function(self, sid, key, value)
	local s = sessions[str_arg(sid, 1)]
	if s then s.meta[str_arg(key, 2)] = value end
end
M["SessionService.GetMetadata"] = function(self, sid, key)
	local s = sessions[str_arg(sid, 1)]
	return s and s.meta[str_arg(key, 2)]
end
M["SessionService.RemoveMetadata"] = function(self, sid, key)
	local s = sessions[str_arg(sid, 1)]
	if s then s.meta[str_arg(key, 2)] = nil end
end
M["SessionService.RemoveSessionsWithMetadataKey"] = function(self, key)
	str_arg(key, 1)
	for k, s in sessions do
		if k ~= root_sid and s.meta[key] ~= nil then sessions[k] = nil end
	end
end
M["SessionService.GetSessionID"] = function(self, structural) str_arg(structural, 1) return "" end
M["SessionService.GenerateSessionInfoString"] = function() return "" end
M["SessionService.GetBreadcrumbs"] = function() return {} end
M["SessionService.GetHistory"] = function() return {} end
M["SessionService.AcquireContextFocus"] = function(self, c) focused_contexts[str_arg(c, 1)] = true end
M["SessionService.ReleaseContextFocus"] = function(self, c) focused_contexts[str_arg(c, 1)] = nil end
M["SessionService.IsContextFocused"] = function(self, c) return focused_contexts[str_arg(c, 1)] == true end

------------------------------------------------------------ ExperienceService
-- The app's experience launcher. A Play session is in its experience and
-- launches no other: join / leave callbacks are held on real connections
-- (fired by nothing here), launches and cross-experience calls do nothing.
local experience_events = {}
local function experience_event(kind)
	experience_events[kind] = experience_events[kind] or Instance.new("BindableEvent")
	return experience_events[kind].Event
end
M["ExperienceService.RegisterForExperienceJoin"] = function(self, fn)
	if type(fn) ~= "function" then fail("Argument 1 missing or nil") end
	return experience_event("join"):Connect(fn)
end
M["ExperienceService.RegisterForExperienceLeave"] = function(self, fn)
	if type(fn) ~= "function" then fail("Argument 1 missing or nil") end
	return experience_event("leave"):Connect(fn)
end
M["ExperienceService.IsInExperience"] = function() return true end
M["ExperienceService.GetPlaceJoinState"] = function() return "" end
M["ExperienceService.GetQueuePosition"] = function() return 0 end
M["ExperienceService.GetFollowUserId"] = function() return 0 end
M["ExperienceService.GetPendingJoinAttempt"] = function() return {} end
M["ExperienceService.ConsumePendingExperienceLeaveWithReason"] = function() return {} end
for _, n in { "LaunchExperience", "LaunchExperienceFromSource" } do
	M["ExperienceService." .. n] = function() return "" end
end
for _, n in { "LaunchExperienceFromSourceWithCallback", "LeaveExperienceWithReason", "StartCrossExperience", "StopCrossExperience", "ExecuteCrossExperienceCall" } do
	M["ExperienceService." .. n] = function() end
end

------------------------------------------------------------ TelemetryService
-- Roblox's client telemetry: no backend in a Play session (stubs).
for _, n in { "LogEvent", "LogCounter", "LogStat", "LogDurationEvent", "LogDurationEventWithTimestamp" } do
	M["TelemetryService." .. n] = function() end
end

------------------------------------------------------------ TextService
-- The TextSize offset the player's preferred text size adds (GuiService.
-- PreferredTextSize): rustudio's is Medium, which adds none (estimate: the
-- offsets of the larger sizes aren't measured).
M["TextService.GetTextSizeOffsetAsync"] = function(self, size)
	if tonumber(size) == nil then fail("Argument 1 missing or nil") end
	return 0
end

return M