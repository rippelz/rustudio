//! Engine API domain: gui_input (GUI objects, input services, the new input
//! action system, styles, haptics / VR / gamepad). Hook contract in mod.rs.
//!
//! Most behaviour is Luau (gui_input.luau, loaded by `install`): its
//! methods are reached through `call` by "BaseClass.Method", its computed
//! properties through `prop_get` by "Class.Prop". Rust here: the glue, the
//! property store for classes rbx-reflection doesn't know (UIBlur,
//! ParticleEmitter2D, …), Path2D's curve maths and Path2DControlPoint.
//!
//! Studio's answers where there's no device (measured in an offline Studio
//! in an offline Studio, 2026-11-07; checks/parity/gui_input.md): no VR (identity
//! CFrames, VREnabled false), no accelerometer / gyroscope (warnings and an
//! empty InputObject), no touch (Touch* events exist and never fire), no
//! on-screen keyboard (0, 0), haptics as Studio's desktop (gamepad motors
//! Large / Small on connected pads), notifications "not yet enabled".

#![allow(clippy::too_many_arguments)]

use mlua::{Function, Lua, MultiValue, Table, Value};
use rbx_dom_weak::types::{Ref, Variant, VariantType};

use crate::ltypes::*;
use crate::runtime::*;

#[path = "gui_input_path2d.rs"]
mod path2d;


/// (method, base class), each one a function in gui_input.luau keyed
/// "Base.Method" (Path2D's are Rust, in gui_input_path2d.rs).
pub const METHODS: &[(&str, &str)] = &[
    // GuiService
    ("AddSelectionParent", "GuiService"),
    ("AddSelectionTuple", "GuiService"),
    ("RemoveSelectionGroup", "GuiService"),
    ("CloseInspectMenu", "GuiService"),
    ("DismissNotification", "GuiService"),
    ("SendNotification", "GuiService"),
    ("GetGameplayPausedNotificationEnabled", "GuiService"),
    ("SetGameplayPausedNotificationEnabled", "GuiService"),
    ("GetInsetArea", "GuiService"),
    ("InspectPlayerFromHumanoidDescription", "GuiService"),
    ("InspectPlayerFromUserId", "GuiService"),
    ("Select", "GuiService"),
    ("SetEmotesMenuOpen", "GuiService"),
    ("SetInspectMenuEnabled", "GuiService"),
    ("GetResolutionScale", "GuiService"),
    ("GetSafeZoneOffsets", "GuiService"),
    ("GetHardwareSafeViewport", "GuiService"),
    ("SetTopbarInset", "GuiService"),
    ("SetGlobalGuiInset", "GuiService"),
    ("SetMenuIsOpen", "GuiService"),
    ("GetErrorMessage", "GuiService"),
    ("GetErrorCode", "GuiService"),
    ("GetErrorType", "GuiService"),
    ("BroadcastNotification", "GuiService"),
    ("AddCenterDialog", "GuiService"),
    ("RemoveCenterDialog", "GuiService"),
    ("IsMemoryTrackerEnabled", "GuiService"),
    ("GetAutoUIScaleHundredths", "GuiService"),
    ("GetEffectiveUIScaleHundredths", "GuiService"),
    ("GetRawScreenScale", "GuiService"),
    ("GetScreenResolution", "GuiService"),
    ("GetBrickCount", "GuiService"),
    ("GetUiMessage", "GuiService"),
    ("GetErrorDetails", "GuiService"),
    ("GetGuiIsVisible", "GuiService"),
    ("ClearError", "GuiService"),
    ("CloseStatsBasedOnInputString", "GuiService"),
    ("ShowStatsBasedOnInputString", "GuiService"),
    ("ForceTenFootInterface", "GuiService"),
    ("AddKey", "GuiService"),
    ("RemoveKey", "GuiService"),
    ("AddSpecialKey", "GuiService"),
    ("RemoveSpecialKey", "GuiService"),
    ("SetHardwareSafeAreaInsets", "GuiService"),
    ("SetPurchasePromptIsShown", "GuiService"),
    ("SetSafeZoneOffsets", "GuiService"),
    ("SetUIScaleMultiplier", "GuiService"),
    ("SetUiMessage", "GuiService"),
    ("ToggleFullscreen", "GuiService"),
    ("ToggleGuiIsVisibleForCaptures", "GuiService"),
    ("ToggleGuiIsVisibleIfAllowed", "GuiService"),
    ("OnNotificationDisplayed", "GuiService"),
    ("OnNotificationInteraction", "GuiService"),
    ("OpenBrowserWindow", "GuiService"),
    ("OpenNativeOverlay", "GuiService"),
    ("SendUIOcclusionMetricsForQueryRegion", "GuiService"),
    ("InspectPlayerFromUserIdWithCtx", "GuiService"),
    // UserInputService
    ("CreateVirtualInput", "UserInputService"),
    ("GetDeviceAcceleration", "UserInputService"),
    ("GetMouseButtonsPressed", "UserInputService"),
    ("GetUserCFrame", "UserInputService"),
    ("RecenterUserHeadCFrame", "UserInputService"),
    ("SetNavigationGamepad", "UserInputService"),
    ("GetFocusedTextBox", "UserInputService"),
    // VirtualInput (also its non-Instance form from CreateVirtualInput)
    ("SendKey", "VirtualInput"),
    ("SendMouseButton", "VirtualInput"),
    ("SendMouseDelta", "VirtualInput"),
    ("SendMousePosition", "VirtualInput"),
    ("SendPointerAction", "VirtualInput"),
    ("SendTextInput", "VirtualInput"),
    // ContextActionService
    ("BindActionToInputTypes", "ContextActionService"),
    ("GetCurrentLocalToolIcon", "ContextActionService"),
    ("BindCoreAction", "ContextActionService"),
    ("BindCoreActionAtPriority", "ContextActionService"),
    ("UnbindCoreAction", "ContextActionService"),
    ("GetBoundCoreActionInfo", "ContextActionService"),
    ("GetAllBoundCoreActionInfo", "ContextActionService"),
    // LayerCollector / PlayerGui
    ("GetGuiObjectsAtPosition", "LayerCollector"),
    ("GetGuiObjectsAtPosition", "BasePlayerGui"),
    ("GetLayoutNodeTree", "LayerCollector"),
    ("GetTopbarTransparency", "PlayerGui"),
    ("SetTopbarTransparency", "PlayerGui"),
    // TextBox
    ("CaptureFocus", "TextBox"),
    ("IsFocused", "TextBox"),
    ("ReleaseFocus", "TextBox"),
    // ScrollingFrame
    ("GetScrollVelocity", "ScrollingFrame"),
    ("ResetScrollVelocity", "ScrollingFrame"),
    // Layouts
    ("JumpTo", "UIPageLayout"),
    ("JumpToIndex", "UIPageLayout"),
    ("Next", "UIPageLayout"),
    ("Previous", "UIPageLayout"),
    ("ApplyLayout", "UIGridStyleLayout"),
    ("SetCustomSortFunction", "UIGridStyleLayout"),
    // UIDragDetector
    ("AddConstraintFunction", "UIDragDetector"),
    ("GetReferencePosition", "UIDragDetector"),
    ("GetReferenceRotation", "UIDragDetector"),
    ("SetDragStyleFunction", "UIDragDetector"),
    // Styles
    ("GetDefaultPropertyTransition", "StyleRule"),
    ("GetProperties", "StyleRule"),
    ("GetProperty", "StyleRule"),
    ("GetPropertyTransitions", "StyleRule"),
    ("SetDefaultPropertyTransition", "StyleRule"),
    ("SetProperties", "StyleRule"),
    ("SetProperty", "StyleRule"),
    ("SetPropertyTransition", "StyleRule"),
    ("SetPropertyTransitions", "StyleRule"),
    ("GetDerives", "StyleSheet"),
    ("SetDerives", "StyleSheet"),
    ("GetCondition", "StyleQuery"),
    ("GetConditions", "StyleQuery"),
    ("SetCondition", "StyleQuery"),
    ("SetConditions", "StyleQuery"),
    ("GetStyleRules", "StyleBase"),
    ("InsertStyleRule", "StyleBase"),
    ("SetStyleRules", "StyleBase"),
    // ParticleEmitter2D
    ("Clear", "ParticleEmitter2D"),
    ("Emit", "ParticleEmitter2D"),
    // Path2D (Rust)
    ("GetBoundingRect", "Path2D"),
    ("GetControlPoint", "Path2D"),
    ("GetControlPoints", "Path2D"),
    ("GetLength", "Path2D"),
    ("GetMaxControlPoints", "Path2D"),
    ("GetPositionOnCurve", "Path2D"),
    ("GetPositionOnCurveArcLength", "Path2D"),
    ("GetTangentOnCurve", "Path2D"),
    ("GetTangentOnCurveArcLength", "Path2D"),
    ("InsertControlPoint", "Path2D"),
    ("RemoveControlPoint", "Path2D"),
    ("SetControlPoints", "Path2D"),
    ("UpdateControlPoint", "Path2D"),
    // Input actions
    ("Fire", "InputAction"),
    ("GetState", "InputAction"),
    ("Fire", "InputBinding"),
    // Gamepad / haptics / VR
    ("DisableGamepadCursor", "GamepadService"),
    ("EnableGamepadCursor", "GamepadService"),
    ("GetMotor", "HapticService"),
    ("IsMotorSupported", "HapticService"),
    ("IsVibrationSupported", "HapticService"),
    ("SetMotor", "HapticService"),
    ("Play", "HapticEffect"),
    ("SetWaveformKeys", "HapticEffect"),
    ("Stop", "HapticEffect"),
    ("GetTouchpadMode", "VRService"),
    ("SetTouchpadMode", "VRService"),
    ("IsVRAppBuild", "VRService"),
    ("IsMaquettes", "VRService"),
    // ProximityPrompt / Dialog / TextGenerator
    ("InputHoldBegin", "ProximityPrompt"),
    ("InputHoldEnd", "ProximityPrompt"),
    ("GetCurrentPlayers", "Dialog"),
];

