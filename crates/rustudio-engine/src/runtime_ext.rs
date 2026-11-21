//! Engine members beyond the core Instance API, following the public Roblox
//! docs: legacy lowercase aliases, ClickDetector / GUI / Sound / Tool /
//! Mouse events, Sound playback requests, CollectionService tags, raycasts
//! and region queries, Humanoid helpers, Players / Teams / HttpService
//! helpers. Lua-side services (TweenService, Debris, DataStores…) live in
//! library.luau.

use mlua::ObjectLike;
use rbx_dom_weak::types::Tags;

use super::*;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SoundCmd {
    Play,
    Stop,
    Pause,
    Resume,
    /// SoundService:PlayLocalSound (client): a detached copy plays.
    PlayLocal,
}

/// Things scripts asked the engine for this frame; the play session acts on them.
#[derive(Default)]
pub struct Requests {
    pub sounds: Vec<(Ref, SoundCmd)>,
    /// Sound.TimePosition set by a script while it plays: (sound, seconds).
    pub sound_seeks: Vec<(Ref, f64)>,
    /// Humanoid:MoveTo / WalkToPoint: (humanoid, target).
    pub move_to: Vec<(Ref, Vec3)>,
    /// Humanoid.Jump = true / ChangeState(Jumping).
    pub jumps: Vec<Ref>,
    /// Humanoid:EquipTool / UnequipTools: (humanoid, tool or None).
    pub equip: Vec<(Ref, Option<Ref>)>,
    /// Explosions parented into Workspace.
    pub explosions: Vec<Ref>,
    /// Humanoid:ChangeState other than Jumping: (humanoid, state name).
    pub change_state: Vec<(Ref, String)>,
    /// Seat:Sit / VehicleSeat:Sit: (seat, humanoid).
    pub sits: Vec<(Ref, Ref)>,
    /// Humanoid:SetStateEnabled: (humanoid, state name, enabled).
    pub state_enabled: Vec<(Ref, String, bool)>,
}

/// What the play session tells scripts each frame.
#[derive(Default, Clone)]
pub struct Inputs {
    /// The in-game menu is open (GuiService.MenuIsOpen).
    pub menu_open: bool,
    /// Connected gamepad slots, from the least significant bit (Gamepad1).
    pub gamepads: u8,
    /// Point and part under the cursor (client).
    pub mouse_hit: Option<Xform>,
    pub mouse_target: Option<Ref>,
    pub mouse_ray: (Vec3, Vec3),
    pub mouse_xy: [f32; 2],
    pub view_size: [f32; 2],
    /// The Player's Mouse object, once a script asked for it.
    pub mouse: Option<Ref>,
    /// Sound playback position / length by Sound.
    pub sound_pos: HashMap<Ref, f64>,
    pub sound_listener: Option<(u32, Option<Xform>, Option<Ref>)>,
    pub sound_len: HashMap<Ref, f64>,
    pub sound_playing: HashSet<Ref>,
    /// Sounds paused (Pause, or Playing = false) and not stopped since.
    pub sound_paused: HashSet<Ref>,
    /// Where the GUI was last drawn (client): AbsolutePosition / Size / TextBounds.
    pub gui_layout: HashMap<Ref, crate::gui::GuiBox>,
}

/// Deprecated camelCase spellings (still used by old models) → current name.
pub fn canonical(key: &str) -> Option<&'static str> {
    Some(match key {
        "findFirstChild" => "FindFirstChild",
        "FindFirstChildOfClass" => return None,
        "getChildren" | "children" => "GetChildren",
        "GetChildren" => return None,
        "clone" => "Clone",
        "destroy" => "Destroy",
        "remove" => "Remove",
        "isA" => "IsA",
        "isDescendantOf" => "IsDescendantOf",
        "getFullName" => "GetFullName",
        "clearAllChildren" => "ClearAllChildren",
        "getPlayers" => "GetPlayers",
        "playerFromCharacter" | "getPlayerFromCharacter" | "GetPlayerFromCharacter" => {
            "GetPlayerFromCharacter"
        }
        "breakJoints" => "BreakJoints",
        "makeJoints" => "MakeJoints",
        "move" | "moveTo" => "MoveTo",
        "takeDamage" => "TakeDamage",
        "loadAnimation" => "LoadAnimation",
        "addItem" => "AddItem",
        "service" | "getService" => "GetService",
        "findPartOnRay" => "FindPartOnRay",
        "findPartsInRegion3" => "FindPartsInRegion3",
        "getMouse" => "GetMouse",
        "isKeyDown" => "IsKeyDown",
        "resize" => "Resize",
        "getMass" => "GetMass",
        "GetModelCFrame" => return None,
        "lerp" => "Lerp",
        _ => return None,
    })
}

/// Events beyond the core set (event_exists in runtime.rs).
pub fn event_exists(class: &str, name: &str) -> bool {
    let is = |b: &str| doc::is_a(class, b);
    match name {
        "MouseClick" | "RightMouseClick" | "MouseHoverEnter" | "MouseHoverLeave" => {
            class == "ClickDetector"
        }
        "Activated" => is("GuiButton") || class == "Tool",
        "MouseButton1Click" | "MouseButton1Down" | "MouseButton1Up" | "MouseButton2Click"
        | "MouseButton2Down" | "MouseButton2Up" => is("GuiButton"),
        "MouseEnter" | "MouseLeave" | "MouseMoved" | "MouseWheelForward" | "MouseWheelBackward"
        | "SelectionGained" | "SelectionLost" => is("GuiObject"),
        "TouchTap" => is("GuiObject") || class == "UserInputService",
        "InputBegan" | "InputEnded" | "InputChanged" => {
            is("GuiObject") || class == "UserInputService"
        }
        "FocusLost" | "Focused" | "ReturnPressedFromOnScreenKeyboard" => class == "TextBox",
        "Chatted" | "Idled" | "CharacterAppearanceLoaded" => class == "Player",
        "MoveToFinished" | "Seated" | "Climbing" | "GettingUp" | "PlatformStanding" | "Ragdoll"
        | "Strafing" | "Swimming" | "FallingDown" | "AnimationPlayed" | "Touched" => {
            class == "Humanoid" || (name == "Touched" && is("BasePart"))
        }
        "Ended" | "Played" | "Paused" | "Resumed" | "Stopped" | "DidLoop" | "Loaded" => {
            class == "Sound"
                || (matches!(name, "Stopped" | "Ended" | "DidLoop") && class == "AnimationTrack")
                || (name == "Loaded" && class == "DataModel")
        }
        "KeyframeReached" => class == "AnimationTrack",
        "Deactivated" | "Equipped" | "Unequipped" => class == "Tool",
        "Button1Down" | "Button1Up" | "Button2Down" | "Button2Up" | "Move" | "KeyDown"
        | "KeyUp" | "WheelForward" | "WheelBackward" | "Idle" => is("Mouse"),
        "JumpRequest"
        | "TextBoxFocused"
        | "TextBoxFocusReleased"
        | "WindowFocused"
        | "WindowFocusReleased"
        | "LastInputTypeChanged"
        | "GamepadConnected"
        | "GamepadDisconnected"
        | "DeviceRotationChanged" => class == "UserInputService",
        "PreferredInputChanged"
        | "PointerAction"
        | "TouchStarted"
        | "TouchMoved"
        | "TouchEnded"
        | "TouchTapInWorld"
        | "TouchPinch"
        | "TouchSwipe"
        | "TouchLongPress"
        | "TouchPan"
        | "TouchRotate" => class == "UserInputService",
        "MenuOpened" | "MenuClosed" => class == "GuiService",
        "UserCFrameChanged" | "UserCFrameEnabled" | "NavigationRequested" => class == "VRService",
        "StateEnabledChanged" => class == "Humanoid",
        "Hit" => class == "Explosion",
        "Completed" => is("TweenBase"),
        "PlayerAdded" | "PlayerRemoved" => class == "Team",
        "Triggered"
        | "TriggerEnded"
        | "PromptShown"
        | "PromptHidden"
        | "PromptButtonHoldBegan"
        | "PromptButtonHoldEnded" => class == "ProximityPrompt",
        "Error" => class == "ScriptContext",
        "Close" => class == "DataModel",
        "LightingChanged" => class == "Lighting",
        "PromptProductPurchaseFinished"
        | "PromptGamePassPurchaseFinished"
        | "PromptPurchaseFinished" => class == "MarketplaceService",
        "OnTeleport" => class == "Player",
        _ => name.starts_with("TagAdded:") || name.starts_with("TagRemoved:"),
    }
}

/// (method, class that has it), implemented in `call` below.
pub const METHODS: &[(&str, &str)] = &[
    ("Spherecast", "WorldRoot"),
    ("Blockcast", "WorldRoot"),
    ("Shapecast", "WorldRoot"),
    ("GetRenderCFrame", "BasePart"),
    ("GetClosestPointOnSurface", "BasePart"),
    ("GetJoints", "BasePart"),
    ("GetNoCollisionConstraints", "BasePart"),
    ("ApplyImpulseAtPosition", "BasePart"),
    ("GetSunDirection", "Lighting"),
    ("GetMoonDirection", "Lighting"),
    ("GetMoonPhase", "Lighting"),
    ("players", "Players"),
    ("GetTestArgs", "StudioTestService"),
    ("BreakJoints", "Model"),
    ("BreakJoints", "BasePart"),
    ("MakeJoints", "Model"),
    ("MakeJoints", "BasePart"),
    ("GetModelCFrame", "Model"),
    ("GetModelSize", "Model"),
    ("GetExtentsSize", "Model"),
    ("GetBoundingBox", "Model"),
    ("TranslateBy", "Model"),
    ("GetScale", "Model"),
    ("GetTouchingParts", "BasePart"),
    ("GetConnectedParts", "BasePart"),
    ("GetRootPart", "BasePart"),
    ("SetNetworkOwner", "BasePart"),
    ("SetNetworkOwnershipAuto", "BasePart"),
    ("GetNetworkOwner", "BasePart"),
    ("CanSetNetworkOwnership", "BasePart"),
    ("Resize", "BasePart"),
    ("ApplyAngularImpulse", "BasePart"),
    ("GetVelocityAtPosition", "BasePart"),
    ("CanCollideWith", "BasePart"),
    ("Raycast", "WorldRoot"),
    ("FindPartOnRay", "WorldRoot"),
    ("FindPartOnRayWithIgnoreList", "WorldRoot"),
    ("FindPartOnRayWithWhitelist", "WorldRoot"),
    ("FindPartsInRegion3", "WorldRoot"),
    ("FindPartsInRegion3WithIgnoreList", "WorldRoot"),
    ("FindPartsInRegion3WithWhiteList", "WorldRoot"),
    ("GetPartBoundsInBox", "WorldRoot"),
    ("GetPartBoundsInRadius", "WorldRoot"),
    ("GetPartsInPart", "WorldRoot"),
    ("GetServerTimeNow", "Workspace"),
    ("GetRealPhysicsFPS", "Workspace"),
    ("MoveTo", "Humanoid"),
    ("EquipTool", "Humanoid"),
    ("UnequipTools", "Humanoid"),
    ("ChangeState", "Humanoid"),
    ("GetAccessories", "Humanoid"),
    ("AddAccessory", "Humanoid"),
    ("RemoveAccessories", "Humanoid"),
    ("ApplyDescription", "Humanoid"),
    ("BuildRigFromAttachments", "Humanoid"),
    ("SetStateEnabled", "Humanoid"),
    ("GetStateEnabled", "Humanoid"),
    ("Sit", "Seat"),
    ("Sit", "VehicleSeat"),
    ("Play", "Sound"),
    ("Stop", "Sound"),
    ("Pause", "Sound"),
    ("Resume", "Sound"),
    ("PlayLocalSound", "SoundService"),
    ("SetListener", "SoundService"),
    ("GetListener", "SoundService"),
    ("Activate", "Tool"),
    ("Deactivate", "Tool"),
    ("AddTag", "Instance"),
    ("RemoveTag", "Instance"),
    ("HasTag", "Instance"),
    ("GetTags", "Instance"),
    ("AddTag", "CollectionService"),
    ("RemoveTag", "CollectionService"),
    ("HasTag", "CollectionService"),
    ("GetTags", "CollectionService"),
    ("GetTagged", "CollectionService"),
    ("GetAllTags", "CollectionService"),
    ("GetInstanceAddedSignal", "CollectionService"),
    ("GetInstanceRemovedSignal", "CollectionService"),
    ("GetTeams", "Teams"),
    ("GetPlayers", "Team"),
    ("GetPlayerByUserId", "Players"),
    ("GetMouse", "Player"),
    ("HasAppearanceLoaded", "Player"),
    ("DistanceFromCharacter", "Player"),
    ("GetNetworkPing", "Player"),
    ("ClearCharacterAppearance", "Player"),
    ("JSONEncode", "HttpService"),
    ("JSONDecode", "HttpService"),
    ("GenerateGUID", "HttpService"),
    ("UrlEncode", "HttpService"),
    ("GetMinutesAfterMidnight", "Lighting"),
    ("SetMinutesAfterMidnight", "Lighting"),
    ("GetMouseLocation", "UserInputService"),
    ("IsMouseButtonPressed", "UserInputService"),
    ("GetKeysPressed", "UserInputService"),
    ("IsLoaded", "DataModel"),
    ("BindToClose", "DataModel"),
    ("SetCoreGuiEnabled", "StarterGui"),
    ("GetCoreGuiEnabled", "StarterGui"),
    ("SetCore", "StarterGui"),
    ("GetCore", "StarterGui"),
    ("PreloadAsync", "ContentProvider"),
    ("RegisterKeyframeSequence", "KeyframeSequenceProvider"),
    ("RegisterActiveKeyframeSequence", "KeyframeSequenceProvider"),
    ("GetKeyframeSequenceAsync", "KeyframeSequenceProvider"),
    ("LoadAsset", "InsertService"),
    ("CreateMeshPartAsync", "AssetService"),
    ("CreateMeshPartAsync", "InsertService"),
    ("LoadAssetVersion", "InsertService"),
    ("RemoveDefaultLoadingScreen", "ReplicatedFirst"),
    ("GetHumanoidDescriptionFromUserId", "Players"),
    ("GetHumanoidDescriptionFromOutfitId", "Players"),
    ("CreateHumanoidModelFromDescription", "Players"),
    ("CreateHumanoidModelFromUserId", "Players"),
    ("GetLastInputType", "UserInputService"),
    ("GetConnectedGamepads", "UserInputService"),
    ("GetGamepadConnected", "UserInputService"),
    ("FillBlock", "Terrain"),
    ("FillBall", "Terrain"),
    ("FillCylinder", "Terrain"),
    ("FillWedge", "Terrain"),
    ("FillRegion", "Terrain"),
    ("Clear", "Terrain"),
    ("ReplaceMaterial", "Terrain"),
    ("WriteVoxels", "Terrain"),
    ("ReadVoxels", "Terrain"),
    ("WriteVoxelChannels", "Terrain"),
    ("ReadVoxelChannels", "Terrain"),
    ("CopyRegion", "Terrain"),
    ("PasteRegion", "Terrain"),
    ("CellCenterToWorld", "Terrain"),
    ("CellCornerToWorld", "Terrain"),
    ("WorldToCell", "Terrain"),
    ("WorldToCellPreferSolid", "Terrain"),
    ("WorldToCellPreferEmpty", "Terrain"),
    ("SetCell", "Terrain"),
    ("SetMaterialColor", "Terrain"),
    ("GetMaterialColor", "Terrain"),
];

/// Methods written in Lua (library.luau).
pub const LUA_METHODS: &[(&str, &str)] = &[
    ("PreloadAsync", "ContentProvider"),
    ("AdvanceToNextPageAsync", "Pages"),
    ("RegisterCollisionGroup", "WorldRoot"),
    ("UnregisterCollisionGroup", "WorldRoot"),
    ("RenameCollisionGroup", "WorldRoot"),
    ("CollisionGroupSetCollidable", "WorldRoot"),
    ("CollisionGroupsAreCollidable", "WorldRoot"),
    ("IsCollisionGroupRegistered", "WorldRoot"),
    ("GetRegisteredCollisionGroups", "WorldRoot"),
    ("GetMaxCollisionGroups", "WorldRoot"),
    ("GetMaxCollisionGroups", "PhysicsService"),
    ("GetCollisionGroupName", "PhysicsService"),
    ("CollisionGroupContainsPart", "PhysicsService"),
    ("TweenPosition", "GuiObject"),
    ("TweenSize", "GuiObject"),
    ("TweenSizeAndPosition", "GuiObject"),
    ("GetLimb", "Humanoid"),
    ("GetBodyPartR15", "Humanoid"),
    ("GetMoveVelocity", "Humanoid"),
    ("GetActor", "Instance"),
    ("BindToMessage", "Actor"),
    ("BindToMessageParallel", "Actor"),
    ("SendMessage", "Actor"),
    ("GetLastInputType", "UserInputService"),
    ("RegisterKeyframeSequence", "KeyframeSequenceProvider"),
    ("RegisterActiveKeyframeSequence", "KeyframeSequenceProvider"),
    ("Move", "Humanoid"),
    ("Move", "Player"),
    ("UnbindAllActions", "ContextActionService"),
    ("GetBoundActionInfo", "ContextActionService"),
    ("GetAllBoundActionInfo", "ContextActionService"),
    ("GetButton", "ContextActionService"),
    ("Emit", "ParticleEmitter"),
    ("Clear", "ParticleEmitter"),
    ("SetTitle", "ContextActionService"),
    ("SetDescription", "ContextActionService"),
    ("SetImage", "ContextActionService"),
    ("SetPosition", "ContextActionService"),
    ("BindActivate", "ContextActionService"),
    ("UnbindActivate", "ContextActionService"),
    ("GetMouseDelta", "UserInputService"),
    ("GetGamepadState", "UserInputService"),
    ("GetNavigationGamepads", "UserInputService"),
    ("GetSupportedGamepadKeyCodes", "UserInputService"),
    ("IsGamepadButtonDown", "UserInputService"),
    ("GamepadSupports", "UserInputService"),
    ("IsNavigationGamepad", "UserInputService"),
    ("GetStringForKeyCode", "UserInputService"),
    ("GetImageForKeyCode", "UserInputService"),
    ("GetDeviceRotation", "UserInputService"),
    ("GetDeviceGravity", "UserInputService"),
    ("GetUserCFrame", "VRService"),
    ("GetUserCFrameEnabled", "VRService"),
    ("RecenterUserHeadCFrame", "VRService"),
    ("RequestNavigation", "VRService"),
    ("IsTenFootInterface", "GuiService"),
    ("GetGuiInset", "GuiService"),
    ("GetEmotesMenuOpen", "GuiService"),
    ("GetInspectMenuEnabled", "GuiService"),
    ("RegisterTouchCameraMovementMode", "PlayerScripts"),
    ("RegisterComputerCameraMovementMode", "PlayerScripts"),
    ("RegisterTouchMovementMode", "PlayerScripts"),
    ("RegisterComputerMovementMode", "PlayerScripts"),
    ("ClearComputerCameraMovementModes", "PlayerScripts"),
    ("ClearComputerMovementModes", "PlayerScripts"),
    ("ClearTouchCameraMovementModes", "PlayerScripts"),
    ("ClearTouchMovementModes", "PlayerScripts"),
    ("ViewportPointToRay", "Camera"),
    ("ScreenPointToRay", "Camera"),
    ("WorldToViewportPoint", "Camera"),
    ("WorldToScreenPoint", "Camera"),
    ("GetRenderCFrame", "Camera"),
    ("GetPartsObscuringTarget", "Camera"),
    ("GetLargestCutoffDistance", "Camera"),
    ("GetRoll", "Camera"),
    ("SetRoll", "Camera"),
    ("ZoomToExtents", "Camera"),
    ("AddItem", "Debris"),
    ("Create", "TweenService"),
    ("GetValue", "TweenService"),
    ("Play", "Tween"),
    ("Pause", "Tween"),
    ("Cancel", "Tween"),
    ("LoadAnimation", "Humanoid"),
    ("LoadAnimation", "Animator"),
    ("LoadAnimation", "AnimationController"),
    ("GetPlayingAnimationTracks", "Humanoid"),
    ("GetPlayingAnimationTracks", "Animator"),
    ("Play", "AnimationTrack"),
    ("Stop", "AnimationTrack"),
    ("AdjustSpeed", "AnimationTrack"),
    ("AdjustWeight", "AnimationTrack"),
    ("GetMarkerReachedSignal", "AnimationTrack"),
    ("GetTimeOfKeyframe", "AnimationTrack"),
    ("GetDataStore", "DataStoreService"),
    ("GetGlobalDataStore", "DataStoreService"),
    ("GetOrderedDataStore", "DataStoreService"),
    ("RegisterCollisionGroup", "PhysicsService"),
    ("CreateCollisionGroup", "PhysicsService"),
    ("UnregisterCollisionGroup", "PhysicsService"),
    ("RemoveCollisionGroup", "PhysicsService"),
    ("RenameCollisionGroup", "PhysicsService"),
    ("CollisionGroupSetCollidable", "PhysicsService"),
    ("CollisionGroupsAreCollidable", "PhysicsService"),
    ("IsCollisionGroupRegistered", "PhysicsService"),
    ("GetRegisteredCollisionGroups", "PhysicsService"),
    ("GetCollisionGroups", "PhysicsService"),
    ("SetPartCollisionGroup", "PhysicsService"),
    ("GetCollisionGroupId", "PhysicsService"),
    ("BindAction", "ContextActionService"),
    ("BindActionAtPriority", "ContextActionService"),
    ("UnbindAction", "ContextActionService"),
    ("BindToRenderStep", "RunService"),
    ("UnbindFromRenderStep", "RunService"),
    ("BulkMoveTo", "WorldRoot"),
    ("IsUserFeatureEnabled", "UserSettings"),
    ("GetService", "UserSettings"),
    ("FindService", "UserSettings"),
    ("GetService", "GlobalSettings"),
    ("FindService", "GlobalSettings"),
    ("GetSharedTable", "SharedTableRegistry"),
    ("SetSharedTable", "SharedTableRegistry"),
    ("GetProductInfo", "MarketplaceService"),
    ("GetProductInfoAsync", "MarketplaceService"),
    // (LocalizationService / Translator: domains/players_chat.)
    ("FilterStringAsync", "TextService"),
    ("GetNonChatStringForUserAsync", "TextFilterResult"),
    ("GetNonChatStringForBroadcastAsync", "TextFilterResult"),
    ("GetChatForUserAsync", "TextFilterResult"),
];

fn none() -> mlua::Result<MultiValue> {
    Ok(MultiValue::new())
}

fn one(v: Value) -> mlua::Result<MultiValue> {
    Ok(MultiValue::from_iter([v]))
}

// ------------------------------------------------------------ terrain edits

fn num_arg(args: &MultiValue, i: usize) -> Option<f32> {
    match arg(args, i) {
        Value::Number(n) => Some(n as f32),
        Value::Integer(n) => Some(n as f32),
        _ => None,
    }
}

/// A terrain material argument: its SmoothGrid slot (Air 0, Water 1, …).
fn terrain_material(args: &MultiValue, i: usize, call: &str) -> mlua::Result<u8> {
    let e = opt_ud::<LEnumItem>(&arg(args, i))
        .ok_or_else(|| rt_err(&format!("Terrain:{call}: Enum.Material expected")))?;
    rustudio_core::terrain::material_index(&e.name)
        .ok_or_else(|| rt_err(&format!("Terrain:{call}: Enum.Material.{} is not a terrain material", e.name)))
}

/// FillBlock's distance to a box of half size `h` (local coordinates): the
/// largest per-axis distance, inside and out (so a voxel's share is the
/// smallest per-axis share). Studio fills a 2-stud cube's four voxels at
/// 0.254 each, which a Euclidean distance (0.146) doesn't give (round 21,
/// ReadVoxels in an offline Studio).
fn box_distance(p: Vec3, h: Vec3) -> f32 {
    (p.abs() - h).max_element()
}

/// Terrain editing (Roblox's Terrain:Fill…, Clear, ReplaceMaterial and the
/// material colours), round 19: Place C builds its planet surfaces with
/// FillBlock, Place B its water. Edits go to a decoded copy of the voxels
/// that `flush_terrain` writes back to SmoothGrid once a step.
fn terrain_call(lua: &Lua, name: &str, this: Ref, args: &MultiValue) -> mlua::Result<MultiValue> {
    use rustudio_core::terrain::{self, Voxels};
    if name == "SetMaterialColor" || name == "GetMaterialColor" {
        let slot = terrain_material(args, 1, name)? as usize;
        let mut w = world(lua)?;
        let mut blob: Vec<u8> = match w.d().prop(this, "MaterialColors") {
            Some(Variant::BinaryString(b)) if AsRef::<[u8]>::as_ref(&b).len() >= 69 => {
                AsRef::<[u8]>::as_ref(&b).to_vec()
            }
            _ => (0..23u8).flat_map(terrain::material_color).collect(),
        };
        if name == "GetMaterialColor" {
            let c = &blob[slot * 3..slot * 3 + 3];
            drop(w);
            let v = LColor3([c[0], c[1], c[2]].map(|x| x as f32 / 255.0));
            return one(Value::UserData(lua.create_userdata(v)?));
        }
        let c = opt_ud::<LColor3>(&arg(args, 2))
            .ok_or_else(|| rt_err("Terrain:SetMaterialColor: Color3 expected"))?;
        blob[slot * 3..slot * 3 + 3].copy_from_slice(&c.0.map(|x| (x.clamp(0.0, 1.0) * 255.0).round() as u8));
        if let Some(i) = w.dm().dom.get_by_ref_mut(this) {
            i.properties.insert(
                ustr("MaterialColors"),
                Variant::BinaryString(blob.into()),
            );
        }
        w.dm().mark_prop(this, ustr("MaterialColors"));
        w.record(RepOp::Prop(this, "MaterialColors".into()));
        return none();
    }
    // Cell ↔ world (round 25): cells are 4-stud voxels from the origin.
    match name {
        "CellCenterToWorld" | "CellCornerToWorld" => {
            let c = Vec3::new(
                num_arg(args, 1).unwrap_or(0.0).floor(),
                num_arg(args, 2).unwrap_or(0.0).floor(),
                num_arg(args, 3).unwrap_or(0.0).floor(),
            );
            let half = if name == "CellCenterToWorld" { 0.5 } else { 0.0 };
            return one(vec3_value(lua, (c + half) * terrain::VOXEL)?);
        }
        "WorldToCell" | "WorldToCellPreferSolid" | "WorldToCellPreferEmpty" => {
            let p = vec3_arg(args, 1, &format!("Terrain:{name}"))?;
            return one(vec3_value(lua, (p / terrain::VOXEL).floor())?);
        }
        _ => {}
    }
    // The voxels being edited: this frame's copy, else the saved grid.
    let mut voxels = {
        let mut w = world(lua)?;
        match w.terrain_edit.take() {
            Some((r, v)) if r == this => v,
            _ => match Voxels::from_dom(&w.d().dom) {
                Some(Ok(v)) => v,
                _ => Voxels::empty(),
            },
        }
    };
    let corners = |xf: &Xform, h: Vec3| {
        let e = xf.rot.x_axis.abs() * h.x + xf.rot.y_axis.abs() * h.y + xf.rot.z_axis.abs() * h.z;
        ((xf.pos - e).to_array(), (xf.pos + e).to_array())
    };
    let cframe = |i: usize| {
        opt_ud::<LCFrame>(&arg(args, i))
            .map(|c| c.0)
            .ok_or_else(|| rt_err(&format!("Terrain:{name}: CFrame expected")))
    };
    let region = |i: usize| {
        opt_ud::<crate::ltypes::LRegion3>(&arg(args, i))
            .ok_or_else(|| rt_err(&format!("Terrain:{name}: Region3 expected")))
    };
    match name {
        "FillBlock" | "FillWedge" => {
            let xf = cframe(1)?;
            let h = vec3_arg(args, 2, name)?.abs() * 0.5;
            let mat = terrain_material(args, 3, name)?;
            let (lo, hi) = corners(&xf, h);
            let wedge = name == "FillWedge";
            voxels.fill(lo, hi, mat, |c| {
                let p = xf.rot.transpose() * (Vec3::from(c) - xf.pos);
                let d = box_distance(p, h);
                if wedge {
                    // A WedgePart: full height at the back (+Z), none at the front.
                    let n = Vec3::new(0.0, h.z, -h.y).normalize_or_zero();
                    d.max(n.dot(p))
                } else {
                    d
                }
            });
        }
        "FillBall" => {
            let c0 = vec3_arg(args, 1, name)?;
            let r = num_arg(args, 2).unwrap_or(0.0).abs();
            let mat = terrain_material(args, 3, name)?;
            voxels.fill((c0 - r).to_array(), (c0 + r).to_array(), mat, |c| {
                (Vec3::from(c) - c0).length() - r
            });
        }
        "FillCylinder" => {
            let xf = cframe(1)?;
            let height = num_arg(args, 2).unwrap_or(0.0).abs();
            let r = num_arg(args, 3).unwrap_or(0.0).abs();
            let mat = terrain_material(args, 4, name)?;
            let (lo, hi) = corners(&xf, Vec3::new(r, height * 0.5, r));
            voxels.fill(lo, hi, mat, |c| {
                let p = xf.rot.transpose() * (Vec3::from(c) - xf.pos);
                let q = glam::Vec2::new(glam::Vec2::new(p.x, p.z).length() - r, p.y.abs() - height * 0.5);
                q.max(glam::Vec2::ZERO).length() + q.max_element().min(0.0)
            });
        }
        "FillRegion" => {
            let rg = region(1)?;
            let mat = terrain_material(args, 3, name)?;
            let (lo, hi) = (rg.min, rg.max);
            // Whole voxels whose centres are inside the region.
            voxels.fill(lo.to_array(), hi.to_array(), mat, |c| {
                let c = Vec3::from(c);
                if c.cmpge(lo).all() && c.cmple(hi).all() {
                    -terrain::VOXEL
                } else {
                    terrain::VOXEL
                }
            });
        }
        "ReplaceMaterial" => {
            let rg = region(1)?;
            let from = terrain_material(args, 3, name)?;
            let to = terrain_material(args, 4, name)?;
            let a = (rg.min / terrain::VOXEL).round().as_ivec3().to_array();
            let b = (rg.max / terrain::VOXEL).round().as_ivec3().to_array();
            voxels.replace(a, b, from, to);
        }
        "Clear" => voxels.clear(),
        // CopyRegion(Region3int16 of cells, Max inclusive) → a TerrainRegion
        // (ExtentsMin / ExtentsMax, saved only, its cells in SmoothGrid at
        // their world cells); PasteRegion(region, corner cell,
        // pasteEmptyCells). Round 25; Max and the saved form measured in
        // round 26 (region_probe.luau: (10, 1, 20) .. (13, 2, 23) holds 4 × 2
        // × 4 cells, and saves those extents and world cells).
        "CopyRegion" => {
            let rg = opt_ud::<crate::ltypes::LRegion3int16>(&arg(args, 1))
                .ok_or_else(|| rt_err("Terrain:CopyRegion: Region3int16 expected"))?;
            let (lo, hi) = (rg.min.0.map(i32::from), rg.max.0.map(i32::from));
            let size = [0, 1, 2].map(|k| (hi[k] - lo[k] + 1).max(0));
            let region = rustudio_core::terrain_region::Region::copy(&voxels, lo, size);
            world(lua)?.terrain_edit = Some((this, voxels));
            let r = create_any(lua, "TerrainRegion")?;
            let v3 = |c: [i32; 3]| Variant::Vector3int16(rbx_dom_weak::types::Vector3int16::new(c[0] as i16, c[1] as i16, c[2] as i16));
            if let Some(i) = world(lua)?.dm().dom.get_by_ref_mut(r) {
                i.properties.insert(ustr("ExtentsMin"), v3(lo));
                i.properties.insert(ustr("ExtentsMax"), v3([0, 1, 2].map(|k| lo[k] + size[k] - 1)));
                i.properties.insert(ustr("SmoothGrid"), Variant::BinaryString(region.to_smooth_grid(lo).into()));
            }
            return one(inst_value(lua, r)?);
        }
        "PasteRegion" => {
            let src = opt_inst(&arg(args, 1))
                .filter(|r| inst_meta(lua, *r).is_ok_and(|m| m.0 == "TerrainRegion"))
                .ok_or_else(|| rt_err("Terrain:PasteRegion: region has to be a TerrainRegion"))?;
            let corner = opt_ud::<crate::ltypes::LVector3int16>(&arg(args, 2))
                .ok_or_else(|| rt_err("Terrain:PasteRegion: Vector3int16 expected"))?
                .0
                .map(i32::from);
            let paste_empty = matches!(arg(args, 3), Value::Boolean(true));
            let region = {
                let w = world(lua)?;
                let d = w.d();
                let ext = |k: &str| match d.prop(src, k) {
                    Some(Variant::Vector3int16(v)) => [v.x as i32, v.y as i32, v.z as i32],
                    _ => [0; 3],
                };
                let (min, max) = (ext("ExtentsMin"), ext("ExtentsMax"));
                let grid = match d.prop(src, "SmoothGrid") {
                    Some(Variant::BinaryString(b)) => AsRef::<[u8]>::as_ref(&b).to_vec(),
                    _ => Vec::new(),
                };
                rustudio_core::terrain_region::Region::from_smooth_grid(&grid, min, max)
            };
            match region {
                Ok(region) => region.paste(&mut voxels, corner, paste_empty),
                Err(e) => {
                    world(lua)?.terrain_edit = Some((this, voxels));
                    return Err(rt_err(format!("Terrain:PasteRegion: {e}")));
                }
            }
        }
        // ReadVoxelChannels(region, 4, {names}) → {Name = [x][y][z], Size}
        // and WriteVoxelChannels(region, 4, {Name = [x][y][z]}): SolidMaterial,
        // SolidOccupancy and LiquidOccupancy, the grid's two layers (a Water
        // voxel is Air with liquid; a part-full solid may hold shoreline water).
        "ReadVoxelChannels" | "WriteVoxelChannels" => {
            let rg = region(1)?;
            let res = num_arg(args, 2).unwrap_or(4.0);
            if (res - terrain::VOXEL).abs() > 1e-6 {
                return Err(rt_err("Terrain:".to_string() + name + ": resolution must be 4"));
            }
            let a = (rg.min / terrain::VOXEL).round().as_ivec3();
            let n = ((rg.max / terrain::VOXEL).round().as_ivec3() - a).max(glam::IVec3::ZERO);
            const CHANNELS: [&str; 3] = ["SolidMaterial", "SolidOccupancy", "LiquidOccupancy"];
            // (solid material, solid occupancy, liquid occupancy) of a cell.
            let read = |v: glam::IVec3| -> (u8, f32, f32) {
                let (m, o) = voxels.get(v.x, v.y, v.z);
                if m == terrain::WATER {
                    // LiquidOccupancy reads (byte + 1) / 255 where ReadVoxels
                    // reads (byte + 1) / 256 (round 26, region_probe.luau:
                    // WriteVoxels Water 0.1 reads 0.101562 and 0.101961;
                    // Place B's lake one step over rustudio's above half).
                    (0, 0.0, (o * 256.0 / 255.0).min(1.0))
                } else {
                    (m, o, voxels.water(v.x, v.y, v.z))
                }
            };
            if name == "ReadVoxelChannels" {
                let wanted: Vec<String> = match arg(args, 3) {
                    Value::Table(t) => t.sequence_values::<String>().flatten().collect(),
                    _ => return Err(rt_err("Terrain:ReadVoxelChannels: channel names expected")),
                };
                let out = lua.create_table()?;
                for ch in CHANNELS.iter().filter(|c| wanted.iter().any(|w| w == *c)) {
                    let t = lua.create_table()?;
                    for x in 0..n.x {
                        let tx = lua.create_table()?;
                        for y in 0..n.y {
                            let ty = lua.create_table()?;
                            for z in 0..n.z {
                                let (m, so, lo) = read(a + glam::IVec3::new(x, y, z));
                                let item = match *ch {
                                    "SolidMaterial" => LEnumItem::from_name("Material", terrain::material_name(m))
                                        .map(|e| crate::ltypes::enum_value(lua, e))
                                        .transpose()?
                                        .unwrap_or(Value::Nil),
                                    "SolidOccupancy" => Value::Number(so as f64),
                                    _ => Value::Number(lo as f64),
                                };
                                ty.raw_set(z + 1, item)?;
                            }
                            tx.raw_set(y + 1, ty)?;
                        }
                        t.raw_set(x + 1, tx)?;
                    }
                    out.raw_set(*ch, t)?;
                }
                out.raw_set("Size", vec3_value(lua, n.as_vec3())?)?;
                world(lua)?.terrain_edit = Some((this, voxels));
                return one(Value::Table(out));
            }
            let Value::Table(chans) = arg(args, 3) else {
                return Err(rt_err("Terrain:WriteVoxelChannels: channel table expected"));
            };
            let given: Vec<(usize, Table)> = CHANNELS
                .iter()
                .enumerate()
                .filter_map(|(i, c)| chans.get::<Option<Table>>(*c).ok().flatten().map(|t| (i, t)))
                .collect();
            let mut out = Vec::new();
            for x in 0..n.x {
                for y in 0..n.y {
                    for z in 0..n.z {
                        let v = a + glam::IVec3::new(x, y, z);
                        let (mut m, mut so, mut lq) = read(v);
                        for (i, t) in &given {
                            let cell: Value = t.get::<Option<Table>>(x + 1)?
                                .and_then(|tx| tx.get::<Option<Table>>(y + 1).ok().flatten())
                                .map(|ty| ty.get::<Value>(z + 1))
                                .transpose()?
                                .unwrap_or(Value::Nil);
                            match i {
                                0 => {
                                    m = opt_ud::<LEnumItem>(&cell)
                                        .and_then(|e| terrain::material_index(&e.name))
                                        .ok_or_else(|| rt_err("Terrain:WriteVoxelChannels: Enum.Material expected"))?;
                                    if m == terrain::WATER {
                                        m = 0;
                                    }
                                }
                                1 => so = num(&cell).unwrap_or(0.0).clamp(0.0, 1.0) as f32,
                                _ => lq = num(&cell).unwrap_or(0.0).clamp(0.0, 1.0) as f32,
                            }
                        }
                        let solid = terrain::occupancy_to_byte(so).filter(|_| m != 0);
                        let water = (lq > 0.0).then(|| ((lq * 256.0).round() as u16).clamp(1, 256));
                        let cell = match (solid, water) {
                            (Some(b), w) => (m, b, if b == 255 { 0 } else { w.unwrap_or(0) }),
                            (None, Some(_)) => match terrain::occupancy_to_byte(lq) {
                                Some(b) => (terrain::WATER, b, 0),
                                None => (0, 0, 0),
                            },
                            (None, None) => (0, 0, 0),
                        };
                        out.push((v.to_array(), cell));
                    }
                }
            }
            voxels.set_raw_many(out);
        }
        // WriteVoxels(region, 4, materials[x][y][z], occupancies[x][y][z]) and
        // ReadVoxels(region, 4) → (materials, occupancies), each with a Size:
        // the voxels whose cells lie in the (4-stud aligned) region.
        "WriteVoxels" | "ReadVoxels" => {
            let rg = region(1)?;
            let res = num_arg(args, 2).unwrap_or(4.0);
            if (res - terrain::VOXEL).abs() > 1e-6 {
                return Err(rt_err("Terrain:".to_string() + name + ": resolution must be 4"));
            }
            let a = (rg.min / terrain::VOXEL).round().as_ivec3();
            let b = (rg.max / terrain::VOXEL).round().as_ivec3();
            let n = (b - a).max(glam::IVec3::ZERO);
            if name == "ReadVoxels" {
                let mats = lua.create_table()?;
                let occs = lua.create_table()?;
                for x in 0..n.x {
                    let (mx, ox) = (lua.create_table()?, lua.create_table()?);
                    for y in 0..n.y {
                        let (my, oy) = (lua.create_table()?, lua.create_table()?);
                        for z in 0..n.z {
                            let v = a + glam::IVec3::new(x, y, z);
                            let (m, o) = voxels.get(v.x, v.y, v.z);
                            let w = voxels.water(v.x, v.y, v.z);
                            let (m, o) = if m == 0 && w > 0.0 { (terrain::WATER, w) } else { (m, o) };
                            let item = LEnumItem::from_name("Material", terrain::material_name(m))
                                .map(|e| crate::ltypes::enum_value(lua, e))
                                .transpose()?
                                .unwrap_or(Value::Nil);
                            my.raw_set(z + 1, item)?;
                            oy.raw_set(z + 1, o as f64)?;
                        }
                        mx.raw_set(y + 1, my)?;
                        ox.raw_set(y + 1, oy)?;
                    }
                    mats.raw_set(x + 1, mx)?;
                    occs.raw_set(x + 1, ox)?;
                }
                let size = LVector3(n.as_vec3()).into_value();
                mats.raw_set("Size", size.clone())?;
                occs.raw_set("Size", size)?;
                world(lua)?.terrain_edit = Some((this, voxels));
                return Ok(MultiValue::from_iter([Value::Table(mats), Value::Table(occs)]));
            }
            let (Value::Table(mats), Value::Table(occs)) = (arg(args, 3), arg(args, 4)) else {
                return Err(rt_err("Terrain:WriteVoxels: material and occupancy arrays expected"));
            };
            let mut out = Vec::new();
            for x in 0..n.x {
                let (mx, ox): (Table, Table) = (mats.get(x + 1)?, occs.get(x + 1)?);
                for y in 0..n.y {
                    let (my, oy): (Table, Table) = (mx.get(y + 1)?, ox.get(y + 1)?);
                    for z in 0..n.z {
                        let m = opt_ud::<LEnumItem>(&my.get::<Value>(z + 1)?)
                            .and_then(|e| terrain::material_index(&e.name))
                            .ok_or_else(|| rt_err("Terrain:WriteVoxels: Enum.Material expected"))?;
                        let o: f64 = oy.get::<Option<f64>>(z + 1)?.unwrap_or(0.0);
                        let v = a + glam::IVec3::new(x, y, z);
                        // WriteVoxels truncates to the byte (FillBlock rounds):
                        // 0.1 / 0.25 / 0.3 / 0.5 / 0.7 / 0.9 read back as
                        // 26 / 64 / 77 / 128 / 179 / 230 ÷ 256 in Studio (round
                        // 26, region_probe.luau), bytes floor(share × 255).
                        let o = (o.clamp(0.0, 1.0) * 255.0).floor() / 255.0;
                        out.push((v.to_array(), m, o as f32));
                    }
                }
            }
            voxels.set_many(out);
        }
        _ => {}
    }
    world(lua)?.terrain_edit = Some((this, voxels));
    none()
}

