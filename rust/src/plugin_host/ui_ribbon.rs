//! The editor's side of plugin toolbars, menus and activation: the ribbon's
//! Plugins tab, the toolbars in the Plugins menu, PluginMenu:ShowAsync
//! popups, toolbar icons (loaded off the UI thread), and a plugin's
//! Activate / Deactivate against the ribbon's Select / Move / Scale /
//! Rotate tools.
//!
//! Measured in Studio 0.747 (a hidden Studio, 1920x1058 window;
//! probes checks/plugins/ui/probes, notes checks/plugins/ui/README.md):
//! - Plugins tab: only plugin toolbars (Manage Plugins / Plugins Folder
//!   are in the Plugins menu), one group per toolbar separated by a 1 px
//!   divider (7 px before the first cell, 4 px after the last); each
//!   button a 32x32 icon box over a one-line label, cell width
//!   max(label + 6, 52) px; labels wider than ~76 px lose words from the end ("Launch…"), a single long word its characters ("WWWWWWW…"); image
//!   icons drawn 24x24 in the box; an empty icon is an outlined diamond;
//!   SetActive(true) fills the box grey; Enabled=false greys icon and
//!   label and the click does nothing; with a script tab in front every
//!   button whose ClickableWhenViewportHidden is false is greyed and
//!   unclickable. Too many buttons: the strip clips at the window edge and
//!   the mouse wheel scrolls it sideways (no scroll bar); switching tabs
//!   scrolls it back to the start.
//! - Hover: a dark tooltip under the ribbon at the button's left edge,
//!   the label in bold over the tooltip text.
//! - Plugins menu: after Manage Plugins … Publish as Plugin, a separator,
//!   then one submenu per toolbar (its name) in the tab's order listing
//!   the buttons' labels; picking one fires Click, also for a disabled
//!   button.
//! - Activate: the ribbon tool is drawn unselected while a plugin is
//!   active (GetSelectedRibbonTool still answers the old tool); picking a
//!   ribbon tool fires the plugin's Deactivation; Deactivate() brings the
//!   ribbon tool's highlight back.
//! - Play: the Plugins tab shows the toolbars of the DataModel in front
//!   (the client's in a Test session), clicks go to that copy.

use std::collections::HashMap;
use std::sync::mpsc::{channel, Receiver, Sender};

use egui::{Color32, Pos2, Rect, Sense, Vec2};
use rbx_dom_weak::types::Ref;

use crate::app::{Editor, Level, Tool};
use crate::plugin_host::api::menus::{self, MenuView, OpenMenu};
use crate::plugin_host::api::toolbar::{self, RibbonButton, RibbonGroup};
use rustudio_engine::runtime::Side;
use rustudio_engine::theme;

use super::{Trigger, UiKind};

/// Studio's ribbon cell metrics (px at 100 % scale, measured).
const BOX: f32 = 32.0;
const IMG: f32 = 24.0;
const CELL_MIN: f32 = 52.0;
const CELL_PAD: f32 = 6.0;
const LABEL_MAX: f32 = 76.0;
const GROUP_LEFT: f32 = 7.0;
const GROUP_RIGHT: f32 = 4.0;
const STRIP_H: f32 = 54.0;
const FAILED_BOX: Color32 = Color32::from_rgb(47, 48, 57);
const ACTIVE_BOX: Color32 = Color32::from_rgb(64, 66, 74);
const TIP_BG: Color32 = Color32::from_rgb(52, 54, 62);

/// Which VM's toolbars the editor shows.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Where {
    Edit,
    Play(Side),
}

/// A PluginAction as Customize Shortcuts lists it.
#[derive(Clone, Debug, PartialEq)]
pub struct ActionRow {
    pub r: Ref,
    /// Studio's key: the ActionId (`<plugin>_<id>`).
    pub key: String,
    pub text: String,
    pub status_tip: String,
}

/// What the UI draws this frame, read from the plugin VM after its tick.
#[derive(Clone, Default)]
pub struct UiModel {
    pub at: Option<Where>,
    pub groups: Vec<RibbonGroup>,
    pub actions: Vec<ActionRow>,
    /// Open ShowAsync popups, bottom first, with their entries.
    pub menus: Vec<(OpenMenu, Vec<MenuView>)>,
}

#[derive(Default)]
pub struct PluginUi {
    pub model: UiModel,
    pub icons: IconCache,
    pub shortcuts: super::ui_shortcuts::Shortcuts,
    scroll: f32,
    was_plugins_tab: bool,
    /// The ribbon tool while a plugin is activated (drawn unselected).
    saved_tool: Option<Tool>,
    /// The tool last frame, to see the person picking one.
    last_tool: Option<Tool>,
    popup_pos: HashMap<u64, Pos2>,
    /// Button rects of the last drawn Plugins tab ("toolbar/button id"),
    /// the Plugins menu's toolbar entries, open popups (--ui-shots, tests).
    pub rects: Vec<(String, Rect)>,
    pub menu_rects: Vec<(String, Rect)>,
    pub popup_rects: Vec<(u64, Rect)>,
    /// Cache key for skipping `snapshot` on idle edit frames:
    /// `(at, doc.id, vm.ui_rev, deferred_rev, has_vm)`.
    last_sync: Option<(Where, u64, u64, u64, bool)>,
}

