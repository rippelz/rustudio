//! The Toolbox's tabs and categories, as Studio's Toolbox (0.747,
//! Toolbox_25017 Category.lua and the Creator Store's home configuration,
//! checks/toolbox/ref/NOTES.md) lists them, each mapped to its asset type
//! and the routes that list it.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, Default)]
pub enum Tab {
    #[default]
    Store,
    Inventory,
    Recent,
    Creations,
}

impl Tab {
    pub const ALL: [Tab; 4] = [Tab::Store, Tab::Inventory, Tab::Recent, Tab::Creations];
    pub fn label(self) -> &'static str {
        match self {
            Tab::Store => "Creator Store",
            Tab::Inventory => "Inventory",
            Tab::Recent => "Recent",
            Tab::Creations => "Creations",
        }
    }
}

/// What kind of asset a category lists (decides the routes and the insert).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, Default)]
pub enum Kind {
    #[default]
    Model,
    Audio,
    Decal,
    MeshPart,
    Plugin,
    Font,
    Video,
    Animation,
    Package,
}

impl Kind {
    pub const ALL: [Kind; 9] = [Kind::Model, Kind::Audio, Kind::Decal, Kind::MeshPart, Kind::Plugin, Kind::Font, Kind::Video, Kind::Animation, Kind::Package];

    /// Enum.AssetType value.
    pub fn asset_type(self) -> u32 {
        match self {
            Kind::Model | Kind::Package => 10,
            Kind::Audio => 3,
            Kind::Decal => 13,
            Kind::MeshPart => 40,
            Kind::Plugin => 38,
            Kind::Font => 73,
            Kind::Video => 62,
            Kind::Animation => 24,
        }
    }

    pub fn from_asset_type(t: u32) -> Option<Kind> {
        Some(match t {
            10 => Kind::Model,
            3 => Kind::Audio,
            13 | 1 => Kind::Decal,
            40 | 4 => Kind::MeshPart,
            38 => Kind::Plugin,
            73 => Kind::Font,
            62 => Kind::Video,
            24 => Kind::Animation,
            _ => return None,
        })
    }

    /// `searchCategoryType` of the v2 search; None: not searchable there
    /// (Animation, Package use the v1 routes).
    pub fn v2(self) -> Option<&'static str> {
        Some(match self {
            Kind::Model | Kind::Package => "Model",
            Kind::Audio => "Audio",
            Kind::Decal => "Decal",
            Kind::MeshPart => "MeshPart",
            Kind::Plugin => "Plugin",
            Kind::Font => "FontFamily",
            Kind::Video => "Video",
            Kind::Animation => return None,
        })
    }

    /// API name of the inventory / recent routes (Category.lua API_NAMES).
    pub fn api(self) -> &'static str {
        match self {
            Kind::Model => "model",
            Kind::Audio => "audio",
            Kind::Decal => "decal",
            Kind::MeshPart => "meshpart",
            Kind::Plugin => "plugin",
            Kind::Font => "fontFamily",
            Kind::Video => "video",
            Kind::Animation => "animation",
            Kind::Package => "package",
        }
    }

    /// Asset type name of the creations route.
    pub fn creations_name(self) -> &'static str {
        match self {
            Kind::Model => "Model",
            Kind::Audio => "Audio",
            Kind::Decal => "Decal",
            Kind::MeshPart => "MeshPart",
            Kind::Plugin => "Plugin",
            Kind::Font => "FontFamily",
            Kind::Video => "Video",
            Kind::Animation => "Animation",
            Kind::Package => "Package",
        }
    }

    /// `cat` of autocomplete-studio (Category.lua AUTOCOMPLETE_API_NAMES).
    pub fn autocomplete(self) -> &'static str {
        match self {
            Kind::Model | Kind::Package | Kind::Animation => "model",
            Kind::Audio => "audio",
            Kind::Decal => "image",
            Kind::MeshPart => "mesh",
            Kind::Plugin => "plugin",
            Kind::Font => "fontFamily",
            Kind::Video => "video",
        }
    }

    /// productType of the purchase request for a free asset (FiatUtil.lua).
    pub fn product_type(self) -> Option<&'static str> {
        Some(match self {
            Kind::Model | Kind::Package => "PRODUCT_TYPE_MODEL",
            Kind::Audio => "PRODUCT_TYPE_AUDIO",
            Kind::Decal => "PRODUCT_TYPE_DECAL",
            Kind::MeshPart => "PRODUCT_TYPE_MESH_PART",
            Kind::Plugin => "PRODUCT_TYPE_PLUGIN",
            Kind::Font => "PRODUCT_TYPE_FONT_FAMILY",
            Kind::Video => "PRODUCT_TYPE_VIDEO",
            Kind::Animation => return None,
        })
    }

    /// The word Studio's detail table uses ("Model Details").
    pub fn noun(self) -> &'static str {
        match self {
            Kind::Model => "Model",
            Kind::Audio => "Audio",
            Kind::Decal => "Decal",
            Kind::MeshPart => "Mesh",
            Kind::Plugin => "Plugin",
            Kind::Font => "Font",
            Kind::Video => "Video",
            Kind::Animation => "Animation",
            Kind::Package => "Package",
        }
    }

    /// The main button: Install for plugins, else Insert.
    pub fn action(self) -> &'static str {
        if self == Kind::Plugin { "Install" } else { "Insert" }
    }
}

