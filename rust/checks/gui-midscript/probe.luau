-- Read geometry before and after querying the layout's content size.
local function log(label, ...)
	local fields = {label}
	for _, value in {...} do
		table.insert(fields, `{value.X},{value.Y}`)
	end
	print("MID|" .. table.concat(fields, "|"))
end

local player = game:GetService("Players").LocalPlayer
local root = Instance.new("ScreenGui")
root.Parent = if player then player:WaitForChild("PlayerGui") else game:GetService("StarterGui")
local function frame(parent, x, y, w, h)
	local f = Instance.new("Frame")
	f.Position = UDim2.fromOffset(x, y)
	f.Size = UDim2.fromOffset(w, h)
	f.Parent = parent
	return f
end

for _, class in {"UIListLayout", "UIGridLayout"} do
	local container = frame(root, 12, 18, 170, 160)
	local function measure(label, ...)
		log(class .. "/" .. label, ...)
	end
	local layout = Instance.new(class)
	layout.SortOrder = Enum.SortOrder.LayoutOrder
	if class == "UIListLayout" then
		layout.Padding = UDim.new(0, 5)
	else
		layout.CellSize = UDim2.fromOffset(55, 35)
		layout.CellPadding = UDim2.fromOffset(5, 6)
	end
	layout.Parent = container
	local a = frame(nil, 7, 9, 23, 21)
	a.LayoutOrder = 1
	local b = frame(nil, 11, 13, 32, 27)
	b.LayoutOrder = 2
	a.Parent = container
	measure("first parent", a.AbsolutePosition, a.AbsoluteSize)
	b.Parent = container
	measure("second parent", a.AbsolutePosition, a.AbsoluteSize, b.AbsolutePosition, b.AbsoluteSize)
	local function snapshot(label)
		measure(label, a.AbsolutePosition, a.AbsoluteSize, b.AbsolutePosition, b.AbsoluteSize,
			layout.AbsoluteContentSize, a.AbsolutePosition, a.AbsoluteSize, b.AbsolutePosition, b.AbsoluteSize)
	end
	snapshot("content read")
	b.LayoutOrder = 0
	snapshot("order")
	a.Size = UDim2.fromOffset(42, 33)
	snapshot("size")
	if class == "UIListLayout" then
		layout.Padding = UDim.new(0, 14)
	else
		layout.CellPadding = UDim2.fromOffset(22, 14)
	end
	snapshot("padding")
	if class == "UIGridLayout" then
		layout.CellSize = UDim2.fromOffset(69, 37)
		snapshot("cell size")
	end
	local nested = Instance.new("UIListLayout", b)
	local c = frame(b, 6, 8, 17, 19)
	local d = frame(b, 8, 10, 18, 20)
	measure("nested", b.AbsolutePosition, c.AbsolutePosition, d.AbsolutePosition,
		nested.AbsoluteContentSize, c.AbsolutePosition, d.AbsolutePosition, layout.AbsoluteContentSize)
	b.AutomaticSize = Enum.AutomaticSize.Y
	measure("autosize", b.AbsoluteSize, layout.AbsoluteContentSize, b.AbsoluteSize)
	d.Size = UDim2.fromOffset(18, 28)
	measure("autosize growth", b.AbsoluteSize, nested.AbsoluteContentSize,
		b.AbsoluteSize, layout.AbsoluteContentSize, b.AbsoluteSize)
	task.wait(0.12)
	snapshot("frame")
	b.LayoutOrder = 3
	snapshot("order after frame")
	a.Size = UDim2.fromOffset(46, 38)
	snapshot("size after frame")
end

local scroll = Instance.new("ScrollingFrame")
scroll.Position = UDim2.fromOffset(12, 18)
scroll.Size = UDim2.fromOffset(180, 120)
scroll.CanvasSize = UDim2.fromOffset(200, 300)
scroll.Parent = root
local child = frame(scroll, 7, 9, 23, 21)
child.Size = UDim2.fromScale(0.5, 0.5)
child.Position = UDim2.fromScale(0.5, 0.5)
local function canvas(label)
	log("scroll/" .. label, scroll.AbsoluteCanvasSize, scroll.AbsoluteWindowSize,
		child.AbsolutePosition, child.AbsoluteSize)
end
canvas("parent")
scroll.CanvasSize = UDim2.fromOffset(160, 400)
canvas("change")
task.wait(0.12)
canvas("frame")
scroll.CanvasSize = UDim2.fromOffset(100, 70)
canvas("change after frame")
scroll.AutomaticCanvasSize = Enum.AutomaticSize.Y
child.Position = UDim2.fromOffset(90, 60)
child.Size = UDim2.fromOffset(45, 500)
canvas("auto")
root:Destroy()
