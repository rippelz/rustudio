//! Where dock panels open, and panels popped out into their own OS windows.
//!
//! Placement: a table (`PLACEMENTS`), one row per panel, of where Studio
//! docks it by default. Only the 3D view and script tabs share the centre
//! tab group; everything else docks around it (Output at the bottom,
//! Explorer / Properties at the right, Toolbox / Terrain Editor at the
//! left) or goes back where it was last (`Home`, kept with the layout).
//! The rows follow app/STUDIO-PANELS.md (a survey of Studio 0.744: shipped
//! plugins' DockWidgetPluginGuiInfo InitialDockState plus what the live
//! Studio showed); update them from there.
//!
//! Pop-out: each tab has a button next to its close × (vendor/egui_dock,
//! `TabViewer::show_pop_out_button`) that moves it into its own OS window
//! (an egui immediate viewport: its own surface on the wgpu renderer, so it
//! can sit on another monitor). Inside such a window the same button docks
//! the tab back, and closing the window docks all of its tabs back. A
//! popped window holds its own `DockState`, so several tabs can share one
//! (tab menu > Move to window) and be split / rearranged inside it; a tab
//! dragged out of a dock (egui_dock's floating window) becomes a popped
//! window too. Each window has a stable app id, `rustudio-panel-<key>`
//! (Wayland app_id / X11 WM_CLASS), so a window manager rule can pin it to
//! a monitor where the window can't place itself (Wayland).

use std::collections::HashMap;

use egui_dock::{DockState, Node, NodeIndex, SurfaceIndex, TabPath};
use serde::{Deserialize, Serialize};

use crate::app::Tab;

/// Which side of the 3D view a panel docks on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Spot {
    /// The centre tab group, with the 3D view (scripts only).
    Center,
    /// A column left of everything, full height.
    Left,
    /// A column right of everything, full height.
    Right,
    /// Under the 3D view (between the side columns).
    Bottom,
    /// A narrow strip under everything, full width (the Command Bar).
    BottomFull,
}

/// A spot relative to another panel (by key), tried before the side.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Near {
    /// A tab in the same group as this panel.
    TabWith(&'static str),
    /// A new group under this panel's group, taking `1 - f` of it.
    Below(&'static str, f32),
    /// A new group above this panel's group, taking `f` of it.
    Above(&'static str, f32),
}

/// One panel's row: where it opens.
#[derive(Debug, Clone, Copy)]
pub struct Placement {
    /// `Tab::key` (also the popped window's app id: `rustudio-panel-<key>`).
    pub key: &'static str,
    pub title: &'static str,
    pub spot: Spot,
    /// Tried in order before falling back to `spot`.
    pub near: &'static [Near],
    /// Share of the window a new side group takes (Left / Right / Bottom).
    pub size: f32,
    /// Studio opens it floating rather than docked: rustudio opens it as a
    /// popped-out window.
    pub floats: bool,
}

const fn row(key: &'static str, title: &'static str, spot: Spot, near: &'static [Near], size: f32) -> Placement {
    Placement { key, title, spot, near, size, floats: false }
}

const fn float(key: &'static str, title: &'static str) -> Placement {
    Placement { key, title, spot: Spot::Bottom, near: &[], size: 0.3, floats: true }
}

use Near::{Above, Below, TabWith};
use Spot::{Bottom, BottomFull, Center, Left, Right};

/// Studio's default dock spots, from app/STUDIO-PANELS.md. A panel with no
/// `near` match joins a group already on its side (bottom tools tab
/// together with Output, left tools with the Toolbox, …), else splits a
/// new one off. Rows for panels rustudio doesn't have yet are here so they
/// open in the right place when they're added.
pub const PLACEMENTS: &[Placement] = &[
    // Documents: the centre group.
    row("viewport", "Viewport", Center, &[], 0.0),
    row("script", "Script", Center, &[], 0.0),
    row("object_browser", "Object Browser", Center, &[], 0.0),
    // Right: Explorer above Properties, two groups (not tabbed together).
    row("explorer", "Explorer", Right, &[Above("properties", 0.54)], 0.24),
    row("properties", "Properties", Right, &[Below("explorer", 0.46)], 0.24),
    row("tag_editor", "Tag Editor", Right, &[], 0.2),
    row("collision_groups", "Collision Groups", Right, &[], 0.24),
    row("live_collaborators", "Live Collaborators", Right, &[], 0.2),
    row("controller_emulator", "Controller Emulator", Right, &[], 0.2),
    row("style_editor", "Style Editor", Right, &[], 0.24),
    // Left.
    row("toolbox", "Toolbox", Left, &[], 0.2),
    row("terrain_editor", "Terrain Editor", Left, &[TabWith("toolbox")], 0.29),
    row("find_in_place", "Find in Place", Left, &[], 0.2),
    row("align", "Align", Left, &[], 0.18),
    row("insert_object", "Insert Object", Left, &[], 0.18),
    row("perf_diagnostics", "Performance Diagnostics", Left, &[], 0.2),
    row("script_activity", "Script Activity", Left, &[], 0.22),
    row("task_scheduler", "Task Scheduler", Left, &[], 0.22),
    row("localization_tools", "Localization Tools", Left, &[], 0.2),
    row("activity_history", "Activity History", Left, &[], 0.2),
    row("material_generator", "Material Generator", Left, &[], 0.22),
    row("player_emulator", "Player Emulator", Left, &[], 0.2),
    row("accessory_editor", "Accessory", Left, &[], 0.2),
    row("avatar_setup", "Setup", Left, &[], 0.22),
    // Bottom, under the 3D view (tool groups; Output first).
    row("output", "Output", Bottom, &[], 0.24),
    row("avatar_settings", "Avatar Settings", Bottom, &[TabWith("output")], 0.24),
    // Shipped default Bottom (the survey's own Studio had moved it left).
    row("asset_manager", "Asset Manager", Bottom, &[TabWith("output")], 0.24),
    row("script_analysis", "Script Analysis", Bottom, &[TabWith("output")], 0.22),
    row("breakpoints", "Breakpoints", Bottom, &[TabWith("watch"), TabWith("call_stack")], 0.22),
    row("watch", "Watch", Bottom, &[TabWith("breakpoints"), TabWith("call_stack")], 0.22),
    row("call_stack", "Call Stack", Bottom, &[TabWith("breakpoints"), TabWith("watch")], 0.22),
    row("material_manager", "Material Manager", Bottom, &[], 0.22),
    row("clip_editor", "Animation Editor", Bottom, &[], 0.3),
    row("scene_analysis", "Scene Analysis", Bottom, &[], 0.22),
    // A narrow full-width strip under all the panes.
    row("command_bar", "Command Bar", BottomFull, &[], 0.05),
    // Studio opens these floating.
    float("manage_plugins", "Manage Plugins"),
    float("version_history", "Version History"),
];

/// The row for `key` (unknown keys dock at the bottom).
pub fn placement(key: &str) -> Placement {
    PLACEMENTS
        .iter()
        .copied()
        .find(|p| p.key == key)
        .unwrap_or(row("other", "Panel", Spot::Bottom, &[], 0.24))
}

impl Tab {
    /// A stable name for the panel (placement row, saved homes, app ids).
    pub fn key(&self) -> &'static str {
        match self {
            Tab::Viewport => "viewport",
            Tab::Explorer => "explorer",
            Tab::Properties => "properties",
            Tab::Output => "output",
            Tab::AvatarSettings => "avatar_settings",
            Tab::TerrainEditor => "terrain_editor",
            Tab::Script(_) => "script",
        }
    }
}

/// Where a panel sat last in the main window (so it goes back there).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum Home {
    /// Complete last topology, including temporarily hidden groups (reserved map entry).
    Topology(String),
    Window(String),
    /// In a tab group with these panels.
    With(Vec<String>),
    /// Alone in a group split off next to these panels' group: `first`
    /// is whether it was the left / top half, `fraction` the split's.
    Split { next_to: Vec<String>, vertical: bool, first: bool, fraction: f32 },
}

