//! Output lines: their level, one line of Output, and the optional copy of
//! every line to a file.

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Level {
    Info,
    Warn,
    Error,
}

pub struct LogLine {
    pub at: String,
    pub level: Level,
    pub text: String,
}

/// `RUSTUDIO_LOG_TEE=FILE`: every Output line is also appended to FILE as it
/// is logged (the Output keeps only its newest lines; per-frame traces such
/// as checks/humanoid-physics/feel need them all).
pub fn tee_log(text: &str) {
    use std::io::Write;
    static TEE: std::sync::OnceLock<Option<std::sync::Mutex<std::io::BufWriter<std::fs::File>>>> = std::sync::OnceLock::new();
    let tee = TEE.get_or_init(|| {
        let path = std::env::var_os("RUSTUDIO_LOG_TEE")?;
        let f = std::fs::OpenOptions::new().create(true).append(true).open(path).ok()?;
        Some(std::sync::Mutex::new(std::io::BufWriter::new(f)))
    });
    if let Some(f) = tee {
        if let Ok(mut f) = f.lock() {
            let _ = writeln!(f, "{text}");
            // Whole lines reach the file even if the run is killed.
            let _ = f.flush();
        }
    }
}
