//! Roblox's published client fast flags, for the CoreScripts (core_scripts.rs).
//!
//! The CoreScripts choose their UI by fast flags (DataModel:GetFastFlag /
//! DefineFastFlag, settings():GetFFlag): the top bar, the in-game menu and
//! the chat each have older and newer variants. A Roblox client reads them
//! from Roblox's public client settings,
//! `https://clientsettingscdn.roblox.com/v2/settings/application/PCDesktopClient`
//! (no sign-in; `{"applicationSettings": {name: value}}`, values as strings):
//! `FFlag` / `DFFlag` "True" / "False", `FInt` / `DFInt`, `FString` /
//! `DFString`, `FLog` / `DFLog`, and `<name>_PlaceFilter` entries
//! "value;placeId;placeId…" that apply only in the listed places.
//!
//! Fetched only when Roblox's CoreScripts are on, kept in
//! `<data>/rustudio/client-flags/PCDesktopClient.json` and fetched again
//! once it is older than [`MAX_AGE`]; offline, the kept copy is used, and
//! with none the CoreScripts' own defaults stand. Rollout flags (per-mille
//! numbers such as `…RolloutPermille`) are read as the plain numbers they
//! are; where the engine itself buckets a user against one, rustudio treats
//! the user as in (estimate). Engine features (DataModel:GetEngineFeature)
//! are not in this document: they are built into each client.
//!
//! For tests, `<config>/rustudio/client-flags-override.json` ({name: value},
//! full names with their prefix, values as in the document) wins over the
//! fetched values.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, RwLock};
use std::time::{Duration, SystemTime};

pub const URL: &str = "https://clientsettingscdn.roblox.com/v2/settings/application/PCDesktopClient";
/// How old the kept copy may get before Play fetches it again (Roblox's
/// clients ask at every launch; a day keeps rustudio from asking on every
/// Play while staying on the day's rollout).
pub const MAX_AGE: Duration = Duration::from_secs(24 * 60 * 60);

/// The flag kinds a lookup asks for: each has a plain and a D-prefixed name.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Flag,
    Int,
    Str,
}

impl Kind {
    fn prefixes(self) -> [&'static str; 2] {
        match self {
            Kind::Flag => ["FFlag", "DFFlag"],
            Kind::Int => ["FInt", "DFInt"],
            Kind::Str => ["FString", "DFString"],
        }
    }
}

/// The settings document, by full name.
#[derive(Default, Debug, Clone)]
pub struct Flags(HashMap<String, String>);

impl Flags {
    /// Reads `{"applicationSettings": {...}}` (or a bare `{name: value}`
    /// object, the override file's form).
    pub fn parse(bytes: &[u8]) -> Option<Flags> {
        let v: serde_json::Value = serde_json::from_slice(bytes).ok()?;
        let obj = v.get("applicationSettings").unwrap_or(&v).as_object()?;
        Some(Flags(
            obj.iter()
                .filter_map(|(k, v)| {
                    let s = match v {
                        serde_json::Value::String(s) => s.clone(),
                        serde_json::Value::Bool(b) => if *b { "True".into() } else { "False".into() },
                        serde_json::Value::Number(n) => n.to_string(),
                        _ => return None,
                    };
                    Some((k.clone(), s))
                })
                .collect(),
        ))
    }

