//! rustudio's own options (File > Studio Settings > "rustudio" page): the
//! behaviours that were only reachable through `RUSTUDIO_*` environment
//! variables. Roblox Studio's settings stay in src/studio_settings.rs; these
//! are rustudio's additions and live in their own file,
//! `$XDG_CONFIG_HOME/rustudio/rustudio-settings.json` (only values that differ
//! from the defaults; real Studio never reads it).
//!
//! Every option is read through one function below: the environment variable
//! when set (always wins, so test harnesses keep their switches),
//! then the saved value, then the default (today's behaviour).
//!
//! The saved file is loaded only by `init(true)`, which main calls for runs a
//! person drives (`layout_store::enabled`). Test, harness, bench, headless and
//! tool runs never call it with true, and `RUSTUDIO_DETERMINISTIC` refuses it
//! outright, so a choice made in the dialog can't change what a test measures.
//! `RUSTUDIO_SETTINGS_FILE=<file>` picks the file for any other run (the UI
//! checks use it), still not under `RUSTUDIO_DETERMINISTIC`.
//!
//! Options a Play session keeps (solver, spin guard, client link, live data) are
//! latched at Play start (`latch_for_play`), so a change applies at the next
//! Play and never in the middle of a session.

use std::path::PathBuf;
use std::sync::atomic::{AtomicU8, Ordering};
use std::sync::RwLock;
use std::time::Instant;

use serde_json::Value;

/// Which solver steps the bodies (`RUSTUDIO_SOLVER`, read once per process).
/// Unset or empty = `studio` (the default since the solver landed on master);
/// `rapier` = the old physics, unchanged.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SolverKind {
    /// `rapier`: rapier's PhysicsPipeline (collision + solver + integration), the
    /// physics before the port; no longer the default.
    Rapier,
    /// `studio` (default; also unset / empty): Studio's legacy solver (dual LDL + PGS, "Solve Batch"), what Studio
    /// 0.747 runs with workspace.NextGenerationPhysicsSolver = Default. Hooks: DUAL,
    /// DUALROWS, JOINT, STEP.
    Studio,
    /// `studio-nextgen`: the primal ("next generation") solver Roblox is rolling
    /// out (NextGenerationPhysicsSolver = Enabled). Hooks: ITER, LINSYS, CONTACT,
    /// JOINT, STEP.
    StudioNextGen,
}

impl SolverKind {
    pub fn name(self) -> &'static str {
        match self {
            SolverKind::Rapier => "rapier",
            SolverKind::Studio => "studio",
            SolverKind::StudioNextGen => "studio-nextgen",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum OptKind {
    Bool,
    /// (saved value, shown text)
    Choice(&'static [(&'static str, &'static str)]),
    Path,
}

pub struct Opt {
    pub key: &'static str,
    pub cat: &'static str,
    pub label: &'static str,
    pub desc: &'static str,
    /// The environment variable that overrides it.
    pub env: &'static str,
    pub kind: OptKind,
}

pub const SOLVERS: &[(&str, &str)] = &[("studio", "Studio solver (default)"), ("rapier", "Legacy (rapier)")];

/// Implementation choice for a tool that exists both natively in rustudio and
/// as a Roblox Studio built-in plugin.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ToolImpl {
    #[default]
    Rustudio,
    Roblox,
}

impl ToolImpl {
    pub fn id(self) -> &'static str {
        match self {
            ToolImpl::Rustudio => "rustudio",
            ToolImpl::Roblox => "roblox",
        }
    }
}

pub const TOOL_IMPLS: &[(&str, &str)] = &[("rustudio", "rustudio's"), ("roblox", "Roblox's")];

/// Duplicated Studio tools that can use either rustudio's native
/// implementation or Roblox's built-in plugin when built-in plugins are on.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum BuiltinTool {
    Toolbox,
    TerrainEditor,
    Draggers,
    ViewSelector,
    RigBuilder,
    MaterialPicker,
}

impl BuiltinTool {
    pub const ALL: [BuiltinTool; 6] = [
        BuiltinTool::Toolbox,
        BuiltinTool::TerrainEditor,
        BuiltinTool::Draggers,
        BuiltinTool::ViewSelector,
        BuiltinTool::RigBuilder,
        BuiltinTool::MaterialPicker,
    ];

    pub fn key(self) -> &'static str {
        match self {
            BuiltinTool::Toolbox => "builtin_toolbox",
            BuiltinTool::TerrainEditor => "builtin_terrain_editor",
            BuiltinTool::Draggers => "builtin_draggers",
            BuiltinTool::ViewSelector => "builtin_view_selector",
            BuiltinTool::RigBuilder => "builtin_rig_builder",
            BuiltinTool::MaterialPicker => "builtin_material_picker",
        }
    }

    pub fn env(self) -> &'static str {
        match self {
            BuiltinTool::Toolbox => "RUSTUDIO_BUILTIN_TOOLBOX",
            BuiltinTool::TerrainEditor => "RUSTUDIO_BUILTIN_TERRAIN_EDITOR",
            BuiltinTool::Draggers => "RUSTUDIO_BUILTIN_DRAGGERS",
            BuiltinTool::ViewSelector => "RUSTUDIO_BUILTIN_VIEW_SELECTOR",
            BuiltinTool::RigBuilder => "RUSTUDIO_BUILTIN_RIG_BUILDER",
            BuiltinTool::MaterialPicker => "RUSTUDIO_BUILTIN_MATERIAL_PICKER",
        }
    }

    /// Maps a Studio built-in `.rbxm` file stem to the duplicated tool it
    /// implements, if any.
    pub fn for_plugin_stem(stem: &str) -> Option<Self> {
        if stem.eq_ignore_ascii_case("Toolbox") {
            Some(BuiltinTool::Toolbox)
        } else if stem.eq_ignore_ascii_case("TerrainEditor")
            || stem.eq_ignore_ascii_case("TerrainPaletteEditor")
        {
            Some(BuiltinTool::TerrainEditor)
        } else if stem.eq_ignore_ascii_case("SelectDragger")
            || stem.eq_ignore_ascii_case("MoveDragger")
            || stem.eq_ignore_ascii_case("RotateDragger")
            || stem.eq_ignore_ascii_case("ScaleDragger")
        {
            Some(BuiltinTool::Draggers)
        } else if stem.eq_ignore_ascii_case("ViewSelector") {
            Some(BuiltinTool::ViewSelector)
        } else if stem.eq_ignore_ascii_case("RigGenerator") {
            Some(BuiltinTool::RigBuilder)
        } else if stem.eq_ignore_ascii_case("MaterialPicker") {
            Some(BuiltinTool::MaterialPicker)
        } else {
            None
        }
    }
}

