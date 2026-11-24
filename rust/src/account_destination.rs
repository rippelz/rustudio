use std::collections::HashMap;
use std::sync::mpsc::{self, Receiver, TryRecvError};

use egui::{Button, Color32, RichText};
use rustudio_account::{
    cloud::{CloudPlace, GroupInfo},
    Client, CloudApi, Error, Experience, ExperienceSource, VersionType,
};

use super::{client_snapshot, divider, theme, Editor, SessionState};
use rustudio_core::{assets::SharedClient, doc::DocKind};

#[derive(Clone)]
struct Target {
    universe: u64,
    place: u64,
    experience_name: String,
    place_name: String,
}

enum Page {
    Experiences,
    Places(Experience),
    NewExperience,
    NewPlace(Experience),
    Created { target: Target, back: Box<Page> },
}

enum Request {
    Experiences(ExperienceSource),
    Places(u64),
    CreateExperience(String, String, Option<u64>),
    CreatePlace(Experience, String),
}

enum Reply {
    Experiences(Vec<Experience>),
    Places(Vec<CloudPlace>),
    Created(Target),
}

impl Request {
    fn execute(self, client: Client) -> rustudio_account::Result<Reply> {
        match self {
            Self::CreateExperience(name, description, group) => {
                let (universe, place) = client.create_experience(&name, &description, group)?;
                Ok(Reply::Created(Target {
                    universe,
                    place,
                    experience_name: name.clone(),
                    place_name: name,
                }))
            }
            Self::CreatePlace(experience, name) => {
                let place = client.create_place(experience.id, &name)?;
                Ok(Reply::Created(Target {
                    universe: experience.id,
                    place,
                    experience_name: experience.name,
                    place_name: name,
                }))
            }
            request => {
                let api = CloudApi::new(client.session, Default::default(), None);
                match request {
                    Self::Experiences(ExperienceSource::Mine) => {
                        api.my_experiences().map(Reply::Experiences)
                    }
                    Self::Experiences(ExperienceSource::Shared) => {
                        api.shared_experiences().map(Reply::Experiences)
                    }
                    Self::Experiences(ExperienceSource::Group(id)) => {
                        // The Start Page's search: develop's group list refuses
                        // a role without manageGroupGames even where an
                        // experience gives this account edit.
                        api.group_search_experiences(id).map(Reply::Experiences)
                    }
                    Self::Places(id) => api.places(id).map(Reply::Places),
                    Self::CreateExperience(..) | Self::CreatePlace(..) => unreachable!(),
                }
            }
        }
    }
}

pub(crate) struct Picker {
    mode: VersionType,
    document: u64,
    owner: Option<u64>,
    page: Page,
    source: ExperienceSource,
    experiences: Option<Vec<Experience>>,
    places: Option<Vec<CloudPlace>>,
    place_choice: Option<PlaceChoice>,
    thumbnails: HashMap<u64, egui::TextureHandle>,
    thumbnail_worker: Option<Receiver<(u64, egui::ColorImage)>>,
    groups: Option<rustudio_account::Result<Vec<GroupInfo>>>,
    group_worker: Option<Receiver<rustudio_account::Result<Vec<GroupInfo>>>>,
    search: String,
    name: String,
    description: String,
    error: Option<String>,
    submitted: Option<(u64, u64)>,
    worker: Option<Receiver<rustudio_account::Result<Reply>>>,
    creating: bool,
    show_sign_in: bool,
}

impl Picker {
    fn new(mode: VersionType, document: u64) -> Self {
        Self {
            mode,
            document,
            owner: None,
            page: Page::Experiences,
            source: ExperienceSource::Mine,
            experiences: None,
            places: None,
            groups: None,
            group_worker: None,
            search: String::new(),
            place_choice: None,
            thumbnails: HashMap::new(),
            thumbnail_worker: None,
            name: String::new(),
            description: String::new(),
            error: None,
            submitted: None,
            worker: None,
            creating: false,
            show_sign_in: false,
        }
    }

    fn start(&mut self, ctx: &egui::Context, client: SharedClient, request: Request) {
        self.error = None;
        self.creating = matches!(
            request,
            Request::CreateExperience(..) | Request::CreatePlace(..)
        );
        self.worker = Some(self.fetch(ctx, client, move |client| request.execute(client)));
    }