fn node_of(tree: &egui_dock::Tree<Tab>, key: &str) -> Option<NodeIndex> {
    tree.find_tab_from(|t| t.key() == key).map(|(n, _)| n)
}

fn fraction_ok(f: f32) -> f32 {
    if f.is_finite() { f.clamp(0.05, 0.95) } else { 0.5 }
}

/// Docks `tab` in the main window: scripts in the centre group, everything
/// else where it was last (`homes`) or where its `PLACEMENTS` row says;
/// never in the 3D view's group.
pub fn place(dock: &mut DockState<Tab>, tab: Tab, homes: &HashMap<String, Home>) {
    if let Some(Home::Topology(json)) = homes.get("__topology") {
        if let Ok(saved) = serde_json::from_str::<DockState<crate::layout_store::SavedTab>>(json) {
            let mut full = saved.map_tabs(|t| t.tab());
            let visible: Vec<Tab> = dock.iter_all_tabs().map(|(_, t)| *t).collect();
            let mut projected = full.clone();
            retain_visible(&mut projected, |t| visible.contains(t));
            // Only replace the tree when it still represents the live layout.
            // A user move while another panel is hidden must never be undone.
            if full.find_tab(&tab).is_some() && topology(&projected) == topology(dock) {
                sync_fractions(&mut full, dock);
                let mut restored = full;
                retain_visible(&mut restored, |t| *t == tab || visible.contains(t));
                *dock = restored;
                if let Some(at) = dock.find_tab(&tab) {
                    let _ = dock.set_active_tab(at);
                }
                return;
            }
        }
    }
    let key = tab.key();
    let p = placement(key);
    let tree = dock.main_surface_mut();
    if tree.root_node().is_none_or(|n| n.is_empty()) {
        tree.push_to_first_leaf(tab);
        return;
    }
    let vp = node_of(tree, "viewport");
    if p.spot == Spot::Center {
        match vp {
            Some(n) => {
                tree.set_focused_node(n);
                tree.push_to_focused_leaf(tab);
            }
            None => tree.push_to_first_leaf(tab),
        }
        return;
    }
    // Where it was last.
    match homes.get(key) {
        Some(Home::With(with)) => {
            for k in with.iter().filter(|k| *k != "viewport" && *k != "script") {
                if let Some(n) = node_of(tree, k) {
                    tree.set_focused_node(n);
                    tree.push_to_focused_leaf(tab);
                    return;
                }
            }
        }
        Some(Home::Split { next_to, vertical, first, fraction }) => {
            if let Some(n) = next_to.iter().find_map(|k| node_of(tree, k)) {
                let f = fraction_ok(*fraction);
                let new = match (vertical, first) {
                    (true, true) => tree.split_above(n, f, vec![tab]),
                    (true, false) => tree.split_below(n, f, vec![tab]),
                    (false, true) => tree.split_left(n, f, vec![tab]),
                    (false, false) => tree.split_right(n, f, vec![tab]),
                };
                tree.set_focused_node(new[1]);
                return;
            }
        }
        _ => {}
    }
    // Next to the panels its row names.
    for near in p.near {
        match *near {
            Near::TabWith(k) => {
                if let Some(n) = node_of(tree, k) {
                    tree.set_focused_node(n);
                    tree.push_to_focused_leaf(tab);
                    return;
                }
            }
            Near::Below(k, f) => {
                if let Some(n) = node_of(tree, k) {
                    tree.split_below(n, fraction_ok(f), vec![tab]);
                    return;
                }
            }
            Near::Above(k, f) => {
                if let Some(n) = node_of(tree, k) {
                    tree.split_above(n, fraction_ok(f), vec![tab]);
                    return;
                }
            }
        }
    }
    // A group already on its side (not the 3D view's, not the Command Bar's).
    if p.spot != Spot::BottomFull {
        let side = tree.iter().enumerate().find_map(|(i, n)| {
            let tabs = n.get_leaf()?.tabs();
            let same = tabs.iter().any(|t| {
                let q = placement(t.key());
                // (Explorer and Properties stay two groups.)
                let pair = |k: &str| k == "explorer" || k == "properties";
                q.spot == p.spot && !q.floats && !(pair(key) && pair(t.key()))
            });
            let center = tabs.iter().any(|t| placement(t.key()).spot == Spot::Center);
            (same && !center).then_some(NodeIndex(i))
        });
        if let Some(n) = side {
            tree.set_focused_node(n);
            tree.push_to_focused_leaf(tab);
            return;
        }
    }
    // Its side of the 3D view. (split's fraction is the left / top child's.)
    let size = fraction_ok(p.size);
    match p.spot {
        Spot::BottomFull => {
            tree.split_below(NodeIndex::root(), 1.0 - size, vec![tab]);
        }
        Spot::Left => {
            tree.split_left(NodeIndex::root(), size, vec![tab]);
        }
        Spot::Right => {
            tree.split_right(NodeIndex::root(), 1.0 - size, vec![tab]);
        }
        Spot::Bottom | Spot::Center => {
            tree.split_below(vp.unwrap_or(NodeIndex::root()), 1.0 - size, vec![tab]);
        }
    }
}