/// A Creator Store category (the hamburger tree, the category tiles and
/// pills): what its results page searches.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoreCat {
    /// Studio's categoryPath ("3d", "3d__characters", "audio").
    pub key: &'static str,
    pub label: &'static str,
    pub kind: Kind,
    /// Sent as `categoryPath` (None: the whole asset type).
    pub category_path: Option<&'static str>,
    /// Sent as `assetSubTypes` (Packages, Ads).
    pub sub_type: Option<&'static str>,
    /// One-line description under the breadcrumb.
    pub blurb: &'static str,
    pub icon: &'static str,
}

const fn cat(key: &'static str, label: &'static str, kind: Kind, path: Option<&'static str>) -> StoreCat {
    StoreCat { key, label, kind, category_path: path, sub_type: None, blurb: "", icon: "" }
}

const fn top(key: &'static str, label: &'static str, kind: Kind, path: Option<&'static str>, blurb: &'static str, icon: &'static str) -> StoreCat {
    StoreCat { key, label, kind, category_path: path, sub_type: None, blurb, icon }
}

/// The six top categories (the home's tiles, in Studio's order).
pub const TOP: &[StoreCat] = &[
    top("3d", "3D Assets", Kind::Model, Some("3d"), "Bring life to your games with models of various characters, objects, and environments.", "cube"),
    top("visual-effects", "Visual Effects", Kind::Model, Some("visual-effects"), "Materials, textures, particles and skies to set the look of your world.", "fx"),
    top("2d", "2D Assets", Kind::Decal, None, "Images, videos and fonts for surfaces and user interfaces.", "image"),
    top("gameplay", "Gameplay", Kind::Model, Some("gameplay"), "Ready-made mechanics, systems and interfaces for your experience.", "gameplay"),
    top("plugins", "Plugins", Kind::Plugin, None, "Studio extensions that enhance workflows and speed up your creative process.", "plugin"),
    top("audio", "Audio", Kind::Audio, None, "Wide sound library of music and sound effects that fit any mood.", "audio"),
];

