//! The in-game menu's settings, kept between playtests and runs, as
//! Roblox keeps UserGameSettings per user: Volume (MasterVolume), Camera
//! Sensitivity (MouseSensitivity) and Camera Inverted (CameraYInverted).
//! Without this every Play started at full volume again.
//!
//! `$XDG_CONFIG_HOME/rustudio/game-settings.json`. Only runs a person
//! drives read or write it (`Harness::plain`): tests and harness runs keep
//! the defaults and never touch the user's file.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct GameSettings {
    /// MasterVolume, 0..1.
    pub volume: Option<f32>,
    /// MouseSensitivity.
    pub sensitivity: Option<f64>,
    pub inverted: Option<bool>,
}

fn path() -> std::path::PathBuf {
    crate::paths::config().join("rustudio").join("game-settings.json")
}

pub fn load() -> GameSettings {
    std::fs::read_to_string(path()).ok().and_then(|t| serde_json::from_str(&t).ok()).unwrap_or_default()
}

/// Changes the saved settings (read, change, write back).
pub fn update(f: impl FnOnce(&mut GameSettings)) {
    let mut s = load();
    let before = s;
    f(&mut s);
    if s == before {
        return;
    }
    let p = path();
    if let Some(dir) = p.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    if let Ok(text) = serde_json::to_string_pretty(&s) {
        // Written beside it, then renamed: a crash mid-write keeps the old.
        let tmp = p.with_extension("json.tmp");
        if std::fs::write(&tmp, text).is_ok() {
            let _ = std::fs::rename(&tmp, &p);
        }
    }
}

impl GameSettings {
    /// Client Luau that puts the saved values on UserGameSettings.
    pub fn client_lua(&self) -> Vec<String> {
        let ugs = "UserSettings():GetService(\"UserGameSettings\")";
        let mut v = Vec::new();
        if let Some(s) = self.sensitivity.filter(|s| s.is_finite()) {
            v.push(format!("{ugs}.MouseSensitivity = {s}"));
        }
        if let Some(f) = self.volume.filter(|f| f.is_finite()) {
            v.push(format!("{ugs}.MasterVolume = {}", f.clamp(0.0, 1.0)));
        }
        if let Some(on) = self.inverted {
            v.push(format!("{ugs}.CameraYInverted = {on}"));
        }
        v
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trips_and_builds_the_client_lua() {
        let s = GameSettings { volume: Some(0.2), sensitivity: Some(0.37), inverted: Some(true) };
        let back: GameSettings = serde_json::from_str(&serde_json::to_string(&s).unwrap()).unwrap();
        assert_eq!(back, s);
        // Older / partial files load with the rest unset.
        let partial: GameSettings = serde_json::from_str(r#"{"volume":0.3}"#).unwrap();
        assert_eq!(partial, GameSettings { volume: Some(0.3), ..Default::default() });
        let ugs = "UserSettings():GetService(\"UserGameSettings\")";
        assert_eq!(
            s.client_lua(),
            [
                format!("{ugs}.MouseSensitivity = 0.37"),
                format!("{ugs}.MasterVolume = 0.2"),
                format!("{ugs}.CameraYInverted = true"),
            ]
        );
        assert!(GameSettings::default().client_lua().is_empty());
    }
}