    fn fetch<T: Send + 'static>(
        &self,
        ctx: &egui::Context,
        client: SharedClient,
        fetch: impl FnOnce(Client) -> rustudio_account::Result<T> + Send + 'static,
    ) -> Receiver<rustudio_account::Result<T>> {
        let (tx, rx) = mpsc::channel();
        let owner = self.owner;
        let ctx = ctx.clone();
        std::thread::spawn(move || {
            let result = client_snapshot(&client).and_then(|mut client| {
                if Some(client.user_id()) != owner {
                    return Err(Error::new(
                        "The Roblox account changed. Choose the destination again.",
                    ));
                }
                client.session = client
                    .session
                    .with_timeout(std::time::Duration::from_secs(20));
                fetch(client)
            });
            let _ = tx.send(result);
            ctx.request_repaint();
        });
        rx
    }

    fn poll(&mut self, ctx: &egui::Context) -> Option<Target> {
        if let Some(rx) = &self.group_worker {
            match rx.try_recv() {
                Ok(result) => self.groups = Some(result),
                Err(TryRecvError::Disconnected) => {
                    self.groups = Some(Err(Error::new("Group lookup stopped unexpectedly.")));
                }
                Err(TryRecvError::Empty) => {}
            }
            if self.groups.is_some() {
                self.group_worker = None;
            }
        }
        if let Some(rx) = &self.thumbnail_worker {
            loop {
                match rx.try_recv() {
                    Ok((id, image)) => {
                        let texture = ctx.load_texture(
                            format!("destination-place-{id}"),
                            image,
                            egui::TextureOptions::LINEAR,
                        );
                        self.thumbnails.insert(id, texture);
                    }
                    Err(TryRecvError::Empty) => break,
                    Err(TryRecvError::Disconnected) => {
                        self.thumbnail_worker = None;
                        break;
                    }
                }
            }
        }
        let result = match self.worker.as_ref()?.try_recv() {
            Ok(result) => result,
            Err(TryRecvError::Empty) => return None,
            Err(TryRecvError::Disconnected) => {
                Err(Error::new("The Roblox request stopped unexpectedly."))
            }
        };
        self.worker = None;
        self.creating = false;
        match result {
            Ok(Reply::Experiences(rows)) => self.experiences = Some(rows),
            Ok(Reply::Places(rows)) => {
                let ids = rows
                    .iter()
                    .map(|p| p.id)
                    .filter(|id| !self.thumbnails.contains_key(id))
                    .collect::<Vec<_>>();
                self.fetch_place_thumbnails(ctx, ids);
                self.places = Some(rows);
            }
            Ok(Reply::Created(target)) => {
                let back = match std::mem::replace(&mut self.page, Page::Experiences) {
                    Page::NewPlace(experience) => Page::Places(experience),
                    _ => Page::Experiences,
                };
                self.experiences = None;
                self.places = None;
                self.page = Page::Created {
                    target: target.clone(),
                    back: Box::new(back),
                };
                return Some(target);
            }
            Err(error) => self.error = Some(error.to_string()),
        }
        None
    }

    fn fetch_place_thumbnails(&mut self, ctx: &egui::Context, ids: Vec<u64>) {
        use rustudio_account::characters::{thumbnails, Thumb};

        if ids.is_empty() {
            return;
        }
        let (tx, rx) = mpsc::channel();
        self.thumbnail_worker = Some(rx);
        let ctx = ctx.clone();
        std::thread::spawn(move || {
            for batch in ids.chunks(12) {
                let Ok(images) = thumbnails(Thumb::Asset, batch) else {
                    continue;
                };
                for (id, bytes) in images {
                    let Ok(img) = image::load_from_memory(&bytes) else {
                        continue;
                    };
                    let img = img.thumbnail(128, 128).to_rgba8();
                    let size = [img.width() as usize, img.height() as usize];
                    if tx
                        .send((
                            id,
                            egui::ColorImage::from_rgba_unmultiplied(size, img.as_raw()),
                        ))
                        .is_err()
                    {
                        return;
                    }
                }
                ctx.request_repaint();
            }
        });
    }

    fn load_page(&mut self, ctx: &egui::Context, client: SharedClient) {
        let request = match &self.page {
            Page::Experiences => Request::Experiences(self.source),
            Page::Places(experience) => Request::Places(experience.id),
            _ => return,
        };
        self.start(ctx, client, request);
    }

    fn selected_target(&self) -> Option<&Target> {
        match (&self.page, &self.place_choice) {
            (Page::Created { target, .. }, _)
            | (Page::Places(_), Some(PlaceChoice::Existing(target))) => Some(target),
            _ => None,
        }
    }

    fn back(&mut self) {
        // Results from an abandoned page must not populate the new page.
        self.worker = None;
        self.page = match std::mem::replace(&mut self.page, Page::Experiences) {
            Page::NewPlace(experience) => Page::Places(experience),
            Page::Created { back, .. } => *back,
            _ => Page::Experiences,
        };
        self.search.clear();
        self.error = None;
    }
}

