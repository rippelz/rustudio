//! Keep the paint and hit-test data of inactive docked panels across viewport
//! redraws. A panel runs its normal widgets as soon as it needs input or has
//! something new to display.

use std::sync::Arc;

use egui::{Context, Event, Id, Rect, Style, Ui, UiSnapshot, Vec2};
use rbx_dom_weak::types::Ref;
use rustudio_core::doc::Mark;
use rustudio_engine::runtime::{RunMode, Side};

use crate::app::{Editor, Tab};

#[derive(Default)]
pub(crate) struct Panels {
    explorer: Option<Panel>,
    properties: Option<Panel>,
    output: Option<Panel>,
}

struct Panel {
    state: State,
    geometry: Geometry,
    fonts: Fonts,
    style: Arc<Style>,
    content: Rect,
    paint: UiSnapshot,
    rows: usize,
}

#[derive(PartialEq)]
enum State {
    Explorer {
        doc: u64,
        tree: u64,
        selection: Vec<Ref>,
        expanded: u64,
        filter: String,
        play_view: Option<(RunMode, Side)>,
    },
    Properties {
        doc: u64,
        tree: u64,
        selection: Vec<Ref>,
        filter: String,
        viewport: Vec2,
        title: String,
        journal: u64,
    },
    Output {
        lines: usize,
        generation: u64,
        row_height: f32,
        wrap_text: bool,
    },
}

#[derive(PartialEq)]
struct Geometry {
    id: Id,
    layer: egui::LayerId,
    area: Rect,
    clip: Rect,
    enabled: bool,
    focus: Option<Id>,
    /// (Luau Panic Mode translates the text: a different paint)
    panic: bool,
}

struct Fonts {
    scale: f32,
    size: [usize; 2],
    fill: f32,
    defs: (usize, usize, usize),
}

impl Fonts {
    fn current(ctx: &Context) -> Self {
        let (size, fill, defs) = ctx.fonts(|f| {
            let d = f.definitions();
            let entries: usize = d.families.values().map(Vec::len).sum();
            (
                f.font_image_size(),
                f.font_atlas_fill_ratio(),
                (d.font_data.len(), d.families.len(), entries),
            )
        });
        Self {
            scale: ctx.pixels_per_point(),
            size,
            fill,
            defs,
        }
    }

    fn reusable(&self, now: &Self) -> bool {
        self.scale == now.scale
            && self.size == now.size
            && self.fill <= now.fill
            && self.defs == now.defs
    }
}

impl Editor {
    pub(crate) fn docked_panel(&mut self, ui: &mut Ui, tab: Tab, draw: fn(&mut Self, &mut Ui)) {
        let mut saved = std::mem::take(self.panel_slot(tab));
        let ctx = ui.ctx().clone();
        let area = ui.max_rect();
        let focus = ctx.memory(|m| m.focused()).filter(|&id| {
            ctx.read_response(id)
                .is_some_and(|response| area.contains(response.rect.center()))
        });
        let geometry = Geometry {
            id: ui.id(),
            layer: ui.layer_id(),
            area,
            clip: ui.clip_rect(),
            enabled: ui.is_enabled(),
            focus,
            panic: crate::luau_panic::active(),
        };
        let editing_text =
            focus.is_some_and(|id| egui::text_edit::TextEditState::load(&ctx, id).is_some());
        if !editing_text && self.panel_inactive(&ctx, area, tab) {
            let fonts = Fonts::current(&ctx);
            if let Some(panel) = saved.as_mut() {
                if panel.geometry == geometry
                    && panel.fonts.reusable(&fonts)
                    && (Arc::ptr_eq(&panel.style, ui.style()) || panel.style == *ui.style())
                    && self.panel_unchanged(&mut panel.state)
                {
                    let _p = rustudio_core::bench::scope("panel_replay");
                    ctx.replay_ui_snapshot(&panel.paint);
                    ui.expand_to_include_rect(panel.content);
                    *self.panel_rows(tab) = panel.rows;
                    *self.panel_slot(tab) = saved;
                    return;
                }
            }
            let style = ui.style().clone();
            let start = ctx.begin_ui_snapshot(ui.layer_id());
            draw(self, ui);
            if let Some(paint) = ctx.end_ui_snapshot(start) {
                *self.panel_slot(tab) = Some(Panel {
                    state: self.panel_state(tab),
                    geometry,
                    fonts: Fonts::current(&ctx),
                    style,
                    content: ui.min_rect(),
                    paint,
                    rows: *self.panel_rows(tab),
                });
            }
        } else {
            draw(self, ui);
        }
    }