/// Each top category's children (the hamburger tree / the category page's pills).
pub fn children(top_key: &str) -> Vec<StoreCat> {
    match top_key {
        "3d" => vec![
            cat("3d__characters", "Characters", Kind::Model, Some("3d__characters")),
            cat("3d__vehicles", "Vehicles", Kind::Model, Some("3d__vehicles")),
            cat("3d__weapons", "Weapons", Kind::Model, Some("3d__weapons")),
            cat("3d__structures", "Structures", Kind::Model, Some("3d__structures")),
            cat("3d__nature", "Nature", Kind::Model, Some("3d__nature")),
            cat("3d__props-and-decor", "Props and Decor", Kind::Model, Some("3d__props-and-decor")),
            cat("3d__maps", "Maps", Kind::Model, Some("3d__maps")),
            cat("3d__building-blocks", "Parts", Kind::Model, Some("3d__building-blocks")),
            cat("3d__meshparts", "MeshParts", Kind::MeshPart, None),
        ],
        "visual-effects" => vec![
            cat("visual-effects__materials", "Materials", Kind::Model, Some("visual-effects__materials")),
            cat("visual-effects__materials-and-textures", "Textured Assets", Kind::Model, Some("visual-effects__materials-and-textures")),
            cat("visual-effects__particle-effects", "Particle Effects", Kind::Model, Some("visual-effects__particle-effects")),
            cat("visual-effects__skyboxes-and-atmosphere", "Sky and Atmosphere", Kind::Model, Some("visual-effects__skyboxes-and-atmosphere")),
        ],
        "2d" => vec![
            cat("2d__decals", "Decals", Kind::Decal, None),
            cat("2d__videos", "Videos", Kind::Video, None),
            cat("2d__fonts", "Fonts", Kind::Font, None),
        ],
        "gameplay" => vec![
            cat("gameplay__mechanics", "Mechanics", Kind::Model, Some("gameplay__mechanics")),
            cat("gameplay__game-systems", "Game Systems", Kind::Model, Some("gameplay__game-systems")),
            cat("gameplay__user-interface", "User Interface", Kind::Model, Some("gameplay__user-interface")),
            cat("gameplay__utilities", "Utilities", Kind::Model, Some("gameplay__utilities")),
            StoreCat { sub_type: Some("Ad"), ..cat("gameplay__ads", "Ads", Kind::Model, None) },
            StoreCat { sub_type: Some("Package"), ..cat("gameplay__packages", "Packages", Kind::Package, None) },
        ],
        "plugins" => vec![
            cat("plugins__building", "Building", Kind::Plugin, Some("plugins__building")),
            cat("plugins__scripting", "Scripting", Kind::Plugin, Some("plugins__scripting")),
            cat("plugins__ai-tools", "AI Tools", Kind::Plugin, Some("plugins__ai-tools")),
            cat("plugins__animation", "Animation", Kind::Plugin, Some("plugins__animation")),
            cat("plugins__workflow", "Workflow", Kind::Plugin, Some("plugins__workflow")),
            cat("plugins__avatar", "Avatar", Kind::Plugin, Some("plugins__avatar")),
            cat("plugins__visuals", "Lighting & Atmosphere", Kind::Plugin, Some("plugins__visuals")),
            cat("plugins__user-interface", "User Interface", Kind::Plugin, Some("plugins__user-interface")),
        ],
        _ => Vec::new(),
    }
}

/// A category by its key (top or child; "models" is Studio's plain Models
/// search from Home; "animations" the v1 Animation list).
pub fn find(key: &str) -> Option<StoreCat> {
    if let Some(t) = TOP.iter().find(|c| c.key == key) {
        return Some(t.clone());
    }
    for t in TOP {
        if let Some(c) = children(t.key).into_iter().find(|c| c.key == key) {
            return Some(c);
        }
    }
    Some(match key {
        "models" | "model" => cat("models", "Models", Kind::Model, None),
        "audio" => TOP[5].clone(),
        "decals" | "decal" | "images" => cat("2d__decals", "Decals", Kind::Decal, None),
        "meshes" | "meshparts" | "meshpart" => cat("3d__meshparts", "MeshParts", Kind::MeshPart, None),
        "plugins" | "plugin" => TOP[4].clone(),
        "videos" | "video" => cat("2d__videos", "Videos", Kind::Video, None),
        "fonts" | "font" => cat("2d__fonts", "Fonts", Kind::Font, None),
        "animations" | "animation" => cat("animations", "Animations", Kind::Animation, None),
        "packages" | "package" => StoreCat { sub_type: Some("Package"), ..cat("gameplay__packages", "Packages", Kind::Package, None) },
        _ => return None,
    })
}

/// The line under the breadcrumb (Studio's category descriptions; the
/// sub-categories Studio showed have their own).
pub fn blurb(key: &str) -> &'static str {
    match key {
        "3d__vehicles" => "Cars, planes, boats, and other modes of transportation.",
        "3d__nature" => "Models of plants, terrain, and wildlife.",
        "2d__decals" => "Static images and texture assets.",
        "plugins" => "Studio extensions that enhance workflows and speed up your creative process.",
        "models" => "",
        k => top_of(k).blurb,
    }
}

