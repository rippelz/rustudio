//! Android has no Secret Service and the `keyring` crate has no store for it
//! here, so secrets (the session cookie, the publishing key) live in files
//! under the app's private folder (`$HOME/.config/rustudio/secrets`, mode
//! 0600; the folder is invisible to other apps). Same surface as the parts
//! of `keyring` that store.rs uses.

use std::path::PathBuf;

#[derive(Debug)]
pub enum Error {
    NoEntry,
    Io(std::io::Error),
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Error::NoEntry => write!(f, "no matching entry"),
            Error::Io(e) => write!(f, "{e}"),
        }
    }
}

pub struct Entry {
    path: PathBuf,
}

impl Entry {
    pub fn new(service: &str, user: &str) -> Result<Entry, Error> {
        let home = std::env::var_os("HOME").map(PathBuf::from).unwrap_or_else(|| PathBuf::from("."));
        let safe = |s: &str| s.chars().map(|c| if c.is_ascii_alphanumeric() || c == '-' { c } else { '_' }).collect::<String>();
        let path = home.join(".config/rustudio/secrets").join(format!("{}__{}", safe(service), safe(user)));
        Ok(Entry { path })
    }

    pub fn get_secret(&self) -> Result<Vec<u8>, Error> {
        match std::fs::read(&self.path) {
            Ok(v) => Ok(v),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Err(Error::NoEntry),
            Err(e) => Err(Error::Io(e)),
        }
    }

    pub fn set_secret(&self, secret: &[u8]) -> Result<(), Error> {
        use std::os::unix::fs::PermissionsExt;
        if let Some(dir) = self.path.parent() {
            std::fs::create_dir_all(dir).map_err(Error::Io)?;
        }
        std::fs::write(&self.path, secret).map_err(Error::Io)?;
        let _ = std::fs::set_permissions(&self.path, std::fs::Permissions::from_mode(0o600));
        Ok(())
    }

    pub fn get_password(&self) -> Result<String, Error> {
        let v = self.get_secret()?;
        String::from_utf8(v).map_err(|e| Error::Io(std::io::Error::new(std::io::ErrorKind::InvalidData, e)))
    }

    pub fn set_password(&self, password: &str) -> Result<(), Error> {
        self.set_secret(password.as_bytes())
    }

    pub fn delete_credential(&self) -> Result<(), Error> {
        match std::fs::remove_file(&self.path) {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Err(Error::NoEntry),
            Err(e) => Err(Error::Io(e)),
        }
    }
}