    fn panel_inactive(&self, ctx: &Context, area: Rect, tab: Tab) -> bool {
        // A pointer press, keyboard action, or active control can change
        // state even outside the panel. Scroll inertia and animations that
        // continue on their own prevent capture by requesting a repaint.
        !egui::Popup::is_any_open(ctx)
            && !crate::explorer::explorer_drag_active(ctx)
            && ctx.input(|i| {
                let area = area.expand(4.0);
                !i.pointer.latest_pos().is_some_and(|p| area.contains(p))
                    && !(i.pointer.any_down()
                        && i.pointer.press_origin().is_some_and(|p| area.contains(p)))
                    && i.events.iter().all(|event| {
                        matches!(
                            event,
                            Event::PointerMoved(_)
                                | Event::MouseMoved(_)
                                | Event::PointerGone
                                | Event::MouseWheel { .. }
                                | Event::Screenshot { .. }
                        )
                    })
            })
            && match tab {
                Tab::Explorer => {
                    self.explorer.renaming.is_none() && self.explorer.scroll_to.is_none()
                }
                Tab::Properties => !self.properties_interacting(),
                Tab::Output => true,
                _ => false,
            }
    }

    fn panel_unchanged(&self, state: &mut State) -> bool {
        let doc = &self.game.doc;
        match state {
            State::Explorer {
                doc: id,
                tree,
                selection,
                expanded,
                filter,
                play_view,
            } => {
                *id == doc.id
                    && *tree == doc.tree_revision
                    && *selection == doc.selection
                    && *filter == self.explorer.filter
                    && *expanded == crate::explorer::expanded_key(&self.explorer)
                    && *play_view == self.play_view()
            }
            State::Properties {
                doc: id,
                tree,
                selection,
                viewport,
                title,
                filter,
                journal,
            } => {
                if *id != doc.id || *selection != doc.selection || *filter != self.props.filter {
                    return false;
                }
                let Some(&first) = selection.first() else {
                    return true;
                };
                if *tree != doc.tree_revision
                    || *viewport != self.game.viewport_rect.size()
                    || *title != doc.title
                {
                    return false;
                }
                // A GUI object shows values the GUI layout computes
                // (AbsolutePosition, AbsoluteSize, TextBounds…) from its
                // ancestors and the view, which no journal mark on the
                // selection covers: draw it fresh.
                if doc.class_of(first).is_some_and(|class| rustudio_core::doc::is_a(&class, "GuiBase2d")) {
                    return false;
                }
                let now = doc.journal_end();
                if *journal == now {
                    return true;
                }
                // Models can display the transform of a descendant or their
                // PrimaryPart. Other classes read values from the selection,
                // and resolve reference names using the tree revision above.
                if doc.class_of(first).is_some_and(|class| class == "Model")
                    || !doc.marks_since(*journal).is_some_and(|marks| {
                        !marks.iter().any(|mark| match *mark {
                            Mark::Prop(r, _) => selection.contains(&r),
                            Mark::Tree(_) => true,
                        })
                    })
                {
                    return false;
                }
                *journal = now;
                true
            }
            State::Output {
                lines,
                generation,
                row_height,
                wrap_text,
            } => {
                *lines == self.game.log.len()
                    && *generation == self.output_view.log_generation
                    && *row_height == crate::output_panel::line_h()
                    && *wrap_text == self.output_view.wrap_text
            }
        }
    }

