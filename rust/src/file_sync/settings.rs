//! File Sync's settings (Argon's plugin settings, minus the server ones:
//! rustudio reads the disk itself) and the projects remembered per place.
//! Kept in `$XDG_CONFIG_HOME/rustudio/file-sync.json` for runs a person
//! drives; `RUSTUDIO_FILE_SYNC_SETTINGS=<file>` picks the file for any run.
//! Test and harness runs start from the defaults.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

/// What happens when a project is attached (Argon's Initial Sync Priority).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Initial {
    /// The differences are shown; Apply makes the files win.
    Preview,
    /// The files win at once (still asks over the threshold / in live places).
    Files,
    /// The place is kept as it is; only files changed from now on sync.
    Place,
}

impl Initial {
    pub const ALL: [Initial; 3] = [Initial::Preview, Initial::Files, Initial::Place];
    pub fn label(self) -> &'static str {
        match self {
            Initial::Preview => "Show diff first",
            Initial::Files => "Files win",
            Initial::Place => "Place wins",
        }
    }
    pub fn hint(self) -> &'static str {
        match self {
            Initial::Preview => "Compare the files with the place and show what would change; Apply makes the files win",
            Initial::Files => "Apply the files to the place at once (still asks over the changes threshold, and always in live places)",
            Initial::Place => "Keep the place as it is; only files changed after attaching are synced",
        }
    }
    pub fn key(self) -> &'static str {
        match self {
            Initial::Preview => "preview",
            Initial::Files => "files",
            Initial::Place => "place",
        }
    }
    pub fn parse(s: &str) -> Option<Initial> {
        Initial::ALL.into_iter().find(|i| i.key() == s)
    }
}

/// When a batch of changes waits for an OK.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Prompts {
    Always,
    Threshold,
    Never,
}

impl Prompts {
    pub const ALL: [Prompts; 3] = [Prompts::Always, Prompts::Threshold, Prompts::Never];
    pub fn label(self) -> &'static str {
        match self {
            Prompts::Always => "Always",
            Prompts::Threshold => "Over threshold",
            Prompts::Never => "Never",
        }
    }
}

/// When a batch that deletes instances waits for an OK, whatever Display
/// Prompts says.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum AskDeletes {
    Always,
    Live,
    Never,
}