// Remove through the same collapse path as interactive closes. Batch retain
// can rebalance an empty sibling subtree differently from sequential closes.
fn retain_visible(dock: &mut DockState<Tab>, keep: impl Fn(&Tab) -> bool) {
    let removed: Vec<_> = dock.iter_all_tabs().filter(|(_, t)| !keep(t)).map(|(_, t)| *t).collect();
    for tab in removed {
        if let Some(at) = dock.find_tab(&tab) { dock.remove_tab(at); }
    }
}

/// Carry resized surviving splits into the remembered tree.
fn sync_fractions(full: &mut DockState<Tab>, live: &DockState<Tab>) {
    let visible = subtree_keys(live.main_surface(), NodeIndex::root());
    let t = full.main_surface_mut();
    for i in 0..t.len() {
        let n = NodeIndex(i);
        if !matches!(t[n], Node::Vertical(_) | Node::Horizontal(_)) { continue; }
        let keys = |tree: &egui_dock::Tree<Tab>, at| {
            let mut k = subtree_keys(tree, at);
            k.retain(|k| visible.contains(k));
            k.sort(); k
        };
        let left = keys(t, n.left());
        let right = keys(t, n.right());
        if left.is_empty() || right.is_empty() { continue; }
        for (j, node) in live.main_surface().iter().enumerate() {
            let fraction = match node { Node::Vertical(s) | Node::Horizontal(s) => s.fraction, _ => continue };
            let at = NodeIndex(j);
            if keys(live.main_surface(), at.left()) == left && keys(live.main_surface(), at.right()) == right {
                match &mut t[n] { Node::Vertical(s) | Node::Horizontal(s) => s.fraction = fraction, _ => {} }
                break;
            }
        }
    }
}

/// Stable topology for checking a projected tree against the live dock.
fn topology(dock: &DockState<Tab>) -> String {
    fn node(tree: &egui_dock::Tree<Tab>, n: NodeIndex) -> String {
        if n.0 >= tree.len() { return String::new(); }
        match &tree[n] {
            Node::Leaf(l) => format!("{:?}", l.tabs()),
            Node::Vertical(_) => format!("v({},{})", node(tree, n.left()), node(tree, n.right())),
            Node::Horizontal(_) => format!("h({},{})", node(tree, n.left()), node(tree, n.right())),
            Node::Empty => String::new(),
        }
    }
    node(dock.main_surface(), NodeIndex::root())
}

