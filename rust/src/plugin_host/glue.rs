//! The editor's side of the plugin host: the per-frame tick, Output, the
//! requests plugins make of the editor, and the Play session's plugin copies.

use crate::app::{Editor, Level};

impl Editor {
    /// Once a frame, after the Play tick: the edit DataModel's plugins run
    /// (on the saved edit document while Play runs, without RenderStepped).
    pub fn plugin_tick(&mut self, dt: f64) {
        // --plugin / --plugins-dir / --no-plugins, once the place is open.
        if !self.harness.plugins_started && !self.place_loading() {
            self.harness.plugins_started = true;
            // Studio is signed in before plugins run (GetStudioUserId, the
            // installed list): a --no-ui run lets the session check answer
            // first, as Play's start does (at most 10 s).
            if self.harness.run.no_ui {
                for (level, text) in self.game.account.settle_for_play(std::time::Duration::from_secs(10), false) {
                    self.push_log(level, text);
                }
            }
            if self.harness.no_plugins {
                self.plugins.set_enabled(false);
            } else {
                self.builtin_tools = rustudio_core::rustudio_settings::BuiltinToolChoices::from_settings();
                if self.builtin_tools.use_roblox(rustudio_core::rustudio_settings::BuiltinTool::Draggers)
                    && matches!(
                        self.tool,
                        crate::app::Tool::Select | crate::app::Tool::Move | crate::app::Tool::Scale | crate::app::Tool::Rotate
                    )
                {
                    self.tool = crate::app::Tool::None;
                }
                if self.builtin_tools.plugins_enabled {
                    let _ = self.plugins.load_builtins_with_choices(self.builtin_tools);
                    if self.builtin_tools.use_roblox(rustudio_core::rustudio_settings::BuiltinTool::ViewSelector) {
                        self.plugins.wake_plugin_by_name("ViewSelector");
                    }
                }
                for d in self.harness.plugin_dirs.clone() {
                    let _ = self.plugins.load_dir(&d);
                }
                for (asset_id, path) in self.harness.installed_plugins.clone() {
                    let _ = self.plugins.load(super::PluginSource::Cloud { asset_id, path });
                }
                for f in self.harness.plugin_files.clone() {
                    let _ = self.plugins.load(super::PluginSource::File(f));
                }
            }
        }
        if self.plugins.is_idle() || self.place_loading() {
            self.plugin_ui_sync();
            return;
        }
        // Plugin GUI / viewport input since the last tick (widgets_ui.rs).
        self.plugin_ui.dt = dt as f32;
        let ui_flush = self.prepare_plugin_ui_flush();
        // (Before the session check answers: the cached signed-in profile, as
        // the plugin store uses; Studio has the id before plugins run.)
        let uid = self.game.account.user_id().unwrap_or_else(|| {
            static FALLBACK_UID: std::sync::OnceLock<u64> = std::sync::OnceLock::new();
            *FALLBACK_UID.get_or_init(|| {
                if std::env::var_os("RUSTUDIO_NO_SESSION").is_some() {
                    0
                } else {
                    crate::plugin_store::current_user_id()
                }
            })
        });
        super::settings::set_user_id(uid);
        // Plugin.GridSize / GetJoinMode read the editor's toolbar settings
        // (Play keeps them current while it runs).
        if self.game.play.is_none() {
            rustudio_engine::runtime::domains::testing::set_editor_state(rustudio_engine::runtime::domains::testing::EditorState {
                snap_move: self.game.snap_move,
                move_inc: self.game.move_inc,
                snap_rotate: self.game.snap_rotate,
                rotate_inc: self.game.rotate_inc,
                local_space: self.game.local_space,
                collisions: self.collisions,
                join_surfaces: self.join_surfaces,
            });
        }
        // GetSelectedRibbonTool reads the editor's tool; while a plugin is
        // the active tool it keeps the last ribbon tool.
        if self.plugins.active_plugin().is_none() && !matches!(self.tool, crate::app::Tool::None) {
            crate::plugin_host::api::studio::set_ribbon_tool(self.tool.label());
        }
        // Headless with no prompt answers scheduled: permission prompts are
        // refused instead of waiting forever.
        let answers = self.harness.run.no_ui
            && self
                .harness
                .run
                .input
                .actions
                .iter()
                .any(|(_, a)| matches!(a, crate::app::HarnessAction::UiAction(n) if n.starts_with("plugin-prompt:")));
        super::permissions::set_auto_deny(self.harness.run.no_ui && !answers);
        let rendering = self.game.play.is_none();
        let realtime = self.game.fixed_dt.is_none() && !rustudio_core::sim_clock::on();
        let mut cam_sent = ui_flush.cam_sent;
        let pre = |rt: &rustudio_engine::runtime::Runtime| {
            super::widgets_ui::apply_plugin_ui_flush(rt, ui_flush, &mut cam_sent);
        };
        match self.game.play_saved_doc_mut() {
            Some(doc) => {
                // (Play holds the edit document meanwhile.)
                let doc: *mut rustudio_core::doc::Document = doc;
                // SAFETY: `doc` lives in self.game.play, which plugin_tick
                // doesn't touch while the host runs.
                self.plugins.tick_with_pre(unsafe { &mut *doc }, dt, rendering, realtime, pre);
            }
            None => self.plugins.tick_with_pre(&mut self.game.doc, dt, rendering, realtime, pre),
        }
        self.finish_plugin_ui_flush(cam_sent);
        self.plugin_ui_after_tick();
        self.plugin_output();
        self.profiler_open_requests();
        self.plugin_ui_sync();
    }

