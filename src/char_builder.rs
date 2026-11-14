//! The character builder (Home / Avatar → Character): inserts rigs built
//! from Roblox outfits and avatars. Studio's Rig Builder plugin is a list of
//! outfit ids (RigPresets: GetHumanoidDescriptionFromOutfitId →
//! CreateHumanoidModelFromDescription, grey 0.5 body, prebaked animations
//! cleared, model "Rig" dropped where the screen centre hits). rustudio's
//! keeps those presets and adds thumbnails, the person's own saved outfits,
//! friends and player search, options (anchored, clothing, real colours,
//! Animate script) and inserting as StarterPlayer.StarterCharacter.

use std::collections::{HashMap, HashSet};
use std::sync::mpsc::{channel, Receiver, Sender};

use egui::{Color32, Rect, Sense, Vec2};
use rbx_dom_weak::types::{Ref, Variant};
use rbx_dom_weak::{ustr, InstanceBuilder};
use rustudio_account::characters::{self, Outfit, Person, Thumb};

use crate::app::Editor;
use crate::avatar::{self, AvatarLook};
use crate::doc;
use crate::rig::{self, RigType};
use crate::theme;

/// Studio's presets (RigGenerator/Src/RigPresets): label, masculine outfit,
/// feminine outfit.
pub const PRESETS: &[(&str, &str, u64, u64)] = &[
    ("mannequin", "Mannequin", 4990637247331259, 4990637247331259),
    ("skinned", "Skinned", 11999916658, 12006493372),
    ("rthro", "Rthro", 1545219396, 1545101850),
    ("block", "Block (2012)", 131929545, 131929545),
    ("mesh2016", "Mesh (2016)", 131830044, 131830043),
    ("mesh2012", "Mesh (2012)", 131929496, 131929497),
];

/// What a card builds.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum Pick {
    /// One of [`PRESETS`] (its outfit for the chosen body shape).
    Preset { key: &'static str, outfit: u64 },
    /// The signed-in person's avatar.
    Me,
    Outfit { id: u64, name: String },
    User { id: u64, name: String },
}

impl Pick {
    fn label(&self) -> String {
        match self {
            Pick::Preset { key, .. } => PRESETS.iter().find(|p| p.0 == *key).map(|p| p.1).unwrap_or("Preset").into(),
            Pick::Me => "My avatar".into(),
            Pick::Outfit { name, .. } | Pick::User { name, .. } => name.clone(),
        }
    }

    fn thumb(&self, me: Option<u64>) -> Option<(Thumb, u64)> {
        match self {
            Pick::Preset { outfit, .. } => Some((Thumb::Outfit, *outfit)),
            Pick::Me => me.map(|id| (Thumb::User, id)),
            Pick::Outfit { id, .. } => Some((Thumb::Outfit, *id)),
            Pick::User { id, .. } => Some((Thumb::User, *id)),
        }
    }

