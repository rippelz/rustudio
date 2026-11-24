//! The open document: a Roblox DOM plus undo/redo, selection and the edit
//! operations every panel and tool goes through.

use std::io::Cursor;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use glam::Vec3;
use rbx_dom_weak::types::{CFrame, Color3uint8, Enum, Ref, Variant, Vector3};
use rbx_dom_weak::{ustr, InstanceBuilder, Ustr, WeakDom};
use rbx_reflection::ClassTag;

use crate::mech;
use crate::math::Xform;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DocKind {
    /// `.rbxl` / `.rbxlx`: the root is the DataModel.
    Place,
    /// `.rbxm` / `.rbxmx`: the root's children are the model's top-level items.
    Model,
}

#[derive(Debug, Clone)]
pub enum Change {
    Prop {
        r: Ref,
        name: Ustr,
        old: Option<Variant>,
        new: Option<Variant>,
    },
    Name {
        r: Ref,
        old: String,
        new: String,
    },
    Parent {
        r: Ref,
        old: Ref,
        new: Ref,
    },
    /// Forward: the instance lives in the DOM under `parent`.
    Create {
        r: Ref,
        parent: Ref,
    },
    /// Forward: the instance lives in limbo.
    Remove {
        r: Ref,
        parent: Ref,
    },
    /// A script moved an instance from outside the DataModel (`old`, none
    /// for a parentless one) into it (doc_history.rs): undo takes it out
    /// again, keeping it alive for the script.
    Enter {
        r: Ref,
        old: Ref,
        new: Ref,
    },
    /// A script destroyed the instance: the step has it, but nothing can
    /// bring it back (Studio's undo leaves Parent nil and locked).
    Destroy {
        r: Ref,
    },
}

/// One entry in a document's change journal.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Mark {
    /// A property (or the Name) of an instance changed.
    Prop(Ref, Ustr),
    /// An instance was created, reparented, removed or destroyed: its whole
    /// subtree must be re-checked.
    Tree(Ref),
}

/// Who reads a journal (each keeps its own position).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Reader {
    Scene = 0,
    Physics = 1,
    /// Script world queries (raycasts, region queries).
    Query = 2,
    /// A project folder's file write-back (src/project.rs).
    Project = 3,
    /// The client's own joints (crates/rustudio-physics/src/client_joints.rs).
    ClientJoints = 4,
    /// The SurfaceGuis / BillboardGuis under Workspace (crates/rustudio-engine/src/gui_input.rs).
    Gui = 5,
    /// FindFirstChild's name index (crates/rustudio-engine/src/runtime.rs).
    Children = 6,
    /// The lights under Workspace (crates/rustudio-render/src/lighting.rs).
    Lights = 7,
    /// The client DataModel's anchored parts its LocalScripts move
    /// (crates/rustudio-physics/src/physics.rs follow_client).
    ClientPhysics = 8,
    /// ParticleEmitters, Beams and Trails under Workspace (crates/rustudio-render/src/effects.rs).
    Effects = 9,
    /// Outgoing Team Create edits.
    TeamCreate = 10,
    /// CollectionService's tag index (crates/rustudio-engine/src/runtime_ext.rs GetTagged).
    Tags = 11,
    /// Sounds, audio / video instances and Wires (crates/rustudio-engine/src/domains/media.rs).
    Media = 12,
    /// Motors and IKControls (crates/rustudio-engine/src/domains/anim.rs).
    Anim = 13,
    /// GUI boxes scripts listen to (runtime_ext::fire_layout_changes).
    Layout = 14,
    /// The studio solver's connector descriptions (physics_joints.rs studio_constraints).
    SolverJoints = 15,
    /// Parts inside drawn ViewportFrames (scene.rs `Scene::viewport_frames`).
    VfScene = 16,
    /// The edit-mode plugin runtime's view of edits made outside it
    /// (src/plugin_host/edit_sync.rs).
    Plugins = 17,
    /// The plugins' GUIs: dock widgets and CoreGui in the edit viewport
    /// (src/plugin_host/widgets_ui.rs).
    PluginGui = 18,
    /// The Highlights the editor's 3D view draws (src/viewport.rs).
    Highlights = 19,
    /// The Humanoids Play drives besides the player's (src/play_npc.rs).
    Npcs = 20,
    /// The Play worker's presentation document (rustudio-render/src/snapshot.rs).
    RenderSnapshot = 21,
    /// Animator bindings to joints and bones in a rig.
    AnimatorBindings = 22,
    /// Server-side anchored / AnimationController rigs posed from Motor6D.Transform.
    ServerJoints = 23,
    /// Edit-mode Sound preview playback and on-demand Sound loading
    /// (crates/rustudio-player/src/edit_audio.rs).
    EditAudio = 24,
}

const JOURNAL_READERS: usize = Reader::EditAudio as usize + 1;

/// What a reader gets back: everything changed (rebuild), or the new marks.
pub enum Marks {
    All,
    Some(Vec<Mark>),
}

/// Change journal: lets the scene and physics update only what changed.
struct Journal {
    /// Sequence number of `marks[0]`.
    base: u64,
    marks: Vec<Mark>,
    /// Spatial queries can be idle through many unrelated UI or script
    /// changes. Retain only marks the query cache consumes until its next
    /// read. None means its next read must rebuild.
    query: Option<Vec<Mark>>,
    /// Selector inputs can outlive the general journal between GUI steps.
    styles: Option<Vec<Mark>>,
    /// Next sequence number each reader wants; None = hasn't read yet.
    read: [Option<u64>; JOURNAL_READERS],
    /// The joint graph's position (crates/rustudio-engine/src/domains/world_part.rs), read through
    /// `&self`: 0 = not reading, else the next sequence number + 1.
    graph_pos: std::sync::atomic::AtomicU64,
}

static NEXT_DOC_ID: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);

#[derive(Debug, Clone)]
pub struct Command {
    pub label: String,
    pub changes: Vec<Change>,
    merge_key: Option<String>,
    at: Instant,
    /// Selection when the command was made; undo restores it.
    sel_before: Vec<Ref>,
    /// Selection right before it was undone; redo restores it.
    sel_after: Vec<Ref>,
    /// A ChangeHistoryService recording's display name (the editor shows it).
    display: Option<String>,
}

pub struct Document {
    pub dom: WeakDom,
    /// Removed instances wait here so undo can bring them back with the same refs.
    limbo: WeakDom,
    pub kind: DocKind,
    pub path: Option<PathBuf>,
    pub title: String,
    /// The place on Roblox this document is (opened from Roblox, round 27):
    /// Ctrl+S saves a new version there, Play reports its ids.
    pub cloud: Option<rustudio_account::CloudOrigin>,
    undo: Vec<Command>,
    redo: Vec<Command>,
    /// Bumped on every change; the renderer and panels cache against it.
    pub revision: u64,
    saved_revision: u64,
    pub selection: Vec<Ref>,
    /// Unique per document (a play copy gets a new one).
    pub id: u64,
    /// Bumped when instances are added, moved, removed or renamed.
    pub tree_revision: u64,
    /// Bumped when tree structure or style-relevant properties (Name, Tags,
    /// Attributes, Selector, Priority, StyleSheet) change.
    pub style_revision: u64,
    journal: Journal,
    /// ChangeHistoryService's state (doc_history.rs).
    history: history::History,
}

#[path = "doc_history.rs"]
mod history;
pub use history::{studio_step_name, FinishOp, HistoryEvent};

/// Studio's default for a property missing from a place file, where it
/// isn't the reflection database's: Lighting.Brightness reads 1 in Studio and
/// 1.981 from the database (round 21, terrain corpus place: its sun was twice
/// Studio's).
/// The Name a new instance of `class` gets: the name each class's
/// constructor gives it (Studio 0.748: the post effects pass
/// it to their shared base constructor Studio, e.g. Studio passes
/// "DepthOfField"; the rest by the same scan of creatable constructors),
/// else the class name. Team Create leaves Name out when it is this.
pub fn default_instance_name(class: &str) -> &str {
    match class {
        "BindableEvent" => "Event",
        "BindableFunction" => "Function",
        "WedgePart" => "Wedge",
        "CornerWedgePart" => "CornerWedge",
        "UnionOperation" => "Union",
        "BloomEffect" => "Bloom",
        "BlurEffect" => "Blur",
        "ColorCorrectionEffect" => "ColorCorrection",
        "ColorGradingEffect" => "ColorGrading",
        "DepthOfFieldEffect" => "DepthOfField",
        "SunRaysEffect" => "SunRays",
        "TouchTransmitter" => "TouchInterest",
        // Measured 2026-11-21: Instance.new(class).Name in Studio for every
        // creatable class (measured in Studio).
        "AnimationGraphDefinition" | "AnimationNodeDefinition" | "AnimationValueNodeDefinition" |
        "AnimationValueOutputDefinition" | "Annotation" | "AudioSearchParams" |
        "BubbleChatMessageProperties" | "CompositeValueCurve" | "CurveAnimation" |
        "DataStoreGetOptions" | "DataStoreIncrementOptions" | "DataStoreOptions" |
        "DataStoreSetOptions" | "EulerRotationCurve" | "ExperienceInviteOptions" | "FloatCurve" |
        "GetTextBoundsParams" | "HiddenSurfaceRemovalAsset" | "HumanoidController" | "MarkerCurve" |
        "PartOperationAsset" | "PluginAction" | "ReflectionMetadata" | "ReflectionMetadataCallbacks" |
        "ReflectionMetadataClass" | "ReflectionMetadataClasses" | "ReflectionMetadataEnum" |
        "ReflectionMetadataEnumItem" | "ReflectionMetadataEnums" | "ReflectionMetadataEvents" |
        "ReflectionMetadataFunctions" | "ReflectionMetadataMember" | "ReflectionMetadataProperties" |
        "ReflectionMetadataYieldFunctions" | "RotationCurve" | "StateMachineTransitionDefinition" |
        "TeleportOptions" | "TextChatMessageProperties" | "ValueCurve" | "Vector3Curve" |
        "VehicleController" | "VisualizationMode" | "VisualizationModeCategory" | "WorkspaceAnnotation" => "Instance",
        "BinaryStringValue" | "BoolValue" | "BrickColorValue" | "CFrameValue" | "Color3Value" |
        "IntValue" | "NumberValue" | "ObjectValue" | "RayValue" | "StringValue" | "Vector3Value" => "Value",
        "BlockMesh" | "FileMesh" | "SpecialMesh" => "Mesh",
        "Pants" | "Shirt" => "Clothing",
        "BodyColors" => "Body Colors",
        "GeneratedFolder" => "Folder",
        "IntersectOperation" => "Intersection",
        "NegateOperation" => "NegativePart",
        "NumberPose" => "Pose",
        "ShirtGraphic" => "Shirt Graphic",
        "TrussPart" => "Truss",
        _ => class,
    }
}

/// The attribute Studio leaves on Lighting once it has turned a place's
/// Technology into LightingStyle / PrioritizeLightingQuality (its unified
/// lighting migration); with it set, nothing is derived again.
pub const LIGHTING_MIGRATED_ATTRIBUTE: &str = "RBX_LightingTechnologyUnifiedMigration";

/// Lighting's LightingStyle (0 Realistic, 1 Soft) and
/// PrioritizeLightingQuality when the instance stores none. Already migrated
/// (LIGHTING_MIGRATED_ATTRIBUTE true, as every Team Create server's Lighting
/// is): the constructor's values, Realistic and true (0.744 Studio
/// stores 0 at and 1 at, the fields getters Studio and
/// Studio read). Not migrated (older files): Studio's migration from
/// Technology: Future → Realistic, true; ShadowMap → Soft, true; anything
/// else → Soft, false (measured too: MECHANICS.md "LightingStyle default").
pub fn lighting_style_default(inst: &rbx_dom_weak::Instance) -> (u32, bool) {
    let migrated = match inst.properties.get(&ustr("Attributes")) {
        Some(Variant::Attributes(a)) => matches!(a.get(LIGHTING_MIGRATED_ATTRIBUTE), Some(Variant::Bool(true))),
        _ => false,
    };
    if migrated {
        return (0, true);
    }
    let tech = match inst.properties.get(&ustr("Technology")) {
        Some(Variant::Enum(e)) => e.to_u32(),
        _ => crate::mech::TECHNOLOGY_SOFT[1],
    };
    (u32::from(crate::mech::TECHNOLOGY_SOFT.contains(&tech)), crate::mech::TECHNOLOGY_QUALITY.contains(&tech))
}

pub fn studio_default(class: &str, name: &str) -> Option<Variant> {
    if let Some(v) = constructor_default(class, name) {
        return Some(v);
    }
    match (class, name) {
        ("Lighting", "Brightness") => Some(Variant::Float32(crate::mech::LIGHTING_BRIGHTNESS_DEFAULT)),
        // Studio's UICorner default; the reflection database has none, and
        // files saved at the default leave it out (Place B's index UI).
        ("UICorner", "CornerRadius") => Some(Variant::UDim(rbx_dom_weak::types::UDim::new(0.0, 8))),
        // Every instance's (a plugin reads false: ); the
        // reflection database has no default.
        (_, "RobloxLocked") => Some(Variant::Bool(false)),
        // Every instance's: the empty set (Studio reads an empty
        // SecurityCapabilities; the reflection database has no default).
        (_, "Capabilities") => Some(Variant::SecurityCapabilities(rbx_dom_weak::types::SecurityCapabilities::from_bits(0))),
        // The other BaseParts are Smooth on every face from the constructor
        // (measured: MeshPart, CornerWedgePart, UnionOperation …; Part and
        // its kin are in constructor_default); the reflection database still
        // has the old Studs top / Inlet bottom. TrussPart keeps its own.
        (
            c,
            "TopSurface" | "BottomSurface" | "FrontSurface" | "BackSurface" | "LeftSurface" | "RightSurface",
        ) if c != "TrussPart" && is_a(c, "BasePart") => {
            Some(Variant::Enum(rbx_dom_weak::types::Enum::from_u32(0)))
        }
        _ => None,
    }
}

/// A property's class default, as `Document::prop` reads a missing one
/// (Studio's measured values first, then the reflection database's, its
/// serialized name followed). It is also what a Team Create server leaves out
/// of a record.
pub fn class_default(class: &str, name: &str) -> Option<Variant> {
    prop_plan(ustr(class), ustr(name)).and_then(|p| p.fixed.clone())
}

