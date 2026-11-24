-- rustudio engine library, players_chat domain (src/domains/players_chat.rs):
-- the members of Player / Players / Chat / TextChatService that yield, and
-- the TextChatService message flow, as measured in Studio's Play test
-- (checks/parity/players_chat.md). Loaded once per runtime.
local M, R = ...
-- R: Rust helpers (players_chat.rs `install`):
--   fire(inst, event, ...), create(class), side() -> "Server"|"Client"|"Run",
--   web(kind, a, b) -> 0, value | 1, error | 2 (pending), guid(),
--   load_character(player), kick(player, message), pages(inst, rows, size)

-- Services are looked up when used (this loads before `game` exists).
local svc = setmetatable({}, { __index = function(_, k) return game:GetService(k) end })
local add_bubble -- (part, message instance or nil, text), set further down

local function side() return R.side() end
local function on_server() return side() ~= "Client" end

-- Engine errors carry no position (pcall gets the bare text).
local function fail(msg) error(msg, 0) end

-- Polls a web lookup until it's answered, yielding between polls.
local function web(kind, a, b)
	while true do
		local state, v = R.web(kind, a, b)
		if state == 0 then return v end
		if state == 1 then return nil, v end
		task.wait()
	end
end

local function is_a(v, class)
	return typeof(v) == "Instance" and v:IsA(class)
end

local function user_id_arg(v, what)
	local n = tonumber(v)
	if n == nil then fail("Argument " .. (what or 1) .. " missing or nil") end
	return math.floor(n)
end

-------------------------------------------------------------- property store
-- TextChat objects keep their engine state here (players_chat.rs reads it
-- through __rs_pc_get / writes through __rs_pc_set).
local P = setmetatable({}, { __mode = "k" })
local function props(inst)
	local t = P[inst]
	if not t then t = {}; P[inst] = t end
	return t
end
function __rs_pc_get(inst, key)
	local t = P[inst]
	if t == nil then return false end
	local v = t[key]
	if v == nil then return false end
	return true, v
end
function __rs_pc_set(inst, key, value)
	props(inst)[key] = value
end

---------------------------------------------------------------- Player

-- Group membership through the session (groups roles list, as Studio's
-- engine asks the groups API), cached per user like Studio.
local function group_row(player, group, what)
	if not is_a(player, "Player") then fail("Player:" .. what .. " error: Player not in datamodel") end
	local gid = user_id_arg(group)
	local rows, err = web("groups", player.UserId)
	if rows == nil then fail("Player:" .. what .. " failed because " .. tostring(err)) end
	for _, r in rows do
		if r.id == gid then return r end
	end
	return nil
end
local function in_group(self, group) return group_row(self, group, "IsInGroup") ~= nil end
local function rank_in_group(self, group)
	local r = group_row(self, group, "GetRankInGroup")
	return r and r.rank or 0
end
local function role_in_group(self, group)
	local r = group_row(self, group, "GetRoleInGroup")
	return r and r.role or "Guest"
end
M.IsInGroup, M.IsInGroupAsync = in_group, in_group
M.GetRankInGroup, M.GetRankInGroupAsync = rank_in_group, rank_in_group
M.GetRoleInGroup, M.GetRoleInGroupAsync = role_in_group, role_in_group

local function friends_with(self, other)
	local uid = user_id_arg(other)
	if uid == self.UserId then return false end
	local v, err = web("friend", self.UserId, uid)
	if v == nil then fail(tostring(err)) end
	return v
end
M.IsFriendsWith, M.IsFriendsWithAsync = friends_with, friends_with
-- Roblox retired best friends: the friends lookup runs (its errors come
-- through, as in Studio) and nobody is a best friend.
M.IsBestFriendsWith = function(self, other)
	friends_with(self, other)
	return false
end

local function friends_online(self, max)
	max = tonumber(max) or 200
	if max < 0 then fail("maxFriends argument can not be negative") end
	local rows, err = web("friends_online", self.UserId)
	if rows == nil then fail(tostring(err)) end
	local out = {}
	for i, r in rows do
		if i > max then break end
		out[i] = table.clone(r)
	end
	return out
end
M.GetFriendsOnline, M.GetFriendsOnlineAsync = friends_online, friends_online

-- Friends in this server: Studio answers "Not Implemented".
M.GetFriendsInServerAsync = function(self) fail("Not Implemented") end
-- An unpublished / local place has no universe: no friend is in it or
-- played it (the lookups need the universe id).
M.GetFriendsInUniverseAsync = function(self)
	local rows = friends_online(self, 200)
	local out = {}
	local gid = game.GameId
	if gid ~= 0 then
		for _, r in rows do
			if r.GameId ~= nil and r.PlaceId == game.PlaceId then table.insert(out, r) end
		end
	end
	return out
end
M.GetFriendsWhoPlayedAsync = function(self) return {} end

-- LoadCharacter: server only; yields until the new character is added.
local function load_character(self, desc)
	if not on_server() then fail("LoadCharacter can only be called by the backend server") end
	if not is_a(self, "Player") then fail("Expected ':' not '.' calling member function LoadCharacter") end
	if self.Parent == nil then fail("Player:LoadCharacter() failed because the player is no longer in the game") end
	local done = false
	local conn
	conn = self.CharacterAdded:Connect(function(char)
		done = true
		conn:Disconnect()
		if desc then
			local hum = char:FindFirstChildOfClass("Humanoid")
			if hum then pcall(hum.ApplyDescription, hum, desc) end
		end
	end)
	R.load_character(self)
	local t0 = os.clock()
	while not done do
		task.wait()
		-- A Run (F8) session has no character pipeline.
		if side() == "Run" and os.clock() - t0 > 5 then conn:Disconnect(); return end
	end
end
M.LoadCharacter = function(self) load_character(self) end
M.LoadCharacterAsync = M.LoadCharacter
M.LoadCharacterBlocking = M.LoadCharacter
local function load_with_desc(self, desc)
	if not is_a(desc, "HumanoidDescription") then fail("Unable to cast value to Object") end
	load_character(self, desc)
end
M.LoadCharacterWithHumanoidDescription = load_with_desc
M.LoadCharacterWithHumanoidDescriptionAsync = load_with_desc
-- Avatar rules come from the experience's settings; a Studio Play test
-- uses the place's (StarterPlayer) ones, which the loader applies.
M.LoadCharacterWithAvatarRulesAsync = function(self) load_character(self) end

M.RequestStreamAroundAsync = function(self, pos, timeout)
	if typeof(pos) ~= "Vector3" then fail("Unable to cast value to Vector3") end
	if not workspace.StreamingEnabled then fail("Workspace.StreamingEnabled is false, cannot RequestStreamAroundAsync") end
	if timeout ~= nil and typeof(timeout) ~= "number" then fail("Unable to cast value to double") end
	local done = R.stream_around(self, pos)
	local deadline = time() + (timeout or math.huge)
	repeat task.wait() until done() or time() >= deadline
