#!/usr/bin/env python3
"""Writes corpus/muzzle-soft.rbxlx: a game's muzzle flash (WeaponFx.luau)
in a dark room, LightingStyle Soft (Technology ShadowMap).

A camera-parented, invisible 0.05-stud "MuzzleFx" part (anchored, CFrame
set every frame) carries LockedToPart, Emit()ed flash sprites (Lifetime
0.03-0.05 s), a FaceCamera side-flame Beam enabled for 35 ms and a
PointLight (Range 12) at Brightness 3.5 for 35 ms after each shot, as the game
does. Surfaces stand 3, 6 and 20 studs from the muzzle.

Timeline (seconds after the LocalScript starts):
  0-5 off; 5-10 lamp (the PointLight alone at 3.5, no sprites or flame);
  10-15 hold (a shot every frame: the flash held on);
  15-25 semi (a shot each second); 25-35 auto (10 shots/s, still camera);
  35-47 automove (10 shots/s, camera swaying); then off.
A HUD label shows the phase, the shot count, frames and ms since the last
shot, so each screenshot says where in a shot it is."""
import os
from xml.sax.saxutils import escape

HERE = os.path.dirname(os.path.abspath(__file__))
ref = [100]

SCRIPT = r'''local RunService = game:GetService("RunService")
local Players = game:GetService("Players")
local cam = workspace.CurrentCamera
local function tex(id) return "rbxassetid://" .. id end
local T = {
	flashFront = tex(11724484287), flashFront2 = tex(11037537064), flashSide = tex(11037504615),
	core = tex(11037564249),
}
local function seq(...)
	local v = { ... }
	local k = {}
	for i = 1, #v, 2 do table.insert(k, NumberSequenceKeypoint.new(v[i], v[i + 1])) end
	return NumberSequence.new(k)
end
local function emitter(parent, props)
	local e = Instance.new("ParticleEmitter")
	e.Enabled = false
	e.Rate = 0
	e.LightInfluence = 0
	for k, v in props do e[k] = v end
	e.Parent = parent
	return e
end
local muzzlePart = Instance.new("Part")
muzzlePart.Name = "MuzzleFx"
muzzlePart.Anchored, muzzlePart.CanCollide, muzzlePart.CanQuery, muzzlePart.CanTouch = true, false, false, false
muzzlePart.Transparency = 1
muzzlePart.Size = Vector3.new(0.05, 0.05, 0.05)
muzzlePart.Parent = cam
local muzzleAtt = Instance.new("Attachment")
muzzleAtt.Parent = muzzlePart
local flameEnd = Instance.new("Attachment")
flameEnd.Parent = muzzlePart
local flashFront = emitter(muzzleAtt, {
	Texture = T.flashFront, LightEmission = 1, Brightness = 2.2, LockedToPart = true,
	Lifetime = NumberRange.new(0.035, 0.05), Speed = NumberRange.new(0),
	Rotation = NumberRange.new(0, 360), Size = seq(0, 0.2, 1, 0.34),
	Transparency = seq(0, 0.05, 0.6, 0.35, 1, 1), ZOffset = 0.5,
	Color = ColorSequence.new(Color3.fromRGB(255, 214, 150)),
})
local flashFront2 = emitter(muzzleAtt, {
	Texture = T.flashFront2, LightEmission = 1, Brightness = 1.6, LockedToPart = true,
	Lifetime = NumberRange.new(0.03, 0.045), Speed = NumberRange.new(0),
	Rotation = NumberRange.new(0, 360), Size = seq(0, 0.16, 1, 0.26),
	Transparency = seq(0, 0.2, 1, 1), ZOffset = 0.4,
	Color = ColorSequence.new(Color3.fromRGB(255, 190, 120)),
})
local flashCore = emitter(muzzleAtt, {
	Texture = T.core, LightEmission = 1, Brightness = 3, LockedToPart = true,
	Lifetime = NumberRange.new(0.05), Speed = NumberRange.new(0),
	Size = seq(0, 0.32, 1, 0.12), Transparency = seq(0, 0, 1, 1), ZOffset = 0.6,
	Color = ColorSequence.new(Color3.fromRGB(255, 236, 200)),
})
local flame = Instance.new("Beam")
flame.Attachment0 = flameEnd
flame.Attachment1 = muzzleAtt
flame.Texture = T.flashSide
flame.LightEmission = 1
flame.LightInfluence = 0
flame.Brightness = 2
flame.FaceCamera = true
flame.Segments = 1
flame.Enabled = false
flame.Color = ColorSequence.new(Color3.fromRGB(255, 200, 130))
flame.Parent = muzzlePart
local light = Instance.new("PointLight")
light.Color = Color3.fromRGB(255, 190, 120)
light.Range = 12
light.Brightness = 0
light.Shadows = false
light.Parent = muzzlePart
-- a stand-in gun barrel under the camera, the muzzle at its front
local gun = Instance.new("Part")
gun.Name = "Gun"
gun.Anchored, gun.CanCollide, gun.CanQuery = true, false, false
gun.Size = Vector3.new(0.18, 0.22, 0.9)
gun.Color = Color3.fromRGB(30, 30, 32)
gun.Parent = cam

local flameUntil = 0
local function muzzle(s)
	if math.random() < 0.6 then flashFront:Emit(1) else flashFront2:Emit(1) end
	flashCore:Emit(1)
	local len = (0.32 + math.random() * 0.2) * s
	flameEnd.Position = Vector3.new(0, 0, -len)
	flame.Width0 = 0.3 * s
	flame.Width1 = 0.1 * s
	flame.Transparency = seq(0, 0.1 + math.random() * 0.3, 1, 0.2)
	flameUntil = os.clock() + 0.035
end

local gui = Instance.new("ScreenGui")
gui.Name = "MuzzleHud"
gui.ResetOnSpawn = false
local label = Instance.new("TextLabel")
label.Size = UDim2.fromOffset(420, 26)
label.Position = UDim2.new(0, 6, 1, -34)
label.BackgroundColor3 = Color3.new(0, 0, 0)
label.TextColor3 = Color3.new(1, 1, 1)
label.Font = Enum.Font.Code
label.TextSize = 20
label.TextXAlignment = Enum.TextXAlignment.Left
label.Parent = gui
gui.Parent = Players.LocalPlayer:WaitForChild("PlayerGui")

local EYE = Vector3.new(0, 5, -4)
local t0 = os.clock()
local frame, shots, lastShotFrame, lastShotAt, nextShot = 0, 0, -1, -1, 0
RunService:BindToRenderStep("MuzzleRepro", Enum.RenderPriority.Camera.Value + 1, function()
	frame += 1
	local t = os.clock() - t0
	local phase = if t < 5 then "off" elseif t < 10 then "lamp" elseif t < 15 then "hold" elseif t < 25 then "semi"
		elseif t < 35 then "auto" elseif t < 47 then "automove" else "off"
	local yaw = if phase == "automove" then math.sin(t * 2.2) * 0.12 else 0
	cam.CameraType = Enum.CameraType.Scriptable
	cam.FieldOfView = 70
	cam.CFrame = CFrame.new(EYE) * CFrame.Angles(0, yaw, 0)
	gun.CFrame = cam.CFrame * CFrame.new(0.55, -0.5, -1.45)
	local muzzleCF = cam.CFrame * CFrame.new(0.55, -0.45, -1.95)
	local fire = false
	if phase == "hold" then
		fire = true
	elseif phase == "semi" or phase == "auto" or phase == "automove" then
		local period = if phase == "semi" then 1 else 0.1
		if t >= nextShot then
			fire = true
			nextShot = math.max(nextShot + period, t)
		end
	else
		nextShot = t
	end
	if fire then
		muzzle(1)
		shots += 1
		lastShotFrame, lastShotAt = frame, t
	end
	-- WeaponFx.follow
	muzzlePart.CFrame = muzzleCF
	local on = os.clock() < flameUntil
	flame.Enabled = on
	if phase == "lamp" then on = true end -- the light alone, no sprites or flame
	light.Brightness = if on then 3.5 else 0
	label.Text = string.format("%s shot %d f+%d %dms L%s", phase, shots,
		if lastShotFrame < 0 then -1 else frame - lastShotFrame,
		if lastShotAt < 0 then -1 else math.floor((t - lastShotAt) * 1000 + 0.5), if on then "1" else "0")
end)
'''