/// What `Instance.new(class)` reads in Studio where the reflection
/// database's default (or, for classes newer than it, the API stubs') is
/// wrong. A Team Create server leaves a property out of a record when it
/// equals this value, so the live model reads the same through it.
/// Measured 2026-11-21 in Studio for every creatable class, compared with a
/// bare rustudio instance (measuring Studio lists any
/// difference left); services (Lighting, Terrain) from the Team Create mass
/// test: values the server omitted that Studio reads.
pub fn constructor_default(class: &str, name: &str) -> Option<Variant> {
    use rbx_dom_weak::types::{Content, ContentId, Enum};
    const SMOOTH: u32 = 0;
    const STUDS: u32 = 3;
    const INLET: u32 = 4;
    let id = |s: &str| Some(Variant::ContentId(ContentId::from(s.to_string())));
    match (class, name) {
        // The Part constructor's Studs top / Inlet bottom (Insert Part makes
        // them Smooth: new_part); WedgePart has an Inlet bottom only.
        ("Part" | "Seat" | "SpawnLocation" | "VehicleSeat" | "Terrain", "TopSurface") => Some(Variant::Enum(Enum::from_u32(STUDS))),
        ("WedgePart", "TopSurface") => Some(Variant::Enum(Enum::from_u32(SMOOTH))),
        ("Part" | "Seat" | "SpawnLocation" | "VehicleSeat" | "Terrain" | "WedgePart", "BottomSurface") => {
            Some(Variant::Enum(Enum::from_u32(INLET)))
        }
        // The constructor's (and the reflection database's) false; Studio's
        // new-place template turns it on (Document::new_place does too).
        ("Lighting", "GlobalShadows") => Some(Variant::Bool(false)),
        ("AdGui", "Active" | "AutoLocalize" | "EnableVideoAds" | "Enabled" | "ResetOnSpawn") => Some(Variant::Bool(true)),
        ("AdGui", "Face") => Some(Variant::Enum(Enum::from_u32(5))), // Front
        ("AdGui", "FallbackImageContent") => Some(Variant::Content(Content::none())),
        ("AudioDeviceInput", "Active") => Some(Variant::Bool(false)),
        ("AudioSearchParams", "MaxDuration") => Some(Variant::Int32(i32::MAX)),
        ("AvatarAccessoryRules", n) if n.ends_with("AccessoryOverride") => Some(Variant::Int64(-1)),
        ("AvatarAnimationRules", n) if n.ends_with("AnimationOverride") => Some(Variant::Int64(-1)),
        ("AvatarBodyRules", n) if n.ends_with("Override") => Some(Variant::Int64(-1)),
        // (Not the Classic* / *Shoes ones: Studio can't read those.)
        (
            "AvatarClothingRules",
            "DressSkirtAccessoryOverride" | "JacketAccessoryOverride" | "PantsAccessoryOverride" | "ShirtAccessoryOverride"
            | "ShortsAccessoryOverride" | "SweaterAccessoryOverride" | "TShirtAccessoryOverride",
        ) => Some(Variant::Int64(-1)),
        ("Camera" | "ViewportCamera", "OrthographicSize") => Some(Variant::Float32(35.0)),
        ("ViewportCamera", "ViewStretch") => Some(Variant::Float32(1.0)),
        // An asset number, not a content id.
        ("CharacterMesh", "MeshId" | "BaseTextureId" | "OverlayTextureId") => Some(Variant::Int64(0)),
        ("DataStoreGetOptions", "UseCache") => Some(Variant::Bool(true)),
        ("DebuggerWatch", "Expression") => Some(Variant::String(String::new())),
        ("GetTextBoundsParams", "Size") => Some(Variant::Float32(20.0)),
        ("ScrollingFrame", "TopImage") => id("rbxasset://textures/ui/Scroll/scroll-top.png"),
        ("ScrollingFrame", "MidImage") => id("rbxasset://textures/ui/Scroll/scroll-middle.png"),
        ("ScrollingFrame", "BottomImage") => id("rbxasset://textures/ui/Scroll/scroll-bottom.png"),
        ("SurfaceAppearance", "SurfaceSlot") => Some(Variant::Int32(-1)),
        _ => None,
    }
}

/// The instance-independent part of `Document::prop`'s fallback chain.
struct PropPlan {
    stored_as: Option<Ustr>,
    twin: Option<Ustr>,
    /// Studio's or the reflection database's default (or the empty id of a
    /// content property): returned before the XML twin is looked at.
    fixed: Option<Variant>,
    xml: Ustr,
    xml_default: Option<Variant>,
    upper: Option<String>,
}

/// None when the class isn't in the reflection database (prop gives None).
fn prop_plan(class: Ustr, key: Ustr) -> Option<std::rc::Rc<PropPlan>> {
    thread_local! {
        static PLANS: std::cell::RefCell<rustc_hash::FxHashMap<(Ustr, Ustr), Option<std::rc::Rc<PropPlan>>>> =
            Default::default();
    }
    if let Some(p) = PLANS.with(|m| m.borrow().get(&(class, key)).cloned()) {
        return p;
    }
    let name = key.as_str();
    let plan = (|| {
        let db = db();
        let Some(c) = db.classes.get(class.as_str()) else {
            // Newer than the reflection database: only a measured default.
            return Some(std::rc::Rc::new(PropPlan {
                stored_as: None,
                twin: None,
                fixed: Some(studio_default(class.as_str(), name)?),
                xml: ustr(&format!("{name}_XML")),
                xml_default: None,
                upper: None,
            }));
        };
        let stored_as = db.superclasses_iter(c).find_map(|c| match &c.properties.get(name)?.kind {
            rbx_reflection::PropertyKind::Canonical {
                serialization: rbx_reflection::PropertySerialization::SerializesAs(alt),
            } => Some(alt.to_string()),
            _ => None,
        });
        let twin = content_twin(name);
        let fixed = studio_default(class.as_str(), name)
            .or_else(|| {
                db.find_default_property(c, name)
                    .or_else(|| stored_as.as_deref().and_then(|alt| db.find_default_property(c, alt)))
                    .cloned()
            })
            .or_else(|| {
                // The legacy name of a Content twin with no default of its
                // own: the twin's (Sky.MoonTextureId reads the constructor's
                // rbxasset://sky/moon.jpg: 0.748 Studio).
                let t = twin?;
                match db.find_default_property(c, t)? {
                    Variant::Content(content) => content
                        .as_uri()
                        .map(|u| Variant::ContentId(rbx_dom_weak::types::ContentId::from(u.to_string()))),
                    _ => None,
                }
            })
            .or_else(|| {
                // A content property the class has but nothing set: the
                // empty id (MeshPart.TextureID reads "" in Roblox, not nil).
                // (Only a content property: CharacterMesh.MeshId is an asset
                // number, and reads 0.)
                let content = db.superclasses_iter(c).find_map(|c| c.properties.get(name)).is_some_and(|p| {
                    matches!(p.data_type, rbx_reflection::DataType::Value(rbx_dom_weak::types::VariantType::ContentId))
                });
                (twin.is_some() && content)
                    .then(|| Variant::ContentId(rbx_dom_weak::types::ContentId::from(String::new())))
            });
        let xml = format!("{name}_XML");
        let xml_default = db.find_default_property(c, &xml).cloned();
        let mut chars = name.chars();
        let upper = match chars.next() {
            Some(ch) if ch.is_ascii_lowercase() => Some(ch.to_ascii_uppercase().to_string() + chars.as_str()),
            _ => None,
        };
        Some(std::rc::Rc::new(PropPlan {
            stored_as: stored_as.map(|s| ustr(&s)),
            twin: twin.map(ustr),
            fixed,
            xml: ustr(&xml),
            xml_default,
            upper,
        }))
    })();
    PLANS.with(|m| m.borrow_mut().insert((class, key), plan.clone()));
    plan
}

pub fn db() -> &'static rbx_reflection::ReflectionDatabase<'static> {
    rbx_reflection_database::get().unwrap_or_else(|_| rbx_reflection_database::get_bundled())
}

/// Services Studio keeps for the session only (never saved with the place):
/// the plugin host puts plugin widgets and plugin GUI in them.
pub const UNSAVED_SERVICES: &[&str] = &[
    "PluginGuiService",
    "RobloxPluginGuiService",
    "CoreGui",
    "StudioService",
    "PluginDebugService",
    "ScriptEditorService",
];

/// Whether `r` is in (or is) one of the UNSAVED_SERVICES.
pub fn in_unsaved_service(d: &Document, r: Ref) -> bool {
    let root = d.dom.root_ref();
    let mut cur = r;
    while let Some(i) = d.dom.get_by_ref(cur) {
        if i.parent() == root {
            // (A service itself appearing, as GetService makes one a file
            // left out, isn't an edit of the place either: Studio has them all.)
            return UNSAVED_SERVICES.contains(&i.class.as_str()) || (cur == r && is_service(i.class.as_str()));
        }
        cur = i.parent();
    }
    // (Removed: no longer anywhere; limbo instances count as the place's.)
    false
}

// ------------------------------------------- property readers (shared)

#[inline]
pub fn f32_of(v: Option<Variant>) -> Option<f32> {
    match v? {
        Variant::Float32(f) => Some(f),
        Variant::Float64(f) => Some(f as f32),
        Variant::Int32(i) => Some(i as f32),
        _ => None,
    }
}

#[inline]
pub fn vec3_of(v: Option<Variant>) -> Option<Vec3> {
    match v? {
        Variant::Vector3(v) => Some(Vec3::new(v.x, v.y, v.z)),
        _ => None,
    }
}

#[inline]
pub fn xform_of(doc: &Document, r: Ref) -> Option<Xform> {
    let key = crate::ukey!("CFrame");
    let inst = doc.dom.get_by_ref(r)?;
    // The pose journal normally points to a saved CFrame. Read the borrowed
    // value directly; older parts can still need the property fallback.
    let xf = match inst.properties.get(&key) {
        Some(Variant::CFrame(cf)) => Xform::from_cframe(cf),
        Some(_) => return None,
        None => match doc.prop_key(r, key)? {
            Variant::CFrame(cf) => Xform::from_cframe(&cf),
            _ => return None,
        },
    };
    // Old places can hold slightly skewed rotations.
    Some(xf.orthonormalized())
}

/// `Instance:IsA` against the reflection database.
pub fn is_a(class: &str, base: &str) -> bool {
    if class == base {
        return true;
    }
    // Memoized: engine code asks this per part per frame in places, and
    // every script property read or method call asks it many times (each
    // engine domain checks the class). Keyed by the strings themselves,
    // base first: interning both names for the key (two locked, hashed
    // lookups in the global string table) was the single largest cost of
    // a property read from Luau (Ustr::from: 10-14 % of a Play frame).
    thread_local! {
        static CACHE: std::cell::RefCell<rustc_hash::FxHashMap<Box<str>, rustc_hash::FxHashMap<Box<str>, bool>>> =
            Default::default();
    }
    if let Some(v) = CACHE.with(|c| c.borrow().get(base).and_then(|m| m.get(class).copied())) {
        return v;
    }
    let db = db();
    let v = match db.classes.get(class) {
        Some(c) => db.superclasses_iter(c).any(|d| d.name == base),
        // Classes newer than the reflection database (UIBlur,
        // ParticleEmitter2D…): the API dump's hierarchy.
        None => crate::api::get().is_some_and(|a| a.is_a(class, base)),
    };
    CACHE.with(|c| c.borrow_mut().entry(base.into()).or_default().insert(class.into(), v));
    v
}

pub fn is_service(class: &str) -> bool {
    db().classes
        .get(class)
        .is_some_and(|c| c.tags.contains(&ClassTag::Service))
}

pub fn is_creatable(class: &str) -> bool {
    is_script_creatable(class) && db().classes.get(class).is_some_and(|c| !c.tags.contains(&ClassTag::Deprecated))
}

/// What `Instance.new` accepts: Studio creates deprecated classes too
/// (FunctionalTest, Hint, Message); only the Insert Object list hides them.
pub fn is_script_creatable(class: &str) -> bool {
    match db().classes.get(class) {
        Some(c) => !c.tags.contains(&ClassTag::NotCreatable) && !c.tags.contains(&ClassTag::Service),
        // Newer than the reflection database: as the API dump says.
        None => crate::api::get()
            .and_then(|a| a.classes.get(class))
            .is_some_and(|c| c.creatable && !c.service),
    }
}

/// A property's enum item as its value (`Variant::Enum` / `Variant::EnumItem`).
pub fn enum_u32(v: Option<Variant>) -> Option<u32> {
    match v? {
        Variant::Enum(e) => Some(e.to_u32()),
        Variant::EnumItem(e) => Some(e.value),
        _ => None,
    }
}

/// The MaterialVariant a part is drawn with: its MaterialVariant name, else
/// MaterialService's `<Material>Name` override for its Material; the variant
/// (in MaterialService, same BaseMaterial) gives its ColorMap and StudsPerTile.
/// MaterialService.Use2022Materials (absent: false, the pre-2022 table,
/// which is what Studio draws for a place without the setting).
pub fn use_2022_materials(doc: &Document) -> bool {
    doc.find_child(doc.dom.root_ref(), "MaterialService")
        .and_then(|ms| {
            // Consult serialized values before reflection defaults: the API
            // property can default false while the XML property stores true.
            ["Use2022Materials", "Use2022MaterialsXml"]
                .iter()
                .find_map(|k| match doc.dom.get_by_ref(ms)?.properties.get(&rbx_dom_weak::ustr(k)).cloned() {
                    Some(Variant::Bool(b)) => Some(b),
                    _ => None,
                })
        })
        .unwrap_or(false)
}

/// A Content property's id as a string (an EditableImage / EditableMesh
/// resolves to its current generated id), trimmed; None when empty.
pub fn content_string(v: Option<Variant>) -> Option<String> {
    match v? {
        Variant::ContentId(c) => Some(c.as_str().to_string()),
        Variant::Content(c) => c.as_uri().map(str::to_string).or_else(|| {
            c.as_object().and_then(|o| crate::editable_source::content_uri(o, None))
        }),
        Variant::String(s) => Some(s),
        _ => None,
    }
    // Roblox reads content ids trimmed (Place B has "…?id=125882923   ").
    .map(|s| s.trim().to_string())
    .filter(|s| !s.is_empty())
}

/// `Enum.<enum_type>.<name>`'s value, from the reflection database (else the
/// API dump); a legacy name is the item it became (the dump's LegacyNames).
pub fn enum_item_value(enum_type: &str, name: &str) -> Option<u32> {
    enum_item_by_name(enum_type, name).map(|(_, value)| value)
}

/// The canonical name and value, preserving reflection's exact-name and
/// legacy-name precedence before falling back to a dump-only enum.
pub fn enum_item_by_name(enum_type: &str, name: &str) -> Option<(&'static str, u32)> {
    match db().enums.get(enum_type) {
        Some(e) => {
            let (name, value) = e.items.get_key_value(name).or_else(|| {
                let now = crate::api::get()?.legacy_enum_items.get(&(enum_type.to_string(), name.to_string()))?;
                e.items.get_key_value(now.as_str())
            })?;
            Some((*name, *value))
        }
        None => crate::api::get()?.enums.get(enum_type)?.iter()
            .find(|(n, _)| n == name).map(|(n, v)| (n.as_str(), *v)),
    }
}

/// The name of `Enum.<enum_type>`'s item `value` (the reflection database,
/// else the API dump).
pub fn enum_item_name(enum_type: &str, value: u32) -> Option<String> {
    match db().enums.get(enum_type) {
        Some(e) => e.items.iter().find(|(_, v)| **v == value).map(|(k, _)| k.to_string()),
        None => crate::api::get()?.enums.get(enum_type)?.iter().find(|(_, v)| *v == value).map(|(n, _)| n.clone()),
    }
}