enum PlaceChoice {
    New,
    Existing(Target),
}

enum Action {
    Close,
    Back,
    Refresh,
    Source(ExperienceSource),
    Experience(Experience),
    NewExperience,
    NewPlace(Experience),
    Create(Request),
    Submit(Target),
    SignIn,
}

impl Editor {
    pub(super) fn roblox_destination_window(&mut self, ctx: &egui::Context) {
        let Some(mode) = self.cloud_upload_mode() else {
            self.account_destination = None;
            return;
        };
        if self.cloud_upload_result().is_some_and(Result::is_ok) {
            self.account_destination = None;
            self.close_cloud_upload();
            return;
        }
        let owner = self.game.account.user_id();
        let mut picker = self.account_destination
            .take()
            .filter(|p| {
                p.mode == mode
                    && p.document == self.game.doc.id
                    && (p.owner.is_none() || p.owner == owner)
            })
            .unwrap_or_else(|| Picker::new(mode, self.game.doc.id));
        picker.owner = owner;
        let created_target = picker.poll(ctx);
        let save_error = self
            .cloud_upload_result()
            .and_then(|result| result.as_ref().err())
            .cloned();
        let client = self.game.account.shared_client();
        if let Some(client) = &client {
            if picker.groups.is_none() && picker.group_worker.is_none() {
                picker.group_worker = Some(picker.fetch(ctx, client.clone(), |client| {
                    CloudApi::new(client.session, Default::default(), None).publish_groups()
                }));
            }
            if matches!(picker.page, Page::Experiences)
                && picker.experiences.is_none()
                && picker.worker.is_none()
                && picker.error.is_none()
                && self.game.doc.kind == DocKind::Place
            {
                picker.load_page(ctx, client.clone());
            }
        }
        if let Some(Ok(groups)) = &picker.groups {
            crate::group_icons::request(ctx, &self.cloud.endpoints.thumbnails, groups.iter().map(|g| g.id));
        }
        let uploading = self.cloud_upload_pending();
        let blocked = picker.creating || uploading || created_target.is_some();
        let personal_creator = self.game.account.profile()
            .map(|p| format!("@{}", p.name));
        let copy_source = self
            .game.doc
            .cloud
            .as_ref()
            .filter(|o| o.local_copy)
            .map(|o| o.place_id);
        let title = if mode == VersionType::Published {
            "Publish As"
        } else {
            "Save to Roblox As"
        };
        let browsing = matches!(picker.page, Page::Experiences | Page::Places(_));
        let (max_width, max_body_height) = if browsing {
            (960.0, 480.0)
        } else {
            (650.0, 360.0)
        };
        let width = (ctx.content_rect().width() - 64.0).clamp(250.0, max_width);
        let body_height = (ctx.content_rect().height() - 220.0).clamp(100.0, max_body_height);
        let mut visible_icons = Vec::new();
        let modal = egui::Modal::new(egui::Id::new("roblox_destination"))
            .backdrop_color(theme::BG.gamma_multiply_u8(100))
            .frame(egui::Frame::new()
                .inner_margin(egui::Margin::same(20))
                .fill(theme::PANEL)
                .stroke(egui::Stroke::new(1.0, theme::DIVIDER))
                .corner_radius(10))
            .show(ctx, |ui| {
                ui.set_width(width);
                ui.horizontal(|ui| {
                    ui.label(RichText::new(title).size(22.0).strong());
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if ui.add_enabled(!blocked, Button::new("×").frame(false))
                            .on_hover_text("Close").clicked()
                        {
                            ui.close();
                        }
                    });
                });
                ui.add_space(6.0);
                ui.label(RichText::new(self.game.account.status_short()).color(theme::TEXT_DIM));
                ui.add_space(12.0);
                divider(ui);
                ui.add_space(12.0);
                let mut action = None;
                let scroll_id = match picker.page {
                    Page::Experiences => "experiences",
                    Page::Places(_) => "places",
                    Page::NewExperience => "new_experience",
                    Page::NewPlace(_) => "new_place",
                    Page::Created { .. } => "confirm",
                };
                egui::ScrollArea::vertical().id_salt(scroll_id).max_height(body_height)
                    .auto_shrink([false, false]).show(ui, |ui| {
                        ui.set_min_height(body_height);
                        if self.game.doc.kind != DocKind::Place {
                            ui.label("Open a place to save or publish to Roblox.");
                        } else if owner.is_some() {
                            action = picker.contents(ui, uploading, &self.cloud, personal_creator.as_deref(), &mut visible_icons);
                            if matches!(&picker.place_choice, Some(PlaceChoice::Existing(target)) if Some(target.place) == copy_source)
                                && matches!(picker.page, Page::Places(_))
                            {
                                ui.label(RichText::new("Overwriting this Team Create place replaces its server version with your local copy.").color(theme::WARN));
                            }
                        } else if matches!(self.game.account.state, SessionState::Checking(_)) {
                            ui.spinner();
                        } else if picker.show_sign_in {
                            self.game.account.heads(ctx);
                            self.account_contents(ui);
                        } else {
                            ui.label("Sign in to choose a destination on Roblox.");
                            if ui.button("Sign in").clicked() {
                                action = Some(Action::SignIn);
                            }
                        }
                    });
                let target_error = save_error.as_deref().filter(|_| picker.selected_target().is_some_and(|target|
                    picker.submitted == Some((target.universe, target.place))));
                if let Some(error) = picker.error.as_deref().or(target_error) {
                    ui.add_space(8.0);
                    ui.label(RichText::new(error).color(theme::ERROR));
                }
                ui.add_space(10.0);
                divider(ui);
                ui.add_space(12.0);
                ui.horizontal(|ui| {
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if let Some(next) = picker.footer(ui, blocked, owner.is_some(), uploading) {
                            action = Some(next);
                        }
                    });
                });
                action
            });
        if !blocked && (modal.should_close() || matches!(modal.inner, Some(Action::Close))) {
            if picker.show_sign_in {
                if let Some(quick) = &self.game.account.quick {
                    quick.cancel();
                }
                self.game.account.cookie_input.clear();
            }
            self.close_cloud_upload();
            return;
        }
        if !visible_icons.is_empty() {
            self.cloud
                .request_experience_icons(client.clone(), visible_icons);
        }
        let action = created_target.map(Action::Submit).or(modal.inner);
        match (action, client) {
            (Some(Action::SignIn), _) => picker.show_sign_in = true,
            (Some(action), Some(client)) => match action {
                Action::Close | Action::SignIn => {}
                Action::Refresh => {
                    if matches!(picker.page, Page::Experiences) && picker.group_worker.is_none() {
                        picker.groups = None;
                    }
                    picker.load_page(ctx, client);
                }
                Action::Source(source) => {
                    picker.source = source;
                    picker.experiences = None;
                    picker.search.clear();
                    picker.load_page(ctx, client);
                }
                Action::Back => {
                    picker.back();
                    if matches!(picker.page, Page::Places(_)) && picker.places.is_none() {
                        picker.load_page(ctx, client);
                    }
                }
                Action::Experience(experience) => {
                    picker.page = Page::Places(experience);
                    picker.places = None;
                    picker.place_choice = None;
                    picker.search.clear();
                    picker.load_page(ctx, client);
                }
                Action::NewExperience | Action::NewPlace(_) => {
                    picker.page = match action {
                        Action::NewPlace(experience) => Page::NewPlace(experience),
                        _ => Page::NewExperience,
                    };
                    picker.name.clone_from(&self.game.doc.title);
                    picker.description.clear();
                    picker.error = None;
                }
                Action::Create(request) => picker.start(ctx, client, request),
                Action::Submit(target) => {
                    picker.submitted = Some((target.universe, target.place));
                    picker.error = self
                        .submit_cloud_upload(target.universe, target.place, mode)
                        .err();
                }
            },
            _ => {}
        }
        if picker.worker.is_some() || blocked {
            ctx.request_repaint_after(std::time::Duration::from_millis(100));
        }
        self.account_destination = self.cloud_upload_mode().map(|_| picker);
    }
}

