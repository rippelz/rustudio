//! What the window's cursor does for UserInputService.MouseBehavior while
//! playing (viewport.rs `update_game_mouse` sends the commands).
//!
//! Roblox rules this follows:
//! - LockCenter: the cursor is held and hidden; the game's mouse sits in the
//!   middle of the view (Mouse.Hit, GetMouseLocation) and the cursor image
//!   (UserInputService.MouseIcon, else PlayerMouse.Icon, else Roblox's own
//!   arrow) is drawn there at its own size, centred on that point. Shift lock
//!   sets PlayerMouse.Icon to rbxasset://textures/MouseLockedCursor.png
//!   (CameraModule/MouseLockController.lua, 32 × 32).
//! - Back to Default: the cursor is let go where it is held. It never jumps
//!   to the middle of the view. On Wayland a Locked pointer never moved, so
//!   letting go is all there is (no position hint: Hyprland warps the cursor
//!   to a lock's hint when the lock goes away). A Confined fallback (X11,
//!   Windows: no real lock) is held at the pre-lock spot and put back there.

use egui::{Pos2, Rect, Vec2};

/// Whether the window can truly lock the cursor (CursorGrab::Locked): winit
/// on Wayland (pointer-constraints) and macOS. X11 and Windows only confine.
/// winit picks Wayland whenever a Wayland socket is set.
pub fn platform_locks() -> bool {
    *LOCKS.get_or_init(detect_locks)
}

static LOCKS: std::sync::OnceLock<bool> = std::sync::OnceLock::new();

/// The window's real backend, once it exists (app.rs, from its raw window
/// handle): Some(true) on Wayland. Better than the environment guess (a
/// failed Wayland connection falls back to X11).
pub fn note_window(wayland: Option<bool>) {
    if let Some(w) = wayland {
        let _ = LOCKS.set(w || cfg!(target_os = "macos"));
    }
}

fn detect_locks() -> bool {
    if cfg!(target_os = "macos") {
        return true;
    }
    if cfg!(all(unix, not(target_os = "macos"))) {
        return wayland_session(
            std::env::var_os("WAYLAND_DISPLAY").is_some_and(|v| !v.is_empty()),
            std::env::var_os("WAYLAND_SOCKET").is_some_and(|v| !v.is_empty()),
        );
    }
    false
}

fn wayland_session(display: bool, socket: bool) -> bool {
    display || socket
}

/// Whether a Locked grab that still sees the pointer move may be taken for
/// "this platform can't lock" and turned into a Confined one. Never on a
/// platform that locks: on Wayland motion queued before the compositor
/// activated the lock (a fast flick as shift lock comes on) looked like
/// that, and the Confined grab that replaced the lock left the cursor free
/// in the window (Wayland can't re-centre an unlocked cursor) until the
/// window lost focus.
pub fn may_fall_back(platform_locks: bool) -> bool {
    !platform_locks
}

/// Roblox's own arrow while the mouse is locked and the game set no icon.
pub const LOCKED_ARROW: &str = "rbxasset://textures/Cursors/KeyboardMouse/ArrowFarCursor.png";

/// One cursor command for the window.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Cmd {
    /// Let the cursor go (CursorGrab::None).
    Release,
    /// Put the cursor at this point (window coordinates).
    MoveTo(Pos2),
}

/// The commands that end a game grab that began with the cursor at `start`.
/// A Locked grab only lets go: the cursor is still where it was. A Confined
/// one could move, so it goes back to `start` (after the release: winit only
/// moves an unlocked cursor on X11 / Windows, and the Wayland hint is never
/// sent, see the module note).
pub fn release(start: Pos2, confined: bool) -> Vec<Cmd> {
    let mut out = vec![Cmd::Release];
    if confined {
        out.push(Cmd::MoveTo(start));
    }
    out
}

/// Where a held cursor is pinned every frame: a Confined grab is kept at the
/// spot it began (both lock kinds); a Locked one is left alone (the
/// platform holds it, and moving it would only set a Wayland hint).
pub fn hold_at(start: Pos2, confined: bool) -> Option<Pos2> {
    confined.then_some(start)
}

/// Where the game's mouse is: the middle of the view while LockCenter holds
/// the cursor, else the pointer over the view.
pub fn game_pointer(lock_center: bool, view: Rect, hover: Option<Pos2>) -> Option<Pos2> {
    if lock_center {
        Some(view.center())
    } else {
        hover
    }
}

/// The rectangle a cursor image of `size` pixels takes with its middle at
/// `at` (Roblox draws mouse icons at their own size, centred on the mouse),
/// snapped to whole screen pixels so it stays sharp. Sizes are in GUI units
/// (rustudio lays a Roblox pixel out as one egui point, as its GUIs are).
pub fn icon_rect(at: Pos2, size: Vec2, pixels_per_point: f32) -> Rect {
    let ppp = pixels_per_point.max(0.01);
    let min = at - size * 0.5;
    let snap = |v: f32| (v * ppp).round() / ppp;
    Rect::from_min_size(Pos2::new(snap(min.x), snap(min.y)), size)
}

