//! Hyprland placement for panel windows (src/panels.rs).
//!
//! On Wayland a window can't learn or set where it is, and Hyprland tiles a
//! new window unless a rule says otherwise: a panel the user had floating
//! on another monitor came back tiled over half the screen. Under Hyprland
//! rustudio asks the compositor instead: a background thread reads
//! `hyprctl clients -j` once a second for its `rustudio-panel-*` windows
//! (floating, position, size, workspace), the layout saves that, and when
//! the window opens again `hyprctl dispatch` puts it back.

use std::collections::HashMap;
use std::process::Command;
use std::sync::{Mutex, OnceLock};
use std::time::Duration;

use serde::{Deserialize, Serialize};

/// Where Hyprland had a window: global layout pixels.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Placement {
    pub floating: bool,
    pub at: [i32; 2],
    pub size: [i32; 2],
    /// Workspace name (`2`, `special:magic`, …).
    pub workspace: String,
}

/// Running under Hyprland.
pub fn active() -> bool {
    std::env::var_os("HYPRLAND_INSTANCE_SIGNATURE").is_some_and(|v| !v.is_empty())
}

/// Hyprland tiles a new window unless a rule says otherwise, and
/// `restore` only floats it after the next poll (up to a second of a tiled
/// panel). A runtime window rule (not written to the user's config) floats
/// rustudio's panel windows as they open. Blocks once: call before the
/// first panel window is created.
pub fn float_panel_windows() {
    static DONE: OnceLock<()> = OnceLock::new();
    DONE.get_or_init(|| {
        if !active() {
            return;
        }
        let lua = r#"hl.window_rule({ name = "rustudio-panel-windows", match = { class = "^rustudio-panel-.*$" }, float = true })"#;
        let ok = Command::new("hyprctl")
            .args(["eval", lua])
            .output()
            .is_ok_and(|o| o.status.success() && String::from_utf8_lossy(&o.stdout).trim() == "ok");
        if !ok {
            // Before 0.55 (no Lua config).
            let _ = Command::new("hyprctl")
                .args(["keyword", "windowrulev2", "float,class:^(rustudio-panel-.*)$"])
                .output();
        }
    });
}

/// Before a panel window opens: a rule for its class with the size, place
/// and workspace it should open at (the saved placement, else the size it
/// asks for), so it doesn't map at a default size and jump a poll later.
/// `at` / `size` in Hyprland's layout pixels.
pub fn preplace(class: &str, saved: Option<&Placement>, size: [f32; 2], at: Option<[f32; 2]>) {
    if !active() || saved.is_some_and(|p| !p.floating) {
        return;
    }
    float_panel_windows();
    let (size, at, workspace) = match saved {
        Some(p) => ([p.size[0] as f32, p.size[1] as f32], Some([p.at[0] as f32, p.at[1] as f32]), p.workspace.clone()),
        None => (size, at, String::new()),
    };
    let mut fields = vec![
        format!("name = {:?}", format!("rustudio-{class}")),
        format!("match = {{ class = {:?} }}", format!("^{class}$")),
        "float = true".to_string(),
        format!("size = \"{} {}\"", size[0].round(), size[1].round()),
    ];
    match at {
        Some(a) => fields.push(format!("move = \"{} {}\"", a[0].round(), a[1].round())),
        None => fields.push("center = true".to_string()),
    }
    if !workspace.is_empty() {
        fields.push(format!("workspace = {:?}", format!("{workspace} silent")));
    }
    let lua = format!("hl.window_rule({{ {} }})", fields.join(", "));
    let _ = Command::new("hyprctl").args(["eval", &lua]).output();
}

/// The latest snapshot: class → (address, placement).
fn seen_map() -> &'static Mutex<HashMap<String, (String, Placement)>> {
    static SEEN: OnceLock<Mutex<HashMap<String, (String, Placement)>>> = OnceLock::new();
    SEEN.get_or_init(|| {
        if active() {
            let _ = std::thread::Builder::new().name("hyprland".into()).spawn(|| loop {
                if let Some(map) = poll() {
                    if let Ok(mut s) = seen_map().lock() {
                        *s = map;
                    }
                }
                std::thread::sleep(Duration::from_secs(1));
            });
        }
        Mutex::new(HashMap::new())
    })
}

fn poll() -> Option<HashMap<String, (String, Placement)>> {
    let out = Command::new("hyprctl").args(["clients", "-j"]).output().ok()?;
    parse_clients(&String::from_utf8_lossy(&out.stdout))
}