/// Yielding methods, written in gui_input.luau (put in `__rs_lua_methods`).
pub const LUA_METHODS: &[(&str, &str)] = &[
    ("GenerateTextAsync", "TextGenerator"),
    ("CaptureSnapshotAsync", "ViewportFrame"),
];

/// Events this domain fires (the rest of its classes' events were already
/// known to runtime_ext.rs; gui_input.luau fires many of those too).
pub const EVENTS: &[(&str, &str)] = &[
    ("DeviceAccelerationChanged", "UserInputService"),
    ("DeviceGravityChanged", "UserInputService"),
    ("TouchDrag", "UserInputService"),
    ("UserCFrameChanged", "UserInputService"),
    ("LocalToolEquipped", "ContextActionService"),
    ("LocalToolUnequipped", "ContextActionService"),
    ("SafeZoneOffsetsChanged", "GuiService"),
    ("ErrorMessageChanged", "GuiService"),
    ("NativeClose", "GuiService"),
    ("CoreGuiRenderOverflowed", "GuiService"),
    ("BrowserWindowClosed", "GuiService"),
    ("CloseInspectMenuRequest", "GuiService"),
    ("EmotesMenuOpenChanged", "GuiService"),
    ("GuiVisibilityChangedSignal", "GuiService"),
    ("InspectMenuEnabledChangedSignal", "GuiService"),
    ("InspectPlayerFromHumanoidDescriptionRequest", "GuiService"),
    ("InspectPlayerFromUserIdWithCtxRequest", "GuiService"),
    ("KeyPressed", "GuiService"),
    ("NetworkPausedEnabledChanged", "GuiService"),
    ("PurchasePromptShown", "GuiService"),
    ("ScrollStateChanged", "GuiService"),
    ("ShowLeaveConfirmation", "GuiService"),
    ("SpecialKeyPressed", "GuiService"),
    ("UiMessageChanged", "GuiService"),
    ("Open9SliceEditor", "GuiService"),
    ("OpenStyleEditor", "GuiService"),
    ("LaserPointerTriggered", "VRService"),
    ("DragBegin", "GuiObject"),
    ("DragStopped", "GuiObject"),
    ("TouchLongPress", "GuiObject"),
    ("TouchPan", "GuiObject"),
    ("TouchPinch", "GuiObject"),
    ("TouchRotate", "GuiObject"),
    ("TouchSwipe", "GuiObject"),
    ("SelectionChanged", "GuiBase2d"),
    ("TopbarTransparencyChangedSignal", "PlayerGui"),
    ("PageEnter", "UIPageLayout"),
    ("PageLeave", "UIPageLayout"),
    ("Stopped", "UIPageLayout"),
    ("DragStart", "UIDragDetector"),
    ("DragContinue", "UIDragDetector"),
    ("DragEnd", "UIDragDetector"),
    ("StyleRulesChanged", "StyleBase"),
    ("ControlPointChanged", "Path2D"),
    ("Pressed", "InputAction"),
    ("Released", "InputAction"),
    ("StateChanged", "InputAction"),
    ("Ended", "HapticEffect"),
    ("TouchpadModeChanged", "VRService"),
    ("IndicatorShown", "ProximityPrompt"),
    ("IndicatorHidden", "ProximityPrompt"),
    ("PromptShown", "ProximityPromptService"),
    ("PromptHidden", "ProximityPromptService"),
    ("PromptTriggered", "ProximityPromptService"),
    ("PromptTriggerEnded", "ProximityPromptService"),
    ("PromptButtonHoldBegan", "ProximityPromptService"),
    ("PromptButtonHoldEnded", "ProximityPromptService"),
    ("IndicatorShown", "ProximityPromptService"),
    ("IndicatorHidden", "ProximityPromptService"),
    ("DialogChoiceSelected", "Dialog"),
    ("MouseButton1Down", "HandleAdornment"),
    ("MouseButton1Up", "HandleAdornment"),
    ("MouseEnter", "HandleAdornment"),
    ("MouseLeave", "HandleAdornment"),
    ("MouseButton1Down", "HandlesBase"),
    ("MouseButton1Up", "HandlesBase"),
    ("MouseDrag", "HandlesBase"),
    ("MouseEnter", "HandlesBase"),
    ("MouseLeave", "HandlesBase"),
];

/// Properties of classes rbx-reflection doesn't know, kept in the DOM (so
/// saved and replicated like any other): class, name, type, enum, Studio's
/// default (a Luau expression; measured, the API dump's are placeholders).
const STORED: &[(&str, &str, VariantType, &str, &str)] = &[
    ("UserInputService", "OverrideMouseIconBehavior", VariantType::Enum, "OverrideMouseIconBehavior", "Enum.OverrideMouseIconBehavior.None"),
    ("ScreenGui", "OnTopOfCoreBlur", VariantType::Bool, "", "false"),
    ("ScreenGui", "IgnoresTitleBarReservation", VariantType::Bool, "", "false"),
    ("UIBlur", "Color", VariantType::Color3, "", "Color3.new(1, 1, 1)"),
    ("UIBlur", "Enabled", VariantType::Bool, "", "true"),
    ("UIBlur", "Strength", VariantType::Enum, "UIBlurStrength", "Enum.UIBlurStrength.Heavy"),
    ("UIBlur", "Transparency", VariantType::Float32, "", "0"),
    ("UIDragDetector", "Selectable", VariantType::Bool, "", "true"),
    ("InputBinding", "Deadzone", VariantType::NumberRange, "", "NumberRange.new(0.1, 1)"),
    ("ParticleEmitter2D", "Acceleration", VariantType::Vector2, "", "Vector2.zero"),
    ("ParticleEmitter2D", "Blend", VariantType::Float32, "", "0"),
    ("ParticleEmitter2D", "Brightness", VariantType::Float32, "", "1"),
    ("ParticleEmitter2D", "Color", VariantType::ColorSequence, "", "ColorSequence.new(Color3.new(1, 1, 1))"),
    ("ParticleEmitter2D", "Drag", VariantType::Float32, "", "0"),
    ("ParticleEmitter2D", "EmissionAngle", VariantType::Float32, "", "0"),
    ("ParticleEmitter2D", "EmissionMode", VariantType::Enum, "ParticleEmitter2DEmissionMode", "Enum.ParticleEmitter2DEmissionMode.BoundingBox"),
    ("ParticleEmitter2D", "EmissionOffset", VariantType::Vector2, "", "Vector2.zero"),
    ("ParticleEmitter2D", "EmissionSpread", VariantType::Vector2, "", "Vector2.zero"),
    ("ParticleEmitter2D", "Enabled", VariantType::Bool, "", "true"),
    ("ParticleEmitter2D", "FlipbookBlendFrames", VariantType::Bool, "", "true"),
    ("ParticleEmitter2D", "FlipbookFramerate", VariantType::NumberRange, "", "NumberRange.new(1)"),
    ("ParticleEmitter2D", "FlipbookLayout", VariantType::Enum, "ParticleFlipbookLayout", "Enum.ParticleFlipbookLayout.None"),
    ("ParticleEmitter2D", "FlipbookMode", VariantType::Enum, "ParticleFlipbookMode", "Enum.ParticleFlipbookMode.Loop"),
    ("ParticleEmitter2D", "FlipbookSizeX", VariantType::Int32, "", "1"),
    ("ParticleEmitter2D", "FlipbookSizeY", VariantType::Int32, "", "1"),
    ("ParticleEmitter2D", "FlipbookStartRandom", VariantType::Bool, "", "false"),
    ("ParticleEmitter2D", "Lifetime", VariantType::NumberRange, "", "NumberRange.new(5, 10)"),
    ("ParticleEmitter2D", "LightEmission", VariantType::Float32, "", "0"),
    ("ParticleEmitter2D", "LockedToParent", VariantType::Bool, "", "false"),
    ("ParticleEmitter2D", "Rate", VariantType::Float32, "", "20"),
    ("ParticleEmitter2D", "RotSpeed", VariantType::NumberRange, "", "NumberRange.new(0)"),
    ("ParticleEmitter2D", "Rotation", VariantType::NumberRange, "", "NumberRange.new(0)"),
    ("ParticleEmitter2D", "Size", VariantType::NumberSequence, "", "NumberSequence.new(32)"),
    ("ParticleEmitter2D", "Speed", VariantType::NumberRange, "", "NumberRange.new(64)"),
    ("ParticleEmitter2D", "SpreadAngle", VariantType::Float32, "", "0"),
    ("ParticleEmitter2D", "Squash", VariantType::NumberSequence, "", "NumberSequence.new(0)"),
    ("ParticleEmitter2D", "TextureContent", VariantType::Content, "", "Content.fromUri(\"rbxasset://textures/particles/sparkles_main.dds\")"),
    ("ParticleEmitter2D", "TimeScale", VariantType::Float32, "", "1"),
    ("ParticleEmitter2D", "Transparency", VariantType::NumberSequence, "", "NumberSequence.new(0)"),
    ("ParticleEmitter2D", "ZIndex", VariantType::Int32, "", "1"),
];

