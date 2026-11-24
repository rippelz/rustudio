//! `--action ptr:VERB:TARGET@S`: the pointer moved / pressed / released on
//! a named place of the editor's UI, as a person's mouse would, so drags
//! between panels run through the panels' own drag-and-drop code (Explorer
//! rows, Toolbox cards, the 3D view, plugin widgets, the Toolbox dialogs).
//! Targets are found in last frame's layout, so an action reaches what is
//! on screen now.
//!
//! VERB: `move`, `down`, `up`, `click`. TARGET:
//! * `explorer:Workspace/Model/Part`: the middle of that Explorer row (its
//!   ancestors are expanded and the list scrolled when it isn't drawn: the
//!   action then does nothing, give it again a frame later);
//! * `card:ID` / `card:#N`: a Toolbox card (or audio row) by asset id, or
//!   the N-th drawn;
//! * `ask:LABEL`: a button of the Toolbox's open dialog (Insert, Workspace,
//!   StarterPack, Cancel ...);
//! * `widget:PLUGIN/WIDGET:X,Y`: widget pixels (as plugin-widget:), or
//!   `X%,Y%` of its size;
//! * `view:X,Y`: 3D view pixels;
//! * `face:Workspace/Part:Top`: the middle of a part's face (NormalId name)
//!   in the 3D view.
//!
//! Any target may end in `~DX,DY` (moved by that many points). Every
//! action prints `ptr: VERB TARGET -> x,y` (or why it found nothing) to
//! Output, which the checks read.

use egui::{Pos2, Vec2};
use rbx_dom_weak::types::Ref;

use crate::app::{Editor, Level};

fn xy(s: &str) -> Option<Vec2> {
    let (x, y) = s.split_once(',')?;
    Some(Vec2::new(x.trim().parse().ok()?, y.trim().parse().ok()?))
}

impl Editor {
    fn ptr_path(&self, path: &str) -> Option<Ref> {
        let mut r = self.game.doc.dom.root_ref();
        for name in path.split('/').filter(|n| !n.is_empty()) {
            let i = self.game.doc.dom.get_by_ref(r)?;
            r = i.children().iter().copied().find(|c| self.game.doc.dom.get_by_ref(*c).is_some_and(|c| c.name == name))?;
        }
        Some(r)
    }

    /// Where `target` is on screen now (Err: why not).
    fn ptr_target(&mut self, target: &str) -> Result<Pos2, String> {
        let (target, offset) = match target.rsplit_once('~') {
            Some((t, o)) => (t, xy(o).ok_or("bad ~DX,DY")?),
            None => (target, Vec2::ZERO),
        };
        let (kind, rest) = target.split_once(':').ok_or("want KIND:WHERE")?;
        let p = match kind {
            "explorer" => {
                let r = self.ptr_path(rest).ok_or("no such instance")?;
                match self.explorer.row_rects.iter().find(|(x, _)| *x == r) {
                    Some((_, rect)) => Pos2::new(rect.min.x + 60.0_f32.min(rect.width() / 2.0), rect.center().y),
                    None => {
                        let mut a = self.game.doc.dom.get_by_ref(r).map(|i| i.parent()).unwrap_or(Ref::none());
                        while let Some(i) = self.game.doc.dom.get_by_ref(a) {
                            self.explorer.expanded.insert(a);
                            a = i.parent();
                        }
                        self.explorer.scroll_to = Some(r);
                        return Err("row not drawn (expanded / scrolled to it: try again)".into());
                    }
                }
            }
            "card" => {
                let cards = &self.toolbox.ui.card_rects;
                let found = match rest.strip_prefix('#') {
                    Some(n) => cards.get(n.parse::<usize>().map_err(|_| "bad #N")?),
                    None => {
                        let id: u64 = rest.parse().map_err(|_| "bad asset id")?;
                        cards.iter().find(|(c, _)| *c == id)
                    }
                };
                found.ok_or("card not drawn")?.1.center()
            }
            "ask" => self.toolbox.ui.ask_buttons.iter().find(|(l, _)| l == rest).ok_or("no such dialog button")?.1.center(),
            "widget" => {
                let (who, at) = rest.rsplit_once(':').ok_or("want PLUGIN/WIDGET:X,Y")?;
                let rect = self.plugin_widget_rect(who).ok_or("widget not drawn")?;
                // (X%,Y%: that fraction of the widget's size)
                match at.strip_suffix('%').and_then(|a| a.split_once("%,")) {
                    Some((fx, fy)) => {
                        let f = |v: &str| v.trim().parse::<f32>().map_err(|_| "bad X%,Y%");
                        rect.min + Vec2::new(rect.width() * f(fx)? / 100.0, rect.height() * f(fy)? / 100.0)
                    }
                    None => rect.min + xy(at).ok_or("bad X,Y")?,
                }
            }
            "view" => {
                if !self.game.viewport_rect.is_finite() {
                    return Err("no 3D view".into());
                }
                self.game.viewport_rect.min + xy(rest).ok_or("bad X,Y")?
            }
            "face" => {
                let (path, face) = rest.rsplit_once(':').ok_or("want PATH:FACE")?;
                let r = self.ptr_path(path).ok_or("no such instance")?;
                let xf = rustudio_render::scene::xform_of(&self.game.doc, r).ok_or("not a part")?;
                let size = rustudio_render::scene::vec3_of(self.game.doc.prop(r, "Size")).ok_or("no Size")?;
                let n = match face {
                    "Right" => glam::Vec3::X,
                    "Top" => glam::Vec3::Y,
                    "Back" => glam::Vec3::Z,
                    "Left" => -glam::Vec3::X,
                    "Bottom" => -glam::Vec3::Y,
                    "Front" => -glam::Vec3::Z,
                    _ => return Err("face: Right, Top, Back, Left, Bottom or Front".into()),
                };
                let world = xf.pos + xf.rot * (n * size * 0.5);
                if !self.game.viewport_rect.is_finite() {
                    return Err("no 3D view".into());
                }
                self.game.cam.project(self.game.viewport_rect, world).filter(|p| self.game.viewport_rect.contains(*p)).ok_or("face not in view")?
            }
            _ => return Err(format!("unknown target kind {kind}")),
        };
        Ok(p + offset)
    }

    /// `ptr:VERB:TARGET` (see the module notes). False when `name` isn't one.
    pub fn harness_pointer(&mut self, name: &str, raw: &mut egui::RawInput) -> bool {
        let Some(rest) = name.strip_prefix("ptr:") else { return false };
        let Some((verb, target)) = rest.split_once(':') else {
            self.push_log(Level::Warn, format!("ptr: want ptr:VERB:TARGET, got {name}"));
            return true;
        };
        let presses: &[bool] = match verb {
            "move" => &[],
            "down" => &[true],
            "up" => &[false],
            "click" => &[true, false],
            _ => {
                self.push_log(Level::Warn, format!("ptr: unknown verb {verb}"));
                return true;
            }
        };
        match self.ptr_target(target) {
            Ok(p) => {
                raw.events.push(egui::Event::PointerMoved(p));
                for &pressed in presses {
                    raw.events.push(egui::Event::PointerButton { pos: p, button: egui::PointerButton::Primary, pressed, modifiers: egui::Modifiers::NONE });
                }
                self.push_log(Level::Info, format!("ptr: {verb} {target} -> {:.0},{:.0}", p.x, p.y));
            }
            Err(e) => self.push_log(Level::Warn, format!("ptr: {verb} {target}: {e}")),
        }
        true
    }
}
