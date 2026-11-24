-- rustudio engine library: CoreGui-based ExperienceChat, BubbleChat,
-- ChannelTabs, TopBar chat toggle, and LegacyChat UI.
-- Built entirely on the engine GUI path (ScreenGui / BillboardGui under CoreGui),
-- matching Roblox Studio's layout, styling, keybinds, and behaviour.
local R, API = ...

local svc = setmetatable({}, { __index = function(_, k) return game:GetService(k) end })

local ui_ready = false
local root_gui = nil       -- CoreGui.ExperienceChat
local topbar_gui = nil     -- CoreGui.RobloxChatTopBar
local topbar_enabled = true
local bubble_gui = nil     -- CoreGui.ExperienceChat.bubbleChat
local app_layout = nil
local channel_bar = nil
local channel_tabs_container = nil
local chat_window = nil
local scroll_view = nil
local dot_menu = nil
local input_row = nil
local input_group = nil
local input_stroke = nil
local target_chip = nil
local input_box = nil
local send_btn = nil
local ac_frame = nil
local ac_list = nil
local topbar_btn = nil
local topbar_chat_slot = nil
local topbar_unibar = nil
local topbar_icon_body = nil
local topbar_icon_tail = nil
local topbar_badge = nil
local top_padding = nil

-- Legacy chat UI
local legacy_gui = nil
local legacy_window = nil
local legacy_scroll = nil
local legacy_bar = nil
local legacy_box = nil

-- Interactive state
local chat_visible = true
local unread_count = 0
local last_activity_t = -100
local last_message_t = -100
local hover_window = false
local hover_bar = false
local hover_tabs = false
local box_focused = false
local bg_alpha = 0         -- 0 = hidden bg, 1 = visible bg
local text_alpha = 1       -- 0 = hidden text, 1 = visible text
local history = {}
local history_idx = 0
local history_draft = ""
local ac_items = {}
local ac_selected = 1
local custom_textbox_bound = nil
local custom_textbox_conns = {}
local line_instances = {}  -- { { frame = TextLabel, channel = TextChannel?, id = number, time = number } }
local bubble_boards = {}   -- adornee -> { board = BillboardGui, list = Frame, entries = {}, minimized = Frame?, typing = Frame? }
local tab_buttons = {}
local last_channels_sig = ""
local last_vp_w, last_vp_h = 0, 0
local typing_dots_phase = 0
local chat_bar_disabled = false
local welcome_shown = false

-- Classic Roblox 8-color username hash (GetNameValue).
local NAME_COLORS = {
	Color3.fromRGB(253, 41, 67),
	Color3.fromRGB(1, 162, 255),
	Color3.fromRGB(2, 184, 87),
	Color3.fromRGB(107, 50, 124),
	Color3.fromRGB(218, 133, 65),
	Color3.fromRGB(245, 205, 48),
	Color3.fromRGB(232, 186, 200),
	Color3.fromRGB(215, 197, 154),
}

local function name_color(name)
	local n = #name
	local val = 0
	for i = 1, n do
		local rev = n - i + 1
		if n % 2 == 1 then
			rev = rev - 1
		end
		local b = string.byte(name, i)
		if rev % 4 >= 2 then
			val = val - b
		else
			val = val + b
		end
	end
	return NAME_COLORS[(val % 8) + 1]
end

local function color_hex(c)
	local r = math.clamp(math.floor(c.R * 255 + 0.5), 0, 255)
	local g = math.clamp(math.floor(c.G * 255 + 0.5), 0, 255)
	local b = math.clamp(math.floor(c.B * 255 + 0.5), 0, 255)
	return string.format("#%02x%02x%02x", r, g, b)
end

-- Common emoji shortcodes supported in Roblox ExperienceChat.
local EMOJIS = {
	smile = "😄", grinning = "😀", joy = "😂", rofl = "🤣", wink = "😉",
	blush = "😊", heart_eyes = "😍", sunglasses = "😎", thinking = "🤔",
	sob = "😭", cry = "😢", angry = "😠", rage = "😡", skull = "💀",
	fire = "🔥", heart = "❤️", sparkles = "✨", star = "⭐", ["100"] = "💯",
	thumbsup = "👍", ["+1"] = "👍", thumbsdown = "👎", ["-1"] = "👎",
	wave = "👋", clap = "👏", eyes = "👀", ok_hand = "👌", pray = "🙏",
	check = "✅", white_check_mark = "✅", x = "❌", warning = "⚠️",
	tada = "🎉", crown = "👑", rocket = "🚀", gg = "🎮",
}

local function replace_emojis(text)
	return (text:gsub(":([%w_%+%-]+):", function(code)
		return EMOJIS[code:lower()] or (":" .. code .. ":")
	end))
end

local function utf8_truncate(s, max_chars)
	if #s <= max_chars then return s end
	local ok, pos = pcall(utf8.offset, s, max_chars + 1)
	if ok and pos then
		return s:sub(1, pos - 1)
	end
	return s:sub(1, max_chars)
end

local function key_label(kc)
	if typeof(kc) ~= "EnumItem" then return "/" end
	local n = kc.Name
	if n == "Slash" then return "/" end
	if n == "BackSlash" then return "\\" end
	if n == "Period" then return "." end
	if n == "Comma" then return "," end
	if n == "Semicolon" then return ";" end
	if n == "Quote" then return "'" end
	if n == "Backquote" then return "`" end
	if n == "Return" then return "Enter" end
	return n
end

local function get_tcs() return svc.TextChatService end
local function get_cwc() return get_tcs():FindFirstChildOfClass("ChatWindowConfiguration") end
local function get_cib() return get_tcs():FindFirstChildOfClass("ChatInputBarConfiguration") end
local function get_ctc() return get_tcs():FindFirstChildOfClass("ChannelTabsConfiguration") end
local function get_bcc() return get_tcs():FindFirstChildOfClass("BubbleChatConfiguration") end

local function is_tcs_mode()
	local ok, v = pcall(function() return get_tcs().ChatVersion end)
	return ok and v == Enum.ChatVersion.TextChatService
end

local function core_chat_enabled()
	return R.core_gui_enabled("Chat")
end

local function channel_display_name(ch)
	if not ch then return "" end
	local n = ch.Name
	if n == "RBXGeneral" then return "Here" end
	if n:sub(1, 7) == "RBXTeam" then return "Team" end
	if n:sub(1, 11) == "RBXWhisper:" then
		local lp = svc.Players.LocalPlayer
		local ids = n:sub(12)
		local a, b = ids:match("^(%-?%d+)_(%-?%d+)$")
		local other_id = nil
		if a and b and lp then
			if tonumber(a) == lp.UserId then other_id = tonumber(b) else other_id = tonumber(a) end
		end
		if other_id then
			local p = svc.Players:GetPlayerByUserId(other_id)
			if p then return "To " .. p.DisplayName end
		end
		return "Whisper"
	end
	return n
end

local function joinable_channels()
	local T = get_tcs()
	local lp = svc.Players.LocalPlayer
	if not lp then return {} end
	local out = {}
	local general = nil
	local teams = {}
	local others = {}
	for _, d in T:GetDescendants() do
		if d:IsA("TextChannel") and d.Name ~= "RBXSystem" then
			local has_src = false
			for _, s in d:GetChildren() do
				if s:IsA("TextSource") and s.UserId == lp.UserId and s.CanSend then
					has_src = true
					break
				end
			end
			if has_src then
				if d.Name == "RBXGeneral" then
					general = d
				elseif d.Name:sub(1, 7) == "RBXTeam" then
					table.insert(teams, d)
				else
					table.insert(others, d)
				end
			end
		end
	end
	if general then table.insert(out, general) end
	table.sort(teams, function(a, b) return a.Name < b.Name end)
	for _, c in teams do table.insert(out, c) end
	table.sort(others, function(a, b) return a.Name < b.Name end)
	for _, c in others do table.insert(out, c) end
	return out
end

local function set_target_channel(ch)
	local cib = get_cib()
	if cib and ch then
		pcall(function() cib.TargetTextChannel = ch end)
	end
end

local function reset_target_to_general()
	local T = get_tcs()
	local tc = T:FindFirstChild("TextChannels")
	local gen = tc and tc:FindFirstChild("RBXGeneral")
	if gen then set_target_channel(gen) end
end

local function update_send_button()
	if not send_btn or not input_box then return end
	local has_text = input_box.Text:match("%S") ~= nil
	if has_text then
		send_btn.BackgroundColor3 = Color3.fromRGB(51, 95, 255)
		send_btn.BackgroundTransparency = 0
		send_btn.TextColor3 = Color3.fromRGB(255, 255, 255)
	else
		send_btn.BackgroundColor3 = Color3.fromRGB(208, 217, 251)
		send_btn.BackgroundTransparency = 0.88
		send_btn.TextColor3 = Color3.fromRGB(247, 247, 248)
	end
end

