//! The Toolbox's side of the editor: per-frame tick, inserting what the
//! workers prepared (as one undo step), the deep links, the 3D view's and
//! Explorer's drop targets.

use std::sync::atomic::Ordering;

use glam::Vec3;
use rbx_dom_weak::types::{Ref, UDim, UDim2, Variant};
use rbx_dom_weak::InstanceBuilder;

use super::categories::Kind;
use super::insert::{self, Home, Prepared, Ready, Target};
use super::ui::ToolboxDrag;
use super::{Ask, Msg};
use crate::app::Editor;

impl Editor {
    /// Once a frame (panel shown or not): session, worker replies, inserts.
    pub fn toolbox_tick(&mut self, ctx: &egui::Context) {
        let t0 = std::time::Instant::now();
        // The account's client is locked by its own jobs (sign-in, avatar):
        // never wait for it here, and look at most twice a second.
        if self.toolbox.ui.session_checked.is_none_or(|t| t.elapsed().as_millis() > 500) {
            self.toolbox.ui.session_checked = Some(std::time::Instant::now());
            if let Some(c) = self.game.account.shared_client() {
                let got = c.try_lock().ok().map(|g| g.as_ref().map(|c| (c.session.downloader(), c.user_id())));
                if let Some(s) = got {
                    self.toolbox.set_session(s.filter(|_| std::env::var_os("RUSTUDIO_NO_SESSION").is_none()));
                }
            } else {
                self.toolbox.set_session(None);
            }
        }
        let ready = self.toolbox.poll();
        for r in ready {
            match r {
                // (cancelled after its file was read: nothing is inserted)
                Ok(ready) if !self.toolbox.inserts.iter().any(|j| j.job == ready.job && !j.cancel.load(Ordering::Relaxed)) => {
                    self.toolbox.inserts.retain(|j| j.job != ready.job);
                    self.toolbox.harness.insert(format!("insert_{}", ready.asset.id), serde_json::json!({"ok": false, "error": "cancelled"}));
                }
                Ok(ready) => self.toolbox_ready(ready),
                Err((job, id, e)) => {
                    self.toolbox.inserts.retain(|j| j.job != job);
                    let msg = format!("Toolbox failed to insert asset {id}: {}", e.describe());
                    self.toolbox.harness.insert(format!("insert_{id}"), serde_json::json!({"ok": false, "error": e.describe()}));
                    if e != super::api::Failure::Cancelled {
                        self.error(msg);
                    }
                }
            }
        }
        // Plugin installs started here: done when the store isn't busy with them.
        let installing: Vec<u64> = self.toolbox.ui.installing.iter().copied().collect();
        for id in installing {
            let cid = crate::plugin_store::cloud_id(id);
            if self.plugin_store.busy.contains(&cid) {
                continue;
            }
            self.toolbox.ui.installing.remove(&id);
            let ok = self.plugin_store.store.get(&cid).is_some();
            let err = self.plugin_store.errors.get(&cid).cloned();
            self.toolbox.harness.insert(format!("install_{id}"), serde_json::json!({"ok": ok, "error": err}));
            if ok {
                self.toolbox.owned.insert(id, true);
                self.info(format!("Installed plugin {id}"));
            }
        }
        for (err, line) in std::mem::take(&mut self.toolbox.log) {
            if err { self.error(line) } else { self.info(line) }
        }
        if self.toolbox.thumbs.tick(ctx) || !self.toolbox.inserts.is_empty() || self.toolbox.pool.busy.load(Ordering::Relaxed) > 0 {
            ctx.request_repaint_after(std::time::Duration::from_millis(50));
        }
        if let Some(a) = self.toolbox.player.audio.as_mut() {
            for k in a.poll_finished() {
                if self.toolbox.player.playing == Some(k) {
                    self.toolbox.player.playing = None;
                }
            }
        }
        if self.toolbox.player.playing.is_some() {
            ctx.request_repaint_after(std::time::Duration::from_millis(100));
        }
        self.toolbox.save_if_changed(false);
        let ms = t0.elapsed().as_secs_f32() * 1000.0;
        if self.toolbox.frame_ms.len() < 100_000 {
            self.toolbox.frame_ms.push(ms);
        }
    }