    /// Steps Edit-mode audio loading, `PlayLocalSound`, and Properties-panel
    /// preview playback, bridging incremental DOM edits and Edit VM requests.
    pub fn edit_audio_tick(&mut self) {
        if self.game.play.is_some() || self.place_loading() {
            return;
        }
        let (cmds, seeks, listener) = self.plugins.take_edit_audio_requests();
        let events = self.game.edit_audio_tick(cmds, seeks, listener);
        self.plugins.sync_edit_audio_to_vm(&self.game.edit_audio.lengths);
        self.plugins.deliver_edit_audio_events(&mut self.game.doc, &events);
    }

    /// Mirrors loaded audio lengths into the Edit VM (used before running Edit
    /// Luau so newly created VMs immediately see already-loaded sounds).
    pub fn sync_edit_audio_to_vm(&mut self) {
        if self.game.play.is_some() {
            return;
        }
        self.plugins.sync_edit_audio_to_vm(&self.game.edit_audio.lengths);
    }

    /// Live runtime Sound / AudioPlayer property value in Edit mode (Properties
    /// panel and MCP `get_properties`), without mutating or dirtying the saved
    /// document. Matches Studio 0.748: Preview playback is invisible to `Sound`
    /// properties (`Playing`, `IsPlaying`, `IsPaused`, `TimePosition`).
    pub fn live_sound_prop(&self, r: rbx_dom_weak::types::Ref, name: &str) -> Option<rbx_dom_weak::types::Variant> {
        use rbx_dom_weak::types::Variant;
        if self.game.play.is_some() {
            return None;
        }
        let ea = &self.game.edit_audio;
        match self.game.doc.class_of(r).as_deref() {
            Some("Sound") => {
                let playing = matches!(self.game.doc.prop(r, "Playing"), Some(Variant::Bool(true)));
                match name {
                    "IsLoaded" => Some(Variant::Bool(ea.is_loaded(r))),
                    "TimeLength" => Some(Variant::Float64(ea.time_length(r))),
                    "IsPlaying" => Some(Variant::Bool(playing)),
                    "IsPaused" => Some(Variant::Bool(!playing)),
                    _ => None,
                }
            }
            Some("AudioPlayer") => match name {
                "IsReady" => Some(Variant::Bool(ea.is_loaded(r))),
                "TimeLength" => Some(Variant::Float64(ea.time_length(r))),
                "IsPlaying" if ea.preview_ref() == Some(r) => Some(Variant::Bool(ea.preview_state(r).0)),
                "TimePosition" if ea.preview_ref() == Some(r) => Some(Variant::Float64(ea.preview_state(r).1)),
                _ => None,
            },
            _ => None,
        }
    }

    /// The profiler's Scripts tab asked to open a script at a line.
    fn profiler_open_requests(&mut self) {
        let Some(req) = rustudio_profiler_ui::scripts::take_open_request() else { return };
        let doc = &self.game.doc;
        let Some(script) = profiler_script_ref(doc, &req.script) else {
            self.push_log(Level::Warn, format!("Profiler: can't find {} in this place to open it", req.script));
            return;
        };
        self.open_script.push(script);
        super::script_docs::set_selection(script, (req.line.max(1) as i64, 1, req.line.max(1) as i64, 1), false);
    }

