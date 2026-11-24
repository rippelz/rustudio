//! Self-test step for panel windows (src/panels.rs): Output pops out into
//! its own OS window and keeps drawing (new lines included), a panel
//! closed and reopened docks beside the 3D view rather than over it, and
//! Output docks back where it was.

use super::{SelfTest, Step};
use crate::app::{Editor, Tab};
use crate::panels::DockMove;

#[derive(Default)]
pub(super) struct PopoutTest {
    started: u64,
    ok: bool,
    notes: Vec<String>,
}

impl SelfTest {
    /// Phase 117.
    pub(super) fn tick_popout(&mut self, f: u64, ed: &mut Editor) -> bool {
        if self.popout.started == 0 {
            self.popout = PopoutTest { started: f, ok: true, ..Default::default() };
            ed.ui.panel_requests.push((Tab::Output, Some(true)));
            return false;
        }
        let t = f - self.popout.started;
        let p = &mut self.popout;
        match t {
            2 => ed.ui.dock_moves.push(DockMove::PopOut(Tab::Output)),
            6 => ed.info("popout: a line written while Output is in its own window"),
            12 => {
                let popped = ed.ui.popped.iter().any(|(k, tabs)| k == "output" && tabs == &vec![Tab::Output]);
                let open = ed.ui.panels_open.contains("Output");
                let lines = ed.hud.output_lines;
                let native = ed.ui.popped_class == Some(egui::ViewportClass::Immediate);
                // Output sticks to the bottom. Asset polling can append a
                // warning after the marker and before this check.
                let last = ed.game.log.iter().rev().take(lines).any(|l| l.text.starts_with("popout:"));
                p.ok &= popped && open && lines > 0 && last && native;
                p.notes.push(format!(
                    "Output popped into window rustudio-panel-output: {popped} (windows {:?}, shown as {}), still checked in the Window menu: {open}, {lines} lines drawn there, newest line shown: {last}",
                    ed.ui.popped.iter().map(|(k, _)| k.as_str()).collect::<Vec<_>>(),
                    match ed.ui.popped_class {
                        Some(egui::ViewportClass::Immediate) => "its own OS window",
                        Some(egui::ViewportClass::EmbeddedWindow) => "a window inside the main one (backend without multiple windows)",
                        _ => "not shown",
                    }
                ));
                ed.ui.panel_requests.push((Tab::Explorer, Some(false)));
            }
            14 => ed.ui.panel_requests.push((Tab::Explorer, Some(true))),
            18 => {
                let alone = ed.ui.viewport_group == vec![Tab::Viewport];
                let back = ed.ui.panels_open.contains("Explorer");
                p.ok &= alone && back;
                p.notes.push(format!("Explorer closed and reopened: open {back}, the 3D view's tab group {:?}", ed.ui.viewport_group));
                ed.ui.dock_moves.push(DockMove::DockBack(Tab::Output));
            }
            24 => {
                let docked = ed.ui.popped.is_empty() && ed.ui.panels_open.contains("Output");
                let alone = ed.ui.viewport_group == vec![Tab::Viewport];
                p.ok &= docked && alone;
                p.notes.push(format!("Output docked back: {docked}, not over the 3D view: {alone}"));
                self.steps.push(Step { step: "panel-popout", passed: p.ok, detail: p.notes.join("; ") });
                self.r4.phase = 116;
            }
            _ => {}
        }
        false
    }
}