impl Picker {
    fn contents(
        &mut self,
        ui: &mut egui::Ui,
        uploading: bool,
        cloud: &crate::cloud_ui::CloudUi,
        personal_creator: Option<&str>,
        visible_icons: &mut Vec<u64>,
    ) -> Option<Action> {
        ui.spacing_mut().item_spacing.y = 0.0;
        let mut action = None;
        let available = self.worker.is_none() && !uploading;
        match &self.page {
            Page::Experiences => {
                ui.horizontal_wrapped(|ui| {
                    ui.spacing_mut().item_spacing = egui::vec2(8.0, 4.0);
                    let creator_width = (ui.available_width() * 0.75).min(300.0);
                    let creator_name = match self.source {
                        ExperienceSource::Mine => "My experiences",
                        ExperienceSource::Shared => "Shared with me",
                        ExperienceSource::Group(id) => self
                            .groups
                            .as_ref()
                            .and_then(|result| result.as_ref().ok())
                            .and_then(|groups| groups.iter().find(|g| g.id == id))
                            .map(|g| g.name.as_str())
                            .unwrap_or("Group experiences"),
                    };
                    let button = ui.add_sized(
                        [creator_width, ui.spacing().interact_size.y],
                        Button::new(creator_name).right_text(rustudio_engine::ti::text("chevron-down", 12.0)).truncate(),
                    );
                    egui::Popup::menu(&button)
                        .width(creator_width)
                        .close_behavior(egui::PopupCloseBehavior::CloseOnClickOutside)
                        .show(|ui| {
                            // An open popup retains its previous size. Give the scroll area the
                            // new height when the asynchronous membership list arrives.
                            let group_count = match &self.groups {
                                Some(Ok(groups)) => {
                                    groups.len()
                                }
                                _ => 1,
                            };
                            let height = ((2 + group_count) as f32
                                * (26.0 + ui.spacing().item_spacing.y))
                                .min(360.0)
                                .min(ui.ctx().content_rect().height() * 0.6);
                            ui.set_height(height);
                            ui.spacing_mut().scroll = egui::style::ScrollStyle::solid();
                            egui::ScrollArea::vertical()
                                .id_salt("creator_groups")
                                .show(ui, |ui| {
                                    let row_size = egui::vec2(ui.available_width(), 26.0);
                                    for (source, label) in [
                                        (ExperienceSource::Mine, "My experiences"),
                                        (ExperienceSource::Shared, "Shared with me"),
                                    ] {
                                        if ui
                                            .add_enabled(
                                                available,
                                                Button::selectable(self.source == source, label)
                                                    .wrap()
                                                    .min_size(row_size),
                                            )
                                            .clicked()
                                        {
                                            ui.close();
                                            action = Some(Action::Source(source));
                                        }
                                    }
                                    if let Some(Ok(groups)) = &self.groups {
                                        // publish_groups: groups with an experience this
                                        // account can edit, or whose role may make one.
                                        for group in groups {
                                            let source = ExperienceSource::Group(group.id);
                                            if ui
                                                .add_enabled(
                                                    available,
                                                    Button::selectable(
                                                        self.source == source,
                                                        (
                                                            crate::group_icons::image(ui.ctx(), group.id, 16.0),
                                                            group.name.as_str(),
                                                        ),
                                                    )
                                                    .wrap()
                                                    .min_size(row_size),
                                                )
                                                .clicked()
                                            {
                                                ui.close();
                                                action = Some(Action::Source(source));
                                            }
                                        }
                                    } else if let Some(Err(error)) = &self.groups {
                                        ui.label("Couldn’t load groups. Use Refresh to retry.")
                                            .on_hover_text(error.to_string());
                                    } else {
                                        ui.label(
                                            RichText::new("Loading groups…").color(theme::TEXT_DIM),
                                        );
                                    }
                                });
                        });
                    ui.add(
                        egui::TextEdit::singleline(&mut self.search)
                            .hint_text("Search experiences")
                            .desired_width(150.0),
                    );
                    if ui.add_enabled(available, Button::new("Refresh")).clicked() {
                        action = Some(Action::Refresh);
                    }
                });
                ui.add_space(10.0);
                if let Some(experiences) = &self.experiences {
                    let query = self.search.trim().to_lowercase();
                    let mut matches = experiences
                        .iter()
                        .filter(|e| e.name.to_lowercase().contains(&query))
                        .peekable();
                    if matches.peek().is_none() {
                        ui.label(if query.is_empty() {
                            "No experiences found."
                        } else {
                            "No matching experiences."
                        });
                    }
                    tile_grid(ui, matches, |ui, width, experience| {
                        let icon = cloud.icon(experience.id);
                        let tile = destination_tile(
                            ui,
                            TileId::Experience(experience.id),
                            width,
                            available,
                            Tile {
                                name: &experience.name,
                                image: icon.as_ref(),
                                ..Tile::default()
                            },
                        );
                        if ui.is_rect_visible(tile.rect) {
                            visible_icons.push(experience.id);
                        }
                        if tile.clicked() {
                            action = Some(Action::Experience(experience.clone()));
                        }
                    });
                }
            }
            Page::Places(experience) => {
                visible_icons.push(experience.id);
                ui.horizontal(|ui| {
                    if ui
                        .add_enabled(!uploading, Button::new("‹ Experiences").frame(false))
                        .clicked()
                    {
                        action = Some(Action::Back);
                    }
                    if self.error.is_some()
                        && self.places.is_none()
                        && ui.add_enabled(available, Button::new("Retry")).clicked()
                    {
                        action = Some(Action::Refresh);
                    }
                    ui.add(egui::Label::new(RichText::new(&experience.name).strong()).truncate())
                        .on_hover_text(&experience.name);
                });
                ui.add_space(7.0);
                let places = self.places.as_deref().unwrap_or_default();
                let is_root = |place: &CloudPlace| Some(place.id) == experience.root_place_id;
                let root = places.iter().find(|place| is_root(place));
                let others = places.iter().filter(|place| !is_root(place));
                let choices = std::iter::once(None).chain(root.into_iter().chain(others).map(Some));
                tile_grid(ui, choices, |ui, width, place| {
                    let tile = if let Some(place) = place {
                        let root = is_root(place);
                        let selected = matches!(&self.place_choice, Some(PlaceChoice::Existing(target)) if target.place == place.id);
                        let experience_icon = root.then(|| cloud.icon(experience.id)).flatten();
                        destination_tile(
                            ui,
                            TileId::Place(place.id),
                            width,
                            available,
                            Tile {
                                name: &place.name,
                                subtitle: if root { "Current Start Place" } else { "" },
                                image: self.thumbnails.get(&place.id).or(experience_icon.as_ref()),
                                selected: Some(selected),
                            },
                        )
                    } else {
                        destination_tile(
                            ui,
                            TileId::NewPlace,
                            width,
                            available,
                            Tile {
                                name: "Add as a new place",
                                selected: Some(matches!(self.place_choice, Some(PlaceChoice::New))),
                                ..Tile::default()
                            },
                        )
                    };
                    if tile.clicked() {
                        self.error = None;
                        self.place_choice = Some(match place {
                            Some(place) => PlaceChoice::Existing(Target {
                                universe: experience.id,
                                place: place.id,
                                experience_name: experience.name.clone(),
                                place_name: place.name.clone(),
                            }),
                            None => PlaceChoice::New,
                        });
                    }
                });
            }
            Page::NewExperience | Page::NewPlace(_) => {
                let new_experience = matches!(&self.page, Page::NewExperience);
                let title = if new_experience {
                    "New Experience"
                } else {
                    "Add New Place"
                };
                ui.label(RichText::new(title).size(18.0).strong());
                if let Page::NewPlace(experience) = &self.page {
                    ui.label(RichText::new(&experience.name).color(theme::TEXT_DIM));
                } else {
                    let creator = match self.source {
                        ExperienceSource::Group(id) => self
                            .groups
                            .as_ref()
                            .and_then(|result| result.as_ref().ok())
                            .and_then(|groups| groups.iter().find(|g| g.id == id))
                            .map(|g| g.name.as_str())
                            .unwrap_or("Selected group"),
                        _ => personal_creator.unwrap_or("Personal account"),
                    };
                    ui.label(RichText::new(format!("Creator · {creator}")).color(theme::TEXT_DIM));
                }
                ui.add_space(14.0);
                ui.label("Name");
                ui.add_space(4.0);
                ui.add_enabled(
                    available,
                    egui::TextEdit::singleline(&mut self.name).desired_width(f32::INFINITY),
                );
                if new_experience {
                    ui.add_space(12.0);
                    ui.label("Description");
                    ui.add_space(4.0);
                    ui.add_enabled(
                        available,
                        egui::TextEdit::multiline(&mut self.description)
                            .desired_width(f32::INFINITY)
                            .desired_rows(4),
                    );
                }
            }
            Page::Created { target, .. } => {
                ui.label(RichText::new(&target.place_name).size(18.0).strong());
                ui.label(RichText::new(&target.experience_name).color(theme::TEXT_DIM));
                if !uploading {
                    ui.add_space(12.0);
                    ui.label("The destination was created. Select Retry to upload to this place.");
                }
            }
        }
        if uploading || self.worker.is_some() {
            ui.add_space(12.0);
            ui.horizontal(|ui| {
                ui.spinner();
                ui.label(if uploading {
                    if self.mode == VersionType::Published {
                        "Publishing…"
                    } else {
                        "Saving…"
                    }
                } else if self.creating {
                    "Creating the destination…"
                } else {
                    "Loading from Roblox…"
                });
            });
        }
        action
    }

