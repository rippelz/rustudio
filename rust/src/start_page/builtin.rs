//! Studio's generic templates as rustudio builds them offline: generated
//! by `start_page::tests::generate_builtin_templates` from the DataModels
//! Studio holds after opening each template place (offline Studio, File >
//! Save to File As). Only the values rustudio's class defaults don't give
//! are listed. Regenerate rather than edit.

use super::templates::{Inst, Patch, V::*};

pub(super) static BASEPLATE: &[Inst] = &[
    Inst { path: "Workspace", class: "Workspace", props: &[
        ("CurrentCamera", Ref("Workspace/Camera")),
        ("PlayerCharacterDestroyBehavior", E(2)),
        ("PlayerScriptsUseInputActionSystem", E(2)),
        ("SignalBehavior", E(2)),
        ("StreamOutBehavior", E(2)),
        ("StreamingEnabled", B(true)),
        ("StreamingIntegrityMode", E(3)),
        ("WorldPivotData", OCf([0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 1.0])),
    ] },
    Inst { path: "Workspace/Camera", class: "Camera", props: &[
        ("CFrame", Cf([-19.696095, 14.091624, -19.303886, -0.7061509, 0.31296578, -0.6351404, 1.4901163e-8, 0.8970132, 0.44200373, 0.7080614, 0.31212133, -0.63342667])),
        ("Focus", Cf([-18.425814, 13.207617, -18.037033, 1.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 1.0])),
    ] },
    Inst { path: "Workspace/Baseplate", class: "Part", props: &[
        ("Anchored", B(true)),
        ("BottomSurface", E(0)),
        ("CFrame", Cf([0.0, -8.0, 0.0, 1.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 1.0])),
        ("Color", C8(91, 91, 91)),
        ("FormFactor", E(0)),
        ("Locked", B(true)),
        ("Size", V3(2048.0, 16.0, 2048.0)),
        ("TopSurface", E(0)),
    ] },
    Inst { path: "Workspace/Baseplate/Texture", class: "Texture", props: &[
        ("Color3", C3(0.0, 0.0, 0.0)),
        ("Face", E(1)),
        ("StudsPerTileU", F(8.0)),
        ("StudsPerTileV", F(8.0)),
        ("TextureContent", Uri("rbxassetid://6372755229")),
        ("Transparency", F(0.8)),
    ] },
    Inst { path: "Workspace/Terrain", class: "Terrain", props: &[
        ("BottomSurface", E(4)),
        ("Decoration", B(true)),
        ("MaterialColors", Bin(b"\x00\x00\x00\x00\x00\x00o~>XYV\x98\x98\x98\x8aaI\xcf\xcb\xa7\xac\x94lcdf\xdd\xe4\xe5\xeb\xfd\xff\x94|_ypbKJJ\x8c\x82h\xff\x18CPTT\x86\x86v\xcc\xd2\xdfj\x86@\xff\xff\xfe\xff\xf3\xc0\x8f\x90\x87")),
        ("Materials", Shared(b"\x01\x00\x00\x00\x15\x00\x00\x00\x02\x00\x00\x00o~>\x00\x05\x00\x00\x00\x00\x00\x00\x03\x00\x00\x00XYV \x03\x00\x00\x00\x00\x00\x00\x04\x00\x00\x00\x98\x98\x980\x03\x00\x00\x00\x00\x00\x00\x05\x00\x00\x00\x8aaIP\x03\x00\x00\x00\x00\x00\x00\x06\x00\x00\x00\xcf\xcb\xa7\x10\x05\x00\x00\x00\x00\x00\x00\x07\x00\x00\x00\xac\x94l\x10\x02\x00\x00\x00\x00\x00\x00\x08\x00\x00\x00cdf\x80\x03\x00\x00\x00\x00\x00\x00\x09\x00\x00\x00\xdd\xe4\xe5\x10\x06\x00\x00\x00\x00\x00\x00\x0a\x00\x00\x00\xeb\xfd\xff0\x05\x00\x00\x00\x00\x00\x00\x0b\x00\x00\x00\x94|_\x90\x03\x00\x00\x00\x00\x00\x00\x0c\x00\x00\x00ypb@\x05\x00\x00\x00\x00\x00\x00\x0d\x00\x00\x00KJJ\x14\x03\x00\x00\x00\x00\x00\x00\x0e\x00\x00\x00\x8c\x82hP\x05\x00\x00\x00\x00\x00\x00\x0f\x00\x00\x00\xff\x18C$\x03\x00\x00\x00\x00\x00\x00\x10\x00\x00\x00PTT`\x05\x00\x00\x00\x00\x00\x00\x11\x00\x00\x00\x86\x86vp\x03\x00\x00\x00\x00\x00\x00\x12\x00\x00\x00\xcc\xd2\xdf\x00\x06\x00\x00\x00\x00\x00\x00\x13\x00\x00\x00j\x86@\x04\x05\x00\x00\x00\x00\x00\x00\x14\x00\x00\x00\xff\xff\xfep\x05\x00\x00\x00\x00\x00\x00\x15\x00\x00\x00\xff\xf3\xc04\x03\x00\x00\x00\x00\x00\x00\x16\x00\x00\x00\x8f\x90\x87D\x03\x00\x00\x00\x00\x00\x00")),
        ("TopSurface", E(3)),
        ("WaterColor", C3(0.04705883, 0.32941177, 0.36078432)),
    ] },
    Inst { path: "Workspace/SpawnLocation", class: "SpawnLocation", props: &[
        ("Anchored", B(true)),
        ("BottomSurface", E(0)),
        ("CFrame", Cf([0.0, 0.5, 0.0, 1.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 1.0])),
        ("Duration", I(0)),
        ("Size", V3(12.0, 1.0, 12.0)),
        ("TopSurface", E(0)),
    ] },
    Inst { path: "Workspace/SpawnLocation/Decal", class: "Decal", props: &[
        ("Face", E(1)),
        ("TextureContent", Uri("rbxasset://textures/SpawnLocation.png")),
    ] },
    Inst { path: "Instance@TimerService", class: "TimerService", props: &[
    ] },
    Inst { path: "CollectionService", class: "CollectionService", props: &[
    ] },
    Inst { path: "SoundService", class: "SoundService", props: &[
        ("DefaultListenerLocation", E(3)),
        ("RespectFilteringEnabled", B(true)),
    ] },
    Inst { path: "VideoCaptureService", class: "VideoCaptureService", props: &[
    ] },
    Inst { path: "NonReplicatedCSGDictionaryService", class: "NonReplicatedCSGDictionaryService", props: &[
    ] },
    Inst { path: "CSGDictionaryService", class: "CSGDictionaryService", props: &[
    ] },
    Inst { path: "Chat", class: "Chat", props: &[
        ("BubbleChatEnabled", B(true)),
    ] },
    Inst { path: "Players", class: "Players", props: &[
        ("MaxPlayers", I(30)),
        ("PreferredPlayers", I(30)),
        ("RespawnTime", F(3.0)),
    ] },
    Inst { path: "ReplicatedFirst", class: "ReplicatedFirst", props: &[
    ] },
    Inst { path: "TweenService", class: "TweenService", props: &[
    ] },
    Inst { path: "MaterialService", class: "MaterialService", props: &[
        ("Use2022MaterialsXml", B(true)),
    ] },
    Inst { path: "TextChatService", class: "TextChatService", props: &[
        ("ChatVersion", E(1)),
        ("IsLegacyChatDisabled", B(true)),
    ] },
    Inst { path: "TextChatService/ChatWindowConfiguration", class: "ChatWindowConfiguration", props: &[
        ("FontFace", Font("rbxasset://fonts/families/BuilderSans.json", 500, 0)),
        ("TextSize", L(18)),
    ] },
    Inst { path: "TextChatService/ChatInputBarConfiguration", class: "ChatInputBarConfiguration", props: &[
        ("FontFace", Font("rbxasset://fonts/families/BuilderSans.json", 500, 0)),
        ("TextSize", L(18)),
    ] },
    Inst { path: "TextChatService/BubbleChatConfiguration", class: "BubbleChatConfiguration", props: &[
        ("Font", E(47)),
        ("FontFace", Font("rbxasset://fonts/families/BuilderSans.json", 500, 0)),
        ("TextSize", L(20)),
    ] },
    Inst { path: "TextChatService/BubbleChatConfiguration/UIGradient", class: "UIGradient", props: &[
        ("Enabled", B(false)),
    ] },
    Inst { path: "TextChatService/BubbleChatConfiguration/ImageLabel", class: "ImageLabel", props: &[
        ("BackgroundColor3", C3(1.0, 1.0, 1.0)),
        ("Size", Ud2(0.0, 100, 0.0, 100)),
    ] },
    Inst { path: "TextChatService/BubbleChatConfiguration/UICorner", class: "UICorner", props: &[
        ("BottomLeftRadius", Ud(0.0, 12)),
        ("BottomRightRadius", Ud(0.0, 12)),
        ("TopLeftRadius", Ud(0.0, 12)),
        ("TopRightRadius", Ud(0.0, 12)),
    ] },
    Inst { path: "TextChatService/BubbleChatConfiguration/UIPadding", class: "UIPadding", props: &[
        ("PaddingBottom", Ud(0.0, 8)),
        ("PaddingLeft", Ud(0.0, 8)),
        ("PaddingRight", Ud(0.0, 8)),
        ("PaddingTop", Ud(0.0, 8)),
    ] },
    Inst { path: "TextChatService/ChannelTabsConfiguration", class: "ChannelTabsConfiguration", props: &[
    ] },
    Inst { path: "PermissionsService", class: "PermissionsService", props: &[
    ] },
    Inst { path: "PlayerEmulatorService", class: "PlayerEmulatorService", props: &[
    ] },
    Inst { path: "StudioData", class: "StudioData", props: &[
    ] },
    Inst { path: "StarterPlayer", class: "StarterPlayer", props: &[
        ("AvatarJointUpgrade", E(2)),
        ("CameraMaxZoomDistance", F(128.0)),
        ("CharacterBreakJointsOnDeath", B(false)),
        ("CharacterUseJumpPower", B(false)),
    ] },
    Inst { path: "StarterPlayer/StarterPlayerScripts", class: "StarterPlayerScripts", props: &[
    ] },
    Inst { path: "StarterPlayer/StarterCharacterScripts", class: "StarterCharacterScripts", props: &[
    ] },
    Inst { path: "StarterPack", class: "StarterPack", props: &[
    ] },
    Inst { path: "StarterGui", class: "StarterGui", props: &[
        ("ScreenOrientation", E(4)),
    ] },
    Inst { path: "LocalizationService", class: "LocalizationService", props: &[
    ] },
    Inst { path: "Teleport Service", class: "TeleportService", props: &[
    ] },
    Inst { path: "PhysicsService", class: "PhysicsService", props: &[
    ] },
    Inst { path: "InsertService", class: "InsertService", props: &[
    ] },
    Inst { path: "InsertService/InsertionHash", class: "StringValue", props: &[
        ("Value", S("{523448dd-6dcc-405c-9a7a-0b46b21706d8}")),
    ] },
    Inst { path: "GamePassService", class: "GamePassService", props: &[
    ] },
    Inst { path: "Debris", class: "Debris", props: &[
    ] },
    Inst { path: "CookiesService", class: "CookiesService", props: &[
    ] },
    Inst { path: "Selection", class: "Selection", props: &[
    ] },
    Inst { path: "VRService", class: "VRService", props: &[
    ] },
    Inst { path: "ContextActionService", class: "ContextActionService", props: &[
    ] },
    Inst { path: "Instance@ScriptService", class: "ScriptService", props: &[
    ] },
    Inst { path: "AssetService", class: "AssetService", props: &[
    ] },
    Inst { path: "TouchInputService", class: "TouchInputService", props: &[
    ] },
    Inst { path: "AvatarSettings", class: "AvatarSettings", props: &[
    ] },
    Inst { path: "AvatarSettings/DefaultAvatarRules", class: "AvatarRules", props: &[
    ] },
    Inst { path: "AvatarSettings/DefaultAvatarRules/AvatarAbilityRules", class: "AvatarAbilityRules", props: &[
    ] },
    Inst { path: "AvatarSettings/DefaultAvatarRules/AvatarAccessoryRules", class: "AvatarAccessoryRules", props: &[
    ] },
    Inst { path: "AvatarSettings/DefaultAvatarRules/AvatarAnimationRules", class: "AvatarAnimationRules", props: &[
    ] },
    Inst { path: "AvatarSettings/DefaultAvatarRules/AvatarBodyRules", class: "AvatarBodyRules", props: &[
        ("CustomHeight", Range(3.6, 6.5)),
        ("ScaleMode", E(1)),
    ] },
    Inst { path: "AvatarSettings/DefaultAvatarRules/AvatarClothingRules", class: "AvatarClothingRules", props: &[
    ] },
    Inst { path: "AvatarSettings/DefaultAvatarRules/AvatarCollisionRules", class: "AvatarCollisionRules", props: &[
    ] },
    Inst { path: "Packages", class: "Packages", props: &[
    ] },
    Inst { path: "GuidRegistryService", class: "GuidRegistryService", props: &[
    ] },
    Inst { path: "ServiceVisibilityService", class: "ServiceVisibilityService", props: &[
    ] },
    Inst { path: "LuaWebService", class: "LuaWebService", props: &[
    ] },
    Inst { path: "ProcessInstancePhysicsService", class: "ProcessInstancePhysicsService", props: &[
    ] },
    Inst { path: "ReplicatedStorage", class: "ReplicatedStorage", props: &[
    ] },
    Inst { path: "ServerScriptService", class: "ServerScriptService", props: &[
    ] },
    Inst { path: "ServerStorage", class: "ServerStorage", props: &[
    ] },
    Inst { path: "VideoService", class: "VideoService", props: &[
    ] },
    Inst { path: "HttpService", class: "HttpService", props: &[
    ] },
    Inst { path: "DataStoreService", class: "DataStoreService", props: &[
    ] },
    Inst { path: "Lighting", class: "Lighting", props: &[
        ("Ambient", C3(0.27450982, 0.27450982, 0.27450982)),
        ("Attributes", Attrs(&[("RBX_LightingTechnologyUnifiedMigration", B(true)), ("RBX_OriginalTechnologyOnFileLoad", I(3))])),
        ("Brightness", F(3.0)),
        ("EnvironmentDiffuseScale", F(1.0)),
        ("EnvironmentSpecularScale", F(1.0)),
        ("FogColor", C3(0.75294125, 0.75294125, 0.75294125)),
        ("GeographicLatitude", F(0.0)),
        ("GlobalShadows", B(true)),
        // What Studio's migration stored on open (ShadowMap → Soft): with
        // the attribute set nothing is derived again (doc::lighting_style_default).
        ("LightingStyle", E(1)),
        ("OutdoorAmbient", C3(0.27450982, 0.27450982, 0.27450982)),
        ("Outlines", B(false)),
        ("PrioritizeLightingQuality", B(true)),
        ("ShadowSoftness", F(0.2)),
        ("Technology", E(3)),
        ("TimeOfDay", S("14:30:00")),
    ] },
    Inst { path: "Lighting/Sky", class: "Sky", props: &[
        ("MoonTextureContent", Uri("rbxassetid://6444320592")),
        ("SkyboxBackContent", Uri("rbxassetid://6444884337")),
        ("SkyboxDownContent", Uri("rbxassetid://6444884785")),
        ("SkyboxFrontContent", Uri("rbxassetid://6444884337")),
        ("SkyboxLeftContent", Uri("rbxassetid://6444884337")),
        ("SkyboxRightContent", Uri("rbxassetid://6444884337")),
        ("SkyboxUpContent", Uri("rbxassetid://6412503613")),
        ("SunAngularSize", F(11.0)),
        ("SunTextureContent", Uri("rbxassetid://6196665106")),
    ] },
    Inst { path: "Lighting/SunRays", class: "SunRaysEffect", props: &[
        ("Intensity", F(0.01)),
        ("Spread", F(0.1)),
    ] },
    Inst { path: "Lighting/Atmosphere", class: "Atmosphere", props: &[
        ("Color", C3(0.78039217, 0.78039217, 0.78039217)),
        ("Decay", C3(0.41568628, 0.4392157, 0.49019608)),
        ("Density", F(0.3)),
        ("Offset", F(0.25)),
    ] },
    Inst { path: "Lighting/Bloom", class: "BloomEffect", props: &[
        ("Intensity", F(1.0)),
        ("Threshold", F(2.0)),
    ] },
    Inst { path: "Lighting/DepthOfField", class: "DepthOfFieldEffect", props: &[
        ("Enabled", B(false)),
        ("FarIntensity", F(0.1)),
        ("InFocusRadius", F(30.0)),
    ] },
    Inst { path: "Instance@LodDataService", class: "LodDataService", props: &[
    ] },
    Inst { path: "ProximityPromptService", class: "ProximityPromptService", props: &[
    ] },
    Inst { path: "Teams", class: "Teams", props: &[
    ] },
    Inst { path: "TestService", class: "TestService", props: &[
    ] },
    Inst { path: "UGCAvatarService", class: "UGCAvatarService", props: &[
    ] },
    Inst { path: "VirtualInputManager", class: "VirtualInputManager", props: &[
    ] },
    Inst { path: "VoiceChatService", class: "VoiceChatService", props: &[
        ("UseAudioApi", E(2)),
    ] },
    Inst { path: "SerializationService", class: "SerializationService", props: &[
    ] },
];