/// Notes where each panel in the main window sits (call every frame).
pub fn record_homes(dock: &DockState<Tab>, homes: &mut HashMap<String, Home>) {
    let keep = match homes.get("__topology") {
        Some(Home::Topology(json)) => serde_json::from_str::<DockState<crate::layout_store::SavedTab>>(json)
            .ok().is_some_and(|d| d.iter_all_tabs().any(|(_, t)| dock.find_tab(&t.tab()).is_none())),
        _ => false,
    };
    if !keep {
        if let Ok(json) = serde_json::to_string(&crate::layout_store::saved_dock(dock)) {
            homes.insert("__topology".into(), Home::Topology(json));
        }
    }

    let tree = dock.main_surface();
    for (n, node) in tree.iter().enumerate().map(|(i, n)| (NodeIndex(i), n)) {
        let Some(leaf) = node.get_leaf() else { continue };
        let keys: Vec<&'static str> = leaf.tabs().iter().map(|t| t.key()).collect();
        for &k in &keys {
            if k == "viewport" || k == "script" {
                continue;
            }
            let others: Vec<String> = keys.iter().filter(|o| **o != k && **o != "script").map(|o| o.to_string()).collect();
            let home = if !others.is_empty() {
                Home::With(others)
            } else if let Some(parent) = n.parent() {
                let sibling = if n.is_left() { parent.right() } else { parent.left() };
                let (vertical, fraction) = match &tree[parent] {
                    Node::Vertical(s) => (true, s.fraction),
                    Node::Horizontal(s) => (false, s.fraction),
                    _ => continue,
                };
                let next_to = subtree_keys(tree, sibling);
                if next_to.is_empty() {
                    continue;
                }
                Home::Split { next_to, vertical, first: n.is_left(), fraction }
            } else {
                continue;
            };
            homes.insert(k.to_string(), home);
        }
    }
}

/// The panels in a subtree, nearest the split first.
fn subtree_keys(tree: &egui_dock::Tree<Tab>, at: NodeIndex) -> Vec<String> {
    let mut out = Vec::new();
    let mut stack = vec![at];
    while let Some(n) = stack.pop() {
        if n.0 >= tree.len() {
            continue;
        }
        match &tree[n] {
            Node::Leaf(l) => {
                for t in l.tabs() {
                    let k = t.key();
                    if k != "script" && !out.iter().any(|o| o == k) {
                        out.push(k.to_string());
                    }
                }
            }
            Node::Vertical(_) | Node::Horizontal(_) => {
                stack.push(n.right());
                stack.push(n.left());
            }
            Node::Empty => {}
        }
    }
    out
}

/// A panel window: one or more tabs out of the main window.
pub struct Popped {
    /// `output`, `output-2`, …: the app id is `rustudio-panel-<key>`.
    pub key: String,
    pub dock: DockState<Tab>,
    /// Inner size and outer position in logical pixels, as last seen
    /// (the position is None where the platform won't say: Wayland).
    pub size: [f32; 2],
    pub pos: Option<[f32; 2]>,
    /// What the window was opened with (kept fixed so egui doesn't keep
    /// moving the window to where it already is).
    open_size: [f32; 2],
    open_pos: Option<[f32; 2]>,
    /// Raise / focus it next frame.
    pub focus: bool,
    /// Hyprland: where the compositor had it (floating, monitor
    /// position, size, workspace), and the saved one still to apply once
    /// the window shows up (src/hypr.rs).
    pub wm: Option<crate::hypr::Placement>,
    pub wm_restore: Option<crate::hypr::Placement>,
}

impl Popped {
    pub fn new(key: String, dock: DockState<Tab>, size: [f32; 2], pos: Option<[f32; 2]>) -> Self {
        let size = sane_size(size);
        let pos = pos.filter(|p| p.iter().all(|v| v.is_finite()));
        Popped { key, dock, size, pos, open_size: size, open_pos: pos, focus: false, wm: None, wm_restore: None }
    }
    pub fn viewport_id(&self) -> egui::ViewportId {
        egui::ViewportId::from_hash_of(("rustudio-panel", &self.key))
    }
    pub fn app_id(&self) -> String {
        format!("rustudio-panel-{}", self.key)
    }
    /// The window title: the panel it was opened for.
    pub fn title(&self) -> String {
        let base = self.key.split('-').next().unwrap_or(&self.key);
        format!("{} — rustudio", placement(base).title)
    }
    pub fn builder(&self) -> egui::ViewportBuilder {
        let mut b = egui::ViewportBuilder::default()
            .with_title(self.title())
            .with_app_id(self.app_id())
            .with_inner_size(self.open_size)
            .with_min_inner_size([160.0, 100.0]);
        if let Some(p) = self.open_pos {
            b = b.with_position(p);
        }
        b
    }
    pub fn tabs(&self) -> Vec<Tab> {
        self.dock.iter_all_tabs().map(|(_, t)| *t).collect()
    }
    /// Notes the window's size and position (inside its viewport).
    pub fn track(&mut self, ctx: &egui::Context) {
        let zoom = ctx.zoom_factor();
        let (inner, outer, minimized) = ctx.input(|i| {
            let v = i.viewport();
            (v.inner_rect, v.outer_rect, v.minimized.unwrap_or(false) || v.fullscreen.unwrap_or(false))
        });
        if minimized {
            return;
        }
        // Hyprland: put a reopened window back where it was, then keep up
        // with where the user moves it.
        let class = self.app_id();
        match self.wm_restore.take() {
            Some(p) => {
                if !crate::hypr::restore(&class, &p) {
                    self.wm_restore = Some(p);
                    ctx.request_repaint_after(std::time::Duration::from_millis(250));
                }
            }
            None => {
                if let Some(p) = crate::hypr::seen(&class) {
                    self.wm = Some(p);
                }
            }
        }
        if let Some(r) = inner {
            let s = r.size() * zoom;
            if s.x >= 50.0 && s.y >= 50.0 {
                self.size = [s.x.round(), s.y.round()];
            }
        }
        if let Some(o) = outer {
            self.pos = Some([(o.min.x * zoom).round(), (o.min.y * zoom).round()]);
        }
    }
}

