//! The Roblox session: the `.ROBLOSECURITY` cookie is the one credential
//! (docs/AUTH.md). Signed in means `users.roblox.com/v1/users/authenticated`
//! accepted it; anything else is "signed out".
//!
//! Where the cookie comes from: the login keyring (the `keyring` crate,
//! service `rustudio`, user `roblox-cookie`), else `~/.config/rustudio/roblox-cookie`
//! (mode 600, one line, written by hand), which is then imported into the
//! keyring. The value lives only in memory and in those two places: it is
//! never formatted into a message, log line, error or file. [`Cookie`]'s
//! Debug and Display print `<redacted>`.

use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use ureq::ResponseExt;
use sha2::{Digest, Sha256};

use crate::api::AssetDownloadError;
use crate::{Error, Result};

pub const AUTHENTICATED_URL: &str = "https://users.roblox.com/v1/users/authenticated";
pub const MY_AVATAR_URL: &str = "https://avatar.roblox.com/v1/avatar";
pub const MY_UNIVERSES_URL: &str = "https://develop.roblox.com/v1/user/universes";
/// The Roblox place the editor has open (0: none), sent with asset requests.
pub static PLACE_CONTEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// Sets the place context for asset delivery (see `download_asset`).
pub fn set_place_context(place_id: u64) {
    PLACE_CONTEXT.store(place_id, std::sync::atomic::Ordering::Relaxed);
}

pub const ASSET_V2_URL: &str = "https://assetdelivery.roblox.com/v2/assetId";

const KEYRING_SERVICE: &str = "rustudio";
const KEYRING_KEY: &str = "roblox-cookie";

// Serialize rotation's compare-and-save with account switches and sign-out.
static SESSION_STORAGE: Mutex<()> = Mutex::new(());

/// Retries of a CSRF-refused request (Studio: counter at 
///).
pub const CSRF_MAX_RETRIES: usize = 3;

/// Cookie names Studio keeps as secrets:
/// none of them is ever shown.
pub const SECRET_COOKIES: [&str; 3] = [".ROBLOSECURITY", ".RBXIDCHECK", ".RBXID"];

/// Shown for any cookie Roblox doesn't accept (never the value itself).
pub const SIGNED_OUT: &str = "signed out";

/// The session cookie. Never printed.
#[derive(Clone)]
pub struct Cookie(String);

impl std::fmt::Debug for Cookie {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("<redacted>")
    }
}

impl std::fmt::Display for Cookie {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("<redacted>")
    }
}

impl Cookie {
    /// From the raw line (with or without the `.ROBLOSECURITY=` prefix).
    pub fn new(raw: &str) -> Option<Cookie> {
        let v = raw.lines().next()?.trim();
        let v = v.strip_prefix(".ROBLOSECURITY=").unwrap_or(v).trim();
        (!v.is_empty()).then(|| Cookie(v.to_string()))
    }

    pub(crate) fn header(&self) -> String {
        format!(".ROBLOSECURITY={}", self.0)
    }

    /// A one-way key for caches tied to this cookie (a cookie change
    /// invalidates them). Not reversible to the value.
    pub fn cache_key(&self) -> String {
        let mut h = Sha256::new();
        h.update(b"rustudio-session-cache-v1\0");
        h.update(self.0.as_bytes());
        h.finalize().iter().map(|b| format!("{b:02x}")).collect()
    }

    /// Whether `text` contains the cookie (to keep it out of anything shown).
    pub fn appears_in(&self, text: &str) -> bool {
        self.0.len() >= 8 && text.contains(&self.0)
    }
}

/// `~/.config/rustudio/roblox-cookie`.
pub fn cookie_file() -> PathBuf {
    if let Ok(p) = std::env::var("RUSTUDIO_COOKIE_FILE") {
        return PathBuf::from(p);
    }
    dirs::config_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("rustudio")
        .join("roblox-cookie")
}

/// Where a loaded cookie came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CookieSource {
    Keyring,
    /// Read from the import file (and copied into the keyring if it could be).
    File {
        imported: bool,
    },
}

fn keyring_disabled() -> bool {
    std::env::var_os("RUSTUDIO_NO_KEYRING").is_some()
}

/// Sessions older builds stored with `secret-tool` (attributes `service
/// rustudio key roblox-cookie`, plus `user <id>` for saved accounts). The
/// keyring crate looks items up by `service` and `username`, so it doesn't
/// see them. One is read once, moved into the keyring and then removed.
#[cfg(all(unix, not(target_os = "macos"), not(test)))]
mod legacy {
    use super::Cookie;
    use std::process::{Command, Stdio};

    pub fn lookup(attrs: &[&str]) -> Option<Cookie> {
        let out = Command::new("secret-tool")
            .arg("lookup")
            .args(attrs)
            .stdin(Stdio::null())
            .stderr(Stdio::null())
            .output()
            .ok()?;
        if !out.status.success() {
            return None;
        }
        Cookie::new(&String::from_utf8_lossy(&out.stdout))
    }

    pub fn clear(attrs: &[&str]) {
        let _ = Command::new("secret-tool")
            .arg("clear")
            .args(attrs)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
    }
}

/// Nothing was ever stored with `secret-tool` here.
#[cfg(not(any(all(unix, not(target_os = "macos")), test)))]
mod legacy {
    use super::Cookie;

    pub fn lookup(_attrs: &[&str]) -> Option<Cookie> {
        None
    }

    pub fn clear(_attrs: &[&str]) {}
}

/// Tests stand in for `secret-tool` with a map (per test thread).
#[cfg(test)]
mod legacy {
    use super::Cookie;
    use std::cell::RefCell;
    use std::collections::HashMap;

    thread_local! {
        pub static ITEMS: RefCell<HashMap<String, String>> = Default::default();
    }

    pub fn lookup(attrs: &[&str]) -> Option<Cookie> {
        ITEMS.with(|m| {
            m.borrow()
                .get(&attrs.join(" "))
                .and_then(|v| Cookie::new(v))
        })
    }

    pub fn clear(attrs: &[&str]) {
        ITEMS.with(|m| m.borrow_mut().remove(&attrs.join(" ")));
    }
}

/// Reads a session from the keyring (entry `user` under service
/// `rustudio`), else from where `secret-tool` kept it before.
fn secret_lookup(user: &str, legacy_attrs: &[&str]) -> Option<Cookie> {
    if keyring_disabled() {
        return None;
    }
    if let Ok(Some(v)) = crate::store::load(user) {
        return Cookie::new(&String::from_utf8_lossy(&v));
    }
    let c = legacy::lookup(legacy_attrs)?;
    if crate::store::save(user, c.0.as_bytes()).is_ok() {
        legacy::clear(legacy_attrs);
    }
    Some(c)
}

fn secret_store(user: &str, c: &Cookie) -> Result<()> {
    if keyring_disabled() {
        return Err(Error::new("keyring disabled (RUSTUDIO_NO_KEYRING)"));
    }
    crate::store::save(user, c.0.as_bytes())
        .map_err(|_| Error::new("the login keyring refused to store the session"))
}

/// Removes the keyring entry and any copy `secret-tool` kept, so an old one
/// can't come back after sign-out.
fn secret_clear(user: &str, legacy_attrs: &[&str]) {
    if keyring_disabled() {
        return;
    }
    let _ = crate::store::delete(user);
    legacy::clear(legacy_attrs);
}

const LEGACY_COOKIE: [&str; 4] = ["service", KEYRING_SERVICE, "key", KEYRING_KEY];