fn limbo_dom() -> WeakDom {
    WeakDom::new(InstanceBuilder::new("Folder").with_name("Limbo"))
}

pub fn new_part(
    name: &str,
    shape: u32,
    pos: glam::Vec3,
    size: glam::Vec3,
    color: [u8; 3],
) -> InstanceBuilder {
    let mut b = InstanceBuilder::new("Part")
        .with_name(name)
        .with_property("Anchored", true)
        .with_property("Size", Vector3::new(size.x, size.y, size.z))
        .with_property(
            "CFrame",
            CFrame::new(
                Vector3::new(pos.x, pos.y, pos.z),
                CFrame::identity().orientation,
            ),
        )
        .with_property("Color", Color3uint8::new(color[0], color[1], color[2]))
        // Insert Part's smooth faces: the constructor's are a Studs top and
        // an Inlet bottom (constructor_default).
        .with_property("TopSurface", Enum::from_u32(0))
        .with_property("BottomSurface", Enum::from_u32(0));
    // Block is the default shape; wedge classes have no Shape property at all.
    if shape != 1 {
        b.add_property("Shape", Enum::from_u32(shape));
    }
    b
}

impl Document {
    fn from_dom(mut dom: WeakDom, kind: DocKind, path: Option<PathBuf>, title: String) -> Self {
        // Instances of a class share their common property values from here
        // on (rbx_dom_weak's PropMap): ~1 GB less on a 180 000 instance place.
        // RUSTUDIO_NO_SHARE_PROPS=1 leaves every instance with its own
        // values, to rule the sharing in or out of a property bug.
        static OFF: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
        if !*OFF.get_or_init(|| std::env::var_os("RUSTUDIO_NO_SHARE_PROPS").is_some()) {
            // (a place's worth: the full-size maps the file was read into
            // are free now, and go back to the system)
            if dom.share_properties() >= 10_000 {
                crate::heap::trim();
            }
        }
        Document {
            dom,
            limbo: limbo_dom(),
            kind,
            path,
            title,
            cloud: None,
            undo: Vec::new(),
            redo: Vec::new(),
            revision: 1,
            saved_revision: 1,
            selection: Vec::new(),
            id: NEXT_DOC_ID.fetch_add(1, std::sync::atomic::Ordering::Relaxed),
            tree_revision: 0,
            style_revision: 0,
            journal: Journal {
                base: 0,
                marks: Vec::new(),
                query: None,
                styles: None,
                read: [None; JOURNAL_READERS],
                graph_pos: std::sync::atomic::AtomicU64::new(0),
            },
            history: Default::default(),
        }
    }

    // -------------------------------------------------------------- journal

    /// Notes a property change (and bumps the revision).
    pub fn mark_prop(&mut self, r: Ref, name: Ustr) {
        self.revision += 1;
        let s = name.as_str();
        if s == "Name" {
            self.tree_revision += 1;
        }
        let mark = Mark::Prop(r, name);
        if matches!(s, "Name" | "Tags" | "Attributes" | "Selector" | "Priority" | "StyleSheet" | "Interactable") {
            self.push_style_mark(mark);
        }
        self.push_mark(mark);
    }

    /// Notes that `r`'s subtree was created, moved or removed.
    pub fn mark_tree(&mut self, r: Ref) {
        self.revision += 1;
        self.tree_revision += 1;
        let mark = Mark::Tree(r);
        self.push_style_mark(mark);
        self.push_mark(mark);
    }

    /// Everything may have changed: readers rebuild.
    pub fn mark_all(&mut self) {
        self.journal.query = None;
        self.journal.styles = None;
        self.style_revision += 1;
        self.reset_general_marks();
    }

    fn reset_general_marks(&mut self) {
        self.revision += 1;
        self.tree_revision += 1;
        let j = &mut self.journal;
        j.base += j.marks.len() as u64 + 1;
        j.marks.clear();
    }

    fn push_style_mark(&mut self, mark: Mark) {
        self.style_revision += 1;
        if let Some(marks) = self.journal.styles.as_mut() {
            if marks.len() >= mech::JOURNAL_READER_LAG {
                self.journal.styles = None;
            } else {
                marks.push(mark);
            }
        }
    }

    /// Reads the style selector changes independently of spatial readers.
    pub fn take_style_marks(&mut self) -> Marks {
        self.journal.styles.replace(Vec::new()).map_or(Marks::All, Marks::Some)
    }

    fn push_mark(&mut self, m: Mark) {
        let j = &mut self.journal;
        if let Some(query) = &mut j.query {
            let relevant = match m {
                Mark::Tree(_) => true,
                Mark::Prop(_, name) => matches!(
                    name.as_str(), "CFrame" | "Size" | "CanCollide" | "CanQuery" | "SmoothGrid" | "Shape"
                ),
            };
            if relevant {
                if query.len() >= mech::JOURNAL_READER_LAG {
                    j.query = None;
                } else {
                    query.push(m);
                }
            }
        }
        if j.marks.len() >= mech::JOURNAL_LIMIT {
            // General readers rebuild. Spatial queries still have their
            // relevant marks, including the one that overflowed this list.
            self.reset_general_marks();
            return;
        }
        j.marks.push(m);
    }

    /// The marks `reader` hasn't seen yet (All on its first call, or when it
    /// fell too far behind). Marks every reader has seen are dropped.
    pub fn marks_for(&mut self, reader: Reader) -> Marks {
        let j = &mut self.journal;
        let out = if reader == Reader::Query {
            j.query.replace(Vec::new()).map_or(Marks::All, Marks::Some)
        } else {
            match j.read[reader as usize] {
                Some(pos) if pos >= j.base => Marks::Some(j.marks[(pos - j.base) as usize..].to_vec()),
                _ => Marks::All,
            }
        };
        self.advance_mark_reader(reader);
        out
    }

    /// Advance past changes already known to a reader, such as its own writes.
    pub fn discard_marks_for(&mut self, reader: Reader) {
        if reader == Reader::Query {
            self.journal.query.get_or_insert_with(Vec::new).clear();
        }
        self.advance_mark_reader(reader);
    }

    fn advance_mark_reader(&mut self, reader: Reader) {
        let j = &mut self.journal;
        let end = j.base + j.marks.len() as u64;
        j.read[reader as usize] = Some(end);
        // A reader that stopped reading doesn't hold the journal back: it
        // rebuilds from scratch if it comes back.
        for r in j.read.iter_mut() {
            if r.is_some_and(|p| end - p > mech::JOURNAL_READER_LAG as u64) {
                *r = None;
            }
        }
        use std::sync::atomic::Ordering::Relaxed;
        let gp = j.graph_pos.load(Relaxed);
        if gp > 0 && end - (gp - 1) > mech::JOURNAL_READER_LAG as u64 {
            j.graph_pos.store(0, Relaxed);
        }
        let gp = j.graph_pos.load(Relaxed).checked_sub(1);
        let keep_from = j.read.iter().flatten().copied().chain(gp).min().unwrap_or(end);
        if keep_from > j.base {
            let n = ((keep_from - j.base) as usize).min(j.marks.len());
            j.marks.drain(..n);
            j.base += n as u64;
        }
    }

    /// The journal's end: what the next mark's sequence number will be.
    pub fn journal_end(&self) -> u64 {
        self.journal.base + self.journal.marks.len() as u64
    }

    /// The marks from sequence number `pos` on, without moving any reader;
    /// None when they were dropped already (or everything was marked).
    pub fn marks_since(&self, pos: u64) -> Option<&[Mark]> {
        let j = &self.journal;
        (pos >= j.base).then(|| &j.marks[((pos - j.base) as usize).min(j.marks.len())..])
    }

    /// The marks since the joint graph last looked (All the first time or
    /// after it fell behind). Works through `&self` so property reads can
    /// keep the graph current without a mutable document.
    pub fn graph_marks(&self) -> Marks {
        use std::sync::atomic::Ordering::Relaxed;
        let j = &self.journal;
        let end = j.base + j.marks.len() as u64;
        let out = match j.graph_pos.load(Relaxed).checked_sub(1) {
            Some(pos) if pos >= j.base => Marks::Some(j.marks[(pos - j.base) as usize..].to_vec()),
            _ => Marks::All,
        };
        j.graph_pos.store(end + 1, Relaxed);
        out
    }

    /// Lends a removed instance (and its subtree) from the undo limbo back
    /// to the DOM, parentless, so code reacting to its removal can still read
    /// it (the plugin host's edit sync). `relimbo` takes it back.
    pub fn unlimbo(&mut self, r: Ref) -> bool {
        if self.limbo.get_by_ref(r).is_some() && r != self.limbo.root_ref() {
            // (transfer needs a parent: in under the root, then out.)
            let root = self.dom.root_ref();
            self.limbo.transfer(r, &mut self.dom, root);
            self.dom.transfer_within(r, Ref::none());
            true
        } else {
            false
        }
    }

    /// Returns an instance `unlimbo` lent, if it is still parentless (a
    /// script that parented it somewhere keeps it there). (The plugin host
    /// no longer returns them: a plugin may keep reading an instance the
    /// person deleted, as in Studio, and undo / redo find it either way.)
    pub fn relimbo(&mut self, r: Ref) {
        if self.dom.get_by_ref(r).is_some_and(|i| i.parent().is_none()) && r != self.dom.root_ref() {
            let lr = self.limbo.root_ref();
            self.dom.transfer(r, &mut self.limbo, lr);
        }
    }

    /// `r` and its descendants, whether in the DOM or removed (in limbo).
    pub fn subtree(&self, r: Ref) -> Vec<Ref> {
        let dom = if self.dom.get_by_ref(r).is_some() {
            &self.dom
        } else {
            &self.limbo
        };
        if dom.get_by_ref(r).is_none() {
            return vec![r];
        }
        dom.descendants_of(r).map(|i| i.referent()).collect()
    }

    /// A fresh place like the starter "Baseplate" template.
    pub fn new_place() -> Self {
        let bp = mech::BASEPLATE_SIZE;
        let workspace = InstanceBuilder::new("Workspace")
            .with_name("Workspace")
            .with_child(InstanceBuilder::new("Camera").with_name("Camera"))
            .with_child(
                new_part(
                    "Baseplate",
                    1,
                    glam::Vec3::new(0.0, -bp[1] / 2.0, 0.0),
                    glam::Vec3::from(bp),
                    mech::BASEPLATE_COLOR,
                )
                .with_property("Locked", true),
            )
            .with_child(
                InstanceBuilder::new("SpawnLocation")
                    .with_name("SpawnLocation")
                    .with_property("Anchored", true)
                    .with_property("Size", Vector3::new(12.0, 1.0, 12.0))
                    .with_property(
                        "CFrame",
                        CFrame::new(Vector3::new(0.0, 0.5, 0.0), CFrame::identity().orientation),
                    )
                    .with_property("Color", Color3uint8::new(163, 162, 165))
                    .with_property("TopSurface", rbx_dom_weak::types::Enum::from_u32(0))
                    .with_property("BottomSurface", rbx_dom_weak::types::Enum::from_u32(0)),
            );
        let lighting = InstanceBuilder::new("Lighting")
            .with_name("Lighting")
            .with_property("ClockTime", mech::CLOCK_TIME)
            .with_property("Brightness", mech::LIGHT_BRIGHTNESS)
            // On in Studio's templates (the constructor's is off).
            .with_property("GlobalShadows", true)
            // Studio's Baseplate template is lit by Future (Realistic).
            .with_property(
                "Technology",
                rbx_dom_weak::types::Enum::from_u32(mech::TECHNOLOGY_FUTURE),
            );
        let player = InstanceBuilder::new("StarterPlayer")
            .with_name("StarterPlayer")
            .with_child(
                InstanceBuilder::new("StarterPlayerScripts").with_name("StarterPlayerScripts"),
            )
            .with_child(
                InstanceBuilder::new("StarterCharacterScripts")
                    .with_name("StarterCharacterScripts"),
            );
        let mut root = InstanceBuilder::new("DataModel").with_name("Game");
        root.add_child(workspace);
        root.add_child(InstanceBuilder::new("Players").with_name("Players"));
        root.add_child(lighting);
        for class in [
            "ReplicatedFirst",
            "ReplicatedStorage",
            "ServerScriptService",
            "ServerStorage",
            "StarterGui",
            "StarterPack",
        ] {
            root.add_child(InstanceBuilder::new(class).with_name(class));
        }
        root.add_child(player);
        for class in ["Teams", "SoundService"] {
            root.add_child(InstanceBuilder::new(class).with_name(class));
        }
        // Studio's new places start with TextChatService (Enum.ChatVersion.TextChatService = 1).
        root.add_child(
            InstanceBuilder::new("TextChatService")
                .with_name("TextChatService")
                .with_property("ChatVersion", Variant::Enum(rbx_dom_weak::types::Enum::from_u32(1))),
        );
        let mut doc = Self::from_dom(
            WeakDom::new(root),
            DocKind::Place,
            None,
            "Untitled place".into(),
        );
        // What every Studio place has (Terrain, the Camera, the services),
        // the same as an opened file gets.
        doc.ensure_services();
        doc
    }

    /// A throwaway document (used while the real one is lent to Lua).
    /// Wraps a DOM built for a play session (the client's copy).
    pub fn from_play_dom(dom: WeakDom, title: String) -> Self {
        Self::from_dom(dom, DocKind::Place, None, title)
    }

    /// A placeholder while the real document is lent to a script world: its two
    /// roots take refs outside the creation counter (RUSTUDIO_DETERMINISTIC), since how
    /// many a frame makes depends on the frame's work.
    pub fn placeholder() -> Self {
        rbx_dom_weak::with_scratch_refs(Self::empty)
    }

    pub fn empty() -> Self {
        Self::from_dom(
            WeakDom::new(InstanceBuilder::new("DataModel")),
            DocKind::Place,
            None,
            String::new(),
        )
    }

    /// A deep copy of the whole DataModel for a play session (new refs; Ref
    /// properties inside the copy point at the copied instances).
    pub fn clone_for_play(&self, title: String) -> Self {
        let root = self.dom.root();
        // (Play's DataModels are "Game" whatever the edit one is called:
        // .)
        let mut builder = InstanceBuilder::new(root.class).with_name("Game");
        for (k, v) in &root.properties {
            builder.add_property(*k, v.clone());
        }
        let mut dom = WeakDom::new(builder);
        let kids = root.children().to_vec();
        let copies = self.dom.clone_multiple_into_external(&kids, &mut dom);
        let new_root = dom.root_ref();
        for c in copies {
            dom.transfer_within(c, new_root);
        }
        let mut doc = Self::from_dom(dom, DocKind::Place, self.path.clone(), title);
        doc.cloud = self.cloud.clone();
        doc
    }

    pub fn kind_for(path: &Path) -> Option<(DocKind, bool)> {
        let ext = path.extension()?.to_str()?.to_ascii_lowercase();
        match ext.as_str() {
            "rbxl" => Some((DocKind::Place, false)),
            "rbxlx" => Some((DocKind::Place, true)),
            "rbxm" => Some((DocKind::Model, false)),
            "rbxmx" => Some((DocKind::Model, true)),
            _ => None,
        }
    }

