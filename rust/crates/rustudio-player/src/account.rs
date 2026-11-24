//! Roblox account window (docs/AUTH.md): one sign-in, the session cookie from the
//! login keyring (imported once from ~/.config/rustudio/roblox-cookie).
//! Network and keyring work runs on background threads; results come back
//! over a channel and are polled each frame. The cookie never reaches this
//! module: it stays inside rustudio_account's Session.

use std::collections::{HashMap, HashSet};
use std::sync::mpsc::{channel, Receiver, Sender};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use rustudio_account::{AccountConfig, Client, CookieSource, Profile, PublishKeyStore, SavedAccount, SignedOut};

use rustudio_core::log::Level;
use rustudio_engine::theme;

enum Msg {
    Log(Level, String),
    SignedIn {
        profile: Profile,
        source: CookieSource,
    },
    Head(u64, egui::ColorImage),
    /// A switch or add that failed; the previous session was kept.
    Failed(String),
    SignedOut(String),
    CachedName(Option<String>),
    Avatar(Box<rustudio_engine::avatar::AvatarLook>),
    Done,
}

/// What the session check found.
#[derive(Debug, Clone, PartialEq)]
pub enum SessionState {
    /// Not checked yet (or being checked); the cached name if any.
    Checking(Option<String>),
    SignedIn(String),
    /// "signed out" and why (never the cookie).
    SignedOut(String),
}

pub struct Account {
    pub open: bool,
    profile: Option<Profile>,
    config: Option<AccountConfig>,
    client: Arc<Mutex<Option<Client>>>,
    pub state: SessionState,
    pub source: Option<CookieSource>,
    tx: Sender<Msg>,
    rx: Receiver<Msg>,
    jobs: usize,
    restored: bool,
    avatar: Option<rustudio_engine::avatar::AvatarLook>,
    avatar_requested: bool,
    /// The add-account form: the pasted `.ROBLOSECURITY` value (cleared as
    /// soon as it is handed to the sign-in job).
    pub adding: bool,
    pub cookie_input: String,
    pub add_error: Option<String>,
    pub confirm_remove: Option<u64>,
    /// Who the active session belongs to (account switching).
    user: Option<u64>,
    /// Set when the signed-in account changes; the editor resets lists.
    changed: bool,
    heads: HashMap<u64, egui::TextureHandle>,
    head_images: Vec<(u64, egui::ColorImage)>,
    heads_asked: HashSet<u64>,
    adding_pending: bool,
    /// A Quick Sign-in in progress (Sign in with your browser).
    pub quick: Option<QuickView>,
}

/// What the Quick Sign-in job shares with the panel: the code to show, a
/// line about where it is, and a flag that stops it.
#[derive(Default)]
pub struct QuickView {
    pub code: Arc<Mutex<Option<String>>>,
    pub note: Arc<Mutex<String>>,
    stop: Arc<AtomicBool>,
    /// The browser was sent to the confirm page for this code.
    pub opened: bool,
}

impl QuickView {
    pub fn cancel(&self) {
        self.stop.store(true, Ordering::Relaxed);
    }
}

impl Account {
    pub fn new() -> Self {
        let (tx, rx) = channel();
        Account {
            open: false,
            profile: None,
            config: None,
            client: Arc::new(Mutex::new(None)),
            state: SessionState::Checking(None),
            source: None,
            tx,
            rx,
            jobs: 0,
            restored: false,
            avatar: None,
            avatar_requested: false,
            adding: false,
            cookie_input: String::new(),
            add_error: None,
            confirm_remove: None,
            user: None,
            changed: false,
            heads: HashMap::new(),
            head_images: Vec::new(),
            heads_asked: HashSet::new(),
            adding_pending: false,
            quick: None,
        }
    }

    pub fn busy(&self) -> bool {
        self.jobs > 0
    }

    pub(crate) fn play_copy(&self) -> Self {
        let mut copy = Self::new();
        copy.profile = self.profile.clone();
        copy.client = self.client.clone();
        copy.state = self.state.clone();
        copy.restored = self.restored;
        copy.avatar = self.avatar.clone();
        copy.avatar_requested = self.avatar_requested;
        copy.user = self.user;
        copy
    }

    /// The loaded avatar, if any.
    pub fn avatar(&self) -> Option<&rustudio_engine::avatar::AvatarLook> {
        self.avatar.as_ref()
    }

    /// The signed-in client, for asset downloads (None when signed out).
    pub fn shared_client(&self) -> Option<rustudio_core::assets::SharedClient> {
        self.signed_in().then(|| self.client.clone())
    }

