//! The MCP server's settings: on/off, port, token, the tool groups an agent
//! may use and which calls wait for the person's OK. Kept in
//! `$XDG_CONFIG_HOME/rustudio/mcp.json` (mode 0600: it holds the token) for
//! runs a person drives, like the Studio settings (`layout_store::enabled`);
//! `RUSTUDIO_MCP_SETTINGS=<file>` picks the file for any run. Test and
//! harness runs start from the defaults and only listen when
//! `RUSTUDIO_MCP=<port>` asks them to.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

pub const DEFAULT_PORT: u16 = 44755;
/// Ports tried after the configured one when it is taken (another rustudio
/// window listens there).
pub const PORT_TRIES: u16 = 20;

/// What a tool does, for the panel's permission switches.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Group {
    /// Reads the place, Output, screenshots, Team Create status.
    Read,
    /// Changes the edit DataModel: properties, scripts, instances, Luau in
    /// edit, projects and models applied, plugins, undo.
    Edit,
    /// Play / Run / Stop, Luau in a Play DataModel, input, the camera.
    Play,
    /// Opens places (files, Roblox, Team Create) and saves copies.
    Places,
    /// Writes that reach a live Team Create server: any edit while the open
    /// place is live, committing / discarding script drafts.
    Live,
}

impl Group {
    pub const ALL: [Group; 5] = [Group::Read, Group::Edit, Group::Play, Group::Places, Group::Live];

    pub fn key(self) -> &'static str {
        match self {
            Group::Read => "read",
            Group::Edit => "edit",
            Group::Play => "play",
            Group::Places => "places",
            Group::Live => "live",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Group::Read => "Read the place",
            Group::Edit => "Edit the place",
            Group::Play => "Play and input",
            Group::Places => "Open and save places",
            Group::Live => "Live Team Create writes",
        }
    }

    pub fn hint(self) -> &'static str {
        match self {
            Group::Read => "Explorer tree, properties, script source, Output, screenshots, Team Create status",
            Group::Edit => "Properties, scripts, instances, Luau in the edit DataModel, project folders and models, plugins, undo",
            Group::Play => "Play / Run / Stop, Luau on the Play server or client, keys and clicks, the camera",
            Group::Places => "Open a place file, a Roblox place or a Team Create session; save copies to files",
            Group::Live => "Edits while the open place is a live Team Create session reach every collaborator; committing and discarding script drafts",
        }
    }
}

/// Which calls wait for the person to allow them in the editor.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Confirm {
    /// Nothing asks.
    Never,
    /// Writes to a live Team Create place ask.
    Live,
    /// Every call that changes something asks.
    Writes,
}

impl Confirm {
    pub fn label(self) -> &'static str {
        match self {
            Confirm::Never => "Never",
            Confirm::Live => "Live Team Create writes",
            Confirm::Writes => "Every write",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Groups {
    pub read: bool,
    pub edit: bool,
    pub play: bool,
    pub places: bool,
    pub live: bool,
}

impl Default for Groups {
    fn default() -> Self {
        // Live writes are off until the person turns them on.
        Groups { read: true, edit: true, play: true, places: true, live: false }
    }
}

impl Groups {
    pub fn allows(&self, g: Group) -> bool {
        match g {
            Group::Read => self.read,
            Group::Edit => self.edit,
            Group::Play => self.play,
            Group::Places => self.places,
            Group::Live => self.live,
        }
    }

    pub fn get_mut(&mut self, g: Group) -> &mut bool {
        match g {
            Group::Read => &mut self.read,
            Group::Edit => &mut self.edit,
            Group::Play => &mut self.play,
            Group::Places => &mut self.places,
            Group::Live => &mut self.live,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct McpSettings {
    pub enabled: bool,
    pub port: u16,
    /// Calls must carry `Authorization: Bearer <token>`.
    pub require_token: bool,
    pub token: String,
    pub groups: Groups,
    pub confirm: Confirm,
}

impl Default for McpSettings {
    fn default() -> Self {
        McpSettings {
            enabled: false,
            port: DEFAULT_PORT,
            require_token: true,
            token: String::new(),
            groups: Groups::default(),
            confirm: Confirm::Live,
        }
    }
}

/// The settings file, when this run keeps settings.
pub fn path() -> Option<PathBuf> {
    if let Some(p) = std::env::var_os("RUSTUDIO_MCP_SETTINGS") {
        return Some(PathBuf::from(p));
    }
    Some(dirs::config_dir()?.join("rustudio").join("mcp.json"))
}

impl McpSettings {
    /// The saved settings (defaults when there are none), with a token made
    /// if there wasn't one.
    pub fn load(file: Option<&std::path::Path>) -> McpSettings {
        let mut s = file
            .and_then(|p| std::fs::read_to_string(p).ok())
            .and_then(|t| match serde_json::from_str::<McpSettings>(&t) {
                Ok(s) => Some(s),
                Err(e) => {
                    eprintln!("rustudio: ignoring the MCP settings ({e})");
                    None
                }
            })
            .unwrap_or_default();
        if s.token.len() < 16 {
            s.token = new_token();
        }
        s
    }

    pub fn save(&self, file: &std::path::Path) -> Result<(), String> {
        if let Some(dir) = file.parent() {
            std::fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
        }
        let text = serde_json::to_string_pretty(self).map_err(|e| e.to_string())?;
        let tmp = file.with_extension("json.tmp");
        std::fs::write(&tmp, text).map_err(|e| format!("{}: {e}", tmp.display()))?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let _ = std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o600));
        }
        std::fs::rename(&tmp, file).map_err(|e| format!("{}: {e}", file.display()))
    }
}

/// 32 hex digits from the system's random source (a hash of the time and
/// process where there is none).
pub fn new_token() -> String {
    use std::io::Read;
    let mut bytes = [0u8; 16];
    let random = std::fs::File::open("/dev/urandom").and_then(|mut f| f.read_exact(&mut bytes)).is_ok();
    if !random {
        use sha2::Digest;
        let seed = format!(
            "{:?}{}{:p}",
            std::time::SystemTime::now(),
            std::process::id(),
            &bytes as *const _
        );
        bytes.copy_from_slice(&sha2::Sha256::digest(seed.as_bytes())[..16]);
    }
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_keep_live_writes_off_and_make_a_token() {
        let s = McpSettings::load(None);
        assert!(!s.enabled);
        assert!(s.groups.read && s.groups.edit && !s.groups.live);
        assert_eq!(s.confirm, Confirm::Live);
        assert_eq!(s.token.len(), 32);
        assert_ne!(s.token, new_token());
    }

    #[test]
    fn settings_round_trip_through_the_file() {
        let dir = std::env::temp_dir().join(format!("rustudio-mcp-settings-{}", std::process::id()));
        let file = dir.join("mcp.json");
        let mut s = McpSettings::load(None);
        s.enabled = true;
        s.port = 45000;
        s.groups.live = true;
        s.confirm = Confirm::Writes;
        s.save(&file).unwrap();
        let back = McpSettings::load(Some(&file));
        assert_eq!(back, s);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(std::fs::metadata(&file).unwrap().permissions().mode() & 0o777, 0o600);
        }
        // Unknown or missing keys fall back to defaults.
        std::fs::write(&file, r#"{"enabled": true, "future": 1}"#).unwrap();
        let partial = McpSettings::load(Some(&file));
        assert!(partial.enabled && partial.port == DEFAULT_PORT && partial.token.len() == 32);
        let _ = std::fs::remove_dir_all(dir);
    }
}