    pub fn decode(bytes: &[u8]) -> Result<WeakDom, String> {
        let xml = bytes.starts_with(b"<roblox") && !bytes.starts_with(b"<roblox!");
        if xml {
            rbx_xml::from_reader_default(Cursor::new(bytes)).map_err(|e| e.to_string())
        } else {
            rbx_binary::from_reader(Cursor::new(bytes)).map_err(|e| e.to_string())
        }
    }

    pub fn open(path: &Path) -> Result<Self, String> {
        let (kind, _) = Self::kind_for(path)
            .ok_or("unsupported file type (use .rbxl, .rbxlx, .rbxm or .rbxmx)")?;
        let bytes = std::fs::read(path).map_err(|e| format!("{}: {e}", path.display()))?;
        crate::load_trace::mark(&format!("place read ({:.1} MB)", bytes.len() as f64 / 1e6));
        let dom = Self::decode(&bytes)?;
        crate::load_trace::mark("place parsed");
        let title = path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        if kind == DocKind::Place {
            crate::assets::note_opened_place(path);
        }
        let mut doc = Self::from_dom(dom, kind, Some(path.to_path_buf()), title);
        doc.ensure_services();
        Ok(doc)
    }

    /// A new, unsaved place from a DataModel built in memory (the Start
    /// Page's built-in templates), with what every place has.
    pub fn from_template_dom(dom: WeakDom, title: String) -> Self {
        let mut doc = Self::from_dom(dom, DocKind::Place, None, title);
        doc.ensure_services();
        doc
    }

    pub fn from_place_bytes(bytes: &[u8], title: String) -> Result<Self, String> {
        let mut doc = Self::from_dom(Self::decode(bytes)?, DocKind::Place, None, title);
        doc.ensure_services();
        Ok(doc)
    }