    /// The point Studio inserts at: the ray through the 3D view's centre
    /// hits something within 2048 studs, else 64 studs in front.
    pub fn toolbox_insert_point(&self) -> Vec3 {
        let rect = if self.game.viewport_rect.is_positive() { self.game.viewport_rect } else { egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(1280.0, 720.0)) };
        let ray = self.game.cam.ray(rect, rect.center());
        match self.ray_distance(&ray).filter(|t| *t <= 2048.0) {
            Some(t) => ray.origin + ray.dir * t,
            None => self.game.cam.pos + ray.dir * 64.0,
        }
    }

    fn world_boxes(&self) -> Vec<(Vec3, Vec3)> {
        let Some(ws) = self.game.doc.workspace() else { return Vec::new() };
        insert_boxes(&self.game.doc, ws)
    }

    fn toolbox_ready(&mut self, ready: Ready) {
        // Questions first (Studio's dialogs), then the insert.
        if let Prepared::Model { scripts, .. } = &ready.prepared {
            let mine = ready.asset.creator.id != 0 && ready.asset.creator.id == self.toolbox.user_id && ready.asset.creator.kind != 2;
            if *scripts > 0 && !mine && std::env::var_os("RUSTUDIO_TOOLBOX_NO_SCRIPT_PROMPT").is_none() {
                let count = *scripts;
                self.toolbox.ask = Some(Ask::Scripts { ready, count });
                return;
            }
            if let Prepared::Model { dom, .. } = &ready.prepared {
                let tool = dom.root().children().iter().any(|r| dom.get_by_ref(*r).is_some_and(|i| insert::home_of(i.class.as_str(), &ready.asset.sub_types) == Home::AskToolHome));
                if tool && !matches!(ready.target, Target::Parent(_)) {
                    self.toolbox.ask = Some(Ask::ToolHome { ready });
                    return;
                }
            }
        }
        self.toolbox_apply(ready, None);
    }

    /// Answers to the dialogs.
    pub fn toolbox_answer(&mut self, insert_it: bool, starter_pack: bool) {
        let Some(ask) = self.toolbox.ask.take() else { return };
        match ask {
            Ask::Scripts { ready, .. } => {
                if !insert_it {
                    self.toolbox.inserts.retain(|j| j.job != ready.job);
                    self.toolbox.harness.insert(format!("insert_{}", ready.asset.id), serde_json::json!({"ok": false, "error": "cancelled"}));
                    return;
                }
                let tool = matches!(&ready.prepared, Prepared::Model { dom, .. } if dom.root().children().iter().any(|r| dom.get_by_ref(*r).is_some_and(|i| matches!(i.class.as_str(), "Tool" | "HopperBin"))));
                if tool && !matches!(ready.target, Target::Parent(_)) {
                    self.toolbox.ask = Some(Ask::ToolHome { ready });
                } else {
                    self.toolbox_apply(ready, None);
                }
            }
            Ask::ToolHome { ready } => {
                if !insert_it {
                    self.toolbox.inserts.retain(|j| j.job != ready.job);
                    return;
                }
                let home = if starter_pack { self.game.doc.find_child(self.game.doc.dom.root_ref(), "StarterPack") } else { None };
                self.toolbox_apply(ready, Some(home.unwrap_or_else(|| self.game.doc.default_parent())));
            }
            Ask::GetPlugin { asset } => {
                if insert_it {
                    let client = self.game.account.shared_client();
                    self.plugin_store.install(client, asset.id);
                    self.toolbox.note_recent(asset.id, asset.type_id);
                    self.toolbox.ui.installing.insert(asset.id);
                    self.info(format!("Installing plugin {} \"{}\"…", asset.id, asset.name));
                }
            }
            Ask::Paid { asset } => {
                if insert_it {
                    crate::toolbox::ui::open_url(&super::api::store_url(asset.id, Kind::from_asset_type(asset.type_id).unwrap_or(Kind::Model)));
                }
            }
        }
    }

    /// The UI thread part of an insert: placement and one undo step.
    pub fn toolbox_apply(&mut self, ready: Ready, tool_parent: Option<Ref>) {
        let Ready { job, asset, kind, target, prepared } = ready;
        self.toolbox.inserts.retain(|j| j.job != job);
        let label = insert::waypoint(asset.id, kind);
        let id = asset.id;
        let name = if asset.name.is_empty() { format!("Asset {id}") } else { asset.name.clone() };
        let sel_first = self.game.doc.selection.first().copied().filter(|r| self.game.doc.exists(*r));
        let ws = self.game.doc.default_parent();
        let explicit = match target {
            Target::Parent(p) if self.game.doc.exists(p) => Some(p),
            _ => None,
        };
        let source_id = Variant::Int64(id as i64);
        let inserted: Vec<Ref> = match prepared {
            Prepared::Plugin => {
                self.toolbox_install_plugin(asset);
                return;
            }
            Prepared::Font => {
                // Studio's insertFont does nothing; the font is in the account's
                // inventory for the font pickers.
                self.info(format!("Fonts aren't inserted: \"{name}\" can be picked in a FontFace property."));
                self.toolbox.harness.insert(format!("insert_{id}"), serde_json::json!({"ok": true, "kind": "Font", "inserted": []}));
                self.toolbox.note_recent(id, asset.type_id);
                return;
            }
            Prepared::Sound => {
                let parent = explicit.or(sel_first).unwrap_or(ws);
                let b = InstanceBuilder::new("Sound")
                    .with_name(name.clone())
                    .with_property("SoundId", Variant::ContentId(format!("rbxassetid://{id}").into()))
                    .with_property("SourceAssetId", source_id);
                self.toolbox_insert_built(vec![(b, parent)], &label)
            }
            Prepared::Video => {
                let parent = explicit.or(sel_first).unwrap_or(ws);
                let b = InstanceBuilder::new("VideoFrame")
                    .with_name(name.clone())
                    .with_property("Video", Variant::ContentId(format!("rbxassetid://{id}").into()))
                    .with_property("Size", Variant::UDim2(UDim2::new(UDim::new(1.0, 0), UDim::new(1.0, 0))))
                    .with_property("SourceAssetId", source_id);
                self.toolbox_insert_built(vec![(b, parent)], &label)
            }
            Prepared::Model { mut dom, .. } => {
                let roots = dom.root().children().to_vec();
                for r in &roots {
                    if let Some(i) = dom.get_by_ref_mut(*r) {
                        i.properties.insert("SourceAssetId".into(), source_id.clone());
                    }
                }
                if kind == Kind::Decal {
                    // A decal: into the part dropped on (that face), else the
                    // selection's first object, else Workspace.
                    let (parent, face) = match target {
                        Target::Drop { part: Some(p), face, .. } if self.game.doc.exists(p) => (p, face),
                        Target::Parent(p) if self.game.doc.exists(p) => (p, None),
                        _ => (sel_first.unwrap_or(ws), None),
                    };
                    if let Some(f) = face {
                        for r in &roots {
                            if let Some(i) = dom.get_by_ref_mut(*r) {
                                if matches!(i.class.as_str(), "Decal" | "Texture") {
                                    i.properties.insert("Face".into(), Variant::Enum(rbx_dom_weak::types::Enum::from_u32(f)));
                                }
                            }
                        }
                    }
                    let bytes = insert::encode(&dom, &roots);
                    let refs = self.game.doc.paste_groups(vec![(bytes, parent)], &label);
                    self.select(refs.clone());
                    refs
                } else {
                    let lighting = self.game.doc.find_child(self.game.doc.dom.root_ref(), "Lighting");
                    let material = self.game.doc.find_child(self.game.doc.dom.root_ref(), "MaterialService");
                    let world_parent = tool_parent.or(explicit).unwrap_or(ws);
                    let mut world = Vec::new();
                    let mut groups: Vec<(Vec<u8>, Ref)> = Vec::new();
                    let mut light = Vec::new();
                    let mut mats = Vec::new();
                    for r in &roots {
                        let Some(i) = dom.get_by_ref(*r) else { continue };
                        match insert::home_of(i.class.as_str(), &asset.sub_types) {
                            Home::Lighting if explicit.is_none() && lighting.is_some() => light.push(*r),
                            Home::MaterialService if explicit.is_none() && material.is_some() => mats.push(*r),
                            _ => world.push(*r),
                        }
                    }
                    // Placement: only what goes into Workspace, and not when it
                    // was dropped on an Explorer row (Studio keeps the
                    // model's own position there).
                    let in_world = explicit.is_none() && tool_parent.is_none_or(|p| p == ws);
                    if in_world && !world.is_empty() {
                        if let Some((lo, hi)) = insert::aabb(&dom, &world) {
                            let to = match target {
                                Target::Drop { at: Some(p), .. } => {
                                    if self.game.snap_move { insert::snap(p, self.game.move_inc) } else { p }
                                }
                                Target::Drop { at: None, ray_origin, ray_dir, .. } => ray_origin + ray_dir * 64.0,
                                _ => self.toolbox_insert_point(),
                            };
                            let boxes = self.world_boxes();
                            let d = insert::move_to(lo, hi, to, &boxes);
                            insert::translate(&mut dom, &world, d);
                        }
                    }
                    if !world.is_empty() {
                        groups.push((insert::encode(&dom, &world), world_parent));
                    }
                    if !light.is_empty() {
                        groups.push((insert::encode(&dom, &light), lighting.unwrap_or(ws)));
                    }
                    if !mats.is_empty() {
                        groups.push((insert::encode(&dom, &mats), material.unwrap_or(ws)));
                    }
                    let refs = self.game.doc.paste_groups(groups, &label);
                    self.select(refs.clone());
                    refs
                }
            }
        };
        if inserted.is_empty() {
            self.error(format!("Toolbox failed to insert asset {id}: nothing to insert"));
            self.toolbox.harness.insert(format!("insert_{id}"), serde_json::json!({"ok": false, "error": "empty"}));
            return;
        }
        self.toolbox.note_recent(id, asset.type_id);
        // Inserted while the card is still dragged in the 3D view with a
        // plugin's mouse active: the plugin's PluginMouse.DragEnter gets the
        // instances (the button still held; the plugin drags them).
        // (with no plugin active, the editor's own dragger takes them: a
        // lone Decal / Texture by the face under the mouse, the rest freeform)
        let made: Vec<Ref> = inserted.iter().flat_map(|r| std::iter::once(*r).chain(self.game.doc.dom.descendants_of(*r).map(|i| i.referent()))).collect();
        let face = match inserted.as_slice() {
            [r] if self.game.doc.dom.get_by_ref(*r).is_some_and(|i| matches!(i.class.as_str(), "Decal" | "Texture")) => {
                let i = self.game.doc.dom.get_by_ref(*r).expect("checked");
                Some((*r, i.parent(), i.properties.get(&rbx_dom_weak::ustr("Face")).cloned()))
            }
            _ => None,
        };
        if let Some(d) = self.toolbox.drag_in.as_mut().filter(|d| d.job == Some(job)) {
            d.job = None;
            d.inserted = inserted.clone();
            d.label = label.clone();
            d.made = made;
            d.face = face;
            if d.builtin {
                self.toolbox.harness.insert(format!("drag_builtin_{id}"), serde_json::json!(inserted.len()));
            } else {
                self.plugin_mouse_drag_enter(inserted.clone());
                self.toolbox.harness.insert(format!("drag_enter_{id}"), serde_json::json!(inserted.len()));
            }
        }
        // Studio counts the insert (and Roblox's Recent list follows it).
        let m = super::api::insert_tracking_request(id);
        if !self.toolbox.dry_run {
            self.toolbox.pool.run(move |h| {
                let _ = h.post(&m.url, "application/json", b"");
                None::<Msg>
            });
        }
        let names: Vec<serde_json::Value> = inserted
            .iter()
            .filter_map(|r| self.game.doc.dom.get_by_ref(*r).map(|i| serde_json::json!({"class": i.class.as_str(), "name": i.name, "parent": self.game.doc.dom.get_by_ref(i.parent()).map(|p| p.class.to_string()).unwrap_or_default()})))
            .collect();
        self.toolbox.harness.insert(
            format!("insert_{id}"),
            serde_json::json!({"ok": true, "kind": format!("{kind:?}"), "inserted": names, "undo": self.game.doc.can_undo().map(str::to_string)}),
        );
        self.info(format!("Inserted \"{name}\" ({id})"));
    }

    fn toolbox_insert_built(&mut self, items: Vec<(InstanceBuilder, Ref)>, label: &str) -> Vec<Ref> {
        let mut groups = Vec::new();
        for (b, parent) in items {
            let mut tmp = rbx_dom_weak::WeakDom::new(InstanceBuilder::new("DataModel"));
            let root = tmp.root_ref();
            let r = tmp.insert(root, b);
            groups.push((insert::encode(&tmp, &[r]), parent));
        }
        let refs = self.game.doc.paste_groups(groups, label);
        self.select(refs.clone());
        refs
    }

    /// Install: owned → install now; free and not owned → Studio's "Get" dialog.
    pub fn toolbox_install_plugin(&mut self, asset: super::model::Asset) {
        if !self.toolbox.signed_in {
            self.warn("Sign in to install plugins.");
            return;
        }
        let owned = self.toolbox.owned.get(&asset.id).copied().unwrap_or(false);
        if !owned && !asset.price.is_free() && asset.price != super::model::Price::Unknown {
            self.toolbox.ask = Some(Ask::Paid { asset });
            return;
        }
        if !owned {
            self.toolbox.ask = Some(Ask::GetPlugin { asset });
            return;
        }
        let client = self.game.account.shared_client();
        self.plugin_store.install(client, asset.id);
        self.toolbox.ui.installing.insert(asset.id);
        self.toolbox.note_recent(asset.id, asset.type_id);
        self.info(format!("Installing plugin {} \"{}\"…", asset.id, asset.name));
    }

    /// Insert button / click on a card / deep link.
    /// The insert job started (None: a plugin install or a paid asset's dialog).
    pub fn toolbox_insert(&mut self, id: u64, target: Target) -> Option<u64> {
        let a = self.toolbox.asset(id).cloned();
        if let Some(a) = &a {
            let kind = Kind::from_asset_type(a.type_id);
            if kind == Some(Kind::Plugin) {
                self.toolbox.want_owned(id);
                self.toolbox_install_plugin(a.clone());
                return None;
            }
            if let super::model::Price::Cents(_) = a.price {
                if !self.toolbox.owned.get(&id).copied().unwrap_or(false) {
                    self.toolbox.ask = Some(Ask::Paid { asset: a.clone() });
                    return None;
                }
            }
        }
        Some(self.toolbox.start_insert(id, target))
    }

    /// `toolbox:search:<category>:<query>`, `toolbox:insert:<assetId>`,
    /// `toolbox:answer:insert|cancel|starterpack`,
    /// `toolbox:answer-when-asked:insert|cancel|starterpack`, `toolbox:tab:<n>`,
    /// `toolbox:await-ask:<seconds>` (the schedule held until a prompt shows).
    pub fn toolbox_action(&mut self, a: &str) {
        let rest = &a["toolbox:".len()..];
        if !rest.starts_with("layout:") {
            self.toolbox.ui.action_seq += 1;
            // A pick still waiting for its reply when the next action comes
            // fails, saying what it waited for.
            if let Some(n) = self.toolbox.ui.pick_waiting.take() {
                let (cat, q) = (self.toolbox.current_cat().unwrap_or_else(|| "models".into()), self.toolbox.st.query.clone());
                let why = format!("suggest-pick:{n}: no autocomplete reply for ({cat}, {q}) before {a} (signed in: {})", self.toolbox.signed_in);
                self.toolbox_pick_error(why);
                self.toolbox.harness.remove("waiting_pick");
            }
        }
        if let Some(s) = rest.strip_prefix("search:") {
            let (cat, q) = s.split_once(':').unwrap_or((s, ""));
            self.ui.panel_requests.push((crate::app::Tab::Toolbox, Some(true)));
            match self.toolbox.deep_search(cat, q) {
                Ok(r) => self.toolbox.harness.insert(format!("search_{cat}_{q}"), serde_json::json!({"pending": true, "req": format!("{r:?}")})),
                Err(e) => {
                    self.error(e);
                    None
                }
            };
            self.toolbox.ui.deep_searches.push((cat.to_string(), q.to_string()));
        } else if let Some(id) = rest.strip_prefix("insert:").and_then(|i| i.trim().parse::<u64>().ok()) {
            self.toolbox.want_details(vec![id]);
            self.toolbox.ui.deep_inserts.push(id);
        } else if let Some(ans) = rest.strip_prefix("answer-when-asked:") {
            // Answered in toolbox_harness_tick the frame the prompt shows.
            self.toolbox.ui.answer_when_asked = Some(ans.to_string());
        } else if let Some(secs) = rest.strip_prefix("await-ask:").and_then(|s| s.parse::<f32>().ok()) {
            // The schedule holds (toolbox_harness_tick) until a prompt shows,
            // at most `secs` of wall time.
            if self.toolbox.ask.is_none() {
                let at = self.harness.run.unheld_now();
                let deadline = std::time::Instant::now() + std::time::Duration::from_secs_f32(secs.max(0.0));
                self.toolbox.ui.await_ask = Some((at, self.harness.run.held, deadline, secs));
            } else {
                self.toolbox_awaited(0.0);
            }
        } else if let Some(ans) = rest.strip_prefix("answer:") {
            self.toolbox_answer(ans != "cancel", ans == "starterpack");
        } else if let Some(t) = rest.strip_prefix("tab:").and_then(|n| n.parse::<usize>().ok()) {
            if let Some(t) = super::categories::Tab::ALL.get(t) {
                self.toolbox.st.tab = *t;
            }
        } else if rest == "cancel" {
            let jobs: Vec<u64> = self.toolbox.inserts.iter().map(|j| j.job).collect();
            for j in jobs {
                self.toolbox.cancel_insert(j);
            }
        } else if let Some(n) = rest.strip_prefix("listcat:").and_then(|n| n.parse::<usize>().ok()) {
            match self.toolbox.st.tab {
                super::categories::Tab::Inventory => self.toolbox.st.inv_cat = n,
                super::categories::Tab::Recent => self.toolbox.st.recent_cat = n,
                super::categories::Tab::Creations => self.toolbox.st.creations_cat = n,
                _ => {}
            }
        } else if rest == "home" {
            self.toolbox.home();
        } else if let Some(id) = rest.strip_prefix("open:").and_then(|i| i.parse::<u64>().ok()) {
            self.toolbox.st.tab = super::categories::Tab::Store;
            self.toolbox.want_details(vec![id]);
            self.toolbox.want_favorites(id);
            self.toolbox.want_owned(id);
            self.toolbox.push(super::Nav::Asset(id));
        } else if let Some(k) = rest.strip_prefix("category:") {
            self.toolbox.st.tab = super::categories::Tab::Store;
            self.toolbox.home();
            self.toolbox.push(super::Nav::Category(k.to_string()));
        } else if let Some(t) = rest.strip_prefix("tile:") {
            self.toolbox.st.tab = super::categories::Tab::Store;
            self.toolbox.home();
            self.toolbox.push(super::Nav::Category("audio".into()));
            self.toolbox.push(super::Nav::Results { cat: "audio".into(), query: String::new(), tile: Some(t.to_string()) });
        } else if let Some(u) = rest.strip_prefix("ui:") {
            let ui = &mut self.toolbox.ui;
            match u {
                "filters" => ui.filters_open = !ui.filters_open,
                "hamburger" => ui.hamburger = !ui.hamburger,
                "more" => ui.more_menu = !ui.more_menu,
                "tree" => ui.show_tree = !ui.show_tree,
                "close" => {
                    ui.filters_open = false;
                    ui.hamburger = false;
                    ui.more_menu = false;
                    ui.dropdown = None;
                    ui.force_suggest = false;
                    ui.force_hover = None;
                }
                d if d.starts_with("dropdown:") => {
                    ui.dropdown = match &d[9..] {
                        "sort" => Some("sort"),
                        "price" => Some("price"),
                        "contains" => Some("contains"),
                        "atype" => Some("atype"),
                        "length" => Some("length"),
                        _ => None,
                    }
                }
                h if h.starts_with("hover:") => ui.force_hover = h[6..].parse().ok(),
                w if w.starts_with("width:") => ui.want_width = w[6..].parse().ok(),
                _ => {}
            }
        } else if let Some(q) = rest.strip_prefix("type:") {
            // Text typed into the search box (suggestions show as if focused).
            self.toolbox.st.query = q.to_string();
            let cat = self.toolbox.current_cat();
            self.toolbox.want_suggestions(cat.as_deref(), q);
            self.toolbox.ui.force_suggest = true;
        } else if let Some(n) = rest.strip_prefix("suggest-pick:").and_then(|n| n.parse::<usize>().ok()) {
            // The n-th suggestion row chosen (as a click on it), once the
            // reply to the typed text came (toolbox_harness_tick).
            self.toolbox.ui.pick_waiting = Some(n);
            self.toolbox_pick_when_replied();
        } else if let Some(tag) = rest.strip_prefix("layout:") {
            // What the panel drew (wide / narrow layout checks), reported
            // once a draw has seen every earlier action (toolbox_harness_tick).
            self.toolbox.ui.layout_waiting.push(tag.to_string());
            self.toolbox_layout_reports();
        } else if rest == "undo" {
            let label = self.game.doc.can_undo().map(str::to_string);
            self.undo();
            self.toolbox.harness.insert("last_undo".into(), serde_json::json!(label));
        } else if rest == "redo" {
            self.redo();
        } else if let Some(tag) = rest.strip_prefix("snapshot:") {
            // The services an insert can touch, and the selection (checks).
            let mut tree = serde_json::Map::new();
            for svc in ["Workspace", "Lighting", "StarterPack", "MaterialService", "SoundService", "StarterGui"] {
                let Some(s) = self.game.doc.find_child(self.game.doc.dom.root_ref(), svc) else { continue };
                let kids: Vec<serde_json::Value> = self.game
                    .doc
                    .dom
                    .get_by_ref(s)
                    .map(|i| i.children().to_vec())
                    .unwrap_or_default()
                    .iter()
                    .filter_map(|r| self.game.doc.dom.get_by_ref(*r))
                    .map(|i| {
                        let size = match i.properties.get(&"Size".into()) {
                            Some(Variant::Vector3(v)) => serde_json::json!([v.x, v.y, v.z]),
                            _ => serde_json::Value::Null,
                        };
                        let pos = match i.properties.get(&"CFrame".into()) {
                            Some(Variant::CFrame(c)) => serde_json::json!([c.position.x, c.position.y, c.position.z]),
                            _ => serde_json::Value::Null,
                        };
                        let bbox = super::insert::aabb(&self.game.doc.dom, &[i.referent()]).map(|(lo, hi)| serde_json::json!([[lo.x, lo.y, lo.z], [hi.x, hi.y, hi.z]]));
                        let mut props = serde_json::Map::new();
                        for k in ["SoundId", "Video", "Texture", "MeshId", "Face", "CollisionFidelity", "SourceAssetId", "TextureID", "SkyboxBk", "VideoContent", "AudioContent"] {
                            if let Some(v) = self.game.doc.prop(i.referent(), k) {
                                props.insert(k.into(), serde_json::json!(variant_text(&v)));
                            }
                        }
                        props.insert("_keys".into(), serde_json::json!(i.properties.keys().map(|k| k.to_string()).collect::<Vec<_>>()));
                        let kids = i.children().iter().filter_map(|c| self.game.doc.dom.get_by_ref(*c)).map(|c| format!("{}:{}", c.class, c.name)).take(50).collect::<Vec<_>>();
                        serde_json::json!({"class": i.class.as_str(), "name": i.name, "size": size, "pos": pos, "bbox": bbox, "props": props, "children": kids, "descendants": self.game.doc.dom.descendants_of(i.referent()).count()})
                    })
                    .collect();
                tree.insert(svc.into(), serde_json::Value::Array(kids));
            }
            let sel: Vec<String> = self.game.doc.selection.iter().filter_map(|r| self.game.doc.dom.get_by_ref(*r)).map(|i| format!("{}:{}", i.class, i.name)).collect();
            tree.insert("selection".into(), serde_json::json!(sel));
            // Decals / Textures anywhere in Workspace: parent and Face (drops on a face).
            let faces: Vec<serde_json::Value> = self.game
                .doc
                .dom
                .descendants_of(self.game.doc.default_parent())
                .filter(|i| matches!(i.class.as_str(), "Decal" | "Texture"))
                .map(|i| {
                    let face = self.game.doc.prop(i.referent(), "Face").map(|v| variant_text(&v));
                    let parent = self.game.doc.dom.get_by_ref(i.parent()).map(|p| p.name.clone()).unwrap_or_default();
                    serde_json::json!({"class": i.class.as_str(), "parent": parent, "face": face})
                })
                .collect();
            tree.insert("decals".into(), serde_json::json!(faces));
            tree.insert("undo".into(), serde_json::json!(self.game.doc.can_undo()));
            tree.insert("asking".into(), serde_json::json!(self.toolbox_asking()));
            self.toolbox.harness.insert(format!("snapshot_{tag}"), serde_json::Value::Object(tree));
        }
    }

    /// An `await-ask` that saw its prompt: which one, and how long the
    /// schedule held for it ("awaited", a list: the checks assert on it).
    fn toolbox_awaited(&mut self, held: f32) {
        let asking = self.toolbox_asking();
        self.push_log(crate::app::Level::Info, format!("await-ask: {asking} after {held:.2} s held"));
        let all = self.toolbox.harness.entry("awaited").or_insert_with(|| serde_json::json!([]));
        if let Some(a) = all.as_array_mut() {
            a.push(serde_json::json!({"asking": asking, "held": held}));
        }
    }

    /// The prompt showing, as the harness names it ("scripts:<n>", "tool",
    /// "get-plugin", "paid", "" for none).
    fn toolbox_asking(&self) -> String {
        match &self.toolbox.ask {
            Some(Ask::Scripts { count, .. }) => format!("scripts:{count}"),
            Some(Ask::ToolHome { .. }) => "tool".into(),
            Some(Ask::GetPlugin { .. }) => "get-plugin".into(),
            Some(Ask::Paid { .. }) => "paid".into(),
            None => String::new(),
        }
    }

    /// `toolbox:layout:<tag>`s whose draw came: the panel's last draw saw
    /// every action before them. Those still waiting are listed in
    /// "waiting_layout" (a panel never drawn, as in --no-ui, stays there).
    fn toolbox_layout_reports(&mut self) {
        let ui = &mut self.toolbox.ui;
        if ui.layout_waiting.is_empty() {
            return;
        }
        if ui.drawn_seq == ui.action_seq {
            let l = ui.layout;
            let nav = format!("{:?}", self.toolbox.nav());
            for tag in std::mem::take(&mut self.toolbox.ui.layout_waiting) {
                self.toolbox.harness.insert(format!("layout_{tag}"), serde_json::json!({"width": l.width, "tile_cols": l.tile_cols, "lane_cards": l.lane_cards, "grid_cols": l.grid_cols, "grid_cell": l.grid_cell, "nav": nav}));
            }
            self.toolbox.harness.remove("waiting_layout");
        } else {
            let w = serde_json::json!({"tags": ui.layout_waiting, "action_seq": ui.action_seq, "drawn_seq": ui.drawn_seq});
            self.toolbox.harness.insert("waiting_layout".into(), w);
        }
    }

    /// A failed pick: "picked" says why, and "pick_errors" keeps every one.
    fn toolbox_pick_error(&mut self, why: String) {
        self.toolbox.harness.insert("picked".into(), serde_json::json!({"error": why}));
        let all = self.toolbox.harness.entry("pick_errors").or_insert_with(|| serde_json::json!([]));
        if let Some(a) = all.as_array_mut() {
            a.push(serde_json::json!(why));
        }
    }

    /// A `toolbox:suggest-pick:<n>` whose reply came: the n-th row chosen
    /// (as a click on it); the search it starts is recorded as
    /// search_<cat>_<query>.
    fn toolbox_pick_when_replied(&mut self) {
        let Some(n) = self.toolbox.ui.pick_waiting else { return };
        if !self.toolbox.suggest_replied {
            self.toolbox.harness.insert("waiting_pick".into(), serde_json::json!({"row": n, "query": self.toolbox.st.query, "signed_in": self.toolbox.signed_in}));
            return;
        }
        self.toolbox.ui.pick_waiting = None;
        self.toolbox.harness.remove("waiting_pick");
        let row = self.toolbox.suggestions_for(&self.toolbox.st.query).get(n).cloned();
        self.toolbox.ui.force_suggest = false;
        match row {
            Some(s) => {
                self.toolbox.harness.insert("picked".into(), serde_json::json!({"query": s.query, "cat": s.cat, "label": s.label}));
                self.toolbox.choose_suggestion(&s);
                self.toolbox.ui.deep_searches.push((s.cat.clone(), s.query.clone()));
            }
            None => {
                let rows = self.toolbox.suggestions_for(&self.toolbox.st.query).len();
                self.toolbox_pick_error(format!("no suggestion row {n} ({rows} rows, signed in: {})", self.toolbox.signed_in));
            }
        }
    }

    /// Deep links waiting on details, and search results for `--result`.
    pub fn toolbox_harness_tick(&mut self) {
        self.toolbox_layout_reports();
        // Typed text's suggestion rows once they came (checks).
        if self.toolbox.ui.force_suggest {
            let q = self.toolbox.st.query.clone();
            let cat = self.toolbox.current_cat().unwrap_or_else(|| "models".into());
            let rows: Vec<serde_json::Value> = self.toolbox.suggestions_for(&q).iter().map(|s| serde_json::json!({"query": s.query, "cat": s.cat, "label": s.label})).collect();
            if !rows.is_empty() {
                self.toolbox.harness.insert(format!("suggest_{cat}_{q}"), serde_json::Value::Array(rows));
            }
        }
        // (after the rows above are recorded: a pick hides them)
        self.toolbox_pick_when_replied();
        // `toolbox:await-ask`: held until a prompt shows or the wall-clock
        // limit passes (then "await_timeouts" says so; the check fails).
        if let Some((at, before, deadline, secs)) = self.toolbox.ui.await_ask {
            let waited = self.harness.run.unheld_now() - at;
            if self.toolbox.ask.is_some() {
                self.toolbox.ui.await_ask = None;
                self.harness.run.held = before + waited;
                self.toolbox_awaited(waited);
            } else if std::time::Instant::now() >= deadline {
                self.toolbox.ui.await_ask = None;
                self.harness.run.held = before + waited;
                let why = format!("await-ask: no prompt within {secs} s (wall clock)");
                self.push_log(crate::app::Level::Warn, why.clone());
                let all = self.toolbox.harness.entry("await_timeouts").or_insert_with(|| serde_json::json!([]));
                if let Some(a) = all.as_array_mut() {
                    a.push(serde_json::json!(why));
                }
            } else {
                self.harness.run.held = before + waited;
            }
        }
        // An armed answer goes to the first prompt that shows, recording
        // which prompt it was ("answered": the check asserts on it).
        if self.toolbox.ask.is_some() {
            if let Some(ans) = self.toolbox.ui.answer_when_asked.take() {
                let asking = self.toolbox_asking();
                self.toolbox.harness.insert("answered".into(), serde_json::json!({"asking": asking, "answer": ans}));
                self.toolbox_answer(ans != "cancel", ans == "starterpack");
            }
        }
        let pending: Vec<u64> = std::mem::take(&mut self.toolbox.ui.deep_inserts);
        for id in pending {
            if !self.toolbox.signed_in && self.toolbox.ui.deep_waits.get(&id).copied().unwrap_or(0) < 1200 {
                *self.toolbox.ui.deep_waits.entry(id).or_insert(0) += 1;
                self.toolbox.ui.deep_inserts.push(id);
                continue;
            }
            if self.toolbox.signed_in && self.toolbox.ui.deep_asked.insert(id) {
                self.toolbox.assets.remove(&id);
                self.toolbox.want_details(vec![id]);
            }
            let known = self.toolbox.asset(id).is_some_and(|a| a.detailed || a.type_id != 0) || self.toolbox.missing.contains(&id);
            let waited = self.toolbox.ui.deep_waits.entry(id).or_insert(0);
            *waited += 1;
            if known || *waited > 1800 {
                self.toolbox_insert(id, Target::Click);
            } else {
                self.toolbox.ui.deep_inserts.push(id);
            }
        }
        let searches = self.toolbox.ui.deep_searches.clone();
        if !searches.is_empty() {
            self.toolbox.ui.deep_frames += 1;
        }
        for (cat, q) in searches {
            let r = self.toolbox.store_req(&cat, &q, None);
            // (the session check finishes a moment after start)
            if !self.toolbox.signed_in && self.toolbox.ui.deep_frames < 1200 {
                continue;
            }
            if self.toolbox.signed_in {
                self.toolbox.ensure(&r);
            }
            if let Some(l) = self.toolbox.list(&r) {
                if l.loading {
                    continue;
                }
                let ids: Vec<serde_json::Value> = l.ids.iter().take(30).map(|i| {
                    let a = self.toolbox.asset(*i);
                    serde_json::json!({"id": i, "name": a.map(|a| a.name.clone()).unwrap_or_default(), "type": a.map(|a| a.type_id).unwrap_or(0)})
                }).collect();
                self.toolbox.harness.insert(
                    format!("search_{cat}_{q}"),
                    serde_json::json!({"ok": l.error.is_none(), "error": l.error.as_ref().map(|e| e.describe()), "total": l.total, "results": ids}),
                );
                self.toolbox.ui.deep_searches.retain(|s| s != &(cat.clone(), q.clone()));
            }
        }
    }

    /// The 3D view's drop target for Toolbox cards (call after the view
    /// drew, with its rect and response).
    pub fn toolbox_viewport_drop(&mut self, ui: &egui::Ui, rect: egui::Rect, resp: &egui::Response) {
        if self.toolbox_drag_in(ui, rect) {
            return;
        }
        let Some(payload) = egui::DragAndDrop::payload::<ToolboxDrag>(ui.ctx()) else { return };
        let Some(p) = ui.ctx().pointer_hover_pos().filter(|p| rect.contains(*p)) else { return };
        let ray = self.game.cam.ray(rect, p);
        let hit = self.ray_distance(&ray).filter(|t| *t <= 2048.0).map(|t| ray.origin + ray.dir * t);
        let part = self.pick(rect, p, true);
        // Preview: a marker at the drop point (and the face for decals).
        let painter = ui.painter_at(rect);
        painter.circle_stroke(p, 10.0, egui::Stroke::new(2.0, rustudio_engine::theme::SELECT));
        if let Some(a) = self.toolbox.asset(payload.0) {
            painter.text(p + egui::vec2(14.0, -8.0), egui::Align2::LEFT_BOTTOM, &a.name, egui::FontId::proportional(12.0), rustudio_engine::theme::TEXT);
        }
        let released = ui.ctx().input(|i| i.pointer.any_released());
        if released || resp.dnd_release_payload::<ToolboxDrag>().is_some() {
            egui::DragAndDrop::clear_payload(ui.ctx());
            let face = part.and_then(|r| {
                let xf = rustudio_render::scene::xform_of(&self.game.doc, r)?;
                let size = rustudio_render::scene::vec3_of(self.game.doc.prop(r, "Size"))?;
                Some(insert::face_of(&xf, size, hit?))
            });
            self.toolbox_insert(payload.0, Target::Drop { at: hit, ray_origin: ray.origin, ray_dir: ray.dir, part, face });
        }
    }
}

