//! Which DataModel children Studio's Explorer lists.
//!
//! Studio's Explorer asks the engine for each child of the DataModel
//! (`ExplorerServiceVisibilityService:GetServiceVisibility`, Studio
//! 0.7xx) and draws only the ones
//! it says yes to; the instances stay in the DataModel. In order:
//!
//! 1. Forced off: CoreGui unless "Show Core GUI in Explorer
//!    while Playing", PluginGuiService and RobloxPluginGuiService unless
//!    "Show Plugin GUI Service in Explorer", PluginDebugService, and
//!    CorePackages / InternalSyncService (Roblox-internal builds only).
//!    "Show Hidden Objects in Explorer" lifts all but the last two.
//! 2. The place's own choice: ServiceVisibilityService.VisibleServices /
//!    HiddenServices (class names; Studio's Insert Service / hide service
//!    actions write them) show or hide a class.
//! 3. "Show Hidden Objects in Explorer" shows everything else.
//! 4. ReflectionMetadata, from the class's own ReflectionMetadataClass or
//!    the nearest superclass that has one: the class is listed only when
//!    it sets ExplorerOrder >= 0 (setting it
//!    raises a flag the default leaves off), and then by its
//!    ServiceVisibility: Always (0, the default), Off (1), or WithChildren
//!    (2: only while it has children).
//!
//! So a default place lists Workspace, Players, Lighting, MaterialService,
//! ReplicatedFirst, ReplicatedStorage, ServerScriptService, ServerStorage,
//! StarterGui, StarterPack, StarterPlayer, Teams, SoundService and
//! TextChatService (measured in Studio for a new Baseplate and two
//! published places), plus Chat / LocalizationService / TestService /
//! VoiceChatService / VRService once they have children.

use rbx_dom_weak::types::{Ref, Variant};
use rustudio_core::doc::Document;

/// The Explorer settings that change the list (Studio > Explorer).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Prefs {
    pub show_hidden: bool,
    pub show_core_gui: bool,
    pub show_plugin_gui: bool,
}

impl Prefs {
    pub fn current() -> Self {
        let f = |p: &str| crate::studio_settings::flag("Studio", p, false);
        Prefs {
            show_hidden: f("Show Hidden Objects in Explorer"),
            show_core_gui: f("Show Core GUI in Explorer while Playing"),
            show_plugin_gui: f("Show Plugin GUI Service in Explorer"),
        }
    }
}

/// A class's ReflectionMetadataClass fields that matter here.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Meta {
    pub explorer_order: Option<i32>,
    pub service_visibility: Option<i32>,
}

/// The place's ServiceVisibilityService lists (class names).
#[derive(Debug, Clone, Default)]
pub struct PlaceLists {
    pub visible: Vec<String>,
    pub hidden: Vec<String>,
}

impl PlaceLists {
    pub fn of(doc: &Document) -> Self {
        let root = doc.dom.root();
        let Some(svs) = root
            .children()
            .iter()
            .filter_map(|r| doc.dom.get_by_ref(*r))
            .find(|i| i.class.as_str() == "ServiceVisibilityService")
        else {
            return Self::default();
        };
        let list = |k: &str| match svs.properties.get(&k.into()) {
            Some(Variant::BinaryString(b)) => decode_list(b.as_ref()),
            Some(Variant::String(s)) => decode_list(s.as_bytes()),
            _ => Vec::new(),
        };
        PlaceLists { visible: list("VisibleServices"), hidden: list("HiddenServices") }
    }
}

/// VisibleServices / HiddenServices: Studio's Variant array:
/// u32 count, then per item a type byte (2 = string), u32 length and the
/// bytes. Anything else ends the list.
pub fn decode_list(b: &[u8]) -> Vec<String> {
    let u32_at = |o: usize| b.get(o..o + 4).map(|s| u32::from_le_bytes([s[0], s[1], s[2], s[3]]) as usize);
    let mut out = Vec::new();
    let Some(n) = u32_at(0) else { return out };
    let mut o = 4;
    for _ in 0..n.min(4096) {
        if b.get(o) != Some(&2) {
            break;
        }
        let Some(len) = u32_at(o + 1) else { break };
        let Some(s) = b.get(o + 5..o + 5 + len) else { break };
        out.push(String::from_utf8_lossy(s).into_owned());
        o += 5 + len;
    }
    out
}