fn stored(class: &str, key: &str) -> Option<&'static (&'static str, &'static str, VariantType, &'static str, &'static str)> {
    STORED.iter().find(|s| s.1 == key && rustudio_core::doc::is_a(class, s.0))
}

/// Computed properties answered by gui_input.luau's `get["Class.Prop"]`
/// (class matched with IsA).
const LUA_PROPS: &[(&str, &str)] = &[
    ("GuiService", "CoreGuiNavigationEnabled"),
    ("GuiService", "SendCoreUiNotification"),
    ("GuiService", "PreferredTextSize"),
    ("GuiService", "PreferredTransparency"),
    ("GuiService", "ReducedMotionEnabled"),
    ("GuiService", "TopbarInset"),
    ("GuiService", "ViewportDisplaySize"),
    ("GuiService", "SelectedCoreObject"),
    ("GuiService", "CoreGuiFolder"),
    ("GuiService", "CoreEffectFolder"),
    ("GuiService", "DisplayScalingMode"),
    ("GuiService", "ViewportSizeInMM"),
    ("GuiService", "IsWindows"),
    ("UserInputService", "OnScreenKeyboardPosition"),
    ("UserInputService", "OnScreenKeyboardSize"),
    ("UserInputService", "UserHeadCFrame"),
    ("GuiObject", "GuiState"),
    ("GuiObject", "InputSink"),
    ("GuiObject", "Transparency"),
    ("BillboardGui", "CurrentDistance"),
    ("PlayerGui", "CurrentScreenOrientation"),
    ("CoreGui", "Version"),
    ("TextBox", "ContentText"),
    ("TextBox", "CursorPosition"),
    ("TextBox", "SelectionStart"),
    ("TextBox", "OpenTypeFeaturesError"),
    ("TextBox", "TextFits"),
    ("TextButton", "ContentText"),
    ("TextButton", "LocalizedText"),
    ("TextButton", "OpenTypeFeaturesError"),
    ("TextButton", "TextFits"),
    ("TextLabel", "ContentText"),
    ("TextLabel", "LocalizedText"),
    ("TextLabel", "OpenTypeFeaturesError"),
    ("TextLabel", "TextFits"),
    ("ImageButton", "IsLoaded"),
    ("ImageLabel", "IsLoaded"),
    ("ScrollingFrame", "AbsoluteCanvasSize"),
    ("ScrollingFrame", "AbsoluteWindowSize"),
    ("UIPageLayout", "CurrentPage"),
    ("UIGridLayout", "AbsoluteCellCount"),
    ("UIGridLayout", "AbsoluteCellSize"),
    ("StyleRule", "SelectorError"),
    ("StyleQuery", "IsActive"),
    ("InputAction", "PreferredBinding"),
    ("InputActionLabel", "ResolvedText"),
    ("InputActionLabel", "ResolvedImageContent"),
    ("VRService", "GuiInputUserCFrame"),
    ("VRService", "VRDeviceAvailable"),
    ("VRService", "VRDeviceName"),
    ("VRService", "VRSessionState"),
    ("VRService", "DidPointerHit"),
    ("VRService", "LaserDistance"),
    ("VRService", "PointerHitCFrame"),
    ("VRService", "QuestDisplayRefreshRate"),
    ("VRService", "QuestASWState"),
    ("VRService", "LaserPointer"),
    ("VRService", "PresentationMode"),
    ("Dialog", "InUse"),
    ("GuiService", "MenuIsOpen"),
];

fn lua_prop(class: &str, key: &str) -> Option<&'static str> {
    LUA_PROPS
        .iter()
        .find(|(c, k)| *k == key && rustudio_core::doc::is_a(class, c))
        .map(|(c, _)| *c)
}

/// Writes gui_input.luau acts on ("Class.Prop" in its `set` table).
const LUA_WRITES: &[(&str, &str)] = &[
    ("TextBox", "CursorPosition"),
    ("TextBox", "SelectionStart"),
    ("GuiService", "SelectedObject"),
    ("GuiService", "SelectedCoreObject"),
    ("GuiService", "CoreGuiNavigationEnabled"),
    ("GuiService", "SendCoreUiNotification"),
    ("GuiObject", "InputSink"),
    ("GuiObject", "Interactable"),
    ("GuiObject", "Transparency"),
    ("VRService", "GuiInputUserCFrame"),
    ("Dialog", "InUse"),
    ("InputAction", "Type"),
    ("InputAction", "Enabled"),
    ("InputContext", "Enabled"),
];

fn lua_write(class: &str, key: &str) -> Option<&'static str> {
    LUA_WRITES
        .iter()
        .find(|(c, k)| *k == key && rustudio_core::doc::is_a(class, c))
        .map(|(c, _)| *c)
}

/// A non-Instance engine Object (typeof "Object": VirtualInput), its
/// methods from a Lua table.
struct LObject {
    class: String,
    methods: Table,
}

impl mlua::UserData for LObject {
    fn add_fields<F: mlua::UserDataFields<Self>>(f: &mut F) {
        f.add_meta_field("__type", "Object");
    }

    fn add_methods<M: mlua::UserDataMethods<Self>>(m: &mut M) {
        m.add_meta_method(mlua::MetaMethod::Index, |_, this, key: String| {
            if key == "ClassName" {
                return Ok(Value::String(this.methods.raw_get::<mlua::String>("__class")?));
            }
            let f: Value = this.methods.raw_get(key.as_str())?;
            if f.is_nil() || key.starts_with("__") {
                return Err(rt_err(format!("{key} is not a valid member of {}", this.class)));
            }
            Ok(f)
        });
        m.add_meta_method(mlua::MetaMethod::NewIndex, |_, this, (k, _): (String, Value)| {
            Err::<(), _>(rt_err(format!("{k} is not a valid member of {}", this.class)))
        });
        m.add_meta_method(mlua::MetaMethod::ToString, |_, this, ()| Ok(this.class.clone()));
    }
}

fn module(lua: &Lua) -> mlua::Result<Table> {
    lua.named_registry_value::<Table>("rs_gui_input")
}

fn class_of(lua: &Lua, r: Ref) -> mlua::Result<String> {
    Ok(world(lua)?
        .d()
        .class_of(r)
        .map(|c| c.to_string())
        .unwrap_or_default())
}

pub fn call(lua: &Lua, name: &str, this: Ref, args: &MultiValue) -> Option<mlua::Result<MultiValue>> {
    if !METHODS.iter().any(|(m, _)| *m == name) {
        return None;
    }
    let class = match class_of(lua, this) {
        Ok(c) => c,
        Err(e) => return Some(Err(e)),
    };
    let base = METHODS
        .iter()
        .find(|(m, b)| *m == name && rustudio_core::doc::is_a(&class, b))
        .map(|(_, b)| *b)?;
    if base == "Path2D" {
        return Some(path2d::call(lua, name, this, args));
    }
    lua.remove_app_data::<EditStepCache>();
    Some((|| {
        let m: Table = module(lua)?.get("methods")?;
        let f: Function = m.get(format!("{base}.{name}"))?;
        f.call::<MultiValue>(args.clone())
    })())
}

pub fn prop_get(lua: &Lua, r: Ref, class: &str, key: &str) -> mlua::Result<Option<Value>> {
    if let Some(s) = stored(class, key) {
        let v = world(lua)?.d().prop(r, key);
        return Ok(Some(match v {
            Some(v) => variant_to_lua(lua, &v, (!s.3.is_empty()).then_some(s.3))?,
            None => default_of(lua, s.4)?,
        }));
    }
    if class == "ScrollingFrame" && matches!(key, "AbsoluteCanvasSize" | "AbsoluteWindowSize") {
        return crate::runtime::ext::scrolling_prop(lua, r, key).map(Some);
    }
    if let Some(base) = lua_prop(class, key) {
        let get: Table = module(lua)?.get("get")?;
        let f: Function = get.get(format!("{base}.{key}"))?;
        return Ok(Some(f.call::<Value>((inst_value(lua, r)?, key))?));
    }
    Ok(None)
}

fn default_of(lua: &Lua, expr: &str) -> mlua::Result<Value> {
    let cache: Table = module(lua)?.get("defaults")?;
    let v: Value = cache.get(expr)?;
    if !v.is_nil() {
        return Ok(v);
    }
    let v: Value = lua.load(format!("return {expr}")).set_name("=gui_input.default").eval()?;
    cache.set(expr, v.clone())?;
    Ok(v)
}

pub fn newindex(lua: &Lua, r: Ref, class: &str, key: &str, value: &Value) -> mlua::Result<bool> {
    if let Some(s) = stored(class, key) {
        let existing = world(lua)?.d().prop(r, key);
        let en = (!s.3.is_empty()).then_some(s.3);
        let v = lua_to_variant(key, value, s.2, en, existing.as_ref())?;
        store_prop(lua, r, key, v)?;
        lua.remove_app_data::<EditStepCache>();
        return Ok(true);
    }
    if let Some(base) = lua_write(class, key) {
        let set: Table = module(lua)?.get("set")?;
        let f: Function = set.get(format!("{base}.{key}"))?;
        // true: the Luau stored it itself; false: store it as usual too.
        let done: bool = f.call((inst_value(lua, r)?, key, value.clone()))?;
        lua.remove_app_data::<EditStepCache>();
        return Ok(done);
    }
    Ok(false)
}