    /// Every Roblox DataModel has these services, whatever the file lists
    /// (a place file may hold only a few, e.g. ReplicatedStorage,
    /// ServerScriptService and StarterPlayer): Studio makes the missing ones
    /// when it loads the place, and Workspace always has a Terrain and a
    /// Camera that is its CurrentCamera. Returns the services added.
    pub fn ensure_services(&mut self) -> Vec<&'static str> {
        if self.kind != DocKind::Place {
            return Vec::new();
        }
        let root = self.dom.root_ref();
        let mut added = Vec::new();
        for svc in mech::ALWAYS_SERVICES {
            if self.find_child(root, svc).is_none() {
                self.dom
                    .insert(root, InstanceBuilder::new(*svc).with_name(*svc));
                added.push(*svc);
            }
        }
        if let Some(sp) = self.find_child(root, "StarterPlayer") {
            for svc in ["StarterPlayerScripts", "StarterCharacterScripts"] {
                if self.find_child(sp, svc).is_none() {
                    self.dom.insert(sp, InstanceBuilder::new(svc).with_name(svc));
                }
            }
        }
        if let Some(ws) = self.workspace() {
            if self.find_child(ws, "Terrain").is_none() {
                self.dom
                    .insert(ws, InstanceBuilder::new("Terrain").with_name("Terrain"));
            }
        }
        self.ensure_current_camera();
        // A place saved without LightingStyle: Studio takes it from
        // Technology (tools/fidelity: Place A, which saves neither, reads Soft
        // with PrioritizeLightingQuality false; the control place, Future,
        // reads Realistic with it true; Place B, ShadowMap, Soft with it true).
        if let Some(l) = self.find_child(root, "Lighting") {
            if let Some(i) = self.dom.get_by_ref_mut(l) {
                let key = |k: &str| rbx_dom_weak::ustr(k);
                if !i.properties.contains_key(&key("LightingStyle")) {
                    // Neither saved (Place A: its chapel is built at run time)
                    // reads Soft too; a place Studio already migrated keeps
                    // the constructor's (lighting_style_default).
                    let (style, quality) = lighting_style_default(i);
                    i.properties.insert(key("LightingStyle"), Variant::Enum(rbx_dom_weak::types::Enum::from_u32(style)));
                    if !i.properties.contains_key(&key("PrioritizeLightingQuality")) {
                        i.properties.insert(key("PrioritizeLightingQuality"), Variant::Bool(quality));
                    }
                }
            }
        }
        added
    }

    /// Workspace.CurrentCamera: Studio makes the edit camera itself, in every
    /// DataModel (a Team Create server never sends one: the 2026-11-20 FTF
    /// [DEV] join has no Camera record). Made in place, not as an edit.
    pub fn ensure_current_camera(&mut self) -> Option<Ref> {
        let ws = self.workspace()?;
        if let Some(Variant::Ref(c)) = self.prop(ws, "CurrentCamera") {
            if self.exists(c) {
                return Some(c);
            }
        }
        let cam = self.find_child(ws, "Camera").unwrap_or_else(|| {
            self.dom.insert(
                ws,
                InstanceBuilder::new("Camera").with_name("Camera").with_property("FieldOfView", mech::FOV_DEG),
            )
        });
        if let Some(i) = self.dom.get_by_ref_mut(ws) {
            i.properties.insert(rbx_dom_weak::ustr("CurrentCamera"), Variant::Ref(cam));
        }
        Some(cam)
    }

    pub fn encode(&self, refs: &[Ref], xml: bool) -> Result<Vec<u8>, String> {
        let mut out = Vec::new();
        if xml {
            rbx_xml::to_writer_default(&mut out, &self.dom, refs).map_err(|e| e.to_string())?;
        } else {
            rbx_binary::to_writer(&mut out, &self.dom, refs).map_err(|e| e.to_string())?;
        }
        Ok(out)
    }

    /// The whole document in its file format (place or model). Studio's
    /// session services that plugins fill (PluginGuiService's widgets,
    /// CoreGui) aren't saved, as in Studio.
    pub fn encode_all(&self, xml: bool) -> Result<Vec<u8>, String> {
        self.encode(&self.save_roots(), xml)
    }

    /// Copies the saveable tree so background serialization sees the revision
    /// that was submitted, while the editor keeps working on the original.
    pub fn save_snapshot(&self) -> (WeakDom, Vec<Ref>) {
        let mut dom = WeakDom::new(InstanceBuilder::new("DataModel"));
        let roots = self.dom.clone_multiple_into_external(&self.save_roots(), &mut dom);
        (dom, roots)
    }

    fn save_roots(&self) -> Vec<Ref> {
        self.dom
            .root()
            .children()
            .iter()
            .copied()
            .filter(|c| !self.dom.get_by_ref(*c).is_some_and(|i| UNSAVED_SERVICES.contains(&i.class.as_str())))
            .collect()
    }

    pub fn save_to(&mut self, path: &Path) -> Result<(), String> {
        let (kind, xml) = Self::kind_for(path).ok_or("save as .rbxl, .rbxlx, .rbxm or .rbxmx")?;
        let bytes = self.encode_all(xml)?;
        std::fs::write(path, bytes).map_err(|e| format!("{}: {e}", path.display()))?;
        self.kind = kind;
        self.path = Some(path.to_path_buf());
        self.title = path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        self.saved_revision = self.revision;
        Ok(())
    }

    /// Writes only the given instances as a model file.
    pub fn save_selection_to(&self, refs: &[Ref], path: &Path) -> Result<(), String> {
        let xml = path
            .extension()
            .is_some_and(|e| e.eq_ignore_ascii_case("rbxmx"));
        let bytes = self.encode(&self.top_level(refs), xml)?;
        std::fs::write(path, bytes).map_err(|e| format!("{}: {e}", path.display()))
    }

    pub fn dirty(&self) -> bool {
        self.revision != self.saved_revision
    }

    /// Marks the document saved as of `revision` (a save that ran in the
    /// background: edits made since stay unsaved).
    pub fn mark_saved_at(&mut self, revision: u64) {
        self.saved_revision = revision;
    }

    // ---------------------------------------------------------------- queries

    pub fn find_child(&self, parent: Ref, class: &str) -> Option<Ref> {
        let p = self.dom.get_by_ref(parent)?;
        p.children()
            .iter()
            .copied()
            .find(|c| self.dom.get_by_ref(*c).is_some_and(|i| i.class == class))
    }

    /// First descendant of `ancestor` with this name.
    pub fn find_named(&self, ancestor: Ref, name: &str) -> Option<Ref> {
        // (A ref from the other DataModel, or a removed one: nothing here.)
        self.dom.get_by_ref(ancestor)?;
        self.dom
            .descendants_of(ancestor)
            .find(|i| i.referent() != ancestor && i.name == name)
            .map(|i| i.referent())
    }

    pub fn workspace(&self) -> Option<Ref> {
        self.find_child(self.dom.root_ref(), "Workspace")
    }

    /// Where new objects go: Workspace for places, the root for models.
    pub fn default_parent(&self) -> Ref {
        match self.kind {
            DocKind::Place => self.workspace().unwrap_or_else(|| self.dom.root_ref()),
            DocKind::Model => self.dom.root_ref(),
        }
    }

    #[inline]
    pub fn exists(&self, r: Ref) -> bool {
        self.dom.get_by_ref(r).is_some()
    }

    /// Whether `r` is in the 3D world under Workspace `ws`: a descendant
    /// not inside a ViewportFrame (whose parts, lights and effects are its
    /// own, vf_render.rs, even on a SurfaceGui under Workspace).
    pub fn in_world(&self, ws: Ref, r: Ref) -> bool {
        let mut cur = self.dom.get_by_ref(r).map(|i| i.parent());
        while let Some(p) = cur {
            if p.is_none() {
                return false;
            }
            if p == ws {
                return true;
            }
            let i = self.dom.get_by_ref(p);
            if i.is_some_and(|i| i.class == "ViewportFrame") {
                return false;
            }
            cur = i.map(|i| i.parent());
        }
        false
    }

    pub fn is_ancestor(&self, ancestor: Ref, of: Ref) -> bool {
        let mut cur = self.dom.get_by_ref(of).map(|i| i.parent());
        while let Some(p) = cur {
            if p.is_none() {
                return false;
            }
            if p == ancestor {
                return true;
            }
            cur = self.dom.get_by_ref(p).map(|i| i.parent());
        }
        false
    }

    /// Drops refs whose ancestor is also in the list (for copy/move/delete).
    pub fn top_level(&self, refs: &[Ref]) -> Vec<Ref> {
        refs.iter()
            .copied()
            .filter(|r| self.exists(*r) && !refs.iter().any(|a| a != r && self.is_ancestor(*a, *r)))
            .collect()
    }

    #[inline]
    pub fn class_of(&self, r: Ref) -> Option<Ustr> {
        self.dom.get_by_ref(r).map(|i| i.class)
    }

    /// `class_of`, also for an instance removed to limbo.
    pub fn class_any(&self, r: Ref) -> Option<Ustr> {
        self.dom.get_by_ref(r).or_else(|| self.limbo.get_by_ref(r)).map(|i| i.class)
    }

    /// Reads a property, falling back to the class default.
    #[inline]
    pub fn prop(&self, r: Ref, name: &str) -> Option<Variant> {
        self.prop_key(r, ustr(name))
    }

    /// `prop` by an already interned name (the runtime's member routes).
    pub fn prop_key(&self, r: Ref, key: rbx_dom_weak::Ustr) -> Option<Variant> {
        let name = key.as_str();
        let inst = self.dom.get_by_ref(r)?;
        if inst.class == "ScreenGui" && matches!(name, "IgnoreGuiInset" | "ScreenInsets") {
            return Some(screen_insets_read(inst, name));
        }
        if let Some(v) = inst.properties.get(&key) {
            return Some(v.clone());
        }
        // Reflection's Terrain.MaterialColors default describes the legacy
        // palette. A modern place with no saved blob uses the 2022 tints.
        if inst.class == "Terrain" && name == "MaterialColors" && use_2022_materials(self) {
            return Some(Variant::MaterialColors(crate::terrain_mat::default_colors_2022()));
        }
        if inst.class == "Lighting" && matches!(name, "LightingStyle" | "PrioritizeLightingQuality") {
            let (style, quality) = lighting_style_default(inst);
            return Some(if name == "LightingStyle" {
                Variant::Enum(rbx_dom_weak::types::Enum::from_u32(style))
            } else {
                Variant::Bool(quality)
            });
        }
        // What doesn't depend on the instance (aliases, class defaults) is
        // worked out once per (class, name): the reflection walks it takes
        // were ~50 µs a part when a 50k-part map was cloned in.
        let plan = prop_plan(inst.class, key)?;
        if let Some(alt) = plan.stored_as {
            if let Some(v) = inst.properties.get(&alt) {
                return Some(v.clone());
            }
        }
        // Newer files keep asset ids in Content twins (MeshId → MeshContent …);
        // a saved twin wins over any class default.
        match plan.twin.and_then(|t| inst.properties.get(&t)) {
            Some(Variant::Content(c)) => {
                if let Some(uri) = c.as_uri() {
                    return Some(Variant::ContentId(rbx_dom_weak::types::ContentId::from(
                        uri.to_string(),
                    )));
                }
                // An EditableImage / EditableMesh (runtime/editable.rs): its
                // current generated id; this instance redraws on its writes.
                if let Some(uri) = c
                    .as_object()
                    .and_then(|o| crate::editable_source::content_uri(o, Some((r, name))))
                {
                    return Some(Variant::ContentId(rbx_dom_weak::types::ContentId::from(uri)));
                }
                // Stored empty (Team Create sends SunTextureContent ""): the
                // empty id, not the constructor's texture.
                if c.as_object().is_none() {
                    return Some(Variant::ContentId(rbx_dom_weak::types::ContentId::from(String::new())));
                }
            }
            // Team Create's live stream gives Content twins as plain strings.
            Some(Variant::String(uri)) => {
                return Some(Variant::ContentId(rbx_dom_weak::types::ContentId::from(
                    uri.clone(),
                )));
            }
            _ => {}
        }
        if let Some(v) = &plan.fixed {
            return Some(v.clone());
        }
        // Not serialised itself: the value lives in its XML twin
        // (Humanoid.Health → Health_XML) or, for deprecated lowercase names,
        // in the current property (maxHealth → MaxHealth).
        if let Some(v) = inst.properties.get(&plan.xml) {
            return Some(v.clone());
        }
        if let Some(v) = &plan.xml_default {
            return Some(v.clone());
        }
        match &plan.upper {
            Some(u) => self.prop(r, u),
            None => None,
        }
    }

    pub fn can_edit_structure(&self, r: Ref) -> bool {
        r != self.dom.root_ref() && self.class_of(r).is_some_and(|c| !is_service(&c))
    }

    // ----------------------------------------------------------- undo / redo

    fn mark_change(&mut self, c: &Change) {
        match c {
            Change::Prop { r, name, .. } => self.mark_prop(*r, *name),
            Change::Name { r, .. } => self.mark_prop(*r, ustr("Name")),
            Change::Parent { r, .. }
            | Change::Create { r, .. }
            | Change::Remove { r, .. }
            | Change::Enter { r, .. }
            | Change::Destroy { r } => self.mark_tree(*r),
        }
    }

    fn apply(&mut self, c: &Change, forward: bool) {
        self.apply_inner(c, forward);
        self.mark_change(c);
    }

    fn apply_inner(&mut self, c: &Change, forward: bool) {
        match c {
            Change::Prop { r, name, old, new } => {
                let v = if forward { new } else { old };
                if let Some(inst) = self.dom.get_by_ref_mut(*r) {
                    match v {
                        Some(v) => {
                            inst.properties.insert(*name, v.clone());
                        }
                        None => {
                            inst.properties.remove(name);
                        }
                    }
                }
            }
            Change::Name { r, old, new } => {
                if let Some(inst) = self.dom.get_by_ref_mut(*r) {
                    inst.name = if forward { new.clone() } else { old.clone() };
                }
            }
            Change::Parent { r, old, new } | Change::Enter { r, old, new } => {
                let mut to = if forward { *new } else { *old };
                // Undoing an arrival whose old (orphan) parent is gone leaves
                // the DataModel parentless: Brushtool brushes clones through a
                // temporary Model it destroys right after.
                if !forward && matches!(c, Change::Enter { .. }) && to.is_some() && !self.exists(to) {
                    to = Ref::none();
                }
                // (A script's instance can go back to no parent at all.)
                if self.exists(*r) && (self.exists(to) || to.is_none()) && !(to.is_some() && self.is_ancestor(*r, to)) {
                    self.dom.transfer_within(*r, to);
                }
            }
            Change::Destroy { .. } => {}
            Change::Create { r, parent } | Change::Remove { r, parent } => {
                let into_dom = matches!(c, Change::Create { .. }) == forward;
                if into_dom {
                    if self.limbo.get_by_ref(*r).is_some() && self.exists(*parent) {
                        self.limbo.transfer(*r, &mut self.dom, *parent);
                    } else if self.exists(*r) && self.exists(*parent) && !self.is_ancestor(*r, *parent) {
                        // (Lent back to the document, parentless, because a
                        // plugin still holds it: plugin_host/edit_sync.rs.)
                        self.dom.transfer_within(*r, *parent);
                    }
                } else if self.exists(*r) {
                    let lr = self.limbo.root_ref();
                    self.dom.transfer(*r, &mut self.limbo, lr);
                }
            }
        }
    }

    /// Records an already-applied command. Commands with the same merge key
    /// within a short window fold together (dragging a number, typing).
    fn push(&mut self, label: impl Into<String>, changes: Vec<Change>, merge_key: Option<String>) {
        if changes.is_empty() {
            return;
        }
        for c in &changes {
            self.mark_change(c);
        }
        // What scripts changed since their last waypoint joins the
        // person's step (Studio: Ctrl+Z after a plugin's change and a
        // Delete reverts both).
        let mut changes = changes;
        let pending = self.take_pending_for_step();
        if !pending.is_empty() {
            changes.splice(0..0, pending);
        }
        self.redo.clear();
        let now = Instant::now();
        if let (Some(key), Some(last)) = (&merge_key, self.undo.last_mut()) {
            if last.merge_key.as_ref() == Some(key)
                && now.duration_since(last.at) < Duration::from_millis(mech::UNDO_MERGE_MS)
            {
                for c in changes {
                    if let Change::Prop { r, name, new, .. } = &c {
                        if let Some(Change::Prop { new: n, .. }) = last.changes.iter_mut().find(|e| matches!(e, Change::Prop { r: r2, name: n2, .. } if r2 == r && n2 == name)) {
                            *n = new.clone();
                            continue;
                        }
                    }
                    last.changes.push(c);
                }
                last.at = now;
                return;
            }
        }
        let sel_before = self.selection.clone();
        self.undo.push(Command {
            label: label.into(),
            changes,
            merge_key,
            at: now,
            sel_before,
            sel_after: Vec::new(),
            display: None,
        });
        if self.undo.len() > mech::UNDO_LIMIT {
            self.undo.remove(0);
        }
    }

    /// How many undo steps there are.
    pub fn undo_len(&self) -> usize {
        self.undo.len()
    }

    pub fn can_undo(&self) -> Option<&str> {
        self.undo.last().map(|c| c.label.as_str())
    }

    pub fn can_redo(&self) -> Option<&str> {
        self.redo.last().map(|c| c.label.as_str())
    }

    pub fn undo(&mut self) -> Option<String> {
        self.history_undo_begin();
        let mut cmd = self.undo.pop()?;
        cmd.sel_after = self.selection.clone();
        for c in cmd.changes.iter().rev() {
            self.apply(c, false);
        }
        self.revision += 1;
        self.selection = cmd.sel_before.clone();
        self.prune_selection();
        let label = cmd.display.clone().unwrap_or_else(|| cmd.label.clone());
        self.history_after(true, &cmd);
        self.redo.push(cmd);
        Some(label)
    }

    pub fn redo(&mut self) -> Option<String> {
        let mut cmd = self.redo.pop()?;
        for c in cmd.changes.iter() {
            self.apply(c, true);
        }
        self.revision += 1;
        self.selection = cmd.sel_after.clone();
        self.prune_selection();
        let label = cmd.display.clone().unwrap_or_else(|| cmd.label.clone());
        cmd.merge_key = None;
        self.history_after(false, &cmd);
        self.undo.push(cmd);
        Some(label)
    }

    /// Forgets undo history and marks the document unchanged.
    pub fn clear_history(&mut self) {
        self.undo.clear();
        self.redo.clear();
        self.chs_reset();
        self.saved_revision = self.revision;
    }

    fn prune_selection(&mut self) {
        let sel = std::mem::take(&mut self.selection);
        self.selection = sel
            .into_iter()
            .filter(|r| self.dom.get_by_ref(*r).is_some())
            .collect();
    }

    // ---------------------------------------------------------------- edits

    /// Sets properties on many instances as one undo step.
    pub fn set_props(
        &mut self,
        label: &str,
        edits: Vec<(Ref, Ustr, Variant)>,
        merge_key: Option<String>,
    ) {
        let mut changes = Vec::new();
        let mut edits = edits;
        let twins: Vec<_> = edits
            .iter()
            .filter_map(|(r, name, value)| {
                let class = self.dom.get_by_ref(*r)?.class;
                screen_insets_twin(class.as_str(), name.as_str(), value)
                    .or_else(|| lighting_time_twin(class.as_str(), name.as_str(), value))
                    .map(|(n, v)| (*r, ustr(n), v))
            })
            .collect();
        edits.extend(twins);
        for (r, name, value) in edits {
            let Some(inst) = self.dom.get_by_ref_mut(r) else {
                continue;
            };
            let old = inst.properties.insert(name, value.clone());
            if old.as_ref() != Some(&value) {
                changes.push(Change::Prop {
                    r,
                    name,
                    old,
                    new: Some(value),
                });
            }
        }
        self.push(label, changes, merge_key);
    }

    /// Records property values that were already changed live (gizmo drags).
    pub fn record_props(
        &mut self,
        label: &str,
        changes: Vec<(Ref, Ustr, Option<Variant>, Variant)>,
    ) {
        let changes = changes
            .into_iter()
            .filter(|(_, _, old, new)| old.as_ref() != Some(new))
            .map(|(r, name, old, new)| Change::Prop {
                r,
                name,
                old,
                new: Some(new),
            })
            .collect();
        self.push(label, changes, None);
    }

    /// Records a drag as one step: property values already changed live and
    /// instances already inserted (`(instance, parent)`, e.g. Join Surfaces welds).
    pub fn record_drag(
        &mut self,
        label: &str,
        changes: Vec<(Ref, Ustr, Option<Variant>, Variant)>,
        created: Vec<(Ref, Ref)>,
    ) {
        let mut all: Vec<Change> = changes
            .into_iter()
            .filter(|(_, _, old, new)| old.as_ref() != Some(new))
            .map(|(r, name, old, new)| Change::Prop { r, name, old, new: Some(new) })
            .collect();
        all.extend(created.into_iter().map(|(r, parent)| Change::Create { r, parent }));
        self.push(label, all, None);
    }

    /// Changes a property without recording undo (live preview while dragging).
    pub fn set_prop_live(&mut self, r: Ref, name: Ustr, value: Variant) {
        if let Some(inst) = self.dom.get_by_ref_mut(r) {
            inst.properties.insert(name, value);
            self.mark_prop(r, name);
        }
    }

    pub fn rename(&mut self, r: Ref, name: &str) {
        let Some(inst) = self.dom.get_by_ref_mut(r) else {
            return;
        };
        if inst.name == name {
            return;
        }
        let old = std::mem::replace(&mut inst.name, name.to_string());
        self.push(
            format!("Rename to {name}"),
            vec![Change::Name {
                r,
                old,
                new: name.into(),
            }],
            None,
        );
    }

    /// Moves instances under a new parent. Refuses cycles, services and the root.
    pub fn reparent(&mut self, refs: &[Ref], new_parent: Ref) -> usize {
        if !self.exists(new_parent) {
            return 0;
        }
        let mut changes = Vec::new();
        for r in self.top_level(refs) {
            if r == new_parent || self.is_ancestor(r, new_parent) || !self.can_edit_structure(r) {
                continue;
            }
            let old = self
                .dom
                .get_by_ref(r)
                .map(|i| i.parent())
                .unwrap_or_else(Ref::none);
            if old == new_parent {
                continue;
            }
            self.dom.transfer_within(r, new_parent);
            changes.push(Change::Parent {
                r,
                old,
                new: new_parent,
            });
        }
        let n = changes.len();
        self.push("Reparent", changes, None);
        n
    }

    pub fn insert(&mut self, parent: Ref, builder: InstanceBuilder) -> Ref {
        let parent = if self.exists(parent) {
            parent
        } else {
            self.default_parent()
        };
        let r = self.dom.insert(parent, builder);
        let class = self.class_of(r).map(|c| c.to_string()).unwrap_or_default();
        self.push(
            format!("Insert {class}"),
            vec![Change::Create { r, parent }],
            None,
        );
        r
    }

    pub fn remove(&mut self, refs: &[Ref]) -> usize {
        let mut changes = Vec::new();
        for r in self.top_level(refs) {
            if !self.can_edit_structure(r) {
                continue;
            }
            let parent = self
                .dom
                .get_by_ref(r)
                .map(|i| i.parent())
                .unwrap_or_else(Ref::none);
            let lr = self.limbo.root_ref();
            self.dom.transfer(r, &mut self.limbo, lr);
            changes.push(Change::Remove { r, parent });
        }
        let n = changes.len();
        self.push("Delete", changes, None);
        self.prune_selection();
        n
    }

    /// Serialises instances into clipboard bytes (binary model format).
    pub fn copy(&self, refs: &[Ref]) -> Option<Vec<u8>> {
        let top: Vec<Ref> = self
            .top_level(refs)
            .into_iter()
            .filter(|r| self.can_edit_structure(*r))
            .collect();
        if top.is_empty() {
            return None;
        }
        self.encode(&top, false).ok()
    }

    /// Gives every script in these subtrees a new ScriptGuid, as Studio does
    /// for a paste, duplicate or insert (Studio parity; tools that track
    /// scripts by ScriptGuid need them unique).
    pub fn renew_script_guids(&mut self, roots: &[Ref]) {
        let mut targets = Vec::new();
        for &root in roots {
            for r in self.subtree(root) {
                if matches!(self.dom.get_by_ref(r).and_then(|i| i.properties.get(&Ustr::from("ScriptGuid"))), Some(Variant::String(g)) if !g.is_empty()) {
                    targets.push(r);
                }
            }
        }
        for r in targets {
            if let Some(i) = self.dom.get_by_ref_mut(r) {
                i.properties.insert("ScriptGuid".into(), Variant::String(new_script_guid()));
            }
        }
    }

    /// Inserts clipboard bytes under `parent`; returns the new top-level refs.
    pub fn paste(&mut self, bytes: &[u8], parent: Ref, label: &str) -> Vec<Ref> {
        let Ok(mut tmp) = Self::decode(bytes) else {
            return Vec::new();
        };
        let parent = if self.exists(parent) {
            parent
        } else {
            self.default_parent()
        };
        let roots = tmp.root().children().to_vec();
        let mut changes = Vec::new();
        for r in &roots {
            tmp.transfer(*r, &mut self.dom, parent);
            changes.push(Change::Create { r: *r, parent });
        }
        self.renew_script_guids(&roots);
        self.push(label, changes, None);
        roots
    }

    /// Inserts several groups of model bytes, each under its own parent,
    /// as one undo step (the Toolbox's insert: a Sky goes to Lighting, the
    /// rest to Workspace, one waypoint). Returns the new top-level refs.
    pub fn paste_groups(&mut self, groups: Vec<(Vec<u8>, Ref)>, label: &str) -> Vec<Ref> {
        let mut changes = Vec::new();
        let mut out = Vec::new();
        for (bytes, parent) in groups {
            let Ok(mut tmp) = Self::decode(&bytes) else { continue };
            let parent = if self.exists(parent) { parent } else { self.default_parent() };
            let roots = tmp.root().children().to_vec();
            for r in &roots {
                tmp.transfer(*r, &mut self.dom, parent);
                changes.push(Change::Create { r: *r, parent });
            }
            out.extend(roots);
        }
        self.renew_script_guids(&out);
        self.push(label, changes, None);
        out
    }

    /// Duplicates each instance next to the original.
    pub fn duplicate(&mut self, refs: &[Ref]) -> Vec<Ref> {
        let mut out = Vec::new();
        let mut changes = Vec::new();
        for r in self.top_level(refs) {
            if !self.can_edit_structure(r) {
                continue;
            }
            let parent = self
                .dom
                .get_by_ref(r)
                .map(|i| i.parent())
                .unwrap_or_else(Ref::none);
            let Ok(bytes) = self.encode(&[r], false) else {
                continue;
            };
            let Ok(mut tmp) = Self::decode(&bytes) else {
                continue;
            };
            for nr in tmp.root().children().to_vec() {
                tmp.transfer(nr, &mut self.dom, parent);
                changes.push(Change::Create { r: nr, parent });
                out.push(nr);
            }
        }
        self.renew_script_guids(&out);
        self.push("Duplicate", changes, None);
        out
    }

    /// Wraps the instances in a new Model under their first common parent.
    pub fn group(&mut self, refs: &[Ref]) -> Option<Ref> {
        self.group_as(refs, "Model")
    }

    /// Groups into a new `class` (Model, or Folder: Studio's Group as Folder).
    pub fn group_as(&mut self, refs: &[Ref], class: &str) -> Option<Ref> {
        let top: Vec<Ref> = self
            .top_level(refs)
            .into_iter()
            .filter(|r| self.can_edit_structure(*r))
            .collect();
        let first = *top.first()?;
        let parent = self.dom.get_by_ref(first)?.parent();
        let model = self
            .dom
            .insert(parent, InstanceBuilder::new(class).with_name(class));
        let mut changes = vec![Change::Create { r: model, parent }];
        for r in top {
            let old = self
                .dom
                .get_by_ref(r)
                .map(|i| i.parent())
                .unwrap_or_else(Ref::none);
            self.dom.transfer_within(r, model);
            changes.push(Change::Parent { r, old, new: model });
        }
        self.push("Group", changes, None);
        Some(model)
    }

    /// Moves a Model/Folder's children up and removes it.
    pub fn ungroup(&mut self, refs: &[Ref]) -> Vec<Ref> {
        let mut out = Vec::new();
        let mut changes = Vec::new();
        for r in self.top_level(refs) {
            let Some(class) = self.class_of(r) else {
                continue;
            };
            if !(class == "Model" || class == "Folder") || !self.can_edit_structure(r) {
                continue;
            }
            let parent = self
                .dom
                .get_by_ref(r)
                .map(|i| i.parent())
                .unwrap_or_else(Ref::none);
            for c in self
                .dom
                .get_by_ref(r)
                .map(|i| i.children().to_vec())
                .unwrap_or_default()
            {
                self.dom.transfer_within(c, parent);
                changes.push(Change::Parent {
                    r: c,
                    old: r,
                    new: parent,
                });
                out.push(c);
            }
            let lr = self.limbo.root_ref();
            self.dom.transfer(r, &mut self.limbo, lr);
            changes.push(Change::Remove { r, parent });
        }
        self.push("Ungroup", changes, None);
        out
    }

    /// Counts instances of a class (IsA) in the whole document.
    /// Sources of scripts whose full name contains `pattern`.
    /// (full name, source) of every script (development: --dump-scripts).
    pub fn all_script_sources(&self) -> Vec<(String, String)> {
        let mut out = Vec::new();
        for i in self.dom.descendants() {
            if !matches!(i.class.as_str(), "Script" | "LocalScript" | "ModuleScript") {
                continue;
            }
            let mut path = Vec::new();
            let mut cur = i.referent();
            while let Some(x) = self.dom.get_by_ref(cur) {
                if x.parent().is_none() {
                    break;
                }
                path.push(x.name.replace(['/', '\\'], "_"));
                cur = x.parent();
            }
            path.reverse();
            if let Some(Variant::String(src)) = i.properties.get(&ustr("Source")) {
                out.push((path.join("."), src.clone()));
            }
        }
        out
    }

    pub fn script_sources(&self, pattern: &str) -> String {
        let mut out = String::new();
        for i in self.dom.descendants() {
            if !matches!(i.class.as_str(), "Script" | "LocalScript" | "ModuleScript") {
                continue;
            }
            let mut path = Vec::new();
            let mut cur = i.referent();
            while let Some(x) = self.dom.get_by_ref(cur) {
                if x.parent().is_none() {
                    break;
                }
                path.push(x.name.clone());
                cur = x.parent();
            }
            path.reverse();
            let full = path.join(".");
            if !full.contains(pattern) {
                continue;
            }
            let src = match i.properties.get(&ustr("Source")) {
                Some(Variant::String(s)) => s.clone(),
                _ => String::new(),
            };
            out.push_str(&format!("==== {full} ({})\n", i.class));
            for (n, line) in src.lines().enumerate().take(60) {
                out.push_str(&format!("{:4} {line}\n", n + 1));
            }
        }
        out
    }

    /// A text summary of the place: instance counts by class and part flags.
    pub fn stats(&self) -> String {
        let mut by_class: std::collections::HashMap<String, usize> = Default::default();
        let (mut parts, mut anchored, mut collide_off, mut in_ws) = (0, 0, 0, 0);
        let ws = self.workspace();
        for i in self.dom.descendants() {
            *by_class.entry(i.class.to_string()).or_default() += 1;
            if is_a(&i.class, "BasePart") {
                parts += 1;
                if matches!(
                    self.prop(i.referent(), "Anchored"),
                    Some(Variant::Bool(true))
                ) {
                    anchored += 1;
                }
                if matches!(
                    self.prop(i.referent(), "CanCollide"),
                    Some(Variant::Bool(false))
                ) {
                    collide_off += 1;
                }
                if ws.is_some_and(|w| self.is_ancestor(w, i.referent())) {
                    in_ws += 1;
                }
            }
        }
        let mut v: Vec<_> = by_class.into_iter().collect();
        v.sort_by(|a, b| b.1.cmp(&a.1));
        let mut out = format!("parts {parts} (in Workspace {in_ws}, anchored {anchored}, CanCollide off {collide_off})\n");
        for (c, n) in v.iter().take(60) {
            out.push_str(&format!("{n:>8} {c}\n"));
        }
        out
    }

    pub fn count_class(&self, base: &str) -> usize {
        self.dom
            .descendants()
            .filter(|i| is_a(&i.class, base))
            .count()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn idle_queries_keep_spatial_marks_through_unrelated_journal_churn() {
        let mut doc = Document::new_place();
        let root = doc.dom.root_ref();
        let changed = Mark::Prop(root, ustr("Size"));
        assert!(matches!(doc.marks_for(Reader::Query), Marks::All));
        let _ = doc.marks_for(Reader::Physics);
        doc.mark_prop(root, ustr("Size"));
        for _ in 0..=mech::JOURNAL_READER_LAG {
            doc.mark_prop(root, ustr("Text"));
        }
        let _ = doc.marks_for(Reader::Physics);
        assert!(matches!(doc.marks_for(Reader::Query), Marks::Some(m) if m == [changed]));
        doc.mark_all();
        assert!(matches!(doc.marks_for(Reader::Query), Marks::All));
    }

    #[test]
    fn stylesheet_marks_survive_general_journal_consumption() {
        let mut doc = Document::new_place();
        let root = doc.dom.root_ref();
        assert!(matches!(doc.take_style_marks(), Marks::All));
        let _ = doc.marks_for(Reader::Physics);
        let tag = ustr("Tags");
        let interactable = ustr("Interactable");
        doc.mark_prop(root, tag);
        doc.mark_prop(root, interactable);
        let rev = doc.style_revision;
        for _ in 0..=mech::JOURNAL_LIMIT {
            doc.mark_prop(root, ustr("CFrame"));
        }
        let _ = doc.marks_for(Reader::Physics);
        assert_eq!(doc.style_revision, rev);
        assert!(matches!(
            doc.take_style_marks(),
            Marks::Some(m) if m == [Mark::Prop(root, tag), Mark::Prop(root, interactable)]
        ));
        doc.mark_all();
        assert!(matches!(doc.take_style_marks(), Marks::All));
    }

    /// ScreenGui.IgnoreGuiInset is ScreenInsets ≠ CoreUISafeInsets, both ways
    /// (measured in Studio 0.744 on Laser Tag's GameplayGui / ReticleGui).
    #[test]
    fn screen_gui_inset_is_screen_insets() {
        use rbx_dom_weak::types::Enum;
        let mut d = Document::new_place();
        let sg = d.find_child(d.dom.root_ref(), "StarterGui").unwrap();
        let fresh = d.insert(sg, InstanceBuilder::new("ScreenGui"));
        assert_eq!(d.prop(fresh, "ScreenInsets"), Some(Variant::Enum(Enum::from_u32(2))));
        assert_eq!(d.prop(fresh, "IgnoreGuiInset"), Some(Variant::Bool(false)));
        for (si, ignore) in [(0, true), (1, true), (2, false), (3, true)] {
            let g = d.insert(
                sg,
                InstanceBuilder::new("ScreenGui")
                    .with_property("ScreenInsets", Enum::from_u32(si))
                    .with_property("IgnoreGuiInset", !ignore),
            );
            assert_eq!(d.prop(g, "IgnoreGuiInset"), Some(Variant::Bool(ignore)), "ScreenInsets {si}");
        }
        let legacy = d.insert(sg, InstanceBuilder::new("ScreenGui").with_property("IgnoreGuiInset", true));
        assert_eq!(d.prop(legacy, "ScreenInsets"), Some(Variant::Enum(Enum::from_u32(1))));
        d.set_props("t", vec![(legacy, ustr("IgnoreGuiInset"), Variant::Bool(false))], None);
        assert_eq!(d.prop(legacy, "ScreenInsets"), Some(Variant::Enum(Enum::from_u32(2))));
        d.set_props("t", vec![(legacy, ustr("ScreenInsets"), Variant::Enum(Enum::from_u32(0)))], None);
        assert_eq!(d.prop(legacy, "IgnoreGuiInset"), Some(Variant::Bool(true)));
    }

    /// A Toolbox model (a Model with a Part) inserted as one step, as the
    /// Toolbox does: its roots and everything it made.
    fn toolbox_insert(d: &mut Document, label: &str) -> (Vec<Ref>, std::collections::HashSet<Ref>) {
        let ws = d.workspace().unwrap();
        let mut tmp = rbx_dom_weak::WeakDom::new(InstanceBuilder::new("DataModel"));
        let root = tmp.root_ref();
        let m = tmp.insert(root, InstanceBuilder::new("Model").with_name("Dragged"));
        tmp.insert(m, new_part("Inside", 1, glam::Vec3::new(0.0, 5.0, 0.0), glam::Vec3::ONE, [1, 2, 3]));
        let mut bytes = Vec::new();
        rbx_binary::to_writer(&mut bytes, &tmp, &[m]).unwrap();
        let roots = d.paste_groups(vec![(bytes, ws)], label);
        let mut made: std::collections::HashSet<Ref> = roots.iter().copied().collect();
        for r in &roots {
            made.extend(d.dom.descendants_of(*r).map(|i| i.referent()));
        }
        (roots, made)
    }

    /// A plugin's change (applied, then noted for the next waypoint).
    fn script_prop(d: &mut Document, r: Ref, name: &str, v: Variant) {
        let name = ustr(name);
        let old = d.dom.get_by_ref(r).and_then(|i| i.properties.get(&name).cloned());
        d.set_prop_live(r, name, v.clone());
        d.note_script_change(Change::Prop { r, name, old, new: Some(v) });
    }

    fn inside_of(d: &Document, made: &std::collections::HashSet<Ref>) -> Ref {
        *made.iter().find(|r| d.dom.get_by_ref(**r).is_some_and(|i| i.class == "Part")).unwrap()
    }

    /// Dragged back out: the insert is gone with no undo / redo step left,
    /// the step before it untouched; something never inserted isn't taken.
    #[test]
    fn retract_inserted_takes_back_only_the_insert() {
        let mut d = Document::new_place();
        let ws = d.workspace().unwrap();
        let first = d.insert(ws, new_part("Keep", 1, glam::Vec3::ZERO, glam::Vec3::ONE, [1, 2, 3]));
        let before = d.undo_len();
        let (roots, made) = toolbox_insert(&mut d, "After insert asset 5");
        assert!(!d.retract_inserted(&[Ref::none()], &[Ref::none()].into_iter().collect()), "not an insert");
        assert!(d.exists(first) && d.exists(roots[0]));
        assert!(d.retract_inserted(&roots, &made));
        assert!(!d.exists(roots[0]));
        assert!(d.exists(first));
        assert_eq!(d.undo_len(), before);
        assert_eq!(d.can_redo(), None);
        assert!(d.selection.iter().all(|r| !made.contains(r)));
    }

    /// A script edit of something else between the insert and the drag-out
    /// (and the plugin moving the dragged instance): the edit stays pending
    /// and becomes the next waypoint; the dragged instance's changes go.
    #[test]
    fn retract_inserted_keeps_an_independent_script_edit() {
        let mut d = Document::new_place();
        let ws = d.workspace().unwrap();
        let keep = d.insert(ws, new_part("Keep", 1, glam::Vec3::ZERO, glam::Vec3::ONE, [1, 2, 3]));
        let before = d.undo_len();
        let (roots, made) = toolbox_insert(&mut d, "After insert asset 5");
        let inside = inside_of(&d, &made);
        script_prop(&mut d, keep, "Transparency", Variant::Float32(0.5));
        script_prop(&mut d, inside, "Transparency", Variant::Float32(0.25));
        assert!(d.retract_inserted(&roots, &made));
        assert!(!d.exists(roots[0]) && !d.exists(inside));
        assert_eq!(d.prop(keep, "Transparency"), Some(Variant::Float32(0.5)));
        assert_eq!(d.undo_len(), before);
        d.chs_set_waypoint("Plugin edit");
        assert_eq!(d.undo_len(), before + 1);
        assert_eq!(d.can_undo(), Some("Plugin edit"));
        d.undo();
        assert_ne!(d.prop(keep, "Transparency"), Some(Variant::Float32(0.5)));
        assert!(!d.exists(roots[0]), "undo of the edit doesn't bring the insert back");
        d.redo();
        assert_eq!(d.prop(keep, "Transparency"), Some(Variant::Float32(0.5)));
        assert!(!d.exists(roots[0]));
    }

    /// A script edit pending when the card entered (swept into the insert's
    /// step) is pending again after the drag-out: not reverted, not lost.
    #[test]
    fn retract_inserted_returns_earlier_pending_edits() {
        let mut d = Document::new_place();
        let ws = d.workspace().unwrap();
        let keep = d.insert(ws, new_part("Keep", 1, glam::Vec3::ZERO, glam::Vec3::ONE, [1, 2, 3]));
        let before = d.undo_len();
        script_prop(&mut d, keep, "Transparency", Variant::Float32(0.5));
        let (roots, made) = toolbox_insert(&mut d, "After insert asset 5");
        assert!(d.retract_inserted(&roots, &made));
        assert_eq!(d.prop(keep, "Transparency"), Some(Variant::Float32(0.5)));
        assert_eq!(d.undo_len(), before);
        d.chs_set_waypoint("Plugin edit");
        assert_eq!(d.undo_len(), before + 1);
        d.undo();
        assert_ne!(d.prop(keep, "Transparency"), Some(Variant::Float32(0.5)));
        assert!(!d.exists(roots[0]));
    }

    /// An undo step made on top of the insert mid-drag (the person's edit,
    /// a plugin's waypoint) stays and still undoes / redoes; the insert
    /// beneath it goes and never comes back.
    #[test]
    fn retract_inserted_under_another_step() {
        let mut d = Document::new_place();
        let ws = d.workspace().unwrap();
        let keep = d.insert(ws, new_part("Keep", 1, glam::Vec3::ZERO, glam::Vec3::ONE, [1, 2, 3]));
        let before = d.undo_len();
        let (roots, made) = toolbox_insert(&mut d, "After insert asset 5");
        let inside = inside_of(&d, &made);
        d.set_props("Transparency", vec![(keep, ustr("Transparency"), Variant::Float32(0.5))], None);
        script_prop(&mut d, inside, "Reflectance", Variant::Float32(0.9));
        script_prop(&mut d, keep, "Reflectance", Variant::Float32(0.3));
        d.chs_set_waypoint("Plugin step");
        assert_eq!(d.undo_len(), before + 3);
        assert!(d.retract_inserted(&roots, &made));
        assert!(!d.exists(roots[0]));
        assert_eq!(d.undo_len(), before + 2, "only the insert's step goes");
        assert_eq!(d.can_undo(), Some("Plugin step"));
        assert_eq!(d.prop(keep, "Transparency"), Some(Variant::Float32(0.5)));
        d.undo();
        assert_ne!(d.prop(keep, "Reflectance"), Some(Variant::Float32(0.3)));
        d.undo();
        assert_ne!(d.prop(keep, "Transparency"), Some(Variant::Float32(0.5)));
        assert!(!d.exists(roots[0]));
        assert_eq!(d.undo_len(), before);
        d.redo();
        d.redo();
        assert_eq!(d.prop(keep, "Transparency"), Some(Variant::Float32(0.5)));
        assert_eq!(d.prop(keep, "Reflectance"), Some(Variant::Float32(0.3)));
        assert!(!d.exists(roots[0]) && !d.exists(inside));
        assert_eq!(d.can_redo(), None);
    }

    #[test]
    fn commit_pending_script_changes_isolates_manual_delete_and_preserves_redo() {
        let mut d = Document::new_place();
        let ws = d.workspace().unwrap();
        let a = d.insert(ws, new_part("A", 1, glam::Vec3::ZERO, glam::Vec3::ONE, [1, 2, 3]));
        let b = d.insert(ws, new_part("B", 1, glam::Vec3::ZERO, glam::Vec3::ONE, [1, 2, 3]));
        let before = d.undo_len();

        // Simulate a script (e.g. MCP run_luau) mutating A without SetWaypoint.
        script_prop(&mut d, a, "Transparency", Variant::Float32(0.5));
        assert!(d.commit_pending_script_changes("Run Luau (MCP)"));
        assert_eq!(d.undo_len(), before + 1);
        assert_eq!(d.can_undo(), Some("Run Luau (MCP)"));

        // Deleting B next must create its own step without absorbing the script edit.
        assert_eq!(d.remove(&[b]), 1);
        assert!(!d.exists(b));
        assert_eq!(d.can_undo(), Some("Delete"));

        // First undo reverts only the Delete of B; A's scripted edit remains.
        assert_eq!(d.undo().as_deref(), Some("Delete"));
        assert!(d.exists(b));
        assert_eq!(d.prop(a, "Transparency"), Some(Variant::Float32(0.5)));
        assert_eq!(d.can_undo(), Some("Run Luau (MCP)"));
        assert_eq!(d.can_redo(), Some("Delete"));

        // A no-op commit (read-only run_luau) does not clear the redo stack.
        assert!(!d.commit_pending_script_changes("Run Luau (MCP)"));
        assert_eq!(d.can_redo(), Some("Delete"));

        // Second undo cleanly reverts the scripted edit.
        assert_eq!(d.undo().as_deref(), Some("Run Luau (MCP)"));
        assert_ne!(d.prop(a, "Transparency"), Some(Variant::Float32(0.5)));

        // An open CHS recording is left alone.
        let rec = d.chs_begin_recording("CustomRec", None).expect("recording");
        script_prop(&mut d, a, "Transparency", Variant::Float32(0.25));
        assert!(!d.commit_pending_script_changes("Run Luau (MCP)"));
        d.chs_finish_recording(&rec, FinishOp::Commit).expect("commit");
        assert_eq!(d.can_undo(), Some("CustomRec"));
    }

    /// Ctrl+Z mid-drag took the insert to the redo stack: the drag-out
    /// clears it there, so a redo can't put the leaked instance back.
    #[test]
    fn retract_inserted_after_its_undo() {
        let mut d = Document::new_place();
        let ws = d.workspace().unwrap();
        d.insert(ws, new_part("Keep", 1, glam::Vec3::ZERO, glam::Vec3::ONE, [1, 2, 3]));
        let before = d.undo_len();
        let (roots, made) = toolbox_insert(&mut d, "After insert asset 5");
        d.undo();
        assert!(!d.exists(roots[0]));
        assert!(d.can_redo().is_some());
        assert!(d.retract_inserted(&roots, &made));
        assert_eq!(d.can_redo(), None);
        assert!(d.redo().is_none());
        assert!(!d.exists(roots[0]));
        assert_eq!(d.undo_len(), before);
    }

    /// Something a script moved into the dragged model mid-drag isn't taken
    /// with it: it goes back where it was.
    #[test]
    fn retract_inserted_leaves_a_foreign_child() {
        let mut d = Document::new_place();
        let ws = d.workspace().unwrap();
        let keep = d.insert(ws, new_part("Keep", 1, glam::Vec3::ZERO, glam::Vec3::ONE, [1, 2, 3]));
        let (roots, made) = toolbox_insert(&mut d, "After insert asset 5");
        d.dom.transfer_within(keep, roots[0]);
        d.note_script_change(Change::Parent { r: keep, old: ws, new: roots[0] });
        assert!(d.retract_inserted(&roots, &made));
        assert!(d.exists(keep));
        assert_eq!(d.dom.get_by_ref(keep).unwrap().parent(), ws);
    }

    fn parent_of(d: &Document, r: Ref) -> Option<Ref> {
        d.dom.get_by_ref(r).map(|i| i.parent())
    }

    fn in_place(d: &Document, r: Ref) -> bool {
        d.is_ancestor(d.dom.root_ref(), r)
    }

    /// The finding: a plugin moves an inserted child out of the dragged
    /// model (to Workspace, during DragEnter) and moves a foreign model
    /// (with a part nested in it) into that child. The drag-out takes the
    /// model and the child out of the DataModel; the foreign model survives
    /// back where it was, its part still in it; no undo or redo brings the
    /// model or the child back or puts the foreign model into them, also
    /// while a plugin still holds them (lent back to the DOM parentless, as
    /// plugin_host/edit_sync.rs does); unrelated steps still undo and redo.
    #[test]
    fn retract_inserted_takes_a_moved_out_child() {
        let mut d = Document::new_place();
        let ws = d.workspace().unwrap();
        let keep = d.insert(ws, new_part("Keep", 1, glam::Vec3::ZERO, glam::Vec3::ONE, [1, 2, 3]));
        let home = d.insert(ws, InstanceBuilder::new("Folder").with_name("Home"));
        let foreign = d.insert(home, InstanceBuilder::new("Model").with_name("Foreign"));
        let nested = d.insert(foreign, new_part("Nested", 1, glam::Vec3::ZERO, glam::Vec3::ONE, [1, 2, 3]));
        let before = d.undo_len();
        let (roots, made) = toolbox_insert(&mut d, "After insert asset 5");
        let m = roots[0];
        let child = inside_of(&d, &made);
        // DragEnter: the plugin takes the child out to Workspace, puts the
        // foreign model under it and edits something else: one waypoint.
        d.dom.transfer_within(child, ws);
        d.note_script_change(Change::Parent { r: child, old: m, new: ws });
        d.dom.transfer_within(foreign, child);
        d.note_script_change(Change::Parent { r: foreign, old: home, new: child });
        script_prop(&mut d, keep, "Transparency", Variant::Float32(0.5));
        d.chs_set_waypoint("Plugin drag");
        // the person's unrelated edit on top
        d.set_props("Reflectance", vec![(keep, ustr("Reflectance"), Variant::Float32(0.3))], None);
        assert_eq!(d.undo_len(), before + 3);

        assert!(d.retract_inserted(&roots, &made));
        for r in &made {
            assert!(d.dom.get_by_ref(*r).is_none(), "every inserted instance left the DOM, the moved-out child too");
        }
        assert_eq!(parent_of(&d, foreign), Some(home), "the foreign model goes home");
        assert_eq!(parent_of(&d, nested), Some(foreign), "with its part");
        assert_eq!(d.prop(keep, "Transparency"), Some(Variant::Float32(0.5)));
        assert_eq!(d.prop(keep, "Reflectance"), Some(Variant::Float32(0.3)));
        assert_eq!(d.undo_len(), before + 2, "only the insert's step goes");

        // A plugin still holding them: lent back, parentless.
        assert!(d.unlimbo(m) && d.unlimbo(child));
        let check = |d: &Document, what: &str| {
            assert!(!in_place(d, m) && !in_place(d, child), "{what}: the insert stays out");
            assert!(d.dom.get_by_ref(child).unwrap().children().is_empty(), "{what}: nothing goes into the child");
            assert!(in_place(d, foreign), "{what}: the foreign model stays in the place");
            assert_eq!(parent_of(d, foreign), Some(home), "{what}");
            assert_eq!(parent_of(d, nested), Some(foreign), "{what}");
        };
        check(&d, "after the drag-out");
        assert_eq!(d.can_undo(), Some("Reflectance"));
        d.undo();
        assert_ne!(d.prop(keep, "Reflectance"), Some(Variant::Float32(0.3)));
        check(&d, "undo of the person's edit");
        assert_eq!(d.can_undo(), Some("Plugin drag"));
        d.undo();
        assert_ne!(d.prop(keep, "Transparency"), Some(Variant::Float32(0.5)));
        check(&d, "undo of the plugin's step");
        assert_eq!(d.undo_len(), before);
        d.redo();
        assert_eq!(d.prop(keep, "Transparency"), Some(Variant::Float32(0.5)));
        check(&d, "redo of the plugin's step");
        d.redo();
        assert_eq!(d.prop(keep, "Reflectance"), Some(Variant::Float32(0.3)));
        check(&d, "redo of the person's edit");
        assert_eq!(d.can_redo(), None);
        // (all the way back, the test's own setup steps too)
        let out = |d: &Document, what: &str| {
            assert!(!in_place(d, m) && !in_place(d, child), "{what}: the insert stays out");
            assert!(d.dom.get_by_ref(child).unwrap().children().is_empty(), "{what}");
            assert!(d.dom.get_by_ref(m).unwrap().children().is_empty(), "{what}");
        };
        while d.undo().is_some() {
            out(&d, "undo all");
        }
        while d.redo().is_some() {
            out(&d, "redo all");
        }
        check(&d, "after redo all");
        assert_eq!(d.prop(keep, "Reflectance"), Some(Variant::Float32(0.3)));
    }

    /// The same left pending (no waypoint yet) and nobody holding the
    /// instances: they stay in limbo, and the foreign move through the
    /// child is no move at all.
    #[test]
    fn retract_inserted_takes_a_moved_out_child_pending() {
        let mut d = Document::new_place();
        let ws = d.workspace().unwrap();
        let home = d.insert(ws, InstanceBuilder::new("Folder").with_name("Home"));
        let other = d.insert(ws, InstanceBuilder::new("Folder").with_name("Other"));
        let foreign = d.insert(home, InstanceBuilder::new("Model").with_name("Foreign"));
        let before = d.undo_len();
        let (roots, made) = toolbox_insert(&mut d, "After insert asset 5");
        let child = inside_of(&d, &made);
        d.dom.transfer_within(child, other);
        d.note_script_change(Change::Parent { r: child, old: roots[0], new: other });
        d.dom.transfer_within(foreign, child);
        d.note_script_change(Change::Parent { r: foreign, old: home, new: child });
        assert!(d.retract_inserted(&roots, &made));
        assert!(made.iter().all(|r| d.dom.get_by_ref(*r).is_none()));
        assert_eq!(parent_of(&d, foreign), Some(home));
        d.chs_set_waypoint("Nothing left");
        assert_eq!(d.undo_len(), before, "no step: nothing of it is left to record");
        while d.undo().is_some() {}
        while d.redo().is_some() {}
        assert!(made.iter().all(|r| d.dom.get_by_ref(*r).is_none()));
        assert_eq!(parent_of(&d, foreign), Some(home));
    }

    /// A foreign part moved into the dragged model and on out to B (two
    /// steps): after the drag-out its history reads A to B, so undo and redo
    /// never put it into the removed model, even one a plugin still holds.
    #[test]
    fn retract_inserted_rewrites_foreign_moves_through_it() {
        let mut d = Document::new_place();
        let ws = d.workspace().unwrap();
        let a = d.insert(ws, InstanceBuilder::new("Folder").with_name("A"));
        let b = d.insert(ws, InstanceBuilder::new("Folder").with_name("B"));
        let f = d.insert(a, new_part("Foreign", 1, glam::Vec3::ZERO, glam::Vec3::ONE, [1, 2, 3]));
        let (roots, made) = toolbox_insert(&mut d, "After insert asset 5");
        let m = roots[0];
        d.dom.transfer_within(f, m);
        d.note_script_change(Change::Parent { r: f, old: a, new: m });
        d.chs_set_waypoint("Into the model");
        d.dom.transfer_within(f, b);
        d.note_script_change(Change::Parent { r: f, old: m, new: b });
        d.chs_set_waypoint("Out to B");
        assert!(d.retract_inserted(&roots, &made));
        assert!(d.unlimbo(m), "a plugin still holds the model");
        assert_eq!(parent_of(&d, f), Some(b));
        assert_eq!(d.can_undo(), Some("Out to B"));
        d.undo();
        assert_eq!(parent_of(&d, f), Some(a), "undo: back to A, not into the model");
        assert_ne!(d.can_undo(), Some("Into the model"), "that step moved only into the model: gone");
        d.redo();
        assert_eq!(parent_of(&d, f), Some(b));
        while d.undo().is_some() {
            assert!(!in_place(&d, m) && parent_of(&d, f) != Some(m));
        }
        while d.redo().is_some() {
            assert!(!in_place(&d, m) && parent_of(&d, f) != Some(m));
        }
        assert_eq!(parent_of(&d, f), Some(b));
    }

    /// Entering, leaving, entering again: each drag-out takes back only its
    /// own insert; a final drop stays as one undoable step.
    #[test]
    fn retract_inserted_repeated_reentry() {
        let mut d = Document::new_place();
        let before = d.undo_len();
        for n in 0..3 {
            let (roots, made) = toolbox_insert(&mut d, "After insert asset 5");
            assert_eq!(d.undo_len(), before + 1, "entry {n}");
            assert!(d.retract_inserted(&roots, &made));
            assert!(!d.exists(roots[0]));
            assert_eq!(d.undo_len(), before);
            assert_eq!(d.can_redo(), None);
        }
        let (roots, _) = toolbox_insert(&mut d, "After insert asset 5");
        assert_eq!(d.undo_len(), before + 1);
        d.undo();
        assert!(!d.exists(roots[0]));
        d.redo();
        assert!(d.exists(roots[0]));
    }

    #[test]
    fn undo_redo_insert_delete() {
        let mut d = Document::new_place();
        let ws = d.workspace().unwrap();
        let n0 = d.count_class("BasePart");
        let p = d.insert(
            ws,
            new_part("P", 1, glam::Vec3::ZERO, glam::Vec3::ONE, [1, 2, 3]),
        );
        assert_eq!(d.count_class("BasePart"), n0 + 1);
        d.remove(&[p]);
        assert_eq!(d.count_class("BasePart"), n0);
        d.undo();
        assert!(d.exists(p));
        d.undo();
        assert!(!d.exists(p));
        d.redo();
        assert!(d.exists(p));
        let dup = d.duplicate(&[p]);
        assert_eq!(dup.len(), 1);
        let bytes = d.encode_all(false).unwrap();
        let back = Document::from_place_bytes(&bytes, "t".into()).unwrap();
        assert_eq!(back.count_class("BasePart"), d.count_class("BasePart"));
        let xml = d.encode_all(true).unwrap();
        let back = Document::from_place_bytes(&xml, "t".into()).unwrap();
        assert_eq!(back.count_class("BasePart"), d.count_class("BasePart"));
    }
}