fn sane_size(s: [f32; 2]) -> [f32; 2] {
    if s.iter().all(|v| v.is_finite() && *v >= 50.0) {
        [s[0].min(8000.0), s[1].min(8000.0)]
    } else {
        [420.0, 320.0]
    }
}

/// A move between the main window and the panel windows (the tab
/// buttons and menus ask; RustudioApp does it at the end of the frame).
#[derive(Debug, Clone, PartialEq)]
pub enum DockMove {
    /// Into a new window of its own.
    PopOut(Tab),
    /// Back into the main window.
    DockBack(Tab),
    /// Into the panel window with this key.
    MoveTo(Tab, String),
    /// Every panel window's tabs back into the main window.
    DockAll,
}

/// The popped windows and where each panel lives in the main window.
#[derive(Default)]
pub struct Panels {
    pub popped: Vec<Popped>,
    pub homes: HashMap<String, Home>,
}

/// Where a tab is.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Location {
    Main(TabPath),
    /// Window index, path in its dock.
    Popped(usize, TabPath),
}

impl Panels {
    pub fn remember_windows(&mut self) {
        for saved in crate::layout_store::saved_popped(self) {
            let key = format!("__window:{}", saved.key);
            let keep = match self.homes.get(&key) {
                Some(Home::Window(json)) => serde_json::from_str::<crate::layout_store::SavedPopped>(json).ok()
                    .is_some_and(|old| old.dock.iter_all_tabs().any(|(_, t)| saved.dock.find_tab(t).is_none())),
                _ => false,
            };
            if !keep {
                if let Ok(json) = serde_json::to_string(&saved) { self.homes.insert(key, Home::Window(json)); }
            }
        }
    }

    pub fn restore_window(&mut self, main: &DockState<Tab>, tab: Tab) -> bool {
        let saved = self.homes.values().find_map(|h| {
            let Home::Window(json) = h else { return None };
            let w = serde_json::from_str::<crate::layout_store::SavedPopped>(json).ok()?;
            w.dock.find_tab(&crate::layout_store::SavedTab::from_tab(&tab)?).map(|_| w)
        });
        let Some(saved) = saved else { return false };
        let mut full = saved.dock.map_tabs(|t| t.tab());
        if let Some(w) = self.popped.iter_mut().find(|w| w.key == saved.key) {
            let visible = w.tabs();
            let mut projection = full.clone();
            retain_visible(&mut projection, |t| visible.contains(t));
            if topology(&projection) != topology(&w.dock) { return false; }
            sync_fractions(&mut full, &w.dock);
            retain_visible(&mut full, |t| *t == tab || visible.contains(t));
            w.dock = full;
            w.focus = true;
        } else {
            retain_visible(&mut full, |t| *t == tab && main.find_tab(t).is_none());
            let mut w = Popped::new(saved.key, full, saved.size, saved.pos);
            w.wm_restore = saved.wm;
            self.popped.push(w);
        }
        true
    }

    pub fn locate(&self, main: &DockState<Tab>, tab: &Tab) -> Option<Location> {
        if let Some(p) = main.find_tab(tab) {
            return Some(Location::Main(p));
        }
        self.popped
            .iter()
            .enumerate()
            .find_map(|(i, w)| w.dock.find_tab(tab).map(|p| Location::Popped(i, p)))
    }

    /// A key not used by another window: `output`, `output-2`, ….
    pub fn free_key(&self, base: &str) -> String {
        let used = |k: &str| self.popped.iter().any(|w| w.key == k);
        if !used(base) {
            return base.to_string();
        }
        (2..).map(|n| format!("{base}-{n}")).find(|k| !used(k)).unwrap()
    }

    /// Takes a tab out of wherever it is (dropping a window left empty).
    pub fn take(&mut self, main: &mut DockState<Tab>, tab: &Tab) -> Option<Tab> {
        self.remember_windows();
        match self.locate(main, tab)? {
            Location::Main(p) => main.remove_tab(p),
            Location::Popped(i, p) => {
                let t = self.popped[i].dock.remove_tab(p);
                if self.popped[i].dock.iter_all_tabs().next().is_none() {
                    self.popped.remove(i);
                }
                t
            }
        }
    }

    /// Opens `tab` in a new panel window.
    pub fn pop_out(&mut self, tab: Tab, size: [f32; 2], pos: Option<[f32; 2]>) -> usize {
        let key = self.free_key(tab.key());
        self.popped.push(Popped::new(key, DockState::new(vec![tab]), size, pos));
        self.popped.len() - 1
    }

    /// Docks every tab of window `i` back into the main window.
    pub fn dock_back_window(&mut self, main: &mut DockState<Tab>, i: usize) {
        if i >= self.popped.len() {
            return;
        }
        let w = self.popped.remove(i);
        self.homes.remove(&format!("__window:{}", w.key));
        for t in w.tabs() {
            if main.find_tab(&t).is_none() {
                place(main, t, &self.homes);
            }
        }
    }