end

M.Kick = function(self, message)
	if not is_a(self, "Player") then fail("Expected ':' not '.' calling member function Kick") end
	if side() == "Client" and self ~= svc.Players.LocalPlayer then
		fail("Cannot kick a non-local Player from a LocalScript")
	end
	R.kick(self, message ~= nil and tostring(message) or "")
end

---------------------------------------------------------------- Players

local function lookup_id_by_name(self, name)
	if type(name) ~= "string" then fail("Argument 1 missing or nil") end
	for _, p in svc.Players:GetPlayers() do
		if p.Name:lower() == name:lower() then return p.UserId end
	end
	local v, err = web("id_by_name", name)
	if v == nil then fail("Players:GetUserIdFromNameAsync() failed: " .. tostring(err)) end
	return v
end
M.GetUserIdFromNameAsync = lookup_id_by_name
local function lookup_name_by_id(self, id)
	local uid = user_id_arg(id)
	local p = svc.Players:GetPlayerByUserId(uid)
	if p then return p.Name end
	local v, err = web("name_by_id", uid)
	-- (Studio's own message names the other function.)
	if v == nil then fail("Players:GetUserIdFromNameAsync() failed: " .. tostring(err)) end
	return v
end
M.GetNameFromUserIdAsync = lookup_name_by_id

M.GetFriendsAsync = function(self, id)
	local uid = user_id_arg(id)
	local rows, err = web("friends", uid)
	if rows == nil then fail(tostring(err)) end
	local pages = R.create("FriendPages")
	R.pages(pages, rows, 50)
	return pages
end

-- Bans: Studio validates the config, then skips the request in a test
-- session ("{}Async will succeed on production game servers…").
local function check_ban_config(cfg, unban)
	if type(cfg) ~= "table" then fail("Unable to cast value to Dictionary") end
	if type(cfg.UserIds) ~= "table" then fail("Config must contain field UserIds with type array") end
	if unban then
		if type(cfg.ApplyToUniverse) ~= "boolean" then fail("Config must contain field ApplyToUniverse with type boolean") end
	else
		if type(cfg.Duration) ~= "number" then fail("Config must contain field Duration with type number") end
		if type(cfg.DisplayReason) ~= "string" then fail("Config must contain field DisplayReason with type string") end
		if type(cfg.PrivateReason) ~= "string" then fail("Config must contain field PrivateReason with type string") end
		if cfg.Duration <= 0 and cfg.Duration ~= -1 then fail("Duration must be a positive number or -1") end
		if #cfg.DisplayReason > 400 then fail("DisplayReason exceeded the maximum length of 400") end
		if #cfg.PrivateReason > 1000 then fail("PrivateReason exceeded the maximum length of 1000") end
	end
	if #cfg.UserIds > 50 then fail("UserIds exceeded the maximum length of 50") end
	for i, id in cfg.UserIds do
		if type(id) ~= "number" then fail("Expected type number in UserIds at index " .. tostring(i)) end
		if id <= 0 then fail("Invalid UserId: " .. tostring(id) .. ". All UserIds must be positive") end
	end
end
M.BanAsync = function(self, cfg)
	if not on_server() then fail("BanAsync() can only be called on the server") end
	check_ban_config(cfg, false)
	task.wait()
	print("BanAsync will succeed on production game servers. Skipping request in test environment...")
end
M.UnbanAsync = function(self, cfg)
	if not on_server() then fail("UnbanAsync() can only be called on the server") end
	check_ban_config(cfg, true)
	task.wait()
	print("UnbanAsync will succeed on production game servers. Skipping request in test environment...")
end
M.GetBanHistoryAsync = function(self, id)
	if not on_server() then fail("GetBanHistoryAsync() can only be called on the server") end
	local uid = tonumber(id)
	if uid == nil or uid <= 0 then fail("UserId must be a positive number") end
	print("GetBanHistoryAsync will succeed on production game servers. Request will fail in test environment.")
	task.wait()
	fail("HTTP 403 (Forbidden)")
end

---------------------------------------------------------------- legacy Chat

local chat_callbacks = {}
M.RegisterChatCallback = function(self, kind, fn)
	if typeof(kind) ~= "EnumItem" or not tostring(kind):find("^Enum%.ChatCallbackType%.") then
		fail("Unable to cast value to Enum.ChatCallbackType")
	end
	if fn ~= nil and type(fn) ~= "function" then fail("Unable to cast value to function") end
	chat_callbacks[kind.Name] = fn
end
M.InvokeChatCallback = function(self, kind, ...)
	if typeof(kind) ~= "EnumItem" or not tostring(kind):find("^Enum%.ChatCallbackType%.") then
		fail("Unable to cast value to Enum.ChatCallbackType")
	end
	local fn = chat_callbacks[kind.Name]
	if fn then return fn(...) end
end

-- Studio's chat doesn't filter (Studio has no text filter); the text comes
-- back. From a LocalScript Studio warns that this is deprecated.
local warned_local_filter = false
local function warn_local_filter()
	if side() == "Client" and not warned_local_filter then
		warned_local_filter = true
		warn("Calling Chat:FilterStringAsync or Chat:FilterStringForBroadcast from LocalScripts is deprecated and will be disabled in the future. Text filtering should be done from server-side Scripts. For more info see: https://devforum.roblox.com/t/deprecating-text-filtering-from-localscripts/46403")
	end
end
-- (__rs_lua_methods is keyed by name: TextService's FilterStringAsync,
-- loaded before this, keeps answering for TextService.)
local other_filter = M.FilterStringAsync
M.FilterStringAsync = function(self, text, from, to)
	if typeof(self) == "Instance" and not self:IsA("Chat") and other_filter then
		return other_filter(self, text, from, to)
	end
	if type(text) ~= "string" then fail("Argument 1 missing or nil") end
	if not is_a(from, "Player") then fail("Argument 2 missing or nil") end
	warn_local_filter()
	task.wait()
	return text
end
M.FilterStringForPlayerAsync = M.FilterStringAsync
M.FilterStringForBroadcast = function(self, text, from)
	if type(text) ~= "string" then fail("Argument 1 missing or nil") end
	if not is_a(from, "Player") then fail("Argument 2 missing or nil") end
	warn_local_filter()
	return text
end
-- A Studio Play test's player may chat (Roblox's own settings allow the
-- signed-in account to chat in its own test session).
local function can_chat(self, a) user_id_arg(a); task.wait(); return true end
local function can_chat2(self, a, b) user_id_arg(a); user_id_arg(b, 2); task.wait(); return true end
M.CanUserChatAsync = can_chat
M.CanUsersChatAsync = can_chat2

local UI = nil
local bubble_settings = nil
local muted_users = {} -- [userId] = true (client-side /mute)
local pending_whisper = nil -- { name = string, body = string }

