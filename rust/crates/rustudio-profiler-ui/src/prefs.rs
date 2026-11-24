//! What the user customised, saved with the rustudio settings (key
//! "profiler_ui"; the capture engine's own settings are under "profiler").

use rustudio_core::profiler::Cat;
use serde::{Deserialize, Serialize};

pub const SETTINGS_KEY: &str = "profiler_ui";

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum TabId {
    Timeline,
    Table,
    Counters,
    Gpu,
    Scripts,
}

impl TabId {
    pub const ALL: [TabId; 5] = [TabId::Timeline, TabId::Table, TabId::Counters, TabId::Gpu, TabId::Scripts];
    pub fn title(self) -> &'static str {
        match self {
            TabId::Timeline => "Timeline",
            TabId::Table => "Table",
            TabId::Counters => "Counters",
            TabId::Gpu => "GPU",
            TabId::Scripts => "Scripts",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Labels {
    WhenTheyFit,
    Always,
    Never,
}

impl Labels {
    pub const ALL: [Labels; 3] = [Labels::WhenTheyFit, Labels::Always, Labels::Never];
    pub fn title(self) -> &'static str {
        match self {
            Labels::WhenTheyFit => "When they fit",
            Labels::Always => "Always",
            Labels::Never => "Never",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum TooltipDetail {
    Full,
    Short,
    Off,
}

impl TooltipDetail {
    pub const ALL: [TooltipDetail; 3] = [TooltipDetail::Full, TooltipDetail::Short, TooltipDetail::Off];
    pub fn title(self) -> &'static str {
        match self {
            TooltipDetail::Full => "Full",
            TooltipDetail::Short => "Short",
            TooltipDetail::Off => "Off",
        }
    }
}

/// A named set of visible categories and lanes.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Preset {
    pub name: String,
    pub hidden_cats: Vec<Cat>,
    pub hidden_lanes: Vec<String>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Prefs {
    pub tab: TabId,
    pub style: crate::Style,
    pub hidden_cats: Vec<Cat>,
    /// Lanes by thread name.
    pub hidden_lanes: Vec<String>,
    /// Lanes in this order first (by thread name), the rest after by kind.
    pub lane_order: Vec<String>,
    pub hidden_tabs: Vec<TabId>,
    pub pinned_counters: Vec<String>,
    pub counter_order: Vec<String>,
    /// "Capture N frames".
    pub capture_frames: u32,
    pub labels: Labels,
    pub tooltip: TooltipDetail,
    pub presets: Vec<Preset>,
    pub table_mode: u8,
    pub table_agg: u8,
    pub counters_list: bool,
    pub counters_window: u8,
    /// The sidebar's width on the Timeline.
    pub side_width: f32,
}

impl Default for Prefs {
    fn default() -> Self {
        Prefs {
            tab: TabId::Timeline,
            style: crate::Style::default(),
            hidden_cats: Vec::new(),
            hidden_lanes: Vec::new(),
            lane_order: Vec::new(),
            hidden_tabs: Vec::new(),
            pinned_counters: vec![
                "instances".into(),
                "draw calls (all)".into(),
                "Luau heap".into(),
                "physics contact pairs".into(),
            ],
            counter_order: Vec::new(),
            capture_frames: 300,
            labels: Labels::WhenTheyFit,
            tooltip: TooltipDetail::Full,
            presets: Vec::new(),
            table_mode: 0,
            table_agg: 0,
            counters_list: false,
            counters_window: 1,
            side_width: crate::theme::SIDE_W,
        }
    }
}

impl Prefs {
    pub fn load() -> Prefs {
        rustudio_core::rustudio_settings::saved_value(SETTINGS_KEY)
            .and_then(|v| serde_json::from_value(v).ok())
            .unwrap_or_default()
    }

    pub fn save(&self) {
        let v = serde_json::to_value(self).ok();
        let default = serde_json::to_value(Prefs::default()).ok();
        rustudio_core::rustudio_settings::set(SETTINGS_KEY, v.filter(|v| Some(v) != default.as_ref()));
    }

    pub fn cat_visible(&self, c: Cat) -> bool {
        !self.hidden_cats.contains(&c)
    }

    /// The built-in presets, then the user's.
    pub fn all_presets(&self) -> Vec<Preset> {
        let only = |keep: &[Cat]| Cat::ALL.iter().copied().filter(|c| !keep.contains(c) && *c != Cat::Frame && *c != Cat::Wait).collect();
        let mut v = vec![
            Preset { name: "Everything".into(), hidden_cats: vec![], hidden_lanes: vec![] },
            Preset { name: "Scripts only".into(), hidden_cats: only(&[Cat::Script]), hidden_lanes: vec![] },
            Preset { name: "Rendering".into(), hidden_cats: only(&[Cat::Render, Cat::Gpu, Cat::Ui, Cat::Gui]), hidden_lanes: vec![] },
            Preset { name: "Physics".into(), hidden_cats: only(&[Cat::Physics, Cat::Animation]), hidden_lanes: vec![] },
        ];
        v.extend(self.presets.iter().cloned());
        v
    }
}