    /// Does one move. `size` / `pos`: a new window's (logical pixels).
    pub fn apply(&mut self, main: &mut DockState<Tab>, mv: DockMove, size: [f32; 2], pos: Option<[f32; 2]>) {
        // Explicit docking/moving supersedes remembered hidden-window placement.
        self.homes.retain(|_, h| !matches!(h, Home::Window(_)));
        match mv {
            DockMove::DockAll => {
                while !self.popped.is_empty() {
                    self.dock_back_window(main, 0);
                }
            }
            DockMove::PopOut(tab) => {
                if tab == Tab::Viewport {
                    return;
                }
                // Already alone in a window: just raise it.
                if let Some(Location::Popped(i, _)) = self.locate(main, &tab) {
                    if self.popped[i].dock.iter_all_tabs().count() == 1 {
                        self.popped[i].focus = true;
                        return;
                    }
                }
                if let Some(t) = self.take(main, &tab) {
                    self.pop_out(t, size, pos);
                }
            }
            DockMove::DockBack(tab) => {
                if let Some(Location::Popped(..)) = self.locate(main, &tab) {
                    if let Some(t) = self.take(main, &tab) {
                        place(main, t, &self.homes);
                    }
                }
            }
            DockMove::MoveTo(tab, key) => {
                let Some(dest) = self.popped.iter().position(|w| w.key == key) else { return };
                if let Some(Location::Popped(i, _)) = self.locate(main, &tab) {
                    if i == dest {
                        return;
                    }
                }
                if tab == Tab::Viewport {
                    return;
                }
                let dest_key = self.popped[dest].key.clone();
                if let Some(t) = self.take(main, &tab) {
                    // `take` may have dropped an emptied window before it.
                    if let Some(w) = self.popped.iter_mut().find(|w| w.key == dest_key) {
                        w.dock.push_to_first_leaf(t);
                        w.focus = true;
                    }
                }
            }
        }
        self.homes.retain(|_, h| !matches!(h, Home::Window(_)));
    }

    /// Turns egui_dock's floating windows (a tab dragged out of a dock) in
    /// `dock` into panel windows. `origin`: the dock window's top-left on
    /// the screen, when known, to open each where it was dropped.
    pub fn adopt_floating(&mut self, dock: &mut DockState<Tab>, origin: Option<egui::Pos2>, zoom: f32) {
        let floating: Vec<SurfaceIndex> = dock
            .iter_surfaces_indexed()
            .filter(|(i, s)| !i.is_main() && !s.is_empty())
            .map(|(i, _)| i)
            .collect();
        for idx in floating.into_iter().rev() {
            let rect = dock.get_window_state(idx).and_then(|w| w.rect_or_next());
            let Some(surface) = dock.remove_surface(idx) else { continue };
            let Some(tree) = surface.node_tree() else { continue };
            let mut tabs: Vec<Tab> = tree.tabs().copied().collect();
            // The 3D view never leaves the main window.
            if let Some(i) = tabs.iter().position(|t| *t == Tab::Viewport) {
                tabs.remove(i);
                place(dock, Tab::Viewport, &self.homes);
            }
            let Some(first) = tabs.first().copied() else { continue };
            let mut new = DockState::new(vec![]);
            *new.main_surface_mut() = tree.clone();
            new.retain_tabs(|t| *t != Tab::Viewport);
            let size = rect
                .filter(|r| r.is_finite() && r.width() > 50.0)
                .map(|r| [r.width() * zoom, r.height() * zoom])
                .unwrap_or([420.0, 320.0]);
            let pos = match (origin, rect) {
                (Some(o), Some(r)) if r.is_finite() => Some([(o.x + r.min.x) * zoom, (o.y + r.min.y) * zoom]),
                _ => None,
            };
            let key = self.free_key(first.key());
            self.popped.push(Popped::new(key, new, size, pos));
        }
    }

    /// Every tab in a panel window.
    pub fn popped_tabs(&self) -> impl Iterator<Item = Tab> + '_ {
        self.popped.iter().flat_map(|w| w.dock.iter_all_tabs().map(|(_, t)| *t))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::default_dock;

    fn leaf_with_viewport(dock: &DockState<Tab>) -> Vec<Tab> {
        let (n, _) = dock.main_surface().find_tab(&Tab::Viewport).unwrap();
        dock.main_surface()[n].tabs().unwrap().to_vec()
    }

    fn remove(dock: &mut DockState<Tab>, t: Tab) {
        let p = dock.find_tab(&t).unwrap();
        dock.remove_tab(p);
    }

    #[test]
    fn hidden_floating_tabs_restore_window_and_order() {
        let mut p = Panels::default();
        let mut main = default_dock();
        p.apply(&mut main, DockMove::PopOut(Tab::Explorer), [510.0, 420.0], Some([60.0, 80.0]));
        p.apply(&mut main, DockMove::MoveTo(Tab::Properties, "explorer".into()), [1.0, 1.0], None);
        p.remember_windows();
        p.take(&mut main, &Tab::Explorer);
        p.take(&mut main, &Tab::Properties);
        assert!(p.popped.is_empty());
        assert!(p.restore_window(&main, Tab::Properties));
        assert!(p.restore_window(&main, Tab::Explorer));
        assert_eq!(p.popped[0].tabs(), vec![Tab::Explorer, Tab::Properties]);
        assert_eq!(p.popped[0].size, [510.0, 420.0]);
        assert_eq!(p.popped[0].pos, Some([60.0, 80.0]));
    }