pub fn write_acts(class: &str, key: &str) -> bool {
    stored(class, key).is_some() || lua_write(class, key).is_some()
}

/// The library's InputObject maker, for Rust callers.
/// Every GuiObject drawn at (x, y) in GUI coordinates (AbsolutePosition's)
/// in `guis` (LayerCollectors, top-most first), top-most first: ZIndexBehavior's
/// order inside each, clipped by ClipsDescendants and ScrollingFrames. Boxes
/// are AbsolutePosition / AbsoluteSize as a script reads them (abs_now).
fn objects_at(w: &mut World, guis: &[Ref], x: f32, y: f32) -> Vec<Ref> {
    use rustudio_core::doc::is_a;
    fn class(w: &World, r: Ref) -> Option<&str> {
        w.d().dom.get_by_ref(r).map(|i| i.class.as_str())
    }
    fn boolp(w: &World, r: Ref, name: &str, default: bool) -> bool {
        match w.d().prop(r, name) {
            Some(Variant::Bool(b)) => b,
            _ => default,
        }
    }
    fn zindex(w: &World, r: Ref) -> i64 {
        match w.d().prop(r, "ZIndex") {
            Some(Variant::Int32(z)) => z as i64,
            Some(Variant::Int64(z)) => z,
            _ => 1,
        }
    }
    // GuiObject children, a Folder's taken in its place.
    fn kids(w: &World, p: Ref, out: &mut Vec<Ref>) {
        let Some(i) = w.d().dom.get_by_ref(p) else { return };
        for &c in i.children() {
            match class(w, c) {
                Some(k) if is_a(k, "GuiObject") => out.push(c),
                Some(k) if is_a(k, "Folder") => kids(w, c, out),
                _ => {}
            }
        }
    }
    let (x, y) = (x as f64, y as f64);
    let inside = |w: &mut World, o: Ref| {
        let b = crate::runtime::ext::abs_now(w, o);
        let (px, py, sx, sy) = (b.pos[0] as f64, b.pos[1] as f64, b.size[0] as f64, b.size[1] as f64);
        if b.rot.abs() > 1e-3 {
            let cx = px + sx * 0.5;
            let cy = py + sy * 0.5;
            let rad = -(b.rot as f64).to_radians();
            let (sn, cs) = rad.sin_cos();
            let dx = x - cx;
            let dy = y - cy;
            let lx = cs * dx - sn * dy;
            let ly = sn * dx + cs * dy;
            lx >= -sx * 0.5 && ly >= -sy * 0.5 && lx < sx * 0.5 && ly < sy * 0.5
        } else {
            x >= px && y >= py && x < px + sx && y < py + sy
        }
    };
    // One walk in draw order (ZIndex then child order: among siblings, or
    // across the whole GUI for a Global ScreenGui). An invisible object's
    // subtree is never drawn, so it isn't walked. `clip_ok`: every GuiObject
    // above (up to the first non-GuiObject: a Folder ends the climb) that
    // clips (ClipsDescendants, ScrollingFrame) holds the point.
    struct Walk<'a> {
        global: bool,
        all: Vec<(Ref, i64, usize)>,
        inside: &'a dyn Fn(&mut World, Ref) -> bool,
    }
    fn walk(w: &mut World, st: &mut Walk, p: Ref, p_gui: bool, clip_ok: bool) {
        let mut list = Vec::new();
        {
            let w = &*w;
            kids(w, p, &mut list);
        }
        // Children reached through a Folder have it as their parent: the climb stops there.
        let mut keyed: Vec<(i64, usize, Ref, bool)> = list
            .into_iter()
            .enumerate()
            .map(|(i, c)| {
                let direct = w.d().dom.get_by_ref(c).is_some_and(|k| k.parent() == p);
                (zindex(w, c), i, c, direct)
            })
            .collect();
        if !st.global {
            keyed.sort_unstable_by_key(|e| (e.0, e.1));
        }
        let mine = p_gui && clip_ok && {
            let clips = class(w, p).is_some_and(|k| is_a(k, "ScrollingFrame")) || boolp(w, p, "ClipsDescendants", false);
            let rotated = clips && crate::runtime::ext::abs_now(w, p).rot.abs() > 1e-3;
            !clips || rotated || (st.inside)(w, p)
        };
        for (z, _, c, direct) in keyed {
            if !boolp(w, c, "Visible", true) {
                continue;
            }
            let ok = if direct { if p_gui { mine } else { true } } else { true };
            let seq = st.all.len();
            st.all.push((if ok { c } else { Ref::none() }, z, seq));
            walk(w, st, c, true, ok);
        }
    }
    let mut res = Vec::new();
    for &g in guis {
        let global = class(w, g) == Some("ScreenGui")
            && matches!(w.d().prop(g, "ZIndexBehavior"), Some(Variant::Enum(e)) if e.to_u32() == 0);
        // (every object's climb ends at this LayerCollector: drawn while it's Enabled)
        if !boolp(w, g, "Enabled", true) {
            continue;
        }
        let mut st = Walk { global, all: Vec::new(), inside: &inside };
        walk(w, &mut st, g, false, true);
        let mut all = st.all;
        if global {
            all.sort_unstable_by_key(|e| (e.1, e.2));
        }
        let mut here = Vec::new();
        for (o, _, _) in all {
            if !o.is_none() && inside(w, o) {
                here.push(o);
            }
        }
        res.extend(here.into_iter().rev());
    }
    res
}

fn input_object(lua: &Lua, fields: Table) -> mlua::Result<Value> {
    let f: Function = lua.globals().get("__rs_new_input_object")?;
    f.call(fields)
}