local function escape_rich_text(s)
	return (tostring(s or ""):gsub("&", "&amp;"):gsub("<", "&lt;"):gsub(">", "&gt;"))
end

local function utf8_count(s)
	local ok, n = pcall(utf8.len, s)
	if ok and n then return n end
	return #s
end

local function tcs_enabled()
	local ok, v = pcall(function() return svc.TextChatService.ChatVersion end)
	return ok and v == Enum.ChatVersion.TextChatService
end

local function ensure_ui()
	if R.core_scripts_active and R.core_scripts_active() and tcs_enabled() then return nil end
	if UI == nil and side() == "Client" and R.load_ui then
		UI = R.load_ui({
			get_bubble_settings = function() return bubble_settings end,
			send_legacy = function(text)
				table.insert(__rs_pc_outbox, {
					kind = "legacy",
					channel = nil,
					text = text,
					raw_text = text,
					metadata = "",
					id = "",
					target_uid = 0,
				})
			end,
		})
	end
	return UI
end

local function resolve_adornee(part)
	if not part then return nil end
	if part:IsA("Model") then
		local cfg = svc.TextChatService:FindFirstChildOfClass("BubbleChatConfiguration")
		local name = cfg and cfg.AdorneeName or "HumanoidRootPart"
		return part:FindFirstChild(name) or part:FindFirstChild("Head") or part.PrimaryPart or part:FindFirstChildWhichIsA("BasePart")
	end
	return part
end

M.SetBubbleChatSettings = function(self, settings)
	if type(settings) ~= "table" then
		fail("Bad settings object passed to Chat:SetBubbleChatSettings:\ninside consolidated BubbleChat table expected, got " .. typeof(settings))
	end
	bubble_settings = settings
	R.fire(self, "BubbleChatSettingsChanged", settings)
	-- TextChatService's bubbles take the settings, as ExpChat maps them.
	local cfg = svc.TextChatService:FindFirstChildOfClass("BubbleChatConfiguration")
	if cfg then
		for k, v in settings do
			if type(k) == "string" then
				if k == "Transparency" then
					pcall(function() cfg.BackgroundTransparency = v end)
				elseif k == "CornerRadius" then
					local c = cfg:FindFirstChildOfClass("UICorner")
					if c then pcall(function() c.CornerRadius = v end) end
				elseif k == "Padding" and type(v) == "number" then
					local p = cfg:FindFirstChildOfClass("UIPadding")
					if p then
						local u = UDim.new(0, v)
						pcall(function() p.PaddingTop = u; p.PaddingBottom = u; p.PaddingLeft = u; p.PaddingRight = u end)
					end
				elseif k == "BackgroundGradient" and type(v) == "table" then
					local g = cfg:FindFirstChildOfClass("UIGradient")
					if g then
						for gk, gv in v do pcall(function() g[gk] = gv end) end
					end
				elseif k == "BackgroundImage" and type(v) == "table" then
					local img = cfg:FindFirstChildOfClass("ImageLabel")
					if img then
						for ik, iv in v do pcall(function() img[ik] = iv end) end
					end
				elseif k ~= "UserSpecificSettings" and k ~= "CornerEnabled" and k ~= "MaxWidth" and k ~= "SizeAnimation" and k ~= "TransparencyAnimation" and k ~= "Voice" then
					pcall(function() cfg[k] = v end)
				end
			end
		end
	end
end

-- Chat:Chat(part, message, color): a bubble over the part, and Chat.Chatted.
M.Chat = function(self, part, message, color)
	if typeof(part) ~= "Instance" then fail("Argument 1 missing or nil") end
	local raw = tostring(message or "")
	local col = color or Enum.ChatColor.Blue
	R.fire(self, "Chatted", part, raw, col)
	if part:IsDescendantOf(workspace) then
		local ui = ensure_ui()
		if ui then
			local ad = resolve_adornee(part)
			if ad then ui.add_bubble(ad, escape_rich_text(raw), nil, nil, col) end
		end
	end
end

------------------------------------------------------------ TextChatService

local STATUS = setmetatable({}, { __index = function(_, k) return Enum.TextChatMessageStatus[k] end })

local function channel_ok(ch, what)
	if not ch:IsDescendantOf(svc.TextChatService) then
		fail(what .. " error: TextChannel not a descendant of TextChatService")
	end
end

local function source_of(ch, uid)
	for _, s in ch:GetChildren() do
		if s:IsA("TextSource") and s.UserId == uid then return s end
	end
end

local function prefix_for(channel, sender)
	if not sender then return "" end
	local base = sender.DisplayName .. ":"
	if channel then
		local cn = channel.Name
		if cn:sub(1, 7) == "RBXTeam" then
			return "[Team] " .. base
		elseif cn:sub(1, 11) == "RBXWhisper:" then
			local lp = svc.Players.LocalPlayer
			local a, b = cn:sub(12):match("^(%-?%d+)_(%-?%d+)$")
			if lp and a and b then
				local other_id = tonumber(a) == lp.UserId and tonumber(b) or tonumber(a)
				local other = other_id and svc.Players:GetPlayerByUserId(other_id)
				if sender == lp and other then
					return "[To " .. other.DisplayName .. "] " .. base
				elseif other then
					return "[From " .. sender.DisplayName .. "] " .. base
				end
			end
		end
	end
	return base
end

local function new_message(fields)
	local m = R.create("TextChatMessage")
	local t = props(m)
	t.Text = fields.Text or ""
	t.TextChannel = fields.TextChannel
	t.TextSource = fields.TextSource
	t.Status = fields.Status or STATUS.Unknown
	t.MessageId = fields.MessageId or ""
	t.Metadata = fields.Metadata or ""
	t.PrefixText = fields.PrefixText or ""
	t.Translation = fields.Translation or ""
	t.Timestamp = fields.Timestamp or DateTime.now()
	return m
end

-- OnIncomingMessage: TextChatService FIRST, then TextChannel (verified in Studio:
-- both_order=TCS,CH). A returned TextChatMessageProperties mutates the message
-- so the channel callback observes any updates made by the service callback.
local function run_incoming(m)
	local ch = props(m).TextChannel
	for _, owner in { svc.TextChatService, ch } do
		if owner then
			local has, ok, res = __rs_call_callback(owner, "OnIncomingMessage", m)
			if has then
				if not ok then
					warn(tostring(res))
				elseif typeof(res) == "Instance" and res:IsA("TextChatMessageProperties") then
					local t = props(m)
					local function take(k)
						local v = res[k]
						if v ~= nil and v ~= "" then t[k] = v end
					end
					take("PrefixText"); take("Text"); take("Translation")
					if res:IsA("BubbleChatMessageProperties") then t.BubbleChatMessageProperties = res end
					if res:IsA("ChatWindowMessageProperties") then t.ChatWindowMessageProperties = res end
					for _, c in res:GetChildren() do
						if c:IsA("BubbleChatMessageProperties") then t.BubbleChatMessageProperties = c end
						if c:IsA("ChatWindowMessageProperties") then t.ChatWindowMessageProperties = c end
					end
				end
			end
		end
	end