    fn footer(
        &self,
        ui: &mut egui::Ui,
        blocked: bool,
        signed_in: bool,
        uploading: bool,
    ) -> Option<Action> {
        let action = match &self.page {
            Page::Experiences if self.source != ExperienceSource::Shared => {
                Some(("New Experience", Action::NewExperience))
            }
            Page::Places(experience) => match &self.place_choice {
                Some(PlaceChoice::New) => Some(("Next", Action::NewPlace(experience.clone()))),
                Some(PlaceChoice::Existing(target)) => {
                    Some(("Overwrite", Action::Submit(target.clone())))
                }
                None => None,
            },
            Page::NewExperience if !self.name.trim().is_empty() => Some((
                self.create_label(),
                Action::Create(Request::CreateExperience(
                    self.name.trim().into(),
                    self.description.clone(),
                    match self.source {
                        ExperienceSource::Group(id) => Some(id),
                        _ => None,
                    },
                )),
            )),
            Page::NewPlace(experience) if !self.name.trim().is_empty() => Some((
                self.create_label(),
                Action::Create(Request::CreatePlace(
                    experience.clone(),
                    self.name.trim().into(),
                )),
            )),
            Page::Created { target, .. } => Some(("Retry", Action::Submit(target.clone()))),
            _ => None,
        };
        let disabled_label = match self.page {
            Page::Places(_) => "Overwrite",
            Page::NewExperience | Page::NewPlace(_) => self.create_label(),
            _ => "",
        };
        let label = action.as_ref().map_or(disabled_label, |(label, _)| *label);
        let mut next = None;
        if signed_in && !label.is_empty() {
            let label = if uploading { "Uploading…" } else { label };
            if ui
                .add_enabled(
                    !blocked && self.worker.is_none() && action.is_some(),
                    footer_button(label, true),
                )
                .clicked()
            {
                next = action.map(|(_, action)| action);
            }
        }
        if ui
            .add_enabled(!blocked, footer_button("Cancel", false))
            .clicked()
        {
            next = Some(Action::Close);
        }
        if matches!(
            self.page,
            Page::NewExperience | Page::NewPlace(_) | Page::Created { .. }
        ) && ui
            .add_enabled(!blocked, Button::new("Back").frame(false))
            .clicked()
        {
            next = Some(Action::Back);
        }
        next
    }

