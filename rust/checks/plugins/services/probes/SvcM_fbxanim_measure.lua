-- Plugin:ImportFbxAnimation probe, the same file in Studio and rustudio.
-- Studio (studio_fbxanim.py): installed as a local plugin in a hidden Studio, idle until
-- the driver sets workspace attributes FBXM_CASES (JSON list of {label, rig, r15, async, undo, ser})
-- and bumps FBXM_GO, then runs each case while the driver answers each "Open 3D File" dialog.
-- rustudio (check.py fbxanim, tests_services.rs): the case list is written into AUTO below and the
-- dialogs are answered with --action plugin-prompt:<file>. Output lines start with
-- "[FBXM] <label>"; captures/fbxanim-studio.txt is Studio's. Rigs (built first, in Workspace):
--   Rig    HumanoidRootPart -Root-> LowerTorso -Waist-> UpperTorso -> RightUpperArm / LeftUpperArm / Head,
--          Bone Antenna under UpperTorso; identity C0/C1; Humanoid; PrimaryPart HumanoidRootPart
--   RigC   the same with R15-like C0/C1 offsets and a rotated RightShoulder C0
--   Crate  Base -Hinge-> Lid
--   R6     HumanoidRootPart -RootJoint-> Torso -Neck-> Head, -Right Shoulder-> Right Arm (R6 C0/C1)
--   Part   a lone Part
local AUTO = nil
local CHS = game:GetService("ChangeHistoryService")
local Http = game:GetService("HttpService")
local Sel = game:GetService("Selection")

local function out(label, ...)
	local t = table.pack(...)
	for i = 1, t.n do t[i] = tostring(t[i]) end
	print("[FBXM] " .. label .. " " .. table.concat(t, " ", 1, t.n))
end

local function part(name, parent, size)
	local p = Instance.new("Part")
	p.Name = name
	p.Anchored = name == "HumanoidRootPart"
	p.Size = size or Vector3.new(1, 1, 1)
	p.Parent = parent
	return p
end
local function motor(name, p0, p1, c0, c1)
	local m = Instance.new("Motor6D")
	m.Name = name
	m.Part0, m.Part1 = p0, p1
	if c0 then m.C0 = c0 end
	if c1 then m.C1 = c1 end
	m.Parent = p1
	return m
end

local function r15(name, offsets)
	local rig = Instance.new("Model")
	rig.Name = name
	local hrp = part("HumanoidRootPart", rig)
	local lt = part("LowerTorso", rig)
	local ut = part("UpperTorso", rig)
	local rua = part("RightUpperArm", rig)
	local lua_ = part("LeftUpperArm", rig)
	local head = part("Head", rig)
	local C = CFrame.new
	if offsets then
		motor("Root", hrp, lt, C(0, -1, 0), C(0, -0.2, 0))
		motor("Waist", lt, ut, C(0, 0.2, 0), C(0, -0.8, 0))
		motor("RightShoulder", ut, rua, C(1, 0.5, 0) * CFrame.Angles(0, 0, math.rad(30)), C(-0.5, 0.4, 0))
		motor("LeftShoulder", ut, lua_, C(-1, 0.5, 0), C(0.5, 0.4, 0))
		motor("Neck", ut, head, C(0, 0.8, 0), C(0, -0.5, 0))
	else
		motor("Root", hrp, lt)
		motor("Waist", lt, ut)
		motor("RightShoulder", ut, rua)
		motor("LeftShoulder", ut, lua_)
		motor("Neck", ut, head)
	end
	local bone = Instance.new("Bone")
	bone.Name = "Antenna"
	if offsets then bone.CFrame = C(0, 0.3, 0) end
	bone.Parent = ut
	rig.PrimaryPart = hrp
	Instance.new("Humanoid").Parent = rig
	rig.Parent = workspace
	return rig
end