/// ScreenGui.IgnoreGuiInset is ScreenInsets seen as a bool in Studio 0.744
/// (Studio, Laser Tag template): IgnoreGuiInset = ScreenInsets ≠
/// CoreUISafeInsets (2); a new ScreenGui is CoreUISafeInsets. Files save
/// ScreenInsets (older ones only IgnoreGuiInset), so a saved ScreenInsets wins.
fn screen_insets_read(inst: &rbx_dom_weak::Instance, name: &str) -> Variant {
    use rbx_dom_weak::types::Enum;
    let si = match inst.properties.get(&ustr("ScreenInsets")) {
        Some(Variant::Enum(e)) => e.to_u32(),
        _ => match inst.properties.get(&ustr("IgnoreGuiInset")) {
            Some(Variant::Bool(true)) => 1,
            _ => 2,
        },
    };
    if name == "IgnoreGuiInset" {
        Variant::Bool(si != 2)
    } else {
        Variant::Enum(Enum::from_u32(si))
    }
}

/// The other half of a ScreenGui inset write: IgnoreGuiInset true/false sets
/// ScreenInsets DeviceSafeInsets / CoreUISafeInsets, and ScreenInsets sets
/// IgnoreGuiInset (measured in Studio: assignments either way stay in step).
pub fn screen_insets_twin(class: &str, name: &str, value: &Variant) -> Option<(&'static str, Variant)> {
    use rbx_dom_weak::types::Enum;
    if class != "ScreenGui" {
        return None;
    }
    match (name, value) {
        ("IgnoreGuiInset", Variant::Bool(b)) => {
            Some(("ScreenInsets", Variant::Enum(Enum::from_u32(if *b { 1 } else { 2 }))))
        }
        ("ScreenInsets", Variant::Enum(e)) => Some(("IgnoreGuiInset", Variant::Bool(e.to_u32() != 2))),
        _ => None,
    }
}

