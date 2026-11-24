//! Deferred (lazy) loading manifests for Roblox Studio's built-in plugins.
//!
//! Real Studio 0.748 skips 9 internal-only built-ins via
//! `FStringExternalUserPluginLoadDenyList` and defers heavy plugin startup via
//! `PluginLoaderBuilder:waitForUserInteraction()` and `PluginManifestV0` /
//! `PreInitHandoff` (deserializing the full plugin only on first interaction).
//!
//! In rustudio we never drop any of the 58 built-in plugins; instead, plugins
//! that only activate on user interaction (toolbar button, PluginAction, dock
//! widget, selection of a specific class, or Studio service signal) register
//! lightweight ribbon/action metadata at startup with zero `.rbxm` disk I/O,
//! zero `WeakDom` instances, and zero Luau heap until first used.
//!
//! Set `RUSTUDIO_BUILTIN_PLUGINS_EAGER=1` (or pass an explicit plugin filter in
//! `RUSTUDIO_BUILTIN_PLUGINS=<Name1,Name2>`) to load matching built-ins eagerly.

use std::path::Path;

use rbx_dom_weak::types::Ref;

use super::api::toolbar::{button_label, RibbonButton, RibbonGroup};
use super::ui_ribbon::ActionRow;
use super::PluginId;

/// Static metadata for one ribbon button belonging to a deferred built-in plugin.
#[derive(Clone, Copy, Debug)]
pub struct LazyButton {
    pub toolbar: &'static str,
    pub id: &'static str,
    pub label: &'static str,
    pub tooltip: &'static str,
    pub icon: &'static str,
    pub enabled: bool,
    pub clickable_when_viewport_hidden: bool,
}

/// Static metadata for one bindable `PluginAction` of a deferred built-in plugin.
#[derive(Clone, Copy, Debug)]
pub struct LazyAction {
    pub id: &'static str,
    pub text: &'static str,
    pub status_tip: &'static str,
}

/// Editor conditions that automatically materialize a deferred plugin.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WakeOn {
    /// Materialize when any selected instance `IsA(class)`.
    SelectionClass(&'static str),
    /// Materialize when `(service, signal)` fires.
    Signal(&'static str, &'static str),
}

/// Deferred-loading entry for one Studio built-in `.rbxm` (matched by file stem).
#[derive(Clone, Copy, Debug)]
pub struct LazyManifest {
    pub stem: &'static str,
    pub buttons: &'static [LazyButton],
    pub actions: &'static [LazyAction],
    pub wake: &'static [WakeOn],
    /// True if Studio 0.748 skips this plugin for external users via
    /// `FStringExternalUserPluginLoadDenyList`. Rustudio keeps it available
    /// lazily instead of dropping it.
    pub studio_deny_listed: bool,
}

/// Live UI stub kept for a deferred `Spec` until it is materialized.
#[derive(Clone, Debug)]
pub struct DeferredStub {
    pub plugin: PluginId,
    pub manifest: &'static LazyManifest,
    pub groups: Vec<RibbonGroup>,
    pub actions: Vec<ActionRow>,
}

/// What a deferred `Ref` in the ribbon / shortcuts maps back to.
#[derive(Clone, Debug)]
pub enum DeferredTarget {
    Button { plugin: PluginId, toolbar: String, id: String, event: &'static str },
    Action { plugin: PluginId, id: String },
}

impl DeferredStub {
    /// Builds synthetic `RibbonGroup`s and `ActionRow`s with unique `Ref`s so
    /// the Plugins tab, Plugins menu, Customize Shortcuts, and `--result` work
    /// identically before the `.rbxm` is read from disk.
    pub fn new(plugin: PluginId, plugin_name: &str, manifest: &'static LazyManifest) -> Self {
        let mut groups: Vec<RibbonGroup> = Vec::new();
        for b in manifest.buttons {
            let group = match groups.iter_mut().find(|g| g.name == b.toolbar) {
                Some(g) => g,
                None => {
                    groups.push(RibbonGroup {
                        plugin,
                        plugin_name: plugin_name.to_string(),
                        toolbar: Ref::new(),
                        name: b.toolbar.to_string(),
                        buttons: Vec::new(),
                    });
                    groups.last_mut().expect("just pushed")
                }
            };
            let resolved_icon = if b.icon.is_empty() {
                builtin_default_icon(plugin_name, b.toolbar, b.id).unwrap_or("")
            } else {
                b.icon
            };
            group.buttons.push(RibbonButton {
                r: Ref::new(),
                id: b.id.to_string(),
                label: button_label(b.label, b.id, b.tooltip),
                tooltip: b.tooltip.to_string(),
                icon: resolved_icon.to_string(),
                enabled: b.enabled,
                clickable_when_viewport_hidden: b.clickable_when_viewport_hidden,
                active: false,
                key: format!("{plugin_name}_{}_{}", b.toolbar, b.id),
            });
        }
        groups.sort_by(|a, b| a.name.cmp(&b.name));

        let actions = manifest
            .actions
            .iter()
            .map(|a| ActionRow {
                r: Ref::new(),
                key: format!("{plugin_name}_{}", a.id),
                text: a.text.to_string(),
                status_tip: a.status_tip.to_string(),
            })
            .collect();

        DeferredStub { plugin, manifest, groups, actions }
    }