// ------------------------------------------------------------------ icons

enum Icon {
    Loading,
    Image(egui::ColorImage),
    Tex(egui::TextureHandle),
    /// `local`: an rbxasset:// file that isn't there (Studio draws a grey
    /// box); otherwise nothing is drawn.
    Failed { local: bool },
}

/// Toolbar / action icons by content id, decoded on worker threads.
pub struct IconCache {
    map: HashMap<String, Icon>,
    tx: Sender<(String, Result<egui::ColorImage, String>)>,
    rx: Receiver<(String, Result<egui::ColorImage, String>)>,
    /// Ids whose failure Output already got, and with which message.
    kind: HashMap<String, bool>,
}

impl Default for IconCache {
    fn default() -> Self {
        let (tx, rx) = channel();
        IconCache { map: HashMap::new(), tx, rx, kind: HashMap::new() }
    }
}

impl IconCache {
    /// Starts loading `id` (once). `button`: the Output message on failure
    /// is a toolbar button's (else a PluginAction's / PluginMenu's).
    pub fn request(&mut self, id: &str, button: bool) {
        let id = id.trim();
        if id.is_empty() || self.map.contains_key(id) {
            return;
        }
        self.kind.insert(id.to_string(), button);
        self.map.insert(id.to_string(), Icon::Loading);
        let tx = self.tx.clone();
        let owned = id.to_string();
        std::thread::spawn(move || {
            let r = load_icon(&owned);
            let _ = tx.send((owned, r));
        });
    }

    /// Finished loads; failures as Output lines (Studio's messages,
    /// ).
    pub fn poll(&mut self) -> Vec<String> {
        let mut out = Vec::new();
        for (id, r) in self.rx.try_iter() {
            match r {
                Ok(img) => {
                    self.map.insert(id, Icon::Image(img));
                }
                Err(_) => {
                    let local = id.starts_with("rbxasset://") || id.starts_with("rbxlocaltheme://");
                    self.map.insert(id.clone(), Icon::Failed { local });
                    if self.kind.get(&id).copied().unwrap_or(true) {
                        out.push(format!("Unable to load plugin icon: {id}"));
                    } else {
                        out.push(format!("Unable to load icon {id}. Image may have an invalid or unknown format."));
                    }
                }
            }
        }
        out
    }

    pub fn busy(&self) -> bool {
        self.map.values().any(|i| matches!(i, Icon::Loading))
    }

    fn texture(&mut self, ctx: &egui::Context, id: &str) -> Option<Result<egui::TextureHandle, bool>> {
        let e = self.map.get_mut(id.trim())?;
        if let Icon::Image(img) = e {
            let tex = ctx.load_texture(format!("plugin-icon:{id}"), std::mem::take(img), egui::TextureOptions::LINEAR);
            *e = Icon::Tex(tex);
        }
        match e {
            Icon::Tex(t) => Some(Ok(t.clone())),
            Icon::Failed { local } => Some(Err(*local)),
            Icon::Loading => {
                ctx.request_repaint();
                None
            }
            _ => None,
        }
    }
}

/// An icon's image: rbxassetid / roblox.com asset URLs and rbxasset://
/// files through the asset pipeline, rbxthumb:// through the thumbnails
/// API, other http(s) URLs fetched directly. A Decal's model leads to its
/// Texture.
fn load_icon(id: &str) -> Result<egui::ColorImage, String> {
    let bytes: Vec<u8> = if let Some(q) = id.strip_prefix("rbxthumb://") {
        let field = |k: &str| {
            q.split('&').find_map(|kv| kv.split_once('=').filter(|(a, _)| a.eq_ignore_ascii_case(k)).map(|(_, v)| v.to_string()))
        };
        let tid: u64 = field("id").and_then(|v| v.parse().ok()).ok_or("rbxthumb without an id")?;
        let kind = match field("type").unwrap_or_default().as_str() {
            "AvatarHeadShot" | "Avatar" | "AvatarBust" => rustudio_account::characters::Thumb::User,
            "Outfit" => rustudio_account::characters::Thumb::Outfit,
            _ => rustudio_account::characters::Thumb::Asset,
        };
        rustudio_account::characters::thumbnails(kind, &[tid])
            .map_err(|e| e.to_string())?
            .into_iter()
            .next()
            .map(|(_, b)| b)
            .ok_or("thumbnail not ready")?
    } else if (id.starts_with("http://") || id.starts_with("https://")) && rustudio_core::assets::asset_ref(id).is_none() {
        let mut resp = ureq::get(id).call().map_err(|e| e.to_string())?;
        resp.body_mut().with_config().limit(8 << 20).read_to_vec().map_err(|e| e.to_string())?
    } else {
        rustudio_core::assets::content_blocking(id)?.as_ref().clone()
    };
    match decode(&bytes) {
        Ok(img) => Ok(img),
        Err(e) => {
            // A Decal / model: follow its texture id once.
            let text = String::from_utf8_lossy(&bytes);
            let inner = ["rbxassetid://", "?id="].iter().find_map(|p| {
                let i = text.find(p)? + p.len();
                let n: String = text[i..].chars().take_while(|c| c.is_ascii_digit()).collect();
                (!n.is_empty()).then_some(n)
            });
            match inner {
                Some(n) if text.starts_with("<roblox") => {
                    let b = rustudio_core::assets::content_blocking(&format!("rbxassetid://{n}"))?;
                    decode(&b)
                }
                _ => Err(e),
            }
        }
    }
}