local function build()
	if workspace:FindFirstChild("Rig") then return end
	r15("Rig", false)
	r15("RigC", true)
	local crate = Instance.new("Model")
	crate.Name = "Crate"
	local a, b = part("Base", crate), part("Lid", crate)
	motor("Hinge", a, b)
	crate.Parent = workspace
	local r6 = Instance.new("Model")
	r6.Name = "R6"
	local hrp = part("HumanoidRootPart", r6, Vector3.new(2, 2, 1))
	local torso = part("Torso", r6, Vector3.new(2, 2, 1))
	local head = part("Head", r6, Vector3.new(2, 1, 1))
	local ra = part("Right Arm", r6, Vector3.new(1, 2, 1))
	local A = CFrame.fromMatrix
	motor("RootJoint", hrp, torso, CFrame.new(0, 0, 0, -1, 0, 0, 0, 0, 1, 0, 1, 0),
		CFrame.new(0, 0, 0, -1, 0, 0, 0, 0, 1, 0, 1, 0))
	motor("Neck", torso, head, CFrame.new(0, 1, 0, -1, 0, 0, 0, 0, 1, 0, 1, 0),
		CFrame.new(0, -0.5, 0, -1, 0, 0, 0, 0, 1, 0, 1, 0))
	motor("Right Shoulder", torso, ra, CFrame.new(1, 0.5, 0, 0, 0, 1, 0, 1, 0, -1, 0, 0),
		CFrame.new(-0.5, 0.5, 0, 0, 0, 1, 0, 1, 0, -1, 0, 0))
	r6.PrimaryPart = hrp
	Instance.new("Humanoid").Parent = r6
	r6.Parent = workspace
	part("Part", workspace)
	CHS:SetWaypoint("FBXM rigs")
end

local function f6(v)
	local s = string.format("%.6f", v)
	return s == "-0.000000" and "0.000000" or s
end
local function cfs(cf)
	local c = { cf:GetComponents() }
	for i, v in c do c[i] = f6(v) end
	return table.concat(c, " ")
end