/// The keyring copy (service `rustudio`, user `roblox-cookie`).
pub fn keyring_cookie() -> Option<Cookie> {
    secret_lookup(KEYRING_KEY, &LEGACY_COOKIE)
}

/// Stores the cookie in the keyring as the active session.
pub fn keyring_store(c: &Cookie) -> Result<()> {
    let _storage = SESSION_STORAGE.lock().unwrap_or_else(|e| e.into_inner());
    secret_store(KEYRING_KEY, c)
}

/// Removes the keyring copy (sign out). The import file is the person's and
/// is left alone.
pub fn keyring_clear() -> Result<()> {
    let _storage = SESSION_STORAGE.lock().unwrap_or_else(|e| e.into_inner());
    secret_clear(KEYRING_KEY, &LEGACY_COOKIE);
    Ok(())
}

/// Saved accounts (account switching): each signed-in account's session is
/// kept under user `roblox-account-<id>`, next to the active one. Switching
/// copies a saved session into the active slot.
const ACCOUNT_KEY: &str = "roblox-account";

fn account_user(user_id: u64) -> String {
    format!("{ACCOUNT_KEY}-{user_id}")
}

pub fn account_cookie(user_id: u64) -> Option<Cookie> {
    let id = user_id.to_string();
    secret_lookup(
        &account_user(user_id),
        &["service", KEYRING_SERVICE, "key", ACCOUNT_KEY, "user", &id],
    )
}

pub fn account_store(user_id: u64, c: &Cookie) -> Result<()> {
    let _storage = SESSION_STORAGE.lock().unwrap_or_else(|e| e.into_inner());
    secret_store(&account_user(user_id), c)
}

pub fn account_forget(user_id: u64) {
    let _storage = SESSION_STORAGE.lock().unwrap_or_else(|e| e.into_inner());
    let id = user_id.to_string();
    secret_clear(
        &account_user(user_id),
        &["service", KEYRING_SERVICE, "key", ACCOUNT_KEY, "user", &id],
    );
}

/// Next to the cookie file: the one-way key of the file's cookie once it has
/// been imported, so a file left in place doesn't take the active slot back
/// after a switch. A new value pasted into the file still wins.
fn file_marker(file: &Path) -> PathBuf {
    let mut p = file.as_os_str().to_owned();
    p.push(".imported");
    PathBuf::from(p)
}

fn file_seen(file: &Path, c: &Cookie) -> bool {
    std::fs::read_to_string(file_marker(file)).is_ok_and(|s| s.trim() == c.cache_key())
}

fn mark_seen(file: &Path, c: &Cookie) {
    let _ = std::fs::write(file_marker(file), c.cache_key());
}

/// Marks whatever the cookie file holds now as already imported (before a
/// switch, so the file doesn't switch back).
pub fn mark_file_seen() {
    if let Ok(Some(f)) = file_cookie() {
        mark_seen(&cookie_file(), &f);
    }
}

/// The import file's cookie (only if it is not readable by others).
pub fn file_cookie() -> Result<Option<Cookie>> {
    read_cookie_file(&cookie_file())
}

/// A cookie file named for this run, never imported: a missing or empty
/// file is an error, not a fall-back to the keyring's account.
fn explicit_cookie(path: &Path) -> Result<Option<(Cookie, CookieSource)>> {
    match read_cookie_file(path)? {
        Some(c) => Ok(Some((c, CookieSource::File { imported: false }))),
        None => Err(Error(format!("{} holds no Roblox session", path.display()))),
    }
}

fn read_cookie_file(path: &Path) -> Result<Option<Cookie>> {
    let Ok(meta) = std::fs::metadata(path) else {
        return Ok(None);
    };
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if meta.permissions().mode() & 0o077 != 0 {
            return Err(Error(format!(
                "{} is readable by other users; chmod 600 it",
                path.display()
            )));
        }
    }
    #[cfg(not(unix))]
    let _ = meta;
    let mut s = String::new();
    std::fs::File::open(&path)?.read_to_string(&mut s)?;
    Ok(Cookie::new(&s))
}

/// The cookie to use: the keyring, else the import file (copied into the
/// keyring). `allow_file` false skips the file (after an explicit sign-out).
/// A cookie file that differs from the keyring copy wins once: it is the
/// person pasting a fresh session after the old one expired.
///
/// A file named by `RUSTUDIO_COOKIE_FILE` is a choice for this one run: it
/// is used as-is and never imported or marked seen. Imported, a second run
/// with the same file lost to whatever the keyring held (the file was seen),
/// and silently signed in as the keyring's account instead.
pub fn load_cookie(allow_file: bool) -> Result<Option<(Cookie, CookieSource)>> {
    load_cookie_from(allow_file, &cookie_file(), std::env::var_os("RUSTUDIO_COOKIE_FILE").is_some())
}

/// `load_cookie` with the cookie file at `file`, `explicit` when the run
/// named it.
fn load_cookie_from(allow_file: bool, file: &Path, explicit: bool) -> Result<Option<(Cookie, CookieSource)>> {
    if explicit {
        return explicit_cookie(file);
    }
    let keyring = keyring_cookie();
    if !allow_file {
        return Ok(keyring.map(|c| (c, CookieSource::Keyring)));
    }
    let from_file = match read_cookie_file(file) {
        Ok(f) => f,
        Err(e) if keyring.is_none() => return Err(e),
        Err(_) => None,
    };
    match (keyring, from_file) {
        (Some(k), Some(f)) if k.0 == f.0 || file_seen(file, &f) => Ok(Some((k, CookieSource::Keyring))),
        (_, Some(f)) => {
            let imported = keyring_store(&f).is_ok();
            mark_seen(file, &f);
            Ok(Some((f, CookieSource::File { imported })))
        }
        (Some(k), None) => Ok(Some((k, CookieSource::Keyring))),
        (None, None) => Ok(None),
    }
}

/// Who the session belongs to (users.roblox.com/v1/users/authenticated).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Profile {
    pub id: u64,
    pub name: String,
    #[serde(rename = "displayName")]
    pub display_name: String,
}

impl Profile {
    pub fn display(&self) -> String {
        if self.display_name.is_empty() || self.display_name == self.name {
            format!("@{}", self.name)
        } else {
            format!("{} (@{})", self.display_name, self.name)
        }
    }
}

/// Display cache, keyed to the cookie: `~/.cache/rustudio/session.json`
/// holds the profile and avatar JSON of the last session and the cookie's
/// one-way key; any other cookie misses it. It is never what decides
/// "signed in".
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProfileCache {
    pub key: String,
    pub profile: Profile,
    #[serde(default)]
    pub avatar: Option<serde_json::Value>,
}

pub fn cache_path() -> PathBuf {
    if let Ok(p) = std::env::var("RUSTUDIO_SESSION_CACHE") {
        return PathBuf::from(p);
    }
    let base = std::env::var("RUSTUDIO_CACHE_DIR")
        .ok()
        .filter(|s| !s.is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            dirs::cache_dir()
                .unwrap_or_else(|| PathBuf::from("."))
                .join("rustudio")
        });
    base.join("session.json")
}

impl ProfileCache {
    /// The cache entry for this cookie, if it is the one cached.
    pub fn load(c: &Cookie) -> Option<ProfileCache> {
        let text = std::fs::read_to_string(cache_path()).ok()?;
        let p: ProfileCache = serde_json::from_str(&text).ok()?;
        (p.key == c.cache_key()).then_some(p)
    }