fn decode(bytes: &[u8]) -> Result<egui::ColorImage, String> {
    let img = image::load_from_memory(bytes).map_err(|e| e.to_string())?;
    // Icons are drawn at 24 px; keep up to 64 for crisp scaling.
    let img = if img.width() > 64 || img.height() > 64 {
        img.resize(64, 64, image::imageops::FilterType::Triangle)
    } else {
        img
    };
    let rgba = img.to_rgba8();
    Ok(egui::ColorImage::from_rgba_unmultiplied([rgba.width() as usize, rgba.height() as usize], rgba.as_raw()))
}

// ------------------------------------------------------------- the model

/// A label cut to `max` px with "…" as Studio's ribbon does: whole words
/// dropped from the end ("Launch Archimedes" → "Launch…", "Easy Weld
/// ------------" → "Easy Weld…"), characters only when the first word
/// alone doesn't fit ("WWWWWWW…").
pub fn elide(mut w: impl FnMut(&str) -> f32, text: &str, max: f32) -> String {
    if w(text) <= max {
        return text.to_string();
    }
    let words: Vec<&str> = text.split(' ').collect();
    for n in (1..words.len()).rev() {
        let s = format!("{}…", words[..n].join(" ").trim_end());
        if !words[..n].join("").is_empty() && w(&s) <= max {
            return s;
        }
    }
    let chars: Vec<char> = text.chars().collect();
    let mut n = chars.len();
    while n > 0 {
        let s: String = chars[..n].iter().collect::<String>() + "…";
        if w(&s) <= max {
            return s;
        }
        n -= 1;
    }
    "…".to_string()
}

#[cfg(test)]
mod elide_tests {
    #[test]
    fn words_then_chars() {
        // 6 px per char; "…" counts as one.
        let w = |s: &str| s.chars().count() as f32 * 6.0;
        assert_eq!(super::elide(w, "Launch Archimedes", 76.0), "Launch…");
        assert_eq!(super::elide(w, "Easy Weld ------------", 76.0), "Easy Weld…");
        assert_eq!(super::elide(w, "WWWWWWWWWWWWWWWWWWWW", 76.0), "WWWWWWWWWWW…");
        assert_eq!(super::elide(w, "Install", 76.0), "Install");
    }
}

/// True when a plugin ribbon group belongs to one of Roblox Studio's built-in
/// plugins (`builtin_*.rbxm`). Real Studio 0.748 routes all built-in toolbars
/// into Home, Model, Avatar, UI, Script, or top menus (File/View/Test/Window)
/// via `Ribbon.rbxm` and `PluginUIRelayGuard`; only user/third-party plugins
/// belong on the Plugins ribbon tab and Plugins menu.
pub fn is_builtin_plugin_group(plugin_name: &str) -> bool {
    plugin_name.starts_with("builtin_")
}

/// If a plugin ribbon group belongs to a Roblox built-in plugin that is
/// promoted into rustudio's own ribbon / menus via the duplicated-tool
/// selector (Toolbox, Terrain Editor, Draggers, View Selector, Rig Builder,
/// Material Picker), returns which `BuiltinTool` owns it.
pub fn promoted_builtin_tool(
    plugin_name: &str,
    choices: rustudio_core::rustudio_settings::BuiltinToolChoices,
) -> Option<rustudio_core::rustudio_settings::BuiltinTool> {
    let stem = plugin_name.strip_prefix("builtin_")?.trim_end_matches(".rbxm");
    let tool = rustudio_core::rustudio_settings::BuiltinTool::for_plugin_stem(stem)?;
    choices.use_roblox(tool).then_some(tool)
}

impl Editor {
    /// True when `g`'s buttons should stay off the Plugins tab and Plugins menu
    /// (every Roblox Studio built-in plugin; only third-party / user plugins
    /// belong on the Plugins tab).
    pub fn is_hidden_from_plugins_tab(&self, g: &RibbonGroup) -> bool {
        is_builtin_plugin_group(&g.plugin_name)
    }

    /// True when `g`'s buttons belong to a duplicated tool whose selector is
    /// set to Roblox's implementation.
    pub fn is_promoted_plugin_group(&self, g: &RibbonGroup) -> bool {
        promoted_builtin_tool(&g.plugin_name, self.builtin_tools).is_some()
    }

    /// Looks up a ribbon button belonging to built-in plugin `stem` (e.g.
    /// `"Toolbox"`, `"PivotEditor"`, `"ConstraintTool"`), optionally filtered
    /// by toolbar name and/or button id.
    pub fn builtin_plugin_button(&self, stem: &str, toolbar: Option<&str>, id: Option<&str>) -> Option<&RibbonButton> {
        let want = format!("builtin_{stem}.rbxm");
        self.plugin_bar
            .model
            .groups
            .iter()
            .filter(|g| g.plugin_name.eq_ignore_ascii_case(&want))
            .filter(|g| toolbar.is_none_or(|tb| g.name == tb))
            .flat_map(|g| g.buttons.iter())
            .find(|b| id.is_none_or(|want_id| b.id == want_id))
    }

