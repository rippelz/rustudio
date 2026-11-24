use std::path::{Path, PathBuf};

use winit::platform::android::activity::AndroidApp;

fn set_env_if_unset(key: &str, value: impl AsRef<std::ffi::OsStr>) {
    if std::env::var_os(key).is_none() {
        std::env::set_var(key, value);
    }
}

/// `KEY=VALUE` lines (`#` comments) from a file, applied over the defaults.
fn apply_env_file(path: &Path) {
    let Ok(text) = std::fs::read_to_string(path) else { return };
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        if let Some((k, v)) = line.split_once('=') {
            let (k, v) = (k.trim(), v.trim());
            if !k.is_empty() {
                std::env::set_var(k, v);
            }
        }
    }
}

pub(super) const SHARED_STUDIO: &str = "/storage/emulated/0/RuStudio";

pub(super) fn setup(app: &AndroidApp) -> PathBuf {
    let data: PathBuf = app
        .internal_data_path()
        .unwrap_or_else(|| PathBuf::from("/data/data/com.rustudio.app/files"));
    let cache = data.parent().map(|p| p.join("cache")).unwrap_or_else(|| data.join("cache"));
    let _ = std::fs::create_dir_all(&data);
    let _ = std::fs::create_dir_all(&cache);

    // paths.rs reads HOME (and the XDG variables, which `dirs` also honours).
    std::env::set_var("HOME", &data);
    std::env::set_var("XDG_CONFIG_HOME", data.join(".config"));
    std::env::set_var("XDG_DATA_HOME", data.join(".local/share"));
    std::env::set_var("XDG_STATE_HOME", data.join(".local/state"));
    std::env::set_var("XDG_CACHE_HOME", data.join(".cache"));
    std::env::set_var("TMPDIR", &cache);
    if let Some(ext) = app.external_data_path() {
        std::env::set_var("RUSTUDIO_EXTERNAL_FILES", ext);
    }

    // TLS: Android keeps its CA bundle as hashed PEM files here.
    set_env_if_unset("SSL_CERT_DIR", "/system/etc/security/cacerts");
    // No self-update and no desktop keyring on a phone.
    set_env_if_unset("RUSTUDIO_NO_UPDATE_CHECK", "1");
    set_env_if_unset("RUST_BACKTRACE", "1");

    // Mobile quality profile (cheap wins on a Mali-class GPU). Each one is
    // the desktop build's own switch; rustudio_env.txt can override them.
    set_env_if_unset("RUSTUDIO_NO_GRASS_SHADOW", "1");

    // User overrides: app-private first, then the shared RuStudio folder.
    apply_env_file(&data.join("rustudio_env.txt"));
    apply_env_file(&Path::new(SHARED_STUDIO).join("rustudio_env.txt"));
    data
}