    pub fn len(&self) -> usize {
        self.0.len()
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    /// The raw value of flag `name` (without its prefix) of `kind` in place
    /// `place_id`: a `_PlaceFilter` entry naming the place first, then the
    /// plain entry; the plain prefix before the D one.
    pub fn raw(&self, kind: Kind, name: &str, place_id: i64) -> Option<&str> {
        for prefix in kind.prefixes() {
            if let Some(f) = self.0.get(&format!("{prefix}{name}_PlaceFilter")) {
                let mut parts = f.split(';');
                let value = parts.next().unwrap_or("");
                if parts.any(|p| p.trim().parse::<i64>().ok() == Some(place_id)) {
                    return Some(value);
                }
            }
        }
        kind.prefixes().iter().find_map(|p| self.0.get(&format!("{p}{name}")).map(String::as_str))
    }

    pub fn flag(&self, name: &str, place_id: i64) -> Option<bool> {
        match self.raw(Kind::Flag, name, place_id)?.trim().to_ascii_lowercase().as_str() {
            "true" => Some(true),
            "false" => Some(false),
            _ => None,
        }
    }

    pub fn int(&self, name: &str, place_id: i64) -> Option<i64> {
        self.raw(Kind::Int, name, place_id)?.trim().parse().ok()
    }

    pub fn string(&self, name: &str, place_id: i64) -> Option<String> {
        self.raw(Kind::Str, name, place_id).map(str::to_string)
    }

    fn merge(&mut self, other: Flags) {
        self.0.extend(other.0);
    }
}

static CURRENT: RwLock<Option<Arc<Flags>>> = RwLock::new(None);

/// The flags loaded for this Play (none before `load`).
pub fn current() -> Option<Arc<Flags>> {
    CURRENT.read().ok()?.clone()
}

fn cache_file() -> PathBuf {
    crate::paths::data().join("rustudio/client-flags/PCDesktopClient.json")
}

fn override_file() -> PathBuf {
    crate::paths::config().join("rustudio/client-flags-override.json")
}

/// Loads the flags for a Play with Roblox's CoreScripts: the kept copy when
/// fresh, else a fetch (kept on success), else the kept copy however old;
/// then the override file. Returns a line for Output.
pub fn load() -> String {
    let file = cache_file();
    let age = std::fs::metadata(&file).and_then(|m| m.modified()).ok().and_then(|t| SystemTime::now().duration_since(t).ok());
    let mut note;
    let mut flags = match age {
        Some(a) if a < MAX_AGE => {
            note = format!("kept copy, {} min old", a.as_secs() / 60);
            std::fs::read(&file).ok().and_then(|b| Flags::parse(&b))
        }
        _ => match fetch() {
            Ok(bytes) => {
                if let Some(dir) = file.parent() {
                    let _ = std::fs::create_dir_all(dir);
                }
                let _ = std::fs::write(&file, &bytes);
                note = "fetched".to_string();
                Flags::parse(&bytes)
            }
            Err(e) => {
                note = format!("fetch failed ({e}), kept copy");
                std::fs::read(&file).ok().and_then(|b| Flags::parse(&b))
            }
        },
    }
    .unwrap_or_else(|| {
        note = "none: the CoreScripts' defaults".to_string();
        Flags::default()
    });
    if let Some(o) = std::fs::read(override_file()).ok().and_then(|b| Flags::parse(&b)) {
        note.push_str(&format!(", {} overridden", o.len()));
        flags.merge(o);
    }
    let n = flags.len();
    if let Ok(mut c) = CURRENT.write() {
        *c = Some(Arc::new(flags));
    }
    format!("Roblox client flags: {n} ({note})")
}

fn fetch() -> Result<Vec<u8>, String> {
    let agent: ureq::Agent = ureq::Agent::config_builder()
        .timeout_global(Some(Duration::from_secs(10)))
        .user_agent(concat!("rustudio/", env!("CARGO_PKG_VERSION")))
        .build()
        .into();
    crate::studio_content::get(&agent, URL, None)
}

#[cfg(test)]
mod tests {
    use super::*;

    const FIXTURE: &str = r#"{"applicationSettings": {
        "FFlagTopBarUseNewBadge": "True",
        "DFFlagOnlyDynamic": "False",
        "FFlagFiltered": "False",
        "FFlagFiltered_PlaceFilter": "True;1818;920587237",
        "DFIntSomeRolloutPermille": "250",
        "FStringGreeting": "hello;world",
        "FIntNotANumber": "x"
    }}"#;

    #[test]
    fn reads_plain_dynamic_and_place_filtered_flags() {
        let f = Flags::parse(FIXTURE.as_bytes()).unwrap();
        assert_eq!(f.flag("TopBarUseNewBadge", 1), Some(true));
        // The D-prefixed name answers when the plain one is missing.
        assert_eq!(f.flag("OnlyDynamic", 1), Some(false));
        // A place filter applies only in its places.
        assert_eq!(f.flag("Filtered", 1818), Some(true));
        assert_eq!(f.flag("Filtered", 920587237), Some(true));
        assert_eq!(f.flag("Filtered", 1), Some(false));
        assert_eq!(f.int("SomeRolloutPermille", 1), Some(250));
        // (a string keeps its semicolons: only _PlaceFilter entries split)
        assert_eq!(f.string("Greeting", 1).as_deref(), Some("hello;world"));
        assert_eq!(f.int("NotANumber", 1), None);
        assert_eq!(f.flag("Missing", 1), None);
    }

    #[test]
    fn the_override_form_is_a_bare_object_and_wins() {
        let mut f = Flags::parse(FIXTURE.as_bytes()).unwrap();
        f.merge(Flags::parse(br#"{"FFlagTopBarUseNewBadge": false, "FIntExtra": 3}"#).unwrap());
        assert_eq!(f.flag("TopBarUseNewBadge", 1), Some(false));
        assert_eq!(f.int("Extra", 1), Some(3));
    }
}