    /// `(enabled, Some(active))` for a built-in plugin button.
    pub fn builtin_button_state(&self, stem: &str, toolbar: Option<&str>, id: Option<&str>) -> (bool, Option<bool>) {
        match self.builtin_plugin_button(stem, toolbar, id) {
            Some(b) => {
                let usable = b.enabled && (!self.viewport_hidden() || b.clickable_when_viewport_hidden);
                (usable, Some(b.active))
            }
            None => (false, Some(false)),
        }
    }

    /// Fires `Click` on a built-in plugin button (materializing it lazily on
    /// first click if deferred).
    pub fn trigger_builtin_plugin_button(&mut self, stem: &str, toolbar: Option<&str>, id: Option<&str>) -> bool {
        if let Some(r) = self.builtin_plugin_button(stem, toolbar, id).map(|b| b.r) {
            self.plugin_fire(r, "Click");
            true
        } else {
            false
        }
    }

    /// Looks up the ribbon button belonging to built-in plugin `stem`
    /// (e.g. `"Toolbox"`, `"TerrainEditor"`, `"SelectDragger"`).
    pub fn promoted_plugin_button(&self, stem: &str) -> Option<&RibbonButton> {
        self.builtin_plugin_button(stem, None, None)
    }

    /// `(enabled, Some(active))` for a promoted built-in plugin button.
    pub fn promoted_button_state(&self, stem: &str) -> (bool, Option<bool>) {
        self.builtin_button_state(stem, None, None)
    }

    /// Fires `Click` on a promoted built-in plugin button (materializing it
    /// lazily on first click if deferred).
    pub fn trigger_promoted_plugin_button(&mut self, stem: &str) -> bool {
        self.trigger_builtin_plugin_button(stem, None, None)
    }

    /// Fires `DropdownClick` on a promoted built-in popup button (materializing
    /// it lazily on first click if deferred).
    pub fn trigger_promoted_plugin_dropdown(&mut self, stem: &str) -> bool {
        if let Some(r) = self.builtin_plugin_button(stem, None, None).map(|b| b.r) {
            self.plugin_fire(r, "DropdownClick");
            true
        } else {
            false
        }
    }

    /// Draws a resolved Studio plugin icon texture into `rect` if already
    /// decoded. Returns `true` when painted, `false` while loading or
    /// unavailable (caller renders its Tabler fallback — never a diamond).
    pub fn paint_builtin_icon(&mut self, ctx: &egui::Context, p: &egui::Painter, rect: Rect, icon: &str, tint: Color32) -> bool {
        let icon = icon.trim();
        if icon.is_empty() {
            return false;
        }
        self.plugin_bar.icons.request(icon, true);
        if let Some(Ok(tex)) = self.plugin_bar.icons.texture(ctx, icon) {
            p.image(tex.id(), rect, Rect::from_min_max(Pos2::ZERO, egui::pos2(1.0, 1.0)), tint);
            return true;
        }
        false
    }

