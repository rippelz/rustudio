//! The MCP tools, run on the editor's UI thread (see mod.rs). Each returns a
//! result at once or an operation the editor polls every frame.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use rbx_dom_weak::types::{Ref, Variant};
use rbx_dom_weak::{ustr, InstanceBuilder};
use rustudio_engine::runtime::Side;
use serde_json::{json, Value as J};

use super::server::ToolResult;
use super::{project_apply, sessions, tree, Op, Step};
use crate::app::Editor;

type R<T> = Result<T, String>;
/// Resolved property values, and a new Name when one was given.
type Props = (Vec<(String, Variant)>, Option<String>);

fn s<'a>(a: &'a J, k: &str) -> Option<&'a str> {
    a.get(k).and_then(|v| v.as_str())
}

fn req<'a>(a: &'a J, k: &str) -> R<&'a str> {
    s(a, k).ok_or_else(|| format!("missing \"{k}\" (a string)"))
}

fn num(a: &J, k: &str) -> Option<f64> {
    a.get(k).and_then(|v| v.as_f64())
}

fn flag(a: &J, k: &str, d: bool) -> bool {
    a.get(k).and_then(|v| v.as_bool()).unwrap_or(d)
}

fn path_arg(a: &J, k: &str) -> R<PathBuf> {
    let p = PathBuf::from(req(a, k)?);
    if !p.is_absolute() {
        return Err(format!("\"{k}\" must be an absolute path (rustudio's working folder isn't the agent's)"));
    }
    Ok(p)
}

/// Base64 (standard alphabet, padded).
pub fn base64(bytes: &[u8]) -> String {
    const T: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for c in bytes.chunks(3) {
        let n = (c[0] as u32) << 16 | (*c.get(1).unwrap_or(&0) as u32) << 8 | *c.get(2).unwrap_or(&0) as u32;
        out.push(T[(n >> 18) as usize & 63] as char);
        out.push(T[(n >> 12) as usize & 63] as char);
        out.push(if c.len() > 1 { T[(n >> 6) as usize & 63] as char } else { '=' });
        out.push(if c.len() > 2 { T[n as usize & 63] as char } else { '=' });
    }
    out
}

fn class_exists(class: &str) -> bool {
    rustudio_core::doc::db().classes.contains_key(class)
}

impl Editor {
    pub(crate) fn mcp_call(&mut self, tool: &str, a: &J) -> Step {
        let r: R<Step> = match tool {
            "session_info" => Ok(self.mcp_session_info().into()),
            "get_tree" => self.mcp_get_tree(a).map(Into::into),
            "get_properties" => self.mcp_get_properties(a).map(Into::into),
            "get_source" => self.mcp_get_source(a).map(Into::into),
            "grep_scripts" => self.mcp_grep(a).map(Into::into),
            "read_output" => Ok(self.mcp_read_output(a).into()),
            "screenshot" => Ok(Step::Wait(Op::Shot { save: s(a, "path").map(PathBuf::from), requested: false, settled: false, asked: Instant::now() }, Duration::from_secs(40))),
            "list_plugins" => Ok(ToolResult::json(&self.plugins.result_json()).into()),
            "team_create_status" => Ok(ToolResult::json(&self.mcp_live_status()).into()),
            "run_luau" => self.mcp_run_luau(a),
            "set_properties" => self.mcp_writable().and_then(|_| self.mcp_set_properties(a)).map(Into::into),
            "set_source" => self.mcp_writable().and_then(|_| self.mcp_set_source(a)).map(Into::into),
            "create_instance" => self.mcp_writable().and_then(|_| self.mcp_create(a)).map(Into::into),
            "clone_instance" => self.mcp_writable().and_then(|_| self.mcp_clone(a)).map(Into::into),
            "delete_instances" => self.mcp_writable().and_then(|_| self.mcp_delete(a)).map(Into::into),
            "move_instance" => self.mcp_writable().and_then(|_| self.mcp_move(a)).map(Into::into),
            "insert_model" => self.mcp_writable().and_then(|_| self.mcp_insert_model(a)).map(Into::into),
            "apply_project" => self.mcp_apply_project(a).map(Into::into),
            "undo" | "redo" => self.mcp_writable().and_then(|_| self.mcp_undo(tool == "undo")).map(Into::into),
            "load_plugin" | "reload_plugin" | "unload_plugin" => self.mcp_plugin(tool, a).map(Into::into),
            "ui_action" => self.mcp_ui_action(a).map(Into::into),
            "play" => self.mcp_play(a),
            "stop" => self.mcp_stop(),
            "send_input" => self.mcp_send_input(a),
            "set_camera" => self.mcp_set_camera(a).map(Into::into),
            "open_place" => self.mcp_open_place(a),
            "open_roblox_place" => self.mcp_open_roblox(a),
            "join_team_create" => self.mcp_join(a),
            "save_place" => self.mcp_save_place(a).map(Into::into),
            "team_create_commit_drafts" => self.mcp_drafts(a, true).map(Into::into),
            "team_create_discard_drafts" => self.mcp_drafts(a, false).map(Into::into),
            "team_create_wait" => self.mcp_tc_wait(a),
            "file_sync_status" => Ok(ToolResult::json(&self.file_sync_mcp_status(a)).into()),
            "file_sync_diff" => self.file_sync_mcp_diff(a).map(|j| ToolResult::json(&j).into()),
            "file_sync_attach" => self.mcp_writable().and_then(|_| self.file_sync_mcp_attach(a)).map(|j| ToolResult::json(&j).into()),
            "file_sync_apply" => self.mcp_writable().and_then(|_| self.file_sync_mcp_apply(a)).map(|j| ToolResult::json(&j).into()),
            "file_sync_detach" => self.file_sync_mcp_detach(a).map(|j| ToolResult::json(&j).into()),
            _ => Err(format!("Unknown tool {tool}")),
        };
        r.unwrap_or_else(|e| Step::Done(ToolResult::error(e)))
    }

    /// Edit-DataModel writes need edit mode.
    fn mcp_writable(&self) -> R<()> {
        if self.game.play.is_some() {
            return Err("Play is running: an edit now would land in the Play DataModel and be lost at Stop. Call stop first, or use run_luau with target \"server\" / \"client\" to change the running game.".into());
        }
        if self.place_loading() {
            return Err("A place is still loading; try again when it has opened.".into());
        }
        Ok(())
    }

    fn mcp_resolve(&self, p: &str) -> R<Ref> {
        tree::resolve(&self.game.doc, p)
    }