impl Editor {
    /// Whether a card for asset `id` is inserted as the drag enters the view
    /// (`insert::DragIn`): what lands in the DataModel as instances to drag.
    /// Plugins install, fonts and paid assets ask, sounds and videos go to
    /// the selection: those keep the drop on release.
    fn toolbox_drags_in(&self, id: u64) -> bool {
        let Some(a) = self.toolbox.asset(id) else { return true };
        let kind = Kind::from_asset_type(a.type_id);
        if matches!(kind, Some(Kind::Plugin | Kind::Audio | Kind::Video | Kind::Font)) {
            return false;
        }
        !matches!(a.price, super::model::Price::Cents(_)) || self.toolbox.owned.get(&id).copied().unwrap_or(false)
    }

    /// A card dragged over the 3D view (`insert::DragIn`): inserted as it
    /// enters; the active plugin's PluginMouse.DragEnter gets it, or with no
    /// plugin active the editor's own dragger moves it under the pointer;
    /// taken back when the drag leaves the view before the release, kept on
    /// the release (the built-in dragger's move becomes its own step). True
    /// while this handles the drag (the plain drop below must not insert
    /// again).
    fn toolbox_drag_in(&mut self, ui: &egui::Ui, rect: egui::Rect) -> bool {
        let ctx = ui.ctx();
        let payload = egui::DragAndDrop::payload::<ToolboxDrag>(ctx).map(|p| p.0);
        let pointer = ctx.pointer_hover_pos();
        let inside = pointer.is_some_and(|p| rect.contains(p));
        let released = ctx.input(|i| i.pointer.any_released()) || !ctx.input(|i| i.pointer.primary_down());
        let Some(d) = self.toolbox.drag_in.clone() else {
            // Starts when a card enters the view.
            let (Some(id), true, false) = (payload, inside, released) else { return false };
            if !self.toolbox_drags_in(id) {
                return false;
            }
            let builtin = self.plugins.active_plugin().is_none();
            let p = pointer.unwrap_or(rect.center());
            let ray = self.game.cam.ray(rect, p);
            let hit = self.ray_distance(&ray).filter(|t| *t <= 2048.0).map(|t| ray.origin + ray.dir * t);
            let part = self.pick(rect, p, true);
            let face = part.and_then(|r| {
                let xf = rustudio_render::scene::xform_of(&self.game.doc, r)?;
                let size = rustudio_render::scene::vec3_of(self.game.doc.prop(r, "Size"))?;
                Some(insert::face_of(&xf, size, hit?))
            });
            let Some(job) = self.toolbox_insert(id, Target::Drop { at: hit, ray_origin: ray.origin, ray_dir: ray.dir, part, face }) else { return false };
            self.toolbox.drag_in = Some(insert::DragIn {
                id,
                job: Some(job),
                inserted: Vec::new(),
                label: String::new(),
                made: Vec::new(),
                builtin,
                moved: Vec::new(),
                face: None,
                at: None,
            });
            self.toolbox.harness.insert("drag_in".into(), serde_json::json!({"id": id, "builtin": builtin}));
            ctx.request_repaint();
            return true;
        };
        ctx.request_repaint();
        if payload != Some(d.id) || released {
            // Let go (or the drag was dropped): in the view the insert stays
            // (a job still loading lands as a plain drop); outside, it goes.
            // (outside the view the panel under the pointer may take it: an
            // Explorer row inserts it there)
            if released && inside && payload == Some(d.id) {
                egui::DragAndDrop::clear_payload(ctx);
            }
            if inside {
                if let Some(p) = pointer {
                    self.toolbox_dragger_move(rect, p);
                }
                self.toolbox_dragger_end();
            } else {
                self.toolbox_drag_out(&d);
            }
            self.toolbox.drag_in = None;
            return true;
        }
        if !inside {
            self.toolbox_drag_out(&d);
            self.toolbox.drag_in = None;
            return true;
        }
        if let Some(p) = pointer {
            self.toolbox_dragger_move(rect, p);
        }
        true
    }