    /// Fetches the avatar in the background (once per sign-in unless asked again).
    pub fn load_avatar(&mut self) {
        if !self.signed_in() {
            return;
        }
        self.avatar_requested = true;
        self.spawn("Loading the avatar for Play", move |client, tx| {
            let txl = tx.clone();
            let mut log = |s: String| {
                let _ = txl.send(Msg::Log(Level::Info, s));
            };
            let snapshot = Arc::new(Mutex::new(Some(client_snapshot(client)?)));
            match rustudio_engine::avatar::load(&snapshot, &mut log) {
                Ok(look) => {
                    let _ = tx.send(Msg::Avatar(Box::new(look)));
                    Ok(())
                }
                Err(e) => Err(rustudio_account::Error::new(e)),
            }
        });
    }

    /// Runs `f` with the signed-in client (self-test checks).
    pub fn with_client<R>(&self, f: impl FnOnce(&Client) -> R) -> Option<R> {
        let g = self.client.lock().ok()?;
        g.as_ref().map(f)
    }

    /// Forgets the session for this run only (self-test); the keyring keeps it.
    pub fn drop_session(&mut self) {
        if let Ok(mut g) = self.client.lock() {
            *g = None;
        }
        self.state = SessionState::SignedOut("signed out (session closed for this run)".into());
    }

    /// Starts a session check now (self-test).
    pub fn check_now(&mut self) {
        self.restored = true;
        self.check_session();
    }

    /// The signed-in account's user id (Play's local player takes it, as in
    /// Studio).
    pub fn user_id(&self) -> Option<u64> {
        if !self.signed_in() {
            return None;
        }
        self.user
    }

    /// The signed-in account's username and display name (Play's local
    /// player takes them, as in signed-in Studio; offline it is "Player").
    pub fn profile(&self) -> Option<&Profile> {
        self.profile.as_ref()
    }

    pub fn names(&self) -> Option<(String, String)> {
        if !self.signed_in() {
            return None;
        }
        self.profile.as_ref().map(|p| {
            let disp = if p.display_name.is_empty() { p.name.clone() } else { p.display_name.clone() };
            (p.name.clone(), disp)
        })
    }