    fn mcp_datamodel(&self) -> String {
        match self.game.play.as_ref() {
            None => "edit".into(),
            Some(p) => format!("{} ({:?} shown)", self.mcp_mode(), p.showing).to_lowercase(),
        }
    }

    // ---------------------------------------------------------------- read

    fn mcp_session_info(&self) -> ToolResult {
        let doc = &self.game.doc;
        let me = std::process::id() as u64;
        let others: Vec<J> = sessions::list().into_iter().filter(|v| v.get("pid").and_then(|p| p.as_u64()) != Some(me)).collect();
        let groups: Vec<&str> = super::settings::Group::ALL.iter().filter(|g| self.mcp.settings.groups.allows(**g)).map(|g| g.key()).collect();
        let live = self.mcp_live_activity().map(|l| if l.joined { "connected" } else { "joining" });
        ToolResult::json(&json!({
            "pid": me,
            "title": doc.title,
            "file": doc.path.as_ref().map(|p| p.display().to_string()),
            "roblox_place": doc.cloud.as_ref().map(|o| json!({ "place_id": o.place_id, "universe_id": o.universe_id, "name": o.name, "experience": o.experience, "version": o.version, "local_copy": o.local_copy })),
            "team_create": live,
            "project": self.project.as_ref().map(|p| p.file.display().to_string()),
            "mode": self.mcp_mode(),
            "datamodel": self.mcp_datamodel(),
            "loading": self.place_loading(),
            "unsaved_changes": doc.dirty(),
            "can_undo": doc.can_undo(),
            "instances": doc.dom.descendants().count(),
            "game_name": doc.dom.get_by_ref(doc.dom.root_ref()).map(|i| i.name.clone()),
            "mcp": { "url": self.mcp.url(), "groups": groups, "confirm": format!("{:?}", self.mcp.settings.confirm).to_lowercase() },
            "other_windows": others,
        }))
    }

    fn mcp_get_tree(&self, a: &J) -> R<ToolResult> {
        let root = self.mcp_resolve(s(a, "path").unwrap_or("game"))?;
        let q = tree::TreeQuery {
            depth: num(a, "depth").map_or(0, |d| d.clamp(1.0, 64.0) as usize),
            class: s(a, "class"),
            name: s(a, "name").map(String::from),
            offset: num(a, "offset").map_or(0, |o| o.max(0.0) as usize),
            limit: num(a, "limit").map_or(200, |l| l.clamp(1.0, 2000.0) as usize),
        };
        if let Some(c) = q.class {
            if !class_exists(c) {
                return Err(format!("{c:?} isn't a class name"));
            }
        }
        let mut out = tree::list(&self.game.doc, root, &q);
        out["datamodel"] = json!(self.mcp_datamodel());
        Ok(ToolResult::json(&out))
    }

    fn mcp_get_properties(&self, a: &J) -> R<ToolResult> {
        let r = self.mcp_resolve(req(a, "path")?)?;
        let doc = &self.game.doc;
        let inst = doc.dom.get_by_ref(r).ok_or("gone")?;
        let class = inst.class.to_string();
        let mut props = serde_json::Map::new();
        let mut attributes = J::Null;
        let mut tags = J::Null;
        let wanted: Option<Vec<String>> = a.get("properties").and_then(|p| p.as_array()).map(|v| v.iter().filter_map(|x| x.as_str().map(String::from)).collect());
        match &wanted {
            Some(names) => {
                for n in names {
                    let v = self
                        .live_sound_prop(r, n)
                        .or_else(|| doc.prop(r, n))
                        .map(|v| tree::value_json(&class, n, &v))
                        .unwrap_or(J::Null);
                    props.insert(n.clone(), v);
                }
            }
            None => {
                let mut names: Vec<_> = inst.properties.keys().copied().collect();
                names.sort_by(|x, y| x.as_str().cmp(y.as_str()));
                for n in names {
                    let v = &inst.properties[&n];
                    match (n.as_str(), v) {
                        ("Source", Variant::String(src)) => {
                            props.insert("Source".into(), json!(format!("<{} lines, {} bytes: get_source>", src.lines().count(), src.len())));
                        }
                        ("Attributes" | "Tags", _) => {}
                        _ => {
                            let val = self.live_sound_prop(r, n.as_str()).unwrap_or_else(|| v.clone());
                            props.insert(n.to_string(), tree::value_json(&class, n.as_str(), &val));
                        }
                    }
                }
                if self.game.play.is_none() {
                    let ea = &self.game.edit_audio;
                    if class == "Sound"
                        && (ea.is_loaded(r) || matches!(doc.prop(r, "Playing"), Some(Variant::Bool(true))))
                    {
                        for k in ["IsLoaded", "TimeLength", "IsPlaying", "IsPaused"] {
                            if let Some(val) = self.live_sound_prop(r, k) {
                                props.insert(k.to_string(), tree::value_json(&class, k, &val));
                            }
                        }
                    } else if class == "AudioPlayer" && (ea.is_loaded(r) || ea.preview_ref() == Some(r)) {
                        for k in ["IsReady", "TimeLength", "IsPlaying", "TimePosition"] {
                            if let Some(val) = self.live_sound_prop(r, k) {
                                props.insert(k.to_string(), tree::value_json(&class, k, &val));
                            }
                        }
                    }
                }
            }
        }
        if let Some(Variant::Attributes(at)) = inst.properties.get(&ustr("Attributes")) {
            let mut m = serde_json::Map::new();
            for (k, v) in at.iter() {
                m.insert(k.clone(), tree::attribute_json(v));
            }
            attributes = J::Object(m);
        }
        if let Some(Variant::Tags(t)) = inst.properties.get(&ustr("Tags")) {
            tags = json!(t.iter().collect::<Vec<_>>());
        }
        Ok(ToolResult::json(&json!({
            "path": tree::path(doc, r),
            "id": tree::id(r),
            "class": class,
            "name": inst.name,
            "children": inst.children().len(),
            "properties": props,
            "attributes": attributes,
            "tags": tags,
        })))
    }

    fn mcp_get_source(&self, a: &J) -> R<ToolResult> {
        let r = self.mcp_resolve(req(a, "path")?)?;
        let doc = &self.game.doc;
        let Some(Variant::String(src)) = doc.prop(r, "Source") else {
            return Err(format!("{} has no Source (a {})", tree::path(doc, r), doc.class_of(r).map(|c| c.to_string()).unwrap_or_default()));
        };
        let lines: Vec<&str> = src.lines().collect();
        let first = num(a, "start_line").map_or(1, |n| n.max(1.0) as usize);
        let last = num(a, "end_line").map_or(lines.len(), |n| (n as usize).min(lines.len()));
        let mut out = format!("-- {} ({} lines)\n", tree::path(doc, r), lines.len());
        for (i, l) in lines.iter().enumerate().take(last).skip(first.saturating_sub(1)) {
            out += &format!("{:5}\t{l}\n", i + 1);
        }
        Ok(ToolResult::text(out))
    }