    /// The editor's own dragger (no plugin active): the inserted instances
    /// follow the pointer as DraggerFramework's `DraggingParts` /
    /// `DraggingFaceInstance` move them (see `insert::DragIn`). Live: the
    /// step is made at the release.
    fn toolbox_dragger_move(&mut self, rect: egui::Rect, p: egui::Pos2) {
        let Some(d) = self.toolbox.drag_in.as_ref() else { return };
        if !d.builtin || d.inserted.is_empty() || d.at == Some([p.x, p.y]) {
            return;
        }
        let made: std::collections::HashSet<Ref> = d.made.iter().copied().collect();
        let face = d.face.clone();
        let inserted = d.inserted.clone();
        let ray = self.game.cam.ray(rect, p);
        let mut moved: Vec<(Ref, rbx_dom_weak::Ustr, Option<Variant>)> = Vec::new();
        if let Some((decal, _, _)) = face {
            // Over a part: parented to it, Face = the surface under the mouse.
            let hit = self.game
                .scene
                .parts
                .iter()
                .filter(|sp| sp.draw.alpha > 0.02 && !made.contains(&sp.r))
                .filter_map(|sp| ray.hit_obb(&sp.xf, sp.size).filter(|t| *t > 0.0).map(|t| (t, sp.r, sp.xf, sp.size)))
                .min_by(|a, b| a.0.total_cmp(&b.0));
            if let Some((t, part, xf, size)) = hit {
                if self.game.doc.exists(decal) {
                    let f = insert::face_of(&xf, size, ray.origin + ray.dir * t);
                    self.game.doc.reparent_live(decal, part);
                    let name = rbx_dom_weak::ustr("Face");
                    let v = Variant::Enum(rbx_dom_weak::types::Enum::from_u32(f));
                    if self.game.doc.prop(decal, "Face").as_ref() != Some(&v) {
                        self.game.doc.set_prop_live(decal, name, v);
                    }
                }
            }
        } else {
            // Freeform: the box's centre to the surface under the pointer
            // (not the dragged parts themselves), raised clear of the parts
            // there, as the drop places it.
            let ws = self.game.doc.workspace();
            let world: Vec<Ref> = inserted.iter().copied().filter(|r| ws.is_some_and(|w| self.game.doc.is_ancestor(w, *r))).collect();
            if world.is_empty() {
                return;
            }
            let t = self.game
                .scene
                .parts
                .iter()
                .filter(|sp| sp.draw.alpha > 0.02 && !made.contains(&sp.r))
                .filter_map(|sp| ray.hit_obb(&sp.xf, sp.size).filter(|t| *t > 0.0))
                .min_by(f32::total_cmp)
                .filter(|t| *t <= 2048.0);
            let to = match t {
                Some(t) => {
                    let hit = ray.origin + ray.dir * t;
                    if self.game.snap_move { insert::snap(hit, self.game.move_inc) } else { hit }
                }
                None => ray.origin + ray.dir * 64.0,
            };
            let Some((lo, hi)) = insert::aabb(&self.game.doc.dom, &world) else { return };
            let boxes: Vec<(Vec3, Vec3)> = match ws {
                Some(w) => insert_boxes_except(&self.game.doc, w, &made),
                None => Vec::new(),
            };
            let delta = insert::move_to(lo, hi, to, &boxes);
            if delta.length_squared() > 1e-8 && delta.is_finite() {
                let mut edits: Vec<(Ref, rbx_dom_weak::Ustr, Variant)> = Vec::new();
                for r in world.iter().flat_map(|r| std::iter::once(*r).chain(self.game.doc.dom.descendants_of(*r).map(|i| i.referent()))) {
                    let Some(i) = self.game.doc.dom.get_by_ref(r) else { continue };
                    let part = rustudio_core::doc::is_a(i.class.as_str(), "BasePart");
                    for key in ["CFrame", "WorldPivotData", "WorldPivot"] {
                        let name = rbx_dom_weak::ustr(key);
                        let new = match i.properties.get(&name) {
                            Some(Variant::CFrame(cf)) if key != "CFrame" || part => {
                                let mut cf = *cf;
                                cf.position = rbx_dom_weak::types::Vector3::new(cf.position.x + delta.x, cf.position.y + delta.y, cf.position.z + delta.z);
                                Variant::CFrame(cf)
                            }
                            Some(Variant::OptionalCFrame(Some(cf))) => {
                                let mut cf = *cf;
                                cf.position = rbx_dom_weak::types::Vector3::new(cf.position.x + delta.x, cf.position.y + delta.y, cf.position.z + delta.z);
                                Variant::OptionalCFrame(Some(cf))
                            }
                            _ => continue,
                        };
                        moved.push((r, name, i.properties.get(&name).cloned()));
                        edits.push((r, name, new));
                    }
                }
                for (r, name, v) in edits {
                    self.game.doc.set_prop_live(r, name, v);
                }
            }
        }
        if let Some(d) = self.toolbox.drag_in.as_mut() {
            d.at = Some([p.x, p.y]);
            for m in moved {
                if !d.moved.iter().any(|(r, n, _)| *r == m.0 && *n == m.1) {
                    d.moved.push(m);
                }
            }
        }
    }

