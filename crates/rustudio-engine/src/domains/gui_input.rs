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
    // GuiService members the CoreScripts call (RobloxScriptSecurity)
    ("SetTopbarInset", "GuiService"),
    ("SetGlobalGuiInset", "GuiService"),
    ("SetMenuIsOpen", "GuiService"),
    ("GetResolutionScale", "GuiService"),
    ("GetRawScreenScale", "GuiService"),
    ("GetAutoUIScaleHundredths", "GuiService"),
    ("GetEffectiveUIScaleHundredths", "GuiService"),
    ("SetUIScaleMultiplier", "GuiService"),
    ("GetSafeZoneOffsets", "GuiService"),
    ("GetGuiIsVisible", "GuiService"),
    ("GetErrorMessage", "GuiService"),
    ("GetUiMessage", "GuiService"),
    ("GetErrorCode", "GuiService"),
    ("GetErrorType", "GuiService"),
    ("GetErrorDetails", "GuiService"),
    ("ClearError", "GuiService"),
    ("BroadcastNotification", "GuiService"),
    ("SetPurchasePromptIsShown", "GuiService"),
    ("AddCenterDialog", "GuiService"),
    ("RemoveCenterDialog", "GuiService"),
    ("GetClosestDialogToPosition", "GuiService"),
    ("GetClosestVisibleDialogToPosition", "GuiService"),
    ("AddKey", "GuiService"),
    ("RemoveKey", "GuiService"),
    ("AddSpecialKey", "GuiService"),
    ("RemoveSpecialKey", "GuiService"),
    ("GetHardwareSafeViewport", "GuiService"),
    ("CloseStatsBasedOnInputString", "GuiService"),
    ("OpenBrowserWindow", "GuiService"),
    ("OpenNativeOverlay", "GuiService"),
    ("ToggleFullscreen", "GuiService"),
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
    // ProximityPrompt / Dialog / TextGenerator
    ("InputHoldBegin", "ProximityPrompt"),
    ("InputHoldEnd", "ProximityPrompt"),
    ("GetCurrentPlayers", "Dialog"),
];

/// Yielding methods, written in gui_input.luau (put in `__rs_lua_methods`).
pub const LUA_METHODS: &[(&str, &str)] = &[("GenerateTextAsync", "TextGenerator")];