    fn mcp_grep(&self, a: &J) -> R<ToolResult> {
        let pattern = req(a, "pattern")?;
        let limit = num(a, "limit").map_or(200, |l| l.clamp(1.0, 2000.0) as usize);
        let doc = &self.game.doc;
        let mut hits = Vec::new();
        let mut total = 0;
        for inst in doc.dom.descendants() {
            if !rustudio_core::doc::is_a(inst.class.as_str(), "LuaSourceContainer") {
                continue;
            }
            let Some(Variant::String(src)) = inst.properties.get(&ustr("Source")) else { continue };
            if !src.contains(pattern) {
                continue;
            }
            let p = tree::path(doc, inst.referent());
            for (n, l) in src.lines().enumerate() {
                if l.contains(pattern) {
                    total += 1;
                    if hits.len() < limit {
                        let mut t = l.trim().to_string();
                        if t.len() > 200 {
                            t.truncate(200);
                            t.push('…');
                        }
                        hits.push(format!("{p}:{}: {t}", n + 1));
                    }
                }
            }
        }
        let more = if total > hits.len() { format!("\n… {} more matches (raise limit)", total - hits.len()) } else { String::new() };
        Ok(ToolResult::text(if hits.is_empty() { format!("No script contains {pattern:?}") } else { hits.join("\n") + &more }))
    }

    fn mcp_read_output(&self, a: &J) -> ToolResult {
        let out = &self.mcp.output;
        let limit = num(a, "limit").map_or(200, |l| l.clamp(1.0, 5000.0) as usize);
        let levels: Option<Vec<String>> = a.get("levels").and_then(|l| l.as_array()).map(|v| v.iter().filter_map(|x| x.as_str().map(str::to_lowercase)).collect());
        let contains = s(a, "contains").map(str::to_lowercase);
        let since = a.get("since").and_then(|v| v.as_u64());
        let first = out.lines.front().map_or(out.next, |l| l.seq);
        let pick = |l: &&super::OutLine| {
            levels.as_ref().is_none_or(|lv| lv.iter().any(|x| x == l.kind)) && contains.as_ref().is_none_or(|c| l.text.to_lowercase().contains(c))
        };
        let lines: Vec<&super::OutLine> = match since {
            Some(seq) => out.since(seq).filter(pick).take(limit).collect(),
            None => {
                let all: Vec<&super::OutLine> = out.lines.iter().filter(pick).collect();
                all[all.len().saturating_sub(limit)..].to_vec()
            }
        };
        let next = match (since, lines.last()) {
            (Some(_), Some(l)) if lines.len() == limit => l.seq + 1,
            _ => out.next,
        };
        ToolResult::json(&json!({
            "lines": lines.iter().map(|l| json!({ "seq": l.seq, "at": l.at, "level": l.kind, "text": l.text })).collect::<Vec<_>>(),
            "next": next,
            "dropped": since.is_some_and(|s| s < first),
        }))
    }

    // ---------------------------------------------------------------- Luau

    fn mcp_run_luau(&mut self, a: &J) -> R<Step> {
        let code = req(a, "code")?.to_string();
        let target = s(a, "target").unwrap_or("edit");
        let timeout = Duration::from_secs_f64(num(a, "timeout").unwrap_or(15.0).clamp(0.1, 600.0));
        let id = self.mcp.next_eval_id();
        let out_from = self.mcp.output.next;
        let side = match target {
            "edit" => None,
            "server" => Some(Side::Server),
            "client" => Some(Side::Client),
            t => return Err(format!("target must be edit, server or client (not {t:?})")),
        };
        let done = match side {
            None => {
                if self.game.play.is_some() {
                    return Err("Play is running; the edit DataModel is put away until Stop. Use target \"server\" or \"client\", or call stop first.".into());
                }
                if self.place_loading() {
                    return Err("A place is still loading; try again when it has opened.".into());
                }
                let identity = rustudio_engine::runtime::security::COMMAND;
                self.edit_audio_tick();
                let ea_len = self.game.edit_audio.lengths.clone();
                let sync_done = self.plugins.with_edit_vm(&mut self.game.doc, |rt| -> R<Option<J>> {
                    if let Ok(mut w) = rustudio_engine::runtime::world(&rt.lua) {
                        for (&r, &len) in &ea_len {
                            w.input.sound_len.insert(r, len);
                        }
                    }
                    rustudio_player::luau_eval::start(&rt.lua, &code, id, identity)?;
                    rt.dispatch();
                    Ok(rustudio_player::luau_eval::poll(&rt.lua, id))
                })??;
                self.edit_audio_tick();
                if sync_done.is_some() {
                    self.game.doc.commit_pending_script_changes("Run Luau (MCP)");
                }
                sync_done.map(|v| (v, 0))
            }
            Some(side) => {
                if !self.game.is_playing() {
                    return Err(format!("Play isn't running: start it with play (mode \"play\" for a client, \"run\" for the server alone) to run Luau on the {side:?}."));
                }
                if side == Side::Client && !self.game.play_sides().contains(&Side::Client) {
                    return Err("This Play session has no client (Run mode): use target \"server\".".into());
                }
                self.game.mcp_eval_start(side, &code, id)?;
                None
            }
        };
        Ok(Step::Wait(Op::Eval { side, id, out_from, done }, timeout))
    }

    fn mcp_eval_poll(&mut self, side: Option<Side>, id: u64) -> Option<J> {
        match side {
            None => match self.plugins.with_edit_vm(&mut self.game.doc, |rt| rustudio_player::luau_eval::poll(&rt.lua, id)) {
                Ok(r) => r,
                Err(e) => Some(json!({ "ok": false, "error": e })),
            },
            Some(side) => self.game.mcp_eval_poll(side, id),
        }
    }

    // ---------------------------------------------------------------- edits

