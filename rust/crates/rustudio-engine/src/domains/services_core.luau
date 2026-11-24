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

------------------------------------------------------------ StarterGui SetCore/GetCore registry
local set_core_handlers = {}
local get_core_handlers = {}

M["StarterGui.RegisterSetCore"] = function(self, name, fn)
	local k = str_arg(name, 1)
	if type(fn) ~= "function" and typeof(fn) ~= "Instance" then
		fail("RegisterSetCore: Argument 2 must be a function or BindableEvent")
	end
	set_core_handlers[k] = fn
end
M["StarterGui.RegisterGetCore"] = function(self, name, fn)
	local k = str_arg(name, 1)
	if type(fn) ~= "function" and typeof(fn) ~= "Instance" then
		fail("RegisterGetCore: Argument 2 must be a function or BindableFunction")
	end
	get_core_handlers[k] = fn
end

function __rs_core_set_core(name, val)
	local h = set_core_handlers[name]
	if h == nil then return false end
	if type(h) == "function" then
		h(val)
	elseif typeof(h) == "Instance" and h:IsA("BindableEvent") then
		h:Fire(val)
	end
	return true
end

function __rs_core_get_core(name)
	local h = get_core_handlers[name]
	if h == nil then return false, nil end
	if type(h) == "function" then
		return true, h()
	elseif typeof(h) == "Instance" and h:IsA("BindableFunction") then
		return true, h:Invoke()
	end
	return true, nil
end

------------------------------------------------------------ RunService CoreScript members
local roblox_gui_focused = false
M["RunService.GetRobloxVersion"] = function() return "0.747.0.7470000" end
M["RunService.GetCoreScriptVersion"] = function() return "0.747.0.7470000" end
M["RunService.SetRobloxGuiFocused"] = function(_, on) roblox_gui_focused = on == true end
M["RunService.GetRobloxGuiFocused"] = function() return roblox_gui_focused end

------------------------------------------------------------ IXPService
local ixp_user_layers = {}
local ixp_creator_layers = {}
local function ixp_init_enum()
	local ok, v = pcall(function() return Enum.IXPLoadingStatus.Initialized end)
	return ok and v or nil
end
M["IXPService.GetUserLayerLoadingStatus"] = function() return ixp_init_enum() end
M["IXPService.GetBrowserTrackerLayerLoadingStatus"] = function() return ixp_init_enum() end
M["IXPService.GetCreatorLayerLoadingStatus"] = function() return ixp_init_enum() end
M["IXPService.GetUserStatusForLayer"] = function() return ixp_init_enum() end
M["IXPService.GetBrowserTrackerStatusForLayer"] = function() return ixp_init_enum() end
M["IXPService.GetCreatorStatusForLayer"] = function() return ixp_init_enum() end
M["IXPService.GetUserLayerVariables"] = function() return {} end
M["IXPService.GetBrowserTrackerLayerVariables"] = function() return {} end
M["IXPService.GetCreatorLayerVariables"] = function() return {} end
M["IXPService.GetRegisteredUserLayersToStatus"] = function()
	local out, st = {}, ixp_init_enum()
	for k in pairs(ixp_user_layers) do out[k] = st end
	return out
end
M["IXPService.GetRegisteredCreatorLayersToStatus"] = function()
	local out, st = {}, ixp_init_enum()
	for k in pairs(ixp_creator_layers) do out[k] = st end
	return out
end
M["IXPService.RegisterUserLayers"] = function(_, layers)
	if type(layers) == "table" then
		for k, v in pairs(layers) do ixp_user_layers[type(v) == "string" and v or tostring(k)] = true end
	end
end
M["IXPService.RegisterCreatorLayers"] = function(_, layers)
	if type(layers) == "table" then
		for k, v in pairs(layers) do ixp_creator_layers[type(v) == "string" and v or tostring(k)] = true end
	end
end
M["IXPService.ClearUserLayers"] = function() table.clear(ixp_user_layers) end
M["IXPService.ClearCreatorLayers"] = function() table.clear(ixp_creator_layers) end
M["IXPService.InitializeUserLayers"] = function(self)
	rs.fire(self, "OnUserLayerLoadingStatusChanged", ixp_init_enum())
end
M["IXPService.InitializeCreatorLayers"] = function(self)
	rs.fire(self, "OnCreatorLayerLoadingStatusChanged", ixp_init_enum())