pub fn install(lua: &Lua) -> mlua::Result<()> {
    path2d::install(lua)?;
    let g = lua.create_table()?;
    // fire(inst, name, ...): queue an event the way the engine does.
    g.set(
        "fire",
        lua.create_function(|lua, args: MultiValue| {
            let mut it = args.into_iter();
            let r = it.next().as_ref().and_then(opt_inst).ok_or_else(|| rt_err("fire: Instance expected"))?;
            let name = match it.next() {
                Some(Value::String(s)) => s.to_str()?.to_string(),
                _ => return Err(rt_err("fire: event name expected")),
            };
            world(lua)?.fire(r, &name, it.collect());
            Ok(())
        })?,
    )?;
    // Studio's error / warning lines (Output) without throwing.
    g.set(
        "log",
        lua.create_function(|lua, (level, msg): (String, String)| {
            let lv = match level.as_str() {
                "error" => rustudio_core::log::Level::Error,
                "warn" => rustudio_core::log::Level::Warn,
                _ => rustudio_core::log::Level::Info,
            };
            world(lua)?.output.push((lv, msg));
            Ok(())
        })?,
    )?;
    // The GUI module keeps its DataModel and Workspace across frames.
    // Refresh the Lua references only when those instances change.
    let cached_roots = std::cell::Cell::new(None::<(u64, u64, Ref, Option<Ref>)>);
    g.set(
        "world_roots",
        lua.create_function(move |lua, ()| {
            let previous = cached_roots.get();
            let (doc_id, revision, root, workspace) = {
                let w = world(lua)?;
                let d = w.d();
                if previous.is_some_and(|(id, rev, _, _)| id == d.id && rev == d.tree_revision) {
                    return Ok((Value::Nil, Value::Nil));
                }
                let root = d.dom.root_ref();
                let workspace = d.dom.get_by_ref(root).and_then(|r| {
                    r.children().iter().copied().find(|child| d.class_of(*child).is_some_and(|class| class == "Workspace"))
                });
                (d.id, d.tree_revision, root, workspace)
            };
            let old = previous.filter(|(id, _, _, _)| *id == doc_id);
            let game = if old.is_none_or(|(_, _, prev, _)| root != prev) {
                inst_value(lua, root)?
            } else {
                Value::Nil
            };
            let workspace_value = match workspace {
                Some(ws) if old.is_none_or(|(_, _, _, prev)| Some(ws) != prev) => inst_value(lua, ws)?,
                _ => Value::Nil,
            };
            cached_roots.set(Some((doc_id, revision, root, workspace)));
            Ok((game, workspace_value))
        })?,
    )?;
    // class_signals({classes}): (added, removing) signals on game for the
    // instances of those classes only (runtime::class_signals).
    g.set(
        "class_signals",
        lua.create_function(|lua, classes: Vec<String>| crate::runtime::class_signals(lua, classes))?,
    )?;
    g.set(
        "is_client",
        // (the edit DataModel's plugins see the edit viewport as a client
        // does its view: src/plugin_host/widgets_ui.rs feeds its input)
        lua.create_function(|lua, ()| Ok(world(lua).is_ok_and(|w| w.side == Side::Client || w.mode == RunMode::Edit)))?,
    )?;
    g.set(
        "is_edit",
        lua.create_function(|lua, ()| Ok(world(lua).is_ok_and(|w| w.mode == RunMode::Edit)))?,
    )?;
    g.set(
        "topbar_inset",
        lua.create_function(|lua, ()| {
            let w = world(lua)?;
            Ok(w.topbar_inset(w.input.view_size[0]))
        })?,
    )?;
    g.set(
        "set_topbar_inset",
        lua.create_function(|lua, rect: LRect| {
            let mut w = world(lua)?;
            if w.topbar_inset == Some(rect) {
                return Ok(false);
            }
            w.topbar_inset = Some(rect);
            w.abs_all = true;
            Ok(true)
        })?,
    )?;
    // A handle adornment is under the mouse or held: the edit viewport's
    // selection tools leave the press to it (Studio's priority).
    g.set(
        "set_adorn_hot",
        lua.create_function(|_, (on, handles): (bool, Option<bool>)| {
            let mut s = crate::gui::share();
            s.adorn_hot = on;
            s.adorn_handles = on && handles.unwrap_or(false);
            Ok(())
        })?,
    )?;
    g.set(
        "playing",
        lua.create_function(|lua, ()| {
            Ok(world(lua).is_ok_and(|w| w.side == Side::Client && w.mode == RunMode::Play && w.has_peer))
        })?,
    )?;
    g.set(
        "time",
        lua.create_function(|lua, ()| Ok(world(lua).map(|w| w.time).unwrap_or(0.0)))?,
    )?;
    g.set(
        "view",
        lua.create_function(|lua, ()| {
            let w = world(lua)?;
            let v = w.input.view_size;
            Ok((v[0], v[1], w.input.mouse_xy[0], w.input.mouse_xy[1]))
        })?,
    )?;
    g.set(
        "gamepads",
        lua.create_function(|lua, ()| Ok(world(lua).map(|w| w.input.gamepads).unwrap_or(0)))?,
    )?;
    g.set(
        "mouse",
        lua.create_function(|lua, ()| {
            // (The World borrow ends before inst_value borrows it again.)
            let m = world(lua)?.input.mouse;
            match m {
                Some(m) => inst_value(lua, m),
                None => Ok(Value::Nil),
            }
        })?,
    )?;
    g.set(
        "menu_open",
        lua.create_function(|lua, ()| Ok(world(lua).is_ok_and(|w| w.input.menu_open)))?,
    )?;
    // Drawn box of a GuiObject last frame: x, y, w, h, canvas w, canvas h (or nil).
    g.set(
        "layout",
        lua.create_function(|lua, inst: Value| {
            let r = opt_inst(&inst).ok_or_else(|| rt_err("Instance expected"))?;
            let w = world(lua)?;
            Ok(match w.input.gui_layout.get(&r) {
                Some(b) => {
                    let c = if w.d().class_of(r).as_deref() == Some("ScrollingFrame") {
                        let cs = match w.d().prop(r, "CanvasSize") {
                            Some(Variant::UDim2(u)) => [u.x.scale, u.x.offset as f32, u.y.scale, u.y.offset as f32],
                            _ => [0.0, 0.0, 2.0, 0.0],
                        };
                        b.canvas_for(cs).or(b.canvas).unwrap_or([-1.0, -1.0])
                    } else {
                        b.canvas.unwrap_or([-1.0, -1.0])
                    };
                    let t = b.text.unwrap_or([-1.0, -1.0]);
                    let win = b.window.unwrap_or([-1.0, -1.0]);
                    MultiValue::from_iter([
                        Value::Number(b.pos[0] as f64),
                        Value::Number(b.pos[1] as f64),
                        Value::Number(b.size[0] as f64),
                        Value::Number(b.size[1] as f64),
                        Value::Number(c[0] as f64),
                        Value::Number(c[1] as f64),
                        Value::Number(t[0] as f64),
                        Value::Number(t[1] as f64),
                        Value::Number(win[0] as f64),
                        Value::Number(win[1] as f64),
                        Value::Boolean(b.text_fits),
                    ])
                }
                None => MultiValue::new(),
            })
        })?,
    )?;
    // objects_at({LayerCollector…}, x, y): GetGuiObjectsAtPosition's walk
    // (gui_input.luau), native: a test place's PlayerGui has ~4,600 GuiObjects and
    // Kohl's Admin asks on every mouse move over its buttons; walked in Luau
    // through the engine that was 80+ ms a call.
    g.set(
        "objects_at",
        lua.create_function(|lua, (guis, x, y): (Vec<Value>, f64, f64)| {
            let guis: Vec<Ref> = guis.iter().filter_map(opt_inst).collect();
            let hits = {
                let mut w = world(lua)?;
                objects_at(&mut w, &guis, x as f32, y as f32)
            };
            let out = lua.create_table_with_capacity(hits.len(), 0)?;
            for (i, r) in hits.into_iter().enumerate() {
                out.raw_set(i + 1, inst_value(lua, r)?)?;
            }
            Ok(out)
        })?,
    )?;
    // The viewport GUI's shared state (crates/rustudio-engine/src/gui.rs GuiShare).
    g.set(
        "focus",
        lua.create_function(|lua, ()| match crate::gui::share().focus {
            Some(r) => inst_value(lua, r),
            None => Ok(Value::Nil),
        })?,
    )?;
    g.set(
        "set_focus",
        lua.create_function(|_, inst: Value| {
            let r = opt_inst(&inst);
            let mut sh = crate::gui::share();
            sh.focus = r;
            sh.focus_req.push(r);
            Ok(())
        })?,
    )?;
    g.set(
        "sync_tb_sel",
        lua.create_function(|_, (inst, cur, sel): (Value, Option<i32>, Option<i32>)| {
            if let Some(r) = opt_inst(&inst) {
                let mut sh = crate::gui::share();
                if let Some(c) = cur {
                    sh.tb_cursor.insert(r, c);
                }
                if let Some(s) = sel {
                    sh.tb_sel.insert(r, s);
                }
                sh.tb_script_sel.push((r, cur, sel));
            }
            Ok(())
        })?,
    )?;
    g.set(
        "take_focus_click",
        lua.create_function(|_, inst: Value| {
            let Some(r) = opt_inst(&inst) else { return Ok(None) };
            let mut sh = crate::gui::share();
            if sh.tb_focus_click.is_some_and(|(fr, _)| fr == r) {
                Ok(sh.tb_focus_click.take().map(|(_, pos)| pos as i32))
            } else {
                Ok(None)
            }
        })?,
    )?;
    g.set(
        "set_text_silent",
        lua.create_function(|lua, (inst, text): (Value, String)| {
            if let Some(r) = opt_inst(&inst) {
                let mut w = world(lua)?;
                let d = w.dm();
                if d.exists(r) {
                    d.set_prop_live(r, rbx_dom_weak::ustr("Text"), Variant::String(text));
                }
            }
            Ok(())
        })?,
    )?;
    // Sync GuiService.SelectedCoreObject into the Document so gui_walk can draw
    // the white keyboard/gamepad focus ring around the selected CoreGui object.
    g.set(
        "set_selected_core",
        lua.create_function(|lua, (gs, inst): (Value, Value)| {
            if let Some(gs_r) = opt_inst(&gs) {
                let target = opt_inst(&inst).unwrap_or_else(Ref::none);
                let mut w = world(lua)?;
                let d = w.dm();
                if d.exists(gs_r) {
                    d.set_prop_live(gs_r, rbx_dom_weak::ustr("SelectedCoreObject"), Variant::Ref(target));
                }
            }
            Ok(())
        })?,
    )?;
    g.set(
        "set_core_menu_open",
        lua.create_function(|_, open: bool| {
            crate::gui::share().core_menu_open = open;
            Ok(())
        })?,
    )?;
    // Fast native class scan over DataModel skipping CorePackages (49k+ ModuleScripts),
    // replacing full-tree Lua game:GetDescendants() scans at Play startup.
    g.set(
        "find_classes",
        lua.create_function(|lua, classes: Vec<String>| {
            let found = {
                let w = world(lua)?;
                let d = w.d();
                let mut out = Vec::new();
                let mut stack = vec![d.dom.root_ref()];
                while let Some(r) = stack.pop() {
                    let Some(inst) = d.dom.get_by_ref(r) else { continue };
                    if inst.class == "CorePackages" {
                        continue;
                    }
                    if classes.iter().any(|c| rustudio_core::doc::is_a(&inst.class, c)) {
                        out.push(r);
                    }
                    for &child in inst.children().iter().rev() {
                        stack.push(child);
                    }
                }
                out
            };
            let t = lua.create_table_with_capacity(found.len(), 0)?;
            for (i, r) in found.into_iter().enumerate() {
                t.raw_set(i + 1, inst_value(lua, r)?)?;
            }
            Ok(t)
        })?,
    )?;
    g.set(
        "profile_step",
        lua.create_function(|_, (phase, step, args): (u8, Function, MultiValue)| {
            let name = match phase {
                1 => "input_prune",
                2 => "input_styles",
                3 => "input_styles_rules",
                4 => "input_styles_index",
                5 => "input_styles_match",
                6 => "input_styles_apply",
                _ => return Err(rt_err("unknown GUI step phase")),
            };
            let _scope = rustudio_core::bench::scope(name);
            step.call::<()>(args)
        })?,
    )?;
    // Fast native style target walk under a StyleLink root: returns only
    // styleable UI nodes (GuiBase2d / UIBase), skipping CorePackages and script
    // module subtrees (CoreGui.RobloxGui.Modules has thousands of ModuleScripts).
    g.set(
        "style_targets",
        lua.create_function(|lua, root: Value| {
            let Some(root_r) = opt_inst(&root) else {
                return lua.create_table();
            };
            let _scope = rustudio_core::bench::scope("input_style_targets");
            let found = {
                let w = world(lua)?;
                let d = w.d();
                let mut out = Vec::new();
                let mut stack = vec![root_r];
                while let Some(r) = stack.pop() {
                    let Some(inst) = d.dom.get_by_ref(r) else { continue };
                    let cls = inst.class.as_str();
                    if matches!(cls, "CorePackages" | "ModuleScript" | "LocalScript" | "Script" | "LocalizationTable") {
                        continue;
                    }
                    if rustudio_core::doc::is_a(cls, "GuiBase2d") || rustudio_core::doc::is_a(cls, "UIBase") {
                        out.push(r);
                    }
                    for &child in inst.children().iter().rev() {
                        stack.push(child);
                    }
                }
                out
            };
            let t = lua.create_table_with_capacity(found.len(), 0)?;
            for (i, r) in found.into_iter().enumerate() {
                t.raw_set(i + 1, inst_value(lua, r)?)?;
            }
            Ok(t)
        })?,
    )?;
    // style_targets plus each node's ClassName, Name and tag set, read in one
    // native pass (the restyle index otherwise makes three Lua→Rust calls a
    // node, which dominates restyling CoreGui).
    g.set(
        "style_index",
        lua.create_function(|lua, root: Value| {
            let Some(root_r) = opt_inst(&root) else {
                return Ok((lua.create_table()?, lua.create_table()?, lua.create_table()?, lua.create_table()?));
            };
            let _scope = rustudio_core::bench::scope("input_style_index");
            let found: Vec<(Ref, String, String, Vec<String>)> = {
                let w = world(lua)?;
                let d = w.d();
                let mut out = Vec::new();
                let mut stack = vec![root_r];
                while let Some(r) = stack.pop() {
                    let Some(inst) = d.dom.get_by_ref(r) else { continue };
                    let cls = inst.class.as_str();
                    if matches!(cls, "CorePackages" | "ModuleScript" | "LocalScript" | "Script" | "LocalizationTable") {
                        continue;
                    }
                    if rustudio_core::doc::is_a(cls, "GuiBase2d") || rustudio_core::doc::is_a(cls, "UIBase") {
                        let tags = match inst.properties.get(&rbx_dom_weak::ustr("Tags")) {
                            Some(Variant::Tags(t)) => t.iter().map(|s| s.to_string()).collect(),
                            Some(Variant::BinaryString(b)) => rbx_dom_weak::types::Tags::decode(b.as_ref())
                                .map(|t| t.iter().map(|s| s.to_string()).collect())
                                .unwrap_or_default(),
                            _ => Vec::new(),
                        };
                        out.push((r, cls.to_string(), inst.name.to_string(), tags));
                    }
                    for &child in inst.children().iter().rev() {
                        stack.push(child);
                    }
                }
                out
            };
            let nodes = lua.create_table_with_capacity(found.len(), 0)?;
            let classes = lua.create_table_with_capacity(found.len(), 0)?;
            let names = lua.create_table_with_capacity(found.len(), 0)?;
            let tag_sets = lua.create_table_with_capacity(found.len(), 0)?;
            for (i, (r, cls, name, tags)) in found.into_iter().enumerate() {
                nodes.raw_set(i + 1, inst_value(lua, r)?)?;
                classes.raw_set(i + 1, cls)?;
                names.raw_set(i + 1, name)?;
                let set = lua.create_table_with_capacity(0, tags.len())?;
                for t in tags {
                    set.raw_set(t, true)?;
                }
                tag_sets.raw_set(i + 1, set)?;
            }
            Ok((nodes, classes, names, tag_sets))
        })?,
    )?;
    // Style application depends on the linked roots and their sheets, not
    // on edits elsewhere in the DataModel. Include relatives in the old
    // tree to notice an entire styled subtree moving out.
    #[derive(Default)]
    struct StyleWatch {
        document: u64,
        roots: rustc_hash::FxHashSet<Ref>,
        members: rustc_hash::FxHashSet<Ref>,
        ancestors: rustc_hash::FxHashSet<Ref>,
    }
    let watch = std::rc::Rc::new(std::cell::RefCell::new(StyleWatch::default()));
    let reader = watch.clone();
    let read_roots = |roots: Vec<Value>| roots.iter().filter_map(opt_inst).collect::<rustc_hash::FxHashSet<_>>();
    g.set(
        "style_watch_changed",
        lua.create_function(move |lua, ()| {
            let mut w = world(lua)?;
            let d = w.dm();
            let watch = reader.borrow();
            if watch.document != d.id {
                return Ok(true);
            }
            let rustudio_core::doc::Marks::Some(marks) = d.take_style_marks() else {
                rustudio_core::bench::add("input_style_journal_miss", 0.0, 1);
                return Ok(true);
            };
            Ok(marks.iter().any(|&mark| match mark {
                rustudio_core::doc::Mark::Tree(r) => {
                    if watch.members.contains(&r) || watch.ancestors.contains(&r) {
                        return true;
                    }
                    // A new member can enter an old watched root. Walk its
                    // parent chain once instead of once per StyleLink/sheet.
                    let mut current = r;
                    while let Some(inst) = d.dom.get_by_ref(current) {
                        let parent = inst.parent();
                        if watch.roots.contains(&parent) {
                            return true;
                        }
                        if parent.is_none() {
                            break;
                        }
                        current = parent;
                    }
                    false
                }
                rustudio_core::doc::Mark::Prop(r, _) => {
                    watch.members.contains(&r) || watch.ancestors.contains(&r)
                }
            }))
        })?,
    )?;
    g.set(
        "style_watch_reset",
        lua.create_function(move |lua, roots: Vec<Value>| {
            let roots = read_roots(roots);
            let mut w = world(lua)?;
            let d = w.dm();
            let mut watch = watch.borrow_mut();
            let _scope = rustudio_core::bench::scope("input_style_watch_reset");
            watch.members.clear();
            watch.ancestors.clear();
            for &root in &roots {
                if d.dom.get_by_ref(root).is_none() { continue; }
                watch.members.extend(d.dom.descendants_of(root).map(|inst| inst.referent()));
                let mut ancestor = root;
                while let Some(parent) = d.dom.get_by_ref(ancestor).map(|inst| inst.parent()).filter(Ref::is_some) {
                    if !watch.ancestors.insert(parent) { break; }
                    ancestor = parent;
                }
            }
            watch.document = d.id;
            d.take_style_marks();
            watch.roots = roots;
            Ok(())
        })?,
    )?;
    // Cheap revision/state snapshot for skipping idle prune() and step_styles() frames.
    g.set(
        "style_state",
        lua.create_function(|lua, ()| {
            use std::hash::{Hash, Hasher};
            let (tree_rev, style_rev, vw, vh) = {
                let w = world(lua)?;
                let d = w.d();
                (d.tree_revision, d.style_revision, w.input.view_size[0], w.input.view_size[1])
            };
            let hover_sig = {
                let sh = crate::gui::share();
                let mut h = std::collections::hash_map::DefaultHasher::new();
                sh.pressed.hash(&mut h);
                sh.focus.hash(&mut h);
                sh.hovered.len().hash(&mut h);
                // Order-independent mix of hovered refs.
                let mut x = 0u64;
                for r in &sh.hovered {
                    let mut rh = std::collections::hash_map::DefaultHasher::new();
                    r.hash(&mut rh);
                    x ^= rh.finish();
                }
                x.hash(&mut h);
                (h.finish() & 0x001f_ffff_ffff_ffff) as i64
            };
            Ok((tree_rev as i64, style_rev as i64, vw as f64, vh as f64, hover_sig))
        })?,
    )?;
    g.set(
        "focus_events",
        lua.create_function(|lua, ()| {
            let evs = std::mem::take(&mut crate::gui::share().focus_events);
            let out = lua.create_table()?;
            for (r, gained, enter, by) in evs {
                let t = lua.create_table()?;
                t.set("box", inst_value(lua, r)?)?;
                t.set("gained", gained)?;
                t.set("enter", enter)?;
                if let Some((b, x, y)) = by {
                    t.set("button", b)?;
                    t.set("x", x)?;
                    t.set("y", y)?;
                }
                out.push(t)?;
            }
            Ok(out)
        })?,
    )?;
    // Mouse presses / releases since the last call: {button, down, gui}.
    g.set(
        "mouse_edges",
        lua.create_function(|lua, ()| {
            let evs = std::mem::take(&mut crate::gui::share().mouse_edges);
            let out = lua.create_table()?;
            for (b, down, gui) in evs {
                let t = lua.create_table()?;
                t.set("button", b)?;
                t.set("down", down)?;
                t.set("gui", gui)?;
                out.push(t)?;
            }
            Ok(out)
        })?,
    )?;
    // The mouse's ray into the world this frame (origin, unit direction).
    g.set(
        "mouse_ray",
        lua.create_function(|lua, ()| {
            let (o, d) = world(lua)?.input.mouse_ray;
            Ok((
                LVector3(o).into_value(),
                LVector3(d.normalize_or_zero()).into_value(),
            ))
        })?,
    )?;
    // ParticleEmitter2D:Emit(n) / Clear() (n = 0) for the GUI walk.
    g.set(
        "p2d",
        lua.create_function(|_, (inst, n): (Value, u32)| {
            if let Some(r) = opt_inst(&inst) {
                crate::gui::share().p2d.push((r, n));
            }
            Ok(())
        })?,
    )?;
    g.set(
        "drags",
        lua.create_function(|lua, ()| {
            let evs = std::mem::take(&mut crate::gui::share().drags);
            let out = lua.create_table()?;
            for (r, kind, x, y) in evs {
                let t = lua.create_table()?;
                t.set("obj", inst_value(lua, r)?)?;
                t.set("kind", kind)?;
                t.set("x", x)?;
                t.set("y", y)?;
                out.push(t)?;
            }
            Ok(out)
        })?,
    )?;
    g.set(
        "hover_state",
        lua.create_function(|_, inst: Value| {
            let Some(r) = opt_inst(&inst) else { return Ok((false, false)) };
            let sh = crate::gui::share();
            Ok((sh.hovered.contains(&r), sh.pressed == Some(r)))
        })?,
    )?;
    g.set(
        "image_loaded",
        lua.create_function(|_, id: String| Ok(!id.is_empty() && crate::gui::share().loaded.contains(&id)))?,
    )?;
    g.set(
        "buttons",
        lua.create_function(|_, ()| {
            let b = crate::gui::share().buttons;
            Ok((b[0], b[1], b[2]))
        })?,
    )?;
    g.set(
        "content_text",
        lua.create_function(|_, s: String| Ok(crate::gui::content_text(&s)))?,
    )?;
    g.set(
        "set_page",
        lua.create_function(|_, (lay, from, to, alpha, dir): (Value, Value, Value, f32, Option<i8>)| {
            let Some(l) = opt_inst(&lay) else { return Ok(()) };
            let mut sh = crate::gui::share();
            match opt_inst(&to) {
                Some(t) => {
                    sh.pages.insert(l, (opt_inst(&from).unwrap_or(t), t, alpha, dir.unwrap_or(0)));
                }
                None => {
                    sh.pages.remove(&l);
                }
            }
            sh.pages_rev = sh.pages_rev.wrapping_add(1);
            Ok(())
        })?,
    )?;
    g.set(
        "page_scrolls",
        lua.create_function(|lua, ()| {
            let evs = std::mem::take(&mut crate::gui::share().page_scrolls);
            let out = lua.create_table()?;
            for (lay, dir) in evs {
                let t = lua.create_table()?;
                t.set("layout", inst_value(lua, lay)?)?;
                t.set("dir", dir)?;
                out.push(t)?;
            }
            Ok(out)
        })?,
    )?;
    g.set(
        "camera",
        lua.create_function(|lua, ()| {
            // The client camera's position (BillboardGui.CurrentDistance).
            let w = world(lua)?;
            let d = w.d();
            let cam = d
                .workspace()
                .and_then(|ws| match d.prop(ws, "CurrentCamera") {
                    Some(Variant::Ref(c)) if d.exists(c) => Some(c),
                    _ => None,
                });
            Ok(match cam.and_then(|c| d.prop(c, "CFrame")) {
                Some(Variant::CFrame(cf)) => (cf.position.x as f64, cf.position.y as f64, cf.position.z as f64, true),
                _ => (0.0, 0.0, 0.0, false),
            })
        })?,
    )?;
    g.set(
        "make_object",
        lua.create_function(|_, (class, methods): (String, Table)| {
            methods.raw_set("__class", class.as_str())?;
            Ok(LObject { class, methods })
        })?,
    )?;
    // Studio's own proximity prompt UI (CoreScripts/ProximityPrompt.lua from
    // the Studio install), with its two CoreGui-only requires shimmed.
    g.set(
        "prompt_ui",
        lua.create_function(|lua, ()| {
            let Some(dir) = rustudio_core::default_scripts::studio_dir() else { return Ok(Value::Nil) };
            let path = dir.join("ExtraContent/scripts/CoreScripts/CoreScripts/ProximityPrompt.lua");
            let Ok(mut src) = std::fs::read_to_string(&path) else { return Ok(Value::Nil) };
            let shims: [(&str, &str); 6] = [
                ("local CoreGui = game:GetService(\"CoreGui\")\n", ""),
                ("local CorePackages = game:GetService(\"CorePackages\")\n", ""),
                ("local RobloxGui = CoreGui.RobloxGui\n", ""),
                (
                    "local CoreUtility = require(RobloxGui.Modules.CoreUtility)",
                    "local CoreUtility = { waitForChildOfClass = function(p, c) local x = p:FindFirstChildOfClass(c) while not x do p.ChildAdded:Wait() x = p:FindFirstChildOfClass(c) end return x end }",
                ),
                (
                    "local AppFonts = require(CorePackages.Workspace.Packages.Style).AppFonts",
                    "local AppFonts = { default = { getMedium = function() return Enum.Font.BuilderSansMedium end } }",
                ),
                ("UserInputService:GetPlatform() == Enum.Platform.OSX", "false"),
            ];
            for (from, to) in shims {
                if !src.contains(from) {
                    return Ok(Value::Nil);
                }
                src = src.replacen(from, to, 1);
            }
            let f = lua.load(src).set_name("=CoreGui.RobloxGui.CoreScripts/ProximityPrompt").set_environment(crate::globals_parity::new_env(lua, None)?).into_function()?;
            Ok(Value::Function(f))
        })?,
    )?;
    // Studio's legacy Dialog conversations (CoreScripts/MainBotChatScript2.lua
    // from the Studio install): its CoreGui-only pieces (RobloxGui, the
    // SetDialogInUse remote, three CoreGui modules, analytics, core actions,
    // SignalDialogChoiceSelected, the dialog collection) point at stand-ins
    // gui_input.luau passes in as `...`.
    g.set(
        "dialog_ui",
        lua.create_function(|lua, ()| {
            let Some(dir) = rustudio_core::default_scripts::studio_dir() else { return Ok(Value::Nil) };
            let path = dir.join("ExtraContent/scripts/CoreScripts/CoreScripts/MainBotChatScript2.lua");
            let Ok(mut src) = std::fs::read_to_string(&path) else { return Ok(Value::Nil) };
            let once: [(&str, &str); 14] = [
                ("local CoreGui = game:GetService(\"CoreGui\")", "local CoreGui = __D.coregui"),
                ("local CorePackages = game:GetService(\"CorePackages\")", ""),
                ("local RobloxGui = CoreGui.RobloxGui", "local RobloxGui = __D.robloxgui"),
                ("local RobloxReplicatedStorage = game:GetService('RobloxReplicatedStorage')", "local RobloxReplicatedStorage = __D.rrs"),
                ("local AnalyticsService = game:GetService(\"RbxAnalyticsService\")", "local AnalyticsService = __D.analytics"),
                ("require(RobloxGui.Modules.TenFootInterface)", "__D.tenfoot"),
                ("require(RobloxGui.Modules.Settings.Utility)", "__D.utility"),
                ("require(RobloxGui.Modules.GameTranslator)", "__D.translator"),
                ("currentConversationDialog:SignalDialogChoiceSelected(localPlayer, dialogChoice)", "__D.signal(currentConversationDialog, localPlayer, dialogChoice)"),
                ("game:GetService(\"Chat\"):ChatLocal(...)", "__D.chat_local(...)"),
                ("game:GetService(\"CollectionService\").ItemAdded", "__D.item_added"),
                ("game:GetService(\"CollectionService\").ItemRemoved", "__D.item_removed"),
                ("game:GetService(\"CollectionService\"):GetCollection(\"Dialog\")", "__D.dialogs()"),
                ("local getClosestDialogToPosition = guiService.GetClosestDialogToPosition", "local getClosestDialogToPosition = __D.closest"),
            ];
            for (from, to) in once {
                if !src.contains(from) {
                    return Ok(Value::Nil);
                }
                src = src.replacen(from, to, 1);
            }
            src = src
                .replace("RobloxLocked = true", "Archivable = true")
                .replace("BindCoreAction", "BindAction")
                .replace("UnbindCoreAction", "UnbindAction")
                .replace("SelectedCoreObject", "SelectedObject")
                .replace("game:GetEngineFeature(\"ClosestVisibleDialogAPI\")", "false");
            let src = format!("local __D = ...\n{src}");
            let f = lua.load(src).set_name("=CoreGui.RobloxGui.CoreScripts/MainBotChatScript2").set_environment(crate::globals_parity::new_env(lua, None)?).into_function()?;
            Ok(Value::Function(f))
        })?,
    )?;
    // TextGenerator: Studio's user-auth generateText call on a worker
    // thread (the login session); the Luau polls it.
    g.set(
        "textgen_start",
        lua.create_function(|_, (universe, body): (i64, String)| {
            let url = format!("https://apis.roblox.com/user/cloud/v2/universes/{universe}:generateText");
            Ok(super::avatar::web::start("POST", url, Some(body)))
        })?,
    )?;
    // A read-only GET through the login session (poll with textgen_poll).
    g.set(
        "web_get",
        lua.create_function(|_, url: String| {
            if !url.starts_with("https://avatar.roblox.com/") {
                return Err(rt_err("web_get: only the avatar API"));
            }
            Ok(super::avatar::web::start("GET", url, None))
        })?,
    )?;
    g.set(
        "textgen_poll",
        lua.create_function(|lua, id: u64| {
            Ok(match super::avatar::web::poll(id) {
                None => MultiValue::new(),
                Some(Ok((status, text))) => MultiValue::from_vec(vec![
                    Value::Integer(status as i64),
                    Value::String(lua.create_string(text)?),
                ]),
                Some(Err(e)) => MultiValue::from_vec(vec![Value::Integer(0), Value::String(lua.create_string(e)?)]),
            })
        })?,
    )?;
    g.set(
        "http_error",
        lua.create_function(|_, status: u16| Ok(super::avatar::web::http_error(status)))?,
    )?;
    // ViewportFrame:CaptureSnapshotAsync: queue on the renderer, poll from Luau.
    g.set(
        "vf_snap_start",
        lua.create_function(|_, vf: LInst| Ok(crate::gui::vf_snap_request(vf.0)))?,
    )?;
    g.set(
        "vf_snap_poll",
        lua.create_function(|lua, id: u64| match crate::gui::vf_snap_poll(id) {
            None => Ok(Value::Nil),
            Some(uri) => Ok(Value::String(lua.create_string(uri)?)),
        })?,
    )?;
    // The inspect menu: set(view table or nil) / closed() (taken once).
    g.set(
        "inspect_set",
        lua.create_function(|_, v: Option<Table>| {
            let mut sh = crate::gui::share();
            sh.inspect = match v {
                None => None,
                Some(t) => {
                    let mut items = Vec::new();
                    if let Ok(list) = t.get::<Table>("items") {
                        for it in list.sequence_values::<Table>() {
                            let it = it?;
                            items.push((it.get::<String>("name").unwrap_or_default(), it.get::<Option<i64>>("price").unwrap_or(None)));
                        }
                    }
                    let scroll = sh.inspect.as_ref().map(|i| i.scroll).unwrap_or(0.0);
                    Some(crate::gui::InspectView {
                        title: t.get("title").unwrap_or_default(),
                        robux: t.get("robux").unwrap_or(0),
                        items,
                        loading: t.get("loading").unwrap_or(false),
                        scroll,
                    })
                }
            };
            Ok(())
        })?,
    )?;
    g.set(
        "inspect_closed",
        lua.create_function(|_, ()| Ok(std::mem::take(&mut crate::gui::share().inspect_closed)))?,
    )?;
    g.set("new_input_object", lua.create_function(|lua, t: Table| input_object(lua, t))?)?;
    // send(inst, kind, ...): an engine message to the server's copy of inst
    // (the client's prompt input; see `engine_message`).
    g.set(
        "send",
        lua.create_function(|lua, args: MultiValue| {
            let mut it = args.into_iter();
            let r = it.next().as_ref().and_then(opt_inst).ok_or_else(|| rt_err("send: Instance expected"))?;
            let mut xs = vec![Xfer::Str(ENGINE_MSG.to_vec())];
            xs.extend(it.map(|v| export(&v, 0)));
            let mut w = world(lua)?;
            if w.side == Side::Client {
                w.outbox.push(RemoteMsg { inst: r, kind: RemoteKind::Event, args: xs });
            }
            Ok(())
        })?,
    )?;
    let m: Table = lua
        .load(include_str!("gui_input.luau"))
        .set_name("=rustudio.gui_input")
        .call(g)?;
    m.set("defaults", lua.create_table()?)?;
    lua.set_named_registry_value("rs_gui_input", m)?;
    Ok(())
}