end

local function ensure_window_props(m)
	local t = props(m)
	if t._win_done then return t.ChatWindowMessageProperties end
	if t.Status ~= STATUS.Success then return t.ChatWindowMessageProperties end
	t._win_done = true
	local cb = __rs_get_callback(svc.TextChatService, "OnChatWindowAdded")
	if cb then
		local ok, res = pcall(cb, m)
		if ok and typeof(res) == "Instance" and res:IsA("ChatWindowMessageProperties") then
			t.ChatWindowMessageProperties = res
			local function take(k)
				local v = res[k]
				if v ~= nil and v ~= "" then t[k] = v end
			end
			take("PrefixText"); take("Text"); take("Translation")
		elseif not ok then
			warn(tostring(res))
		end
	end
	return t.ChatWindowMessageProperties
end

local function ensure_bubble_props(m, adornee)
	if not m then return nil end
	local t = props(m)
	if t._bub_done then return t.BubbleChatMessageProperties end
	if t.Status ~= STATUS.Success then return t.BubbleChatMessageProperties end
	t._bub_done = true
	if adornee == nil then
		local src = t.TextSource
		local sender = src and svc.Players:GetPlayerByUserId(src.UserId)
		local char = sender and sender.Character
		if char then
			local cfg = svc.TextChatService:FindFirstChildOfClass("BubbleChatConfiguration")
			local name = cfg and cfg.AdorneeName or "HumanoidRootPart"
			adornee = char:FindFirstChild(name) or char:FindFirstChild("Head") or char.PrimaryPart
		end
	end
	local cb = __rs_get_callback(svc.TextChatService, "OnBubbleAdded")
	if cb then
		local ok, res = pcall(cb, m, adornee)
		if ok and typeof(res) == "Instance" and res:IsA("BubbleChatMessageProperties") then
			t.BubbleChatMessageProperties = res
		elseif not ok then
			warn(tostring(res))
		end
	end
	return t.BubbleChatMessageProperties
end

local function deliver_to_window(m)
	local t = props(m)
	local src = t.TextSource
	local player = src and svc.Players:GetPlayerByUserId(src.UserId)
	local color = nil
	if player and player.Team and player.Neutral == false then
		local ok, tc = pcall(function() return player.Team.TeamColor.Color end)
		color = (ok and tc) or player.TeamColor.Color
	end
	local wp = ensure_window_props(m)
	local ui = ensure_ui()
	if not ui then return end
	ui.add_line({
		prefix = t.PrefixText or "",
		text = t.Text or "",
		system = src == nil,
		sender_name = player and player.Name or nil,
		name_color = color,
		window_props = wp,
		channel = t.TextChannel,
		message_id = t.MessageId or "",
	})
end

local function deliver_to_bubble(adornee, m, text, user_id)
	local bp = ensure_bubble_props(m, adornee)
	local ui = ensure_ui()
	if not ui then return end
	ui.add_bubble(adornee, text, bp, user_id)
end

-- The character part a bubble hangs over (BubbleChatConfiguration.AdorneeName).
local function bubble_adornee(player)
	local char = player and player.Character
	if not char then return nil end
	local cfg = svc.TextChatService:FindFirstChildOfClass("BubbleChatConfiguration")
	local name = cfg and cfg.AdorneeName or "HumanoidRootPart"
	return char:FindFirstChild(name) or char:FindFirstChild("Head") or char.PrimaryPart
end

local function find_matching_commands(text)
	local word = text:match("^(%S+)")
	if not word then return {} end
	local lw = word:lower()
	local matched = {}
	for _, cmd in svc.TextChatService:GetDescendants() do
		if cmd:IsA("TextChatCommand") and cmd.Enabled then
			local a, b = cmd.PrimaryAlias, cmd.SecondaryAlias
			if (a ~= "" and lw == a:lower()) or (b ~= "" and lw == b:lower()) then
				table.insert(matched, cmd)
			end
		end
	end
	return matched
end

local function sys_notice(msg)
	local tc = svc.TextChatService:FindFirstChild("TextChannels")
	local sys = tc and tc:FindFirstChild("RBXSystem")
	if sys then
		sys:DisplaySystemMessage(msg)
	else
		local ui = ensure_ui()
		if ui then ui.add_line({ prefix = "", text = msg, system = true, message_id = "" }) end
	end
end