/// The top category a key belongs to.
pub fn top_of(key: &str) -> &'static StoreCat {
    let head = key.split("__").next().unwrap_or(key);
    TOP.iter().find(|c| c.key == head).unwrap_or(&TOP[0])
}

/// One entry of the Inventory / Recent / Creations dropdowns.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ListCat {
    pub label: &'static str,
    pub kind: Kind,
    /// A group's items (the group picker shows).
    pub group: bool,
}

const fn lc(label: &'static str, kind: Kind, group: bool) -> ListCat {
    ListCat { label, kind, group }
}

pub const INVENTORY: &[ListCat] = &[
    lc("My Models", Kind::Model, false),
    lc("My Decals", Kind::Decal, false),
    lc("My Meshes", Kind::MeshPart, false),
    lc("My Audio", Kind::Audio, false),
    lc("My Plugins", Kind::Plugin, false),
    lc("My Animations", Kind::Animation, false),
    lc("My Fonts", Kind::Font, false),
    lc("My Videos", Kind::Video, false),
    lc("My Packages", Kind::Package, false),
    lc("Group Models", Kind::Model, true),
    lc("Group Decals", Kind::Decal, true),
    lc("Group Meshes", Kind::MeshPart, true),
    lc("Group Audio", Kind::Audio, true),
    lc("Group Plugins", Kind::Plugin, true),
    lc("Group Packages", Kind::Package, true),
];

pub const RECENT: &[ListCat] = &[
    lc("Recent Models", Kind::Model, false),
    lc("Recent Audio", Kind::Audio, false),
    lc("Recent Decals", Kind::Decal, false),
    lc("Recent MeshParts", Kind::MeshPart, false),
    lc("Recent Animations", Kind::Animation, false),
    lc("Recent Videos", Kind::Video, false),
];

pub const CREATIONS: &[ListCat] = &[
    lc("Models", Kind::Model, false),
    lc("Plugins", Kind::Plugin, false),
    lc("Audio", Kind::Audio, false),
    lc("Decals", Kind::Decal, false),
    lc("Meshes", Kind::MeshPart, false),
    lc("Animations", Kind::Animation, false),
    lc("Videos", Kind::Video, false),
    lc("Packages", Kind::Package, false),
    lc("Group Models", Kind::Model, true),
    lc("Group Plugins", Kind::Plugin, true),
    lc("Group Audio", Kind::Audio, true),
    lc("Group Decals", Kind::Decal, true),
    lc("Group Meshes", Kind::MeshPart, true),
    lc("Group Animations", Kind::Animation, true),
    lc("Group Videos", Kind::Video, true),
    lc("Group Packages", Kind::Package, true),
];

pub fn list_cats(tab: Tab) -> &'static [ListCat] {
    match tab {
        Tab::Inventory => INVENTORY,
        Tab::Recent => RECENT,
        Tab::Creations => CREATIONS,
        Tab::Store => &[],
    }
}

/// The audio page's tiles (home configuration of asset type 3, recorded
/// 2026-11-14): (section, [(key, label, keyword, audio type 0 music / 1 sfx, colour)]).
pub struct AudioTile {
    pub key: &'static str,
    pub label: &'static str,
    pub keyword: &'static str,
    pub audio_type: Option<u8>,
    pub color: u32,
    pub sort_new: bool,
}

const fn at(key: &'static str, label: &'static str, keyword: &'static str, audio_type: Option<u8>, color: u32, sort_new: bool) -> AudioTile {
    AudioTile { key, label, keyword, audio_type, color, sort_new }
}