/// Snapshot of the built-in plugin toggle and per-tool choices latched for a
/// place session.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct BuiltinToolChoices {
    pub plugins_enabled: bool,
    pub toolbox: ToolImpl,
    pub terrain_editor: ToolImpl,
    pub draggers: ToolImpl,
    pub view_selector: ToolImpl,
    pub rig_builder: ToolImpl,
    pub material_picker: ToolImpl,
}

impl BuiltinToolChoices {
    /// Reads the live settings (`env > saved > default`).
    pub fn from_settings() -> Self {
        Self {
            plugins_enabled: builtin_plugins(),
            toolbox: builtin_tool_impl(BuiltinTool::Toolbox),
            terrain_editor: builtin_tool_impl(BuiltinTool::TerrainEditor),
            draggers: builtin_tool_impl(BuiltinTool::Draggers),
            view_selector: builtin_tool_impl(BuiltinTool::ViewSelector),
            rig_builder: builtin_tool_impl(BuiltinTool::RigBuilder),
            material_picker: builtin_tool_impl(BuiltinTool::MaterialPicker),
        }
    }

    /// All duplicated tools set to Roblox's plugin (used by synthetic host
    /// tests and the eager 58-plugin survey).
    pub fn all_roblox() -> Self {
        Self {
            plugins_enabled: true,
            toolbox: ToolImpl::Roblox,
            terrain_editor: ToolImpl::Roblox,
            draggers: ToolImpl::Roblox,
            view_selector: ToolImpl::Roblox,
            rig_builder: ToolImpl::Roblox,
            material_picker: ToolImpl::Roblox,
        }
    }

    pub fn get(self, tool: BuiltinTool) -> ToolImpl {
        match tool {
            BuiltinTool::Toolbox => self.toolbox,
            BuiltinTool::TerrainEditor => self.terrain_editor,
            BuiltinTool::Draggers => self.draggers,
            BuiltinTool::ViewSelector => self.view_selector,
            BuiltinTool::RigBuilder => self.rig_builder,
            BuiltinTool::MaterialPicker => self.material_picker,
        }
    }

    /// True only when built-in plugins are enabled AND this tool is set to
    /// Roblox's plugin.
    pub fn use_roblox(self, tool: BuiltinTool) -> bool {
        self.plugins_enabled && self.get(tool) == ToolImpl::Roblox
    }

    pub fn use_native(self, tool: BuiltinTool) -> bool {
        !self.use_roblox(tool)
    }

    /// Whether a built-in `.rbxm` file stem should be registered/loaded under
    /// these choices. Non-duplicated built-in plugins always load when
    /// `plugins_enabled` is true.
    pub fn should_load_builtin_stem(self, stem: &str) -> bool {
        if !self.plugins_enabled {
            return false;
        }
        match BuiltinTool::for_plugin_stem(stem) {
            Some(tool) => self.use_roblox(tool),
            None => true,
        }
    }
}