local function find_player_by_query(query)
	if not query or query == "" then return nil end
	local lq = query:lower()
	local prefix_match = nil
	for _, p in svc.Players:GetPlayers() do
		if p.Name:lower() == lq or p.DisplayName:lower() == lq then return p end
		if not prefix_match and (p.Name:lower():sub(1, #lq) == lq or p.DisplayName:lower():sub(1, #lq) == lq) then
			prefix_match = p
		end
	end
	return prefix_match
end

local function play_emote(name)
	local lp = svc.Players.LocalPlayer
	local hum = lp and lp.Character and lp.Character:FindFirstChildOfClass("Humanoid")
	if not hum or name == "" then return end
	task.spawn(function()
		if not pcall(hum.PlayEmote, hum, name) then
			local animate = lp.Character and lp.Character:FindFirstChild("Animate")
			local bf = animate and animate:FindFirstChild("PlayEmote")
			if bf and bf:IsA("BindableFunction") then
				pcall(bf.Invoke, bf, name:lower())
			end
		end
	end)
end

-- Execute Roblox's built-in TextChatCommands on the client.
local function run_builtin_command(cmd_name, raw_text)
	local rest = raw_text:match("^%S+%s+(.-)%s*$") or ""
	local lp = svc.Players.LocalPlayer
	if cmd_name == "RBXClearCommand" then
		local ui = ensure_ui()
		if ui then ui.clear_lines() end
	elseif cmd_name == "RBXHelpCommand" then
		sys_notice(
			"These are the basic chat commands.\n"
			.. "/me <text> : roleplaying command for doing actions.\n"
			.. "/c <channel> : switch channel menu tabs.\n"
			.. "/join <channel> or /j <channel> : join channel.\n"
			.. "/leave <channel> or /l <channel> : leave channel.\n"
			.. "/whisper <speaker> or /w <speaker> : open private message channel with speaker.\n"
			.. "/mute <speaker> : mute a speaker.\n"
			.. "/unmute <speaker> : unmute a speaker.\n"
			.. "/team <message> or /t <message> : send a team chat to people on your team.\n"
			.. "/clear or /cls : clear the chat window.\n"
			.. "/emote <emote> or /e <emote> : perform an emote."
		)
	elseif cmd_name == "RBXVersionCommand" then
		sys_notice("This experience is running TextChatService (ExperienceChat).")
	elseif cmd_name == "RBXConsoleCommand" then
		sys_notice("Developer console command triggered (/console).")
	elseif cmd_name == "RBXEmoteCommand" then
		if rest == "" then
			sys_notice("You can use /emote <emoteName> (wave, point, dance, dance2, dance3, laugh, cheer).")
		else
			play_emote(rest)
		end
	elseif cmd_name == "RBXTeamCommand" then
		if not lp or lp.Neutral or not lp.Team then
			sys_notice("You cannot team chat if you are not on a team.")
			return
		end
		local tc = svc.TextChatService:FindFirstChild("TextChannels")
		local team_ch = tc and tc:FindFirstChild("RBXTeam" .. lp.TeamColor.Name)
		if team_ch then
			local cib = svc.TextChatService:FindFirstChildOfClass("ChatInputBarConfiguration")
			if cib then pcall(function() cib.TargetTextChannel = team_ch end) end
			local ui = ensure_ui()
			if ui then ui.refresh_tabs() end
			if rest ~= "" then
				task.spawn(function() team_ch:SendAsync(rest) end)
			end
		else
			sys_notice("You cannot team chat if you are not on a team.")
		end
	elseif cmd_name == "RBXMuteCommand" then
		local target = find_player_by_query(rest)
		if not target then
			sys_notice("Player '" .. rest .. "' does not exist.")
		elseif target == lp then
			sys_notice("You cannot mute yourself.")
		else
			muted_users[target.UserId] = true
			sys_notice(target.DisplayName .. " has been muted.")
		end
	elseif cmd_name == "RBXUnmuteCommand" then
		local target = find_player_by_query(rest)
		if not target then
			sys_notice("Player '" .. rest .. "' does not exist.")
		else
			muted_users[target.UserId] = nil
			sys_notice(target.DisplayName .. " has been unmuted.")
		end
	elseif cmd_name == "RBXWhisperCommand" then
		local target_name, body = rest:match("^(%S+)%s*(.-)$")
		local target = find_player_by_query(target_name)
		if not target then
			sys_notice("Player '" .. tostring(target_name or "") .. "' does not exist.")
		elseif target == lp then
			sys_notice("You cannot whisper to yourself.")
		else
			local a, b = math.min(lp.UserId, target.UserId), math.max(lp.UserId, target.UserId)
			local cname = string.format("RBXWhisper:%d_%d", a, b)
			local tc = svc.TextChatService:FindFirstChild("TextChannels")
			local existing = tc and tc:FindFirstChild(cname)
			if existing then
				local cib = svc.TextChatService:FindFirstChildOfClass("ChatInputBarConfiguration")
				if cib then pcall(function() cib.TargetTextChannel = existing end) end
				local ui = ensure_ui()
				if ui then ui.refresh_tabs() end
				if body and body ~= "" then
					task.spawn(function() existing:SendAsync(body) end)
				end
			else
				pending_whisper = { name = cname, body = body or "" }
				table.insert(__rs_pc_outbox, {
					kind = "whisper_open",
					channel = nil,
					text = "",
					raw_text = "",
					metadata = "",
					id = "",
					target_uid = target.UserId,
				})
			end
		end
	end
end

__rs_pc_outbox = {}

M.SendAsync = function(self, text, metadata)
	if side() ~= "Client" then fail("TextChannel:SendAsync can only be called on the client.") end
	channel_ok(self, "SendAsync")
	local raw = tostring(text or "")
	local lp = svc.Players.LocalPlayer
	local src = lp and source_of(self, lp.UserId)
	local msg_id = tostring(lp and lp.UserId or 0) .. "-" .. svc.HttpService:GenerateGUID(true)
	local meta = metadata ~= nil and tostring(metadata) or ""

	-- Empty / whitespace-only: returns immediately with Sending & empty text, goes nowhere (Studio verified).
	if raw:match("^%s*$") then
		return new_message({
			Text = "",
			TextChannel = self,
			TextSource = src,
			Status = STATUS.Sending,
			MessageId = msg_id,
			Metadata = meta,
			PrefixText = prefix_for(self, lp),
		})
	end

	local escaped = escape_rich_text(raw)
	local m = new_message({
		Text = escaped,
		TextChannel = self,
		TextSource = src,
		Status = STATUS.Sending,
		MessageId = msg_id,
		Metadata = meta,
		PrefixText = prefix_for(self, lp),
	})

	-- Check TextChatCommands first (commands fire Triggered on client & server, don't broadcast as chat)
	local cmds = find_matching_commands(raw)
	if #cmds > 0 then
		for _, cmd in cmds do
			R.fire(cmd, "Triggered", src, raw)
			run_builtin_command(cmd.Name, raw)
		end
		table.insert(__rs_pc_outbox, {
			kind = "cmd",
			channel = self,
			text = escaped,
			raw_text = raw,
			metadata = meta,
			id = msg_id,
			target_uid = 0,
		})
		return m
	end

	run_incoming(m)
	R.fire(svc.TextChatService, "SendingMessage", m)

	-- Permission / length checks: SendAsync returns Sending immediately (Studio verified),
	-- then delivers the failure status asynchronously via MessageReceived.
	if src == nil or not src.CanSend then
		task.defer(function()
			local err_text = "You do not have permission to send messages to this channel"
			local fail_m = new_message({
				Text = err_text,
				TextChannel = self,
				TextSource = src,
				Status = src == nil and STATUS.InvalidPrivacySettings or STATUS.InvalidTextChannelPermissions,
				MessageId = msg_id,
				Metadata = meta,
				PrefixText = "",
			})
			run_incoming(fail_m)
			R.fire(svc.TextChatService, "MessageReceived", fail_m)
			R.fire(self, "MessageReceived", fail_m)
		end)
		return m
	end

	if utf8_count(raw) > 200 then
		task.defer(function()
			local fail_m = new_message({
				Text = "",
				TextChannel = self,
				TextSource = src,
				Status = STATUS.MessageTooLong,
				MessageId = msg_id,
				Metadata = meta,
				PrefixText = "",
			})
			run_incoming(fail_m)
			R.fire(svc.TextChatService, "MessageReceived", fail_m)
			R.fire(self, "MessageReceived", fail_m)
		end)
		return m
	end

	table.insert(__rs_pc_outbox, {
		kind = "msg",
		channel = self,
		text = escaped,
		raw_text = raw,
		metadata = meta,
		id = msg_id,
		target_uid = 0,
	})
	return m
end

M.SendInternalAsync = M.SendAsync
M.SendDictatedSpeechAsync = M.SendAsync
M.SendPresetAsync = M.SendAsync

M.DisplaySystemMessage = function(self, text, metadata)
	if side() ~= "Client" then fail("TextChannel:DisplaySystemMessage can only be called on the client.") end
	channel_ok(self, "DisplaySystemMessage")
	local m = new_message({
		Text = tostring(text or ""),
		TextChannel = self,
		Status = STATUS.Success,
		MessageId = "0-" .. svc.HttpService:GenerateGUID(true),
		Metadata = metadata ~= nil and tostring(metadata) or "",
	})
	run_incoming(m)
	R.fire_game(svc.TextChatService, "MessageReceived", m)
	R.fire_game(self, "MessageReceived", m)
	deliver_to_window(m)
	R.fire_core(svc.TextChatService, "MessageReceived", m)
	R.fire_core(self, "MessageReceived", m)
	return m
end

M.DisplayBubble = function(self, part, text)
	if part == nil then fail("partOrCharacter must be non-nil") end
	if typeof(part) ~= "Instance" or not part:IsDescendantOf(workspace) then
		fail("partOrCharacter must be in the Workspace")
	end
	local ad = resolve_adornee(part) or part
	local m = new_message({
		Text = escape_rich_text(text),
		Status = STATUS.Success,
		MessageId = "0-" .. svc.HttpService:GenerateGUID(true),
	})
	R.fire_game(svc.TextChatService, "BubbleDisplayed", part, m)
	deliver_to_bubble(ad, m, props(m).Text, nil)
	R.fire_core(svc.TextChatService, "BubbleDisplayed", part, m)
end

M.AddUserAsync = function(self, uid)
	if not on_server() then fail("TextChannel:AddUserAsync can only be called on the server.") end
	uid = user_id_arg(uid)
	task.wait()
	local existing = source_of(self, uid)
	if existing then return existing, false end
	local p = svc.Players:GetPlayerByUserId(uid)
	if not p then return nil, false end
	local s = R.create("TextSource")
	s.Name = p.Name
	__rs_set_raw(s, "UserId", uid)
	s.CanSend = true
	s.Parent = self
	return s, true
end

M.SetDirectChatRequester = function(self, player)
	if not is_a(player, "Player") then fail("Unable to cast value to Object") end
	for _, c in self:GetChildren() do
		if c:IsA("TextSource") then
			fail("TextChannel:SetDirectChatRequester: Cannot update direct chat requester when TextSources are already present")
		end
	end
	props(self).DirectChatRequester = player
end

M.CanUsersDirectChatAsync = function(self, requester, ids)
	user_id_arg(requester)
	if type(ids) ~= "table" then fail("Unable to cast value to std::vector<int64>") end
	task.wait()
	local out = {}
	for _, id in ids do
		if tonumber(id) and tonumber(id) ~= requester then table.insert(out, tonumber(id)) end
	end
	return out
end

-- Everyone in a Studio test may chat with everyone: one group.
M.GetChatGroupsAsync = function(self, players)
	if type(players) ~= "table" then fail("Unable to cast value to Objects") end
	task.wait()
	local group = {}
	for _, p in players do
		if is_a(p, "Player") then table.insert(group, p) end
	end
	return { group }
end

M.DeriveNewMessageProperties = function(self)
	local d = R.create("ChatWindowMessageProperties")
	for _, k in { "FontFace", "TextColor3", "TextSize", "TextStrokeColor3", "TextStrokeTransparency" } do
		pcall(function() d[k] = self[k] end)
	end
	return d
end

local function make_text_source(ch, player, can_send)
	local existing = source_of(ch, player.UserId)
	if existing then return existing end
	local s = R.create("TextSource")
	s.Name = player.Name
	__rs_set_raw(s, "UserId", player.UserId)
	s.CanSend = can_send
	s.Parent = ch
	return s
end

-- Server: a client's message/command/whisper/legacy chat arrived.
function __rs_pc_server_receive(player, channel, kind, text, raw_text, metadata, id, target_uid)
	if kind == "legacy" then
		local cb_res = M.InvokeChatCallback(svc.Chat, Enum.ChatCallbackType.OnServerReceivingMessage, {
			FromSpeaker = player.Name,
			Message = raw_text,
		})
		if cb_res == false then return {} end
		R.fire(player, "Chatted", raw_text, nil)
		local deliver = {}
		for _, p in svc.Players:GetPlayers() do table.insert(deliver, p.UserId) end
		return deliver
	end

	if kind == "whisper_open" then
		local target = svc.Players:GetPlayerByUserId(target_uid)
		if not target or target == player then return {} end
		local folder = svc.TextChatService:FindFirstChild("TextChannels")
		if not folder then return {} end
		local a, b = math.min(player.UserId, target.UserId), math.max(player.UserId, target.UserId)
		local cname = string.format("RBXWhisper:%d_%d", a, b)
		local wch = folder:FindFirstChild(cname)
		if not wch then
			wch = Instance.new("TextChannel")
			wch.Name = cname
			props(wch).DirectChatRequester = player
			wch.Parent = folder
		end
		make_text_source(wch, player, true)
		make_text_source(wch, target, true)
		if raw_text and raw_text ~= "" then
			R.fire(player, "Chatted", raw_text, target)
			return { player.UserId, target.UserId }
		end
		return {}
	end

	if not channel then return {} end
	local src = source_of(channel, player.UserId)

	if kind == "cmd" then
		R.fire(player, "Chatted", raw_text, nil)
		for _, cmd in find_matching_commands(raw_text) do
			R.fire(cmd, "Triggered", src, raw_text)
		end
		return {}
	end

	-- Regular message ("msg")
	local recipient = nil
	if channel.Name:sub(1, 11) == "RBXWhisper:" then
		local a, b = channel.Name:sub(12):match("^(%-?%d+)_(%-?%d+)$")
		if a and b then
			local oid = tonumber(a) == player.UserId and tonumber(b) or tonumber(a)
			recipient = oid and svc.Players:GetPlayerByUserId(oid) or nil
		end
	end
	R.fire(player, "Chatted", raw_text, recipient)

	if src == nil or not src.CanSend or utf8_count(raw_text) > 200 then return {} end

	local deliver = {}
	local cb = __rs_get_callback(channel, "ShouldDeliverCallback")
	local probe = cb and new_message({
		Text = text,
		TextChannel = channel,
		TextSource = src,
		Status = STATUS.Success,
		MessageId = id,
		Metadata = metadata,
		PrefixText = prefix_for(channel, player),
	})
	for _, s in channel:GetChildren() do
		if s:IsA("TextSource") then
			local ok = true
			if cb then
				local good, res = pcall(cb, probe, s)
				ok = good and res ~= false
				if not good then warn(tostring(res)) end
			end
			if ok then table.insert(deliver, s.UserId) end
		end
	end
	return deliver
end

-- Client: the server delivered a message to this player.
function __rs_pc_client_receive(channel, sender_id, kind, text, raw_text, metadata, id)
	if muted_users[sender_id] then return end
	local sender = svc.Players:GetPlayerByUserId(sender_id)

	if kind == "legacy" then
		local ui = ensure_ui()
		local name = sender and sender.DisplayName or "Player"
		if ui then
			ui.add_legacy_line("<b>" .. escape_rich_text(name) .. ":</b> " .. escape_rich_text(raw_text))
			local bub_on = svc.Chat.BubbleChatEnabled or svc.Players.BubbleChat
			if bub_on and sender then
				local a = bubble_adornee(sender)
				if a then ui.add_bubble(a, escape_rich_text(raw_text), nil, sender_id) end
			end
		end
		if sender then R.fire(sender, "Chatted", raw_text, nil) end
		return
	end

	if not channel then return end
	local src = source_of(channel, sender_id)
	local m = new_message({
		Text = text,
		TextChannel = channel,
		TextSource = src,
		Status = STATUS.Success,
		MessageId = id,
		Metadata = metadata,
		PrefixText = prefix_for(channel, sender),
	})
	run_incoming(m)
	-- Match Studio's exact event order: game MessageReceived listeners run
	-- first, then OnChatWindowAdded & OnBubbleAdded populate message
	-- properties, then CoreScripts (ExpChat) receive the populated message.
	R.fire_game(svc.TextChatService, "MessageReceived", m)
	R.fire_game(channel, "MessageReceived", m)
	deliver_to_window(m)
	local cfg = svc.TextChatService:FindFirstChildOfClass("BubbleChatConfiguration")
	if cfg and cfg.Enabled and sender then
		local a = bubble_adornee(sender)
		if a then deliver_to_bubble(a, m, props(m).Text, sender_id) end
	end
	R.fire_core(svc.TextChatService, "MessageReceived", m)
	R.fire_core(channel, "MessageReceived", m)
	if sender then R.fire(sender, "Chatted", raw_text, nil) end
end

-- Default channels and commands (server), TextSources per player, Team channels,
-- BubbleChatConfiguration default children (UIGradient, ImageLabel, UICorner, UIPadding).
local DEFAULT_COMMANDS = {
	{ "RBXTeamCommand", "/team", "/t" },
	{ "RBXClearCommand", "/clear", "/cls" },
	{ "RBXMuteCommand", "/mute", "/m" },
	{ "RBXUnmuteCommand", "/unmute", "/um" },
	{ "RBXHelpCommand", "/help", "/?" },
	{ "RBXEmoteCommand", "/emote", "/e" },
	{ "RBXVersionCommand", "/version", "/v" },
	{ "RBXConsoleCommand", "/console", "" },
	{ "RBXWhisperCommand", "/whisper", "/w" },
}

local function ensure_bcc_children(bcc)
	if not bcc:FindFirstChildOfClass("UIGradient") then
		local g = Instance.new("UIGradient")
		g.Enabled = false
		g.Parent = bcc
	end
	if not bcc:FindFirstChildOfClass("ImageLabel") then
		local img = Instance.new("ImageLabel")
		img.Image = ""
		img.ImageColor3 = Color3.new(1, 1, 1)
		img.ScaleType = Enum.ScaleType.Stretch
		img.TileSize = UDim2.fromScale(1, 1)
		img.Parent = bcc
	end
	if not bcc:FindFirstChildOfClass("UICorner") then
		local c = Instance.new("UICorner")
		c.CornerRadius = UDim.new(0, 12)
		c.Parent = bcc
	end
	if not bcc:FindFirstChildOfClass("UIPadding") then
		local p = Instance.new("UIPadding")
		local u = UDim.new(0, 8)
		p.PaddingTop = u; p.PaddingBottom = u; p.PaddingLeft = u; p.PaddingRight = u
		p.Parent = bcc
	end
end

local set_up, configs = false, false
local function server_setup()
	-- The service's configuration children (Studio always has them).
	if not configs then
		configs = true
		local T = svc.TextChatService
		for _, class in { "ChatWindowConfiguration", "ChatInputBarConfiguration", "BubbleChatConfiguration", "ChannelTabsConfiguration" } do
			local c = T:FindFirstChildOfClass(class)
			if not c then
				c = R.create(class)
				c.Parent = T
			end
			if class == "BubbleChatConfiguration" then
				ensure_bcc_children(c)
			end
		end
	end
	if set_up or not tcs_enabled() then return end
	set_up = true
	if svc.TextChatService.CreateDefaultTextChannels then
		local folder = svc.TextChatService:FindFirstChild("TextChannels") or Instance.new("Folder")
		folder.Name = "TextChannels"
		for _, n in { "RBXSystem", "RBXGeneral" } do
			if not folder:FindFirstChild(n) then
				local c = Instance.new("TextChannel")
				c.Name = n
				c.Parent = folder
			end
		end
		folder.Parent = svc.TextChatService
	end
	if svc.TextChatService.CreateDefaultCommands then
		local folder = svc.TextChatService:FindFirstChild("TextChatCommands") or Instance.new("Folder")
		folder.Name = "TextChatCommands"
		for _, d in DEFAULT_COMMANDS do
			if not folder:FindFirstChild(d[1]) then
				local c = Instance.new("TextChatCommand")
				c.Name = d[1]
				c.PrimaryAlias = d[2]
				c.SecondaryAlias = d[3]
				c.Parent = folder
			end
		end
		folder.Parent = svc.TextChatService
	end
end

local sourced = setmetatable({}, { __mode = "k" })
local player_team_channel = setmetatable({}, { __mode = "k" })

local function server_sources()
	if not set_up then return end
	local T = svc.TextChatService
	local folder = T:FindFirstChild("TextChannels")
	local players = svc.Players:GetPlayers()

	-- 1. Default channels + Team channels (RBXTeam<BrickColor.Name>)
	if T.CreateDefaultTextChannels and folder then
		-- Ensure RBXTeam* channels exist for all Teams
		local teams_svc = svc.Teams
		local active_team_channels = {}
		for _, tm in teams_svc:GetChildren() do
			if tm:IsA("Team") then
				local cname = "RBXTeam" .. tm.TeamColor.Name
				local tch = folder:FindFirstChild(cname)
				if not tch then
					tch = Instance.new("TextChannel")
					tch.Name = cname
					tch.Parent = folder
				end
				active_team_channels[tm] = tch
			end
		end

		for _, p in players do
			if not sourced[p] then
				sourced[p] = true
				for _, n in { "RBXSystem", "RBXGeneral" } do
					local ch = folder:FindFirstChild(n)
					if ch then make_text_source(ch, p, n == "RBXGeneral") end
				end
			end
			-- Sync player's Team TextSource
			local want_ch = (not p.Neutral and p.Team) and active_team_channels[p.Team] or nil
			local cur_ch = player_team_channel[p]
			if cur_ch ~= want_ch then
				if cur_ch and cur_ch.Parent then
					local old_s = source_of(cur_ch, p.UserId)
					if old_s then old_s:Destroy() end
				end
				if want_ch then
					make_text_source(want_ch, p, true)
				end
				player_team_channel[p] = want_ch
			end
		end
	end

	-- 2. Any TextChannel with AddPlayersOnJoin == true
	for _, ch in T:GetDescendants() do
		if ch:IsA("TextChannel") and ch.AddPlayersOnJoin then
			for _, p in players do
				make_text_source(ch, p, true)
			end
		end
	end

	-- 3. Remove TextSources for departed players
	for p in sourced do
		if p.Parent == nil then
			sourced[p] = nil
			player_team_channel[p] = nil
			for _, ch in T:GetDescendants() do
				if ch:IsA("TextChannel") then
					local s = source_of(ch, p.UserId)
					if s then s:Destroy() end
				end
			end
		end
	end
end

local client_ready = false
local expchat_target_synced = false
local last_tabs_enabled = false
local last_local_team = nil
local function client_setup(dt)
	local ui = ensure_ui()
	if ui then ui.step(dt) end
	if not tcs_enabled() then return end

	local bar = svc.TextChatService:FindFirstChildOfClass("ChatInputBarConfiguration")
	local tc = svc.TextChatService:FindFirstChild("TextChannels")
	local general = tc and tc:FindFirstChild("RBXGeneral")
	if not client_ready and bar and general then
		client_ready = true
		if bar.TargetTextChannel == nil then bar.TargetTextChannel = general end
		if ui and svc.TextChatService.ChatTranslationEnabled then
			ui.show_translation_notice()
		end
	end
	-- When ExpChat mounts after TargetTextChannel was already initialized on frame 1,
	-- re-emit TargetTextChannel once so ExpChat's ChannelTabs store selects the active tab.
	if not ui and bar and general then
		local ec = svc.CoreGui:FindFirstChild("ExperienceChat")
		local ctc = svc.TextChatService:FindFirstChildOfClass("ChannelTabsConfiguration")
		local tabs_on = ctc and ctc.Enabled == true or false
		local tabs_just_on = tabs_on and not last_tabs_enabled
		last_tabs_enabled = tabs_on
		if ec and (not expchat_target_synced or tabs_just_on) then
			expchat_target_synced = true
			local cur = bar.TargetTextChannel or general
			pcall(function()
				bar.TargetTextChannel = nil
				bar.TargetTextChannel = cur
			end)
		end
	end

	-- Pending whisper channel replicated from server: switch target & send body
	if pending_whisper and tc then
		local wch = tc:FindFirstChild(pending_whisper.name)
		if wch and bar then
			local pw = pending_whisper
			pending_whisper = nil
			pcall(function() bar.TargetTextChannel = wch end)
			if ui then ui.refresh_tabs() end
			if pw.body ~= "" then
				task.spawn(function() wch:SendAsync(pw.body) end)
			end
		end
	end

	-- Announce team changes on the client ("You are now on the 'X' team."), matching Studio!
	local lp = svc.Players.LocalPlayer
	if lp and ui then
		local cur_team = (not lp.Neutral) and lp.Team or nil
		if cur_team ~= last_local_team then
			last_local_team = cur_team
			if cur_team then
				sys_notice(string.format("You are now on the '%s' team.", cur_team.Name))
				ui.refresh_tabs()
			end
		end
	end
end

-- The engine queues this after constructing an avatar. Assigning
-- Player.Character in Luau does not load an appearance.
function __rs_character_appearance_loaded(player, character)
	task.wait()
	if player.Character ~= character then return end
	local humanoid = character:FindFirstChildOfClass("Humanoid")
	if humanoid and not humanoid:FindFirstChildOfClass("HumanoidDescription") then
		local ok, description = pcall(function()
			if not player.CanLoadCharacterAppearance then return R.create("HumanoidDescription") end
			return svc.Players:GetHumanoidDescriptionFromUserIdAsync(player.CharacterAppearanceId)
		end)
		if ok and description and player.Character == character then
			description.Name = "HumanoidDescription"
			description.Parent = humanoid
		end
	end
	if player.Character == character then
		R.fire(player, "CharacterAppearanceLoaded", character)
	end
end

function __rs_pc_step(dt)
	local s = side()
	if s ~= "Client" then
		server_setup()
		server_sources()
	end
	if s == "Client" then
		client_setup(dt)
	end
end

local core_layout = {
	ChatWindowConfiguration = { AbsolutePosition = { 0, 0 }, AbsoluteSize = { 0, 0 } },
	ChatInputBarConfiguration = { AbsolutePosition = { 0, 0 }, AbsoluteSize = { 0, 0 }, IsFocused = false },
	ChannelTabsConfiguration = { AbsolutePosition = { 0, 0 }, AbsoluteSize = { 0, 0 } },
}

function __rs_pc_set_layout(class, key, a, b)
	local t = core_layout[class]
	if not t then return end
	if key == "IsFocused" then
		t.IsFocused = a == true
	else
		t[key] = { tonumber(a) or 0, tonumber(b) or 0 }
	end
end

function __rs_pc_layout(class, key)
	local ui = ensure_ui()
	if not ui then
		local t = core_layout[class]
		if key == "IsFocused" then return t and t.IsFocused or false end
		local v = t and t[key]
		if v and (v[1] ~= 0 or v[2] ~= 0) then return v[1], v[2] end
		-- Fallback before ExpChat's onAbsoluteSizeChanged fires: read the mounted
		-- CoreGui.ExperienceChat chatWindow box directly when enabled and visible.
		if class == "ChatWindowConfiguration" then
			local cwc = svc.TextChatService:FindFirstChildOfClass("ChatWindowConfiguration")
			if cwc and cwc.Enabled then
				local ec = svc.CoreGui:FindFirstChild("ExperienceChat")
				local cw = ec and ec:FindFirstChild("chatWindow", true)
				if cw and cw:IsA("GuiObject") and cw.Visible then
					local vec = key == "AbsolutePosition" and cw.AbsolutePosition or cw.AbsoluteSize
					if vec.X ~= 0 or vec.Y ~= 0 then
						return vec.X, vec.Y
					end
				end
			end
		end
		return 0, 0
	end
	return ui.layout_query(class, key)
end

function __rs_pc_set_core(key, val)
	local ui = ensure_ui()
	if ui then ui.set_core(key, val) end
end

function __rs_pc_get_core(key)
	local ui = ensure_ui()
	if ui then return ui.get_core(key) end
	return nil
end

function __rs_pc_chat_input(text)
	local raw = tostring(text or "")
	if raw:match("^%s*$") then return true end
	if not tcs_enabled() then
		table.insert(__rs_pc_outbox, {
			kind = "legacy",
			channel = nil,
			text = raw,
			raw_text = raw,
			metadata = "",
			id = "",
			target_uid = 0,
		})
		return true
	end
	local bar = svc.TextChatService:FindFirstChildOfClass("ChatInputBarConfiguration")
	local ch = bar and bar.TargetTextChannel
	if ch == nil then
		local tc = svc.TextChatService:FindFirstChild("TextChannels")
		ch = tc and tc:FindFirstChild("RBXGeneral")
	end
	if ch == nil then return false end
	task.spawn(function() ch:SendAsync(raw) end)
	return true
end