    #[test]
    fn hidden_topology_survives_both_orders_and_persistence() {
        for custom in 0..3 {
            for reverse in [false, true] {
                let mut dock = default_dock();
                if custom == 1 {
                    remove(&mut dock, Tab::Explorer);
                    remove(&mut dock, Tab::Properties);
                    let children = dock.main_surface_mut().split_left(NodeIndex::root(), 0.31, vec![Tab::Properties]);
                    dock.main_surface_mut().split_below(children[1], 0.72, vec![Tab::Explorer]);
                } else if custom == 2 {
                    remove(&mut dock, Tab::Properties);
                    let (n, _) = dock.main_surface().find_tab(&Tab::Explorer).unwrap();
                    dock.main_surface_mut()[n].append_tab(Tab::Properties);
                }
                let fractions = |d: &DockState<Tab>| d.main_surface().iter().filter_map(|n| match n {
                    Node::Vertical(s) | Node::Horizontal(s) => Some(s.fraction), _ => None
                }).collect::<Vec<_>>();
                let expected_fractions = fractions(&dock);
                let expected = topology(&dock);
                let mut homes = HashMap::new();
                record_homes(&dock, &mut homes);
                remove(&mut dock, Tab::Explorer);
                record_homes(&dock, &mut homes);
                remove(&mut dock, Tab::Properties);
                record_homes(&dock, &mut homes);
                homes = serde_json::from_str(&serde_json::to_string(&homes).unwrap()).unwrap();
                let order = if reverse { [Tab::Properties, Tab::Explorer] } else { [Tab::Explorer, Tab::Properties] };
                for tab in order {
                    place(&mut dock, tab, &homes);
                    record_homes(&dock, &mut homes);
                }
                assert_eq!(topology(&dock), expected, "custom={custom}, reverse={reverse}");
                assert_eq!(fractions(&dock), expected_fractions);
                remove(&mut dock, Tab::Explorer);
                record_homes(&dock, &mut homes);
                place(&mut dock, Tab::Explorer, &homes);
                assert_eq!(topology(&dock), expected);
            }
        }
    }

    /// The default layout: Explorer right, Properties under it, Output under the 3D view.
    #[test]
    fn default_dock_from_table() {
        let dock = default_dock();
        assert_eq!(leaf_with_viewport(&dock), vec![Tab::Viewport]);
        let t = dock.main_surface();
        let (vp, _) = t.find_tab(&Tab::Viewport).unwrap();
        let (ex, _) = t.find_tab(&Tab::Explorer).unwrap();
        let (pr, _) = t.find_tab(&Tab::Properties).unwrap();
        let (out, _) = t.find_tab(&Tab::Output).unwrap();
        // Explorer above Properties, same column.
        assert_eq!(ex.parent(), pr.parent());
        assert!(ex.is_left());
        // Output under the 3D view.
        assert_eq!(vp.parent(), out.parent());
        assert!(matches!(t[vp.parent().unwrap()], Node::Vertical(_)));
        // The right column is right of everything.
        assert!(matches!(t[NodeIndex::root()], Node::Horizontal(_)));
    }

    /// Panels never open in the 3D view's tab group; scripts do.
    #[test]
    fn new_panels_avoid_the_game_view() {
        let homes = HashMap::new();
        let mut dock = DockState::new(vec![Tab::Viewport]);
        for t in [Tab::Output, Tab::Explorer, Tab::Properties, Tab::TerrainEditor, Tab::AvatarSettings] {
            place(&mut dock, t, &homes);
            assert_eq!(leaf_with_viewport(&dock), vec![Tab::Viewport], "{t:?} opened over the 3D view");
        }
        place(&mut dock, Tab::Script(rbx_dom_weak::types::Ref::new()), &homes);
        assert_eq!(leaf_with_viewport(&dock).len(), 2, "scripts open next to the 3D view");
        // Avatar Settings joins Output's group; the Terrain Editor is left of everything.
        let t = dock.main_surface();
        assert_eq!(t.find_tab(&Tab::AvatarSettings).unwrap().0, t.find_tab(&Tab::Output).unwrap().0);
        let (te, _) = t.find_tab(&Tab::TerrainEditor).unwrap();
        assert_eq!(te, NodeIndex::root().left());
    }

    /// Closing a panel and opening it again puts it back where it was.
    #[test]
    fn reopen_goes_home() {
        let mut homes = HashMap::new();
        let mut dock = default_dock();
        // Output dragged into Explorer's group.
        remove(&mut dock, Tab::Output);
        let (ex, _) = dock.main_surface().find_tab(&Tab::Explorer).unwrap();
        dock.main_surface_mut()[ex].append_tab(Tab::Output);
        record_homes(&dock, &mut homes);
        remove(&mut dock, Tab::Output);
        place(&mut dock, Tab::Output, &homes);
        let t = dock.main_surface();
        assert_eq!(t.find_tab(&Tab::Output).unwrap().0, t.find_tab(&Tab::Explorer).unwrap().0);

        // Alone under the 3D view at 70 %: back at the same split.
        let mut homes = HashMap::new();
        let mut dock = DockState::new(vec![Tab::Viewport]);
        dock.main_surface_mut().split_below(NodeIndex::root(), 0.7, vec![Tab::Output]);
        record_homes(&dock, &mut homes);
        assert_eq!(
            homes.get("output"),
            Some(&Home::Split { next_to: vec!["viewport".into()], vertical: true, first: false, fraction: 0.7 })
        );
        remove(&mut dock, Tab::Output);
        place(&mut dock, Tab::Output, &homes);
        let t = dock.main_surface();
        let (out, _) = t.find_tab(&Tab::Output).unwrap();
        let parent = out.parent().unwrap();
        match &t[parent] {
            Node::Vertical(s) => assert!((s.fraction - 0.7).abs() < 1e-6),
            n => panic!("not a vertical split: {n:?}"),
        }
        assert_eq!(leaf_with_viewport(&dock), vec![Tab::Viewport]);
    }