    /// Property JSON → (name, value) for `class`, all checked before use.
    fn mcp_resolve_props(class: &str, props: &serde_json::Map<String, J>) -> R<Props> {
        let mut out = Vec::new();
        let mut rename = None;
        let mut errors = Vec::new();
        for (k, v) in props {
            match k.as_str() {
                "Name" => match v.as_str() {
                    Some(n) => rename = Some(n.to_string()),
                    None => errors.push("Name: a string".to_string()),
                },
                "Parent" => errors.push("Parent: use move_instance".into()),
                "ClassName" => errors.push("ClassName can't change".into()),
                _ => match rustudio_core::project_values::resolve(class, k, v) {
                    Ok(x) => out.push(x),
                    Err(e) => errors.push(format!("{k}: {e}")),
                },
            }
        }
        if errors.is_empty() {
            Ok((out, rename))
        } else {
            Err(format!("Nothing was changed. {}", errors.join("; ")))
        }
    }

    fn mcp_set_properties(&mut self, a: &J) -> R<ToolResult> {
        let r = self.mcp_resolve(req(a, "path")?)?;
        let class = self.game.doc.class_of(r).ok_or("gone")?.to_string();
        let empty = serde_json::Map::new();
        let props = a.get("properties").and_then(|p| p.as_object()).unwrap_or(&empty);
        let (mut sets, rename) = Self::mcp_resolve_props(&class, props)?;
        if let Some(attrs) = a.get("attributes").and_then(|p| p.as_object()) {
            let mut cur = match self.game.doc.prop(r, "Attributes") {
                Some(Variant::Attributes(at)) => at,
                _ => Default::default(),
            };
            for (k, v) in attrs {
                if v.is_null() {
                    cur.remove(k.as_str());
                    continue;
                }
                match rustudio_core::project_values::resolve(&class, "Attributes", &json!({ "Attributes": { k.clone(): v } })) {
                    Ok((_, Variant::Attributes(one))) => {
                        if let Some(x) = one.get(k.as_str()) {
                            cur.insert(k.clone(), x.clone());
                        }
                    }
                    Ok(_) => return Err(format!("Nothing was changed. Attribute {k}: not an attribute value")),
                    Err(e) => return Err(format!("Nothing was changed. Attribute {k}: {e}")),
                }
            }
            sets.push(("Attributes".into(), Variant::Attributes(cur)));
        }
        if let Some(tags) = a.get("tags").and_then(|t| t.as_array()) {
            let mut t = rbx_dom_weak::types::Tags::default();
            for x in tags {
                t.push(x.as_str().ok_or("Nothing was changed. tags: strings")?);
            }
            sets.push(("Tags".into(), Variant::Tags(t)));
        }
        let n = sets.len();
        let source = sets.iter().find(|(k, _)| k == "Source").and_then(|(_, v)| if let Variant::String(s) = v { Some(s.clone()) } else { None });
        if !sets.is_empty() {
            self.game.doc.set_props("Set properties (MCP)", sets.into_iter().map(|(k, v)| (r, ustr(&k), v)).collect(), None);
        }
        if let Some(src) = source {
            self.mcp_sync_buffer(r, src);
        }
        if let Some(name) = &rename {
            self.game.doc.rename(r, name);
        }
        Ok(ToolResult::text(format!("Set {n} value(s){} on {}", if rename.is_some() { " and renamed it" } else { "" }, tree::path(&self.game.doc, r))))
    }

    /// An open script tab shows the new Source.
    fn mcp_sync_buffer(&mut self, r: Ref, src: String) {
        if let Some(buf) = self.scripts.bufs.get_mut(&r) {
            buf.text = src;
        }
    }

    fn mcp_set_source(&mut self, a: &J) -> R<ToolResult> {
        let r = self.mcp_resolve(req(a, "path")?)?;
        let src = req(a, "source")?.to_string();
        let class = self.game.doc.class_of(r).ok_or("gone")?;
        if !rustudio_core::doc::is_a(class.as_str(), "LuaSourceContainer") {
            return Err(format!("{} is a {class}, not a script", tree::path(&self.game.doc, r)));
        }
        let lines = src.lines().count();
        self.game.doc.set_props("Edit script (MCP)", vec![(r, ustr("Source"), Variant::String(src.clone()))], None);
        self.mcp_sync_buffer(r, src);
        let live = if self.live_team_create_ids().is_some() { " (a live draft: team_create_commit_drafts sends it)" } else { "" };
        Ok(ToolResult::text(format!("Wrote {lines} lines to {}{live}", tree::path(&self.game.doc, r))))
    }

    fn mcp_create(&mut self, a: &J) -> R<ToolResult> {
        let parent = self.mcp_resolve(req(a, "parent")?)?;
        let class = req(a, "class")?;
        if !class_exists(class) {
            return Err(format!("{class:?} isn't a class name"));
        }
        let empty = serde_json::Map::new();
        let (props, rename) = Self::mcp_resolve_props(class, a.get("properties").and_then(|p| p.as_object()).unwrap_or(&empty))?;
        let name = s(a, "name").map(String::from).or(rename).unwrap_or_else(|| class.to_string());
        let mut b = InstanceBuilder::new(class).with_name(name);
        for (k, v) in props {
            b.add_property(ustr(&k), v);
        }
        let r = self.game.doc.insert(parent, b);
        if self.game.doc.dom.get_by_ref(r).map(|i| i.parent()) != Some(parent) {
            return Err("The instance couldn't be made there".into());
        }
        Ok(ToolResult::json(&json!({ "path": tree::path(&self.game.doc, r), "id": tree::id(r) })))
    }

    fn mcp_clone(&mut self, a: &J) -> R<ToolResult> {
        let r = self.mcp_resolve(req(a, "path")?)?;
        let parent = match s(a, "parent") {
            Some(p) => self.mcp_resolve(p)?,
            None => self.game.doc.dom.get_by_ref(r).map(|i| i.parent()).ok_or("gone")?,
        };
        let bytes = self.game.doc.copy(&[r]).ok_or_else(|| format!("{} can't be copied (a service?)", tree::path(&self.game.doc, r)))?;
        let refs = self.game.doc.paste(&bytes, parent, "Clone (MCP)");
        let c = *refs.first().ok_or("The copy couldn't be placed there")?;
        Ok(ToolResult::json(&json!({ "path": tree::path(&self.game.doc, c), "id": tree::id(c) })))
    }

    fn mcp_delete(&mut self, a: &J) -> R<ToolResult> {
        let paths: Vec<&str> = a.get("paths").and_then(|p| p.as_array()).map(|v| v.iter().filter_map(|x| x.as_str()).collect()).unwrap_or_default();
        if paths.is_empty() {
            return Err("paths: a list of instance paths".into());
        }
        let mut refs = Vec::new();
        let mut errors = Vec::new();
        for p in &paths {
            match self.mcp_resolve(p) {
                Ok(r) if self.game.doc.can_edit_structure(r) => refs.push(r),
                Ok(_) => errors.push(format!("{p}: can't be deleted (a service or the DataModel)")),
                Err(e) => errors.push(e),
            }
        }
        if !errors.is_empty() {
            return Err(format!("Nothing was deleted. {}", errors.join("; ")));
        }
        let n = self.game.doc.remove(&refs);
        Ok(ToolResult::text(format!("Deleted {n} instance(s) (with their descendants)")))
    }

