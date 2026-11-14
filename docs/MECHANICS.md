## Read from Studio's program (Studio)

| Rule | What the code does | Where |
|---|---|---|
| Camera settings |  |; values; getters |
| Fly step per camera tick | step = 0.5 studs; if Shift is not held and the keys have been held for more than 120 ticks, step = min(ticks / 240, 15). The key direction (W/S, A/D, Q/E, each ±1) is normalised and scaled by the step | |
| Fly move | the step is multiplied by Camera Speed (1.5), and by Camera Shift Speed (0.2) while Shift is held, turned into camera space and added to the camera and its focus | |
| Fly hold counter | +1 each camera tick a movement key is down; reset to 0 as soon as no movement key is down, and while Shift is held | , counter at |
| Fly tick rate | Studio ticks the camera once per rendered frame, so its fly speed follows the frame rate. rustudio uses a fixed 142.7 ticks/s, the rate that reproduces the offline Studio's measurement: 107 studs/s at first, then 300 studs after 2 s (measured 300) and 1 065 after 4 s (measured 1 070) | inferred from the rule above and the offline Studio measurement |
| Wheel zoom | each notch moves the camera and its focus together by Camera Mouse Wheel Speed (15 studs) × notches, or by Camera Shift Speed (0.2 studs) with Shift (KeyCode 303/304) held. With Zoom to Mouse Position on (default) the direction is the ray under the mouse, otherwise straight ahead. There is no slow-down or stop near what is under the cursor |– |
| Snap increments | typing a move increment above 0 turns move snapping on; same for the rotate increment and rotate snapping | ("Linear snap increment change -> enable linear snap"), ("Angle snap increment change -> enable angle snap") |
| ScrollingFrame wheel | each wheel notch adds wheel × ScrollWheelDeltaAmount (100) × a per-frame scale to CanvasPosition, then clamps it to the canvas. rustudio scrolls 100 px per notch | flag name set, default 100 (FInt values sit 8 bytes before their name slot);, clamp |
| Top bar inset | the FInt InGameGlobalGuiInset defaults to 36 (the older top bar) and is handed to the inset; Studio's current top bar makes GuiService:GetGuiInset report 58 at run time, so rustudio keeps the measured 58 |, |
| AutoButtonColor | a GuiButton bool, read by getter and written (fires Changed); GuiState is what hover / press change. The code that darkens the colour wasn't found, so rustudio's 30% / 50% stay estimated | (registration) |
| Light Range | the Range setters clamp to [0, 120] before storing; rustudio used 60 before | SpotLight, PointLight/SurfaceLight |
| Light Angle | SpotLight.Angle and SurfaceLight.Angle clamp to [0, 180] | |
| ClockTime | the setter clamps to [0, 24], multiplies by 3600, truncates to whole seconds and stores microseconds (× 1 000 000) as the time of day; so values outside 0–24 clamp rather than wrap, and fractions of a second are dropped |, then |
| EnvironmentDiffuseScale | clamped to [0, 1] | (, category "Appearance") |
| ShadowSoftness | clamped to [0, 1] | |
| Lighting.Brightness, ExposureCompensation | stored as given, no clamp in the setter | (, category "Exposure") |
| Light.Brightness | the Light base setter passes the value through a per-class virtual before storing (limit not visible statically); rustudio stores as given | |
| GeographicLatitude |  |, getter |
| GetSunDirection |  | |
| Humanoid jump speed | GetJumpVelocity: if UseJumpPower return JumpPower; else v = √(2·g·JumpHeight) / 1.06, clamped to [0, 1000], g from Workspace. Its inverse gives the jump height of a speed as (1.06·v)² / (2g), and is √(2·a·b) / 1.06: the Humanoid leaves the ground at 1.06 × the commanded speed. rustudio: take-off = 1.06 × commanded (JumpPower 50 → 7.16 studs; an offline Studio measured 7.27, the rest is the hip spring settling) |, inverse |
| Humanoid jump fields |  | ("Jump Settings"), |
| Turning with the camera | each step the Humanoid sets its "face the camera" flag = AutoRotate && UserGameSettings.RotationType is CameraRelative; the Running state reads it. rustudio faces the camera only then. The turn toward MoveDirection itself sits in split state code the lifter can't follow: rustudio keeps the measured exp(−7 t) |, reader |
| Default character scripts | a new character's Animate: R6 → avatar/scripts/humanoidAnimatePlayEmote.rbxm; R15 → avatar/scripts/humanoidAnimateR15_DEPRECATED_20260813.rbxm unless the flag picks the Lua / server-authority Animate (off in this build: Studio offline inserted the.rbxm); Health from avatar/scripts/humanoidHealthRegenScript.rbxmx through the content-model loader | |
| Default player scripts | StarterPlayerScripts' children are compared by name with "PlayerScriptsLoader", "PlayerModule" and "RbxCharacterSounds"; only the missing ones are inserted (a place's own of the same name wins) | |
| Legacy surfaces | the part shader: VARYING1 = (uv.x · 4, uv.y, surface − 1) from the vertex (surface code in NORMAL.w); texel = StudsMapTexture((u, (fract(v) + band) · 0.25)).r; colour × texel × 2, then squared. With the vertex stud coordinates that is 2 studs across and 8 per band. rustudio: the same, red channel only | Studio's shaders (StudsMapTexture fragment and vertex shaders); texture bound |
| Wheel input | no conversion constant for the game's wheel input (120 / 240 next to an int-to-float) was found: the 0.5 per notch stays measured | searched .text for cvtsi2ss + mulss/divss by 120, 240, 1/120, 1/240 |
| FBX | Studio's only FBX code is the rig / animation importer (ImportFbxRig, ImportFbxAnimation, FbxMemoryAllocator); FBX model uploads are converted by Roblox's servers, not by Studio, so rustudio's import rules come from Place A' records of what its uploads became (FBX section below) | strings… |
| Mesh collision decomposition | Studio decomposes Default / PreciseConvexDecomposition MeshParts with HACD tuned by fast-ints: HACDConcavityMillionth 10 000 000 over HACDNormalizationDistanceMillionth 1 000 000 000 (concavity 1% of the mesh size), HACDQuantizationDistanceMillionth 100 000, HACDSpatialGridSizeTenths 1000, HACDPointSampleDistApartTenths 300, HACDPreprocessNaiveMultiplierTenth 5, HACDNumPreprocessSteps 4, HACDMaximumQuickhullReprocessAttempts 2. rustudio decomposes with V-HACD at concavity 0.01 | registrations.. |
| Part material tables | one function builds three tables of 43 entries (0x58 bytes each): PartMaterialInfosPre2022, then — when the bool RenderEnablePartBaseMaterial4K (default 1) is set — the 4K 2022 table, else the plain 2022 table. Each entry: texture pack id string, detail (normal-only) pack id string, average colour 0xAABBGGRR, one 16-bit word. Slot order is the Enum.Material list (Plastic, SmoothPlastic, Snow, Sandstone, Neon, Wood, …, Rubber). Ids and colours in matpack.rs |..; id strings.. |
| Material colour | the table colour is the material's average: pre-2022 Fabric 0x666869 against its colour map's mean 101, 100, 98; squared, the pre-2022 colours equal the darkening measured on offline Studio's flat materials within 0.013 (CorrodedMetal 0.39 / 0.22 / 0.14 vs 0.38 / 0.22 / 0.14). rustudio draws it when a material's maps aren't loaded | colour map 7547100606 measured; matcal.json |
| Material texture coordinate | every part shader samples the material maps at TEXCOORD0, the same vertex coordinate the studs use at × 4 with 2-stud stud tiles, i.e. studs ÷ 8: a material repeats every 8 studs (× the pack's tiling). The 2022 shader: albedo = (DiffuseMap × colour)², normal from NormalMap .wy (x, y; z rebuilt), SpecularMap .x metalness and .y roughness (0.089 + 0.911 r) | Studio's shaders (part vertex shaders: VARYING0 = TEXCOORD0; PBR fragment shaders) |
| WrapLayer.Puffiness / ShrinkFactor | The default puffiness is FInt WrapPuffinessDefaultPercent = 100 (%). rustudio clamps Puffiness the same way (its fitting multiplies the garment's height above the inner cage by it); ShrinkFactor is read but not used | setters, flag name set, |
| Mesh 7.00 COREMESH v2 | the chunk must hold at least 4 bytes (else "COREMESH chunk too small to hold the compressed mesh data size"); a u32 compressed size follows, which must fit in the rest (else "Compressed mesh data size exceeds COREMESH chunk size"); the Draco stream is decoded into the same 40-byte vertices and 12-byte faces as COREMESH v1 | |
| Draco attributes |  | |
| Play avatar | Studio asks avatar.roblox.com/v2/avatar/avatar-fetch/?placeId={place}&userId={user} and takes the rig from `resolvedAvatarType` ("R6" or "R15"; anything else or missing is an error): Roblox's server resolves the place's avatar setting with the person's avatar. A universe whose `universeAnimationType` is missing or "PlayerChoice" leaves it to the player. rustudio loads the Play avatar from the same request (place 0 for a local file) | request built ("placeId=%s&userId=%s"), parsed by AvatarFetchParser::parseUseR15; PlayerChoice check |
| CSRF | after a reply, a 403 (0x193) to a method other than GET, HEAD, OPTIONS or TRACE is retried when its `X-CSRF-TOKEN` header is non-empty and differs from the token sent, at most 3 times; rustudio does the same | |
| Secret cookies | Studio keeps a set of cookie names it treats as secrets:.ROBLOSECURITY,.RBXIDCHECK,.RBXID. rustudio never shows any of them (a reply naming one is left out of error text) | |

## Play, Run and Stop

| Value | Number | Source |
|---|---|---|
| Play / Run / Stop | F5 / F8 / Shift+F5 (also toolbar "Test" group) | person's request |
| Session | the edit DataModel is deep-copied; Stop discards the copy and restores the edit DataModel unchanged | person's request |
| Play | two DataModels: a server and one local client. The server creates Players.Player (Studio's offline name; Backpack from StarterPack, PlayerGui from StarterGui); the client gets Players.LocalPlayer and PlayerScripts from StarterPlayerScripts; then a character spawns | Roblox docs (Players, Player, StarterPlayer, client-server model) |
| Run | server only: scripts and physics, no client, no character, editor camera stays free | Roblox docs ("Run" mode) |
| Start order | server Scripts start → the player joins (Players.Player made, PlayerAdded) → Backpack scripts → client copy made → client LocalScripts → character + CharacterAdded (server, then client) → character scripts. Server scripts see an empty Players:GetPlayers() at first, so a script handling "players already here" plus PlayerAdded sees the player once | Roblox docs; Place A (its Parties made two parties for one player when the player existed first) |
| Characters | only when Players.CharacterAutoLoads is on (default); otherwise none until a script calls Player:LoadCharacter, and none after a death | Roblox docs (CharacterAutoLoads); Place A' lobby has no character in Studio |
| View while playing | the client's (Studio's Play shows the client); the Explorer's Server / Client toggle switches | Roblox Studio (offline Studio) |
| Scripts started later | a Script / LocalScript parented (or cloned) into a place where it runs starts at the next dispatch, once; e.g. Place A starts each match by cloning a folder of Scripts into an Actor under ServerScriptService | Roblox docs; Place A (Server/Matches) |
| Script teleports | when a script moves the HumanoidRootPart (PivotTo / CFrame) more than 0.5 studs from where the character controller put it, on either side, the character carries on from there | Roblox behaviour; threshold estimated |
| Scripts that run | Script (RunContext Legacy) under Workspace / ServerScriptService, Script with RunContext Server anywhere replicated or server-only, Script with RunContext Client on the client; LocalScript only in StarterPlayerScripts (as PlayerScripts), StarterCharacterScripts (copied into the character), PlayerGui, Backpack, the character, and ReplicatedFirst. Disabled / Enabled=false scripts skip | Roblox docs (Script, LocalScript, RunContext) |

## Client and server (Play)

| Value | Setting | Source |
|---|---|---|
| Never sent to the client | ServerStorage, ServerScriptService (and everything inside) | Roblox docs (ServerStorage, ServerScriptService) |
| Sent to the client | everything else the client sees: Workspace, ReplicatedStorage, ReplicatedFirst, Lighting, Players, StarterGui/StarterPack/StarterPlayer, Teams, SoundService… | Roblox docs |
| Server → client | new instances, destroys / reparents, Name and every property change stream over once per frame, after the server's scripts and physics; client-side events (Changed, ChildAdded…) fire for them | Roblox docs; "once per frame" estimated |
| Client → server | client changes stay on the client, except the character it owns: its pose is copied to the server every frame (network ownership) | Roblox docs (network ownership) |
| Moving parts | unanchored parts simulate on the server; their CFrame streams to the client | estimated (one shared physics world on the server side) |
| RemoteEvent | FireServer (client only) → OnServerEvent(player, …); FireClient / FireAllClients (server only) → OnClientEvent(…); calling from the wrong side errors | Roblox docs (RemoteEvent) |
| UnreliableRemoteEvent | same as RemoteEvent, nothing is ever dropped locally | Roblox docs; no packet loss to simulate |
| RemoteFunction | InvokeServer yields until OnServerInvoke returns (errors are passed back to the caller); InvokeClient not supported | Roblox docs (RemoteFunction) |
| Remote delivery | at the end of the frame, after both sides ran; values passed by copy, instances mapped to the other side's copy (nil when the other side doesn't have it) | Roblox docs ("argument limitations"); timing estimated |
| BindableEvent / BindableFunction | same side only, deferred like other signals; Invoke runs OnInvoke and returns its results | Roblox docs |
| RunService | IsServer / IsClient by side, IsStudio true, IsRunning; server: Stepped, Heartbeat; client: RenderStepped, Stepped, Heartbeat | Roblox docs (RunService) |
| BindToRenderStep | bound functions run every frame in priority order (lowest first; Enum.RenderPriority values or numbers), each in its own thread so one that yields doesn't hold up the rest | Roblox docs (BindToRenderStep, RenderPriority) |
| Attributes | a replicated attribute change fires AttributeChanged and GetAttributeChangedSignal on the client, as a local SetAttribute does | Roblox docs |
| Buffers | a buffer passed through a remote arrives as a copy | Roblox docs (buffer, remote argument limits) |
| Camera | every client has Workspace.CurrentCamera (made when the place has none) | Roblox docs |
| Output | every script line is tagged [Server] or [Client] while Play runs | person's request |
| Explorer | Server / Client toggle at the top while Play runs; the viewport shows the side you pick | person's request |

## Character (Play)

| Value | Number | Source |
|---|---|---|
| Rig | R15 by default (toolbar R15 / R6 picker, or the avatar's own type); a Humanoid Model with HumanoidRootPart and Motor6D joints | StarterPlayer default R15; joint names from the Roblox docs |
| R15 parts | HumanoidRootPart 2×2×1, LowerTorso 2×0.4×1, UpperTorso 2×1.6×1, Head 1.2³, arms 1 wide (upper 1, lower 0.95, hand 0.3 tall), legs 1 wide (upper 1.15, lower 1.15, foot 0.3 tall) | joint layout from the Roblox docs; sizes estimated |
| R15 joints | Root, Waist, Neck, Left/Right Shoulder, Elbow, Wrist, Hip, Knee, Ankle; each part has its …RigAttachment | Roblox docs |
| R6 parts / joints | Torso 2×2×1, Head 1.2³, arms and legs 1×2×1; RootJoint, Neck, Left/Right Shoulder, Left/Right Hip | Roblox docs; sizes estimated |
| Hip height | R15 2 studs × height scale, R6 0 (R6 uses its legs) | estimated |
| Body | Humanoid properties from StarterPlayer (CharacterWalkSpeed, CharacterJumpPower/Height, CharacterUseJumpPower, CharacterMaxSlopeAngle) | Roblox docs (StarterPlayer) |
| Physics shape | one dynamic assembly: the HumanoidRootPart with every part the joints hold; Head, torso parts and the root collide as their own boxes at the animated pose, limbs don't (R15 CanCollide false); mass, centre and inertia from the joints' own pose. See "Round 27" | Studio code and measurements, checks/humanoid-physics |
| Upright | the character never tips over; with Humanoid.AutoRotate it turns toward its MoveDirection, the angle left shrinking as exp(−7 t); with UserGameSettings.RotationType CameraRelative (shift-lock, first person) it faces the camera at once | Studio: turning to a new direction left 1.48 → 0.93 / 0.38 / 0.12 rad after 0.10 / 0.22 / 0.35 s |
| Walk speed | Humanoid.MoveDirection × WalkSpeed (16), full speed from the first frame; the client's Humanoid counts (a LocalScript changing WalkSpeed moves the player at once) | Studio: 15.8 studs/s from the first frame |
| Jump | while Humanoid.Jump is set (the ControlModule holds it while Space is down): commanded speed JumpPower with UseJumpPower, else √(2·g·JumpHeight) / 1.06 clamped to [0, 1000]; it leaves the ground at 1.06 × that | Studio code Studio (table at the top); Studio: JumpPower 50 → 7.27 studs, 100 → 28.86; JumpHeight 7.2 → 7.31, 3 → 3.07; 0.51 s in the air at 50 |
| States | Running ↔ Jumping → Freefall → Landed (0.1 s) → Running; Dead on death. GetState, StateChanged, Jumping, FreeFalling, Running events | Roblox docs (HumanoidStateType); Landed duration estimated; Climbing: see Climbing below |
| Animation | each joint is posed by its Motor6D.Transform (Part1 = Part0 · C0 · Transform · C1⁻¹) as the client has it; AnimationTracks write it ("Animation" below), and so can a game's own scripts. No procedural swing: Studio offline can't load the default animations (their tracks report Length 0) and its character stands still, as rustudio's does | Studio: Animate's idle tracks playing with Length 0 |
| Network owner | the client moves its own character and sends the pose to the server | Roblox docs (network ownership) |
| Default look (no avatar) | blocky parts in Studio's offline colours: head and arms (245, 205, 48), torso (13, 105, 172), legs (164, 189, 71) | Roblox Studio: the Play character's Head / UpperTorso / RightUpperArm / RightUpperLeg Color |
| Avatar | signed in (or Account → avatar username): body type, BodyColors, scales, Shirt / Pants / T-shirt, face decal, rigid accessories by attachment name | Roblox docs (avatar API, HumanoidDescription) |
| Avatar not supported | layered clothing, body-part / head meshes, dynamic heads, emotes, animations | not implemented |
| Acceleration | Running's force: 150 × (target − velocity) per mass, at most mass × g × the floor's friction, on the 240 Hz steps (level 2: every 2nd tick over 2h) | Studio code Studio; "Round 27" |
| Step-up height | what the hip-height spring and the floor rays give (no number of its own) | Studio code; probes T5 |
| Max slope | 89° | Humanoid.MaxSlopeAngle default (Roblox docs) |
| Movement keys | Roblox's own ControlModule (Keyboard): ContextActionService PlayerActions CharacterForward W / Up, Backward S / Down, Left A, Right D, Jump Space; Player:Move relative to the camera's heading. The Left / Right arrows turn the camera (CameraInput) | PlayerModule source (Studio install); Studio's key tool can't send arrow keys, so that part is from the scripts |
| Health / death | Humanoid.Health 100; at 0 (or below FallenPartsDestroyHeight −500) limbs fall apart, respawn after 5 s | Players.RespawnTime default 5; Workspace.FallenPartsDestroyHeight default −500 |
| Look | Studio's offline colours: head and arms (245, 205, 48), torso (13, 105, 172), legs (164, 189, 71); Place A's viewmodel arms take their colour from these | Roblox Studio |
| Camera | the client's Workspace.CurrentCamera, moved by Roblox's CameraModule (Classic camera, Poppercam, ZoomController, MouseLockController, TransparencyController); see "Player controller" | Studio: starts 12.5 studs from the head, pitch −15°, both matched |

## Player controller (Roblox's default scripts, Play)

Studio's own scripts run unchanged, read from the Studio install at Play
start (nothing copied into rustudio). Checked in Studio by listing the
client's PlayerScripts and the character during Play and matching each
script's source length with the files.

| Value | Setting | Source |
|---|---|---|
| Which scripts | StarterPlayerScripts gets PlayerScriptsLoader, PlayerModule (CameraModule, ControlModule, CommonUtils; `*.spec.lua` left out) from ExtraContent/scripts/PlayerScripts/StarterPlayerScripts, and RbxCharacterSounds (+ AtomicBinding) from StarterPlayerScriptsCommon. Each character gets Health (content/avatar/scripts/humanoidHealthRegenScript.rbxmx) and Animate (R15: content/avatar/scripts/humanoidAnimateR15_DEPRECATED_20260813.rbxm; R6: humanoidAnimatePlayEmote.rbxm) | Studio: PlayerModule 793, CameraModule 25 338, ControlModule 24 195, Animate 28 802, Health 703 characters, equal to those files; the R6 / R15 choice and the name rule from Studio code Studio (table at the top) |
| Overrides | a place's own script of the same name in StarterPlayerScripts / StarterCharacterScripts wins; Place A keeps the defaults and turns the controls off itself (GetControls():Disable()) | Roblox behaviour |
| Not inserted | Studio's legacy ChatScript / BubbleChat; rustudio keeps its own chat box | rustudio |
| Player settings | each Player copies StarterPlayer's CameraMaxZoomDistance, CameraMinZoomDistance, CameraMode, DevCameraOcclusionMode, DevComputerCameraMovementMode → DevComputerCameraMode, DevComputerMovementMode, DevTouchCameraMovementMode → DevTouchCameraMode, DevTouchMovementMode, EnableMouseLockOption → DevEnableMouseLock, AutoJumpEnabled, Health / NameDisplayDistance | Roblox docs (StarterPlayer) |
| Core services | Play makes Players, ReplicatedStorage, ReplicatedFirst, StarterGui, StarterPack, StarterPlayer (with its two script folders) when the file has none | Roblox: every DataModel has them |
| Device | keyboard and mouse: TouchEnabled, GamepadEnabled, VREnabled false; PreferredInput KeyboardAndMouse; touch and gamepad modules load but stay idle | Studio's Play on a desktop |
| User feature flags | UserSettings():IsUserFeatureEnabled answers as Studio does for the 22 flags the scripts read (UserFixStuckShiftLock, UserRaycastUpdateAPI2, UserCurrentCameraUpdate2… true; UserPSSinkUnknownTouchEvents, UserFixVRImmersionAutoRotate, UserAnimateRemoveEmoteChatHook, UserAnimationAbilityManagerFixed false); unknown names error, like Studio's | Studio, 2026-11-01 |
| Input order | every key, mouse button, mouse move and wheel becomes one InputObject per device (a userdata: typeof gives "InputObject"); ContextActionService's bound actions see it first, highest priority first (newest first among equal), nil / Sink stops it, Pass hands it on, End goes to the actions that took the Begin; then UserInputService InputBegan / InputChanged / InputEnded, and PointerAction for the wheel | Roblox docs |
| Mouse motion | InputObject.Delta and GetMouseDelta are non-zero only while the cursor is locked (MouseBehavior LockCenter or LockCurrentPosition); the view grabs the cursor then (Locked, Confined where locking isn't possible) and lets go when the game sets Default | Roblox docs; CameraInput relies on it |
| Wheel | 0.5 of wheel input (InputObject.Position.Z, PointerAction) per notch | Studio: one wheel event of −3 / +2 / −1 notches zoomed 12.5 → 23.375 → 14.917 → 19.146 studs, ZoomController's step for 0.5 per notch; rustudio gives the same numbers |
| Client frame order | input → RenderStepped / PreRender (BindToRenderStep: ControlModule at Input 100, CameraModule at Camera 200) → PreAnimation (AnimationTracks) → Stepped / PreSimulation → PostSimulation / Heartbeat; the character moves next frame from what the client's Humanoid holds | Roblox docs (RunService); Place A moves its player on PreSimulation |
| Humanoid:Move / Player:Move | sets MoveDirection (length at most 1), turned by the camera's heading when relative | Roblox docs |
| Root constraints | a LinearVelocity (Plane, World) on the player's HumanoidRootPart sets its horizontal velocity, a rigid one-attachment AlignOrientation its facing, a script setting AssemblyLinearVelocity its vertical speed; AssemblyLinearVelocity / AssemblyMass read back the character's | Roblox physics (rustudio's character is kinematic); Place A' "pmove" |
| Shift-lock / first person | MouseLockController (LeftShift) and zooming in to 0.5 studs set MouseBehavior LockCenter and RotationType CameraRelative; the head and accessories fade (LocalTransparencyModifier) | the scripts; self-test "player-module" |
| Right-drag | CameraInput turns 0.5° per pixel (vertical × 0.77) while MouseBehavior is LockCurrentPosition | CameraInput source |
| Place A match | its own movement now gets its keys and frame events, but the player stays put: Nacht's collision MeshParts collide as boxes in rustudio's physics (and some hull brushes fall back to boxes offline), so the capsule starts inside them | open; physics shapes, not the controller |

## Animation

| Value | Setting | Source |
|---|---|---|
| Animator | spawned characters carry Humanoid.Animator; Humanoid / AnimationController:LoadAnimation go through their Animator (made on first use); AnimationPlayed fires on the Animator and its Humanoid; GetPlayingAnimationTracks reads the Animator | Roblox docs; humanoidClassicAnimate.lua (FindFirstChildOfClass("Animator")) |
| Assets | an AnimationId (rbxassetid://, …?id=) loads from KeyframeSequenceProvider registrations, the local asset store, the cache, else public asset delivery in the background; the track has Length 0 until it arrives, then plays; KeyframeSequence decoded from the asset's rbxm / rbxmx (Keyframes → Pose trees, Weight 0 poses skipped, KeyframeMarkers) | Roblox docs (Length is 0 until loaded); verified: 507766388 (R15 idle, 227 keyframes), 180435571 (R6 idle) download without a login |
| Default ids | Roblox's Animate scripts from the Studio install pick them: R15 idle 507766388 / 507766666, walk 913402848, run 913376220, jump 507765000, fall 507767968, climb 507765644, sit 2506281703, swim 913384386; R6 idle 180435571 / 180435792, walk 180426354, jump 125750702, fall 180436148, climb 180436334, sit 178130996 | humanoidAnimateR15_DEPRECATED_20260813.rbxm, humanoidAnimatePlayEmote.rbxm |
| Blending | per joint, priorities layer from Core up (Core, Idle, Movement, Action, Action2–4): a level's tracks blend by weight (normalised past a total of 1) and cover the levels below by min(total weight, 1); Play / Stop / AdjustWeight fade over fadeTime (0.1 s default) | Roblox docs (AnimationPriority, weights) |
| Easing | a pose eases to the next keyframe by its EasingStyle: Linear, Constant (holds), Cubic, Elastic, Bounce with EasingDirection reversed against TweenService's, CubicV2 the TweenService way | Roblox docs (PoseEasingStyle: CubicV2 added because Cubic's direction was reversed) |
| Events and values | KeyframeReached (named keyframes), GetMarkerReachedSignal, DidLoop, Stopped, Ended; Length, TimePosition, Looped, Priority, Speed, WeightCurrent / WeightTarget | Roblox docs |

## Materials

Studio's install only holds the textures for legacy surfaces (studs.dds),
Plastic and water. Every other material's textures are TexturePack assets
Studio downloads at run time (ids in its three material tables, see "Read
from Studio's program"); offline Studio can't, and draws them flat
in the table's colour. rustudio downloads them the way MATERIALS.md
describes (public asset delivery, no login, gzip, two hops) and caches them
by id.

| Value | Setting | Source |
|---|---|---|
| Legacy surfaces | Studs, Inlet, Weld (Glue drawn the same), Universal on block parts, from Studio's PlatformContent/pc/textures/studs.dds: 128 px wide, four 512 px bands (studs, weld, inlet, universal) of 2 × 8 studs, counted from the face's corner; its grey 0.5 leaves the colour, the bevels scale it (squared) | Studio (offline Studio) at the same view (mat.rbxlx, 607 × 522 from (−3, 9, 18)): face level / contrast studs 125 / 16.8, inlet 121 / 18.9, weld 103 / 34.1, universal 123 / 18.8; rustudio 126 / 12.6, 126 / 14.5, 110 / 32.4, 126 / 16.7 |
| Terrain colours | Studio's PlatformContent/pc/terrain/materials.json base_color per material (Grass 106, 127, 63 …) unless the place sets Terrain.MaterialColors; materials2022.json and materialTransitions.json (texture tiling, transitions) need the texture packs and aren't used | Studio (offline Studio): Terrain:GetMaterialColor equals them for Grass, Rock, Sand, Snow, Mud, Slate, Brick |
| MaterialVariant | Part.MaterialVariant, else MaterialService.<Material>Name, names a MaterialVariant in MaterialService with the same BaseMaterial: its ColorMap covers every face, tiled every StudsPerTile (default 10), tinted by the part colour; NormalMap / RoughnessMap / MetalnessMap not used yet | Roblox docs |
| Use2022Materials | MaterialService.Use2022Materials true picks the 2022 table (its 4K variant, RenderEnablePartBaseMaterial4K's default), false or absent the pre-2022 table | Studio table builder; offline Studio's flat colours match the pre-2022 table |
| Texture packs | pack XML (`<color> <normal> <roughness> <metalness> <height> <tiling>`) → each map a PNG; decoded, resized to 1024² and mipmapped (colour averaged as squares, the rest linearly) into two texture arrays: colour + height, normal X / Y + roughness + metalness; a missing map is flat (the table colour, a flat normal, roughness 1, metalness 0) | MATERIALS.md; e.g. 2022 4K Brick 85077012310177: colour, normal, roughness, height |
| Drawing | untextured parts (no TextureID / SurfaceAppearance / MaterialVariant, not Neon / SmoothPlastic / Glass / ForceField) draw colour map × part colour, squared, and bend the normal by the normal map in the face's tangent frame; box faces repeat every 8 studs from the face corner, MeshParts use their UVs. Roughness, metalness, height and the detail pack aren't used in lighting yet | Studio part shaders (glsl3 pack) |
| Before a pack arrives | the table's average colour, squared, times the part colour | the colour column of the tables |

## Collision (MeshParts)

| Value | Setting | Source |
|---|---|---|
| CollisionFidelity | Box (2): the part's box; Hull (1): one convex hull of the mesh's vertices; Default (0) and PreciseConvexDecomposition (3): a convex decomposition of the mesh | Roblox docs (Enum.CollisionFidelity) |
| Decomposition | V-HACD at concavity 0.01 (Studio's HACD flags: 1% of the mesh size), voxel resolution 64 for Precise, 32 for Default; hulls kept in the mesh's unit box and stretched to each part's Size | Studio HACD fast-ints; resolutions estimated (Precise documented as the finer) |
| While computing | decompositions run on background threads; an anchored part collides with the mesh's exact triangles meanwhile, an unanchored one with its hull; the finished pieces replace them within 0.25 s | rustudio |
| Cache | ~/.cache/rustudio/collision/{hash}.bin, keyed by mesh id, fidelity and settings version | rustudio |
| Rules kept | CanCollide / Anchored / welds / assemblies as for other parts; a character's own parts never block it | as in "Physics" |
| Place A | Nacht's map pieces use PreciseConvexDecomposition, brushes Hull (its Bo3Map CreateMeshPartAsync options); in a match the character walks (4000, 3.1, −34) → (4006.7, 3.1, −45.2) → (4025.7, 4, −34.1) → (4014.7, 3, −11) → (3993.6, 3.1, −22.1) | rustudio, Place A match walk |

## Avatar (bodies and layered clothing)

| Value | Setting | Source |
|---|---|---|
| R15 body | Studio's content/avatar/characterR15DynamicHeadV2.rbxm: Roblox's body-part MeshParts (LeftHand 7430070991 … LowerTorso 7430071109), each with a WrapTarget cage (7430071100 …), RigAttachments, Motor6Ds and a Humanoid (HipHeight 2). Its head — Roblox's current dynamic head, mesh 123480233606534 (Draco), cage 76523574292282, face 111092388570647 (transparent around the face, so the head colour shows) — is served only to signed-in accounts: kept when signed in or cached, else Studio's defaultDynamicHead.rbxm head (mesh 11445807850, face 11445807951) takes the Neck | Studio install; asset delivery (session for the head); self-test "catalog-assets" |
| Layered clothing, real item | Roblox's "Hooded Jacket - Gray" (8516620262, an Accessory whose Handle has a WrapLayer: mesh 8917454991, inner cage 8917455036, outer cage 8917455040, Order 1, Puffiness 1): 1 706 of its 1 709 inner-cage points pair with the body cages by UV; it lands on 11 body parts, 90% of its vertices outside the body parts' boxes | self-test "catalog-assets" |
| R6 body | Studio's content/avatar/character.rbxm (Head with a Head SpecialMesh × 1.25 and face decal, Torso, limbs, accessory attachments) with Roblox's six classic joints: RootJoint and Neck C0/C1 rotated (−1,0,0 / 0,0,1 / 0,1,0), shoulders and hips at (±1, 0.5 / −1, 0) with C1 (∓0.5, 0.5 / ±0.5, 1), MaxVelocity 0.1 | Studio install; the classic R6 joint frames R6 animations are authored in |
| Head mesh type | SpecialMesh MeshType Head draws Studio's content/avatar/heads/head.mesh (1.2 studs native), round across the part's smaller horizontal side × Scale | Studio install; diameter rule estimated |
| R15 scales | each part's OriginalSize and each attachment's OriginalPosition × (width, height, depth) (the head × head scale); Motor6Ds re-seated on their RigAttachments; HipHeight × height | values stored in the body file |
| Collisions | Head, UpperTorso, LowerTorso (R6: Torso) and the HumanoidRootPart collide; limbs don't | Studio (CanCollide of the default rigs) |
| Layered clothing | an Accessory whose Handle has an enabled WrapLayer fits onto the character's WrapTarget cages: inner cage (ReferenceMeshContent at ReferenceOrigin) and body cage points paired by template UV; garments go on in Order, each next one onto the previous one's fitted outer cage (CageMeshContent at CageOrigin); each garment vertex follows its 4 nearest inner-cage points (inverse-square weights) and its height above them × Puffiness (clamped to [−1, 1], default 1, as Studio's setter does); accessories put on during Play attach by matching attachment names like spawn-time ones; drawn cut by body part so it follows the joints | Roblox docs (WrapLayer, WrapTarget, cages share one template); Puffiness range and default from Studio (Studio, WrapPuffinessDefaultPercent); binding rule rustudio's |

## Physics (rapier3d)

| Value | Number | Source |
|---|---|---|
| Gravity | Workspace.Gravity, default 196.2 studs/s² | Roblox docs |
| Step | fixed 60 Hz, 4 solver iterations, at most 3 steps per frame (a slow frame slows the simulation instead of piling up), solver runs on all cores | estimated |
| Assemblies | parts joined by rigid joints (Weld, ManualWeld, WeldConstraint, Snap, Glue, Motor6D, Motor) are one assembly; an assembly with an Anchored part is anchored | Roblox docs (Assemblies) |
| Bodies | anchored assembly → fixed colliders built once; other assembly → one dynamic body with one collider per part (root = biggest part); CanCollide=false → sensor (still reports touches) | Roblox docs (Anchored, CanCollide, CanTouch); root choice estimated |
| Sleeping | bodies that stay still for 2 s sleep; rapier length scale 5 studs for its sleep thresholds and contact tolerances | estimated |
| Updates | only assemblies whose parts, joints or flags changed are rebuilt; a script moving a part moves its whole assembly | Roblox docs; incremental update is rustudio's |
| Humanoid models | an assembly holding a HumanoidRootPart keeps its rotation locked (stays upright) | estimated |
| Material | density 0.7, friction 0.3, elasticity 0.5 for every part | Plastic PhysicalProperties (Roblox docs); per-material values not used yet |
| Shapes | Block, Ball (radius = smallest size / 2), Cylinder (along X), Wedge, CornerWedge; MeshParts by CollisionFidelity (see "Collision"); Unions use their box | Roblox docs |
| Movers | BodyForce, BodyThrust, VectorForce, Torque; BodyVelocity, LinearVelocity; BodyPosition, AlignPosition; BodyGyro, AlignOrientation; BodyAngularVelocity, AngularVelocity — recomputed every step, defaults (P, D, MaxForce, Responsiveness…) from the reflection database | Roblox docs for what each property does; control laws estimated: body movers are springs P·error − D·velocity solved implicitly, Align* are critically damped springs with natural frequency = Responsiveness, velocity constraints close the error in one step, BodyVelocity / LinearVelocity / AlignPosition also hold the part's weight; an unlimited LinearVelocity (ForceLimitsEnabled off or MaxForce inf, any mode) is rigid against friction: the velocity the floor's friction took off its axes last step (not what a contact normal held back) is added to the next push, so a box driven at 10 on a Plastic floor holds 10.00 and covers 10 studs a second (it held 9.02) |
| Touched / TouchEnded | contact start/stop between parts (CanTouch respected), and the character's parts touching a part | Roblox docs |
| Terrain | Workspace.Terrain voxels (SmoothGrid) → fixed triangle meshes in 64-stud cells, friction 0.4, elasticity 0.1; water has no collider (no swimming yet); Touched reports the Terrain | SmoothGrid format from community docs (rbx-dom); cell size, friction and elasticity estimated |
| Explosion | an Explosion put in Workspace: Hit(part, distance) for every part within BlastRadius (default 4); unanchored parts pushed outward by BlastPressure × 0.0004 / mass studs/s (at most 150); joints of parts within BlastRadius × DestroyJointRadiusPercent break and Humanoids there die; a 0.6 s flash | Roblox docs (Explosion); push strength, cap and flash estimated |

## Meshes

| Value | Setting | Source |
|---|---|---|
| Formats | Roblox mesh 1.00–5.00, 6.00 and 7.00 (plain COREMESH), OBJ; Draco-compressed 7.00 meshes are skipped with an Output warning | community-documented mesh format |
| Sources | `rbxasset://` → the content folder you choose (RUSTUDIO_CONTENT_DIR, else ~/.local/share/rustudio/content); `file://`, absolute and relative paths (for a place's own properties in Edit only: while scripts run (Play/Run) place content — images, meshes, sounds — loads only `rbxasset://` inside the content folder, cloud ids and engine-generated `rbxtemp://` ids, checked before every cache, since what renders or plays can be read back; RUSTUDIO_ALLOW_LOCAL_CONTENT=1 lifts that for local development. A script loading content itself never gets local files); `rbxassetid://`, `?id=` and plain ids download (below) | clean room: no Roblox program files, no private endpoints |
| MeshPart | mesh stretched to the part's Size | Roblox docs |
| SpecialMesh (FileMesh) | mesh at its own size × Scale, shifted by Offset | Roblox docs |

## Asset download

| Value | Setting | Source |
|---|---|---|
| Endpoint | GET assetdelivery.roblox.com/v2/assetId/{id} (or …/version/{n}) → JSON `locations`, then the first location (gzip inflated) | MATERIALS.md; AUTH.md (evidence table) |
| Auth | signed in: the session cookie (assets served only to signed-in accounts included: the current dynamic head, catalog layered clothing, the account's private places); signed out or refused: the same endpoint anonymously for public assets. Background loads (cages, animations, packs) try anonymous first, then the session | AUTH.md; round 10 probes |
| Meshes 7.00 | COREMESH chunk v2 = u32 length + a Draco bitstream (Position, Normal, TexCoord attributes and triangles), decoded with draco-core; e.g. the dynamic head 123480233606534: 5 406 points, 1.196 × 1.203 × 1.198 studs, the Head part's Size in Studio's file | the mesh bytes ("DRACO" magic after the length); characterR15DynamicHeadV2.rbxm |
| InsertService:LoadAsset | the local asset store, the upload ledger, the cache; signed in, a model not on disk is downloaded with the session (waiting for it; a failure is remembered for the run) | Roblox docs (LoadAsset yields) |
| Cache | ~/.cache/rustudio/assets/{id}_v{version or latest}.bin (RUSTUDIO_CACHE_DIR overrides); read before any download, gzip bodies unpacked first | chosen |
| Textures | PNG, JPEG, DDS (BC1/BC2/BC3 and uncompressed) | community-documented formats |
| Applied | MeshId / MeshContent, TextureID, SpecialMesh TextureId, SurfaceAppearance ColorMap (replaces the part colour where opaque), Decal (one image stretched over a face) and Texture (repeats every StudsPerTileU/V, offset by OffsetStudsU/V) | Roblox docs |
| Decal look | multiplied by Color3, Transparency respected, drawn just above the face | Roblox docs; draw offset estimated |
| Classic clothing | Shirt / Pants templates 585 × 559 px with the documented torso and limb regions on the body boxes; pants drawn under the shirt; T-shirt (ShirtGraphic) on the torso front | Roblox docs (classic clothing template) |
| Failed or unsupported | Output warning, part keeps its plain look; retried after signing in | chosen |

## Shortcuts

| Action | Key | Source |
|---|---|---|
| Select / Move / Scale / Rotate tool | Ctrl+1 / Ctrl+2 / Ctrl+3 / Ctrl+4 | estimated |
| Undo / Redo | Ctrl+Z / Ctrl+Y (also Ctrl+Shift+Z) | request (undo/redo) |
| Copy / Cut / Paste / Paste into | Ctrl+C / Ctrl+X / Ctrl+V / Ctrl+Shift+V | request (copy/paste) |
| Duplicate | Ctrl+D (in place) | request; placement estimated |
| Delete | Delete | estimated |
| Group / Ungroup | Ctrl+G / Ctrl+U | estimated |
| Rename | F2 or double-click in Explorer | request (rename) |
| Focus selection | F | estimated |
| Insert object | Ctrl+I | estimated |
| New / Open / Save / Save as | Ctrl+N / Ctrl+O / Ctrl+S / Ctrl+Shift+S | estimated |
| Multi-select | Ctrl or Shift click (viewport), Ctrl toggle / Shift range (Explorer), drag box | request (multi-select) |

## New objects and the starter place

| Value | Number | Source |
|---|---|---|
| New block part size | 4 × 1.2 × 2 studs | Roblox Studio: Instance.new("Part").Size |
| New sphere / cylinder size | 2 × 2 × 2 studs | estimated |
| New part colour | RGB 163, 162, 165 | rbx_reflection_database default for Part.Color |
| New part material | Plastic (256) | rbx_reflection_database default |
| New parts anchored | yes | estimated |
| Baseplate | 512 × 20 × 512 studs, top at y = 0, RGB 99, 95, 98, locked | estimated |
| SpawnLocation | 12 × 1 × 12 studs at (0, 0.5, 0) | estimated |
| Ground grid | 4-stud lines, every 4th brighter, ±256 studs | estimated |

## Lighting (used when the place's Lighting service has no value)

| Value | Number | Source |
|---|---|---|
| ClockTime | 14 | estimated |
| Brightness | 2 | estimated |
| GeographicLatitude | 41.733° | estimated |
| Ambient | RGB 70, 70, 70 | estimated |
| OutdoorAmbient | RGB 128, 128, 128 | estimated |
| Neon / SmoothPlastic / Glass / ForceField | see the model below | enum numbers from rbx_reflection_database |

## Lighting model (measured against Studio)

Every number here was measured in Roblox Studio
(Technology Future, an 863×675 view from (4, 9, 22) toward (4, 2, 0), FOV 70,
test places in /tmp/rs-light/places), reading screenshot pixels with
/tmp/rs-light/sample.py, or read from Studio's own shader pack (glsl3 / Vulkan
"RBXS" packs in the Studio folder; Globals constant buffer CB0[13..31]:
AmbientColor, SkyAmbient, Lamp0Color/Dir, FogParams, FogColor, Exposure,
LightConfig, SkyGradient/Env scales). "Display" values are 0–255 screen
levels; "linear" is light before the tone curve.

| Value | Number | Source |
|---|---|---|
| Colour to linear | channel² (gamma 2.0) for part colours, light colours, Ambient, skybox texels | Studio shaders (sqrt on output, squaring on input); fits all scenes below |
| Display gamma | output = sqrt(tone-mapped) | Studio shaders ("sqrt(c × Exposure)" in the Future pack) |
| Default tone curve | y = x(ax+b) / (x(ax+c)+d) × e, a 6.0402, b 0.0064675, c 2.41386, d 0.656391, e 1.157207 | fitted to 16 Ambient/Brightness/Exposure screenshots, RMS 0.38 levels (/tmp/rs-light/fit_tone.py) |
| Retro tone curve (ColorGradingEffect.TonemapperPreset Retro) | none: sqrt(clamp(x)) | fitted, RMS 2.6 levels (fit_retro.py) |
| ExposureCompensation | light × 2^E | Exposure 1/2/3 screenshots |
| Sun | 0.5 × Brightness × N·L, white at every ClockTime | 5 clock times, Brightness 0.5–4 (calib.json, calib2.json): front face 149 at 6:30, 7:45, 10:00 and 17:00 |
| ColorShift_Top | tints the sun by its hue only (averaged to 1); black/grey/white change nothing | calib2 clock14shift; ColorShift_Bottom has no effect (clock14shiftbot) |
| Sun direction | (cos θ cos φ, sin θ cos φ, sin φ), θ = (ClockTime − 6)·15°, φ = latitude − 23.5°; ClockTime first clamped to [0, 24] and cut to whole seconds | Lighting:GetSunDirection at 5 clock times, error < 0.001; cached at Lighting |
| Ambient / OutdoorAmbient | Ambient² + max(Outdoor² − Ambient², 0) × sky visibility (outdoors: the larger of the two), at night too | calib.json; future_night side face 130 vs 132 |
| Sky visibility (rustudio) | 0 under a roof: highest underside of mostly-opaque parts over each 4-stud cell, 128² cells around the camera | rustudio approximation of Studio's 4-stud lighting voxels |
| Sun shadows | 2048² shadow map, ±128 studs around the view, 3×3 filter | rustudio; Studio's edge is slightly softer (future_day base_shadow 29 vs 30) |
| Local lights (LightingStyle Realistic) | Color² × Brightness × 1.06 × window × 110/(110 + x²) × N·L, x = clamp(d, 0.7R, R) − 0.7R, window = 2t³ − 3t² + 1, t = x / 0.3R | 5 PointLight profiles (B 0.5–2, R 20–40, height 4–8), RMS ~3 levels (lightcal.json, fit_light3.py) |
| SpotLight cone | Angle is the full cone: cos cut = cos(Angle/2), strength × sat((cos − cos cut)/(1 − cos cut))² | Future spot 60° at height 10: 97 under it vs 99 predicted, edge at 5.5 studs vs 5.77 (studio_spot60.png); a Light1-like spot (studio_spot2.png) |
| Lights in Attachments | at the attachment's world CFrame, Face relative to it (Front = LookVector) | studio_spot2 (Attachment spot) matches |
| LightingStyle Soft: point lights | no N·L (a ball reads 224 all round), 0.98 × Brightness on a plateau, fade from 0.6 Range to max(Range, 8) + 1, short lights weaker (0.13 at Range 4, 0.49 at 8, 0.87 at 12, 1 from 20) | softcal2/3.json, ball_soft.json; Place A lobby uses Soft (from round 11: see LightingStyle default) |
| LightingStyle Soft: spot lights | no N·L, edge blurred −2/+4 studs, strength × min(1, cone cross-section / 800 studs²) | softcal2/3.json (spot 30–120°, height 5–20); approximate (lobby red spots read about half as bright as Studio's) |
| LightingStyle default | When the place doesn't save LightingStyle it follows Technology: Legacy / Voxel / Compatibility, or no Technology at all, → Soft; ShadowMap / Future → Realistic. PrioritizeLightingQuality follows (on only for Future). Stored into the place on open (`doc::ensure_services`), so scripts read what is drawn; new places save Technology Future (Studio's Baseplate) | tools/fidelity: Place A saves neither and Studio reads Soft, PrioritizeLightingQuality false; the control place (Future) reads Realistic, true; future_day/night (Future) N·L shading. (Round 10 drew Place A Realistic: its chapel read at 25–35 % of Studio's brightness.) |
| EnvironmentDiffuseScale | skybox irradiance (cosine-weighted, texels²) × scale | envd1: within 7 levels with no tuning; Future = ShadowMap (studio_envd1 / studioF_envd1) |
| Sky by time of day | full by day; dusk (sun 0.54 below) = day × (0.051, 0.032, 0.049); black at 00:00 | studio_envd_night (sky (10,14,27)), future_night (sky 0) |
| Default skybox | PlatformContent/pc/textures/sky/sky512_{bk,dn,ft,lf,rt,up}.tex (DDS), placed as any Sky's faces (next row) | Studio folder; sky top 137 vs 134, horizon 163 vs 161 |
| Skybox faces | looking +X Lf, −X Rt, +Y Up, −Y Dn, +Z Bk, −Z Ft; side faces read unmirrored from inside, image top up; Up / Dn as `viewport.rs prepare_sky` turns them; texels² with no scale by day | `skydiag` (below): 14 views within 1.4–2.2 levels |
| SkyboxOrientation | turns the whole sky: drawn(d) = sky(R⁻¹·d), R = Rz·Rx·Ry, each Roblox's rotation by that axis's degrees (not CFrame.fromOrientation's Y·X·Z) | `skydiag` orientations: 49–69 → 3.3–3.7 levels |
| Glass | lit colour × (1 − F) + sky(reflected) × F × mix(0.5, 1, colour), F = (1 − cos)^3.5; reflects even with EnvironmentSpecularScale 0 | glass_dark/glass_day cubes (Future and ShadowMap identical), largest error 23 at grazing |
| Neon | colour × 4, unlit (full channel → 255) | future_day neon (0, 255, 0) |
| Material darkening (Future) | per Material, linear: Wood 0.785, WoodPlanks 0.68, Marble 0.6, Basalt 0.36, Slate 0.56, CrackedLava 0.09, Concrete 0.66, Limestone 0.83, Granite 0.2, Pavement 0.31, Brick 0.62, Pebble 0.66, Cobblestone 0.61, Rock 0.36, Sandstone 0.44, CorrodedMetal (0.38, 0.22, 0.14), DiamondPlate 0.43, Foil 0.99, Metal 0.6, Grass 0.4, LeafyGrass 0.21, Sand 0.74, Fabric 0.17, Snow 0.73, Mud 0.26, Ground 0.2, Asphalt 0.23, Salt 0.74, Ice 0.99, Glacier 0.79, Cardboard 0.43, Carpet 0.64, CeramicTiles 0.87, ClayRoofTiles 0.36, RoofShingles 0.48, Leather 0.77, Plaster 0.74, Rubber 0.66 (full table in mech::material_albedo) | white cubes per Material vs SmoothPlastic, top and front agree within 0.01 (matcal.json) |
| MeshPart TextureID / SpecialMesh TextureId | covers the part; Color never shows through (except character body parts). Transparency 0: texel alpha ignored, transparent texels draw their own RGB opaque (a glasses texture's lenses come out black). Transparency > 0: alpha = texel alpha × (1 − Transparency), blended to the scene behind (smooth, not a cutout); MeshPart and SpecialMesh FileMesh behave the same | Place A lobby floor turned red in Studio: no change; 2026-11-13 Studio quads (alpha ramp, glasses PNG, a cloud beam texture) at Transparency 0 / 0.02 / 0.5 in front of a red wall |
| SurfaceAppearance AlphaMode | Overlay: transparent texels show Color, at any Transparency; Transparency: cut out below about half alpha at Transparency 0, blended (texel alpha × (1 − Transparency)) to the scene above it | Roblox docs; lobby railings; same 2026-11-13 Studio quads |
| Mesh vertex colours | `.mesh` RGBA (40-byte vertices, +36) and OBJ `v x y z r g b [a]` multiply Color, or the texture when there is one (checker × colours), lit as usual; vertex alpha counts only when Transparency > 0 (alpha × vertex alpha), like texel alpha | Studio part VS `VARYING2 = COLOR0 × instance colour` (glsl3 pack); 2026-11-13 Studio: vertex-coloured cube white / textured / Color (255,128,128), quads with two alpha-0 corners at Transparency 0 / 0.02 / 0.5 |
| SurfaceAppearance emission | ColorMap × EmissiveMask × EmissiveTint² × EmissiveStrength, unlit | lobby stained glass (strength 2.2) stays lit with every light off |
| Part textures | mipmapped (averaged in linear), trilinear, 8× anisotropic | lobby floor tiles: aliasing sparkle gone, floor 47 vs 43 |
| Post: ColorCorrection, Bloom | colour correction after the tone curve; bloom over Threshold, Intensity × spread over Size | Future pack post shader order (HDR → tone curve → 3×4 colour matrix; bloom thresholds on max RGB) |
| Fog | linear FogStart → FogEnd to FogColor² | estimated (Studio uses exp2 of FogParams; not yet fitted) |
| Not modelled yet | local-light shadows of MeshParts and unions (round 26: built-in parts block Shadows lights), Atmosphere, SunRays, DepthOfField, specular highlights, MaterialVariants, PBR maps | — |

Place A lobby (chapel) side by side: ambient-only matches (walls 8–9 vs 8–21);
with lights the floor matches (47 vs 43) and walls are lit, but Soft spot
lights stay weaker than Studio's (wall 32 vs 77). Nacht match room: lights
now sit where Place A puts them (Attachment.WorldCFrame was missing).

## Undo

| Value | Number | Source |
|---|---|---|
| Undo history | 500 steps | estimated |
| Merge window for repeated edits of the same property | 700 ms | estimated |

## Performance (measured on og.rbxl, 50 828 parts, 1265×900, Radeon RX 9070 XT)

| Value | Number | Source |
|---|---|---|
| Before this round | Play: 4.8 fps average, 1.3 fps 1% low (208 ms / 789 ms per frame); edit: 201 fps average, 121 fps 1% low | measured with `--bench` |
| After this round | Play: about 200 fps average, 72–77 fps 1% low (5.0 ms / 13.5 ms); edit with the whole place in view, camera turning: 1 157 fps average, 394 fps 1% low | measured with `--bench` (three runs each, empty content folder, not signed in) |
| Change journal | every edit, script write, physics move and replication is journalled per DataModel; the scene, physics and script queries read only what changed (1 000 000 entries kept; a reader 200 000 behind rebuilds) | estimated limits |
| Drawing | one instanced draw per (mesh, texture) per 128-stud cell; anchored, opaque parts and decals live in a static GPU buffer written once (changed slots rewritten in place); moving and see-through parts are uploaded per frame; new parts join the static buffer after 0.5 s without more new parts (or at 4 096 waiting) | estimated |
| Culling | per cell: frustum test on the cell's box; parts whose bounding sphere covers less than 1 pixel are skipped (cells keep parts sorted by size, so the cut is one search) | estimated |
| Depth | reverse-Z float depth; each part gets a fixed nudge of 0–15 × 4·10⁻⁷ of its depth so exactly coplanar faces of two parts always resolve the same way (no flicker) | estimated |
| Explorer | rows rebuilt only when the tree, names, expansion or filter change; only visible rows are drawn | estimated |
| Malicious scripts | scripts named like the classic free-model "Virus" don't run: "Script X detected as malicious.  Script will not run." | Roblox Studio (offline Studio) Output on og.rbxl |
| Unpublished DataStores | GetDataStore errors "You must publish this place to the web to access DataStore." while PlaceId is 0 | Roblox Studio (offline Studio) Output on og.rbxl |
| Local asset store | asset ids load from ~/.local/share/rustudio/assets/<id>[.ext] (RUSTUDIO_LOCAL_ASSETS) before downloading, read only | person's request (offline Studio serves its assets from there) |
| Script budget | each side resumes waiting scripts for at most 6 ms per frame; the rest resume next frame; a single resume over 3 s errors with the script and line | estimated |
| Replication | physics movement is copied without Changed events (as in Roblox); property ops are de-duplicated per frame; removals walk only the removed subtree | Roblox docs (Changed doesn't fire for physics); rest estimated |
| Self-test perf check | opens og.rbxl (or RUSTUDIO_PERF_PLACE), Play for 10 s after a 3 s warm-up, records average and 1% low; passes at ≥ 60 fps average | person's request |

## Shapes and meshes on parts

| Value | Setting | Source |
|---|---|---|
| BlockMesh / CylinderMesh | Scale stretches and Offset shifts the drawn shape; CylinderMesh is a vertical (Y) cylinder fitted to the part | Roblox docs (DataModelMesh); vertical orientation estimated |
| SpecialMesh shapes | Head (vertical cylinder), Torso / Brick (block), Wedge, Sphere (an ellipsoid fitted to Size × Scale), Cylinder (along X), CornerWedge; Prism / Pyramid / ramps draw as blocks for now | Roblox docs (MeshType); shapes estimated |
| Rotations | CFrames are squared up (orthonormalised) before drawing, so slightly skewed rotations in old places don't stretch parts | estimated |

## Scripting APIs added this round

| API | Behaviour | Source |
|---|---|---|
| Datatypes | Ray, Region3, Region3int16, Vector3int16, Vector2int16, NumberRange, NumberSequence(+Keypoint), ColorSequence(+Keypoint), Rect, PhysicalProperties, TweenInfo, Random (PCG32), DateTime (UTC), Faces, Axes, RaycastParams, OverlapParams, CatalogSearchParams; typeof gives their names; they convert to and from properties | Roblox docs; Random sequence and tostring formats estimated |
| Legacy spellings | findFirstChild, clone, remove, destroy, getChildren / children, isA, isDescendantOf, getPlayers, playerFromCharacter, breakJoints, makeJoints, move, takeDamage, loadAnimation, addItem, connect / wait on signals, lowercase properties (script.parent, game.workspace, className); globals Game, Workspace, ypcall, Spawn, Delay, ElapsedTime, version(), printidentity, settings(); LoadLibrary errors | Roblox docs (deprecated members); lowercase-property fallback is lenient (estimated) |
| Unset object properties | game.Workspace, workspace.Terrain and other unset object properties give the child of that name | estimated |
| Serialised twins | Humanoid.Health reads Health_XML when the file stores it that way | rbx-dom reflection database |
| ClickDetector | MouseClick / RightMouseClick (player), MouseHoverEnter / MouseHoverLeave; the detector can sit in the part or any Model above it; within MaxActivationDistance (default 32) of the character; hand cursor over clickable parts | Roblox docs |
| Mouse | Player:GetMouse(): Hit, Target, Origin, UnitRay, X, Y, ViewSizeX/Y; Button1/2 Down/Up | Roblox docs |
| Chat | "/" opens a chat box over the viewport; Enter fires Player.Chatted(message) on both sides and shows the line for 30 s; ServerScriptService.ChatServiceRunner with a ChatService stand-in module is added when missing | Roblox docs; the stand-in is rustudio's own |
| Sound | Play, Stop, Pause, Resume, Playing, Looped, Volume, PlaybackSpeed, TimePosition, TimeLength, IsPlaying; Played / Stopped / Paused / Resumed / Ended / DidLoop; real audio (Ogg, MP3, WAV, FLAC) from the content folder, files or asset ids; sounds saved Playing start with the game; sounds in parts fade as min(1, RollOffMinDistance / distance), silent past RollOffMaxDistance | Roblox docs; Volume above 1 grows as 1 + 0.5·ln(v) (estimated) |
| Humanoid | Health / MaxHealth / Died / TakeDamage; MoveTo and WalkToPoint walk the player or an NPC at WalkSpeed (reached within 1 stud, gives up after 8 s, MoveToFinished); Jump; Sit and PlatformStand stop walking; LoadAnimation returns an AnimationTrack (Play / Stop / Stopped / Ended, no keyframes yet); EquipTool / UnequipTools; ChangeState(Jumping); accessories | Roblox docs; reach distance estimated |
| Tools | StarterPack tools in the Backpack (their Scripts run there); hotbar over the viewport (keys 1–9 or click); equipping welds the Handle to the right hand (RightGripAttachment × Tool.Grip⁻¹), Equipped / Unequipped; left click fires Activated / Deactivated | Roblox docs |
| Teams | a player joins the first AutoAssignable Team (Team, TeamColor, Neutral=false) and spawns on a Neutral SpawnLocation or one in the team colour; Teams:GetTeams, Team:GetPlayers | Roblox docs; team choice simplified (one player) |
| TweenService | Create / Play / Pause / Cancel / Completed; Linear, Sine, Quad, Cubic, Quart, Quint, Exponential, Circular, Back, Elastic, Bounce with In / Out / InOut; RepeatCount, Reverses, DelayTime; numbers, Vector2/3, CFrame, Color3, UDim, UDim2 interpolate, other values switch at the end; advances on Heartbeat | Roblox docs; Back s = 1.70158, Elastic period 0.3 (common easing formulas) |
| CollectionService | AddTag / RemoveTag / HasTag / GetTags / GetTagged / GetAllTags / GetInstanceAddedSignal / GetInstanceRemovedSignal, and Instance:AddTag/HasTag/RemoveTag/GetTags (tags stored in the Tags property) | Roblox docs |
| Debris | AddItem(instance, lifetime = 10) | Roblox docs |
| World queries | Raycast (RaycastParams), FindPartOnRay(+IgnoreList/Whitelist), FindPartsInRegion3(+lists), GetPartBoundsInBox / InRadius, GetPartsInPart (true shapes, see "Spatial queries") | Roblox docs |
| Other services | HttpService JSONEncode / JSONDecode / GenerateGUID / UrlEncode (requests error "Http requests are not enabled", like a place with HTTP off); DataStores kept in memory for the session; PhysicsService collision groups recorded but not yet used by physics; ContextActionService BindAction on keys; RunService BindToRenderStep; Marketplace / Teleport / Badge calls warn and do nothing | Roblox docs; in-memory DataStores and inert purchases are rustudio's choices |
| Terrain editing | FillBlock, FillBall, … warn once and leave the terrain as saved | not implemented yet |
| Avatar type | StarterPlayer.GameSettingsAvatar R6 / R15 picks the rig; PlayerChoice uses the avatar or toolbar | Roblox docs |

## Terrain (drawing)

| Value | Setting | Source |
|---|---|---|
| Voxels | Workspace.Terrain.SmoothGrid, 4-stud voxels, occupancy (b + 1) / 256 | rbx-dom community docs |
| Surface | surface nets at occupancy 0.5, one mesh per material | rustudio's choice |
| Colours | the place's Terrain.MaterialColors, else rustudio's own flat colour per material; water 45% opaque | MaterialColors layout from rbx-dom docs; colours and water opacity estimated |

## Side by side with Roblox Studio (offline Studio, offline)

| Place | Studio | rustudio | Source |
|---|---|---|---|
| og.rbxl, edit, camera (0, 600, 600) pitch −35° | the same zones, buildings and terrain water; Studio draws the mesh trees, rocks and lava from local assets and lighter, more opaque water | same layout; meshes now load from the same local store | an offline Studio screenshot 1-20261101-020500-066.png vs `rustudio --camera 0,600,600,0,-35` |
| og.rbxl, Play, Output | asset-fetch failures (offline), "Virus … detected as malicious. Script will not run." ×3, "You must publish this place to the web to access DataStore.", UserHasBadgeAsync internal error, infinite yields on Torso (R15) and charactersCollected | matches for scripts: viruses refused, DataStore error, the same infinite yields | an offline Studio logs |
| Place A latest, Play (lobby) | lobby room with Place A's menu GUI and scripted camera at the altar; Output: offline asset warnings, GetHumanoidDescriptionFromUserId HTTP 503 | lobby room built and drawn from local assets (≈286 fps); Output: FBX uploads not imported yet (gun kits, perk bottle, grenades, map packs), one missing asset; no GUI drawn; camera follows Place A's Scriptable camera but lands at the player | an offline Studio logs + screenshot 1-20261101-021551-071.png, rustudio --bench / --screenshot |
| Place A (2026-11-01 build), lobby menu, 863×675 view | "PRIVATE GAME" title and the START GAME / CHANGE MAP / WEAPON KITS / GOBBLEGUM / DR. MONTY'S FACTORY list on tilted panels (SurfaceGuis on parts in front of the camera, AlwaysOnTop), START GAME highlighted orange, party panel top right, map card bottom left, "P Settings" footer; camera at the altar, FOV 50 | the same panels in the same places and perspective, the same fonts (Studio's Builder Extended / Sarpanch / Oswald files), the orange highlight; camera at the altar, no character. The room is darker (lighting look not matched yet) | an offline Studio screenshot 0-20261101-085209-816.png vs `rustudio … --play --window 1148x1104 --screenshot … --shot-after 40` |
| … Weapon Kits (click WEAPON KITS) | dark page, "WEAPON KITS" title, eight category cards in a grid with gun outline images and names | the same page, grid positions and images (the click goes through the SurfaceGui button) | an offline Studio 0-20261101-085218-785.png vs `--click 110,209@38` |
| … in a match (START GAME, Map 1) | loading screen, then the HUD: portrait and 500 points bottom left, crosshair, ammo 8 / 32 "MR6" with the grenade icon bottom right; view on the shelves; the MR6 (its part replica: the FBX packs don't load in Studio offline) held by the yellow right arm from the bottom right. Viewmodel at render time: MR6 0.90–1.50 studs ahead, 0.07–0.45 below the eye; right arm at (0.34, −0.54, −0.32) from the camera | the same loading screen, HUD and view; the MR6 drawn from its imported FBX meshes in the same place, 0.94–1.50 ahead and 0.06–0.42 below; right arm at (0.34, −0.54, −0.32) in the same yellow | an offline Studio 0-20261101-105553-348.png plus a render-step probe vs `--luau client vm.luau@101 --shot-after 104` |

## GUI (ScreenGui, SurfaceGui, BillboardGui)

| Value | Setting | Source |
|---|---|---|
| Which GUIs | while playing: the LocalPlayer's PlayerGui (ScreenGuis, and SurfaceGuis / BillboardGuis with an Adornee) plus SurfaceGuis / BillboardGuis in the Workspace; while editing: StarterGui's ScreenGuis (StarterGui.ShowDevelopmentGui) and the Workspace ones. Enabled = false hides one | Roblox docs; Studio shows StarterGui in the edit view |
| Top bar inset | 58 px: a ScreenGui without IgnoreGuiInset starts 58 px down and is 58 px shorter; GUI coordinates (AbsolutePosition, InputObject.Position) count from below it, so an IgnoreGuiInset ScreenGui sits at (0, −58) | Roblox Studio (offline Studio, 863×675 view): GuiService:GetGuiInset() = (0, 58); ScreenGui AbsoluteSize 863×617 at (0, 0), or 863×675 at (0, −58) with IgnoreGuiInset |
| Draw order | 3D GUIs first (far to near, AlwaysOnTop ones after), then ScreenGuis by DisplayOrder; inside a GUI, ZIndexBehavior Sibling sorts siblings by ZIndex with children over their parent, Global sorts everything by ZIndex | Roblox docs |
| Layout | Size / Position as UDim2 of the parent (inside its UIPadding), AnchorPoint, AutomaticSize (X / Y / XY: grows to the text or the children plus padding), UIListLayout (FillDirection, Padding, Horizontal/VerticalAlignment, SortOrder Name / LayoutOrder), UIGridLayout (CellSize, CellPadding, FillDirectionMaxCells), UIScale (the object and everything inside grows around its anchor), UIAspectRatioConstraint (FitWithinMaxSize / ScaleWithParentSize, DominantAxis), UISizeConstraint, UITextSizeConstraint | Roblox docs |
| Look | BackgroundColor3 / BackgroundTransparency, BorderSizePixel outside the box, UICorner (CornerRadius scale of the shorter side), UIGradient (Color / Transparency sequences, Rotation, Offset) on backgrounds and images, UIStroke (Border round the box, Contextual round the text), Rotation (children turn too; ClipsDescendants is off for turned objects), CanvasGroup (GroupTransparency, GroupColor3), Visible | Roblox docs |
| Images | ImageLabel / ImageButton: Image (local asset store, upload ledger, cache, content folder; Decal assets point at their image), ImageColor3, ImageTransparency, ImageRectOffset / ImageRectSize, ScaleType Stretch / Slice (SliceCenter, SliceScale) / Tile (TileSize) / Fit / Crop | Roblox docs |
| Fonts | Studio's own font files: Font / FontFace families from the content folder's fonts/families/*.json, the nearest weight on disk when a face isn't (Studio's offline warnings name the same missing faces); Enum.Font mapped as Font.fromEnum does (Gotham → Montserrat) | Roblox docs; Studio content folder |
| Text size | a line is TextSize tall: font em = TextSize × 1.04 ÷ the font's line height per em | Roblox Studio (offline Studio): TextService:GetTextSize("START GAME", n, Oswald) = 46 / 58 / 78 / 104 px wide at 14 / 18 / 24 / 32, height = TextSize |
| Text layout | TextXAlignment / TextYAlignment, TextWrapped, TextScaled (largest size ≤ 100 that fits, or MaxTextSize), TextTruncate (…), LineHeight, TextTransparency, TextStrokeColor3 / Transparency (1 px outline), RichText (b, i, u, s, font color / size / transparency / weight, br, uppercase / smallcaps, XML escapes); TextBounds is the drawn text's size | Roblox docs |
| SurfaceGui | on the Adornee's (or parent part's) Face, CanvasSize (FixedSize) or PixelsPerStud, drawn in perspective (triangles split to ~40 px on screen), only from in front of the face, MaxDistance; LightInfluence and depth against the world are not done (drawn over the 3D view) | Roblox docs; Place A's lobby menu panels |
| BillboardGui | over a part, attachment or model's centre, StudsOffset (camera space), StudsOffsetWorldSpace, Size scale in studs at that distance plus pixels, MaxDistance | Roblox docs |
| ViewportFrame | its parts (boxes; MeshParts thinned to ~3 000 triangles) drawn flat-shaded from its CurrentCamera with Ambient, LightColor and LightDirection, back to front | Roblox docs; shading estimated |
| ScrollingFrame | CanvasSize, CanvasPosition (the wheel scrolls it 100 px per notch: ScrollWheelDeltaAmount, table at the top), scroll bar ScrollBarThickness / ScrollBarImageColor3 / Transparency, always clips | Roblox docs |
| Input | MouseEnter / MouseLeave / MouseMoved, InputBegan / InputEnded (InputObject with UserInputType, UserInputState, Position) on every object under the mouse; the topmost button / TextBox / Active object takes the click: MouseButton1Down / Up / Click, MouseButton2…, Activated(input, 1); a click the GUI takes doesn't reach the world, and UserInputService.InputBegan gets gameProcessedEvent = true | Roblox docs |
| Buttons | AutoButtonColor darkens the background 30% under the mouse and 50% while pressed | estimated |
| TextBox | a click focuses it (Focused; caret blinks), typing edits Text, Backspace deletes, Enter → FocusLost(true), Escape or a click elsewhere → FocusLost(false); while one has the keyboard, keys don't reach the game or the camera | Roblox docs |
| Selection | GuiService.SelectedObject gets a 2 px outline; arrow keys move it to NextSelectionUp/Down/Left/Right or the nearest Selectable button / TextBox that way; Enter activates it | Roblox docs; outline colour estimated |
| While editing | clicking a GUI object in the view selects it in the Explorer, as Studio does | Roblox Studio |
| AbsolutePosition / AbsoluteSize / AbsoluteRotation | where the object was drawn last frame (before that: from its UDim2s) | Roblox docs |
| Cost | Place A's lobby (17 GUIs, several thousand objects) takes ~2.6 ms a frame | measured (`--bench`, load average ~10) |

## FBX models (InsertService:LoadAsset)

| Value | Setting | Source |
|---|---|---|
| Where | an asset id whose file in the local asset store (`<id>.fbx`) or the rbxasset upload ledger is an FBX; read only and imported in memory (meshes kept as rbxtemp://rustudio/fbx/<id>/<n>); nothing is written to the store or the ledger | person's rule (the offline Studio and rbxasset folders are shared) |
| What comes out | a Model holding one MeshPart per mesh node, named after the node; empty nodes become Models and children nest under them; MeshParts are re-centred on their own bounds (CFrame at the centre, Size = bounds) and anchored; a material's DiffuseColor becomes Color, an embedded texture the TextureID | Roblox's FBX import; Place A' loaders keep the MeshParts by name |
| Units and axes | node transforms (translation, pre/post rotation, XYZ rotation, scaling, pivots) and GlobalSettings UnitScaleFactor applied; 1 metre (100 cm) = 1 stud; then turned 180° about Y | Place A' Shared/GunMeshes records the uploaded meshes' sizes and centres "turned 180 degrees about Y" from gun space; the sample pack imports to exactly those (unit test) |
| Formats | binary FBX 7.x (zlib-compressed arrays included); ASCII FBX gives a clear error | FBX file layout (community docs) |

## Spatial queries

| Value | Setting | Source |
|---|---|---|
| Raycast, GetPartBoundsInBox / InRadius, GetPartsInPart, FindPartsInRegion3 | a 16-stud grid over the Workspace's parts, kept up to date from the document's change journal; rays walk it 64 studs at a time; parts covering more than 512 cells, and boxes over 4 096 cells, fall back to testing every part | estimated; Place A's nav grid (tens of thousands of include-list rays) now builds in 40 s, Studio's own time, instead of minutes |

| Ray starting inside a part | a ray that starts inside a part's box doesn't hit that part | Roblox docs (raycasting); Place A's gun-near-a-wall check had pulled the gun back because its ray started inside a big collision part |
| MeshParts / unions | queried by Studio's stored collision pieces (PhysicalConfigData, `CSGPHS` v8: convex hulls split by an Edgebreaker CLERS stream, plus explicit triangle pieces; src/physics_mesh.rs), stretched by Size / InitialSize; the blob also gives CollisionFidelity (type byte 1 Hull, 2 Default, 3 Precise, `BLOCK` Box: the file has no such property). Rays meet the pieces as stored; a ray starting inside any piece (or on a solid piece's face) skips the whole part; flat pieces are hit but never contain the origin. Spherecast / Blockcast / Shapecast meet the pieces scaled towards the part's centre by (half − 0.05) / half per axis, and skip only the pieces they start in. Box fidelity is the box. Without a blob: the mesh's own triangles (CollisionFidelity other than Box) | Studio encoder Studio / decoder Studio / CLERS writer Studio; Studio probes (checks/laser-tag-parity/raycast); Place A's "Terrain" collision meshes for the blob-less case |
| CanQuery | only counts once CanCollide is off: a CanCollide part is queried whatever its CanQuery | Studio: Laser Tag's spawn forcefields and containment walls (CanCollide, CanQuery false) are hit |
| Box contacts | rays: a ray that only touches an edge or corner (no length inside) misses; the normal is the face entered, or the faces entered together summed at an edge / corner ((-1, -1, 1) / √3 into a corner); a ray along a face's plane enters through the face ahead. Sweeps against boxes are exact (sphere: root of the centre-to-box distance; box: moving separating axes) | Studio probes; parry's GJK cast was up to 0.29 studs short on Laser Tag's 381-stud FLOOR |
| CollisionGroup | RaycastParams / OverlapParams.CollisionGroup skips parts whose CollisionGroup was set not to collide with it (PhysicsService:CollisionGroupSetCollidable on that side) | Roblox docs; Place A's "WeaponRay" group |

## Content folder

| Value | Setting | Source |
|---|---|---|
| rbxasset:// | read at run time from RUSTUDIO_CONTENT_DIR or ~/.local/share/rustudio/content (which the person may point at their own Studio install); nothing from it is copied, cached or shipped | person's request |
| Formats | .mesh (text 1.00/1.01 and binary 2.00–7.00), PNG, JPEG, DDS, and Ogg / MP3 / WAV / FLAC sounds | community docs |
| Tests | the self-test writes its own generated mesh, PNG and WAV into a temporary content folder | no Studio files needed |

## Harness

| Value | Number | Source |
|---|---|---|
| Frames before the screenshot | 120 (or `--shot-after S` seconds, which also saves FILE.view.png, just the 3D view) | Craft Anything requirement |
| Timed input | `--click X,Y@S` (3D view pixels), `--key NAME@S`, `--luau server|client FILE@S` (runs Luau on that side of Play, like the offline Studio's run_luau), `--log FILE` (Output), `--window WxH` (1148×1104 gives Studio's 863×675 view); `--play` starts once the view has its size | test harness |
| Test runs | --screenshot, --self-test and --bench run inside a headless gamescope (RUSTUDIO_VISIBLE_TESTS=1 shows them) | person's request |
| --bench | 3 s warm-up, then N s (default 10) without vsync at 1265×900; edit mode frames the whole place and turns the camera 36°/s | estimated |
| Lighting checks | RUSTUDIO_NO_GRID=1 hides the editor grid (Studio's test views have none); RUSTUDIO_SHADE_DEBUG=normals, albedo or uv draws that instead of light; RUSTUDIO_LIGHT_DEBUG=1 prints the nearest lights sent to the shader | test harness |

## Round 12: engine (Place A playable, sounds, API gaps)

### Sounds

| Rule | Roblox | rustudio |
|---|---|---|
| PlaybackRegionsEnabled | the sound plays PlaybackRegion (Min..Max) of its asset; looped, it wraps over LoopRegion (else PlaybackRegion) | `audio::Region`: the source starts at Min (unless TimePosition is inside), stops at Max, wraps over the loop span; TimePosition / Ended / DidLoop follow the same spans. Place A's SoundBank keeps many clips in one sheet: without this every clip played the whole sheet |
| A Sound leaving the DataModel | stops (Destroy, Debris, Parent = nil) | each frame, a track whose Sound no longer descends from the DataModel on either side is stopped |
| `Sound.Playing` | idempotent: setting the value it has does nothing; false pauses (TimePosition kept), true goes on | as Roblox; `Play()` still restarts |
| `IsPaused` | `not Playing` | `not` the playing set |
| Autoplay at Play start | sounds saved with Playing = true start once | the scripts' first-run `Play()` had already queued them: autoplay skips those (each started twice) |
| An unavailable SoundId | a warning; the game goes on | one warning; the id isn't fetched again this session |

### From Studio (Studio, round 12)

| Member | Studio's code | Rule | rustudio |
|---|---|---|---|
| Sound.IsPaused | getter: `return Sound == 0` | the same byte Playing and IsPlaying read: **IsPaused = not Playing**, so a sound that never played or has ended reads IsPaused true | restored (round 12 had briefly made it "only after Pause"; AUDIO-BUG.md's "Defect 1" was Studio's real behaviour) |
| Sound.Playing (set) | ; `cmp al,; je return` | setting the value it already has does nothing; otherwise stores it and calls the sound's play-state hook | idempotent, as implemented |
| Sound.PlaybackRegion / LoopRegion (set) |; setters | no clamping: if Min or Max changed the NumberRange is stored and the region hook runs; PlaybackRegionsEnabled's setter is | regions stored as given; the player clamps an empty or reversed range to "no region" |
| WorldRoot:Spherecast | (position, radius, direction, params) → → checks | position, radius and direction must be finite ("Invalid position/Radius/direction. Must contain finite values."); radius ≥ 0 ("Invalid Radius. Must be greater than or equal to 0, got %f."); `radius + radius ≤ ShapecastMaxSize` else "Attempt to spherecast with radius %f. The maximum radius is %f."; `|direction| ≤ ShapecastMaxDistance` else "Attempt to shapecast with distance %f. The maximum distance is %d." | as Studio (`SHAPECAST_MAX_SIZE`, `SHAPECAST_MAX_DISTANCE`) |
| WorldRoot:Blockcast | (cframe, size, direction, params) →; check | side length ≤ ShapecastMaxSize else "Attempt to blockcast with side length %f. The maximum side length is %f." | as Studio |
| WorldRoot:Shapecast | | not Terrain ("Shapecasted part cannot be Terrain."); side ≤ ShapecastMaxSize; distance ≤ ShapecastMaxDistance | as Studio |
| FInt ShapecastMaxSize / ShapecastMaxDistance |; values = 512, = 1024 | | `mech::SHAPECAST_MAX_SIZE`, `SHAPECAST_MAX_DISTANCE` |
| GetMaxCollisionGroups |: `return 0x20` | 32 | 32 |

### Spatial queries (the Place A match: enemies never spawned)

| Query | Roblox | rustudio |
|---|---|---|
| Raycast normal on a MeshPart | the hit triangle's surface normal | `assets::ray_triangles_normal`, to world by the inverse-transpose of the mesh scale (it was the bounding box face's: Normal.Y = 0 on Map 1's baked terrain, so Place A's nav grid found no floor outside and no spawn reached a window) |
| Raycast on a Ball / Cylinder / Wedge / CornerWedge | the true shape: a ray down at x = 1 onto a 4-stud ball at y 110 hits (1, 111.732, 0) n (0.5, 0.866, 0) d 8.268; a WedgePart's slope (normal (0, 0.707, -0.707), rising toward +Z) at its centre height | `query_shapes::ray_part`: sphere / X-axis cylinder (radius = min of the Y, Z half sizes) / convex faces in unit-cube space, normal by the inverse-transpose of the size (it was the box's top face: wedge ramps read as 4-stud steps) |
| GetPartsInPart | exact geometry | `query_shapes`: box vs box by separating axes (15), box vs mesh triangles (13 axes each) or wholly inside (ray parity); a ball / cylinder / wedge / corner wedge on either side by parry's intersection test on its true shape (a part in a ball's box corner, 0.2 studs off the sphere, is no longer listed) |
| GetPartBoundsInBox / InRadius | bounding boxes | unchanged (bounds, as documented) |
| Spherecast / Blockcast / Shapecast | the shape swept along the direction; parts it starts inside are ignored | parts narrowed by their box grown by the shape's half extents, then the true probe shape (sphere, turned block, the Shapecast part's shape) swept against each part's true shape (`query_shapes::sweep_part`, parry shape cast); Position = where the shape touches, Normal the part's surface normal there |

### Events now fired

PlayerRemoving (on Stop, both sides, with one dispatch so handlers run),
CharacterRemoving (before respawn / LoadCharacter removes the old one),
ScriptContext.Error (message, stack, script), UserInputService
TextBoxFocused / TextBoxFocusReleased / WindowFocused / WindowFocusReleased /
LastInputTypeChanged, Humanoid.PlatformStanding.

### Render and determinism

| What | Rule | Evidence |
|---|---|---|
| Default glow | without a BloomEffect, the bloom pass runs with BloomEffect's defaults (`DEFAULT_GLOW_*`: Intensity 1, Size 24 px, Threshold 2) | Studio's control shot glows ~12 px around a 1-stud Neon bar in a place with no post effects; rustudio's halo now within ~8 levels |
| os.clock | seconds since the process started (Studio: 2633 s into a long session), not the machine's uptime (Luau's built-in on Linux); `RUSTUDIO_CLOCK_BASE` (harness only) = Studio's reading at Play + time() | Place A's menu camera, which sways on os.clock, now matches Studio's to 0.15 studs, 0° |
| Coplanar faces | the per-part depth nudge comes from the part's size and position, not its Ref (random per run) | the self-test's night scene read either of two coplanar floors from run to run (8 levels apart) |
| Lighting self-test | material packs off for the lighting steps (`matpack::set_enabled`) | their Studio references were captured offline in an offline Studio, which can't download packs; rustudio passed only while its packs hadn't arrived |

### Gap telemetry and coverage

`RUSTUDIO_GAPS=FILE` (src/telemetry.rs) records, against app/api-dump.json,
every touch of a real member rustudio lacks (missing, call-unimplemented,
default-only run-time properties, write-ignored, event-unwired), with the
scripts. build.rs scans the engine's sources for the properties it reads or
writes (`prop(…, "X")`, `ustr("X")`) and the event names it mentions outside
the `event_exists` declaration lists. `--api-coverage` measures the coverage
table (README; ENGINE-GAPS.md has the method and the ranked list).

### Place A gate (self-test `fidelity-place_a`)

Place A in its own process from the self-test's first frame (alongside every
other step: the self-test has 90 s in all): the lobby at 10 s against
Studio's shot (max_mae 34), then START GAME (110, 143) at 12 s, W held at
54 s (3 s) and 62 s (2 s), a click at the view's centre every second from
44 s, Output kept to 74 s. tools/fidelity/play/place-a-server.luau waits for
round 1 and watches `WATCH` seconds (18 here: enemies alive at peak, round,
downed); place-a-client.luau records the walk and the HUD's clip count. Fails on
any Output error, an empty view, the pixel gate, or a match that doesn't
start, spawn a enemy, let the player walk `PLACE_A_GATE_MIN_MOVE` (2 studs) and
fire. The full 60 s of play: `tools/fidelity/play/place_a_match.py` (round 12:
round 1 active 37 s after the probe began, 6 enemies, walked 8.4 studs,
fired 11 shots, downed, 0 error lines).

## Round 13: humanoid states, collision groups, Place A as a game

### Collision groups (src/collision_groups.rs)

PhysicsService groups were a Luau table in each script VM, so the client
never saw the groups Place A's server registers: its weapon rays
(`CollisionGroup = "WeaponRay"`) stopped on the window PlayerClip brushes
and no shot ever hit a enemy. Now one table per Play session is shared by
the server and client runtimes and the physics: ray / overlap queries skip
uncollidable groups on both sides, every collider gets rapier interaction
groups from its part's CollisionGroup (Roblox's 32 groups = rapier's 32
bits: membership = its bit, filter = the groups it collides with), and the
character controller collides as its HumanoidRootPart's group.

### Scripting fixes

- `Instance:SetAttribute` with the value it already has stores nothing and
  fires nothing (Place A's Rounds set ZombiesLeft every 0.05 s: 2236 AttributeChanged
  in a minute before, as many as real changes now).
- A key pressed and released within one frame reaches UserInputService
  (InputBegan then InputEnded): harness taps and very quick key presses were lost.
- Lighting.ClockTime / TimeOfDay / SetMinutesAfterMidnight are one value
  (whole seconds in [0, 24]); ClockTime reads the saved TimeOfDay (was nil).

### Place A played as a game (`tools/fidelity/play/place_a_match.py`)

The client probe aims at the nearest enemy a WeaponRay can reach, bound
right after Place A's camera step (Camera + 1) and before its weapons step
(Camera + 2), so the harness's clicks fire at enemies. Round 13: round 1
active after 35 s, 6 kills, 1010 points, ZombiesLeft 6 → 1 and the round
ending in intermission within the 60 s watched, player not downed, 0
error lines. The gate now also needs ≥ 1 kill and points above the 500
start. Doors and wall buys (tag ZmBuy, hold F → `Buy` remote) and reload
(R, or automatic on an empty clip) work; the HUD shows points, weapon,
clip / reserve and the buy hints.

### Night lighting calibration (tools/fidelity/corpus/night.rbxlx)

Place A's chapel at run time (Shared/NightLighting: ClockTime 07:45, Brightness
0.35, Ambient (8, 9, 14), OutdoorAmbient (12, 15, 24), ColorShift_Top
(140, 175, 230), EnvironmentDiffuseScale 0.08, fog 40–400, GlobalShadows
off, Technology Voxel → Soft) in a roofed room with Place A's spot / point light
values, no spawn ForceField. Studio vs rustudio at 10 s: MAE 20.4. The sun
through the walls (GlobalShadows off) matches; the gap is Soft spot lights:
Studio spreads a downward SpotLight (Brightness 2, Range 32, Angle 60) over
the back wall 10 studs off its axis, rustudio keeps it inside the cone. The
captures are in `../checks/compare/night/` (a calibration, not in score.json).

Studio also draws the spawn ForceField bubble at 10 s in demo and control
(SpawnLocation.Duration 10); rustudio draws no ForceField yet.

### From Studio (Studio, round 13)

**Collision groups** (PhysicsService / WorldRoot):

| Rule | Function | rustudio |
|---|---|---|
| Name: empty → "Collision group requires a valid name."; "Default" → "Can not use/change the Default collision group."; over 100 bytes (0x64) → "Collision group name length cannot exceed 100 chars" | | `collision_groups::check_name` (Register, Unregister, Rename) |
| RegisterCollisionGroup: the name check, then ≥ 32 groups (Default counts) → "Cannot register more than 32 collision groups.", then a taken name → "Could not register collision group, the name already exists." | | `CollisionGroups::register` (was: silently ignored) |
| CollisionGroupSetCollidable: on a client while running → "This API can only be used on the server!"; either group unregistered → "Both collision groups must be registered." | | `__rs_cg("set")` |
| GetCollisionGroupId(name): unknown → "Collision group not found." | | `__rs_cg("id")` (was −1) |
| GetCollisionGroupName(id): id > 31 → "The Collision group Id must be between 0 and 32" | | `__rs_cg("name")` |

Proved in Play on the control place (server and client probes): each
message above comes back from the matching call.

**Seat weld** (, the function that names the weld from the
immediate "SeatWeld"; Part0 = seat, Part1 = the root, parent = seat):

- C0 = CFrame(0, Seat.Size.Y × 0.5, 0) and C1 = CFrame(0, y, 0), both with
  the basis built from (0, −1, 0) and (0, 0, −1), i.e.
  Angles(−π/2, 0, 0). 0.5 is.
- R6: y = −(max(root.Size.Y, 0.05) × 0.5 + 0.5) : −1.5 for the 2-stud root.
Measured:   −1.487 on rustudio's default R15 rig, root 1.99 over the seat's centre.
- rustudio builds the weld this way (library.luau `__rs_seat`) and places
  the seated root at seat · C0 · C1⁻¹ from the weld itself (play.rs), so a
  script editing the SeatWeld moves the sitter as in Roblox. (The old fixed
  1.5-stud offset was only the R6 case.)

**Ragdoll**:

- Constru: timer = FInt RagdollDefaultTimerTenthSecond /
  10 = 50 / 10 = 5.0 s (flag, its name stored
  after it; divisor 10.0).
Proved: StateChanged   Ragdoll → GettingUp → Running after ChangeState(Ragdoll).

**FallingDown / GettingUp** (state base starts
every state's timer at 0):

- RBX::HUMAN::FallingDown,: timer = 3.0
  (0x40400000), then GettingUp. rustudio: `FALLING_DOWN_S` 3.0 (was a 1 s
  estimate). Proved: StateChanged FallingDown@0.0 → GettingUp@3.0.
The controller's character never tips,   so rustudio leaves GettingUp on the next step (`GETTING_UP_S` 0).

**Climbing**:

- Target velocity (). With a ladder (FUN_Studio) and a move
  vector d: if d̂ · flat look ≥ −0.2 (Studio), or there is no floor
  (FUN_Studio = the floor sensor's part, flag CacheAndReduceCallsToFloor
  Studio only caches it), the target is (0, ±|d| × 0.7, 0)
  (Studio; 0.01 under |d| 0.1, Studio; −1 when
  moving away, Studio). Moving away *with* a floor keeps the move
  vector itself: the climber walks off the foot of the ladder. With a floor,
  the target is then bent along the floor normal (FUN_Studio = sensor
  ): Studio if normal.y ≥ cos(MaxSlopeAngle) (Studio), else
  Studio removes the into-floor part. Both leave a vertical target
  vertical. The physics is the Running/Climbing controller Studio
  (Studio + Studio), not reconstructed; its response was measured.
- Controller response, own hidden Studio pid 6184, R6, WalkSpeed 16, per
  1/240 s step: vertical closes 0.625 of the gap (re-grab from −0.8175:
  6.6934, then 10.5663, 11.1109 after two more steps each); horizontal the
  same but at most 3.0902 per step (walk-off 0 → 3.0902 → 9.2705 →
  14.6352). rustudio: `physics::climb_step`, `mech::climb_controller`,
  `CLIMB_GAIN_PER_STEP`, `CLIMB_HORIZONTAL_STEP_MAX`, `humanoid_steps`.
- FindLadder (classic, Studio; Studio is the
  ControllerManager sensor). Look = the root CFrame's look vector. Broad
  query: a world box centred reach × 0.75 (Studio) ahead, ± that in
  X and Z, from root.y − (height − 0.3) up to root.y + 1 (Studio).
  Its parts are the *include list* of every later ray (FUN_Studio).
  A down ray of height − 0.3 (only candidates) sets the drop; samples start
  at root.y − drop + step (index 1) every height/3 × 1/7 (Studio), up to index floor((height − 0.3)/step) + 10 (Studio),
  each a ray of reach × 1.5 (Studio) along the look. A TrussPart hit
  (virtual 0x2a8 == 3) succeeds at once; otherwise the rung pattern below
  2.45 × height/3 (Studio, /5 Studio). Reach R6 0.7 / R15 1.2
  (Studio) × head scale; height = half root Y + HipHeight (+ max leg Y
  for R6, Studio). Result stored at sensor  (part, point, hit
  normal) and kept between runs.
- Measured, pid 6184 (gravity 0, root Y 10): a 2-stud truss whose bottom is
  0.9 above the root climbs, 1.1 does not (box top, although samples reach
  +1.157). With a slab (top 9) under the root reaching into the box
  (Z −1..3), a truss spanning Y 6.9–8.9 is not found (the floor hit lifts
  the samples); the same slab ending at Z 0.1 is not a candidate and the
  truss climbs (root 10.378). Earlier: rung gaps 0.2–2 climb, 3–4 do not;
  truss face 1.0 from the root climbs, 1.1 does not.
- ClimbSensorUseLookAsNormal (registered Studio, value Studio;
  `game:GetFastFlag` → true) is read only in Studio (Studio): the ControllerManager sensor replaces its
  normal with the look. The classic Humanoid keeps the ray's hit normal.
- When FindLadder runs (Studio): only if the state's row of the
  transition table Studio (0x4c per state, 18 = none) has a ladder
  entry and Climbing is enabled — Running (found → Climbing), Freefall
  (→ Climbing), Climbing (lost → Running); Jumping and Landed have none —
  and then every vtable  updates: Climbing and Freefall 0
  (Studio), Running/Landed/Jumping 2 (Studio). rustudio:
  `play::ladder_interval`, `Character::ladder` / `ladder_wait`.
- Tilt. Own hidden Studio pid 6184, R6, 80-stud truss centred (0,40,0)
  rotated about X so it leans over the climber; 1 s averages: 0°
  (0,11.2000,0), 10° (0,10.7205,1.8894), 20° (0,10.4890,3.8164);
  instantaneous 5° (0,10.8239,0.9450), 15° (0,10.6114,2.8328); 30° did
  not climb. Root distance from the truss axis 1.603 / 1.778 / 2.088. Leaning
  away (−5, −10, −20°) the climber bounces off and on (Climbing ↔ Running),
  following the face on average (dz/dy −0.07, −0.14, −0.37). The path
  follows the face exactly (angle within 0.05°) while the speed settles 0.04
  to 0.33 under 11.2. rustudio: the target is the vertical climb speed turned
  into the plane of the sensor's hit normal, speed kept
  (`mech::climb_velocity`, a fit, not Studio code); rustudio 10°
  (0,11.0288,1.9508), 20° (0,10.5245,3.8306).
- Jumping (constructor Studio, previous state 12): impulse along
  normalize(up − flat look); update Studio tops the velocity along it
  up to 1.06 × jump velocity. Truss and Part ladder: (0,42.0197,32.4547).
- Re-grab trace, pid 6184 (root forced back to the rung on Freefall):
  Climbing → Jumping at a frame start, root velocity (0,11.2,0); the
  impulse holds (0,42.0197,32.4547) for two 1/240 s steps (a third step is
  gravity only: 41.2022); Jumping → Freefall at the next frame start;
  Freefall → Climbing after one step (vy −0.8175); then the 0.625 response.
  StateChanged fires at frame starts, so states change per frame while
  forces act per step. rustudio: Jumping cannot enter Climbing (its row has
  no ladder entry), so Climbing → Jumping → Freefall → Climbing takes three
  frames, as in Studio.
- Held Jump (2026-11-11). Table Studio, row = state + 1 (the reader
  Studio indexes `(state + 1) * 0x4c`), 19 columns: 0 ladder found,
  1 ladder lost, 3 no floor, 4 floor, 7 water, 12 Jump. Freefall
  [12,18,18,18,7,18,18,4,…, col 12 = 18]: ladder → Climbing, nothing on
  Jump. Climbing [18,8,12,12,12,18,18,4,18,·,15,15,3,…]: Jump → Jumping,
  ladder lost → Running. Running: ladder → Climbing and Jump → Jumping.
  Climbing's update Studio never reads Jump. So holding Jump does not
  stop a grab. Own hidden Studio (climb-r6/r15.rbxlx, Jump forced true
  every RenderStepped from 2.5 s while walking in): R6 truss Climbing →
  Jumping → Freefall → Climbing at 3.034, 3.500, 4.090, 4.590, 5.062,
  5.652 s; rungs and R15 the same. Running on a floor with Jump held jumps
  even next to a ladder (R15 rungs, 1.17 from the face: Running → Jumping).
  rustudio had climbed only with Jump up, so after one jump off it never
  grabbed again. Now `physics::climbs(climbing, on_floor, ladder, jump)` =
  ladder && !(jump && (climbing || on_floor)), used by play.rs.
- Distance from the ladder (2026-11-11). Nothing pulls the climber in:
  Climbing's target has no horizontal part, and the controller only brakes
  the walk-in (≈0.15 studs from 16). The root rests where that runs out
  after the sensor fires, or where the body box meets the ladder. Since
  Running runs FindLadder only every third update, the grab distance
  depends on timing, anywhere between the torso flush and the ray reach
  (reach × 1.5) minus the brake. Own hidden Studio, walking in at 16,
  root to face while climbing (constant over the whole climb): R6 truss
  0.770 (grabbed at 0.863), rungs 0.519 (torso flush, depth 1); R15 truss
  1.435, rungs 1.035. The 4-stud parts with 1-stud gaps ("Tall" lane) are
  only found at a gap (walked into, Running against a wall at 0.48); grabbed
  from a jump, 0.49–0.68. rustudio had moved the climber with the old
  radius-1 capsule (`move_character_floating`): root ≥ 1.0 off the face,
  and the round bottom rode out over each rung's lower edge (0.52 → 0.75
  within 4 studs on the Tall lane, more on long climbs). `climb_step` now
  moves the Humanoid body box (`hum_body_shape`, the one Running uses)
  each 1/240 s step with `hum_controller`. rustudio 60 fps walk-in: R6 0.809,
  R15 1.609 (Studio's range: 0.5–0.9 R6, 0.58–1.65 R15).
- Top lip, pid 6184, 20-stud truss (top Y 20): Climbing at root.Y 22.4788,
  released at 22.5721 (lowest sample root − 2.557 clears the top; rustudio
  threshold 22.557). Climbing → Running with (0,11.2,0); Running's one step
  (0,10.3825,−0.5958): gravity plus 0.5958/step = 143 studs/s² air steering
  (`HUMANOID_AIR_ACCEL`); Running → Freefall next frame; Freefall → Landed at
  22.8555 (0,−1.88,−9.533), then Landed (0,2.19,−14.73). No extra lip push.
- Descent, same truss, moving away: −11.2 until the floor sensor finds the
  floor between root.Y 4.148 and 4.055; then the walk target (vertical
  −4.2000, −0.5906, −0.0831; horizontal 3.0902, 9.2705, 14.6352);
  Climbing → Running at 4.0269 with (0,−0.0831,14.6352) once the ladder
  leaves the sensor (root 0.19 further out); then Running falls to rest.
  rustudio (R6 floor reach root − 4.2): walk-off from 4.157, released at
  4.145 with (0,−0.22,12.36); its capsule (radius 1) loses the 1.05 sample
  rays sooner than Studio's body.
- Tests: `ladder_floor_adjusts_scan_origin`,
  `ladder_query_box_ends_above_root`, `climbing_tilted_truss_matches_studio`,
  `climbing_regrab_steps_match_studio`, `climbing_top_release_matches_studio`,
  `climbing_descent_walks_off_like_studio`, `humanoid_ladder_jump_impulse`,
  `humanoid_part_ladder_sensor`, `ladder_scan_uses_rig_height`,
  `ladder_top_releases_when_lowest_sample_clears`,
  `climbing_tilt_velocity_uses_contact_plane`,
  `climbing_held_jump_regrabs_like_studio`, `climbing_hugs_ladder_like_studio`,
  `climbing_tall_part_ladder_stays_close`.
- Ledges (2026-11-11). Studio has no ledge climb for a wall that reaches
  the floor. Studio, rung pattern: a sample counts as a
  hit only if the hit point is within reach (R6 0.7, R15 1.2) of the
  ray's start; the ray itself is reach × 1.5 long, and that length only
  matters for a TrussPart, which succeeds on any hit. `first` is the
  first hit's height up the scan and `run` how far the hits go before the
  first miss. `second` is set by a hit after that miss. The scan fails if
  first ≥ 2.45 × h/3, run ≤ 0, run ≥ 2.45 × h/3, or (no second and
  first ≤ 2.45 × h/3 / 5; 0.49 for h = 3). A block on the floor is hit
  from the first sample, h/21 up, with nothing after it, so it fails. A
  part that is a ladder needs room under it, or a gap and another part
  above. The sample reach only matters for rungs set in front of a wall.
  Own hidden Studio, ledge-r6/r15.rbxlx (scratch places: 8 × h × 6 blocks
  with their faces 6 ahead, walking at 16 with Humanoid:Move from a
  LocalScript), ~110 fps with 3D off:
  - Blocks of 2.5, 3, 3.5, 4, 4.5, 5, 6 and 8 studs, R6 and R15: Running
    throughout, root at rest height (R6 3.000, R15 3.192) with the torso
    against the face (z −5.53 / −5.48 for a face at −6). A 2-stud block is
    stepped onto, then the character runs off its far side.
  - 1-stud table top at 2–3, and a single 0.4 bar at 2.6–3.0: Climbing for
    ~0.12 s (table) or ~0.18 s (bar), then Running onto the top (root
    6.000 / 6.192), across it and off the far side.
  - 2-stud shelves at 1–3, 1.5–3.5 and 0.8–2.8 (nothing under them): the
    character is found, climbs 0.2–0.8, then loses the pattern because the
    run now starts under 0.49. It flickers Climbing ↔ Running against the
    face and never gets on: R6 shelf 1–3 made 40+ switches in 2.6 s and
    peaked at root 3.58 (R15 3.67); shelf 1.5–3.5 peaked at R6 4.42,
    R15 4.14. On the loaded machine at ~15 fps, Studio's longer frames
    climb further per frame (the sensor runs once a frame while the
    controller steps at 240 Hz), so it did get on (R6 1.5–3.5: one climb
    3.0 → 4.465 over two frames). That outcome depends on frame rate in
    Studio too. A 3-stud shelf (1–4) is not a ladder (run ≥ 2.45).
  - Tops, 12-stud truss or rungs (2 × 0.4 × 1 every stud, top 12), on
    their own or against a 10 × 12 × 10 platform. Climbing lasts R6 truss
    1.042 / 1.050 s, rungs 0.954 / 0.963 s; R15 1.050 / 1.041 and
    0.970 / 0.971. The first Running frame is at root 14.62–14.92 (truss)
    or 13.94–14.04 (rungs). Most runs then show Freefall → Landed within
    0.1 s (R6 truss with platform stayed Running), standing on top at R6
    15.000 / R15 15.192 (top 12 + rest height) 0.1–0.2 s after letting go.
    From there it runs on across the top, off the column's far side or
    along the platform. It never slides back down the face or hangs at
    the lip. Nothing pushes it over: the vertical climb speed it keeps,
    plus walking on, carries it (Climbing's update Studio has no
    exit impulse).
  rustudio: the same lanes in the real app (hidden `--play` runs of the
  same places, the LocalScript using `time()` because rustudio's
  DistributedGameTime stays 0), and in the test harness at 30–240 fps,
  give the same states and end positions: no block climbs; the table and
  the bar get climbed; the shelves flicker; truss and rung tops end on top
  (R6 climbs 1.038 / 1.050 s for truss and 0.98 s for rungs, R15 1.05 /
  0.98). The only difference is at the R6 lip, where rustudio's floor rays
  find the top on the first Running frame, so it goes straight to Running
  with no Freefall → Landed. Tests: `walking_into_a_block_never_climbs_like_studio`,
  `overhang_ledges_climb_like_studio`, `ladder_tops_carry_over_like_studio`,
  `rung_ladder_top_release_matches_studio` (rungs are kept up to root
  13.871; Studio's first Running frame was 13.942 / 13.992). They pass on
  the reworked climbing code without changes. Mutations show they catch
  regressions: dropping the 0.49 single-run rule makes the 2.5-stud block
  climb; zeroing the climb velocity on letting go means the character never
  gets onto the bar.
- Open: the controller Studio itself (why tilted speed settles lower),
  R15 response constants (only R6 measured), the floor sensor's exact reach
  and caching while climbing.

**Swimming**:

- Desired velocity = the move vector v; when its horizontal length is over
  2 × |v.y| it swims flat with v's full length.
So   the velocity closes on the move vector at 35 /s and Jump settles at   500 / 35 ≈ 14.3 studs/s up. rustudio: `SWIM_GAIN`, `SWIM_RISE_SPEED`   (was 8, estimated). The idle float is the water's buoyancy, not this   state: `SWIM_FLOAT_SPEED` stays an estimate.

**Attributes**:

- Empty → "Attribute name cannot be empty"; over 100 bytes (0x64) →
  "Attribute name exceeds 100 character limit"; any byte that isn't
  alphanumeric (isalnum), '_' or '-' '.' '/' (c − 0x2d ≤ 2) → "Attribute
  name contains illegal character '%c'". rustudio: `check_attribute_name`
  (runtime.rs), `ATTRIBUTE_NAME_MAX`. Proved with "", "a b", 101 × "x"
  and "ok_-./9".

Seated was located; the re-sit cooldown wasn't found
in it, so `SEAT_COOLDOWN_S` stays an estimate.

## Round 14: API surface (from Studio)

**Member access messages** (Luau instance bridge, Studio; strings at
…). The stubs (src/stubs.rs) and the runtime
use Studio's own wording:

- `%s is not a valid member of %s "%s"` (, raised from
 and five other sites) — a name the dump doesn't have.
- `Unable to assign property %s. Property is read only` (,
) — a read-only stub or property.
- `%s is a callback member of %s; you can only set the callback value, get
  is not available` (, raised by the __index path at
): **reading any callback errors, set or not.**
  rustudio did answer callback reads (the stored function, or nil); now
  `index` refuses for every member the dump marks Callback, and the engine's
  own Invoke code reads them through `__rs_get_callback`. The coverage probe
  counts that error as "exists"; only OnInvoke / OnServerInvoke /
  OnClientInvoke (which rustudio calls) count as "real".
- Also in the bridge, not yet used by rustudio's messages: `Unable to assign
  property %s. It is not scriptable`, `… %s
  expected, got %s`, `… EnumItem of type %s expected, got an
  EnumItem of type %s`.

**Model.WorldPivot** (registration, category "Pivot"):

The box takes each   part's half size through the absolute   rotation matrix: the parts' real extents, not   their centres. rustudio's `model_pivot` fallback did the centres only;   now the extents (two parts 10 and 2 studs long at x 0 and 10: pivot x 3,   was 5). (With a PrimaryPart rustudio keeps the documented   PrimaryPart.CFrame · PivotOffset.)
- Setter: records an undo step when the value changes , applies it through the model's virtual setter, then   raises the property-changed signal for WorldPivot (descriptor ). rustudio now fires Changed("WorldPivot") /   GetPropertyChangedSignal("WorldPivot") after storing it (PrimaryPart's   PivotOffset, else WorldPivotData).

**UserInputService:GetGamepadConnected** (registration, implementation): the UserInputType is looked up (FNV-1a over its 4 bytes: basis 0xcbf29ce484222325, prime 0x100000001b3) in the service's gamepad table; found → that entry's connected byte, not found → false. No error for non-gamepad values. rustudio: Gamepad1…8 against the pads its device scan saw, else false.

**Tween.PlaybackState** (registration, category "Behavior"):
a plain field read; its values come from the tween's
run (library.luau `run_tween`: Begin, Delayed while DelayTime runs,
Playing, Paused, Completed, Cancelled).

## Round 16: Studio's window, measured in an offline Studio

| What | Studio | rustudio | How it was measured |
|---|---|---|---|
| Q / E fly direction | along the camera's own up: E held 600 ms with the camera pitched down moved it along CFrame.UpVector (0, 0.914, −0.406) exactly | `camera.rs` `fly` uses `Camera::up()` | a Studio MCP bridge `key e` on the viewport, positions read with `run_luau` |
| Fly keys | WASD / Q / E work with the mouse anywhere | anywhere but a focused text field / script editor / game TextBox / Play input | Studio behaviour; self-test `camera-keys-anywhere` |
| Explorer sibling order | ReflectionMetadata ExplorerOrder, then name A–Z (Camera, Camera, Terrain, SpawnLocation, Ball, Baseplate, Blue…) | `explorer.rs` `sorted_children` with `studio_panels::explorer_order` | Workspace expanded in Studio vs `workspace:GetChildren()` order |
| Explorer start state | every service collapsed | the same | screenshot |
| Menus | items, order and shortcuts as in checks/ui/studio/menu-*.png; File menu 233 px; shortcuts left-aligned after the longest label | `studio_ui.rs` (contents), `studio_actions.rs` `menu_metrics` | menu popups screenshotted by window id; `list_actions` for texts and shortcuts |
| Test drop-down | Test, Test Here, Run, Server & Clients | the same (Server & Clients disabled) | popup screenshot |
| Account menu | Add Account, Log Out | the same | popup screenshot |
| Ctrl+V / Ctrl+Shift+V | paste / paste into | egui-winit turns both into a Paste event, and only when the system clipboard holds text: copy now writes a line there, and Shift held at the Paste means Paste Into | egui-winit 0.36.2 `is_paste_command` |

### Round 16, read from Studio

| What | Studio's code | rustudio |
|---|---|---|
| Fly direction | : the key vector at [rsi] is multiplied by the camera's 3×3 rotation loaded from,,; the look column is negated with the sign mask. The y (Q/E) term uses the rotation's up column, so Q/E move along the camera's own up axis. The step is × Camera Speed, and × Camera Shift Speed while Shift is held unless and a setting at == 1 both hold — that exception is not identified yet | `camera.rs` `fly`: normalise the key vector, then right·x + up·y + forward·z (the same orthonormal basis); measured in an offline Studio: E moved along CFrame.UpVector exactly |
| StarterPlayer.GameSettingsAvatar |  | Avatar Settings (`studio_panels.rs` `set_avatar_type`): an unchanged value makes no edit and no undo step |
| ReflectionMetadataClass.ExplorerOrder | (int, category "Reflection") | `explorer.rs` `sorted_children` reads it from Studio's ReflectionMetadata.xml; then name A–Z, as measured |
| Test Here | the action SimulationPlayFromCameraAction is declared (name, title key Studio.TestingControls.Title.TestHere, tooltip key, mode string "PlayHere", whose only code reference registers a "RunTools" flag). | `start_run(TestHere)` |
| Paste Into / at Original Location | the action names are (action tables); register the PasteDataModelActions flags (owners "Common", "BuilderTools"), not the paste itself | keys from Studio's own action list (`list_actions`) |

## Round 17: drag, box select, Output and Properties, measured in an offline Studio

Measured over a Studio MCP bridge in the hidden Studio (1280×891, the control place);
pictures in `checks/ui/studio/`.

| What | Studio (measured) | rustudio |
|---|---|---|
| Part drag, snap off | a 4-stud cube at (−12, 2, −16), pressed and dragged with the Select tool, slid over the baseplate to (−9.33, 2, −1.98): it stays resting on the surface under the cursor (Y = 2 = half its height), keeps the grab offset along the surface | `part_drag.rs`: centre = surface hit − grab offset (in the surface plane) + normal × half-extent along the normal |
| Part drag, 1-stud snap | the same drag ends at (−7, 2, −2): the axes along the surface snap to the Move increment, the axis along the normal doesn't | axes with \|n\| < 0.9 snap to `move_inc` when snapping is on |
| Drag onto a part | lands on top of it (the surface under the cursor is the other part's) | `drag_surface`: the nearest part under the ray, the dragged parts skipped; else the ground plane Y = 0 |
| Undo | one Undo returns the part | one "Drag" step (`record_props`) for the whole selection |
| Box selection | parts highlight while the box is dragged | the selection is recomputed every frame from the box (`marquee_pick`) |
| Output colours | print (183, 184, 194), warn (245, 118, 48) bold, error (231, 87, 80) bold, info / stack (139, 175, 248); server bar (0, 204, 103), client bar (52, 154, 213); 15 px line pitch | `output_panel.rs` constants |
| Output line | "message - Script:line" (the source's last name); error then "Stack Begin", "Script 'Full.Name', Line N - Script:N", "Stack End"; timestamp "HH:MM:SS.mmm  " (local), off by default; Show Context off, Show Source on | `output_panel.rs` `parse` / `line_job`; `runtime.rs` `report` adds the stack lines |
| Properties metrics | rows 21 px + 1 px line, category band 25 px, name indent 25 px, divider at 195 / 407 px (48 %), text (188, 190, 200), read-only (73, 77, 90), bands (32, 34, 39), background (25, 26, 31), unticked box (40, 41, 49) | `properties.rs` constants |
| Properties order | category order Appearance, Data, Transform, Pivot, Behavior, Collision, Part, Assembly, Surface Inputs, Surface, others A–Z, Permissions; within a category ReflectionMetadata PropertyOrder then name A–Z ignoring case | `props_layout.rs` (unit test against all five captures) |
| Part mass rows | Mass 6.72 for 4 × 1.2 × 2 Plastic (density 0.7); AssemblyMass "inf" when anchored; AssemblyRootPart the part; AssemblyCenterOfMass its position | `properties.rs` `prop_row` |

### Round 17, from Studio

Read from `~/.local/share/rustudio/studio/Studio` (sha256 81be5bc2…, an offline Studio) and from the DraggerFramework inside `BuiltInPlugins/Optimized_Embedded_Signature/SelectDragger.rbxm`. Theme colours were confirmed by asking the hidden offline Studio (`settings().Studio.Theme:GetColor`).

| What | Studio's code | rustudio |
|---|---|---|
| Output colour per message type | builds a list of theme colours indexed by message type: 0 Log → StudioStyleGuideColor 37 MainText (188, 190, 200), 1 Information → 45 InfoText (143, 180, 255), 2 Warning → 43 WarningText (245, 118, 48), 3 Error → 44 ErrorText (231, 87, 80), 4 System → 46 SensitiveText (223, 106, 247); sets it as the foreground (QTextFormat property 0x821) and FontWeight (0x2003) 63 (DemiBold) for types above Information, else 50 | `output_panel.rs` PRINT / INFO / WARN / ERROR; warnings and errors in Builder Mono Bold (no semi-bold face ships) |
| Output type names |: 0 "Log", 1 "Information", 2 "Warning", 3 "Error", 4 "System" (Studio.App.RobloxOutputRibbonDropdown.*) | kinds Print / Info / Warn / Error |
| Context bar |: context 1 (client) → `<span style="background:#349AD5">&nbsp;</span>`, 2 (server) → `#00CC67`, else `&nbsp;`; then one more `&nbsp;` | the bar is one monospace space wide, then one space |
| Timestamp |: `QTime::toString("hh:mm:ss.zzz")` of the message's time (local) | `app.rs` `timestamp` in local time (libc `localtime_r`) |
| Stack after an error |: "Stack Begin"; per frame (0xa0 bytes each; frames with line < 0 skipped) "Script '{}', Line {} - function {}" when the frame has a function name, else "Script '{}', Line {}"; "Stack End"; posted as MessageInfo (blue) | `runtime.rs` `report_frames`, with the frames from Luau's own `debug.traceback(thread)` (C frames and rustudio's own chunks left out); unit test `error_stack_lines` |
| Box select | DraggerSchemaCore.beginBoxSelect (line 9): candidates are Workspace's unlocked BaseParts as their selectable (top Model or the part); centre = Model:GetBoundingBox().Position, else the part's ExtentsCFrame.Position (in a Tool: the part's Position); visible Attachments by WorldPosition. DragSelector.updateDrag (line 110): on every mouse move once `(mouse − start).Magnitude >= 3`; getSelectionFrustum (line 48) makes 4 planes from the camera rays through the box's corners; in when `(Center − origin):Dot(normal) >= 0` for all four; Shift / Ctrl extend from the selection at the press | `viewport.rs` `marquee_pick` (one centre per selectable, a Model by its parts' box; the old "parts over 200 studs" filter is gone: only Locked parts are skipped); `mech::DRAG_SELECT_MIN_PX` = 3 |
| Drag: grab point | dispatchWorldClick.buildFreeformDragInfo (line 44): with grid snap on, the clicked point is rounded in the clicked face's TargetMatrix on X and Z: `math.floor(x / grid + 0.5) * grid` | `part_drag.rs` `start_part_drag` |
| Drag: surface frame | DragHelper.getSurfaceSnapForPartTreatedAsBox (line 578), buildSurfaceMatrix (line 175): the face is the box axis most aligned with the hit normal; the frame's origin is the face corner nearest the hit, Y = the face normal, X = −(normal × edge) with edge the face axis whose edge is nearer | `part_drag.rs` `Face::of_box` |
| Drag: snapping | DragHelper.computeDragSnapping (line 951): the hit, in the target frame, moves by `floor(x / grid + 0.5) * grid − x` on X and Z (never along the normal); soft snaps to the target's edges within `0.02 · sin(rad(FOV)) · camera distance · margin` | `Face::snap` on the target face (was: the part's centre on world axes); soft snaps not built |
| Drag: placement | DragHelper.getDragTargetNew (line 1187): the grab point goes to the snapped hit and the selection's bounding box rests on the surface, lifted `0.5 · size.Y − offset.Y` along the normal | `update_part_drag`: centre = hit − in-plane grab offset + normal × the selection box's half-extent (several parts: their world-aligned box) |
| Properties: collapsed categories | uses a category's remembered open state; with none, the set built {"Surface", "Surface Inputs"} starts collapsed and the rest open | `props_layout.rs` (Surface Inputs added) |
| Properties: category order | not found as data: the category strings are used only by property registrations and the translation table built | kept as captured |
| Properties colours | theme: MainText (188, 190, 200), disabled (73, 77, 90); CategoryItem / Border (32, 34, 39); MainBackground (25, 26, 31); CheckedFieldBackground (40, 41, 49) | the same constants (measured, now confirmed by the theme) |

## Round 18: Properties editors, measured in an offline Studio

| What | Studio (measured / read) | rustudio |
|---|---|---|
| BrickColor popup | `ColorPickerPopup` 368 × 400 on (25, 26, 31); pointy-top hexagon cells 26 × 29, centres 27.75 px apart, rows 24 px apart from y 22.9, row r holding 7 + min(r, 12 − r) cells starting at x 100.5 − 13.875 · min(r, 12 − r); `BrickColor.palette(0 … 126)` fills the rows in order; grey row at y 339.5 from x 30.8; More Colors bar (53, 55, 65) at y 365; current colour ringed 2 px red outside 2 px yellow | `color_editors::popup_cells`, unit test `studio_layouts` |
| Select Color | Qt QColorDialog 522 × 399: field 220 × 200 at (248, 21), hue = 360 − x · 360 / 219, saturation = 255 − y · 255 / 199, drawn at value 200; cross = 20 × 2 and 2 × 20 black bars; value strip 13 × 200 at (486, 21), value = 255 − y · 255 / 199, arrow at its right; basic colours R ∈ {0, 170} (rows 0–2) or {85, 255} (rows 3–5) by column parity, G = 0 / 85 / 170 / 255 by column pair, B = 0 / 127 / 255 by row; HSV as QColor (grey 163, 162, 165 → 260, 5, 165) | `color_editors::color_dialog_ui` |
| Colour edits | live on the part while the dialog is open; OK = one undo step; Cancel restores | `properties.rs` `color_editors_ui` |
| Sliders | ReflectionMetadata UIMinimum / UIMaximum / UINumTicks / SliderScaling; step = range / UINumTicks, 20 without (Transparency 0.05, ParticleEmitter.Drag 0.25, ShadowSoftness 0.01, measured by dragging) | `prop_slider.rs`, unit test `studio_steps` |
| Slider editor | 65-px spin box then slider; handle 9 × 14 (53, 55, 65) edged (32, 34, 39); groove 2 px; editor background (40, 41, 49); row selection (19, 35, 93) | the same constants |

## Round 19: engine fixes from the person's own games, measured in an offline Studio

Calibration scenes (`tools/fidelity/corpus/*.rbxlx`, `"calibration": true`
in corpus.json, run only when named: `fidelity.py run --places bloom`):

| What | Studio (measured) | rustudio before → now | Where |
|---|---|---|---|
| Neon bloom (`bloom`: orange Neon balls of 1, 2, 4 studs at 15–60 studs, a white ball, a bar, over black) | a smooth halo in the part's hue, the same width in pixels at every distance: two Gaussians of σ 3.28 and 8.72 px (863×675 view), 0.533 of the wide one, 2.98 × the colour value next to a Neon part (Neon is 4× its colour); the halo's hue is the colour value's (green / red 0.651 for Color (255, 170, 0)), not linear light's (0.444). Fit RMS 1.06 levels over 32 px, R and G | 4 rings × 8 taps of the full image at Size/4 steps, threshold subtracted per channel: concentric rings and petals, a red halo on orange, blue on cyan → bright pass at half resolution (per pixel: hue √(c / max) × (max − Threshold) × 1.49), separable two-Gaussian blur with every texel weighted, bilinear composite before the tone curve. Ball4 +2 / +6 / +12 px: Studio 219 / 163 / 71, rustudio 212 / 155 / 67 | `shader.wgsl` fs_bloom_*, `mech::BLOOM_*`; self-test `bloom-smooth` (phases 114–115) fails on a rise > 1 level, a kink > 6, petals > 2.5 px, a profile > 14 levels off Studio's or the wrong hue (the old bloom: kink 16.3, profile 20.9 off, green / red 0) |
| Local lights, LightingStyle Soft (`lightcap-soft`: PointLights Range 12 at 4 studs over a (40, 40, 40) floor, Brightness 0.5–32, four overlapping, one Range 45) | 38 / 67 / 112 / 164 / 211 under Brightness 2 / 4 / 8 / 16 / 32; the four overlapping 210 | 34 / 63 / 106 / 158 / 205; 206: no cap needed | unchanged |
| Local lights, Realistic (`lightcap-real`) | 96 / 170 / 220 / 235 under Brightness 1 / 2 / 4 / 8; 4 studs off, 75 at Brightness 4 | 43 / 42 / 74 / 121; 56 | **open**: near a light Studio's Future falloff is far brighter (an inverse-square-like core the fitted model lacks) |
| CFrame.lookAt near vertical | right = look × up however short (1e-5 off straight down keeps right +X); exactly parallel: right = look × X (down: right +Z, up +X; up: right −Z); the two-Vector3 CFrame.new(pos, at) already falls back 1e-5 off vertical | fell back below 1e-8 with look × Z: a top-down camera turned 180° | `ltypes::look_at` / `look_at_legacy`, unit test `look_at_vertical_like_studio` |
| Script camera roll | Camera.CFrame's whole rotation is the view | the view kept yaw / pitch only (no roll; pitch −π/2 has cos < 0 in f32 and flipped it) → `Camera::basis` from the script's CFrame, dropped once anything turns the camera by yaw / pitch | `camera.rs` |
| Lowercase datatype members | CFrame `lerp`, `toAxisAngle`, `toEulerAngles`, `toEulerAnglesXYZ`, `toEulerAnglesYXZ`, `toOrientation`; Color3 `lerp` (everything else probed matched) | missing → aliases. Place B's Poppercam calls `toAxisAngle` every frame | `ltypes.rs` |
| UserSettings():IsUserFeatureEnabled | UserNewPoppercam4 true (the Popper camera), UserPoppercamLooseOpacityThreshold true, … 16 names the corpus games ask; UserAllowDisableCustomAnims2 and UserPortraitPopperFix error | unknown names errored, so Place B's PlayerModule fell back to Poppercam_Classic and the camera never pulled in | `library.luau` user_features |
| Technology → LightingStyle on load | ShadowMap (Place B's og.rbxl saves only Technology 3) reads Soft with PrioritizeLightingQuality true | Realistic, false | `mech::TECHNOLOGY_SOFT`, `TECHNOLOGY_QUALITY` |
| See-through parts | back faces aren't drawn; from inside a translucent part nothing of it shows (Place B's camera sits inside a Transparency 0.55 Neon union) | both faces drawn: a pale wash over the whole view → built-in shapes cull back faces; a translucent mesh or union is skipped while the camera is inside its box | `render.rs` transparent_culled, `camera_inside` |
| A character held by movers (Place C's ship: PlatformStand, a Vector-mode LinearVelocity with MaxForce 1e7, AlignOrientation Responsiveness 40) | stays on the plane Y = 400 and turns toward the align | fell and played `fall` → a Vector-mode LinearVelocity (the default mode, World) with MaxForce ≥ `MOVER_HOLDS_CHARACTER_FORCE` sets the whole velocity; a non-rigid AlignOrientation eases the yaw at its Responsiveness | `play.rs` client_root_movers |
| Parts welded to a character (Place C's ship, welded to the HumanoidRootPart) | move with it as one assembly | simulated loose: fell and were destroyed → carried at their weld offset (`Rig::refresh_attached`), left out of the simulation | `rig.rs`, `play.rs` |
| Humanoid.PlatformStanding on the client | fires on the client when the server sets PlatformStand (the client's state enters PlatformStanding) | only in the VM that set it → also fired on the client from the state | `play.rs` fire_humanoid |
| Place C's leftover idle track | at 1 s Studio's character has no Animate yet (offline appearance loading), so its Animate misses PlatformStanding and keeps an idle track that never loaded (WeightCurrent 0, Length 0) | rustudio's Animate is in the character before CharacterAdded, as online, and stops its tracks as Animate does on the event: Place C's `animation` area reads 0 from this offline artifact | `tools/fidelity/probe_states.luau` (corpus `place-c-probe`) |
| Terrain editing | Terrain:FillBlock / FillBall / FillCylinder / FillWedge / FillRegion / Clear / ReplaceMaterial, SetMaterialColor / GetMaterialColor (Place C builds every planet surface with FillBlock, Place B its water) | warned and did nothing → voxels edited (share inside = 0.5 − signed distance / 4 studs; a material takes a voxel at the larger occupancy, Air carves), written back to SmoothGrid once a step (SmoothGrid encoder, unit test `fill_encode_round_trip`) and replicated: drawn and collided (a character dropped on a filled slab rests at its top + 3.19) | `terrain.rs` encode / fill, `runtime_ext::terrain_call` |
| Rays and terrain | WorldRoot:Raycast and FindPartOnRay… hit terrain, with its material and the surface normal | never saw terrain → where the voxels' occupancy, blended between centres, crosses 0.5 (the mesher's surface): half-stud steps refined by bisection; water isn't hit yet | `runtime_ext::terrain_hit` |
| Humanoid.FloorMaterial on terrain | the voxel's material | Plastic → the first solid voxel under the root | `physics::terrain_material_under` |

## Round 20: performance, from Studio (Studio)

| What | Studio does | Where |
|---|---|---|
| Humanoid.RootPart and Torso | both registered under "Data"; **one getter for both**. RootPart has no setter (ReadOnly), Torso has one. The slots are 16-byte part references in the order of the rig's name tables: R6, R15. So RootPart / Torso = HumanoidRootPart, else UpperTorso (R15) / Torso (R6). rustudio returned nil without a HumanoidRootPart and had no Torso at all before this round; both now follow this (`ROOT_PART_FALLBACK_R15` / `_R6`, src/runtime_ext.rs) | registration; getter (this − 0xc0) →; Torso (begins with permission checks; its body wasn't read, so a written Torso is stored as an ordinary value in rustudio) |
| FindFirstChild | a straight scan of the child list comparing the name's length, then its bytes; the **first** match in child order is the result, and it then passes an access check or the call returns nil (it does not go on to a later child of the same name). No index. rustudio's journal-kept name index (round 20) returns the same child: first in child order among same-named children; rustudio has no locked instances, so the check never applies | registration ("findFirstChild", arguments "name", "recursive") →; non-recursive → scan; recursive |
| SelectionSurfaceGuiCullAngle | FInt **20**, by its name a limit on selecting (gamepad / GuiService selection) SurfaceGui objects seen at an angle; no reader references it directly, so what the 20 measures wasn't confirmed and rustudio doesn't use it. rustudio's own SurfaceGui cull is the face's side: drawn only when the camera is in front of the face (unchanged) | name set, value (FInt values sit 8 bytes before their name slot), |

Not from Studio (rustudio's own, round 20): the SurfaceGui split bound
(0.5 px of perspective error, `GUI_SURFACE_AFFINE_ERR_PX`), drawing only
GUI items that can cover a pixel centre, skipping the walk of unseen
clipped SurfaceGuis, the interrupt's clock check one call in 256, and the
journal readers (GUI roots, lights, client joints, child names). None
changes a value a script reads except that an unseen SurfaceGui's layout
is the one from its last walk.

## Round 21: terrain, measured in an offline Studio and read from its shaders

The last column says where rustudio builds it.

**Superseded (2026-11-10, "Terrain against textured Studio" below): the
offline Studio now draws textured terrain.** Earlier: Studio asks for terrain
TexturePacks as KTX2 representations (`7rdo`, `doNotFallbackToBaseline`);
with FStringTextureTranscodeRollout off it asks for the baseline pack but
never fetches the maps inside it (SSL "certificate unknown" in
the local backend.err). So an offline Studio shows each material's `avgAlbedo`
(FStringTerrainColorTable) recoloured by MaterialColors. The terrain corpus
runs rustudio with `RUSTUDIO_MATPACKS=0` so both draw the same fallback;
with packs rustudio draws the real textures (same shader, layer ≥ 0).

### Voxels

| What | Studio | Source / rustudio |
|---|---|---|
| Occupancy byte | round(share × 255); read back as (byte + 1) / 256; a share that rounds to 0 leaves Air | ReadVoxels after FillBlock / FillBall (terrain-probe: docc 0.0 on all five fills); `occupancy_to_byte`, `mech::TERRAIN_OCCUPANCY_SCALE` |
| FillBlock share | 0.5 − d / 4 with d = max over axes of (\|p\| − half size) (a box distance, not Euclidean at corners) | probe Fill tests; `runtime_ext::box_distance` |
| Surface crossing | from an occupied voxel to air at t = its occupancy (samples 0.5 + 0.5 × occupancy, so any occupied voxel is inside); surface-nets vertex = mean of the cell's crossings. Lone voxel tops 3.33 / 2.67 / 2.33 / 2.14 studs for occupancy 1 / .5 / .25 / .1; a full slab's top half a voxel above its voxels | probe rays: dh 0.0 (lone voxels, Brick wall), ~0.02 (slabs); `Grid::fill`, `cell_vertex` |
| Water | meshed the same way as solid: any water voxel is inside, its occupancy moves the surface; no face where water meets solid | Place B's lake: ~1 100 part-filled water voxels drew faces inside the lake before; `Grid::fill` |
| Shape per material | `cubify` (Brick, WoodPlanks…): vertices on voxel boundaries; `quantize` snaps; `shift` jitters by a fixed random amount per cell | FStringTerrainMaterialTable2022 / Pre2022 (copied to assets/terrain-materials.json); Brick wall dh 0.0 |
| Quad split | along the longer diagonal | stairs dh 0.278 with a fixed split, 0.086 with this |
| Cell material | majority by occupancy, ties to the lower slot | two_materials probe (a lone Snow voxel no longer spreads over a Basalt slab) |
| Rays | hit the physics mesh, which is the drawn mesh | probe: zero hit disagreements; `runtime_ext::terrain_hit` tests the chunk triangles (Möller–Trumbore), material from the corner with the largest weight |

### Shading (SmoothCluster)

| What | Studio | rustudio |
|---|---|---|
| Corners | every triangle carries its three corners' materials; each corner picks one of 18 projection axes (6 faces, 12 edges; cube-mapped materials only the 6) and a random rotation and offset (`fract(k·0.618034, floor(k·2.6651442)·0.381966)`) to break tiling | `TerrainVertex`, `AXES`, `terrain_uv` |
| Blend | height blend: w = clamp((h − max(others) + s/2) / s), h = remapped height × barycentric, s = Σ bary × blend scale; faded back to plain barycentrics as the texture gets small (UV derivative × 7) | `fs_terrain` |
| Recolour | albedo to YCoCg; luma × scale, chroma turned and scaled by (cos, sin) (CB4); out R = Y − Cg + Co, G = Y + Cg, B = Y − Cg − Co. rustudio solves scale / cos / sin so the default MaterialColor shows the texture as it is, and another colour moves luma and chroma from the default's to it | `terrain_mat::recolor` (solving from avgAlbedo blew chroma up ×15) |
| Light | GGX sun term with F0 0.04, roughness 0.089 + 0.911 × map; ambient + OutdoorAmbient × sky + sun × N·L × shadow; the light grid is sampled at pos + 6 × normal (VS VARYING4) | GGX and ambient as parts; light grid since round 25 (`lightgrid.rs`) |
| Output | `sqrt(clamp(c × Exposure))`: no tone curve in this shader | see "Tone curve by Technology" |
| Faces | a corner shows its top texture from normal Y ≥ 0.5, bottom from ≤ −0.5, else side | `mech::TERRAIN_TOP_NORMAL_Y` (estimated) |

### Tone curve by Technology

Legacy / Voxel / Compatibility (and a place that saves no Technology, as
Voxel is the default) draw with **no tone curve**: sqrt(c × Exposure)
straight to the screen, as SmoothCluster FS HQ ends. ShadowMap / Future keep
the measured rational curve. A/B on the same Studio shots (rustudio only):

| Place (Technology) | curve | none |
|---|---|---|
| place-c-terrain (Voxel) terrain | 60.4 | **77.5** |
| place_a (none saved) render | 70.6 | **81.7** |
| place_c (Voxel) render | 95.6 | 95.3 |
| terrain (ShadowMap) terrain | **83.3** | 72.0 |
| demo (Future) render | **78.0** | 76.2 |
| place-b-terrain (ShadowMap) terrain | **62.5** | 58.7 |

`mech::TECHNOLOGY_NO_TONE_CURVE`, `lighting::globals`. The curve's toe
(y ≈ 0.011 x for small x) had turned Place C's night planets black.

### Water

| What | Studio | rustudio |
|---|---|---|
| Surface (LQ) | WaterColor lit by ambient + shadowed sun (no N·L), mixed with the sky's reflection by (clamp(0.78 − 2.5 \|N·V\|) + 0.3) × WaterReflectance; WaterTransparency | `fs_part` kind 5; `mech::WATER_*_DEFAULT` (12, 84, 92), 0.3, 1 |
| From below | the surface is drawn (terrain place Place A) | alpha 0.9, lit colour |
| Under water | the view fogs toward the lit WaterColor: visibility = clamp(2^(1.066 − 0.00137 d) − 1, 0, 1) (Studio's exp2 fog form): clear to 48 studs, gone by 778 | fitted to Place B's lake (F4: hit distances 63–698 studs, linear blue channel, < 0.02 off); `mech::UNDERWATER_FOG`, `fog_amount` / `fog_rgb`; the sky is the fog colour |
| Waves | WaterWaveSize / Speed move the surface (SmoothWater VS) | **not built** |

### Grass (Terrain.Decoration)

Prebuilt blades on Grass tops whose tips sink into the ground with distance
(Grass VS). rustudio: 4 blades / stud², height 2.5 × GrassLength (default
0.7), half-width 0.1, sinking from 150 to 250 studs, within 260 studs of the
camera, one chunk per frame (`terrain::grass_blades`, `GRASS_*`; the counts
and sizes are estimated from the grass hill shot).

### Random (from Studio)

| What | Studio | Where |
|---|---|---|
| Generator | PCG32, multiplier 0x5851f42d4c957f2d, **increment 105**, output XSH-RR; seeded like `pcg32_srandom` (state 0, step, add the seed, step) | NextNumber (the step is inlined) |
| NextNumber | (second draw << 32 \| first draw) × 2⁻⁶⁴ (ldexp, rounded to a double) |; method table → wrapper |
| NextNumber(a, b) | ends put in order (no error when a > b); min × (1 − t) + max × t, clamped into [min, max] | |
| NextInteger(a, b) | one 32-bit draw: min + (u32 × span) >> 32 | matched to Studio's sequences (seeds 0, 1, 1001, 1005, 123456789); wrapper not read |
| math.noise | already identical (Luau's own) | FIDRNG line in place_c_terrain.luau |

rustudio's Random (`ltypes::LRandom`, `mech::RANDOM_PCG_INC`) was a guessed
PCG with another increment and word order, so seeded generation (Place C's
surface patches) came out different. The unit test
`ltypes_random_matches_studio` pins Studio's numbers.

### Other

- Lighting.Brightness in a file that doesn't set it reads **1** in Studio
  (the reflection database says 1.981): `doc::studio_default`.
- Harness: Studio's 38 px device bar overlays the top of its 863 × 675 view;
  both images now lose those rows before comparing (render method 2: demo
  22.5 → 13.5 MAE, place_a 32.4 → 18.7, with no engine change). An "empty view"
  only fails the gate when Studio's shot isn't near-empty too.
- Not from Studio (rustudio's own): the Terrain Editor's brush maths (share
  0.5 − d / 4 like Fill*, Sculpt 0.35 × strength × falloff, Smooth over the
  six neighbours, one application per 1/30 s), and Generate's noise
  (value-noise fBm, not Studio's generator).

## Round 22: the Humanoid character controller, measured and from Studio

Measured with tools/fidelity's movement set: `movement-r15` / `movement-r6` (movement_course.luau builds 47 lanes, movement_probe.luau drives `Humanoid:Move` / `Jump` with one fixed program per lane and records every frame after physics, at most 60 a second), `movement-place_b` (the same probe injected into Place B as a LocalScript, 8 segments from its spawn) and `movement-place_c` (calibration: Place C's ship). movement_compare.py lines the traces up per lane from the lane's start (1/30 s grid).

### What collides

| Rig | Studio | Source / rustudio |
|---|---|---|
| R15 | HumanoidRootPart (2 × 2 × 1), UpperTorso, LowerTorso, Head have CanCollide true; arms, legs, hands, feet false. The body spans the root's bottom (HipHeight above the floor) to the head's top | movement_probe FIDMOVEDONE parts; rustudio set every character part false before (scripts read it): `studio_collides` at spawn |
| R6 | Torso and Head collide; HumanoidRootPart and the limbs don't | same |
| Body | the colliding parts are boxes: rustudio moves one box (their union in the root's frame, from the root's bottom up), turned with the root's yaw | `play::hum_shape`, `physics::move_humanoid` (rapier's controller with no autostep and no snapping: the floor rays do both) |

### Finding the floor

- Rays straight down from the root's local (0, −0.4·size.y, k·0.4·size.z), k = 0, 1, −1, 2, −2; if none hits, from the corners (±0.4·size.x, ±0.4·size.z). Sizes count at least 0.05, × 0.4.
- Length = 0.3 × (HipHeight + root size.y) + HipHeight × 1.5 while it has a floor, × 1.1 without, plus |v| / (HipHeight / 2) / 100 when that exceeds 100 (fast falls only).
- The hits are averaged: one floor height, normal and point. A ray that starts inside a part doesn't hit it (Roblox raycasts).
- Consequences, all seen in Studio's traces: steps lower than the root's bottom are walked up (2.0 studs yes, 2.5 no, both rigs); a 2-stud drop stays Running (the 1.5 × reach still finds the floor) and falls by gravity; walking off an 8-stud ledge goes to Freefall after ~2.1 studs of fall (Studio 0.42 s); a 75° or 85° ramp stops the body at its foot (the front rays start inside the ramp), 60° is walked.

### States

| What | Studio | rustudio before |
|---|---|---|
| Jumping | one physics step, then Freefall (0.01–0.02 s) | until the apex (0.27 s) |
| Landed → Running | 0.05 s | 0.12 s |
| Running → Freefall | when the floor rays stop finding a floor (no timer) | 0.1 s after leaving the ground |
| FloorMaterial | Air while Seated, PlatformStanding, FallingDown | the floor's |
| Seats | sit when a touch begins (standing on a seat after jumping off it doesn't sit again) | any touch after a 1 s cooldown |
| Swimming | speed 35 / (35 + 13.4 × parts / mass) × WalkSpeed (R15 11.1, R6 13.3 studs/s), rising 1.63 (R15); the root lies flat (looks down); Jump gives the full jump speed (Jumping, then Swimming again; held, it leaves the water) | 16 studs/s upright, 14.3 up |

### Open

- (Round 23: done) A part a LocalScript moves on the client (a tweened
  platform) wasn't seen by rustudio's physics; see "Client-moved parts".
- (2026-11-11: done) Truss climbing kept the body further from the truss
  than Studio; climbing now moves the body box (see Climbing). R6 on
  Friction 0 falls over in Studio (not modelled).
- Wall friction and the ceiling bounce are fits from one lane each; the
  body's turn against walls is a rule (face the wall), not the contact
  torque Studio integrates.
- Network: the client still owns and writes its character to both
  DataModels each frame (no interpolation lag on the server copy).

## Round 23: assets, world GUI depth, walls, client platforms, swimming

Measured with tools/fidelity: the asset audit (`asset_audit.py`, every
place), `gui-depth` (make_gui_depth_place.py), `movement-wall-r15` / `-r6`
(the wall lanes alone), and the `movement-r15` / `-r6` lanes
`tween_platform` and `water` on the round 22 Studio captures.

### Assets (bug 1)

| What | Studio / Roblox | rustudio |
|---|---|---|
| Download | Roblox's asset delivery answers "Authentication required" for most assets without a signed-in session (Place B: 478 ids); an offline Studio gets none of them | the AssetStore says once in Output that signing in shows them; `RUSTUDIO_ASSET_REPORT` lists every id with loaded / failed (why) / downloading / not requested |
| Content ids | trimmed (`…?id=125882923   ` loads) | `scene::content_string` trims |
| A mesh that hasn't loaded | drawn as nothing | was a striped placeholder box; now hidden until it arrives (failed: hidden for good) |
| SpecialMesh under a MeshPart | the MeshPart keeps its own mesh | an empty SpecialMesh used to replace it |
| CharacterMesh | swaps the BodyPart's limb (1–5) for MeshId, unscaled | `scene.rs` CharacterMesh block |
| Not drawn by rustudio yet | ParticleEmitter, Beam and Trail textures (30 / 8 / 1 in Place B) | listed as "not requested" in the report |

### Walls in the air (bug 2)

Studio: a character jumping into a wall while holding toward it slides down
and lands (movement-wall wall_jump: the root is back at 3.09 by 1.2 s).
rustudio hung on it: rapier's character controller had
`min_slope_slide_angle` 90°, so the wall counted as a slope too steep to
slide down and the vertical move was cancelled. Now 1°
(`physics::hum_controller`). wall_jump 33.8 → 83.6 (R15). The 45° lane
(`wall_jump_diag`) still slides slower along the wall than Studio, the same
fit as round 22's wall_slide.

### Client-moved parts

Studio's client simulates its character against the client's own copy: a
platform a LocalScript tweens away slides out from under the character,
which drops to the ground (tween_platform ends at 3.19, the ground).
rustudio's physics is the server's copy; `Physics::follow_client` now moves
the collider of every anchored part whose CFrame the client DataModel
changed (marks read with `Reader::ClientPhysics`), so the character meets
the client's world. tween_platform 57 → 94 (R15 and R6). Parts with an
assembly (simulated) stay the server's.

### Gates

- `terrain`'s cave shot (24 s) is dark in both engines (MAE 4.8, the lowest
  of the set); the empty-view check flagged it because rustudio's is one
  colour where Studio shows its light-grid creases. A near-empty view now
  fails only if its MAE is ≥ 8 (`EMPTY_MAE_MIN`); the crease shading is
  still missing (no light grid) and the shot is marked `flat_close`.
- `place_a_match` and `place_b` have `"pixel_gate": false` (reasons in
  corpus.json): Place A spawns enemies at random windows and the view follows
  the nearest (render spread 21 over three runs); the reference Studio
  can't download Place B's assets while rustudio draws the ones in its
  cache. Both keep the error, empty-view, asset and score-drop gates.
- `place_c` has `volatile_parts` for `Workspace.Scenery.Part` (position,
  transparency): World.luau sets those ring parts from os.clock every
  frame, so the two captures read them at different moments; materials
  100 → 96 in round 21 came from the rings first matching.

## Round 24: sign-in, legacy GUI, UICorner images, water, swimming

Fixed directly after the person's Place B play (not a rebuild round).

### Sign-in and downloads

| What | Was | Now |
|---|---|---|
| A new cookie in `~/.config/rustudio/roblox-cookie` | ignored while the login keyring held the old (expired) one: 401 | a file that differs from the keyring wins and replaces it (`session::load_cookie`) |
| Downloads made before the session check ends | background loads (`public_cached`) failed anonymously for good; the mesh / texture caches never asked again | they wait for the check (30 s at most); `AssetStore::fetch` defers while it runs and uses the session as soon as the worker sets it; failures from before a sign-in are handed back for retry |
| Parallel downloads | every session download held the account's lock: one at a time | each takes a copy of the session (`Session::downloader`) and runs side by side |

Place B from an empty cache: images 83 → 354 loaded, meshes 187 → 386; 7 + 7 left
(private assets and old built-in `fonts/*.mesh` files).

### Legacy GUI properties and UICorner

| What | Studio | rustudio |
|---|---|---|
| `TextLabel/Button/Box.FontSize` ("Size28", Enum.FontSize.Size28) | sets TextSize (they are one property) | `runtime::newindex` sets TextSize |
| `Frame.Style` RobloxSquare (2) / RobloxRound (3) | blackBkg_square/_round.png: black at 179/255, an 8 px corner on Round; the frame's own colour and border are not drawn | `gui_walk` |
| UICorner on an ImageLabel / ImageButton | the image is rounded too | `rounded_quad_mesh` for Stretch, Fit and Crop (Slice and Tile stay square) |

### Water (HQ shader)

As Studio's water shaders (SmoothWaterFS with GBufferColor /
GBufferDepth; SmoothWaterVS). `water.rs`, `fs_water`.

| What | Studio | rustudio |
|---|---|---|
| Ripples | NormalMap1 / NormalMap2 (PlatformContent textures/water/normal_01..25.dds, DXT5, X in .w, Y in .y) blended by CB3[0].w, projected on the cell's axes × 0.05 (20 studs a tile) | the 25 frames as one mipmapped array, the face's main axis; frame rate 0.8 / s per WaterWaveSpeed (estimated) |
| Reflection normal | mix(geometric, mapped, 0.25) | same |
| Refraction | the opaque scene (GBufferColor) at the pixel offset by mapped N.xz × 0.05 of the view, the unbent one where the bent sample is in front of the water | a copy of the opaque scene's colour after the opaque pass; the water tests the multisampled depth itself |
| Depth blend | mix(refracted, lit WaterColor, clamp((scene depth − water depth) × CB3[2].x + CB3[2].y) + clamp(view depth × 0.004 − 1)); GBufferDepth holds to 500 studs | the same form; CB3[2].x fitted (`WATER_DEPTH_SCALE`): Studio shows the bottom through 50+ studs at WaterTransparency 0.3 |
| Fresnel | (clamp(0.78 − 2.5 \|N·V\|) + 0.3) × occupancy × CB3[2].z, toward the env map × sky light | the same with WaterReflectance; the sky colour (no SSR) |
| Sun glint | GGX, F0 0.02, roughness 0.089 + 0.911 × map .x, × N·L × shadow, fading by CB0[28].y | same; fade estimated |
| Waves (VS) | y += (sin((z − x)k − φ) + sin((z + x)k + φ)) × amplitude × occupancy × fade | same form; k, φ rate and amplitude estimated (`WATER_WAVE_*`) |

place-b-water (new place, `place_b_water.luau`): render 55.7 with round 23's LQ
water (murky teal where Studio is clear and bright) → 60.2; terrain 56.1 → 60.8.
Its tree spots (S1, S2) can't be compared: the reference Studio can't download
the tree meshes.

### Sky visibility (OutdoorAmbient) under turned parts

Each part used to roof every cell of its bounding rectangle, so a diagonal
tree branch darkened the whole square around it (stepped dark blocks on the
grass under trees). Now a turned part roofs a cell only where the cell's
vertical line meets its box (`lighting::line_box_entry`), and the shader
blends the four nearest cells (Studio's light grid is smooth).

### Checks after this round

The `place_b` render score fell 36.9 → 22.7: rustudio now downloads the place's
meshes and textures (pumpkins, trees), which the reference Studio still can't,
so the two differ more. Place B Play measured 56.6 fps with the machine under
load 16–30 (OCR and OBS running), with the water pass on or off alike; it needs
measuring again on a quiet machine.

## Round 25: wall slide and corners

Measured on the saved Studio captures of `movement-r15` / `-r6` (lanes
`wall_slide`, `corner`) and `movement-wall-r15` / `-r6` (`wall_jump`,
`wall_jump_diag`), frame by frame.

| What | Studio (measured) | rustudio |
|---|---|---|
| Sliding along a wall on a floor | `wall_slide` (45° into it): the body bounces off the wall about 12 times a second; vz swings 7.1–9.8 and averages **8.62** studs/s in both rigs (round 22's 9.93 was a peak). Held exactly flush it slides at Plastic's Coulomb 0.3: `corner` a steady 7.92 = 11.31 × 0.7, R6 `wall_jump_diag` after landing 2.87 = 7.16 − 0.3 × 14.31. R15's slides at ~5 there | `HUMANOID_WALL_FRICTION` 0.24: the slide loses 0.24 × the speed pushed into the wall (fitted to the mean; 0.3 scored R15's `wall_jump_diag` 58 and `wall_slide` 78, 0.22 dropped R6's `wall_jump_diag` to 52) |
| Sliding along a wall in the air | `wall_jump_diag` (jump at 0.35 s, walking 26.6° off the wall's normal): along the wall the body speeds up from ~3.3 to **11.44** (R15) / **10.83** (R6) studs/s within ~0.2 s, faster than the walk's 7.16 along it. The push into the wall (14.31) turns into slide: 0.30 / 0.26 of it | `HUMANOID_WALL_AIR_PUSH` 0.28, reached at `HUMANOID_WALL_AIR_ACCEL` 50 studs/s² (fitted). It was 5.41 (the floor's friction) before |
| Landing from that slide | the slide wears off over ~0.1 s (R15 11.5 → 8.2 → 5.9 → 3.8 at 0.05 s steps) | the floor's slower slide is reached at the same 50 studs/s² (it dropped at once) |
| Facing in the air | AutoRotate keeps turning toward the walk (yaw −0.07 → −0.46, the walk's own) while pressed against the wall; on a floor the body turns flush | `play.rs`: the flush turn only while `grounded` |
| Inside corner | the body stays flush to the wall it slid along (yaw −1.57) and stops in the corner | of the walls the box presses into, the one it already faces most (`move_humanoid`). Before, the last contact rapier reported won; it changed from run to run, and when it was the end wall the body turned 90° to face it (`corner` read 79 or 93 at random in both rigs, so the "R6 drop" after round 23's 1° `min_slope_slide_angle` was this coin toss, not the angle) |

The wall-float fix stays: `min_slope_slide_angle` is still 1° and
`humanoid_falls_down_walls` passes. `humanoid_slides_along_walls` now
expects 8.62.

Scores (rustudio-only runs on the saved captures, two runs after, one
before):

| Place | before | after |
|---|---|---|
| movement-r15 | 92.1 | 93.2 / 93.1 |
| movement-r6 | 92.9 | 93.3 / 93.3 |
| movement-wall-r15 | 73.8 | 86.6 / 86.8 |
| movement-wall-r6 | 64.0 | 73.0 / 72.9 |

| Lane | before | after |
|---|---|---|
| wall_slide (r15, r6, wall-r15, wall-r6) | 73.5, 72.9, 75.7, 75.9 | 89.6–89.9, 91.6–91.9, 88.7–88.9, 89.1–89.5 |
| corner (same order) | 79.2, 92.8, 91.3, 79.9 | 95.2–95.3, 95.3, 95.0–95.2, 93.9–94.2 |
| wall_jump_diag (wall-r15, wall-r6) | 44.3, 48.4 | 78.4–79.1, 56.7–57.9 |

Lanes these changes can't reach (no walk into a wall: jump_power, jump_low,
ceiling_jump, sprint, ramp_85) moved 2–6 points between runs either way;
the round 23 checks had them at 90.8 / 96.7 / 89.9 / 91.0 / 89.1.

Open:
- R6's `wall_jump_diag` (57): its ground slide is the 0.3 one (2.87), slower
  than the 0.24 fit, so it reaches the wall's end ~0.25 s late. R15's runs
  faster (~5). The difference is Studio's contact dynamics (how flush the
  box stays, how often it bounces off), which a single friction can't fit.
- R6's `wall_jump` (51): jumping straight into the wall, Studio's R6 drifts
  3.6 studs sideways (symmetry broken by its solver); R15 drifts 0.3.
- Leaving a wall's end Studio pivots the body round the edge (yaw +0.2) and
  gets free ~0.3 studs earlier.
- Jumping into a wall at an angle Studio's apex is higher than a free jump
  (R15 11.9 vs 10.4); straight in, lower (9.7). Not modelled.

### What Studio's plugin does

| What | Studio | rustudio |
|---|---|---|
| Tools | Region tools Select, Transform (move / resize / rotate handles), Fill; Copy, Cut, Paste, Duplicate, Delete are Select's actions (Ctrl+C / X / V / D, Delete) | Region row: Select, Move, Resize, Rotate, Fill (the old Region tab's split of Transform); the five actions as buttons, and the same keys while a Region tool is picked (`app::run_action`) |
| Grid snap | getVoxelAlignedRegion: each side floor((pos ± size / 2) / 4) × 4; ConvertTransformToRegion: cells floor(min / 4) to ceil(max / 4) | `RegionBox` (first voxel + size in voxels); `RegionBox::from_studs`; handle drags move in whole voxels |
| Copy / Cut / Delete | CopyRegion of the box into a buffer with its transform; Cut and Delete fill the box with Air (fillAir) | `terrain_region_action`: `Region::copy`, `terrain_region::set_box(…, Air)` |
| Paste | sets TransformMode Paste, opens Transform at the copied box's place (TerrainRegionBufferTransform) | pastes at the copied box, then picks Move |
| Duplicate | copies the box, opens Transform in Duplicate mode (the source stays) | a lift with no source, then Move |
| Transform step | restore the last preview's backup; Move clears the source to Air; Merge Empty clears the target to Air; the copy is turned (Rotation) and stretched (Size) by TerrainRegion:ApplyTransform; PasteRegion with pasteEmptyCells false | `place`: from the grid as it was when the box was lifted, source cleared (Move), `Region::rotated` then `resized`, pasted with empty cells only under Merge Empty (the same result as clearing first) |
| Rotation | CFrame from Euler degrees; handles snap | quarter turns only: an integer matrix composed per turn (`terrain_region::quarter_turns`, `mul`), whole cells, nothing lost; the box turns about its centre |
| Fill | Fill: SetMaterialInTransformSubregion (the whole box takes the material); Replace: source → target | `set_box` (full occupancy) / `replace_box` (occupancy kept; Air as the source fills the empty voxels) |
| Undo | one ChangeHistory waypoint per operation (TransformAction, FillAction) | one undo step per change: each handle drag, field edit, quarter turn, Paste, Cut, Delete, Fill |

A Move / Resize / Rotate keeps what the box held when it was lifted and
the grid from before, so dragging again re-places the same terrain
(no merge build-up, no repeated resampling). Anything else changing the
document (undo included, `Document::revision`) ends it; the next drag lifts
what the box holds then. Resize stretches by the nearest cell. The box is
drawn without depth, like Studio's selection box.

### Engine (runtime_ext.rs)

| Member | Behaviour |
|---|---|
| Terrain:CopyRegion(Region3int16) | a TerrainRegion: ExtentsMin / ExtentsMax (cells, Max exclusive), SmoothGrid holding the cells from voxel (0, 0, 0); only the part with terrain is stored, so CopyRegion(MaxExtents) is cheap |
| Terrain:PasteRegion(region, corner, pasteEmptyCells) | the cells from `corner`; empty cells clear the box only with pasteEmptyCells; stored bytes copied as they are (no occupancy rounding) |
| TerrainRegion.SizeInCells / IsSmooth | ExtentsMax − ExtentsMin / true |
| Terrain.MaxExtents | Region3int16 −32000 … 32000 on each axis |
| ReadVoxelChannels / WriteVoxelChannels | SolidMaterial, SolidOccupancy, LiquidOccupancy (a Water voxel reads as Air with liquid; a part-full solid keeps shoreline water), resolution 4, with Size |
| CellCenterToWorld / CellCornerToWorld / WorldToCell(PreferSolid / PreferEmpty) | (cell + 0.5) × 4 / cell × 4 / floor(pos / 4) |

`terrain_region.rs` holds the region type and its operations;
`Voxels::get_raw` / `set_raw_many` (terrain.rs) move stored bytes. Tests:
`terrain_region::tests` (round trip, paste empty cells, rotate, resize,
fill / replace), `terrain_editor::tests::region_*` (Move with and without
Merge Empty, Duplicate, box turns), `runtime::ext::tests::terrain_regions_and_channels`,
and the self-test step `terrain-region` (a Select drag on the ground, Copy,
Delete, a Move drag on the +X arrow, a quarter turn, Fill, Paste, each one
undo step that undoes to the same grid byte for byte).

Not measured: whether Studio's Region3int16 Max is inclusive (rustudio:
exclusive, as the plugin's ceil suggests), and whether a saved
TerrainRegion's SmoothGrid holds region or world cells (rustudio writes
region cells and reads either). Not built: Studio's Live Edit preview
while dragging (the terrain follows on release), Import, the plugin-only
TerrainRegion methods (ApplyTransform, SetMaterialInTransform…), and
free-angle rotation.

## Round 25: terrain light grid, LOD

As Studio's terrain shaders (SmoothCluster VS / FS
HQ, SmoothWater FS HQ) and compared with the saved an offline Studio
captures (`terrain`, `place-b-terrain`, `place-c-terrain`; no new Studio runs).

### Water seen from under water

Place B's lake holds air pockets (for example x −8396, z 1230–1276, y 64–116,
next to the F4 camera). Their walls drew as dark-green and teal sheets.
Studio draws none of them in F4, but does draw the surface over the camera
(terrain Place A).

- **Rule.** With the camera under water (fog.z 2), a water face is drawn only
  when it's the surface over the camera: outward normal y ≥ 0.5
  (`WATER_INSIDE_MIN_UP`), seen from below. This is in both `fs_water` (HQ)
  and `fs_part` kind 5 (LQ).
- **F4.** MAE 26.7 → 19.4, the same as drawing no water at all (19.5).
- **Place A.** Unchanged.
- **Not checked.** Whether Studio really keeps these voxels as air: ReadVoxels
  wasn't run in Studio because the user's own Studio was running.

### Far terrain LOD

- **Studio.** Its SmoothCluster VS has a LOD seam step. Vertices outside the
  chunk box CB10[0..1] are moved along a per-vertex direction (CB12, chosen
  by POSITION.w >> 8) by CB10[0].w, toward the camera's side, with a depth
  nudge of × 0.99998. Far terrain is drawn from coarser chunks whose borders
  are pulled onto the finer ones.
- **rustudio.** It draws every 32³ chunk at full detail with a frustum test
  per chunk. Place B has 465 236 triangles (2.78 M voxels).
- **Measured.** The Place B Play bench's `viewport` section is 5.1–6.6 ms per
  frame with or without this round. The machine was at load 12–25, so the GPU
  side isn't separable. At F5 (overview) and T9 (far view) the differences
  from Studio aren't the terrain's detail:
  - F5: downloaded meshes Studio lacks.
  - T9: the skybox's Dn face. It draws as a lighter quad across the bottom of
    the view, where Studio's sky continues the side faces.
- **Not built.** A LOD would cost seams and popping for no measured gain on
  this GPU. Revisit if a big place shows the terrain pass in a GPU profile.

## Round 25: particles, beams, trails

ParticleEmitter, Beam and Trail were not drawn at all before this round.
`src/effects.rs` simulates them on the CPU each frame (Edit and Play, the
document the viewport shows) and hands their triangles to the round 23
world GUI pass (`world_gui.rs`). That pass tests them against the scene's
depth, lights them, and applies the tone curve and fog. Measured in the new
`effects` place (make_effects_place.py, Studio captured once) and on
Place B from fixed cameras (`place-b-fx`, place_b_fx.luau).

### Studio's shaders

The pack's index (see Round 23) names them: ParticleCustomVS (sources 2593 /
2594), ParticleCustomFS (74), ParticleLightingVS / FS (76 / 77), TrailVS /
FS, TrailVSQuad / FSQuad and their `Light` variants (18–25), all read by
name, not by sampler.

| What | Studio's shader | rustudio |
|---|---|---|
| Particle colour | t = texture.rgb × texture.a (two flipbook frames mixed by the blend fraction), × vertex colour; lin = t² | egui's premultiplied texture × vertex colour, unpremultiplied, squared |
| Particle light | mix(lin × Brightness, lin × LightingAtlas × 120, LightInfluence). The atlas is ParticleLightingFS: min(LightMap + Ambient + SkyAmbient × sky, cap) + Lamp0 × sky, × 1/120, sampled 6 studs toward the camera. There is no N·L and no shadow | `Look::light`: Brightness × (1 − LI) + LI × `gui::scene_light(d, None)` (Ambient + OutdoorAmbient + the whole sun). Pixels: LI 1 grey reads 111 (Studio 109), Brightness 3 reads 209 (208) |
| Trail / Beam light | TrailFSLight: the same mix with Params.z = LightInfluence and Params.w = Brightness. The light is LightMap + ambient + sky, plus Lamp0 × shadow × sky, also without N·L | same model; no shadow |
| Fog | alpha × clamp(exp2(FogParams.z × d + FogParams.x) − FogParams.w). Effects fade out; they are not tinted toward the fog colour | `fs` in world_gui.rs: alpha × (1 − fog amount) for effect vertices |
| Out | rgb = sqrt(clamp(lin × exposure)) × alpha; a = alpha × CB1 (1 − LightEmission) | premultiplied, alpha × (1 − LightEmission) (`GuiVertex::alpha_mul`); LightEmission 1 = additive |
| Tone | Studio blends before its tone curve | the tone curve of the premultiplied colour, tonemap(lin × a²). This is exact over black and when opaque. Red at Transparency 0.5 with LightEmission 1 reads 146 against Studio's 166; tone(c) × a read 126 |
| Corners (VS) | c ∈ [−1, 1]²; squash factors A, B = (1 − s, 1/(1 − s)) for s ≤ 0, (1/(1 + s), 1 + s) above; offset = X × (c.x cos θ A − c.y sin θ B) × size + Y × (c.x sin θ A + c.y cos θ B) × size | same (`particles_draw`) |
| Size | TEXCOORD5 is the half-extent | Size is the quad's half-width: a Size 4 SquareParticle is 71 px wide in Studio and in rustudio (35 px with Size as the full width). `FX_PARTICLE_HALF_SIZE` |
| Orientation | FacingCamera: ViewRight / ViewUp. FacingCameraWorldUp: ViewRight / world Y. VelocityParallel: (v̂, normalize(v̂ × ViewDir)). VelocityPerpendicular: from the emitter's rotation (CB1[4] quaternion): B = normalize(Rx × v̂), or Ry with its v̂ part removed when that is short; A = v̂ × B | same |
| UV / flipbook | uv = (c01.x, 1 − c01.y) / layout + (frame mod cols, floor(frame / cols)) / layout, row-major from the top left | same; Grid2x2 / 4x4 / 8x8, Custom = FlipbookSizeX × Y. Modes: Loop, OneShot (over the lifetime), PingPong, Random (a new frame each tick) |
| ZOffset | particles: only the depth moves (clip z of the point ZOffset studs toward the camera, × its own w). Beams: the whole position moves (POSITION + view dir × ZOffset) | same |

### Simulation (no shader: Roblox's documented behaviour, partly estimated)

- **Spawning.** Rate × TimeScale per second, plus `:Emit(n)` (default 16;
  it emits even while the emitter is disabled). `:Clear()` drops the live
  particles. Both go through `__rs_particles` to a queue keyed by
  document id. Lifetime, Speed, Rotation, RotSpeed and FlipbookFramerate
  are each drawn uniformly from their NumberRange. Each particle also draws
  one envelope offset (−1..1) per sequence (Size, Transparency, Squash).
- **Shape** (in the host part's frame; an Attachment host is a point):
  - Box: anywhere in the part (Volume), or on its faces weighted by area
    (Surface). Particles move along EmissionDirection.
  - Sphere: in or on the ellipsoid, moving out along the radius.
    ShapePartial keeps a cap of that share of the sphere around
    EmissionDirection.
  - Cylinder: the axis is EmissionDirection and particles move out from
    it. ShapePartial is the top radius's share.
  - Disc: lies across EmissionDirection and particles move along it.
    ShapePartial is the inner radius's share.
  - Inward flips the direction; InAndOut flips it at random.
  - SpreadAngle X and Y turn the direction about the two axes across it.
  - The Cylinder and Disc conventions are estimates.
- **Motion.** Acceleration is in world space, or turned into the part's
  frame when LockedToPart. Drag halves the speed every 1 / Drag seconds.
  VelocityInheritance × the host's velocity comes from its position
  change. LockedToPart particles live in the host's frame.
- **Texture.** A ParticleEmitter with no Texture draws nothing (effects
  place: Studio shows no squares). Beams and Trails without one draw flat
  colour.
- **Beam.** A cubic bezier: P1 = A0 + A0.Axis (X) × CurveSize0,
  P2 = A1 − A1.Axis × CurveSize1. It has Segments pieces (1..1000).
  - Colour, Transparency, Width0 → Width1 and the texture run by arc
    length, not the curve parameter. On a straight beam the bezier bunches
    up at the ends; the effects place's arrow was 23 % too long until this
    changed.
  - Width lies across the camera when FaceCamera is set. Otherwise it lies
    along the attachments' SecondaryAxis (Y), blended between the two
    attachments and squared to the curve. Place B's flat conveyor and
    waterfall beams lie flat that way.
  - The image's Y runs along the beam from Attachment0 and its X runs
    across, X 0 on the +side. The arrow cursor lies on its side in Studio.
  - TextureMode:
    - Stretch: TextureLength copies over the beam.
    - Wrap: one copy every TextureLength studs from Attachment0.
    - Static: the same, from Attachment1 (estimated).
  - TextureSpeed scrolls the texture toward Attachment1.
- **Trail.** A point pair (the two attachments) is laid each frame once
  the midpoint has moved MinLength. Points expire after Lifetime, and
  MaxLength trims the oldest. The live attachments form the head.
  - Colour, Transparency and WidthScale run over the age (0 at the head).
    WidthScale scales about the midpoint.
  - FaceCamera turns the width across the camera.
  - TextureMode:
    - Stretch: over the lifetime.
    - Wrap: every TextureLength studs back from the head.
    - Static: fixed where the trail was laid.
  - Disabling a Trail stops new points; the old ones fade out.

### Costs and caps (mech.rs `FX_*`)

- Effects farther than 1024 studs are not simulated (their particles are
  dropped).
- Each emitter holds at most 2000 particles (estimated; round 26: 10 000,
  and at most 500 made a second, measured). Studio has a particle
  budget (FRMParticleBudget* flags, values not found). Place B's Snowfall is
  Rate 5000 × 3–5 s, and Studio shows none of its flakes near F7's camera.
- At most 20 000 particles are drawn a frame, nearest emitters first.
- A particle under 0.35 px is skipped.
- Particles are sorted far to near within an emitter, and effects far to
  near among themselves, after the world GUIs.
- The properties are cached per instance and read again from the
  journal's marks (`Reader::Effects`); transforms are read live.
- `RUSTUDIO_NO_FX=1` leaves effects out, to measure their cost.

### Numbers

`fidelity.py run --skip-studio --skip-perf` on the same Studio captures:
before is the build without effects, after is this round's.

| Place | Render before | Render after | Notes |
|---|---|---|---|
| effects | 75.6 | 86.6 | worst MAE 15.6 → 8.6 |
| place-b-fx | 56.9 | 56.4 | MAE per shot: waterfall 27.2 → 24.5, stage lights 20.3 → 20.1; conveyor 18.5 → 22.8 (rustudio lights the beam with the open sky, where Studio's light grid darkens it); clouds 39.5 → 40.4 (Studio shows no clouds there) |
| gui-depth | 87.0 | 87.0 | the world GUI pass changes leave GUIs alone |
| place_b | 43.0 | 42.7 | within its spread (3.2) |

Place B Play bench, one binary, `RUSTUDIO_NO_FX=1` against effects on,
interleaved, at load 12–19: `fx` costs 0.34–0.41 ms a frame and
`gui_world_pass` 0.1 ms more.

### Open

- Flipbook frame blending (FlipbookBlendFrames). Studio mixes two frames;
  rustudio shows one.
- Soft particles (the `_Soft` vertex shaders).
- Local lights, the light grid and shadows on effects.
- Server-side `:Emit` doesn't reach the client's view (the queue is per
  DataModel).
- Trail colour: Studio's white → green trail turns green sooner than
  rustudio's.
- Particles behind glass are drawn over it, not tinted by it: the pass
  runs after the transparent parts.
- Beam:SetTextureOffset.
- Place B's Static conveyor beam (TextureLength 5) shows about 1.5× as many
  chevrons as Studio. Static / Wrap spacing isn't measured.
- The asset report still lists effect textures as "not requested": they
  load through `Gui::texture`, which it doesn't track (fixed in round 26).

## Round 26: particles

Round 25's leftovers. New corpus place `effects-lanes`
(make_effects_place.py, captured in offline Studio, D3D11): two screens of
still particles at Rate 1000 and 100, the Cylinder and Disc shapes, four
flat arrow beams (Stretch, Wrap, Static at Width 2 and 4; TextureLength 4
over 15 studs), and three trails on parts moving at a steady 6 studs/s
(colour only, transparency only, both), so distance from the head is age.
Its Studio Output lost the FIDCAP lines in the first three captures (the
log stops before them; capture.luau's run_luau answered "Done", so the
lines were lost on the way, not by an error: fidelity.py now keeps each
script's answer in studio/luau-results.txt); the fourth kept them and the
harness scores it. The shots below were read with small scripts.

### Studio's shaders (GLSL3, D3D11 and Vulkan)

The pack's 64-byte source records hold the blob of the *next* record's
name: the source listed as ParticleLightingFS is a generic VS, and
ParticleLightingVS's is the lighting FS. Read by contents.

| What | Studio's shader | rustudio |
|---|---|---|
| Flipbook blend | ParticleCustomFS: mix(t0.rgb × t0.a, t1.rgb × t1.a, f) and mix(t0.a, t1.a, f); ParticleCustomVS: TEXCOORD7 = (frame, next frame, f), both frames' UVs from the same corner | `GuiVertex::uv2` / `blend`: both frames sampled, egui's premultiplied texels mixed. FlipbookBlendFrames on: Loop / PingPong blend toward the next frame, OneShot toward the next until the last; Random doesn't |
| Soft particles | Only in Studio's D3D11 and Vulkan shaders (the offline Studio's Studio runs D3D11: FFlagDebugGraphicsPreferD3D11). ParticleCustomVS_Soft: VARYING3 = (screen uv, min(w − ZOffset, 500 − fade), 1 / fade), fade = TEXCOORD5.x, the particle's half size. ParticleCustomFS_Soft: only when Orientation is 0 (FacingCamera), alpha × clamp(|depth × 500 − z| / fade); the depth texture holds view depth / 500 | `GuiVertex::soft` (1 / half size, ZOffset) for FacingCamera particles; world_gui.rs reads the scene's depth as view depth (near / reverse-Z depth), capped at 500 |
| Particle light | ParticleLightingFS: min(LightMap × 120 a + Ambient + SkyAmbient × skylight.x, cap) + Lamp0 × skylight.y, at the centre + 6 studs toward the camera (ParticleLightingVS) | `SceneLight::at` per particle, on the CPU, at the same point |
| Trail light | TrailFSLight: the same at the pixel, Lamp0 × (1 − shadow) × skylight.y | per strip vertex |

### What changed

| Item | Measured / read | rustudio now |
|---|---|---|
| Flipbook blend | the shader (above) | blended |
| Soft particles | the shader (above) | faded where they meet the scene |
| Light | the shaders (above) | `effects::SceneLight`, kept by the viewport from the frame's lighting (`LightingState::fx`): Ambient, OutdoorAmbient and the sun × the roof map × the terrain light grid, the skybox's diffuse light × the light grid (as parts take it), and the frame's local lights without N·L (shader.wgsl's point / spot / surface and Soft falloffs, ported). No shadow map on the CPU: the roof map stands in for Studio's voxel sun visibility. Only lights within reach of an effect's bounds are tried |
| Behind glass | Studio sorts blended parts and effects together, far to near, by object | each particle / strip vertex is dimmed by (1 − opacity) of every see-through part (`Effects::glass`) whose box the camera ray crosses before it, if that part is nearer the camera than the effect (Studio's object order) and isn't the effect's host. Over the drawn view that is the glass drawn over the effect, up to ga × (B − G) |
| Server `:Emit` | Roblox replicates it | `FrameIn::server`: on the client in Play, the server's requests are mapped to the client's copies (`RepMap::s2c`) |
| Trail colour | colour lane: Studio's white → green ramp reads to ±2 levels as Color at (age + 0.12 s) / Lifetime; the trail is 0.7 studs short (5.3 of 6). Fade lane: Transparency fits ±2 levels over the first two thirds at (age + 0.06 s) | `FX_TRAIL_AGE_LEAD` 0.12 (Color, WidthScale, Stretch, and points expire that much early), `FX_TRAIL_FADE_LEAD` 0.06 (Transparency). Colour lane now matches to ±2 levels head to tail (was 10–15 bright mid-trail) |
| Beam Static | Static draws exactly as Wrap: one copy every TextureLength studs from Attachment0, the cut copy at Attachment1, the same at Width 2 and 4 | Static = Wrap (was anchored at Attachment1) |
| Conveyor chevrons | Place B F6, from the rows of the chevrons and the camera's height above the beam: one every 5.0–5.4 studs in Studio, 5.0 in rustudio (TextureLength 5). The "1.5×" doesn't reproduce: the chevrons scroll (TextureSpeed 3) and their phase differs | spacing unchanged |
| Asset report | effect textures load through `Gui::texture` | `Gui::image_state` is consulted: place-b-fx's 20 effect textures read "loaded" (230 → 206 "not requested" lines) |
| Particle limit | Rate 1000 screen: coverage 40.7 % at 6 s, 66.9 % at 12 s; Rate 100: 10.5 % / 21.8 %. Calibrated on the Rate 100 screen (coverage = 1 − exp(−N k)), Rate 1000 holds ~2700 particles at 6 s and ~5700 at 12 s: ~470 / s, still rising, no live cap. The light over each screen (mean above the wall) now matches: 46.6 / 83.7 against Studio's 44.7 / 80.3, and 10.1 / 19.8 against 10.5 / 21.6 | `FX_MAX_RATE` 500 a second; `FX_PARTICLES_PER_EMITTER` raised 2000 → 10 000 (a guard). Place B's Snowfall (Rate 5000, 3–5 s) holds ~2000 either way; why Studio shows none of it near F7's camera is still open |
| Cylinder | Top and Front both draw an upright 6 × 4 block from a 6 × 4 × 2 part; Surface fills the caps too; the moving Surface lane spreads ±4 studs in Y (off the caps) and out from the axis | axis = the part's Y whatever EmissionDirection, radii X / Z; Surface picks side or caps by area and moves along their normal; Volume moves out from the axis (estimated) |
| Disc | Front draws a filled 6 × 4 ellipse; Right draws the same ellipse drifting +X (mean +1.1–1.5 studs at Speed 2, 1.2 s); ShapePartial 0.5 leaves a hole of half the radius, 1 none | the part's XY plane, radii X / Y, moving along EmissionDirection; filled from the rim in over ShapePartial of the radius |

Not changed: Studio's particles look larger and softer than rustudio's at
2–4 px (its textures are mipmapped, egui's aren't): the same light spread
over ~2× the area.

### Costs

Place B Play bench, one machine under other agents' Studio captures (fps
not comparable between runs; load 11–23): two runs each, round 25's
build against this one, `fx` 0.43 / 0.67 → 1.02 / 1.16 ms a frame, plus
`fx_glass` (collecting the see-through parts) 0.27 / 0.32 ms and
`gui_world_pass` (the soft particles' depth) +0.04–0.11 ms; with
`RUSTUDIO_NO_FX=1` all three are gone. Frame time moved with the load, not
the build (19.5 → 18.5 ms on the quieter pair). The glass is
capped (`FX_GLASS_MAX_DIST` 256 studs, the nearest `FX_GLASS_MAX` 512) and
binned into 16 × 16 screen tiles per effect when more than 8 lie in front
of it (Place B's Snowfall box spans hundreds); before that it cost 1.4 ms.

### Numbers

`fidelity.py run --skip-studio --skip-perf` on the same Studio captures,
the round 25 build (before) against this round's, one after the other:

| Place | Render before | Render after | Notes |
|---|---|---|---|
| effects | 86.6 | 86.8 | MAE 8.4 / 8.5 → 8.3 / 8.5 |
| place-b-fx | 56.4 | 56.9 | MAE per shot: waterfall 24.5 → 24.9, fire 17.4 → 17.3, clouds 40.4 → 40.2, conveyor 22.8 → 20.4; the rest within 0.1 |
| place_b | 43.1 | 43.1 | unchanged |
| gui-depth | 87.0 | 87.0 | unchanged |
| effects-lanes | 77.8 | 79.0 | new place; worst MAE 14.2 → 13.4 |
| place_d | 47.4 | 50.7 | its run-to-run spread is ~3 (round 25 scored 49.6) |

Place B's Snowfall, read in Play: `Resize Me`, an anchored invisible box
386 × 103 × 358 studs around F7's camera, Box Volume, Rate 5000, Lifetime
3–5 s, Size 0 → 0.94 → 0, Transparency 1 → 0 by 5 % of the life,
LightEmission 1, LightInfluence 0. ~2000 flakes over 14 M studs³ is one per
~7000 studs³: rustudio shows a handful of 1–3 px flakes at F7, Studio
none.

### Open

- Why Studio shows no Snowfall flakes near F7's camera (the rate cap
  leaves ~2000 alive in both).
- Place B F6's covered conveyor: Studio lights its chevrons at ~0.2 of the
  open sky (green 117); the roof map puts it under a part 140 studs up, so
  rustudio lights it with the skybox's diffuse light alone (green 185).
  Studio's skylight voxels see past distant roofs; the roof map doesn't.
- Mipmapped effect textures.
- Beam:SetTextureOffset; the Cylinder's Volume motion and the Disc's
  Surface style are estimates.

## Round 25: misc

### Old built-in content (Place B's swords, rocket launcher, Merlin's limbs)

Studio no longer ships `content/fonts/*.mesh` and most old `content/textures/*.png`; none of the 337 files in its table are in content/ or PlatformContent/pc. The offline Studio prints `ContentId: rbxasset://fonts/sword.mesh failed due to: Request asset was not found`, its mock's answer for an asset id it doesn't have.

| Path | Asset id |
|---|---|
| fonts/sword.mesh | 12221720 |
| fonts/torso.mesh, leftarm.mesh, rightarm.mesh | 12221758, 12221505, 12221614 |
| fonts/rocketlauncher.mesh, clonewand.mesh | 12221651, 12221344 |
| textures/SwordTexture.png, rocketlaunchertex.png | 12224218, 12224070 |

rustudio: the table is `assets/legacy-content.tsv`; `assets::asset_ref`
turns a table path that isn't on disk into its asset id (matched without
case, as on Windows), and `assets::resolve` hands such a path back to the
cloud path, so meshes, textures and sounds all download it. Place B Newer:
all 8 of the place's legacy files load (they failed before), and
`Textures/Sword128.png` matches too. `fill_asset_store.py` copied the
downloads into the offline Studio's store, so the reference Studio has them now.

### Require by string, and modules that return nil (Place D's second admin script)

another admin script failed with `Shared.Util.Suggest:4: Attempted to call require with invalid argument(s)`: Suggest does `require("./String")`. Studio's Luau requires by path.

| Path | Resolves to |
|---|---|
| `./X` | a sibling: script.Parent.X |
| `../X` | script.Parent.Parent.X |
| `@self/X` | a child: script.X |
| `@game/S/X` | game.S.X (estimated: the string `@game/` is in the binary, but beside Studio's OpenScript paths) |
| `..` inside | the parent |

A child is found, not waited for. rustudio: `require_path` in the runtime's
prelude, with the requiring script read from the calling function's
environment (`getfenv(2).script`).

The next error was `Flux.State:5: Module code did not return exactly one
value`. Flux.Type is types only and ends `return nil`. Roblox's rule is a
value count, as the message says: one value, nil or not, passes; no
`return` (or two values) fails. rustudio rejected any nil. It counts now,
and caches a nil result like any other.

another admin script then ran into three engine gaps:

| Call | Studio (API dump / docs) | rustudio before | now |
|---|---|---|---|
| `InsertService:CreateMeshPartAsync(id, CollisionFidelity, RenderFidelity)` | the two enums as arguments (AssetService's takes an options table) | a stub returning nil: `attempt to index nil with 'AddTag'` (AlphaWings, Crown, Fun's sphere) | the AssetService version, either argument form |
| `GroupService:GetRolesInGroupAsync(userId, groupId)` | a Variant; another admin script reads `info.Roles[1].Rank` | nil: `attempt to index nil with 'Roles'` | `{ Roles = {} }`, not in the group (as `GetRankInGroup` gives 0) |
| `part.TopSurface = 0` | an enum property takes its number | `EnumItem expected, got integer` (mlua hands whole numbers over as integers) | integers accepted (and for string properties) |

Place D's Output now matches Studio's: `[another admin script] Initializing DataStore
_KData...` in both, no errors; the ADMIN button shows. Test:
`require_by_string_and_nil_modules`.

### World GUI LightInfluence 0 (round 23's open item)

The "199 in rustudio, 151 in Studio" probe was pixel (500, 355) of
gui-depth's 6 s shot, on the anti-aliased right edge of the "I" in HIDDEN,
where Studio's glyph sits about 1 px from rustudio's. The label's fill reads
(31, 199, 31) in Studio and (31, 200, 31) in rustudio on every row and in
every shot, which is what Studio's `UILitFogFSRRectClip` gives at
LightInfluence 0 (lin × Brightness, then the tone curve). There was no light
model error; `world_gui_light` is unchanged. Still different there: the label's 1 px
border. Studio's top row is a blend (G 124), rustudio's a full dark
border (G 22).

### Fidelity corpus: place_d, place-b-newer

- Both are place files in `projects/rustudio/places`, read where they lie;
  Studio gets a /tmp copy.
- Fixed cameras: `place_d_cams.luau` and `place_b_newer_cams.luau`.
- Material packs are off (`RUSTUDIO_MATPACKS=0`), as in the terrain set.
- Place D's vote ends about 23 s in and its next map is random, with a cutscene
  camera, so its shots fall at 6–19 s.
- `fidelity.py` run from a worktree (`app/.worktrees/NAME`) now
  reads and writes the main checkout's `checks/`. It used to write to
  `app/.worktrees/checks`.
- Script timing on a big place: the harness sends Place B Newer's camera
  script to Studio 2 s after Play. Studio ran it 8–14 s late, and Studio's
  `time()` starts late as well (its spots trailed rustudio's by 8 s on
  os.clock and by 4–16 s on time()). `place_b_newer_cams.luau` therefore
  holds each spot for 16 s from the script's own start, and the shots fall
  15 s into each window (17, 33, 49, 65, 81 s), so any lag up to 15 s sees
  the right spot. Place D's light place lined up with 1 s holds.

Place B Newer also showed two small gaps:

| What | Studio | rustudio before | now |
|---|---|---|---|
| `http://www.roblox.com/asset/?ID=94746105` (upper-case query) | loads (sword model's texture) | "not a local or cloud asset id": the only asset regression | `asset_ref` matches the query without case |
| `IntValue.Value = "042917"` (FrenchBinary's code) | takes the numeric string | `number expected, got string` | number properties take numeric strings, as `tonumber` |

Scores (`fidelity.py run`, Studio captured once each, round 0, not
recorded in score.json):

| Place | render | lighting | materials | scripting_api | camera_controls | gate |
|---|---|---|---|---|---|---|
| place_d | 45.0 | 100 | 100 | 99.9 | 75 | PASS (pixel gate off: the vote board's map cards are random; worst MAE 55.0 at the lobby) |
| place-b-newer | 57.3 | 100 | — | 99.9 | 50 | PASS at max_mae 48 (worst 45.5, N5) |

## Round 26: sky and lighting

Place D (night, Atmosphere) and Place B Newer (noon, ShadowMap,
EnvironmentSpecularScale 1) against Studio. Calibration places (not in the
default pass): `atmos` (make_atmos_place.py, atmos_cams.luau), `sa-mesh` and
`sa-far` (make_sa_place.py), `place-b-dome` (place_b_dome.luau) and
`place_d-lobby` (place_d_lobby.luau).

### Atmosphere (`lighting::atmosphere`, mech `ATMOS_*`)

Place D's night sky was black with stars and its ground black; Studio's is a
blue band over the horizon and the ground reads blue. Fitted on `atmos`
(white walls at 25–1600 studs, Place D's night lighting and 14:00):

- The haze band at the horizon is Atmosphere.Color² × (0.208, 0.527, 0.760)
  before the tone curve, by day and by night alike (the sun doesn't light
  it). Its strength is 1.05 × (Haze / 1.2)^3.6 at the horizon, ÷ (1 +
  (elevation / 3.09°)^1.61) above it, capped at 1, over a 2.75 % floor
  everywhere (even Haze 0). Stars fade under it, and each star now has its
  own brightness, most of them faint (Studio's median star reads 28).
- With an Atmosphere, FogStart / FogEnd don't apply: objects fade into the
  sky behind them (the sky colour along the view ray, as Studio's FS samples
  its cube when FogParams.w is 0), visibility e^(−k·d), k = 0.149 ×
  Density^4.2 per stud. Offset, Haze and Decay changed nothing there; Glare
  needs the sun in view and isn't modelled.
- At night the moon (−sun direction, as GetMoonDirection) lights the scene
  at 0.03 × Brightness × N·L, tinted like the sun, fading in as the sun sinks
  0.1 below the horizon; it casts the shadows then.

### Place D's lobby (10 s)

Not a lighting difference: Studio's 10 s shot came 1 s after the camera
jumped there from the Mall, before Studio's lights caught up (held at the
lobby from the start, `place_d-lobby`, Studio's lobby is lit at every shot from
6 s, as rustudio's). `place_d_cams.luau` now holds each spot from 3 s before
its shot, and the shots moved to 6, 9, 12, 15, 18, 21 s (the vote still
ends ~23 s in).

### Place B Newer's sky per area

Lighting holds one Sky (Studio's default faces, 3000 stars); no client
script changes the Sky or Atmosphere at the camera spots (the JToH lighting
changers fire on touch, PyramidGemClient swaps the Sky only during its
event). What differs per area are sky domes: 2048-stud sphere MeshParts
(mesh 1527559) with a cloud SurfaceAppearance (rbxassetid://18252487167),
HallowsVille.HallowsvilleSky (Color (213, 115, 61), N5) and
HallowsVille.WitchSky (Color (185, 196, 177), N4), plus JohnDoeSky and two
more. Studio draws them in their plain Color (N5 orange, N4 pale), even with
the SurfaceAppearance's Color white or the SurfaceAppearance removed;
rustudio drew the clouds. The same mesh and SurfaceAppearance do show their
clouds in Studio on small copies (`sa-mesh`) and at 100–1200 studs and from
inside a 300-stud copy (`sa-far`). With two Skys in Lighting, Studio draws
the first (`sa-mesh`: SkyA empty, then SkyB clouds: the default sky shows),
as rustudio does.

**Cause: the TexturePack.** These SurfaceAppearances were saved with a
TexturePack (newer Studio packs the maps into one asset), and Studio's log
reads "Failed to fetch TexturePack in Workspace.HallowsVille.HallowsvilleSky.
SurfaceAppearance" (likewise WitchSky, JohnDoeSky and 300 more): the
offline Studio reads the pack instead of ColorMap, can't fetch it, and draws
the part as if it had no SurfaceAppearance. Size has nothing to do with it
(`place-b-dome`: shrunk to 300 studs and 500 studs ahead, then 1200 and 2048,
the dome stays plain orange at every step), and the copies in `sa-mesh` /
`sa-far` show clouds because they carry no TexturePack. rustudio now does
the same while material packs are off (`RUSTUDIO_MATPACKS=0`, as every
corpus run against the offline Studio): a SurfaceAppearance with a
TexturePack draws nothing (`scene::sa_unloaded`). With packs on it keeps the
ColorMap, the look online.

### Sun highlight and sky reflection (ShadowMap / Future parts)

Studio's ShadowMap / Future part FS adds to every
opaque part, with roughness r = 0.089 + 0.911 × a per-material value:

- the sun's GGX highlight, F · (a + a²) / (d² (3 L·H + 0.5)(0.75 N·H +
  0.25)) · N·L · shadow · sun, a = r⁴, d = (N·H · a − N·H) N·H + 1, F
  Schlick with F0 0.04; not scaled by EnvironmentSpecularScale;
- the sky's reflection, the prefiltered cube at mip r × 5 along the
  reflected ray (the indoor cube where the skylight is 0), times (0.04 A +
  B) / (A + B) from PrecomputedBRDFTexture(r, N·V), times
  EnvironmentSpecularScale; the diffuse sun and sky light lose that Fresnel
  share times the scale.

rustudio draws both with r = 0.5 for every material
(`mech::ENV_SPEC_ROUGHNESS`, estimated; the per-material table isn't read
yet), the env BRDF from the analytic fit (Karis), and the reflection from
the skybox's own mip chain (mip coarsest − 5 × (1 − r)). Place B Newer's N1
road (Color (0, 21, 31)) was navy (16, 41, 64) where Studio shows (49, 68,
86); now (45, 66, 89).

### Sky visibility under meshes, CastShadow

- Parts read the sky-visibility map 6 studs out along their normal
  (`mech::SKYVIS_NORMAL_OFFSET`), as Studio's part VSs place their
  light-grid lookup at pos + normal × 6. A building's walls were roofed by
  its own roof (only the cell's vertical line was checked), so they lost
  OutdoorAmbient: Place D's lobby walls and Place B Newer's conservatory.
- Roofs more than 64 studs over the camera roof nothing
  (`mech::SKYVIS_ROOF_ABOVE`): Studio's light grid is 128 studs tall around
  the camera (round 25's estimate), and Studio lights the conservatory
  under WitchSky, whose top is 620 studs over the camera. The map rebuilds
  when the camera moves 16 studs up or down.
- A MeshPart or union now roofs a sky-visibility cell only where the
  cell's vertical line meets its shape (16³ voxels per loaded mesh,
  `assets::MeshVox`), not its whole box: Place B Newer's spawn wheel, a ring
  of 58-stud vertical unions, roofed the road beside it. Meshes that haven't
  loaded (drawn as nothing) roof nothing; the map rebuilds as meshes load.
- BasePart.CastShadow false keeps a part out of the sun's shadow map.

### Scores

Saved Studio captures, `--skip-studio`; "before" is the round 25 binary on
the same captures (place_d recaptured once at its new times; its vote board
cards are random, so its render moves ±2 between runs):

| Place | before render (worst MAE) | after |
|---|---|---|
| place_d | 62.8 (30.8) | 66.7–68.8 over 3 runs (worst 22.7–24.3) |
| place-b-newer | 55.8 (45.5, N5) | 79.4 (17.5); N4 31.0 → 5.5, N5 45.5 → 11.9, N1 29.3 → 17.5 |
| place_b | 43.1 | 66.3 |
| control | 79.9 | 83.0 |
| demo | 77.7 | 78.4 |
| place_a | 81.7 | 81.7 |
| place_c | 84.2–96.5 (a planet lands at random) | 83.1–96.6 |
| terrain | 83.0 / 83.4 | 83.0 / 83.4 |

Lighting and materials stay 100 everywhere. bloom, lightcap-soft and
lightcap-real have no valid Studio capture (studio_valid false before this
round too), so they score nothing. Place D's animation score reads 0 against
the new capture: that Studio session had no character yet at the capture
(parts_studio 0), not a rendering change.

### Open

- Materials' roughness: every material uses r 0.5 for the highlight and
  the reflection; Studio reads a per-material value (VARYING4.w) from a
  table not found yet.
- The reflection's outdoor cube is tinted in Studio by mix(CB0[31],
  CB0[30], reflected y × 1.588); what those hold isn't known, so it's left
  out.
- Place B Newer N1: the "Realistic Tree" by the left road draws dark blue
  where Studio's is lit yellow-green (a materials question).
- N4's conservatory front still has rustudio's glowing spots (point lights
  behind glass) that Studio blurs to almost nothing.
- Atmosphere Offset, Decay and Glare aren't modelled.

## Round 26: translucency, local lights, bloom

The four differences the person pointed at in round 25's side-by-sides
(Place B Newer N3 / N4, Place D D5 / D6), measured in an offline Studio with
new calibration scenes (`tools/fidelity/make_lightwall_place.py`:
`lightwall-soft` / `-real`, `lightfocus-soft` / `-real`, `lightreach-soft`
/ `-real`, all `"calibration": true`) and probes (`place-b-newer-probe`:
probe_dome.luau; `place_d-probe`: probe_focus.luau, probe_hide_gui.luau).
The lightwall scenes: at night, Ambient 100, over a (128, 128, 128) floor
from a fixed camera above: two closed 16 × 10 × 16 rooms (1-stud walls and
roof) with a PointLight inside, Shadows on (RoomS) and off (RoomN); four
16 × 8 walls with a PointLight 3 studs behind, 1 / 0.2 / 4 studs thick with
Shadows on and 1 stud with it off; a SpotLight at a wall; a room lit by a
SurfaceLight; cyan (60, 220, 255) Neon panels at Transparency 0, 0.5, 0.86
and three stacked at 0.86.

| What | Studio (measured) | rustudio before → now | Where |
|---|---|---|---|
| N3: the Dome (two shells, 1.7 studs apart) | outer: 352 Bright red SmoothPlastic panes, Transparency 0, **Reflectance 0.2**, on Bright red Marble frames; inner: Lily white SmoothPlastic panes T 0.3, R 0.2 on grey Marble frames (the same in Studio's client and rustudio's, probe_dome.luau). Studio's part shaders: light × mix(albedo, the sky map in the mirror direction (blurred by roughness, tinted by the sky gradient), Reflectance) (Studio's part shaders, e.g. the studs variant: `mix(albedo, PrefilteredEnvTexture(reflect) × mix(CB0[31], CB0[30], …), VARYING7.w)`): the sky in the red reads pink | Reflectance was ignored: opaque red → the same mix, the tint taken as 1 (N3 MAE 14.45 → 10.2). The map is the skybox indoors as well (a first try that went white under a roof lit Place A's chapel: render 81.7 → 77.3) | `fs_part` (Reflectance in the instance's extra.z, + 2 when hovered); `scene.rs` |
| N4: lights through walls | **Shadows on**: Realistic blocks hard (lightwall-real: every wall leaves the floor behind it at the ambient 31, RoomS 31 around, a 0.2-stud wall 30); Soft blocks through its 4-stud voxels: a closed room keeps the light in (floor 3 studs out 49 vs RoomN's 175, its wall 59 vs 186) but light gets round an open wall (floor in front of the 1 / 0.2 / 4-stud walls 180 / 184 / 135 vs 183 with Shadows off). Shadows off goes through everything in both | lights went through everything → a grid of how much of each voxel the built-in parts fill (`lightocc.rs`: 128 × 48 × 128 voxels, 4 studs Soft / 2 Realistic, centred 0.6 half-widths ahead of the camera, snapped to 8 voxels, filled on a worker, rebuilt at most every 0.5 s while parts change); the part and terrain shaders march each Shadows light from a voxel out along the normal to half a voxel short of the light (≤ 24 nearest samples, exp(−20 × studs of solid)); Soft also takes the way 8 studs over the light and the surface. lightwall-soft MAE 10.65 → 9.21, lightwall-real 5.22 → 2.99; N4's blobs are gone (Studio keeps faint ones, +6 levels on its 52). Left out: parts more than half see-through, the lights' own parts, and MeshParts / unions (their box is no guide to their shape: as boxes Place A's chapel went dark, 81.7 → 77.3). Place B Play: `lightocc` 0.01 ms/frame | `lightocc.rs`, `light_occlusion` / `local_lights` in shader.wgsl, `sync_lightocc` in viewport.rs, `mech::LIGHTOCC_*` |
| Soft lights and Camera.Focus | lightfocus-soft: with the Focus 400 studs away the lights still shine, 1500 away every local light is out; a Scriptable camera whose CFrame is set without a Focus gets Focus = CFrame × 20 studs ahead (place_d-probe and lightfocus' last phase print it each second) | not modelled: rustudio lights all lights in range wherever the Focus is, and keeps an untouched Focus where it was (Place D's spawn). The harness views always have the Focus near the camera in Studio, so nothing in the corpus depends on it yet | probe_focus.luau |
| See-through Neon | adds a glow over what's behind without hiding it: red stays the floor's 31, green = blue 231 / 114 / 175 at T 0.5 / 0.86 / three stacked 0.86 (Soft and Realistic alike) | alpha-blended (hid the floor, added red) → glow max(4 × colour² − 1, 0), at most 1.2 per channel, × (1 − Transparency), premultiplied blending with cover 0: 200 / 121 / 164; with fog the glow fades instead of turning fog-coloured (Studio's DefaultUnifiedNeonFS covers by 1 − visibility × alpha) | `fs_part`, `mech::NEON_SEE_THROUGH_*`; the see-through pipelines blend premultiplied |
| D5: the Hospital's glow blobs | not Bloom or Neon: the blobs are the Hospital's own local lights (77 in view: `Lights.Tube*` PointLights, Shadows off, Brightness 0.1–0.55, Range 20–24) on its roof; with the board hidden Studio shows them too (place_d-probe 16 s: pinker, fainter blobs in the same places). In rustudio disabling DoobBloom (0.7, 26, 1.6) changes nothing, disabling the lights removes them | unchanged (Shadows off: nothing to block). What still differs is the night around them: Studio's Atmosphere (Density 0.3, Haze 1.2, Color (92, 102, 138)) hazes the roof and turns the sky (19, 57, 130); rustudio has no Atmosphere yet (black sky) | the sky / lighting work |
| D6: the Lodge's cyan band | the same band is in Studio (Lodge.Skyline.Aurora1–5: Neon, 170 × 26 × 0.5, ~200 studs out, Transparency 0.74–0.94 breathing on os.clock in LodgeFX.luau, random phases): (48, 128, 190) and (88, 198, 224) in the round 25 capture, unhidden by the board, rustudio (0, 89, 110) / (86, 188, 202). It reads as an extra band in rustudio because the sky behind it is black instead of Studio's Atmosphere blue (20, 59, 131) | see-through Neon as above; the sky is the Atmosphere's | — |
| Opaque Neon (open) | cyan (60, 220, 255) reads (62, 255, 255) in both styles, its halo adds ~5 levels of red | (211, 255, 255): colour² × 4 = 0.22 red plus the glow's red share √(0.055) of its strong channels; the halo's green / blue match (54 / 83 / 135 / 187 / 220 vs 59 / 89 / 133 / 179 / 214 at 25…5 px). Round 19's orange balls fit the hue rule (green / red 0.651), cyan's weak channel doesn't: needs a scene with a few colours to refit | `fs_bloom_bright`, `NEON_GLOW` |

Scores after this round (`fidelity.py run --skip-studio`, against the
round 25 Studio captures; before = the round 25 code run the same way,
the same day):

| Place | render before → now | worst shot MAE before → now |
|---|---|---|
| place-b-newer | 55.7 → 57.4 | N3 14.45 → 10.23, N4 30.96 → 31.17 (Studio's walls are lit 52, rustudio's 20: sky / ambient) |
| place_d | 46.3 → 48.3 (the board's random maps move it ±3) | D5 31.95 → 35.26, D6 31.49 → 26.92 |
| control / demo / place_a / place_c / place_b / gui-depth | 79.9 / 77.7 / 81.7 / 96.3 / 43.0 / 87.0 → 79.9 / 77.7 / 81.7 / 96.5 / 43.0 / 87.0 | — |
| lightreach-soft / lightfocus-soft | 83.3 / 77.1 → 86.3 / 81.3 | lightfocus at 19 s (the Focus 1500 away) 24.2 → 19.2 |

## Round 26: scripts

Studio's and rustudio's Output for place_d, place-b-newer, place_b and place_a
(`checks/compare/<place>/{studio,rustudio}/output.log`), line by line.

### The Place B Newer errors are the game's

All four are in Studio's Output too, word for word; rustudio matches:

| Error | Cause (same in Studio) |
|---|---|
| `Players.Player.Backpack.Script:1: Incomplete statement: expected assignment or a function call` | StarterPack.Script's Source is a list of names ("Spytoon", "Poop", …), not Luau |
| `EquipHuman:49: equippedHuman is not a valid member of Player` | nothing creates `player.equippedHuman` |
| `Workspace.OG Human.collectionDyn:85: attempt to index nil with 'WaitForChild'` | the script's `player` is nil there |
| `TP1 is not a valid member of Workspace` (TP20.Script:2) | the place has no Workspace.TP1 |

### The client's children were reversed (Fast Travel list)

`replicate::copy_subtree` copied the server tree with a stack and pushed
children first-to-last, so every parent on the client got its children in
reverse. The Fast Travel buttons all have LayoutOrder 0 under a
UIListLayout (SortOrder LayoutOrder), so ties keep child order: Studio
reads Desert, Forest, Snow, Water Cave, City, Spawn, Jungle, Mushroom (the
file's order), rustudio read it bottom up. Children are pushed last-first
now, so each parent's list is the server's, and the copy is pre-order like
GetDescendants. Test: `client_keeps_child_order`.

Place D's preview builder then hit a second gap: it loops over
`copy:GetDescendants()` destroying lights, parts and scripts, so a child of
something already destroyed is destroyed again. Roblox ignores that;
rustudio raised `The Parent property of PointLight is locked`. Destroy on a
destroyed instance is a no-op now.

### "Now playing" (Place B Newer's music GUI)

Two gaps:

- **A script enabled at run time never started.** MusicSystemInitializer
  parents ModernMusicGui into PlayerGui, then sets its Handler's
  `Enabled = true`; the place saves the Handler as `Disabled = true`.
  rustudio kept `Script.Enabled` in a stub, apart from Disabled, and only
  started scripts when they were parented, so the Handler never ran and
  the GUI kept its saved "Artist: / Title:" layout. Enabled is now
  Disabled turned round (read and write), and enabling a Script or
  LocalScript that hasn't run, where it runs, starts it (next dispatch).
  Test: `enabling_a_script_starts_it`.
- **MarketplaceService:GetProductInfo returned `{}`**, so the song's name
  was nil and the title assignment failed. It now yields until Roblox's
  catalog answers and returns its record: economy's asset details
  (`economy.roblox.com/v2/assets/ID/details`, the same fields:
  Name, Creator, AssetTypeId, IsForSale, …; anonymous), or a game pass's
  product info. A refusal raises `HTTP 404 (Not Found)` and the like.
  Other InfoTypes (developer products, bundles) need the experience's
  universe and still give `{}`. Test: `product_info_and_engine_error_text`
  (the web call stubbed).

Studio's capture shows `Now Playing: [getting song name]`: the offline
offline Studio's lookup fails, so the script keeps its placeholder.
rustudio, online, now shows the same legacy layout with the song's real
title ("Fun on the Holidays (instrumental)"), which is what a networked
Studio shows.

### Engine errors carry no position

Studio's Output for a pcall'd DataStore call: `[PortalState] DataStore
unavailable, progress won't be saved: You must publish this place to the
web to access DataStore.` rustudio's had `ServerStorage.PortalState:53:` in
front. Errors Roblox's C++ raises (reflection, services) are the message
alone; the script and line are the Stack Begin / Script 'X', Line N /
Stack End lines after an uncaught one. VM errors (`attempt to index nil`)
and Luau `error("x")` keep theirs. an admin script depends on it word for word: its
anti-kick check expects `Kick is not a valid member of Workspace
"Workspace"` from `workspace:Kick()`.

- The prelude's `check` rethrows engine errors at level 0, and
  `describe_error` no longer puts a Rust callback's first Lua frame in front;
  `report_frames` uses that frame for the stack when the thread's own
  frames are missing, and ScriptContext.Error finds the script from the
  innermost frame.
- The unpublished-DataStore error (library.luau) is level 0 too.
- A method called with an instance of a class that doesn't have it
  (`Player.Kick(workspace, …)`) fails `Expected ':' not '.' calling member
  function Kick`, as in Roblox; rustudio ran Kick on the Workspace.

### Smaller Output gaps

| What | Studio | rustudio before | now |
|---|---|---|---|
| `Player:Move` with no character (Place A's menu) | warns `Player:Move called, but player currently has no character.` | silent | warns, once per player |
| `debug.profileend()` after a yield (Place B's RbxCharacterSounds AtomicBinding) | `debug.profileEnd() - No active profile annotation. At: Stack Begin` … | no-ops | the markers count per thread for one resumption (resume clears them), and an end with nothing open warns |
| `WaitForChild` infinite-yield name | the instance's full name at the call (`'Player:WaitForChild("Torso")'`, the character before it's parented) | the name 5 s later | at the call |
| A script whose name has a NUL (the one under Place B's `Adonis_Loader.Loader`) | runs | `invalid name: nul byte found in provided data` | runs (the NUL is left out of the chunk name) |
| `Enum.X:GetEnumItems()` | the same EnumItem objects as `Enum.X.Y` | new objects, so they failed as table keys (an admin script's mock DataStore: `unable to cast 'Enum.DataStoreRequestType.SetIncrementAsync'`) | the same objects |
| `LocalizationService:GetTranslatorForPlayerAsync` | a Translator (LocaleId en-us) | nil (an admin script: `attempt to index nil with 'GetPropertyChangedSignal'`) | a Translator; Translate gives the text back |

Tests: `profile_markers_and_move_warnings`, `ltypes_enum_items_are_unique`.

With that script running, Place B and Place B Newer now load an admin script (rustudio
downloads MainModule 7510592873; the offline Studio can't: `Downloading
asset failed for asset id 7510592873`). an admin script's client checks find what
is still different.

### Scripts that started running show more gaps

Starting a script when it's enabled at run time also started Place D's second admin script
Admin client (its loader enables it), and an admin script now gets further. That
brought up these gaps:

| What | Studio / Roblox | rustudio before | now |
|---|---|---|---|
| A signal's type | a userdata: `type` "userdata", `typeof` "RBXScriptSignal" | a Luau table (`__type` only counts on userdata): another admin script Flux picks events by `typeof(inst[k]) == "RBXScriptSignal"` and assigned instead (`FocusLost is not a valid member of TextBox`). an admin script's Wrap copied the table's fields and lost Connect (`attempt to call missing method 'Connect' of table`) | a userdata (ltypes `LSignal`) over the prelude's signal table, with its methods from the prelude's Signal; library.luau's LuaSignal too |
| `FloatCurveKey.new(t, v, mode)` (another admin script haptics) | a datatype | missing (`attempt to index nil with 'new'`) | Time, Value, Interpolation (Cubic by default) read-only; LeftTangent / RightTangent nil until set |
| A bool property given another value (another admin script Flux binds a number to Visible) | Luau truthiness (`Visible = 0` shows) | `Unable to assign property Visible. bool expected, got number` | truthiness; Script.Enabled too |
| `TextService:FilterStringAsync` (an admin script filters every UI string) | a TextFilterResult; Studio doesn't filter | nil | a TextFilterResult whose Get…Async methods give the text back |
| `StarterGui:GetCoreGuiEnabled("Chat")` (an admin script's console) | the item, its name or its value | only an EnumItem | all three (`ltypes::enum_arg`) |
| `settings()` from a game script | fails: an admin script kicks a client whose `pcall(settings)` succeeds (rt0001), and it runs in every Studio Play | worked | `The current thread cannot call 'settings' (lacking capability Plugin)`, measured; decided by the calling thread's identity (see Thread identity and capabilities below) |
| `Players:CreateHumanoidModelFromUserId` (another admin script avatar preview) | the Humanoid has its Animator | no Animator (`Infinite yield possible on '….Humanoid:WaitForChild("Animator")'`) | has one |
| A legacy Script in the player's PlayerGui (Place B Newer's StarterGui `version.Script`) | runs: Studio's label reads `server version: 0` | didn't run (only Backpack scripts did), so the label kept `server version` | runs, like Backpack scripts, at join and when parented or enabled there |

Tests: `signal_typeof`, `script_library_gaps`, `player_gui_scripts_run`,
`ltypes_float_curve_key`.

### Still only in rustudio's Output

| Line | Why |
|---|---|
| `:: an admin script :: Using MockDataStoreService instead of DataStoreService` | an admin script runs at all |
| `Sound rbxassetid://… isn't available: …User is not authorized…` | rustudio's own asset report; Studio prints `Failed to load sound …: Request asset was not found` as an error |
| `Infinite yield possible on 'Workspace.Player:WaitForChild("Torso")'` (Place B) | Studio fires CharacterAdded before the character is parented to the Workspace (Roblox docs), so the name there is `Player`; rustudio parents first. Reordering spawn_character (physics, replication) is left for a movement round |
| `[Place A] gun skin primitives failed to load: Asset … isn't available: …` (Place A) | the same failure; rustudio words why differently |

Gaps scripts reach now that an admin script and another admin script run (gaps.json;
scripting_api counts calls, so these hold Place B at 99.9 and Place D and Place B
Newer at 99.9): an admin script calls `LogService:GetLogHistory` / `MessageOut`,
`GroupService:GetGroupsAsync`, `RemoteFunction:InvokeClient`,
`TeleportService.TeleportInitFailed`, the MarketplaceService purchase-finished
events and TextChatService's `SendingMessage`. another admin script calls
`HapticEffect:SetWaveformKeys`, UIPageLayout (`JumpTo`, `CurrentPage`,
`PageEnter` / `PageLeave`), TextChannel, `TextService:GetTextBoundsAsync`,
`AvatarEditorService:SearchCatalogAsync`, `MessagingService:SubscribeAsync`
and the touch gestures.

### Scores

`fidelity.py run --skip-studio --skip-perf` (the same Studio captures),
against round 25 in score.json:

| Place | render | scripting_api | physics | camera_controls | gate |
|---|---|---|---|---|---|
| place-b-newer | 55.8 → 56.1 | 99.9 → 99.9 | — | 50 | PASS |
| place_d | 49.6 → 50.7 | 99.9 → 99.9 | — | 75 | PASS (another admin script client errors: none) |
| place_b | 43.0 → 43.1 | 100.0 → 99.9 | 100 | 100 | PASS |
| place_a | 81.7 → 81.8 | 100 | — | 100 | PASS |
| place_a_match | 44.7 → 55.2 | 100 | — | 75 → 100 | PASS |
| place_c | 96.5 → 96.2 | 100 | 88.9 | 100 | PASS |
| control | 79.7 → 79.9 | 100 | 100 | 100 | PASS |
| demo | 77.7 → 77.7 | 100 | 100 | 100 | PASS |

The flagged drop, Place B's scripting_api 100 → 99.9, is an admin script's calls
above: an admin script runs at all now. Place C's render drop is within its spread (its
sector is random). Place D's vote board shows three random maps (renders of
49.1 to 50.9 over this work's runs).

## Round 26: terrain

Measured in an offline Studio with new calibration places in
`tools/fidelity` (captures in `checks/compare/<place>/studio`):
`lightgrid`, `lightgrid2` (light grid probes, `make_lightgrid_place.py`),
`lightgrid-sun` (the same under a low sun, Voxel), `skydown` (the default
skybox from 0° to 90° down, `make_skydown_place.py`) and `terraincolor`
(flat slabs per material, `make_terraincolor_place.py`). Edit-mode probes
ran with `studio_edit.py` (a hidden Studio, no Play): `region_probe.luau`
and `place_b_voxels.luau`, logs in `checks/compare/round26-terrain/`.

### Light grid (skylight), refitted

The probes light terrain with OutdoorAmbient alone (Ambient 0, Brightness 0,
no environment light), so each surface shows its skylight. What Studio
shows:

- A 16-stud shaft's floor darkens steadily with depth: lit at 10 studs
  down, 0.13 at 58.
- The foot of a 96-stud wall darkens only a little (0.74), within ~10 studs.
- The ground under a 64-stud roof 16 studs up is black. Under one 32 up
  its middle is black and its edges dim. Under one 64 up its middle is
  ~0.15 and it fades out toward the edges. Seen from the side at ground
  level (lightgrid R1–R3), the last one hides all that: read the ground
  from above.
- The wall's foot stays darkened with the camera 96 to 640 studs over it
  (H96 … H640), so Studio's grid reaches at least 640 studs below the
  camera, or isn't centred on it.

The model (`lightgrid.rs`, constants in `mech.rs`) is an open voxel's
straight-up column (what `LIGHTGRID_RANGE` = 12 voxels over it let through)
× one upward cone (tan 0.7, as far): exp(−6 × Σ occupancy³ × step /
footprint width), the occupancy averaged over the cone's square footprint
(a summed-area table). Cubing the footprint's occupancy is what darkens a
shaft (footprints walled all round) while one wall barely darkens its foot.
The grid is now 64 × 160 × 64 voxels (640 studs tall): H96 … H320 then
match. H448 and H640 still don't (rustudio's grid ends above the ground).

| Place | round 25 (17 rays, range 8, 32 tall) | round 26 |
|---|---|---|
| lightgrid (render) | 79.1 | 88.2 |
| lightgrid2 (render) | 64.8 | 90.6 |

Misfits left: the 64-stud roof (lightgrid2 78 s, MAE 22.9, rustudio leaves
the ground lit: its column and cone end at 48 studs) and the tunnel mouth
(83 s, 24.0). A second, narrow cone (tan 0.2–0.35, 80–96 studs, density
8–20, power 1–6; 6 variants) fixed the roof (to 10.6) only by darkening
the tunnel, the tall wall's foot and the wall-height series as much
again, so it isn't used. D448 … D900 differ in shape, not light: Studio
draws the far wall from coarser terrain (its LOD).

Cost: Place B's massif reads 1.3 M voxels on the main thread (~5 ms, once
per 32 studs the camera moves) and traces in ~12 ms on a worker
(`lightgrid::tests::lightgrid_place_timing`, ignored). Place B Play bench
(load 8–10, base round 25 build against this one, alternated): 64–94 fps
against 56–81 fps, sections the same within noise.

### Voxel sun shadow: not built

Under Technology Voxel Studio's sun shadows are soft, coarse and leaky
(`lightgrid-sun`: the ground beside 24 / 48-stud walls is half lit where
rustudio's shadow map draws black; render 75.9). None of the user's places
that use Voxel (place-c-terrain, Place A by default) shows it: Place C is lit from
above with no relief to shade, Place A is indoors. Revisit when a Voxel place
with sunlit relief scores low.

### Place B's lake air pockets

`place_b_voxels.luau` read cells x −2105…−2093, y 14…30, z 304…322 (the
F4 neighbourhood) with ReadVoxelChannels: 1758 of 4199 are air (no solid,
no liquid), the rest water. rustudio's decode of og.rbxl holds the same
1758 air cells, and the same liquid byte in every water cell. So the
pockets are real, and round 25's rule (under water, draw only the surface
over the camera) stays.

### T9: the skybox's bottom face

Studio's sky cube is rustudio's mirrored left to right. Looking down, its
SkyboxDn shows flipped; SkyboxFt does too; at 45° between faces it matches
with Lf and Rt swapped as well. `viewport.rs` now flips each face
horizontally and puts Rt at −X, Lf at +X (the only sky change; nothing
else in the sky code moved). Unflipped, the Dn face's brighter edge met the
side faces as a lighter band (terrain T9).

| skydown pitch | 0° | 30° | 45° | 55° | 65° | 75° | 90° | 55°, turned 45° |
|---|---|---|---|---|---|---|---|---|
| MAE before | 6.04 | 6.33 | 15.52 | 8.89 | 9.53 | 10.09 | 11.07 | 9.11 |
| MAE after | 1.22 | 3.54 | 13.24 | 5.51 | 6.07 | 7.26 | 8.07 | 5.66 |

Terrain T9 11.73 → 9.38.

### Skybox orientation and the skydiag sky (2026-11-08)

Map 2's sky through the hut roof read nearly white in rustudio (linear
0.84, 0.93, 0.95) where Studio shows dark storm clouds (0.13, 0.20, 0.22).
The face mapping was not at fault. Its Sky saves **SkyboxOrientation
(0, 90, 0)**, and rustudio ignored the property, so the view through the
hole fell on the bright band of the wrong side face.

Test sky (`~/.cache/rustudio/skydiag/`, `make_faces.py`): six 512² faces,
each its own colour, with its name in large letters, a black corner at the
image's top left and a white one at the top right, a black→white ramp
along the bottom, a ▶ on the left, and grey 64 / 128 / 192 patches. Local
offline Studio ids `skydiag:<sha>:<face>` (8000000000064779…84). The place has
ShadowMap, ClockTime 12, no Atmosphere, no post effects,
CelestialBodiesShown off and 0 stars. Captures: an offline Studio edit mode
(`studio_cap.py` / `studio_cap2.py`) and rustudio `--window 1146x1072`
(`rs_cap.py`); both give an 862×675 view at FOV 70. Views: the 6 axes,
straight up and down at two yaws, five diagonals, and the hut's sky
direction. `cmp.py` gives the per-view mean absolute error. MASK=24 skips
Studio's "Current device" toolbar, which some sessions draw.

- Orientation 0: every view is within 1.3–2.2 levels (mean 1.8). That is
  only the gizmo and selection chip, so face assignment, per-face turns and
  mirroring, and brightness (texel², no factor by day) already matched.
  Map 2's own faces in an empty place match too.
- SkyboxOrientation: (0,90,0), looking −Z, shows Lf. (90,0,0), looking −Z,
  shows Dn. (0,0,90), looking +X, shows Dn. So each axis turns the sky by
  Roblox's own rotation about it (Ry(90) takes +X to −Z), and drawn(d) =
  sky(R⁻¹·d). Composite (30,60,20), tested against all six orders (4 views):
  Z·X·Y gives 3.6, and the others give 36–56 (Y·X·Z, CFrame.fromOrientation's
  order, gives 50).
- `viewport.rs orient_sky` turns the cube once on the sky worker, bilinear
  within a face; quarter turns are an exact reshuffle. The drawn sky, its
  reflections (same cube) and its irradiance (`sky_irradiance` runs on the
  turned cube) therefore all turn together. The sky key includes the
  orientation, so a change rebuilds it.

| orientation | (0,90,0) | (90,0,0) | (0,0,90) | (30,60,20) | (0,45,0) |
|---|---|---|---|---|---|
| mean MAE before (14 views) | 48.8 | 69.4 | 60.5 | 59.9 | 53.5 |
| after | 3.47 | 3.28 | 3.31 | 3.63 | 3.71 |

Map 2 hut sky (`sumpf_ref`, nopost + noatmos, same camera, 862×675): sky_hole linear (0.84, 0.93, 0.95) → (0.153, 0.213, 0.248). Studio reads (0.127, 0.203, 0.218), and the MAE fell from 121.1 to 10.1. thatch_sky is unchanged (17.3 / 17.7). Not done: the sky gradient c0/c1 uses the turned height in Studio. It is white by day, so these tests can't see it, and rustudio doesn't apply c0/c1 to custom skies.

### Terrain colours without packs

`terraincolor` lights flat slabs with Ambient 128 alone, OutdoorAmbient
alone, the sun alone, then all three. Studio's grey materials come out
greener and less blue than their avgAlbedo, while Snow and Sand nearly
match it. Rock's avgAlbedo is (80, 84, 90). Studio's linear colour over
rustudio's gives (72, 91, 79), the same at Ambient and OutdoorAmbient. No
recolour formula tried reproduces the shift: avgAlbedo solved to the
MaterialColor in sRGB or linear, or the 2022 table's average. So
`mech::TERRAIN_FALLBACK_ALBEDO` holds the measured colour of the 10
materials probed (Rock, Concrete, Slate, Basalt, Mud, Sand, Snow, Asphalt,
Pavement, Ground). It is used only at the default MaterialColors: with
the table also applied under place-c-terrain's custom colours, Place C's blue
terrain turned teal (render 85.6 → 85.4). The other materials keep
avgAlbedo. This only matters for the packless fallback, which is what the
offline Studio draws. With packs, rustudio draws the textures.

### Region tools vs Studio

`region_probe.luau`, in a hidden Studio's edit DataModel:

| What | Studio | rustudio now |
|---|---|---|
| Region3int16 Max in CopyRegion | inclusive: (10, 1, 20) … (13, 2, 23) holds 4 × 2 × 4 cells; (10, 1, 20) … (10, 1, 20) one cell | the same (`runtime_ext` CopyRegion: size = Max − Min + 1; SizeInCells = ExtentsMax − ExtentsMin + 1) |
| PasteRegion | the cells from the corner cell: pasting r1 at (100, 1, 100) puts Snow at (100, 1, 100), Grass at (103, 2, 103); with pasteEmptyCells only the region's own box changes | the same |
| Saved TerrainRegion | ExtentsMin (10, 1, 20), ExtentsMax (13, 2, 23) (inclusive), SmoothGrid holding the cells at their **world** cells (SerializationService, the LZ4 chunks decoded: the blob is byte for byte what rustudio now writes) | `Region::to_smooth_grid(min)` writes world cells; `from_smooth_grid` reads them, and still reads round 25's region-cell blobs |
| ExtentsMin / ExtentsMax from a script | "ExtentsMin is not a valid member of TerrainRegion" | the same error |
| WriteVoxels occupancy | truncated to a byte: 0.1 / 0.25 / 0.3 / 0.5 / 0.7 / 0.9 read back as 26 / 64 / 77 / 128 / 179 / 230 ÷ 256 (FillBlock rounds) | floor(share × 255) |
| LiquidOccupancy | (byte + 1) / 255 where ReadVoxels reads (byte + 1) / 256 (0.101961 against 0.101562) | the same |

Test: `terrain_region::tests::studio_saved_region_holds_world_cells` holds
Studio's blob.

### Live preview while dragging

Move, Resize and Rotate now show the terrain in the box while a handle is
dragged, as Studio's Live Edit (TransformOperation restores the last
preview's backup, then places again). `Editor::preview_lift` touches only
the drawn grid: it restores, from the grid as it was at the lift, the cells
the source and the last preview covered, places the content, and re-meshes
just those chunks as a brush stroke does. The document doesn't change
until release, which commits one undo step as before. A release that
commits nothing (a ring drag back to 0°) resyncs the drawn grid from the
document, and the next preview restores the box where the drag began. The
self-test's `terrain-region` step checks it mid-drag: the moved box
already shows the content, while the document and its undo stack are
untouched.

### Scores (saved Studio captures, `--skip-studio`)

| Place | round 25 build (render / terrain) | round 26 |
|---|---|---|
| terrain | 83.0 / 83.4 | 89.1 / 89.3 |
| place-b-terrain | 71.8 / 72.2 | 74.4 / 74.8 |
| place-c-terrain | 85.6 / 86.0 | 85.6 / 85.9 |
| terraincolor | 82.2 | 84.0 |

terrain gains on every view, most at T2–T4, T8b and T9 (the sky cube).
place-b-terrain gains most at F3 (12.1 → 6.0) and F5 (23.1 → 21.7).

## Round 27: places from Roblox

File > Open from Roblox (Ctrl+Shift+O) opens places stored on Roblox the
way Studio does, and the open document knows which server-side place it is
(`Document::cloud`, a `CloudOrigin`: universe, place, version, place and
experience names, creator id / type / name, Team Create, local copy).
Code: `crates/rustudio-account/src/cloud.rs` (calls, cache, link parsing),
`src/cloud_ui.rs` (picker, open / save flow, Experience Settings),
`src/selftest/cloud.rs` (step "cloud-place"). Endpoints and their verified
status codes are in AUTH.md, "Places on Roblox".

### Finding places

The picker has Studio's Start Page tabs: Recent, My Experiences, Group
Experiences, Shared with Me, plus "Open by place ID or URL" (a bare id,
`roblox.com/games/<id>/…` with any locale prefix, `games/start?placeId=`,
`create.roblox.com/…/experiences/<u>/places/<id>/…`). Read out of Studio's
StartPage plugin (BuiltInStandalonePlugins StartPage.rbxm, `Src.Network.Urls`,
`Src.Foundation.Components.ExperiencesPage`, `Src.Util.getQueryString`):

- Owned: `creatorType=User&creatorTargetId=<me>`; rustudio keeps develop
  `v1/user/universes` (same rows).
- Shared: the same search with `creatorType=Team&creatorTargetId=<me>`
  (`apis.roblox.com/universes/v1/search`, `surface=StudioStartPage`). The
  rows carry `isTeamCreateEnabled`, shown as "Team Create".
- Group: Studio's group menu is `creator-home-api/v1/groups` (every group
  the account is in, with the role name). Whether the role may edit is
  `groups.roblox.com/v1/groups/{id}/membership` →
  `permissions.groupEconomyPermissions.manageGroupGames`. A role without
  it is listed with "can't edit" and a line saying so; develop
  `v1/groups/{id}/universes` answers it 403 "You are not authorized for
  access." (Place B, role Member).
- Recent: `~/.cache/rustudio/places/recent.json`, the last 20 places opened.
- Places: develop `v1/universes/{id}/places`; icons: thumbnails
  `v1/games/icons` (150x150 PNG, shrunk to 64 px textures).

### Opening

As Studio: first the universe's Team Create status (develop `v1/universes/multiget/teamcreate`, Studio's own call in Studio and in StartPage `DiscoverTeamCreateStatus`).

- Team Create off or unknown: the latest saved version downloads (asset
  delivery v2 with the session). Its number is the reply header
  `roblox-assetversionnumber` (a live Place A session: 757, the newest Saved version in
  the history, not the published one).
- Team Create on: Studio joins the live session instead (gamejoin team
  create, then the RCC server). rustudio asks: "Join Team Create session"
  (`Editor::open_team_create`, a hook: it says joining isn't supported yet;
  the live client is another agent's work), "Open a local copy (download
  the last saved version)", or Cancel. A copy is titled "… (local copy)",
  Ctrl+S writes a file, and Save to Roblox / Publish ask first.
- An older version: asset delivery `v2/assetId/{id}/version/{n}` serves
  any version to anyone who can read the place (Baseplate v1: 36 KB). The
  list comes from Studio's PlaceVersionHistory plugin call
  `place-version-history-api/v1/{placeId}/history` (version numbers are
  strings, `saveType` a number), else develop `v1/assets/{id}/saved-versions`
  (Studio's C++). Both are 403 for people who can't edit; the picker then
  says so and still opens a version by number.
- Cache: `~/.cache/rustudio/places/<placeId>/v<N>.rbxl` (`.rbxlx` for XML)
  and `origin.json`, five versions kept. When Roblox can't be asked (or
  signed out) the newest cached copy opens with a warning; a cached Team
  Create place opens as a copy. A refusal (no access, no such place) is
  reported, never covered by the cache.
- A place the account can't read: asset delivery answers 200 with
  `errors[0].code` 409 "User is not authorized to access Asset."; shown as
  "Roblox won't give this account place 100000005 (…): only the owner, the
  group's editors and collaborators can open a place that isn't
  uncopylocked". Anonymous asset delivery refuses every place (401
  "Authentication required").
- Shown: the tab and window title are the place's name (window
  "<name> - rustudio"), the menu bar adds "@ experience (creator) vN · Team
  Create on", and File > Experience Settings lists place, experience,
  creator, ids, start place, version, how it was opened, the cache folder
  and Team Create (`GetCanCollaborate`, develop `universes/{id}/permissions`
  canManage / canCloudEdit).

### Play

Before: `game.PlaceId`, `GameId`, `CreatorId`, `PlaceVersion` were 0 and
`CreatorType` User for every place. Now the play copy's DataModel holds the
origin, as Studio's for a cloud place: a live Place A session prints `100000000000004
10000000003 100000002 Enum.CreatorType.User 758`. A file place still reads
0. Not done: Studio refuses DataStores unless the experience allows Studio
API access (games `v1/games` `studioAccessToApisAllowed`); rustudio's
in-memory DataStores now just see a non-zero PlaceId.

### Saving

Ctrl+S and File > Save to Roblox save a Saved version, File > Publish a
Published one; "Save to File" / "Save to File As" write a file and keep the
origin. Before sending, the server's newest version (the asset delivery
header) is compared with the one opened; if someone saved since, nothing is
sent and rustudio asks: Overwrite, Download version N, or Cancel. Saving
uses Open Cloud Place Publishing with the publishing key; without one it
says what key to make and sends nothing (the session upload is refused by
Roblox, AUTH.md). Tested only against a local mock of Roblox
(`rustudio_account::mock`, `Endpoints::mock`): the unit test
`save_checks_the_server_version_first` and the self-test's Ctrl+S /
Overwrite / no-key checks. No save was made to Roblox.

## Round 27: particles against Studio, property by property

The person's verdict on effects: "not even relatively close". Each
ParticleEmitter property was isolated in a calibration place (scratch
`.rbxlx` files, written by small scripts; textures: Studio's built-ins,
Place B's mist / ribbon / spark textures from the offline Studio store, and two test
images added there as ids 999000000000001 (a 4 × 4 flipbook whose frames
light 4 quarters as bits) and 999000000000002 (four coloured quarters, for
orientation)). Both engines run the same LocalScript in Play: 2 s in it
`:Clear()`s every emitter, `:Emit`s / enables them, waits T and sets
`TimeScale` 0, so both show particles of the same age; shots are compared
by blob position, colour layout and inverse-tone-curve light. an offline Studio
Studio, D3D11, 863 × 675 view, camera FOV 70.

### Matched already (no change)

Speed (3.07 vs 3.02 studs after 1 s), Acceleration, Drag (halving:
Drag 1 2.23 vs 2.32, Drag 3 1.24 vs 1.25), Lifetime, Rate phase (Rate 3:
1.31 / 0.74 / 0.03 vs 1.36 / 0.78 / 0.03), TimeScale 0.5 on motion,
Size / Transparency / Squash sequences with envelopes, Color sequences,
Rotation, ZOffset in both directions, Sphere and Box Volume, Transparency
over black (223 / 191 / 133 / 51 / 16 at 0 / .25 / .5 / .75 / .88 against
223 / 191 / 132 / 47 / 12), Brightness 0.05–2. Emitters simulate in Edit
in both.

### Fixed

| Item | Studio (measured) | rustudio before | now |
|---|---|---|---|
| SpreadAngle axes | X turns the direction about the part's X (Z when the direction lies along X), Y about the third: Top spreads X into Z and Y into X, Front X into Y, Right X into Y and Y into Z (calibration d) | swapped for Top and Front | `spread_axes` |
| RotSpeed sign | +10°/s turns clockwise on screen (−10.5 / −30.5 / −50.6° at 2 / 4 / 6 s) while Rotation +30 is counter-clockwise (calibration f) | counter-clockwise | `p.rot -= spin × dt` |
| Box Surface | only the face EmissionDirection names | all six faces | that face |
| Cylinder | a Roblox cylinder along the part's X (Z for Right / Left), radii the other extents; Surface = the side only; ShapePartial = the radius at the +axis end (a cone); moves out from the axis (calibration h, three views of a 6 × 4 × 2 part) | axis Y with caps | as measured |
| Disc | across EmissionDirection, radii the other two extents; Surface = a filled disc on the emitting face, Volume = through the part; moves along the direction | XY plane, Surface a rim | as measured |
| VelocityParallel | image X along v, Y = forward × v (moving right shows it upside down; calibration j, coloured quarters) | Y = v × forward | flipped |
| VelocityPerpendicular | Y = world up made square to v, X = v × Y: moving away as is, toward the camera mirrored left-right whatever the part's turn; straight up / down Y = v × the part's X (calibration j / k) | from the part's X axis | as measured; 15 / 15 colour layouts match |
| Flipbook OneShot | frame = t × (frames − 1), blended to the next (half way: frames 7 and 8 half and half; 0.625: 9 → 10 at 0.375); FlipbookBlendFrames defaults to true | t × frames | t × (frames − 1) |
| LightInfluence | the scene's light weighs √LightInfluence: 0.30 / 0.46 / 0.71 / 0.87 / 0.95 at 0.1 / .25 / .5 / .75 / .9, Brightness the rest (calibration o, night, Brightness 0 / 1 / 3) | linear | √; the night sweep is within 3 levels everywhere (was up to 70 off) |
| Night | the moon lights a LightInfluence 1 particle at 0.115 × Brightness (white reads 148 = 0.308 light at Brightness 2, 70 at Brightness 0) | the parts' 0.03 × Brightness (95) | `FX_MOON_PER_BRIGHTNESS`, set in viewport.rs' SceneLight fill (151) |
| Local lights | from the voxel light grid: a straight line, ~0.055 per stud × Brightness, to 0 at Range + 4 (Range 8: 0.42 at 4 studs … 0.071 at 10.7; Range 16: 0.99 … 0.023 at 19.5); ambient + local held to luminance 1, an orange light's red still 1.58 (calibration q, p) | a plateau of 1.06 to 0.7 Range, cut at Range | `FX_LOCAL_SLOPE` / `FX_LOCAL_REACH` / `FX_LIGHT_CAP`: Range 16 reads 0.87 … 0.021, Range 8 0.435 … 0.06 |
| Texture sampling | mipmapped (calibration r: small particles keep their texture's average light) | one level, sparse texels (Place B's mist speckled) | effect textures get a mipmapped copy (`WorldGuiPass::mipped`), trilinear |
| Blending | before the tone curve, in the square root of linear light (ParticleCustomFS); overlapping LightEmission 1 particles add up there (calibration t: Place B's path ribbons, Rate 30, Transparency 0.88, Brightness 0.1, light 570 k over the ground) | each particle tone-mapped alone and added on the shown view: the curve's toe crushed faint ones (Brightness 0.1 invisible; 0.3–0.45 × Studio's light) | effects drawn into the scene's HDR image in that space (`WorldGuiPass::effects`: copy √, draw, square back) and the bloom and final image run again (`Renderer::finish_again`): 682 k / 1141 k / 1683 k against Studio's 570 k / 869 k / 1509 k at Brightness 0.1 / 1 / 4 |
| Bloom | effects glow with the scene; their light is clamped first: white squares glow the same at Brightness 4, 10, 100 (halo 1.8–3.5 studs out 8.8 / 10.0 / 12.4 levels) | no glow (0 / 0 / 0) | through the scene's bloom; `FX_LIGHT_CLAMP` 4.5: 9.1 / 10.6 / 11.2 |
| LockedToPart Acceleration | in the part's frame (+X on a part turned 90° about Z moves up) | world | the part's frame |
| VelocityInheritance | the host's physics velocity: an anchored part slid 4 studs/s by CFrame passes on none (a 4-stud line at 1 and 0.5 alike) | the host's motion | an anchored host's AssemblyLinearVelocity, else its motion |
| WindAffectsDrag | Drag pulls toward Workspace.GlobalWind (10 studs/s wind blows a Drag 1 stream out ~2–3 studs in 1 s) | ignored | v = wind + (v − wind) × 2^(−Drag dt) |

Over a bright panel (calibration b, row 5: Transparency 0.5, the same with
LightEmission 1, grey, LightEmission 1 Brightness 0.1, Transparency 0.88,
the same with LightEmission 1) blending in that space also fixed
translucent particles: Studio 219 / 245 / 129 / 239 / 216 / 226, rustudio
before 244 / 255 / 133 / 255 / 209 / 236, now 223 / 253 / 132 / 244 / 224
/ 233.

### Still different

- Low alpha reads a few levels dark (Transparency 0.75 / 0.88: 47 / 12
  against 51 / 16).
- TimeScale and RotSpeed: Studio's spin angle is RotSpeed × (TimeScale ×
  age) plus an offset of RotSpeed × (1 − TimeScale) × the particle's birth
  time since Play, and setting TimeScale to 0 drops the spin term; rustudio
  integrates the spin.
- Local light comes from Studio's voxel grid: parts in the voxels darken
  it, the Range 30 tail falls faster than the line, and the same light read
  0.52 or 0.95 at 6 studs in two layouts. rustudio has no occlusion here.
- Place B's paths: Studio's SpawnWater part (Granite, light blue) draws bright
  where rustudio's is dark (materials / lighting, not effects), so the
  same mist reads as a large change over rustudio's river and a small one
  over Studio's.
- Studio's Size 0.08 sparkles read brighter than rustudio's (r: 0.11 of
  the light).
- Not measured: FlipbookStartRandom / Random / PingPong rates beyond one
  frame, sorting between emitters, SpotLight / SurfaceLight cones on
  effects, Trail / Beam light under the new local-light model.

### Costs

Effects in the HDR image cost one more copy, the square-back and the
bloom + final pass again on frames with effects (`gui_world_pass`).

## an admin script anti-tamper (2026-11-08)

Every Play of Place B / Place B Newer kicked the player: `Tampering with
Client [read rt0001]`, then `Adonis_0x273A`, with `Anti_Cheat:723: attempt
to call a nil value`, `Invalid Remote Data (r10004)` and the detector
warnings around them. Studio shows none of it.

| Check (an admin script 285 client) | Roblox | rustudio before | now |
|---|---|---|---|
| ReadOnly table `__index` (Shared.Service): `pcall(settings)` must fail | fails (Plugin capability) | succeeded: settings() only refused yieldable threads, and a metamethod never is. So every read of `client.X` ran the rt0001 handler, which kicks and returns nil (`Anti.AddDetector` nil → Anti_Cheat:723; client.Core etc. nil → 0x273A; the remote key nil → r10004) | refused unless the nearest non-C, non-rustudio caller is the harness |
| indexInstance / newindexInstance / namecallInstance / indexEnum / namecallEnum: the raiser of `game.____________` (`debug.info(2, "f")` in the xpcall handler) | a C function: `[C]`, line -1, name "", arity 0; calling it with bad arguments fails with the same string each time | the prelude's `error` (name "error"), and for Enum an mlua callback whose error is a new userdata each call | unnamed C closures over the real implementation (src/cmeta.rs), which raise every failure as a string |
| `RemoteFunction.InvokeServer(workspace)` | `Expected ':' not '.' calling member function InvokeServer` | sent an invoke for the Workspace | Lua-written methods (Invoke, InvokeServer, InvokeClient) check their self's class first (`__rs_member_check`) |

The C closure costs one more nested C call per metamethod level, so the
vendored Luau is built with LUAI_MAXCCALLS 300 (was 200): Immediate signal
re-entrancy to depth 80 needs ~240. Index and Enum reads cost the same as
before (~280 / ~480 ns in the demo). After the fix Place B Newer plays 60 s
with no an admin script line beyond its loading messages. Test:
`adonis_tamper_checks`.

## Global library parity (2026-11-08)

- **Environment.** A script's environment holds exactly `_G`, `shared`,
  `script`; its metatable is locked (`getmetatable(getfenv())` is "The
  metatable is locked") and reads through to the public globals: the host
  globals minus rustudio's own (`__rs_*`, `__ac_*`, `__ws_*`, `__geo_*`,
  `LoadLibrary`, `integer`, the Dialog CoreScript's functions, which now
  run in their own environment). The engine Lua (prelude, library.luau,
  domains) keeps using the real globals. `_G` is its own per-VM table, not
  the environment. `require` is one shared function; a path's requiring
  script comes from the caller's function environment read in C
  (`__rs_caller_script`), so the caller keeps safeenv. The test harness
  (`=harness`) shares `_G`/`shared` with scripts and still sees internals.
- **Engine Lua reads as C.** Vendored Luau: a closure from a `=rustudio.*`
  chunk is `[C]`, line -1, arity 0 vararg to `debug.info`, is skipped by
  `debug.traceback`, gets no position in `error`/runtime-error messages, and
  getfenv/setfenv treat it as a C function. Only the script-facing entry
  points do this (`lua_getinfoscript`); the host's `lua_getinfo` still sees
  the truth (security.rs's caller check relies on it). So print, warn, wait,
  task.wait, require, version, settings, stats, … all report Studio's
  native metadata. mlua C functions get their key as debug name
  (`lua_setcfunctionname`); `Spawn`/`Delay`/`ElapsedTime` are their own
  named functions.
- **getfenv never returns the host globals.** Threads keep them (mlua's
  `Lua::globals` reads the running thread's), so where getfenv would answer
  them (level 0, C functions, engine frames) it answers the nearest script
  frame's environment instead (registry `rs_public_env` when none): so
  `getfenv(0) == getfenv(print) == getfenv(2) == getfenv()` in a script and
  in its callbacks, as in Studio.
- **Addresses.** `lua_setpointerencodekey` with a random key per VM: tostring
  of tables/functions/threads/userdata/buffers prints Luau's own encoded
  address (Feistel over SipHash, the mechanism Roblox uses): 16 lowercase hex
  digits, stable per live object, differing between server and client and
  between runs. The exact Roblox key is not reproduced (not recoverable).
- **GC / compilation.** `collectgarbage` accepts only "count" (a whole KiB
  number equal to `gcinfo()`), with Studio's missing/invalid-argument and
  "collectgarbage must be called with 'count'; use gcinfo() instead"
  errors; `loadstring` raises "loadstring() is not available" (no
  position) on both sides.
- **require.** Failures carry no position: "Attempted to call require with
  invalid argument(s).", 'error requiring "bad": require path must start
  with a valid prefix: ./, ../, or @', "Module code did not return exactly
  one value"; a module that raised gives "Requested module experienced an
  error while loading" (its own error goes to Output) and keeps failing on
  later requires; `false` is asset id 0; on the client a number is
  "require(assetId) cannot be called from a client.  assetId = N".
- **Misc.** `task.cancel` of the running thread: "cannot cancel thread";
  `Version()`/`version()` = "0.744.0.7441077"; `_VERSION` = "Luau"; the
  string metatable is locked; `elapsedTime` is process time, `tick()` local
  time; Studio's extra `debug.*` (dumpheap, dumprefs, dumpcodesize,
  tracerefs, loadmodule: no-ops; set/get/resetmemorycategory) and
  `utf8.graphemes`/`nfcnormalize`/`nfdnormalize` exist (behaviour beyond
  presence/metadata unmeasured: normalization passes text through,
  graphemes approximates UAX #29).

Probe diff (`tools/parity_probe/globals/compare.py`, 1232 server/client
probe keys against studio-complete.log, addresses/player names/timings
normalized; raw host pointers still count as mismatches): **before** 581
mismatches, 28 missing, 117 internal names visible (captured logs; a
same-day build without these changes: 551 / 26 / 116); **after** 7 / 2 / 0
(`rustudio-parity-{server,client}.log`, `run_probes.sh OUTDIR`). Left:
DebuggerManager (absent), stats()/UserSettings() as tables rather than
Instances (security agent's area), and `misc.time` (Studio sampled at Play
start, the harness at 3 s).

**Follow-up (settings globals).** `stats()` is the Stats service;
`UserSettings()` a UserSettings Instance named "GlobalBasicSettings" (made on
first use: the library loads before the runtime has a document) whose child
is the UserGameSettings Instance: `GetService`/`FindService`
("UserGameSettings"), `.GameSettings`, `IsUserFeatureEnabled` with the
measured flags and "Flag X does not exist" otherwise (library.luau Lua
methods registered for UserSettings only). UserGameSettings' properties read
and write library.luau's `ugs_values` (registry `rs_user_game_settings`,
domains/services.rs hooks; the engine still reads it for RotationType and the
game menu), then take the normal write path, so Changed and
GetPropertyChangedSignal fire as for any Instance. `DebuggerManager()` is a
C function wanting Plugin like `settings()` (identity 1 gets the
DebuggerManager if one exists). Globals probe: **7 → 1** mismatch
(`misc.time` only); tests `bridge_settings_globals_are_instances`.

Gates (same-time baseline build without these changes vs with): Place B
Newer 60 s, no ANTI-EXPLOIT/kick/tamper lines, identical 7 Output errors;
Place A `--bench --seconds 20` ×2 each, 0 Output errors both, script/UI cost
ratio unchanged (p6b_lua/dock_panels 0.49–0.51 both; absolute frame times
tracked load average 12–17); a safeenv microbenchmark (fastcall builtins,
pairs, imports) equal within noise. Engine-internal errors in Output lose
their `rustudio.*:line` prefixes (Studio's native functions have none
either). Tests: `globals_parity::tests` (5).

## Bridge parity (2026-11-08)

Probe rows matching Studio: **394 / 1250 before, 1230 / 1250 now.** The 20 left: Destroy (6 rows × 2 sides, only in Studio's earlier pass, not in the final corpus), and not bridge behaviour: `UserInputService.IsWindowFocused` exists here, `Player.Move` lacks Studio's WritePlayer capability refusal, RealtimeMedia is missing from the reflection database (GetService errors instead of nil), and GetEnums counts 656 enums (Studio 652: our database is a slightly different version).

**Follow-up (reflection version).** `app/api-dump.json` is now Studio 0.744.0.7441077's own dump: the offline Studio copy (`Studio`, SHA256 81be5bc2…, the build every probe ran on) writes it with `OFFLINE_STUDIO_HIDDEN=1 offline-studio -API 'Z:\…\api.json'` and exits. Against the old dump: 942 classes (−FeatureMatchingService, FeatureMatchingSession, IntegrityService; +BulkImportService), 652 enums (−PlayerDeviceTrustDecision, PlayerDeviceTrustLevel; PartType loses Capsule), 11 members added, 50 gone, 10 changed. `tools/gen_stubs.py` regenerated src/api_stubs.rs (4646 → 4618 members). What scripts see now follows the dump:

- `Enum:GetEnums()` and `Enum.X` are the dump's enums (api.rs keeps enums
  with no items, AudioCaptureMode / IntentReplicability); the rbx-dom
  reflection database (`rbx_reflection_database` 3, another version) still
  backs file I/O, defaults and engine-side enum lookups, which is why it
  isn't swapped.
- GetService: a name the dump lacks is "not a valid Service name" whatever
  the database says; any other non-service class answers nil
  (RealtimeMedia, like Terrain or AnimationTrack), after creating-time
  capability checks for creatable ones, so `GetService('Player')` from a
  game script is `The current thread cannot create 'Player' (lacking
  capability WritePlayer)` (Studio's library.Player.Move row).
- `UserInputService.IsWindowFocused` is gone from scripts (not in 0.744;
  WindowFocused/WindowFocusReleased still fire).

The corpus now runs as Studio's did, as a Script and a LocalScript named
BridgeProbe (`tools/parity_probe/bridge/run.sh OUT; compare.py OUT`; the
earlier runs went through the harness at identity 1, where Instance.new
('Player') is allowed). **Real mismatches 8 → 0** (1238 of 1250 rows
measured and matching; the 12 Destroy rows are only in Studio's first pass).
Security corpus against `studio-delayed.txt` still 0 real mismatches with
the new dump. Not matched, and why: the database-backed paths (property
defaults, serialization) remain rbx-dom's version; no 0.744 rbx-dom
database was built here.

| Check | Roblox | rustudio before | now |
|---|---|---|---|
| `getmetatable` of Instance, signal, connection, Enum, Enums, EnumItem | "The metatable is locked" | the metatable / false | locked (set raw: mlua refuses a `__metatable` field) |
| Metamethod called with bad arguments (`f()`, `f(o)`, `f(o, {})`, `f(o, nil)`) | Luau's `missing argument #1 (Instance expected)` / `invalid argument #2 (string expected, got table)` with the caller's position; order per type (Instance, connection, EnumItem, Enum reads: self first; signals, Enums and every write: key first) | mlua's `bad argument #2: error converting Lua nil to string` | as Studio (src/cmeta.rs `Spec`, `luaL_typeerror`) |
| `o[1]` | `1 is not a valid member of Part "Part"` | — | number keys convert |
| `game.XYZ` | `… of DataModel "Game"` | `"…"` empty | the DataModel by its name |
| `inst:Method()` | a C `__namecall`, unnamed, different from `__index` | none: `__index` then a Lua closure | C `__namecall` reading `lua_namecallatom` on entry; one Rust call per namecall |
| `workspace:Name()`, `p:Changed()`, `folder:Child()` | `X is not a valid member of …` | `attempt to call a string value` | as Studio |
| `debug.info(game.GetService, "slna")` | `[C] -1 GetService 0 true` | `rustudio.prelude 15` | named vararg C function |
| Method identity | one per descriptor (`Part.IsA == game.IsA`); Enum / signal / connection methods new each read | per name; signal methods shared | as Studio |
| Bad self (`game.GetService({})`, `f.WaitForChild(nil)`) | `Expected ':' not '.' calling member function X` | WaitForChild: `rustudio.prelude:173: attempt to index nil` | checked before anything, Lua-written methods included |
| `sig.Connect()`, `Enum.Material.FromName({})` | `missing argument #1 to 'Connect' (RBXScriptSignal expected)` at the caller | mlua conversion errors | as Studio |
| `GetService({})`, `(true)`, `(3)`, `()` | `'{}' / 'true' / '3' is not a valid Service name`; `Argument 1 missing or nil` | `Argument 1 missing or nil (expected string, got table)` | as Studio; a non-creatable class (WorldRoot, AnimationTrack) answers nil |
| `p.Parent = 3` | `…:30: invalid argument #3 (Instance expected, got number)` | `Unable to assign property Parent. Instance expected, got integer` | as Studio (all Instance-typed properties) |
| tostring(Enum), GetEnums, FromName/FromValue, `Enum.Material == Enum.Material` | `Enums`, 652, item or nil, true | `Enum`, missing, missing, false | as Studio |
| EnumItem writes (even Name) / unknown reads | `Name is not a valid member of EnumItem` / `XYZ is not a valid member of "Enum.Material.Plastic"` | VM `attempt to index`, nil | as Studio |
| Connection: tostring, unknown read, writes | `Connection`, `X is not a valid member of RBXScriptConnection` | `RBXScriptConnection: 0x…`, nil | as Studio |

Estimated (no Studio row): writes to Enum / Enums say `X is not a valid
member of "Enum.Material"` / `"Enum"`; a non-empty table converts to a
string argument as `Unable to cast table to string`.

Lua-written methods (WaitForChild, Invoke, InvokeServer, InvokeClient,
library.luau's) are named C functions (`cmeta::method`) that check self and
then call the Lua body with `lua_pcallyieldable` and a continuation, so they
still yield from scripts, coroutines and pcall. A nested `lua_call` there
(Luau's `lua_callyieldable`) crashed the resume when a script `pcall` inside
the body caught an error (fast-pcall handler frame with a stale `errfunc`);
the continuation re-raises failures instead. `__rs_lua_methods` is read from
the registry: inside a callback `lua.globals()` can be a script environment
now (globals_parity).

Performance (alternating before/after runs, minimum of 5, ns per op,
demo client; the machine was shared with other builds): index Name 265 →
320 (includes the security agent's index hook, landed between the two
binaries), index Anchored 866 → 860, newindex 808 → 790, namecall IsA 1057
→ 844, FindFirstChild 1207 → 998, GetAttribute 1087 → 888, Enum index 491 →
455. Place A `--bench --play --seconds 20`, three alternating pairs under load
average 10–17: work 3.78–4.01 ms before, 3.19–4.20 after (client Lua
1.18–1.23 → 1.00–1.29); no regression beyond noise. Place B Newer plays 60 s
with an admin script loaded and no exploit / kick / tamper line, the same Output
errors as before. Tests: `runtime::bridge_tests` (8, src/selftest/bridge.rs).

## Datatype parity (2026-11-08)

- **SIGILL (crash.txt).** Luau was built with `LUAU_ASSERT` on in release:
  `vendor/luau0-src` used the build script's own `cfg!(debug_assertions)`,
  and `Luau/Common.h` enables asserts whenever `NDEBUG` is unset (cc never
  sets it). A script that captured mlua's internal `error` (the raiser of an
  unknown datatype field, via `debug.info(2, "f")`) and called it with no
  arguments tripped `api_checknelems` → `LUAU_DEBUGBREAK` → `ud2`. Asserts
  now follow the target profile and release defines `NDEBUG`. Regression
  test: `ltypes::direct_call_tests`.
- **Metatables (src/dtmeta.rs).** Each datatype's mlua metatable is rebuilt
  in place from mlua's own getter/setter/method tables (the generated
  closures' upvalues): `__index`/`__newindex`/`__namecall` are unnamed C
  closures shared by every value of the type, `__metatable` is locked.
  Two families: plain (`X is not a valid member of T`, separate namecall)
  and quoted (`'X' is not a valid member of T`, namecall goes through
  `__index`), per the `index.namecall.same` rows. Known read-only members
  say `X member of T is read-only and cannot be assigned to`; Vector3 says
  `X cannot be assigned to`. Direct calls check the key (#2) before self.
  Methods are new C closures named after the method on every read, with
  Luau (`missing argument #1 to 'Dot' (Vector3 expected)`) or reflection
  (`Argument 1 missing or nil`, `Unable to cast Array to Vector2`) self
  errors per type. Operators Studio lacks are removed so the VM reports
  them; the rest are C closures. Signal/Connection metatables are the
  bridge's (cmeta), so Signal:Wait can still yield.
- **Messages.** mlua conversion failures become Luau's argument errors with
  the caller's position; constructor tables are frozen, their functions
  named (PhysicalProperties.new is `newPhysicalPropertiesWithClampedMsgClosure`)
  and lenient where Studio's are (`Vector2.new('a')` is 0, 0). Connect/Once
  with a non-function return a dead Connection and report
  `Attempt to connect failed: Passed value is not a function` to Output;
  ConnectParallel outside an Actor fails first.
- **tostring.** C `%.9g` (Vector2, CFrame, UDim, UDim2) or `%g` (Color3,
  NumberRange, sequence keypoints, TweenInfo, PhysicalProperties) of the
  float32; params/Content print Studio's `Key=value` text.
- **Vector3 is Luau's native vector**: `type` "vector", `typeof` "Vector3",
  rawequal when equal, one table key, float32, `-0`, Luau's own text and
  arithmetic. `LVector3` converts to/from `Value::Vector`; generic
  `ud::<T>` helpers see vectors through `ltypes::vec_any`.

Corpus (`run.sh OUT; compare.py OUT`, rows normalized for script paths and
pointers; 5952 Studio rows):

| Build | Measured | Mismatches | Direct-call mismatches |
|---|---|---|---|
| before (SIGILL fixed, nothing else) | 5693 | 1723 | 754 |
| + metatables (item 2) | 5823 | 508 | 66 |
| + messages (item 3) | 5823 | 184 | 52 |
| + tostring (item 4) | 5823 | 135 | 52 |
| + native Vector3 (item 5) | 5823 | 107 | 40 |

The 107 left (client + server rows): RaycastResult (56) and SharedTable (47) are still Lua tables, PhysicalProperties.new('a') prints "default" in Studio (2), and two fetches of one Signal are rawequal here (2).

**Item 6 and the rest (follow-up).**

- **RaycastResult** (`ltypes::LRaycastResult`): read-only userdata, the
  quoted dtmeta family (`'X' is not a valid member`, `Distance member of
  RaycastResult is read-only…`, namecall through `__index`), tostring
  `RaycastResult{Part @ x, y, z; normal = …; material = Plastic}` (`%.9g`,
  the hit's name when cast), `==` by content (Studio: two casts are equal,
  not rawequal). Its metatable is rebuilt from a sample in `install_all`.
- **SharedTable** (src/sharedtable.rs): userdata whose elements live in a
  Rust-only Lua table; unnamed C `__index`/`__newindex` (cmeta `wrap`),
  `__iter` over a snapshot, no `__namecall` (`st:f()` is the VM's "attempt to
  call missing method 'f' of SharedTable"), no `__tostring` (Luau's default
  prints `SharedTable: 0x…` from `__type`), no operators, identity equality,
  locked metatable. The `SharedTable` global is frozen and its functions
  (new, clone, cloneAndFreeze, isFrozen, size, clear, increment, update)
  are dtmeta-wrapped constructors (named, `invalid argument #1 to 'new'
  (table expected, got string)`). Frozen = the inner table is read-only.
  rustudio runs every script and Actor of a side in one Luau state, so a
  SharedTable is shared across Actors by reference and
  SharedTableRegistry:GetSharedTable hands out one object per name.
  Estimated (no Studio row): string/number keys only, functions and threads
  refused as values, plain tables stored are copied into SharedTables,
  writes to a frozen one say "attempt to modify a readonly table",
  `increment` treats nil as 0 and returns the old value.
- **Signals**: every read of an event is a new RBXScriptSignal userdata over
  one state table per (instance, event) (weak-valued cache keyed
  "slot:event"; the state keeps the cache entry alive while any object
  does). `==` compares the state; connections, Wait and gates read the
  state, so they are unchanged.
- **PhysicalProperties.new('a')**: a lone non-number is cast to a Material;
  a known Material name gives its properties, anything else the "default"
  value (tostring `default`; assigned to a part it means no custom
  properties).

| Build | Measured | Mismatches | Direct-call mismatches |
|---|---|---|---|
| + item 6, signals, PhysicalProperties | 5873 | **0** | 0 |

(Measured rows rose because RaycastResult/SharedTable rows that used to stop
early now run; the 79 unmeasured are rows Studio printed and the corpus
doesn't reach here, as before minus those.) Tests: `sharedtable::tests` (2),
`dtmeta::tests::{physical_properties_default, raycast_result_metatable}`,
`bridge_signal_reads_are_fresh`, `bridge_raycast_result_userdata`.

Gates for the three follow-ups together (same-time baseline = HEAD a92978c
plus the other agents' uncommitted files, built separately): Place B Newer
60 s, no anti-exploit/kick/tamper line, the same 7 Output errors as the
baseline; Place A `--bench --play --seconds 20`, 0 Output errors in all 18 runs.
Avg frame / client Lua (ms), pinned to cores 0–7, alternating: baseline
3.07 / 0.819, 3.06 / 0.815, 3.11 / 0.833; now 3.10 / 0.819, 3.25 / 0.849,
3.41 / 0.899 (best runs within 1 %, medians ~5 % apart; a fresh signal
userdata per event read is the likely cost, Studio allocates one too).
Unpinned under another agent's load, 3 of 6 runs of this build were ~1.6×
slower in every section (rendering and GUI included), the baseline's never;
not reproduced pinned. `--self-test` 62/64 (perf, fidelity-place_a as before).
`cargo test --release`: 384 passed, only product_info_and_engine_error_text
and script_library_gaps fail (pre-existing).

Gates after items 2, 3 and 5: Place B Newer 60 s with no anti-exploit/kick/
tamper lines and the same Output errors as before; Place A 20 s with no Output
errors; `--self-test` 62/64 as before (perf, fidelity-place_a failed before
too). Place A bench (headless, 20 s, frame / Lua client / Lua client 2 /
server, ms): before 9.68 / 2.43 / 2.23 / 0.30, item 2 4.15 / 1.10 / 1.02 /
0.11, item 3 3.81 / 1.03 / 0.96 / 0.10, item 5 3.29 / 0.85 / 0.79 / 0.09.
The first drop is not isolated (other agents' commits and concurrent
builds landed between those runs); native vectors cut Lua time again
(0.96 → 0.79 on otherwise unchanged code).

### Editor model pivot probe (2026-11-10)

Owned hidden Studio PID 1880 (slot 4), `/tmp/rustudio-drag-reference.rbxlx`:
`Model.WorldPivot = CFrame.new(23,5,1)` with a descendant Part at (20,2,0).
`PivotTo(GetPivot() + Vector3.new(2,0,0))` yielded pivot (25,5,1),
part (22,2,0), identity rotation. Setting PrimaryPart and its PivotOffset to
(1,2,3) yielded GetPivot (23,4,3), identity. These are the numerical
fixtures in `part_drag::model_pivot_tests::studio_world_pivot_translation_and_undo`.
Editor model handle frames now use PrimaryPart.CFrame * PivotOffset,
otherwise WorldPivotData, otherwise descendant world bounds. Translation and
rotation previews transform stored model pivots by the same rigid transform
as the parts, recording the original optional property for undo.

Additional Studio mouse observations (862x675 viewport, camera at (0,20,20)
looking at (0,2,0), 4x4x4 Probe initially (0,2,0)):
viewport drag (430,380) -> (580,420), grid enabled: (12,2,0), identity;
onto a 12x4x12 target at (9,2,0), endpoint (580,330): (9,6,-4), identity.
These two mouse observations have not yet been reproduced in rustudio and
must not be counted as parity tests. Studio screenshot before dragging:
(a Studio screenshot, not kept).

Private-copy verification of the editor pivot probe: `cargo test --release
model_pivot_tests` passed (1 passed, 0 failed, 465 filtered). Log:
`/tmp/drag-private-tests.log`. The copy restored only the incompatible
Terrain Editor and dependent selftest/ui_shots files to HEAD and removed
its terrain_ui module declaration; the real checkout was not restored.


### Terrain palette uploads and overrides (2026-11-10)

`terrain_mat::palette_generation` fingerprints the current MaterialColors bytes,
material table selection and resolved terrain variants on each upload check.
A runtime palette write therefore invalidates GPU parameters independently of
mesh reconstruction. `terrain_override` resolves MaterialService.<Material>Name
only against MaterialVariants with that BaseMaterial. Authored ColorMap,
NormalMap, RoughnessMap and MetalnessMap use the asynchronous material-pack
loader; StudsPerTile controls projection density and variant faces have no
random rotation or offset. These paths still require Studio image parity
verification; the 25 original flat-reference MAEs are invalid as texture scores.

## Freeform drag parity (2026-11-10)

Reference: own hidden Studio, Select tool, 862x675 view,
`Camera.CFrame = CFrame.lookAt((0,20,20), (0,2,0))`, FieldOfView 70, a
512x4x512 locked baseplate at (0,−2,0); snapping through
`DraggerService.LinearSnapEnabled / LinearSnapIncrement` (scriptable). Mouse
drags were sent with a scripted mouse `drag`; R / T were pressed by a second, concurrent
`key` call while the drag (6 s, 60 steps) was held. Studio's own SelectDragger plugin scripts
(DraggerFramework.Utility.DragHelper, getGeometry, JointPairs, PartMover, Math;
DraggerSchemaCore.FreeformDragger, SelectionInfo, dispatchWorldClick,
TransformHandlesImplementation, ExtrudeHandlesImplementation;
DraggerContext_PluginImpl). `Camera:ViewportPointToRay(431, 337.5)` returned the
LookVector exactly: the mouse ray uses no half-pixel offset (as `camera.rs`).

| Plugin function | What it does | rustudio |
|---|---|---|
| DraggerContext_PluginImpl.getGridSize / shouldGridSnap | `DraggerService.LinearSnapIncrement`; snapping is off while Shift is held (getGridSnapEnabled, used for the grab point, ignores Shift) | `drag_grid`, `PartDrag::shift` |
| areCollisionsEnabled / shouldJoinSurfaces | `plugin.CollisionEnabled`; `plugin:GetJoinMode() ~= None` (both off in a fresh Studio; the ribbon actions toggleCollisionCheckAction / toggleJointCreationAction flip them) | `Editor::collisions` / `join_surfaces`, menu on the Geometric button; `mech::*_ON_BY_DEFAULT` = false |
| SelectionInfo.computeInfo | box basis = `BasisObject:GetPivot()`; BasisObject is the last object collected, measured to be the grabbed one when several are selected (UseLocalSpace made no difference); parts' boxes in that frame; with more than one object the basis moves to the box centre | `DragBox::new`, `start_part_drag` |
| dispatchWorldClick.buildFreeformDragInfo | grab point rounded on X / Z of the clicked surface's TargetMatrix, `floor(x/grid + 0.5)*grid` | `FreeDrag::begin` |
| getGeometry | vertex / edge / face tables (Brick for blocks, balls, cylinders; Wedge; faces carry point, normal, `direction`, CCW vertices) | `Solid::geometry` (same order, 0-based) |
| getSurfaceMatrixForPrimitiveMesh (+ getClosestFace, buildSurfaceMatrix) | closest face = min \|(hit−point)·n\|; of the edges not above that face's plane (`(v−point)·n < 0.001`), the one whose line passes nearest the hit; origin = its nearer end; `CFrame.fromMatrix(origin, n × edge, n)` | `surface_matrix` |
| getSurfaceMatrixForSurfaceExtension | nothing under the mouse: the last target's plane, extended; no last target: no move | `FreeDrag::surface` |
| getDragTargetNew + computeDragSnapping | R7 = (M⁻¹·basis).Rotation; size in surface = AABB of the box rotated by R7·tilt; hit rounded on M's X / Z; lift `0.5·size.Y − (R7·tilt·(offset − draggedPoint)).Y`; `mainCFrame = M·(x,0,z)·(0,lift,0)·R7·tilt·(−draggedPoint)`; soft snaps only with Snap to Parts (off) | `FreeDrag::target` |
| updateTiltRotateNew (DraggingParts.processKeyDown) | R: axis = target UpVector under the mouse; T: `cameraCFrame:VectorToWorldSpace(1,0,0)`; the basis axis (±X, ±Y, ±Z of the original box) most aligned with it; `tilt = roundRotation(fromAxisAngle(a, π/2)) · tilt` | `FreeDrag::turn`, `rotate_part_drag` (re-runs the drag) |
| JointPairs.new / tryToCreateJointPair / createJoints | candidates within `|Δpos| ≤ |size₀|/2 + |size₁|/2 + 0.05`; faces with normals dot < −0.999 and planes within 0.05 that overlap (isVertexInFace with 0.01 margin, either way, or edgeIntersectsEdge); cylinders only on end faces; no joints in a Humanoid model; one `Weld` per pair parented to the moved part, C0/C1 = part⁻¹ · fromMatrix(face centre, direction, normal × direction) | `joint_between`, `surface_joints`, one undo step (`Document::record_drag`) |
| TransformHandlesImplementation._safelyTransformParts | Collisions: wanted transform overlaps (`ArePartsTouchingOthers`, 0.001 ignored) → bisect from the last good one, 32 steps, stop within 0.0005; within 0.001 of the last good → keep it; a target past the obstacle that doesn't overlap is accepted | `safe_amount`, `solids_overlap` (SAT), `Editor::overlaps_others`; Move / Rotate / Scale handles |
| ExtrudeHandles._binarySearchForGoodPartSize | the same search on the size fraction (32 halvings) | Scale handles use `safe_amount` on the amount |

Measured final CFrames (position; rotation shown when not identity). All are
unit-tested in `part_drag::studio_parity_tests::studio_freeform_drags` (Studio
values as expected, ≤ 0.001 studs, rotation to Studio's 5 printed decimals) and
replayed through the editor in `selftest/drag_parity.rs` (tests/drag_parity.rs).

| Case (press → release px, snap) | Studio | rustudio |
|---|---|---|
| 2x4x6 on baseplate, (441,319)→(538,298), snap 1 | (6, 2, −7) | (6, 2, −7) |
| same, snap 0.5 | (7, 2, −7.5) | (7, 2, −7.5) |
| same, snap off | (6.78342, 2, −7.04042) | (6.78342, 2, −7.04041) |
| snap 1, R | (6, 2, −5), rot Y +90° | same |
| snap 1, T | (6, 3, −8), rot X +90° | same |
| snap off, R then T | (6.51291, 1, −8.25159), rows (0,0,1)(1,0,0)(0,1,0) | same |
| onto 30° wedge slope, →(552,300), snap 1 | (6, 4.73205, −2.26795) | same |
| same, snap off | (6.95338, 4.45120, −1.78150) | same |
| same, snap off, R | (6.68287, 3.01916, −1.30114), rot Y +90° | same |
| onto a 12x4x12 part's top, →(583,221), snap 1 / off | (9, 6, −11) / (9.79344, 6, −10.38101) | same |
| onto a wall's +Z side, →(584,232), snap 1 / off | (8, 3, −3) / (8.75779, 3.20530, −3) | same |
| part turned 30° about Y, R / T, snap off | (7.00563, 2, −5.93604) / (6.19330, 3, −7.92103) | same |
| group (cube + 30° cube), snap 1 / off | Probe (6,2,−7), Probe2 (2,1,−4) / +(0.78342, 0, −0.04042) | same |
| group with a 30°/20° tilted cube, grabbing the cube / the tilted one | Probe y 2.11 / 5.87141 (box in the grabbed part's frame) | same |
| same, R then T | Probe (6.51291, 5.28171, −8.25159) | same |
| Model, no PrimaryPart, no WorldPivot | pivot (1,2,0) → (7,2,−7) | same |
| Model, WorldPivot (5,7,1) turned 30° Y; snap off | pivot (11,7,−6) / (11.78342, 7, −6.04042) | same |
| Model, WorldPivot turned (30°, 0, 15°), snap off | parts lifted to y 4.62708 (box in the pivot's frame) | same |
| Model, PrimaryPart turned 30° Y / 25° X | pivot (9,1,−7) / (9.78342, 2.53209, −7.04042) | same |
| Model with tilted WorldPivot, T | pivot (11.81823, 5.49907, −3.70759) | same |
| Join Surfaces, probe dropped at (6,2,−7) | `Weld` in Probe, Part1 Baseplate, C0 (0,−2,0) rows (1,0,0)(0,0,−1)(0,1,0), C1 (6,2,−7) | same (`studio_join_surfaces`) |
| Join, against the wall's +Z face | C0 (0,0,−3) rows (1,0,0)(0,−1,0)(0,0,−1), C1 (0,−1,2) | same |
| Collisions, Move arrow +X into a 2x8x8 wall at (8,4,0) | x = 5.00074 (off: 7.98286) | 5.0007 ± 0.001 over the same 16 mouse steps (`studio_move_handle_collision`) |

Visual parity (screenshot pairs, Studio left, rustudio right, both 862x675
with the same camera; rustudio headless under gamescope): selection is drawn
as Studio's Outlines mode, a ~2.5 px band of Select Color (0.1, 0.6, 1) around
the silhouette (convex hull of the projected shape) instead of a box
wireframe; hover is a 4 px band of Hover Over Color (0.7, 0.9, 1)
(DraggerService.HoverThickness = 4) and is not drawn while dragging; Move /
Scale / Rotate / Transform show a white pivot dot ringed in Select Color; a
free drag hides the handles and shows the hotkey list (T, R, Alt, Shift,
Shift+X/C, H; H hides it); a Move arrow drag draws only that arrow, a white
guide along its axis and the distance in a label, and the 4-row hotkey list.
Not built: the dragged-point marker and snap relation lines
(_renderSnapRelation / TargetGridView), Studio's arrow size and thickness
(its arrows are about 1.7x longer here), Align Dragged Objects, Snap to Parts
soft snaps, Physical (constraint-solving) mode, terrain targets, corner wedge
and mesh surface frames (treated as their boxes), and the tilt animation.

## Ctrl+R / Ctrl+T: RotateSelectionAction / TiltSelectionAction (2026-11-11)

Both are native C++ actions (RTTI `.?AVRotateSelectionAction@Studio@RBX@@`
TD Studio, vtable Studio, ctor Studio;
`.?AVTiltSelectionAction@Studio@RBX@@` TD Studio, vtable Studio,
ctor Studio). Their trigger (vtable) builds a 3x3 matrix and calls
the shared Studio, which runs the legacy part mover (Studio …
Studio) and sets the waypoint named by vtable +8 of the
sub-object at.

| Piece | Studio | rustudio |
|---|---|---|
| Rotate matrix | constant at Studio, rows (0,0,1)(0,1,0)(−1,0,0): 90° about **world +Y** (`CFrame.Angles(0, π/2, 0)`), regardless of camera or part orientation | `ROTATE_MATRIX` |
| Tilt matrix | camera CFrame (): θ = atan2(R02, R22) = heading of the camera's back vector; `q = floor((θ + 2.25π)·4/2π) & 3`; q 0 → Rx(+90°), 1 → Rz(−90°), 2 → Rx(−90°), 3 → Rz(+90°) (q 1 and 3 are the constants at Studio, q 0 and 2 are written inline). That is a forward quarter turn about the world horizontal axis nearest the camera's right; pitch is ignored; quarter 0 is θ ∈ [−45°, 45°), so exactly 45° already counts as quarter 1 | `tilt_matrix` |
| Parts | every selected PVInstance (Models, parts; Folders etc. are searched for PVInstances), expanded to their parts; **Locked parts turn too**; deduplicated through a `std::set` keyed by the parts' shared_ptr control block, so the list ends up in **heap-address order** | `selected_parts()` (selection order) |
| Centre | the frame of the *last* part of that list (a "largest part" loop whose running maximum is never updated: `comiss xmm1, xmm6` with xmm6 = 0, so any part with area wins); the box around every part's corners in that frame; its centre back in world space | `quarter_turn_centre` with the last selected part |
| Transform | each part: `T · M · T⁻¹ · cf`, T = translation to the centre; Model WorldPivots (no PrimaryPart) get the same rigid transform | `quarter_turn_selection` (+ `apply_model_pivots`) |
| Rounding | every CFrame component to the nearest 1e-6 (`ceil(v·1e6 − 0.5)/1e6`, mirrored for negatives; \|v\| < 1e-5 → 0), then Gram-Schmidt orthonormalisation of the columns. **No grid snap**, no push out of overlaps (Studio afterwards only updates contacts) | `round_turned` |
| Undo | waypoint `"SelectionRotate"` / `"SelectionTilt"`; `ChangeHistoryService:GetCanUndo()` returned `true, "SelectionTilt"`. Nothing to turn → no waypoint | `record_props("SelectionRotate" / "SelectionTilt")` |
| Enabled | whenever something is selected (disabled with an empty selection; enabled for an empty Folder, which then does nothing); the trigger has no tool or drag check, and it worked with the Move tool active | always bound; no tool check |
| Bare R / T | only bound to disabled plugin actions (LayeredClothingEditor LockControl, PathEditor AddTangentMode); outside a part drag they do nothing in the 3D view. During a drag they are DraggingParts.processKeyDown (updateTiltRotateNew, see "Freeform drag parity"), unchanged and still covered by the flat_rot_r / flat_tilt_t / rotated_r / rotated_t cases | `rotate_part_drag` only while `part_drag` is set |

Live check: own hidden Studio, anchored parts
A (1.3,2.7,−0.4)·Angles(0.3,0.5,0.1) 2x4x6, B (6,1,3)·Angles(0,0.4,0) 1x2x3,
C (−3,5,2)·Angles(0.2,0,0.7) 3x1x1, a Model holding A and B with WorldPivot
(5,7,1)·Angles(0,0.5,0.2), the actions run through `invoke_action`. Each
measured centre matched the corner box in exactly one part's frame (to 1e-5),
always the last of Studio's list: {A,B,C} → C, {C,B,A} → A, Model{A,B} → B,
{A,B} → B, {B,A} → B, but {A,B} with B locked and Folder{A,B} → A, i.e. the
order is not the selection order (heap order, effectively arbitrary). With
parts that share an orientation the frame makes no difference. A single
rotated part turned in place; a Model's pivot moved with its parts
((5,7,1) → (3.06465,7,−1.68193)); a PrimaryPart model's pivot followed its
PrimaryPart; a 1x3x8 part at (0.5,1.5,0.25) on a baseplate tilted into the
baseplate and stayed at (0.5,1.5,0.25). Tilt from cameras at (0,20,20),
(20,10,5), (−6,10,−20), (−20,5,3) looking at (0,2,0) gave Rx(+90), Rz(−90),
Rx(−90), Rz(+90). Ctrl+R pressed in the middle of a mouse drag was not
measured (the hidden window can't hold a drag and a chord at once); from the
code it turns the parts and the drag's next mouse move puts them back on its
own transform, which is what rustudio does too.

All of these cases are `part_drag::quarter_turn_cases::CASES`, run through
`quarter_turn_selection` (CFrames, Model pivot, then undo) in
`part_drag::quarter_turn_tests::studio_quarter_turns`, with Studio's order
given as the part order; `tilt_quarters_follow_the_camera_heading` covers the
quarter seams and `turned_cframes_are_rounded_like_studio` the rounding.

## Terrain against textured Studio (2026-11-10)

**Reference.** Studio fetches terrain TexturePacks as alternate content
representations (`assetType` TexturePack, priority list `ktx2` / `7rdo`,
fidelities `AkA=` / `IkA=` / `QkA=`, `doNotFallbackToBaselineRepresentation`)
and draws flat avgAlbedo from the baseline XML. Studio now serves Roblox's own
KTX2 files, mirrored anonymously into `~/.local/share/Studio/assets/acr/<id>/
ktx2-7rdo-<fidelity hex>.bin` (+ `.json`, the batch reply's metadata) by
`assets.py`; `routes.py` reloads `assets.py`. Wine does socket and file I/O
in the shared wineserver, so every Studio's HTTP reaches the first launch's
the local backend: per-launch env or bind mounts don't apply. Studio's log then shows
`Terrain TextureArray 512x512, arraySize: 37, mip: 10`.

**Rig.** `tools/fidelity/terrain_closeup.py` (`--reuse-studio DIR`, `--only`):
Studio's 3D view runs under its 38 px device bar (the vertical FOV spans all
675 px), so rustudio renders the full viewport and both lose the bar's rows;
the old 637 px rustudio view was 5.7 % zoomed and shifted 19 px (per-patch
shift fields, `shiftmap`). The overlay chip and gizmo are masked.
25 close-ups: mean MAE 20.6 → 5.5, 20 under 8
(`~/.cache/rustudio/terrain-cmp/report.md`).

| What | Studio (Studio 0.744) | rustudio |
|---|---|---|
| Terrain textures | the KTX2 representations: BC1 / BC3, zstd, 512² pre-2022 / 1024² 2022, full mips. AkA= albedo (pre-2022 YCoCg: R = Co + ½, G = Y, B = Cg + ½; 2022 RGB + tint mask in A), IkA= normal (X in A, Y in G), QkA= specular (G roughness, B emissive mask, A height). Pre-2022 albedo is ~7 levels darker than the pack's colour PNG | `matpack::build_acr` (fetched like Studio, cached `<id>_acr_ktx2-7rdo-<fid>.ktx2`; `RUSTUDIO_TERRAIN_ACR=0` or offline falls back to the pack maps) |
| Array size | 512²: a 1024² layer's level 1 | shader: LOD never below 1 |
| Tiling | CB2.x = table tiling × 0.25 (per 4-stud voxel); Brick 0.8 → a tile every 5 studs | `terrain_mat::VOXEL_STUDS` |
| Detiling | CB2.y = 1 / table detiling; offset (r, ⌊2.6651442 r⌋) × CB2.y | `Tex::detiling` |
| Axes | 18 U / V vectors (.rdata Studio; diagonals 0.7): 0 +X … 3 −Z, 4–7 XZ diagonals, 8 up, 9–12 tilted up, 13 down, 14–17 tilted down. Chooser Studio: y > 0.9 up, > 0.4 tilted, ≥ −0.6 side (diagonal when the other component ≥ half), ≥ −0.8 tilted down; cube materials the dominant axis | `terrain::AXIS_U / AXIS_V / studio_axis` |
| Axis normal | soft: vertex normal; hard: face; hardsoft: vertex while dot(face, vertex) > 0.75, else face | `chunk_triangles` |
| Face | axis 0–7 side, 8–12 top, 13–17 bottom; a missing bottom is the top | `face_of_axis`, `terrain_mat::parse` |
| Corner random | boost hash_combine of the cell's global voxel index, then Wang's hash; byte 0 = texture random, corner k = its own cell | `studio_cell_hash` |
| Quad diagonal | q1–q3 when q1, q3 share a material q0 or q2 has (every flat one-material quad), else q0–q2 | `chunk_triangles`, `build_meshes` |
| Deform | one mode per material (last of shift, cubify, quantize): shift ± amount × (2 b / 255 − 1), b = hash bytes 0 / 1 / 2; then clamp to the cell | `cell_vertex` |
| Vertex normals | area-weighted sum of touching triangles after deform | `chunk_triangles` (rim cells keep the field gradient) |
| FS HQ | YCoCg texels blended then decoded unclamped; normal texels blended, bump × 1/|w|, frame (N.z,0,−N.x)/(0,1,0) for axes 0–7, (−N.y,N.x,0)/(0,−N.z,N.y) for 8–17; F0 0.04 env BRDF reflection × EnvironmentSpecularScale; emissive = Σ bary CB5 × mask | `fs_terrain` |
| Emissive | static defaults: CrackedLava strength 2.0, tint white; others 0 | `terrain_mat::CRACKED_LAVA_EMISSIVE` |
| MaterialColors | no rebake: a CB4 shader variant. Built-in (pre-2022) layers: HSV of default and current colour from sRGB / 255, x = clamp(V/V₀, 0, 2), s = clamp(S/S₀, 0, 2), θ = H − H₀, CB4 = (x, cos θ·s·x, sin θ·s·x); Y × x, chroma turned. Other layers (2022, MaterialVariant): CB4 = colour / 255, albedo × mix(1, colour, mask), always | `terrain_mat::recolor`, `set_override_tint` |
| Water cover | CB3[2] = (T / (1990 T + 10), 1 − T, WaterReflectance): cover = clamp(depth × .x + .y); WaterColor squared | `water.rs` (Water 41.5 → 12.6) |

Measured on the rig: live Concrete 127,102,63 → 220,40,70 comes out pale
pink in both (rustudio Play vs Studio command bar, MAE 1.98); the
MaterialVariant override (Brick maps, 8 studs per tile) MAE 5.54.

Open: 2022 materials are unverified (this Studio draws the pre-2022 table even
with `Use2022MaterialsXml` true in the file); Studio lowers water vertices by
half a voxel (not applied); the TerrainVirtualTexture path; Rock / Cobblestone
bumps a little stronger than Studio's (sun vs ambient balance).

## Round 27: the Humanoid as a dynamic assembly (the only character path)

The kinematic capsule of rounds 22 to 26 is gone (its code, `RUSTUDIO_HUMANOID`, `mech::CAPSULE_*`, the climb /
turn / ragdoll timers fitted for it). A character is one rapier body driven by a port of Studio's `HUMAN::` state
machine. The specification with every address is `checks/humanoid-physics/SPEC.md`; what is measured against
Studio is `checks/humanoid-physics/RESULTS.md` (probes T1 to T19 from the agents' Studio) and
`checks/humanoid-physics/ticks/` (Studio stepped one 240 Hz tick at a time).

| What | How | Source |
|---|---|---|
| World step | 240 Hz for every body (`mech::PHYSICS_HZ`), at most 16 a frame | Studio's world step |
| State machine, forces | `src/humanoid.rs`: Running (hip-height spring on the floor rays, horizontal PD with the friction limit, yaw servo, balance torque P 5000 / D 100), Jumping, Freefall, Landed, Climbing, Swimming, Seated, PlatformStanding, FallingDown, GettingUp, Ragdoll, Dead | Studio and the state classes' force functions (SPEC §2) |
| Timestep level | a Running character at WalkSpeed 8 to 16 is solved on every 2nd tick over 2h and is out of the solver in between; a drop of level is immediate, a rise waits 60 ticks | Studio; probes T1, T3, T10 |
| Jump | the take-off speed 1.06 × JumpPower in one step; the push repeats while the root part's own velocity is short of it (a spinning assembly: until the floor rays lose the floor) and never starts under a ceiling (rays 1.5 × root height + Head height up from the root) | Studio; probes T4, T19; tools/fidelity movement-Place B jump_run |
| Mass | parts weigh their physics geometry (the Head its mesh hull); the assembly's centre and inertia are those of the joints' own pose, whatever the animation does | Studio; measured (AssemblyCenterOfMass constant under animation) |
| AssemblyLinearVelocity | the centre of mass's velocity | measured (53.0114 against the root part's 52.9998) |
| Contacts | touching points only (no speculative gap); up to 0.05 studs of penetration stays, 0.7 of the excess leaves per solve; friction and elasticity are the two parts' weighted means; a contact bounces at e × its closing speed when that was 1 stud/s or more and it carried no impulse before | Studio; probes T8, T10, T19. Known differences below |
| Seats | a touch seats 3 frames later (the server's SeatWeld on its way back); only the seat just left is refused | probes T12, T13 |
| Scripts | a script disabled while it runs ends (connections, waiting threads) | probes T8 on (Animate off) |
| Frame order | PostSimulation and Heartbeat with what they resume, then RenderStepped | checks/humanoid-physics/feel |

Gates (tools/fidelity, `tools/fidelity/movement_gates.md`): movement-r15 95.3, movement-r6 95.5, movement-Place B
91.6, movement-nova 100; the wall places against Studio's three reference runs: R15 85.6 (Studio's own runs 86.6),
R6 77.4 (80.9).

Known differences (open, `checks/humanoid-physics/PROGRESS.md`): a pressed character's bouncing against a wall
(rustudio's rule for a bounce is not Studio's rows: those bounce on every closing contact, and a port of them
follows Studio tick for tick in the joints' own pose but bounces far more than Studio once the animation moves
the head, `ticks/own-contact-solve.patch`); pushing a server-owned character (T11); Studio's contact rules for
bodies that are not characters and their 60 Hz stepping (T14); a pushed block lagging its pusher by a tick of
friction (T10).