    /// A home "with the 3D view" is never used (that's how panels ended
    /// up over the game).
    #[test]
    fn home_with_viewport_ignored() {
        let mut homes = HashMap::new();
        homes.insert("output".to_string(), Home::With(vec!["viewport".into()]));
        let mut dock = DockState::new(vec![Tab::Viewport]);
        place(&mut dock, Tab::Output, &homes);
        assert_eq!(leaf_with_viewport(&dock), vec![Tab::Viewport]);
    }

    /// Pop out, move another tab into the same window, dock back, close.
    #[test]
    fn pop_out_and_back() {
        let mut p = Panels::default();
        let mut dock = default_dock();
        record_homes(&dock, &mut p.homes);
        p.apply(&mut dock, DockMove::PopOut(Tab::Output), [500.0, 300.0], Some([1920.0, 40.0]));
        assert!(dock.find_tab(&Tab::Output).is_none());
        assert_eq!(p.popped.len(), 1);
        assert_eq!(p.popped[0].key, "output");
        assert_eq!(p.popped[0].app_id(), "rustudio-panel-output");
        assert_eq!(p.popped[0].pos, Some([1920.0, 40.0]));
        // The 3D view stays.
        p.apply(&mut dock, DockMove::PopOut(Tab::Viewport), [500.0, 300.0], None);
        assert!(dock.find_tab(&Tab::Viewport).is_some());
        assert_eq!(p.popped.len(), 1);
        // Properties joins Output's window.
        p.apply(&mut dock, DockMove::MoveTo(Tab::Properties, "output".into()), [1.0, 1.0], None);
        assert_eq!(p.popped[0].tabs(), vec![Tab::Output, Tab::Properties]);
        // Properties back in: under the Explorer again.
        p.apply(&mut dock, DockMove::DockBack(Tab::Properties), [1.0, 1.0], None);
        let t = dock.main_surface();
        let (ex, _) = t.find_tab(&Tab::Explorer).unwrap();
        let (pr, _) = t.find_tab(&Tab::Properties).unwrap();
        assert_eq!(ex.parent(), pr.parent());
        // Closing the window docks Output back under the 3D view.
        p.dock_back_window(&mut dock, 0);
        assert!(p.popped.is_empty());
        let t = dock.main_surface();
        let (vp, _) = t.find_tab(&Tab::Viewport).unwrap();
        let (out, _) = t.find_tab(&Tab::Output).unwrap();
        assert_eq!(vp.parent(), out.parent());
        assert_eq!(leaf_with_viewport(&dock), vec![Tab::Viewport]);
    }

    /// A tab dragged out of the dock (egui_dock's floating window) becomes a panel window.
    #[test]
    fn dragged_out_becomes_window() {
        let mut p = Panels::default();
        let mut dock = default_dock();
        let at = dock.find_tab(&Tab::Output).unwrap();
        dock.detach_tab(at, egui::Rect::from_min_size(egui::pos2(100.0, 50.0), egui::vec2(400.0, 200.0)));
        p.adopt_floating(&mut dock, Some(egui::pos2(10.0, 20.0)), 1.0);
        assert_eq!(dock.iter_surfaces_indexed().filter(|(i, s)| !i.is_main() && !s.is_empty()).count(), 0);
        assert_eq!(p.popped.len(), 1);
        assert_eq!(p.popped[0].tabs(), vec![Tab::Output]);
        assert_eq!(p.popped[0].pos, Some([110.0, 70.0]));
        // egui_dock opens a tab dragged off the main dock at 80 % of its group.
        assert_eq!(p.popped[0].size, [320.0, 160.0]);
    }

    #[test]
    fn keys_unique() {
        let mut p = Panels::default();
        p.pop_out(Tab::Script(rbx_dom_weak::types::Ref::new()), [400.0, 300.0], None);
        p.pop_out(Tab::Script(rbx_dom_weak::types::Ref::new()), [400.0, 300.0], None);
        assert_eq!(p.popped[0].key, "script");
        assert_eq!(p.popped[1].key, "script-2");
        assert_eq!(p.popped[1].title(), "Script — rustudio");
    }
}

/// Script tabs belong to the replaced document; preserve all tool topology.
pub fn close_document_tabs(dock: &mut DockState<Tab>, panels: &mut Panels) {
    retain_visible(dock, |t| !matches!(t, Tab::Script(_)));
    for window in &mut panels.popped {
        retain_visible(&mut window.dock, |t| !matches!(t, Tab::Script(_)));
    }
    panels.popped.retain(|w| w.dock.iter_all_tabs().next().is_some());
}

#[test]
fn document_close_removes_scripts_preserving_tools() {
    let script = Tab::Script(rbx_dom_weak::types::Ref::new());
    let mut dock = DockState::new(vec![Tab::Viewport, script, Tab::Output]);
    let mut panels = Panels::default();
    panels.pop_out(script, [420.0, 520.0], None);
    close_document_tabs(&mut dock, &mut panels);
    assert!(dock.find_tab(&script).is_none());
    assert!(panels.popped.is_empty());
    assert!(dock.find_tab(&Tab::Viewport).is_some());
    assert!(dock.find_tab(&Tab::Output).is_some());
    place(&mut dock, script, &panels.homes);
    assert!(dock.find_tab(&script).is_some());
}