    fn mcp_move(&mut self, a: &J) -> R<ToolResult> {
        let r = self.mcp_resolve(req(a, "path")?)?;
        let parent = s(a, "parent").map(|p| self.mcp_resolve(p)).transpose()?;
        let name = s(a, "name");
        if parent.is_none() && name.is_none() {
            return Err("give a parent and/or a name".into());
        }
        if let Some(p) = parent {
            if p == r || self.game.doc.is_ancestor(r, p) {
                return Err("Nothing was moved: the new parent is inside the instance".into());
            }
            if !self.game.doc.can_edit_structure(r) {
                return Err("Nothing was moved: services and the DataModel stay where they are".into());
            }
            if self.game.doc.dom.get_by_ref(r).map(|i| i.parent()) != Some(p) && self.game.doc.reparent(&[r], p) == 0 {
                return Err("Nothing was moved: rustudio refused that parent".into());
            }
        }
        if let Some(n) = name {
            self.game.doc.rename(r, n);
        }
        Ok(ToolResult::json(&json!({ "path": tree::path(&self.game.doc, r), "id": tree::id(r) })))
    }

    fn mcp_insert_model(&mut self, a: &J) -> R<ToolResult> {
        let file = path_arg(a, "file")?;
        let parent = match s(a, "parent") {
            Some(p) => self.mcp_resolve(p)?,
            None => self.game.doc.workspace().ok_or("no Workspace")?,
        };
        let bytes = std::fs::read(&file).map_err(|e| format!("{}: {e}", file.display()))?;
        rustudio_core::doc::Document::decode(&bytes).map_err(|e| format!("{}: {e}", file.display()))?;
        let refs = self.game.doc.paste(&bytes, parent, "Insert model (MCP)");
        Ok(ToolResult::json(&json!({ "inserted": refs.iter().map(|r| tree::path(&self.game.doc, *r)).collect::<Vec<_>>() })))
    }

    fn mcp_apply_project(&mut self, a: &J) -> R<ToolResult> {
        let dry = flag(a, "dry_run", true);
        if !dry {
            self.mcp_writable()?;
        }
        let file = project_apply::project_file(&path_arg(a, "project")?)?;
        let (snap, warnings) = project_apply::read(&file)?;
        let plan = project_apply::plan(&self.game.doc, &snap, flag(a, "delete_extra", false));
        let mut report = project_apply::report(&self.game.doc, &plan);
        report["project"] = json!(file.display().to_string());
        report["warnings"] = json!(warnings);
        report["dry_run"] = json!(dry);
        if !dry {
            let (made, set, gone) = project_apply::apply(&mut self.game.doc, plan)?;
            for r in self.game.doc.dom.descendants().filter(|i| rustudio_core::doc::is_a(i.class.as_str(), "LuaSourceContainer")).map(|i| i.referent()).collect::<Vec<_>>() {
                if let (Some(buf), Some(Variant::String(src))) = (self.scripts.bufs.get_mut(&r), self.game.doc.prop(r, "Source")) {
                    buf.text = src;
                }
            }
            report["applied"] = json!({ "instances_created": made, "properties_set": set, "instances_deleted": gone });
            if self.live_team_create_ids().is_some() {
                report["next"] = json!("Script Source changes are live drafts: team_create_commit_drafts, then team_create_wait.");
            }
        }
        Ok(ToolResult::json(&report))
    }

    fn mcp_undo(&mut self, undo: bool) -> R<ToolResult> {
        let label = if undo { self.game.doc.can_undo() } else { self.game.doc.can_redo() }.map(String::from);
        let Some(label) = label else {
            return Err(format!("Nothing to {}", if undo { "undo" } else { "redo" }));
        };
        self.run_action(if undo { "undo" } else { "redo" });
        Ok(ToolResult::text(format!("{} {label}", if undo { "Undid" } else { "Redid" })))
    }

    fn mcp_plugin(&mut self, tool: &str, a: &J) -> R<ToolResult> {
        if tool == "load_plugin" {
            let file = path_arg(a, "file")?;
            if !file.is_file() {
                return Err(format!("{} isn't a file", file.display()));
            }
            let id = self.plugins.load(crate::plugin_host::PluginSource::File(file.clone()))?;
            return Ok(ToolResult::json(&json!({ "id": id.0, "file": file.display().to_string(), "note": "It starts at the next frame; list_plugins shows its errors, read_output its prints." })));
        }
        let want = a.get("plugin").ok_or("plugin: an id or a name")?;
        let list = self.plugins.loaded();
        let found = list.iter().find(|p| match want {
            J::Number(n) => n.as_u64() == Some(p.id.0 as u64),
            J::String(s) => p.name == *s || s.parse::<u64>().ok() == Some(p.id.0 as u64) || p.source.path().is_some_and(|f| f.display().to_string() == *s),
            _ => false,
        });
        let p = found.ok_or_else(|| format!("No loaded plugin is {want} (loaded: {})", list.iter().map(|p| format!("{} {}", p.id.0, p.name)).collect::<Vec<_>>().join(", ")))?;
        if tool == "reload_plugin" {
            self.plugins.reload(p.id);
            Ok(ToolResult::text(format!("Reloading {} at the next frame", p.name)))
        } else {
            self.plugins.unload(p.id);
            Ok(ToolResult::text(format!("Unloading {} at the next frame", p.name)))
        }
    }

    fn mcp_ui_action(&mut self, a: &J) -> R<ToolResult> {
        let name = req(a, "action")?;
        if let Some(t) = crate::plugin_host::Trigger::parse(name) {
            match t {
                crate::plugin_host::Trigger::Button(tb, b) => self.plugin_harness_click(&tb, &b),
                t => self.plugins.trigger(t),
            }
            return Ok(ToolResult::text(format!("Fired {name}")));
        }
        let (enabled, _) = self.action_state(name);
        if !enabled {
            return Err(format!("{name:?} isn't available now (unknown, or greyed out in this state)"));
        }
        self.run_action(name);
        Ok(ToolResult::text(format!("Ran {name}")))
    }

    // ---------------------------------------------------------------- play