pub const OPTS: &[Opt] = &[
    Opt { key: "quality", cat: "Performance", label: "Graphics quality", desc: "Automatic adjusts the viewport quality level using recent frame times. Manual uses the settings below.", env: "RUSTUDIO_PERF_QUALITY", kind: OptKind::Choice(&[("manual", "Manual"), ("automatic", "Automatic")]) },
    Opt { key: "resolution", cat: "Performance", label: "3D resolution", desc: "Scales the 3D scene while keeping editor text sharp. The biggest saving on a weak graphics chip: 50% draws a quarter of the pixels.", env: "RUSTUDIO_PERF_RESOLUTION", kind: OptKind::Choice(&[("100", "100%"), ("75", "75%"), ("50", "50%"), ("33", "33%"), ("25", "25%")]) },
    Opt { key: "detail", cat: "Performance", label: "Shading detail", desc: "Medium drops sky reflections and extra lighting work; Low also drops material maps, the light grid, local lights and multisample anti-aliasing (as Studio's lower quality levels do), keeping sun, shadow and ambient. The biggest saving on a weak graphics chip.", env: "RUSTUDIO_PERF_DETAIL", kind: OptKind::Choice(&[("high", "High"), ("medium", "Medium"), ("low", "Low")]) },
    Opt { key: "ssao", cat: "Performance", label: "Ambient occlusion", desc: "Darkens creases and corners. Off skips the screen-space pass, which costs several milliseconds a frame on a weak graphics chip.", env: "RUSTUDIO_PERF_SSAO", kind: OptKind::Choice(&[("on", "On"), ("off", "Off")]) },
    Opt { key: "shadows", cat: "Performance", label: "Shadow quality", desc: "Reduced draws smaller sun shadow tiles. Off also disables local light shadows.", env: "RUSTUDIO_PERF_SHADOWS", kind: OptKind::Choice(&[("full", "Full"), ("reduced", "Reduced"), ("off", "Off")]) },
    Opt { key: "grass", cat: "Performance", label: "Grass detail", desc: "Reduced grows fewer blades over a shorter distance.", env: "RUSTUDIO_PERF_GRASS", kind: OptKind::Choice(&[("full", "Full"), ("reduced", "Reduced"), ("off", "Off")]) },
    Opt { key: "bloom", cat: "Performance", label: "Glow effects", desc: "Controls the bloom glow around bright surfaces.", env: "RUSTUDIO_PERF_BLOOM", kind: OptKind::Choice(&[("on", "On"), ("off", "Off")]) },
    Opt { key: "fps", cat: "Performance", label: "Editor frame limit", desc: "Limits scheduled editor and playtest redraws. Input and window changes can redraw immediately.", env: "RUSTUDIO_PERF_FPS", kind: OptKind::Choice(FPS_PRESETS) },
    Opt { key: "present", cat: "Performance", label: "Presentation", desc: "Low latency shows each finished frame at the next screen refresh without waiting for one, like Studio, and paces redraws to the frame limit (the screen's refresh rate when Unlimited). Wait for vsync blocks each frame on the refresh. Applies after a restart.", env: "RUSTUDIO_PERF_PRESENT", kind: OptKind::Choice(&[("mailbox", "Low latency"), ("fifo", "Wait for vsync")]) },
    Opt { key: "idle", cat: "Performance", label: "Idle grass animation", desc: "Pauses grass animation redraws while the editor is idle.", env: "RUSTUDIO_PERF_IDLE", kind: OptKind::Choice(&[("animate", "Animate"), ("pause", "Pause when idle")]) },
    Opt { key: "background", cat: "Performance", label: "Hidden window", desc: "Keeps a running playtest and Team Create going while the window is on another workspace or minimized, without drawing (as Studio does behind other windows). Pause freezes them until the window shows again.", env: "RUSTUDIO_PERF_BACKGROUND", kind: OptKind::Choice(&[("run", "Keep running"), ("pause", "Pause")]) },
    Opt {
        key: "solver",
        cat: "Physics",
        label: "Physics solver",
        desc: "Studio solver matches Roblox; Legacy (rapier) is the old physics, for places the Studio solver doesn't cover yet. Takes effect at the next Play.",
        env: "RUSTUDIO_SOLVER",
        kind: OptKind::Choice(SOLVERS),
    },
    Opt {
        key: "spin_guard",
        cat: "Physics",
        label: "Spin guard",
        desc: "Stops thin fast-spinning parts gaining speed without limit; off gives Studio's exact rotation step. Studio solver only. Next Play.",
        env: "RUSTUDIO_SOLVER_GYRO_GUARD",
        kind: OptKind::Bool,
    },
    Opt {
        key: "client_link",
        cat: "Physics",
        label: "Client loose parts react to the character",
        desc: "Parts simulated on the client get pushed by your character during Play. Next Play.",
        env: "RUSTUDIO_NO_CLIENT_LINK",
        kind: OptKind::Bool,
    },
    Opt {
        key: "classic_death_sound",
        cat: "Assets",
        label: "Classic death sound",
        desc: "Downloads the classic \"oof\" once from Roblox's CDN and plays it for the death sound; off plays Studio's replacement.",
        env: "RUSTUDIO_NO_STUDIO_FETCH",
        kind: OptKind::Bool,
    },
    Opt {
        key: "local_assets",
        cat: "Assets",
        label: "Local asset folder",
        desc: "A folder of <id> or <id>.<ext> files, used only when Roblox's copy of an asset can't be fetched. Empty = off.",
        env: "RUSTUDIO_LOCAL_ASSETS",
        kind: OptKind::Path,
    },
    Opt {
        key: "live_data",
        cat: "Data",
        label: "Use live DataStores",
        desc: "Off: in a published place, saves made while playing stay in memory and are thrown away when Play stops. On: Play reads and writes the place's real DataStores, MemoryStores and Messaging, like Studio with API access enabled. Next Play.",
        env: "RUSTUDIO_LIVE_DATA",
        kind: OptKind::Bool,
    },
    Opt {
        key: "core_scripts",
        cat: "Play",
        label: "Roblox's CoreScripts",
        desc: "Play runs Roblox's own CoreScripts and CorePackages from Studio's data model patch (in-game chat, top bar, player list and escape menu) when installed, falling back to rustudio's built-in UI when absent or turned off. Next Play.",
        env: "RUSTUDIO_CORE_SCRIPTS",
        kind: OptKind::Bool,
    },
    Opt {
        key: "builtin_plugins",
        cat: "Plugins",
        label: "Roblox's built-in plugins (experimental)",
        desc: "Loads Roblox Studio's own built-in plugins from Studio's BuiltInPlugins folder in Edit mode when installed. Off by default. Applies when opening a place or restarting.",
        env: "RUSTUDIO_BUILTIN_PLUGINS",
        kind: OptKind::Bool,
    },
    Opt {
        key: "builtin_toolbox",
        cat: "Plugins",
        label: "Toolbox",
        desc: "Use rustudio's native Toolbox or Roblox's built-in Toolbox plugin. Applies when opening a place or restarting.",
        env: "RUSTUDIO_BUILTIN_TOOLBOX",
        kind: OptKind::Choice(TOOL_IMPLS),
    },
    Opt {
        key: "builtin_terrain_editor",
        cat: "Plugins",
        label: "Terrain Editor",
        desc: "Use rustudio's native Terrain Editor or Roblox's built-in TerrainEditor plugin. Applies when opening a place or restarting.",
        env: "RUSTUDIO_BUILTIN_TERRAIN_EDITOR",
        kind: OptKind::Choice(TOOL_IMPLS),
    },
    Opt {
        key: "builtin_draggers",
        cat: "Plugins",
        label: "Select / Move / Scale / Rotate",
        desc: "Use rustudio's native viewport tools or Roblox's built-in Select, Move, Scale and Rotate dragger plugins. Applies when opening a place or restarting.",
        env: "RUSTUDIO_BUILTIN_DRAGGERS",
        kind: OptKind::Choice(TOOL_IMPLS),
    },
    Opt {
        key: "builtin_view_selector",
        cat: "Plugins",
        label: "View cube",
        desc: "Use rustudio's native viewport orientation cube or Roblox's built-in ViewSelector plugin. Applies when opening a place or restarting.",
        env: "RUSTUDIO_BUILTIN_VIEW_SELECTOR",
        kind: OptKind::Choice(TOOL_IMPLS),
    },
    Opt {
        key: "builtin_rig_builder",
        cat: "Plugins",
        label: "Character / Rig Builder",
        desc: "Use rustudio's native Character builder or Roblox's built-in RigGenerator plugin. Applies when opening a place or restarting.",
        env: "RUSTUDIO_BUILTIN_RIG_BUILDER",
        kind: OptKind::Choice(TOOL_IMPLS),
    },
    Opt {
        key: "builtin_material_picker",
        cat: "Plugins",
        label: "Material Picker",
        desc: "Use rustudio's native Material picker or Roblox's built-in MaterialPicker plugin. Applies when opening a place or restarting.",
        env: "RUSTUDIO_BUILTIN_MATERIAL_PICKER",
        kind: OptKind::Choice(TOOL_IMPLS),
    },
    Opt {
        key: "project_any_path",
        cat: "Projects",
        label: "Let project files reach outside their repository",
        desc: "Off: a project file's $path, symlinks and write-back stay inside the git repository the project file is in (its folder when it isn't in one), so a downloaded project can't read or overwrite other files on your computer. On: any path the project names is used, as in Rojo. Applies the next time a project opens.",
        env: "RUSTUDIO_PROJECT_ANY_PATH",
        kind: OptKind::Bool,
    },
    Opt {
        key: "update_check",
        cat: "Updates",
        label: "Check for updates on start",
        desc: "Looks for a newer rustudio release on GitHub when rustudio starts. Takes effect at the next start.",
        env: "RUSTUDIO_NO_UPDATE_CHECK",
        kind: OptKind::Bool,
    },
];

pub fn opt(key: &str) -> &'static Opt {
    OPTS.iter().find(|o| o.key == key).expect("rustudio option")
}

// ------------------------------------------------------------------ store

struct Store {
    vals: Option<serde_json::Map<String, Value>>,
    file: Option<PathBuf>,
    dirty: Option<Instant>,
}

static STORE: RwLock<Store> = RwLock::new(Store { vals: None, file: None, dirty: None });

fn default_file() -> PathBuf {
    crate::paths::config().join("rustudio").join("rustudio-settings.json")
}