pub(super) static CLASSIC_BASEPLATE: Patch = Patch {
    remove: &["Workspace/Baseplate/Texture", "Workspace/SpawnLocation", "DataStoreService", "Lighting/Sky", "Lighting/SunRays", "Lighting/Atmosphere", "Lighting/Bloom", "Lighting/DepthOfField"],
    add: &[
    ],
    set: &[
        ("Workspace/Camera", "CFrame", Cf([-30.734314, 16.456863, 14.594533, 0.71446794, 0.19519782, -0.67188793, -0.0, 0.9602951, 0.27898628, 0.69966817, -0.19932675, 0.68610007])),
        ("Workspace/Camera", "Focus", Cf([-29.390537, 15.8988905, 13.222332, 1.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 1.0])),
        ("Workspace/Baseplate", "BottomSurface", E(4)),
        ("Workspace/Baseplate", "CFrame", Cf([0.0, -10.0, 0.0, 1.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 1.0])),
        ("Workspace/Baseplate", "Color", C8(99, 95, 98)),
        ("Workspace/Baseplate", "Size", V3(512.0, 20.0, 512.0)),
        ("Workspace/Baseplate", "TopSurface", E(3)),
        ("Workspace/Terrain", "Decoration", Default),
        ("Chat", "BubbleChatEnabled", Default),
        ("Players", "PreferredPlayers", I(56832)),
        ("Players", "MaxPlayers", Default),
        ("Players", "RespawnTime", Default),
        ("PlayerEmulatorService", "EmulatedCountryCode", S("US")),
        ("StarterPlayer", "CharacterUseJumpPower", Default),
        ("StarterGui", "ScreenOrientation", Default),
        ("InsertService/InsertionHash", "Value", S("{92c1f159-07af-407a-ba4f-5fb4db7d3271}")),
        ("Lighting", "Ambient", C3(0.5411765, 0.5411765, 0.5411765)),
        ("Lighting", "Brightness", F(2.0)),
        ("Lighting", "EnvironmentDiffuseScale", Default),
        ("Lighting", "EnvironmentSpecularScale", Default),
        ("Lighting", "GeographicLatitude", Default),
        ("Lighting", "OutdoorAmbient", Default),
        ("Lighting", "TimeOfDay", Default),
    ],
};

pub(super) static FLAT_TERRAIN: Patch = Patch {
    remove: &["Workspace/Baseplate", "Workspace/SpawnLocation", "AvatarSettings/DefaultAvatarRules", "DataStoreService"],
    add: &[
    ],
    set: &[
        ("Workspace/Camera", "CFrame", Cf([58.453537, 31.631641, -219.4025, -0.11296367, 0.26742166, -0.9569352, -0.0, 0.96309996, 0.26914442, 0.9935991, 0.030403541, -0.10879531])),
        ("Workspace/Camera", "Focus", Cf([60.367405, 31.093353, -219.1849, 1.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 1.0])),
        ("Workspace/Terrain", "AcquisitionMethod", E(2)),
        ("Players", "PreferredPlayers", I(1)),
        ("Players", "MaxPlayers", Default),
        ("StarterPlayer", "CameraMaxZoomDistance", Default),
        ("StarterGui", "ScreenOrientation", Default),
        ("InsertService/InsertionHash", "Value", S("{05408a00-6452-406b-a252-6dbcfd9c40ce}")),
    ],
};
