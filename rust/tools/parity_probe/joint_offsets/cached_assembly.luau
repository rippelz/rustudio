local storage = Instance.new("Model", game.ReplicatedStorage)
local parked = CFrame.new(1000000, 10000, 0)
local function part(parent, cf, anchored)
    local p = Instance.new("Part")
    p.CanCollide, p.Anchored, p.CFrame = false, anchored, cf
    p.Parent = parent
    return p
end
local function eq(actual, expected, label)
    assert((actual.Position - expected.Position).Magnitude < 0.001, label .. ": " .. tostring(actual.Position))
    assert((actual.LookVector - expected.LookVector).Magnitude < 0.001, label .. ": " .. tostring(actual.LookVector))
end

local grip = part(storage, parked, true)
grip.Name, grip.RootPriority = "Grip", 10
local sight = part(storage, parked, true)
sight.Name = "Sight"
local weld = Instance.new("Weld")
local c0 = CFrame.new(0.25, 0.5, -2) * CFrame.Angles(0, 0.2, 0)
weld.Part0, weld.Part1, weld.C0, weld.Parent = grip, sight, c0, grip
eq(sight.CFrame, parked, "inactive in storage")

local origin = CFrame.new(15, 1000, 10) * CFrame.Angles(0, 0.6, 0)
local root = part(workspace, origin, true)
for switch = 1, 2 do
    local clone = storage:Clone()
    clone.Grip.Anchored, clone.Sight.Anchored = false, false
    eq(clone.Sight.CFrame, parked, "inactive clone")
    clone.Parent = workspace
    eq(clone.Sight.CFrame, clone.Grip.CFrame * c0, "cached offset restored on parenting")
    local m = Instance.new("Motor6D")
    m.Part0, m.Part1, m.Parent = root, clone.Grip, root
    eq(clone.Grip.CFrame, origin, "equipped grip")
    eq(clone.Sight.CFrame, origin * c0, "sight follows grip")
    m.Transform = CFrame.new(0, 50, 0)
    m.C0 = CFrame.new(0, 0, -1)
    local pose = origin * m.C0
    eq(clone.Sight.CFrame, pose * c0, "C0 applies before pending Transform")
    m.Enabled = false
    m.C1 = CFrame.new(0, 0, -3)
    eq(clone.Sight.CFrame, pose * c0, "disabled C1")
    m.Enabled = true
    pose = origin * m.C0 * m.C1:Inverse()
    eq(clone.Sight.CFrame, pose * c0, "enabled with latest offsets")
    clone:Destroy()
    m:Destroy()
end
root:Destroy()
storage:Destroy()
print("JOINT_CACHE_OK")