/// Writes scripts' terrain edits back to Terrain.SmoothGrid (and
/// replicates it), so the drawn terrain and the physics follow.
pub fn flush_terrain(lua: &Lua) {
    let Some(mut w) = lua.app_data_mut::<World>() else {
        return;
    };
    let Some((r, v)) = w.terrain_edit.take() else {
        return;
    };
    if w.d().dom.get_by_ref(r).is_none() {
        return;
    }
    let blob = v.encode();
    if let Some(i) = w.dm().dom.get_by_ref_mut(r) {
        i.properties
            .insert(ustr("SmoothGrid"), Variant::BinaryString(blob.into()));
    }
    w.dm().mark_prop(r, ustr("SmoothGrid"));
    w.record(RepOp::Prop(r, "SmoothGrid".into()));
}

fn vec3_arg(args: &MultiValue, i: usize, what: &str) -> mlua::Result<Vec3> {
    opt_ud::<LVector3>(&arg(args, i))
        .map(|v| v.0)
        .ok_or_else(|| rt_err(format!("{what}: Vector3 expected")))
}

fn ud_field<T: Clone + 'static>(v: &Value, key: &str) -> Option<T> {
    match v {
        Value::UserData(ud) => ud.get::<Value>(key).ok().and_then(|x| opt_ud::<T>(&x)),
        Value::Table(t) => t.get::<Value>(key).ok().and_then(|x| opt_ud::<T>(&x)),
        _ => None,
    }
}

fn ud_value(v: &Value, key: &str) -> Value {
    match v {
        Value::UserData(ud) => ud.get::<Value>(key).unwrap_or(Value::Nil),
        Value::Table(t) => t.get::<Value>(key).unwrap_or(Value::Nil),
        _ => Value::Nil,
    }
}

fn refs_in(v: &Value) -> Vec<Ref> {
    match v {
        Value::Table(t) => t
            .sequence_values::<Value>()
            .flatten()
            .filter_map(|x| opt_inst(&x))
            .collect(),
        other => opt_inst(other).into_iter().collect(),
    }
}

fn vec3_value(lua: &Lua, v: Vec3) -> mlua::Result<Value> {
    Ok(LVector3(v).into_value())
}

fn cframe_value(lua: &Lua, xf: Xform) -> mlua::Result<Value> {
    Ok(LCFrame(xf).into_lua(lua)?)
}

// ------------------------------------------------------------- world queries

/// A Workspace part for queries: (ref, CFrame, size, CanCollide, CanQuery).
#[derive(Clone, Copy)]
struct QPart {
    r: Ref,
    xf: Xform,
    size: Vec3,
    collide: bool,
    query: bool,
    /// A MeshPart whose CollisionFidelity isn't Box: rays test its mesh.
    mesh: bool,
    /// query_shapes::BOX / BALL / CYLINDER / WEDGE / CORNER_WEDGE: rays,
    /// sweeps and GetPartsInPart test the true shape.
    shape: u8,
    /// Studio's stored collision pieces (physics_mesh registry id, 0 none):
    /// rays and sweeps test these instead of the mesh or the box.
    cvx: u32,
    /// InitialSize, the space the pieces are stored in.
    initial: Vec3,
}

/// CollectionService's tag → instances, kept up to date from the change
/// journal: a Tree mark re-reads only that subtree, a Tags change only that
/// instance (round 27 rebuilt the whole index on any Tree mark, so with
/// enemies and effects spawning every frame each GetTagged walked the whole
/// 65k-instance DataModel: ~2.5 ms a call on a live place).
#[derive(Default)]
struct TagCache {
    doc_id: u64,
    valid: bool,
    /// tag → (instances in tag-add order, membership). Removals only drop
    /// from the set; the order list is compacted when read.
    by_tag: HashMap<String, (Vec<Ref>, rustc_hash::FxHashSet<Ref>, bool)>,
    /// instance → its tags as indexed.
    of: rustc_hash::FxHashMap<Ref, Vec<String>>,
}

fn tags_in(v: Option<&Variant>) -> Vec<String> {
    match v {
        Some(Variant::Tags(t)) => t.iter().map(|s| s.to_string()).collect(),
        Some(Variant::BinaryString(b)) => Tags::decode(b.as_ref())
            .map(|t| t.iter().map(|s| s.to_string()).collect())
            .unwrap_or_default(),
        _ => Vec::new(),
    }
}

impl TagCache {
    fn set(&mut self, r: Ref, tags: Vec<String>) {
        let old = self.of.remove(&r).unwrap_or_default();
        if old == tags {
            if !tags.is_empty() {
                self.of.insert(r, tags);
            }
            return;
        }
        for t in &old {
            if !tags.contains(t) {
                if let Some(e) = self.by_tag.get_mut(t) {
                    if e.1.remove(&r) {
                        e.2 = true;
                    }
                }
            }
        }
        for t in &tags {
            let e = self.by_tag.entry(t.clone()).or_default();
            if e.1.insert(r) {
                e.0.push(r);
            }
        }
        if !tags.is_empty() {
            self.of.insert(r, tags);
        }
    }
}

pub(super) fn tagged(lua: &Lua, tag: &str) -> mlua::Result<Vec<Ref>> {
    if lua.app_data_ref::<TagCache>().is_none() {
        lua.set_app_data(TagCache::default());
    }
    let mut w = world(lua)?;
    let d = w.dm();
    let marks = d.marks_for(doc::Reader::Tags);
    let mut c = lua
        .app_data_mut::<TagCache>()
        .ok_or_else(|| rt_err("tag cache missing"))?;
    let key = ustr("Tags");
    let marks = if c.doc_id != d.id || !c.valid { doc::Marks::All } else { marks };
    match marks {
        doc::Marks::All => {
            c.by_tag.clear();
            c.of.clear();
            for i in d.dom.descendants() {
                let t = tags_in(i.properties.get(&key));
                if !t.is_empty() {
                    c.set(i.referent(), t);
                }
            }
            c.doc_id = d.id;
            c.valid = true;
        }
        doc::Marks::Some(m) => {
            for mk in m {
                match mk {
                    doc::Mark::Tree(r) => {
                        for x in d.subtree(r) {
                            let t = d
                                .dom
                                .get_by_ref(x)
                                .map(|i| tags_in(i.properties.get(&key)))
                                .unwrap_or_default();
                            if !t.is_empty() || c.of.contains_key(&x) {
                                c.set(x, t);
                            }
                        }
                    }
                    doc::Mark::Prop(r, n) if n.as_str() == "Tags" => {
                        let t = d
                            .dom
                            .get_by_ref(r)
                            .map(|i| tags_in(i.properties.get(&key)))
                            .unwrap_or_default();
                        c.set(r, t);
                    }
                    _ => {}
                }
            }
        }
    }
    let c = &mut *c;
    Ok(match c.by_tag.get_mut(tag) {
        Some(e) => {
            // Also drop instances freed before their subtree was re-read.
            let (list, set, _) = e;
            let of = &mut c.of;
            list.retain(|r| {
                let keep = set.contains(r) && d.dom.get_by_ref(*r).is_some();
                if !keep && set.remove(r) {
                    of.remove(r);
                }
                keep
            });
            e.2 = false;
            e.0.clone()
        }
        None => Vec::new(),
    })
}

/// Workspace parts for queries, kept up to date from the document's change
/// journal (only changed parts are re-read).
#[derive(Default)]
struct QueryCache {
    doc_id: u64,
    parts: std::rc::Rc<Vec<QPart>>,
    // (Fx hashing: Ref keys are hashed thousands of times a frame here.)
    index: rustc_hash::FxHashMap<Ref, usize>,
    grid: QueryGrid,
    /// Workspace.Terrain's voxels for rays (None = read them again).
    /// With its voxel bounds (studs), which every ray needs (round 20: the
    /// scan over every chunk per ray was 12% of Place B's Play frame).
    terrain: Option<Option<(Ref, std::sync::Arc<rustudio_core::terrain::Voxels>, Option<([f32; 3], [f32; 3])>)>>,
    /// The material of the last ray that hit terrain (for its RaycastResult).
    terrain_material: Option<u8>,
    /// Terrain triangles per chunk for rays (dropped with `terrain`).
    terrain_tris: HashMap<[i32; 3], std::rc::Rc<Vec<([Vec3; 3], [u8; 3])>>>,
    /// Water surface triangles per chunk for rays (dropped with `terrain`).
    terrain_water: HashMap<[i32; 3], std::rc::Rc<Vec<[Vec3; 3]>>>,
    /// A cell index over each chunk's `terrain_tris` for rays (dropped with
    /// them): a ray tests the triangles in the cells it crosses, not all
    /// of the chunk's thousands.
    terrain_index: HashMap<[i32; 3], std::rc::Rc<TriIndex>>,
    /// Whether a class is a BasePart (a query part when under Workspace):
    /// property marks of other instances (a rig's Bones, Attachments) are
    /// passed over without reading the instance again.
    part_classes: rustc_hash::FxHashMap<rbx_dom_weak::Ustr, bool>,
    /// The same per instance (an instance's class never changes).
    part_refs: rustc_hash::FxHashMap<Ref, bool>,
}

impl QueryCache {
    /// Whether `r` is a BasePart other than Terrain (what `qpart` can list).
    fn is_part(&mut self, d: &Document, r: Ref) -> bool {
        if let Some(&p) = self.part_refs.get(&r) {
            return p;
        }
        let Some(inst) = d.dom.get_by_ref(r) else { return false };
        let class = inst.class;
        let p = *self
            .part_classes
            .entry(class)
            .or_insert_with(|| class != "Terrain" && doc::is_a(class.as_str(), "BasePart"));
        // (Destroyed instances' entries pile up: dropped now and then.)
        if self.part_refs.len() > 200_000 {
            self.part_refs.clear();
        }
        self.part_refs.insert(r, p);
        p
    }
}

/// A uniform grid over the query parts (by bounding box) so rays and box
/// queries only test parts near them. Parts spanning too many cells sit in
/// `big` and are always tested.
#[derive(Default)]
struct QueryGrid {
    cells: rustc_hash::FxHashMap<[i32; 3], Vec<Ref>>,
    /// Each part's cell range (inclusive).
    of: rustc_hash::FxHashMap<Ref, ([i32; 3], [i32; 3])>,
    big: rustc_hash::FxHashSet<Ref>,
}

impl QueryGrid {
    fn cell_range(lo: Vec3, hi: Vec3) -> ([i32; 3], [i32; 3]) {
        let c = mech::QUERY_GRID_CELL;
        let a = (lo / c).floor();
        let b = (hi / c).floor();
        (
            [a.x as i32, a.y as i32, a.z as i32],
            [b.x as i32, b.y as i32, b.z as i32],
        )
    }

    /// Cells in an inclusive range, in i64: a part far out (cell indices
    /// saturated at i32::MIN / MAX) wrapped the i32 width to a small count,
    /// and `insert` then walked billions of cells, growing `cells` by
    /// ~200 MB/s with the main thread stuck (a live place power-up weapon freeze).
    fn cells_in(a: [i32; 3], b: [i32; 3]) -> i64 {
        (0..3)
            .map(|i| b[i] as i64 - a[i] as i64 + 1)
            .fold(1i64, |n, w| if w <= 0 { 0 } else { n.saturating_mul(w) })
    }

    fn remove(&mut self, r: Ref) {
        self.big.remove(&r);
        if let Some((a, b)) = self.of.remove(&r) {
            for x in a[0]..=b[0] {
                for y in a[1]..=b[1] {
                    for z in a[2]..=b[2] {
                        let k = [x, y, z];
                        if let Some(v) = self.cells.get_mut(&k) {
                            v.retain(|x| *x != r);
                            if v.is_empty() {
                                self.cells.remove(&k);
                            }
                        }
                    }
                }
            }
        }
    }

    /// Removes many parts at once: each cell they were in is filtered once
    /// (removing them one by one filtered a shared cell once per part:
    /// quadratic when a 20k-part model left or arrived; round 2, bulk).
    fn remove_many(&mut self, rs: &rustc_hash::FxHashSet<Ref>) {
        let mut hit: rustc_hash::FxHashSet<[i32; 3]> = Default::default();
        for r in rs {
            self.big.remove(r);
            if let Some((a, b)) = self.of.remove(r) {
                for x in a[0]..=b[0] {
                    for y in a[1]..=b[1] {
                        for z in a[2]..=b[2] {
                            hit.insert([x, y, z]);
                        }
                    }
                }
            }
        }
        for k in hit {
            if let Some(v) = self.cells.get_mut(&k) {
                v.retain(|x| !rs.contains(x));
                if v.is_empty() {
                    self.cells.remove(&k);
                }
            }
        }
    }

    /// A part moved or resized: nothing to do while it stays in the same
    /// cells (most moving parts, most frames).
    fn update(&mut self, p: &QPart) {
        if let Some(range) = self.of.get(&p.r) {
            let (lo, hi) = part_aabb(p);
            if lo.is_finite() && hi.is_finite() && Self::cell_range(lo, hi) == *range {
                return;
            }
        }
        self.remove(p.r);
        self.insert(p);
    }

    fn insert(&mut self, p: &QPart) {
        let (lo, hi) = part_aabb(p);
        let (a, b) = Self::cell_range(lo, hi);
        let count = Self::cells_in(a, b);
        if !(lo.is_finite() && hi.is_finite()) || count <= 0 || count > mech::QUERY_GRID_MAX_CELLS as i64 {
            if crate::runtime::memstats_on() && count > 1 << 31 {
                eprintln!("MEMSTATS query part spans {count} cells: pos {:?} size {:?}", p.xf.pos, p.size);
            }
            self.big.insert(p.r);
            return;
        }
        for x in a[0]..=b[0] {
            for y in a[1]..=b[1] {
                for z in a[2]..=b[2] {
                    self.cells.entry([x, y, z]).or_default().push(p.r);
                }
            }
        }
        self.of.insert(p.r, (a, b));
    }

    fn rebuild(&mut self, parts: &[QPart]) {
        *self = QueryGrid::default();
        for p in parts {
            self.insert(p);
        }
    }

    /// Parts whose cells meet a box; None when the box covers too many cells
    /// (the caller scans everything instead).
    fn in_box(&self, lo: Vec3, hi: Vec3) -> Option<rustc_hash::FxHashSet<Ref>> {
        let (a, b) = Self::cell_range(lo, hi);
        let count = Self::cells_in(a, b);
        if count > mech::QUERY_GRID_SCAN_CELLS as i64 || count <= 0 {
            return None;
        }
        let mut out: rustc_hash::FxHashSet<Ref> = self.big.clone();
        for x in a[0]..=b[0] {
            for y in a[1]..=b[1] {
                for z in a[2]..=b[2] {
                    if let Some(v) = self.cells.get(&[x, y, z]) {
                        out.extend(v.iter().copied());
                    }
                }
            }
        }
        Some(out)
    }

    /// `in_box` for rays: the cells' parts without the always-tested big
    /// ones and without removing repeats (a ray testing a part twice gets
    /// the same nearest hit), into a reused buffer.
    fn in_box_into(&self, lo: Vec3, hi: Vec3, out: &mut Vec<Ref>) -> bool {
        let (a, b) = Self::cell_range(lo, hi);
        let count = Self::cells_in(a, b);
        if count > mech::QUERY_GRID_SCAN_CELLS as i64 || count <= 0 {
            return false;
        }
        for x in a[0]..=b[0] {
            for y in a[1]..=b[1] {
                for z in a[2]..=b[2] {
                    if let Some(v) = self.cells.get(&[x, y, z]) {
                        out.extend_from_slice(v);
                    }
                }
            }
        }
        true
    }
}

/// Where a part's Size box sits: its CFrame, except an off-centre union
/// (GeometryService keeps the main part's CFrame; mesh_bvh::union_offset).
fn geometry_box(d: &Document, r: Ref, class: &str, xf: Xform, size: Vec3) -> Xform {
    let id = if doc::is_a(class, "PartOperation") {
        rustudio_core::mesh::union_mesh(d, r).map(|m| m.id)
    } else if class == "MeshPart" {
        rustudio_render::scene::content_string(d.prop(r, "MeshId"))
    } else {
        None
    };
    match id {
        Some(id) => Xform { pos: xf.pos + xf.rot * (rustudio_core::mesh_bvh::union_offset(&id) * size), rot: xf.rot },
        None => xf,
    }
}

fn qpart(d: &Document, r: Ref) -> Option<QPart> {
    let class = d.class_of(r)?;
    if class == "Terrain" || !doc::is_a(&class, "BasePart") {
        return None;
    }
    let ws = d.workspace()?;
    if !d.is_ancestor(ws, r) {
        return None;
    }
    let xf = rustudio_render::scene::xform_of(d, r)?;
    let size = rustudio_render::scene::vec3_of(d.prop(r, "Size")).unwrap_or(Vec3::ONE);
    let collide = !matches!(d.prop(r, "CanCollide"), Some(Variant::Bool(false)));
    let query = !matches!(d.prop(r, "CanQuery"), Some(Variant::Bool(false)));
    let mesh_class = class == "MeshPart" || doc::is_a(&class, "PartOperation");
    // Studio's own collision pieces (PhysicalConfigData), which also say the
    // CollisionFidelity: the file has no such property for these classes.
    let geo = if mesh_class {
        rustudio_physics::physics_mesh::of_part_id(d, r)
    } else {
        None
    };
    let fidelity = if mesh_class {
        rustudio_physics::physics_mesh::fidelity(d, r)
    } else {
        None
    };
    let cvx = match &geo {
        Some((rustudio_physics::physics_mesh::Geometry::Pieces(_), id)) if fidelity != Some(rustudio_physics::physics_mesh::FIDELITY_BOX) => *id,
        _ => 0,
    };
    let initial = rustudio_render::scene::vec3_of(d.prop(r, "InitialSize"))
        .or_else(|| rustudio_render::scene::vec3_of(d.prop(r, "MeshSize")))
        .filter(|v| v.max_element() > 0.0)
        .unwrap_or(size);
    // The pieces sit around the part's CFrame; otherwise the drawn mesh's box.
    let xf = if cvx != 0 { xf } else { geometry_box(d, r, &class, xf, size) };
    // CollisionFidelity: Default 0, Hull 1, Box 2, PreciseConvexDecomposition 3.
    let mesh = mesh_class && cvx == 0 && fidelity != Some(rustudio_physics::physics_mesh::FIDELITY_BOX);
    let shape = rustudio_physics::query_shapes::kind_of(d, r, &class);
    Some(QPart {
        r,
        xf,
        size,
        collide,
        query,
        mesh,
        shape,
        cvx,
        initial,
    })
}

/// Builds the world-query cache now (Play start) rather than inside the
/// game frame of the first ray: Place B's 50k parts took 80–90 ms there.
pub fn warm_queries(lua: &Lua) {
    let _ = query_parts(lua);
}

fn query_parts(lua: &Lua) -> mlua::Result<std::rc::Rc<Vec<QPart>>> {
    if lua.app_data_ref::<QueryCache>().is_none() {
        lua.set_app_data(QueryCache::default());
    }
    let mut w = world(lua)?;
    let d = w.dm();
    let marks = d.marks_for(doc::Reader::Query);
    let mut c = lua
        .app_data_mut::<QueryCache>()
        .ok_or_else(|| rt_err("query cache missing"))?;
    let marks = if c.doc_id != d.id {
        doc::Marks::All
    } else {
        marks
    };
    match marks {
        doc::Marks::All => {
            let parts: Vec<QPart> = match d.workspace() {
                Some(ws) => d
                    .dom
                    .descendants_of(ws)
                    .filter_map(|i| qpart(d, i.referent()))
                    .collect(),
                None => Vec::new(),
            };
            c.index = parts.iter().enumerate().map(|(i, p)| (p.r, i)).collect();
            c.grid.rebuild(&parts);
            c.parts = std::rc::Rc::new(parts);
            c.doc_id = d.id;
            c.terrain = None;
        }
        doc::Marks::Some(m) if !m.is_empty() => {
            let mut touched: rustc_hash::FxHashSet<Ref> = Default::default();
            // Property changes of parts already listed: class and place in
            // the tree are as they were, only these values are read again.
            // Which of CFrame 1, Size 2, CanCollide 4, CanQuery 8 changed.
            let mut props: rustc_hash::FxHashMap<Ref, u8> = Default::default();
            // (Names compared as interned pointers: a frame of animation
            // writes is thousands of marks, most of other properties.)
            thread_local!(static NAMES: [rbx_dom_weak::Ustr; 6] =
                ["CFrame", "Size", "CanCollide", "CanQuery", "SmoothGrid", "Shape"].map(rbx_dom_weak::ustr));
            let [n_cf, n_size, n_collide, n_query, n_smooth, n_shape] = NAMES.with(|k| *k);
            for mark in m {
                match mark {
                    doc::Mark::Prop(r, name) => {
                        let bit = if name == n_cf {
                            1
                        } else if name == n_size {
                            2
                        } else if name == n_collide {
                            4
                        } else if name == n_query {
                            8
                        } else {
                            0
                        };
                        // Parts only (a rig's Bones and Attachments are most
                        // of a frame's marks: qpart answers None for them).
                        if bit != 0 && c.is_part(d, r) {
                            if c.index.contains_key(&r) {
                                *props.entry(r).or_default() |= bit;
                            } else {
                                // Not a query part yet: checked in full.
                                touched.insert(r);
                            }
                        }
                        if bit == 0 && name == n_smooth {
                            c.terrain = None;
                        }
                        // A part's Shape: read the part again in full.
                        if bit == 0 && name == n_shape {
                            touched.insert(r);
                        }
                    }
                    doc::Mark::Tree(r) => {
                        if d.class_of(r).is_some_and(|k| k == "Terrain" || k == "Workspace") {
                            c.terrain = None;
                        }
                        touched.extend(d.subtree(r))
                    }
                }
            }
            let QueryCache {
                parts, index, grid, ..
            } = &mut *c;
            let parts = std::rc::Rc::make_mut(parts);
            for (r, bits) in props {
                if touched.contains(&r) {
                    continue;
                }
                let Some(&i) = index.get(&r) else {
                    touched.insert(r);
                    continue;
                };
                // The instance once, names interned once (stored values;
                // Document::prop for defaults).
                thread_local!(static KEYS: [rbx_dom_weak::Ustr; 4] =
                    ["CFrame", "Size", "CanCollide", "CanQuery"].map(rbx_dom_weak::ustr));
                let [k_cf, k_size, k_collide, k_query] = KEYS.with(|k| *k);
                let Some(inst) = d.dom.get_by_ref(r) else {
                    touched.insert(r);
                    continue;
                };
                let get = |k: rbx_dom_weak::Ustr| {
                    inst.properties.get(&k).cloned().or_else(|| d.prop(r, k.as_str()))
                };
                let p = &mut parts[i];
                if bits & 1 != 0 {
                    let Some(Variant::CFrame(cf)) = get(k_cf) else {
                        touched.insert(r);
                        continue;
                    };
                    p.xf = rustudio_core::math::Xform::from_cframe(&cf).orthonormalized();
                }
                if bits & 2 != 0 {
                    p.size = rustudio_render::scene::vec3_of(get(k_size)).unwrap_or(Vec3::ONE);
                }
                if bits & 4 != 0 {
                    p.collide = !matches!(get(k_collide), Some(Variant::Bool(false)));
                }
                if bits & 8 != 0 {
                    p.query = !matches!(get(k_query), Some(Variant::Bool(false)));
                }
                let p = *p;
                grid.update(&p);
            }
            // Out of the grid all at once, then back in (re-read) one by one.
            if touched.len() > 64 {
                grid.remove_many(&touched);
            }
            let bulk = touched.len() > 64;
            for r in touched {
                if !bulk {
                    grid.remove(r);
                }
                match (qpart(d, r), index.get(&r).copied()) {
                    (Some(p), Some(i)) => {
                        grid.insert(&p);
                        parts[i] = p;
                    }
                    (Some(p), None) => {
                        grid.insert(&p);
                        index.insert(r, parts.len());
                        parts.push(p);
                    }
                    (None, Some(i)) => {
                        parts.swap_remove(i);
                        index.remove(&r);
                        if let Some(moved) = parts.get(i) {
                            index.insert(moved.r, i);
                        }
                    }
                    (None, None) => {}
                }
            }
        }
        _ => {}
    }
    Ok(c.parts.clone())
}

/// Include / exclude filter by instance and descendants.
struct Filter {
    list: rustc_hash::FxHashSet<Ref>,
    include: bool,
    respect_collide: bool,
    /// Collision groups that don't collide with the query's CollisionGroup
    /// (PhysicsService:CollisionGroupSetCollidable): their parts are passed.
    skip_groups: HashSet<String>,
    /// RaycastParams.IgnoreWater (and the legacy ignoreWater argument):
    /// rays and shapecasts pass through water terrain.
    ignore_water: bool,
}

impl Filter {
    fn none() -> Filter {
        Filter {
            list: Default::default(),
            include: false,
            respect_collide: false,
            skip_groups: HashSet::new(),
            ignore_water: false,
        }
    }

    fn passes(&self, d: &Document, r: Ref) -> bool {
        if !self.skip_groups.is_empty() {
            if let Some(Variant::String(g)) = d.prop(r, "CollisionGroup") {
                if self.skip_groups.contains(&g) {
                    return false;
                }
            }
        }
        if self.list.is_empty() {
            return !self.include;
        }
        let mut cur = r;
        let mut inside = false;
        while let Some(i) = d.dom.get_by_ref(cur) {
            if self.list.contains(&cur) {
                inside = true;
                break;
            }
            cur = i.parent();
        }
        inside == self.include
    }
}

/// A query's filter with its RaycastParams / OverlapParams CollisionGroup
/// applied (groups set not to collide with it are skipped).
fn params_filter_groups(lua: &Lua, v: &Value) -> Filter {
    // RaycastParams / OverlapParams: read in place (each field through
    // __index was most of a short ray's cost).
    let fast = match v {
        Value::UserData(u) => u
            .borrow::<crate::ltypes::LRaycastParams>()
            .ok()
            .map(|p| (params_filter_of(&p.0), p.0.collision_group.clone()))
            .or_else(|| {
                u.borrow::<crate::ltypes::LOverlapParams>()
                    .ok()
                    .map(|p| (params_filter_of(&p.0), p.0.collision_group.clone()))
            }),
        _ => None,
    };
    let (mut f, group) = match fast {
        Some(x) => x,
        None => {
            let f = params_filter(v);
            let group = match ud_value(v, "CollisionGroup") {
                Value::String(s) => s.to_str().map(|s| s.to_string()).unwrap_or_default(),
                _ => String::new(),
            };
            (f, group)
        }
    };
    let group = if group.is_empty() {
        "Default".to_string()
    } else {
        group
    };
    if let Ok(w) = world(lua) {
        if let Ok(g) = w.groups.lock() {
            f.skip_groups.extend(g.uncollidable_with(&group));
        }
    }
    f
}

/// `params_filter` from the params' own fields (what their getters return).
fn params_filter_of(q: &crate::ltypes::QueryParams) -> Filter {
    Filter {
        list: q.filter.iter().filter_map(opt_inst).collect(),
        include: q.is_include(),
        respect_collide: q.respect_can_collide,
        skip_groups: HashSet::new(),
        ignore_water: q.ignore_water,
    }
}

fn params_filter(v: &Value) -> Filter {
    let list: rustc_hash::FxHashSet<Ref> = refs_in(&ud_value(v, "FilterDescendantsInstances"))
        .into_iter()
        .collect();
    let include = opt_ud::<LEnumItem>(&ud_value(v, "FilterType"))
        .is_some_and(|e| matches!(e.name.as_str(), "Include" | "Whitelist"));
    let respect_collide = matches!(ud_value(v, "RespectCanCollide"), Value::Boolean(true));
    let ignore_water = matches!(ud_value(v, "IgnoreWater"), Value::Boolean(true));
    Filter {
        list,
        include,
        respect_collide,
        skip_groups: HashSet::new(),
        ignore_water,
    }
}

/// Closest hit along origin + dir (dir's length is the range): (part, point, normal, distance).
fn cast(
    lua: &Lua,
    origin: Vec3,
    dir: Vec3,
    filter: &Filter,
) -> mlua::Result<Option<(Ref, Vec3, Vec3, f32)>> {
    cast_swept(lua, origin, dir, filter, Vec3::ZERO, None)
}

/// A ray, or with `grow` > 0 a swept shape: each part's box grown by the
/// shape's half extents (a sphere's radius, a block's world half extents),
/// so the centre's ray against it is the sweep (Spherecast / Blockcast /
/// Shapecast). Parts the shape starts inside aren't hit, as in Roblox.
/// The grown box only narrows the parts down: `probe` (the swept shape's
/// query_shapes kind, size and turn) is then swept against each part's true
/// shape, and rays test balls / cylinders / wedges exactly too.
fn cast_swept(
    lua: &Lua,
    origin: Vec3,
    dir: Vec3,
    filter: &Filter,
    grow: Vec3,
    probe: Option<(u8, Vec3, glam::Mat3)>,
) -> mlua::Result<Option<(Ref, Vec3, Vec3, f32)>> {
    let hit = cast_swept_once(lua, origin, dir, filter, grow, probe)?;
    match hit {
        Some(h) if !queryable(lua, h.0)? => {
            // The cache held a part that is destroyed or out of the
            // Workspace: rebuilt from the tree, then cast again (a query
            // never returns one, as in Studio).
            stale_query_part(lua, h.0, "cast");
            let again = cast_swept_once(lua, origin, dir, filter, grow, probe)?;
            Ok(match again {
                Some(h) if !queryable(lua, h.0)? => None,
                other => other,
            })
        }
        other => Ok(other),
    }
}

/// Whether `r` may come back from a world query: in the tree, under the
/// Workspace (Terrain included).
fn queryable(lua: &Lua, r: Ref) -> mlua::Result<bool> {
    let w = world(lua)?;
    let d = w.d();
    Ok(d.workspace().is_some_and(|ws| d.dom.get_by_ref(r).is_some() && d.is_ancestor(ws, r)))
}

/// A query met a cached part that is no longer queryable (the change
/// journal missed its removal): the cache is rebuilt on the next query, and
/// the first few are reported with what is known of the part.
fn stale_query_part(lua: &Lua, r: Ref, query: &str) {
    if let Some(mut c) = lua.app_data_mut::<QueryCache>() {
        c.doc_id = 0;
    }
    static SEEN: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
    if SEEN.fetch_add(1, std::sync::atomic::Ordering::Relaxed) < 8 {
        let what = world(lua)
            .ok()
            .map(|w| match w.d().dom.get_by_ref(r) {
                Some(i) => format!("{} {:?} (not under Workspace: {})", i.class, i.name, w.full_name(r)),
                None => "destroyed".to_string(),
            })
            .unwrap_or_default();
        eprintln!("[query] {query}: stale part {r:?} in the query cache, {what}; rebuilt");
    }
}