    fn create_label(&self) -> &'static str {
        if self.mode == VersionType::Published {
            "Create and Publish"
        } else {
            "Create and Save"
        }
    }
}

fn footer_button(label: &str, primary: bool) -> Button<'_> {
    let text = RichText::new(label).size(14.0);
    let button = if primary {
        Button::new(text.color(Color32::WHITE)).fill(theme::ACCENT)
    } else {
        Button::new(text.color(theme::TEXT))
    };
    button.min_size(egui::vec2(118.0, 32.0)).corner_radius(5)
}

fn tile_grid<T>(
    ui: &mut egui::Ui,
    items: impl IntoIterator<Item = T>,
    mut show_tile: impl FnMut(&mut egui::Ui, f32, T),
) {
    const GAP: f32 = 8.0;
    const MIN_TILE_WIDTH: f32 = 150.0;
    let width = ui.available_width().max(1.0);
    let columns = ((width + GAP) / (MIN_TILE_WIDTH + GAP)).floor().max(1.0) as usize;
    let tile_width = ((width + GAP) / columns as f32 - GAP).floor();
    let mut items = items.into_iter().peekable();
    while items.peek().is_some() {
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = GAP;
            for item in items.by_ref().take(columns) {
                show_tile(ui, tile_width, item);
            }
        });
        ui.add_space(GAP);
    }
}

