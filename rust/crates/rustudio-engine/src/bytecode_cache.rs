//! Compiled scripts kept by source: a Play session compiles every
//! ModuleScript the server and the client each require, and a place with
//! data modules of tens of megabytes (a large ported place: 150 MB of
//! Luau, one module 47 MB) spent seconds compiling the same text twice per
//! Play and again on every run. Bytecode of sources over MIN_MEMORY is kept
//! for the process; over MIN_DISK (on Android: 4 KB) also in
//! <cache>/bytecode/, keyed by the source and the compiler (settings and
//! its output for a probe). RUSTUDIO_NO_BYTECODE_CACHE=1 turns it off.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex, OnceLock};

/// (1 KB: a Play compiles most ModuleScripts twice, once a side; below
/// this the hash costs what the compiler does.)
const MIN_MEMORY: usize = 1024;
/// On a phone every launch is a cold process and the compiler is slower
/// (a fighting game's 685 scripts, 7 MB of Luau, compiled again each
/// launch otherwise): nearly everything the memory cache keeps goes to
/// disk too (a few MB, one file a script, in the app's cache directory).
const MIN_DISK: usize = if cfg!(target_os = "android") { 4 * 1024 } else { 256 * 1024 };
/// Bytecode held in memory at most (later sources compile as before).
const MEMORY_CAP: usize = if cfg!(target_os = "android") { 128 * 1024 * 1024 } else { 768 * 1024 * 1024 };

/// The compiler every script runs through: Studio's options (runtime.rs
/// Runtime::new explains each).
pub fn compiler() -> mlua::chunk::Compiler {
    mlua::chunk::Compiler::new()
        .set_optimization_level(1)
        .set_debug_level(1)
        .set_vector_ctor("Vector3.new")
        .set_vector_type("Vector3")
        // (Roblox's list. Imports now resolve when a script loads (vendor
        // lvmload.cpp resolveImportSafe), so chains through these, which
        // scripts change, must stay run-time lookups: `_G.Config` set by
        // another script after this one loaded.)
        .set_mutable_globals(["Game", "Workspace", "_G", "game", "plugin", "script", "shared", "workspace"])
}

fn enabled() -> bool {
    static ON: OnceLock<bool> = OnceLock::new();
    *ON.get_or_init(|| std::env::var_os("RUSTUDIO_NO_BYTECODE_CACHE").is_none_or(|v| v == "0"))
}

/// What the bytecode depends on besides the source: the compiler settings
/// and the compiler itself, fingerprinted by its output for a probe chunk
/// (a Luau update that changes code generation or the bytecode version
/// changes it; rebuilding rustudio alone does not).
fn salt() -> &'static [u8] {
    static SALT: OnceLock<Vec<u8>> = OnceLock::new();
    SALT.get_or_init(|| {
        const PROBE: &str = r##"local t = {1, 2.5, "s", true, nil, x = {y = vector.create(1, 2, 3)}, [10] = -0.0}
local function f(a, ...) local s = `{a}:{select("#", ...)}` for i, v in ipairs(t) do s ..= tostring(v) end return s, #t end
local v = Vector3.new(1, 2, 3) local g = game:GetService("Workspace") workspace.Name = script.Name
return f(1, 2, 3), math.floor(v.X // 2) % 3, (if t[1] then 1 else 2) + bit32.band(7, 3), string.format("%d", 4)"##;
        let mut h = blake3::Hasher::new();
        h.update(b"rustudio bytecode 2|O1 D1 Vector3.new Vector3 mutable globals _G|");
        h.update(&compiler().compile(PROBE).unwrap_or_default());
        h.finalize().as_bytes().to_vec()
    })
}

struct Memory {
    map: HashMap<[u8; 32], Arc<Vec<u8>>>,
    bytes: usize,
}

fn memory() -> &'static Mutex<Memory> {
    static M: OnceLock<Mutex<Memory>> = OnceLock::new();
    M.get_or_init(|| Mutex::new(Memory { map: HashMap::new(), bytes: 0 }))
}

fn dir() -> PathBuf {
    rustudio_core::assets::cache_dir().join("bytecode")
}

/// Bytecode for a script's source, compiled once per process (and kept on
/// disk for big sources). None: compile it the usual way (small, a syntax
/// error, or the cache is off).
pub fn get(source: &str) -> Option<Arc<Vec<u8>>> {
    // (A Source holding bytecode already is loaded as such by mlua.)
    if !enabled() || source.len() < MIN_MEMORY || source.as_bytes()[0] < 0x09 {
        return None;
    }
    let mut h = blake3::Hasher::new();
    h.update(salt());
    h.update(source.as_bytes());
    let key: [u8; 32] = *h.finalize().as_bytes();
    if let Some(b) = memory().lock().ok()?.map.get(&key) {
        return Some(b.clone());
    }
    let hex: String = key.iter().map(|b| format!("{b:02x}")).collect();
    let file = dir().join(format!("{hex}.luauc"));
    let disk = source.len() >= MIN_DISK;
    let from_disk = disk.then(|| std::fs::read(&file).ok()).flatten().filter(|b| b.first().is_some_and(|v| *v != 0));
    let bytes = match from_disk {
        Some(b) => b,
        None => {
            // A failed compile (or Luau's error bytecode, first byte 0) goes
            // the usual way, for the usual message.
            let b = compiler().compile(source).ok().filter(|b| b.first().is_some_and(|v| *v != 0))?;
            if disk {
                let _ = std::fs::create_dir_all(dir());
                let tmp = file.with_extension(format!("tmp{}", std::process::id()));
                if std::fs::write(&tmp, &b).is_ok() {
                    let _ = std::fs::rename(&tmp, &file);
                }
            }
            b
        }
    };
    let b = Arc::new(bytes);
    if let Ok(mut m) = memory().lock() {
        if m.bytes + b.len() <= MEMORY_CAP {
            m.bytes += b.len();
            m.map.insert(key, b.clone());
        }
    }
    Some(b)
}