/// Lighting.ClockTime and TimeOfDay are one value in Studio (the time of day
/// in whole seconds; files and Team Create carry only TimeOfDay): setting
/// either sets the other.
pub fn lighting_time_twin(class: &str, name: &str, value: &Variant) -> Option<(&'static str, Variant)> {
    if class != "Lighting" {
        return None;
    }
    match (name, value) {
        ("ClockTime", Variant::Float32(h)) => Some(("TimeOfDay", Variant::String(crate::mech::format_time_of_day(*h)))),
        ("ClockTime", Variant::Float64(h)) => Some(("TimeOfDay", Variant::String(crate::mech::format_time_of_day(*h as f32)))),
        ("TimeOfDay", Variant::String(s)) => crate::mech::parse_time_of_day(s).map(|h| ("ClockTime", Variant::Float32(crate::mech::clock_time(h)))),
        _ => None,
    }
}

/// The Content twin newer files keep a legacy asset-id property in
/// (MeshId → MeshContent …). Roblox treats the pair as one property: setting
/// either changes both.
pub fn content_twin(name: &str) -> Option<&'static str> {
    Some(match name {
        "MeshId" => "MeshContent",
        "TextureID" | "TextureId" | "Texture" => "TextureContent",
        "ShirtTemplate" => "ShirtTemplateContent",
        "PantsTemplate" => "PantsTemplateContent",
        "Graphic" => "GraphicContent",
        "Image" => "ImageContent",
        "ColorMap" => "ColorMapContent",
        "NormalMap" => "NormalMapContent",
        "MetalnessMap" => "MetalnessMapContent",
        "RoughnessMap" => "RoughnessMapContent",
        "SoundId" => "AudioContent",
        // AudioPlayer (the only class with Asset): files keep AudioContent,
        // scripts read Asset ("rbxassetid://…" in Studio, not "").
        "Asset" => "AudioContent",
        "AnimationId" => "AnimationContent",
        // Sky (2026-11-05: Place B saves only these; rustudio
        // drew the default sky instead of its own).
        "SkyboxBk" => "SkyboxBackContent",
        "SkyboxDn" => "SkyboxDownContent",
        "SkyboxFt" => "SkyboxFrontContent",
        "SkyboxLf" => "SkyboxLeftContent",
        "SkyboxRt" => "SkyboxRightContent",
        "SkyboxUp" => "SkyboxUpContent",
        "SunTextureId" => "SunTextureContent",
        "MoonTextureId" => "MoonTextureContent",
        _ => return None,
    })
}