fn cast_swept_once(
    lua: &Lua,
    origin: Vec3,
    dir: Vec3,
    filter: &Filter,
    grow: Vec3,
    probe: Option<(u8, Vec3, glam::Mat3)>,
) -> mlua::Result<Option<(Ref, Vec3, Vec3, f32)>> {
    let swept = grow != Vec3::ZERO;
    let len = dir.length();
    if len < 1e-6 {
        return Ok(None);
    }
    let ray = rustudio_core::math::Ray {
        origin,
        dir: dir / len,
    };
    // (distance, part, the hit's world normal when not a box face, the
    // touching point when not on the ray)
    type Best = Option<(f32, QPart, Option<Vec3>, Option<Vec3>)>;
    let mut best: Best = None;
    let test = |p: &QPart, d: &Document, best: &mut Best| {
        // CanQuery only counts once CanCollide is off: Studio still hits a
        // CanCollide part whose CanQuery is false (Laser Tag's containment
        // walls and spawn forcefields).
        if (!p.query && !p.collide) || (filter.respect_collide && !p.collide) {
            return;
        }
        // Boxes: entered from outside (a ray starting inside a part doesn't
        // hit it). MeshParts: their mesh's triangles, when the mesh is here.
        let mut normal: Option<Vec3> = None;
        let mut touch: Option<Vec3> = None;
        let t = if p.cvx != 0 {
            // Studio's collision pieces: the grown box only narrows down (a
            // probe may start inside the box yet outside every piece).
            if ray.hit_obb(&p.xf, p.size + grow * 2.0).is_none_or(|t| best.is_some_and(|(bt, ..)| t >= bt)) {
                return;
            }
            let scale = rustudio_physics::physics_mesh::scale(p.initial, p.size);
            if swept {
                let Some((kind, size, rot)) = probe else { return };
                let Some(shapes) = rustudio_physics::physics_mesh::shapes(p.cvx, scale, p.size) else { return };
                let start = rustudio_core::math::Xform { pos: ray.origin, rot };
                let Some((t, at, n)) =
                    rustudio_physics::query_shapes::sweep_pieces(
                        &start,
                        size,
                        kind,
                        ray.dir,
                        len,
                        &p.xf,
                        &shapes,
                    )
                else {
                    return;
                };
                normal = Some(n);
                touch = Some(at);
                t
            } else {
                let Some(pieces) = rustudio_physics::physics_mesh::pieces(p.cvx) else { return };
                let Some((t, n)) = rustudio_physics::physics_mesh::ray(&pieces, &p.xf, scale, ray.origin, ray.dir, len) else {
                    return;
                };
                normal = Some(n);
                t
            }
        } else if swept {
            // Exact shapes below: only a box the probe starts inside is out
            // (sweep_part then reports the start overlap).
            let Some(t) = (if probe.is_some() { ray.hit_obb(&p.xf, p.size + grow * 2.0) } else { ray.enter_obb(&p.xf, p.size + grow * 2.0) }) else {
                return;
            };
            match probe {
                Some((kind, size, rot)) => {
                    if best.is_some_and(|(bt, ..)| t >= bt) {
                        return;
                    }
                    let start = rustudio_core::math::Xform { pos: ray.origin, rot };
                    let Some((t, at, n)) = rustudio_physics::query_shapes::sweep_part(
                        &start, size, kind, ray.dir, len, &p.xf, p.size, p.shape,
                    ) else {
                        return;
                    };
                    normal = Some(n);
                    touch = Some(at);
                    t
                }
                None => t,
            }
        } else if p.shape != rustudio_physics::query_shapes::BOX && !p.mesh {
            let Some((t, n)) = rustudio_physics::query_shapes::ray_part(&p.xf, p.size, p.shape, ray.origin, ray.dir)
            else {
                return;
            };
            normal = Some(n);
            t
        } else if p.mesh {
            let Some(_) = ray.hit_obb(&p.xf, p.size) else {
                return;
            };
            // Borrowed when stored as MeshId; else the canonical lookup
            // (MeshContent and other stored names).
            let owned;
            let id = match d.dom.get_by_ref(p.r).and_then(|i| i.properties.get(&ustr("MeshId"))) {
                Some(Variant::ContentId(c)) => c.as_str(),
                _ => {
                    owned = match d.prop(p.r, "MeshId") {
                        Some(Variant::ContentId(c)) => c.as_str().to_string(),
                        // A union: its mesh (geometry_box already centred p.xf on it).
                        _ => rustudio_core::mesh::union_mesh(d, p.r).map(|m| m.id).unwrap_or_default(),
                    };
                    owned.as_str()
                }
            };
            let _m = rustudio_core::bench::scope("ray_mesh");
            match (!id.is_empty())
                .then(|| rustudio_core::mesh_bvh::for_mesh(id))
                .flatten()
            {
                Some(bvh) => {
                    rustudio_core::bench::add("ray_mesh_tris", 0.0, bvh.tris.len() as u64);
                    let inv = p.xf.rot.transpose();
                    let size = p.size.max(Vec3::splat(1e-4));
                    let o = (inv * (ray.origin - p.xf.pos)) / size;
                    let dl = (inv * ray.dir) / size;
                    let Some((t, n)) = bvh.ray_normal(o, dl) else {
                        return;
                    };
                    // Unit-mesh normal to world: inverse-transpose of the scale.
                    normal = Some((p.xf.rot * (n / size)).normalize_or_zero());
                    t
                }
                None => {
                    let Some(t) = ray.enter_obb(&p.xf, p.size) else {
                        return;
                    };
                    t
                }
            }
        } else {
            // A ray that only touches an edge or corner (no length inside)
            // misses the box, as in Studio (a diagonal ray through a wall's
            // corner edge passes it).
            let Some(t) = ray.enter_obb(&p.xf, p.size) else {
                return;
            };
            if ray_box_chord(&ray, &p.xf, p.size) <= 1e-4 {
                return;
            }
            t
        };
        if t < 0.0 || t > len || best.is_some_and(|(bt, ..)| t >= bt) {
            return;
        }
        if filter.passes(d, p.r) {
            *best = Some((t, *p, normal, touch));
        }
    };
    // Along the ray a few grid cells at a time; stop once a hit is nearer
    // than the next stretch.
    let parts_scope = rustudio_core::bench::scope("ray_parts");
    {
        let _q = rustudio_core::bench::scope("ray_query_sync");
        query_parts(lua)?;
    }
    let step = mech::QUERY_GRID_CELL * 4.0;
    let mut t0 = 0.0f32;
    let mut scanned_all = false;
    let mut refs: Vec<Ref> = Vec::new();
    let mut big_done = false;
    while t0 < len && !scanned_all {
        let t1 = (t0 + step).min(len);
        let (a, b) = (ray.at(t0), ray.at(t1));
        let w = world(lua)?;
        let d = w.d();
        let c = lua.app_data_ref::<QueryCache>();
        refs.clear();
        let gridded = c.as_ref().is_some_and(|c| c.grid.in_box_into(a.min(b) - grow, a.max(b) + grow, &mut refs));
        match (gridded, c.as_ref()) {
            (true, Some(c)) => {
                // Parts too big for the grid: once per ray.
                if !big_done {
                    big_done = true;
                    refs.extend(c.grid.big.iter().copied());
                }
                for r in &refs {
                    if let Some(p) = c.index.get(r).and_then(|i| c.parts.get(*i)) {
                        test(p, d, &mut best);
                    }
                }
            }
            _ => {
                drop(c);
                let parts = query_parts_cached(lua)?;
                for p in parts.iter() {
                    test(p, d, &mut best);
                }
                scanned_all = true;
            }
        }
        if best.is_some_and(|(bt, ..)| bt <= t1) {
            break;
        }
        t0 = t1;
    }
    // Terrain: rays meet its surface, sweeps its surface with the shape.
    {
        drop(parts_scope);
        let _t = rustudio_core::bench::scope("ray_terrain");
        let terrain = match probe {
            Some(pr) if swept => terrain_sweep(lua, pr, ray.origin, grow, ray.dir, len, filter.ignore_water)?,
            _ => terrain_hit(lua, ray.origin, ray.dir, len, filter.ignore_water)?,
        };
        let passes = terrain.as_ref().is_some_and(|(r, ..)| {
            world(lua).is_ok_and(|w| filter.passes(w.d(), *r))
        });
        if let Some(mut c) = lua.app_data_mut::<QueryCache>() {
            c.terrain_material = None;
        }
        if let (true, Some((r, point, n, t, m))) = (passes, terrain) {
            if best.as_ref().is_none_or(|(bt, ..)| t < *bt) {
                if let Some(mut c) = lua.app_data_mut::<QueryCache>() {
                    c.terrain_material = Some(m);
                }
                return Ok(Some((r, point, n, t)));
            }
        }
    }
    Ok(best.map(|(t, p, shape_normal, touch)| {
        let point = ray.at(t);
        if let Some(n) = shape_normal {
            return (p.r, touch.unwrap_or(point), n, t);
        }
        // Normal of the box face the ray enters through (of the grown box for
        // a sweep): the slab entered last; through an edge or corner, the
        // faces entered there together, summed (Studio: a ray into a corner
        // gets (-1, -1, 1) / √3). A face along the ray isn't entered (from
        // x = -150 along +Z into a wall whose side is at x = -150: its front
        // face, (0, 0, -1)).
        let inv = p.xf.rot.transpose();
        let (lo, ld) = (inv * (ray.origin - p.xf.pos), inv * ray.dir);
        let half = ((p.size + grow * 2.0) * 0.5).max(Vec3::splat(1e-4));
        let mut slabs = [f32::NEG_INFINITY; 3];
        for k in 0..3 {
            if ld[k].abs() >= 1e-8 {
                slabs[k] = ((-half[k] - lo[k]) / ld[k]).min((half[k] - lo[k]) / ld[k]);
            }
        }
        let enter = slabs[0].max(slabs[1]).max(slabs[2]);
        let mut n = Vec3::ZERO;
        for k in 0..3 {
            if slabs[k].is_finite() && (enter - slabs[k]) * ld[k].abs() <= 1e-4 {
                n[k] = -ld[k].signum();
            }
        }
        let n = p.xf.rot * if n == Vec3::ZERO { -ld } else { n }.normalize_or_zero();
        // A sweep reports where the shape touches the part: back from the
        // centre by the shape's extent along the normal.
        let touch = if swept {
            point - n * (n.abs().dot(grow))
        } else {
            point
        };
        (p.r, touch, n, t)
    }))
}

/// How long a ray stays inside a box (0 when it misses or only touches it).
fn ray_box_chord(ray: &rustudio_core::math::Ray, xf: &Xform, size: Vec3) -> f32 {
    let inv = xf.rot.transpose();
    let (o, d) = (inv * (ray.origin - xf.pos), inv * ray.dir);
    let h = size * 0.5;
    let (mut a, mut b) = (f32::NEG_INFINITY, f32::INFINITY);
    for k in 0..3 {
        if d[k].abs() < 1e-8 {
            if o[k] < -h[k] || o[k] > h[k] {
                return 0.0;
            }
        } else {
            let (t1, t2) = ((-h[k] - o[k]) / d[k], (h[k] - o[k]) / d[k]);
            a = a.max(t1.min(t2));
            b = b.min(t1.max(t2));
        }
    }
    (b - a).max(0.0)
}

/// Parts a query has to look at: with an include list just the parts in it.
fn candidates(lua: &Lua, filter: &Filter) -> mlua::Result<std::rc::Rc<Vec<QPart>>> {
    if filter.include {
        // The listed parts as the (current) query cache holds them: every
        // BasePart under Workspace is in it.
        let all = query_parts(lua)?;
        let w = world(lua)?;
        let d = w.d();
        let c = lua.app_data_ref::<QueryCache>();
        let mut out = Vec::new();
        for root in &filter.list {
            for i in d.dom.descendants_of(*root) {
                let r = i.referent();
                match c.as_ref().and_then(|c| c.index.get(&r)).and_then(|k| all.get(*k)) {
                    Some(p) if p.r == r => out.push(*p),
                    _ => {
                        if let Some(p) = qpart(d, r) {
                            out.push(p);
                        }
                    }
                }
            }
        }
        return Ok(std::rc::Rc::new(out));
    }
    query_parts(lua)
}

fn part_aabb(p: &QPart) -> (Vec3, Vec3) {
    let h = p.size * 0.5;
    let r = p.xf.rot;
    let ext = r.x_axis.abs() * h.x + r.y_axis.abs() * h.y + r.z_axis.abs() * h.z;
    (p.xf.pos - ext, p.xf.pos + ext)
}

/// The query parts near a box (from the grid), or None when the box is too
/// big for the grid to help. The cache must be current (query_parts).
fn near_parts(lua: &Lua, lo: Vec3, hi: Vec3) -> mlua::Result<Option<Vec<QPart>>> {
    let Some(c) = lua.app_data_ref::<QueryCache>() else {
        return Ok(None);
    };
    let Some(refs) = c.grid.in_box(lo, hi) else {
        return Ok(None);
    };
    Ok(Some(
        refs.iter()
            .filter_map(|r| c.index.get(r).and_then(|i| c.parts.get(*i)).copied())
            .collect(),
    ))
}

/// PathfindingService's view of the world near a box (geometry_nav.rs):
/// the Workspace parts the grid has there, in the cache's order (stable for
/// a given history of edits), and the terrain voxels, both kept current
/// from the document journal instead of a walk over Workspace and a
/// terrain decode per path request.
pub(crate) fn nav_world(lua: &Lua, lo: Vec3, hi: Vec3) -> mlua::Result<crate::runtime::domains::geometry::nav::NavWorld> {
    query_parts(lua)?;
    let parts = {
        let Some(c) = lua.app_data_ref::<QueryCache>() else {
            return Ok(Default::default());
        };
        Some(match c.grid.in_box(lo, hi) {
            Some(refs) => {
                let mut v: Vec<(usize, Ref)> = refs.into_iter().filter_map(|r| c.index.get(&r).map(|i| (*i, r))).collect();
                v.sort_unstable_by_key(|e| e.0);
                v.into_iter().map(|e| e.1).collect()
            }
            // Too many cells for the grid: the flat list, by bounds.
            None => c
                .parts
                .iter()
                .filter(|p| {
                    let (a, b) = part_aabb(p);
                    !(a.cmpgt(hi).any() || b.cmplt(lo).any())
                })
                .map(|p| p.r)
                .collect(),
        })
    };
    let voxels = Some(terrain_state(lua)?.map(|(_, v, (blo, bhi))| {
        let cell = |x: f32, up: bool| {
            let c = x / rustudio_core::terrain::VOXEL;
            (if up { c.round() as i32 - 1 } else { c.round() as i32 })
        };
        let b = (
            [cell(blo.x, false), cell(blo.y, false), cell(blo.z, false)],
            [cell(bhi.x, true), cell(bhi.y, true), cell(bhi.z, true)],
        );
        (v, b)
    }));
    Ok(crate::runtime::domains::geometry::nav::NavWorld { parts, voxels })
}

/// The query parts as last updated (no journal read).
fn query_parts_cached(lua: &Lua) -> mlua::Result<std::rc::Rc<Vec<QPart>>> {
    Ok(lua
        .app_data_ref::<QueryCache>()
        .map(|c| c.parts.clone())
        .unwrap_or_default())
}

/// Parts whose bounds overlap an oriented box (as an AABB) or a sphere.
fn overlap(
    lua: &Lua,
    lo: Vec3,
    hi: Vec3,
    sphere: Option<(Vec3, f32)>,
    filter: &Filter,
    max: usize,
) -> mlua::Result<Vec<Ref>> {
    let out = overlap_once(lua, lo, hi, sphere, filter, max)?;
    let mut stale = None;
    for r in &out {
        if !queryable(lua, *r)? {
            stale = Some(*r);
            break;
        }
    }
    let Some(r) = stale else { return Ok(out) };
    stale_query_part(lua, r, "overlap");
    let out = overlap_once(lua, lo, hi, sphere, filter, max)?;
    let mut keep = Vec::with_capacity(out.len());
    for r in out {
        if queryable(lua, r)? {
            keep.push(r);
        }
    }
    Ok(keep)
}

fn overlap_once(
    lua: &Lua,
    lo: Vec3,
    hi: Vec3,
    sphere: Option<(Vec3, f32)>,
    filter: &Filter,
    max: usize,
) -> mlua::Result<Vec<Ref>> {
    query_parts(lua)?;
    let parts = match near_parts(lua, lo, hi)? {
        Some(list) => std::rc::Rc::new(list),
        None => candidates(lua, filter)?,
    };
    let w = world(lua)?;
    let d = w.d();
    let mut out = Vec::new();
    for p in parts.iter() {
        if !p.query && !p.collide {
            continue;
        }
        let (plo, phi) = part_aabb(p);
        let hit = match sphere {
            Some((c, r)) => (c.clamp(plo, phi) - c).length() <= r,
            None => plo.cmple(hi).all() && phi.cmpge(lo).all(),
        };
        if hit && filter.passes(d, p.r) {
            out.push(p.r);
            if out.len() >= max {
                break;
            }
        }
    }
    Ok(out)
}

/// Workspace.Terrain with its voxels and their world bounds (studs), read
/// once per terrain change.
fn terrain_state(lua: &Lua) -> mlua::Result<Option<(Ref, std::sync::Arc<rustudio_core::terrain::Voxels>, (Vec3, Vec3))>> {
    let cached = {
        let c = lua.app_data_ref::<QueryCache>();
        c.and_then(|c| c.terrain.clone())
    };
    let terrain = match cached {
        Some(t) => t,
        None => {
            let t = {
                let w = world(lua)?;
                let d = w.d();
                let r = d.workspace().and_then(|ws| d.find_child(ws, "Terrain"));
                match (r, rustudio_core::terrain::Voxels::from_dom(&d.dom)) {
                    (Some(r), Some(Ok(v))) if v.count() > 0 => {
                        let b = v.world_bounds();
                        Some((r, std::sync::Arc::new(v), b))
                    }
                    _ => None,
                }
            };
            if let Some(mut c) = lua.app_data_mut::<QueryCache>() {
                c.terrain = Some(t.clone());
                c.terrain_tris.clear();
                c.terrain_water.clear();
                c.terrain_index.clear();
            }
            t
        }
    };
    Ok(terrain.and_then(|(r, v, b)| b.map(|(lo, hi)| (r, v, (Vec3::from(lo), Vec3::from(hi))))))
}

/// The chunk keys a world box touches (a chunk's surface reaches one voxel
/// past its corner; water's lies half a voxel lower still).
fn terrain_chunk_keys(lo: Vec3, hi: Vec3) -> impl Iterator<Item = [i32; 3]> {
    use rustudio_core::terrain::VOXEL;
    let chunk = VOXEL * 32.0;
    let a = ((lo - VOXEL) / chunk).floor().as_ivec3();
    let b = ((hi + VOXEL + Vec3::Y * rustudio_core::terrain::WATER_RAY_DROP) / chunk).floor().as_ivec3();
    (a.x..=b.x).flat_map(move |x| (a.y..=b.y).flat_map(move |y| (a.z..=b.z).map(move |z| [x, y, z])))
}

/// Terrain under a ray (WorldRoot:Raycast, the legacy FindPartOnRay family
/// and the play mouse): the smooth surface as drawn (each chunk's
/// triangles, material shapes included), front faces only, nearest along
/// the ray. Water is hit too (Studio's IgnoreWater defaults to false), on
/// its own surface half a voxel below the drawn one, unless `ignore_water`.
/// Returns (terrain, point, normal, distance, SmoothGrid material).
/// Round 19: rays never saw terrain; 2026-11-11: water.
fn terrain_hit(
    lua: &Lua,
    origin: Vec3,
    dir: Vec3,
    len: f32,
    ignore_water: bool,
) -> mlua::Result<Option<(Ref, Vec3, Vec3, f32, u8)>> {
    use rustudio_core::terrain::VOXEL;
    let Some((r, v, (lo, hi))) = terrain_state(lua)? else {
        return Ok(None);
    };
    let (lo, hi) = (lo - VOXEL - Vec3::Y * rustudio_core::terrain::WATER_RAY_DROP, hi + VOXEL);
    let dir_n = dir.normalize_or_zero();
    // The stretch of the ray inside the terrain's bounds.
    let (mut t0, mut t1) = (0.0f32, len);
    for k in 0..3 {
        if dir_n[k].abs() < 1e-9 {
            if origin[k] < lo[k] || origin[k] > hi[k] {
                return Ok(None);
            }
            continue;
        }
        let (a, b) = ((lo[k] - origin[k]) / dir_n[k], (hi[k] - origin[k]) / dir_n[k]);
        t0 = t0.max(a.min(b));
        t1 = t1.min(a.max(b));
    }
    if t0 > t1 {
        return Ok(None);
    }
    let mut best: Option<(f32, Vec3, u8)> = None;
    let chunk = VOXEL * 32.0;
    let mut seen: HashSet<[i32; 3]> = HashSet::new();
    let mut t = t0;
    let step = chunk * 0.5;
    loop {
        let seg_end = (t + step).min(t1);
        let (a, b) = (origin + dir_n * t, origin + dir_n * seg_end);
        for key in terrain_chunk_keys(a.min(b), a.max(b)) {
            if !seen.insert(key) {
                continue;
            }
            let (tris, water) = terrain_chunk_tris(lua, &v, key, !ignore_water);
            // The chunk's nearest front-facing hit in [t0, t1] (the first
            // triangle among equals, as a scan in order finds it), through
            // its cell index.
            if !tris.is_empty() {
                let index = terrain_tri_index(lua, key, &tris);
                if let Some((d, i, w)) = index.ray(&tris, origin, dir_n, t0, t1) {
                    if best.is_none_or(|(bt, _, _)| d < bt) {
                        let tri = &tris[i];
                        let n = (tri.0[1] - tri.0[0]).cross(tri.0[2] - tri.0[0]).normalize_or_zero();
                        let k = if w.x >= w.y && w.x >= w.z { 0 } else if w.y >= w.z { 1 } else { 2 };
                        best = Some((d, n, tri.1[k]));
                    }
                }
            }
            for tri in water.iter().flat_map(|w| w.iter()) {
                if let Some((d, _)) = ray_triangle(origin, dir_n, tri) {
                    if d >= t0 && d <= t1 && best.is_none_or(|(bt, _, _)| d < bt) {
                        let n = (tri[1] - tri[0]).cross(tri[2] - tri[0]).normalize_or_zero();
                        best = Some((d, n, rustudio_core::terrain::WATER));
                    }
                }
            }
        }
        if best.is_some_and(|(bt, _, _)| bt <= seg_end) || seg_end >= t1 {
            break;
        }
        t = seg_end;
    }
    Ok(best.map(|(d, n, m)| (r, origin + dir_n * d, n, d, m)))
}

/// Terrain against a swept shape (Spherecast / Blockcast / Shapecast):
/// `probe` is the query_shapes kind, size and turn, starting at `origin`,
/// with world half extents `ext`, moving along unit `dir` for `len`. The
/// same surfaces as `terrain_hit`; a shape that starts touching terrain
/// doesn't hit it (Studio). Returns (terrain, touching point, normal,
/// distance, material).
fn terrain_sweep(
    lua: &Lua,
    probe: (u8, Vec3, glam::Mat3),
    origin: Vec3,
    ext: Vec3,
    dir: Vec3,
    len: f32,
    ignore_water: bool,
) -> mlua::Result<Option<(Ref, Vec3, Vec3, f32, u8)>> {
    use rustudio_core::terrain::VOXEL;
    let Some((r, v, (lo, hi))) = terrain_state(lua)? else {
        return Ok(None);
    };
    let (lo, hi) = (lo - VOXEL - Vec3::Y * rustudio_core::terrain::WATER_RAY_DROP, hi + VOXEL);
    let end = origin + dir * len;
    let (plo, phi) = (origin.min(end) - ext, origin.max(end) + ext);
    if plo.cmpgt(hi).any() || phi.cmplt(lo).any() {
        return Ok(None);
    }
    let start = rustudio_core::math::Xform { pos: origin, rot: probe.2 };
    let chunk = VOXEL * 32.0;
    let step = chunk * 0.5;
    let mut seen: HashSet<[i32; 3]> = HashSet::new();
    // (distance, point, normal, material)
    let mut best: Option<(f32, Vec3, Vec3, u8)> = None;
    let mut t = 0.0f32;
    let mut cand: Vec<(usize, [Vec3; 3])> = Vec::new();
    let mut mats: Vec<[u8; 3]> = Vec::new();
    loop {
        let seg_end = (t + step).min(len);
        let (a, b) = (origin + dir * t, origin + dir * seg_end);
        let (slo, shi) = (a.min(b) - ext, a.max(b) + ext);
        cand.clear();
        mats.clear();
        for key in terrain_chunk_keys(slo, shi) {
            if !seen.insert(key) {
                continue;
            }
            let (tris, water) = terrain_chunk_tris(lua, &v, key, !ignore_water);
            let near = |p: &[Vec3; 3]| {
                let (tl, th) = (p[0].min(p[1]).min(p[2]), p[0].max(p[1]).max(p[2]));
                tl.cmple(phi).all() && th.cmpge(plo).all()
            };
            // The triangles in the cells the swept box meets, in index
            // order (the same candidates, in the same order, as a scan).
            let index = (!tris.is_empty()).then(|| terrain_tri_index(lua, key, &tris));
            let picked = index.as_ref().map(|ix| ix.in_box(plo, phi));
            let all: Box<dyn Iterator<Item = usize>> = match &picked {
                Some(list) => Box::new(list.iter().map(|i| *i as usize)),
                None => Box::new(0..tris.len()),
            };
            for i in all {
                let (p, m) = &tris[i];
                if near(p) {
                    cand.push((mats.len(), *p));
                    mats.push(*m);
                }
            }
            for p in water.iter().flat_map(|w| w.iter()) {
                if near(p) {
                    cand.push((mats.len(), *p));
                    mats.push([rustudio_core::terrain::WATER; 3]);
                }
            }
        }
        let hit = rustudio_physics::query_shapes::sweep_triangles(&start, probe.1, probe.0, dir, len, &cand);
        match hit {
            Err(()) => return Ok(None),
            Ok(Some((i, d, at, n))) if best.is_none_or(|(bt, ..)| d < bt) => {
                let tri = cand.iter().find(|(k, _)| *k == i).map(|c| c.1).unwrap_or([at; 3]);
                // The material of the triangle corner nearest the contact.
                let k = (0..3)
                    .min_by(|x, y| at.distance_squared(tri[*x]).total_cmp(&at.distance_squared(tri[*y])))
                    .unwrap_or(0);
                best = Some((d, at, n, mats[i][k]));
            }
            _ => {}
        }
        if best.is_some_and(|(bt, ..)| bt <= seg_end) || seg_end >= len {
            break;
        }
        t = seg_end;
    }
    Ok(best.map(|(d, at, n, m)| (r, at, n, d, m)))
}

/// One chunk's terrain triangles (positions, corner materials) and, when
/// `water`, its water surface for rays; cached until the terrain changes.
#[allow(clippy::type_complexity)]
fn terrain_chunk_tris(
    lua: &Lua,
    v: &rustudio_core::terrain::Voxels,
    key: [i32; 3],
    water: bool,
) -> (std::rc::Rc<Vec<([Vec3; 3], [u8; 3])>>, Option<std::rc::Rc<Vec<[Vec3; 3]>>>) {
    thread_local! {
        static NONE: (std::rc::Rc<Vec<([Vec3; 3], [u8; 3])>>, std::rc::Rc<Vec<[Vec3; 3]>>) = Default::default();
    }
    // Empty space (most of a long ray over a big map): nothing to mesh.
    if !v.chunk_near(key) {
        return NONE.with(|(s, w)| (s.clone(), water.then(|| w.clone())));
    }
    let solid = match lua.app_data_ref::<QueryCache>().and_then(|c| c.terrain_tris.get(&key).cloned()) {
        Some(t) => t,
        None => {
            let verts = rustudio_core::terrain::chunk_triangles(v, key, &|_| false);
            let tris: Vec<([Vec3; 3], [u8; 3])> = verts
                .chunks_exact(3)
                .map(|t| {
                    let mats = [0, 1, 2].map(|i| t[0].palette[i] / 3);
                    ([Vec3::from(t[0].pos), Vec3::from(t[1].pos), Vec3::from(t[2].pos)], mats)
                })
                .collect();
            let tris = std::rc::Rc::new(tris);
            if let Some(mut c) = lua.app_data_mut::<QueryCache>() {
                c.terrain_tris.insert(key, tris.clone());
            }
            tris
        }
    };
    if !water {
        return (solid, None);
    }
    let wet = match lua.app_data_ref::<QueryCache>().and_then(|c| c.terrain_water.get(&key).cloned()) {
        Some(t) => t,
        None => {
            let tris: Vec<[Vec3; 3]> = rustudio_core::terrain::chunk_water_triangles(v, key)
                .into_iter()
                .map(|t| t.map(Vec3::from))
                .collect();
            let tris = std::rc::Rc::new(tris);
            if let Some(mut c) = lua.app_data_mut::<QueryCache>() {
                c.terrain_water.insert(key, tris.clone());
            }
            tris
        }
    };
    (solid, Some(wet))
}

/// A uniform grid of cells over a chunk's triangles (by bounding box,
/// grown by `TriIndex::PAD`), in compressed rows: cell c's triangles are
/// `items[starts[c]..starts[c + 1]]`, in index order.
pub(crate) struct TriIndex {
    lo: Vec3,
    n: [i32; 3],
    starts: Vec<u32>,
    items: Vec<u32>,
}

impl TriIndex {
    const CELL: f32 = 8.0;
    /// Studs a triangle's box and a ray stretch's box are grown by: a hit
    /// point the triangle test puts a hair outside the triangle still lies
    /// in a cell the ray visits.
    const PAD: f32 = 0.01;

    fn build(tris: &[([Vec3; 3], [u8; 3])]) -> TriIndex {
        let mut lo = Vec3::splat(f32::INFINITY);
        let mut hi = Vec3::splat(f32::NEG_INFINITY);
        for (t, _) in tris {
            lo = lo.min(t[0]).min(t[1]).min(t[2]);
            hi = hi.max(t[0]).max(t[1]).max(t[2]);
        }
        lo -= Vec3::splat(Self::PAD);
        hi += Vec3::splat(Self::PAD);
        let span = ((hi - lo) / Self::CELL).ceil().max(Vec3::ONE).min(Vec3::splat(64.0));
        let n = [span.x as i32, span.y as i32, span.z as i32];
        let mut idx = TriIndex { lo, n, starts: vec![0; (n[0] * n[1] * n[2]) as usize + 1], items: Vec::new() };
        let range = |t: &[Vec3; 3], idx: &TriIndex| {
            let a = t[0].min(t[1]).min(t[2]) - Vec3::splat(Self::PAD);
            let b = t[0].max(t[1]).max(t[2]) + Vec3::splat(Self::PAD);
            (idx.cell_of(a), idx.cell_of(b))
        };
        for (t, _) in tris {
            let (a, b) = range(t, &idx);
            for x in a[0]..=b[0] {
                for y in a[1]..=b[1] {
                    for z in a[2]..=b[2] {
                        let c = idx.flat([x, y, z]);
                        idx.starts[c + 1] += 1;
                    }
                }
            }
        }
        for c in 1..idx.starts.len() {
            idx.starts[c] += idx.starts[c - 1];
        }
        let mut fill = idx.starts.clone();
        idx.items = vec![0; *idx.starts.last().unwrap_or(&0) as usize];
        for (i, (t, _)) in tris.iter().enumerate() {
            let (a, b) = range(t, &idx);
            for x in a[0]..=b[0] {
                for y in a[1]..=b[1] {
                    for z in a[2]..=b[2] {
                        let c = idx.flat([x, y, z]);
                        idx.items[fill[c] as usize] = i as u32;
                        fill[c] += 1;
                    }
                }
            }
        }
        idx
    }

    /// The triangles whose cells meet the box (a superset of those whose
    /// boxes do), each once, in index order.
    fn in_box(&self, lo: Vec3, hi: Vec3) -> Vec<u32> {
        let pad = Vec3::splat(Self::PAD);
        let top = self.lo + Vec3::new(self.n[0] as f32, self.n[1] as f32, self.n[2] as f32) * Self::CELL;
        if (hi + pad).cmplt(self.lo).any() || (lo - pad).cmpgt(top).any() {
            return Vec::new();
        }
        let (a, b) = (self.cell_of(lo - pad), self.cell_of(hi + pad));
        let mut out = Vec::new();
        for x in a[0]..=b[0] {
            for y in a[1]..=b[1] {
                for z in a[2]..=b[2] {
                    let c = self.flat([x, y, z]);
                    out.extend_from_slice(&self.items[self.starts[c] as usize..self.starts[c + 1] as usize]);
                }
            }
        }
        out.sort_unstable();
        out.dedup();
        out
    }

    fn cell_of(&self, p: Vec3) -> [i32; 3] {
        let c = ((p - self.lo) / Self::CELL).floor();
        [
            (c.x as i32).clamp(0, self.n[0] - 1),
            (c.y as i32).clamp(0, self.n[1] - 1),
            (c.z as i32).clamp(0, self.n[2] - 1),
        ]
    }

    fn flat(&self, c: [i32; 3]) -> usize {
        ((c[0] * self.n[1] + c[1]) * self.n[2] + c[2]) as usize
    }

    /// The nearest front-facing hit of the ray (unit `dir`) at a distance in
    /// [t0, t1]: (distance, triangle, barycentric weights), the lowest
    /// triangle index among equal distances: what testing every triangle
    /// in order with `ray_triangle` finds. Walks the ray a cell length at
    /// a time and stops once a hit lies inside the stretch walked.
    fn ray(&self, tris: &[([Vec3; 3], [u8; 3])], o: Vec3, dir: Vec3, t0: f32, t1: f32) -> Option<(f32, usize, Vec3)> {
        // The ray's stretch inside the index's box.
        let hi = self.lo + Vec3::new(self.n[0] as f32, self.n[1] as f32, self.n[2] as f32) * Self::CELL;
        let (mut ta, mut tb) = (t0, t1);
        for k in 0..3 {
            if dir[k].abs() < 1e-9 {
                if o[k] < self.lo[k] || o[k] > hi[k] {
                    return None;
                }
                continue;
            }
            let (a, b) = ((self.lo[k] - o[k]) / dir[k], (hi[k] - o[k]) / dir[k]);
            ta = ta.max(a.min(b));
            tb = tb.min(a.max(b));
        }
        if ta > tb {
            return None;
        }
        thread_local!(static SEEN: std::cell::RefCell<(Vec<u32>, u32)> = const { std::cell::RefCell::new((Vec::new(), 0)) });
        SEEN.with(|seen| {
            let mut seen = seen.borrow_mut();
            let (stamps, gen) = &mut *seen;
            if stamps.len() < tris.len() {
                stamps.resize(tris.len(), 0);
            }
            *gen = gen.wrapping_add(1);
            if *gen == 0 {
                stamps.iter_mut().for_each(|s| *s = 0);
                *gen = 1;
            }
            let g = *gen;
            let mut best: Option<(f32, usize, Vec3)> = None;
            let pad = Vec3::splat(Self::PAD);
            let mut t = ta;
            loop {
                let end = (t + Self::CELL).min(tb);
                let (p, q) = (o + dir * t, o + dir * end);
                let (a, b) = (self.cell_of(p.min(q) - pad), self.cell_of(p.max(q) + pad));
                for x in a[0]..=b[0] {
                    for y in a[1]..=b[1] {
                        for z in a[2]..=b[2] {
                            let c = self.flat([x, y, z]);
                            for &i in &self.items[self.starts[c] as usize..self.starts[c + 1] as usize] {
                                let i = i as usize;
                                if stamps[i] == g {
                                    continue;
                                }
                                stamps[i] = g;
                                if let Some((d, w)) = ray_triangle(o, dir, &tris[i].0) {
                                    if d >= t0
                                        && d <= t1
                                        && best.is_none_or(|(bd, bi, _)| d < bd || (d == bd && i < bi))
                                    {
                                        best = Some((d, i, w));
                                    }
                                }
                            }
                        }
                    }
                }
                // Every triangle hit up to `end` lies in a cell visited.
                if best.is_some_and(|(bd, ..)| bd + Self::PAD <= end) || end >= tb {
                    break;
                }
                t = end;
            }
            best
        })
    }
}