impl AskDeletes {
    pub const ALL: [AskDeletes; 3] = [AskDeletes::Always, AskDeletes::Live, AskDeletes::Never];
    pub fn label(self) -> &'static str {
        match self {
            AskDeletes::Always => "Always",
            AskDeletes::Live => "In live places",
            AskDeletes::Never => "Never",
        }
    }
    /// Whether a batch with deletes asks, here.
    pub fn asks(self, live: bool) -> bool {
        match self {
            AskDeletes::Always => true,
            AskDeletes::Live => live,
            AskDeletes::Never => false,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum LogLevel {
    Error,
    Warn,
    Info,
    Verbose,
}

impl LogLevel {
    pub const ALL: [LogLevel; 4] = [LogLevel::Error, LogLevel::Warn, LogLevel::Info, LogLevel::Verbose];
    pub fn label(self) -> &'static str {
        match self {
            LogLevel::Error => "Errors",
            LogLevel::Warn => "Warnings",
            LogLevel::Info => "Info",
            LogLevel::Verbose => "Verbose",
        }
    }
}

/// A project attached to a place, to attach again when it opens.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Remembered {
    /// "place:<id>" (a Roblox or live Team Create place) or "file:<path>".
    pub place: String,
    pub project: PathBuf,
    /// The place's name when it was remembered.
    #[serde(default)]
    pub label: String,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct FileSyncSettings {
    pub initial: Initial,
    /// Attach the remembered project when its place opens (Auto Connect).
    pub auto_attach: bool,
    /// Live Hydrate: at attach, create instances for mapped files the place
    /// doesn't have (files added later are always created).
    pub hydrate: bool,
    /// Keep Unknowns in local places / in live Team Create places.
    pub keep_unknowns: bool,
    pub keep_unknowns_live: bool,
    /// Two-Way Sync: edits made in rustudio are written to the files.
    pub two_way: bool,
    /// Under Two-Way Sync: properties other than script Source too.
    pub syncback_props: bool,
    /// Under Two-Way Sync: only scripts and instances with scripts inside.
    pub code_only: bool,
    /// Scripts that came from files open in the OS default editor.
    pub open_in_editor: bool,
    pub prompts: Prompts,
    pub threshold: usize,
    /// Batches that delete ask first (its own switch: not part of prompts).
    pub ask_deletes: AskDeletes,
    /// Projects whose person confirmed "Never" in a live place (asked once).
    pub live_never_ok: Vec<PathBuf>,
    /// Live places: instances sent per batch, a quarter second apart (0: all
    /// at once). Big single-frame applies dropped the Team Create session
    /// (disconnect 0x104).
    pub live_batch: usize,
    pub diff_lines: usize,
    pub log_level: LogLevel,
    pub remembered: Vec<Remembered>,
}

impl Default for FileSyncSettings {
    fn default() -> Self {
        FileSyncSettings {
            initial: Initial::Preview,
            auto_attach: true,
            hydrate: true,
            keep_unknowns: false,
            keep_unknowns_live: true,
            two_way: false,
            syncback_props: false,
            code_only: true,
            open_in_editor: false,
            prompts: Prompts::Threshold,
            threshold: 5,
            ask_deletes: AskDeletes::Live,
            live_never_ok: Vec::new(),
            live_batch: 40,
            diff_lines: 3000,
            log_level: LogLevel::Info,
            remembered: Vec::new(),
        }
    }
}

/// The settings file, when this run keeps settings.
pub fn path() -> Option<PathBuf> {
    if let Some(p) = std::env::var_os("RUSTUDIO_FILE_SYNC_SETTINGS") {
        return Some(PathBuf::from(p));
    }
    Some(dirs::config_dir()?.join("rustudio").join("file-sync.json"))
}

impl FileSyncSettings {
    pub fn load(file: Option<&std::path::Path>) -> FileSyncSettings {
        file.and_then(|p| std::fs::read_to_string(p).ok())
            .and_then(|t| match serde_json::from_str::<FileSyncSettings>(&t) {
                Ok(s) => Some(s),
                Err(e) => {
                    eprintln!("rustudio: ignoring the File Sync settings ({e})");
                    None
                }
            })
            .unwrap_or_default()
    }

    pub fn save(&self, file: &std::path::Path) -> Result<(), String> {
        if let Some(dir) = file.parent() {
            std::fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
        }
        let text = serde_json::to_string_pretty(self).map_err(|e| e.to_string())?;
        let tmp = file.with_extension("json.tmp");
        std::fs::write(&tmp, text).map_err(|e| format!("{}: {e}", tmp.display()))?;
        std::fs::rename(&tmp, file).map_err(|e| format!("{}: {e}", file.display()))
    }

    pub fn remembered_for(&self, place: &str) -> Option<&Remembered> {
        self.remembered.iter().find(|r| r.place == place)
    }

    /// Remembers (or forgets, with None) the project for a place.
    pub fn remember(&mut self, place: &str, project: Option<(PathBuf, String)>) {
        self.remembered.retain(|r| r.place != place);
        if let Some((project, label)) = project {
            self.remembered.push(Remembered { place: place.to_string(), project, label });
        }
    }

    /// Whether a batch of `n` changes with `deletes` deletes waits for an
    /// OK: Display Prompts, then Ask Before Deletes. Nothing else overrides
    /// the person's choice (a live place included).
    pub fn asks(&self, n: usize, deletes: usize, live: bool) -> Option<super::Reason> {
        use super::Reason;
        match self.prompts {
            Prompts::Always => return Some(Reason::Always),
            Prompts::Threshold if n > self.threshold => return Some(Reason::Threshold),
            _ => {}
        }
        (deletes > 0 && self.ask_deletes.asks(live)).then_some(Reason::Deletes)
    }

    pub fn keep_unknowns_for(&self, live: bool) -> bool {
        if live { self.keep_unknowns_live } else { self.keep_unknowns }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_and_the_file_round_trip() {
        let s = FileSyncSettings::load(None);
        assert_eq!(s.initial, Initial::Preview);
        assert!(s.auto_attach && s.hydrate && !s.two_way && !s.keep_unknowns && s.keep_unknowns_live);
        assert_eq!((s.prompts, s.threshold, s.ask_deletes, s.live_batch), (Prompts::Threshold, 5, AskDeletes::Live, 40));
        let dir = std::env::temp_dir().join(format!("rustudio-filesync-settings-{}", std::process::id()));
        let file = dir.join("file-sync.json");
        let mut s = s;
        s.two_way = true;
        s.remember("place:1", Some(("/p/default.project.json".into(), "P".into())));
        s.remember("place:2", Some(("/q/default.project.json".into(), "Q".into())));
        s.remember("place:1", Some(("/r/default.project.json".into(), "R".into())));
        s.save(&file).unwrap();
        let back = FileSyncSettings::load(Some(&file));
        assert_eq!(back, s);
        assert_eq!(back.remembered_for("place:1").unwrap().project, PathBuf::from("/r/default.project.json"));
        assert_eq!(back.remembered.len(), 2);
        std::fs::write(&file, r#"{"threshold": 9, "future": true}"#).unwrap();
        let partial = FileSyncSettings::load(Some(&file));
        assert!(partial.threshold == 9 && partial.hydrate);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn prompts_follow_the_settings_in_live_places_too() {
        use super::super::Reason;
        let mut s = FileSyncSettings::default();
        // Over the threshold asks, live or not; under it doesn't.
        assert_eq!(s.asks(6, 0, true), Some(Reason::Threshold));
        assert_eq!(s.asks(3, 0, true), None);
        // Never is never, in a live place too.
        s.prompts = Prompts::Never;
        assert_eq!(s.asks(500, 0, true), None);
        // Deletes are their own visible switch (default: live places only).
        assert_eq!(s.asks(1, 1, true), Some(Reason::Deletes));
        assert_eq!(s.asks(1, 1, false), None);
        s.ask_deletes = AskDeletes::Never;
        assert_eq!(s.asks(500, 50, true), None);
        s.ask_deletes = AskDeletes::Always;
        assert_eq!(s.asks(1, 1, false), Some(Reason::Deletes));
        s.prompts = Prompts::Always;
        assert_eq!(s.asks(1, 0, false), Some(Reason::Always));
    }
}