/// Events this domain fires (the rest of its classes' events were already
/// known to runtime_ext.rs; gui_input.luau fires many of those too).
pub const EVENTS: &[(&str, &str)] = &[
    ("DeviceAccelerationChanged", "UserInputService"),
    ("DeviceGravityChanged", "UserInputService"),
    ("TouchDrag", "UserInputService"),
    ("UserCFrameChanged", "UserInputService"),
    ("LocalToolEquipped", "ContextActionService"),
    ("LocalToolUnequipped", "ContextActionService"),
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
    ("GuiService", "PreferredTextSize"),
    ("GuiService", "PreferredTransparency"),
    ("GuiService", "ReducedMotionEnabled"),
    ("GuiService", "TopbarInset"),
    ("GuiService", "ViewportDisplaySize"),
    ("UserInputService", "OnScreenKeyboardPosition"),
    ("UserInputService", "OnScreenKeyboardSize"),
    ("UserInputService", "UserHeadCFrame"),
    ("GuiObject", "GuiState"),
    ("GuiObject", "InputSink"),
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
    ("GuiService", "CoreGuiNavigationEnabled"),
    ("GuiObject", "InputSink"),
    ("VRService", "GuiInputUserCFrame"),
    ("Dialog", "InUse"),
    ("InputAction", "Type"),
    ("InputAction", "Enabled"),
    ("InputContext", "Enabled"),
    ("TextLabel", "Font"),
    ("TextButton", "Font"),
    ("TextBox", "Font"),
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
        return Ok(true);
    }
    if let Some(base) = lua_write(class, key) {
        let set: Table = module(lua)?.get("set")?;
        let f: Function = set.get(format!("{base}.{key}"))?;
        // true: the Luau stored it itself; false: store it as usual too.
        let done: bool = f.call((inst_value(lua, r)?, key, value.clone()))?;
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
        x >= px && y >= py && x < px + sx && y < py + sy
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
            !clips || (st.inside)(w, p)
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
    // bench_phase(i, ms): one phase of step_all (gui_input.luau STEP_PHASES)
    // as bench section `gi_<phase>`, inside input_step_client.
    g.set(
        "bench_phase",
        lua.create_function(|_, (i, ms): (usize, f64)| {
            const NAMES: [&str; 12] = [
                "gi_prune", "gi_fx", "gi_inspect", "gi_tracking", "gi_focus", "gi_tools", "gi_pages", "gi_drags",
                "gi_adorns", "gi_mouse", "gi_prompts", "gi_styles",
            ];
            if let Some(name) = NAMES.get(i) {
                rustudio_core::bench::add(name, ms, 1);
            }
            Ok(())
        })?,
    )?;
    // drop_gone(t, …): removes from each table the entries keyed by an
    // Instance that no longer exists (gui_input.luau prune), without a Lua
    // call and pcall closure per key.
    g.set(
        "drop_gone",
        lua.create_function(|lua, tables: mlua::Variadic<Table>| {
            let mut dead = Vec::new();
            for t in tables.iter() {
                {
                    let w = world(lua)?;
                    let d = w.d();
                    for pair in t.pairs::<Value, Value>() {
                        let (k, _) = pair?;
                        if opt_inst(&k).is_some_and(|r| d.dom.get_by_ref(r).is_none()) {
                            dead.push(k);
                        }
                    }
                }
                for k in dead.drain(..) {
                    t.raw_set(k, Value::Nil)?;
                }
            }
            Ok(())
        })?,
    )?;
    // workspace_dialogs(): the Workspace's Dialogs, for GuiService:
    // GetClosestDialogToPosition (MainBotChatScript2 asks every Heartbeat),
    // kept from the document's journal (DialogIndex): GetDescendants and an
    // IsA per instance in Lua cost milliseconds a frame in a large Workspace.
    g.set(
        "workspace_dialogs",
        lua.create_function(|lua, ()| {
            let found = workspace_dialogs(lua)?;
            let t = lua.create_table_with_capacity(found.len(), 0)?;
            for r in found {
                t.raw_push(inst_value(lua, r)?)?;
            }
            Ok(t)
        })?,
    )?;
    // local_player(): Players.LocalPlayer, read here (step_tools asks every
    // frame; svc("Players").LocalPlayer under pcall was about 1 ms of each
    // frame). The Players kept in the tree, not a destroyed copy GetService
    // can hand back.
    g.set(
        "local_player",
        lua.create_function(|lua, ()| {
            let lp = {
                let w = world(lua)?;
                let d = w.d();
                d.dom.root().children().iter().copied().find_map(|p| {
                    if !d.dom.get_by_ref(p).is_some_and(|i| i.class == "Players") {
                        return None;
                    }
                    match d.prop(p, "LocalPlayer") {
                        Some(Variant::Ref(lp)) if d.exists(lp) => Some(lp),
                        _ => None,
                    }
                })
            };
            match lp {
                Some(r) => inst_value(lua, r),
                None => Ok(Value::Nil),
            }
        })?,
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
                    let c = b.canvas.unwrap_or([-1.0, -1.0]);
                    let t = b.text.unwrap_or([-1.0, -1.0]);
                    MultiValue::from_iter(
                        [b.pos[0], b.pos[1], b.size[0], b.size[1], c[0], c[1], t[0], t[1]]
                            .map(|x| Value::Number(x as f64)),
                    )
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
    // The hovered / pressed objects, as a key (gui_input.luau step_styles
    // reruns matching when it changes and a rule has a :state).
    g.set(
        "style_key",
        lua.create_function(|_, ()| {
            let sh = crate::gui::share();
            let mut hover: u64 = 0;
            for r in sh.hovered.iter() {
                use std::hash::{Hash, Hasher};
                let mut h = std::collections::hash_map::DefaultHasher::new();
                r.hash(&mut h);
                hover ^= h.finish();
            }
            if let Some(p) = sh.pressed {
                use std::hash::{Hash, Hasher};
                let mut h = std::collections::hash_map::DefaultHasher::new();
                p.hash(&mut h);
                hover ^= h.finish().rotate_left(1);
            }
            Ok(format!("{hover}"))
        })?,
    )?;
    // What a style sheet can style under `root` for StyleLink `link`, read
    // in one call: root and its descendants but scripts and what they hold
    // (RobloxGui.Modules' 45 000 CoreScript modules), as parallel lists of
    // the instance, its parent's index (0: the root's), ClassName, Name and
    // tags (a set, nil without tags). step_styles matched rules by reading
    // these properties through the engine per rule and element: 150-250 ms
    // a rerun with the settings menu open (debug build). The instances are
    // kept as the link's scope for `style_changes`.
    g.set(
        "style_tree",
        lua.create_function(|lua, (root, link): (Value, Value)| {
            let Some(link) = opt_inst(&link) else { return Ok(Value::Nil) };
            let Some(root) = opt_inst(&root) else { return Ok(Value::Nil) };
            let mut nodes: Vec<(Ref, usize, String, String, Vec<String>)> = Vec::new();
            {
                let w = world(lua)?;
                let d = w.d();
                if d.dom.get_by_ref(root).is_none() {
                    return Ok(Value::Nil);
                }
                let mut stack = vec![(root, 0usize)];
                while let Some((r, parent)) = stack.pop() {
                    let Some(i) = d.dom.get_by_ref(r) else { continue };
                    let tags = match i.properties.get(&ustr("Tags")) {
                        Some(Variant::Tags(t)) => t.iter().map(|s| s.to_string()).collect(),
                        Some(Variant::BinaryString(b)) => rbx_dom_weak::types::Tags::decode(b.as_ref())
                            .map(|t| t.iter().map(|s| s.to_string()).collect())
                            .unwrap_or_default(),
                        _ => Vec::new(),
                    };
                    nodes.push((r, parent, i.class.to_string(), i.name.to_string(), tags));
                    let index = nodes.len();
                    for &c in i.children() {
                        let script = d.dom.get_by_ref(c)
                            .is_some_and(|ci| rustudio_core::doc::is_a(&ci.class, "LuaSourceContainer"));
                        if !script {
                            stack.push((c, index));
                        }
                    }
                }
            }
            style_scope(lua).set_link(link, nodes.iter().map(|n| n.0).collect());
            let n = nodes.len();
            let (inst, parent, class, name, tags) = (
                lua.create_table_with_capacity(n, 0)?,
                lua.create_table_with_capacity(n, 0)?,
                lua.create_table_with_capacity(n, 0)?,
                lua.create_table_with_capacity(n, 0)?,
                lua.create_table()?,
            );
            for (k, (r, p, c, nm, tg)) in nodes.into_iter().enumerate() {
                let k = k + 1;
                inst.raw_set(k, inst_value(lua, r)?)?;
                parent.raw_set(k, p)?;
                class.raw_set(k, c)?;
                name.raw_set(k, nm)?;
                if !tg.is_empty() {
                    let set = lua.create_table_with_capacity(0, tg.len())?;
                    for t in tg {
                        set.raw_set(t, true)?;
                    }
                    tags.raw_set(k, set)?;
                }
            }
            let out = lua.create_table_with_capacity(0, 7)?;
            out.raw_set("n", n)?;
            out.raw_set("inst", inst)?;
            out.raw_set("parent", parent)?;
            out.raw_set("class", class)?;
            out.raw_set("name", name)?;
            out.raw_set("tags", tags)?;
            out.raw_set("outside", lua.create_table()?)?;
            Ok(Value::Table(out))
        })?,
    )?;
    // The style sheets matching read (each one's rules and derived sheets),
    // for `style_changes`: replaces the last set.
    g.set(
        "style_sheets",
        lua.create_function(|lua, sheets: Vec<Value>| {
            let mut set = rustc_hash::FxHashSet::default();
            {
                let w = world(lua)?;
                let d = w.d();
                let mut stack: Vec<Ref> = sheets.iter().filter_map(opt_inst).collect();
                while let Some(r) = stack.pop() {
                    if set.insert(r) {
                        if let Some(i) = d.dom.get_by_ref(r) {
                            stack.extend_from_slice(i.children());
                        }
                    }
                }
            }
            style_scope(lua).sheets = set;
            Ok(())
        })?,
    )?;
    g.set(
        "style_forget",
        lua.create_function(|lua, link: Value| {
            if let Some(link) = opt_inst(&link) {
                style_scope(lua).set_link(link, Vec::new());
            }
            Ok(())
        })?,
    )?;
    // What changed for style matching since the last call, from the
    // DataModel's journal: (true) everything (a sheet, or the journal was
    // lost), else a set of the StyleLinks whose styled tree changed (an
    // instance added, moved or removed there, or a Name / Tags /
    // Interactable written), or nil. Changes elsewhere (a game's parts,
    // properties GUI animations write) rerun nothing.
    g.set(
        "style_changes",
        lua.create_function(|lua, ()| {
            let mut w = world(lua)?;
            let marks = match w.dm().marks_for(rustudio_core::doc::Reader::Styles) {
                rustudio_core::doc::Marks::All => return Ok((true, Value::Nil)),
                rustudio_core::doc::Marks::Some(m) => m,
            };
            if marks.is_empty() {
                return Ok((false, Value::Nil));
            }
            let scope = style_scope(lua);
            let d = w.d();
            let names = [ustr("Name"), ustr("Tags"), ustr("Interactable")];
            let sheet_names = [ustr("Selector"), ustr("Priority"), ustr("StyleSheet"), ustr("Attributes")];
            let mut dirty: rustc_hash::FxHashSet<Ref> = rustc_hash::FxHashSet::default();
            for m in &marks {
                match *m {
                    rustudio_core::doc::Mark::Prop(r, name) => {
                        if scope.sheets.contains(&r) {
                            if sheet_names.contains(&name) {
                                return Ok((true, Value::Nil));
                            }
                        } else if names.contains(&name) || name == sheet_names[2] {
                            // (a StyleLink's own StyleSheet: its tree's link)
                            if let Some(links) = scope.nodes.get(&r) {
                                dirty.extend(links.iter().copied());
                            }
                        }
                    }
                    rustudio_core::doc::Mark::Tree(r) => {
                        // (itself: moved or removed from a styled tree; its
                        // first ancestor in one: added there)
                        if scope.sheets.contains(&r) {
                            return Ok((true, Value::Nil));
                        }
                        if let Some(links) = scope.nodes.get(&r) {
                            dirty.extend(links.iter().copied());
                        }
                        let mut p = d.dom.get_by_ref(r).map(|i| i.parent());
                        while let Some(a) = p.filter(|a| a.is_some()) {
                            if scope.sheets.contains(&a) {
                                return Ok((true, Value::Nil));
                            }
                            if let Some(links) = scope.nodes.get(&a) {
                                dirty.extend(links.iter().copied());
                                break;
                            }
                            p = d.dom.get_by_ref(a).map(|i| i.parent());
                        }
                    }
                }
            }
            drop(scope);
            drop(w);
            if dirty.is_empty() {
                return Ok((false, Value::Nil));
            }
            let out = lua.create_table_with_capacity(0, dirty.len())?;
            for r in dirty {
                out.raw_set(inst_value(lua, r)?, true)?;
            }
            Ok((false, Value::Table(out)))
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
        lua.create_function(|_, (lay, from, to, alpha): (Value, Value, Value, f32)| {
            let Some(l) = opt_inst(&lay) else { return Ok(()) };
            let mut sh = crate::gui::share();
            match opt_inst(&to) {
                Some(t) => {
                    sh.pages.insert(l, (opt_inst(&from).unwrap_or(t), t, alpha));
                }
                None => {
                    sh.pages.remove(&l);
                }
            }
            Ok(())
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
    if let Ok(f) = module(lua).and_then(|m| m.get::<Function>("focus_now")) {
        let _ = f.call::<()>(());
    }
}

/// What style matching last read (gui_input.luau step_styles), for
/// `style_changes` to tell which StyleLinks a journal mark touches.
#[derive(Default)]
struct StyleScope {
    /// Instance → the StyleLinks whose styled tree holds it.
    nodes: rustc_hash::FxHashMap<Ref, Vec<Ref>>,
    /// StyleLink → the instances `style_tree` listed for it.
    by_link: rustc_hash::FxHashMap<Ref, Vec<Ref>>,
    /// The sheets' instances (rules, derives, the sheets).
    sheets: rustc_hash::FxHashSet<Ref>,
}

impl StyleScope {
    fn set_link(&mut self, link: Ref, nodes: Vec<Ref>) {
        for r in self.by_link.remove(&link).unwrap_or_default() {
            if let Some(links) = self.nodes.get_mut(&r) {
                links.retain(|l| *l != link);
                if links.is_empty() {
                    self.nodes.remove(&r);
                }
            }
        }
        for &r in &nodes {
            self.nodes.entry(r).or_default().push(link);
        }
        if !nodes.is_empty() {
            self.by_link.insert(link, nodes);
        }
    }
}

fn style_scope(lua: &Lua) -> mlua::AppDataRefMut<'_, StyleScope> {
    if lua.app_data_ref::<StyleScope>().is_none() {
        lua.set_app_data(StyleScope::default());
    }
    lua.app_data_mut::<StyleScope>().expect("style scope")
}

/// The Workspace's Dialogs (`workspace_dialogs`), kept from the document's
/// journal: a Dialog added anywhere is picked up from its Tree mark, one
/// that left the Workspace or stopped existing is dropped when read.
#[derive(Default)]
struct DialogIndex {
    doc_id: u64,
    list: Vec<Ref>,
}

fn workspace_dialogs(lua: &Lua) -> mlua::Result<Vec<Ref>> {
    use rustudio_core::doc::{Mark, Marks, Reader};
    if lua.app_data_ref::<DialogIndex>().is_none() {
        lua.set_app_data(DialogIndex::default());
    }
    let mut w = world(lua)?;
    let marks = w.dm().marks_for(Reader::Dialogs);
    let d = w.d();
    let mut ix = lua.app_data_mut::<DialogIndex>().ok_or_else(|| rt_err("dialog index missing"))?;
    let ws = d.workspace();
    let is_dialog = |r: Ref| d.dom.get_by_ref(r).is_some_and(|i| i.class == "Dialog");
    match marks {
        Marks::Some(m) if ix.doc_id == d.id => {
            for mark in m {
                let Mark::Tree(t) = mark else { continue };
                if !d.exists(t) {
                    continue;
                }
                for r in d.subtree(t) {
                    if is_dialog(r) && !ix.list.contains(&r) {
                        ix.list.push(r);
                    }
                }
            }
        }
        _ => {
            ix.doc_id = d.id;
            ix.list = match ws {
                Some(ws) => d.dom.descendants_of(ws).filter(|i| i.class == "Dialog").map(|i| i.referent()).collect(),
                None => Vec::new(),
            };
        }
    }
    ix.list.retain(|r| is_dialog(*r) && ws.is_some_and(|ws| d.is_ancestor(ws, *r)));
    Ok(ix.list.clone())
}

pub fn step(lua: &Lua, dt: f64) -> mlua::Result<()> {
    let side = world(lua).map(|w| w.side).ok();
    let _scope = rustudio_core::bench::scope(if side == Some(Side::Server) {
        "input_step_server"
    } else {
        "input_step_client"
    });
    let Ok(m) = module(lua) else { return Ok(()) };
    let f: Function = m.get("step")?;
    if let Err(e) = f.call::<()>(dt) {
        if let Ok(mut w) = world(lua) {
            w.output.push((rustudio_core::log::Level::Error, format!("gui_input step: {}", error_message(&e))));
        }
    }
    Ok(())
}