end
for _, n in { "LogUserLayerExposure", "LogBrowserTrackerLayerExposure", "LogCreatorLayerExposure", "LogFlagLinkedUserLayerExposure" } do
	M["IXPService." .. n] = function() end
end

------------------------------------------------------------ MemStorageService
-- Default GUAC app policy JSON so CapturesPolicy enables the Gallery tab in
-- SettingsHub (fromMemStorageService reads "GUAC:<UserId>:app-policy").
local DEFAULT_APP_POLICY = '{"EligibleForCapturesFeature":true,"EnableCapturesBulkManagement":true,"EnableShareCaptureCTA":true}'
local mem_items = {
	["app-policy"] = DEFAULT_APP_POLICY,
}
local function mem_get(key)
	local v = mem_items[key]
	if v ~= nil then return v end
	if key:match("^GUAC:.*:app%-policy$") then
		return DEFAULT_APP_POLICY
	end
	return nil
end
local mem_subs = {}
local mem_owner = {}
M["MemStorageService.SetItem"] = function(_, key, val)
	key = str_arg(key, 1)
	local s = tostring(val)
	mem_items[key] = s
	local list = mem_subs[key]
	if list then
		for _, fn in pairs(list) do task.spawn(fn, s) end
	end
end
M["MemStorageService.GetItem"] = function(_, key)
	return mem_get(str_arg(key, 1)) or ""
end
M["MemStorageService.HasItem"] = function(_, key)
	return mem_get(str_arg(key, 1)) ~= nil
end
M["MemStorageService.RemoveItem"] = function(_, key)
	key = str_arg(key, 1)
	local had = mem_items[key] ~= nil
	mem_items[key] = nil
	return had
end
M["MemStorageService.Fire"] = function(_, key, ...)
	key = str_arg(key, 1)
	local list = mem_subs[key]
	if list then
		for _, fn in pairs(list) do task.spawn(fn, ...) end
	end
end
M["MemStorageService.Call"] = function(_, key)
	str_arg(key, 1)
	return nil
end
local function mem_bind(key, fn, fire_now)
	key = str_arg(key, 1)
	if type(fn) ~= "function" then fail("Argument 2 missing or nil") end
	local conn = rs.new_object("MemStorageConnection")
	mem_subs[key] = mem_subs[key] or {}
	mem_subs[key][conn] = fn
	mem_owner[conn] = key
	local cur = fire_now and mem_get(key) or nil
	if cur ~= nil then
		task.spawn(fn, cur)
	end
	return conn
end
M["MemStorageService.Bind"] = function(_, key, fn) return mem_bind(key, fn, false) end
M["MemStorageService.BindAndFire"] = function(_, key, fn) return mem_bind(key, fn, true) end
M["MemStorageConnection.Disconnect"] = function(self)
	local k = mem_owner[self]
	if k and mem_subs[k] then mem_subs[k][self] = nil end
	mem_owner[self] = nil
end

------------------------------------------------------------ RbxAnalyticsService
M["RbxAnalyticsService.GetSessionId"] = function() return "00000000-0000-0000-0000-000000000000" end
M["RbxAnalyticsService.GetClientId"] = function() return "00000000-0000-0000-0000-000000000000" end
M["RbxAnalyticsService.GetPlaySessionId"] = function() return "" end
for _, n in {
	"AddGlobalPointsField", "AddGlobalPointsTag", "DEPRECATED_TrackEvent", "DEPRECATED_TrackEventWithArgs",
	"ReleaseRBXEventStream", "RemoveGlobalPointsField", "RemoveGlobalPointsTag", "ReportCounter",
	"ReportInfluxSeries", "ReportStats", "ReportToDiagByCountryCode", "SendEventDeferred",
	"SendEventImmediately", "SetRBXEvent", "SetRBXEventStream", "TrackEvent", "TrackEventWithArgs",
	"UpdateHeartbeatObject",
} do
	M["RbxAnalyticsService." .. n] = function() end
end