/// The encoding Studio writes (for tests and new places).
pub fn encode_list(names: &[&str]) -> Vec<u8> {
    let mut out = (names.len() as u32).to_le_bytes().to_vec();
    for n in names {
        out.push(2);
        out.extend_from_slice(&(n.len() as u32).to_le_bytes());
        out.extend_from_slice(n.as_bytes());
    }
    out
}

/// GetServiceVisibility for a DataModel child of class `class`.
/// `meta` gives a class's own ReflectionMetadataClass (None when it has
/// none), `superclass` its superclass.
pub fn visible(
    class: &str,
    has_children: bool,
    lists: &PlaceLists,
    prefs: Prefs,
    meta: &dyn Fn(&str) -> Option<Meta>,
    superclass: &dyn Fn(&str) -> Option<String>,
) -> bool {
    // 1. Forced off.
    let forced_off = match class {
        "CorePackages" | "InternalSyncService" => return false,
        "CoreGui" => !prefs.show_core_gui,
        "PluginGuiService" | "RobloxPluginGuiService" => !prefs.show_plugin_gui,
        "PluginDebugService" => true,
        _ => false,
    };
    if forced_off && !prefs.show_hidden {
        return false;
    }
    // 2. The place's own lists (VisibleServices first).
    if lists.visible.iter().any(|c| c == class) {
        return true;
    }
    if lists.hidden.iter().any(|c| c == class) {
        return false;
    }
    // 3. Show Hidden Objects.
    if prefs.show_hidden {
        return true;
    }
    // 4. ReflectionMetadata, nearest class that has an entry.
    let mut cur = Some(class.to_string());
    let mut guard = 0;
    while let Some(c) = cur {
        if let Some(m) = meta(&c) {
            if !m.explorer_order.is_some_and(|o| o >= 0) {
                return false;
            }
            return match m.service_visibility.unwrap_or(0) {
                0 => true,
                2 => has_children,
                _ => false,
            };
        }
        guard += 1;
        if guard > 64 {
            break;
        }
        cur = superclass(&c);
    }
    false
}

/// Studio's ReflectionMetadata and the API dump's superclasses.
pub fn studio_meta(class: &str) -> Option<Meta> {
    crate::studio_panels::class_meta()
        .get(class)
        .map(|m| Meta { explorer_order: m.explorer_order, service_visibility: m.service_visibility })
}

pub fn studio_superclass(class: &str) -> Option<String> {
    if let Some(api) = rustudio_core::api::get() {
        if let Some(c) = api.classes.get(class) {
            return c.superclass.clone();
        }
    }
    rbx_reflection_database::get()
        .ok()
        .and_then(|db| db.classes.get(class))
        .and_then(|c| c.superclass.as_ref().map(|s| s.to_string()))
}