/// First argument marking a client → server message as the engine's own
/// (not a RemoteEvent's).
const ENGINE_MSG: &[u8] = b"\x01rustudio.gui_input";

/// Whether a client → server message is one of this domain's (play.rs
/// hands those to `engine_message` instead of OnServerEvent).
pub fn is_engine_msg(args: &[Xfer]) -> bool {
    matches!(args.first(), Some(Xfer::Str(s)) if s.as_slice() == ENGINE_MSG)
}

/// The server's side of an engine message: the prompt events the client's
/// input caused, fired on the server's prompt (gui_input.luau remote_in).
pub fn engine_message(rt: &Runtime, inst: Ref, player: Ref, args: &[Xfer], map: &dyn Fn(Ref) -> Option<Ref>) {
    let lua = &rt.lua;
    let run = || -> mlua::Result<()> {
        let f: Function = module(lua)?.get("remote_in")?;
        let mut vals = vec![inst_value(lua, inst)?, inst_value(lua, player)?];
        for a in args.iter().skip(1) {
            vals.push(import(lua, a, map)?);
        }
        f.call::<()>(MultiValue::from_vec(vals))
    };
    if let Err(e) = run() {
        if let Ok(mut w) = world(lua) {
            w.output.push((rustudio_core::log::Level::Error, format!("gui_input: {}", error_message(&e))));
        }
    }
    rt.dispatch();
}