    /// The release in the view: the built-in dragger's move becomes one step
    /// ("End Freeform Drag" / "Drag Face Instance", DraggerFramework's
    /// waypoint names), nothing when it didn't move.
    fn toolbox_dragger_end(&mut self) {
        let Some(d) = self.toolbox.drag_in.clone() else { return };
        if !d.builtin || d.inserted.is_empty() {
            return;
        }
        let mut changes = Vec::new();
        let label = if let Some((decal, parent0, face0)) = &d.face {
            if let Some(i) = self.game.doc.dom.get_by_ref(*decal) {
                if i.parent() != *parent0 {
                    changes.push(rustudio_core::doc::Change::Parent { r: *decal, old: *parent0, new: i.parent() });
                }
                let face = self.game.doc.dom.get_by_ref(*decal).and_then(|i| i.properties.get(&rbx_dom_weak::ustr("Face")).cloned());
                if face != *face0 {
                    changes.push(rustudio_core::doc::Change::Prop { r: *decal, name: rbx_dom_weak::ustr("Face"), old: face0.clone(), new: face });
                }
            }
            "Drag Face Instance"
        } else {
            for (r, name, old) in &d.moved {
                let new = self.game.doc.dom.get_by_ref(*r).and_then(|i| i.properties.get(name).cloned());
                changes.push(rustudio_core::doc::Change::Prop { r: *r, name: *name, old: old.clone(), new });
            }
            "End Freeform Drag"
        };
        self.game.doc.record_changes(label, changes);
        self.toolbox.harness.insert(format!("drag_end_{}", d.id), serde_json::json!(self.game.doc.can_undo()));
    }