    pub fn save(&self) -> Result<()> {
        let path = cache_path();
        if let Some(d) = path.parent() {
            std::fs::create_dir_all(d)?;
        }
        std::fs::write(path, serde_json::to_string_pretty(self)?)?;
        Ok(())
    }

    pub fn clear() {
        let _ = std::fs::remove_file(cache_path());
    }
}

/// HTTP agent for cookie calls: a browser-like user agent (Cloudflare in
/// front of these hosts turns other clients away) and statuses as values.
pub fn session_agent() -> ureq::Agent {
    session_agent_with_timeout(None)
}

fn session_agent_with_timeout(timeout: Option<std::time::Duration>) -> ureq::Agent {
    use std::time::Duration;
    // Redirects are reported, not followed: a cookie call that is sent to
    // a login page is a refusal, and a POST must not be replayed elsewhere.
    //
    // No limit on a whole call (an upload or a big download takes what it
    // takes), but a limit on every step of one so dead connections fail and
    // retry instead of hanging indefinitely.
    let config = ureq::Agent::config_builder()
        .http_status_as_error(false)
        .max_redirects(0)
        .timeout_global(timeout)
        .timeout_resolve(Some(Duration::from_secs(15)))
        .timeout_connect(Some(Duration::from_secs(10)))
        .timeout_send_request(Some(Duration::from_secs(30)))
        .timeout_recv_response(Some(Duration::from_secs(60)))
        .timeout_per_read(Some(Duration::from_secs(60)))
        .timeout_per_write(Some(Duration::from_secs(60)))
        // Asset downloads run a dozen at a time on a shared copy of the
        // session (`downloader`); with ureq's default of 3 idle
        // connections per host the rest reconnected (TLS and all) per asset.
        .max_idle_connections_per_host(8)
        .max_idle_connections(24)
        .user_agent(concat!(
            "Mozilla/5.0 (X11; Linux x86_64) rustudio/",
            env!("CARGO_PKG_VERSION")
        ))
        .build();
    ureq::Agent::from(config)
}

/// A validated session.
pub struct Session {
    /// The cookie as it stands now. Roblox rotates `.ROBLOSECURITY` through
    /// `Set-Cookie` replies (see [`Session::apply_rotation`]); the value is
    /// shared with every [`Session::downloader`] copy, so one rotation
    /// reaches a fan-out of downloads.
    cookie: Arc<Mutex<Cookie>>,
    agent: ureq::Agent,
    /// X-CSRF-TOKEN from the last 403, reused until the session rotates it.
    csrf: Arc<Mutex<Option<String>>>,
    /// Store a rotation where `load_cookie` finds it. Off for
    /// [`Session::unchecked`]: a mock server's `Set-Cookie` must never
    /// replace the person's stored session.
    persist: bool,
    pub profile: Profile,
}

impl std::fmt::Debug for Session {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Session")
            .field("profile", &self.profile)
            .finish_non_exhaustive()
    }
}

/// A reply body read as text (bounded), with its status.
fn text_of(mut r: ureq::http::Response<ureq::Body>) -> Result<(u16, String)> {
    let status = r.status().as_u16();
    let mut s = String::new();
    r.body_mut()
        .with_config()
        .limit(16 << 20)
        .reader()
        .read_to_string(&mut s)?;
    Ok((status, s))
}

/// The `.ROBLOSECURITY` value a reply rotated out (`Set-Cookie`), if any.
fn trusted_rotation_origin(uri: &ureq::http::Uri) -> bool {
    uri.scheme_str() == Some("https")
        && matches!(uri.port_u16(), None | Some(443))
        && matches!(uri.host(), Some("users.roblox.com" | "auth.roblox.com"
            | "avatar.roblox.com" | "develop.roblox.com" | "data.roblox.com"
            | "assetdelivery.roblox.com" | "apis.roblox.com" | "www.roblox.com"))
}

fn rotated_cookie(r: &ureq::http::Response<ureq::Body>) -> Option<Cookie> {
    // ureq records the URI of the actual response, including after redirects.
    // Cookie attributes cannot establish the responding server's identity.
    if !trusted_rotation_origin(r.get_uri()) {
        return None;
    }
    rotation_value(r.headers())
}

fn rotation_value(headers: &ureq::http::HeaderMap) -> Option<Cookie> {
    headers
        .get_all("set-cookie")
        .iter()
        .filter_map(|v| v.to_str().ok())
        .find_map(|c| {
            let (name, rest) = c.trim().split_once('=')?;
            if name.trim() != ".ROBLOSECURITY" {
                return None;
            }
            Cookie::new(rest.split(';').next()?.trim())
        })
}

/// Roblox rotates `.ROBLOSECURITY` through `Set-Cookie` (docs/AUTH.md). A
/// rotation is stored where the next `load_cookie` finds it: the keyring's
/// active slot, the saved-account slot when this account has one, and the
/// import file's marker — so the file's older copy doesn't take the slot
/// back on the next start.
fn persist_rotation(new: &Cookie, old: &Cookie, user_id: u64) {
    let _storage = SESSION_STORAGE.lock().unwrap_or_else(|e| e.into_inner());
    let active = keyring_cookie().is_some_and(|c| c.0 == old.0);
    let saved_active = active && secret_store(KEYRING_KEY, new).is_ok();
    // An old downloader must not restore a forgotten or replaced account.
    if account_cookie(user_id).is_some_and(|c| c.0 == old.0) {
        let _ = secret_store(&account_user(user_id), new);
    }
    // Do not suppress the file's only viable credential if the keyring is
    // unavailable. A missing active slot also means sign-out, so rotations
    // never create one; load_cookie imports files before validation.
    if !saved_active {
        return;
    }
    if let Ok(Some(f)) = file_cookie() {
        if f.0 == old.0 {
            mark_seen(&cookie_file(), &f);
        }
    }
}

impl Session {
    /// Asks Roblox who the cookie belongs to. Err(SIGNED_OUT…) when it isn't
    /// accepted; the cookie never appears in the error.
    pub fn validate(cookie: Cookie) -> Result<Session> {
        let agent = session_agent();
        let r = agent
            .get(AUTHENTICATED_URL)
            .header("Cookie", &cookie.header())
            .call()
            .map_err(|e| Error(format!("{SIGNED_OUT} (could not reach Roblox: {e})")))?;
        let rotated = rotated_cookie(&r);
        let (status, text) = text_of(r)?;
        if status != 200 {
            return Err(Error(format!(
                "{SIGNED_OUT} (Roblox did not accept the saved session: HTTP {status})"
            )));
        }
        let profile: Profile = serde_json::from_str(&text)
            .map_err(|_| Error(format!("{SIGNED_OUT} (unexpected reply from Roblox)")))?;
        // Roblox hands out the next `.ROBLOSECURITY` value on the identity
        // call when the stored one is outdated. Using it from here on is what
        // keeps asset delivery working: it already refuses an outdated value
        // with "Authentication required to access Asset." [verified: live
        // 2026-11-15 — the stored value validated here but assetdelivery
        // answered 401 code 11, and the same request with the rotated value
        // served the animation asset (1166 bytes) and the account's place].
        let cookie = match rotated {
            Some(new) if new.0 != cookie.0 => {
                persist_rotation(&new, &cookie, profile.id);
                new
            }
            _ => cookie,
        };
        Ok(Session {
            cookie: Arc::new(Mutex::new(cookie)),
            agent,
            csrf: Arc::new(Mutex::new(None)),
            persist: true,
            profile,
        })
    }