#[derive(Default)]
struct Tile<'a> {
    name: &'a str,
    subtitle: &'a str,
    image: Option<&'a egui::TextureHandle>,
    selected: Option<bool>,
}

#[derive(Debug, Hash)]
enum TileId {
    Experience(u64),
    Place(u64),
    NewPlace,
}

fn destination_tile(
    ui: &mut egui::Ui,
    id: TileId,
    width: f32,
    enabled: bool,
    tile: Tile<'_>,
) -> egui::Response {
    let (_, rect) = ui.allocate_space(egui::vec2(width, 110.0));
    let response = ui.interact(
        rect,
        egui::Id::new("roblox_destination_tile").with(&id),
        if enabled {
            egui::Sense::click()
        } else {
            egui::Sense::hover()
        },
    );
    if ui.is_rect_visible(rect) {
        let painter = ui.painter_at(rect);
        let selected = tile.selected == Some(true);
        let (fill, outline) = if selected {
            (theme::SELECT_BG, egui::Stroke::new(2.0, theme::SELECT))
        } else if response.hovered() {
            (theme::HOVER_BG, egui::Stroke::new(1.0, theme::ACCENT))
        } else {
            (theme::PANEL_DARK, egui::Stroke::new(1.0, theme::DIVIDER))
        };
        painter.rect_filled(rect, 6.0, fill);
        painter.rect_stroke(rect, 6.0, outline, egui::StrokeKind::Inside);
        if response.has_focus() {
            painter.rect_stroke(
                rect.shrink(3.0),
                4.0,
                ui.visuals().selection.stroke,
                egui::StrokeKind::Inside,
            );
        }
        let thumbnail_size = (width - 16.0).clamp(0.0, 56.0);
        let image_bounds = egui::Rect::from_center_size(
            egui::pos2(rect.center().x, rect.top() + 36.0),
            egui::Vec2::splat(thumbnail_size),
        );
        let placeholder = if matches!(id, TileId::NewPlace) {
            "plus"
        } else {
            "map"
        };
        paint_thumbnail(ui, &painter, tile.image, image_bounds, placeholder);
        let left = rect.left() + 8.0;
        paint_tile_label(
            ui,
            &painter,
            tile.name,
            egui::pos2(left, rect.top() + 78.0),
            width - 16.0,
            14.0,
            theme::TEXT,
        );
        if !tile.subtitle.is_empty() {
            paint_tile_label(
                ui,
                &painter,
                tile.subtitle,
                egui::pos2(left, rect.top() + 96.0),
                width - 16.0,
                12.0,
                theme::TEXT_DIM,
            );
        }
    }
    response.widget_info(|| match tile.selected {
        Some(selected) => {
            egui::WidgetInfo::selected(egui::WidgetType::RadioButton, enabled, selected, tile.name)
        }
        None => egui::WidgetInfo::labeled(egui::WidgetType::Button, enabled, tile.name),
    });
    let response = if enabled {
        response.on_hover_cursor(egui::CursorIcon::PointingHand)
    } else {
        response
    };
    response.on_hover_ui(|ui| {
        ui.label(tile.name);
        if !tile.subtitle.is_empty() {
            ui.label(tile.subtitle);
        }
    })
}

fn paint_thumbnail(
    ui: &egui::Ui,
    painter: &egui::Painter,
    texture: Option<&egui::TextureHandle>,
    bounds: egui::Rect,
    placeholder: &str,
) {
    if let Some(texture) = texture {
        egui::Image::new(texture).paint_at(ui, bounds);
    } else {
        painter.rect_filled(bounds, 3.0, theme::PANEL_ALT);
        rustudio_engine::ti::paint(
            painter,
            bounds.shrink(bounds.width() * 0.28),
            placeholder,
            theme::TEXT_DIM,
        );
    }
}

fn paint_tile_label(
    ui: &egui::Ui,
    painter: &egui::Painter,
    name: &str,
    left_center: egui::Pos2,
    width: f32,
    size: f32,
    color: Color32,
) {
    let mut job = egui::text::LayoutJob::simple_singleline(
        name.to_owned(),
        egui::FontId::proportional(size),
        color,
    );
    job.wrap.max_width = width.max(0.0);
    job.wrap.max_rows = 1;
    let galley = ui.fonts_mut(|fonts| fonts.layout_job(job));
    let origin = left_center - egui::vec2(0.0, galley.size().y * 0.5);
    painter.galley(origin, galley, color);
}