pub const AUDIO_SECTIONS: &[(&str, &[AudioTile])] = &[
    ("Discover New Audio", &[
        at("todays-new-music", "Today's New Music", "", Some(0), 0x1F2024, true),
        at("trending-music", "Trending Music", "", Some(0), 0x692E3D, false),
        at("trending-sfx", "Trending SFX", "", Some(1), 0x006939, false),
    ]),
    ("Genres", &[
        at("electronic", "Electronic", "electronic music", Some(0), 0x005D92, true),
        at("ambient", "Ambient", "ambient music", Some(0), 0x00456D, true),
        at("pop", "Pop", "pop music", Some(0), 0x009595, true),
        at("hip-hop", "Hip Hop", "hip hop music", Some(0), 0xC73A8A, true),
        at("indie", "Indie", "indie music", Some(0), 0xD8A009, true),
        at("funk", "Funk", "funk music", Some(0), 0xE96113, true),
        at("rock", "Rock", "rock music", Some(0), 0xAA6EC2, true),
        at("lofi", "Lofi", "lofi music", Some(0), 0x0074B6, true),
        at("jazz", "Jazz", "jazz music", Some(0), 0x581DD2, true),
        at("acoustic", "Acoustic", "acoustic music", Some(0), 0x743109, true),
        at("country", "Country", "country music", Some(0), 0xC2A748, true),
        at("metal", "Metal", "metal music", Some(0), 0xA2120B, true),
        at("r-and-b", "R&B", "r&b music", Some(0), 0x2C702A, true),
        at("rap", "Rap", "rap music", Some(0), 0x46188C, true),
        at("folk", "Folk", "folk music", Some(0), 0x6B827C, true),
        at("holiday", "Holiday", "holiday music", Some(0), 0x6D1161, true),
        at("classical", "Classical", "classical music", Some(0), 0xAD8007, true),
        at("world", "World", "world music", Some(0), 0x007777, true),
    ]),
    ("Vibes", &[
        at("exciting", "Exciting", "action music", None, 0x008347, false),
        at("chill", "Chill", "tender music", None, 0x008BDB, false),
        at("suspenseful", "Suspenseful", "creepy music", None, 0x00456D, false),
        at("sad", "Sad", "sad music", None, 0x6D34E3, false),
        at("hopeful", "Hopeful", "hopeful music", None, 0xC2510F, false),
        at("angry", "Angry", "angry music", None, 0x7A0D08, false),
        at("scary", "Scary", "horror music", None, 0x565656, false),
        at("happy", "Happy", "happy music", None, 0xB61CA2, false),
        at("easy-listening", "Easy Listening", "easy listening music", None, 0x007777, false),
        at("dark", "Dark", "dark brooding music", None, 0x3B3B3B, false),
    ]),
    ("SFX", &[
        at("horror", "Horror", "scary sfx", None, 0x3B3B3B, false),
        at("alarm", "Alarm", "alarm sfx", None, 0x7A0D08, false),
        at("car", "Car", "car sfx", None, 0x005D92, false),
        at("explosion", "Explosion", "explosion sfx", None, 0xAD8007, false),
        at("guns", "Guns", "gun sfx", None, 0x3B138C, false),
        at("animal", "Animal", "creature sfx", None, 0x008347, false),
        at("user-interface", "User Interface", "user interface sfx", None, 0x921781, false),
        at("cartoon", "Cartoon", "cartoon sfx", None, 0x009595, false),
        at("industrial", "Industrial", "industrial sfx", None, 0x2C2C2C, false),
        at("city", "City", "city ambience sfx", None, 0xAD8007, false),
        at("birds", "Birds", "bird song sfx", None, 0x005D92, false),
        at("mechanical", "Mechanical", "machine hum sfx", None, 0x005959, false),
        at("train", "Train", "rail train sfx", None, 0x006939, false),
        at("magic", "Magic", "magic sfx", None, 0xB61CA2, false),
    ]),
];

/// The home's swimlanes (home configuration of Models).
pub const HOME_SECTIONS: &[(&str, &str)] = &[("trending", "Trending"), ("priced", "Shop New"), ("essential", "Essential")];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn categories_map() {
        assert_eq!(find("3d__vehicles").unwrap().category_path, Some("3d__vehicles"));
        assert_eq!(find("audio").unwrap().kind, Kind::Audio);
        assert_eq!(find("2d__fonts").unwrap().kind.v2(), Some("FontFamily"));
        assert_eq!(find("animations").unwrap().kind.v2(), None);
        assert_eq!(top_of("plugins__building").label, "Plugins");
        for k in Kind::ALL {
            assert_eq!(Kind::from_asset_type(k.asset_type()).map(|x| x.asset_type()), Some(k.asset_type()));
        }
        assert_eq!(list_cats(Tab::Recent).len(), 6);
        assert!(find("nope").is_none());
    }
}