    /// A copy for background threads, so they don't queue on the account's
    /// lock (shares the connection pool, cookie rotation, and CSRF token).
    pub fn downloader(&self) -> Session {
        Session {
            cookie: self.cookie.clone(),
            agent: self.agent.clone(),
            csrf: self.csrf.clone(),
            persist: self.persist,
            profile: self.profile.clone(),
        }
    }

    /// Gives an owned session a deadline per HTTP request, keeping its
    /// shared credential and rotation behavior.
    pub fn with_timeout(mut self, timeout: std::time::Duration) -> Self {
        self.agent = session_agent_with_timeout(Some(timeout));
        self
    }

    /// A session that was not checked with Roblox, for talking to a local
    /// mock server (tests and the self-test's save checks). Never used for
    /// a real cookie: nothing about the profile is verified.
    pub fn unchecked(cookie: Cookie, profile: Profile) -> Session {
        Session {
            cookie: Arc::new(Mutex::new(cookie)),
            agent: session_agent(),
            csrf: Arc::new(Mutex::new(None)),
            persist: false,
            profile,
        }
    }

    /// The cookie as it stands now: Roblox may have rotated it since the
    /// session was validated.
    pub fn cookie(&self) -> Cookie {
        self.held()
    }

    /// The value the next request carries.
    fn held(&self) -> Cookie {
        self.cookie
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }

    /// Follows a `.ROBLOSECURITY` rotation a reply carried (`Set-Cookie`).
    /// Roblox rotates the cookie on a cadence of hours to days and rejects
    /// an outdated one on asset delivery before the other hosts do, so every
    /// authenticated reply from an allowed HTTPS origin is checked, not just
    /// the identity call.
    fn apply_rotation(&self, r: &ureq::http::Response<ureq::Body>) {
        let Some(new) = rotated_cookie(r) else { return };
        self.apply_rotated_cookie(new);
    }

    fn apply_rotated_cookie(&self, new: Cookie) {
        // Hold this through persistence: a later downloader rotation must not
        // reach storage before this generation and then be overwritten by it.
        let mut held = self.cookie.lock().unwrap_or_else(|e| e.into_inner());
        if held.0 == new.0 {
            return;
        }
        if self.persist {
            persist_rotation(&new, &held, self.profile.id);
        }
        *held = new;
    }

    /// GET with the session.
    pub fn get(&self, url: &str) -> Result<(u16, String)> {
        self.get_with_timeout(url, None)
    }

    /// GET with the session and extra request headers.
    pub fn get_with_headers(&self, url: &str, headers: &[(&str, &str)]) -> Result<(u16, String)> {
        let mut req = self.agent.get(url).header("Cookie", &self.held().header());
        for (k, v) in headers {
            req = req.header(*k, *v);
        }
        let r = req.call()?;
        self.apply_rotation(&r);
        text_of(r)
    }

    pub(crate) fn get_with_timeout(&self, url: &str, timeout: Option<Duration>) -> Result<(u16, String)> {
        let mut req = self.agent.get(url).header("Cookie", &self.held().header());
        if let Some(timeout) = timeout {
            req = req.config().timeout_global(Some(timeout)).build();
        }
        let r = req.call()?;
        self.apply_rotation(&r);
        text_of(r)
    }

    /// GET with the session, also returning one reply header (asset
    /// delivery's `roblox-assetversionnumber`).
    pub fn get_with_header(&self, url: &str, header: &str) -> Result<(u16, Option<String>, String)> {
        let r = self
            .agent
            .get(url)
            .header("Cookie", &self.held().header())
            .call()?;
        let h = r
            .headers()
            .get(header)
            .and_then(|v| v.to_str().ok())
            .map(str::to_string);
        self.apply_rotation(&r);
        let (status, text) = text_of(r)?;
        Ok((status, h, text))
    }

    /// GET returning bytes, gzip bodies inflated; the cookie only when asked
    /// (CDN locations and thumbnails don't want it).
    pub fn fetch_bytes(&self, url: &str, with_cookie: bool) -> Result<(u16, Vec<u8>)> {
        self.get_bytes(url, with_cookie)
    }

    /// GET returning bytes (assets), gzip bodies inflated.
    fn get_bytes(&self, url: &str, with_cookie: bool) -> Result<(u16, Vec<u8>)> {
        let response = self.get_bytes_response(url, with_cookie)?;
        let status = response.status().as_u16();
        Ok((status, Self::read_bytes(response)?))
    }

    fn get_bytes_response(
        &self,
        url: &str,
        with_cookie: bool,
    ) -> Result<ureq::http::Response<ureq::Body>> {
        let mut req = self.agent.get(url).header("Accept-Encoding", "gzip");
        if with_cookie {
            req = req.header("Cookie", &self.held().header());
            let place = PLACE_CONTEXT.load(std::sync::atomic::Ordering::Relaxed);
            if place != 0 {
                req = req.header("Roblox-Place-Id", &place.to_string());
            }
        }
        let r = req.call()?;
        if with_cookie {
            self.apply_rotation(&r);
        }
        Ok(r)
    }

    fn read_bytes(mut r: ureq::http::Response<ureq::Body>) -> Result<Vec<u8>> {
        let mut bytes = Vec::new();
        r.body_mut()
            .with_config()
            .limit(1 << 30)
            .reader()
            .read_to_end(&mut bytes)?;
        if bytes.starts_with(&[0x1f, 0x8b]) {
            let mut out = Vec::new();
            flate2::read::GzDecoder::new(&bytes[..]).read_to_end(&mut out)?;
            bytes = out;
        }
        Ok(bytes)
    }

    /// Engine data requests over the login session, preserving object metadata
    /// response headers. Credentials and response tokens are never returned.
    pub fn engine_data_request(&self, method: &str, url: &str, body: &[u8], extra: &[(&str, &str)]) -> Result<(u16, Vec<(String, String)>, String)> {
        if !matches!(method, "GET" | "POST" | "DELETE") { return Err(Error::new("unsupported engine data HTTP method")); }
        for _ in 0..=CSRF_MAX_RETRIES {
            let token = self.csrf.lock().ok().and_then(|g| g.clone());
            let mut request = ureq::http::Request::builder().method(method).uri(url)
                .header("Cookie", self.held().header());
            for (k,v) in extra { request = request.header(*k,*v); }
            if method != "GET" { if let Some(token) = &token { request = request.header("X-CSRF-TOKEN", token); } }
            let request = request.body(body).map_err(|_|Error::new("invalid engine data HTTP request"))?;
            let response = self.agent.run(request)?;
            self.apply_rotation(&response);
            let fresh = response.headers().get("x-csrf-token").and_then(|v|v.to_str().ok()).map(str::to_owned);
            let headers = response.headers().iter().filter(|(k,_)| k.as_str().starts_with("roblox-object-") || k.as_str()=="retry-after" || k.as_str()=="content-type" || k.as_str()=="etag" || k.as_str()=="usn" || k.as_str()=="content-md5")
                .filter_map(|(k,v)|v.to_str().ok().map(|v|(k.as_str().to_owned(),v.to_owned()))).collect();
            let (status, text) = text_of(response)?;
            if method != "GET" && status == 403 {
                if let Some(fresh) = fresh.filter(|t|!t.is_empty() && token.as_deref()!=Some(t.as_str())) {
                    if let Ok(mut held)=self.csrf.lock(){ *held=Some(fresh); } continue;
                }
            }
            return Ok((status,headers,text));
        }
        Err(Error::new("Roblox kept refusing the CSRF token"))
    }

    /// The CSRF token held now (tests).
    pub fn csrf_token_held(&self) -> bool {
        self.csrf.lock().map(|g| g.is_some()).unwrap_or(false)
    }