local function dumpPose(label, p, path, t)
	local here = path == "" and p.Name or (path .. "/" .. p.Name)
	local extra = {}
	for _, k in p:GetChildren() do
		if not k:IsA("Pose") then table.insert(extra, k.ClassName .. ":" .. k.Name) end
	end
	out(label, "p", f6(t), here, p.ClassName, "w=" .. f6(p.Weight), "mw=" .. f6(p.MaskWeight),
		p.EasingStyle.Name, p.EasingDirection.Name, cfs(p.CFrame), #extra > 0 and table.concat(extra, ",") or "")
	local kids = p:GetSubPoses()
	table.sort(kids, function(x, y) return x.Name < y.Name end)
	for _, k in kids do dumpPose(label, k, here, t) end
end

local function count(root)
	return #root:GetDescendants()
end

local function run(c)
	local label = c.label
	if c.chain and not workspace:FindFirstChild(c.rig) then
		-- {{name, parent}, ...}: parts joined by Motor6Ds (Part0 = parent), the first is the root.
		local m = Instance.new("Model")
		m.Name = c.rig
		local parts = {}
		for i, e in c.chain do
			parts[e[1]] = part(e[1], m)
			if i == 1 then
				parts[e[1]].Anchored = true
				m.PrimaryPart = parts[e[1]]
			else
				motor(e[1], parts[e[2]], parts[e[1]])
			end
		end
		if c.humanoid ~= false then Instance.new("Humanoid").Parent = m end
		m.Parent = workspace
	end
	local rig = c.rig ~= "nil" and workspace:FindFirstChild(c.rig) or nil
	local wsBefore = count(workspace)
	local canUndo = CHS:GetCanUndo()
	local undoName
	local conn = CHS.OnUndo:Connect(function(n) undoName = n end)
	if c.undo then
		CHS:SetWaypoint("FBXM before " .. label)
		canUndo = CHS:GetCanUndo()
	end
	local selBefore = #Sel:Get()
	out(label, "start", c.rig, c.r15)
	local ok, res
	if c.async then
		ok, res = pcall(function() return plugin:ImportFbxAnimationAsync(rig, c.r15) end)
	else
		ok, res = pcall(function() return plugin:ImportFbxAnimation(rig, c.r15) end)
	end
	out(label, "call", ok, typeof(res), ok and "" or res)
	task.wait(AUTO and 0.05 or 0.5)
	out(label, "after", "canUndo", canUndo, CHS:GetCanUndo(), "ws", string.format("%+d", count(workspace) - wsBefore),
		"sel", #Sel:Get() - selBefore)
	if ok and typeof(res) == "Instance" then
		local s = res
		out(label, "res", s.ClassName, s.Name, s.Parent and s.Parent:GetFullName() or "nil", "children", #s:GetChildren())
		if s:IsA("KeyframeSequence") then
			out(label, "props", "Loop", s.Loop, "Priority", s.Priority.Name, "AHH", f6(s.AuthoredHipHeight),
				"Archivable", s.Archivable)
		end
		local attrs = Http:JSONEncode(s:GetAttributes())
		out(label, "attrs", attrs, "tags", table.concat(s:GetTags(), ","))
		local kfs = {}
		for _, k in s:GetChildren() do table.insert(kfs, k) end
		table.sort(kfs, function(x, y)
			local tx = x:IsA("Keyframe") and x.Time or -1
			local ty = y:IsA("Keyframe") and y.Time or -1
			return tx < ty
		end)
		for _, kf in kfs do
			if kf:IsA("Keyframe") then
				local other = {}
				for _, k in kf:GetChildren() do
					if not k:IsA("Pose") then table.insert(other, k.ClassName .. ":" .. k.Name) end
				end
				out(label, "kf", f6(kf.Time), kf.Name, #kf:GetPoses(), table.concat(other, ","))
				local roots = kf:GetPoses()
				table.sort(roots, function(x, y) return x.Name < y.Name end)
				for _, p in roots do dumpPose(label, p, "", kf.Time) end
			else
				out(label, "child", kf.ClassName, kf.Name)
			end
		end
		if c.ser then
			-- The AnimationRigData as an rbxm (hex, in chunks: its properties are binary).
			local rd = s:FindFirstChildOfClass("AnimationRigData")
			local ok2, buf = pcall(function()
				return game:GetService("SerializationService"):SerializeInstancesAsync({ rd })
			end)
			if rd and ok2 then
				local str = buffer.tostring(buf)
				local hex = str:gsub(".", function(ch) return string.format("%02x", string.byte(ch)) end)
				local n = 0
				for i = 1, #hex, 800 do
					n += 1
					out(label, "ser", n, hex:sub(i, i + 799))
				end
				out(label, "serdone", n)
			else
				out(label, "ser none", tostring(rd), ok2 and "" or buf)
			end
		end
		if c.keep then
			s.Parent = workspace
		end
	end
	if CHS:GetCanUndo() ~= canUndo or c.undo then
		local seqParent = ok and typeof(res) == "Instance" and res.Parent
		local ws = count(workspace)
		CHS:Undo()
		task.wait(0.3)
		out(label, "undo", tostring(undoName), "ws", string.format("%+d", count(workspace) - ws), "seqParent",
			ok and typeof(res) == "Instance" and tostring(res.Parent) or "-", "was", tostring(seqParent))
		CHS:Redo()
		task.wait(0.3)
		out(label, "redo", "ws", string.format("%+d", count(workspace) - ws))
	end
	conn:Disconnect()
	out(label, "end")
end

local function batch(cases, go)
	build()
	for _, c in cases do
		local ok, e = pcall(run, c)
		if not ok then out(c.label, "probe error", e) end
		task.wait(AUTO and 0.05 or 1)
	end
	print("[FBXM] batch done " .. tostring(go))
end

if AUTO then
	task.defer(batch, Http:JSONDecode(AUTO), "auto")
	return
end
local busy = false
local lastGo = workspace:GetAttribute("FBXM_GO")
workspace:GetAttributeChangedSignal("FBXM_GO"):Connect(function()
	local go = workspace:GetAttribute("FBXM_GO")
	if busy or go == nil or go == lastGo then return end
	lastGo = go
	busy = true
	batch(Http:JSONDecode(workspace:GetAttribute("FBXM_CASES")), go)
	busy = false
end)
print("[FBXM] loaded")