/// Which file a run reads, if any: `persist` (a run a person drives) gives
/// the user's file, `RUSTUDIO_SETTINGS_FILE` an explicit one; never under
/// `RUSTUDIO_DETERMINISTIC`.
fn file_for(persist: bool, deterministic: bool, explicit: Option<PathBuf>) -> Option<PathBuf> {
    if deterministic {
        return None;
    }
    explicit.or_else(|| persist.then(default_file))
}

/// Startup (main): loads the saved options for runs a person drives.
pub fn init(persist: bool) {
    let file = file_for(
        persist,
        std::env::var_os("RUSTUDIO_DETERMINISTIC").is_some(),
        std::env::var_os("RUSTUDIO_SETTINGS_FILE").map(PathBuf::from),
    );
    let Some(f) = file else { return };
    let vals = std::fs::read_to_string(&f)
        .ok()
        .and_then(|t| match serde_json::from_str::<serde_json::Map<String, Value>>(&t) {
            Ok(m) => Some(m),
            Err(e) => {
                eprintln!("rustudio: ignoring {}: {e}", f.display());
                None
            }
        })
        .unwrap_or_default();
    let mut s = STORE.write().unwrap();
    s.vals = Some(vals);
    s.file = Some(f);
}

fn saved(key: &str) -> Option<Value> {
    STORE.read().unwrap().vals.as_ref()?.get(key).cloned()
}

/// A saved value that isn't one of the dialog's options (the profiler's
/// settings and layout keep a JSON object each).
pub fn saved_value(key: &str) -> Option<Value> {
    saved(key)
}

/// The dialog: a new value (None = back to the default). Kept in memory for
/// any run; written only when a file was loaded.
pub fn set(key: &str, v: Option<Value>) {
    let mut s = STORE.write().unwrap();
    let vals = s.vals.get_or_insert_with(Default::default);
    let changed = match v {
        Some(v) => vals.insert(key.to_string(), v.clone()).as_ref() != Some(&v),
        None => vals.remove(key).is_some(),
    };
    if changed {
        s.dirty = Some(Instant::now());
    }
}

pub fn flush() {
    let (file, text) = {
        let mut s = STORE.write().unwrap();
        if s.dirty.take().is_none() {
            return;
        }
        let Some(f) = s.file.clone() else { return };
        (f, serde_json::to_string_pretty(&Value::Object(s.vals.clone().unwrap_or_default())).unwrap_or_default())
    };
    if let Some(dir) = file.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    let tmp = file.with_extension("json.tmp");
    if std::fs::write(&tmp, text).is_ok() {
        let _ = std::fs::rename(&tmp, &file);
    }
}

pub fn tick() {
    let due = STORE.read().unwrap().dirty.is_some_and(|t| t.elapsed().as_secs_f32() > 0.5);
    if due {
        flush();
    }
}

// ------------------------------------------------------------------ resolution (env > saved > default)

fn env(name: &str) -> Option<String> {
    std::env::var_os(name).map(|v| v.to_string_lossy().into_owned())
}

/// The solver: `RUSTUDIO_SOLVER` (as before: unset/empty/studio, rapier,
/// studio-nextgen; anything else studio with a note), then saved, then studio.
pub fn resolve_solver(env: Option<&str>, saved: Option<&Value>) -> SolverKind {
    match env {
        Some("studio") | Some("") => SolverKind::Studio,
        Some("studio-nextgen") => SolverKind::StudioNextGen,
        Some("rapier") => SolverKind::Rapier,
        Some(other) => {
            eprintln!("RUSTUDIO_SOLVER={other:?}: unknown (studio|rapier|studio-nextgen), using studio");
            SolverKind::Studio
        }
        None => match saved.and_then(Value::as_str) {
            Some("rapier") => SolverKind::Rapier,
            _ => SolverKind::Studio,
        },
    }
}

/// Spin guard: `RUSTUDIO_SOLVER_GYRO_GUARD=off|0` turns it off (any other
/// value leaves it on, as before), then saved, then on.
pub fn resolve_spin_guard(env: Option<&str>, saved: Option<&Value>) -> bool {
    match env {
        Some(v) => !(v == "off" || v == "0"),
        None => saved.and_then(Value::as_bool).unwrap_or(true),
    }
}

/// An on/off option whose env variable, when present at all, turns it off.
pub fn resolve_off_switch(env_present: bool, saved: Option<&Value>, default: bool) -> bool {
    if env_present {
        false
    } else {
        saved.and_then(Value::as_bool).unwrap_or(default)
    }
}

/// Local asset folder: `RUSTUDIO_LOCAL_ASSETS` when set (empty = off), then
/// saved, then off.
pub fn resolve_local_assets(env: Option<&str>, saved: Option<&Value>) -> Option<PathBuf> {
    let p = match env {
        Some(p) => p.to_string(),
        None => saved.and_then(Value::as_str).unwrap_or("").trim().to_string(),
    };
    (!p.is_empty()).then(|| PathBuf::from(p))
}

pub fn solver_now() -> SolverKind {
    resolve_solver(env("RUSTUDIO_SOLVER").as_deref(), saved("solver").as_ref())
}

pub fn spin_guard_now() -> bool {
    resolve_spin_guard(env("RUSTUDIO_SOLVER_GYRO_GUARD").as_deref(), saved("spin_guard").as_ref())
}

/// Live cloud data in a published place: `RUSTUDIO_LIVE_DATA` when set (only
/// "1" means live, as before), then saved, then off.
pub fn resolve_live_data(env: Option<&str>, saved: Option<&Value>) -> bool {
    match env {
        Some(v) => v == "1",
        None => saved.and_then(Value::as_bool).unwrap_or(false),
    }
}

/// Project paths outside the project's repository: `RUSTUDIO_PROJECT_ANY_PATH=1`
/// when set, then saved, then off (project.rs `bound`).
pub fn project_any_path() -> bool {
    match env("RUSTUDIO_PROJECT_ANY_PATH") {
        Some(v) => v == "1",
        None => saved("project_any_path").and_then(|v| v.as_bool()).unwrap_or(false),
    }
}

pub fn live_data_now() -> bool {
    resolve_live_data(env("RUSTUDIO_LIVE_DATA").as_deref(), saved("live_data").as_ref())
}

pub fn client_link_now() -> bool {
    resolve_off_switch(env("RUSTUDIO_NO_CLIENT_LINK").is_some(), saved("client_link").as_ref(), true)
}

/// Fetch the classic death sound: off under `RUSTUDIO_NO_STUDIO_FETCH`.
pub fn classic_death_sound_fetch() -> bool {
    resolve_off_switch(env("RUSTUDIO_NO_STUDIO_FETCH").is_some(), saved("classic_death_sound").as_ref(), true)
}

/// Play the classic death sound once fetched. `RUSTUDIO_NO_STUDIO_FETCH`
/// only stops downloads (as before), so with it set a fetched file still plays.
pub fn classic_death_sound_use() -> bool {
    env("RUSTUDIO_NO_STUDIO_FETCH").is_some() || saved("classic_death_sound").and_then(|v| v.as_bool()).unwrap_or(true)
}