    fn mcp_play(&mut self, a: &J) -> R<Step> {
        use crate::studio_ui::RunType;
        if self.game.play.is_some() {
            return Err(format!("Already running ({}); call stop first", self.mcp_mode()));
        }
        let t = match s(a, "mode").unwrap_or("play") {
            "play" => RunType::Test,
            "run" => RunType::Run,
            m => return Err(format!("mode must be play or run (not {m:?})")),
        };
        self.start_run(t);
        Ok(Step::Wait(Op::Play { want: true }, Duration::from_secs(120)))
    }

    fn mcp_stop(&mut self) -> R<Step> {
        if self.game.play.is_none() {
            return Ok(Step::Done(ToolResult::text("Not playing")));
        }
        self.stop_play();
        Ok(Step::Wait(Op::Play { want: false }, Duration::from_secs(60)))
    }

    fn mcp_play_state(&self) -> J {
        json!({
            "mode": self.mcp_mode(),
            "datamodel": self.mcp_datamodel(),
            "sides": self.game.play_sides().iter().map(|s| format!("{s:?}").to_lowercase()).collect::<Vec<_>>(),
        })
    }

    fn mcp_send_input(&mut self, a: &J) -> R<Step> {
        use crate::app::HarnessAction as H;
        let steps = a.get("steps").and_then(|s| s.as_array()).ok_or("steps: a list")?;
        let now = self.harness.run.now().ok_or("The input clock isn't running (a --from-play run before Play)")?;
        let mut out = Vec::new();
        let mut until = now;
        for (i, st) in steps.iter().enumerate() {
            let at = now + num(st, "at").unwrap_or(0.0).max(0.0) as f32;
            let bad = |what: &str| format!("Nothing was sent. step {}: {what}", i + 1);
            if let Some(k) = s(st, "key") {
                let (key, mods) = rustudio_player::run_harness::parse_harness_key(k).ok_or_else(|| bad(&format!("{k:?} isn't a key name (egui names: A, Space, Enter, ArrowUp, F1, ctrl+a…)")))?;
                match num(st, "hold") {
                    Some(h) if h > 0.0 => {
                        out.push((at, H::KeyState(key, mods, true)));
                        out.push((at + h as f32, H::KeyState(key, mods, false)));
                        until = until.max(at + h as f32);
                    }
                    _ => out.push((at, H::Key(key, mods))),
                }
            } else if let Some(xy) = st.get("click").and_then(|c| c.as_array()) {
                let v: Vec<f32> = xy.iter().filter_map(|x| x.as_f64().map(|f| f as f32)).collect();
                let [x, y] = v[..] else { return Err(bad("click: [x, y]")) };
                out.push((at, H::Click(x, y)));
            } else if let Some(t) = s(st, "type") {
                out.push((at, H::Text(t.to_string())));
            } else if let Some(n) = num(st, "wheel") {
                out.push((at, H::Wheel(n as f32, None)));
            } else if let Some(m) = st.get("move").and_then(|c| c.as_array()) {
                let v: Vec<f32> = m.iter().filter_map(|x| x.as_f64().map(|f| f as f32)).collect();
                let [dx, dy] = v[..] else { return Err(bad("move: [dx, dy]")) };
                out.push((at, H::MouseMotion(dx, dy)));
            } else {
                return Err(bad("each step needs key, click, type, wheel or move"));
            }
            until = until.max(at);
        }
        self.harness.run.input.actions.extend(out);
        Ok(Step::Wait(Op::Input { until: until + 0.1 }, Duration::from_secs_f32(until - now + 30.0)))
    }

    fn mcp_set_camera(&mut self, a: &J) -> R<ToolResult> {
        let p: Vec<f32> = a.get("position").and_then(|p| p.as_array()).map(|v| v.iter().filter_map(|x| x.as_f64().map(|f| f as f32)).collect()).unwrap_or_default();
        let [x, y, z] = p[..] else { return Err("position: [x, y, z]".into()) };
        let cam = &mut self.game.cam;
        cam.pos = glam::Vec3::new(x, y, z);
        if let Some(yaw) = num(a, "yaw") {
            cam.yaw = (yaw as f32).to_radians();
        }
        if let Some(pitch) = num(a, "pitch") {
            cam.pitch = (pitch as f32).to_radians();
        }
        if let Some(fov) = num(a, "fov") {
            cam.fov_deg = (fov as f32).clamp(1.0, 179.0);
        }
        cam.focus = cam.pos + cam.forward() * 50.0;
        Ok(ToolResult::text("Camera moved"))
    }

    // ---------------------------------------------------------------- places

    fn mcp_may_replace(&self, a: &J) -> R<()> {
        if flag(a, "discard_changes", false) {
            return Ok(());
        }
        if self.game.doc.dirty() && self.live_team_create_ids().is_none() {
            return Err("The open place has unsaved changes. Save them (save_place / ui_action \"save\") or pass discard_changes: true.".into());
        }
        if let Some(l) = self.mcp_live_activity() {
            if l.pending_commits > 0 || !self.mcp_live_drafts().is_empty() || l.unsynced > 0 || l.sync_failed {
                return Err("The live Team Create session has drafts, commits in flight or edits that didn't reach the server (team_create_status). Commit or discard them, or pass discard_changes: true.".into());
            }
        }
        Ok(())
    }

    fn mcp_open_place(&mut self, a: &J) -> R<Step> {
        let file = path_arg(a, "file")?;
        if !file.exists() {
            return Err(format!("{} doesn't exist", file.display()));
        }
        self.mcp_may_replace(a)?;
        if self.game.play.is_some() {
            self.stop_play();
        }
        let before = self.game.doc.id;
        self.open_path(file.clone());
        if self.game.doc.id == before {
            return Err(format!("{} didn't open (read_output has the reason)", file.display()));
        }
        Ok(Step::Done(self.mcp_session_info()))
    }

    fn mcp_open_roblox(&mut self, a: &J) -> R<Step> {
        let place = a.get("place_id").and_then(|p| p.as_u64()).ok_or("place_id: a number")?;
        self.mcp_may_replace(a)?;
        if self.game.play.is_some() {
            self.stop_play();
        }
        self.cloud.open_error = None;
        rustudio_account::session::set_place_context(place);
        self.perform_open_cloud(place, a.get("version").and_then(|v| v.as_u64()), flag(a, "local_copy", false));
        Ok(Step::Wait(Op::Load { live: false, frames: 0 }, Duration::from_secs(300)))
    }