def nref():
    ref[0] += 1
    return ref[0]


def cf(x, y, z):
    return (f'<CoordinateFrame name="CFrame"><X>{x}</X><Y>{y}</Y><Z>{z}</Z><R00>1</R00><R01>0</R01><R02>0</R02>'
            f'<R10>0</R10><R11>1</R11><R12>0</R12><R20>0</R20><R21>0</R21><R22>1</R22></CoordinateFrame>')


def rgb(r, g, b):
    return (255 << 24) | (r << 16) | (g << 8) | b


def part(name, pos, size, color=(150, 140, 125), material=256, children=""):
    return (f'<Item class="Part" referent="{nref()}"><Properties><string name="Name">{name}</string><bool name="Anchored">true</bool>'
            f'{cf(*pos)}<Vector3 name="size"><X>{size[0]}</X><Y>{size[1]}</Y><Z>{size[2]}</Z></Vector3>'
            f'<Color3uint8 name="Color3uint8">{rgb(*color)}</Color3uint8><token name="Material">{material}</token>'
            f'</Properties>{children}</Item>')


def make():
    items = [
        # a closed room, x -12..12, y 0..12, z +2..-26 (camera at (0, 5, -4) looking -Z)
        part("Floor", (0, -0.5, -12), (26, 1, 30)),
        part("Roof", (0, 12.5, -12), (26, 1, 30)),
        part("Back", (0, 6, -26.5), (26, 12, 1)),     # ~20 studs ahead of the muzzle
        part("Front", (0, 6, 2.5), (26, 12, 1)),
        part("Left", (-12.5, 6, -12), (1, 12, 30)),
        part("Right", (4.5, 6, -12), (1, 12, 30)),    # ~3 studs right of the muzzle
        part("Pillar", (0.5, 4, -12), (2, 8, 2)),     # ~5 studs ahead (its face at z -11)
        part("Post", (-5, 3, -14), (2, 6, 2)),         # ~10 studs (its face at z -13, x -5)
    ]
    items.append('<Item class="SpawnLocation" referent="99"><Properties><string name="Name">SpawnLocation</string>'
                 '<bool name="Anchored">true</bool>' + cf(0, 0.5, 300) +
                 '<Vector3 name="size"><X>6</X><Y>1</Y><Z>6</Z></Vector3></Properties></Item>')
    xml = f'''<roblox version="4">
<Item class="Workspace" referent="1"><Properties><string name="Name">Workspace</string></Properties>
{"".join(items)}
</Item>
<Item class="Lighting" referent="2"><Properties><string name="Name">Lighting</string>
<Color3 name="Ambient"><R>0.12</R><G>0.12</G><B>0.12</B></Color3><float name="Brightness">0</float><float name="ClockTime">0</float>
<float name="EnvironmentDiffuseScale">0</float><float name="EnvironmentSpecularScale">0</float><bool name="GlobalShadows">true</bool>
<Color3 name="OutdoorAmbient"><R>0</R><G>0</G><B>0</B></Color3><token name="Technology">3</token><token name="LightingStyle">1</token>
</Properties>
<Item class="Sky" referent="3"><Properties><string name="Name">Sky</string><int name="StarCount">0</int><bool name="CelestialBodiesShown">false</bool></Properties></Item>
</Item>
<Item class="StarterPlayer" referent="5"><Properties><string name="Name">StarterPlayer</string></Properties>
<Item class="StarterPlayerScripts" referent="6"><Properties><string name="Name">StarterPlayerScripts</string></Properties>
<Item class="LocalScript" referent="7"><Properties><string name="Name">MuzzleRepro</string><ProtectedString name="Source">{escape(SCRIPT)}</ProtectedString></Properties></Item>
</Item></Item>
<Item class="Players" referent="8"><Properties><string name="Name">Players</string></Properties></Item>
</roblox>
'''
    open(os.path.join(HERE, "corpus", "muzzle-soft.rbxlx"), "w").write(xml)


make()
print("ok")