/// Fires the focus changes queued so far (FocusLost of a box a press left)
/// before the GUI events of the same frame are fired.
pub fn focus_now(lua: &Lua) {
    if crate::gui::share().focus_events.is_empty() {
        return;
    }
    lua.remove_app_data::<EditStepCache>();
    if let Ok(f) = module(lua).and_then(|m| m.get::<Function>("focus_now")) {
        let _ = f.call::<()>(());
    }
}

/// Applies one TextBox edit/navigation commit from the UI thread in Roblox's exact
/// signal order: commit all new values first, then fire CursorPosition -> SelectionStart -> Text.
pub fn apply_textbox_edit(
    lua: &Lua,
    r: Ref,
    text: Option<String>,
    cursor: Option<i32>,
    selection: Option<i32>,
) {
    lua.remove_app_data::<EditStepCache>();
    let Ok(inst) = inst_value(lua, r) else { return };
    let Ok(f) = module(lua).and_then(|m| m.get::<Function>("apply_textbox_edit")) else { return };
    let _ = f.call::<()>((inst, text, cursor, selection));
}

/// Checks whether a GuiObject's `GuiState` transitioned (hover / press / focus)
/// and fires `Changed` / `GetPropertyChangedSignal("GuiState")` if so.
pub fn check_gui_state(lua: &Lua, r: Ref) {
    let Ok(inst) = inst_value(lua, r) else { return };
    let Ok(f) = module(lua).and_then(|m| m.get::<Function>("check_gui_state")) else { return };
    let _ = f.call::<()>(inst);
}

