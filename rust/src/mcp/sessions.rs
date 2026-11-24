//! Discovery: each window with MCP on keeps `<pid>.json` (pid, port, url,
//! title, place, mode) in `~/.cache/rustudio/mcp/sessions/`, so an agent
//! (session_info) or the stdio bridge (`rustudio mcp`) can tell the open
//! windows apart. Entries of processes that are gone are skipped (and
//! removed).

use std::path::PathBuf;

use serde_json::Value as J;

pub fn dir() -> Option<PathBuf> {
    if let Some(d) = std::env::var_os("RUSTUDIO_MCP_SESSIONS") {
        return Some(PathBuf::from(d));
    }
    Some(dirs::cache_dir()?.join("rustudio").join("mcp").join("sessions"))
}

fn alive(pid: u64) -> bool {
    #[cfg(target_os = "linux")]
    {
        std::path::Path::new(&format!("/proc/{pid}")).exists()
    }
    #[cfg(unix)]
    #[cfg(not(target_os = "linux"))]
    {
        // SAFETY: kill with signal 0 only checks the process exists.
        unsafe { libc::kill(pid as i32, 0) == 0 }
    }
    #[cfg(not(unix))]
    {
        let _ = pid;
        true
    }
}

/// The windows with MCP on, newest file first.
pub fn list() -> Vec<J> {
    let Some(dir) = dir() else { return Vec::new() };
    let Ok(rd) = std::fs::read_dir(&dir) else { return Vec::new() };
    let mut out: Vec<(std::time::SystemTime, J)> = Vec::new();
    for e in rd.flatten() {
        let p = e.path();
        if p.extension().is_none_or(|x| x != "json") {
            continue;
        }
        let Some(v) = std::fs::read_to_string(&p).ok().and_then(|t| serde_json::from_str::<J>(&t).ok()) else { continue };
        let pid = v.get("pid").and_then(|p| p.as_u64()).unwrap_or(0);
        if pid == 0 || !alive(pid) {
            let _ = std::fs::remove_file(&p);
            continue;
        }
        let t = e.metadata().and_then(|m| m.modified()).unwrap_or(std::time::UNIX_EPOCH);
        out.push((t, v));
    }
    out.sort_by_key(|b| std::cmp::Reverse(b.0));
    out.into_iter().map(|(_, v)| v).collect()
}