    pub fn find_ref(&self, r: Ref, event: &'static str) -> Option<DeferredTarget> {
        for g in &self.groups {
            for b in &g.buttons {
                if b.r == r {
                    return Some(DeferredTarget::Button {
                        plugin: self.plugin,
                        toolbar: g.name.clone(),
                        id: b.id.clone(),
                        event,
                    });
                }
            }
        }
        for (a, meta) in self.actions.iter().zip(self.manifest.actions.iter()) {
            if a.r == r {
                return Some(DeferredTarget::Action {
                    plugin: self.plugin,
                    id: meta.id.to_string(),
                });
            }
        }
        None
    }

    pub fn matches_button(&self, tb: &str, btn: &str) -> Option<(String, String)> {
        for g in &self.groups {
            if tb != "*" && g.name != tb {
                continue;
            }
            for b in &g.buttons {
                if b.id == btn || b.label == btn {
                    return Some((g.name.clone(), b.id.clone()));
                }
            }
        }
        None
    }

    pub fn matches_action(&self, query: &str) -> Option<String> {
        for (row, meta) in self.actions.iter().zip(self.manifest.actions.iter()) {
            if meta.id == query || row.key == query {
                return Some(meta.id.to_string());
            }
        }
        None
    }
}

/// True when `RUSTUDIO_BUILTIN_PLUGINS_EAGER` asks to load every built-in
/// eagerly at startup (e.g. for the 58-plugin full-load survey).
pub fn eager_env() -> bool {
    std::env::var_os("RUSTUDIO_BUILTIN_PLUGINS_EAGER").is_some_and(|v| !v.is_empty() && v != "0")
}

/// Looks up the deferred manifest for a built-in `.rbxm` path. Returns `None`
/// for plugins that must load eagerly at startup (`SimulationStep`, `UIEditor`,
/// `TutorialLinks`, or any unrecognized future Studio plugin).
pub fn manifest_for_path(path: &Path) -> Option<&'static LazyManifest> {
    let stem = path.file_stem()?.to_str()?;
    MANIFESTS.iter().find(|m| m.stem.eq_ignore_ascii_case(stem))
}