    /// Label of whichever Roblox dragger plugin button is currently active,
    /// for the status bar when Roblox's draggers are selected.
    pub fn active_roblox_dragger_label(&self) -> Option<&'static str> {
        for (stem, label) in [
            ("SelectDragger", "Select"),
            ("MoveDragger", "Move"),
            ("ScaleDragger", "Scale"),
            ("RotateDragger", "Rotate"),
        ] {
            if self.promoted_plugin_button(stem).is_some_and(|b| b.active) {
                return Some(label);
            }
        }
        None
    }

    /// Switches a Select/Move/Scale/Rotate tool using either Roblox's dragger
    /// plugin or rustudio's native `Tool`.
    pub fn trigger_dragger_or_native(&mut self, tool: Tool, stem: &str) {
        if self.builtin_tools.use_roblox(rustudio_core::rustudio_settings::BuiltinTool::Draggers) {
            self.tool = Tool::None;
            self.trigger_promoted_plugin_button(stem);
        } else {
            self.tool = self.tool.toggle(tool);
        }
    }

    /// After the plugin tick: what the UI shows this frame (the toolbars of
    /// the DataModel in front, open popups, bindable actions), icon loads,
    /// and the ribbon tool against plugin activation.
    pub fn plugin_ui_sync(&mut self) {
        if self.plugins.is_idle() && self.plugin_bar.model.at.is_none() {
            self.plugin_activation_sync();
            return;
        }
        let at = match self.game.play.as_ref() {
            Some(p) => Where::Play(p.showing),
            None => Where::Edit,
        };
        let mut rebuilt = true;
        let model = match at {
            Where::Edit => {
                let doc = match self.game.play_saved_doc() {
                    Some(d) => d,
                    None => &self.game.doc,
                };
                let drev = self.plugins.deferred_rev();
                let key = match self.plugins.edit_lua() {
                    Some(lua) => {
                        let rev = lua.app_data_ref::<super::vm::PluginVm>().map(|v| v.ui_rev).unwrap_or(0);
                        (at, doc.id, rev, drev, true)
                    }
                    None => (at, doc.id, 0, drev, false),
                };
                if self.plugin_bar.last_sync == Some(key) {
                    rebuilt = false;
                    UiModel::default()
                } else {
                    self.plugin_bar.last_sync = Some(key);
                    self.plugins.ui_model_for_edit(doc)
                }
            }
            Where::Play(side) if self.game.has_play_worker() => {
                self.plugin_bar.last_sync = None;
                self.plugins.play_ribbon(side)
            }
            Where::Play(side) => {
                self.plugin_bar.last_sync = None;
                self.game
                    .with_play_side(side, |rt, _, _| match rustudio_engine::runtime::world(&rt.lua) {
                        Ok(w) => {
                            let d: &rustudio_core::doc::Document = w.d();
                            snapshot(&rt.lua, d)
                        }
                        Err(_) => UiModel::default(),
                    })
                    .unwrap_or_default()
            }
        };
        if rebuilt {
            self.plugin_bar.model = UiModel { at: Some(at), ..model };
            // Icons load as soon as a button or action exists (Studio reports a
            // bad one in Output whether or not the tab is shown).
            let mut want: Vec<(String, bool)> = Vec::new();
            for g in &self.plugin_bar.model.groups {
                for b in &g.buttons {
                    want.push((b.icon.clone(), true));
                }
            }
            for u in self.plugins.ui_items() {
                match &u.kind {
                    UiKind::Action { icon, .. } | UiKind::Menu { icon, .. } => want.push((icon.clone(), false)),
                    _ => {}
                }
            }
            for (id, button) in want {
                self.plugin_bar.icons.request(&id, button);
            }
        }
        for line in self.plugin_bar.icons.poll() {
            self.push_log(Level::Error, line);
        }
        self.plugin_activation_sync();
    }

    /// A plugin activated: the ribbon tool is drawn unselected (kept for
    /// GetSelectedRibbonTool); deactivated: it comes back. The person
    /// picking a ribbon tool deactivates the active plugin.
    fn plugin_activation_sync(&mut self) {
        let changed = self.plugins.take_activation_change();
        let active = self.plugins.active_plugin();
        if changed {
            match active {
                Some(_) => {
                    if self.tool != Tool::None {
                        self.plugin_bar.saved_tool = Some(self.tool);
                    }
                    self.tool = Tool::None;
                }
                None => {
                    if let Some(t) = self.plugin_bar.saved_tool.take() {
                        let native_ok = !self
                            .builtin_tools
                            .use_roblox(rustudio_core::rustudio_settings::BuiltinTool::Draggers)
                            || !matches!(t, Tool::Select | Tool::Move | Tool::Scale | Tool::Rotate);
                        if self.tool == Tool::None && native_ok {
                            self.tool = t;
                        }
                    }
                }
            }
        } else if active.is_some() && self.tool != Tool::None && self.plugin_bar.last_tool != Some(self.tool) {
            // The person picked a ribbon tool.
            self.plugin_bar.saved_tool = None;
            if let Some(id) = active {
                self.plugins.trigger(Trigger::Deactivate(id));
            }
        }
        self.plugin_bar.last_tool = Some(self.tool);
        let shown = if active.is_some() { self.plugin_bar.saved_tool.unwrap_or(self.tool) } else { self.tool };
        crate::plugin_host::api::studio::set_ribbon_tool(ribbon_tool_name(shown));
    }

    /// Fires `event` on a plugin object of the VM whose toolbars are shown.
    pub fn plugin_fire(&mut self, r: Ref, event: &'static str) {
        match self.plugin_bar.model.at {
            Some(Where::Play(side)) => {
                self.game.queue_play_side(side, move |rt, _, _| {
                    rt.fire(r, event, vec![]);
                    rt.dispatch();
                });
            }
            _ => self.plugins.trigger(Trigger::Fire(r, event)),
        }
    }

    /// `--result`'s "plugin_toolbars": the Plugins tab as drawn (groups in
    /// order with their buttons), promoted built-in groups, open popups, the
    /// active plugin, the ribbon tool.
    pub fn plugin_ui_json(&self) -> serde_json::Value {
        let m = &self.plugin_bar.model;
        let encode_group = |g: &RibbonGroup| {
            serde_json::json!({
                "plugin": g.plugin_name,
                "toolbar": g.name,
                "buttons": g.buttons.iter().map(|b| serde_json::json!({
                    "id": b.id, "label": b.label, "tooltip": b.tooltip, "icon": b.icon,
                    "enabled": b.enabled, "clickable_when_viewport_hidden": b.clickable_when_viewport_hidden,
                    "active": b.active, "shortcut_key": b.key,
                })).collect::<Vec<_>>(),
            })
        };
        let groups: Vec<_> = m.groups.iter().filter(|g| !self.is_hidden_from_plugins_tab(g)).map(encode_group).collect();
        let builtin_groups: Vec<_> = m.groups.iter().filter(|g| self.is_hidden_from_plugins_tab(g)).map(encode_group).collect();
        let promoted_groups: Vec<_> = m.groups.iter().filter(|g| self.is_promoted_plugin_group(g)).map(encode_group).collect();
        let c = self.builtin_tools;
        serde_json::json!({
            "groups": groups,
            "builtin_groups": builtin_groups,
            "promoted_groups": promoted_groups,
            "builtin_tools": {
                "plugins_enabled": c.plugins_enabled,
                "toolbox": c.toolbox.id(),
                "terrain_editor": c.terrain_editor.id(),
                "draggers": c.draggers.id(),
                "view_selector": c.view_selector.id(),
                "rig_builder": c.rig_builder.id(),
                "material_picker": c.material_picker.id(),
            },
            "actions": m.actions.iter().map(|a| serde_json::json!({"action_id": a.key, "text": a.text, "status_tip": a.status_tip})).collect::<Vec<_>>(),
            "open_menus": m.menus.len(),
            "active_plugin": self.plugins.active_plugin().map(|p| p.0),
            "ribbon_tool": ribbon_tool_name(self.tool),
            "icons_loading": self.plugin_bar.icons.busy(),
        })
    }

    /// The harness's `--action plugin-button:TOOLBAR/BUTTON`: a click on
    /// that ribbon button (by id or label; nothing when it's disabled or
    /// hidden behind a script tab, as a real click).
    pub fn plugin_harness_click(&mut self, tb: &str, b: &str) {
        let hit = self
            .plugin_bar
            .model
            .groups
            .iter()
            .filter(|g| tb == "*" || g.name == tb)
            .flat_map(|g| g.buttons.iter())
            .find(|x| x.id == b || x.label == b)
            .cloned();
        match hit {
            Some(x) if x.enabled && (!self.viewport_hidden() || x.clickable_when_viewport_hidden) => self.plugin_fire(x.r, "Click"),
            Some(_) => eprintln!("--action plugin-button:{tb}/{b}: the button is disabled, nothing happens"),
            None if self.plugins.has_deferred() => {
                self.plugins.trigger(Trigger::Button(tb.to_string(), b.to_string()));
            }
            None => self.warn(format!("--action: no plugin button {tb}/{b} to click")),
        }
    }

    /// Answers a ShowAsync popup of the VM whose toolbars are shown.
    fn plugin_menu_answer(&mut self, token: u64, choice: Option<Ref>) {
        self.plugin_bar.popup_pos.remove(&token);
        match self.plugin_bar.model.at {
            Some(Where::Play(side)) => {
                self.game.queue_play_side(side, move |rt, _, _| menus::answer(&rt.lua, token, choice));
            }
            _ => {
                if let Some(lua) = self.plugins.edit_lua() {
                    menus::answer(lua, token, choice);
                }
            }
        }
        self.plugin_bar.model.menus.retain(|(m, _)| m.token != token);
    }

    /// A script tab (not the 3D view) is in front of the centre column.
    pub fn viewport_hidden(&self) -> bool {
        self.ui.viewport_hidden
    }

    // --------------------------------------------------------- Plugins tab

    /// The ribbon's Plugins tab: every plugin toolbar as a group.
    pub fn plugins_ribbon(&mut self, ui: &mut egui::Ui) {
        if !self.plugin_bar.was_plugins_tab {
            self.plugin_bar.scroll = 0.0;
        }
        self.plugin_bar.was_plugins_tab = true;
        let font = egui::FontId::proportional(11.5);
        let hidden = self.viewport_hidden();
        let groups: Vec<RibbonGroup> = self
            .plugin_bar
            .model
            .groups
            .iter()
            .filter(|g| !self.is_hidden_from_plugins_tab(g))
            .cloned()
            .collect();
        // Layout: x of every cell from the strip's left edge.
        let mut cells: Vec<(usize, usize, f32, f32, String)> = Vec::new();
        let mut dividers: Vec<f32> = Vec::new();
        let mut x = 0.0;
        ui.fonts_mut(|f| {
            for (gi, g) in groups.iter().enumerate() {
                if gi > 0 {
                    dividers.push(x);
                }
                x += if gi > 0 { GROUP_LEFT } else { GROUP_LEFT - 2.0 };
                for (bi, b) in g.buttons.iter().enumerate() {
                    let label = elide(|s: &str| f.layout_no_wrap(s.to_string(), font.clone(), Color32::WHITE).size().x, &b.label, LABEL_MAX);
                    let tw = f.layout_no_wrap(label.clone(), font.clone(), Color32::WHITE).size().x;
                    let w = (tw + CELL_PAD).max(CELL_MIN);
                    cells.push((gi, bi, x, w, label));
                    x += w;
                }
                x += GROUP_RIGHT;
            }
        });
        let total = x;
        let avail = ui.available_width();
        let (strip, resp) = ui.allocate_exact_size(Vec2::new(avail, STRIP_H), Sense::hover());
        // Wheel scrolls sideways when the strip doesn't fit (no bar).
        if resp.hovered() && total > strip.width() {
            let d = ui.input(|i| i.smooth_scroll_delta);
            self.plugin_bar.scroll -= d.y + d.x;
        }
        self.plugin_bar.scroll = self.plugin_bar.scroll.clamp(0.0, (total - strip.width()).max(0.0));
        let ox = strip.min.x - self.plugin_bar.scroll;
        let painter = ui.painter_at(strip);
        for d in dividers {
            let r = Rect::from_min_size(egui::pos2(ox + d, strip.min.y + 4.0), Vec2::new(1.0, STRIP_H - 6.0));
            painter.rect_filled(r, 0.0, theme::DIVIDER);
        }
        self.plugin_bar.rects.clear();
        let ctx = ui.ctx().clone();
        let mut clicked: Option<Ref> = None;
        for (gi, bi, cx, w, label) in cells {
            let b = &groups[gi].buttons[bi];
            let cell = Rect::from_min_size(egui::pos2(ox + cx, strip.min.y), Vec2::new(w, STRIP_H));
            self.plugin_bar.rects.push((format!("{}/{}", groups[gi].name, b.id), cell));
            if !cell.intersects(strip) {
                continue;
            }
            let usable = b.enabled && (!hidden || b.clickable_when_viewport_hidden);
            let id = egui::Id::new(("plugin_button", b.r));
            let resp = ui.interact(cell.intersect(strip), id, Sense::click());
            let boxr = Rect::from_center_size(egui::pos2(cell.center().x, strip.min.y + 2.0 + BOX / 2.0), Vec2::splat(BOX));
            if b.active {
                painter.rect_filled(boxr, 4.0, ACTIVE_BOX);
            } else if usable && resp.hovered() {
                painter.rect_filled(boxr, 4.0, theme::HOVER_BG);
            }
            let dim = |c: Color32| if usable { c } else { c.gamma_multiply(0.45) };
            self.paint_plugin_icon(&ctx, &painter, boxr, &b.icon, dim(Color32::WHITE), dim(theme::TEXT));
            painter.text(
                egui::pos2(cell.center().x, strip.min.y + 2.0 + BOX + 11.0),
                egui::Align2::CENTER_CENTER,
                label,
                font.clone(),
                dim(theme::TEXT),
            );
            if resp.hovered() {
                self.plugin_tooltip(&ctx, cell, strip.max.y, b);
            }
            if usable && resp.clicked() {
                clicked = Some(b.r);
            }
        }
        if let Some(r) = clicked {
            self.plugin_fire(r, "Click");
        }
    }

    /// Called by the ribbon each frame another tab is shown.
    pub fn plugins_tab_hidden(&mut self) {
        self.plugin_bar.was_plugins_tab = false;
    }

    fn paint_plugin_icon(&mut self, ctx: &egui::Context, p: &egui::Painter, boxr: Rect, icon: &str, tint: Color32, line: Color32) {
        if icon.trim().is_empty() {
            // Studio's placeholder: an outlined diamond.
            let c = boxr.center();
            let r = 9.5;
            let pts = vec![c + Vec2::new(0.0, -r), c + Vec2::new(r, 0.0), c + Vec2::new(0.0, r), c + Vec2::new(-r, 0.0)];
            p.add(egui::Shape::closed_line(pts, egui::Stroke::new(1.6, line)));
            return;
        }
        match self.plugin_bar.icons.texture(ctx, icon) {
            Some(Ok(tex)) => {
                let img = Rect::from_center_size(boxr.center(), Vec2::splat(IMG));
                p.image(tex.id(), img, Rect::from_min_max(Pos2::ZERO, egui::pos2(1.0, 1.0)), tint);
            }
            Some(Err(true)) => {
                p.rect_filled(boxr, 4.0, FAILED_BOX);
            }
            _ => {}
        }
    }

    fn plugin_tooltip(&mut self, ctx: &egui::Context, cell: Rect, below: f32, b: &RibbonButton) {
        let since = ctx.input(|i| i.pointer.time_since_last_movement());
        if since < 0.5 && !self.harness_tooltips() {
            ctx.request_repaint();
            return;
        }
        egui::Area::new(egui::Id::new(("plugin_tip", b.r)))
            .order(egui::Order::Tooltip)
            .fixed_pos(egui::pos2(cell.min.x, below + 2.0))
            .interactable(false)
            .show(ctx, |ui| {
                egui::Frame::new().fill(TIP_BG).corner_radius(3.0).inner_margin(egui::Margin::symmetric(9, 6)).show(ui, |ui| {
                    ui.set_min_width(134.0);
                    ui.label(egui::RichText::new(&b.label).strong().size(12.5).color(theme::TEXT));
                    if !b.tooltip.is_empty() {
                        ui.label(egui::RichText::new(&b.tooltip).size(12.0).color(theme::TEXT));
                    }
                });
            });
    }

    fn harness_tooltips(&self) -> bool {
        std::env::var_os("RUSTUDIO_PLUGIN_TIPS").is_some()
    }

    // ------------------------------------------------------- Plugins menu

    /// The Plugins menu's toolbar submenus (after its own items).
    pub fn plugins_menu_toolbars(&mut self, ui: &mut egui::Ui) {
        let groups: Vec<RibbonGroup> = self
            .plugin_bar
            .model
            .groups
            .iter()
            .filter(|g| !self.is_hidden_from_plugins_tab(g))
            .cloned()
            .collect();
        self.plugin_bar.menu_rects.clear();
        if groups.is_empty() {
            return;
        }
        ui.separator();
        let mut fire = None;
        for g in &groups {
            let r = ui.menu_button(g.name.as_str(), |ui| {
                for b in &g.buttons {
                    if ui.button(b.label.as_str()).clicked() {
                        fire = Some(b.r);
                        ui.close();
                    }
                }
            });
            self.plugin_bar.menu_rects.push((g.name.clone(), r.response.rect));
        }
        if let Some(r) = fire {
            self.plugin_fire(r, "Click");
        }
    }

    // ------------------------------------------------- ShowAsync popups

    /// Draws the open PluginMenu popups (top-left at the mouse when each
    /// opened); a pick, Escape or a click outside answers the top one.
    pub fn plugin_popups(&mut self, ctx: &egui::Context) {
        let open = self.plugin_bar.model.menus.clone();
        if open.is_empty() {
            self.plugin_bar.popup_pos.clear();
            return;
        }
        let mouse = ctx.input(|i| i.pointer.latest_pos()).unwrap_or(egui::pos2(200.0, 120.0));
        let shortcuts = self.plugin_bar.shortcuts.labels();
        let mut answer: Option<(u64, Option<Ref>)> = None;
        self.plugin_bar.popup_rects.clear();
        let top = open.last().map(|(m, _)| m.token);
        let mut any_hovered = false;
        for (m, items) in &open {
            let pos = *self.plugin_bar.popup_pos.entry(m.token).or_insert(mouse);
            let resp = egui::Popup::new(
                egui::Id::new(("plugin_popup", m.token)), ctx.clone(), pos, egui::LayerId::background(),
            )
            .kind(egui::PopupKind::Menu)
            .align_alternatives(&[])
            .frame(egui::Frame::menu(&ctx.global_style()))
            .show(|ui| {
                if let Some(r) = popup_items(ui, items, &shortcuts) {
                    answer = Some((m.token, Some(r)));
                }
            })
            .expect("plugin popup is open");
            any_hovered |= resp.response.contains_pointer();
            self.plugin_bar.popup_rects.push((m.token, resp.response.rect));
        }
        if answer.is_none() {
            if let Some(t) = top {
                let esc = ctx.input(|i| i.key_pressed(egui::Key::Escape));
                // A press outside every popup and its open submenus (egui
                // menus live on the Foreground layer).
                let press = ctx.input(|i| if i.pointer.any_pressed() { i.pointer.interact_pos() } else { None });
                let away = press.is_some_and(|p| {
                    !any_hovered
                        && !ctx.layer_id_at(p).is_some_and(|l| matches!(l.order, egui::Order::Foreground | egui::Order::Tooltip))
                });
                if esc || away {
                    answer = Some((t, None));
                }
            }
        }
        if let Some((t, c)) = answer {
            self.plugin_menu_answer(t, c);
        }
    }
}