------------------------------------------------------------ SessionService
local sess_map = {} -- sid -> { tag = ..., parent = ..., meta = {}, created = ms }
local sess_root = "root"
sess_map[sess_root] = { tag = "root", parent = nil, meta = {}, created = math.floor(os.time() * 1000) }
local sess_focus = {}
local function sess_find(sid) return sess_map[sid] end
M["SessionService.SetSession"] = function(self, parentSid, sid, tag)
	sid = tostring(sid or "")
	sess_map[sid] = { tag = tostring(tag or sid), parent = parentSid, meta = {}, created = math.floor(os.time() * 1000) }
	rs.fire(self, "SessionChanged", sid)
end
M["SessionService.ReplaceSession"] = function(self, sid, newSid)
	local old = sess_map[sid]
	sess_map[sid] = nil
	sess_map[tostring(newSid or "")] = old or { tag = tostring(newSid or ""), meta = {}, created = math.floor(os.time() * 1000) }
	rs.fire(self, "SessionChanged", newSid)
end
M["SessionService.RemoveSession"] = function(self, sid)
	sess_map[sid] = nil
	rs.fire(self, "SessionChanged", sid)
end
M["SessionService.SessionExists"] = function(_, sid) return sess_map[sid] ~= nil end
M["SessionService.GetSessionID"] = function(_, sid) return sess_map[sid] and tostring(sid) or "" end
M["SessionService.GetSessionTag"] = function(_, sid) local s = sess_find(sid) return s and s.tag or "" end
M["SessionService.SetMetadata"] = function(_, sid, k, v)
	local s = sess_find(sid)
	if s then s.meta[k] = v end
end
M["SessionService.GetMetadata"] = function(_, sid, k)
	local s = sess_find(sid)
	return s and s.meta[k] or nil
end
M["SessionService.RemoveMetadata"] = function(_, sid, k)
	local s = sess_find(sid)
	if s then s.meta[k] = nil end
end
M["SessionService.RemoveSessionsWithMetadataKey"] = function(_, k)
	for sid, s in pairs(sess_map) do
		if s.meta[k] ~= nil then sess_map[sid] = nil end
	end
end
M["SessionService.GetCreatedTimestampUtcMs"] = function(_, sid)
	local s = sess_find(sid)
	return s and s.created or 0
end
M["SessionService.GetRootSID"] = function() return sess_root end
M["SessionService.GetBreadcrumbs"] = function() return {} end
M["SessionService.GetHistory"] = function() return {} end
M["SessionService.GenerateSessionInfoString"] = function() return "" end
M["SessionService.AcquireContextFocus"] = function(_, ctx) sess_focus[ctx] = true end
M["SessionService.ReleaseContextFocus"] = function(_, ctx) sess_focus[ctx] = nil end
M["SessionService.IsContextFocused"] = function(_, ctx) return sess_focus[ctx] == true end

------------------------------------------------------------ ExperienceService
local function dummy_conn()
	return { Connected = true, Disconnect = function(c) c.Connected = false end }
end
M["ExperienceService.IsInExperience"] = function() return true end
M["ExperienceService.GetPlaceJoinState"] = function() return "" end
M["ExperienceService.GetQueuePosition"] = function() return 0 end
M["ExperienceService.GetFollowUserId"] = function() return 0 end
M["ExperienceService.GetPendingJoinAttempt"] = function() return {} end
M["ExperienceService.ConsumePendingExperienceLeaveWithReason"] = function() return {} end
M["ExperienceService.LaunchExperience"] = function() return "" end
M["ExperienceService.LaunchExperienceFromSource"] = function() return "" end
M["ExperienceService.LaunchExperienceFromSourceWithCallback"] = function() end
M["ExperienceService.LeaveExperienceWithReason"] = function() end
M["ExperienceService.ExecuteCrossExperienceCall"] = function() end
M["ExperienceService.StartCrossExperience"] = function() end
M["ExperienceService.StopCrossExperience"] = function() end
M["ExperienceService.RegisterForExperienceJoin"] = function() return dummy_conn() end
M["ExperienceService.RegisterForExperienceLeave"] = function() return dummy_conn() end

------------------------------------------------------------ PolicyService / SafetyService / PermissionsService / UserGameSettings
M["PolicyService.GetPolicyInfoForServerRobloxOnlyAsync"] = function()
	return { ArePaidRandomItemsRestricted = false, IsSubjectToChinaPolicies = false }