    /// The drag left the view: the loading insert is cancelled (its scripts
    /// warning or Tool dialog closed), what it inserted is taken back and
    /// only that (`Document::retract_inserted`: other edits and steps made
    /// meanwhile stay).
    fn toolbox_drag_out(&mut self, d: &insert::DragIn) {
        if let Some(job) = d.job {
            self.toolbox.cancel_insert(job);
            self.toolbox.inserts.retain(|j| j.job != job);
            // (its scripts warning or Tool dialog goes too)
            if matches!(&self.toolbox.ask, Some(Ask::Scripts { ready, .. } | Ask::ToolHome { ready }) if ready.job == job) {
                self.toolbox.ask = None;
            }
            self.toolbox.harness.insert(format!("drag_cancel_{}", d.id), serde_json::json!(job));
        }
        let made: std::collections::HashSet<Ref> = d.made.iter().copied().collect();
        if !d.inserted.is_empty() && self.game.doc.retract_inserted(&d.inserted, &made) {
            self.toolbox.harness.insert(format!("drag_out_{}", d.id), serde_json::json!(d.inserted.len()));
        }
    }

    /// Whether a card dragged in by `toolbox_drag_in` has been inserted and
    /// the drag goes on with a plugin's mouse (it follows the pointer).
    pub fn toolbox_drag_in_live(&self) -> bool {
        self.toolbox.drag_in.as_ref().is_some_and(|d| !d.inserted.is_empty() && !d.builtin)
    }
}