#[derive(Default)]
struct EditStepCache {
    primed: bool,
    busy: bool,
    doc_id: u64,
    tree_rev: u64,
    style_rev: u64,
    view_size: [u32; 2],
    mouse_xy: [u32; 2],
    hover_sig: i64,
}

fn compute_hover_sig() -> (i64, bool) {
    use std::hash::{Hash, Hasher};
    let sh = crate::gui::share();
    let has_events = !sh.focus_events.is_empty() || !sh.mouse_edges.is_empty();
    let mut h = std::collections::hash_map::DefaultHasher::new();
    sh.pressed.hash(&mut h);
    sh.focus.hash(&mut h);
    sh.hovered.len().hash(&mut h);
    let mut x = 0u64;
    for r in &sh.hovered {
        let mut rh = std::collections::hash_map::DefaultHasher::new();
        r.hash(&mut rh);
        x ^= rh.finish();
    }
    x.hash(&mut h);
    ((h.finish() & 0x001f_ffff_ffff_ffff) as i64, has_events)
}

pub fn step(lua: &Lua, dt: f64) -> mlua::Result<()> {
    let (side, edit_key) = match world(lua) {
        Ok(w) => {
            let key = if w.mode == RunMode::Edit {
                let d = w.d();
                Some((
                    d.id,
                    d.tree_revision,
                    d.style_revision,
                    [w.input.view_size[0].to_bits(), w.input.view_size[1].to_bits()],
                    [w.input.mouse_xy[0].to_bits(), w.input.mouse_xy[1].to_bits()],
                ))
            } else {
                None
            };
            (Some(w.side), key)
        }
        Err(_) => (None, None),
    };
    if let Some((doc_id, tree_rev, style_rev, view_size, mouse_xy)) = edit_key {
        let (hover_sig, has_events) = compute_hover_sig();
        let skip = !has_events
            && lua.app_data_ref::<EditStepCache>().is_some_and(|c| {
                c.primed
                    && !c.busy
                    && c.doc_id == doc_id
                    && c.tree_rev == tree_rev
                    && c.style_rev == style_rev
                    && c.view_size == view_size
                    && c.mouse_xy == mouse_xy
                    && c.hover_sig == hover_sig
            });
        if skip {
            return Ok(());
        }
        let _scope = rustudio_core::bench::scope(if side == Some(Side::Server) {
            "input_step_server"
        } else {
            "input_step_client"
        });
        let Ok(m) = module(lua) else { return Ok(()) };
        let f: Function = m.get("step")?;
        let busy = match f.call::<bool>(dt) {
            Ok(b) => b,
            Err(e) => {
                if let Ok(mut w) = world(lua) {
                    w.output.push((rustudio_core::log::Level::Error, format!("gui_input step: {}", error_message(&e))));
                }
                true
            }
        };
        let (hover_after, _) = compute_hover_sig();
        let (tree_after, style_after) = world(lua)
            .map(|w| (w.d().tree_revision, w.d().style_revision))
            .unwrap_or((tree_rev, style_rev));
        lua.set_app_data(EditStepCache {
            primed: true,
            busy,
            doc_id,
            tree_rev: tree_after,
            style_rev: style_after,
            view_size,
            mouse_xy,
            hover_sig: hover_after,
        });
        return Ok(());
    }
    let _scope = rustudio_core::bench::scope(if side == Some(Side::Server) {
        "input_step_server"
    } else {
        "input_step_client"
    });
    let Ok(m) = module(lua) else { return Ok(()) };
    let f: Function = m.get("step")?;
    if let Err(e) = f.call::<Value>(dt) {
        if let Ok(mut w) = world(lua) {
            w.output.push((rustudio_core::log::Level::Error, format!("gui_input step: {}", error_message(&e))));
        }
    }
    Ok(())
}