/// A chunk's triangle index (built on its first ray).
fn terrain_tri_index(lua: &Lua, key: [i32; 3], tris: &[([Vec3; 3], [u8; 3])]) -> std::rc::Rc<TriIndex> {
    if let Some(i) = lua.app_data_ref::<QueryCache>().and_then(|c| c.terrain_index.get(&key).cloned()) {
        return i;
    }
    let i = std::rc::Rc::new(TriIndex::build(tris));
    if let Some(mut c) = lua.app_data_mut::<QueryCache>() {
        c.terrain_index.insert(key, i.clone());
    }
    i
}

/// Terrain under a ray for the play mouse (Mouse.Hit / Mouse.Target):
/// (terrain, distance). Water counts, as for Raycast.
/// The part under the game's mouse: the shown side's Raycast (true shapes, so
/// a union's doorway or a mesh's gaps let the ray through; invisible parts
/// still catch it, as Roblox's Mouse does), passing `skip` (the player's own
/// character and held tool).
pub fn mouse_part_hit(lua: &Lua, origin: Vec3, dir: Vec3, len: f32, skip: &[Ref]) -> Option<(Ref, f32)> {
    let filter = Filter { list: skip.iter().copied().collect(), ..Filter::none() };
    cast(lua, origin, dir.normalize_or_zero() * len, &filter).ok().flatten().map(|(r, _, _, t)| (r, t))
}

pub fn mouse_terrain_hit(lua: &Lua, origin: Vec3, dir: Vec3, len: f32) -> Option<(Ref, f32)> {
    terrain_hit(lua, origin, dir, len, false).ok().flatten().map(|(r, _, _, t, _)| (r, t))
}

/// Ray / triangle (Möller–Trumbore), front faces only (a ray starting
/// inside terrain doesn't hit it on the way out, as in Roblox): distance and
/// barycentric weights of the corners.
fn ray_triangle(o: Vec3, d: Vec3, t: &[Vec3; 3]) -> Option<(f32, Vec3)> {
    let e1 = t[1] - t[0];
    let e2 = t[2] - t[0];
    let p = d.cross(e2);
    let det = e1.dot(p);
    // Counter-clockwise from outside: the ray must come at the front.
    if det < 1e-8 {
        return None;
    }
    let inv = 1.0 / det;
    let s = o - t[0];
    let u = s.dot(p) * inv;
    if !(0.0..=1.0).contains(&u) {
        return None;
    }
    let q = s.cross(e1);
    let w = d.dot(q) * inv;
    if w < 0.0 || u + w > 1.0 {
        return None;
    }
    let dist = e2.dot(q) * inv;
    (dist >= 0.0).then_some((dist, Vec3::new(1.0 - u - w, u, w)))
}

fn raycast_result(lua: &Lua, hit: (Ref, Vec3, Vec3, f32)) -> mlua::Result<Value> {
    let terrain_mat = lua
        .app_data_ref::<QueryCache>()
        .and_then(|c| c.terrain_material)
        .filter(|_| world(lua).is_ok_and(|w| w.d().class_of(hit.0).is_some_and(|k| k == "Terrain")));
    let (material, name) = {
        let w = world(lua)?;
        let d = w.d();
        let material = match terrain_mat {
            Some(m) => LEnumItem::from_name("Material", rustudio_core::terrain::material_name(m)),
            None => match d.prop(hit.0, "Material") {
                Some(Variant::Enum(e)) => LEnumItem::from_value("Material", e.to_u32()),
                _ => LEnumItem::from_name("Material", "Plastic"),
            },
        };
        let name = d.dom.get_by_ref(hit.0).map(|i| i.name.clone()).unwrap_or_default();
        (material, name)
    };
    let r = crate::ltypes::LRaycastResult {
        instance: inst_value(lua, hit.0)?,
        name,
        position: hit.1,
        normal: hit.2,
        distance: hit.3,
        material,
    };
    Ok(Value::UserData(lua.create_userdata(r)?))
}

/// Region3 → its world AABB.
fn region_box(v: &Value) -> Option<(Vec3, Vec3)> {
    let cf = ud_field::<LCFrame>(v, "CFrame")?;
    let size = ud_field::<LVector3>(v, "Size")?;
    let h = size.0 * 0.5;
    Some((cf.0.pos - h, cf.0.pos + h))
}

// -------------------------------------------------------------------- tags

fn tags_of(lua: &Lua, r: Ref) -> mlua::Result<Vec<String>> {
    Ok(
        match world(lua)?
            .d()
            .dom
            .get_by_ref(r)
            .and_then(|i| i.properties.get(&ustr("Tags")).cloned())
        {
            Some(Variant::Tags(t)) => t.iter().map(|s| s.to_string()).collect(),
            Some(Variant::BinaryString(b)) => Tags::decode(b.as_ref())
                .map(|t| t.iter().map(|s| s.to_string()).collect())
                .unwrap_or_default(),
            _ => Vec::new(),
        },
    )
}

fn set_tags(lua: &Lua, r: Ref, tags: Vec<String>) -> mlua::Result<()> {
    let mut w = world(lua)?;
    let mut old = None;
    if let Some(i) = w.dm().dom.get_by_ref_mut(r) {
        old = i.properties
            .insert(ustr("Tags"), Variant::Tags(Tags::from(tags)));
    }
    // (A plugin's tag edit is in the change history: Studio undoes it.)
    crate::runtime::note_script_prop(&mut w, r, ustr("Tags"), old);
    w.dm().mark_prop(r, ustr("Tags"));
    Ok(())
}

fn in_data_model(lua: &Lua, r: Ref) -> mlua::Result<bool> {
    let w = world(lua)?;
    let d = w.d();
    let root = d.dom.root_ref();
    Ok(r == root || d.is_ancestor(root, r))
}

fn add_tag(lua: &Lua, r: Ref, tag: &str) -> mlua::Result<()> {
    let mut tags = tags_of(lua, r)?;
    if tags.iter().any(|t| t == tag) {
        return Ok(());
    }
    tags.push(tag.to_string());
    set_tags(lua, r, tags)?;
    // InstanceAdded only fires for instances in the DataModel; one tagged
    // while out of it fires when it is parented in (set_parent).
    if !in_data_model(lua, r)? {
        return Ok(());
    }
    let cs = get_service(lua, "CollectionService", true)?;
    if let Some(cs) = cs {
        let v = inst_value(lua, r)?;
        world(lua)?.fire(cs, &format!("TagAdded:{tag}"), vec![v]);
    }
    Ok(())
}

fn remove_tag(lua: &Lua, r: Ref, tag: &str) -> mlua::Result<()> {
    let tags = tags_of(lua, r)?;
    if !tags.iter().any(|t| t == tag) {
        return Ok(());
    }
    set_tags(lua, r, tags.into_iter().filter(|t| t != tag).collect())?;
    if !in_data_model(lua, r)? {
        return Ok(());
    }
    if let Some(cs) = get_service(lua, "CollectionService", true)? {
        let v = inst_value(lua, r)?;
        world(lua)?.fire(cs, &format!("TagRemoved:{tag}"), vec![v]);
    }
    Ok(())
}

// --------------------------------------------------------------------- json

fn to_json(v: &Value, depth: usize) -> mlua::Result<serde_json::Value> {
    if depth > 64 {
        return Err(rt_err("Can't convert to JSON: nesting too deep"));
    }
    Ok(match v {
        Value::Nil => serde_json::Value::Null,
        Value::Boolean(b) => serde_json::Value::Bool(*b),
        Value::Integer(i) => serde_json::json!(*i),
        Value::Number(n) => {
            if n.fract() == 0.0 && n.abs() < 1e15 {
                serde_json::json!(*n as i64)
            } else {
                serde_json::json!(*n)
            }
        }
        Value::String(s) => serde_json::Value::String(s.to_str()?.to_string()),
        Value::Table(t) => {
            let n = t.raw_len();
            if n > 0 {
                let mut arr = Vec::with_capacity(n);
                for i in 1..=n {
                    arr.push(to_json(&t.raw_get::<Value>(i)?, depth + 1)?);
                }
                serde_json::Value::Array(arr)
            } else {
                let mut map = serde_json::Map::new();
                for pair in t.pairs::<Value, Value>() {
                    let (k, v) = pair?;
                    let key = match &k {
                        Value::String(s) => s.to_str()?.to_string(),
                        Value::Number(n) => n.to_string(),
                        Value::Integer(i) => i.to_string(),
                        _ => continue,
                    };
                    map.insert(key, to_json(&v, depth + 1)?);
                }
                // An empty table is "[]" in Roblox's JSONEncode.
                if map.is_empty() {
                    serde_json::Value::Array(Vec::new())
                } else {
                    serde_json::Value::Object(map)
                }
            }
        }
        _ => {
            return Err(rt_err(format!(
                "Can't convert to JSON: {} is not supported",
                v.type_name()
            )))
        }
    })
}

pub(crate) fn from_json(lua: &Lua, v: &serde_json::Value) -> mlua::Result<Value> {
    Ok(match v {
        serde_json::Value::Null => Value::Nil,
        serde_json::Value::Bool(b) => Value::Boolean(*b),
        serde_json::Value::Number(n) => Value::Number(n.as_f64().unwrap_or(0.0)),
        serde_json::Value::String(s) => Value::String(lua.create_string(s)?),
        serde_json::Value::Array(a) => {
            let t = lua.create_table()?;
            for (i, x) in a.iter().enumerate() {
                t.raw_set(i + 1, from_json(lua, x)?)?;
            }
            Value::Table(t)
        }
        serde_json::Value::Object(m) => {
            let t = lua.create_table()?;
            for (k, x) in m {
                t.raw_set(k.as_str(), from_json(lua, x)?)?;
            }
            Value::Table(t)
        }
    })
}

fn guid() -> String {
    use std::hash::{BuildHasher, Hasher};
    let mut bytes = [0u8; 16];
    for (i, chunk) in bytes.chunks_mut(8).enumerate() {
        let mut h = std::collections::hash_map::RandomState::new().build_hasher();
        h.write_usize(i);
        h.write_u128(
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0),
        );
        chunk.copy_from_slice(&h.finish().to_le_bytes());
    }
    bytes[6] = (bytes[6] & 0x0f) | 0x40;
    bytes[8] = (bytes[8] & 0x3f) | 0x80;
    let hex: String = bytes.iter().map(|b| format!("{b:02X}")).collect();
    format!(
        "{{{}-{}-{}-{}-{}}}",
        &hex[0..8],
        &hex[8..12],
        &hex[12..16],
        &hex[16..20],
        &hex[20..32]
    )
}

// ------------------------------------------------------------------ helpers

fn joint_class(class: &str) -> bool {
    matches!(
        class,
        "Weld"
            | "ManualWeld"
            | "Snap"
            | "Glue"
            | "ManualGlue"
            | "Motor"
            | "Motor6D"
            | "WeldConstraint"
            | "VelocityMotor"
            | "Rotate"
            | "RotateP"
            | "RotateV"
    )
}

/// BreakJoints: destroys rigid joints inside `r` and those touching its parts.
fn break_joints(lua: &Lua, r: Ref) -> mlua::Result<()> {
    let joints: Vec<Ref> = {
        let w = world(lua)?;
        let d = w.d();
        let parts: HashSet<Ref> = d
            .dom
            .descendants_of(r)
            .filter(|i| doc::is_a(&i.class, "BasePart"))
            .map(|i| i.referent())
            .collect();
        let mut js: Vec<Ref> = d
            .dom
            .descendants_of(r)
            .filter(|i| joint_class(&i.class))
            .map(|i| i.referent())
            .collect();
        // Joints elsewhere that hold one of these parts.
        if let Some(ws) = d.workspace() {
            for i in d.dom.descendants_of(ws) {
                if joint_class(&i.class) && !js.contains(&i.referent()) {
                    let touches = ["Part0", "Part1"].iter().any(|p| matches!(i.properties.get(&ustr(p)), Some(Variant::Ref(x)) if parts.contains(x)));
                    if touches {
                        js.push(i.referent());
                    }
                }
            }
        }
        js
    };
    for j in joints {
        destroy_inst(lua, j)?;
    }
    Ok(())
}

fn model_bounds(lua: &Lua, r: Ref) -> mlua::Result<(Vec3, Vec3)> {
    let w = world(lua)?;
    let d = w.d();
    let mut lo = Vec3::splat(f32::INFINITY);
    let mut hi = Vec3::splat(f32::NEG_INFINITY);
    for i in d.dom.descendants_of(r) {
        if !doc::is_a(&i.class, "BasePart") {
            continue;
        }
        let Some(xf) = rustudio_render::scene::xform_of(d, i.referent()) else {
            continue;
        };
        let size = rustudio_render::scene::vec3_of(d.prop(i.referent(), "Size")).unwrap_or(Vec3::ONE);
        let xf = geometry_box(d, i.referent(), &i.class, xf, size);
        let (a, b) = part_aabb(&QPart {
            r: i.referent(),
            xf,
            size,
            collide: true,
            query: true,
            mesh: false,
            shape: 0,
            cvx: 0,
            initial: Vec3::ONE,
        });
        lo = lo.min(a);
        hi = hi.max(b);
    }
    if !lo.is_finite() {
        return Ok((Vec3::ZERO, Vec3::ZERO));
    }
    Ok((lo, hi))
}

fn character_root(d: &Document, player: Ref) -> Option<Vec3> {
    let ch = match d.prop(player, "Character") {
        Some(Variant::Ref(c)) if c.is_some() => c,
        _ => return None,
    };
    let root = d
        .find_named(ch, "HumanoidRootPart")
        .or_else(|| d.find_named(ch, "Torso"))?;
    rustudio_render::scene::xform_of(d, root).map(|x| x.pos)
}

fn sound_key_of(r: Ref) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    r.hash(&mut h);
    h.finish() % 1_000_000_000
}

/// A parentless instance of any class (Mouse, Tween, AnimationTrack…).
pub fn create_any(lua: &Lua, class: &str) -> mlua::Result<Ref> {
    let mut w = world(lua)?;
    let r=w.dm().dom.insert(Ref::none(), InstanceBuilder::new(class).with_name(class));
    drop(w);
    super::domains::assets_cloud::scene::track_instance(lua,r)?;
    Ok(r)
}

/// Sound:Play / Stop / Pause / Resume.
pub fn sound(lua: &Lua, r: Ref, cmd: SoundCmd) -> mlua::Result<()> {
    sound_ev(lua, r, cmd, true)
}

/// A Sound's TimePosition as scripts read it (the playback clock while it
/// plays, else what Play / Stop / the end or a script left).
fn sound_time(w: &World, r: Ref) -> f64 {
    w.input
        .sound_pos
        .get(&r)
        .copied()
        .unwrap_or_else(|| sound_set_time(w, r))
}

/// The TimePosition a script set (stored), where the next Play() starts.
fn sound_set_time(w: &World, r: Ref) -> f64 {
    match w.d().prop(r, "TimePosition") {
        Some(Variant::Float64(v)) => v,
        Some(Variant::Float32(v)) => v as f64,
        _ => 0.0,
    }
    .max(0.0)
}

/// Sound playback requests. `events` is false for Sound.Playing writes,
/// which pause / go on without Played, Paused or Resumed (Studio).
/// Studio's TimePosition: Play() starts at the TimePosition a script set
/// since the last start, else 0 (also after Pause or the end); Stop()
/// puts it to 0; Pause keeps it. Changed fires for Playing and for a
/// TimePosition that moved.
pub fn sound_ev(lua: &Lua, r: Ref, cmd: SoundCmd, events: bool) -> mlua::Result<()> {
    if cmd == SoundCmd::PlayLocal {
        // SoundService:PlayLocalSound: a copy plays on this client only;
        // the Sound itself is untouched (Playing stays false, no events).
        world(lua)?.requests.sounds.push((r, cmd));
        return Ok(());
    }
    let playing = matches!(cmd, SoundCmd::Play | SoundCmd::Resume);
    let moved = {
        let mut w = world(lua)?;
        let old = sound_time(&w, r);
        let new = match cmd {
            SoundCmd::Play => Some(sound_set_time(&w, r)),
            SoundCmd::Stop => {
                if let Some(i) = w.dm().dom.get_by_ref_mut(r) {
                    i.properties.insert(ustr("TimePosition"), Variant::Float64(0.0));
                }
                Some(0.0)
            }
            _ => None,
        };
        if let Some(t) = new {
            w.input.sound_pos.insert(r, t);
        }
        w.requests.sounds.push((r, cmd));
        if playing {
            w.input.sound_playing.insert(r);
        } else {
            w.input.sound_playing.remove(&r);
        }
        if cmd == SoundCmd::Pause {
            w.input.sound_paused.insert(r);
        } else {
            w.input.sound_paused.remove(&r);
        }
        new.is_some_and(|t| t != old)
    };
    if world(lua)?.d().exists(r) {
        store_prop(lua, r, "Playing", Variant::Bool(playing))?;
    }
    if moved {
        let mut w = world(lua)?;
        w.record(RepOp::Prop(r, "TimePosition".into()));
        let name = Value::String(lua.create_string("TimePosition")?);
        w.fire(r, "Changed", vec![name]);
        w.fire(r, "Changed:TimePosition", vec![]);
    }
    if !events {
        return Ok(());
    }
    let ev = match cmd {
        SoundCmd::Play => "Played",
        SoundCmd::Stop => "Stopped",
        SoundCmd::Pause => "Paused",
        SoundCmd::Resume | SoundCmd::PlayLocal => "Resumed",
    };
    let sid = match world(lua)?.d().prop(r, "SoundId") {
        Some(Variant::ContentId(c)) => c.as_str().to_string(),
        Some(Variant::Content(c)) => c.as_uri().unwrap_or("").to_string(),
        _ => String::new(),
    };
    let s = Value::String(lua.create_string(&sid)?);
    world(lua)?.fire(r, ev, vec![s]);
    Ok(())
}

/// InsertService:LoadAsset: a Model holding the asset's instances, from the
/// local asset store (the offline Studio's ~/.local/share/rustudio/assets) or the asset cache.
fn load_asset(lua: &Lua, id: u64) -> mlua::Result<Ref> {
    let path = rustudio_core::assets::local_asset_path(id);
    if let Some(bytes) = path
        .as_ref()
        .and_then(|p| std::fs::read(p).ok())
        .filter(|b| rustudio_core::fbx::is_fbx(b))
    {
        return load_fbx(lua, id, &bytes);
    }
    // Signed in, a model not on disk is downloaded (LoadAsset yields in
    // Roblox; here it waits for the download), once per id per run.
    // A session check still running at Play start is waited for (a server
    // script's LoadAsset in its first frame), then the model is downloaded
    // with the session; Roblox's refusal is kept for the message.
    static FAILED: std::sync::Mutex<Option<HashMap<u64, String>>> = std::sync::Mutex::new(None);
    let mut why = String::from("signed out");
    let mut fetched = || -> Option<Vec<u8>> {
        if !rustudio_core::assets::wait_for_session(std::time::Duration::from_secs(20)) {
            return None;
        }
        let mut f = FAILED.lock().ok()?;
        let map = f.get_or_insert_with(Default::default);
        if let Some(e) = map.get(&id) {
            why = e.clone();
            return None;
        }
        match rustudio_core::assets::remote_cached(id) {
            Ok(b) => Some((*b).clone()),
            Err(e) => {
                why = e.clone();
                map.insert(id, e);
                None
            }
        }
    };
    // The local store / upload ledger may hold an upload's *source* (a
    // decal's .png): only a model counts, else Roblox's own asset (for a
    // decal, the Model holding its Decal) is fetched.
    let is_model = |b: &Vec<u8>| b.starts_with(b"<roblox") || b.starts_with(b"<?xml");
    let bytes = path
        .and_then(|p| std::fs::read(p).ok())
        .filter(is_model)
        .or_else(|| std::fs::read(rustudio_core::assets::cache_dir().join(format!("assets/{id}_vlatest.bin"))).ok().filter(is_model))
        .or_else(|| crate::runtime::outside_budget(lua, &mut fetched))
        .filter(|b| {
            let ok = is_model(b);
            if !ok {
                why = "Roblox's copy isn't a model".into();
            }
            ok
        })
        .ok_or_else(|| rt_err(format!("Asset {id} isn't available: not in the local asset store, upload ledger or cache, and Roblox refused it to this session ({why})")))?;
    let mut tmp = Document::decode(&bytes)
        .map_err(|e| rt_err(format!("Asset {id} couldn't be read: {e}")))?;
    let mut w = world(lua)?;
    let model = w.dm().dom.insert(
        Ref::none(),
        InstanceBuilder::new("Model").with_name("Model"),
    );
    for c in tmp.root().children().to_vec() {
        tmp.transfer(c, &mut w.dm().dom, model);
    }
    Ok(model)
}

/// Puts a HumanoidDescription's accessories on a character, as Studio's
/// CreateHumanoidModelFromDescription / ApplyDescription do (read from a
/// running Studio with a jacket, pants, shoes and a hat, Order and
/// Puffiness set, checks/mesh-deform/AUDIT.md (d)): each accessory is the
/// asset's own Accessory, AccessoryType set from the entry; a layered one's
/// WrapLayer takes the entry's Order and Puffiness (1 when the entry has
/// none); the Handle carries an OriginalSize Vector3Value (a rigid one also
/// OriginalPosition) and is held by a RigidConstraint named
/// AccessoryRigidConstraint from its attachment to the body's of the same
/// name. An accessory whose asset can't be loaded is left off with a warning.
pub fn dress_from_description(lua: &Lua, model: Ref, desc: Ref) -> mlua::Result<()> {
    use rbx_dom_weak::types::{Enum as DEnum, Vector3 as DV3};
    const TYPES: &[&str] = &[
        "Unknown", "Hat", "Hair", "Face", "Neck", "Shoulder", "Front", "Back", "Waist", "TShirt", "Shirt", "Pants",
        "Jacket", "Sweater", "Shorts", "LeftShoe", "RightShoe", "DressSkirt", "Eyebrow", "Eyelash",
    ];
    let list = crate::runtime::domains::avatar::desc::accessories(lua, desc)?;
    for a in list {
        if a.asset_id <= 0 {
            continue;
        }
        // Studio's loader asks for the MeshPart form of the accessory.
        let meshpart = crate::runtime::outside_budget(lua, || crate::avatar::meshpart_accessory(a.asset_id as u64));
        let holder = match meshpart.and_then(|b| Document::decode(&b).ok()) {
            Some(mut tmp) => {
                let mut w = world(lua)?;
                let model = w.dm().dom.insert(Ref::none(), InstanceBuilder::new("Model").with_name("Model"));
                for c in tmp.root().children().to_vec() {
                    tmp.transfer(c, &mut w.dm().dom, model);
                }
                model
            }
            None => match load_asset(lua, a.asset_id as u64) {
                Ok(m) => m,
                Err(e) => {
                    world(lua)?.output.push((Level::Warn, format!("{e}")));
                    continue;
                }
            },
        };
        let mut w = world(lua)?;
        let d = w.dm();
        let accs: Vec<Ref> = d
            .dom
            .get_by_ref(holder)
            .map(|h| h.children().to_vec())
            .unwrap_or_default()
            .into_iter()
            .filter(|c| d.dom.get_by_ref(*c).is_some_and(|i| rustudio_core::doc::is_a(&i.class, "Accoutrement")))
            .collect();
        for acc in accs {
            d.dom.transfer_within(acc, model);
            if let (Some(i), Some(t)) = (d.dom.get_by_ref_mut(acc), TYPES.iter().position(|t| *t == a.ty)) {
                i.properties.insert(ustr("AccessoryType"), Variant::Enum(DEnum::from_u32(t as u32)));
            }
            let Some(handle) = d.find_named(acc, "Handle") else { continue };
            if a.layered {
                let wl = d.dom.get_by_ref(handle).and_then(|h| {
                    h.children().iter().copied().find(|c| d.dom.get_by_ref(*c).is_some_and(|i| i.class == "WrapLayer"))
                });
                if let Some(i) = wl.and_then(|wl| d.dom.get_by_ref_mut(wl)) {
                    if let Some(o) = a.order {
                        i.properties.insert(ustr("Order"), Variant::Int32(o as i32));
                    }
                    i.properties.insert(ustr("Puffiness"), Variant::Float32(a.puffiness.unwrap_or(1.0).clamp(-1.0, 1.0)));
                }
            }
            // OriginalPosition: the handle attachment's own position.
            let att_pos = d.dom.get_by_ref(handle).and_then(|h| {
                h.children().iter().find_map(|c| {
                    let i = d.dom.get_by_ref(*c)?;
                    match (i.class.as_str(), i.properties.get(&ustr("CFrame"))) {
                        ("Attachment", Some(Variant::CFrame(c))) => Some(c.position),
                        _ => None,
                    }
                })
            });
            let (size, pos) = (d.dom.get_by_ref(handle).and_then(|h| h.properties.get(&ustr("Size")).cloned()), att_pos);
            // (ApplyDescription, avatar_apply.luau, finds the accessories it put on by these)
            if let Some(i) = d.dom.get_by_ref_mut(acc) {
                let mut attrs = match i.properties.get(&ustr("Attributes")) {
                    Some(Variant::Attributes(a)) => a.clone(),
                    _ => rbx_dom_weak::types::Attributes::new(),
                };
                attrs.insert("RustudioAssetId".into(), Variant::Float64(a.asset_id as f64));
                if a.layered {
                    attrs.insert("RustudioLayered".into(), Variant::Bool(true));
                }
                i.properties.insert(ustr("Attributes"), Variant::Attributes(attrs));
            }
            if let Some(i) = d.dom.get_by_ref_mut(handle) {
                i.properties.insert(ustr("Anchored"), Variant::Bool(false));
                i.properties.insert(ustr("CanCollide"), Variant::Bool(false));
            }
            if let Some(Variant::Vector3(v)) = size {
                d.dom.insert(handle, InstanceBuilder::new("Vector3Value").with_name("OriginalSize").with_property("Value", Variant::Vector3(v)));
            }
            if !a.layered {
                if let Some(p) = pos {
                    d.dom.insert(handle, InstanceBuilder::new("Vector3Value").with_name("OriginalPosition").with_property("Value", Variant::Vector3(DV3::new(p.x, p.y, p.z))));
                }
            }
            crate::avatar::hold_accessory(&mut d.dom, model, acc);
            d.mark_tree(acc);
        }
        d.dom.destroy(holder);
    }
    // The Humanoid keeps a copy of the description, as in Studio.
    let mut w = world(lua)?;
    let d = w.dm();
    if let Some(hum) = d.dom.get_by_ref(model).and_then(|m| {
        m.children().iter().copied().find(|c| d.dom.get_by_ref(*c).is_some_and(|i| i.class == "Humanoid"))
    }) {
        let copy = d.dom.clone_within(desc);
        d.dom.transfer_within(copy, hum);
        d.mark_tree(copy);
    }
    Ok(())
}

/// An FBX upload, imported the way Roblox imports FBX models (see fbx.rs):
/// a Model of MeshParts (empty nodes become Models), each re-centred on its
/// own bounds. Meshes and embedded textures get `rbxtemp://rustudio/fbx/…`
/// ids, so every copy of the parts (clones, the client's) draws them.
pub fn load_fbx(lua: &Lua, id: u64, bytes: &[u8]) -> mlua::Result<Ref> {
    use rbx_dom_weak::types::{CFrame, Color3, Matrix3};
    // Parse once per asset; later loads reuse the meshes.
    static IMPORTED: std::sync::Mutex<
        Option<HashMap<u64, std::sync::Arc<Vec<rustudio_core::fbx::ImportNode>>>>,
    > = std::sync::Mutex::new(None);
    let nodes = {
        let mut g = IMPORTED.lock().map_err(|_| rt_err("FBX import busy"))?;
        let map = g.get_or_insert_with(HashMap::new);
        match map.get(&id) {
            Some(n) => n.clone(),
            None => {
                let n =
                    std::sync::Arc::new(rustudio_core::fbx::import(bytes).map_err(|e| {
                        rt_err(format!("Asset {id} (FBX) couldn't be imported: {e}"))
                    })?);
                for (i, node) in n.iter().enumerate() {
                    if let Some(g) = &node.mesh {
                        rustudio_core::assets::register_generated(
                            &format!("rbxtemp://rustudio/fbx/{id}/{i}"),
                            rustudio_core::fbx::mesh_v2(g),
                        );
                    }
                    if let Some((_, tex)) = &node.texture {
                        rustudio_core::assets::register_generated(
                            &format!("rbxtemp://rustudio/fbx/{id}/{i}.tex"),
                            tex.clone(),
                        );
                    }
                }
                map.insert(id, n.clone());
                n
            }
        }
    };
    let mut w = world(lua)?;
    let dom = &mut w.dm().dom;
    let model = dom.insert(
        Ref::none(),
        InstanceBuilder::new("Model").with_name("Model"),
    );
    let mut made: Vec<Ref> = Vec::with_capacity(nodes.len());
    for (i, n) in nodes.iter().enumerate() {
        let parent = n.parent.and_then(|p| made.get(p).copied()).unwrap_or(model);
        let r = match &n.mesh {
            Some(g) => {
                let (mut lo, mut hi) = (Vec3::splat(f32::MAX), Vec3::splat(f32::MIN));
                for p in &g.positions {
                    lo = lo.min(*p);
                    hi = hi.max(*p);
                }
                let size = (hi - lo).max(Vec3::splat(0.001));
                let centre = (hi + lo) * 0.5;
                let color = n
                    .color
                    .unwrap_or([163.0 / 255.0, 162.0 / 255.0, 165.0 / 255.0]);
                let mut b = InstanceBuilder::new("MeshPart")
                    .with_name(n.name.clone())
                    .with_property(
                        "MeshId",
                        Variant::ContentId(format!("rbxtemp://rustudio/fbx/{id}/{i}").into()),
                    )
                    .with_property("Size", Vector3::new(size.x, size.y, size.z))
                    .with_property("InitialSize", Vector3::new(size.x, size.y, size.z))
                    .with_property("MeshSize", Vector3::new(size.x, size.y, size.z))
                    .with_property(
                        "CFrame",
                        CFrame::new(
                            Vector3::new(centre.x, centre.y, centre.z),
                            Matrix3::identity(),
                        ),
                    )
                    .with_property("Color", Color3::new(color[0], color[1], color[2]))
                    .with_property("Anchored", true);
                if n.texture.is_some() {
                    b = b.with_property(
                        "TextureID",
                        Variant::ContentId(format!("rbxtemp://rustudio/fbx/{id}/{i}.tex").into()),
                    );
                }
                dom.insert(parent, b)
            }
            None => dom.insert(
                parent,
                InstanceBuilder::new("Model").with_name(n.name.clone()),
            ),
        };
        made.push(r);
    }
    Ok(model)
}

/// A GUI object's absolute position and size (simple UDim2 layout from the
/// screen down; UIListLayout and friends aren't applied).
/// A GuiBase2d's AbsolutePosition / AbsoluteSize / AbsoluteRotation.
#[derive(Clone, Copy, PartialEq, Debug)]
pub struct AbsBox {
    pub pos: [f32; 2],
    pub size: [f32; 2],
    pub rot: f32,
}

/// A GUI object's box as Studio reports it when a script reads it: Studio
/// lays the object out on the read, so a box read right after a script
/// moved, resized or reparented it (or one of its ancestors) in the same
/// frame is the new one. Here: the drawn box when nothing on its way up
/// changed since the layout was handed over, else placed from the nearest
/// unchanged drawn ancestor with the UDim2s (a game's 2D emitter
/// reads its host's box right after scripts move it; it got last frame's).
/// A listener connected to a GUI object never laid out yet (not drawn, its
/// box never read; Fusion / Roact connect OnChange "AbsoluteSize" as they
/// build): its box is still the zero one, so the first layout is a change it
/// hears (Studio: Argon's error box sizes itself from that change and shows
/// its three lines, checks/plugins/ref/corpus/screens/argon-load-popup.jpg).
/// A read (abs_now through a script) lays it out and replaces the zero box.
pub(crate) fn seed_unlaid(w: &mut World, r: Ref) {
    if !w.d().class_of(r).is_some_and(|c| doc::is_a(&c, "GuiObject")) || w.input.gui_layout.contains_key(&r) {
        return;
    }
    w.abs_seen.entry(r).or_insert(AbsBox { pos: [0.0; 2], size: [0.0; 2], rot: 0.0 });
}

/// `r` is (in) a LayerCollector: a ScreenGui, a widget, a SurfaceGui ….
fn layer_collector(w: &World, r: Ref) -> Option<Ref> {
    let d = w.d();
    let mut c = r;
    for _ in 0..64 {
        let i = d.dom.get_by_ref(c)?;
        if doc::is_a(&i.class, "LayerCollector") {
            return Some(c);
        }
        c = i.parent();
    }
    None
}

pub(crate) fn abs_now(w: &mut World, r: Ref) -> AbsBox {
    abs_scan(w);
    let w = &*w;
    let d = w.d();
    let mut chain: Vec<Ref> = Vec::with_capacity(16);
    let mut c = r;
    while let Some(i) = d.dom.get_by_ref(c) {
        chain.push(c);
        if doc::is_a(&i.class, "LayerCollector") || chain.len() > 64 {
            break;
        }
        c = i.parent();
    }
    if chain.is_empty() {
        let (pos, size) = gui_rect(w, r, 0);
        return AbsBox { pos, size, rot: 0.0 };
    }
    let dirty_top = if w.abs_all {
        Some(chain.len() - 1)
    } else {
        chain.iter().rposition(|x| w.abs_dirty.contains(x))
    };
    let drawn = |x: Ref| drawn_box(w, x);
    if dirty_top.is_none() {
        if let Some(b) = drawn(r) {
            return b;
        }
    }
    // Start from the nearest drawn ancestor above every change (or the root).
    let from = dirty_top.map_or(1, |k| k + 1);
    let top = chain.len() - 1;
    let (base_idx, mut cur) = match (from..chain.len()).find_map(|k| drawn(chain[k]).map(|b| (k, b))) {
        Some(x) => x,
        None => {
            let (pos, size) = gui_rect(w, chain[top], 0);
            (top, AbsBox { pos, size, rot: 0.0 })
        }
    };
    let scale_of = |x: Ref| -> f32 {
        d.dom
            .get_by_ref(x)
            .and_then(|i| i.children().iter().copied().find(|k| d.class_of(*k).is_some_and(|c| c == "UIScale")))
            .and_then(|s| rustudio_render::scene::f32_of(d.prop(s, "Scale")))
            .unwrap_or(1.0)
    };
    // UIScale above: offsets of the objects below grow with it.
    let mut scale: f32 = chain[base_idx..].iter().map(|x| scale_of(*x)).product();
    let mut cur_ref = chain[base_idx];
    for k in (0..base_idx).rev() {
        let x = chain[k];
        let Some(class) = d.class_of(x) else { break };
        if !doc::is_a(&class, "GuiObject") {
            // Folders (and the like) hold GUI objects without a box of their own.
            continue;
        }
        let child_of = |p: Ref, cls: &str| {
            d.dom
                .get_by_ref(p)
                .and_then(|i| i.children().iter().copied().find(|k| d.class_of(*k).is_some_and(|c| doc::is_a(&c, cls))))
        };
        // Laid out by a UIListLayout / grid / page layout, or sized by its
        // content: the drawn box is the best known.
        let auto = rustudio_render::scene::enum_u32(d.prop(x, "AutomaticSize")).unwrap_or(0) != 0;
        if child_of(cur_ref, "UIGridStyleLayout").is_some() || auto {
            if let Some(b) = drawn(x) {
                cur = b;
                cur_ref = x;
                scale *= scale_of(x);
                continue;
            }
        }
        let u2 = |name: &str| match d.prop(x, name) {
            Some(Variant::UDim2(u)) => [u.x.scale, u.x.offset as f32, u.y.scale, u.y.offset as f32],
            _ => [0.0; 4],
        };
        // The parent's content box (inside its UIPadding), unturned.
        let (mut cpos, mut csize) = (cur.pos, cur.size);
        if let Some(pad) = child_of(cur_ref, "UIPadding") {
            let u = |name: &str| match d.prop(pad, name) {
                Some(Variant::UDim(u)) => [u.scale, u.offset as f32],
                _ => [0.0, 0.0],
            };
            let (l, rr, t, b) = (u("PaddingLeft"), u("PaddingRight"), u("PaddingTop"), u("PaddingBottom"));
            let (left, topp) = (l[0] * csize[0] + l[1] * scale, t[0] * csize[1] + t[1] * scale);
            cpos = [cpos[0] + left, cpos[1] + topp];
            csize = [
                csize[0] - left - rr[0] * csize[0] - rr[1] * scale,
                csize[1] - topp - b[0] * csize[1] - b[1] * scale,
            ];
        }
        let s = u2("Size");
        let p = u2("Position");
        let mut size = [s[0] * csize[0] + s[1] * scale, s[2] * csize[1] + s[3] * scale];
        if auto {
            // Not laid out yet (a drawn box `continue`d above): sized by
            // its content now, by the walk's rules.
            let at = [p[0] * csize[0] + p[1] * scale, p[2] * csize[1] + p[3] * scale];
            if let Some(grown) = crate::gui::auto_grow_now(d, x, size, csize, at, scale) {
                size = grown;
            }
        }
        if let Some(c) = child_of(x, "UISizeConstraint") {
            let v = |n: &str, dflt: f32| match d.prop(c, n) {
                Some(Variant::Vector2(v)) => [v.x, v.y],
                _ => [dflt, dflt],
            };
            let (mn, mx) = (v("MinSize", 0.0), v("MaxSize", f32::INFINITY));
            size = [size[0].clamp(mn[0], mx[0].max(mn[0])), size[1].clamp(mn[1], mx[1].max(mn[1]))];
        }
        if let Some(c) = child_of(x, "UIAspectRatioConstraint") {
            let ratio = rustudio_render::scene::f32_of(d.prop(c, "AspectRatio")).unwrap_or(1.0).max(1e-3);
            if rustudio_render::scene::enum_u32(d.prop(c, "AspectType")) == Some(1) {
                if rustudio_render::scene::enum_u32(d.prop(c, "DominantAxis")) == Some(1) {
                    size[0] = size[1] * ratio;
                } else {
                    size[1] = size[0] / ratio;
                }
            } else if size[0] / size[1].max(1e-3) > ratio {
                size[0] = size[1] * ratio;
            } else {
                size[1] = size[0] / ratio;
            }
        }
        let own = scale_of(x);
        // (negative sizes are reported as they are, as in Studio)
        size = [size[0] * own, size[1] * own];
        let anchor = match d.prop(x, "AnchorPoint") {
            Some(Variant::Vector2(v)) => [v.x, v.y],
            _ => [0.0, 0.0],
        };
        // Its centre, unturned, then turned about the parent's centre.
        let cx = cpos[0] + p[0] * csize[0] + p[1] * scale - anchor[0] * size[0] + size[0] * 0.5;
        let cy = cpos[1] + p[2] * csize[1] + p[3] * scale - anchor[1] * size[1] + size[1] * 0.5;
        let (pcx, pcy) = (cur.pos[0] + cur.size[0] * 0.5, cur.pos[1] + cur.size[1] * 0.5);
        let (sn, cs) = cur.rot.to_radians().sin_cos();
        let (dx, dy) = (cx - pcx, cy - pcy);
        let (tx, ty) = (pcx + cs * dx - sn * dy, pcy + sn * dx + cs * dy);
        let rot = rustudio_render::scene::f32_of(d.prop(x, "Rotation")).unwrap_or(0.0);
        cur = AbsBox { pos: [tx - size[0] * 0.5, ty - size[1] * 0.5], size, rot: cur.rot + rot };
        cur_ref = x;
        scale *= own;
    }
    cur
}