    /// General authenticated HTTP request for Studio built-in plugins
    /// (`HttpService:RequestInternal`, `HttpRbxApiService`). Sends the session
    /// cookie only to `*.roblox.com`, forwards caller headers, handles
    /// X-CSRF-TOKEN retries on 403 for non-GET/HEAD methods, and returns
    /// `(status, reason, headers, body_bytes)`.
    pub fn raw_request(
        &self,
        method: &str,
        url: &str,
        headers: &[(String, String)],
        body: Option<&[u8]>,
    ) -> Result<(u16, String, Vec<(String, String)>, Vec<u8>)> {
        let send_cookie = {
            let rest = url.split_once("://").map(|(_, r)| r).unwrap_or(url);
            let host = rest.split(['/', '?', '#']).next().unwrap_or("");
            let host = host.rsplit_once('@').map(|(_, h)| h).unwrap_or(host);
            let host = host.split(':').next().unwrap_or("").to_ascii_lowercase();
            host == "roblox.com" || host.ends_with(".roblox.com")
        };
        let is_read = matches!(method, "GET" | "HEAD" | "OPTIONS" | "TRACE");
        for _ in 0..=CSRF_MAX_RETRIES {
            let token = self.csrf.lock().ok().and_then(|g| g.clone());
            let mut req = ureq::http::Request::builder().method(method).uri(url);
            if send_cookie {
                req = req.header("Cookie", self.held().header());
            }
            for (k, v) in headers {
                if k.eq_ignore_ascii_case("cookie")
                    || k.eq_ignore_ascii_case("host")
                    || k.eq_ignore_ascii_case("content-length")
                {
                    continue;
                }
                req = req.header(k.as_str(), v.as_str());
            }
            if !is_read && send_cookie {
                if let Some(t) = &token {
                    req = req.header("X-CSRF-TOKEN", t);
                }
            }
            let req = match body {
                Some(b) => req.body(b).map_err(|_| Error::new("invalid HTTP request"))?,
                None => req.body(&[][..]).map_err(|_| Error::new("invalid HTTP request"))?,
            };
            let mut resp = self.agent.run(req)?;
            if send_cookie {
                self.apply_rotation(&resp);
            }
            let fresh = resp
                .headers()
                .get("x-csrf-token")
                .and_then(|v| v.to_str().ok())
                .map(str::to_string);
            let status = resp.status();
            let status_code = status.as_u16();
            let reason = status.canonical_reason().unwrap_or("").to_string();
            let resp_headers: Vec<(String, String)> = resp
                .headers()
                .iter()
                .map(|(k, v)| (k.as_str().to_string(), String::from_utf8_lossy(v.as_bytes()).into_owned()))
                .collect();
            let resp_body = resp
                .body_mut()
                .with_config()
                .limit(1 << 28)
                .read_to_vec()?;
            if !is_read && send_cookie && status_code == 403 {
                if let Some(t) = fresh.filter(|t| !t.is_empty() && token.as_deref() != Some(t.as_str())) {
                    if let Ok(mut g) = self.csrf.lock() {
                        *g = Some(t);
                    }
                    continue;
                }
            }
            return Ok((status_code, reason, resp_headers, resp_body));
        }
        Err(Error::new("Roblox kept refusing the CSRF token"))
    }

    /// A state-changing POST with the X-CSRF-TOKEN dance, Studio's rule
    /// a reply of 403 to a method other
    /// than GET / HEAD / OPTIONS / TRACE that carries a non-empty
    /// `x-csrf-token` different from the one sent is retried with it, at most
    /// [`CSRF_MAX_RETRIES`] times. The token is kept for the session.
    pub fn post_csrf(&self, url: &str, content_type: &str, body: &[u8]) -> Result<(u16, String)> {
        self.post_csrf_with(url, content_type, body, &[])
    }

    /// [`Session::post_csrf`] with extra request headers (Team Create's
    /// `robloxctx-authenticated-userid`, Studio's user agent).
    pub fn post_csrf_with(
        &self,
        url: &str,
        content_type: &str,
        body: &[u8],
        extra: &[(&str, &str)],
    ) -> Result<(u16, String)> {
        self.post_csrf_with_timeout(url, content_type, body, extra, None)
    }

    /// As `post_csrf_with`, with a deadline shared by the CSRF retry.
    pub fn post_csrf_with_timeout(
        &self,
        url: &str,
        content_type: &str,
        body: &[u8],
        extra: &[(&str, &str)],
        timeout: Option<std::time::Duration>,
    ) -> Result<(u16, String)> {
        let started = std::time::Instant::now();
        for _ in 0..=CSRF_MAX_RETRIES {
            let token = self.csrf.lock().ok().and_then(|g| g.clone());
            let mut req = self
                .agent
                .post(url)
                .header("Cookie", &self.held().header())
                .header("Content-Type", content_type);
            for (k, v) in extra {
                req = req.header(*k, *v);
            }
            if let Some(t) = &token {
                req = req.header("X-CSRF-TOKEN", t);
            }
            if let Some(timeout) = timeout {
                let remaining = timeout.saturating_sub(started.elapsed());
                if remaining.is_zero() {
                    return Err(Error::new("Request timed out"));
                }
                req = req.config().timeout_global(Some(remaining)).build();
            }
            let r = req.send(body)?;
            self.apply_rotation(&r);
            let fresh = r
                .headers()
                .get("x-csrf-token")
                .and_then(|v| v.to_str().ok())
                .map(str::to_string);
            let (status, text) = text_of(r)?;
            match fresh {
                Some(t)
                    if status == 403 && !t.is_empty() && token.as_deref() != Some(t.as_str()) =>
                {
                    if let Ok(mut g) = self.csrf.lock() {
                        *g = Some(t);
                    }
                    continue;
                }
                _ => return Ok((status, text)),
            }
        }
        Err(Error::new("Roblox kept refusing the CSRF token"))
    }

    /// A state-changing request of any method (POST / PATCH / PUT / DELETE)
    /// with the CSRF dance of [`Session::post_csrf_with`].
    pub fn send_csrf(&self, method: &str, url: &str, content_type: &str, body: &[u8]) -> Result<(u16, String)> {
        self.send_csrf_with_timeout(method, url, content_type, body, None)
    }

    /// The timeout covers the upload and its CSRF retries.
    pub(crate) fn send_csrf_with_timeout(
        &self,
        method: &str,
        url: &str,
        content_type: &str,
        body: &[u8],
        timeout: Option<Duration>,
    ) -> Result<(u16, String)> {
        let started = Instant::now();
        for _ in 0..=CSRF_MAX_RETRIES {
            let token = self.csrf.lock().ok().and_then(|g| g.clone());
            let mut req = ureq::http::Request::builder()
                .method(method)
                .uri(url)
                .header("Cookie", self.held().header())
                .header("Content-Type", content_type);
            if let Some(t) = &token {
                req = req.header("X-CSRF-TOKEN", t);
            }
            let mut req = req.body(body).map_err(|_| Error::new("invalid HTTP request"))?;
            if let Some(timeout) = timeout {
                let remaining = timeout.saturating_sub(started.elapsed());
                if remaining.is_zero() {
                    return Err(Error::new("Request timed out"));
                }
                req = self.agent.configure_request(req).timeout_global(Some(remaining)).build();
            }
            let r = self.agent.run(req)?;
            self.apply_rotation(&r);
            let fresh = r.headers().get("x-csrf-token").and_then(|v| v.to_str().ok()).map(str::to_string);
            let (status, text) = text_of(r)?;
            match fresh {
                Some(t) if status == 403 && !t.is_empty() && token.as_deref() != Some(t.as_str()) => {
                    if let Ok(mut g) = self.csrf.lock() {
                        *g = Some(t);
                    }
                    continue;
                }
                _ => return Ok((status, text)),
            }
        }
        Err(Error::new("Roblox kept refusing the CSRF token"))
    }