    /// Before Play: lets a session check still running finish, then the
    /// avatar load, up to `max` (Studio only plays signed in, and its first
    /// character already wears the account's avatar). Returns the log lines.
    /// `avatar`: wait for the avatar too (a character spawns at once);
    /// otherwise the session's identity is enough and the avatar reaches the
    /// running Play when it arrives (Editor::play_tick).
    pub fn settle_for_play(&mut self, max: std::time::Duration, avatar: bool) -> Vec<(Level, String)> {
        let t0 = std::time::Instant::now();
        let mut logs = Vec::new();
        loop {
            logs.extend(self.poll());
            let checking = matches!(self.state, SessionState::Checking(_)) && self.restored;
            let avatar_pending = avatar && self.signed_in() && self.avatar.is_none() && self.busy();
            if !(checking || avatar_pending) || t0.elapsed() >= max {
                return logs;
            }
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
    }

    /// Signed in means Roblox accepted the session this run.
    pub fn signed_in(&self) -> bool {
        matches!(self.state, SessionState::SignedIn(_))
    }

    pub fn status_short(&self) -> String {
        match &self.state {
            SessionState::SignedIn(w) => format!("Roblox: {w}"),
            SessionState::Checking(Some(w)) => format!("Roblox: checking {w}…"),
            SessionState::Checking(None) => "Roblox: checking…".into(),
            SessionState::SignedOut(_) => "Roblox: signed out".into(),
        }
    }

    /// Drains worker messages; returns log lines for the Output panel.
    pub fn poll(&mut self) -> Vec<(Level, String)> {
        let mut logs = Vec::new();
        while let Ok(m) = self.rx.try_recv() {
            match m {
                Msg::Log(l, t) => logs.push((l, t)),
                Msg::SignedIn {
                    profile,
                    source,
                } => {
                    let who = profile.display();
                    logs.push((
                        Level::Info,
                        format!(
                            "Signed in as {who} (session from {})",
                            match source {
                                CookieSource::Keyring => "the login keyring".to_string(),
                                CookieSource::File { imported: true } =>
                                    "the cookie file, now in the login keyring".to_string(),
                                CookieSource::File { imported: false } =>
                                    "the cookie file (the keyring couldn't store it)".to_string(),
                            }
                        ),
                    ));
                    if self.user != Some(profile.id) {
                        self.changed = self.user.is_some() || self.changed;
                        self.user = Some(profile.id);
                        self.avatar = None;
                        self.avatar_requested = false;
                    }
                    let cfg = self.config();
                    cfg.remember(&profile);
                    cfg.signed_out = false;
                    let _ = self.config().save();
                    if std::mem::take(&mut self.adding_pending) && self.add_error.is_none() {
                        self.adding = false;
                    }
                    self.profile = Some(profile);
                    self.state = SessionState::SignedIn(who);
                    self.source = Some(source);
                }
                Msg::Head(id, img) => self.head_images.push((id, img)),
                Msg::Failed(e) => {
                    logs.push((Level::Error, format!("Roblox: {e}")));
                    self.add_error = Some(e);
                }
                Msg::SignedOut(why) => {
                    logs.push((Level::Info, format!("Roblox: {why}")));
                    if self.user.take().is_some() {
                        self.changed = true;
                    }
                    self.adding_pending = false;
                    self.state = SessionState::SignedOut(why);
                    self.source = None;
                    self.profile = None;
                    self.avatar = None;
                    self.avatar_requested = false;
                }
                Msg::CachedName(name) => {
                    if matches!(self.state, SessionState::Checking(_)) {
                        self.state = SessionState::Checking(name);
                    }
                }
                Msg::Avatar(look) => {
                    logs.push((
                        Level::Info,
                        format!(
                            "Avatar ready for Play (user {}, {}): the next character uses it",
                            look.user_id,
                            if look.r15 { "R15" } else { "R6" }
                        ),
                    ));
                    if self.user == Some(look.user_id) {
                        self.avatar = Some(*look);
                    }
                }
                Msg::Done => {
                    self.jobs = self.jobs.saturating_sub(1);
                    if self.jobs == 0 {
                        self.quick = None;
                    }
                }
            }
        }
        if self.signed_in() && !self.avatar_requested {
            self.load_avatar();
        }
        logs
    }

    pub fn config(&mut self) -> &mut AccountConfig {
        if self.config.is_none() {
            self.config = Some(match AccountConfig::load() {
                Ok(c) => c,
                Err(e) => {
                    let _ = self
                        .tx
                        .send(Msg::Log(Level::Error, format!("Account config: {e}")));
                    AccountConfig::default()
                }
            });
        }
        self.config.as_mut().expect("loaded")
    }

    /// Runs `f` on a worker thread with access to the shared client.
    fn spawn(
        &mut self,
        label: &str,
        f: impl FnOnce(&Arc<Mutex<Option<Client>>>, &Sender<Msg>) -> rustudio_account::Result<()>
            + Send
            + 'static,
    ) {
        self.jobs += 1;
        let tx = self.tx.clone();
        let client = self.client.clone();
        let label = label.to_string();
        let _ = tx.send(Msg::Log(Level::Info, format!("{label}…")));
        std::thread::spawn(move || {
            if let Err(e) = f(&client, &tx) {
                let _ = tx.send(Msg::Log(Level::Error, format!("{label} failed: {e}")));
            }
            let _ = tx.send(Msg::Done);
        });
    }

    /// Checks the session once at start (and on "Check again").
    pub fn restore(&mut self) {
        if self.restored {
            return;
        }
        self.restored = true;
        self.check_session();
    }

    fn check_session(&mut self) {
        self.state = SessionState::Checking(None);
        let tx = self.tx.clone();
        self.connect("Checking the Roblox session", false, move |cfg| {
            let cached = Client::cached_profile().map(|c| c.profile.display());
            let _ = tx.send(Msg::CachedName(cached));
            // Tokens an older (OAuth) rustudio left in the keyring go.
            PublishKeyStore::forget_oauth_tokens();
            Client::open(cfg)
        });
    }

    /// Runs a sign-in style job: on success its client becomes the session
    /// (kept for switching), else the editor is signed out.
    /// With `keep`, a failure reports itself and re-opens the active
    /// session (the keyring's active slot only changes on success).
    fn connect(
        &mut self,
        label: &str,
        keep: bool,
        open: impl FnOnce(AccountConfig) -> Result<Client, SignedOut> + Send + 'static,
    ) {
        let cfg = self.config().clone();
        rustudio_core::assets::session_checking(true);
        self.spawn(label, move |client, tx| {
            // Downloads waiting on the check (LoadAsset at Play start) go on
            // as soon as it ends, without waiting for the next frame.
            let mut r = open(cfg.clone());
            if keep {
                if let Err(e) = &r {
                    let _ = tx.send(Msg::Failed(e.describe()));
                    r = Client::open(cfg);
                }
            }
            match r {
                Ok(c) => {
                    let _ = c.remember_account();
                    let profile = c.session.profile.clone();
                    let source = c.source;
                    *client.lock().unwrap() = Some(c);
                    rustudio_core::assets::session_ready(Some(client.clone()));
                    let _ = tx.send(Msg::SignedIn {
                        profile,
                        source,
                    });
                }
                Err(e) => {
                    *client.lock().unwrap() = None;
                    rustudio_core::assets::session_ready(None);
                    let _ = tx.send(Msg::SignedOut(e.describe()));
                }
            }
            Ok(())
        });
    }

    /// Accounts signed in on this computer before, most recent first.
    pub fn accounts(&mut self) -> Vec<SavedAccount> {
        self.config().accounts.clone()
    }

    /// The active account's user id (also while a check is running).
    pub fn active_id(&self) -> Option<u64> {
        self.user
    }

    /// True once after the signed-in account changed (switch, sign-out).
    pub fn take_changed(&mut self) -> bool {
        std::mem::take(&mut self.changed)
    }

    /// Makes a saved account the active session.
    pub fn switch_to(&mut self, id: u64) {
        if self.busy() || (self.user == Some(id) && self.signed_in()) {
            return;
        }
        let name = self
            .config()
            .accounts
            .iter()
            .find(|a| a.id == id)
            .map(|a| format!("@{}", a.name))
            .unwrap_or_else(|| format!("user {id}"));
        self.state = SessionState::Checking(Some(name.clone()));
        self.add_error = None;
        self.connect(&format!("Switching to {name}"), true, move |cfg| {
            Client::switch_to(cfg, id)
        });
    }

    /// Signs in with the pasted session value (Add account).
    pub fn add_account(&mut self) {
        let raw = std::mem::take(&mut self.cookie_input);
        if raw.trim().is_empty() {
            return;
        }
        self.add_error = None;
        self.adding_pending = true;
        self.state = SessionState::Checking(None);
        self.connect("Signing in", true, move |cfg| {
            let c = Client::sign_in_with(cfg, &raw)?;
            Ok(c)
        });
    }

    /// Forgets a saved account that isn't the active one.
    pub fn remove_account(&mut self, id: u64) {
        self.config().accounts.retain(|a| a.id != id);
        let _ = self.config().save();
        self.heads.remove(&id);
        std::thread::spawn(move || Client::forget_account(id));
    }

    /// Sign in after a sign-out: the import file may be read again.
    pub fn sign_in(&mut self) {
        self.config().signed_out = false;
        let _ = self.config().save();
        self.check_session();
    }

    /// Sign in with the default browser (Roblox's Quick Sign-in): shows a
    /// code, sends the browser to Roblox's confirm page, and once the person
    /// confirms there, the session it yields is added like a pasted one.
    pub fn browser_sign_in(&mut self) {
        use rustudio_account::quick_login::{QuickLogin, QuickStatus};
        self.add_error = None;
        self.adding = true;
        self.adding_pending = true;
        let view = QuickView::default();
        let (code, note, stop) = (view.code.clone(), view.note.clone(), view.stop.clone());
        *note.lock().unwrap() = "Getting a code from Roblox…".into();
        self.quick = Some(view);
        self.connect("Signing in with the browser", true, move |cfg| {
            let fail = |m: String| SignedOut::Rejected(m);
            let q = QuickLogin::start().map_err(|e| fail(e.to_string()))?;
            *code.lock().unwrap() = Some(q.code.clone());
            *note.lock().unwrap() = "Waiting for you to enter the code…".into();
            let mut why = "the sign-in code expired; try again".to_string();
            let got = q
                .wait(&|| stop.load(Ordering::Relaxed), &mut |st| {
                    let line = match st {
                        QuickStatus::UserLinked(Some(name)) => format!("Confirm as @{name} in your browser…"),
                        QuickStatus::UserLinked(None) => "Confirm in your browser…".into(),
                        QuickStatus::Validated => "Confirmed, signing in…".into(),
                        QuickStatus::Cancelled => {
                            why = "the sign-in was turned down in the browser".into();
                            return;
                        }
                        _ => return,
                    };
                    *note.lock().unwrap() = line;
                })
                .map_err(|e| fail(e.to_string()))?;
            match got {
                Some(raw) => Client::sign_in_with(cfg, &raw),
                None if stop.load(Ordering::Relaxed) => Err(fail("sign-in cancelled".into())),
                None => Err(fail(why)),
            }
        });
    }

    /// Signs the active account out and forgets it on this computer; other
    /// saved accounts stay for switching.
    pub fn sign_out(&mut self) {
        let id = self.user;
        if let Some(id) = id {
            self.config().accounts.retain(|a| a.id != id);
        }
        self.config().signed_out = true;
        let cfg = self.config().clone();
        self.spawn("Signing out", move |client, tx| {
            client.lock().unwrap().take();
            rustudio_core::assets::session_ready(None);
            if let Some(id) = id {
                Client::forget_account(id);
            }
            Client::sign_out(cfg)?;
            let _ = tx.send(Msg::SignedOut("signed out".into()));
            Ok(())
        });
    }

    /// Fetches headshots for the saved accounts not asked for yet, and turns
    /// fetched ones into textures.
    pub fn heads(&mut self, ctx: &egui::Context) {
        for (id, img) in std::mem::take(&mut self.head_images) {
            let t = ctx.load_texture(format!("account-head-{id}"), img, egui::TextureOptions::LINEAR);
            self.heads.insert(id, t);
        }
        let user = self.user;
        let mut ids: Vec<u64> = self.config().accounts.iter().map(|a| a.id).chain(user).collect();
        ids.retain(|i| !self.heads_asked.contains(i));
        ids.dedup();
        if ids.is_empty() {
            return;
        }
        self.heads_asked.extend(ids.iter().copied());
        let tx = self.tx.clone();
        std::thread::spawn(move || {
            let Ok(list) = rustudio_account::headshots(&ids) else { return };
            for (id, bytes) in list {
                let Ok(img) = image::load_from_memory(&bytes) else { continue };
                let img = img.thumbnail(96, 96).to_rgba8();
                let size = [img.width() as usize, img.height() as usize];
                let _ = tx.send(Msg::Head(id, egui::ColorImage::from_rgba_unmultiplied(size, img.as_raw())));
            }
        });
    }

    /// The headshot (or an initial on a plate) painted into `r`.
    pub fn paint_head(&self, ui: &egui::Ui, r: egui::Rect, id: u64, name: &str) {
        let size = r.width();
        ui.painter().circle_filled(r.center(), size * 0.5, theme::PANEL_ALT);
        if let Some(t) = self.heads.get(&id) {
            egui::Image::new(t).corner_radius(size * 0.5).paint_at(ui, r);
        } else {
            let initial = name.chars().next().map(|c| c.to_ascii_uppercase().to_string()).unwrap_or_default();
            ui.painter().text(r.center(), egui::Align2::CENTER_CENTER, initial, egui::FontId::proportional(size * 0.45), theme::TEXT_DIM);
        }
    }

    /// The headshot (or an initial on a plate) at `size`.
    pub fn head_widget(&self, ui: &mut egui::Ui, id: u64, name: &str, size: f32) {
        if let Some(t) = self.heads.get(&id) {
            ui.add(
                egui::Image::new(t)
                    .fit_to_exact_size(egui::vec2(size, size))
                    .corner_radius(size * 0.5)
                    .bg_fill(theme::PANEL_ALT),
            );
        } else {
            let (r, _) = ui.allocate_exact_size(egui::vec2(size, size), egui::Sense::hover());
            ui.painter().circle_filled(r.center(), size * 0.5, theme::PANEL_ALT);
            let initial = name.chars().next().map(|c| c.to_ascii_uppercase().to_string()).unwrap_or_default();
            ui.painter().text(
                r.center(),
                egui::Align2::CENTER_CENTER,
                initial,
                egui::FontId::proportional(size * 0.45),
                theme::TEXT_DIM,
            );
        }
    }


}

/// Copy the session under a short lock before doing network work. Downloader
/// copies share cookie rotations with the signed-in session.
pub fn client_snapshot(shared: &rustudio_core::assets::SharedClient) -> rustudio_account::Result<Client> {
    let guard = shared.lock().map_err(|_| rustudio_account::Error::new("Account session is unavailable"))?;
    let client = guard.as_ref().ok_or_else(|| rustudio_account::Error::new("Sign in to Roblox"))?;
    Ok(Client {
        config: client.config.clone(),
        session: client.session.downloader(),
        source: client.source,
    })
}