end
M["SafetyService.DecodeAvatarMovementProto"] = function() return {} end
M["SafetyService.TakeScreenshot"] = function() return 0 end
for _, n in {
	"ReportBuildUIClose", "ReportBuildUIOpen", "ReportCapturesUIClose", "ReportCapturesUIOpen",
	"ReportChatLineReportingClose", "ReportChatLineReportingOpen",
	"ReportChatSuspensionDialogClose", "ReportChatSuspensionDialogOpen",
	"ReportMenuTabClose", "ReportMenuTabOpen",
	"ReportPartyChatWindowClose", "ReportPartyChatWindowOpen",
} do
	M["SafetyService." .. n] = function() end
end
M["PermissionsService.GetIsThirdPartyAssetAllowed"] = function() return true end
M["PermissionsService.GetIsThirdPartyPurchaseAllowed"] = function() return true end
M["PermissionsService.GetIsThirdPartyTeleportAllowed"] = function() return true end
M["PermissionsService.GetPermissions"] = function() return {} end
M["PermissionsService.SetPermissions"] = function() end

local tutorials = {}
M["UserGameSettings.GetDefaultFramerateCap"] = function() return 60 end
M["UserGameSettings.GetTutorialState"] = function(_, id) return tutorials[tostring(id)] == true end
M["UserGameSettings.SetTutorialState"] = function(_, id, v) tutorials[tostring(id)] = v == true end
M["UserGameSettings.ResetOnboardingCompleted"] = function() end

------------------------------------------------------------ ThirdPartyUserService
local function chat_not_restricted()
	local ok, v = pcall(function() return Enum.ChatRestrictionStatus.NotRestricted end)
	return ok and v or nil
end
M["ThirdPartyUserService.GetUserPlatformName"] = function() return "" end
M["ThirdPartyUserService.GetVoiceChatRestrictionStatus"] = function() return chat_not_restricted() end
M["ThirdPartyUserService.HaveActiveUser"] = function() return false end
M["ThirdPartyUserService.IsAccountSwitchingSupported"] = function() return false end
M["ThirdPartyUserService.IsChatRestrictionSupported"] = function() return false end
M["ThirdPartyUserService.IsSingleSignOnSupported"] = function() return false end
M["ThirdPartyUserService.ShowAccountPicker"] = function() end

------------------------------------------------------------ NotificationService
M["NotificationService.GetScheduledNotifications"] = function() return {} end
for _, n in {
	"ActionEnabled", "ActionTaken", "CancelAllNotification", "CancelNotification",
	"ScheduleNotification", "SubscribeToRccEventNamespace", "SubscribeToTopic",
	"SwitchedToAppShellFeature", "UnsubscribeFromTopic",
} do
	M["NotificationService." .. n] = function() end
end

------------------------------------------------------------ AdService
M["AdService.GetAdTeleportInfo"] = function() return 0, 0 end
M["AdService.GetReportAdInfo"] = function() return {} end
M["AdService.GetUniversalAppAdsEligibility"] = function() return {} end
M["AdService.GetAdAvailabilityNowForUniverseAsync"] = function() return {} end
M["AdService.IsAdLoaded"] = function() return false end
for _, n in {
	"HandleWhyThisAdClicked", "HideEudsaDisclosure", "OnDemandVideoCompleteFromUI",
	"ReturnToPublisherExperience", "SetAdGuiInteractivityHandlerInitialized", "SubmitAdNotification",
} do
	M["AdService." .. n] = function() end
end

------------------------------------------------------------ AchievementService
M["AchievementService.IsAvailable"] = function() return false end
M["AchievementService.GrantAchievement"] = function() return false end
M["AchievementService.HasAchieved"] = function() return false end