/// The GitHub release check at start (`RUSTUDIO_UI_SHOTS` keeps it off too).
pub fn update_check() -> bool {
    env("RUSTUDIO_UI_SHOTS").is_none()
        && resolve_off_switch(env("RUSTUDIO_NO_UPDATE_CHECK").is_some(), saved("update_check").as_ref(), true)
}

pub fn local_assets() -> Option<PathBuf> {
    resolve_local_assets(env("RUSTUDIO_LOCAL_ASSETS").as_deref(), saved("local_assets").as_ref())
}

/// Roblox's CoreScripts in Play (core_scripts.rs): `RUSTUDIO_CORE_SCRIPTS`
/// when set (`0` / `false` / `off` turns them off; any other value turns them
/// on), then saved, then on by default (falling back silently to native UI when
/// Studio's DataModelPatch.rbxm is not installed).
pub fn resolve_core_scripts(env: Option<&str>, saved: Option<&Value>) -> bool {
    match env {
        Some(v) => !(v == "0" || v.eq_ignore_ascii_case("false") || v.eq_ignore_ascii_case("off")),
        None => saved.and_then(Value::as_bool).unwrap_or(true),
    }
}

pub fn core_scripts() -> bool {
    resolve_core_scripts(env("RUSTUDIO_CORE_SCRIPTS").as_deref(), saved("core_scripts").as_ref())
}

/// True only when the user explicitly forced CoreScripts on via
/// `RUSTUDIO_CORE_SCRIPTS=1` (so missing `DataModelPatch.rbxm` reports an error
/// instead of silently falling back to the native UI).
pub fn core_scripts_forced() -> bool {
    matches!(env("RUSTUDIO_CORE_SCRIPTS").as_deref(), Some(v) if !(v == "0" || v.eq_ignore_ascii_case("false") || v.eq_ignore_ascii_case("off")))
}

/// Roblox Studio's built-in plugins (experimental): `RUSTUDIO_BUILTIN_PLUGINS`
/// when set (`""` / `0` / `false` / `off` turns them off; `1` / `true` / `on`
/// or a comma-separated list of plugin names turns them on), then saved, then
/// off by default.
pub fn resolve_builtin_plugins(env: Option<&str>, saved: Option<&Value>) -> bool {
    match env {
        Some(v) => {
            let v = v.trim();
            !(v.is_empty() || v == "0" || v.eq_ignore_ascii_case("false") || v.eq_ignore_ascii_case("off"))
        }
        None => saved.and_then(Value::as_bool).unwrap_or(false),
    }
}

pub fn builtin_plugins() -> bool {
    resolve_builtin_plugins(env("RUSTUDIO_BUILTIN_PLUGINS").as_deref(), saved("builtin_plugins").as_ref())
}

/// Implementation choice for a duplicated tool: env var (`rustudio`|`roblox`,
/// also `native`/`0`/`false` and `plugin`/`1`/`true`), then saved, then
/// `ToolImpl::Rustudio`.
pub fn resolve_tool_impl(env: Option<&str>, saved: Option<&Value>) -> ToolImpl {
    match env {
        Some(v) => {
            let v = v.trim();
            if v.eq_ignore_ascii_case("roblox")
                || v.eq_ignore_ascii_case("plugin")
                || v == "1"
                || v.eq_ignore_ascii_case("true")
            {
                ToolImpl::Roblox
            } else if v.is_empty()
                || v.eq_ignore_ascii_case("rustudio")
                || v.eq_ignore_ascii_case("native")
                || v == "0"
                || v.eq_ignore_ascii_case("false")
            {
                ToolImpl::Rustudio
            } else {
                eprintln!("rustudio: unknown tool choice {v:?} (rustudio|roblox), using rustudio");
                ToolImpl::Rustudio
            }
        }
        None => match saved.and_then(Value::as_str) {
            Some(v) if v.eq_ignore_ascii_case("roblox") => ToolImpl::Roblox,
            _ => ToolImpl::Rustudio,
        },
    }
}

/// Configured choice for one duplicated tool (ignoring whether `builtin_plugins`
/// is on; used by the settings UI and `BuiltinToolChoices::from_settings`).
pub fn builtin_tool_impl(tool: BuiltinTool) -> ToolImpl {
    resolve_tool_impl(env(tool.env()).as_deref(), saved(tool.key()).as_ref())
}

/// Effective choice: true only when built-in plugins are enabled and this tool
/// is configured to use Roblox's plugin.
pub fn use_roblox_tool(tool: BuiltinTool) -> bool {
    builtin_plugins() && builtin_tool_impl(tool) == ToolImpl::Roblox
}

/// Optional comma-separated filter from `RUSTUDIO_BUILTIN_PLUGINS` (e.g.
/// `AlignmentTool,TerrainEditor`). Returns `None` when all built-in plugins
/// should load (`1` / `true` / `on` / saved setting).
pub fn builtin_plugins_filter() -> Option<Vec<String>> {
    let raw = env("RUSTUDIO_BUILTIN_PLUGINS")?;
    let v = raw.trim();
    if v.is_empty()
        || v == "0"
        || v == "1"
        || v.eq_ignore_ascii_case("true")
        || v.eq_ignore_ascii_case("false")
        || v.eq_ignore_ascii_case("on")
        || v.eq_ignore_ascii_case("off")
        || v.eq_ignore_ascii_case("all")
    {
        return None;
    }
    let list: Vec<String> = v
        .split(',')
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .collect();
    (!list.is_empty()).then_some(list)
}

// ------------------------------------------------------------------ latched per Play

/// 0 = not latched yet; otherwise 1 + value.
static SOLVER: AtomicU8 = AtomicU8::new(0);
static SPIN_GUARD: AtomicU8 = AtomicU8::new(0);
static CLIENT_LINK: AtomicU8 = AtomicU8::new(0);
static LIVE_DATA: AtomicU8 = AtomicU8::new(0);

fn latched<T: Copy>(cell: &AtomicU8, now: impl Fn() -> T, enc: impl Fn(T) -> u8, dec: impl Fn(u8) -> T) -> T {
    match cell.load(Ordering::Relaxed) {
        0 => {
            let v = now();
            // (First reader wins; a latch_for_play racing it stores the same.)
            let _ = cell.compare_exchange(0, enc(v) + 1, Ordering::Relaxed, Ordering::Relaxed);
            dec(cell.load(Ordering::Relaxed) - 1)
        }
        n => dec(n - 1),
    }
}

fn enc_solver(k: SolverKind) -> u8 {
    match k {
        SolverKind::Studio => 0,
        SolverKind::Rapier => 1,
        SolverKind::StudioNextGen => 2,
    }
}