/// The cursor image to draw while LockCenter holds the mouse: the game's
/// (MouseIcon / Mouse.Icon), else Roblox's arrow; none while
/// MouseIconEnabled is false.
pub fn locked_icon(icon_enabled: bool, image: Option<&str>) -> Option<String> {
    icon_enabled.then(|| image.unwrap_or(LOCKED_ARROW).to_string())
}

/// Shift taps that came and went inside one frame, from egui's
/// ModifiersChanged events: the frame's end state (`shift_now`) never shows
/// them, yet shift lock (LeftShift through ContextActionService) must hear
/// each one. `shift_before` is the state at the end of the previous frame.
/// A press-and-release pair while shift is up counts as a tap.
pub fn shift_taps(shift_before: bool, events: &[egui::Event], shift_now: bool) -> u32 {
    if shift_now {
        // Held at the end: the frame's state shows the press already
        // (a quick re-press while held isn't a new toggle either way).
        return 0;
    }
    let mut down = shift_before;
    let mut taps: u32 = 0;
    for e in events {
        if let egui::Event::ModifiersChanged(m) = e {
            if m.shift && !down {
                down = true;
            } else if !m.shift && down {
                down = false;
                // A release of a press made this frame (one held from the
                // last frame shows as an ordinary key-up).
                taps += 1;
            }
        }
    }
    if shift_before {
        // The first release ended the held press, not a tap.
        taps = taps.saturating_sub(1);
    }
    taps
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mods(shift: bool) -> egui::Event {
        egui::Event::ModifiersChanged(egui::Modifiers { shift, ..Default::default() })
    }

    #[test]
    fn wayland_never_trades_its_lock_for_a_confine() {
        assert!(wayland_session(true, false) && wayland_session(false, true));
        assert!(!wayland_session(false, false));
        assert!(!may_fall_back(true));
        assert!(may_fall_back(false));
    }

    #[test]
    fn locked_release_never_moves_the_cursor() {
        let start = Pos2::new(120.0, 80.0);
        assert_eq!(release(start, false), vec![Cmd::Release]);
        assert_eq!(hold_at(start, false), None);
    }

    #[test]
    fn confined_release_goes_back_to_the_pre_lock_spot_not_the_centre() {
        let start = Pos2::new(120.0, 80.0);
        let r = release(start, true);
        assert_eq!(r, vec![Cmd::Release, Cmd::MoveTo(start)]);
        // Released first: winit moves a cursor only once it's unlocked.
        assert_eq!(r[0], Cmd::Release);
        assert_eq!(hold_at(start, true), Some(start));
    }

    #[test]
    fn lock_center_puts_the_game_mouse_in_the_middle() {
        let view = Rect::from_min_size(Pos2::new(200.0, 60.0), Vec2::new(800.0, 600.0));
        let hover = Some(Pos2::new(250.0, 90.0));
        assert_eq!(game_pointer(true, view, hover), Some(Pos2::new(600.0, 360.0)));
        assert_eq!(game_pointer(true, view, None), Some(Pos2::new(600.0, 360.0)));
        assert_eq!(game_pointer(false, view, hover), hover);
    }

    #[test]
    fn icon_is_centred_at_native_size() {
        // MouseLockedCursor.png is 32 × 32: its middle on the view's middle.
        let r = icon_rect(Pos2::new(600.0, 360.0), Vec2::new(32.0, 32.0), 1.0);
        assert_eq!(r, Rect::from_min_size(Pos2::new(584.0, 344.0), Vec2::new(32.0, 32.0)));
        // Scaled like the GUIs on a 2× display, still centred, snapped to
        // half points (whole screen pixels).
        let r = icon_rect(Pos2::new(600.3, 360.0), Vec2::new(32.0, 32.0), 2.0);
        assert_eq!(r.size(), Vec2::new(32.0, 32.0));
        assert_eq!(r.min, Pos2::new(584.5, 344.0));
        // An odd centre snaps to a whole pixel.
        let r = icon_rect(Pos2::new(600.25, 360.75), Vec2::new(32.0, 32.0), 1.0);
        assert_eq!(r.min, Pos2::new(584.0, 345.0));
    }

    #[test]
    fn locked_icon_falls_back_to_the_arrow() {
        assert_eq!(
            locked_icon(true, Some("rbxasset://textures/MouseLockedCursor.png")).as_deref(),
            Some("rbxasset://textures/MouseLockedCursor.png")
        );
        assert_eq!(locked_icon(true, None).as_deref(), Some(LOCKED_ARROW));
        assert_eq!(locked_icon(false, Some("x")), None);
    }

    #[test]
    fn shift_tap_inside_one_frame_is_heard() {
        assert_eq!(shift_taps(false, &[mods(true), mods(false)], false), 1);
        assert_eq!(shift_taps(false, &[mods(true), mods(false), mods(true), mods(false)], false), 2);
        // Held at the end of the frame: the state shows it.
        assert_eq!(shift_taps(false, &[mods(true)], true), 0);
        // Released after being held last frame: an ordinary key-up.
        assert_eq!(shift_taps(true, &[mods(false)], false), 0);
        // ... and pressed again and let go before the frame ended.
        assert_eq!(shift_taps(true, &[mods(false), mods(true), mods(false)], false), 1);
        // Other modifiers don't count.
        assert_eq!(shift_taps(false, &[mods(false)], false), 0);
    }
}