local function update_target_chip()
	if not target_chip or not input_box then return end
	local cib = get_cib()
	local ch = cib and cib.TargetTextChannel
	local show = false
	local chip_text = ""
	local chip_color = Color3.fromRGB(255, 255, 255)
	if ch and ch.Name ~= "RBXGeneral" and ch.Name ~= "RBXSystem" then
		show = true
		if ch.Name:sub(1, 7) == "RBXTeam" then
			chip_text = "[Team] "
			local lp = svc.Players.LocalPlayer
			if lp and lp.Team and not lp.Neutral then
				local ok, tc = pcall(function() return lp.Team.TeamColor.Color end)
				chip_color = (ok and tc) or lp.TeamColor.Color
			else
				chip_color = Color3.fromRGB(160, 210, 255)
			end
		elseif ch.Name:sub(1, 11) == "RBXWhisper:" then
			chip_text = "[" .. channel_display_name(ch) .. "] "
			chip_color = Color3.fromRGB(210, 175, 255)
		else
			chip_text = "[" .. ch.Name .. "] "
			chip_color = Color3.fromRGB(180, 220, 255)
		end
	end
	target_chip.Visible = show
	target_chip.Text = chip_text
	target_chip.TextColor3 = chip_color
	local chip_w = show and (#chip_text * 9 + 6) or 0
	target_chip.Size = UDim2.new(0, chip_w, 1, 0)
	input_box.Position = UDim2.new(0, 16 + chip_w, 0, 0)
	input_box.Size = UDim2.new(1, -(56 + chip_w), 1, 0)
end

local function hide_autocomplete()
	if ac_frame then ac_frame.Visible = false end
	ac_items = {}
	ac_selected = 1
end

local function apply_autocomplete(item)
	if not input_box or not item then return end
	input_box.Text = item.fill
	hide_autocomplete()
	update_send_button()
end

local function refresh_autocomplete()
	if not ac_frame or not ac_list or not input_box then return end
	local cib = get_cib()
	if not box_focused or not cib or not cib.AutocompleteEnabled then
		hide_autocomplete()
		return
	end
	local txt = input_box.Text
	local matches = {}
	-- 1. Whisper / mute / unmute player autocomplete
	local cmd_prefix, arg_prefix = txt:match("^(/%a+)%s+(%S*)$")
	if cmd_prefix then
		local lc = cmd_prefix:lower()
		if lc == "/w" or lc == "/whisper" or lc == "/m" or lc == "/mute" or lc == "/um" or lc == "/unmute" then
			local lp = svc.Players.LocalPlayer
			local ap = arg_prefix:lower()
			for _, p in svc.Players:GetPlayers() do
				if p ~= lp then
					if ap == "" or p.Name:lower():sub(1, #ap) == ap or p.DisplayName:lower():sub(1, #ap) == ap then
						table.insert(matches, {
							label = p.DisplayName .. " (@" .. p.Name .. ")",
							fill = cmd_prefix .. " " .. p.Name .. " ",
						})
					end
				end
			end
		elseif lc == "/e" or lc == "/emote" then
			local ap = arg_prefix:lower()
			for _, em in { "wave", "point", "dance", "dance2", "dance3", "laugh", "cheer" } do
				if ap == "" or em:sub(1, #ap) == ap then
					table.insert(matches, { label = "/e " .. em, fill = cmd_prefix .. " " .. em })
				end
			end
		end
	elseif txt:sub(1, 1) == "/" and not txt:find("%s") then
		-- 2. TextChatCommand alias autocomplete
		local pfx = txt:lower()
		local seen = {}
		for _, d in get_tcs():GetDescendants() do
			if d:IsA("TextChatCommand") and d.Enabled and d.AutocompleteVisible then
				for _, al in { d.PrimaryAlias, d.SecondaryAlias } do
					if al ~= "" and al:lower():sub(1, #pfx) == pfx and not seen[al:lower()] then
						seen[al:lower()] = true
						table.insert(matches, { label = al, fill = al .. " " })
					end
				end
			end
		end
		table.sort(matches, function(a, b) return a.label < b.label end)
	else
		-- 3. Emoji shortcode autocomplete (e.g. ":sm")
		local before, ecode = txt:match("^(.*):([%w_%+%-]+)$")
		if ecode and #ecode >= 2 then
			local ep = ecode:lower()
			for k, ch in pairs(EMOJIS) do
				if k:sub(1, #ep) == ep then
					table.insert(matches, {
						label = ch .. " :" .. k .. ":",
						fill = before .. ch .. " ",
					})
				end
			end
			table.sort(matches, function(a, b) return a.label < b.label end)
		end
	end

	if #matches == 0 then
		hide_autocomplete()
		return
	end
	if #matches > 6 then
		local trimmed = {}
		for i = 1, 6 do trimmed[i] = matches[i] end
		matches = trimmed
	end
	ac_items = matches
	if ac_selected > #ac_items then ac_selected = 1 end

	for _, c in ac_list:GetChildren() do
		if c:IsA("TextButton") then c:Destroy() end
	end
	for i, item in ipairs(ac_items) do
		local btn = Instance.new("TextButton")
		btn.Name = "AC_" .. i
		btn.LayoutOrder = i
		btn.Size = UDim2.new(1, 0, 0, 26)
		btn.BackgroundColor3 = i == ac_selected and Color3.fromRGB(65, 72, 85) or Color3.fromRGB(32, 35, 39)
		btn.BackgroundTransparency = i == ac_selected and 0.15 or 0.4
		btn.BorderSizePixel = 0
		btn.Text = "  " .. item.label
		btn.TextColor3 = Color3.fromRGB(240, 242, 246)
		btn.TextSize = 15
		btn.FontFace = Font.new("rbxasset://fonts/families/BuilderSans.json", Enum.FontWeight.Medium)
		btn.TextXAlignment = Enum.TextXAlignment.Left
		btn.Activated:Connect(function() apply_autocomplete(item) end)
		btn.Parent = ac_list
	end
	ac_frame.Size = UDim2.new(0.95, 0, 0, #ac_items * 26 + 8)
	ac_frame.Visible = true
end

local function submit_text(raw)
	if not raw or raw:match("^%s*$") then return end
	local text = utf8_truncate(raw, 200)
	text = replace_emojis(text)
	-- Save to history
	if history[#history] ~= text then
		table.insert(history, text)
		if #history > 50 then table.remove(history, 1) end
	end
	history_idx = 0
	history_draft = ""
	last_activity_t = os.clock()
	hide_autocomplete()
	if is_tcs_mode() then
		local cib = get_cib()
		local ch = cib and cib.TargetTextChannel
		if ch == nil then
			local tc = get_tcs():FindFirstChild("TextChannels")
			ch = tc and tc:FindFirstChild("RBXGeneral")
		end
		if ch then
			task.spawn(function() ch:SendAsync(text) end)
		end
	else
		API.send_legacy(text)
	end
end

local function update_topbar_badge()
	if not topbar_badge then return end
	topbar_badge.Visible = (not chat_visible) and unread_count > 0
end

local function refresh_line_visibility()
	local cwc = get_cwc()
	local cib = get_cib()
	local ctc = get_ctc()
	local target = cib and cib.TargetTextChannel
	local per_channel = false
	if cwc and ctc and ctc.Enabled then
		local ok, dm = pcall(function() return cwc.TextChannelDisplayMode end)
		if ok and dm == Enum.TextChannelDisplayMode.Conditional then
			per_channel = true
		end
	end
	for _, entry in line_instances do
		if entry.frame and entry.frame.Parent then
			if per_channel and target and entry.channel and entry.channel.Name ~= "RBXSystem" then
				entry.frame.Visible = (entry.channel == target)
			else
				entry.frame.Visible = true
			end
		end
	end
end

local function rebuild_channel_tabs()
	if not channel_tabs_container then return end
	local ctc = get_ctc()
	local cib = get_cib()
	local chs = joinable_channels()
	local target = cib and cib.TargetTextChannel
	local sig_parts = { tostring(ctc and ctc.Enabled), tostring(target and target.Name or "") }
	for _, c in chs do table.insert(sig_parts, c.Name) end
	local sig = table.concat(sig_parts, "|")
	if sig == last_channels_sig then return end
	last_channels_sig = sig

	for _, c in channel_tabs_container:GetChildren() do
		if c:IsA("Frame") then c:Destroy() end
	end
	tab_buttons = {}
	if #chs == 0 then return end

	local n = #chs
	local frac = 1 / n
	for i, ch in ipairs(chs) do
		local selected = (target == ch)
		local slot = Instance.new("Frame")
		slot.Name = tostring(i)
		slot.LayoutOrder = i
		slot.Size = UDim2.new(frac, 0, 1, 0)
		slot.BackgroundTransparency = 1
		slot.Parent = channel_tabs_container

		local btn = Instance.new("TextButton")
		btn.Name = "ChannelTab"
		btn.Size = UDim2.new(1, 0, 1, 0)
		btn.BackgroundColor3 = ctc and ctc.BackgroundColor3 or Color3.fromRGB(25, 27, 29)
		btn.BackgroundTransparency = 1
		btn.BorderSizePixel = 0
		btn.Text = channel_display_name(ch)
		btn.FontFace = ctc and ctc.FontFace or Font.new("rbxasset://fonts/families/BuilderSans.json", Enum.FontWeight.Bold)
		btn.TextSize = ctc and ctc.TextSize or 18
		btn.TextColor3 = selected
			and (ctc and ctc.SelectedTabTextColor3 or Color3.fromRGB(255, 255, 255))
			or (ctc and ctc.TextColor3 or Color3.fromRGB(175, 175, 175))
		btn.TextStrokeColor3 = ctc and ctc.TextStrokeColor3 or Color3.fromRGB(0, 0, 0)
		btn.TextStrokeTransparency = ctc and ctc.TextStrokeTransparency or 1
		btn.TextTruncate = Enum.TextTruncate.AtEnd
		btn.Parent = slot

		btn.MouseEnter:Connect(function()
			hover_tabs = true
			last_activity_t = os.clock()
			if ctc then
				btn.BackgroundColor3 = ctc.HoverBackgroundColor3
				btn.BackgroundTransparency = 0.5
			end
		end)
		btn.MouseLeave:Connect(function()
			hover_tabs = false
			btn.BackgroundTransparency = 1
		end)
		btn.Activated:Connect(function()
			set_target_channel(ch)
			last_activity_t = os.clock()
			last_channels_sig = ""
			rebuild_channel_tabs()
			update_target_chip()
			refresh_line_visibility()
		end)

		if selected then
			local hl = Instance.new("Frame")
			hl.Name = "Highlight"
			hl.Size = UDim2.new(1, 0, 0, 4)
			hl.Position = UDim2.new(0, 0, 1, -4)
			hl.BackgroundColor3 = Color3.fromRGB(255, 255, 255)
			hl.BackgroundTransparency = 0
			hl.BorderSizePixel = 0
			hl.Parent = slot
		end
		if i < n then
			local div = Instance.new("Frame")
			div.Name = "Divider"
			div.Size = UDim2.new(0, 1, 0, 28)
			div.Position = UDim2.new(1, -1, 0, 6)
			div.BackgroundColor3 = Color3.fromRGB(255, 255, 255)
			div.BackgroundTransparency = 0.78
			div.BorderSizePixel = 0
			div.Parent = slot
		end
		table.insert(tab_buttons, { btn = btn, ch = ch, selected = selected })
	end
end

local function scroll_to_bottom(sf)
	if not sf then return end
	pcall(function()
		local canvas_y = sf.AbsoluteCanvasSize.Y
		local win_y = sf.AbsoluteWindowSize.Y
		if canvas_y > win_y and win_y > 0 then
			sf.CanvasPosition = Vector2.new(0, canvas_y - win_y)
		else
			sf.CanvasPosition = Vector2.new(0, 0)
		end
	end)
end

-- Build CoreGui.ExperienceChat + TopBar toggle + LegacyChat containers once.
local function build_ui()
	if ui_ready then return end
	local core = R.core_gui()
	if not core then return end
	ui_ready = true

	-- 1. CoreGui.TopBarApp (matches Studio's Chrome/Unibar top-left layout:
	--    44x44 Roblox logo circle at (16, 12) + 96x44 pill at (68, 12) holding hamburger + chat toggle)
	topbar_gui = Instance.new("ScreenGui")
	topbar_gui.Name = "TopBarApp"
	topbar_gui.ResetOnSpawn = false
	topbar_gui.DisplayOrder = 6
	topbar_gui.IgnoreGuiInset = true
	topbar_gui.ZIndexBehavior = Enum.ZIndexBehavior.Sibling
	topbar_gui.Parent = core

	local pill_bg = Color3.fromRGB(18, 18, 21)
	local icon_fg = Color3.fromRGB(247, 247, 248)
	local hover_col = Color3.fromRGB(208, 217, 251)

	-- 1a. Left circular Roblox logo button (absPos 16, 12, size 44x44)
	local menu_holder = Instance.new("Frame")
	menu_holder.Name = "MenuIconHolder"
	menu_holder.Position = UDim2.fromOffset(16, 10)
	menu_holder.Size = UDim2.new(1, 0, 0, 48)
	menu_holder.BackgroundTransparency = 1
	menu_holder.Parent = topbar_gui

	local logo_btn = Instance.new("TextButton")
	logo_btn.Name = "IconHitArea"
	logo_btn.Position = UDim2.fromOffset(0, 2)
	logo_btn.Size = UDim2.fromOffset(44, 44)
	logo_btn.BackgroundColor3 = pill_bg
	logo_btn.BackgroundTransparency = 0.08
	logo_btn.BorderSizePixel = 0
	logo_btn.Text = ""
	logo_btn.AutoButtonColor = false
	logo_btn.Parent = menu_holder
	local logo_corner = Instance.new("UICorner")
	logo_corner.CornerRadius = UDim.new(1, 0)
	logo_corner.Parent = logo_btn

	local logo_hover = Instance.new("Frame")
	logo_hover.Name = "StateOverlayRound"
	logo_hover.Size = UDim2.fromScale(1, 1)
	logo_hover.BackgroundColor3 = hover_col
	logo_hover.BackgroundTransparency = 1
	logo_hover.BorderSizePixel = 0
	logo_hover.ZIndex = 2
	logo_hover.Parent = logo_btn
	local lh_c = Instance.new("UICorner")
	lh_c.CornerRadius = UDim.new(1, 0)
	lh_c.Parent = logo_hover
	logo_btn.MouseEnter:Connect(function() logo_hover.BackgroundTransparency = 0.92 end)
	logo_btn.MouseLeave:Connect(function() logo_hover.BackgroundTransparency = 1 end)

	-- Tilted Roblox square-with-hole logo
	local logo_outer = Instance.new("Frame")
	logo_outer.Name = "RobloxTiltOuter"
	logo_outer.AnchorPoint = Vector2.new(0.5, 0.5)
	logo_outer.Position = UDim2.fromScale(0.5, 0.5)
	logo_outer.Size = UDim2.fromOffset(18, 18)
	logo_outer.Rotation = 15
	logo_outer.BackgroundColor3 = icon_fg
	logo_outer.BorderSizePixel = 0
	logo_outer.Parent = logo_btn
	local lo_c = Instance.new("UICorner")
	lo_c.CornerRadius = UDim.new(0, 2)
	lo_c.Parent = logo_outer
	local logo_inner = Instance.new("Frame")
	logo_inner.Name = "RobloxTiltHole"
	logo_inner.AnchorPoint = Vector2.new(0.5, 0.5)
	logo_inner.Position = UDim2.fromScale(0.5, 0.5)
	logo_inner.Size = UDim2.fromOffset(6, 6)
	logo_inner.BackgroundColor3 = pill_bg
	logo_inner.BorderSizePixel = 0
	logo_inner.Parent = logo_outer

	-- 1b. Right Unibar pill group (absPos 68, 12, size 96x44): hamburger + chat toggle
	topbar_unibar = Instance.new("Frame")
	topbar_unibar.Name = "Unibar"
	topbar_unibar.Position = UDim2.fromOffset(68, 12)
	topbar_unibar.Size = UDim2.fromOffset(96, 44)
	topbar_unibar.BackgroundColor3 = pill_bg
	topbar_unibar.BackgroundTransparency = 0.08
	topbar_unibar.BorderSizePixel = 0
	topbar_unibar.Parent = topbar_gui
	local ub_corner = Instance.new("UICorner")
	ub_corner.CornerRadius = UDim.new(1, 0)
	ub_corner.Parent = topbar_unibar

	-- Hamburger menu button (nine_dot at x=4, size 44x44)
	local menu_slot = Instance.new("Frame")
	menu_slot.Name = "nine_dot"
	menu_slot.Position = UDim2.fromOffset(4, 0)
	menu_slot.Size = UDim2.fromOffset(44, 44)
	menu_slot.BackgroundTransparency = 1
	menu_slot.Parent = topbar_unibar
	local menu_hl = Instance.new("Frame")
	menu_hl.Name = "Highlighter"
	menu_hl.AnchorPoint = Vector2.new(0.5, 0.5)
	menu_hl.Position = UDim2.fromScale(0.5, 0.5)
	menu_hl.Size = UDim2.fromOffset(36, 36)
	menu_hl.BackgroundColor3 = hover_col
	menu_hl.BackgroundTransparency = 0.92
	menu_hl.BorderSizePixel = 0
	menu_hl.Visible = false
	menu_hl.Parent = menu_slot
	local mhl_c = Instance.new("UICorner")
	mhl_c.CornerRadius = UDim.new(1, 0)
	mhl_c.Parent = menu_hl
	for b = 1, 3 do
		local bar = Instance.new("Frame")
		bar.Name = "Bar" .. b
		bar.Size = UDim2.fromOffset(16, 2)
		bar.Position = UDim2.fromOffset(14, 15 + (b - 1) * 6)
		bar.BackgroundColor3 = icon_fg
		bar.BorderSizePixel = 0
		bar.ZIndex = 2
		bar.Parent = menu_slot
		local bc = Instance.new("UICorner")
		bc.CornerRadius = UDim.new(1, 0)
		bc.Parent = bar
	end
	local menu_hit = Instance.new("TextButton")
	menu_hit.Name = "IconHitArea_nine_dot"
	menu_hit.Size = UDim2.fromScale(1, 1)
	menu_hit.BackgroundTransparency = 1
	menu_hit.Text = ""
	menu_hit.ZIndex = 3
	menu_hit.Parent = menu_slot
	menu_hit.MouseEnter:Connect(function() menu_hl.Visible = true end)
	menu_hit.MouseLeave:Connect(function() menu_hl.Visible = false end)

	-- Chat toggle button inside Unibar (chat at x=48, size 44x44)
	topbar_chat_slot = Instance.new("Frame")
	topbar_chat_slot.Name = "chat"
	topbar_chat_slot.Position = UDim2.fromOffset(48, 0)
	topbar_chat_slot.Size = UDim2.fromOffset(44, 44)
	topbar_chat_slot.BackgroundTransparency = 1
	topbar_chat_slot.Parent = topbar_unibar

	local chat_hl = Instance.new("Frame")
	chat_hl.Name = "Highlighter"
	chat_hl.AnchorPoint = Vector2.new(0.5, 0.5)
	chat_hl.Position = UDim2.fromScale(0.5, 0.5)
	chat_hl.Size = UDim2.fromOffset(36, 36)
	chat_hl.BackgroundColor3 = hover_col
	chat_hl.BackgroundTransparency = 0.92
	chat_hl.BorderSizePixel = 0
	chat_hl.Visible = false
	chat_hl.Parent = topbar_chat_slot
	local chl_c = Instance.new("UICorner")
	chl_c.CornerRadius = UDim.new(1, 0)
	chl_c.Parent = chat_hl

	topbar_icon_body = Instance.new("Frame")
	topbar_icon_body.Name = "IconBody"
	topbar_icon_body.Size = UDim2.fromOffset(20, 14)
	topbar_icon_body.Position = UDim2.fromOffset(12, 13)
	topbar_icon_body.BackgroundColor3 = icon_fg
	topbar_icon_body.BorderSizePixel = 0
	topbar_icon_body.ZIndex = 2
	topbar_icon_body.Parent = topbar_chat_slot
	local ib_corner = Instance.new("UICorner")
	ib_corner.CornerRadius = UDim.new(0, 3)
	ib_corner.Parent = topbar_icon_body
	topbar_icon_tail = Instance.new("Frame")
	topbar_icon_tail.Name = "IconTail"
	topbar_icon_tail.Size = UDim2.fromOffset(5, 5)
	topbar_icon_tail.Position = UDim2.fromOffset(18, 24)
	topbar_icon_tail.Rotation = 45
	topbar_icon_tail.BackgroundColor3 = icon_fg
	topbar_icon_tail.BorderSizePixel = 0
	topbar_icon_tail.ZIndex = 2
	topbar_icon_tail.Parent = topbar_chat_slot
	for idx = 1, 2 do
		local line = Instance.new("Frame")
		line.Size = UDim2.fromOffset(idx == 1 and 10 or 7, 2)
		line.Position = UDim2.fromOffset(5, idx == 1 and 4 or 8)
		line.BackgroundColor3 = pill_bg
		line.BorderSizePixel = 0
		line.ZIndex = 3
		line.Parent = topbar_icon_body
	end

	topbar_badge = Instance.new("Frame")
	topbar_badge.Name = "UnreadBadge"
	topbar_badge.Size = UDim2.fromOffset(9, 9)
	topbar_badge.Position = UDim2.new(1, -11, 0, 7)
	topbar_badge.BackgroundColor3 = Color3.fromRGB(255, 72, 72)
	topbar_badge.BorderSizePixel = 0
	topbar_badge.Visible = false
	topbar_badge.ZIndex = 4
	topbar_badge.Parent = topbar_chat_slot
	local badge_c = Instance.new("UICorner")
	badge_c.CornerRadius = UDim.new(1, 0)
	badge_c.Parent = topbar_badge

	topbar_btn = Instance.new("TextButton")
	topbar_btn.Name = "IconHitArea_chat"
	topbar_btn.Size = UDim2.fromScale(1, 1)
	topbar_btn.BackgroundTransparency = 1
	topbar_btn.BorderSizePixel = 0
	topbar_btn.Text = ""
	topbar_btn.AutoButtonColor = false
	topbar_btn.ZIndex = 5
	topbar_btn.Parent = topbar_chat_slot
	topbar_btn.MouseEnter:Connect(function() chat_hl.Visible = true end)
	topbar_btn.MouseLeave:Connect(function() chat_hl.Visible = false end)
	topbar_btn.Activated:Connect(function()
		chat_visible = not chat_visible
		if chat_visible then
			unread_count = 0
			last_activity_t = os.clock()
		end
		if topbar_icon_body then
			topbar_icon_body.BackgroundTransparency = chat_visible and 0 or 0.45
		end
		if topbar_icon_tail then
			topbar_icon_tail.BackgroundTransparency = chat_visible and 0 or 0.45
		end
		update_topbar_badge()
	end)

	-- 2. CoreGui.ExperienceChat (matches Studio's CoreGui hierarchy)
	root_gui = Instance.new("ScreenGui")
	root_gui.Name = "ExperienceChat"
	root_gui.ResetOnSpawn = false
	root_gui.DisplayOrder = -1
	root_gui.ZIndexBehavior = Enum.ZIndexBehavior.Sibling
	root_gui.Parent = core

	-- Nested bubbleChat ScreenGui (always active regardless of ChatVersion/CoreGuiType.Chat)
	bubble_gui = Instance.new("ScreenGui")
	bubble_gui.Name = "bubbleChat"
	bubble_gui.ResetOnSpawn = false
	bubble_gui.DisplayOrder = 0
	bubble_gui.ZIndexBehavior = Enum.ZIndexBehavior.Sibling
	bubble_gui.Parent = root_gui

	-- Main appLayout container (unified rounded backdrop matching Studio's ExperienceChat)
	app_layout = Instance.new("Frame")
	app_layout.Name = "appLayout"
	app_layout.Position = UDim2.fromOffset(8, 4)
	app_layout.Size = UDim2.fromOffset(475, 240)
	app_layout.BackgroundColor3 = Color3.fromRGB(25, 27, 29)
	app_layout.BackgroundTransparency = 1
	app_layout.BorderSizePixel = 0
	app_layout.Parent = root_gui
	local al_corner = Instance.new("UICorner")
	al_corner.CornerRadius = UDim.new(0, 8)
	al_corner.Parent = app_layout

	local app_list = Instance.new("UIListLayout")
	app_list.Name = "layout"
	app_list.FillDirection = Enum.FillDirection.Vertical
	app_list.HorizontalAlignment = Enum.HorizontalAlignment.Left
	app_list.VerticalAlignment = Enum.VerticalAlignment.Top
	app_list.SortOrder = Enum.SortOrder.LayoutOrder
	app_list.Padding = UDim.new(0, 0)
	app_list.Parent = app_layout

	-- 2a. Channel tabs bar (LayoutOrder = 1)
	channel_bar = Instance.new("Frame")
	channel_bar.Name = "channelBar"
	channel_bar.LayoutOrder = 1
	channel_bar.Size = UDim2.new(1, 0, 0, 40)
	channel_bar.BackgroundColor3 = Color3.fromRGB(25, 27, 29)
	channel_bar.BackgroundTransparency = 1
	channel_bar.BorderSizePixel = 0
	channel_bar.Visible = false
	channel_bar.Parent = app_layout
	local cb_corner = Instance.new("UICorner")
	cb_corner.CornerRadius = UDim.new(0, 8)
	cb_corner.Parent = channel_bar

	-- Flat lower cover so channelBar has rounded top corners and square bottom edge (matching Studio)
	local cb_lower = Instance.new("Frame")
	cb_lower.Name = "LowerCover"
	cb_lower.Position = UDim2.fromOffset(0, 8)
	cb_lower.Size = UDim2.new(1, 0, 1, -8)
	cb_lower.BackgroundColor3 = Color3.fromRGB(25, 27, 29)
	cb_lower.BackgroundTransparency = 1
	cb_lower.BorderSizePixel = 0
	cb_lower.ZIndex = 1
	cb_lower.Parent = channel_bar

	local cb_border = Instance.new("Frame")
	cb_border.Name = "BottomBorder"
	cb_border.Size = UDim2.new(1, 0, 0, 1)
	cb_border.Position = UDim2.new(0, 0, 1, -1)
	cb_border.BackgroundColor3 = Color3.fromRGB(100, 100, 100)
	cb_border.BackgroundTransparency = 0.4
	cb_border.BorderSizePixel = 0
	cb_border.ZIndex = 2
	cb_border.Parent = channel_bar

	channel_tabs_container = Instance.new("Frame")
	channel_tabs_container.Name = "ContentContainer"
	channel_tabs_container.Size = UDim2.new(1, 0, 1, 0)
	channel_tabs_container.BackgroundTransparency = 1
	channel_tabs_container.ZIndex = 2
	channel_tabs_container.Parent = channel_bar
	local tabs_layout = Instance.new("UIListLayout")
	tabs_layout.FillDirection = Enum.FillDirection.Horizontal
	tabs_layout.HorizontalAlignment = Enum.HorizontalAlignment.Left
	tabs_layout.VerticalAlignment = Enum.VerticalAlignment.Top
	tabs_layout.SortOrder = Enum.SortOrder.LayoutOrder
	tabs_layout.Parent = channel_tabs_container

	-- 2a2. 8px topPadding spacer between channelBar and chatWindow (LayoutOrder = 2, matching Studio)
	top_padding = Instance.new("Frame")
	top_padding.Name = "topPadding"
	top_padding.LayoutOrder = 2
	top_padding.Size = UDim2.new(1, 0, 0, 8)
	top_padding.BackgroundTransparency = 1
	top_padding.BorderSizePixel = 0
	top_padding.Visible = false
	top_padding.Parent = app_layout

	-- 2b. Chat window (LayoutOrder = 3)
	chat_window = Instance.new("Frame")
	chat_window.Name = "chatWindow"
	chat_window.LayoutOrder = 3
	chat_window.Size = UDim2.new(1, 0, 0, 186)
	chat_window.BackgroundColor3 = Color3.fromRGB(25, 27, 29)
	chat_window.BackgroundTransparency = 1
	chat_window.BorderSizePixel = 0
	chat_window.ClipsDescendants = true
	chat_window.Parent = app_layout
	local cw_corner = Instance.new("UICorner")
	cw_corner.CornerRadius = UDim.new(0, 8)
	cw_corner.Parent = chat_window

	chat_window.MouseEnter:Connect(function() hover_window = true; last_activity_t = os.clock() end)
	chat_window.MouseLeave:Connect(function() hover_window = false end)

	dot_menu = Instance.new("TextButton")
	dot_menu.Name = "DotMenu"
	dot_menu.Size = UDim2.fromOffset(28, 22)
	dot_menu.Position = UDim2.new(1, -34, 0, 4)
	dot_menu.BackgroundTransparency = 1
	dot_menu.Text = "•••"
	dot_menu.TextSize = 14
	dot_menu.TextColor3 = Color3.fromRGB(210, 214, 220)
	dot_menu.FontFace = Font.new("rbxasset://fonts/families/BuilderSans.json", Enum.FontWeight.Bold)
	dot_menu.Visible = false
	dot_menu.ZIndex = 5
	dot_menu.Parent = chat_window

	scroll_view = Instance.new("ScrollingFrame")
	scroll_view.Name = "scrollView"
	scroll_view.Position = UDim2.fromOffset(8, 6)
	scroll_view.Size = UDim2.new(1, -16, 1, -12)
	scroll_view.BackgroundTransparency = 1
	scroll_view.BorderSizePixel = 0
	scroll_view.ScrollBarThickness = 5
	scroll_view.ScrollBarImageColor3 = Color3.fromRGB(180, 180, 185)
	scroll_view.ScrollBarImageTransparency = 0.5
	scroll_view.CanvasSize = UDim2.new(0, 0, 0, 0)
	scroll_view.AutomaticCanvasSize = Enum.AutomaticSize.Y
	scroll_view.ClipsDescendants = true
	scroll_view.Parent = chat_window

	local sv_layout = Instance.new("UIListLayout")
	sv_layout.Name = "$layout"
	sv_layout.FillDirection = Enum.FillDirection.Vertical
	sv_layout.HorizontalAlignment = Enum.HorizontalAlignment.Left
	sv_layout.VerticalAlignment = Enum.VerticalAlignment.Top
	sv_layout.SortOrder = Enum.SortOrder.LayoutOrder
	sv_layout.Padding = UDim.new(0, 0)
	sv_layout.Parent = scroll_view

	-- 2c. Chat input bar row (LayoutOrder = 4)
	input_row = Instance.new("Frame")
	input_row.Name = "chatInputRow"
	input_row.LayoutOrder = 4
	input_row.Size = UDim2.new(1, 0, 0, 47)
	input_row.BackgroundColor3 = Color3.fromRGB(25, 27, 29)
	input_row.BackgroundTransparency = 1
	input_row.BorderSizePixel = 0
	input_row.Parent = app_layout

	input_row.MouseEnter:Connect(function() hover_bar = true; last_activity_t = os.clock() end)
	input_row.MouseLeave:Connect(function() hover_bar = false end)

	input_group = Instance.new("Frame")
	input_group.Name = "InputGroup"
	input_group.Position = UDim2.fromOffset(8, 6)
	input_group.Size = UDim2.new(1, -16, 0, 38)
	input_group.BackgroundColor3 = Color3.fromRGB(25, 27, 29)
	input_group.BackgroundTransparency = 1
	input_group.BorderSizePixel = 0
	input_group.Parent = input_row
	local ig_corner = Instance.new("UICorner")
	ig_corner.CornerRadius = UDim.new(0, 20)
	ig_corner.Parent = input_group
	input_stroke = Instance.new("UIStroke")
	input_stroke.Color = Color3.fromRGB(208, 217, 251)
	input_stroke.Thickness = 1
	input_stroke.Transparency = 1
	input_stroke.Parent = input_group

	target_chip = Instance.new("TextButton")
	target_chip.Name = "TargetChannelChip"
	target_chip.Position = UDim2.fromOffset(10, 0)
	target_chip.Size = UDim2.new(0, 0, 1, 0)
	target_chip.BackgroundTransparency = 1
	target_chip.Text = ""
	target_chip.TextSize = 14
	target_chip.FontFace = Font.new("rbxasset://fonts/families/BuilderSans.json", Enum.FontWeight.Medium)
	target_chip.TextXAlignment = Enum.TextXAlignment.Left
	target_chip.Visible = false
	target_chip.Parent = input_group
	target_chip.Activated:Connect(function()
		reset_target_to_general()
		update_target_chip()
	end)

	input_box = Instance.new("TextBox")
	input_box.Name = "TextBox"
	input_box.Position = UDim2.fromOffset(16, 0)
	input_box.Size = UDim2.new(1, -56, 1, 0)
	input_box.BackgroundTransparency = 1
	input_box.BorderSizePixel = 0
	input_box.ClearTextOnFocus = false
	input_box.Text = ""
	input_box.PlaceholderText = "To chat click here or press / key"
	input_box.PlaceholderColor3 = Color3.fromRGB(178, 178, 178)
	input_box.TextColor3 = Color3.fromRGB(255, 255, 255)
	input_box.TextSize = 18
	input_box.FontFace = Font.new("rbxasset://fonts/families/BuilderSans.json", Enum.FontWeight.Medium)
	input_box.TextXAlignment = Enum.TextXAlignment.Left
	input_box.TextYAlignment = Enum.TextYAlignment.Center
	input_box.ClipsDescendants = true
	input_box.Parent = input_group

	send_btn = Instance.new("TextButton")
	send_btn.Name = "SendButton"
	send_btn.Size = UDim2.fromOffset(32, 32)
	send_btn.Position = UDim2.new(1, -35, 0.5, -16)
	send_btn.BackgroundColor3 = Color3.fromRGB(208, 217, 251)
	send_btn.BackgroundTransparency = 0.88
	send_btn.BorderSizePixel = 0
	send_btn.Text = "↑"
	send_btn.TextSize = 16
	send_btn.FontFace = Font.new("rbxasset://fonts/families/BuilderSans.json", Enum.FontWeight.Medium)
	send_btn.TextColor3 = Color3.fromRGB(247, 247, 248)
	send_btn.Parent = input_group
	local sb_corner = Instance.new("UICorner")
	sb_corner.CornerRadius = UDim.new(1, 0)
	sb_corner.Parent = send_btn

	-- Autocomplete dropdown (LayoutOrder = 5)
	ac_frame = Instance.new("Frame")
	ac_frame.Name = "AutocompleteDropdown"
	ac_frame.LayoutOrder = 5
	ac_frame.Size = UDim2.new(0.95, 0, 0, 0)
	ac_frame.BackgroundColor3 = Color3.fromRGB(25, 27, 29)
	ac_frame.BackgroundTransparency = 0.12
	ac_frame.BorderSizePixel = 0
	ac_frame.Visible = false
	ac_frame.ClipsDescendants = true
	ac_frame.Parent = app_layout
	local ac_corner = Instance.new("UICorner")
	ac_corner.CornerRadius = UDim.new(0, 6)
	ac_corner.Parent = ac_frame
	local ac_stroke = Instance.new("UIStroke")
	ac_stroke.Color = Color3.fromRGB(255, 255, 255)
	ac_stroke.Transparency = 0.8
	ac_stroke.Parent = ac_frame
	ac_list = Instance.new("Frame")
	ac_list.Name = "List"
	ac_list.Position = UDim2.fromOffset(4, 4)
	ac_list.Size = UDim2.new(1, -8, 1, -8)
	ac_list.BackgroundTransparency = 1
	ac_list.Parent = ac_frame
	local ac_layout = Instance.new("UIListLayout")
	ac_layout.FillDirection = Enum.FillDirection.Vertical
	ac_layout.SortOrder = Enum.SortOrder.LayoutOrder
	ac_layout.Parent = ac_list

	-- Wire TextBox events
	input_box.Focused:Connect(function()
		box_focused = true
		last_activity_t = os.clock()
		update_target_chip()
		update_send_button()
		refresh_autocomplete()
	end)
	input_box.FocusLost:Connect(function(enter_pressed)
		box_focused = false
		last_activity_t = os.clock()
		hide_autocomplete()
		if enter_pressed then
			local txt = input_box.Text
			input_box.Text = ""
			update_send_button()
			submit_text(txt)
		end
	end)
	input_box:GetPropertyChangedSignal("Text"):Connect(function()
		local t = input_box.Text
		local trunc = utf8_truncate(t, 200)
		if trunc ~= t then
			input_box.Text = trunc
			return
		end
		last_activity_t = os.clock()
		update_send_button()
		refresh_autocomplete()
	end)
	send_btn.Activated:Connect(function()
		local txt = input_box.Text
		input_box.Text = ""
		update_send_button()
		hide_autocomplete()
		submit_text(txt)
	end)

	-- 3. Legacy chat ScreenGui (active when ChatVersion == LegacyChatService and Chat.LoadDefaultChat)
	legacy_gui = Instance.new("ScreenGui")
	legacy_gui.Name = "LegacyChat"
	legacy_gui.ResetOnSpawn = false
	legacy_gui.DisplayOrder = -1
	legacy_gui.Enabled = false
	legacy_gui.ZIndexBehavior = Enum.ZIndexBehavior.Sibling
	legacy_gui.Parent = core

	legacy_window = Instance.new("Frame")
	legacy_window.Name = "ChatWindow"
	legacy_window.Position = UDim2.fromOffset(8, 8)
	legacy_window.Size = UDim2.fromOffset(360, 175)
	legacy_window.BackgroundColor3 = Color3.fromRGB(0, 0, 0)
	legacy_window.BackgroundTransparency = 0.55
	legacy_window.BorderSizePixel = 0
	legacy_window.Parent = legacy_gui
	local lw_corner = Instance.new("UICorner")
	lw_corner.CornerRadius = UDim.new(0, 6)
	lw_corner.Parent = legacy_window

	legacy_scroll = Instance.new("ScrollingFrame")
	legacy_scroll.Name = "Scroll"
	legacy_scroll.Position = UDim2.fromOffset(8, 6)
	legacy_scroll.Size = UDim2.new(1, -16, 1, -12)
	legacy_scroll.BackgroundTransparency = 1
	legacy_scroll.BorderSizePixel = 0
	legacy_scroll.ScrollBarThickness = 5
	legacy_scroll.AutomaticCanvasSize = Enum.AutomaticSize.Y
	legacy_scroll.CanvasSize = UDim2.new(0, 0, 0, 0)
	legacy_scroll.Parent = legacy_window
	local lsl = Instance.new("UIListLayout")
	lsl.FillDirection = Enum.FillDirection.Vertical
	lsl.SortOrder = Enum.SortOrder.LayoutOrder
	lsl.Padding = UDim.new(0, 2)
	lsl.Parent = legacy_scroll

	legacy_bar = Instance.new("Frame")
	legacy_bar.Name = "ChatBar"
	legacy_bar.Position = UDim2.fromOffset(8, 187)
	legacy_bar.Size = UDim2.fromOffset(360, 34)
	legacy_bar.BackgroundColor3 = Color3.fromRGB(0, 0, 0)
	legacy_bar.BackgroundTransparency = 0.45
	legacy_bar.BorderSizePixel = 0
	legacy_bar.Parent = legacy_gui
	local lb_corner = Instance.new("UICorner")
	lb_corner.CornerRadius = UDim.new(0, 6)
	lb_corner.Parent = legacy_bar

	legacy_box = Instance.new("TextBox")
	legacy_box.Name = "TextBox"
	legacy_box.Position = UDim2.fromOffset(8, 0)
	legacy_box.Size = UDim2.new(1, -16, 1, 0)
	legacy_box.BackgroundTransparency = 1
	legacy_box.BorderSizePixel = 0
	legacy_box.ClearTextOnFocus = false
	legacy_box.Text = ""
	legacy_box.PlaceholderText = 'To chat click here or press "/" key'
	legacy_box.PlaceholderColor3 = Color3.fromRGB(178, 178, 178)
	legacy_box.TextColor3 = Color3.fromRGB(255, 255, 255)
	legacy_box.TextSize = 15
	legacy_box.FontFace = Font.new("rbxasset://fonts/families/SourceSansPro.json", Enum.FontWeight.SemiBold)
	legacy_box.TextXAlignment = Enum.TextXAlignment.Left
	legacy_box.Parent = legacy_bar

	legacy_box.FocusLost:Connect(function(enter_pressed)
		if enter_pressed then
			local txt = legacy_box.Text
			legacy_box.Text = ""
			submit_text(txt)
		end
	end)

	-- 4. Keyboard shortcuts via UserInputService
	local UIS = svc.UserInputService
	UIS.InputBegan:Connect(function(input, gpe)
		if input.UserInputType ~= Enum.UserInputType.Keyboard then return end
		local kc = input.KeyCode
		local tcs = is_tcs_mode()
		local cib = get_cib()
		local active_box = tcs and ((cib and cib.TextBox) or input_box) or legacy_box

		-- Focus hotkey (default Slash, or ChatInputBarConfiguration.KeyboardKeyCode)
		if not gpe and not box_focused and core_chat_enabled() and not chat_bar_disabled then
			local hotkey = (tcs and cib and cib.KeyboardKeyCode) or Enum.KeyCode.Slash
			if hotkey ~= Enum.KeyCode.Unknown and kc == hotkey then
				if tcs and cib and not cib.Enabled then return end
				if not tcs and not svc.Chat.LoadDefaultChat then return end
				chat_visible = true
				unread_count = 0
				update_topbar_badge()
				last_activity_t = os.clock()
				if active_box then
					active_box:CaptureFocus()
				end
				return
			end
		end

		-- Focused chat keybinds: Tab autocomplete, Up/Down history, Backspace resets channel chip
		if box_focused and tcs and input_box then
			if kc == Enum.KeyCode.Tab then
				if #ac_items > 0 then
					apply_autocomplete(ac_items[ac_selected] or ac_items[1])
				else
					refresh_autocomplete()
					if #ac_items > 0 then apply_autocomplete(ac_items[1]) end
				end
			elseif kc == Enum.KeyCode.Up then
				if #ac_items > 0 then
					ac_selected = ((ac_selected - 2) % #ac_items) + 1
					refresh_autocomplete()
				elseif #history > 0 then
					if history_idx == 0 then
						history_draft = input_box.Text
						history_idx = #history
					elseif history_idx > 1 then
						history_idx = history_idx - 1
					end
					input_box.Text = history[history_idx] or ""
				end
			elseif kc == Enum.KeyCode.Down then
				if #ac_items > 0 then
					ac_selected = (ac_selected % #ac_items) + 1
					refresh_autocomplete()
				elseif history_idx > 0 then
					if history_idx < #history then
						history_idx = history_idx + 1
						input_box.Text = history[history_idx] or ""
					else
						history_idx = 0
						input_box.Text = history_draft
					end
				end
			elseif kc == Enum.KeyCode.Backspace and input_box.Text == "" then
				reset_target_to_general()
				update_target_chip()
			end
		end
	end)
end

-- Developer-assigned custom ChatInputBarConfiguration.TextBox
local function sync_custom_textbox()
	local cib = get_cib()
	local tb = cib and cib.TextBox
	if tb == custom_textbox_bound then return end
	for _, c in custom_textbox_conns do pcall(function() c:Disconnect() end) end
	custom_textbox_conns = {}
	custom_textbox_bound = tb
	if tb and typeof(tb) == "Instance" and tb:IsA("TextBox") then
		table.insert(custom_textbox_conns, tb.FocusLost:Connect(function(enter)
			if enter and tb.Text ~= "" then
				local t = tb.Text
				tb.Text = ""
				submit_text(t)
			end
		end))
	end
end

-- Add a formatted message line to the ExperienceChat window.
local next_line_order = 0
local function add_window_line(info)
	build_ui()
	if not scroll_view then return end
	local cwc = get_cwc()
	next_line_order += 1
	last_message_t = os.clock()
	if not chat_visible then
		unread_count += 1
		update_topbar_badge()
	end

	local wp = info.window_props
	local prefix_props = wp and wp.PrefixTextProperties or nil
	local body_font = (wp and wp.FontFace) or (cwc and cwc.FontFace) or Font.new("rbxasset://fonts/families/BuilderSans.json", Enum.FontWeight.Medium)
	local body_size = (wp and wp.TextSize and wp.TextSize > 0 and wp.TextSize) or (cwc and cwc.TextSize) or 18
	local body_color = (wp and wp.TextColor3) or (info.system and Color3.fromRGB(212, 212, 212)) or (cwc and cwc.TextColor3) or Color3.fromRGB(255, 255, 255)
	local stroke_color = (wp and wp.TextStrokeColor3) or (cwc and cwc.TextStrokeColor3) or Color3.fromRGB(0, 0, 0)
	local stroke_trans = (wp and wp.TextStrokeTransparency) or (cwc and cwc.TextStrokeTransparency) or 0.5

	local prefix = info.prefix or ""
	local body = info.text or ""
	local full_text
	if info.system then
		-- Studio ExperienceChat wraps every system message (RBXSystem, translation notice,
		-- team-change notice) in <font color='#d4d4d4' size="16">...</font>, so inner
		-- <font color='...'> tags (such as "<font color='#55ccff'>System:</font>") keep
		-- their explicit color while the remaining system body renders in #d4d4d4 at 16px.
		full_text = string.format('<font color="#d4d4d4" size="16">%s</font>', body)
	elseif prefix ~= "" then
		local pcol
		if prefix_props and prefix_props.TextColor3 then
			pcol = prefix_props.TextColor3
		elseif info.name_color then
			pcol = info.name_color
		else
			-- Studio hashes player.Name (not DisplayName) through GetNameValue.
			local hash_key = info.sender_name
			if not hash_key or hash_key == "" then
				hash_key = prefix:gsub("<[^>]+>", ""):gsub(":$", "")
				hash_key = hash_key:match("(%S+)$") or hash_key
			end
			pcol = name_color(hash_key)
		end
		-- Always wrap the full PrefixText in the player's name/team color (matching Studio:
		-- e.g. <font color="#e8bac8"><font color='#ffaa00'>[VIP]</font> Player:</font>),
		-- so inner <font> tags keep their custom color while the username stays colored.
		full_text = string.format('<font color="%s">%s</font> %s', color_hex(pcol), prefix, body)
	else
		full_text = body
	end

	local lbl = Instance.new("TextLabel")
	lbl.Name = info.message_id ~= "" and info.message_id or ("Msg_" .. next_line_order)
	lbl.LayoutOrder = next_line_order
	lbl.Size = UDim2.new(1, -34, 0, 0)
	lbl.AutomaticSize = Enum.AutomaticSize.Y
	lbl.BackgroundTransparency = 1
	lbl.BorderSizePixel = 0
	lbl.RichText = true
	lbl.TextWrapped = true
	lbl.TextXAlignment = Enum.TextXAlignment.Left
	lbl.TextYAlignment = Enum.TextYAlignment.Top
	lbl.FontFace = body_font
	lbl.TextSize = body_size
	lbl.TextColor3 = body_color
	lbl.TextStrokeColor3 = stroke_color
	lbl.TextStrokeTransparency = stroke_trans
	lbl.TextTransparency = 0
	lbl.Text = full_text
	lbl.Parent = scroll_view

	table.insert(line_instances, {
		frame = lbl,
		channel = info.channel,
		id = next_line_order,
		time = last_message_t,
		base_stroke = stroke_trans,
	})
	while #line_instances > 100 do
		local old = table.remove(line_instances, 1)
		if old.frame then old.frame:Destroy() end
	end
	refresh_line_visibility()
	scroll_to_bottom(scroll_view)
	task.defer(function() scroll_to_bottom(scroll_view) end)
end

local function clear_window_lines()
	for _, entry in line_instances do
		if entry.frame then entry.frame:Destroy() end
	end
	table.clear(line_instances)
	if legacy_scroll then
		for _, c in legacy_scroll:GetChildren() do
			if c:IsA("TextLabel") then c:Destroy() end
		end
	end
end

-- Add a line to the LegacyChat window (and SetCore("ChatMakeSystemMessage")).
local legacy_order = 0
local function add_legacy_line(text, color3, text_size, font_face)
	build_ui()
	if not legacy_scroll then return end
	legacy_order += 1
	local lbl = Instance.new("TextLabel")
	lbl.Name = "LegacyMsg_" .. legacy_order
	lbl.LayoutOrder = legacy_order
	lbl.Size = UDim2.new(1, -6, 0, 0)
	lbl.AutomaticSize = Enum.AutomaticSize.Y
	lbl.BackgroundTransparency = 1
	lbl.RichText = true
	lbl.TextWrapped = true
	lbl.TextXAlignment = Enum.TextXAlignment.Left
	lbl.TextYAlignment = Enum.TextYAlignment.Top
	lbl.TextColor3 = color3 or Color3.fromRGB(255, 255, 255)
	lbl.TextStrokeColor3 = Color3.fromRGB(0, 0, 0)
	lbl.TextStrokeTransparency = 0.5
	lbl.TextSize = text_size or 16
	lbl.FontFace = font_face or Font.new("rbxasset://fonts/families/SourceSansPro.json", Enum.FontWeight.SemiBold)
	lbl.Text = text
	lbl.Parent = legacy_scroll
	scroll_to_bottom(legacy_scroll)
end

-- BubbleChat on CoreGui.ExperienceChat.bubbleChat (BillboardGuis)
local bubble_seq = 0
local function get_user_specific_bubble_settings(adornee, user_id)
	local legacy_settings = API.get_bubble_settings()
	if not legacy_settings or type(legacy_settings.UserSpecificSettings) ~= "table" then
		return nil
	end
	local uss = legacy_settings.UserSpecificSettings
	if user_id and uss[user_id] then return uss[user_id] end
	if adornee then
		local ok, full = pcall(function() return adornee:GetFullName() end)
		if ok and uss[full] then return uss[full] end
	end
	return nil
end

local function apply_bcc_children(bcc, frame, legacy_settings, user_spec)
	-- Apply BubbleChatConfiguration children (UICorner, UIPadding, UIGradient, ImageLabel)
	local corner_r = UDim.new(0, 12)
	local corner_on = true
	if legacy_settings and legacy_settings.CornerEnabled == false then corner_on = false end
	if legacy_settings and typeof(legacy_settings.CornerRadius) == "UDim" then corner_r = legacy_settings.CornerRadius end
	local bcc_corner = bcc and bcc:FindFirstChildOfClass("UICorner")
	if bcc_corner then corner_r = bcc_corner.CornerRadius end
	if corner_on then
		local c = Instance.new("UICorner")
		c.CornerRadius = corner_r
		c.Parent = frame
	end

	local pad = Instance.new("UIPadding")
	local bcc_pad = bcc and bcc:FindFirstChildOfClass("UIPadding")
	if bcc_pad then
		pad.PaddingTop = bcc_pad.PaddingTop
		pad.PaddingBottom = bcc_pad.PaddingBottom
		pad.PaddingLeft = bcc_pad.PaddingLeft
		pad.PaddingRight = bcc_pad.PaddingRight
	else
		local p = (legacy_settings and tonumber(legacy_settings.Padding)) or 8
		pad.PaddingTop = UDim.new(0, p)
		pad.PaddingBottom = UDim.new(0, p)
		pad.PaddingLeft = UDim.new(0, p + 4)
		pad.PaddingRight = UDim.new(0, p + 4)
	end
	pad.Parent = frame

	local bcc_grad = bcc and bcc:FindFirstChildOfClass("UIGradient")
	if bcc_grad and bcc_grad.Enabled then
		local g = Instance.new("UIGradient")
		g.Enabled = true
		g.Color = bcc_grad.Color
		g.Offset = bcc_grad.Offset
		g.Rotation = bcc_grad.Rotation
		g.Transparency = bcc_grad.Transparency
		g.Parent = frame
	end

	local bcc_img = bcc and bcc:FindFirstChildOfClass("ImageLabel")
	if bcc_img and bcc_img.Image ~= "" then
		frame.Image = bcc_img.Image
		frame.ImageColor3 = bcc_img.ImageColor3
		frame.ImageRectOffset = bcc_img.ImageRectOffset
		frame.ImageRectSize = bcc_img.ImageRectSize
		frame.ScaleType = bcc_img.ScaleType
		frame.SliceCenter = bcc_img.SliceCenter
		frame.SliceScale = bcc_img.SliceScale
		frame.TileSize = bcc_img.TileSize
	end
end

-- Match Studio's BubbleChatBillboard:getVerticalOffset:
-- For a player character model, distance from adornee to top of character bounding box;
-- for a standalone BasePart (DisplayBubble on a Part/Head), half the part's Y size.
local function get_vertical_offset(adornee, user_id)
	if user_id and adornee and adornee.Parent and adornee.Parent:IsA("Model") then
		local ok, orientation, size = pcall(function() return adornee.Parent:GetBoundingBox() end)
		if ok and typeof(orientation) == "CFrame" and typeof(size) == "Vector3" and size.Y > 0.1 then
			local rel = orientation:PointToObjectSpace(adornee.Position)
			return size.Y * 0.5 - rel.Y
		end
		local part_h = 2
		pcall(function() if adornee:IsA("BasePart") then part_h = adornee.Size.Y end end)
		return part_h * 0.5 + 2.0
	end
	if adornee and adornee:IsA("BasePart") then
		return adornee.Size.Y * 0.5
	end
	return 0
end

local function ensure_bubble_board(adornee, user_id)
	build_ui()
	if not bubble_gui or not adornee then return nil end
	local rec = bubble_boards[adornee]
	if rec and rec.board and rec.board.Parent then
		return rec
	end
	local bcc = get_bcc()
	local lp = svc.Players.LocalPlayer
	local is_local = lp and user_id and user_id == lp.UserId
	local board = Instance.new("BillboardGui")
	board.Name = "BubbleChat_" .. tostring(user_id or adornee.Name)
	board.Adornee = adornee
	board.Size = UDim2.fromOffset(500, 200)
	board.SizeOffset = Vector2.new(0, 0.5)
	board.MaxDistance = bcc and bcc.MaxDistance or 100
	board.AlwaysOnTop = false
	local base_y = get_vertical_offset(adornee, user_id)
	local base_studs = (is_local and bcc and bcc.LocalPlayerStudsOffset) or Vector3.new(0, 1, 0.1)
	board.StudsOffset = base_studs + Vector3.new(0, bcc and bcc.VerticalStudsOffset or 0, 0)
	board.StudsOffsetWorldSpace = Vector3.new(0, base_y, 0)
	board.Parent = bubble_gui

	local list = Instance.new("Frame")
	list.Name = "BubbleChatList"
	list.Size = UDim2.fromScale(1, 1)
	list.BackgroundTransparency = 1
	list.Parent = board

	local pad = Instance.new("UIPadding")
	pad.Name = "CaretPadding"
	pad.PaddingBottom = UDim.new(0, 8)
	pad.Parent = list

	local layout = Instance.new("UIListLayout")
	layout.Name = "Layout"
	layout.FillDirection = Enum.FillDirection.Vertical
	layout.HorizontalAlignment = Enum.HorizontalAlignment.Center
	layout.VerticalAlignment = Enum.VerticalAlignment.Bottom
	layout.SortOrder = Enum.SortOrder.LayoutOrder
	layout.Padding = UDim.new(0, bcc and bcc.BubblesSpacing or 6)
	layout.Parent = list

	rec = {
		board = board,
		list = list,
		layout = layout,
		entries = {},
		adornee = adornee,
		user_id = user_id,
		is_local = is_local,
		base_y = base_y,
		minimized = nil,
		typing = nil,
	}
	bubble_boards[adornee] = rec
	return rec
end

local function refresh_bubble_carets(rec)
	local n = #rec.entries
	for i, e in ipairs(rec.entries) do
		local show_caret = (i == n) and e.tail_visible and not (rec.typing and rec.typing.Visible)
		if e.caret then
			e.caret.Visible = show_caret
		end
		if e.wrap and e.box_w and e.box_h then
			e.wrap.Size = UDim2.fromOffset(e.box_w, e.box_h + (show_caret and 5 or 0))
		end
	end
end

local function strip_rich_tags(s)
	local out = tostring(s or ""):gsub("<[^>]+>", "")
	out = out:gsub("&lt;", "<"):gsub("&gt;", ">"):gsub("&amp;", "&"):gsub("&quot;", '"'):gsub("&apos;", "'")
	return out
end

local function measure_text(plain, text_size, max_w, font_face)
	local enum_font = Enum.Font.BuilderSansMedium
	if font_face and typeof(font_face) == "Font" and font_face.Family:find("Gotham", 1, true) then
		enum_font = Enum.Font.GothamMedium
	end
	local ok, v = pcall(function()
		return svc.TextService:GetTextSize(plain, text_size, enum_font, Vector2.new(max_w, 1000))
	end)
	if ok and typeof(v) == "Vector2" and v.X > 0 and v.Y > 0 then
		return math.ceil(v.X), math.ceil(v.Y)
	end
	-- Fallback heuristic
	local approx_w = math.min(max_w, math.max(24, math.ceil(#plain * text_size * 0.52)))
	local lines = math.max(1, math.ceil((#plain * text_size * 0.52) / math.max(1, max_w)))
	return approx_w, math.ceil(lines * text_size * 1.2)
end

local function add_bubble_ui(adornee, text, bp, user_id, chat_color)
	local bcc = get_bcc()
	if not bcc or not bcc.Enabled then
		-- Legacy Chat:Chat still draws bubbles if Chat.BubbleChatEnabled or explicit Chat:Chat
		if not chat_color then return end
	end
	local rec = ensure_bubble_board(adornee, user_id)
	if not rec then return end

	bubble_seq += 1
	local legacy_settings = API.get_bubble_settings()
	local user_spec = get_user_specific_bubble_settings(adornee, user_id)

	local bg_c = (bp and bp.BackgroundColor3)
		or (user_spec and user_spec.BackgroundColor3)
		or (bcc and bcc.BackgroundColor3)
		or Color3.fromRGB(250, 250, 250)
	local bg_t = (bp and bp.BackgroundTransparency)
		or (user_spec and user_spec.Transparency)
		or (bcc and bcc.BackgroundTransparency)
		or 0.1
	local fg_c = (bp and bp.TextColor3)
		or (user_spec and user_spec.TextColor3)
		or (bcc and bcc.TextColor3)
		or Color3.fromRGB(57, 59, 61)
	local ts = (bp and bp.TextSize and bp.TextSize > 0 and bp.TextSize)
		or (user_spec and user_spec.TextSize)
		or (bcc and bcc.TextSize)
		or 20
	local ff = (bp and bp.FontFace)
		or (bcc and bcc.FontFace)
		or Font.new("rbxasset://fonts/families/BuilderSans.json", Enum.FontWeight.Medium)
	local tail_vis = true
	if bp and bp.TailVisible ~= nil then tail_vis = bp.TailVisible
	elseif bcc and bcc.TailVisible ~= nil then tail_vis = bcc.TailVisible end

	-- Legacy Chat:Chat(part, msg, ChatColor) border accent
	local border_accent = nil
	if chat_color == Enum.ChatColor.Blue then border_accent = Color3.fromRGB(35, 115, 225)
	elseif chat_color == Enum.ChatColor.Green then border_accent = Color3.fromRGB(40, 175, 85)
	elseif chat_color == Enum.ChatColor.Red then border_accent = Color3.fromRGB(220, 60, 60) end

	-- Compute exact bubble pixel size matching Studio's ExperienceChat BubbleChat (pad 8px)
	local pad_h, pad_v = 8, 8
	local bcc_pad = bcc and bcc:FindFirstChildOfClass("UIPadding")
	if bcc_pad then
		pad_h = math.max(6, math.floor((bcc_pad.PaddingLeft.Offset + bcc_pad.PaddingRight.Offset) * 0.5))
		pad_v = math.max(6, math.floor((bcc_pad.PaddingTop.Offset + bcc_pad.PaddingBottom.Offset) * 0.5))
	elseif legacy_settings and tonumber(legacy_settings.Padding) then
		pad_h = tonumber(legacy_settings.Padding)
		pad_v = tonumber(legacy_settings.Padding)
	end
	local max_w = (legacy_settings and tonumber(legacy_settings.MaxWidth)) or 300
	local plain = strip_rich_tags(text)
	local tw, th = measure_text(plain, ts, max_w - pad_h * 2, ff)
	local box_w = math.clamp(tw + pad_h * 2 + 4, 32, max_w)
	local box_h = math.max(ts + pad_v * 2, th + pad_v * 2)

	local wrap = Instance.new("Frame")
	wrap.Name = "Bubble_" .. bubble_seq
	wrap.LayoutOrder = bubble_seq
	wrap.Size = UDim2.fromOffset(box_w, box_h + (tail_vis and 5 or 0))
	wrap.BackgroundTransparency = 1
	wrap.Parent = rec.list

	local wrap_layout = Instance.new("UIListLayout")
	wrap_layout.Name = "Layout"
	wrap_layout.FillDirection = Enum.FillDirection.Vertical
	wrap_layout.HorizontalAlignment = Enum.HorizontalAlignment.Center
	wrap_layout.VerticalAlignment = Enum.VerticalAlignment.Top
	wrap_layout.SortOrder = Enum.SortOrder.LayoutOrder
	wrap_layout.Padding = UDim.new(0, -1)
	wrap_layout.Parent = wrap

	local frame = Instance.new("ImageLabel")
	frame.Name = "ChatBubbleFrame"
	frame.LayoutOrder = 1
	frame.Size = UDim2.fromOffset(box_w, box_h)
	frame.BackgroundColor3 = bg_c
	frame.BackgroundTransparency = bg_t
	frame.BorderSizePixel = 0
	frame.Image = ""
	frame.Parent = wrap
	apply_bcc_children(bcc, frame, legacy_settings, user_spec)

	if border_accent then
		local st = Instance.new("UIStroke")
		st.Color = border_accent
		st.Thickness = 2
		st.Parent = frame
	end

	local lbl = Instance.new("TextLabel")
	lbl.Name = "Text"
	lbl.Position = UDim2.fromScale(0, 0)
	lbl.Size = UDim2.fromScale(1, 1)
	lbl.BackgroundTransparency = 1
	lbl.RichText = true
	lbl.TextWrapped = true
	lbl.TextXAlignment = Enum.TextXAlignment.Center
	lbl.TextYAlignment = Enum.TextYAlignment.Center
	lbl.FontFace = ff
	lbl.TextSize = ts
	lbl.TextColor3 = fg_c
	lbl.TextStrokeTransparency = 1
	lbl.Text = text
	lbl.Parent = frame

	-- Downward caret triangle (9x6 clipped diamond, matching Studio's Caret without external texture dependency)
	local caret = Instance.new("Frame")
	caret.Name = "Caret"
	caret.LayoutOrder = 2
	caret.Size = UDim2.fromOffset(12, 6)
	caret.BackgroundTransparency = 1
	caret.BorderSizePixel = 0
	caret.ClipsDescendants = true
	caret.Visible = tail_vis
	caret.Parent = wrap
	local caret_tri = Instance.new("Frame")
	caret_tri.Name = "Tri"
	caret_tri.AnchorPoint = Vector2.new(0.5, 0.5)
	caret_tri.Position = UDim2.fromOffset(6, 0)
	caret_tri.Size = UDim2.fromOffset(8, 8)
	caret_tri.Rotation = 45
	caret_tri.BackgroundColor3 = bg_c
	caret_tri.BackgroundTransparency = bg_t
	caret_tri.BorderSizePixel = 0
	caret_tri.Parent = caret

	table.insert(rec.entries, {
		wrap = wrap,
		frame = frame,
		label = lbl,
		caret = caret,
		caret_tri = caret_tri,
		box_w = box_w,
		box_h = box_h,
		tail_visible = tail_vis,
		base_bg_t = bg_t,
		created = os.clock(),
	})

	local max_b = math.max(1, math.floor((bcc and bcc.MaxBubbles) or 3))
	while #rec.entries > max_b do
		local old = table.remove(rec.entries, 1)
		if old.wrap then old.wrap:Destroy() end
	end
	refresh_bubble_carets(rec)
end

-- Studio does NOT show a typing indicator bubble over the local player's own head
-- while focused/typing (verified via studio_focused_typing_check.txt).
local function update_typing_indicator(_dt)
end

-- Step bubbles: expiration, smooth fade-out, distance minimize, and config sync.
local function step_bubbles(dt)
	local bcc = get_bcc()
	local dur = (bcc and bcc.BubbleDuration) or 15
	local min_dist = (bcc and bcc.MinimizeDistance) or 40
	local max_dist = (bcc and bcc.MaxDistance) or 100
	local spacing = (bcc and bcc.BubblesSpacing) or 6
	local vert_off = (bcc and bcc.VerticalStudsOffset) or 0
	local lp = svc.Players.LocalPlayer
	local lp_off = (bcc and bcc.LocalPlayerStudsOffset) or Vector3.zero
	local cam = workspace.CurrentCamera
	local cam_pos = cam and cam.CFrame.Position or nil
	local now = os.clock()

	update_typing_indicator(dt)

	for adornee, rec in pairs(bubble_boards) do
		local alive = pcall(function() return adornee:IsDescendantOf(workspace) end) and adornee:IsDescendantOf(workspace)
		if not alive then
			if rec.board then rec.board:Destroy() end
			bubble_boards[adornee] = nil
		else
			rec.board.MaxDistance = max_dist
			rec.layout.Padding = UDim.new(0, spacing)
			rec.board.StudsOffsetWorldSpace = Vector3.new(0, rec.base_y, 0)
			local base_studs = (lp and rec.user_id == lp.UserId) and lp_off or Vector3.new(0, 1, 0.1)
			rec.board.StudsOffset = base_studs + Vector3.new(0, vert_off, 0)

			-- Expire & fade bubbles (keeping UIListLayout stack heights intact while fading)
			local removed = false
			for i = #rec.entries, 1, -1 do
				local e = rec.entries[i]
				local age = now - e.created
				if age >= dur then
					if e.wrap then e.wrap:Destroy() end
					table.remove(rec.entries, i)
					removed = true
				elseif age > dur - 0.5 then
					local k = math.clamp((dur - age) / 0.5, 0, 1)
					e.frame.BackgroundTransparency = 1 - (1 - e.base_bg_t) * k
					e.label.TextTransparency = 1 - k
					if e.caret_tri then e.caret_tri.BackgroundTransparency = e.frame.BackgroundTransparency end
				end
			end
			if removed then refresh_bubble_carets(rec) end

			-- Distance minimize (MinimizeDistance .. MaxDistance -> single "..." bubble)
			local dist = 0
			if cam_pos and adornee:IsA("BasePart") then
				dist = (adornee.Position - cam_pos).Magnitude
			end
			local minimize = (#rec.entries > 0) and (dist >= min_dist) and (dist <= max_dist)
			if minimize then
				for _, e in rec.entries do e.wrap.Visible = false end
				if not rec.minimized then
					local m = Instance.new("Frame")
					m.Name = "Minimized"
					m.LayoutOrder = 99999998
					m.Size = UDim2.fromOffset(44, 28)
					m.BackgroundColor3 = bcc and bcc.BackgroundColor3 or Color3.fromRGB(250, 250, 250)
					m.BackgroundTransparency = bcc and bcc.BackgroundTransparency or 0.1
					m.BorderSizePixel = 0
					m.Parent = rec.list
					local c = Instance.new("UICorner")
					c.CornerRadius = UDim.new(0, 12)
					c.Parent = m
					local tl = Instance.new("TextLabel")
					tl.Size = UDim2.fromScale(1, 1)
					tl.BackgroundTransparency = 1
					tl.Text = "•••"
					tl.TextSize = 15
					tl.TextColor3 = bcc and bcc.TextColor3 or Color3.fromRGB(57, 59, 61)
					tl.FontFace = bcc and bcc.FontFace or Font.new("rbxasset://fonts/families/BuilderSans.json", Enum.FontWeight.Medium)
					tl.Parent = m
					rec.minimized = m
				end
				rec.minimized.Visible = true
			else
				for _, e in rec.entries do e.wrap.Visible = true end
				if rec.minimized then rec.minimized.Visible = false end
			end

			-- Clean up empty board
			if #rec.entries == 0 and (not rec.typing or not rec.typing.Visible) then
				if rec.board then rec.board:Destroy() end
				bubble_boards[adornee] = nil
			end
		end
	end
end

local function show_translation_notice()
	if welcome_shown then return end
	welcome_shown = true
	add_window_line({
		prefix = "",
		text = "Roblox automatically translates supported languages in chat",
		system = true,
		message_id = "0-translation-notice",
	})
end

-- Per-frame UI sync: layout, configuration styling, fade timing, CoreGuiType.Chat.
local function step_ui(dt)
	build_ui()
	if not root_gui then return end
	dt = math.clamp(dt or (1 / 60), 0, 0.25)

	local tcs = is_tcs_mode()
	local chat_on = core_chat_enabled()
	local cwc = get_cwc()
	local cib = get_cib()
	local ctc = get_ctc()

	-- Toggle top-bar chat slot visibility (TopBarApp logo + hamburger stay visible as in Studio)
	if topbar_gui then
		topbar_gui.Enabled = topbar_enabled
		local show_chat_btn = chat_on and (tcs or svc.Chat.LoadDefaultChat)
		if topbar_chat_slot then topbar_chat_slot.Visible = show_chat_btn end
		if topbar_unibar then
			topbar_unibar.Size = UDim2.fromOffset(show_chat_btn and 96 or 52, 44)
		end
	end

	-- Legacy vs TextChatService visibility
	if legacy_gui then
		legacy_gui.Enabled = (not tcs) and chat_on and chat_visible and svc.Chat.LoadDefaultChat
		if legacy_bar then legacy_bar.Visible = not chat_bar_disabled end
	end
	if app_layout then
		app_layout.Visible = tcs and chat_on and chat_visible
	end

	step_bubbles(dt)
	if not tcs then return end

	sync_custom_textbox()

	-- Show initial translation notice once in TCS mode (matching Studio's first system line)
	if not welcome_shown and get_tcs().ChatTranslationEnabled then
		show_translation_notice()
	end

	-- Compute responsive appLayout dimensions & alignment matching Studio's ExperienceChat
	local vp_w, vp_h = R.viewport_size()
	if vp_w <= 0 then vp_w = 1280 end
	if vp_h <= 0 then vp_h = 720 end
	local usable_h = math.max(120, vp_h - 58)
	local w_scale = (cwc and cwc.WidthScale) or 1
	local h_scale = (cwc and cwc.HeightScale) or 1
	local base_w = math.clamp(vp_w * 0.4, 280, 475) * w_scale
	local base_h = math.clamp(usable_h * 0.25, 140, 210) * h_scale
	local tabs_on = ctc and ctc.Enabled or false
	local win_on = cwc and cwc.Enabled or false
	local bar_on = (cib and cib.Enabled and not chat_bar_disabled) or false
	local total_h = (tabs_on and 48 or 0) + (win_on and base_h or 0) + (bar_on and 55 or 0)

	local h_align = cwc and cwc.HorizontalAlignment or Enum.HorizontalAlignment.Left
	local v_align = cwc and cwc.VerticalAlignment or Enum.VerticalAlignment.Top
	local x = 8
	if h_align == Enum.HorizontalAlignment.Center then x = math.floor((vp_w - base_w) * 0.5)
	elseif h_align == Enum.HorizontalAlignment.Right then x = math.floor(vp_w - base_w - 8) end
	local y = 4
	if v_align == Enum.VerticalAlignment.Center then y = math.floor((usable_h - total_h) * 0.5)
	elseif v_align == Enum.VerticalAlignment.Bottom then y = math.floor(usable_h - total_h - 8) end

	app_layout.Position = UDim2.fromOffset(x, y)
	app_layout.Size = UDim2.fromOffset(math.floor(base_w), math.floor(total_h))
	channel_bar.Visible = tabs_on
	if top_padding then top_padding.Visible = tabs_on end
	chat_window.Visible = win_on
	chat_window.Size = UDim2.new(1, 0, 0, math.floor(base_h))
	input_row.Visible = bar_on

	if tabs_on then rebuild_channel_tabs() end
	update_target_chip()

	-- Sync TextBox & styling with ChatInputBarConfiguration / ChatWindowConfiguration
	if cib and input_box then
		local hk = key_label(cib.KeyboardKeyCode)
		input_box.PlaceholderText = string.format("To chat click here or press %s key", hk)
		input_box.PlaceholderColor3 = cib.PlaceholderColor3
		input_box.TextColor3 = cib.TextColor3
		input_box.TextSize = cib.TextSize
		input_box.FontFace = cib.FontFace
		input_group.BackgroundColor3 = cib.BackgroundColor3
	end
	if cwc and chat_window then
		chat_window.BackgroundColor3 = cwc.BackgroundColor3
	end
	if ctc and channel_bar then
		channel_bar.BackgroundColor3 = ctc.BackgroundColor3
	end

	-- Fade timing (Studio ExperienceChat: bg fades out after 3.5s idle; text fades out after 30s)
	local now = os.clock()
	local active = box_focused or hover_window or hover_bar or hover_tabs
	if active then last_activity_t = now end
	local want_bg = (active or (now - last_activity_t) < 3.5) and 1 or 0
	local want_text = (active or (now - last_message_t) < 30.0) and 1 or 0
	local fade_speed = dt * 5.0
	if want_bg > bg_alpha then bg_alpha = math.min(1, bg_alpha + fade_speed * 2)
	else bg_alpha = math.max(0, bg_alpha - fade_speed) end
	if want_text > text_alpha then text_alpha = math.min(1, text_alpha + fade_speed * 2)
	else text_alpha = math.max(0, text_alpha - fade_speed * 0.5) end

	local cwc_bg_t = (cwc and cwc.BackgroundTransparency) or 0.3
	local cib_bg_t = (cib and cib.BackgroundTransparency) or 0.2
	local ctc_bg_t = (ctc and ctc.BackgroundTransparency) or 0.0

	app_layout.BackgroundColor3 = (cwc and cwc.BackgroundColor3) or Color3.fromRGB(25, 27, 29)
	app_layout.BackgroundTransparency = 1 - (1 - cwc_bg_t) * bg_alpha
	chat_window.BackgroundTransparency = 1
	input_group.BackgroundTransparency = 1 - (1 - cib_bg_t) * bg_alpha
	input_stroke.Transparency = 1 - 0.22 * bg_alpha
	if dot_menu then dot_menu.Visible = bg_alpha > 0.15 end
	if channel_bar and tabs_on then
		local ct = 1 - (1 - ctc_bg_t) * bg_alpha
		channel_bar.BackgroundTransparency = ct
		local lc = channel_bar:FindFirstChild("LowerCover")
		if lc then
			lc.BackgroundColor3 = channel_bar.BackgroundColor3
			lc.BackgroundTransparency = ct
		end
	end
	if input_box then
		local bar_text_t = 1 - bg_alpha
		input_box.TextTransparency = box_focused and 0 or bar_text_t
		send_btn.Visible = bg_alpha > 0.1 or box_focused
	end

	-- Fade chat window message text smoothly
	for _, entry in line_instances do
		if entry.frame and entry.frame.Parent then
			entry.frame.TextTransparency = 1 - text_alpha
			entry.frame.TextStrokeTransparency = 1 - (1 - entry.base_stroke) * text_alpha
		end
	end
end

-- Layout / focus queries for ChatWindowConfiguration / ChatInputBarConfiguration / ChannelTabsConfiguration
local function layout_query(class, key)
	if not ui_ready or not is_tcs_mode() or not core_chat_enabled() or not chat_visible then
		if key == "IsFocused" then return false end
		return 0, 0
	end
	if key == "IsFocused" then
		local cib = get_cib()
		local tb = (cib and cib.TextBox) or input_box
		return tb ~= nil and R.is_focused(tb)
	end
	local target = nil
	if class == "ChatWindowConfiguration" then
		local cwc = get_cwc()
		if cwc and cwc.Enabled then target = chat_window end
	elseif class == "ChatInputBarConfiguration" then
		local cib = get_cib()
		if cib and cib.Enabled then target = input_row end
	elseif class == "ChannelTabsConfiguration" then
		local ctc = get_ctc()
		if ctc and ctc.Enabled then target = channel_bar end
	end
	if not target then return 0, 0 end
	local px, py, sx, sy = R.gui_box(target)
	if sx == 0 and sy == 0 then
		-- Fallback before the first GUI walk lays out CoreGui.
		local vp_w, vp_h = R.viewport_size()
		if vp_w <= 0 then vp_w = 1280 end
		if vp_h <= 0 then vp_h = 720 end
		local usable_h = math.max(120, vp_h - 58)
		local cwc = get_cwc()
		local ctc = get_ctc()
		local tabs_h = (ctc and ctc.Enabled) and 40 or 0
		local w = math.floor(math.clamp(vp_w * 0.4, 280, 475) * ((cwc and cwc.WidthScale) or 1))
		local h = math.floor(math.clamp(usable_h * 0.25, 140, 210) * ((cwc and cwc.HeightScale) or 1))
		if class == "ChannelTabsConfiguration" then
			px, py, sx, sy = 8, 4, w, 40
		elseif class == "ChatWindowConfiguration" then
			px, py, sx, sy = 8, 4 + tabs_h, w, h
		else
			px, py, sx, sy = 16, 4 + tabs_h + h, w - 16, 47
		end
	end
	if key == "AbsolutePosition" then return px, py end
	return sx, sy
end

local function set_core(key, val)
	if key == "TopbarEnabled" then
		topbar_enabled = not not val
		if topbar_gui then topbar_gui.Enabled = topbar_enabled end
	elseif key == "ChatActive" then
		chat_visible = not not val
		if chat_visible then unread_count = 0; last_activity_t = os.clock() end
		update_topbar_badge()
	elseif key == "ChatBarDisabled" then
		chat_bar_disabled = not not val
	elseif key == "ChatMakeSystemMessage" and type(val) == "table" then
		local txt = tostring(val.Text or "")
		local col = typeof(val.Color) == "Color3" and val.Color or Color3.fromRGB(255, 255, 255)
		local sz = tonumber(val.TextSize) or 16
		if is_tcs_mode() then
			local tc = get_tcs():FindFirstChild("TextChannels")
			local sys = tc and tc:FindFirstChild("RBXSystem")
			if sys then
				local rich = string.format('<font color="%s">%s</font>', color_hex(col), txt)
				sys:DisplaySystemMessage(rich)
			else
				add_window_line({ text = txt, system = true, prefix = "", message_id = "" })
			end
		else
			add_legacy_line(txt, col, sz)
		end
	elseif key == "ChatWindowPosition" and typeof(val) == "UDim2" and legacy_window then
		legacy_window.Position = val
	elseif key == "ChatWindowSize" and typeof(val) == "UDim2" and legacy_window then
		legacy_window.Size = val
	end
end

local function get_core(key)
	if key == "ChatActive" then return chat_visible end
	if key == "ChatBarDisabled" then return chat_bar_disabled end
	if key == "ChatWindowPosition" then
		return legacy_window and legacy_window.Position or UDim2.fromOffset(8, 8)
	end
	if key == "ChatWindowSize" then
		return legacy_window and legacy_window.Size or UDim2.fromOffset(360, 175)
	end
	return nil
end

return {
	build = build_ui,
	step = step_ui,
	add_line = add_window_line,
	clear_lines = clear_window_lines,
	add_legacy_line = add_legacy_line,
	add_bubble = add_bubble_ui,
	show_translation_notice = show_translation_notice,
	layout_query = layout_query,
	set_core = set_core,
	get_core = get_core,
	refresh_tabs = function() last_channels_sig = ""; rebuild_channel_tabs(); update_target_chip() end,
	replace_emojis = replace_emojis,
}