fn dec_solver(n: u8) -> SolverKind {
    match n {
        1 => SolverKind::Rapier,
        2 => SolverKind::StudioNextGen,
        _ => SolverKind::Studio,
    }
}

/// The solver of this Play session (crate::solver::kind()).
pub fn solver() -> SolverKind {
    latched(&SOLVER, solver_now, enc_solver, dec_solver)
}

pub fn spin_guard() -> bool {
    latched(&SPIN_GUARD, spin_guard_now, u8::from, |n| n != 0)
}

/// Live DataStores for this Play session (domains::data::local_mode_for).
pub fn live_data() -> bool {
    latched(&LIVE_DATA, live_data_now, u8::from, |n| n != 0)
}

pub fn client_link() -> bool {
    latched(&CLIENT_LINK, client_link_now, u8::from, |n| n != 0)
}

/// Studio Settings values read from below the studio crate: `physics` (solver
/// AllowSleep) and `play` (Clear Output On Start) ask here instead of
/// importing src/studio_settings.rs. The studio crate installs the reader at
/// start-up (`studio_settings::init`); with none installed, `or` stands.
static STUDIO_FLAG: std::sync::OnceLock<fn(&str, &str) -> Option<bool>> = std::sync::OnceLock::new();

pub fn studio_flag(class: &str, prop: &str, or: bool) -> bool {
    match STUDIO_FLAG.get().and_then(|read| read(class, prop)) {
        Some(b) => b,
        None => or,
    }
}

/// Installs `studio_flag`'s reader, once (studio_settings::init).
pub fn install_studio_flag(read: fn(&str, &str) -> Option<bool>) {
    let _ = STUDIO_FLAG.set(read);
}

/// Play start, before the physics is built: re-reads the options the
/// physics keeps for a whole session. Returns the solver.
pub fn latch_for_play() -> SolverKind {
    let k = solver_now();
    SOLVER.store(enc_solver(k) + 1, Ordering::Relaxed);
    SPIN_GUARD.store(u8::from(spin_guard_now()) + 1, Ordering::Relaxed);
    CLIENT_LINK.store(u8::from(client_link_now()) + 1, Ordering::Relaxed);
    LIVE_DATA.store(u8::from(live_data_now()) + 1, Ordering::Relaxed);
    k
}

// ------------------------------------------------------------------ the dialog's view

/// The env variable overriding an option, if one is set.
pub fn overridden_by(o: &Opt) -> Option<&'static str> {
    if env(o.env).is_some() {
        return Some(o.env);
    }
    if o.key == "update_check" && env("RUSTUDIO_UI_SHOTS").is_some() {
        return Some("RUSTUDIO_UI_SHOTS");
    }
    None
}

/// Options read once at startup (or when a place opens): changing one shows
/// "Restart" beside its label until the app restarts.
const RESTART_KEYS: &[&str] = &["present", "builtin_plugins"];

static FIRST_SHOWN: std::sync::Mutex<Option<std::collections::HashMap<&'static str, String>>> =
    std::sync::Mutex::new(None);

/// The option was changed this run and only takes effect after a restart.
pub fn restart_pending(o: &Opt) -> bool {
    if !RESTART_KEYS.contains(&o.key) {
        return false;
    }
    let now = match shown(o) {
        Shown::Bool(b) => b.to_string(),
        Shown::Choice(n) => n.to_string(),
        Shown::Path(p) => p,
    };
    let mut first = FIRST_SHOWN.lock().unwrap_or_else(|e| e.into_inner());
    let first = first.get_or_insert_with(Default::default).entry(o.key).or_insert_with(|| now.clone());
    *first != now
}

/// What the row shows now (the effective value, env included).
pub enum Shown {
    Bool(bool),
    Choice(usize),
    Path(String),
}

static WEAK_GPU: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// Weak-GPU mode (rustudio-render gpu_caps.rs: software adapters, OpenGL,
/// old Intel graphics, RUSTUDIO_LOWEND=1): Graphics quality defaults to
/// Automatic, which then starts at its lowest level.
pub fn set_weak_gpu(on: bool) {
    WEAK_GPU.store(on, std::sync::atomic::Ordering::Relaxed);
}

pub fn weak_gpu() -> bool {
    WEAK_GPU.load(std::sync::atomic::Ordering::Relaxed)
}

/// A Performance row's default when nothing is saved or set.
fn choice_default(key: &str) -> Option<&'static str> {
    match key {
        "quality" if weak_gpu() => Some("automatic"),
        "resolution" if cfg!(target_os = "android") => Some("50"),
        "present" if cfg!(target_os = "android") => Some("fifo"),
        _ => None,
    }
}

pub fn performance_choice(key: &str) -> usize {
    let o = opt(key);
    let OptKind::Choice(items) = o.kind else { return 0 };
    let value = env(o.env)
        .or_else(|| saved(key)?.as_str().map(str::to_owned))
        .or_else(|| choice_default(key).map(str::to_owned));
    items.iter().position(|(id, _)| Some(*id) == value.as_deref()).unwrap_or(0)
}

/// The selected choice's saved id for a Performance row ("50", "low", "off"),
/// so code reads the value it means rather than an index into the list.
pub fn performance_value(key: &str) -> &'static str {
    let o = opt(key);
    let OptKind::Choice(items) = o.kind else { return "" };
    items.get(performance_choice(key)).map_or("", |(id, _)| *id)
}

pub fn shown(o: &Opt) -> Shown {
    match o.key {
        key if o.cat == "Performance" => Shown::Choice(performance_choice(key)),
        "solver" => Shown::Choice(if solver_now() == SolverKind::Rapier { 1 } else { 0 }),
        "spin_guard" => Shown::Bool(spin_guard_now()),
        "client_link" => Shown::Bool(client_link_now()),
        "live_data" => Shown::Bool(live_data_now()),
        "core_scripts" => Shown::Bool(core_scripts()),
        "builtin_plugins" => Shown::Bool(builtin_plugins()),
        "builtin_toolbox" => Shown::Choice(usize::from(builtin_tool_impl(BuiltinTool::Toolbox) == ToolImpl::Roblox)),
        "builtin_terrain_editor" => Shown::Choice(usize::from(builtin_tool_impl(BuiltinTool::TerrainEditor) == ToolImpl::Roblox)),
        "builtin_draggers" => Shown::Choice(usize::from(builtin_tool_impl(BuiltinTool::Draggers) == ToolImpl::Roblox)),
        "builtin_view_selector" => Shown::Choice(usize::from(builtin_tool_impl(BuiltinTool::ViewSelector) == ToolImpl::Roblox)),
        "builtin_rig_builder" => Shown::Choice(usize::from(builtin_tool_impl(BuiltinTool::RigBuilder) == ToolImpl::Roblox)),
        "builtin_material_picker" => Shown::Choice(usize::from(builtin_tool_impl(BuiltinTool::MaterialPicker) == ToolImpl::Roblox)),
        "project_any_path" => Shown::Bool(project_any_path()),
        "classic_death_sound" => Shown::Bool(classic_death_sound_fetch()),
        "update_check" => Shown::Bool(update_check()),
        "local_assets" => Shown::Path(local_assets().map(|p| p.display().to_string()).unwrap_or_default()),
        _ => Shown::Bool(false),
    }
}

