//! Studio Settings across a restart (tests/studio_settings.rs runs this
//! twice on one settings file, RUSTUDIO_STUDIO_SETTINGS):
//! - `write`: File > Studio Settings, then real clicks and typing in the
//!   dialog (untick Scroll Towards Cursor Position, Camera Speed 3, the
//!   Selection page's Selection Line Thickness 4), Close, plus two values
//!   set the way settings() does (Maximum Output Lines 100, Script Editor
//!   Font 14 pt).
//! - `check`: a new process reads them back from the file, and they act:
//!   the camera flies twice as fast and the wheel zooms straight ahead,
//!   the selection outline is twice as thick, Output keeps 100 lines, the
//!   script editor's text is bigger.

use egui::{Event, Key, Modifiers, PointerButton, Pos2};
use glam::Vec3;
use serde_json::{json, Value};

use super::{SelfTest, CTRL};
use crate::app::Editor;
use crate::studio_settings::{self as ss, Hit, Val};

fn row(class: &str, prop: &str) -> usize {
    ss::index(class, prop).expect("settings row")
}

impl SelfTest {
    fn click_at(&mut self, p: Pos2) {
        self.press(p, PointerButton::Primary, Modifiers::NONE);
        self.release(p, PointerButton::Primary, Modifiers::NONE);
    }

    /// Replaces a line edit's text and presses Enter.
    fn type_value(&mut self, text: &str) {
        self.press_combo(CTRL, Key::A);
        self.modifiers = Modifiers::NONE;
        self.queue.push(Event::Text(text.into()));
        self.press_combo(Modifiers::NONE, Key::Enter);
    }

    fn click_hit(&mut self, ed: &Editor, h: Hit, notes: &mut Vec<String>) {
        match ed.ui.studio_settings.hit(h) {
            Some(r) => self.click_at(Pos2::new(r.min.x + 10.0, r.center().y)),
            None => notes.push(format!("{h:?} not on screen")),
        }
    }

    /// One frame of the regression; Some(result) when it's done.
    pub(super) fn studio_settings_regression(&mut self, ed: &mut Editor, phase: &str) -> Option<Value> {
        let f = self.frame;
        let mut notes = Vec::new();
        if phase == "write" {
            match f {
                // Filling the window, so every row this clicks is on screen.
                3 => ed.ui_action(&egui::Context::default(), "studio_settings_fill:Studio"),
                6 => self.click_hit(ed, Hit::Value(row("Studio", "Camera Zoom to Mouse Position")), &mut notes),
                9 => self.click_hit(ed, Hit::Value(row("Studio", "Camera Speed")), &mut notes),
                12 => self.type_value("3"),
                16 => self.click_hit(ed, Hit::Page(1), &mut notes),
                19 => self.click_hit(ed, Hit::Value(row("Studio", "Selection Line Thickness")), &mut notes),
                22 => self.type_value("4"),
                26 => {
                    // settings().Studio["Maximum Output Lines"] = 100 / the
                    // QFont editor: the same store.
                    ss::set("Studio", "Maximum Output Lines", Val::Num(100.0));
                    ss::set("Studio", "Font", Val::Font("Fira Mono".into(), 14.0));
                }
                28 => self.click_hit(ed, Hit::Close, &mut notes),
                32 => {
                    let open = ed.ui.studio_settings.open;
                    let got = json!({
                        "dialog_closed": !open,
                        "Camera Zoom to Mouse Position": format!("{:?}", ss::get("Studio", "Camera Zoom to Mouse Position")),
                        "Camera Speed": format!("{:?}", ss::get("Studio", "Camera Speed")),
                        "Selection Line Thickness": format!("{:?}", ss::get("Studio", "Selection Line Thickness")),
                    });
                    let ok = !open
                        && ss::get("Studio", "Camera Zoom to Mouse Position") == Some(Val::Bool(false))
                        && ss::get("Studio", "Camera Speed") == Some(Val::Num(3.0))
                        && ss::get("Studio", "Selection Line Thickness") == Some(Val::Num(4.0));
                    ss::flush();
                    return Some(json!({"test": "studio_settings_write", "passed": ok, "values": got, "notes": notes}));
                }
                _ => {}
            }
            if !notes.is_empty() {
                return Some(json!({"test": "studio_settings_write", "passed": false, "notes": notes}));
            }
            return None;
        }
        if f < 3 {
            return None;
        }
        // check: the values came back from the file…
        let loaded = [
            ("Camera Zoom to Mouse Position", Val::Bool(false)),
            ("Camera Speed", Val::Num(3.0)),
            ("Selection Line Thickness", Val::Num(4.0)),
            ("Maximum Output Lines", Val::Num(100.0)),
            ("Font", Val::Font("Fira Mono".into(), 14.0)),
        ];
        let mut ok = true;
        let mut values = serde_json::Map::new();
        for (p, want) in &loaded {
            let got = ss::get("Studio", p);
            ok &= got.as_ref() == Some(want);
            values.insert(p.to_string(), json!(format!("{got:?}")));
        }
        // …and act. Fly one second straight ahead: Camera Speed 3 is twice
        // the default 1.5.
        let mut cam = crate::camera::Camera::default();
        let start = cam.pos;
        cam.fly(Vec3::new(0.0, 0.0, 1.0), 1.0, false, 1.0);
        let flown = (cam.pos - start).length();
        let fly_ok = (flown - crate::mech::FLY_SPEED * 2.0).abs() < 0.01;
        // The wheel with Scroll Towards Cursor Position off: straight ahead,
        // whatever ray the cursor is on.
        let mut cam = crate::camera::Camera::default();
        let start = cam.pos;
        let fwd = cam.forward();
        cam.zoom_toward(Vec3::X, 1.0, false);
        let moved = cam.pos - start;
        let wheel_ok = (moved.normalize().dot(fwd) - 1.0).abs() < 1e-4 && (moved.length() - crate::mech::WHEEL_STEP).abs() < 0.01;
        // Selection outline: thickness 4 is twice the default 2.
        let outline = ss::selection_outline_px();
        let outline_ok = (outline - crate::mech::SELECTION_OUTLINE_PX * 2.0).abs() < 1e-4;
        // Output keeps at most 100 lines.
        for i in 0..300 {
            ed.info(format!("studio settings line {i}"));
        }
        let lines = ed.log.len();
        let output_ok = lines <= 100 && lines >= 80;
        // Script editor text: 14 pt.
        let font = ss::font_px("Studio", "Font");
        let font_ok = (font - 14.0 * 1.3).abs() < 1e-4;
        ok &= fly_ok && wheel_ok && outline_ok && output_ok && font_ok;
        Some(json!({
            "test": "studio_settings_check",
            "passed": ok,
            "values": values,
            "flown_studs_per_s": flown,
            "base_fly_speed": crate::mech::FLY_SPEED,
            "wheel_moved": [moved.x, moved.y, moved.z],
            "selection_outline_px": outline,
            "output_lines": lines,
            "script_font_px": font,
        }))
    }
}