/// Whether `r`'s drawn text may be out of date (its text or box changed
/// since the layout was handed over), for TextBounds.
fn text_dirty(w: &mut World, r: Ref) -> bool {
    abs_scan(w);
    if w.abs_all {
        return true;
    }
    let d = w.d();
    let mut c = r;
    for _ in 0..64 {
        if w.abs_dirty.contains(&c) {
            return true;
        }
        match d.dom.get_by_ref(c) {
            Some(i) if !doc::is_a(&i.class, "LayerCollector") => c = i.parent(),
            _ => break,
        }
    }
    false
}

/// Properties that change a GUI object's box, its children's, or its text.
fn layout_props() -> &'static rustc_hash::FxHashSet<rbx_dom_weak::Ustr> {
    static PROPS: std::sync::OnceLock<rustc_hash::FxHashSet<rbx_dom_weak::Ustr>> = std::sync::OnceLock::new();
    PROPS.get_or_init(|| {
        [
            "Size", "Position", "AnchorPoint", "Rotation", "SizeConstraint", "AutomaticSize", "Scale",
            "AspectRatio", "AspectType", "DominantAxis", "MinSize", "MaxSize", "PaddingLeft", "PaddingRight",
            "PaddingTop", "PaddingBottom", "IgnoreGuiInset", "ScreenInsets", "CanvasSize", "CanvasPosition",
            "Text", "TextSize", "Font", "FontFace", "TextScaled", "TextWrapped", "RichText", "LineHeight",
            "MaxTextSize", "MinTextSize", "Padding", "FillDirection", "HorizontalAlignment", "VerticalAlignment",
            "SortOrder", "CellSize", "CellPadding", "LayoutOrder", "Visible", "Enabled", "Name",
            "FillDirectionMaxCells", "StartCorner", "HorizontalFlex", "VerticalFlex",
            "ItemLineAlignment", "Wraps", "FlexMode", "GrowRatio", "ShrinkRatio", "AutomaticCanvasSize",
        ]
        .into_iter()
        .map(rbx_dom_weak::ustr)
        .collect()
    })
}

/// Non-GUI properties with overlapping names (such as Part.Size and
/// Attachment.Position) cannot change a screen layout.
fn mark_layout_roots(d: &Document, marks: &[doc::Mark], roots: &mut rustc_hash::FxHashSet<Ref>) {
    let props = layout_props();
    for mark in marks {
        let (r, tree) = match *mark {
            doc::Mark::Prop(r, name) if props.contains(&name) => (r, false),
            doc::Mark::Tree(r) => (r, true),
            _ => continue,
        };
        if tree {
            // A Folder or Model may contain a GUI, so a tree change is
            // checked against the listener's ancestry regardless of class.
            roots.insert(r);
        }
        let Some(instance) = d.dom.get_by_ref(r) else { continue };
        if doc::is_a(&instance.class, "UIComponent") {
            if instance.parent().is_some() {
                roots.insert(instance.parent());
            }
        } else if !tree && doc::is_a(&instance.class, "GuiBase2d") {
            roots.insert(r);
        }
    }
}

fn layout_ancestor_dirty(d: &Document, roots: &rustc_hash::FxHashSet<Ref>, mut r: Ref) -> bool {
    if roots.is_empty() {
        return false;
    }
    while let Some(instance) = d.dom.get_by_ref(r) {
        if roots.contains(&r) {
            return true;
        }
        r = instance.parent();
    }
    false
}

/// Reads the journal since the last look and notes what scripts changed
/// (see `World::abs_dirty`).
fn abs_scan(w: &mut World) {
    if w.abs_all {
        return;
    }
    let end = w.d().journal_end();
    if end == w.abs_scan {
        return;
    }
    let World { doc, abs_dirty, abs_scan, abs_all, .. } = w;
    let d = doc.as_ref().expect("checked by World::d");
    let Some(marks) = d.marks_since(*abs_scan) else {
        *abs_all = true;
        return;
    };
    mark_layout_roots(d, marks, abs_dirty);
    *abs_scan = end;
}

fn drawn_box(w: &World, r: Ref) -> Option<AbsBox> {
    w.input.gui_layout.get(&r).map(|b| AbsBox { pos: b.pos, size: b.size, rot: b.rot })
}

/// The inputs needed by the last AbsoluteContentSize calculation. The
/// child list detects removals as well as additions and moves. The drawn
/// boxes can change without a document mark, for example on window resize.
pub(crate) struct ContentDependencies {
    parent: Ref,
    children: Vec<Ref>,
    drawn: Vec<(Ref, AbsBox)>,
    widget_canvas: Option<(Ref, Option<[f32; 2]>)>,
}

impl ContentDependencies {
    fn dirty(
        &self,
        w: &World,
        parent: Ref,
        children: &[Ref],
        roots: &rustc_hash::FxHashSet<Ref>,
    ) -> bool {
        self.parent != parent
            || self.children != children
            || layout_ancestor_dirty(w.d(), roots, parent)
            || (!roots.is_empty() && children.iter().any(|r| roots.contains(r)))
            || self.widget_canvas.is_some_and(|(r, old)| old != drawn_box(w, r).map(|b| b.size))
            || ((!w.input.gui_layout.is_empty() || !self.drawn.is_empty())
                && !children.iter().filter_map(|&r| drawn_box(w, r).map(|b| (r, b)))
                    .eq(self.drawn.iter().copied()))
    }

    fn capture(w: &World, parent: Ref, children: &[Ref]) -> Self {
        // A dock widget can be resized independently of the main viewport.
        let widget_canvas = layer_collector(w, parent)
            .filter(|r| w.d().class_of(*r).is_some_and(|c| doc::is_a(&c, "PluginGui")))
            .map(|r| (r, drawn_box(w, r).map(|b| b.size)));
        Self {
            parent,
            children: children.to_vec(),
            drawn: children.iter().filter_map(|&r| drawn_box(w, r).map(|b| (r, b))).collect(),
            widget_canvas,
        }
    }
}

fn layout_parent_and_children(d: &Document, r: Ref) -> (Ref, &[Ref]) {
    let parent = d.dom.get_by_ref(r).map_or_else(Ref::none, |i| i.parent());
    (parent, d.dom.get_by_ref(parent).map_or(&[], |p| p.children()))
}