    /// The signed-in person's avatar description (avatar.roblox.com/v1/avatar).
    pub fn avatar_json(&self) -> Result<serde_json::Value> {
        let (status, text) = self.get(MY_AVATAR_URL)?;
        if status != 200 {
            return Err(Error(format!(
                "avatar.roblox.com/v1/avatar returned HTTP {status}"
            )));
        }
        Ok(serde_json::from_str(&text)?)
    }

    /// An asset's bytes through asset delivery v2 with the session (assets
    /// served only to signed-in accounts included), then its CDN location.
    ///
    /// Roblox's asset delivery refuses an outdated `.ROBLOSECURITY` with
    /// "Authentication required to access Asset." (HTTP 200, `errors[0]`
    /// code 401) while other endpoints still accept it [verified: live
    /// 2026-11-15 — the stored value validated at
    /// `users.roblox.com/v1/users/authenticated`, asset delivery answered
    /// 401 code 11, and the rotated value from that call's `Set-Cookie`
    /// served the same asset]. So a refusal asks the identity endpoint for
    /// the rotation once and retries.
    pub fn download_asset(&self, asset_id: u64, version: Option<u64>) -> Result<Vec<u8>> {
        self.download_asset_typed(asset_id, version).map_err(Error::from)
    }

    /// Like [Self::download_asset], with HTTP retry information for scheduling.
    pub fn download_asset_typed(
        &self,
        asset_id: u64,
        version: Option<u64>,
    ) -> std::result::Result<Vec<u8>, AssetDownloadError> {
        let first = self.download_asset_at(asset_id, version);
        if let Err(AssetDownloadError::Refused(e) | AssetDownloadError::Other(e)) = &first {
            if e.0.contains("Authentication required") && self.refresh_cookie().unwrap_or(false) {
                return self.download_asset_at(asset_id, version);
            }
        }
        first
    }

    /// Asks the identity endpoint for the next `.ROBLOSECURITY` value — the
    /// `Set-Cookie` rotation — and reports whether the session's value
    /// changed. Every reply is checked anyway (`apply_rotation`); this is for
    /// a session that has not talked to a rotating endpoint since the value
    /// went stale.
    fn refresh_cookie(&self) -> Result<bool> {
        let before = self.held();
        let (status, _) = self.get(AUTHENTICATED_URL)?;
        Ok(status == 200 && self.held().0 != before.0)
    }

    /// One asset delivery attempt.
    fn download_asset_at(
        &self,
        asset_id: u64,
        version: Option<u64>,
    ) -> std::result::Result<Vec<u8>, AssetDownloadError> {
        let url = match version {
            Some(v) => format!("{ASSET_V2_URL}/{asset_id}/version/{v}"),
            None => format!("{ASSET_V2_URL}/{asset_id}"),
        };
        // Studio asks asset delivery in the open place's context
        // (`Roblox-Place-Id`): assets the place's creator owns (private
        // animations, audio) are then served to its collaborators.
        let place = PLACE_CONTEXT.load(std::sync::atomic::Ordering::Relaxed);
        let mut req = self.agent.get(&url).header("Cookie", &self.held().header());
        if place != 0 {
            req = req.header("Roblox-Place-Id", &place.to_string());
        }
        let r = req.call().map_err(Error::from)?;
        self.apply_rotation(&r);
        if let Some(error) = AssetDownloadError::transient_response(asset_id, "asset delivery", &r) {
            return Err(error);
        }
        let (status, text) = text_of(r)?;
        let v: serde_json::Value = serde_json::from_str(&text).map_err(|_| {
            Error(format!(
                "asset {asset_id}: asset delivery returned HTTP {status}"
            ))
        })?;
        let Some(location) = v.pointer("/locations/0/location").and_then(|l| l.as_str()) else {
            let why = v.pointer("/errors/0/message").and_then(|m| m.as_str());
            // v2 answers 200 with the refusal in the body (see `public_asset_typed`).
            let http = if status == 200 { String::new() } else { format!(" (HTTP {status})") };
            let error = Error(format!(
                "asset {asset_id}: asset delivery: {}{http}",
                why.unwrap_or("no download location")
            ));
            return Err(match why {
                Some(_) => AssetDownloadError::Refused(error),
                None => AssetDownloadError::Other(error),
            });
        };
        let response = self.get_bytes_response(location, false)?;
        if let Some(error) = AssetDownloadError::transient_response(asset_id, "download", &response) {
            return Err(error);
        }
        let status = response.status().as_u16();
        let bytes = Self::read_bytes(response)?;
        if !(200..300).contains(&status) {
            return Err(Error(format!(
                "asset {asset_id}: download returned HTTP {status}"
            )).into());
        }
        Ok(bytes)
    }

    /// Experiences this account owns (develop.roblox.com/v1/user/universes;
    /// `limit` must be 10, 25, 50 or 100).
    pub fn my_universe_ids(&self) -> Result<Vec<(u64, String)>> {
        let mut out = Vec::new();
        let mut cursor = String::new();
        loop {
            let mut url = format!("{MY_UNIVERSES_URL}?limit=50&sortOrder=Asc");
            if !cursor.is_empty() {
                url.push_str(&format!("&cursor={cursor}"));
            }
            let (status, text) = self.get(&url)?;
            if status != 200 {
                return Err(Error(format!(
                    "develop.roblox.com/v1/user/universes returned HTTP {status}"
                )));
            }
            let v: serde_json::Value = serde_json::from_str(&text)?;
            for u in v
                .get("data")
                .and_then(|d| d.as_array())
                .into_iter()
                .flatten()
            {
                if let Some(id) = u.get("id").and_then(|i| i.as_u64()) {
                    let name = u
                        .get("name")
                        .and_then(|n| n.as_str())
                        .unwrap_or("")
                        .to_string();
                    out.push((id, name));
                }
            }
            match v.get("nextPageCursor").and_then(|c| c.as_str()) {
                Some(c) if !c.is_empty() => cursor = c.to_string(),
                _ => break,
            }
        }
        Ok(out)
    }