------------------------------------------------------------ CaptureService (CoreScript members)
M["CaptureService.CanCaptureVideo"] = function() return false end
M["CaptureService.IsCapturingVideo"] = function() return false end
M["CaptureService.RetrieveCaptures"] = function() return {} end
M["CaptureService.GetScreenshotCaptureObject"] = function() return nil end
M["CaptureService.PreCaptureShared"] = function() return "" end
M["CaptureService.PreVideoCaptureShared"] = function() return "" end
M["CaptureService.CreatePostAsync"] = function() return {} end
M["CaptureService.DeleteCapturesAsync"] = function() return 0 end
M["CaptureService.DeleteVideoCaptureAsync"] = function() return false end
M["CaptureService.GetCaptureFilePathAsync"] = function() return "" end
M["CaptureService.GetCaptureSizeAsync"] = function() return Vector2.new(0, 0) end
M["CaptureService.GetCaptureStorageSizeAsync"] = function() return 0 end
M["CaptureService.GetCaptureUploadDataAsync"] = function() return {} end
M["CaptureService.SaveCapturesToExternalStorageAsync"] = function() return 0 end
M["CaptureService.UploadPostAsync"] = function() return {} end
M["CaptureService.StartVideoCaptureForMCPAsync"] = function() return Enum.VideoCaptureStartedResult.OtherError end
M["CaptureService.StartVideoCaptureInternalAsync"] = function() return Enum.VideoCaptureStartedResult.OtherError end
for _, n in {
	"DeleteCapture", "DeleteVideoCapture", "OnCaptureBegan", "OnCaptureEnded",
	"OnCaptureObjectShared", "OnCapturePermissionsPromptFinished", "OnCaptureShared",
	"OnSavePromptFinished", "OnSharePromptFinished", "OnVideoCaptureShared",
	"SaveCaptureObjectToExternalStorage", "SaveCaptureToExternalStorage",
	"SaveScreenshotCapture", "SaveVideoCaptureToExternalStorage",
	"StopVideoCaptureForMCP", "StopVideoCaptureInternal", "UploadCaptureAndPostMoment",
	"CheckMomentTextStatusAsync", "CheckUploadCaptureStatusForSupportTicketAsync",
	"GenerateMomentTextAsync", "StartUploadCaptureForSupportTicketAsync",
} do
	M["CaptureService." .. n] = function() end
end

------------------------------------------------------------ HttpService / HttpRequest (CoreScript internal)
local http_req_cancelled = setmetatable({}, { __mode = "k" })
local http_req_opts = setmetatable({}, { __mode = "k" })
M["HttpService.RequestInternal"] = function(_, options)
	local req = rs.new_object("HttpRequest")
	if type(options) == "table" then
		http_req_opts[req] = {
			Url = options.Url,
			Method = options.Method or "GET",
			Headers = options.Headers,
			Body = options.Body,
		}
	end
	return req
end
M["HttpService.GetHttpEnabled"] = function(self) return self.HttpEnabled end
M["HttpService.SetHttpEnabled"] = function(self, on) self.HttpEnabled = on == true end
M["HttpService.GetUserAgent"] = function() return "Roblox/WinInet" end
M["HttpService.JSONDecodeAsync"] = function(self, input) return self:JSONDecode(input) end
M["HttpService.JSONEncodeAsync"] = function(self, obj) return self:JSONEncode(obj) end
M["HttpRbxApiService.GetDocumentationUrl"] = function(_, partial)
	return "https://create.roblox.com/docs/" .. tostring(partial or "")