/// Fires Changed / GetPropertyChangedSignal for AbsolutePosition,
/// AbsoluteSize and AbsoluteRotation of every listened GuiBase2d whose box
/// moved since scripts last saw it, and for Camera.ViewportSize, after a new
/// GUI layout or view size. Roblox fires these as the screen resizes; layouts
/// that fit themselves to the screen (Place A's 720p HUD canvas: a UIScale set
/// from ScreenGui.AbsoluteSize) stayed at the size the window had when they
/// were made.
pub fn fire_layout_changes(lua: &Lua) -> mlua::Result<()> {
    let mut fires: Vec<(Ref, &'static str)> = Vec::new();
    {
        let mut w = world(lua)?;
        let view = w.input.view_size;
        let mut all = false;
        if view[0] > 0.0 && w.viewport_seen != Some(view) {
            let had = w.viewport_seen.replace(view).is_some();
            let d = w.d();
            let cam = d.workspace().and_then(|ws| match d.prop(ws, "CurrentCamera") {
                Some(Variant::Ref(c)) if d.exists(c) => Some(c),
                _ => None,
            });
            if let (true, Some(c)) = (had, cam) {
                fires.push((c, "ViewportSize"));
            }
            // Every box scaled from the screen may have moved.
            all = true;
        }
        let marks = w.dm().marks_for(doc::Reader::Layout);
        // The delivered drawing covers the journal up to this pass. Later
        // script reads still detect any mutations that happen after it.
        w.abs_scan = w.d().journal_end();
        w.abs_dirty.clear();
        w.abs_all = false;
        let mut roots = rustc_hash::FxHashSet::default();
        match marks {
            doc::Marks::All => all = true,
            doc::Marks::Some(marks) if !all && !(w.gui_listening.is_empty() && w.layout_listening.is_empty()) => {
                mark_layout_roots(w.d(), &marks, &mut roots);
            }
            doc::Marks::Some(_) => {}
        }

        let mut boxes = Vec::new();
        let mut laid = std::mem::take(&mut w.abs_laid);
        for &r in &w.gui_listening {
            if let Some(now) = drawn_box(&w, r) {
                laid.insert(r);
                if w.abs_seen.get(&r) != Some(&now) {
                    boxes.push((r, Some(now)));
                }
            } else {
                let was_laid = laid.remove(&r);
                if all || was_laid || w.listen_new.contains(&r) || layout_ancestor_dirty(w.d(), &roots, r) {
                    boxes.push((r, None));
                }
            }
        }
        w.listen_new.clear();
        w.abs_laid = laid;
        for (r, drawn) in boxes {
            // A seeded zero box waits until it is inside a LayerCollector.
            if w.abs_seen.get(&r).is_some_and(|b| b.size == [0.0; 2] && b.pos == [0.0; 2])
                && layer_collector(&w, r).is_none()
            {
                continue;
            }
            let now = drawn.unwrap_or_else(|| abs_now(&mut w, r));
            if let Some(old) = w.abs_seen.insert(r, now) {
                if old.pos != now.pos {
                    fires.push((r, "AbsolutePosition"));
                }
                if old.size != now.size {
                    fires.push((r, "AbsoluteSize"));
                }
                if old.rot != now.rot {
                    fires.push((r, "AbsoluteRotation"));
                }
            }
        }

        let layouts: Vec<Ref> = w.layout_listening.iter().copied().filter(|r| {
            let (parent, children) = layout_parent_and_children(w.d(), *r);
            all || roots.contains(r) || w.layout_content.get(r)
                .is_none_or(|deps| deps.dirty(&w, parent, children, &roots))
        }).collect();
        for r in layouts {
            let (parent, children) = layout_parent_and_children(w.d(), r);
            let deps = ContentDependencies::capture(&w, parent, children);
            let size = content_size(&w, r);
            w.layout_content.insert(r, deps);
            let now = AbsBox { pos: [0.0; 2], size, rot: 0.0 };
            // The first layout arrives from a zero-size state.
            let old = w.abs_seen.insert(r, now).map_or([0.0; 2], |b| b.size);
            if old != size {
                fires.push((r, "AbsoluteContentSize"));
            }
        }
        // (nothing kept for instances gone or no longer listened to)
        if w.abs_seen.len() > w.listening.len() {
            let World { abs_seen, listening, .. } = &mut *w;
            abs_seen.retain(|r, _| listening.contains(r));
        }
        for (r, name) in &fires {
            let pname = Value::String(lua.create_string(name)?);
            w.fire(*r, "Changed", vec![pname]);
            w.fire(*r, &format!("Changed:{name}"), vec![]);
        }
    }
    Ok(())
}

/// UIListLayout / UIGridLayout / UITableLayout.AbsoluteContentSize: the
/// extent of the visible GuiObjects it lays out (its siblings), as drawn
/// last frame (before that, from their UDim2s).
pub(crate) fn content_size(w: &World, r: Ref) -> [f32; 2] {
    let d = w.d();
    let (_, children) = layout_parent_and_children(d, r);
    let (mut mn, mut mx) = ([f32::MAX; 2], [f32::MIN; 2]);
    let mut any = false;
    for &k in children {
        if !d.class_of(k).is_some_and(|c| doc::is_a(&c, "GuiObject"))
            || matches!(d.prop(k, "Visible"), Some(Variant::Bool(false)))
        {
            continue;
        }
        any = true;
        let (pos, size) = match w.input.gui_layout.get(&k) {
            Some(b) => (b.pos, b.size),
            None => gui_rect(w, k, 0),
        };
        for a in 0..2 {
            mn[a] = mn[a].min(pos[a]);
            mx[a] = mx[a].max(pos[a] + size[a]);
        }
    }
    if any { [mx[0] - mn[0], mx[1] - mn[1]] } else { [0.0; 2] }
}

fn gui_rect(w: &World, r: Ref, depth: usize) -> ([f32; 2], [f32; 2]) {
    let d = w.d();
    let screen = if w.input.view_size[0] > 0.0 {
        w.input.view_size
    } else {
        [1265.0, 836.0]
    };
    let Some(i) = d.dom.get_by_ref(r) else {
        return ([0.0; 2], screen);
    };
    if depth <= 64 && !doc::is_a(&i.class, "GuiBase2d") && !i.parent().is_none() && i.class != "DataModel" && d.dom.get_by_ref(i.parent()).is_some_and(|p| doc::is_a(&p.class, "GuiBase2d") || p.class == "Folder") {
        // A Folder (or any non-GUI holder) between GUI objects has no box:
        // its children are laid out in the GUI object above it.
        return gui_rect(w, i.parent(), depth + 1);
    }
    if depth > 64 || !doc::is_a(&i.class, "GuiObject") {
        // The GUI's own canvas: a ScreenGui below the top bar inset (unless it
        // ignores it), a SurfaceGui its CanvasSize.
        // (no top bar in the edit DataModel: a ScreenGui there, in CoreGui
        // or StarterGui, is the whole view: )
        let inset = if w.mode == crate::runtime::RunMode::Edit { 0.0 } else { mech::GUI_TOP_INSET };
        // A plugin's dock widget: its panel's content, 0 x 0 while closed.
        if doc::is_a(&i.class, "PluginGui") {
            return ([0.0, 0.0], w.input.gui_layout.get(&r).filter(|_| matches!(d.prop(r, "Enabled"), Some(Variant::Bool(true)))).map_or([0.0, 0.0], |b| b.size));
        }
        return match i.class.as_str() {
            "ScreenGui" if matches!(rustudio_render::scene::enum_u32(d.prop(r, "ScreenInsets")), Some(0 | 1)) => {
                ([0.0, -inset], screen)
            }
            "ScreenGui" => ([0.0, 0.0], [screen[0], screen[1] - inset]),
            "SurfaceGui" => match d.prop(r, "CanvasSize") {
                Some(Variant::Vector2(v)) => ([0.0, 0.0], [v.x, v.y]),
                _ => ([0.0, 0.0], [800.0, 600.0]),
            },
            // A BillboardGui not drawn yet: its canvas is its Size in pixels
            // (the stud-scaled part needs the camera; the screen stands in).
            "BillboardGui" => match d.prop(r, "Size") {
                Some(Variant::UDim2(u)) if u.x.scale == 0.0 && u.y.scale == 0.0 => {
                    ([0.0, 0.0], [u.x.offset as f32, u.y.offset as f32])
                }
                _ => ([0.0, 0.0], screen),
            },
            _ => ([0.0, 0.0], screen),
        };
    }
    let (mut ppos, mut psize) = gui_rect(w, i.parent(), depth + 1);
    // Children sit inside the parent's UIPadding (a ScreenGui's too).
    let pad = d.dom.get_by_ref(i.parent()).and_then(|p| {
        p.children().iter().copied().find(|c| d.class_of(*c).is_some_and(|k| k == "UIPadding"))
    });
    if let Some(pad) = pad {
        let u = |name: &str| match d.prop(pad, name) {
            Some(Variant::UDim(u)) => [u.scale, u.offset as f32],
            _ => [0.0, 0.0],
        };
        let (l, rr, t, b) = (u("PaddingLeft"), u("PaddingRight"), u("PaddingTop"), u("PaddingBottom"));
        let (left, top) = (l[0] * psize[0] + l[1], t[0] * psize[1] + t[1]);
        ppos = [ppos[0] + left, ppos[1] + top];
        psize = [
            psize[0] - left - rr[0] * psize[0] - rr[1],
            psize[1] - top - b[0] * psize[1] - b[1],
        ];
    }
    let udim2 = |name: &str| match d.prop(r, name) {
        Some(Variant::UDim2(u)) => [u.x.scale, u.x.offset as f32, u.y.scale, u.y.offset as f32],
        _ => [0.0; 4],
    };
    let s = udim2("Size");
    let p = udim2("Position");
    let mut size = [s[0] * psize[0] + s[1], s[2] * psize[1] + s[3]];
    if rustudio_render::scene::enum_u32(d.prop(r, "AutomaticSize")).unwrap_or(0) != 0 {
        let at = [p[0] * psize[0] + p[1], p[2] * psize[1] + p[3]];
        if let Some(grown) = crate::gui::auto_grow_now(d, r, size, psize, at, 1.0) {
            size = grown;
        }
    }
    let anchor = match d.prop(r, "AnchorPoint") {
        Some(Variant::Vector2(v)) => [v.x, v.y],
        _ => [0.0, 0.0],
    };
    let pos = [
        ppos[0] + p[0] * psize[0] + p[1] - anchor[0] * size[0],
        ppos[1] + p[2] * psize[1] + p[3] - anchor[1] * size[1],
    ];
    (pos, size)
}

/// Read-side overrides: Mouse, Sound playback state, GUI layout, camera viewport.
/// A stored value, else `default` (for engine-owned properties the
/// reflection database has no default for).
fn stored_or(
    lua: &Lua,
    r: Ref,
    key: &str,
    default: impl FnOnce(&Lua) -> mlua::Result<Value>,
) -> mlua::Result<Option<Value>> {
    let v = world(lua)?
        .d()
        .dom
        .get_by_ref(r)
        .and_then(|i| i.properties.get(&rbx_dom_weak::ustr(key)).cloned());
    match v {
        Some(v) => {
            let enum_name = match key {
                "MouseBehavior" => Some("MouseBehavior"),
                "FloorMaterial" => Some("Material"),
                "CreatorType" => Some("CreatorType"),
                _ => None,
            };
            Ok(Some(variant_to_lua(lua, &v, enum_name)?))
        }
        None => Ok(Some(default(lua)?)),
    }
}

fn enum_item(lua: &Lua, en: &str, item: &str) -> mlua::Result<Value> {
    match LEnumItem::from_name(en, item) {
        Some(i) => crate::ltypes::enum_value(lua, i),
        None => Ok(Value::Nil),
    }
}

/// Client device and engine state the default player scripts read: a
/// desktop with keyboard and mouse, no touch, gamepad or VR (Studio's Play).
fn device_prop(lua: &Lua, r: Ref, class: &str, key: &str) -> mlua::Result<Option<Value>> {
    let b = |v: bool| Ok(Some(Value::Boolean(v)));
    match (class, key) {
        (
            "UserInputService",
            "TouchEnabled"
            | "GamepadEnabled"
            | "VREnabled"
            | "AccelerometerEnabled"
            | "GyroscopeEnabled"
            | "OnScreenKeyboardVisible"
            | "ModalEnabled",
        ) => b(false),
        ("UserInputService", "KeyboardEnabled" | "MouseEnabled") => b(true),
        ("UserInputService", "PreferredInput") => {
            Ok(Some(enum_item(lua, "PreferredInput", "KeyboardAndMouse")?))
        }
        ("UserInputService", "MouseBehavior") => {
            stored_or(lua, r, key, |l| enum_item(l, "MouseBehavior", "Default"))
        }
        ("UserInputService", "MouseIconEnabled") => {
            stored_or(lua, r, key, |_| Ok(Value::Boolean(true)))
        }
        ("UserInputService", "MouseDeltaSensitivity") => {
            stored_or(lua, r, key, |_| Ok(Value::Number(1.0)))
        }
        ("UserInputService", "MouseIcon") => {
            stored_or(lua, r, key, |l| Ok(Value::String(l.create_string("")?)))
        }
        ("Lighting", "ClockTime") => Ok(Some(Value::Number(clock_hours(lua, r)? as f64))),
        ("GuiService", "IsModalDialog" | "IsWindows") => b(false),
        ("GuiService", "TouchControlsEnabled") => {
            stored_or(lua, r, key, |_| Ok(Value::Boolean(true)))
        }
        (
            "VRService",
            "VREnabled"
            | "ThirdPersonFollowCamEnabled"
            | "AvatarGestures"
            | "AutomaticScaling"
            | "FadeOutViewOnCollision",
        ) => b(false),
        ("VRService", "VRDeviceName") => Ok(Some(Value::String(lua.create_string("")?))),
        ("Camera", "NearPlaneZ") => Ok(Some(Value::Number(-0.1))),
        // Round 25: the cell box CopyRegion(MaxExtents) copies everything in,
        // and a TerrainRegion's size from its extents.
        ("Terrain", "MaxExtents") => {
            let v = |c: i16| crate::ltypes::LVector3int16([c; 3]);
            let r = crate::ltypes::LRegion3int16 { min: v(-32000), max: v(32000) };
            Ok(Some(Value::UserData(lua.create_userdata(r)?)))
        }
        ("Terrain" | "TerrainRegion", "IsSmooth") => b(true),
        // Saved, not scriptable: Studio says "ExtentsMin is not a valid
        // member of TerrainRegion" (round 26, region_probe.luau).
        ("TerrainRegion", "ExtentsMin" | "ExtentsMax") => {
            let full = world(lua)?.full_name(r);
            Err(rt_err(format!("{key} is not a valid member of TerrainRegion \"{full}\"")))
        }
        ("TerrainRegion", "SizeInCells") => {
            let w = world(lua)?;
            let ext = |k: &str| match w.d().prop(r, k) {
                Some(Variant::Vector3int16(v)) => Vec3::new(v.x as f32, v.y as f32, v.z as f32),
                _ => Vec3::ZERO,
            };
            // (Max inclusive; a TerrainRegion with no extents holds nothing)
            let has = w.d().prop(r, "ExtentsMax").is_some();
            let size = if has { (ext("ExtentsMax") - ext("ExtentsMin") + Vec3::ONE).max(Vec3::ZERO) } else { Vec3::ZERO };
            drop(w);
            Ok(Some(vec3_value(lua, size)?))
        }
        ("Camera", "HeadScale") => stored_or(lua, r, key, |_| Ok(Value::Number(1.0))),
        ("Camera", "HeadLocked") => stored_or(lua, r, key, |_| Ok(Value::Boolean(true))),
        ("Humanoid", "MoveDirection") => stored_or(lua, r, key, |l| {
            Ok(LVector3(Vec3::ZERO).into_value())
        }),
        // RootPart and the deprecated Torso share one getter in Studio
        // the rig's HumanoidRootPart, else
        // UpperTorso (R15) / Torso (R6). Old free-model NPCs test
        // `Humanoid.Torso ~= nil` to see they're alive (Place B's ZombieScript).
        ("Humanoid", "RootPart" | "Torso") => {
            let root = {
                let w = world(lua)?;
                let d = w.d();
                let parent = d
                    .dom
                    .get_by_ref(r)
                    .map(|i| i.parent())
                    .unwrap_or_else(Ref::none);
                // RigType: R6 0, R15 1.
                let r15 = rustudio_render::scene::enum_u32(d.prop(r, "RigType")).unwrap_or(0) != 0;
                let fallback = if r15 { mech::ROOT_PART_FALLBACK_R15 } else { mech::ROOT_PART_FALLBACK_R6 };
                let part_named = |name: &str| {
                    d.dom.get_by_ref(parent).and_then(|p| {
                        p.children().iter().copied().find(|c| {
                            d.dom
                                .get_by_ref(*c)
                                .is_some_and(|i| i.name == name && doc::is_a(&i.class, "BasePart"))
                        })
                    })
                };
                part_named("HumanoidRootPart").or_else(|| part_named(fallback))
            };
            Ok(Some(match root {
                Some(x) => inst_value(lua, x)?,
                None => Value::Nil,
            }))
        }
        (_, "Mass" | "AssemblyMass") if doc::is_a(class, "BasePart") => {
            // Plastic density × volume; a character's assembly is its whole
            // model (less Massless parts), other parts count alone.
            let w = world(lua)?;
            let d = w.d();
            let mass = |p: Ref| {
                let s = rustudio_render::scene::vec3_of(d.prop(p, "Size")).unwrap_or(Vec3::ONE);
                s.x * s.y * s.z * mech::PART_DENSITY
            };
            let mut m = mass(r);
            if key == "AssemblyMass" {
                let model = d
                    .dom
                    .get_by_ref(r)
                    .map(|i| i.parent())
                    .unwrap_or_else(Ref::none);
                let humanoid_model = d.dom.get_by_ref(model).is_some_and(|i| {
                    i.children()
                        .iter()
                        .any(|c| d.class_of(*c).is_some_and(|k| k == "Humanoid"))
                });
                if humanoid_model {
                    m = d
                        .dom
                        .descendants_of(model)
                        .filter(|i| {
                            doc::is_a(&i.class, "BasePart")
                                && !matches!(
                                    d.prop(i.referent(), "Massless"),
                                    Some(Variant::Bool(true))
                                )
                        })
                        .map(|i| mass(i.referent()))
                        .sum();
                }
            }
            Ok(Some(Value::Number(m as f64)))
        }
        // An unpublished place opened from a file, as in Studio: no place,
        // universe or creator ids (0) and version 0.
        ("DataModel", "PlaceId" | "GameId" | "CreatorId" | "PlaceVersion") => {
            stored_or(lua, r, key, |_| Ok(Value::Number(0.0)))
        }
        ("Humanoid", "CameraOffset") => stored_or(lua, r, key, |l| {
            Ok(LVector3(Vec3::ZERO).into_value())
        }),
        ("Humanoid", "SeatPart") => stored_or(lua, r, key, |_| Ok(Value::Nil)),
        ("Humanoid", "FloorMaterial") => {
            stored_or(lua, r, key, |l| enum_item(l, "Material", "Plastic"))
        }
        ("Humanoid", "EvaluateStateMachine") => {
            stored_or(lua, r, key, |_| Ok(Value::Boolean(true)))
        }
        _ => Ok(None),
    }
}

pub fn prop_get(lua: &Lua, r: Ref, class: &str, key: &str) -> mlua::Result<Option<Value>> {
    if class == "MeshPart" && matches!(key, "MeshContent" | "MeshId") {
        if let Some(em) = super::editable::linked_mesh(r) {
            return Ok(Some(if key == "MeshId" {
                Value::String(lua.create_string("")?)
            } else {
                super::editable::content_value(lua, &rbx_dom_weak::types::Content::from_referent(em))?
            }));
        }
    }
    if class.starts_with("Editable") {
        if let Some(v) = super::editable::prop_get(lua, r, class, key)? {
            return Ok(Some(v));
        }
    }
    if let Some(v) = device_prop(lua, r, class, key)? {
        return Ok(Some(v));
    }
    if matches!(
        key,
        "AbsoluteSize" | "AbsolutePosition" | "AbsoluteRotation"
    ) && doc::is_a(class, "GuiBase2d")
    {
        // As drawn last frame; before that (or when hidden), from the UDim2s.
        let b = {
            let mut w = world(lua)?;
            let b = abs_now(&mut w, r);
            // (what the script saw: a later layout that differs fires Changed)
            w.abs_seen.insert(r, b);
            b
        };
        return Ok(Some(match key {
            "AbsoluteSize" => Value::UserData(
                lua.create_userdata(LVector2(glam::Vec2::new(b.size[0], b.size[1])))?,
            ),
            "AbsolutePosition" => Value::UserData(
                lua.create_userdata(LVector2(glam::Vec2::new(b.pos[0], b.pos[1])))?,
            ),
            _ => Value::Number(b.rot as f64),
        }));
    }
    // UIListLayout / UIGridLayout / UITableLayout.AbsoluteContentSize: the
    // extent of the visible GuiObjects it lays out (its siblings), as drawn
    // last frame (before that, from their UDim2s).
    if key == "AbsoluteContentSize" && doc::is_a(class, "UIGridStyleLayout") {
        let size = {
            let mut w = world(lua)?;
            let size = content_size(&w, r);
            // (what the script saw: a later layout that differs fires Changed)
            w.abs_seen.insert(r, AbsBox { pos: [0.0; 2], size, rot: 0.0 });
            size
        };
        return Ok(Some(Value::UserData(lua.create_userdata(LVector2(glam::Vec2::new(size[0], size[1])))?)));
    }
    if key == "TextBounds" && matches!(class, "TextLabel" | "TextButton" | "TextBox") {
        let measured = {
            let mut w = world(lua)?;
            let drawn = w.input.gui_layout.get(&r).and_then(|b| b.text);
            match drawn {
                Some(t) if !text_dirty(&mut w, r) => Some(t),
                _ => {
                    // Not drawn with this text / box yet: measured now, as
                    // Studio does on the read.
                    let b = abs_now(&mut w, r);
                    crate::gui::text_bounds_now(w.d(), r, b.size, 1.0).or(drawn)
                }
            }
        };
        if let Some(t) = measured {
            return Ok(Some(Value::UserData(
                lua.create_userdata(LVector2(glam::Vec2::new(t[0], t[1])))?,
            )));
        }
        // Not drawn yet: estimate from the text length and TextSize.
        let w = world(lua)?;
        let d = w.d();
        let text = match d.prop(r, "Text") {
            Some(Variant::String(s)) => s,
            _ => String::new(),
        };
        let size = rustudio_render::scene::f32_of(d.prop(r, "TextSize")).unwrap_or(14.0);
        let width = text.chars().count() as f32 * size * mech::TEXT_WIDTH_PER_SIZE;
        return Ok(Some(Value::UserData(lua.create_userdata(LVector2(
            glam::Vec2::new(width, if text.is_empty() { 0.0 } else { size }),
        ))?)));
    }
    if key == "LocalTransparencyModifier" && doc::is_a(class, "BasePart") {
        // Client-only (never saved): 0 until a script sets it.
        let v = rustudio_render::scene::f32_of(
            world(lua)?
                .d()
                .dom
                .get_by_ref(r)
                .and_then(|i| i.properties.get(&rbx_dom_weak::ustr(key)).cloned()),
        )
        .unwrap_or(0.0);
        return Ok(Some(Value::Number(v as f64)));
    }
    if class == "AnimationTrack"
        && matches!(
            key,
            "Speed"
                | "IsPlaying"
                | "Length"
                | "TimePosition"
                | "WeightCurrent"
                | "WeightTarget"
                | "Looped"
                | "Priority"
                | "Animation"
        )
    {
        // The library's track state (an animation that couldn't load has Length 0, as in Roblox).
        let get: mlua::Function = lua.globals().get("__rs_track_get")?;
        let v: Value = get.call((inst_value(lua, r)?, key))?;
        if !v.is_nil() {
            return Ok(Some(v));
        }
    }
    if class == "Camera" && key == "ViewportSize" {
        let v = world(lua)?.input.view_size;
        let v = if v[0] > 0.0 { v } else { [1265.0, 836.0] };
        return Ok(Some(Value::UserData(
            lua.create_userdata(LVector2(glam::Vec2::new(v[0], v[1])))?,
        )));
    }
    if doc::is_a(class, "Mouse") {
        let inp = world(lua)?.input.clone();
        let hit = inp.mouse_hit.unwrap_or(Xform {
            pos: inp.mouse_ray.0 + inp.mouse_ray.1 * 1000.0,
            rot: glam::Mat3::IDENTITY,
        });
        return Ok(Some(match key {
            "Hit" | "hit" => cframe_value(lua, hit)?,
            // The face of Target the mouse is over (the hit's nearest face).
            "TargetSurface" => {
                let face = inp.mouse_target.and_then(|t| {
                    let w = world(lua).ok()?;
                    let d = w.d();
                    let txf = rustudio_render::scene::xform_of(d, t)?;
                    let size = rustudio_render::scene::vec3_of(d.prop(t, "Size")).unwrap_or(Vec3::ONE);
                    let rel = (txf.rot.transpose() * (hit.pos - txf.pos))
                        / (size * 0.5).max(Vec3::splat(1e-4));
                    let a = rel.abs();
                    Some(if a.x >= a.y && a.x >= a.z {
                        if rel.x > 0.0 { "Right" } else { "Left" }
                    } else if a.y >= a.z {
                        if rel.y > 0.0 { "Top" } else { "Bottom" }
                    } else if rel.z > 0.0 {
                        "Back"
                    } else {
                        "Front"
                    })
                });
                enum_item(lua, "NormalId", face.unwrap_or("Top"))?
            }
            "Target" | "target" => match inp.mouse_target {
                Some(t) => inst_value(lua, t)?,
                None => Value::Nil,
            },
            "Origin" => cframe_value(
                lua,
                Xform {
                    pos: inp.mouse_ray.0,
                    rot: glam::Mat3::IDENTITY,
                },
            )?,
            "UnitRay" => {
                let t = lua.create_table()?;
                t.set("Origin", vec3_value(lua, inp.mouse_ray.0)?)?;
                t.set("Direction", vec3_value(lua, inp.mouse_ray.1)?)?;
                Value::Table(t)
            }
            "X" => Value::Number(inp.mouse_xy[0] as f64),
            "Y" => Value::Number(inp.mouse_xy[1] as f64),
            "ViewSizeX" => Value::Number(inp.view_size[0] as f64),
            "ViewSizeY" => Value::Number(inp.view_size[1] as f64),
            _ => return Ok(None),
        }));
    }
    // Run-time properties with Roblox's documented values (round 12, from the
    // API-gap telemetry): what a game script reads in a Studio session.
    if class == "Tween" && matches!(key, "Instance" | "TweenInfo" | "PlaybackState") {
        let get: mlua::Function = lua.globals().get("__rs_tween_get")?;
        let v: Value = get.call((inst_value(lua, r)?, key))?;
        return Ok(Some(v));
    }
    if class == "Player" {
        let name = world(lua)?
            .d()
            .dom
            .get_by_ref(r)
            .map(|i| i.name.clone())
            .unwrap_or_default();
        let v = match key {
            // An offline Studio session: no account age, nobody followed,
            // en-us; the display name is the name unless one was set.
            "AccountAge" | "FollowUserId" | "DataComplexity" => Some(Value::Number(0.0)),
            "DisplayName" => {
                return stored_or(lua, r, key, |l| Ok(Value::String(l.create_string(&name)?)))
            }
            "MembershipType" => {
                return stored_or(lua, r, key, |l| enum_item(l, "MembershipType", "None"))
            }
            "LocaleId" => Some(Value::String(lua.create_string("en-us")?)),
            "PartyId" => Some(Value::String(lua.create_string("")?)),
            "CanLoadCharacterAppearance" => {
                return stored_or(lua, r, key, |_| Ok(Value::Boolean(true)))
            }
            "HasVerifiedBadge" | "GameplayPaused" | "DataReady" | "HasRobloxSubscription" => {
                return stored_or(lua, r, key, |_| Ok(Value::Boolean(false)))
            }
            "userId" => return index_alias(lua, r, "UserId"),
            _ => None,
        };
        if v.is_some() {
            return Ok(v);
        }
    }
    if class == "Players" {
        match key {
            "NumPlayers" | "numPlayers" => {
                let n = {
                    let w = world(lua)?;
                    let d = w.d();
                    d.dom
                        .get_by_ref(r)
                        .map(|i| {
                            i.children()
                                .iter()
                                .filter(|c| d.class_of(**c).is_some_and(|k| k == "Player"))
                                .count()
                        })
                        .unwrap_or(0)
                };
                return Ok(Some(Value::Number(n as f64)));
            }
            "localPlayer" => return index_alias(lua, r, "LocalPlayer"),
            _ => {}
        }
    }
    if class == "DataModel" {
        let v = match key {
            // A Studio test session: no job id, no private server.
            "JobId" | "PrivateServerId" | "VIPServerId" => {
                Some(Value::String(lua.create_string("")?))
            }
            "PrivateServerOwnerId" | "VIPServerOwnerId" => Some(Value::Number(0.0)),
            "CreatorType" => {
                return stored_or(lua, r, key, |l| enum_item(l, "CreatorType", "User"))
            }
            "Genre" => return stored_or(lua, r, key, |l| enum_item(l, "Genre", "All")),
            "MatchmakingType" => Some(enum_item(lua, "MatchmakingType", "Default")?),
            _ => None,
        };
        if v.is_some() {
            return Ok(v);
        }
    }
    if class == "Humanoid" && matches!(key, "Sit" | "PlatformStand") {
        return stored_or(lua, r, key, |_| Ok(Value::Boolean(false)));
    }
    if class == "Humanoid" && matches!(key, "WalkToPoint" | "TargetPoint") {
        return stored_or(lua, r, key, |l| {
            Ok(LVector3(Vec3::ZERO).into_value())
        });
    }
    if class == "Humanoid" && key == "WalkToPart" {
        return stored_or(lua, r, key, |_| Ok(Value::Nil));
    }
    if class == "Model" && key == "WorldPivot" {
        let xf = model_pivot(&*world(lua)?, r);
        return Ok(Some(cframe_value(lua, xf)?));
    }
    if class == "Attachment" && matches!(key, "Rotation" | "WorldRotation") {
        // Older names of Orientation / WorldOrientation.
        let canon = if key == "Rotation" { "Orientation" } else { "WorldOrientation" };
        return index_alias(lua, r, canon);
    }
    if doc::is_a(class, "BasePart")
        && matches!(
            key,
            "ExtentsCFrame" | "ExtentsSize" | "CenterOfMass" | "AssemblyCenterOfMass"
        )
    {
        // One part: its own box; its centre of mass is its centre.
        let xf = part_xform(lua, r)?;
        return Ok(Some(match key {
            "ExtentsSize" => vec3_value(lua, part_size(lua, r)?)?,
            "CenterOfMass" | "AssemblyCenterOfMass" => vec3_value(lua, xf.pos)?,
            _ => cframe_value(lua, xf)?,
        }));
    }
    if class == "Sound" {
        let w = world(lua)?;
        return Ok(match key {
            "TimePosition" => Some(Value::Number(sound_time(&w, r))),
            "TimeLength" => {
                drop(w);
                Some(Value::Number(super::domains::media::sound_length(lua, r)?.unwrap_or(0.0)))
            }
            "IsPlaying" => Some(Value::Boolean(w.input.sound_playing.contains(&r))),
            // Studio's IsPaused getter is
            // == 0`, the same byte Playing / IsPlaying read
            // a
            // sound that never played or has ended reads IsPaused = true.
            "IsPaused" => Some(Value::Boolean(!w.input.sound_playing.contains(&r))),
            "IsLoaded" => {
                drop(w);
                Some(Value::Boolean(super::domains::media::sound_length(lua, r)?.is_some()))
            }
            _ => None,
        });
    }
    Ok(None)
}

/// A property under its canonical name, through the normal index.
fn index_alias(lua: &Lua, r: Ref, key: &str) -> mlua::Result<Option<Value>> {
    let v = match inst_value(lua, r)? {
        Value::UserData(u) => u.get::<Value>(key)?,
        _ => Value::Nil,
    };
    Ok(Some(v))
}

/// Writes newindex acts on and then still stores (returns false for).
pub fn write_acts(class: &str, key: &str) -> bool {
    matches!(
        (class, key),
        ("Humanoid", "Jump")
            | ("Humanoid", "WalkToPoint")
            | ("Humanoid", "PlatformStand")
            | ("Model", "WorldPivot")
    )
}

/// Lighting.ClockTime and TimeOfDay are one value in Roblox (whole seconds
/// in [0, 24], mech::clock_time): writing either sets both.
fn set_clock(lua: &Lua, r: Ref, hours: f32) -> mlua::Result<()> {
    let t = mech::clock_time(hours);
    store_prop(lua, r, "ClockTime", Variant::Float32(t))?;
    store_prop(lua, r, "TimeOfDay", Variant::String(mech::format_time_of_day(t)))
}

/// Lighting's hours now: ClockTime, else the saved TimeOfDay, else 14.
fn clock_hours(lua: &Lua, r: Ref) -> mlua::Result<f32> {
    let w = world(lua)?;
    let d = w.d();
    Ok(rustudio_render::lighting::clock_hours(d.prop(r, "ClockTime"), d.prop(r, "TimeOfDay")))
}

/// Write-side hooks: returns true when handled here.
pub fn newindex(lua: &Lua, r: Ref, class: &str, key: &str, value: &Value) -> mlua::Result<bool> {
    // Model.WorldPivot (Roblox): with a PrimaryPart, its PivotOffset becomes
    // PrimaryPart.CFrame⁻¹ · pivot; without one, the model keeps the pivot
    // itself (WorldPivotData). Nothing moves. (model_pivot reads both.)
    if key == "WorldPivot" && doc::is_a(class, "Model") {
        let pivot = opt_ud::<LCFrame>(value)
            .ok_or_else(|| rt_err("Unable to assign property WorldPivot. CFrame expected"))?
            .0;
        let primary = match world(lua)?.d().prop(r, "PrimaryPart") {
            Some(Variant::Ref(pp)) if pp.is_some() => Some(pp),
            _ => None,
        };
        let primary_cf = primary.and_then(|pp| match world(lua).ok()?.d().prop(pp, "CFrame") {
            Some(Variant::CFrame(cf)) => Some((pp, Xform::from_cframe(&cf))),
            _ => None,
        });
        match primary_cf {
            Some((pp, cf)) => {
                let off = crate::ltypes::cf_mul(&crate::ltypes::cf_inv(&cf), &pivot);
                super::newindex(
                    lua,
                    pp,
                    "PivotOffset",
                    LCFrame(off).into_lua(lua)?,
                )?;
            }
            None => store_prop(
                lua,
                r,
                "WorldPivotData",
                Variant::OptionalCFrame(Some(pivot.to_cframe())),
            )?,
        }
        // Studio's setter then signals WorldPivot itself changed.
        let mut w = world(lua)?;
        w.fire(r, "Changed", vec![Value::String(lua.create_string("WorldPivot")?)]);
        w.fire(r, "Changed:WorldPivot", vec![]);
        return Ok(true);
    }
    match (class, key) {
        ("Lighting", "ClockTime") => {
            let h = num(value).ok_or_else(|| rt_err("ClockTime: number expected"))?;
            set_clock(lua, r, h as f32)?;
            Ok(true)
        }
        ("Lighting", "TimeOfDay") => {
            let h = match value {
                Value::String(s) => mech::parse_time_of_day(&s.to_str()?),
                _ => None,
            }
            .ok_or_else(|| rt_err("TimeOfDay: \"HH:MM:SS\" expected"))?;
            set_clock(lua, r, h)?;
            Ok(true)
        }
        ("Sound", "Playing") => {
            // Idempotent, unlike Play(): setting the value it already has
            // does nothing; false pauses (TimePosition is kept), true goes
            // on from there (Roblox Sound.Playing).
            let on = matches!(value, Value::Boolean(true));
            let (playing, paused) = {
                let w = world(lua)?;
                (
                    w.input.sound_playing.contains(&r),
                    w.input.sound_paused.contains(&r),
                )
            };
            let cmd = match (on, playing, paused) {
                (true, true, _) | (false, false, _) => return Ok(true),
                (true, false, true) => SoundCmd::Resume,
                (true, false, false) => SoundCmd::Play,
                (false, true, _) => SoundCmd::Pause,
            };
            sound_ev(lua, r, cmd, false)?;
            Ok(true)
        }
        ("Sound", "TimePosition") => {
            // Read back at once; while it plays the sound seeks there; the
            // next Play() starts there (stored). Changed fires when it moves.
            let Some(t) = num(value) else {
                return Err(rt_err(format!(
                    "Unable to assign property TimePosition. Number expected, got {}",
                    value.type_name()
                )));
            };
            let t = if t.is_finite() { t.max(0.0) } else { 0.0 };
            let mut w = world(lua)?;
            let old = sound_time(&w, r);
            w.input.sound_pos.insert(r, t);
            if w.input.sound_playing.contains(&r) || w.input.sound_paused.contains(&r) {
                w.requests.sound_seeks.push((r, t));
            }
            if let Some(i) = w.dm().dom.get_by_ref_mut(r) {
                i.properties.insert(ustr("TimePosition"), Variant::Float64(t));
            }
            w.dm().mark_prop(r, ustr("TimePosition"));
            w.record(RepOp::Prop(r, "TimePosition".into()));
            if old != t {
                let name = Value::String(lua.create_string("TimePosition")?);
                w.fire(r, "Changed", vec![name]);
                w.fire(r, "Changed:TimePosition", vec![]);
            }
            Ok(true)
        }
        ("Humanoid", "PlatformStand") => {
            // Humanoid.PlatformStanding(active) when it changes (Roblox).
            let on = matches!(value, Value::Boolean(true));
            let was = matches!(world(lua)?.d().prop(r, "PlatformStand"), Some(Variant::Bool(true)));
            if on != was {
                world(lua)?.fire(r, "PlatformStanding", vec![Value::Boolean(on)]);
            }
            Ok(false)
        }
        ("Humanoid", "Jump") if matches!(value, Value::Boolean(true)) => {
            world(lua)?.requests.jumps.push(r);
            Ok(false)
        }
        ("AnimationTrack", "Looped" | "Priority" | "TimePosition") => {
            let set: mlua::Function = lua.globals().get("__rs_track_set")?;
            set.call::<bool>((inst_value(lua, r)?, key, value.clone()))
        }
        ("Humanoid", "WalkToPoint") => {
            if let Some(p) = opt_ud::<LVector3>(value) {
                world(lua)?.requests.move_to.push((r, p.0));
            }
            Ok(false)
        }
        _ => Ok(false),
    }
}

/// Engine methods from METHODS; None when `name` isn't one of them.
pub fn call(
    lua: &Lua,
    name: &str,
    this: Ref,
    args: &MultiValue,
) -> Option<mlua::Result<MultiValue>> {
    let mask = match super::inst_class(lua, this) {
        Ok((class, _)) => super::domains::call_claims(class.as_str(), name, false, None),
        Err(_) => u16::MAX,
    };
    Some(call_inner(lua, name, this, args, mask))
}

/// `call` asking only the domains in `domains` (the method's route,
/// runtime_routes.rs).
pub fn call_routed(lua: &Lua, name: &str, this: Ref, args: &MultiValue, domains: u16) -> mlua::Result<MultiValue> {
    call_inner(lua, name, this, args, domains)
}

fn call_inner(lua: &Lua, name: &str, this: Ref, args: &MultiValue, domains: u16) -> mlua::Result<MultiValue> {
    // EditableMesh / EditableImage and AssetService's Editable* calls (editable.rs).
    if let Some(r) = super::editable::call(lua, name, this, args) {
        return r;
    }
    if let Some(r) = super::domains::call_masked(lua, name, this, args, domains) {
        return r;
    }
    match name {
        "BreakJoints" => {
            break_joints(lua, this)?;
            none()
        }
        "MakeJoints"
        | "SetNetworkOwner"
        | "SetNetworkOwnershipAuto"
        | "ApplyAngularImpulse"
        | "ApplyDescription"
        | "ClearCharacterAppearance"
        | "BindToClose"
        | "SetCore" => none(),
        "GetModelCFrame" => {
            let xf = model_pivot(&*world(lua)?, this);
            one(cframe_value(lua, xf)?)
        }
        "GetModelSize" | "GetExtentsSize" => {
            let (lo, hi) = model_bounds(lua, this)?;
            one(vec3_value(lua, hi - lo)?)
        }
        "GetBoundingBox" => {
            let (lo, hi) = model_bounds(lua, this)?;
            let rot = model_pivot(&*world(lua)?, this).rot;
            Ok(MultiValue::from_iter([
                cframe_value(
                    lua,
                    Xform {
                        pos: (lo + hi) * 0.5,
                        rot,
                    },
                )?,
                vec3_value(lua, hi - lo)?,
            ]))
        }
        "TranslateBy" => {
            let v = vec3_arg(args, 1, "TranslateBy")?;
            let p = model_pivot(&*world(lua)?, this);
            pivot_to(
                lua,
                this,
                Xform {
                    pos: p.pos + v,
                    rot: p.rot,
                },
            )?;
            none()
        }
        "GetScale" => one(Value::Number(1.0)),
        "GetTouchingParts" => one(Value::Table(list(lua, Vec::new())?)),
        "GetConnectedParts" | "GetRootPart" => {
            if name == "GetRootPart" {
                one(inst_value(lua, this)?)
            } else {
                one(Value::Table(list(lua, vec![this])?))
            }
        }
        "GetNetworkOwner" => one(Value::Nil),
        "CanSetNetworkOwnership" => Ok(MultiValue::from_iter([Value::Boolean(true), Value::Nil])),
        "Resize" | "CanCollideWith" => one(Value::Boolean(true)),
        "GetVelocityAtPosition" => {
            let v = world(lua)?
                .velocities
                .get(&this)
                .copied()
                .unwrap_or(Vec3::ZERO);
            one(vec3_value(lua, v)?)
        }
        "Raycast" => {
            let origin = vec3_arg(args, 1, "Raycast")?;
            let dir = vec3_arg(args, 2, "Raycast")?;
            let filter = match arg(args, 3) {
                Value::Nil => Filter::none(),
                p => params_filter_groups(lua, &p),
            };
            match cast(lua, origin, dir, &filter)? {
                Some(hit) => one(raycast_result(lua, hit)?),
                None => one(Value::Nil),
            }
        }
        // WorldRoot:Spherecast(position, radius, direction, params),
        // Blockcast(cframe, size, direction, params), Shapecast(part,
        // direction, params): the shape swept along `direction`.
        "Spherecast" | "Blockcast" | "Shapecast" => {
            let probe: Option<(u8, Vec3, glam::Mat3)>;
            let (origin, grow, dir, params) = match name {
                // Studio's checks (Spherecast, Blockcast
                // Shapecast), with its messages:
                // finite inputs, radius ≥ 0, sizes up to FInt
                // ShapecastMaxSize (512; a sphere's diameter), distance up to
                // ShapecastMaxDistance (1024).
                "Spherecast" => {
                    let o = vec3_arg(args, 1, name)?;
                    if !o.is_finite() {
                        return Err(rt_err("Invalid position. Must contain finite values."));
                    }
                    let r = num(&arg(args, 2)).unwrap_or(0.0) as f32;
                    if !r.is_finite() {
                        return Err(rt_err("Invalid Radius. Must contain finite values."));
                    }
                    if r < 0.0 {
                        return Err(rt_err(format!(
                            "Invalid Radius. Must be greater than or equal to 0, got {r:.6}."
                        )));
                    }
                    if r + r > mech::SHAPECAST_MAX_SIZE {
                        return Err(rt_err(format!(
                            "Attempt to spherecast with radius {r:.6}. The maximum radius is {:.6}.",
                            mech::SHAPECAST_MAX_SIZE * 0.5
                        )));
                    }
                    probe = Some((rustudio_physics::query_shapes::BALL, Vec3::splat(r + r), glam::Mat3::IDENTITY));
                    (o, Vec3::splat(r), vec3_arg(args, 3, name)?, arg(args, 4))
                }
                "Blockcast" => {
                    let cf = opt_ud::<LCFrame>(&arg(args, 1))
                        .ok_or_else(|| rt_err("Blockcast: CFrame expected"))?;
                    let side = vec3_arg(args, 2, name)?;
                    if side.max_element() > mech::SHAPECAST_MAX_SIZE {
                        return Err(rt_err(format!(
                            "Attempt to blockcast with side length {:.6}. The maximum side length is {:.6}.",
                            side.max_element(),
                            mech::SHAPECAST_MAX_SIZE
                        )));
                    }
                    let h = side * 0.5;
                    let r = cf.0.rot;
                    let ext = r.x_axis.abs() * h.x + r.y_axis.abs() * h.y + r.z_axis.abs() * h.z;
                    probe = Some((rustudio_physics::query_shapes::BOX, side, r));
                    (cf.0.pos, ext, vec3_arg(args, 3, name)?, arg(args, 4))
                }
                _ => {
                    let p = opt_inst(&arg(args, 1))
                        .ok_or_else(|| rt_err("Shapecast: BasePart expected"))?;
                    if world(lua)?.d().class_of(p).is_some_and(|c| c == "Terrain") {
                        return Err(rt_err("Shapecasted part cannot be Terrain."));
                    }
                    let xf = part_xform(lua, p)?;
                    let size = part_size(lua, p)?;
                    if size.max_element() > mech::SHAPECAST_MAX_SIZE {
                        return Err(rt_err(format!(
                            "Attempt to shapecast with side length {:.6}. The maximum side length is {:.6}.",
                            size.max_element(),
                            mech::SHAPECAST_MAX_SIZE
                        )));
                    }
                    let kind = {
                        let w = world(lua)?;
                        let d = w.d();
                        let class = d.class_of(p).map(|c| c.to_string()).unwrap_or_default();
                        rustudio_physics::query_shapes::kind_of(d, p, &class)
                    };
                    probe = Some((kind, size, xf.rot));
                    let ball = kind == rustudio_physics::query_shapes::BALL;
                    let ext = if ball {
                        Vec3::splat(size.min_element() * 0.5)
                    } else {
                        let h = size * 0.5;
                        let r = xf.rot;
                        r.x_axis.abs() * h.x + r.y_axis.abs() * h.y + r.z_axis.abs() * h.z
                    };
                    (xf.pos, ext, vec3_arg(args, 2, name)?, arg(args, 3))
                }
            };
            let mut filter = match params {
                Value::Nil => Filter::none(),
                p => params_filter_groups(lua, &p),
            };
            if name == "Shapecast" {
                if let Some(p) = opt_inst(&arg(args, 1)) {
                    if !filter.include {
                        filter.list.insert(p);
                    }
                }
            }
            if !dir.is_finite() {
                return Err(rt_err("Invalid direction. Must contain finite values."));
            }
            if dir.length() > mech::SHAPECAST_MAX_DISTANCE {
                return Err(rt_err(format!(
                    "Attempt to shapecast with distance {:.6}. The maximum distance is {}.",
                    dir.length(),
                    mech::SHAPECAST_MAX_DISTANCE as i32
                )));
            }
            match cast_swept(lua, origin, dir, &filter, grow, probe)? {
                Some(hit) => one(raycast_result(lua, hit)?),
                None => one(Value::Nil),
            }
        }
        // BasePart helpers (Roblox docs), for one part at a time.
        "GetRenderCFrame" => one(cframe_value(lua, part_xform(lua, this)?)?),
        "GetClosestPointOnSurface" => {
            // The closest point on (or in) the part's box to `position`.
            let p = vec3_arg(args, 1, name)?;
            let xf = part_xform(lua, this)?;
            let h = part_size(lua, this)? * 0.5;
            let local = (xf.rot.transpose() * (p - xf.pos)).clamp(-h, h);
            one(vec3_value(lua, xf.pos + xf.rot * local)?)
        }
        "GetJoints" | "GetNoCollisionConstraints" => {
            // Joints / NoCollisionConstraints anywhere that name this part.
            let want_nc = name == "GetNoCollisionConstraints";
            let found: Vec<Ref> = {
                let w = world(lua)?;
                let d = w.d();
                d.dom
                    .descendants()
                    .filter(|i| {
                        let is_joint = doc::is_a(&i.class, "JointInstance")
                            || matches!(i.class.as_str(), "WeldConstraint" | "NoCollisionConstraint")
                            || doc::is_a(&i.class, "Constraint");
                        let ok = if want_nc {
                            i.class == "NoCollisionConstraint"
                        } else {
                            is_joint && i.class != "NoCollisionConstraint"
                        };
                        ok && ["Part0", "Part1"].iter().any(|k| {
                            matches!(i.properties.get(&ustr(k)), Some(Variant::Ref(x)) if *x == this)
                        })
                    })
                    .map(|i| i.referent())
                    .collect()
            };
            one(Value::Table(list(lua, found)?))
        }
        "ApplyImpulseAtPosition" => {
            // The linear part of the impulse (no angular response yet).
            let imp = vec3_arg(args, 1, name)?;
            let mass = num(&index_alias(lua, this, "AssemblyMass")?.unwrap_or(Value::Nil))
                .unwrap_or(1.0)
                .max(1e-4) as f32;
            let v = world(lua)?.velocities.get(&this).copied().unwrap_or(Vec3::ZERO);
            world(lua)?.velocity_requests.push((this, v + imp / mass));
            none()
        }
        // Lighting:GetSunDirection / GetMoonDirection (unit vectors toward
        // them, from ClockTime and GeographicLatitude) and GetMoonPhase.
        "GetSunDirection" | "GetMoonDirection" => {
            let (clock, lat) = {
                let w = world(lua)?;
                let d = w.d();
                (
                    rustudio_render::lighting::clock_hours(d.prop(this, "ClockTime"), d.prop(this, "TimeOfDay")),
                    rustudio_render::scene::f32_of(d.prop(this, "GeographicLatitude")).unwrap_or(41.733),
                )
            };
            let s = rustudio_render::lighting::sun_direction(clock, lat);
            one(vec3_value(lua, if name == "GetSunDirection" { s } else { -s })?)
        }
        "GetMoonPhase" => one(Value::Number(mech::MOON_PHASE as f64)),
        "players" => {
            let kids: Vec<Ref> = {
                let all = children_of(lua, this)?;
                let w = world(lua)?;
                all.into_iter()
                    .filter(|c| w.d().class_of(*c).is_some_and(|k| k == "Player"))
                    .collect()
            };
            one(Value::Table(list(lua, kids)?))
        }
        "FindPartOnRay" | "FindPartOnRayWithIgnoreList" | "FindPartOnRayWithWhitelist" => {
            let ray = arg(args, 1);
            let origin = ud_field::<LVector3>(&ray, "Origin")
                .map(|v| v.0)
                .ok_or_else(|| rt_err(format!("{name}: Ray expected")))?;
            let dir = ud_field::<LVector3>(&ray, "Direction")
                .map(|v| v.0)
                .unwrap_or(Vec3::ZERO);
            let list: rustc_hash::FxHashSet<Ref> = refs_in(&arg(args, 2)).into_iter().collect();
            // FindPartOnRay(ray, ignore, terrainCellsAreCubes, ignoreWater),
            // ...WithIgnoreList(ray, list, terrainCellsAreCubes, ignoreWater),
            // ...WithWhitelist(ray, list, ignoreWater).
            let water_arg = if name == "FindPartOnRayWithWhitelist" { 3 } else { 4 };
            let filter = Filter {
                list,
                include: name == "FindPartOnRayWithWhitelist",
                respect_collide: false,
                skip_groups: HashSet::new(),
                ignore_water: matches!(arg(args, water_arg), Value::Boolean(true)),
            };
            match cast(lua, origin, dir, &filter)? {
                Some((r, p, n, _)) => {
                    let terrain = lua
                        .app_data_ref::<QueryCache>()
                        .and_then(|c| c.terrain_material)
                        .filter(|_| world(lua).is_ok_and(|w| w.d().class_of(r).is_some_and(|k| k == "Terrain")));
                    let mat = match (terrain, world(lua)?.d().prop(r, "Material")) {
                        (Some(m), _) => LEnumItem::from_name("Material", rustudio_core::terrain::material_name(m)),
                        (None, Some(Variant::Enum(e))) => LEnumItem::from_value("Material", e.to_u32()),
                        _ => None,
                    };
                    let mat = match mat {
                        Some(m) => crate::ltypes::enum_value(lua, m)?,
                        None => Value::Nil,
                    };
                    Ok(MultiValue::from_iter([
                        inst_value(lua, r)?,
                        vec3_value(lua, p)?,
                        vec3_value(lua, n)?,
                        mat,
                    ]))
                }
                // A miss: the ray's end, a zero normal and Material Air (Studio).
                None => Ok(MultiValue::from_iter([
                    Value::Nil,
                    vec3_value(lua, origin + dir)?,
                    vec3_value(lua, Vec3::ZERO)?,
                    match LEnumItem::from_name("Material", "Air") {
                        Some(m) => crate::ltypes::enum_value(lua, m)?,
                        None => Value::Nil,
                    },
                ])),
            }
        }
        "FindPartsInRegion3"
        | "FindPartsInRegion3WithIgnoreList"
        | "FindPartsInRegion3WithWhiteList" => {
            let (lo, hi) = region_box(&arg(args, 1))
                .ok_or_else(|| rt_err(format!("{name}: Region3 expected")))?;
            let listed: rustc_hash::FxHashSet<Ref> = refs_in(&arg(args, 2)).into_iter().collect();
            let filter = Filter {
                list: listed,
                include: name.ends_with("WhiteList"),
                respect_collide: false,
                skip_groups: HashSet::new(),
                ignore_water: false,
            };
            let max = num(&arg(args, 3)).map(|n| n as usize).unwrap_or(20);
            let parts = overlap(lua, lo, hi, None, &filter, max)?;
            one(Value::Table(list(lua, parts)?))
        }
        "GetPartBoundsInBox" => {
            let cf = opt_ud::<LCFrame>(&arg(args, 1))
                .ok_or_else(|| rt_err("GetPartBoundsInBox: CFrame expected"))?;
            let size = vec3_arg(args, 2, "GetPartBoundsInBox")?;
            let h = size * 0.5;
            let r = cf.0.rot;
            let ext = r.x_axis.abs() * h.x + r.y_axis.abs() * h.y + r.z_axis.abs() * h.z;
            let filter = match arg(args, 3) {
                Value::Nil => Filter::none(),
                p => params_filter_groups(lua, &p),
            };
            let max = num(&ud_value(&arg(args, 3), "MaxParts"))
                .filter(|n| *n > 0.0)
                .map(|n| n as usize)
                .unwrap_or(usize::MAX);
            let parts = overlap(lua, cf.0.pos - ext, cf.0.pos + ext, None, &filter, max)?;
            one(Value::Table(list(lua, parts)?))
        }
        "GetPartBoundsInRadius" => {
            let c = vec3_arg(args, 1, "GetPartBoundsInRadius")?;
            let r = num(&arg(args, 2)).unwrap_or(0.0) as f32;
            let filter = match arg(args, 3) {
                Value::Nil => Filter::none(),
                p => params_filter_groups(lua, &p),
            };
            let parts = overlap(
                lua,
                c - Vec3::splat(r),
                c + Vec3::splat(r),
                Some((c, r)),
                &filter,
                usize::MAX,
            )?;
            one(Value::Table(list(lua, parts)?))
        }
        "GetPartsInPart" => {
            let p = opt_inst(&arg(args, 1))
                .ok_or_else(|| rt_err("GetPartsInPart: BasePart expected"))?;
            let xf = part_xform(lua, p)?;
            let size = part_size(lua, p)?;
            let probe_kind = {
                let w = world(lua)?;
                let d = w.d();
                let class = d.class_of(p).map(|c| c.to_string()).unwrap_or_default();
                rustudio_physics::query_shapes::kind_of(d, p, &class)
            };
            let (lo, hi) = part_aabb(&QPart {
                r: p,
                xf,
                size,
                collide: true,
                query: true,
                mesh: false,
                shape: 0,
                cvx: 0,
                initial: Vec3::ONE,
            });
            let mut filter = match arg(args, 2) {
                Value::Nil => Filter::none(),
                v => params_filter(&v),
            };
            if !filter.include {
                filter.list.insert(p);
            }
            let max = num(&ud_value(&arg(args, 2), "MaxParts"))
                .filter(|n| *n > 0.0)
                .map(|n| n as usize)
                .unwrap_or(usize::MAX);
            // Bounds first, then the real shapes (GetPartsInPart is exact in
            // Roblox: a turned brush's or a terrain chunk's box covers far more
            // than the part).
            let near = overlap(lua, lo, hi, None, &filter, usize::MAX)?;
            let mut parts = Vec::new();
            for r in near {
                if parts.len() >= max {
                    break;
                }
                let q = lua.app_data_ref::<QueryCache>().and_then(|c| {
                    c.index.get(&r).and_then(|i| c.parts.get(*i)).copied()
                });
                let Some(q) = q else { continue };
                let exact = if q.mesh {
                    let id = match world(lua)?.d().prop(r, "MeshId") {
                        Some(Variant::ContentId(c)) => c.as_str().to_string(),
                        _ => rustudio_core::mesh::union_mesh(world(lua)?.d(), r).map(|m| m.id).unwrap_or_default(),
                    };
                    match (!id.is_empty()).then(|| rustudio_core::mesh_bvh::for_mesh(&id)).flatten() {
                        Some(bvh) => rustudio_physics::query_shapes::obb_mesh_bvh(&xf, size, &q.xf, q.size, &bvh),
                        None => rustudio_physics::query_shapes::obb_obb(&xf, size, &q.xf, q.size),
                    }
                } else {
                    rustudio_physics::query_shapes::part_part(&xf, size, probe_kind, &q.xf, q.size, q.shape)
                };
                if exact {
                    parts.push(r);
                }
            }
            one(Value::Table(list(lua, parts)?))
        }
        "GetServerTimeNow" => one(Value::Number(
            crate::runtime::fixed_epoch_time(lua).unwrap_or_else(rustudio_core::sim_clock::epoch_secs),
        )),
        "GetRealPhysicsFPS" => one(Value::Number(rustudio_physics::physics::step_rate().0 as f64)),
        "MoveTo" => {
            let target = vec3_arg(args, 1, "MoveTo")?;
            world(lua)?.requests.move_to.push((this, target));
            none()
        }
        // Studio 0.744 (Studio, client script): the tool is in the character
        // as EquipTool returns and back in the Backpack as UnequipTools
        // returns; Equipped / Unequipped fire after. The play session does
        // the rest (grip, server copy, events) from the request.
        "EquipTool" => {
            let tool = opt_inst(&arg(args, 1)).ok_or_else(|| rt_err("EquipTool: Tool expected"))?;
            equip_now(lua, this, Some(tool))?;
            world(lua)?.requests.equip.push((this, Some(tool)));
            none()
        }
        "UnequipTools" => {
            equip_now(lua, this, None)?;
            world(lua)?.requests.equip.push((this, None));
            none()
        }
        "ChangeState" => {
            if let Some(e) = opt_ud::<LEnumItem>(&arg(args, 1)) {
                let mut w = world(lua)?;
                if e.name == "Jumping" {
                    w.requests.jumps.push(this);
                } else {
                    w.requests.change_state.push((this, e.name.to_string()));
                }
            }
            none()
        }
        // Humanoid:SetStateEnabled / GetStateEnabled: crates/rustudio-engine/src/domains/anim.rs.
        // Seat:Sit(humanoid): the humanoid sits in this seat.
        "Sit" => {
            let h = opt_inst(&arg(args, 1)).ok_or_else(|| rt_err("Sit: Humanoid expected"))?;
            world(lua)?.requests.sits.push((this, h));
            none()
        }
        "HasAppearanceLoaded" | "IsLoaded" => one(Value::Boolean(true)),
        "SetCoreGuiEnabled" | "GetCoreGuiEnabled" => {
            // StarterGui::setCoreGuiEnabled / getCoreGuiEnabled
            // client only; All (4) sets every type 0..9; one
            // type is set, then All becomes `enabled` only if every other
            // type agrees with it (else false). Get reads the type's own
            // entry, All included.
            if world(lua)?.side != Side::Client {
                return Err(rt_err(format!(
                    "StarterGui:{name} must be called from a local script."
                )));
            }
            // The item, its name or its value, as Roblox's enum arguments
            // (an admin script's console: `GetCoreGuiEnabled("Chat")`).
            let ty = match arg(args, 1) {
                Value::Nil => return Err(rt_err(format!("{name}: Enum.CoreGuiType expected"))),
                v => crate::ltypes::enum_arg(Some(&v), "CoreGuiType", "All")?,
            };
            let label = |v: u32| LEnumItem::from_value("CoreGuiType", v).map(|e| e.name.to_string());
            let mut w = world(lua)?;
            let ty_name = ty.name.to_string();
            if name == "GetCoreGuiEnabled" {
                return one(Value::Boolean(!w.core_gui_off.contains(&ty_name)));
            }
            let on = matches!(arg(args, 2), Value::Boolean(true));
            let set = |w: &mut World, t: String| {
                if on {
                    w.core_gui_off.remove(&t);
                } else {
                    w.core_gui_off.insert(t);
                }
            };
            if ty.value == mech::CORE_GUI_ALL {
                for v in 0..mech::CORE_GUI_TYPE_COUNT {
                    if let Some(t) = label(v) {
                        set(&mut w, t);
                    }
                }
            } else {
                set(&mut w, ty_name);
                let agree = (0..mech::CORE_GUI_TYPE_COUNT)
                    .filter(|v| *v != mech::CORE_GUI_ALL)
                    .filter_map(label)
                    .all(|t| w.core_gui_off.contains(&t) != on);
                if let Some(all) = label(mech::CORE_GUI_ALL) {
                    if agree && on {
                        w.core_gui_off.remove(&all);
                    } else {
                        w.core_gui_off.insert(all);
                    }
                }
            }
            none()
        }
        "GetAccessories" => {
            let parent = inst_meta(lua, this)?.2;
            let acc: Vec<Ref> = {
                let w = world(lua)?;
                let d = w.d();
                d.dom
                    .get_by_ref(parent)
                    .map(|i| {
                        i.children()
                            .iter()
                            .copied()
                            .filter(|c| {
                                d.class_of(*c)
                                    .is_some_and(|k| doc::is_a(&k, "Accoutrement"))
                            })
                            .collect()
                    })
                    .unwrap_or_default()
            };
            one(Value::Table(list(lua, acc)?))
        }
        "AddAccessory" => {
            let acc = opt_inst(&arg(args, 1))
                .ok_or_else(|| rt_err("AddAccessory: Accessory expected"))?;
            let parent = inst_meta(lua, this)?.2;
            set_parent(lua, acc, parent)?;
            {
                // Welded to the matching body attachment, as Roblox does.
                let mut w = world(lua)?;
                let d = w.dm();
                crate::avatar::weld_accessory(&mut d.dom, parent, acc);
                d.mark_tree(acc);
            }
            none()
        }
        "RemoveAccessories" => {
            let parent = inst_meta(lua, this)?.2;
            let acc: Vec<Ref> = {
                let w = world(lua)?;
                let d = w.d();
                d.dom
                    .get_by_ref(parent)
                    .map(|i| {
                        i.children()
                            .iter()
                            .copied()
                            .filter(|c| {
                                d.class_of(*c)
                                    .is_some_and(|k| doc::is_a(&k, "Accoutrement"))
                            })
                            .collect()
                    })
                    .unwrap_or_default()
            };
            for a in acc {
                destroy_inst(lua, a)?;
            }
            none()
        }
        "Play" | "Resume" | "Stop" | "Pause" => {
            let cmd = match name {
                "Play" => SoundCmd::Play,
                "Resume" => SoundCmd::Resume,
                "Stop" => SoundCmd::Stop,
                _ => SoundCmd::Pause,
            };
            sound(lua, this, cmd)?;
            none()
        }
        "SetListener" => {
            let mode = opt_ud::<LEnumItem>(&arg(args, 1))
                .filter(|e| e.enum_type == "ListenerType")
                .ok_or_else(|| rt_err("SetListener: ListenerType expected"))?
                .value;
            let cf = opt_ud::<LCFrame>(&arg(args, 2)).map(|x| x.0);
            let part = opt_inst(&arg(args, 2));
            if mode > 3 || (mode == 1 && cf.is_none()) || (mode >= 2 && part.is_none()) {
                return Err(rt_err("SetListener: invalid listener"));
            }
            world(lua)?.input.sound_listener = Some((mode, cf, part));
            none()
        }
        "GetListener" => {
            let (mode, cf, part) = world(lua)?.input.sound_listener.unwrap_or((0, None, None));
            let ty = LEnumItem::from_value("ListenerType", mode)
                .ok_or_else(|| rt_err("ListenerType unavailable"))?;
            let listener = if let Some(cf) = cf {
                LCFrame(cf).into_lua(lua)?
            } else if let Some(part) = part {
                inst_value(lua, part)?
            } else {
                Value::Nil
            };
            Ok(MultiValue::from_vec(vec![
                Value::UserData(lua.create_userdata(ty)?),
                listener,
            ]))
        }
        "PlayLocalSound" => {
            let s =
                opt_inst(&arg(args, 1)).ok_or_else(|| rt_err("PlayLocalSound: Sound expected"))?;
            if world(lua)?.side == Side::Server {
                return Err(rt_err("SoundService:PlayLocalSound only works on a client."));
            }
            sound(lua, s, SoundCmd::PlayLocal)?;
            none()
        }
        "Activate" | "Deactivate" => {
            let ev = if name == "Activate" {
                "Activated"
            } else {
                "Deactivated"
            };
            world(lua)?.fire(this, ev, vec![]);
            none()
        }
        "AddTag" | "RemoveTag" | "HasTag" | "GetTags" => {
            // Instance:AddTag("t") or CollectionService:AddTag(inst, "t").
            let on_service = inst_meta(lua, this)?.0 == "CollectionService";
            let (target, tag_at) = if on_service {
                (
                    opt_inst(&arg(args, 1))
                        .ok_or_else(|| rt_err(format!("{name}: Instance expected")))?,
                    2,
                )
            } else {
                (this, 1)
            };
            match name {
                "GetTags" => {
                    let tags = tags_of(lua, target)?;
                    one(Value::Table(lua.create_sequence_from(tags)?))
                }
                _ => {
                    let tag = arg_str(args, tag_at, "string")?;
                    match name {
                        "AddTag" => add_tag(lua, target, &tag).and_then(|_| none()),
                        "RemoveTag" => remove_tag(lua, target, &tag).and_then(|_| none()),
                        _ => one(Value::Boolean(
                            tags_of(lua, target)?.iter().any(|t| *t == tag),
                        )),
                    }
                }
            }
        }
        "GetTagged" => {
            let tag = arg_str(args, 1, "string")?;
            let found = tagged(lua, &tag)?;
            one(Value::Table(list(lua, found)?))
        }
        "GetAllTags" => {
            let mut all: Vec<String> = {
                let w = world(lua)?;
                let d = w.d();
                d.dom
                    .descendants()
                    .filter_map(|i| match i.properties.get(&ustr("Tags")) {
                        Some(Variant::Tags(t)) => {
                            Some(t.iter().map(|s| s.to_string()).collect::<Vec<_>>())
                        }
                        _ => None,
                    })
                    .flatten()
                    .collect()
            };
            all.sort();
            all.dedup();
            one(Value::Table(lua.create_sequence_from(all)?))
        }
        "GetInstanceAddedSignal" | "GetInstanceRemovedSignal" => {
            let tag = arg_str(args, 1, "string")?;
            let ev = if name == "GetInstanceAddedSignal" {
                format!("TagAdded:{tag}")
            } else {
                format!("TagRemoved:{tag}")
            };
            one(signal(lua, this, &ev)?)
        }
        "GetTeams" => {
            let kids: Vec<Ref> = {
                let w = world(lua)?;
                let d = w.d();
                d.dom
                    .get_by_ref(this)
                    .map(|i| {
                        i.children()
                            .iter()
                            .copied()
                            .filter(|c| d.class_of(*c).is_some_and(|k| k == "Team"))
                            .collect()
                    })
                    .unwrap_or_default()
            };
            one(Value::Table(list(lua, kids)?))
        }
        "GetPlayers" => {
            // Team:GetPlayers(): players whose Team is this one.
            let found: Vec<Ref> = {
                let w = world(lua)?;
                let d = w.d();
                let players = d.find_child(d.dom.root_ref(), "Players");
                players
                    .and_then(|p| d.dom.get_by_ref(p))
                    .map(|i| i.children().iter().copied().filter(|p| matches!(d.prop(*p, "Team"), Some(Variant::Ref(t)) if t == this)).collect())
                    .unwrap_or_default()
            };
            one(Value::Table(list(lua, found)?))
        }
        "GetPlayerByUserId" => {
            let id = num(&arg(args, 1)).unwrap_or(-1.0) as i64;
            let found = {
                let w = world(lua)?;
                let d = w.d();
                d.dom.get_by_ref(this).and_then(|i| {
                    i.children().iter().copied().find(
                        |p| matches!(d.prop(*p, "UserId"), Some(Variant::Int64(u)) if u == id),
                    )
                })
            };
            one(opt_ref_value(lua, found)?)
        }
        "GetMouse" => {
            let existing = world(lua)?.input.mouse;
            let m = match existing {
                Some(m) if world(lua)?.d().exists(m) => m,
                _ => {
                    let m = create_any(lua, "PlayerMouse")?;
                    world(lua)?.input.mouse = Some(m);
                    m
                }
            };
            one(inst_value(lua, m)?)
        }
        "GetNetworkPing" => one(Value::Number(0.0)),
        "DistanceFromCharacter" => {
            let p = vec3_arg(args, 1, "DistanceFromCharacter")?;
            let at = character_root(world(lua)?.d(), this);
            one(Value::Number(
                at.map(|a| (a - p).length() as f64).unwrap_or(0.0),
            ))
        }
        "JSONEncode" => {
            let j = to_json(&arg(args, 1), 0)?;
            one(Value::String(lua.create_string(
                serde_json::to_string(&j).map_err(|e| rt_err(e.to_string()))?,
            )?))
        }
        "JSONDecode" => {
            let s = arg_str(args, 1, "string")?;
            let j: serde_json::Value =
                serde_json::from_str(&s).map_err(|e| rt_err(format!("Can't parse JSON: {e}")))?;
            one(from_json(lua, &j)?)
        }
        "GenerateGUID" => {
            let g = guid();
            let wrap = !matches!(arg(args, 1), Value::Boolean(false));
            one(Value::String(lua.create_string(if wrap {
                g
            } else {
                g.trim_matches(['{', '}']).to_string()
            })?))
        }
        "UrlEncode" => {
            let s = arg_str(args, 1, "string")?;
            let enc: String = s
                .bytes()
                .map(|b| {
                    if b.is_ascii_alphanumeric() || b"-_.~".contains(&b) {
                        (b as char).to_string()
                    } else {
                        format!("%{b:02X}")
                    }
                })
                .collect();
            one(Value::String(lua.create_string(enc)?))
        }
        "GetMinutesAfterMidnight" => one(Value::Number(clock_hours(lua, this)? as f64 * 60.0)),
        "SetMinutesAfterMidnight" => {
            let m = num(&arg(args, 1)).ok_or_else(|| rt_err("SetMinutesAfterMidnight: number expected"))?;
            set_clock(lua, this, m as f32 / 60.0)?;
            none()
        }
        "GetMouseLocation" => {
            let xy = world(lua)?.input.mouse_xy;
            one(Value::UserData(
                lua.create_userdata(LVector2(glam::Vec2::new(xy[0], xy[1])))?,
            ))
        }
        "IsMouseButtonPressed" => one(Value::Boolean(false)),
        "GetKeysPressed" => one(Value::Table(lua.create_table()?)),
        "GetCore" => one(Value::Nil),
        "RemoveDefaultLoadingScreen" => none(),
        "RegisterKeyframeSequence" | "RegisterActiveKeyframeSequence" => {
            // A temporary content id for the sequence (Studio hands out a hash id).
            let seq = opt_inst(&arg(args, 1))
                .ok_or_else(|| rt_err(format!("{name}: KeyframeSequence expected")))?;
            let id = format!("rbxtemp://{}", sound_key_of(seq));
            one(Value::String(lua.create_string(id)?))
        }
        "GetKeyframeSequenceAsync" => {
            let r = create_any(lua, "KeyframeSequence")?;
            one(inst_value(lua, r)?)
        }
        "CreateMeshPartAsync" => {
            // A MeshPart showing the mesh, sized to the mesh's own bounds.
            // A Content: a URI, or an EditableMesh (editable.rs snapshots it
            // as a generated mesh, sized to its bounds).
            let mut editable_size = None;
            // (an EditableMesh source: linked to the part, skinned when boned)
            let mut editable_src: Option<rbx_dom_weak::types::Content> = None;
            let content = match arg(args, 1) {
                Value::String(s) => s.to_str()?.to_string(),
                Value::Number(n) => format!("rbxassetid://{}", n as u64),
                v => match opt_ud::<super::editable::LContent>(&v) {
                    Some(c) => match super::editable::mesh_part_source(&c.0) {
                        Some(r) => {
                            let (id, size) = r?;
                            editable_size = Some(size);
                            editable_src = Some(c.0.clone());
                            id
                        }
                        None => c.0.as_uri().map(str::to_string).ok_or_else(|| rt_err("CreateMeshPartAsync: empty Content"))?,
                    },
                    None => return Err(rt_err("CreateMeshPartAsync: mesh id expected")),
                },
            };
            let size = editable_size.unwrap_or_else(|| {
                rustudio_core::assets::asset_ref(&content)
                    .and_then(|a| rustudio_core::assets::local_asset_path(a.id))
                    .and_then(|p| std::fs::read(p).ok())
                    // Else any content (place-local files, cache, download):
                    // Studio's call yields until the mesh is in.
                    .or_else(|| rustudio_core::assets::content_blocking(&content).ok().map(|b| b.to_vec()))
                    .and_then(|b| rustudio_core::mesh::parse(&b).ok())
                    .map(|m| (m.max - m.min).max(Vec3::splat(0.05)))
                    .unwrap_or(Vec3::ONE)
            });
            // AssetService takes an options table (CollisionFidelity,
            // RenderFidelity); InsertService the two enums in turn (API dump).
            let opts = arg(args, 2);
            let fidelity = |key: &str| -> Option<u32> {
                opt_ud::<LEnumItem>(&ud_value(&opts, key)).map(|e| e.value)
            };
            let (collision, render) = match opt_ud::<LEnumItem>(&opts) {
                Some(c) => (Some(c.value), opt_ud::<LEnumItem>(&arg(args, 3)).map(|e| e.value)),
                None => (fidelity("CollisionFidelity"), fidelity("RenderFidelity")),
            };
            let r = create_any(lua, "MeshPart")?;
            {
                let mut w = world(lua)?;
                if let Some(i) = w.dm().dom.get_by_ref_mut(r) {
                    i.properties
                        .insert(ustr("MeshId"), Variant::ContentId(content.clone().into()));
                    i.properties.insert(
                        ustr("Size"),
                        Variant::Vector3(Vector3::new(size.x, size.y, size.z)),
                    );
                    i.properties.insert(
                        ustr("MeshSize"),
                        Variant::Vector3(Vector3::new(size.x, size.y, size.z)),
                    );
                    i.properties.insert(
                        ustr("InitialSize"),
                        Variant::Vector3(Vector3::new(size.x, size.y, size.z)),
                    );
                    let skinned = match &editable_src {
                        Some(c) => super::editable::content_skinned(c),
                        None => rustudio_core::assets::asset_ref(&content)
                            .and_then(|a| rustudio_core::assets::local_asset_path(a.id))
                            .and_then(|p| std::fs::read(p).ok())
                            .or_else(|| rustudio_core::assets::content_blocking(&content).ok().map(|b| b.to_vec()))
                            .and_then(|b| rustudio_core::mesh::parse(&b).ok())
                            .is_some_and(|m| m.skin.is_some()),
                    };
                    i.properties.insert(ustr("HasSkinnedMesh"), Variant::Bool(skinned));
                    if let Some(c) = collision {
                        i.properties.insert(
                            ustr("CollisionFidelity"),
                            Variant::Enum(rbx_dom_weak::types::Enum::from_u32(c)),
                        );
                    }
                    if let Some(c) = render {
                        i.properties.insert(
                            ustr("RenderFidelity"),
                            Variant::Enum(rbx_dom_weak::types::Enum::from_u32(c)),
                        );
                    }
                }
            }
            if let Some(c) = &editable_src {
                super::editable::link(c, r, &content);
            }
            one(inst_value(lua, r)?)
        }
        "GetHumanoidDescriptionFromUserId" | "GetHumanoidDescriptionFromOutfitId" => {
            let r = create_any(lua, "HumanoidDescription")?;
            // The signed-in person's own user id: their avatar, as a
            // signed-in Studio fetches it (body colours, scales; the model
            // made from it is dressed). Others / offline: a blank description
            // (black body), as the offline Studio answers.
            let uid = num(&arg(args, 1)).unwrap_or(-1.0);
            if name == "GetHumanoidDescriptionFromUserId" {
                let mut w = world(lua)?;
                if let Some(look) = w.avatar.clone().filter(|l| l.user_id as f64 == uid) {
                    let c = look.colors;
                    let d = w.dm();
                    if let Some(i) = d.dom.get_by_ref_mut(r) {
                        let col = |v: [u8; 3]| Variant::Color3(rbx_types::Color3::new(v[0] as f32 / 255.0, v[1] as f32 / 255.0, v[2] as f32 / 255.0));
                        for (k, v) in [("HeadColor", c.head), ("TorsoColor", c.torso), ("LeftArmColor", c.left_arm),
                            ("RightArmColor", c.right_arm), ("LeftLegColor", c.left_leg), ("RightLegColor", c.right_leg)] {
                            i.properties.insert(ustr(k), col(v));
                        }
                        let sc = look.scales;
                        for (k, v) in [("HeightScale", sc.height), ("WidthScale", sc.width), ("DepthScale", sc.depth), ("HeadScale", sc.head),
                            ("BodyTypeScale", sc.body_type), ("ProportionScale", sc.proportion)] {
                            i.properties.insert(ustr(k), Variant::Float32(v));
                        }
                        // Worn animation packages (IdleAnimation … MoodAnimation).
                        for (kind, id, _) in &look.animations {
                            i.properties.insert(ustr(kind), Variant::Int64(*id as i64));
                        }
                    }
                    w.avatar_descs.insert(r);
                }
            }
            one(inst_value(lua, r)?)
        }
        "CreateHumanoidModelFromDescription" | "CreateHumanoidModelFromUserId" => {
            let r15 = match arg(args, 2) {
                v @ Value::UserData(_) => opt_ud::<LEnumItem>(&v).is_none_or(|e| e.name != "R6"),
                _ => true,
            };
            let rig = if r15 {
                crate::rig::RigType::R15
            } else {
                crate::rig::RigType::R6
            };
            // The description's body colours and scales. A new
            // HumanoidDescription's colours are black: Studio's offline
            // lobby avatar (Place A, from Instance.new("HumanoidDescription")) has
            // every body part Color (0, 0, 0) (tools/fidelity place_a capture).
            let desc = if name == "CreateHumanoidModelFromDescription" {
                opt_inst(&arg(args, 1))
            } else {
                None
            };
            let (colors, scales) = {
                let w = world(lua)?;
                let d = w.d();
                let color = |k: &str| match desc.and_then(|r| d.prop(r, k)) {
                    Some(Variant::Color3(c)) => [c.r, c.g, c.b].map(|v| (v.clamp(0.0, 1.0) * 255.0).round() as u8),
                    Some(Variant::Color3uint8(c)) => [c.r, c.g, c.b],
                    _ => [0, 0, 0],
                };
                let scale = |k: &str| match desc.and_then(|r| d.prop(r, k)) {
                    Some(Variant::Float32(v)) => v,
                    Some(Variant::Float64(v)) => v as f32,
                    _ => 1.0,
                };
                (
                    crate::rig::BodyColors {
                        head: color("HeadColor"),
                        torso: color("TorsoColor"),
                        left_arm: color("LeftArmColor"),
                        right_arm: color("RightArmColor"),
                        left_leg: color("LeftLegColor"),
                        right_leg: color("RightLegColor"),
                    },
                    crate::rig::Scales {
                        height: scale("HeightScale"),
                        width: scale("WidthScale"),
                        depth: scale("DepthScale"),
                        head: scale("HeadScale"),
                        body_type: match desc.and_then(|r| d.prop(r, "BodyTypeScale")) {
                            Some(Variant::Float32(v)) => v,
                            Some(Variant::Float64(v)) => v as f32,
                            _ => crate::rig::Scales::default().body_type,
                        },
                        proportion: match desc.and_then(|r| d.prop(r, "ProportionScale")) {
                            Some(Variant::Float32(v)) => v,
                            Some(Variant::Float64(v)) => v as f32,
                            _ => crate::rig::Scales::default().proportion,
                        },
                    },
                )
            };
            let (colors, scales) = if desc.is_some() {
                (colors, scales)
            } else {
                (Default::default(), Default::default())
            };
            // Roblox's rig has its Animator under the Humanoid already
            // (Place D's second admin script preview: humanoid:WaitForChild("Animator")
            // warned of an infinite yield).
            let humanoid = InstanceBuilder::new("Humanoid")
                .with_name("Humanoid")
                .with_child(InstanceBuilder::new("Animator").with_name("Animator"));
            let r = crate::rig::insert_character(
                &mut world(lua)?.dm().dom,
                Ref::none(),
                "Player",
                rig,
                &colors,
                &scales,
                humanoid,
            );
            let account_look = {
                let mut w = world(lua)?;
                let dressed = match desc.filter(|d| w.avatar_descs.contains(d)).and(w.avatar.clone()) {
                    Some(look) => {
                        crate::avatar::dress(w.dm(), r, &look);
                        true
                    }
                    None => false,
                };
                // Which body parts collide, as on a spawned player (play.rs; Studio
                // builds both the same way). insert_character leaves every part
                // CanCollide false, so a rig made here (an NPC, the solver
                // harness's scenes) had no contacts at all: it walked through
                // walls and, dead, fell through the floor (solver/HARNESS.md §3).
                let dom = &mut w.dm().dom;
                let body: Vec<(Ref, bool)> = dom
                    .get_by_ref(r)
                    .map(|i| i.children().to_vec())
                    .unwrap_or_default()
                    .into_iter()
                    .filter_map(|c| {
                        let i = dom.get_by_ref(c)?;
                        rustudio_core::doc::is_a(&i.class, "BasePart").then(|| (c, crate::rig::studio_collides(&i.name, rig)))
                    })
                    .collect();
                for (c, on) in body {
                    if let Some(i) = dom.get_by_ref_mut(c) {
                        i.properties.insert(ustr("CanCollide"), Variant::Bool(on));
                        // Every body MeshPart collides as its box (Studio 0.747
                        // 2026-11-14, default description R15: CollisionFidelity Box on all
                        // 15 MeshParts; lane DEATH). As a convex decomposition the
                        // LowerTorso's rounded top let a step's top face lift the character
                        // (probe T5_step2.5's periodic +0.014).
                        if i.class == "MeshPart" {
                            i.properties.insert(
                                ustr("CollisionFidelity"),
                                Variant::Enum(rbx_dom_weak::types::Enum::from_u32(2)),
                            );
                        }
                    }
                }
                // Studio's R15 model from a description is already joint-upgraded, as a
                // spawned character is (measured in Studio-11-13, default
                // description: 15 AnimationConstraints, 14 BallSocketConstraints, 19
                // NoCollisionConstraints, no Motor6D).
                if rig == crate::rig::RigType::R15 {
                    crate::rig::upgrade_rig(dom, r);
                }
                dressed
            };
            // Any other description: its accessories by asset id.
            if let (false, Some(d)) = (account_look, desc) {
                dress_from_description(lua, r, d)?;
            }
            one(inst_value(lua, r)?)
        }
        "GetLastInputType" => match LEnumItem::from_name("UserInputType", "Keyboard") {
            Some(i) => one(crate::ltypes::enum_value(lua, i)?),
            None => one(Value::Nil),
        },
        // The slots assigned by the Play session.
        "GetConnectedGamepads" | "GetNavigationGamepads" => {
            let n = world(lua)?.input.gamepads;
            let t = lua.create_table()?;
            for i in (0..8).filter(|i| n & (1 << i) != 0) {
                if let Some(e) = LEnumItem::from_name("UserInputType", &format!("Gamepad{}", i + 1)) {
                    t.push(crate::ltypes::enum_value(lua, e)?)?;
                }
            }
            one(Value::Table(t))
        }
        "GetGamepadConnected" => {
            let n = world(lua)?.input.gamepads;
            let slot = opt_ud::<LEnumItem>(&arg(args, 1))
                .and_then(|e| e.name.strip_prefix("Gamepad").and_then(|k| k.parse::<usize>().ok()));
            one(Value::Boolean(slot.is_some_and(|k| (1..=8).contains(&k) && n & (1 << (k - 1)) != 0)))
        }
        "FillBlock" | "FillBall" | "FillCylinder" | "FillWedge" | "FillRegion" | "Clear"
        | "ReplaceMaterial" | "SetMaterialColor" | "GetMaterialColor" | "WriteVoxels" | "ReadVoxels"
        | "WriteVoxelChannels" | "ReadVoxelChannels" | "CopyRegion" | "PasteRegion" | "CellCenterToWorld"
        | "CellCornerToWorld" | "WorldToCell" | "WorldToCellPreferSolid" | "WorldToCellPreferEmpty"
            if inst_meta(lua, this)?.0 == "Terrain" =>
        {
            terrain_call(lua, name, this, args)
        }
        "SetCell" => {
            // (no behaviour: counted against scripting_api's coverage)
            crate::lua_bridge::gap(lua, "call-unimplemented", "Terrain", name);
            let first = !world(lua)?
                .output
                .iter()
                .any(|(_, t)| t.starts_with("Terrain editing"));
            if first {
                world(lua)?.output.push((Level::Warn, format!("Terrain editing from scripts (Terrain:{name}) isn't supported in rustudio yet; the terrain stays as saved")));
            }
            none()
        }
        // StudioTestService:GetTestArgs: the arguments of a test run started
        // with ExecuteRunModeAsync / ExecutePlayModeAsync; a plain Play has
        // none (nil), which is what Place A's DevTest and Progression check for.
        "GetTestArgs" => none(),
        _ => {
            let class = inst_meta(lua, this)?.0;
            super::stubs::unimplemented_call(lua, this, &class, name)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn layout_signals_follow_live_connections_and_waiters() {
        let rt = Runtime::new().unwrap();
        let mut w = World::new(Document::new_place(), RunMode::Play);
        w.side = crate::runtime::Side::Client;
        rt.enter(w);
        rt.bind_globals().unwrap();
        let lua = &rt.lua;
        lua.load(r#"
            screen = Instance.new("ScreenGui", workspace)
            holder = Instance.new("Frame", screen)
            frame = Instance.new("Frame", holder)
            layout = Instance.new("UIListLayout", holder)
            part = Instance.new("Part", workspace)
            part.AncestryChanged:Connect(function() end)
            size, pos, rotation, content, viewport = 0, 0, 0, 0, 0
            connection = frame:GetPropertyChangedSignal("AbsoluteSize"):Connect(function() size += 1 end)
            frame:GetPropertyChangedSignal("AbsolutePosition"):Once(function() pos += 1 end)
            task.spawn(function()
                frame:GetPropertyChangedSignal("AbsoluteRotation"):Wait()
                rotation += 1
            end)
            layoutConnection = layout:GetPropertyChangedSignal("AbsoluteContentSize"):Connect(function() content += 1 end)
            workspace.CurrentCamera:GetPropertyChangedSignal("ViewportSize"):Connect(function() viewport += 1 end)
        "#).exec().unwrap();
        let frame = opt_inst(&lua.globals().get::<Value>("frame").unwrap()).unwrap();
        for (view_scale, box_scale) in [(1.0, 1.0), (2.0, 2.0), (2.0, 3.0), (2.0, 3.0)] {
            let mut w = world(lua).unwrap();
            assert_eq!(w.gui_listening.len(), 1);
            assert_eq!(w.layout_listening.len(), 1);
            w.input.view_size = [800.0 * view_scale, 600.0];
            w.input.gui_layout.insert(frame, crate::gui::GuiBox {
                pos: [10.0 * box_scale, 0.0],
                size: [100.0 * box_scale, 50.0],
                rot: 15.0 * box_scale,
                ..Default::default()
            });
            drop(w);
            fire_layout_changes(lua).unwrap();
            rt.dispatch();
        }
        lua.load(r#"
            assert(size == 3 and pos == 1 and rotation == 1 and content == 3 and viewport == 1,
                string.format("%d %d %d %d %d", size, pos, rotation, content, viewport))
            connection:Disconnect()
            layoutConnection:Disconnect()
        "#).exec().unwrap();
        assert!(world(lua).unwrap().gui_listening.is_empty());
        assert!(world(lua).unwrap().layout_listening.is_empty());
        lua.load(r#"
            frame.Changed:Connect(function() end)
            layout.Changed:Connect(function() end)
            screen:Destroy()
        "#).exec().unwrap();
        assert!(world(lua).unwrap().gui_listening.is_empty());
        assert!(world(lua).unwrap().layout_listening.is_empty());
    }

    /// Studio's ProximityPrompt sizes its BillboardGui from an AutomaticSize
    /// X frame's AbsoluteSize two RenderSteppeds after building it. With the
    /// Play worker the walk's boxes reach scripts a step later: an undrawn
    /// AutomaticSize object is measured on the read, not left 0 wide (which
    /// made the prompt 0 wide with a NaN SizeOffset, and invisible).
    #[test]
    fn undrawn_automatic_size_is_measured_on_read() {
        crate::gui::prime_text_measure();
        let rt = Runtime::new().unwrap();
        let mut w = World::new(Document::new_place(), RunMode::Play);
        w.side = crate::runtime::Side::Client;
        w.input.view_size = [1280.0, 800.0];
        rt.enter(w);
        rt.bind_globals().unwrap();
        rt.lua.load(r#"
            local ui = Instance.new("BillboardGui")
            ui.Size = UDim2.fromOffset(72, 72)
            local frame = Instance.new("Frame", ui)
            frame.Size = UDim2.fromScale(0, 1)
            frame.AutomaticSize = Enum.AutomaticSize.X
            local pad = Instance.new("UIPadding", frame)
            pad.PaddingRight = UDim.new(0, 24)
            local label = Instance.new("TextLabel", frame)
            label.Size = UDim2.fromScale(0, 1)
            label.AutomaticSize = Enum.AutomaticSize.X
            label.TextSize = 19
            label.Text = "Take blood bag"
            ui.Parent = workspace
            local text = label.TextBounds.X
            assert(text > 40, "measured text " .. text)
            assert(math.abs(label.AbsoluteSize.X - text) < 0.5, label.AbsoluteSize.X .. " vs " .. text)
            assert(label.AbsoluteSize.Y == 72, "label height " .. label.AbsoluteSize.Y)
            assert(math.abs(frame.AbsoluteSize.X - (text + 24)) < 0.5, "frame " .. frame.AbsoluteSize.X)
            assert(frame.AbsoluteSize.Y == 72, "frame height " .. frame.AbsoluteSize.Y)
            -- A plain Frame keeps its Size (scale 0 is 0 wide).
            local plain = Instance.new("Frame", ui)
            plain.Size = UDim2.fromScale(0, 1)
            assert(plain.AbsoluteSize.X == 0)
        "#).exec().unwrap();
    }

    #[test]
    fn layout_notifications_coalesce_writes_and_follow_removed_children() {
        let rt = crate::runtime::Runtime::new().unwrap();
        let mut w = World::new(Document::new_place(), RunMode::Play);
        w.side = crate::runtime::Side::Client;
        rt.enter(w);
        rt.bind_globals().unwrap();
        let lua = &rt.lua;
        let exec = |source| lua.load(source).exec().unwrap();
        let pass = || {
            fire_layout_changes(lua).unwrap();
            rt.dispatch();
        };
        exec(r#"
            local screen = Instance.new("ScreenGui", workspace)
            local holder = Instance.new("Frame", screen)
            frame = Instance.new("Frame", holder)
            frame.Size = UDim2.fromOffset(10, 20)
            layout = Instance.new("UIListLayout", holder)
            light = Instance.new("PointLight", workspace)
            part = Instance.new("Part", workspace)
            sizes, contents = {}, {}
            frame:GetPropertyChangedSignal("AbsoluteSize"):Connect(function()
                table.insert(sizes, frame.AbsoluteSize.X)
            end)
            layout:GetPropertyChangedSignal("AbsoluteContentSize"):Connect(function()
                table.insert(contents, layout.AbsoluteContentSize.X)
            end)
        "#);
        pass();
        exec(r#"
            assert(table.concat(sizes, ",") == "10")
            assert(table.concat(contents, ",") == "10")
            for _, size in {20, 30, 40} do
                frame.Size = UDim2.fromOffset(size, 20)
            end
            assert(#sizes == 1 and #contents == 1)
        "#);
        pass();
        exec(r#"
            assert(table.concat(sizes, ",") == "10,40")
            assert(table.concat(contents, ",") == "10,40")
            light.Brightness = 2.25
            part.Size = Vector3.new(4, 5, 6)
        "#);
        pass();
        exec(r#"
            assert(#sizes == 2 and #contents == 2)
            frame.Parent = nil
        "#);
        pass();
        exec(r#"assert(table.concat(contents, ",") == "10,40,0")"#);
    }

    /// Terrain's chunk triangle index answers rays and swept boxes exactly
    /// as a scan of every triangle in order does (nearest hit, the first
    /// triangle among equal distances; the same candidates in order).
    #[test]
    fn terrain_tri_index_matches_a_scan() {
        let mut seed = 12345u64;
        let mut rnd = move || {
            seed = seed.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
            ((seed >> 33) as f32) / ((1u64 << 31) as f32)
        };
        // A bumpy grid surface (shared edges: equal distances) plus loose
        // triangles both ways round.
        let mut tris: Vec<([Vec3; 3], [u8; 3])> = Vec::new();
        let h = |x: i32, z: i32| ((x * 7 + z * 13) % 5) as f32 * 0.5;
        for x in 0..32 {
            for z in 0..32 {
                let p = |dx: i32, dz: i32| Vec3::new(((x + dx) * 4) as f32, h(x + dx, z + dz), ((z + dz) * 4) as f32);
                tris.push(([p(0, 0), p(0, 1), p(1, 0)], [1, 2, 3]));
                tris.push(([p(1, 0), p(0, 1), p(1, 1)], [4, 5, 6]));
            }
        }
        for _ in 0..400 {
            let c = Vec3::new(rnd() * 128.0, rnd() * 40.0 - 5.0, rnd() * 128.0);
            let t = [c, c + Vec3::new(rnd() * 9.0 - 4.5, rnd() * 9.0 - 4.5, rnd() * 9.0 - 4.5), c + Vec3::new(rnd() * 9.0 - 4.5, rnd() * 9.0 - 4.5, rnd() * 9.0 - 4.5)];
            tris.push((t, [7, 8, 9]));
        }
        let index = TriIndex::build(&tris);
        let scan = |o: Vec3, d: Vec3, t0: f32, t1: f32| {
            let mut best: Option<(f32, usize, Vec3)> = None;
            for (i, tri) in tris.iter().enumerate() {
                if let Some((dist, w)) = ray_triangle(o, d, &tri.0) {
                    if dist >= t0 && dist <= t1 && best.is_none_or(|(bd, ..)| dist < bd) {
                        best = Some((dist, i, w));
                    }
                }
            }
            best
        };
        let mut hits = 0;
        for k in 0..3000 {
            let o = Vec3::new(rnd() * 160.0 - 16.0, rnd() * 60.0 - 10.0, rnd() * 160.0 - 16.0);
            let d = match k % 4 {
                0 => Vec3::NEG_Y,
                1 => Vec3::new(rnd() - 0.5, -rnd(), rnd() - 0.5).normalize_or(Vec3::NEG_Y),
                2 => Vec3::new(rnd() - 0.5, rnd() - 0.5, rnd() - 0.5).normalize_or(Vec3::X),
                // through grid vertices and edges exactly
                _ => {
                    let o2 = Vec3::new((k % 33 * 4) as f32, 30.0, ((k / 33) % 33 * 4) as f32);
                    let got = index.ray(&tris, o2, Vec3::NEG_Y, 0.0, 100.0);
                    assert_eq!(got.map(|g| (g.0.to_bits(), g.1)), scan(o2, Vec3::NEG_Y, 0.0, 100.0).map(|g| (g.0.to_bits(), g.1)), "vertex ray {o2}");
                    continue;
                }
            };
            let (t0, t1) = if k % 7 == 0 { (5.0, 40.0) } else { (0.0, 300.0) };
            let got = index.ray(&tris, o, d, t0, t1);
            let want = scan(o, d, t0, t1);
            assert_eq!(got.map(|g| (g.0.to_bits(), g.1)), want.map(|g| (g.0.to_bits(), g.1)), "ray {o} {d}");
            hits += usize::from(want.is_some());
        }
        assert!(hits > 500, "{hits}");
        for _ in 0..500 {
            let lo = Vec3::new(rnd() * 140.0 - 10.0, rnd() * 30.0 - 10.0, rnd() * 140.0 - 10.0);
            let hi = lo + Vec3::new(rnd() * 30.0, rnd() * 30.0, rnd() * 30.0);
            let near = |p: &[Vec3; 3]| {
                let (tl, th) = (p[0].min(p[1]).min(p[2]), p[0].max(p[1]).max(p[2]));
                tl.cmple(hi).all() && th.cmpge(lo).all()
            };
            let want: Vec<usize> = (0..tris.len()).filter(|i| near(&tris[*i].0)).collect();
            let got: Vec<usize> = index.in_box(lo, hi).into_iter().map(|i| i as usize).filter(|i| near(&tris[*i].0)).collect();
            assert_eq!(got, want);
        }
    }

    /// game.ItemChanged fires for changes while connected and not after; a
    /// DataModel listened to for something else makes no ItemChanged; a
    /// Position set through an attachment's frame still fires Changed.
    #[test]
    fn item_changed_only_while_connected() {
        let (out, _) = run(r#"
            local part = Instance.new("Part") part.Parent = workspace
            local att = Instance.new("Attachment") att.Parent = part
            local n, a = 0, 0
            game.DescendantAdded:Connect(function() end)
            part.Transparency = 0.2
            local c = game.ItemChanged:Connect(function(i, p) if i == part then n += 1 end end)
            att:GetPropertyChangedSignal("Position"):Connect(function() a += 1 end)
            part.Transparency = 0.5
            att.Position = Vector3.new(1, 2, 3)
            task.wait()
            print("ic1", n, a, tostring(att.Position))
            c:Disconnect()
            part.Transparency = 0.7
            task.wait()
            print("ic2", n)
        "#);
        assert!(out.iter().any(|l| l.starts_with("ic1 1 1 1, 2, 3  ")), "{out:?}");
        assert!(out.iter().any(|l| l.starts_with("ic2 1  ")), "{out:?}");
    }

    /// The query cache follows moved parts through a frame of Bone and
    /// Attachment writes (marks of non-parts passed over), and
    /// RaycastParams' fields are read as the script set them.
    #[test]
    fn query_cache_follows_parts_among_rig_writes() {
        let (out, _) = run(r#"
            local p = Instance.new("Part") p.Anchored = true p.Size = Vector3.new(4, 1, 4) p.Position = Vector3.new(300, 10, 0) p.Parent = workspace
            local q = Instance.new("Part") q.Anchored = true q.Size = Vector3.new(4, 1, 4) q.Position = Vector3.new(350, 10, 0) q.Parent = workspace
            local holder = Instance.new("Part") holder.Anchored = true holder.CanQuery = false holder.CanCollide = false holder.Parent = workspace
            local bones = {}
            for i = 1, 100 do local b = Instance.new("Bone") b.Parent = holder table.insert(bones, b) end
            local att = Instance.new("Attachment") att.Parent = holder
            -- (away from the place's SpawnLocation)
            local function ray(x, params) local r = workspace:Raycast(Vector3.new(x + 300, 20, 0), Vector3.new(0, -20, 0), params) return r and r.Instance.Name or "nil" end
            p.Name = "P" q.Name = "Q"
            local s = { ray(0), ray(50) }
            for f = 1, 3 do
                for _, b in bones do b.CFrame = CFrame.new(0, f, 0) end
                att.Position = Vector3.new(f, 0, 0)
                p.CFrame = CFrame.new(300, 10, 100 * f)
                table.insert(s, ray(0))
                p.CFrame = CFrame.new(300, 10, 0)
                q.Size = Vector3.new(4, 1, 0.1 * f)
                table.insert(s, ray(0) .. ray(50))
            end
            local ex = RaycastParams.new() ex.FilterType = Enum.RaycastFilterType.Exclude ex.FilterDescendantsInstances = { p }
            local inc = RaycastParams.new() inc.FilterType = Enum.RaycastFilterType.Include inc.FilterDescendantsInstances = { q }
            local g = RaycastParams.new() g.CollisionGroup = ""
            table.insert(s, ray(0, ex) .. ray(50, ex) .. ray(0, inc) .. ray(50, inc) .. ray(0, g))
            p.CanQuery = false p.CanCollide = false
            table.insert(s, ray(0))
            print(table.concat(s, " "))
        "#);
        assert!(out.iter().any(|l| l.starts_with("P Q nil PQ nil PQ nil PQ nilQnilQP nil  ")), "{out:?}");
    }

    /// A part far out saturates its cell indices; the width must not wrap
    /// to a small count (that walked billions of cells: a live place freeze).
    #[test]
    fn query_grid_far_part_goes_big() {
        let (a, b) = QueryGrid::cell_range(Vec3::splat(-4.0e10), Vec3::splat(1.0));
        assert_eq!(a[0], i32::MIN);
        // the old i32 width: wraps instead of being huge
        assert!(b[0].wrapping_sub(a[0]).wrapping_add(1) <= mech::QUERY_GRID_MAX_CELLS as i32);
        assert!(QueryGrid::cells_in(a, b) > mech::QUERY_GRID_MAX_CELLS as i64);
        let (a, b) = QueryGrid::cell_range(Vec3::splat(-4.0e10), Vec3::splat(4.0e10));
        assert!(QueryGrid::cells_in(a, b) > mech::QUERY_GRID_MAX_CELLS as i64);
        assert_eq!(QueryGrid::cells_in([0, 0, 0], [1, 1, 1]), 8);
    }

    fn run(src: &str) -> (Vec<String>, rustudio_core::terrain::Voxels) {
        let mut d = Document::new_place();
        let sss = d.find_child(d.dom.root_ref(), "ServerScriptService").unwrap();
        let s = d.dom.insert(sss, InstanceBuilder::new("Script").with_name("T").with_property("Source", src.to_string()));
        let rt = Runtime::new().unwrap();
        rt.enter(World::new(d, RunMode::Run));
        rt.bind_globals().unwrap();
        rt.run_script(s);
        for _ in 0..3 {
            rt.step(1.0 / 60.0);
        }
        let w = rt.leave();
        let v = match rustudio_core::terrain::Voxels::from_dom(&w.d().dom) {
            Some(Ok(v)) => v,
            _ => rustudio_core::terrain::Voxels::empty(),
        };
        (w.output.into_iter().map(|(_, t)| t).collect(), v)
    }

    /// Queries never return a removed part (Laser Tag: castRays indexed
    /// `result.Instance.Parent` and hit "an Instance that no longer exists"):
    /// removed in the same frame by every route, and even a removal the
    /// change journal never saw (freed or moved out of the Workspace behind
    /// its back) is not returned by rays, sweeps or overlap queries.
    #[test]
    fn queries_never_return_removed_parts() {
        let mut d = Document::new_place();
        let ws = d.workspace().unwrap();
        for (r, _) in d.dom.get_by_ref(ws).unwrap().children().to_vec().into_iter().zip(0..) {
            if d.class_of(r).is_some_and(|c| c != "Camera") {
                d.dom.destroy(r);
            }
        }
        let sss = d.find_child(d.dom.root_ref(), "ServerScriptService").unwrap();
        let first = r#"
local function f(r) return r and r.Instance:GetFullName() or "nil" end
local function wall(name, x)
    local m = Instance.new("Model") m.Name = name
    local p = Instance.new("Part") p.Name = "P" p.Anchored = true p.Size = Vector3.new(4, 4, 1)
    p.CFrame = CFrame.new(x, 0, -5) p.Parent = m
    m.Parent = workspace
    return m, p
end
local function cast(x)
    return f(workspace:Raycast(Vector3.new(x, 0, 0), Vector3.new(0, 0, -20))),
        f(workspace:Spherecast(Vector3.new(x, 0, 0), 0.4, Vector3.new(0, 0, -20))),
        f(workspace:Blockcast(CFrame.new(x, 0, 0), Vector3.one, Vector3.new(0, 0, -20))),
        #workspace:GetPartBoundsInRadius(Vector3.new(x, 0, -5), 1)
end
-- Every removal route, cast in the same frame.
local routes = {
    destroy = function(m, p) m:Destroy() end,
    parentNil = function(m, p) m.Parent = nil end,
    partNil = function(m, p) p.Parent = nil end,
    clear = function(m, p) m:ClearAllChildren() end,
    away = function(m, p) m.Parent = game:GetService("ReplicatedStorage") end,
}
local x = 0
for name, remove in routes do
    x += 10
    local m, p = wall(name, x)
    print("before", name, cast(x))
    remove(m, p)
    print("after", name, cast(x))
end
-- Two walls the journal won't hear about (the test removes them in Rust).
local _, g = wall("Ghost", 100)
local _, a = wall("Away", 110)
print("cached", cast(100), cast(110))
"#;
        let second = r#"
local function f(r) return r and r.Instance:GetFullName() or "nil" end
print("ghost", f(workspace:Raycast(Vector3.new(100, 0, 0), Vector3.new(0, 0, -20))),
    f(workspace:Spherecast(Vector3.new(100, 0, 0), 0.4, Vector3.new(0, 0, -20))),
    #workspace:GetPartBoundsInRadius(Vector3.new(100, 0, -5), 1))
print("away", f(workspace:Raycast(Vector3.new(110, 0, 0), Vector3.new(0, 0, -20))),
    #workspace:GetPartBoundsInRadius(Vector3.new(110, 0, -5), 1))
"#;
        let s1 = d.dom.insert(sss, InstanceBuilder::new("Script").with_name("A").with_property("Source", first.to_string()));
        let s2 = d.dom.insert(sss, InstanceBuilder::new("Script").with_name("B").with_property("Source", second.to_string()));
        let rt = Runtime::new().unwrap();
        rt.enter(World::new(d, RunMode::Run));
        rt.bind_globals().unwrap();
        rt.run_script(s1);
        rt.step(1.0 / 60.0);
        let found = {
            // Behind the journal's back: no mark_tree.
            let mut w = rt.world_mut().unwrap();
            let doc = w.dm();
            let ws = doc.workspace().unwrap();
            let rs = doc.find_child(doc.dom.root_ref(), "ReplicatedStorage").unwrap();
            let named = |n: &str| {
                doc.dom.get_by_ref(ws)?.children().iter().copied().find(|c| doc.dom.get_by_ref(*c).is_some_and(|i| i.name == n))
            };
            match (named("Ghost"), named("Away")) {
                (Some(ghost), Some(away)) => {
                    doc.dom.destroy(ghost);
                    doc.dom.transfer_within(away, rs);
                    true
                }
                _ => false,
            }
        };
        if !found {
            panic!("{:?}", rt.leave().output);
        }
        rt.run_script(s2);
        rt.step(1.0 / 60.0);
        let out: Vec<String> = rt.leave().output.into_iter().map(|(_, t)| t.split("  —").next().unwrap_or("").trim().to_string()).collect();
        let text = out.join("\n");
        for name in ["destroy", "parentNil", "partNil", "clear", "away"] {
            let full = format!("Workspace.{name}.P");
            assert!(out.iter().any(|l| *l == format!("before {name} {full} {full} {full} 1")), "{name}: {text}");
            assert!(out.iter().any(|l| *l == format!("after {name} nil nil nil 0")), "{name}: {text}");
        }
        assert!(out.iter().any(|l| l.starts_with("cached Workspace.Ghost.P")), "{text}");
        assert!(out.iter().any(|l| *l == "ghost nil nil 0"), "{text}");
        assert!(out.iter().any(|l| *l == "away nil 0"), "{text}");
    }

    /// Laser Tag's blaster parity (checks/laser-tag-parity/raycast): a
    /// CanCollide part is queried whatever its CanQuery (the spawn-door
    /// forcefields, the invisible containment walls); a MeshPart is queried
    /// by its stored collision pieces (PhysicalConfigData) — rays at the
    /// pieces, sweeps at the pieces pulled in 0.05 — and a Box one by its box.
    #[test]
    fn queries_canquery_rule_and_collision_pieces() {
        use rbx_dom_weak::types::{CFrame, Matrix3, SharedString, Vector3 as V3};
        let mut d = Document::new_place();
        let ws = d.workspace().unwrap();
        let at = |x: f32| CFrame::new(V3::new(x, 0.0, 0.0), Matrix3::identity());
        let part = |name: &str, x: f32, cc: bool, cq: bool| {
            InstanceBuilder::new("Part")
                .with_name(name)
                .with_property("CFrame", at(x))
                .with_property("Size", V3::new(2.0, 2.0, 2.0))
                .with_property("Anchored", true)
                .with_property("CanCollide", cc)
                .with_property("CanQuery", cq)
        };
        d.dom.insert(ws, part("CollideNoQuery", 0.0, true, false));
        d.dom.insert(ws, part("Ghost", 10.0, false, false));
        let mesh = |name: &str, x: f32, blob: Vec<u8>| {
            InstanceBuilder::new("MeshPart")
                .with_name(name)
                .with_property("CFrame", at(x))
                .with_property("Size", V3::new(4.0, 2.0, 2.0))
                .with_property("InitialSize", V3::new(2.0, 2.0, 2.0))
                .with_property("Anchored", true)
                .with_property("PhysicalConfigData", SharedString::new(blob))
        };
        d.dom.insert(ws, mesh("Hull", 20.0, rustudio_physics::physics_mesh::cube_blob()));
        d.dom.insert(ws, mesh("BoxFidelity", 30.0, b"CSGPHS\0\0\0\0BLOCK".to_vec()));
        let sss = d.find_child(d.dom.root_ref(), "ServerScriptService").unwrap();
        let src = r#"
local function f(r) return r and string.format("%s %.3f %.2f,%.2f,%.2f", r.Instance.Name, r.Distance, r.Normal.X, r.Normal.Y, r.Normal.Z) or "nil" end
for _, x in {0, 10, 20, 30} do
    print("ray", x, f(workspace:Raycast(Vector3.new(x, 10, 0), Vector3.new(0, -20, 0))))
    print("sphere", x, f(workspace:Spherecast(Vector3.new(x, 10, 0), 0.5, Vector3.new(0, -20, 0))))
end
-- Into the hull's side: its 4-stud width (scaled from InitialSize 2): x = 22,
-- the sweep meets x = 22 - 0.05.
print("side", f(workspace:Raycast(Vector3.new(25, 0, 0), Vector3.new(-20, 0, 0))))
print("sideS", f(workspace:Spherecast(Vector3.new(26, 0, 0), 0.5, Vector3.new(-20, 0, 0))))
-- From inside the hull: the whole part is skipped.
print("inside", f(workspace:Raycast(Vector3.new(20, 0, 0), Vector3.new(0, 20, 0))))
-- Touching only an edge of a box misses it; a ray into a corner gets the faces summed.
print("edge", f(workspace:Raycast(Vector3.new(3, 0, -1), Vector3.new(-1, 0, 1) * 10)))
print("corner", f(workspace:Raycast(Vector3.new(-3, 3, 3), Vector3.new(1, -1, -1) * 10)))
"#;
        let s = d.dom.insert(sss, InstanceBuilder::new("Script").with_name("T").with_property("Source", src.to_string()));
        let rt = Runtime::new().unwrap();
        rt.enter(World::new(d, RunMode::Run));
        rt.bind_globals().unwrap();
        rt.run_script(s);
        rt.step(1.0 / 60.0);
        let out: Vec<String> = rt.leave().output.into_iter().map(|(_, t)| t).collect();
        let text = out.join("\n");
        for want in [
            "ray 0 CollideNoQuery 9.000 0.00,1.00,0.00",
            "sphere 0 CollideNoQuery 8.500",
            // Through the CanCollide = CanQuery = false part to the baseplate.
            "ray 10 Baseplate 10.000",
            "sphere 10 Baseplate 9.500",
            "ray 20 Hull 9.000 0.00,1.00,0.00",
            "sphere 20 Hull 8.550",
            "ray 30 BoxFidelity 9.000",
            "sphere 30 BoxFidelity 8.500",
            "side Hull 3.000 1.00,0.00,0.00",
            "sideS Hull 3.550 1.00,0.00,0.00",
            "inside nil",
            "edge nil",
            "corner CollideNoQuery 3.464 -0.58,0.58,0.58",
        ] {
            assert!(out.iter().any(|l| l.starts_with(want)), "{want} not in\n{text}");
        }
    }

    /// Round 25: CopyRegion / PasteRegion, ReadVoxelChannels /
    /// WriteVoxelChannels and the cell conversions.
    #[test]
    fn terrain_regions_and_channels() {
        let (out, v) = run(r#"
local t = workspace.Terrain
t:FillBlock(CFrame.new(6, 6, 6), Vector3.new(8, 8, 8), Enum.Material.Rock)
local r = t:CopyRegion(Region3int16.new(Vector3int16.new(0, 0, 0), Vector3int16.new(3, 3, 3)))
print(r.ClassName, tostring(r.SizeInCells), r.IsSmooth)
t:PasteRegion(r, Vector3int16.new(10, 0, 0), true)
local _, a = t:ReadVoxels(Region3.new(Vector3.new(0, 0, 0), Vector3.new(16, 16, 16)), 4)
local m, b = t:ReadVoxels(Region3.new(Vector3.new(40, 0, 0), Vector3.new(56, 16, 16)), 4)
local same, rock = true, 0
for x = 1, 4 do for y = 1, 4 do for z = 1, 4 do
	same = same and a[x][y][z] == b[x][y][z]
	if m[x][y][z] == Enum.Material.Rock then rock += 1 end
end end end
print("paste", same, rock)
t:WriteVoxelChannels(Region3.new(Vector3.new(0, 40, 0), Vector3.new(8, 44, 4)), 4, {
	SolidMaterial = {{{Enum.Material.Air}}, {{Enum.Material.Sand}}},
	SolidOccupancy = {{{0}}, {{0.5}}},
	LiquidOccupancy = {{{1}}, {{0.5}}},
})
local ch = t:ReadVoxelChannels(Region3.new(Vector3.new(0, 40, 0), Vector3.new(8, 44, 4)), 4, {"SolidMaterial", "LiquidOccupancy"})
print("channels", ch.SolidMaterial[1][1][1].Name, ch.SolidMaterial[2][1][1].Name, ch.LiquidOccupancy[1][1][1], ch.SolidOccupancy == nil, tostring(ch.Size))
print("cells", tostring(t:CellCenterToWorld(1, 2, 3)), tostring(t:CellCornerToWorld(1, 2, 3)), tostring(t:WorldToCell(Vector3.new(5, -1, 9))))
local empty = t:CopyRegion(Region3int16.new(Vector3int16.new(100, 100, 100), Vector3int16.new(101, 101, 101)))
t:PasteRegion(empty, Vector3int16.new(10, 0, 0), false)
t:PasteRegion(empty, Vector3int16.new(0, 0, 0), true)
local all = t:CopyRegion(t.MaxExtents)
print("max", tostring(all.SizeInCells))
"#);
        let text = out.join("\n");
        assert!(text.contains("TerrainRegion 4, 4, 4 true"), "{text}");
        assert!(text.contains("paste true 27"), "{text}");
        assert!(text.contains("channels Air Sand 1 true 2, 1, 1"), "{text}");
        assert!(text.contains("cells 6, 10, 14 4, 8, 12 1, -1, 2"), "{text}");
        assert!(text.contains("max 64001, 64001, 64001"), "{text}");
        let rock = rustudio_core::terrain::material_index("Rock").unwrap();
        // pasteEmptyCells cleared the original's 2³ corner; the copy stayed.
        assert_eq!(v.get(0, 0, 0).0, 0);
        assert_eq!(v.get(2, 2, 2).0, rock);
        assert_eq!(v.get(11, 1, 1).0, rock);
        assert_eq!(v.get(0, 10, 0).0, rustudio_core::terrain::WATER);
        assert!((v.water(1, 10, 0) - 0.5).abs() < 0.01, "shoreline water {}", v.water(1, 10, 0));
    }

    /// Runs `src` as a Script for `steps` frames on an empty place (no
    /// Baseplate / SpawnLocation, as Studio's blank place) and returns its
    /// printed lines and the number of terrain chunks rays meshed.
    fn run_terrain_rays(src: &str, steps: usize) -> (Vec<String>, usize) {
        let mut d = Document::new_place();
        if let Some(ws) = d.workspace() {
            let parts: Vec<Ref> = d
                .dom
                .get_by_ref(ws)
                .map(|i| i.children().to_vec())
                .unwrap_or_default()
                .into_iter()
                .filter(|r| d.class_of(*r).is_some_and(|c| c != "Terrain" && c != "Camera"))
                .collect();
            for r in parts {
                d.dom.destroy(r);
            }
        }
        let sss = d.find_child(d.dom.root_ref(), "ServerScriptService").unwrap();
        let s = d.dom.insert(sss, InstanceBuilder::new("Script").with_name("T").with_property("Source", src.to_string()));
        let rt = Runtime::new().unwrap();
        rt.enter(World::new(d, RunMode::Run));
        rt.bind_globals().unwrap();
        rt.run_script(s);
        for _ in 0..steps {
            rt.step(1.0 / 60.0);
        }
        let meshed = rt.lua.app_data_ref::<QueryCache>().map(|c| c.terrain_tris.len()).unwrap_or(0);
        let w = rt.leave();
        let out = w
            .output
            .into_iter()
            .map(|(_, t)| t.split("  —  ").next().unwrap_or_default().to_string())
            .collect();
        (out, meshed)
    }

    const RAY_FMT: &str = r#"
local function f(n) return string.format("%.3f", n) end
local function v(x) return f(x.X)..","..f(x.Y)..","..f(x.Z) end
local function show(name, r)
    print(name.." "..(r and ((r.Instance and r.Instance.Name or "nil").." "..v(r.Position).." n="..v(r.Normal).." "..r.Material.Name.." d="..f(r.Distance)) or "nil"))
end
local T = workspace.Terrain
"#;

    fn has(out: &[String], line: &str) {
        assert!(out.iter().any(|l| l == line), "missing {line:?} in:\n{}", out.join("\n"));
    }

    /// Water terrain: Studio's rays hit it unless IgnoreWater, on the water
    /// mesh moved half a voxel down; from inside water they pass through.
    /// Numbers from an offline Studio (water_probe, 2026-11-11). Before, every
    /// ray went through water to the ground under it.
    #[test]
    fn terrain_rays_hit_water() {
        let src = format!(
            "{RAY_FMT}{}",
            r#"
T:FillBlock(CFrame.new(0, -8, 0), Vector3.new(32, 16, 32), Enum.Material.Water)
T:FillBlock(CFrame.new(200, -5, 0), Vector3.new(32, 10, 32), Enum.Material.Water)
T:FillBlock(CFrame.new(200, -20, 0), Vector3.new(32, 20, 32), Enum.Material.Sand)
task.wait() task.wait()
show("top", workspace:Raycast(Vector3.new(0, 50, 0.5), Vector3.new(0, -100, 0)))
show("edge", workspace:Raycast(Vector3.new(15, 50, 0.5), Vector3.new(0, -100, 0)))
show("side", workspace:Raycast(Vector3.new(50, -8, 0.5), Vector3.new(-100, 0, 0)))
show("below", workspace:Raycast(Vector3.new(0.5, -40, 0.5), Vector3.new(0, 100, 0)))
show("inside_up", workspace:Raycast(Vector3.new(0.5, -4, 0.5), Vector3.new(0, 100, 0)))
show("lake", workspace:Raycast(Vector3.new(200.5, 50, 1), Vector3.new(0, -100, 0)))
local p = RaycastParams.new() p.IgnoreWater = true
show("lake_ignore", workspace:Raycast(Vector3.new(200.5, 50, 1), Vector3.new(0, -100, 0), p))
show("lake_inside", workspace:Raycast(Vector3.new(200.5, -3, 1), Vector3.new(0, -100, 0)))
local h, pos, n, m = workspace:FindPartOnRay(Ray.new(Vector3.new(200.5, 50, 1), Vector3.new(0, -100, 0)))
print("fpor "..tostring(h).." "..v(pos).." "..m.Name)
h, pos, n, m = workspace:FindPartOnRay(Ray.new(Vector3.new(200.5, 50, 1), Vector3.new(0, -100, 0)), nil, false, true)
print("fpor_ign "..tostring(h).." "..v(pos).." "..m.Name)
h, pos, n, m = workspace:FindPartOnRayWithWhitelist(Ray.new(Vector3.new(200.5, 50, 1), Vector3.new(0, -100, 0)), {T}, true)
print("white_ign "..tostring(h).." "..v(pos).." "..m.Name)
h, pos, n, m = workspace:FindPartOnRayWithIgnoreList(Ray.new(Vector3.new(200.5, 50, 1), Vector3.new(0, -100, 0)), {T})
print("miss "..tostring(h).." "..v(pos).." "..v(n).." "..tostring(m and m.Name))
"#
        );
        let (out, _) = run_terrain_rays(&src, 6);
        has(&out, "top Terrain 0.000,0.000,0.500 n=0.000,1.000,0.000 Water d=50.000");
        has(&out, "edge Terrain 15.000,-1.500,0.500 n=0.447,0.894,0.000 Water d=51.500");
        has(&out, "side Terrain 18.000,-8.000,0.500 n=1.000,0.000,0.000 Water d=32.000");
        has(&out, "below Terrain 0.500,-20.000,0.500 n=0.000,-1.000,0.000 Water d=20.000");
        has(&out, "inside_up nil");
        has(&out, "lake Terrain 200.500,0.000,1.000 n=0.000,1.000,0.000 Water d=50.000");
        has(&out, "lake_ignore Terrain 200.500,-7.984,1.000 n=0.000,1.000,0.000 Sand d=57.984");
        has(&out, "lake_inside Terrain 200.500,-7.984,1.000 n=0.000,1.000,0.000 Sand d=4.984");
        has(&out, "fpor Terrain 200.500,0.000,1.000 Water");
        has(&out, "fpor_ign Terrain 200.500,-7.984,1.000 Sand");
        has(&out, "white_ign Terrain 200.500,-7.984,1.000 Sand");
        // A miss gives Material Air (it was nil).
        has(&out, "miss nil 200.500,-50.000,1.000 0.000,0.000,0.000 Air");
    }

    /// Spherecast / Blockcast / Shapecast meet terrain (they only saw parts):
    /// Studio's distances, materials and contact normals.
    #[test]
    fn shapecasts_hit_terrain() {
        let src = format!(
            "{RAY_FMT}{}",
            r#"
T:FillBlock(CFrame.new(0, -10, 0), Vector3.new(64, 20, 64), Enum.Material.Brick)
T:FillBall(Vector3.new(100, 0, 0), 10, Enum.Material.Rock)
T:FillBlock(CFrame.new(200, -5, 0), Vector3.new(32, 10, 32), Enum.Material.Water)
task.wait() task.wait()
local function d(name, r)
    print(name.." "..(r and (r.Instance.Name.." "..r.Material.Name.." d="..f(r.Distance).." y="..f(r.Position.Y).." n="..(string.format("%.2f,%.2f,%.2f", r.Normal.X, r.Normal.Y, r.Normal.Z):gsub("%-0%.00", "0.00"))) or "nil"))
end
d("sphere", workspace:Spherecast(Vector3.new(0.3, 20, 0.7), 2, Vector3.new(0, -100, 0)))
d("sphere_ball", workspace:Spherecast(Vector3.new(103, 30, 0.7), 2, Vector3.new(0, -100, 0)))
d("sphere_water", workspace:Spherecast(Vector3.new(200.5, 20, 1), 2, Vector3.new(0, -100, 0)))
d("block", workspace:Blockcast(CFrame.new(0.3, 20, 0.7), Vector3.new(4, 2, 4), Vector3.new(0, -100, 0)))
d("block_rot", workspace:Blockcast(CFrame.new(0.3, 20, 0.7) * CFrame.Angles(0.3, 0.2, 0.5), Vector3.new(4, 2, 4), Vector3.new(0, -100, 0)))
d("block_side", workspace:Blockcast(CFrame.new(60, -10, 0), Vector3.new(2, 2, 2), Vector3.new(-100, 0, 0)))
local b = Instance.new("Part") b.Shape = Enum.PartType.Ball b.Size = Vector3.new(3, 3, 3) b.Anchored = true b.CFrame = CFrame.new(0.3, 20, 0.7) b.Parent = workspace
d("shape", workspace:Shapecast(b, Vector3.new(0, -100, 0)))
b:Destroy()
d("sphere_inside", workspace:Spherecast(Vector3.new(0.3, -5, 0.7), 2, Vector3.new(0, -100, 0)))
d("sphere_touching", workspace:Spherecast(Vector3.new(0.3, 1, 0.7), 2, Vector3.new(0, -100, 0)))
local ex = RaycastParams.new() ex.FilterDescendantsInstances = {T}
d("sphere_excluded", workspace:Spherecast(Vector3.new(0.3, 20, 0.7), 2, Vector3.new(0, -100, 0), ex))
"#
        );
        let (out, _) = run_terrain_rays(&src, 6);
        has(&out, "sphere Terrain Brick d=18.000 y=0.000 n=0.00,1.00,0.00");
        has(&out, "sphere_ball Terrain Rock d=16.629 y=11.710 n=0.50,0.83,0.25");
        has(&out, "sphere_water Terrain Water d=18.000 y=0.000 n=0.00,1.00,0.00");
        has(&out, "block Terrain Brick d=19.000 y=0.000 n=0.00,1.00,0.00");
        has(&out, "block_rot Terrain Brick d=17.591 y=0.000 n=0.00,1.00,0.00");
        assert!(out.iter().any(|l| l.starts_with("block_side Terrain Brick d=27.000")), "{out:?}");
        has(&out, "shape Terrain Brick d=18.500 y=0.000 n=0.00,1.00,0.00");
        has(&out, "sphere_inside nil");
        has(&out, "sphere_touching nil");
        has(&out, "sphere_excluded nil");
    }

    /// Solid terrain against Studio: FillBlock / FillBall / WriteVoxels
    /// surfaces, include / exclude, rays from inside, long rays, far
    /// terrain, edits seen the frame after (not the same frame), and
    /// Mouse.Hit's terrain ray.
    #[test]
    fn terrain_rays_match_studio() {
        let src = format!(
            "{RAY_FMT}{}",
            r#"
T:FillBlock(CFrame.new(0, -10, 0), Vector3.new(64, 20, 64), Enum.Material.Grass)
T:FillBall(Vector3.new(100, 0, 0), 10, Enum.Material.Rock)
T:FillBlock(CFrame.new(20000, -10, 20000), Vector3.new(16, 20, 16), Enum.Material.Mud)
task.wait() task.wait()
show("center", workspace:Raycast(Vector3.new(0.3, 50, 0.7), Vector3.new(0, -100, 0)))
show("edge", workspace:Raycast(Vector3.new(31.5, 50, 5), Vector3.new(0, -100, 0)))
show("side", workspace:Raycast(Vector3.new(60, -10, 5.3), Vector3.new(-100, 0, 0)))
show("ball", workspace:Raycast(Vector3.new(105.3, 50, 2.1), Vector3.new(0, -100, 0)))
show("far", workspace:Raycast(Vector3.new(20001.3, 50, 20001.7), Vector3.new(0, -100, 0)))
show("long", workspace:Raycast(Vector3.new(0.3, 9000, 0.7), Vector3.new(0, -10000, 0)))
show("inside", workspace:Raycast(Vector3.new(0.3, -5, 0.7), Vector3.new(0, -100, 0)))
local inc = RaycastParams.new() inc.FilterType = Enum.RaycastFilterType.Include inc.FilterDescendantsInstances = {T}
show("include", workspace:Raycast(Vector3.new(0.3, 50, 0.7), Vector3.new(0, -100, 0), inc))
local exc = RaycastParams.new() exc.FilterDescendantsInstances = {T}
show("exclude", workspace:Raycast(Vector3.new(0.3, 50, 0.7), Vector3.new(0, -100, 0), exc))
show("edit_before", workspace:Raycast(Vector3.new(500.5, 50, 0.5), Vector3.new(0, -100, 0)))
T:FillBlock(CFrame.new(500, 0, 0), Vector3.new(8, 8, 8), Enum.Material.Ice)
show("edit_now", workspace:Raycast(Vector3.new(500.5, 50, 0.5), Vector3.new(0, -100, 0)))
task.wait()
show("edit_after", workspace:Raycast(Vector3.new(500.5, 50, 0.5), Vector3.new(0, -100, 0)))
"#
        );
        let (out, _) = run_terrain_rays(&src, 8);
        has(&out, "center Terrain 0.300,2.000,0.700 n=0.000,1.000,0.000 Grass d=48.000");
        has(&out, "edge Terrain 31.500,0.250,5.000 n=0.447,0.894,0.000 Grass d=49.750");
        has(&out, "side Terrain 34.000,-10.000,5.300 n=1.000,0.000,0.000 Grass d=26.000");
        has(&out, "ball Terrain 105.300,10.247,2.100 n=-0.097,0.995,0.037 Rock d=39.753");
        has(&out, "far Terrain 20001.301,2.000,20001.699 n=0.000,1.000,0.000 Mud d=48.000");
        has(&out, "long Terrain 0.300,2.000,0.700 n=0.000,1.000,0.000 Grass d=8998.000");
        has(&out, "inside nil");
        has(&out, "include Terrain 0.300,2.000,0.700 n=0.000,1.000,0.000 Grass d=48.000");
        has(&out, "exclude nil");
        has(&out, "edit_before nil");
        has(&out, "edit_now nil");
        assert!(out.iter().any(|l| l.starts_with("edit_after Terrain 500.500,5.13")), "{out:?}");
    }

    /// A long ray over sparse terrain meshes only the chunks next to stored
    /// voxels: on a big generated map every empty chunk along a 12000-stud
    /// ray was meshed (4.5 ms a ray on a 2.78M-voxel place, 1600 rays 7 s; Studio 17 ms).
    #[test]
    fn long_terrain_rays_skip_empty_chunks() {
        let src = format!(
            "{RAY_FMT}{}",
            r#"
T:FillBlock(CFrame.new(0, -10, 0), Vector3.new(16, 20, 16), Enum.Material.Grass)
T:FillBlock(CFrame.new(6000, 4000, 6000), Vector3.new(16, 20, 16), Enum.Material.Grass)
T:FillBlock(CFrame.new(-6000, -4000, -6000), Vector3.new(16, 20, 16), Enum.Material.Grass)
task.wait() task.wait()
for i = 0, 9 do
    show("r"..i, workspace:Raycast(Vector3.new(0.5 + i, 5000, 0.5), Vector3.new(0, -12000, 0)))
end
"#
        );
        let (out, meshed) = run_terrain_rays(&src, 6);
        has(&out, "r0 Terrain 0.500,2.000,0.500 n=0.000,1.000,0.000 Grass d=4998.000");
        assert!(meshed < 12, "{meshed} chunks meshed for one column");
    }

    /// The play mouse's terrain ray (Mouse.Hit / Mouse.Target), which only
    /// saw parts: Studio's Mouse.Hit lies on the surface Raycast meets,
    /// water included.
    #[test]
    fn mouse_ray_sees_terrain() {
        let mut d = Document::new_place();
        let rt = Runtime::new().unwrap();
        let sss = d.find_child(d.dom.root_ref(), "ServerScriptService").unwrap();
        let s = d.dom.insert(
            sss,
            InstanceBuilder::new("Script").with_name("T").with_property(
                "Source",
                "workspace.Terrain:FillBlock(CFrame.new(400, -8, 300), Vector3.new(32, 16, 32), Enum.Material.Water)".to_string(),
            ),
        );
        rt.enter(World::new(d, RunMode::Run));
        rt.bind_globals().unwrap();
        rt.run_script(s);
        for _ in 0..3 {
            rt.step(1.0 / 60.0);
        }
        let hit = mouse_terrain_hit(&rt.lua, Vec3::new(400.3, 60.0, 330.7), Vec3::new(0.0, -60.0, -30.0).normalize(), 10000.0);
        let w = rt.leave();
        let (r, t) = hit.expect("mouse over the lake hits terrain");
        assert_eq!(w.d().class_of(r).map(|c| c.to_string()).as_deref(), Some("Terrain"));
        let p = Vec3::new(400.3, 60.0, 330.7) + Vec3::new(0.0, -60.0, -30.0).normalize() * t;
        assert!(p.y.abs() < 1e-3, "water surface at {p}");
    }

    /// Dev probe (ignored): runs the Luau file in $RUSTUDIO_PROBE as a
    /// Script and prints its output.
    #[test]
    #[ignore]
    fn probe_file() {
        let Ok(path) = std::env::var("RUSTUDIO_PROBE") else { return };
        let src = std::fs::read_to_string(path).unwrap();
        let mut d = match std::env::var("RUSTUDIO_PROBE_PLACE") {
            Ok(p) => Document::open(std::path::Path::new(&p)).unwrap(),
            Err(_) => Document::new_place(),
        };
        let sss = d.find_child(d.dom.root_ref(), "ServerScriptService").unwrap();
        if let Some(Ok(v)) = rustudio_core::terrain::Voxels::from_dom(&d.dom) {
            println!("probe: terrain {} voxels, bounds {:?}", v.count(), v.world_bounds());
        }
        let s = d.dom.insert(sss, InstanceBuilder::new("Script").with_name("T").with_property("Source", src));
        let rt = Runtime::new().unwrap();
        rt.enter(World::new(d, RunMode::Run));
        rt.bind_globals().unwrap();
        rt.run_script(s);
        for _ in 0..600 {
            rt.step(1.0 / 60.0);
        }
        let w = rt.leave();
        for (_, t) in w.output {
            println!("{t}");
        }
    }
}

/// Humanoid:EquipTool / UnequipTools in this DataModel at once: tools held
/// by the humanoid's character go to its player's Backpack (or where the
/// new tool came from), then `tool`, if any, into the character.
fn equip_now(lua: &Lua, humanoid: Ref, tool: Option<Ref>) -> mlua::Result<()> {
    let (model, held, backpack) = {
        let w = world(lua)?;
        let d = w.d();
        let Some(model) = d.dom.get_by_ref(humanoid).map(|h| h.parent()).filter(|m| m.is_some()) else {
            return Ok(());
        };
        if let Some(t) = tool {
            let ok = d.dom.get_by_ref(t).is_some_and(|i| rustudio_core::doc::is_a(&i.class, "BackpackItem"));
            if !ok || d.dom.get_by_ref(t).map(|i| i.parent()) == Some(model) {
                return Ok(());
            }
        }
        let held: Vec<Ref> = d
            .dom
            .get_by_ref(model)
            .map(|m| m.children().to_vec())
            .unwrap_or_default()
            .into_iter()
            .filter(|c| d.dom.get_by_ref(*c).is_some_and(|i| rustudio_core::doc::is_a(&i.class, "Tool")))
            .collect();
        // The player whose Character this is, and its Backpack.
        let players = d.dom.root().children().iter().copied().find(|c| d.dom.get_by_ref(*c).is_some_and(|i| i.class == "Players"));
        let player = players.and_then(|ps| {
            d.dom.get_by_ref(ps)?.children().iter().copied().find(|p| matches!(d.prop(*p, "Character"), Some(Variant::Ref(m)) if m == model))
        });
        let backpack = player
            .and_then(|p| d.dom.get_by_ref(p)?.children().iter().copied().find(|c| d.dom.get_by_ref(*c).is_some_and(|i| i.class == "Backpack")))
            .or_else(|| tool.and_then(|t| d.dom.get_by_ref(t).map(|i| i.parent())).filter(|p| p.is_some()));
        (model, held, backpack)
    };
    if let Some(bp) = backpack {
        for t in held {
            set_parent(lua, t, bp)?;
        }
    }
    if let Some(t) = tool {
        set_parent(lua, t, model)?;
    }
    Ok(())
}