/// A popup's entries; returns the picked action.
fn popup_items(ui: &mut egui::Ui, items: &[MenuView], shortcuts: &HashMap<String, String>) -> Option<Ref> {
    let mut picked = None;
    for e in items {
        match e {
            MenuView::Separator => {
                ui.separator();
            }
            MenuView::Action { r, text, action_id, .. } => {
                let mut b = egui::Button::new(text.as_str());
                if let Some(sc) = shortcuts.get(action_id) {
                    b = b.shortcut_text(sc.as_str());
                }
                if ui.add(b).clicked() {
                    picked = Some(*r);
                }
            }
            MenuView::Sub { title, items, .. } => {
                ui.menu_button(title.as_str(), |ui| {
                    if let Some(r) = popup_items(ui, items, shortcuts) {
                        picked = Some(r);
                    }
                });
            }
        }
    }
    picked
}

/// Enum.RibbonTool's name for an editor tool.
pub fn ribbon_tool_name(t: Tool) -> &'static str {
    match t {
        Tool::Select => "Select",
        Tool::Move => "Move",
        Tool::Scale => "Scale",
        Tool::Rotate => "Rotate",
        Tool::Transform => "Transform",
        Tool::None => "None",
    }
}

/// The editor tool for an Enum.RibbonTool name (SelectRibbonTool).
pub fn tool_for_ribbon(name: &str) -> Option<Tool> {
    Some(match name {
        "Select" => Tool::Select,
        "Move" => Tool::Move,
        "Scale" => Tool::Scale,
        "Rotate" => Tool::Rotate,
        "Transform" => Tool::Transform,
        "None" => Tool::None,
        _ => return None,
    })
}

/// The UI model of a VM whose objects live in `doc`.
pub(super) fn snapshot(lua: &mlua::Lua, doc: &rustudio_core::doc::Document) -> UiModel {
    let groups = toolbar::ribbon(lua, doc);
    let s = |r: Ref, k: &str| match doc.prop(r, k) {
        Some(rbx_dom_weak::types::Variant::String(s)) => s,
        _ => String::new(),
    };
    let actions = lua
        .app_data_ref::<super::vm::PluginVm>()
        .map(|v| {
            v.ui.iter()
                .filter_map(|u| match &u.kind {
                    UiKind::Action { text, status_tip, allow_binding: true, .. } => Some(ActionRow {
                        r: u.r,
                        key: s(u.r, "ActionId"),
                        text: text.clone(),
                        status_tip: status_tip.clone(),
                    }),
                    _ => None,
                })
                .collect()
        })
        .unwrap_or_default();
    let menus = menus::open_menus(lua).into_iter().map(|m| {
        let items = menus::menu_view(lua, doc, m.menu);
        (m, items)
    });
    UiModel { at: None, groups, actions, menus: menus.collect() }
}
