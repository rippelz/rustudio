//! Toolbox commands (no window, usable headless with
//! a signed-in session):
//!
//! ```text
//! rustudio --toolbox-get <url> [--out FILE]        read-only GET with the session (recording replies)
//! ```
//! The editor-side deep links are `--action toolbox:search:<category>:<query>`
//! and `--action toolbox:insert:<assetId>` (src/toolbox/mod.rs).

/// Runs a toolbox command if `argv` has one. Some(exit code) when it did.
pub fn run(argv: &[String]) -> Option<i32> {
    let pos = |f: &str| argv.iter().position(|a| a == f);
    let arg = |f: &str| pos(f).and_then(|i| argv.get(i + 1)).cloned();
    if pos("--toolbox-get").is_none() {
        return None;
    }
    let r: Result<(), String> = (|| {
        let url = arg("--toolbox-get").ok_or("--toolbox-get URL")?;
        let signed_out = std::env::var_os("RUSTUDIO_NO_SESSION").is_some();
        let (st, body) = if signed_out {
            super::api::anon_get(&url).map_err(|e| e.to_string())?
        } else {
            let client = rustudio_account::Client::open(rustudio_account::AccountConfig::load().map_err(|e| e.0)?)
                .map_err(|e| e.describe())?;
            let (st, b) = client.session.fetch_bytes(&url, true).map_err(|e| e.0)?;
            (st, b)
        };
        match arg("--out") {
            Some(f) => {
                std::fs::write(&f, &body).map_err(|e| format!("{f}: {e}"))?;
                println!("{st} {} bytes -> {f}", body.len());
            }
            None => println!("{st} {}", String::from_utf8_lossy(&body)),
        }
        Ok(())
    })();
    Some(match r {
        Ok(()) => 0,
        Err(e) => {
            eprintln!("{e}");
            1
        }
    })
}