/// The DataModel children Studio's Explorer lists (any order).
pub fn listed_root_children(doc: &Document, prefs: Prefs) -> Vec<Ref> {
    let lists = PlaceLists::of(doc);
    let root = doc.dom.root();
    // Without Studio's ReflectionMetadata (content not installed yet, or
    // RUSTUDIO_NO_STUDIO_FETCH) step 4 would hide every service: list all but
    // the ones Studio forces off, plus StudioService.
    let have_meta = !crate::studio_panels::class_meta().is_empty();
    root.children()
        .iter()
        .filter_map(|r| doc.dom.get_by_ref(*r))
        .filter(|i| {
            if have_meta {
                return visible(i.class.as_str(), !i.children().is_empty(), &lists, prefs, &studio_meta, &studio_superclass);
            }
            let c = i.class.as_str();
            visible(c, !i.children().is_empty(), &lists, prefs, &|_| Some(Meta { explorer_order: Some(0), service_visibility: None }), &|_| None)
                && (prefs.show_hidden || c != "StudioService")
        })
        .map(|i| i.referent())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    /// ReflectionMetadata for the classes below, as Studio ships it.
    fn meta_table() -> HashMap<&'static str, Meta> {
        let m = |o: Option<i32>, v: Option<i32>| Meta { explorer_order: o, service_visibility: v };
        HashMap::from([
            ("Instance", m(None, None)),
            ("Workspace", m(Some(5), None)),
            ("Players", m(Some(20), None)),
            ("Lighting", m(Some(30), None)),
            ("MaterialService", m(Some(30), None)),
            ("ReplicatedFirst", m(Some(30), None)),
            ("ReplicatedStorage", m(Some(30), None)),
            ("ServerScriptService", m(Some(30), None)),
            ("ServerStorage", m(Some(30), None)),
            ("StarterGui", m(Some(30), None)),
            ("StarterPack", m(Some(30), None)),
            ("StarterPlayer", m(Some(30), None)),
            ("Teams", m(Some(140), None)),
            ("SoundService", m(Some(500), None)),
            ("TextChatService", m(Some(511), None)),
            ("Chat", m(Some(510), Some(2))),
            ("VoiceChatService", m(Some(520), Some(2))),
            ("LocalizationService", m(Some(530), Some(2))),
            ("TestService", m(Some(1000), Some(2))),
            ("VRService", m(Some(1000), Some(2))),
            ("Packages", m(None, Some(1))),
            ("CoreGui", m(Some(30), None)),
            ("PluginGuiService", m(Some(30), None)),
            ("PluginDebugService", m(Some(30), None)),
            ("Debris", m(None, None)),
            ("MarketplaceService", m(None, None)),
            ("RunService", m(None, None)),
            ("TeleportService", m(None, None)),
            ("ProximityPromptService", m(None, None)),
        ])
    }

    fn check(class: &str, kids: bool, lists: &PlaceLists, prefs: Prefs) -> bool {
        let t = meta_table();
        let meta = |c: &str| t.get(c).copied();
        // Classes without an entry of their own derive from Instance here.
        let sup = |c: &str| (c != "Instance").then(|| "Instance".to_string());
        visible(class, kids, lists, prefs, &meta, &sup)
    }

    const STUDIO_DEFAULT: &[&str] = &[
        "Workspace", "Players", "Lighting", "MaterialService", "ReplicatedFirst", "ReplicatedStorage",
        "ServerScriptService", "ServerStorage", "StarterGui", "StarterPack", "StarterPlayer", "Teams",
        "SoundService", "TextChatService",
    ];

    /// A test place's DataModel children in Studio (game:GetChildren(), 123
    /// with FilteredSelection) minus the ones listed: none of these show.
    const STUDIO_HIDDEN: &[&str] = &[
        "AnalyticsService", "AssetService", "BadgeService", "Chat", "CollectionService", "CoreGui",
        "Debris", "HttpService", "InsertService", "LocalizationService", "MarketplaceService", "Packages",
        "PluginDebugService", "PluginGuiService", "ProximityPromptService", "RunService", "SafetyService",
        "ScriptCommitService", "ScriptProfilerService", "ScriptScannerService", "ServiceVisibilityService",
        "SlimAnimationReplicationService", "SlimReplicationService", "SocialService", "StudioData",
        "TeamCreatePublishService", "TeleportService", "TestService", "UGCAvatarService",
        "VersionControlService", "VideoService", "VoiceChatService", "VRService", "Instance",
        "CorePackages", "InternalSyncService", "RobloxPluginGuiService",
    ];

    #[test]
    fn default_place_lists_studios_fourteen() {
        let none = PlaceLists::default();
        for c in STUDIO_DEFAULT {
            assert!(check(c, false, &none, Prefs::default()), "{c} should be listed");
        }
        for c in STUDIO_HIDDEN {
            assert!(!check(c, false, &none, Prefs::default()), "{c} should not be listed");
        }
    }

    #[test]
    fn with_children_services_show_once_they_have_children() {
        let none = PlaceLists::default();
        for c in ["Chat", "TestService", "VoiceChatService", "LocalizationService", "VRService"] {
            assert!(!check(c, false, &none, Prefs::default()), "{c} empty");
            assert!(check(c, true, &none, Prefs::default()), "{c} with children");
        }
        // Off and no-ExplorerOrder classes stay hidden with children.
        for c in ["Packages", "Debris", "InsertService"] {
            assert!(!check(c, true, &none, Prefs::default()), "{c}");
        }
    }

    #[test]
    fn place_lists_override_metadata() {
        // Measured: VisibleServices {VideoService, Debris} and HiddenServices
        // {Teams} in a place file show both and drop Teams.
        let lists = PlaceLists {
            visible: vec!["VideoService".into(), "Debris".into()],
            hidden: vec!["Teams".into()],
        };
        assert!(check("VideoService", false, &lists, Prefs::default()));
        assert!(check("Debris", false, &lists, Prefs::default()));
        assert!(!check("Teams", false, &lists, Prefs::default()));
        // Forced-off classes stay off even when the place lists them.
        let core = PlaceLists { visible: vec!["CoreGui".into()], hidden: vec![] };
        assert!(!check("CoreGui", false, &core, Prefs::default()));
    }

    #[test]
    fn explorer_settings() {
        let none = PlaceLists::default();
        let hidden = Prefs { show_hidden: true, ..Prefs::default() };
        // Show Hidden Objects alone lists everything (CoreGui and the
        // plugin services included), except the internal-only two.
        for c in STUDIO_HIDDEN {
            let want = !matches!(*c, "CorePackages" | "InternalSyncService");
            assert_eq!(check(c, false, &none, hidden), want, "{c}");
        }
        let gui = Prefs { show_plugin_gui: true, ..Prefs::default() };
        assert!(check("PluginGuiService", false, &none, gui));
        assert!(!check("PluginDebugService", false, &none, gui));
        let core = Prefs { show_core_gui: true, ..Prefs::default() };
        assert!(check("CoreGui", false, &none, core));
        // The place's HiddenServices still wins over Show Hidden Objects.
        let lists = PlaceLists { visible: vec![], hidden: vec!["Teams".into()] };
        assert!(!check("Teams", false, &lists, hidden));
    }

    /// RUSTUDIO_SVC_PLACES=a.rbxl:b.rbxl: each place lists Studio's 14
    /// services (measured for two test places), and saving and
    /// reopening keeps every DataModel child and the ServiceVisibilityService
    /// bytes (hidden services still save).
    #[test]
    #[ignore]
    fn place_files_match_studio_and_round_trip() {
        let Ok(paths) = std::env::var("RUSTUDIO_SVC_PLACES") else { return };
        for p in paths.split(':') {
            let doc = Document::open(std::path::Path::new(p)).expect("open");
            let names = |d: &Document, refs: Vec<Ref>| {
                let mut v: Vec<String> = refs.iter().filter_map(|r| d.dom.get_by_ref(*r)).map(|i| i.class.to_string()).collect();
                v.sort();
                v
            };
            let listed = names(&doc, listed_root_children(&doc, Prefs::default()));
            let mut want: Vec<String> = STUDIO_DEFAULT.iter().map(|s| s.to_string()).collect();
            want.sort();
            assert_eq!(listed, want, "{p}");
            let all = |d: &Document| {
                let mut v: Vec<(String, String)> = d
                    .dom
                    .root()
                    .children()
                    .iter()
                    .filter_map(|r| d.dom.get_by_ref(*r))
                    .filter(|i| !rustudio_core::doc::UNSAVED_SERVICES.contains(&i.class.as_str()))
                    .map(|i| (i.class.to_string(), i.name.to_string()))
                    .collect();
                v.sort();
                v
            };
            let svs = |d: &Document| {
                d.dom.root().children().iter().filter_map(|r| d.dom.get_by_ref(*r)).find(|i| i.class == "ServiceVisibilityService").map(
                    |i| {
                        let mut v: Vec<String> = i.properties.iter().map(|(k, v)| format!("{k}={v:?}")).collect();
                        v.sort();
                        v
                    },
                )
            };
            let bytes = doc.encode_all(false).expect("encode");
            let back = Document::from_place_bytes(&bytes, "round trip".into()).expect("reopen");
            assert_eq!(all(&doc), all(&back), "{p}: DataModel children after save");
            assert_eq!(svs(&doc), svs(&back), "{p}: ServiceVisibilityService after save");
            assert_eq!(names(&back, listed_root_children(&back, Prefs::default())), want, "{p} reopened");
        }
    }

    #[test]
    fn list_encoding_round_trips() {
        // Bytes Studio saved for an empty list, and a two-item list.
        assert!(decode_list(&[0, 0, 0, 0]).is_empty());
        assert!(decode_list(&[]).is_empty());
        let b = encode_list(&["VideoService", "Debris"]);
        assert_eq!(&b[..10], &[2, 0, 0, 0, 2, 12, 0, 0, 0, b'V']);
        assert_eq!(decode_list(&b), vec!["VideoService".to_string(), "Debris".to_string()]);
        // Truncated data keeps what parsed.
        assert_eq!(decode_list(&b[..b.len() - 2]), vec!["VideoService".to_string()]);
    }
}