    fn mcp_join(&mut self, a: &J) -> R<Step> {
        let universe = a.get("universe_id").and_then(|p| p.as_u64()).ok_or("universe_id: a number")?;
        let place = a.get("place_id").and_then(|p| p.as_u64()).ok_or("place_id: a number")?;
        if self.live_team_create_ids() == Some((universe, place)) {
            return Ok(Step::Done(ToolResult::text("Already in that Team Create session")));
        }
        self.mcp_may_replace(a)?;
        if self.game.play.is_some() {
            self.stop_play();
        }
        self.cloud.open_error = None;
        self.open_team_create(universe, place);
        Ok(Step::Wait(Op::Load { live: true, frames: 0 }, Duration::from_secs(180)))
    }

    fn mcp_save_place(&mut self, a: &J) -> R<ToolResult> {
        let file = path_arg(a, "file")?;
        if self.game.play.is_some() {
            return Err("Play is running: stop first (the edit DataModel is put away while Play runs)".into());
        }
        let ext = file.extension().and_then(|e| e.to_str()).unwrap_or("").to_ascii_lowercase();
        let xml = match ext.as_str() {
            "rbxl" | "rbxm" => false,
            "rbxlx" | "rbxmx" => true,
            _ => return Err("file must end in .rbxl or .rbxlx (.rbxm / .rbxmx for a model)".into()),
        };
        if let Some(dir) = file.parent() {
            std::fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
        }
        if flag(a, "copy", true) {
            let bytes = self.game.doc.encode_all(xml)?;
            std::fs::write(&file, &bytes).map_err(|e| format!("{}: {e}", file.display()))?;
            return Ok(ToolResult::text(format!("Saved a copy ({} bytes, {} instances) to {}; the window's place is unchanged", bytes.len(), self.game.doc.dom.descendants().count(), file.display())));
        }
        if self.live_team_create_ids().is_some() {
            return Err("A live Team Create place can't become a file's place; save a copy (copy: true)".into());
        }
        self.game.doc.save_to(&file)?;
        Ok(ToolResult::text(format!("Saved to {} (now the place's file)", file.display())))
    }

    // ---------------------------------------------------------------- live

    fn mcp_drafts(&mut self, a: &J, commit: bool) -> R<ToolResult> {
        if self.live_team_create_ids().is_none() {
            return Err("This window isn't in a live Team Create session".into());
        }
        let refs: Vec<Ref> = match a.get("paths").and_then(|p| p.as_array()) {
            Some(v) => v.iter().filter_map(|x| x.as_str()).map(|p| self.mcp_resolve(p)).collect::<R<Vec<_>>>()?,
            None => self.mcp_live_drafts(),
        };
        if refs.is_empty() {
            return Ok(ToolResult::text("No script drafts"));
        }
        let mut results = Vec::new();
        let mut failed = false;
        for r in refs {
            let p = tree::path(&self.game.doc, r);
            if commit {
                match self.mcp_commit_draft(r) {
                    Ok(m) => results.push(json!({ "path": p, "result": m })),
                    Err(e) => {
                        failed = true;
                        results.push(json!({ "path": p, "error": e }));
                    }
                }
            } else {
                self.discard_live_draft(r);
                results.push(json!({ "path": p, "result": "discarded; the server's Source is back" }));
            }
        }
        let mut r = ToolResult::json(&json!({ "scripts": results, "next": if commit { "team_create_wait, then team_create_status" } else { "" } }));
        r.is_error = failed;
        Ok(r)
    }

    fn mcp_tc_wait(&mut self, a: &J) -> R<Step> {
        let Some(l) = self.mcp_live_activity() else {
            return Err("This window isn't in a live Team Create session".into());
        };
        let quiet = num(a, "quiet").unwrap_or(3.0).clamp(0.0, 60.0) as f32;
        let timeout = Duration::from_secs_f64(num(a, "timeout").unwrap_or(30.0).clamp(1.0, 600.0));
        Ok(Step::Wait(Op::TcWait { quiet, out_from: self.mcp.output.next, failed_before: l.sync_failed }, timeout))
    }

    /// Warnings and errors in the Output since `seq` (the live sync reports
    /// edits it couldn't send there).
    fn mcp_tc_problems(&self, seq: u64) -> Vec<String> {
        self.mcp
            .output
            .since(seq)
            .filter(|l| l.kind == "warn" || l.kind == "error")
            .map(|l| l.text.clone())
            .take(100)
            .collect()
    }

    // ---------------------------------------------------------------- polling

    /// One frame of a waiting call: Some(answer) when it's done.
    pub(crate) fn mcp_poll(&mut self, op: &mut Op, ctx: &egui::Context) -> Option<ToolResult> {
        match op {
            Op::Eval { side, id, out_from, done } => {
                if done.is_none() {
                    *done = self.mcp_eval_poll(*side, *id).map(|v| (v, 0));
                    if done.is_some() && side.is_none() {
                        self.game.doc.commit_pending_script_changes("Run Luau (MCP)");
                    }
                }
                // Prints reach the Output a frame or two after the chunk.
                let (v, frames) = done.as_mut()?;
                *frames += 1;
                if *frames < 3 {
                    return None;
                }
                if side.is_none() {
                    self.game.doc.commit_pending_script_changes("Run Luau (MCP)");
                }
                let output = self.mcp.output.text_since(*out_from, 400);
                let ok = v["ok"].as_bool().unwrap_or(false);
                let mut body = json!({ "output": output });
                if ok {
                    body["returned"] = v["values"].clone();
                } else {
                    body["error"] = v["error"].clone();
                }
                let mut r = ToolResult::json(&body);
                r.is_error = !ok;
                Some(r)
            }
            Op::Shot { save, requested, settled, asked } => self.mcp_poll_shot(ctx, save.clone(), requested, settled, *asked),
            Op::Play { want } => {
                let playing = self.game.play.is_some();
                if playing == *want && (!*want || !self.place_loading()) {
                    let mut st = self.mcp_play_state();
                    st["output_errors"] = json!(self.mcp.output.lines.iter().rev().take(200).filter(|l| l.kind == "error").map(|l| l.text.clone()).take(10).collect::<Vec<_>>());
                    return Some(ToolResult::json(&st));
                }
                None
            }
            Op::Input { until } => (self.harness.run.now().unwrap_or(f32::MAX) >= *until).then(|| ToolResult::text("Input sent")),
            Op::Load { live, frames } => {
                if let Some(e) = self.cloud.open_error.clone() {
                    return Some(ToolResult::error(format!("The place didn't open: {e}")));
                }
                let ready = if *live { self.live_team_create_ids().is_some() } else { !self.place_loading() };
                if !ready {
                    *frames = 0;
                    return None;
                }
                *frames += 1;
                (*frames > 2).then(|| self.mcp_session_info())
            }
            Op::TcWait { quiet, out_from, failed_before } => {
                let Some(l) = self.mcp_live_activity() else {
                    return Some(ToolResult::error(format!(
                        "The Team Create session ended. {}",
                        self.mcp_tc_problems(*out_from).join("\n")
                    )));
                };
                // The last edit queued (else the document's last change).
                let last = l.last_sent.map_or(self.mcp.doc_rev.2, |t| t.max(self.mcp.doc_rev.2));
                let calm = last.elapsed().as_secs_f32() >= *quiet && l.pending_commits == 0;
                if !calm {
                    return None;
                }
                let problems = self.mcp_tc_problems(*out_from);
                let mut st = self.mcp_live_status();
                let failed = l.sync_failed && !*failed_before;
                let ok = !failed && !problems.iter().any(|p| p.contains("Team Create") || p.contains("synchroniz"));
                st["settled"] = json!(true);
                st["sync_failed_during_wait"] = json!(failed);
                st["warnings_since_call"] = json!(problems);
                let mut r = ToolResult::json(&st);
                r.is_error = !ok;
                Some(r)
            }
        }
    }

