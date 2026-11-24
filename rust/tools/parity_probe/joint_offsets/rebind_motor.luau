local rig = Instance.new("Model")
local root = Instance.new("Part", rig)
root.Name, root.Anchored, root.CFrame = "Root", true, CFrame.new(0, 1000, 0)
local animator = Instance.new("Animator", Instance.new("AnimationController", rig))
local assembly, motor
local function replace()
    if assembly then assembly:Destroy() end
    assembly = Instance.new("Model", rig)
    local limb = Instance.new("Part", assembly)
    limb.Name, limb.CanCollide = "AnimatedPart", false
    if motor then motor:Destroy() end
    motor = Instance.new("Motor6D")
    motor.Part0, motor.Part1, motor.Parent = root, limb, root
end
replace()
rig.Parent = workspace

local sequence = Instance.new("KeyframeSequence")
sequence.Loop = true
for i = 0, 1 do
    local key = Instance.new("Keyframe", sequence)
    key.Time = i
    local base = Instance.new("Pose", key)
    base.Name = "Root"
    local pose = Instance.new("Pose", base)
    pose.Name = "AnimatedPart"
    pose.CFrame = CFrame.new(0, 2, -1)
    local replacement = Instance.new("Pose", base)
    replacement.Name = "ReplacementPart"
    replacement.CFrame = CFrame.new(3, 0, -1)
end
local clip = Instance.new("Animation")
clip.AnimationId = game:GetService("KeyframeSequenceProvider"):RegisterKeyframeSequence(sequence)
local track = animator:LoadAnimation(clip)
track:Play(0)
local function check(label, expected)
    task.wait(0.1)
    assert(track.IsPlaying, label .. " track stopped")
    local position = motor.Transform.Position
    assert((position - (expected or Vector3.new(0, 2, -1))).Magnitude < 0.001, label .. ": " .. tostring(position))
end
check("initial motor")
replace()
check("replacement motor")
local nextPart = Instance.new("Part", assembly)
nextPart.Name, nextPart.CanCollide = "ReplacementPart", false
motor.Part1 = nextPart
check("retargeted motor", Vector3.new(3, 0, -1))
nextPart.Name = "AnimatedPart"
check("name-only edit retains binding", Vector3.new(3, 0, -1))
rig:Destroy()
clip:Destroy()
sequence:Destroy()
print("ANIMATOR_REBIND_OK")