    /// Host and plugin Output lines into the Output panel (and --log).
    pub fn plugin_output(&mut self) {
        for n in super::permissions::take_notes() {
            self.push_log(Level::Warn, n);
        }
        for (level, text) in self.plugins.take_log() {
            self.push_log(level, text);
        }
        // ScriptEditorService / plugin:OpenScript: show or close tabs.
        for req in super::script_docs::take_requests() {
            match req {
                super::script_docs::EditorReq::Show(script, line) => {
                    if self.game.play.is_none() {
                        self.open_script.push(script);
                        if let Some(l) = line {
                            super::script_docs::set_selection(script, (l.max(1), 1, l.max(1), 1), false);
                        }
                    }
                }
                super::script_docs::EditorReq::Close(script) => self.close_script.push(script),
                super::script_docs::EditorReq::Select(..) => {}
            }
        }
        for req in self.plugins.take_requests() {
            match req {
                super::EditorRequest::StartPlaytest { mode, args, completion } => {
                    self.start_plugin_playtest(mode, args, completion);
                }
                super::EditorRequest::OpenScript { script, line } => {
                    super::script_docs::open(script, Some(line), false);
                }
                super::EditorRequest::OpenWikiPage(url) => {
                    self.push_log(Level::Info, format!("A plugin opened {url}"));
                    if !self.harness.run.no_ui {
                        super::prompts::open_url_later(url);
                    }
                }
                // The ribbon's tool changes (Studio's Group / Ungroup act
                // on the selection; the color / material pickers have no
                // rustudio tool and only report back).
                super::EditorRequest::SelectRibbonTool { tool } => {
                    use crate::app::Tool;
                    let roblox_draggers = self
                        .builtin_tools
                        .use_roblox(rustudio_core::rustudio_settings::BuiltinTool::Draggers);
                    match tool.as_str() {
                        "Select" | "Move" | "Scale" | "Rotate" if roblox_draggers => {
                            self.tool = Tool::None;
                        }
                        "Select" => self.tool = Tool::Select,
                        "Move" => self.tool = Tool::Move,
                        "Scale" => self.tool = Tool::Scale,
                        "Rotate" => self.tool = Tool::Rotate,
                        "Transform" => self.tool = Tool::Transform,
                        "None" => self.tool = Tool::None,
                        "Group" => self.run_action("group"),
                        "Ungroup" => self.run_action("ungroup"),
                        _ => {}
                    }
                }
            }
        }
    }
}

/// A profiled script's full name in `doc`: the path itself, else where Play
/// copied it from (PlayerScripts, PlayerGui, Backpack and character scripts
/// come from the Starter containers).
fn profiler_script_ref(doc: &rustudio_core::doc::Document, full: &str) -> Option<rbx_dom_weak::types::Ref> {
    let walk = |path: &[&str]| -> Option<rbx_dom_weak::types::Ref> {
        let mut cur = doc.dom.root_ref();
        for name in path {
            let inst = doc.dom.get_by_ref(cur)?;
            cur = inst.children().iter().copied().find(|c| doc.dom.get_by_ref(*c).is_some_and(|i| i.name == *name))?;
        }
        doc.dom.get_by_ref(cur).filter(|i| i.class.ends_with("Script")).map(|_| cur)
    };
    let parts: Vec<&str> = full.split('.').collect();
    if let Some(r) = walk(&parts) {
        return Some(r);
    }
    let rest = |n: usize| parts.get(n..).unwrap_or(&[]).to_vec();
    let mut tries: Vec<Vec<&str>> = Vec::new();
    match parts.as_slice() {
        ["Players", _, "PlayerScripts", ..] => tries.push([vec!["StarterPlayer", "StarterPlayerScripts"], rest(3)].concat()),
        ["Players", _, "PlayerGui", ..] => tries.push([vec!["StarterGui"], rest(3)].concat()),
        ["Players", _, "Backpack", ..] => tries.push([vec!["StarterPack"], rest(3)].concat()),
        ["Workspace", _, ..] => tries.push([vec!["StarterPlayer", "StarterCharacterScripts"], rest(2)].concat()),
        _ => {}
    }
    tries.iter().find_map(|p| walk(p))
}

#[cfg(test)]
mod profiler_open_tests {
    use rbx_dom_weak::InstanceBuilder;

    #[test]
    fn play_copies_map_back_to_their_starter_scripts() {
        let mut d = rustudio_core::doc::Document::new_place();
        let root = d.dom.root_ref();
        let sss = d.find_child(root, "ServerScriptService").unwrap();
        let npc = d.dom.insert(sss, InstanceBuilder::new("Script").with_name("NPC"));
        let sp = d.find_child(root, "StarterPlayer").unwrap();
        let sps = d.find_child(sp, "StarterPlayerScripts").unwrap();
        let hud = d.dom.insert(sps, InstanceBuilder::new("LocalScript").with_name("HUD"));
        let gui = d.find_child(root, "StarterGui").unwrap();
        let screen = d.dom.insert(gui, InstanceBuilder::new("ScreenGui").with_name("Main"));
        let inv = d.dom.insert(screen, InstanceBuilder::new("LocalScript").with_name("Inventory"));
        assert_eq!(super::profiler_script_ref(&d, "ServerScriptService.NPC"), Some(npc));
        assert_eq!(super::profiler_script_ref(&d, "Players.Player1.PlayerScripts.HUD"), Some(hud));
        assert_eq!(super::profiler_script_ref(&d, "Players.Player1.PlayerGui.Main.Inventory"), Some(inv));
        assert_eq!(super::profiler_script_ref(&d, "ServerScriptService.Missing"), None);
        // A folder isn't a script.
        assert_eq!(super::profiler_script_ref(&d, "StarterGui.Main"), None);
    }
}