    pub(crate) fn mcp_timeout(&mut self, op: &Op) -> ToolResult {
        match op {
            Op::Eval { side, id, out_from, .. } => {
                match side {
                    None => {
                        let _ = self.plugins.with_edit_vm(&mut self.game.doc, |rt| rustudio_player::luau_eval::forget(&rt.lua, *id));
                        self.game.doc.commit_pending_script_changes("Run Luau (MCP)");
                    }
                    Some(side) if self.game.is_playing() => self.game.mcp_eval_forget(*side, *id),
                    Some(_) => {}
                }
                let output = self.mcp.output.text_since(*out_from, 200);
                let mut r = ToolResult::json(&json!({ "error": "still running at the timeout; it keeps running in rustudio, its return values are dropped", "output": output }));
                r.is_error = true;
                r
            }
            Op::Shot { .. } => ToolResult::error("The screenshot didn't come back"),
            Op::Play { want: true } => ToolResult::error(format!("Play didn't start in 2 minutes ({}); read_output may say why", self.mcp_mode())),
            Op::Play { want: false } => ToolResult::error("Play didn't stop in a minute"),
            Op::Input { .. } => ToolResult::error("The input wasn't all sent in time"),
            Op::Load { live, .. } => ToolResult::error(if *live { "Still joining the Team Create session (it keeps going; session_info shows when it's in)" } else { "The place is still loading (it keeps going; session_info shows when it's in)" }),
            Op::TcWait { out_from, .. } => {
                let mut st = self.mcp_live_status();
                st["settled"] = json!(false);
                st["warnings_since_call"] = json!(self.mcp_tc_problems(*out_from));
                let mut r = ToolResult::json(&st);
                r.is_error = true;
                r
            }
        }
    }

    fn mcp_poll_shot(&mut self, ctx: &egui::Context, save: Option<PathBuf>, requested: &mut bool, settled: &mut bool, asked: Instant) -> Option<ToolResult> {
        if !*requested {
            let ready = self.game.viewport_rect.is_positive()
                && !self.place_loading()
                && self.game.draw_cache.waiting.is_empty()
                && rustudio_core::assets::content_queue_size() == 0
                && !self.game.assets.busy()
                && !self.game.textures.decoding();
            if !ready && asked.elapsed() < Duration::from_secs(20) {
                ctx.request_repaint();
                return None;
            }
            *requested = true;
            *settled = ready;
            let direct = self.game.renderer.as_ref().zip(self.game.render_state.as_ref()).and_then(|(r, rs)| r.read_frame(rs));
            if let Some((w, h, rgba)) = direct {
                return Some(shot_result(w, h, rgba, save.as_deref(), *settled));
            }
            ctx.send_viewport_cmd(egui::ViewportCommand::Screenshot(Default::default()));
            return None;
        }
        let image = ctx.input(|i| {
            i.raw.events.iter().find_map(|e| match e {
                egui::Event::Screenshot { image, .. } => Some(image.clone()),
                _ => None,
            })
        });
        let image = image?;
        let ppp = ctx.pixels_per_point();
        let (x0, y0, w, h) = crate::app::view_pixels(self.game.viewport_rect, ppp, image.size);
        let mut rgba = Vec::with_capacity(w * h * 4);
        for y in 0..h {
            for x in 0..w {
                let p = image.pixels[(y0 + y) * image.size[0] + x0 + x];
                rgba.extend_from_slice(&[p.r(), p.g(), p.b(), 255]);
            }
        }
        Some(shot_result(w as u32, h as u32, rgba, save.as_deref(), *settled))
    }
}

/// The view as an MCP image (scaled to at most 1600 wide), saved too when asked.
fn shot_result(w: u32, h: u32, mut rgba: Vec<u8>, save: Option<&Path>, settled: bool) -> ToolResult {
    for px in rgba.chunks_exact_mut(4) {
        px[3] = 255;
    }
    let Some(img) = image::RgbaImage::from_raw(w, h, rgba) else {
        return ToolResult::error("The view's pixels didn't make an image");
    };
    let mut note = format!("{w}x{h}{}", if settled { "" } else { " (taken before everything had loaded)" });
    if let Some(p) = save {
        match img.save(p) {
            Ok(()) => note += &format!(", saved to {}", p.display()),
            Err(e) => return ToolResult::error(format!("{}: {e}", p.display())),
        }
    }
    let img = if w > 1600 { image::imageops::resize(&img, 1600, (h as f32 * 1600.0 / w as f32) as u32, image::imageops::FilterType::Triangle) } else { img };
    let mut png = std::io::Cursor::new(Vec::new());
    if let Err(e) = img.write_to(&mut png, image::ImageFormat::Png) {
        return ToolResult::error(format!("PNG: {e}"));
    }
    ToolResult {
        content: vec![json!({ "type": "image", "data": base64(png.get_ref()), "mimeType": "image/png" }), json!({ "type": "text", "text": note })],
        is_error: false,
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn base64_matches_the_standard() {
        assert_eq!(super::base64(b""), "");
        assert_eq!(super::base64(b"f"), "Zg==");
        assert_eq!(super::base64(b"fo"), "Zm8=");
        assert_eq!(super::base64(b"foo"), "Zm9v");
        assert_eq!(super::base64(b"foobar"), "Zm9vYmFy");
    }
}