/// Studio 0.748's canonical icon URI for a built-in plugin toolbar button.
///
/// Most built-in plugins pass `""` to `PluginToolbar:CreateButton` because
/// real Studio supplies their icons from `content/studio_svg_textures/Shared/`
/// via `Ribbon.rbxm` and C++ `PluginUIRelayGuard`. Returning that URI here lets
/// both deferred stubs and materialized built-ins render Studio's real icons
/// with zero blank diamond placeholders.
pub fn builtin_default_icon(plugin_name: &str, toolbar: &str, button_id: &str) -> Option<&'static str> {
    let stem = plugin_name.strip_prefix("builtin_")?.trim_end_matches(".rbxm");
    Some(match (stem, toolbar, button_id) {
        ("AlignmentTool", _, _) => "rbxasset://studio_svg_textures/Shared/Ribbon/Dark/Medium/RibbonAlignTool.png",
        ("AnimationClipEditor", _, _) => "rbxasset://studio_svg_textures/Shared/Ribbon/Dark/Medium/RibbonAnimation.png",
        ("AssetImporter", _, _) => "rbxasset://studio_svg_textures/Shared/Ribbon/Dark/Medium/Ribbon3DImport.png",
        ("AudioDiscovery", _, _) => "rbxlocaltheme://ViewSounds",
        ("AvatarCompatibilityPreviewer", _, _) => "rbxasset://studio_svg_textures/Shared/Ribbon/Dark/Medium/RibbonSetup.png",
        ("ConstraintTool", "AlignOrientation", _) => "rbxasset://studio_svg_textures/Shared/Ribbon/Dark/Medium/RibbonAlignOrientation.png",
        ("ConstraintTool", "AlignPosition", _) => "rbxasset://studio_svg_textures/Shared/Ribbon/Dark/Medium/RibbonAlignPosition.png",
        ("ConstraintTool", "AngularVelocity", _) => "rbxasset://studio_svg_textures/Shared/Ribbon/Dark/Medium/RibbonAngularVelocity.png",
        ("ConstraintTool", "Attachment", _) => "rbxasset://studio_svg_textures/Shared/Ribbon/Dark/Medium/RibbonAttachment.png",
        ("ConstraintTool", "BallSocketConstraint", _) => "rbxasset://studio_svg_textures/Shared/Ribbon/Dark/Medium/RibbonBallSocketConstraint.png",
        ("ConstraintTool", "Bone", _) => "rbxasset://studio_svg_textures/Shared/Ribbon/Dark/Medium/RibbonBone.png",
        ("ConstraintTool", "CylindricalConstraint", _) => "rbxasset://studio_svg_textures/Shared/Ribbon/Dark/Medium/RibbonCylindricalConstraint.png",
        ("ConstraintTool", "HingeConstraint", _) => "rbxasset://studio_svg_textures/Shared/Ribbon/Dark/Medium/RibbonHingeConstraint.png",
        ("ConstraintTool", "LineForce", _) => "rbxasset://studio_svg_textures/Shared/Ribbon/Dark/Medium/RibbonLineForce.png",
        ("ConstraintTool", "LinearVelocity", _) => "rbxasset://studio_svg_textures/Shared/Ribbon/Dark/Medium/RibbonLinearVelocity.png",
        ("ConstraintTool", "NoCollisionConstraint", _) => "rbxasset://studio_svg_textures/Shared/Ribbon/Dark/Medium/RibbonNoCollisionConstraint.png",
        ("ConstraintTool", "PlaneConstraint", _) => "rbxasset://studio_svg_textures/Shared/Ribbon/Dark/Medium/RibbonPlaneConstraint.png",
        ("ConstraintTool", "PrismaticConstraint", _) => "rbxasset://studio_svg_textures/Shared/Ribbon/Dark/Medium/RibbonPrismaticConstraint.png",
        ("ConstraintTool", "RigidConstraint", _) => "rbxasset://studio_svg_textures/Shared/Ribbon/Dark/Medium/RibbonRigidConstraint.png",
        ("ConstraintTool", "RodConstraint", _) => "rbxasset://studio_svg_textures/Shared/Ribbon/Dark/Medium/RibbonRodConstraint.png",
        ("ConstraintTool", "RopeConstraint", _) => "rbxasset://studio_svg_textures/Shared/Ribbon/Dark/Medium/RibbonRopeConstraint.png",
        ("ConstraintTool", "SpringConstraint", _) => "rbxasset://studio_svg_textures/Shared/Ribbon/Dark/Medium/RibbonSpringConstraint.png",
        ("ConstraintTool", "Torque", _) => "rbxasset://studio_svg_textures/Shared/Ribbon/Dark/Medium/RibbonTorque.png",
        ("ConstraintTool", "TorsionSpringConstraint", _) => "rbxasset://studio_svg_textures/Shared/Ribbon/Dark/Medium/RibbonTorsionSpringConstraint.png",
        ("ConstraintTool", "UniversalConstraint", _) => "rbxasset://studio_svg_textures/Shared/Ribbon/Dark/Medium/RibbonUniversalConstraint.png",
        ("ConstraintTool", "VectorForce", _) => "rbxasset://studio_svg_textures/Shared/Ribbon/Dark/Medium/RibbonVectorForce.png",
        ("ConstraintTool", "WeldConstraint", _) => "rbxasset://studio_svg_textures/Shared/Ribbon/Dark/Medium/RibbonWeldConstraint.png",
        ("DraftsWidget", _, _) => "rbxasset://studio_svg_textures/Shared/WidgetIcons/Dark/Large/Draft.png",
        ("GameSettings", _, _) => "rbxlocaltheme://GameSettings",
        ("LocalizationTools", _, _) => "rbxlocaltheme://LocalizationTools",
        ("MaterialManager", _, _) => "rbxasset://studio_svg_textures/Shared/Ribbon/Dark/Medium/RibbonMaterial.png",
        ("MaterialPicker", _, _) => "rbxasset://studio_svg_textures/Shared/Modeling/Dark/Large/Material.png",
        ("MoveDragger", _, _) => "rbxasset://studio_svg_textures/Shared/Ribbon/Dark/Medium/RibbonMove.png",
        ("PivotEditor", _, "ClearPivot") => "rbxasset://studio_svg_textures/Shared/Ribbon/Dark/Medium/RibbonResetPivot.png",
        ("PivotEditor", _, _) => "rbxasset://studio_svg_textures/Shared/Ribbon/Dark/Medium/RibbonEditPivot.png",
        ("PlayerEmulator", _, _) => "rbxlocaltheme://Player",
        ("R15Migrator", _, _) => "rbxasset://studio_svg_textures/Shared/Ribbon/Dark/Medium/RibbonSetup.png",
        ("RigGenerator", _, _) => "rbxasset://studio_svg_textures/Shared/Ribbon/Dark/Medium/RibbonCharacter.png",
        ("RotateDragger", _, _) => "rbxasset://studio_svg_textures/Shared/Ribbon/Dark/Medium/RibbonRotate.png",
        ("ScaleDragger", _, _) => "rbxasset://studio_svg_textures/Shared/Ribbon/Dark/Medium/RibbonScale.png",
        ("SelectDragger", _, _) => "rbxasset://studio_svg_textures/Shared/Ribbon/Dark/Medium/RibbonSelect.png",
        ("StyleEditor", _, _) => "rbxasset://studio_svg_textures/Shared/Ribbon/Dark/Medium/RibbonStyleEditor.png",
        ("TagEditor", _, _) => "rbxasset://studio_svg_textures/Shared/WidgetIcons/Dark/Large/TagEditor.png",
        ("TerrainEditor", _, _) => "rbxasset://studio_svg_textures/Shared/Ribbon/Dark/Medium/RibbonTerrain.png",
        ("Toolbox", _, _) => "rbxasset://studio_svg_textures/Shared/Ribbon/Dark/Medium/RibbonToolbox.png",
        ("UIEditor", _, _) => "rbxasset://studio_svg_textures/Shared/Ribbon/Dark/Medium/RibbonGUI.png",
        ("ViewSelector", _, _) => "rbxasset://studio_svg_textures/Shared/Ribbon/Dark/Medium/RibbonViewportFrame.png",
        ("WindControl", _, _) => "rbxasset://studio_svg_textures/Shared/WidgetIcons/Dark/Large/GlobalWindControl.png",
        _ => return None,
    })
}