/// Greyed and disabled because another option makes it meaningless (spin guard
/// under Legacy solver; per-tool built-in choices when built-in plugins are off).
pub fn inactive(o: &Opt) -> Option<&'static str> {
    if o.key == "spin_guard" && solver_now() == SolverKind::Rapier {
        return Some("No effect with the Legacy solver.");
    }
    if BuiltinTool::ALL.iter().any(|t| t.key() == o.key) && !builtin_plugins() {
        return Some("Turn on Roblox's built-in plugins to choose.");
    }
    None
}

/// The default of an on/off option (Use live DataStores, project paths
/// outside the repository and built-in plugins start off; Roblox's CoreScripts
/// start on).
pub fn bool_default(key: &str) -> bool {
    !matches!(key, "live_data" | "project_any_path" | "builtin_plugins")
}

/// The dialog writes a value: defaults are removed from the file.
pub fn set_bool(key: &str, b: bool) {
    set(key, (b != bool_default(key)).then_some(Value::Bool(b)));
}

/// The Editor frame limit's presets (any other whole number can be typed in).
pub const FPS_PRESETS: &[(&str, &str)] = &[
    ("unlimited", "Unlimited"),
    ("30", "30 FPS"),
    ("60", "60 FPS"),
    ("75", "75 FPS"),
    ("90", "90 FPS"),
    ("120", "120 FPS"),
    ("144", "144 FPS"),
    ("165", "165 FPS"),
    ("240", "240 FPS"),
    ("360", "360 FPS"),
];

/// The Editor frame limit in frames per second (None: Unlimited). A preset
/// or a typed number, from RUSTUDIO_PERF_FPS or the saved setting.
pub fn fps_limit() -> Option<f32> {
    let v = env("RUSTUDIO_PERF_FPS").or_else(|| saved("fps")?.as_str().map(str::to_owned))?;
    parse_fps(&v)
}

/// "144", "144 FPS", "97.5" → frames per second; "unlimited", 0 or junk → None.
pub fn parse_fps(v: &str) -> Option<f32> {
    let t = v.trim().trim_end_matches(|c: char| c.is_ascii_alphabetic() || c.is_whitespace());
    let f: f32 = t.trim().parse().ok()?;
    (f.is_finite() && f >= 1.0).then(|| f.min(1000.0))
}

pub fn set_choice(key: &str, v: &str, default: &str) {
    let default = choice_default(key).unwrap_or(default);
    set(key, (v != default).then(|| Value::String(v.to_string())));
}