    /// The Model's name when none is typed: Studio's "Rig" for presets.
    fn model_name(&self) -> String {
        match self {
            Pick::Preset { .. } => "Rig".into(),
            Pick::Me => "Avatar".into(),
            Pick::Outfit { name, .. } | Pick::User { name, .. } => {
                let n: String = name.chars().filter(|c| c.is_alphanumeric() || *c == '_' || *c == ' ').collect();
                if n.trim().is_empty() { "Rig".into() } else { n.trim().to_string() }
            }
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tab {
    Presets,
    Outfits,
    Friends,
}

enum Msg {
    Thumb(Thumb, u64, Option<egui::ColorImage>),
    Outfits(Result<Vec<Outfit>, String>),
    Friends(Result<Vec<Person>, String>),
    Search(String, Result<Vec<Person>, String>),
    Built(Pick, Result<AvatarLook, String>),
    Log(String),
}

/// The options a pick was made with (applied when its download lands).
#[derive(Debug, Clone)]
pub struct Opts {
    rig: RigType,
    anchored: bool,
    clothing: bool,
    real_colors: bool,
    animate: bool,
    target: Target,
    name: String,
}

/// Where the rig goes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Target {
    Workspace,
    StarterCharacter,
}

pub struct CharBuilder {
    pub open: bool,
    pub rig: RigType,
    pub feminine: bool,
    pub tab: Tab,
    pub query: String,
    searched: String,
    search_due: Option<std::time::Instant>,
    results: Option<Vec<Person>>,
    outfits: Option<Vec<Outfit>>,
    friends: Option<Vec<Person>>,
    thumbs: HashMap<(Thumb, u64), Option<egui::TextureHandle>>,
    thumb_asked: HashSet<(Thumb, u64)>,
    /// Built looks, so a second insert is instant.
    looks: HashMap<Pick, AvatarLook>,
    /// Picked while still building: inserted when the look arrives.
    pending: Vec<(Pick, Opts)>,
    building: HashSet<Pick>,
    pub recent: Vec<Pick>,
    /// Where rigs were dropped (a new one at the same spot steps aside).
    placed: Vec<glam::Vec3>,
    pub anchored: bool,
    pub clothing: bool,
    pub real_colors: bool,
    pub animate: bool,
    pub target: Target,
    pub name: String,
    pub status: Option<(bool, String)>,
    tx: Sender<Msg>,
    rx: Receiver<Msg>,
}

impl Default for CharBuilder {
    fn default() -> Self {
        let (tx, rx) = channel();
        CharBuilder {
            open: false,
            rig: RigType::R15,
            feminine: false,
            tab: Tab::Presets,
            query: String::new(),
            searched: String::new(),
            search_due: None,
            results: None,
            outfits: None,
            friends: None,
            thumbs: HashMap::new(),
            thumb_asked: HashSet::new(),
            looks: HashMap::new(),
            pending: Vec::new(),
            building: HashSet::new(),
            recent: Vec::new(),
            placed: Vec::new(),
            anchored: true,
            clothing: true,
            real_colors: false,
            animate: false,
            target: Target::Workspace,
            name: String::new(),
            status: None,
            tx,
            rx,
        }
    }
}

fn decode_thumb(bytes: &[u8]) -> Option<egui::ColorImage> {
    let img = image::load_from_memory(bytes).ok()?.thumbnail(150, 150).to_rgba8();
    let size = [img.width() as usize, img.height() as usize];
    Some(egui::ColorImage::from_rgba_unmultiplied(size, img.as_raw()))
}

/// A two-or-more way toggle (Studio's radio list, flattened into one row).
fn segmented<T: Copy + PartialEq>(ui: &mut egui::Ui, value: &mut T, options: &[(T, &str)]) -> bool {
    let mut changed = false;
    let font = egui::FontId::proportional(12.0);
    let widths: Vec<f32> = options
        .iter()
        .map(|(_, l)| ui.fonts_mut(|f| f.layout_no_wrap(l.to_string(), font.clone(), theme::TEXT).size().x) + 20.0)
        .collect();
    let (r, _) = ui.allocate_exact_size(Vec2::new(widths.iter().sum::<f32>() + 4.0, 24.0), Sense::hover());
    let p = ui.painter();
    p.rect_filled(r, 5.0, theme::PANEL_DARK);
    p.rect_stroke(r, 5.0, egui::Stroke::new(1.0, theme::DIVIDER), egui::StrokeKind::Inside);
    let mut x = r.min.x + 2.0;
    for ((v, label), w) in options.iter().zip(widths) {
        let cell = Rect::from_min_size(egui::pos2(x, r.min.y + 2.0), Vec2::new(w, 20.0));
        let resp = ui.interact(cell, ui.id().with(("seg", label)), Sense::click());
        let on = *value == *v;
        if on {
            ui.painter().rect_filled(cell, 4.0, theme::ACCENT);
        } else if resp.hovered() {
            ui.painter().rect_filled(cell, 4.0, theme::HOVER_BG);
        }
        ui.painter().text(
            cell.center(),
            egui::Align2::CENTER_CENTER,
            *label,
            font.clone(),
            if on { Color32::WHITE } else { theme::TEXT },
        );
        if resp.clicked() && !on {
            *value = *v;
            changed = true;
        }
        x += w;
    }
    changed
}

/// A tab header button.
fn tab_button(ui: &mut egui::Ui, icon: &str, label: &str, on: bool) -> bool {
    let font = egui::FontId::proportional(12.5);
    let tw = ui.fonts_mut(|f| f.layout_no_wrap(label.into(), font.clone(), theme::TEXT).size().x);
    let (r, resp) = ui.allocate_exact_size(Vec2::new(tw + 34.0, 28.0), Sense::click());
    let p = ui.painter();
    if resp.hovered() && !on {
        p.rect_filled(r, 4.0, theme::HOVER_BG);
    }
    let col = if on { Color32::WHITE } else { theme::TEXT_DIM };
    crate::ti::paint(p, Rect::from_center_size(egui::pos2(r.min.x + 14.0, r.center().y), Vec2::splat(15.0)), icon, col);
    p.text(egui::pos2(r.min.x + 25.0, r.center().y), egui::Align2::LEFT_CENTER, label, font, col);
    if on {
        p.rect_filled(Rect::from_min_max(egui::pos2(r.min.x + 4.0, r.max.y - 2.0), egui::pos2(r.max.x - 4.0, r.max.y)), 1.0, theme::ACCENT);
    }
    resp.clicked()
}

/// What a card was asked to do.
enum CardDo {
    Insert(Pick),
    InsertAs(Pick, Target),
    Rebuild(Pick),
}

const CARD: Vec2 = Vec2::new(104.0, 132.0);

impl CharBuilder {
    fn opts(&self) -> Opts {
        Opts {
            rig: self.rig,
            anchored: self.anchored,
            clothing: self.clothing,
            real_colors: self.real_colors,
            animate: self.animate,
            target: self.target,
            name: self.name.clone(),
        }
    }

    fn texture(&self, key: (Thumb, u64)) -> Option<&egui::TextureHandle> {
        self.thumbs.get(&key).and_then(|t| t.as_ref())
    }

    /// One card: thumbnail, label, sub-label; click inserts, right-click
    /// offers the other target and a rebuild.
    fn card(&self, ui: &mut egui::Ui, pick: &Pick, sub: &str, me: Option<u64>, out: &mut Option<CardDo>, vis: &mut Vec<(Thumb, u64)>) {
        let (r, resp) = ui.allocate_exact_size(CARD, Sense::click());
        if ui.is_rect_visible(r.expand(CARD.y)) {
            vis.extend(pick.thumb(me));
        }
        let p = ui.painter();
        let busy = self.building.contains(pick);
        let hovered = resp.hovered();
        p.rect_filled(r, 6.0, if hovered { theme::ACTIVE_TAB } else { theme::PANEL_DARK });
        p.rect_stroke(
            r,
            6.0,
            egui::Stroke::new(1.0, if hovered { theme::ACCENT } else { theme::DIVIDER }),
            egui::StrokeKind::Inside,
        );
        let img = Rect::from_min_size(r.min + Vec2::new(6.0, 6.0), Vec2::splat(CARD.x - 12.0));
        p.rect_filled(img, 4.0, theme::PANEL_ALT);
        match pick.thumb(me).and_then(|k| self.texture(k)) {
            Some(t) => {
                egui::Image::new(t).corner_radius(4.0).paint_at(ui, img);
            }
            None => {
                let icon = match pick {
                    Pick::Preset { .. } => "user-square",
                    Pick::Me => "user-circle",
                    Pick::Outfit { .. } => "shirt",
                    Pick::User { .. } => "user",
                };
                crate::ti::paint(ui.painter(), Rect::from_center_size(img.center(), Vec2::splat(34.0)), icon, theme::TEXT_DIM);
            }
        }
        if busy {
            ui.painter().rect_filled(img, 4.0, Color32::from_black_alpha(140));
            egui::Spinner::new().size(22.0).color(Color32::WHITE).paint_at(ui, Rect::from_center_size(img.center(), Vec2::splat(22.0)));
        }
        let label = pick.label();
        let font = egui::FontId::proportional(12.0);
        let galley = ui.fonts_mut(|f| {
            let mut s = label.clone();
            while s.chars().count() > 3 && f.layout_no_wrap(s.clone(), font.clone(), theme::TEXT).size().x > CARD.x - 10.0 {
                s.pop();
                s = s.trim_end_matches('…').to_string();
                s.pop();
                s.push('…');
            }
            f.layout_no_wrap(s, font.clone(), theme::TEXT)
        });
        let ty = img.max.y + 4.0;
        ui.painter().galley(egui::pos2(r.center().x - galley.size().x / 2.0, ty), galley, theme::TEXT);
        if !sub.is_empty() {
            ui.painter().text(
                egui::pos2(r.center().x, ty + 17.0),
                egui::Align2::CENTER_TOP,
                if sub.chars().count() > 17 { format!("{}…", sub.chars().take(16).collect::<String>()) } else { sub.to_string() },
                egui::FontId::proportional(10.5),
                theme::TEXT_DIM,
            );
        }
        let resp = resp.on_hover_text(format!("{label}\nClick to insert · right-click for more"));
        if resp.clicked() {
            *out = Some(CardDo::Insert(pick.clone()));
        }
        resp.context_menu(|ui| {
            if ui.button("Insert into Workspace").clicked() {
                *out = Some(CardDo::InsertAs(pick.clone(), Target::Workspace));
                ui.close();
            }
            if ui.button("Use as StarterCharacter").clicked() {
                *out = Some(CardDo::InsertAs(pick.clone(), Target::StarterCharacter));
                ui.close();
            }
            ui.separator();
            if ui.button("Download again").clicked() {
                *out = Some(CardDo::Rebuild(pick.clone()));
                ui.close();
            }
            let id = match pick {
                Pick::Preset { outfit, .. } => Some(("outfit", *outfit)),
                Pick::Outfit { id, .. } => Some(("outfit", *id)),
                Pick::User { id, .. } => Some(("user", *id)),
                Pick::Me => me.map(|m| ("user", m)),
            };
            if let Some((kind, id)) = id {
                if ui.button(format!("Copy {kind} id")).clicked() {
                    ui.ctx().copy_text(id.to_string());
                    ui.close();
                }
            }
        });
    }

    fn grid(&self, ui: &mut egui::Ui, picks: &[(Pick, String)], me: Option<u64>, out: &mut Option<CardDo>, vis: &mut Vec<(Thumb, u64)>) {
        ui.horizontal_wrapped(|ui| {
            ui.spacing_mut().item_spacing = Vec2::new(8.0, 8.0);
            for (p, sub) in picks {
                self.card(ui, p, sub, me, out, vis);
            }
        });
    }

    fn want_thumbs(&mut self, keys: impl IntoIterator<Item = (Thumb, u64)>) {
        let mut by: HashMap<Thumb, Vec<u64>> = HashMap::new();
        for k in keys {
            if self.thumb_asked.insert(k) {
                by.entry(k.0).or_default().push(k.1);
            }
        }
        for (kind, ids) in by {
            let tx = self.tx.clone();
            std::thread::spawn(move || {
                let got = characters::thumbnails(kind, &ids).unwrap_or_default();
                let mut done = HashSet::new();
                for (id, bytes) in got {
                    done.insert(id);
                    let _ = tx.send(Msg::Thumb(kind, id, decode_thumb(&bytes)));
                }
                for id in ids.into_iter().filter(|i| !done.contains(i)) {
                    let _ = tx.send(Msg::Thumb(kind, id, None));
                }
            });
        }
    }
}

impl Editor {
    /// Opens or closes the builder (the Character ribbon button).
    pub fn toggle_character_builder(&mut self) {
        self.chars.open = !self.chars.open;
    }

    /// Harness / menu entry: `character:<preset key>|me|outfit:<id>|user:<id>`
    /// builds and inserts that pick with the current options.
    pub fn character_action(&mut self, what: &str) {
        let pick = match what.split_once(':') {
            Some(("outfit", id)) => id.parse().ok().map(|id| Pick::Outfit { id, name: format!("Outfit {id}") }),
            Some(("user", id)) => id.parse().ok().map(|id| Pick::User { id, name: format!("User {id}") }),
            Some(("tab", t)) => {
                self.chars.open = true;
                self.chars.tab = match t {
                    "outfits" => Tab::Outfits,
                    "friends" => Tab::Friends,
                    _ => Tab::Presets,
                };
                return;
            }
            Some(("rig", r)) => {
                self.chars.rig = if r.eq_ignore_ascii_case("r6") { RigType::R6 } else { RigType::R15 };
                return;
            }
            Some(("search", q)) => {
                self.chars.open = true;
                self.chars.query = q.to_string();
                self.chars.search_due = Some(std::time::Instant::now());
                return;
            }
            Some(("starter", rest)) => {
                self.chars.target = Target::StarterCharacter;
                return self.character_action(rest);
            }
            _ if what == "me" => Some(Pick::Me),
            _ => PRESETS.iter().find(|p| p.0 == what).map(|p| Pick::Preset {
                key: p.0,
                outfit: if self.chars.feminine { p.3 } else { p.2 },
            }),
        };
        match pick {
            Some(p) => self.pick_character(p),
            None => self.warn(format!("character: unknown pick {what:?}")),
        }
    }

    fn pick_character(&mut self, pick: Pick) {
        if self.play.is_some() {
            self.warn("Stop the playtest to insert a character");
            return;
        }
        let opts = self.chars.opts();
        if self.chars.target == Target::StarterCharacter {
            self.chars.target = Target::Workspace;
        }
        if let Some(look) = self.chars.looks.get(&pick).cloned() {
            self.insert_built_character(&pick, &look, &opts);
            return;
        }
        self.chars.pending.push((pick.clone(), opts));
        self.build_character(pick);
    }

    fn build_character(&mut self, pick: Pick) {
        if !self.chars.building.insert(pick.clone()) {
            return;
        }
        let Some(client) = self.account.shared_client() else {
            self.chars.building.remove(&pick);
            self.chars.pending.retain(|p| p.0 != pick);
            self.chars.status = Some((false, "Sign in to download avatars and outfits".into()));
            return;
        };
        let me = self.account.user_id().unwrap_or(0);
        self.chars.status = Some((true, format!("Downloading {}…", pick.label())));
        let tx = self.chars.tx.clone();
        std::thread::spawn(move || {
            let txl = tx.clone();
            let mut log = |s: String| {
                let _ = txl.send(Msg::Log(s));
            };
            let res = (|| -> Result<AvatarLook, String> {
                if pick == Pick::Me {
                    return avatar::load(&client, &mut log);
                }
                let (uid, json) = {
                    let guard = client.lock().map_err(|_| "account busy".to_string())?;
                    let c = guard.as_ref().ok_or("signed out")?;
                    match &pick {
                        Pick::Preset { outfit, .. } | Pick::Outfit { id: outfit, .. } => {
                            (me, characters::outfit_details(&c.session, *outfit).map_err(|e| e.to_string())?)
                        }
                        Pick::User { id, .. } => (*id, characters::user_avatar(&c.session, *id).map_err(|e| e.to_string())?),
                        Pick::Me => unreachable!(),
                    }
                };
                let info = avatar::info_from_json(uid, &json);
                avatar::look_from_info(&client, uid, &info, &mut log)
            })();
            let _ = tx.send(Msg::Built(pick, res));
        });
    }

    /// Takes finished downloads; inserts picks that were waiting on them.
    fn poll_character_builder(&mut self, ctx: &egui::Context) {
        while let Ok(m) = self.chars.rx.try_recv() {
            match m {
                Msg::Thumb(kind, id, img) => {
                    let t = img.map(|img| ctx.load_texture(format!("char-thumb-{kind:?}-{id}"), img, egui::TextureOptions::LINEAR));
                    self.chars.thumbs.insert((kind, id), t);
                }
                Msg::Outfits(r) => match r {
                    Ok(list) => self.chars.outfits = Some(list),
                    Err(e) => {
                        self.chars.outfits = Some(Vec::new());
                        self.chars.status = Some((false, format!("Outfits: {e}")));
                    }
                },
                Msg::Friends(r) => match r {
                    Ok(list) => self.chars.friends = Some(list),
                    Err(e) => {
                        self.chars.friends = Some(Vec::new());
                        self.chars.status = Some((false, format!("Friends: {e}")));
                    }
                },
                Msg::Search(q, r) => {
                    if q == self.chars.searched {
                        match r {
                            Ok(list) => self.chars.results = Some(list),
                            Err(e) => {
                                self.chars.results = Some(Vec::new());
                                self.chars.status = Some((false, format!("Search: {e}")));
                            }
                        }
                    }
                }
                Msg::Log(s) => self.info(s),
                Msg::Built(pick, r) => {
                    self.chars.building.remove(&pick);
                    let waiting: Vec<Opts> = self.chars.pending.iter().filter(|p| p.0 == pick).map(|p| p.1.clone()).collect();
                    self.chars.pending.retain(|p| p.0 != pick);
                    match r {
                        Ok(look) => {
                            self.chars.looks.insert(pick.clone(), look.clone());
                            self.chars.status = None;
                            for opts in waiting {
                                self.insert_built_character(&pick, &look, &opts);
                            }
                        }
                        Err(e) => {
                            self.chars.status = Some((false, format!("{}: {e}", pick.label())));
                            self.error(format!("Character {}: {e}", pick.label()));
                        }
                    }
                }
            }
        }
    }

    /// Builds the rig from a look with the builder's options and inserts it
    /// (one undo step).
    fn insert_built_character(&mut self, pick: &Pick, look: &AvatarLook, opts: &Opts) {
        let rig_type = opts.rig;
        let starter = opts.target == Target::StarterCharacter;
        let name = if starter {
            "StarterCharacter".to_string()
        } else if opts.name.trim().is_empty() {
            pick.model_name()
        } else {
            opts.name.trim().to_string()
        };
        let mut look = look.clone();
        // Studio's presets: a grey 0.5 body (loadOutfitAsHumanoidDescription).
        if matches!(pick, Pick::Preset { .. }) && !opts.real_colors {
            let g = [128, 128, 128];
            look.colors = rig::BodyColors { head: g, torso: g, left_arm: g, right_arm: g, left_leg: g, right_leg: g };
        }
        if !opts.clothing {
            look.shirt = None;
            look.pants = None;
            look.tshirt = None;
            look.accessories.clear();
        }
        let anchored = opts.anchored && !starter;
        let animate = opts.animate || starter;
        let mut tmp = doc::Document::empty();
        let root = tmp.dom.root_ref();
        let humanoid = InstanceBuilder::new("Humanoid")
            .with_name("Humanoid")
            .with_child(InstanceBuilder::new("Animator").with_name("Animator"));
        let scales = if rig_type == RigType::R15 { look.scales } else { rig::Scales::default() };
        let m = rig::insert_character(&mut tmp.dom, root, &name, rig_type, &look.colors, &scales, humanoid);
        avatar::dress(&mut tmp, m, &look);
        if animate {
            crate::default_scripts::insert_character_scripts(&mut tmp, m, rig_type);
            if rig_type == RigType::R15 {
                avatar::put_on_animations(&mut tmp, m, &look);
            }
        }
        // Body parts and accessory handles stand where their joints put them
        // (packages and scaling moved the attachments the joints sit on).
        solve_rest_pose(&mut tmp, m);
        // Which body parts collide, as on a spawned player (play.rs).
        let body: Vec<(Ref, bool)> = tmp
            .dom
            .get_by_ref(m)
            .map(|i| i.children().to_vec())
            .unwrap_or_default()
            .into_iter()
            .filter_map(|c| {
                let i = tmp.dom.get_by_ref(c)?;
                doc::is_a(&i.class, "BasePart").then(|| (c, crate::play::studio_collides(&i.name, rig_type)))
            })
            .collect();
        for (c, on) in body {
            if let Some(i) = tmp.dom.get_by_ref_mut(c) {
                i.properties.insert(ustr("CanCollide"), Variant::Bool(on));
            }
        }
        // Nothing in it is Locked (Studio's R6 template and some catalog
        // handles are): the rig is clicked, dragged and moved whole.
        let all: Vec<Ref> = tmp.dom.descendants_of(m).filter(|i| doc::is_a(&i.class, "BasePart")).map(|i| i.referent()).collect();
        for p in all {
            if let Some(i) = tmp.dom.get_by_ref_mut(p) {
                i.properties.insert(ustr("Locked"), Variant::Bool(false));
            }
        }
        let hrp = tmp.find_named(m, "HumanoidRootPart");
        if let Some(h) = hrp {
            if let Some(i) = tmp.dom.get_by_ref_mut(h) {
                i.properties.insert(ustr("Anchored"), Variant::Bool(anchored));
            }
            if let Some(i) = tmp.dom.get_by_ref_mut(m) {
                i.properties.insert(ustr("PrimaryPart"), Variant::Ref(h));
            }
        }
        // Stand it where the screen centre hits, facing the camera.
        let mut target = self.spawn_point(glam::Vec3::ZERO);
        if !starter {
            let fwd = self.cam.forward();
            let right = glam::Vec3::new(-fwd.z, 0.0, fwd.x).normalize_or(glam::Vec3::X);
            for _ in 0..40 {
                if !self.chars.placed.iter().any(|p| (*p - target).length() < 3.0) {
                    break;
                }
                target += right * 4.0;
            }
            self.chars.placed.push(target);
        }
        let facing = {
            let f = self.cam.pos - target;
            f.z.atan2(f.x)
        };
        place_model(&mut tmp, m, target, facing);
        // The model's pivot is its root part.
        if let Some(Variant::CFrame(cf)) = hrp.and_then(|h| tmp.dom.get_by_ref(h)).and_then(|i| i.properties.get(&ustr("CFrame")).cloned()) {
            if let Some(i) = tmp.dom.get_by_ref_mut(m) {
                i.properties.insert(ustr("WorldPivotData"), Variant::OptionalCFrame(Some(cf)));
            }
        }
        fix_color_types(&mut tmp, m);
        let bytes = match tmp.encode(&[m], false) {
            Ok(b) => b,
            Err(e) => {
                self.error(format!("Character: could not build the model: {e}"));
                return;
            }
        };
        let parent = if starter {
            let rootd = self.doc.dom.root_ref();
            let Some(sp) = self.doc.find_child(rootd, "StarterPlayer") else {
                self.error("This place has no StarterPlayer");
                return;
            };
            let old: Vec<Ref> = self
                .doc
                .dom
                .get_by_ref(sp)
                .map(|i| i.children().to_vec())
                .unwrap_or_default()
                .into_iter()
                .filter(|c| self.doc.dom.get_by_ref(*c).is_some_and(|i| i.name == "StarterCharacter"))
                .collect();
            if !old.is_empty() {
                self.doc.remove(&old);
            }
            sp
        } else {
            self.doc.workspace().unwrap_or_else(|| self.doc.default_parent())
        };
        let refs = self.doc.paste(&bytes, parent, "Insert character");
        self.select(refs);
        let what = format!("{} ({})", pick.label(), if rig_type == RigType::R15 { "R15" } else { "R6" });
        self.info(if starter {
            format!("StarterCharacter is now {what}: Play spawns it instead of the avatar")
        } else {
            format!("Inserted {what} as \"{name}\"")
        });
        self.chars.status = Some((false, format!("Inserted {what} · Ctrl+Z undoes")));
        self.chars.recent.retain(|p| p != pick);
        self.chars.recent.insert(0, pick.clone());
        self.chars.recent.truncate(6);
    }

    /// The builder window.
    pub fn character_builder_window(&mut self, ctx: &egui::Context) {
        self.poll_character_builder(ctx);
        if !self.chars.open {
            return;
        }
        if self.chars.building.len() + self.chars.pending.len() > 0 || self.chars.search_due.is_some() {
            ctx.request_repaint_after(std::time::Duration::from_millis(100));
        }
        let me = self.account.user_id();
        let signed_in = self.account.signed_in();
        // Lists the tab needs.
        if signed_in {
            if self.chars.tab == Tab::Outfits && self.chars.outfits.is_none() {
                self.chars.outfits = Some(Vec::new());
                if let (Some(client), Some(uid)) = (self.account.shared_client(), me) {
                    let tx = self.chars.tx.clone();
                    std::thread::spawn(move || {
                        let r = client
                            .lock()
                            .map_err(|_| "account busy".to_string())
                            .and_then(|g| {
                                let c = g.as_ref().ok_or("signed out".to_string())?;
                                characters::outfits(&c.session, uid).map_err(|e| e.to_string())
                            });
                        let _ = tx.send(Msg::Outfits(r));
                    });
                }
            }
            if self.chars.tab == Tab::Friends && self.chars.friends.is_none() {
                self.chars.friends = Some(Vec::new());
                if let (Some(client), Some(uid)) = (self.account.shared_client(), me) {
                    let tx = self.chars.tx.clone();
                    std::thread::spawn(move || {
                        let r = client
                            .lock()
                            .map_err(|_| "account busy".to_string())
                            .and_then(|g| {
                                let c = g.as_ref().ok_or("signed out".to_string())?;
                                characters::friends(&c.session, uid).map_err(|e| e.to_string())
                            });
                        let _ = tx.send(Msg::Friends(r));
                    });
                }
            }
        }
        // Search after a short pause in typing.
        if let Some(due) = self.chars.search_due {
            if std::time::Instant::now() >= due {
                self.chars.search_due = None;
                let q = self.chars.query.trim().to_string();
                self.chars.searched = q.clone();
                if q.is_empty() {
                    self.chars.results = None;
                } else if let Some(client) = self.account.shared_client() {
                    self.chars.results = None;
                    let tx = self.chars.tx.clone();
                    std::thread::spawn(move || {
                        let r = client
                            .lock()
                            .map_err(|_| "account busy".to_string())
                            .and_then(|g| {
                                let c = g.as_ref().ok_or("signed out".to_string())?;
                                characters::search(&c.session, &q).map_err(|e| e.to_string())
                            });
                        let _ = tx.send(Msg::Search(q, r));
                    });
                }
            }
        }

        let fem = self.chars.feminine;
        let presets: Vec<(Pick, String)> = PRESETS
            .iter()
            .map(|p| (Pick::Preset { key: p.0, outfit: if fem { p.3 } else { p.2 } }, "Preset".to_string()))
            .collect();
        let searching = !self.chars.searched.is_empty() || !self.chars.query.trim().is_empty();
        let people = |list: &Option<Vec<Person>>| -> Vec<(Pick, String)> {
            list.iter()
                .flatten()
                .map(|p| {
                    let shown = if p.display.is_empty() { p.name.clone() } else { p.display.clone() };
                    (Pick::User { id: p.id, name: shown }, format!("@{}", p.name))
                })
                .collect()
        };
        let shown: Vec<(Pick, String)> = if searching {
            people(&self.chars.results)
        } else {
            match self.chars.tab {
                Tab::Presets => {
                    let mut v = Vec::new();
                    if signed_in {
                        v.push((Pick::Me, "Signed in".to_string()));
                    }
                    v.extend(presets.clone());
                    v
                }
                Tab::Outfits => self
                    .chars
                    .outfits
                    .iter()
                    .flatten()
                    .map(|o| (Pick::Outfit { id: o.id, name: o.name.clone() }, "Outfit".into()))
                    .collect(),
                Tab::Friends => people(&self.chars.friends),
            }
        };
        let recent: Vec<(Pick, String)> = self.chars.recent.iter().map(|p| (p.clone(), "Recent".to_string())).collect();
        let mut vis: Vec<(Thumb, u64)> = Vec::new();

        let mut open = true;
        let mut act: Option<CardDo> = None;
        let anchor = self.ui_rects.get("avatar.character").or(self.ui_rects.get("home.character")).copied();
        let mut win = egui::Window::new("Characters")
            .id(egui::Id::new("character_builder"))
            .open(&mut open)
            .default_width(372.0)
            .default_height(560.0)
            .min_width(372.0)
            .resizable(true)
            .collapsible(false);
        if let Some(a) = anchor {
            win = win.default_pos(egui::pos2(a.min.x, a.max.y + 6.0));
        }
        win.show(ctx, |ui| {
            ui.set_min_height(560.0);
            let cb = &mut self.chars;
            ui.spacing_mut().item_spacing = Vec2::new(8.0, 8.0);
            // Rig and body.
            ui.horizontal(|ui| {
                ui.label(egui::RichText::new("Rig").color(theme::TEXT_DIM));
                segmented(ui, &mut cb.rig, &[(RigType::R15, "R15"), (RigType::R6, "R6")]);
                ui.add_space(6.0);
                ui.label(egui::RichText::new("Body").color(theme::TEXT_DIM));
                segmented(ui, &mut cb.feminine, &[(false, "Masculine"), (true, "Feminine")]);
            });
            // Search.
            ui.horizontal(|ui| {
                let (r, _) = ui.allocate_exact_size(Vec2::new(ui.available_width(), 28.0), Sense::hover());
                ui.painter().rect_filled(r, 5.0, theme::PANEL_DARK);
                ui.painter().rect_stroke(r, 5.0, egui::Stroke::new(1.0, theme::DIVIDER), egui::StrokeKind::Inside);
                crate::ti::paint(ui.painter(), Rect::from_center_size(egui::pos2(r.min.x + 15.0, r.center().y), Vec2::splat(15.0)), "search", theme::TEXT_DIM);
                let edit = egui::TextEdit::singleline(&mut cb.query)
                    .hint_text("Find a player by name or id")
                    .frame(egui::Frame::NONE)
                    .desired_width(r.width() - 60.0);
                let resp = ui.put(Rect::from_min_max(egui::pos2(r.min.x + 28.0, r.min.y + 4.0), egui::pos2(r.max.x - 28.0, r.max.y - 4.0)), edit);
                if resp.changed() {
                    cb.search_due = Some(std::time::Instant::now() + std::time::Duration::from_millis(450));
                }
                if !cb.query.is_empty() {
                    let x = Rect::from_center_size(egui::pos2(r.max.x - 14.0, r.center().y), Vec2::splat(18.0));
                    let xr = ui.interact(x, ui.id().with("char-search-clear"), Sense::click());
                    crate::ti::paint(ui.painter(), x.shrink(2.0), "x", if xr.hovered() { theme::TEXT } else { theme::TEXT_DIM });
                    if xr.clicked() {
                        cb.query.clear();
                        cb.searched.clear();
                        cb.results = None;
                        cb.search_due = None;
                    }
                }
            });
            if !searching {
                ui.horizontal(|ui| {
                    ui.spacing_mut().item_spacing.x = 2.0;
                    for (t, icon, label) in [
                        (Tab::Presets, "user-square", "Presets"),
                        (Tab::Outfits, "shirt", "My outfits"),
                        (Tab::Friends, "users", "Friends"),
                    ] {
                        if tab_button(ui, icon, label, cb.tab == t) {
                            cb.tab = t;
                        }
                    }
                });
            } else {
                ui.label(egui::RichText::new(format!("Players matching \"{}\"", cb.query.trim())).color(theme::TEXT_DIM));
            }
            ui.separator();
            let cb = &self.chars;
            let list_h = (ui.available_height() - 165.0).max(300.0);
            egui::ScrollArea::vertical().max_height(list_h).auto_shrink([false, false]).show(ui, |ui| {
                if !recent.is_empty() && !searching && cb.tab == Tab::Presets {
                    ui.label(egui::RichText::new("Recent").color(theme::TEXT_DIM).size(11.5));
                    cb.grid(ui, &recent, me, &mut act, &mut vis);
                    ui.add_space(4.0);
                    ui.label(egui::RichText::new("Presets").color(theme::TEXT_DIM).size(11.5));
                }
                let loading = if searching {
                    cb.results.is_none()
                } else {
                    match cb.tab {
                        Tab::Outfits => cb.outfits.as_ref().is_some_and(|l| l.is_empty()) && signed_in && cb.status.is_none(),
                        Tab::Friends => cb.friends.as_ref().is_some_and(|l| l.is_empty()) && signed_in && cb.status.is_none(),
                        Tab::Presets => false,
                    }
                };
                if !signed_in && (searching || cb.tab != Tab::Presets) {
                    ui.label(egui::RichText::new("Sign in (top right) to see outfits, friends and other players.").color(theme::TEXT_DIM));
                } else if shown.is_empty() && loading {
                    ui.horizontal(|ui| {
                        ui.add(egui::Spinner::new().size(14.0).color(theme::TEXT_DIM));
                        ui.label(egui::RichText::new("Loading…").color(theme::TEXT_DIM));
                    });
                } else if shown.is_empty() {
                    ui.label(egui::RichText::new(if searching { "No players found" } else { "Nothing here yet" }).color(theme::TEXT_DIM));
                } else {
                    cb.grid(ui, &shown, me, &mut act, &mut vis);
                }
            });
            ui.separator();
            let cb = &mut self.chars;
            // Options.
            egui::Grid::new("char-opts").num_columns(2).spacing([16.0, 4.0]).show(ui, |ui| {
                ui.checkbox(&mut cb.anchored, "Anchored").on_hover_text("Anchor the HumanoidRootPart so the rig stays put (Studio's rigs)");
                ui.checkbox(&mut cb.clothing, "Clothing & accessories");
                ui.end_row();
                ui.checkbox(&mut cb.real_colors, "Outfit skin colours").on_hover_text("Presets keep their own skin tones instead of Studio's grey");
                ui.checkbox(&mut cb.animate, "Animate script").on_hover_text("Add Roblox's Animate and Health scripts, with the outfit's animation packs (R15)");
                ui.end_row();
            });
            ui.horizontal(|ui| {
                ui.label(egui::RichText::new("Insert as").color(theme::TEXT_DIM));
                segmented(ui, &mut cb.target, &[(Target::Workspace, "Rig in Workspace"), (Target::StarterCharacter, "StarterCharacter")]);
            });
            if cb.target == Target::Workspace {
                ui.horizontal(|ui| {
                    ui.label(egui::RichText::new("Name").color(theme::TEXT_DIM));
                    ui.add(egui::TextEdit::singleline(&mut cb.name).hint_text("Automatic").desired_width(ui.available_width()));
                });
            } else {
                ui.label(
                    egui::RichText::new("Replaces StarterPlayer.StarterCharacter; Play spawns it instead of the avatar.")
                        .color(theme::TEXT_DIM)
                        .size(11.5),
                );
            }
            if let Some((_, s)) = &cb.status {
                let busy = !cb.building.is_empty();
                ui.horizontal(|ui| {
                    if busy {
                        ui.add(egui::Spinner::new().size(12.0).color(theme::TEXT_DIM));
                    }
                    ui.label(egui::RichText::new(s).color(theme::TEXT_DIM).size(11.5));
                });
            }
        });
        if !open {
            self.chars.open = false;
        }
        self.chars.want_thumbs(vis);
        match act {
            Some(CardDo::Insert(p)) => self.pick_character(p),
            Some(CardDo::InsertAs(p, t)) => {
                self.chars.target = t;
                self.pick_character(p);
            }
            Some(CardDo::Rebuild(p)) => {
                self.chars.looks.remove(&p);
                self.build_character(p);
            }
            None => {}
        }
    }
}

/// Moves a model so its feet stand on `ground` (its root over it), turned
/// `yaw` radians about Y from the rig's built facing.
fn place_model(d: &mut doc::Document, m: Ref, ground: glam::Vec3, yaw: f32) {
    use crate::math::Xform;
    let parts: Vec<Ref> = d.dom.descendants_of(m).filter(|i| doc::is_a(&i.class, "BasePart")).map(|i| i.referent()).collect();
    let xf = |d: &doc::Document, r: Ref| match d.dom.get_by_ref(r).and_then(|i| i.properties.get(&ustr("CFrame"))) {
        Some(Variant::CFrame(c)) => Some(Xform::from_cframe(c)),
        _ => None,
    };
    let root = d.find_named(m, "HumanoidRootPart").and_then(|h| xf(d, h)).map(|x| x.pos).unwrap_or(glam::Vec3::ZERO);
    // Lowest point of the body (axis-aligned parts at build time).
    let mut min_y = f32::MAX;
    for p in &parts {
        let (Some(x), Some(Variant::Vector3(s))) = (xf(d, *p), d.dom.get_by_ref(*p).and_then(|i| i.properties.get(&ustr("Size")).cloned())) else {
            continue;
        };
        let half = glam::Vec3::new(s.x, s.y, s.z) * 0.5;
        let ext = (x.rot.x_axis * half.x).abs() + (x.rot.y_axis * half.y).abs() + (x.rot.z_axis * half.z).abs();
        min_y = min_y.min(x.pos.y - ext.y);
    }
    if min_y == f32::MAX {
        min_y = root.y;
    }
    // Turn about Y so the root's front (its -Z) looks along (cos yaw, 0,
    // sin yaw); R6 and R15 templates are built facing different ways.
    let front = d
        .find_named(m, "HumanoidRootPart")
        .and_then(|h| xf(d, h))
        .map(|x| -x.rot.z_axis)
        .unwrap_or(glam::Vec3::NEG_Z);
    let now = front.z.atan2(front.x);
    let turn = glam::Mat3::from_rotation_y(now - yaw);
    let to = glam::Vec3::new(ground.x, ground.y + (root.y - min_y), ground.z);
    for p in parts {
        let Some(x) = xf(d, p) else { continue };
        let nx = Xform { pos: to + turn * (x.pos - root), rot: turn * x.rot };
        if let Some(i) = d.dom.get_by_ref_mut(p) {
            i.properties.insert(ustr("CFrame"), Variant::CFrame(nx.to_cframe()));
        }
    }
}

/// Color3 properties the rig builder stored as Color3uint8 (BodyColors'
/// HeadColor3 …) become Color3, so the model encodes.
fn fix_color_types(d: &mut doc::Document, m: Ref) {
    use rbx_reflection::{DataType, PropertyDescriptor};
    let db = doc::db();
    let refs: Vec<Ref> = std::iter::once(m).chain(d.dom.descendants_of(m).map(|i| i.referent())).collect();
    for r in refs {
        let Some(i) = d.dom.get_by_ref_mut(r) else { continue };
        let Some(desc) = db.classes.get(i.class.as_str()) else { continue };
        let wants_color3 = |name: &str| {
            db.superclasses_iter(desc).find_map(|c| c.properties.get(name)).is_some_and(|p: &PropertyDescriptor| {
                matches!(p.data_type, DataType::Value(rbx_dom_weak::types::VariantType::Color3))
            })
        };
        let fix: Vec<(rbx_dom_weak::Ustr, Variant)> = i
            .properties
            .iter()
            .filter_map(|(k, v)| match v {
                Variant::Color3uint8(c) if wants_color3(k.as_str()) => Some((
                    *k,
                    Variant::Color3(rbx_dom_weak::types::Color3::new(c.r as f32 / 255.0, c.g as f32 / 255.0, c.b as f32 / 255.0)),
                )),
                _ => None,
            })
            .collect();
        for (k, v) in fix {
            i.properties.insert(k, v);
        }
    }
}

/// Places every part from the HumanoidRootPart outwards through its joints
/// in the rest pose: Part1 = Part0 · C0 · C1⁻¹ (Motor6D, Weld, ManualWeld,
/// Snap, Glue), either way round, each part once.
fn solve_rest_pose(d: &mut doc::Document, m: Ref) {
    use crate::math::Xform;
    use std::collections::{HashMap, HashSet, VecDeque};
    let cf = |d: &doc::Document, r: Ref, k: &str| match d.dom.get_by_ref(r).and_then(|i| i.properties.get(&ustr(k))) {
        Some(Variant::CFrame(c)) => Some(Xform::from_cframe(c)),
        _ => None,
    };
    let refp = |d: &doc::Document, r: Ref, k: &str| match d.dom.get_by_ref(r).and_then(|i| i.properties.get(&ustr(k))) {
        Some(Variant::Ref(x)) if x.is_some() => Some(*x),
        _ => None,
    };
    let mul = |a: &Xform, b: &Xform| Xform { pos: a.apply(b.pos), rot: a.rot * b.rot };
    let Some(root) = d.find_named(m, "HumanoidRootPart") else { return };
    let Some(root_xf) = cf(d, root, "CFrame") else { return };
    // part → [(other part, other = this · off)]
    let mut links: HashMap<Ref, Vec<(Ref, Xform)>> = HashMap::new();
    let joints: Vec<Ref> = d
        .dom
        .descendants_of(m)
        .filter(|i| matches!(i.class.as_str(), "Motor6D" | "Weld" | "ManualWeld" | "Snap" | "Glue" | "Motor"))
        .map(|i| i.referent())
        .collect();
    for j in joints {
        let (Some(p0), Some(p1), Some(c0), Some(c1)) = (refp(d, j, "Part0"), refp(d, j, "Part1"), cf(d, j, "C0"), cf(d, j, "C1")) else {
            continue;
        };
        let off = mul(&c0, &c1.inverse());
        links.entry(p0).or_default().push((p1, off));
        links.entry(p1).or_default().push((p0, off.inverse()));
    }
    let mut placed: HashMap<Ref, Xform> = HashMap::new();
    placed.insert(root, root_xf);
    let mut seen = HashSet::from([root]);
    let mut queue = VecDeque::from([root]);
    while let Some(p) = queue.pop_front() {
        let here = placed[&p];
        for (other, off) in links.get(&p).cloned().unwrap_or_default() {
            if seen.insert(other) {
                placed.insert(other, mul(&here, &off));
                queue.push_back(other);
            }
        }
    }
    for (p, xf) in placed {
        if let Some(i) = d.dom.get_by_ref_mut(p) {
            i.properties.insert(ustr("CFrame"), Variant::CFrame(xf.to_cframe()));
        }
    }
}