    /// Publishes a place and returns its confirmed revision number as text.
    pub fn upload_place(&self, place_id: u64, bytes: &[u8], is_xml: bool) -> Result<String> {
        crate::cloud::upload_place(
            self,
            &crate::Endpoints::default().apis,
            place_id,
            bytes,
            is_xml,
            crate::VersionType::Published,
        )
        .map(|version| version.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn csrf_post_deadline_stops_a_stalled_server() {
        use std::{io::Read, net::TcpListener, time::{Duration, Instant}};
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}/", listener.local_addr().unwrap());
        let server = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            stream.set_read_timeout(Some(Duration::from_secs(2))).unwrap();
            let mut bytes = [0; 4096];
            let _ = stream.read(&mut bytes);
            // Keep the connection open without ever replying.
            std::thread::sleep(Duration::from_secs(1));
        });
        let session = Session::unchecked(Cookie::new("test-cookie").unwrap(),
            Profile { id: 1, name: "test".into(), display_name: "test".into() });
        let started = Instant::now();
        let result = session.post_csrf_with_timeout(&url, "application/json", b"{}", &[],
            Some(Duration::from_millis(100)));
        let elapsed = started.elapsed();
        server.join().unwrap();
        assert!(result.is_err());
        assert!(elapsed < Duration::from_millis(900), "{elapsed:?}");
    }

    #[test]
    fn cookie_never_prints() {
        let c = Cookie::new(".ROBLOSECURITY=_|WARNING:-secret-value-123\n").unwrap();
        assert_eq!(format!("{c}"), "<redacted>");
        assert_eq!(format!("{c:?}"), "<redacted>");
        assert!(!c.cache_key().contains("secret"));
        assert_eq!(c.cache_key().len(), 64);
        assert!(c.appears_in("x _|WARNING:-secret-value-123 y"));
        let other = Cookie::new("another-value-456").unwrap();
        assert_ne!(c.cache_key(), other.cache_key());
    }

    #[test]
    fn empty_lines_are_no_cookie() {
        assert!(Cookie::new("").is_none());
        assert!(Cookie::new("   \n").is_none());
        assert!(Cookie::new(".ROBLOSECURITY=").is_none());
    }

    /// A run that names its cookie file (RUSTUDIO_COOKIE_FILE) signs in as
    /// that file's account, every time, and leaves the keyring's session
    /// alone. Imported, the file took over the keyring on its first run,
    /// and once seen it lost to whatever the keyring held: a later run with
    /// the same file signed in as another account.
    #[test]
    fn an_explicit_cookie_file_beats_the_keyring_and_is_not_imported() {
        use std::os::unix::fs::PermissionsExt;
        let dir = std::env::temp_dir().join(format!("rustudio-explicit-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("second-account");
        std::fs::write(&path, "second-account-cookie\n").unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
        let person = Cookie::new("first-account-cookie").unwrap();
        keyring_store(&person).unwrap();
        for run in 0..2 {
            let (c, src) = load_cookie_from(true, &path, true).unwrap().unwrap();
            assert_eq!(c.0, "second-account-cookie", "run {run}");
            assert_eq!(src, CookieSource::File { imported: false });
            assert_eq!(keyring_cookie().unwrap().0, person.0, "run {run}: the keyring keeps its session");
        }
        // A file seen by an earlier run (its import marker) still wins.
        mark_seen(&path, &Cookie::new("second-account-cookie").unwrap());
        assert_eq!(load_cookie_from(true, &path, true).unwrap().unwrap().0 .0, "second-account-cookie");
        assert!(load_cookie_from(true, &dir.join("missing"), true).is_err());
        let _ = std::fs::remove_dir_all(&dir);
    }

    fn legacy_put(attrs: &[&str], v: &str) {
        legacy::ITEMS.with(|m| m.borrow_mut().insert(attrs.join(" "), v.to_string()));
    }

    fn legacy_has(attrs: &[&str]) -> bool {
        legacy::ITEMS.with(|m| m.borrow().contains_key(&attrs.join(" ")))
    }

    fn stored(user: &str) -> Option<String> {
        crate::store::load(user)
            .unwrap()
            .map(|v| String::from_utf8(v).unwrap())
    }

    #[test]
    fn session_round_trips_through_the_keyring() {
        assert!(keyring_cookie().is_none());
        keyring_store(&Cookie::new("cookie-value-1").unwrap()).unwrap();
        assert_eq!(stored("roblox-cookie").as_deref(), Some("cookie-value-1"));
        assert_eq!(keyring_cookie().unwrap().0, "cookie-value-1");
        keyring_clear().unwrap();
        assert!(keyring_cookie().is_none());
        assert!(stored("roblox-cookie").is_none());
    }

    #[test]
    fn secret_tool_session_moves_into_the_keyring() {
        legacy_put(
            &["service", "rustudio", "key", "roblox-cookie"],
            "old-cookie-value\n",
        );
        assert_eq!(keyring_cookie().unwrap().0, "old-cookie-value");
        assert_eq!(stored("roblox-cookie").as_deref(), Some("old-cookie-value"));
        assert!(!legacy_has(&LEGACY_COOKIE));
        assert_eq!(keyring_cookie().unwrap().0, "old-cookie-value");
    }

    #[test]
    fn keyring_copy_wins_over_secret_tool() {
        keyring_store(&Cookie::new("new-cookie-value").unwrap()).unwrap();
        legacy_put(&LEGACY_COOKIE, "old-cookie-value");
        assert_eq!(keyring_cookie().unwrap().0, "new-cookie-value");
    }

    #[test]
    fn sign_out_clears_the_secret_tool_copy_too() {
        keyring_store(&Cookie::new("new-cookie-value").unwrap()).unwrap();
        legacy_put(&LEGACY_COOKIE, "old-cookie-value");
        keyring_clear().unwrap();
        assert!(!legacy_has(&LEGACY_COOKIE));
        assert!(keyring_cookie().is_none());
    }

    #[test]
    fn saved_accounts_have_their_own_entries() {
        let old = ["service", "rustudio", "key", "roblox-account", "user", "42"];
        legacy_put(&old, "account-42-cookie");
        assert_eq!(account_cookie(42).unwrap().0, "account-42-cookie");
        assert_eq!(
            stored("roblox-account-42").as_deref(),
            Some("account-42-cookie")
        );
        assert!(!legacy_has(&old));
        account_store(7, &Cookie::new("account-7-cookie").unwrap()).unwrap();
        assert_eq!(account_cookie(7).unwrap().0, "account-7-cookie");
        assert!(keyring_cookie().is_none());
        account_forget(42);
        assert!(account_cookie(42).is_none());
        assert_eq!(account_cookie(7).unwrap().0, "account-7-cookie");
    }
}

#[cfg(test)]
mod live {
    use super::*;

    /// Live probe (network + the person's cookie; `cargo test -- --ignored`).
    /// Prints statuses and the profile name only.
    #[test]
    #[ignore]
    fn probe_live_session() {
        let Some((c, src)) = load_cookie(true).expect("cookie") else {
            println!("no cookie");
            return;
        };
        println!("cookie from {src:?}");
        let s = match Session::validate(c) {
            Ok(s) => s,
            Err(e) => {
                println!("{e}");
                return;
            }
        };
        println!("signed in as {} (id {})", s.profile.display(), s.profile.id);
        match s.avatar_json() {
            Ok(v) => println!(
                "avatar type {:?} scales {:?}",
                v.get("playerAvatarType"),
                v.get("scales")
            ),
            Err(e) => println!("avatar: {e}"),
        }
        match s.my_universe_ids() {
            Ok(u) => println!(
                "{} owned experiences: {:?}",
                u.len(),
                u.iter().map(|x| &x.1).collect::<Vec<_>>()
            ),
            Err(e) => println!("universes: {e}"),
        }
        for id in [123480233606534u64, 76523574292282] {
            match s.download_asset(id, None) {
                Ok(b) => println!(
                    "asset {id}: {} bytes, starts {:?}",
                    b.len(),
                    String::from_utf8_lossy(&b[..b.len().min(12)])
                ),
                Err(e) => println!("asset {id}: {e}"),
            }
        }
    }
}

#[cfg(test)]
mod live_assets {
    use super::*;

    /// Saves login-only assets to /tmp/rs-auth (RUSTUDIO_FETCH=id,id,…) and
    /// prints the avatar JSON's top-level keys.
    #[test]
    #[ignore]
    fn fetch_login_assets() {
        let Some((c, _)) = load_cookie(true).expect("cookie") else {
            return;
        };
        let s = Session::validate(c).expect("session");
        let _ = std::fs::create_dir_all("/tmp/rs-auth");
        for id in std::env::var("RUSTUDIO_FETCH")
            .unwrap_or_default()
            .split(',')
            .filter_map(|x| x.parse::<u64>().ok())
        {
            match s.download_asset(id, None) {
                Ok(b) => {
                    std::fs::write(format!("/tmp/rs-auth/{id}"), &b).unwrap();
                    println!("{id}: {} bytes", b.len());
                }
                Err(e) => println!("{id}: {e}"),
            }
        }
        if let Ok(v) = s.avatar_json() {
            println!(
                "avatar keys {:?}",
                v.as_object().map(|o| o.keys().cloned().collect::<Vec<_>>())
            );
            println!("bodyColors {:?}", v.get("bodyColors"));
            println!(
                "assets {:?}",
                v.get("assets")
                    .map(|a| a.to_string().chars().take(600).collect::<String>())
            );
        }
    }
}

#[cfg(test)]
mod rotation {
    use super::*;
    use crate::mock::{MockReply, MockRequest, MockServer};

    fn mock(cookie: &'static str) -> Profile {
        let _ = cookie;
        Profile {
            id: 1,
            name: "Mock".into(),
            display_name: "Mock".into(),
        }
    }

    /// The reply Roblox sends when it hands out the next `.ROBLOSECURITY`.
    const ROTATED: &str = ".ROBLOSECURITY=rotated-value; Domain=.roblox.com; Path=/; \
        Expires=Mon, 15 Nov 2027 10:25:03 GMT; Secure; HttpOnly";

    #[test]
    fn a_reply_rotates_the_cookie_and_every_copy_uses_it() {
        let server = MockServer::start(std::sync::Arc::new(|_: &MockRequest| {
            MockReply::json(200, "{}").with_header("Set-Cookie", ROTATED)
        }))
        .unwrap();
        let s = Session::unchecked(Cookie::new("stale-value").unwrap(), mock(""));
        let copy = s.downloader();
        let url = format!("{}/identity", server.base);
        s.get(&url).unwrap();
        s.apply_rotated_cookie(Cookie::new("rotated-value").unwrap());
        assert_eq!(
            copy.cookie().cache_key(),
            Cookie::new("rotated-value").unwrap().cache_key()
        );
        copy.get(&url).unwrap();
        let sent = server.requests();
        assert_eq!(sent[0].header("cookie"), Some(".ROBLOSECURITY=stale-value"));
        assert_eq!(sent[1].header("cookie"), Some(".ROBLOSECURITY=rotated-value"));
    }

    #[test]
    fn only_the_session_cookie_value_is_extracted() {
        let mut headers = ureq::http::HeaderMap::new();
        headers.append("set-cookie", "rbx-ip2=1; domain=roblox.com".parse().unwrap());
        assert!(rotation_value(&headers).is_none());
        headers.append("set-cookie", ROTATED.parse().unwrap());
        assert_eq!(rotation_value(&headers).unwrap().0, "rotated-value");
    }

    #[test]
    fn rotations_require_an_exact_https_auth_origin() {
        for url in ["https://users.roblox.com/v1/users/authenticated",
            "https://assetdelivery.roblox.com:443/v2/assetId/1",
            "https://auth.roblox.com/"] {
            assert!(trusted_rotation_origin(&url.parse().unwrap()));
        }
        for url in ["http://users.roblox.com/", "https://users.roblox.com:444/",
            "https://users.roblox.com.evil.example/", "https://evilroblox.com/",
            "https://tr.rbxcdn.com/", "https://images.rbxcdn.com/",
            "https://untrusted.roblox.com/", "http://127.0.0.1/"] {
            assert!(!trusted_rotation_origin(&url.parse().unwrap()));
        }
    }

    #[test]
    fn untrusted_and_anonymous_replies_cannot_rotate_any_copy() {
        let server = MockServer::start(Arc::new(|_: &MockRequest| {
            MockReply::json(200, "{}").with_header("Set-Cookie", ROTATED)
        })).unwrap();
        let s = Session::unchecked(Cookie::new("stale-value").unwrap(), mock(""));
        let copy = s.downloader();
        let url = format!("{}/image", server.base);
        s.get(&url).unwrap();
        s.fetch_bytes(&url, false).unwrap();
        s.fetch_bytes(&url, true).unwrap();
        assert_eq!(copy.cookie().0, "stale-value");
        assert!(server.requests()[1].header("cookie").is_none());
    }

    #[test]
    fn redirected_reply_is_checked_at_its_final_origin() {
        let destination = MockServer::start(Arc::new(|_: &MockRequest| {
            MockReply::json(200, "{}").with_header("Set-Cookie", ROTATED)
        })).unwrap();
        let location = format!("{}/final", destination.base);
        let source = MockServer::start(Arc::new(move |_: &MockRequest| {
            MockReply::json(302, "{}").with_header("Location", &location)
        })).unwrap();
        let agent: ureq::Agent = ureq::Agent::config_builder().http_status_as_error(false)
            .build().into();
        let reply = agent.get(format!("{}/redirect", source.base)).call().unwrap();
        assert_eq!(reply.get_uri().host(), Some("127.0.0.1"));
        assert_eq!(reply.get_uri().path(), "/final");
        assert!(rotated_cookie(&reply).is_none());
    }

    #[test]
    fn late_rotation_cannot_replace_active_account_or_restore_signout() {
        let old = Cookie::new("original-account").unwrap();
        let new = Cookie::new("rotated-account").unwrap();
        keyring_store(&old).unwrap();
        account_store(1, &old).unwrap();
        keyring_store(&Cookie::new("different-account").unwrap()).unwrap();
        persist_rotation(&new, &old, 1);
        assert_eq!(keyring_cookie().unwrap().0, "different-account");
        assert_eq!(account_cookie(1).unwrap().0, "rotated-account");
        keyring_clear().unwrap();
        account_forget(1);
        persist_rotation(&Cookie::new("late-value").unwrap(), &new, 1);
        assert!(keyring_cookie().is_none());
        assert!(account_cookie(1).is_none());
    }

    #[test]
    fn rotations_save_in_shared_cookie_order_and_reject_stale_storage() {
        let old = Cookie::new("original-account").unwrap();
        keyring_store(&old).unwrap();
        account_store(1, &old).unwrap();
        let mut s = Session::unchecked(old.clone(), mock(""));
        s.persist = true;
        let copy = s.downloader();
        s.apply_rotated_cookie(Cookie::new("first-rotation").unwrap());
        copy.apply_rotated_cookie(Cookie::new("second-rotation").unwrap());
        assert_eq!(s.cookie().0, "second-rotation");
        assert_eq!(keyring_cookie().unwrap().0, "second-rotation");
        assert_eq!(account_cookie(1).unwrap().0, "second-rotation");
        persist_rotation(&Cookie::new("stale-write").unwrap(), &old, 1);
        assert_eq!(keyring_cookie().unwrap().0, "second-rotation");
        assert_eq!(account_cookie(1).unwrap().0, "second-rotation");
    }

    #[test]
    fn other_cookies_are_not_a_rotation() {
        let server = MockServer::start(std::sync::Arc::new(|_: &MockRequest| {
            MockReply::json(200, "{}").with_header("Set-Cookie", "rbx-ip2=1; domain=roblox.com; path=/")
        }))
        .unwrap();
        let s = Session::unchecked(Cookie::new("stale-value").unwrap(), mock(""));
        s.get(&format!("{}/x", server.base)).unwrap();
        assert_eq!(
            s.cookie().cache_key(),
            Cookie::new("stale-value").unwrap().cache_key()
        );
    }
}