/// `hyprctl clients -j` → the rustudio panel windows.
fn parse_clients(json: &str) -> Option<HashMap<String, (String, Placement)>> {
    let v: serde_json::Value = serde_json::from_str(json).ok()?;
    let mut map = HashMap::new();
    for c in v.as_array()? {
        let class = c["class"].as_str().unwrap_or_default();
        if !class.starts_with("rustudio-panel-") || c["mapped"].as_bool() == Some(false) {
            continue;
        }
        let pair = |k: &str| -> Option<[i32; 2]> {
            let a = c[k].as_array()?;
            Some([a.first()?.as_i64()? as i32, a.get(1)?.as_i64()? as i32])
        };
        let (Some(at), Some(size), Some(addr)) = (pair("at"), pair("size"), c["address"].as_str()) else {
            continue;
        };
        let workspace = c["workspace"]["name"].as_str().unwrap_or_default().to_string();
        let floating = c["floating"].as_bool().unwrap_or(false);
        map.insert(class.to_string(), (addr.to_string(), Placement { floating, at, size, workspace }));
    }
    Some(map)
}

/// Where Hyprland has the window with this class now (as of the last poll).
pub fn seen(class: &str) -> Option<Placement> {
    if !active() {
        return None;
    }
    seen_map().lock().ok()?.get(class).map(|(_, p)| p.clone())
}

/// Puts the window with this class where `p` says, once it exists: false
/// while Hyprland hasn't reported it yet (call again later).
pub fn restore(class: &str, p: &Placement) -> bool {
    if !active() {
        return true;
    }
    let Some(addr) = seen_map().lock().ok().and_then(|s| s.get(class).map(|(a, _)| a.clone())) else {
        return false;
    };
    let lua = dispatches(&addr, p).join(" ; ");
    let legacy = legacy_dispatches(&addr, p).join(" ; ");
    let _ = std::thread::Builder::new().name("hyprland restore".into()).spawn(move || {
        let out = Command::new("hyprctl").args(["--batch", &lua]).output();
        // Before 0.55 dispatch took the old text form.
        if out.map_or(true, |o| String::from_utf8_lossy(&o.stdout).contains("error")) {
            let _ = Command::new("hyprctl").args(["--batch", &legacy]).output();
        }
    });
    true
}

/// Hyprland's Lua dispatchers (0.55+), each behind `dispatch` in a batch.
/// Resize comes before the move: the other way round the move is lost.
fn dispatches(addr: &str, p: &Placement) -> Vec<String> {
    let w = format!("window = \"address:{addr}\"");
    let mut v = Vec::new();
    if !p.workspace.is_empty() {
        v.push(format!("dispatch hl.dsp.window.move({{ workspace = {:?}, follow = false, {w} }})", p.workspace));
    }
    if p.floating {
        v.push(format!("dispatch hl.dsp.window.float({{ action = \"enable\", {w} }})"));
        v.push(format!("dispatch hl.dsp.window.resize({{ x = {}, y = {}, relative = false, {w} }})", p.size[0], p.size[1]));
        v.push(format!("dispatch hl.dsp.window.move({{ x = {}, y = {}, relative = false, {w} }})", p.at[0], p.at[1]));
    } else {
        v.push(format!("dispatch hl.dsp.window.float({{ action = \"disable\", {w} }})"));
    }
    v
}

/// The same for Hyprland before the Lua config.
fn legacy_dispatches(addr: &str, p: &Placement) -> Vec<String> {
    let a = format!("address:{addr}");
    let mut v = Vec::new();
    if !p.workspace.is_empty() {
        v.push(format!("dispatch movetoworkspacesilent {},{a}", p.workspace));
    }
    if p.floating {
        v.push(format!("dispatch setfloating {a}"));
        v.push(format!("dispatch resizewindowpixel exact {} {},{a}", p.size[0], p.size[1]));
        v.push(format!("dispatch movewindowpixel exact {} {},{a}", p.at[0], p.at[1]));
    } else {
        v.push(format!("dispatch settiled {a}"));
    }
    v
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_panel_windows_and_builds_the_restore() {
        let json = r#"[
            {"address":"0x1","class":"rustudio","mapped":true,"floating":false,"at":[8,8],"size":[2544,1376],"workspace":{"id":3,"name":"3"}},
            {"address":"0x2","class":"rustudio-panel-output","mapped":true,"floating":true,"at":[3767,79],"size":[537,801],"workspace":{"id":2,"name":"2"}}
        ]"#;
        let m = parse_clients(json).unwrap();
        assert_eq!(m.len(), 1, "only panel windows");
        let (addr, p) = &m["rustudio-panel-output"];
        assert_eq!(p, &Placement { floating: true, at: [3767, 79], size: [537, 801], workspace: "2".into() });
        // The forms checked against Hyprland 0.56 by hand (hyprctl --batch).
        assert_eq!(
            dispatches(addr, p),
            [
                r#"dispatch hl.dsp.window.move({ workspace = "2", follow = false, window = "address:0x2" })"#,
                r#"dispatch hl.dsp.window.float({ action = "enable", window = "address:0x2" })"#,
                r#"dispatch hl.dsp.window.resize({ x = 537, y = 801, relative = false, window = "address:0x2" })"#,
                r#"dispatch hl.dsp.window.move({ x = 3767, y = 79, relative = false, window = "address:0x2" })"#,
            ]
        );
        assert_eq!(legacy_dispatches(addr, p)[1], "dispatch setfloating address:0x2");
    }
}