    fn panel_state(&self, tab: Tab) -> State {
        let doc = &self.game.doc;
        match tab {
            Tab::Explorer => State::Explorer {
                doc: doc.id,
                tree: doc.tree_revision,
                selection: doc.selection.clone(),
                expanded: crate::explorer::expanded_key(&self.explorer),
                filter: self.explorer.filter.clone(),
                play_view: self.play_view(),
            },
            Tab::Properties => State::Properties {
                doc: doc.id,
                tree: doc.tree_revision,
                selection: doc.selection.clone(),
                filter: self.props.filter.clone(),
                viewport: self.game.viewport_rect.size(),
                title: doc.title.clone(),
                journal: doc.journal_end(),
            },
            Tab::Output => State::Output {
                lines: self.game.log.len(),
                generation: self.output_view.log_generation,
                row_height: crate::output_panel::line_h(),
                wrap_text: self.output_view.wrap_text,
            },
            _ => unreachable!("only Explorer, Properties and Output are retained"),
        }
    }

    fn play_view(&self) -> Option<(RunMode, Side)> {
        self.game
            .play
            .as_ref()
            .map(|play| (play.mode, play.showing))
    }

    fn panel_slot(&mut self, tab: Tab) -> &mut Option<Panel> {
        match tab {
            Tab::Explorer => &mut self.panel_cache.explorer,
            Tab::Properties => &mut self.panel_cache.properties,
            Tab::Output => &mut self.panel_cache.output,
            _ => unreachable!("only Explorer, Properties and Output are retained"),
        }
    }

    fn panel_rows(&mut self, tab: Tab) -> &mut usize {
        match tab {
            Tab::Explorer => &mut self.hud.explorer_rows,
            Tab::Properties => &mut self.hud.property_rows,
            Tab::Output => &mut self.hud.output_lines,
            _ => unreachable!("only Explorer, Properties and Output are retained"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn idle_snapshot_preserves_hit_targets_until_the_first_click() {
        let ctx = Context::default();
        let target = Rect::from_min_size(egui::pos2(40.0, 40.0), egui::vec2(80.0, 24.0));
        let mut cache = None;
        ctx.run_ui(egui::RawInput::default(), |ui| {
            let start = ctx.begin_ui_snapshot(ui.layer_id());
            ui.put(target, egui::Button::new("Select"));
            cache = ctx.end_ui_snapshot(start);
        })
        .textures_delta
        .clear();
        let cache = cache.expect("inactive button should be captured");
        ctx.run_ui(egui::RawInput::default(), |_| {
            ctx.replay_ui_snapshot(&cache);
        })
        .textures_delta
        .clear();

        let pos = target.center();
        let events = [
            Event::PointerMoved(pos),
            Event::PointerButton {
                pos,
                button: egui::PointerButton::Primary,
                pressed: true,
                modifiers: egui::Modifiers::NONE,
            },
            Event::PointerButton {
                pos,
                button: egui::PointerButton::Primary,
                pressed: false,
                modifiers: egui::Modifiers::NONE,
            },
        ];
        let mut clicked = false;
        ctx.run_ui(
            egui::RawInput {
                events: events.to_vec(),
                ..Default::default()
            },
            |ui| {
                clicked |= ui.put(target, egui::Button::new("Select")).clicked();
            },
        )
        .textures_delta
        .clear();
        assert!(clicked);
    }

    #[test]
    fn adding_font_invalidates_cached_panel_fonts() {
        let ctx = Context::default();
        let mut before = None;
        ctx.run_ui(egui::RawInput::default(), |ui| {
            before = Some(Fonts::current(ui.ctx()));
        })
        .textures_delta
        .clear();
        let before = before.expect("captured before fonts");

        let base = ctx.fonts(|f| f.definitions().font_data.values().next().unwrap().clone());
        ctx.add_font(egui::epaint::text::FontInsert::new(
            "test:extra",
            (*base).clone(),
            vec![egui::epaint::text::InsertFontFamily {
                family: egui::FontFamily::Name("test:extra".into()),
                priority: egui::epaint::text::FontPriority::Highest,
            }],
        ));
        let mut after = None;
        ctx.run_ui(egui::RawInput::default(), |ui| {
            after = Some(Fonts::current(ui.ctx()));
        })
        .textures_delta
        .clear();
        let after = after.expect("captured after fonts");
        assert!(
            !before.reusable(&after),
            "rebuilding the font atlas via ctx.add_font must invalidate cached panel snapshots"
        );
    }
}