/// The legacy names whose Content twin is `twin` (TextureContent → Texture,
/// TextureID, TextureId).
pub fn content_legacy_names(twin: &str) -> Vec<&'static str> {
    const LEGACY: [&str; 23] = [
        "MeshId", "TextureID", "TextureId", "Texture", "ShirtTemplate", "PantsTemplate", "Graphic",
        "Image", "ColorMap", "NormalMap", "MetalnessMap", "RoughnessMap", "SoundId", "Asset", "AnimationId",
        "SkyboxBk", "SkyboxDn", "SkyboxFt", "SkyboxLf", "SkyboxRt", "SkyboxUp", "SunTextureId",
        "MoonTextureId",
    ];
    LEGACY.into_iter().filter(|l| content_twin(l) == Some(twin)).collect()
}

/// The instances of some classes, kept current from a document's change
/// journal instead of walking the whole DataModel: a full scan only the first
/// time (or when the reader fell behind), then just the subtrees that were
/// added, moved or removed. Full scans list in DOM order; later finds are
/// appended.
pub struct ClassIndex {
    reader: Reader,
    doc_id: u64,
    list: Vec<Ref>,
    set: std::collections::HashSet<Ref>,
}

impl ClassIndex {
    pub fn new(reader: Reader) -> Self {
        ClassIndex { reader, doc_id: 0, list: Vec::new(), set: Default::default() }
    }

    pub fn list(&self) -> &[Ref] {
        &self.list
    }

    /// Brings the index up to date with `d`; `want` picks classes. Returns
    /// whether the membership changed.
    pub fn update(&mut self, d: &mut Document, want: impl Fn(&str) -> bool) -> bool {
        let marks = d.marks_for(self.reader);
        let marks = match marks {
            Marks::Some(m) if self.doc_id == d.id => m,
            _ => {
                let list: Vec<Ref> = d
                    .dom
                    .descendants()
                    .filter(|i| want(i.class.as_str()))
                    .map(|i| i.referent())
                    .collect();
                let changed = list != self.list || self.doc_id != d.id;
                self.doc_id = d.id;
                self.set = list.iter().copied().collect();
                self.list = list;
                return changed;
            }
        };
        let mut changed = false;
        let mut removed = false;
        let mut tree = false;
        for m in marks {
            let Mark::Tree(t) = m else { continue };
            tree = true;
            if d.dom.get_by_ref(t).is_none() {
                // Limbo and freed instances are pruned from the live list below.
                continue;
            }
            for instance in d.dom.descendants_of(t) {
                let r = instance.referent();
                if want(instance.class.as_str()) {
                    if self.set.insert(r) {
                        self.list.push(r);
                        changed = true;
                    }
                } else if self.set.remove(&r) {
                    removed = true;
                }
            }
        }
        // Instances freed outright (no longer in the DOM or in limbo) don't
        // come back through `subtree`: drop whatever has left the DOM.
        if removed || tree {
            let before = self.list.len();
            self.list.retain(|r| self.set.contains(r) && d.dom.get_by_ref(*r).is_some());
            if removed || self.list.len() != before {
                self.set = self.list.iter().copied().collect();
                changed = true;
            }
        }
        changed
    }
}


#[cfg(test)]
mod surface_default_tests {
    use rbx_dom_weak::{types::Variant, InstanceBuilder};

    #[test]
    fn new_parts_have_studios_constructor_surfaces() {
        let mut d = super::Document::empty();
        let root = d.dom.root_ref();
        let e = |v| Some(Variant::Enum(rbx_dom_weak::types::Enum::from_u32(v)));
        // Instance.new in Studio (measured in Studio):
        // Part a Studs top and an Inlet bottom, WedgePart an Inlet bottom,
        // the rest smooth.
        for (class, top, bottom) in [("Part", 3, 4), ("SpawnLocation", 3, 4), ("WedgePart", 0, 4), ("CornerWedgePart", 0, 0), ("MeshPart", 0, 0)] {
            let r = d.dom.insert(root, InstanceBuilder::new(class));
            assert_eq!(d.prop(r, "TopSurface"), e(top), "{class}.TopSurface");
            assert_eq!(d.prop(r, "BottomSurface"), e(bottom), "{class}.BottomSurface");
            assert_eq!(d.prop(r, "FrontSurface"), e(0), "{class}.FrontSurface");
        }
        // Insert Part's are smooth.
        let r = d.dom.insert(root, super::new_part("Part", 1, glam::Vec3::ZERO, glam::Vec3::ONE, [1, 2, 3]));
        assert_eq!(d.prop(r, "TopSurface"), e(0));
        assert_eq!(d.prop(r, "BottomSurface"), e(0));
        // A saved Studs surface still shows.
        let r = d.dom.insert(root, InstanceBuilder::new("Part").with_property("TopSurface", Variant::Enum(rbx_dom_weak::types::Enum::from_u32(3))));
        assert_eq!(d.prop(r, "TopSurface"), Some(Variant::Enum(rbx_dom_weak::types::Enum::from_u32(3))));
    }
}

#[cfg(test)]
mod lighting_time_tests {
    use super::*;

    #[test]
    fn clock_time_and_time_of_day_stay_one_value() {
        let mut d = Document::empty();
        d.ensure_services();
        let l = d.find_child(d.dom.root_ref(), "Lighting").unwrap();
        d.set_props("t", vec![(l, ustr("ClockTime"), Variant::Float32(20.3))], None);
        assert_eq!(d.prop(l, "TimeOfDay"), Some(Variant::String("20:18:00".into())));
        d.set_props("t", vec![(l, ustr("TimeOfDay"), Variant::String("02:48:00".into()))], None);
        assert_eq!(d.prop(l, "ClockTime"), Some(Variant::Float32(2.8)));
        // Clamped and cut to whole seconds, as Studio stores it.
        d.set_props("t", vec![(l, ustr("ClockTime"), Variant::Float32(30.0))], None);
        assert_eq!(d.prop(l, "TimeOfDay"), Some(Variant::String("24:00:00".into())));
        assert_eq!(lighting_time_twin("Part", "ClockTime", &Variant::Float32(1.0)), None);
        assert_eq!(crate::mech::parse_time_of_day("07:45:30"), Some(7.0 + 45.0 / 60.0 + 30.0 / 3600.0));
    }
}

#[cfg(test)]
mod studio_defaults_tests {
    use super::*;

    fn lighting(attrs: Option<bool>, tech: Option<u32>) -> (Document, Ref) {
        let mut d = Document::empty();
        let mut b = InstanceBuilder::new("Lighting").with_name("Lighting");
        if let Some(t) = tech {
            b = b.with_property("Technology", Variant::Enum(rbx_dom_weak::types::Enum::from_u32(t)));
        }
        if let Some(m) = attrs {
            let mut a = rbx_dom_weak::types::Attributes::new();
            a.insert(LIGHTING_MIGRATED_ATTRIBUTE.into(), Variant::Bool(m));
            b = b.with_property("Attributes", Variant::Attributes(a));
        }
        let root = d.dom.root_ref();
        let l = d.dom.insert(root, b);
        (d, l)
    }

    #[test]
    fn lighting_style_follows_studio_migration_and_constructor() {
        let style = |d: &Document, l| d.prop(l, "LightingStyle");
        let quality = |d: &Document, l| d.prop(l, "PrioritizeLightingQuality");
        let e = |v| Some(Variant::Enum(rbx_dom_weak::types::Enum::from_u32(v)));
        // Migrated (a Team Create server's Lighting): the constructor's
        // Realistic / true whatever Technology says.
        for tech in [None, Some(1), Some(3), Some(4)] {
            let (d, l) = lighting(Some(true), tech);
            assert_eq!(style(&d, l), e(0), "{tech:?}");
            assert_eq!(quality(&d, l), Some(Variant::Bool(true)));
        }
        // Not migrated: derived from Technology.
        for (tech, s, q) in [(Some(4), 0, true), (Some(3), 1, true), (Some(1), 1, false), (None, 1, false), (Some(0), 1, false)] {
            let (d, l) = lighting(None, tech);
            assert_eq!((style(&d, l), quality(&d, l)), (e(s), Some(Variant::Bool(q))), "{tech:?}");
        }
    }

    #[test]
    fn content_twins_fall_back_to_the_constructor_textures() {
        let mut d = Document::empty();
        let root = d.dom.root_ref();
        let sky = d.dom.insert(root, InstanceBuilder::new("Sky"));
        let id = |s: &str| Some(Variant::ContentId(rbx_dom_weak::types::ContentId::from(s.to_string())));
        assert_eq!(d.prop(sky, "MoonTextureId"), id("rbxasset://sky/moon.jpg"));
        assert_eq!(d.prop(sky, "SunTextureId"), id("rbxasset://sky/sun.jpg"));
        // Sent empty (Team Create: SunTextureContent ""), as a string or as
        // the empty Content the live model stores: empty.
        d.dom.get_by_ref_mut(sky).unwrap().properties.insert(ustr("SunTextureContent"), Variant::String(String::new()));
        assert_eq!(d.prop(sky, "SunTextureId"), id(""));
        d.dom.get_by_ref_mut(sky).unwrap().properties.insert(ustr("SunTextureContent"), Variant::Content(rbx_dom_weak::types::Content::none()));
        assert_eq!(d.prop(sky, "SunTextureId"), id(""));
        assert_eq!(d.prop(sky, "MoonTextureId"), id("rbxasset://sky/moon.jpg"), "the omitted moon keeps its default");
        assert_eq!(default_instance_name("DepthOfFieldEffect"), "DepthOfField");
        assert_eq!(default_instance_name("Atmosphere"), "Atmosphere");
    }
}

/// A new ScriptGuid in Studio's form (`{8-4-4-4-12}` upper-case hex, v4).
pub fn new_script_guid() -> String {
    use std::hash::{BuildHasher, Hasher};
    use std::sync::atomic::{AtomicU64, Ordering};
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let mut word = || {
        let mut h = std::collections::hash_map::RandomState::new().build_hasher();
        h.write_u64(COUNTER.fetch_add(1, Ordering::Relaxed));
        h.write_u128(std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_nanos()));
        h.finish()
    };
    let mut b = [0u8; 16];
    b[..8].copy_from_slice(&word().to_le_bytes());
    b[8..].copy_from_slice(&word().to_le_bytes());
    b[6] = (b[6] & 0x0f) | 0x40;
    b[8] = (b[8] & 0x3f) | 0x80;
    let h: String = b.iter().map(|x| format!("{x:02X}")).collect();
    format!("{{{}-{}-{}-{}-{}}}", &h[0..8], &h[8..12], &h[12..16], &h[16..20], &h[20..32])
}