pub fn set_path(key: &str, v: &str) {
    let t = v.trim();
    set(key, (!t.is_empty()).then(|| Value::String(t.to_string())));
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn frame_limit_takes_presets_and_typed_numbers() {
        assert_eq!(parse_fps("unlimited"), None);
        assert_eq!(parse_fps("144"), Some(144.0));
        assert_eq!(parse_fps(" 97.5 fps"), Some(97.5));
        assert_eq!(parse_fps("0"), None);
        assert_eq!(parse_fps("-5"), None);
        assert_eq!(parse_fps("5000"), Some(1000.0));
        assert_eq!(parse_fps("abc"), None);
    }

    #[test]
    fn restart_shows_only_while_a_restart_option_differs_from_startup() {
        let o = opt("present");
        let start = matches!(shown(o), Shown::Choice(1));
        assert!(!restart_pending(o));
        set_choice("present", if start { "mailbox" } else { "fifo" }, "mailbox");
        assert!(restart_pending(o));
        set_choice("present", if start { "fifo" } else { "mailbox" }, "mailbox");
        assert!(!restart_pending(o));
        assert!(!restart_pending(opt("core_scripts")));
    }

    #[test]
    fn env_beats_saved_beats_default() {
        // Solver.
        assert_eq!(resolve_solver(None, None), SolverKind::Studio);
        assert_eq!(resolve_solver(None, Some(&json!("rapier"))), SolverKind::Rapier);
        assert_eq!(resolve_solver(None, Some(&json!("studio-nextgen"))), SolverKind::Studio, "not offered in the UI");
        assert_eq!(resolve_solver(Some("studio"), Some(&json!("rapier"))), SolverKind::Studio);
        assert_eq!(resolve_solver(Some(""), Some(&json!("rapier"))), SolverKind::Studio);
        assert_eq!(resolve_solver(Some("rapier"), None), SolverKind::Rapier);
        assert_eq!(resolve_solver(Some("studio-nextgen"), Some(&json!("rapier"))), SolverKind::StudioNextGen);
        // Spin guard.
        assert!(resolve_spin_guard(None, None));
        assert!(!resolve_spin_guard(None, Some(&json!(false))));
        assert!(resolve_spin_guard(Some("on"), Some(&json!(false))));
        assert!(!resolve_spin_guard(Some("off"), Some(&json!(true))));
        assert!(!resolve_spin_guard(Some("0"), None));
        // Off switches (client link, update check, classic sound fetch).
        assert!(resolve_off_switch(false, None, true));
        assert!(!resolve_off_switch(false, Some(&json!(false)), true));
        assert!(!resolve_off_switch(true, Some(&json!(true)), true));
        assert!(resolve_off_switch(false, Some(&json!("junk")), true));
        // Live data: only "1" is live; any other env value is off.
        assert!(!resolve_live_data(None, None));
        assert!(resolve_live_data(None, Some(&json!(true))));
        assert!(!resolve_live_data(Some("0"), Some(&json!(true))));
        assert!(!resolve_live_data(Some(""), Some(&json!(true))));
        assert!(resolve_live_data(Some("1"), Some(&json!(false))));
        // CoreScripts: on by default, off with "0" / "false" / "off" or saved false.
        assert!(resolve_core_scripts(None, None));
        assert!(!resolve_core_scripts(None, Some(&json!(false))));
        assert!(resolve_core_scripts(Some("1"), Some(&json!(false))));
        assert!(!resolve_core_scripts(Some("0"), Some(&json!(true))));
        assert!(!resolve_core_scripts(Some("false"), Some(&json!(true))));
        assert!(!resolve_core_scripts(Some("off"), None));
        assert!(bool_default("core_scripts"));
        // Built-in plugins: off by default, on with "1" / "true" / "AlignmentTool" or saved true.
        assert!(!resolve_builtin_plugins(None, None));
        assert!(resolve_builtin_plugins(None, Some(&json!(true))));
        assert!(resolve_builtin_plugins(Some("1"), Some(&json!(false))));
        assert!(resolve_builtin_plugins(Some("AlignmentTool,TerrainEditor"), Some(&json!(false))));
        assert!(!resolve_builtin_plugins(Some("0"), Some(&json!(true))));
        assert!(!resolve_builtin_plugins(Some("false"), Some(&json!(true))));
        assert!(!resolve_builtin_plugins(Some("off"), Some(&json!(true))));
        assert!(!resolve_builtin_plugins(Some(""), Some(&json!(true))));
        assert!(!bool_default("builtin_plugins"));
        // Local assets.
        assert_eq!(resolve_local_assets(None, None), None);
        assert_eq!(resolve_local_assets(None, Some(&json!("/a"))), Some(PathBuf::from("/a")));
        assert_eq!(resolve_local_assets(Some(""), Some(&json!("/a"))), None);
        assert_eq!(resolve_local_assets(Some("/b"), Some(&json!("/a"))), Some(PathBuf::from("/b")));
        // Built-in tool choices: default rustudio, env beats saved beats default.
        assert_eq!(resolve_tool_impl(None, None), ToolImpl::Rustudio);
        assert_eq!(resolve_tool_impl(None, Some(&json!("roblox"))), ToolImpl::Roblox);
        assert_eq!(resolve_tool_impl(None, Some(&json!("rustudio"))), ToolImpl::Rustudio);
        assert_eq!(resolve_tool_impl(Some("roblox"), Some(&json!("rustudio"))), ToolImpl::Roblox);
        assert_eq!(resolve_tool_impl(Some("rustudio"), Some(&json!("roblox"))), ToolImpl::Rustudio);
        assert_eq!(resolve_tool_impl(Some("1"), None), ToolImpl::Roblox);
        assert_eq!(resolve_tool_impl(Some("0"), Some(&json!("roblox"))), ToolImpl::Rustudio);
        assert_eq!(resolve_tool_impl(Some("bogus"), Some(&json!("roblox"))), ToolImpl::Rustudio);
    }

    #[test]
    fn builtin_tool_choices_gate_on_plugins_enabled_and_map_stems() {
        // Built-in plugins OFF: always native regardless of per-tool settings, no built-in loads.
        let off = BuiltinToolChoices {
            plugins_enabled: false,
            toolbox: ToolImpl::Roblox,
            terrain_editor: ToolImpl::Roblox,
            draggers: ToolImpl::Roblox,
            view_selector: ToolImpl::Roblox,
            rig_builder: ToolImpl::Roblox,
            material_picker: ToolImpl::Roblox,
        };
        for tool in BuiltinTool::ALL {
            assert!(!off.use_roblox(tool));
            assert!(off.use_native(tool));
        }
        assert!(!off.should_load_builtin_stem("Toolbox"));
        assert!(!off.should_load_builtin_stem("MaterialPicker"));
        assert!(!off.should_load_builtin_stem("TagEditor"));

        // Built-in plugins ON, default (all rustudio): duplicated plugins skipped, others load.
        let native = BuiltinToolChoices { plugins_enabled: true, ..Default::default() };
        for stem in [
            "Toolbox",
            "TerrainEditor",
            "TerrainPaletteEditor",
            "SelectDragger",
            "MoveDragger",
            "RotateDragger",
            "ScaleDragger",
            "ViewSelector",
            "RigGenerator",
            "MaterialPicker",
        ] {
            assert!(!native.should_load_builtin_stem(stem), "{stem} should be skipped when native");
        }
        assert!(native.should_load_builtin_stem("TagEditor"));
        assert!(native.should_load_builtin_stem("AlignmentTool"));
        assert!(native.should_load_builtin_stem("AssetImporter"));

        // Switching one tool to Roblox loads only its plugin(s).
        let tb = BuiltinToolChoices { plugins_enabled: true, toolbox: ToolImpl::Roblox, ..Default::default() };
        assert!(tb.use_roblox(BuiltinTool::Toolbox));
        assert!(tb.should_load_builtin_stem("Toolbox"));
        assert!(!tb.should_load_builtin_stem("TerrainEditor"));
        assert!(!tb.should_load_builtin_stem("TerrainPaletteEditor"));
        assert!(!tb.should_load_builtin_stem("SelectDragger"));
        assert!(!tb.should_load_builtin_stem("ViewSelector"));
        assert!(!tb.should_load_builtin_stem("RigGenerator"));
        assert!(!tb.should_load_builtin_stem("MaterialPicker"));

        let mp = BuiltinToolChoices { plugins_enabled: true, material_picker: ToolImpl::Roblox, ..Default::default() };
        assert!(mp.use_roblox(BuiltinTool::MaterialPicker));
        assert!(mp.should_load_builtin_stem("MaterialPicker"));
        assert!(!mp.should_load_builtin_stem("Toolbox"));

        let terrain = BuiltinToolChoices { plugins_enabled: true, terrain_editor: ToolImpl::Roblox, ..Default::default() };
        assert!(terrain.should_load_builtin_stem("TerrainEditor"));
        assert!(terrain.should_load_builtin_stem("TerrainPaletteEditor"));
        assert!(!terrain.should_load_builtin_stem("Toolbox"));

        let draggers = BuiltinToolChoices { plugins_enabled: true, draggers: ToolImpl::Roblox, ..Default::default() };
        for stem in ["SelectDragger", "MoveDragger", "RotateDragger", "ScaleDragger"] {
            assert!(draggers.should_load_builtin_stem(stem));
        }
        assert!(!draggers.should_load_builtin_stem("Toolbox"));
    }

    #[test]
    fn set_bool_saves_non_defaults() {
        set_bool("live_data", true);
        assert_eq!(saved("live_data"), Some(json!(true)));
        set_bool("live_data", false);
        assert_eq!(saved("live_data"), None);
        set_bool("update_check", false);
        assert_eq!(saved("update_check"), Some(json!(false)));
        set_bool("update_check", true);
        assert_eq!(saved("update_check"), None);
    }

    #[test]
    fn harness_runs_never_load_the_saved_file() {
        // Runs a person drives read the user's file; harness runs (persist
        // false) read nothing unless a file is named; RUSTUDIO_DETERMINISTIC
        // reads nothing at all.
        assert_eq!(file_for(true, false, None), Some(default_file()));
        assert_eq!(file_for(false, false, None), None);
        assert_eq!(file_for(true, true, None), None);
        assert_eq!(file_for(false, true, Some("/x.json".into())), None);
        assert_eq!(file_for(false, false, Some("/x.json".into())), Some(PathBuf::from("/x.json")));
    }
}