/// Studio 0.748 Model > Constraint dropdown entries (`(ConstraintTool toolbar, display label)`).
/// Mechanical constraints first, then mover constraints / forces (`None` = separator).
pub const CONSTRAINT_MENU_ITEMS: &[Option<(&'static str, &'static str)>] = &[
    Some(("WeldConstraint", "Weld")),
    Some(("RigidConstraint", "Rigid")),
    Some(("HingeConstraint", "Hinge")),
    Some(("BallSocketConstraint", "Ball Socket")),
    Some(("PrismaticConstraint", "Prismatic")),
    Some(("CylindricalConstraint", "Cylindrical")),
    Some(("UniversalConstraint", "Universal")),
    Some(("RodConstraint", "Rod")),
    Some(("RopeConstraint", "Rope")),
    Some(("SpringConstraint", "Spring")),
    Some(("TorsionSpringConstraint", "Torsion Spring")),
    Some(("PlaneConstraint", "Plane")),
    Some(("NoCollisionConstraint", "No Collision")),
    None,
    Some(("AlignPosition", "Align Position")),
    Some(("AlignOrientation", "Align Orientation")),
    Some(("LinearVelocity", "Linear Velocity")),
    Some(("AngularVelocity", "Angular Velocity")),
    Some(("VectorForce", "Vector Force")),
    Some(("LineForce", "Line Force")),
    Some(("Torque", "Torque")),
];