/// `insert_boxes` without the instances being dragged.
fn insert_boxes_except(doc: &rustudio_core::doc::Document, root: Ref, skip: &std::collections::HashSet<Ref>) -> Vec<(Vec3, Vec3)> {
    let mut out = Vec::new();
    for i in doc.dom.descendants_of(root) {
        if skip.contains(&i.referent()) || !rustudio_core::doc::is_a(i.class.as_str(), "BasePart") || i.class.as_str() == "Terrain" {
            continue;
        }
        let r = i.referent();
        let (Some(xf), Some(size)) = (rustudio_render::scene::xform_of(doc, r), rustudio_render::scene::vec3_of(doc.prop(r, "Size"))) else { continue };
        if matches!(doc.prop(r, "CanCollide"), Some(Variant::Bool(false))) {
            continue;
        }
        let half = size * 0.5;
        let ext = xf.rot.x_axis.abs() * half.x + xf.rot.y_axis.abs() * half.y + xf.rot.z_axis.abs() * half.z;
        out.push((xf.pos - ext, xf.pos + ext));
    }
    out
}

fn variant_text(v: &Variant) -> String {
    match v {
        Variant::ContentId(c) => c.as_str().to_string(),
        Variant::Content(c) => c.as_uri().map(str::to_string).unwrap_or_default(),
        Variant::String(s) => s.clone(),
        Variant::Enum(e) => e.to_u32().to_string(),
        Variant::Int64(i) => i.to_string(),
        Variant::Int32(i) => i.to_string(),
        other => format!("{other:?}").chars().take(80).collect(),
    }
}

/// AABBs of the parts under `root` (what an insert must not overlap).
fn insert_boxes(doc: &rustudio_core::doc::Document, root: Ref) -> Vec<(Vec3, Vec3)> {
    insert_boxes_except(doc, root, &std::collections::HashSet::new())
}