end
M["HttpRequest.Cancel"] = function(self) http_req_cancelled[self] = true end
M["HttpRequest.Start"] = function(self, cb)
	if type(cb) ~= "function" then fail("Argument 1 missing or nil") end
	local opts = http_req_opts[self]
	if not opts or type(opts.Url) ~= "string" then
		task.defer(function()
			if http_req_cancelled[self] then return end
			cb(false, {
				StatusCode = 0,
				StatusMessage = "InvalidUrl",
				Body = "",
				Headers = {},
				HttpError = Enum.HttpError.InvalidUrl,
			})
		end)
		return
	end
	local id, err = rs.http_internal_start(tostring(opts.Method or "GET"), opts.Url, opts.Headers, opts.Body)
	if not id then
		if rs.http_debug then
			print(string.format("[HTTP-INT-ERR] %s %s -> %s",
				tostring(opts.Method or "GET"), tostring(opts.Url), tostring(err)))
		end
		task.defer(function()
			if http_req_cancelled[self] then return end
			cb(false, {
				StatusCode = 0,
				StatusMessage = tostring(err or "ConnectFail"),
				Body = "",
				Headers = {},
				HttpError = Enum.HttpError.ConnectFail,
			})
		end)
		return
	end
	task.spawn(function()
		while true do
			task.wait()
			if http_req_cancelled[self] then return end
			local r = table.pack(rs.http_poll(id))
			if r.n > 0 then
				local okk, status, msg, headers, body = r[1], r[2], r[3], r[4], r[5]
				if http_req_cancelled[self] then return end
				if rs.http_debug then
					print(string.format("[HTTP-INT] %s %s -> ok=%s status=%s len=%d",
						tostring(opts.Method or "GET"), tostring(opts.Url), tostring(okk), tostring(status),
						type(body) == "string" and #body or 0))
				end
				if okk then
					cb(true, {
						Success = status >= 200 and status < 300,
						StatusCode = status,
						StatusMessage = msg or "",
						Body = body or "",
						Headers = headers or {},
						HttpError = Enum.HttpError.OK,
						Stats = {
							WasRequestRetried = false,
							DurationInQueue = 0,
							DurationNameLookup = 0,
							DurationConnect = 0,
							DurationSSLHandshake = 0,
							DurationMakeRequest = 0,
							DurationReceiveResponse = 0,
						},
					})
				else
					cb(false, {
						Success = false,
						StatusCode = 0,
						StatusMessage = tostring(status or "ConnectFail"),
						Body = "",
						Headers = {},
						HttpError = Enum.HttpError.ConnectFail,
					})
				end
				return
			end
		end
	end)
end

------------------------------------------------------------ AvatarEditorService / GenericChallengeService
for _, n in {
	"NoPromptApplyProfileConfiguration", "NoPromptCreateOutfit", "NoPromptDeleteOutfit",
	"NoPromptRenameOutfit", "NoPromptSaveAvatar", "NoPromptSaveAvatarThumbnailCustomization",
	"NoPromptSetFavorite", "NoPromptUpdateOutfit",
} do
	M["AvatarEditorService." .. n] = function() return false end
end
for _, n in {
	"BustAvatarFetchCache", "PerformCreateOutfitWithDescription", "PerformDeleteOutfit",
	"PerformRenameOutfit", "PerformSaveAvatarWithDescription", "PerformSetFavorite",
	"PerformUpdateOutfit", "SetAllowInventoryReadAccess", "SignalCreateOutfitFailed",
	"SignalCreateOutfitPermissionDenied", "SignalDeleteOutfitFailed",
	"SignalDeleteOutfitPermissionDenied", "SignalRenameOutfitFailed",
	"SignalRenameOutfitPermissionDenied", "SignalSaveAvatarFailed",
	"SignalSaveAvatarPermissionDenied", "SignalSetFavoriteFailed",
	"SignalSetFavoritePermissionDenied", "SignalUpdateOutfitFailed",
	"SignalUpdateOutfitPermissionDenied", "refreshAvatarThumbnails",
} do
	M["AvatarEditorService." .. n] = function() end
end
for _, n in {
	"SignalChallengeAbandoned", "SignalChallengeCompleted",
	"SignalChallengeInvalidated", "SignalChallengeLoaded", "SignalChallengeRequired",
} do
	M["GenericChallengeService." .. n] = function() end
end

------------------------------------------------------------ AvatarChatService / VideoCaptureService / CoreGui / EventIngest / Social / Group / AvatarCreation / Marketplace
M["AvatarChatService.DebugCounterGet"] = function() return 0 end
M["AvatarChatService.EnableVoice"] = function() return false end
M["AvatarChatService.IsEnabled"] = function() return false end
M["AvatarChatService.IsPlaceEnabled"] = function() return false end
M["AvatarChatService.IsUniverseEnabled"] = function() return false end
M["AvatarChatService.PollClientFeatures"] = function() return 0 end
M["AvatarChatService.PollServerFeatures"] = function() return 0 end
M["AvatarChatService.deviceMeetsRequirementsForFeature"] = function() return false end
M["AvatarChatService.GetClientFeaturesAsync"] = function() return 0 end
M["AvatarChatService.GetServerFeaturesAsync"] = function() return 0 end

M["VideoCaptureService.GetCameraDevices"] = function() return {} end
M["CoreGui.SetUserGuiRendering"] = function() end
M["CoreGui.TakeScreenshot"] = function() end
M["CoreGui.ToggleRecording"] = function() end
for _, n in { "SendEventDeferred", "SendEventImmediately", "SetRBXEvent", "SetRBXEventStream" } do
	M["EventIngestService." .. n] = function() end
end
for _, n in {
	"InvokeGameInvitePromptClosed", "InvokeIrisInvite", "InvokeIrisInvitePromptClosed",
	"InvokeShareSheetClosed", "PromptRsvpToEventCompleted", "SignalFeedbackSubmissionCompleted",
	"SignalFeedbackSubmissionPermissionDenied", "UpdatePlayerPartyData",
} do
	M["SocialService." .. n] = function() end
end
M["GroupService.PromptJoinCompleted"] = function() end
M["AvatarCreationService.DeserializeAvatarModel"] = function() return nil end
M["AvatarCreationService.HandleSelfieConsentResult"] = function() end
M["AvatarCreationService.HandleSelfieQRResult"] = function() end
M["AvatarCreationService.CreateCageMeshPartsWithScaleForExportAsync"] = function() return Instance.new("Folder") end

M["MarketplaceService.IsPurchaseSimulated"] = function() return false end
M["MarketplaceService.PlayerCanMakePurchases"] = function() return false end
M["MarketplaceService.GetAvailableSubscriptionProductsAsync"] = function() return {} end
M["MarketplaceService.GetRobuxBalance"] = function() return 0 end
M["MarketplaceService.GetSubscriptionPurchaseInfoAsync"] = function() return {} end
M["MarketplaceService.GetUserSubscriptionDetailsInternalAsync"] = function() return {} end
M["MarketplaceService.PerformBulkPurchase"] = function() return {} end
M["MarketplaceService.PerformPurchase"] = function() return {} end
M["MarketplaceService.PerformPurchaseV2"] = function() return {} end
M["MarketplaceService.PerformSubscriptionPurchase"] = function() return "" end
for _, n in {
	"ClearProductInfoCaches", "PrepareCollectiblesPurchase", "PromptCollectiblesPurchase",
	"PromptNativePurchase", "PromptNativePurchaseWithLocalPlayer",
	"PromptNativePurchaseWithLocalPlayerWithPaymentSessionId", "PromptNativePurchaseWithPaymentSessionId",
	"PromptRobloxPurchase", "PromptThirdPartyPurchase", "RedeemGamePassFreeTrial",
	"RefreshBulkPurchase", "ReportAssetSale", "ReportRobuxUpsellStarted",
	"SignalAssetTypePurchased", "SignalCheckPlayerHasRobloxSubscription",
	"SignalClientPurchaseSuccess", "SignalMockPurchasePremium", "SignalMockPurchaseRobloxSubscription",
	"SignalPromptBulkPurchaseFinished", "SignalPromptBundlePurchaseFinished",
	"SignalPromptGamePassPurchaseFinished", "SignalPromptPremiumPurchaseFinished",
	"SignalPromptProductPurchaseFinished", "SignalPromptPurchaseFinished",
	"SignalPromptRobloxSubscriptionPurchaseFinished", "SignalPromptSubscriptionPurchaseFinished",
	"SignalRobuxTransferCompleted", "SignalServerLuaDialogClosed",
	"SignalUserSubscriptionStatusChanged", "PerformCancelSubscription",
	"PerformSubscriptionPurchaseV2", "PerformSubscriptionPurchaseV3Async",
	"PerformSubscriptionPurchaseWithRobuxAsync",
} do
	M["MarketplaceService." .. n] = function() end
end
M["MarketplaceService.SignalPromptPurchaseFinished"] = function(self, player, assetId, isPurchased)
	rs.fire(self, "PromptPurchaseFinished", player, assetId, isPurchased == true)
end
M["MarketplaceService.SignalPromptGamePassPurchaseFinished"] = function(self, player, gamePassId, isPurchased)
	rs.fire(self, "PromptGamePassPurchaseFinished", player, gamePassId, isPurchased == true)
end
M["MarketplaceService.SignalPromptProductPurchaseFinished"] = function(self, userId, productId, isPurchased)
	rs.fire(self, "PromptProductPurchaseFinished", userId, productId, isPurchased == true)
end
M["MarketplaceService.SignalPromptBundlePurchaseFinished"] = function(self, player, bundleId, isPurchased)
	rs.fire(self, "PromptBundlePurchaseFinished", player, bundleId, isPurchased == true)
end
M["MarketplaceService.SignalPromptSubscriptionPurchaseFinished"] = function(self, player, subId, isPurchased)
	rs.fire(self, "PromptSubscriptionPurchaseFinished", player, subId, isPurchased == true)
end
M["MarketplaceService.SignalPromptPremiumPurchaseFinished"] = function(self, player)
	rs.fire(self, "PromptPremiumPurchaseFinished", player)
end

------------------------------------------------------------ PlayerScripts / TelemetryService / AudioFocusService
local function copy_modes(bucket, default_list)
	local src = __rs_ps_modes and __rs_ps_modes[bucket]
	if src == nil then src = default_list end
	if not src then return {} end
	local out = {}
	for i, v in ipairs(src) do out[i] = v end
	return out
end
M["PlayerScripts.GetRegisteredComputerCameraMovementModes"] = function() return copy_modes("ComputerCamera") end
M["PlayerScripts.GetRegisteredComputerMovementModes"] = function()
	return copy_modes("ComputerMovement", {
		Enum.ComputerMovementMode.Default,
		Enum.ComputerMovementMode.KeyboardMouse,
		Enum.ComputerMovementMode.ClickToMove,
	})
end
M["PlayerScripts.GetRegisteredTouchCameraMovementModes"] = function() return copy_modes("TouchCamera") end
M["PlayerScripts.GetRegisteredTouchMovementModes"] = function() return copy_modes("TouchMovement") end
for _, n in { "LogCounter", "LogDurationEvent", "LogDurationEventWithTimestamp", "LogEvent", "LogStat" } do
	M["TelemetryService." .. n] = function() end
end
M["AudioFocusService.AcquireFocus"] = function() return true end
M["AudioFocusService.GetFocusedContextId"] = function() return 0 end
M["AudioFocusService.GetRegisteredContexts"] = function() return {} end
M["AudioFocusService.RegisterContextIdFromLua"] = function() return true end
M["AudioFocusService.RequestFocus"] = function() return true end
M["SoundService.GetInputDevice"] = function() return "", "" end
M["SoundService.GetInputDevices"] = function() return {}, {}, 0 end
M["SoundService.GetOutputDevice"] = function() return "System Default", "" end
M["SoundService.GetOutputDevices"] = function() return { "System Default" }, { "" }, 1 end
M["SoundService.SetOutputDevice"] = function() end
M["SoundService.GetRecordingDevices"] = function() return {} end
M["SoundService.SetRecordingDevice"] = function() end
M["SoundService.SetSoundEnabled"] = function() end
M["SoundService.GetAudioApiByDefault"] = function() return false end
M["SoundService.SetAudioApiByDefault"] = function() end
M["SoundService.GetPeakLevel"] = function() return 0 end
M["SoundService.GetListenerWorldCFrame"] = function() return CFrame.new() end
M["SoundService.GetAudioInstances"] = function() return {} end
M["SoundService.GetSoundMemoryData"] = function() return {} end

------------------------------------------------------------ MaterialService
local mat_fill_tool_enabled = false
local mat_override_signals = {}
local function mat_key(m)
	return typeof(m) == "EnumItem" and m.Name or tostring(m or "")
end
M["MaterialService.GetIsMaterialActionAsToolEnabled"] = function()
	return mat_fill_tool_enabled
end
M["MaterialService.ToggleMaterialFillToolEnabled"] = function(self)
	mat_fill_tool_enabled = not mat_fill_tool_enabled
	rs.fire(self, "MaterialFillToolEnabledChanged")
end
M["MaterialService.GetOverrideStatus"] = function()
	local ok, v = pcall(function() return Enum.PropertyStatus.Ok end)
	return ok and v or nil
end
M["MaterialService.GetMaterialOverrideChanged"] = function(_, mat)
	local k = mat_key(mat)
	local be = mat_override_signals[k]
	if not be then
		be = Instance.new("BindableEvent")
		mat_override_signals[k] = be
	end
	return be.Event
end
M["MaterialService.SetCurrentMaterial"] = function() end

------------------------------------------------------------ Animator CoreScript members
M["Animator.GetPlayingAnimationTracksCoreScript"] = function(self)
	return self:GetPlayingAnimationTracks()
end
M["Animator.LoadAnimationCoreScript"] = function(self, anim)
	return self:LoadAnimation(anim)
end
M["Animator.StepAnimationsInternal"] = function() end
M["Animator.SynchronizeWith"] = function() end

return M