/// Studio 0.748 Model > Attachment dropdown entries (`(ConstraintTool toolbar, display label)`).
pub const ATTACHMENT_MENU_ITEMS: &[(&'static str, &'static str)] = &[
    ("Attachment", "Attachment"),
    ("Bone", "Bone"),
];

/// All deferred manifests (exposed for tests validating icon/routing coverage).
pub fn all_manifests() -> &'static [LazyManifest] {
    MANIFESTS
}

const fn btn(
    toolbar: &'static str,
    id: &'static str,
    label: &'static str,
    tooltip: &'static str,
    icon: &'static str,
    clickable_when_viewport_hidden: bool,
) -> LazyButton {
    LazyButton { toolbar, id, label, tooltip, icon, enabled: true, clickable_when_viewport_hidden }
}

const fn constraint_btn(toolbar: &'static str) -> LazyButton {
    btn(toolbar, "Place", "Place", "Create an Attachment.", "", false)
}

/// Deferred manifests for 55 of Studio 0.748's 58 `BuiltInPlugins` `.rbxm` files.
/// The remaining 3 tiny plugins (`SimulationStep`, `TutorialLinks`, `UIEditor`,
/// ~0.6 MB Lua total, 0 ms/frame idle) load eagerly at startup.
const MANIFESTS: &[LazyManifest] = &[
    // --- Toolbar UI / tool plugins (24) ---
    LazyManifest {
        stem: "AlignmentTool",
        buttons: &[btn("Alignment", "AlignTool", "AlignTool", "", "", true)],
        actions: &[],
        wake: &[],
        studio_deny_listed: false,
    },
    LazyManifest {
        stem: "AnimationClipEditor",
        buttons: &[btn(
            "Animations",
            "Animation Editor",
            "Animation Editor",
            "Create, preview, and publish Animation clips",
            "",
            false,
        )],
        actions: &[],
        wake: &[],
        studio_deny_listed: false,
    },
    LazyManifest {
        stem: "AssetImporter",
        buttons: &[btn(
            "Mesh Importer",
            "Import",
            "Import",
            "Import a mesh rig or avatar from a file using the import UI.",
            "",
            true,
        )],
        actions: &[],
        wake: &[],
        studio_deny_listed: false,
    },
    LazyManifest {
        stem: "AudioDiscovery",
        buttons: &[btn(
            "Audio Discovery",
            "View Sounds",
            "View Sounds",
            "Discover the audio assets in this place and see who owns them",
            "rbxlocaltheme://ViewSounds",
            false,
        )],
        actions: &[],
        wake: &[],
        studio_deny_listed: false,
    },
    LazyManifest {
        stem: "AvatarCompatibilityPreviewer",
        buttons: &[btn(
            "AvatarPreviewer",
            "previewAvatar",
            "previewAvatar",
            "Opens the Avatar Setup",
            "",
            false,
        )],
        actions: &[],
        wake: &[],
        studio_deny_listed: false,
    },
    LazyManifest {
        stem: "ConstraintTool",
        buttons: &[
            constraint_btn("AlignOrientation"),
            constraint_btn("AlignPosition"),
            constraint_btn("AngularVelocity"),
            constraint_btn("Attachment"),
            constraint_btn("BallSocketConstraint"),
            constraint_btn("Bone"),
            constraint_btn("CylindricalConstraint"),
            constraint_btn("HingeConstraint"),
            constraint_btn("LineForce"),
            constraint_btn("LinearVelocity"),
            constraint_btn("NoCollisionConstraint"),
            constraint_btn("PlaneConstraint"),
            constraint_btn("PrismaticConstraint"),
            constraint_btn("RigidConstraint"),
            constraint_btn("RodConstraint"),
            constraint_btn("RopeConstraint"),
            constraint_btn("SpringConstraint"),
            constraint_btn("Torque"),
            constraint_btn("TorsionSpringConstraint"),
            constraint_btn("UniversalConstraint"),
            constraint_btn("VectorForce"),
            constraint_btn("WeldConstraint"),
        ],
        actions: &[],
        wake: &[],
        studio_deny_listed: false,
    },
    LazyManifest {
        stem: "DraftsWidget",
        buttons: &[btn("draftsToolbar", "draftsButton", "draftsButton", "View Drafts", "", true)],
        actions: &[],
        wake: &[],
        studio_deny_listed: false,
    },
    LazyManifest {
        stem: "GameSettings",
        buttons: &[btn(
            "gameSettingsToolbar",
            "gameSettingsButton",
            "gameSettingsButton",
            "Studio.GameSettingsPlugin.General.PluginDescription",
            "rbxlocaltheme://GameSettings",
            true,
        )],
        actions: &[],
        wake: &[WakeOn::Signal("StudioService", "OnOpenGameSettings")],
        studio_deny_listed: false,
    },
    LazyManifest {
        stem: "LocalizationTools",
        buttons: &[btn(
            "Localization",
            "Tools",
            "Tools",
            "Hide/show the Localization Tools View",
            "rbxlocaltheme://LocalizationTools",
            false,
        )],
        actions: &[],
        wake: &[],
        studio_deny_listed: false,
    },
    LazyManifest {
        stem: "MaterialManager",
        buttons: &[btn("Edit", "MaterialManager", "MaterialManager", "", "", true)],
        actions: &[],
        wake: &[],
        studio_deny_listed: false,
    },
    LazyManifest {
        stem: "MaterialPicker",
        buttons: &[btn(
            "Edit",
            "MaterialPicker",
            "Material",
            "Choose materials to apply to objects in the scene",
            "",
            false,
        )],
        actions: &[],
        wake: &[],
        studio_deny_listed: false,
    },
    LazyManifest {
        stem: "MoveDragger",
        buttons: &[btn("Home", "Move", "Move", "Move an Object", "", false)],
        actions: &[],
        wake: &[],
        studio_deny_listed: false,
    },
    LazyManifest {
        stem: "PivotEditor",
        buttons: &[
            btn("Pivot", "ClearPivot", "ClearPivot", "", "", true),
            btn("Pivot", "EditPivot", "EditPivot", "", "", true),
        ],
        actions: &[],
        wake: &[],
        studio_deny_listed: false,
    },
    LazyManifest {
        stem: "PlayerEmulator",
        buttons: &[btn(
            "luaPlayerEmulatorToolbar",
            "luaPlayerEmulatorButton",
            "luaPlayerEmulatorButton",
            "",
            "rbxlocaltheme://Player",
            true,
        )],
        actions: &[],
        wake: &[],
        studio_deny_listed: false,
    },
    LazyManifest {
        stem: "RigGenerator",
        buttons: &[btn(
            "Rig Generator",
            "Generate Rig",
            "Generate Rig",
            "Generates Avatar Rigs",
            "",
            false,
        )],
        actions: &[],
        wake: &[],
        studio_deny_listed: false,
    },
    LazyManifest {
        stem: "RotateDragger",
        buttons: &[btn("Home", "Rotate", "Rotate", "Rotate an Object", "", false)],
        actions: &[],
        wake: &[],
        studio_deny_listed: false,
    },
    LazyManifest {
        stem: "ScaleDragger",
        buttons: &[btn("Home", "Scale", "Scale", "Scale an Object", "", false)],
        actions: &[],
        wake: &[],
        studio_deny_listed: false,
    },
    LazyManifest {
        stem: "SelectDragger",
        buttons: &[btn("Home", "Select", "Select", "Select an Object", "", false)],
        actions: &[],
        wake: &[],
        studio_deny_listed: false,
    },
    LazyManifest {
        stem: "StyleEditor",
        buttons: &[btn(
            "StyleEditor",
            "StyleEditorToolbarButton",
            "StyleEditorToolbarButton",
            "Toggle the Style Editor.",
            "",
            false,
        )],
        actions: &[],
        wake: &[WakeOn::Signal("GuiService", "OpenStyleEditor")],
        studio_deny_listed: false,
    },
    LazyManifest {
        stem: "TagEditor",
        buttons: &[btn("tagEditorToolbar", "TagEditor", "TagEditor", "", "", true)],
        actions: &[],
        wake: &[],
        studio_deny_listed: false,
    },
    LazyManifest {
        stem: "TerrainEditor",
        buttons: &[btn(
            "TerrainEditorLuaToolbarName",
            "Editor",
            "Editor",
            "Studio.TerrainEditor.Main.PluginButtonEditorTooltip",
            "",
            true,
        )],
        actions: &[],
        wake: &[],
        studio_deny_listed: false,
    },
    LazyManifest {
        stem: "Toolbox",
        buttons: &[btn(
            "luaToolboxToolbar",
            "luaToolboxButton",
            "luaToolboxButton",
            "Insert items from the toolbox",
            "",
            true,
        )],
        actions: &[],
        wake: &[],
        studio_deny_listed: false,
    },
    LazyManifest {
        stem: "ViewSelector",
        buttons: &[btn(
            "ViewSelector",
            "ViewSelector",
            "ViewSelector",
            "Show View Selector",
            "",
            false,
        )],
        actions: &[],
        wake: &[],
        studio_deny_listed: false,
    },
    LazyManifest {
        stem: "WindControl",
        buttons: &[btn("windControl", "WindControl", "WindControl", "", "", false)],
        actions: &[],
        wake: &[],
        studio_deny_listed: false,
    },
    // --- Action / selection / signal-triggered plugins (22) ---
    LazyManifest {
        stem: "PathEditor",
        buttons: &[],
        actions: &[
            LazyAction {
                id: "PathEditor_DoneEditing",
                text: "Path2D Editor Done Editing",
                status_tip: "Exit the Path2D editing mode",
            },
            LazyAction {
                id: "PathEditor_AddPointMode",
                text: "Path2D Editor Add Point Mode",
                status_tip: "Activate Add Point Mode in the Path2D Editor",
            },
            LazyAction {
                id: "PathEditor_AddTangentMode",
                text: "Path2D Editor Add Tangent Mode",
                status_tip: "Activate Add Tangent Mode in the Path2D Editor",
            },
            LazyAction {
                id: "PathEditor_MoveTool",
                text: "Path2D Editor Select Mode",
                status_tip: "Activate Select Mode in the Path2D Editor",
            },
        ],
        wake: &[WakeOn::SelectionClass("Path2D")],
        studio_deny_listed: false,
    },
    LazyManifest {
        stem: "LightGuides",
        buttons: &[],
        actions: &[],
        wake: &[WakeOn::SelectionClass("Light")],
        studio_deny_listed: false,
    },
    LazyManifest {
        stem: "9SliceEditor",
        buttons: &[],
        actions: &[],
        wake: &[WakeOn::Signal("GuiService", "Open9SliceEditor")],
        studio_deny_listed: false,
    },
    LazyManifest {
        stem: "PublishPlaceAs",
        buttons: &[],
        actions: &[],
        wake: &[WakeOn::Signal("StudioPublishService", "OnSaveOrPublishPlaceToRoblox")],
        studio_deny_listed: false,
    },
    LazyManifest {
        stem: "AdaptiveAnimationSupport",
        buttons: &[],
        actions: &[],
        wake: &[],
        studio_deny_listed: false,
    },
    LazyManifest {
        stem: "AnimatedImageImporter",
        buttons: &[],
        actions: &[],
        wake: &[],
        studio_deny_listed: false,
    },
    LazyManifest {
        stem: "CompositorDebugger",
        buttons: &[],
        actions: &[],
        wake: &[],
        studio_deny_listed: false,
    },
    LazyManifest {
        stem: "ConvertToPackage",
        buttons: &[],
        actions: &[],
        wake: &[],
        studio_deny_listed: false,
    },
    LazyManifest {
        stem: "DeviceEmulator",
        buttons: &[],
        actions: &[],
        wake: &[],
        studio_deny_listed: false,
    },
    LazyManifest {
        stem: "EventEmulator",
        buttons: &[],
        actions: &[],
        wake: &[],
        studio_deny_listed: false,
    },
    LazyManifest {
        stem: "ForceVisualizationControl",
        buttons: &[],
        actions: &[],
        wake: &[],
        studio_deny_listed: false,
    },
    LazyManifest {
        stem: "KnowledgeTutorials",
        buttons: &[],
        actions: &[],
        wake: &[],
        studio_deny_listed: false,
    },
    LazyManifest {
        stem: "LayeredClothingEditor",
        buttons: &[],
        actions: &[],
        wake: &[],
        studio_deny_listed: false,
    },
    LazyManifest {
        stem: "LegacyChatDeprecation",
        buttons: &[],
        actions: &[],
        wake: &[],
        studio_deny_listed: false,
    },
    LazyManifest {
        stem: "ManageCollaborators",
        buttons: &[],
        actions: &[],
        wake: &[],
        studio_deny_listed: false,
    },
    LazyManifest {
        stem: "MaterialGenerator",
        buttons: &[],
        actions: &[],
        wake: &[],
        studio_deny_listed: false,
    },
    LazyManifest {
        stem: "SocialPresence",
        buttons: &[],
        actions: &[],
        wake: &[],
        studio_deny_listed: false,
    },
    LazyManifest {
        stem: "StudioCameraUI",
        buttons: &[],
        actions: &[],
        wake: &[],
        studio_deny_listed: false,
    },
    LazyManifest {
        stem: "TerrainPaletteEditor",
        buttons: &[],
        actions: &[],
        wake: &[],
        studio_deny_listed: false,
    },
    LazyManifest {
        stem: "TextureGenerator",
        buttons: &[],
        actions: &[],
        wake: &[],
        studio_deny_listed: false,
    },
    LazyManifest {
        stem: "TextureViewer",
        buttons: &[],
        actions: &[],
        wake: &[],
        studio_deny_listed: false,
    },
    LazyManifest {
        stem: "VRStatus",
        buttons: &[],
        actions: &[],
        wake: &[],
        studio_deny_listed: false,
    },
    // --- Studio 0.748 FStringExternalUserPluginLoadDenyList internal tools (9) ---
    LazyManifest {
        stem: "AutomaticRun",
        buttons: &[],
        actions: &[],
        wake: &[],
        studio_deny_listed: true,
    },
    LazyManifest {
        stem: "AvatarImporter",
        buttons: &[],
        actions: &[],
        wake: &[],
        studio_deny_listed: true,
    },
    LazyManifest {
        stem: "CageMeshMigrationTool",
        buttons: &[],
        actions: &[],
        wake: &[],
        studio_deny_listed: true,
    },
    LazyManifest {
        stem: "DeveloperInspector",
        buttons: &[],
        actions: &[],
        wake: &[],
        studio_deny_listed: true,
    },
    LazyManifest {
        stem: "DeveloperStorybook",
        buttons: &[],
        actions: &[],
        wake: &[],
        studio_deny_listed: true,
    },
    LazyManifest {
        stem: "DynamicHeadExportTool",
        buttons: &[],
        actions: &[],
        wake: &[],
        studio_deny_listed: true,
    },
    LazyManifest {
        stem: "InternalAvatarTools",
        buttons: &[],
        actions: &[],
        wake: &[],
        studio_deny_listed: true,
    },
    LazyManifest {
        stem: "NestedPackagePublisher",
        buttons: &[],
        actions: &[],
        wake: &[],
        studio_deny_listed: true,
    },
    LazyManifest {
        stem: "RoduxDevtools",
        buttons: &[],
        actions: &[],
        wake: &[],
        studio_deny_listed: true,
    },
    // --- Older pre-0.748 Studio installs ---
    LazyManifest {
        stem: "DEPRECATED_AssetManager",
        buttons: &[],
        actions: &[],
        wake: &[],
        studio_deny_listed: true,
    },
    LazyManifest {
        stem: "R15Migrator",
        buttons: &[LazyButton {
            toolbar: "R15Migrator",
            id: "R15Migrator",
            label: "R15 Migrator",
            tooltip: "Migrate R6 avatars to R15",
            icon: "",
            enabled: true,
            clickable_when_viewport_hidden: true,
        }],
        actions: &[],
        wake: &[],
        studio_deny_listed: false,
    },
    LazyManifest {
        stem: "StreamingServiceDispatcherRegistry",
        buttons: &[],
        actions: &[],
        wake: &[],
        studio_deny_listed: true,
    },
];
