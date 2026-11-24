//! The 3D viewport tab: rendering, camera input, picking, selection boxes and
//! the move / scale / rotate gizmos.

use egui::{Color32, PointerButton, Pos2, Rect, Sense, Stroke, Vec2};
use glam::{Mat3, Mat4, Vec3};
use rbx_dom_weak::types::{Ref, Variant};
use rbx_dom_weak::ustr;

use crate::app::{Editor, Tool};
use rustudio_core::math::{snap, Xform};
use rustudio_core::mech;
use rustudio_render::render::LineVertex;
use rustudio_render::scene::{self, ScenePart};
use rustudio_engine::theme;

pub const AXIS_COLORS: [Color32; 3] = [
    Color32::from_rgb(232, 76, 61),
    Color32::from_rgb(98, 204, 92),
    Color32::from_rgb(66, 136, 240),
];
const HOVER_COLOR: Color32 = Color32::from_rgb(255, 214, 92);

/// A gizmo handle: which axis, which end, which tool.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Handle {
    pub tool: Tool,
    pub axis: usize,
    pub sign: f32,
}

#[derive(Debug, Clone)]
pub struct GizmoDrag {
    undo_parts: Option<Vec<(Ref, Xform, Vec3)>>,
    undo_pivots: Option<Vec<crate::part_drag::PivotSnapshot>>,
    pivots: Vec<crate::part_drag::PivotSnapshot>,
    pub handle: Handle,
    origin: Vec3,
    axes: Mat3,
    start_param: f32,
    last_angle: f32,
    /// Accumulated screen-space turn in radians (unwrapped).
    turned: f32,
    center_px: Pos2,
    parts: Vec<(Ref, Xform, Vec3)>,
    /// The applied amount (studs or degrees) for the status readout.
    pub amount: f32,
    /// Collisions: the last amount that didn't overlap anything
    /// (`_lastGoodGeometricTransform`).
    last_good: f32,
}

pub fn fly_boost_at(held: f32) -> f32 {
    let ticks = held * mech::FLY_TICK_HZ;
    if ticks <= mech::FLY_RAMP_AFTER_TICKS {
        return 1.0;
    }
    (ticks / mech::FLY_RAMP_TICKS_PER_STUD).min(mech::FLY_STEP_CAP) / mech::FLY_BASE_STEP
}

/// World-space axis-aligned bounds of a part.
pub fn part_aabb(p: &ScenePart) -> (Vec3, Vec3) {
    let h = p.size * 0.5;
    let r = p.xf.rot;
    let ext = Vec3::new(
        r.x_axis.x.abs() * h.x + r.y_axis.x.abs() * h.y + r.z_axis.x.abs() * h.z,
        r.x_axis.y.abs() * h.x + r.y_axis.y.abs() * h.y + r.z_axis.y.abs() * h.z,
        r.x_axis.z.abs() * h.x + r.y_axis.z.abs() * h.y + r.z_axis.z.abs() * h.z,
    );
    (p.xf.pos - ext, p.xf.pos + ext)
}

fn box_corners(xf: &Xform, size: Vec3) -> [Vec3; 8] {
    let h = size * 0.5;
    let mut out = [Vec3::ZERO; 8];
    for (i, o) in out.iter_mut().enumerate() {
        let l = Vec3::new(
            if i & 1 == 0 { -h.x } else { h.x },
            if i & 2 == 0 { -h.y } else { h.y },
            if i & 4 == 0 { -h.z } else { h.z },
        );
        *o = xf.pos + xf.rot * l;
    }
    out
}

const BOX_EDGES: [(usize, usize); 12] = [
    (0, 1),
    (2, 3),
    (4, 5),
    (6, 7),
    (0, 2),
    (1, 3),
    (4, 6),
    (5, 7),
    (0, 4),
    (1, 5),
    (2, 6),
    (3, 7),
];

fn dist_to_segment(p: Pos2, a: Pos2, b: Pos2) -> f32 {
    let ab = b - a;
    let t = ((p - a).dot(ab) / ab.length_sq().max(1e-6)).clamp(0.0, 1.0);
    (a + ab * t - p).length()
}

/// World-space targets stay valid across mouse movements and camera changes.
#[derive(Default)]
pub(crate) struct MarqueeCentres {
    key: Option<(u64, u64)>,
    targets: Vec<(Ref, Vec3)>,
}

#[derive(Default)]
pub(crate) struct HoverPick {
    key: Option<HoverKey>,
    hit: Option<Ref>,
}

#[derive(PartialEq)]
struct HoverKey {
    doc: u64,
    journal: u64,
    rect: Rect,
    point: Pos2,
    view_proj: Mat4,
    raw: bool,
}

impl Editor {
    /// Fly speed multiplier over the base step, Studio's rule:
    /// the base step until 120 ticks held, then ticks / 240 studs, capped at 15
    /// (×30 base, reached after about 25 s).
    pub fn fly_boost(&self) -> f32 {
        fly_boost_at(self.fly_held)
    }

    /// Distance along a ray to the nearest visible part (locked ones count).
    pub fn ray_distance(&self, ray: &rustudio_core::math::Ray) -> Option<f32> {
        self.game.scene
            .parts
            .iter()
            .zip(&self.game.scene.bounds)
            .filter(|(sp, b)| {
                let oc = b.truncate() - ray.origin;
                let along = oc.dot(ray.dir);
                sp.draw.alpha > 0.02
                    && oc.length_squared() - along * along <= b.w * b.w
                    && along >= -b.w
            })
            .filter_map(|(sp, _)| ray.hit_obb(&sp.xf, sp.size))
            .filter(|t| *t > 0.0)
            .min_by(f32::total_cmp)
    }

    pub fn pick(&self, rect: Rect, p: Pos2, raw: bool) -> Option<Ref> {
        let _p = rustudio_core::bench::scope("viewport_pick");
        let ray = self.game.cam.ray(rect, p);
        let mut best: Option<(f32, Ref)> = None;
        let mut invisible_host: Option<(f32, Ref)> = None;
        for (sp, b) in self.game.scene.parts.iter().zip(&self.game.scene.bounds) {
            // Cheap bounding-sphere test first.
            let oc = b.truncate() - ray.origin;
            let along = oc.dot(ray.dir);
            if oc.length_squared() - along * along > b.w * b.w || along < -b.w {
                continue;
            }
            if sp.locked {
                continue;
            }
            if let Some(t) = ray.hit_obb(&sp.xf, sp.size) {
                // Invisible FX volumes are editor targets, independent of physics.
                // Prefer ordinary visible geometry so a large volume cannot
                // intercept every click on the scene inside it.
                let target = if sp.draw.alpha < 0.02 {
                    if !rustudio_render::effects::is_emitter_host(&self.game.doc, sp.r) { continue; }
                    &mut invisible_host
                } else {
                    &mut best
                };
                if target.is_none_or(|(bt, _)| t < bt) {
                    *target = Some((t, sp.r));
                }
            }
        }
        let r = best.or(invisible_host)?.1;
        Some(if raw { r } else { self.selection_target(r) })
    }

    fn hover_pick(&mut self, rect: Rect, point: Pos2, raw: bool) -> Option<Ref> {
        let key = HoverKey {
            doc: self.game.doc.id,
            journal: self.game.doc.journal_end(),
            rect,
            point,
            view_proj: self.game.cam.view_proj(rect),
            raw,
        };
        if self.game.is_playing() || self.hover_pick.key.as_ref() != Some(&key) {
            self.hover_pick = HoverPick { key: Some(key), hit: self.pick(rect, point, raw) };
        }
        self.hover_pick.hit
    }

    fn sync_hover_refs(&mut self, hover: Ref) {
        let key = (hover, self.game.doc.id, self.game.doc.journal_end());
        if self.hover_outline_refs.0 != Some(key) {
            let refs = std::iter::once(hover)
                .chain(self.game.doc.dom.descendants_of(hover).map(|i| i.referent()))
                .collect();
            self.hover_outline_refs = (Some(key), refs);
        }
    }

    /// Clicking a part inside a Model selects the outermost Model below Workspace.
    fn selection_target(&self, r: Ref) -> Ref {
        let stop = self.game.doc.default_parent();
        let mut target = r;
        let mut cur = r;
        loop {
            let Some(parent) = self.game.doc.dom.get_by_ref(cur).map(|i| i.parent()) else {
                break;
            };
            if parent.is_none() || parent == stop || parent == self.game.doc.dom.root_ref() {
                break;
            }
            if self.game.doc.class_of(parent).is_some_and(|c| c == "Model") {
                target = parent;
            }
            cur = parent;
        }
        target
    }

    fn gizmo_frame(&self) -> Option<(Vec3, Mat3, Vec3)> {
        let parts = self.selected_parts();
        if parts.is_empty() {
            return None;
        }
        if self.game.doc.selection.len() == 1 {
            if let Some(pivot) = crate::part_drag::editor_model_pivot(&self.game.doc, self.game.doc.selection[0]) {
                let (mn, mx) = self.selection_bounds()?;
                let half = (mx - pivot.pos).abs().max((mn - pivot.pos).abs());
                return Some((pivot.pos, if self.game.local_space { pivot.rot } else { Mat3::IDENTITY }, half));
            }
        }
        if parts.len() == 1 || self.game.local_space {
            let p = self.game.scene.get(parts[0])?;
            if self.game.local_space || self.tool == Tool::Scale {
                return Some((p.xf.pos, p.xf.rot, p.size * 0.5));
            }
        }
        let (mn, mx) = self.selection_bounds()?;
        Some(((mn + mx) * 0.5, Mat3::IDENTITY, (mx - mn) * 0.5))
    }

    /// Screen-space handle shapes for the active tool.
    fn handles(&self, rect: Rect) -> Vec<(Handle, Vec<Pos2>)> {
        if self.tool == Tool::Transform {
            return [Tool::Move, Tool::Scale, Tool::Rotate].into_iter()
                .flat_map(|tool| self.handles_for_tool(rect, tool)).collect();
        }
        self.handles_for_tool(rect, self.tool)
    }

    fn handles_for_tool(&self, rect: Rect, tool: Tool) -> Vec<(Handle, Vec<Pos2>)> {
        let mut out = Vec::new();
        if matches!(tool, Tool::Select | Tool::None) {
            return out;
        }
        let Some((origin, axes, half)) = self.gizmo_frame() else {
            return out;
        };
        let wpp = self.game.cam.world_per_px(rect, origin);
        for axis in 0..3 {
            let dir = axes.col(axis);
            match tool {
                Tool::Move | Tool::Scale => {
                    for sign in [1.0f32, -1.0] {
                        let base = origin + dir * sign * (half[axis] + 6.0 * wpp);
                        let len = if tool == Tool::Move {
                            mech::GIZMO_ARROW_PX * 0.55
                        } else {
                            24.0
                        };
                        let tip = base + dir * sign * len * wpp;
                        if let (Some(a), Some(b)) =
                            (self.game.cam.project(rect, base), self.game.cam.project(rect, tip))
                        {
                            out.push((Handle { tool, axis, sign }, vec![a, b]));
                        }
                    }
                }
                Tool::Rotate => {
                    let radius = mech::GIZMO_RING_PX * wpp;
                    let (u, v) = (axes.col((axis + 1) % 3), axes.col((axis + 2) % 3));
                    let pts: Vec<Pos2> = (0..=64)
                        .filter_map(|i| {
                            let a = i as f32 / 64.0 * std::f32::consts::TAU;
                            self.game.cam
                                .project(rect, origin + (u * a.cos() + v * a.sin()) * radius)
                        })
                        .collect();
                    if pts.len() > 2 {
                        out.push((
                            Handle {
                                tool,
                                axis,
                                sign: 1.0,
                            },
                            pts,
                        ));
                    }
                }
                Tool::Select | Tool::None | Tool::Transform => {}
            }
        }
        out
    }

    fn handle_at(&self, handles: &[(Handle, Vec<Pos2>)], p: Pos2) -> Option<Handle> {
        let mut best: Option<(f32, Handle)> = None;
        for (h, pts) in handles {
            let d = match h.tool {
                Tool::Scale => (pts[1] - p).length() - 4.0,
                _ => pts
                    .windows(2)
                    .map(|w| dist_to_segment(p, w[0], w[1]))
                    .fold(f32::MAX, f32::min),
            };
            if d < mech::GIZMO_HIT_PX && best.is_none_or(|(bd, _)| d < bd) {
                best = Some((d, *h));
            }
        }
        best.map(|b| b.1)
    }

    pub(crate) fn start_drag(&mut self, rect: Rect, handle: Handle, p: Pos2) {
        let Some((origin, axes, _)) = self.gizmo_frame() else {
            return;
        };
        let parts: Vec<(Ref, Xform, Vec3)> = self
            .selected_parts()
            .into_iter()
            .filter_map(|r| self.game.scene.get(r).map(|sp| (r, sp.xf, sp.size)))
            .collect();
        let ray = self.game.cam.ray(rect, p);
        let dir = axes.col(handle.axis) * handle.sign;
        let start_param = ray.closest_on_line(origin, dir).unwrap_or(0.0);
        let center_px = self.game.cam.project(rect, origin).unwrap_or(p);
        let last_angle = (p.y - center_px.y).atan2(p.x - center_px.x);
        self.drag = Some(GizmoDrag {
            undo_parts: None,
            undo_pivots: None,
            pivots: crate::part_drag::selected_model_pivots(&self.game.doc),
            handle,
            origin,
            axes,
            start_param,
            last_angle,
            turned: 0.0,
            center_px,
            parts,
            amount: 0.0,
            last_good: 0.0,
        });
    }

    pub(crate) fn update_drag(&mut self, rect: Rect, p: Pos2, symmetric: bool, uniform: bool) {
        let Some(drag) = self.drag.as_mut() else {
            return;
        };
        let h = drag.handle;
        let dir = drag.axes.col(h.axis) * h.sign;
        let ray = self.game.cam.ray(rect, p);
        // The wanted amount (studs or degrees), snapped.
        let wanted = match h.tool {
            Tool::Move | Tool::Scale => {
                let Some(t) = ray.closest_on_line(drag.origin, dir) else {
                    return;
                };
                snap(t - drag.start_param, if self.game.snap_move { self.game.move_inc } else { 0.0 })
            }
            Tool::Rotate => {
                let a = (p.y - drag.center_px.y).atan2(p.x - drag.center_px.x);
                let pi = std::f32::consts::PI;
                drag.turned += (a - drag.last_angle + pi).rem_euclid(2.0 * pi) - pi;
                drag.last_angle = a;
                let axis = drag.axes.col(h.axis);
                // Screen y points down, so a clockwise screen turn is a
                // counter-clockwise turn about an axis that faces the camera.
                let facing = axis.dot(self.game.cam.pos - drag.origin) > 0.0;
                let mut deg = drag.turned.to_degrees();
                if facing {
                    deg = -deg;
                }
                snap(deg, if self.game.snap_rotate { self.game.rotate_inc } else { 0.0 })
            }
            Tool::Select | Tool::None | Tool::Transform => return,
        };
        drag.amount = wanted;
        let drag = drag.clone();
        let make = |d: f32| gizmo_updates(&drag, dir, d, symmetric, uniform);
        // Collisions: stop short of overlapping (TransformHandlesImplementation
        // ._safelyTransformParts / ExtrudeHandles._binarySearchForGoodPartSize).
        let amount = if self.collisions {
            let eps = if h.tool == Tool::Rotate { mech::COLLISION_EPS.to_degrees() } else { mech::COLLISION_EPS };
            let hits = |d: f32| self.overlaps_others(&make(d));
            crate::part_drag::safe_amount(drag.last_good, wanted, eps, hits)
        } else {
            wanted
        };
        if let Some(d) = self.drag.as_mut() {
            d.last_good = amount;
        }
        let updates = make(amount);
        if h.tool != Tool::Scale {
            if let (Some((_, old, _)), Some((_, new, _))) = (drag.parts.first(), updates.first()) {
                let rot = new.rot * old.rot.transpose();
                crate::part_drag::apply_model_pivots(&mut self.game.doc, &drag.pivots, Xform { pos: new.pos - rot * old.pos, rot });
            }
        }
        for (r, xf, size) in updates {
            self.game.doc
                .set_prop_live(r, ustr("CFrame"), Variant::CFrame(xf.to_cframe()));
            if h.tool == Tool::Scale {
                self.game.doc
                    .set_prop_live(r, ustr("Size"), Variant::Vector3(rustudio_core::math::rv3(size)));
            }
        }
    }

    pub(crate) fn duplicate_handle_drag(&mut self, map: &std::collections::HashMap<Ref, Ref>) {
        let Some(d) = self.drag.as_mut() else { return };
        d.parts.retain_mut(|(r, _, _)| {
            if let Some(copy) = map.get(r) { *r = *copy; true } else { false }
        });
        d.pivots.retain_mut(|p| { if let Some(copy) = map.get(&p.r) { p.r = *copy; true } else { false } });
        d.undo_pivots = Some(crate::part_drag::pivot_undo_baseline(&self.game.doc, &d.pivots));
        d.undo_parts = Some(d.parts.iter().filter_map(|(r, _, _)| {
            let props = &self.game.doc.dom.get_by_ref(*r)?.properties;
            let Variant::CFrame(cf) = props.get(&ustr("CFrame"))? else { return None };
            let Variant::Vector3(size) = props.get(&ustr("Size"))? else { return None };
            Some((*r, Xform::from_cframe(cf), rustudio_core::math::v3(*size)))
        }).collect());
    }

    pub(crate) fn finish_drag(&mut self) {
        let Some(drag) = self.drag.take() else { return };
        let mut changes = Vec::new();
        for (r, xf, size) in drag.undo_parts.as_ref().unwrap_or(&drag.parts) {
            let old_cf = Some(Variant::CFrame(xf.to_cframe()));
            if let Some(new_cf) = self.game
                .doc
                .dom
                .get_by_ref(*r)
                .and_then(|i| i.properties.get(&ustr("CFrame")).cloned())
            {
                changes.push((*r, ustr("CFrame"), old_cf, new_cf));
            }
            if drag.handle.tool == Tool::Scale {
                if let Some(new_size) = self.game
                    .doc
                    .dom
                    .get_by_ref(*r)
                    .and_then(|i| i.properties.get(&ustr("Size")).cloned())
                {
                    changes.push((
                        *r,
                        ustr("Size"),
                        Some(Variant::Vector3(rustudio_core::math::rv3(*size))),
                        new_size,
                    ));
                }
            }
        }
        let label = match drag.handle.tool {
            Tool::Move => "Move",
            Tool::Scale => "Resize",
            Tool::Rotate => "Rotate",
            Tool::Select | Tool::None | Tool::Transform => "Edit",
        };
        changes.extend(crate::part_drag::pivot_changes(&self.game.doc, drag.undo_pivots.as_ref().unwrap_or(&drag.pivots)));
        self.game.doc.record_props(label, changes);
    }

    pub fn viewport_ui(&mut self, ui: &mut egui::Ui) {
        let rect = ui.available_rect_before_wrap();
        self.game.viewport_rect = rect;
        rustudio_render::grass_temporal::tick(&mut self.game.cam, ui.ctx());
        crate::mobile::note_view_rect(rect);
        let selector = Self::view_selector_rect(rect);
        let native_view_selector = self
            .builtin_tools
            .use_native(rustudio_core::rustudio_settings::BuiltinTool::ViewSelector)
            && self.ui.show_view_selector;
        let over_selector = native_view_selector
            && !self.game.is_playing()
            && ui.input(|i| i.pointer.hover_pos().is_some_and(|p| selector.contains(p)));
        let resp = ui.allocate_rect(rect, if over_selector { Sense::hover() } else { Sense::click_and_drag() });
        let ctx = ui.ctx().clone();
        let context_menu_open = resp.context_menu_opened();
        let dt = ctx.input(|i| i.stable_dt).min(0.1);

        if !self.game.is_playing() && self.game.cam.advance_view(dt) { ctx.request_repaint(); }

        // ---------------------------------------------------------- camera
        let (scroll, alt, ctrl, shift, delta) = ctx.input(|i| {
            (
                i.smooth_scroll_delta.y,
                i.modifiers.alt,
                i.modifiers.command,
                i.modifiers.shift,
                i.pointer.delta(),
            )
        });
        let _ = scroll;
        // While playing (the client's view), the mouse and keys belong to the
        // game: its camera scripts turn and zoom the view.
        let driving = self.game.play_game_input();
        let wheel = crate::game_view::wheel_notches(&ctx);
        let notches = wheel + crate::mobile::pinch_notches(&ctx);
        if !driving && (self.game.game_grab.is_some() || self.game.game_cursor_hidden) {
            self.game.release_game_mouse(&ctx);
        }
        if driving {
            let touch = crate::mobile::enabled();
            if touch {
                let look = crate::mobile::take_play_look();
                let zoom = crate::mobile::play_pinch_notches(&ctx) + if resp.hovered() { wheel } else { 0.0 };
                if self.game.update_touch_look(&ctx, look, zoom) {
                    ctx.request_repaint();
                }
            }
            if !touch || !cfg!(target_os = "android") && !ctx.input(|i| i.has_touch_screen()) {
                self.game.update_game_mouse(&ctx, &resp, delta, if touch { 0.0 } else { notches });
            }
        } else if !context_menu_open && !over_selector && resp.hovered() && notches != 0.0 {
            if let Some(p) = resp.hover_pos() {
                let ray = self.game.cam.ray(rect, p);
                let hit = self.ray_distance(&ray);
                self.game.cam.zoom_toward(ray.dir, notches, shift, &crate::studio_settings::camera_prefs());
                self.last_zoom = Some((ray.dir, hit));
            }
        }
        if !driving && !over_selector {
            let look = if cfg!(target_os = "android") {
                resp.dragged_by(PointerButton::Secondary).then_some(delta)
            } else {
                self.game.update_look_grab(&ctx, &resp, delta)
            };
            if let Some(d) = look {
                self.game.cam.rotate(d.x, d.y);
            }
            if resp.dragged_by(PointerButton::Middle) {
                self.game.cam.pan(delta.x, delta.y, &crate::studio_settings::camera_prefs());
            }
            if let Some(d) = crate::mobile::two_finger_pan(&ctx) {
                self.game.cam.pan(d.x, d.y, &crate::studio_settings::camera_prefs());
            }
            if alt && resp.dragged_by(PointerButton::Primary) && self.drag.is_none() {
                self.game.cam.orbit(delta.x, delta.y);
            }
        }
        // WASD / Q / E fly the camera wherever the mouse is (Studio), except
        // while a text field or the script editor has the keyboard, a game
        // TextBox is focused, the game has the input (Play), or Ctrl is held.
        let keyboard_ok = !driving
            && !context_menu_open
            && !ctx.egui_wants_keyboard_input()
            && self.game.gui.focus.is_none()
            && !self.plugin_ui.has_text_focus()
            && self.ui.insert_popup.is_none()
            && self.explorer.renaming.is_none()
            && !ctrl;
        let dt = self.game.fixed_dt.unwrap_or(dt);
        let mut moving = false;
        if keyboard_ok {
            let mut dir = Vec3::ZERO;
            use egui::Key;
            if self.game.phys.down(Key::W) {
                dir.z += 1.0;
            }
            if self.game.phys.down(Key::S) {
                dir.z -= 1.0;
            }
            if self.game.phys.down(Key::D) {
                dir.x += 1.0;
            }
            if self.game.phys.down(Key::A) {
                dir.x -= 1.0;
            }
            if self.game.phys.down(Key::E) {
                dir.y += 1.0;
            }
            if self.game.phys.down(Key::Q) {
                dir.y -= 1.0;
            }
            if dir != Vec3::ZERO {
                moving = true;
                self.game.cam.fly(dir, dt, shift, self.fly_boost(), &crate::studio_settings::camera_prefs());
                ctx.request_repaint();
            }
        }
        // Holding movement keys ramps the speed up. Studio clears its hold
        // counter as soon as no movement key is down, and while Shift is held
        //.
        if moving && !shift {
            self.fly_held += dt;
            self.fly_idle = 0.0;
        } else {
            self.fly_idle += dt;
            self.fly_held = 0.0;
        }

        // ---------------------------------------------------------- render
        let vf_snaps = rustudio_engine::gui::vf_snap_pending();
        {
            let _s = rustudio_core::bench::scope("vp_scene_sync");
            self.game.scene.sync(&mut self.game.doc);
            // The parts of the ViewportFrames the GUI drew last frame
            // (plus Toolbox detail previews, plugin widgets, and pending
            // ViewportFrame:CaptureSnapshotAsync requests).
            self.toolbox_vf = if self
                .builtin_tools
                .use_native(rustudio_core::rustudio_settings::BuiltinTool::Toolbox)
            {
                self.toolbox.preview.frames(&mut self.game.doc)
            } else {
                Default::default()
            };
            let mut roots = self.game.gui.viewport_frames().clone();
            roots.extend(self.toolbox_vf.keys().copied());
            self.plugin_ui.collect_viewport_frames(&mut roots);
            for &(_, vf) in &vf_snaps {
                roots.insert(vf);
            }
            self.game.vf_scene.set_viewport_frames(&self.game.doc, &roots);
            self.game.vf_scene.sync(&mut self.game.doc);
        }
        let pointer = resp.hover_pos();
        let handles = if pointer.is_some() || resp.drag_started_by(PointerButton::Primary) {
            self.handles(rect)
        } else {
            Vec::new()
        };
        let hover_handle = if self.drag.is_none() {
            pointer.and_then(|p| self.handle_at(&handles, p))
        } else {
            None
        };
        // (No hover outline while playing the game, as in Studio.)
        let in_play = self.game
            .play
            .as_ref()
            .is_some_and(|p| p.mode == rustudio_engine::runtime::RunMode::Play);
        let roblox_dragger_idle = self
            .builtin_tools
            .use_roblox(rustudio_core::rustudio_settings::BuiltinTool::Draggers)
            && self.tool == Tool::None;
        self.hover = if hover_handle.is_none()
            && self.drag.is_none()
            && self.marquee.is_none()
            && !in_play
            && !self.clean_view
            && !roblox_dragger_idle
        {
            pointer.and_then(|p| self.hover_pick(rect, p, alt))
        } else {
            None
        };

        self.game.sync_view_draws(&ctx);
        // Hover: the hovered part (or model's parts) drawn again, highlighted.
        let mut overlay = Vec::new();
        if let Some(h) = self.hover {
            self.sync_hover_refs(h);
            if let Some(rend) = self.game.renderer.as_ref() {
                for &r in &self.hover_outline_refs.1 {
                    if let Some(d) = rend.item_draws(r).and_then(|d| d.first()) {
                        overlay.push(*d);
                    }
                }
            }
        }
        let mut lines: Vec<LineVertex> = Vec::new();
        // (RUSTUDIO_NO_GRID hides it for lighting comparisons with Studio;
        // Studio's Play view has none, tools/fidelity demo shots.)
        if self.show_grid
            && !self.game.is_playing()
            && self.game.doc.kind == rustudio_core::doc::DocKind::Place
            && std::env::var_os("RUSTUDIO_NO_GRID").is_none()
            && !self.clean_view
        {
            scene::grid_lines(&mut lines, self.ui.grid_size);
        }
        let painter = ui.painter_at(rect);
        self.game.draw_world(&ctx, &painter, rect, lines, &overlay, self.harness.run.started);
        self.hud.viewport_drawn |= self.game.viewport_drawn;

        let playing = self.game.is_playing();
        let show_gui = self.ui.show_gui;
        let gui_took = self.game.draw_view_gui(&ctx, &painter, rect, &resp, show_gui);
        // What plugins put in CoreGui, over the edit view, and the edit-mode
        // input plugins hear (src/plugin_host/widgets_ui.rs); a press a
        // CoreGui button, a handle or an active plugin's mouse took is not
        // the selection tool's.
        let plugin_took = if !playing && show_gui {
            let took = self.plugin_viewport(&ctx, &painter, rect, &resp);
            self.plugin_keys(&ctx);
            took
        } else {
            false
        };
        {
            let mut extra = self.toolbox_vf.clone();
            self.plugin_ui.collect_vf_requests(&self.game.doc, &mut extra);
            for &(_, vf) in &vf_snaps {
                let laid = self.game.gui.vf_size_of(vf).or_else(|| self.plugin_ui.vf_size_of(vf));
                let size = rustudio_engine::gui::vf_snapshot_size(&self.game.doc, vf, laid);
                if let Some(req) = rustudio_engine::gui::vf_request_for(&self.game.doc, vf, size) {
                    extra.insert(vf, req);
                }
            }
            self.game.render_viewport_frames(&extra);
            if let (Some(r), Some(rs)) = (self.game.renderer.as_ref(), self.game.render_state.as_ref()) {
                self.toolbox.preview.set_texture(|vf| r.vf_texture(vf));
                self.plugin_ui.set_vf_textures(|vf| r.vf_texture(vf));
                for &(snap_id, vf) in &vf_snaps {
                    if !extra.contains_key(&vf) {
                        rustudio_engine::gui::vf_snap_complete(snap_id, String::new());
                    } else if let Some(png) = r.readback_vf_png(rs, vf) {
                        let uri = format!("rbxtemp://rustudio/vf_snap_{snap_id}.png");
                        rustudio_core::assets::register_generated(&uri, png);
                        rustudio_engine::gui::vf_snap_complete(snap_id, uri);
                    }
                }
            } else {
                for &(snap_id, _) in &vf_snaps {
                    rustudio_engine::gui::vf_snap_complete(snap_id, String::new());
                }
            }
        }
        self.game.finish_view_gui(&ctx, rect, dt);

        // Team Create collaborator camera name labels (Studio 0.748        // CloudEditAdornable): projected sphere center,
        // zero offset, centered X/Y, 14px, white text with black outline, no
        // background box, hidden when behind camera.
        if !self.cloud.collaborator_labels.is_empty() {
            let label_painter = painter.with_clip_rect(rect);
            let font = egui::FontId::proportional(14.0);
            for (world, name) in &self.cloud.collaborator_labels {
                let Some(point) = self.game.cam.project(rect, *world) else { continue; };
                if !rect.contains(point) {
                    continue;
                }
                for (dx, dy) in [(-1.0, 0.0), (1.0, 0.0), (0.0, -1.0), (0.0, 1.0), (-1.0, -1.0), (1.0, -1.0), (-1.0, 1.0), (1.0, 1.0)] {
                    label_painter.text(
                        point + egui::vec2(dx, dy),
                        egui::Align2::CENTER_CENTER,
                        name,
                        font.clone(),
                        Color32::BLACK,
                    );
                }
                label_painter.text(point, egui::Align2::CENTER_CENTER, name, font.clone(), Color32::WHITE);
            }
        }

        // Playing: clicks, keys and the hotbar go to the game.
        // (a press a plugin took keeps its release and drag: not the editor's click)
        let (pressed, held) = ctx.input(|i| (i.pointer.primary_pressed(), i.pointer.primary_down()));
        if pressed {
            self.plugin_ui.press_taken = plugin_took;
        }
        let press_taken = self.plugin_ui.press_taken;
        if !held && !pressed {
            self.plugin_ui.press_taken = false;
        }
        let in_game = self.game.game_viewport(ui, rect, &resp, gui_took) || plugin_took;

        // The Terrain Editor's brush has the left button while one is picked.
        let brushing = !in_game
            && !alt
            && self
                .builtin_tools
                .use_native(rustudio_core::rustudio_settings::BuiltinTool::TerrainEditor)
            && self.terrain_brush_ui(&ctx, rect, &resp, pointer, &painter);

        if self.part_drag.is_some() {
            for (key, tilt) in [(egui::Key::R, false), (egui::Key::T, true)] {
                if ctx.input_mut(|i| i.consume_key(egui::Modifiers::NONE, key)) {
                    self.rotate_part_drag(tilt);
                }
            }
            if ctx.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::H)) {
                crate::part_drag::HOTKEY_HELP_HIDDEN.fetch_xor(true, std::sync::atomic::Ordering::Relaxed);
            }
        }
        // -------------------------------------------------- select & gizmo
        let primary_drag = resp.dragged_by(PointerButton::Primary) && (!alt || self.drag.is_some() || self.part_drag.is_some()) && !in_game && !brushing;
        if resp.drag_started_by(PointerButton::Primary) && self.tool != Tool::None && !in_game && !press_taken && !brushing {
            let start = ctx.input(|i| i.pointer.press_origin()).or(pointer);
            if let Some(p) = start {
                // A handle takes the press even with Alt held (Alt = scale all axes).
                if let Some(h) = self.handle_at(&handles, p) {
                    self.start_drag(rect, h, p);
                } else if alt {
                } else if matches!(self.tool, Tool::Select | Tool::Move | Tool::Scale | Tool::Rotate | Tool::Transform)
                    && self.start_part_drag(rect, p, ctrl || shift)
                {
                    // Studio: pressing on a part and dragging moves it (src/part_drag.rs).
                } else {
                    self.marquee = Some(p);
                    self.marquee_base = if ctrl || shift { self.game.doc.selection.clone() } else { Vec::new() };
                }
            }
        }
        if primary_drag {
            if let Some(p) = ctx.input(|i| i.pointer.interact_pos()) {
                if self.drag.is_some() {
                    self.update_drag(rect, p, ctrl, alt);
                } else if let Some(d) = self.part_drag.as_mut() {
                    // Shift held: no grid snapping (DraggerContext.shouldGridSnap).
                    d.shift = shift;
                    // Alt held: keep the starting orientation (no Align Dragged Objects).
                    d.alt = alt;
                    self.update_part_drag(rect, p);
                } else if let Some(start) = self.marquee {
                    // Studio highlights what the box holds while it is dragged.
                    // From 3 px of mouse travel (DragSelector.updateDrag).
                    if (p - start).length() >= mech::DRAG_SELECT_MIN_PX {
                        self.game.doc.selection = self.marquee_pick(rect, Rect::from_two_pos(start, p));
                    }
                }
            }
        }
        if resp.drag_stopped_by(PointerButton::Primary)
            || (!ctx.input(|i| i.pointer.primary_down())
                && (self.drag.is_some() || self.marquee.is_some() || self.part_drag.is_some()))
        {
            if self.drag.is_some() {
                self.finish_drag();
            }
            if self.part_drag.is_some() {
                self.finish_part_drag();
            }
            if let Some(start) = self.marquee.take() {
                if let Some(end) = ctx.input(|i| i.pointer.interact_pos()) {
                    self.marquee_select(rect, Rect::from_two_pos(start, end));
                }
            }
        }
        if resp.clicked_by(PointerButton::Primary) && self.tool != Tool::None && !in_game && !press_taken && !brushing {
            if let Some(p) = pointer {
                // A GUI object under the cursor is picked first, as in Studio.
                let hit = self.game
                    .gui
                    .object_at(p)
                    .or_else(|| self.pick(rect, p, ctx.input(|i| i.modifiers.alt)));
                match (hit, ctrl || shift) {
                    (Some(r), true) => {
                        if let Some(pos) = self.game.doc.selection.iter().position(|s| *s == r) {
                            self.game.doc.selection.remove(pos);
                        } else {
                            self.game.doc.selection.push(r);
                        }
                    }
                    (Some(r), false) => self.select(vec![r]),
                    (None, false) => self.game.doc.selection.clear(),
                    (None, true) => {}
                }
            }
        }

        self.viewport_context_menu(ui, &resp);

        // ------------------------------------------------------- overlays
        self.game.scene.sync(&mut self.game.doc);
        let _overlays = rustudio_core::bench::scope("viewport_overlays");
        // Comparison captures show the scene alone (clean_view).
        if !self.clean_view {
            let _s = rustudio_core::bench::scope("vp_selection_outline");
            self.draw_selection(&painter, rect);
        }
        // Studio's hover outline (DraggerService.HoverThickness); none while
        // dragging (only the Ready state renders hover).
        // Studio Settings > Selection: Show Hover Over, Hover Over Color,
        // Hover Line Thickness.
        let show_hover = crate::studio_settings::flag("Studio", "Show Hover Over", true);
        if let Some(h) = self.hover.filter(|_| show_hover && !self.clean_view && self.part_drag.is_none() && self.drag.is_none()) {
            let _s = rustudio_core::bench::scope("vp_hover_outline");
            self.sync_hover_refs(h);
            let refs = std::mem::take(&mut self.hover_outline_refs.1);
            self.draw_outline_union(&painter, rect, &refs,
                Stroke::new(crate::studio_settings::hover_outline_px(), crate::studio_settings::color("Studio", "Hover Over Color", theme::HOVER_3D)), false);
            self.hover_outline_refs.1 = refs;
        }
        // A free drag hides the handles (Studio draws only the pivot then).
        let handles = if self.part_drag.is_some() { Vec::new() } else { self.handles(rect) };
        // A grab point per handle: arrow/handle tips, and 45° along each ring
        // (where no other ring crosses it).
        self.gizmo_points = handles
            .iter()
            .map(|(h, pts)| {
                (
                    *h,
                    if h.tool == Tool::Rotate {
                        pts[pts.len() / 8]
                    } else {
                        pts[pts.len() - 1]
                    },
                )
            })
            .collect();
        // Dragging a Move arrow: Studio draws only that arrow, a white guide
        // along its axis through the pivot, and the distance by the arrow.
        let moving = self.drag.as_ref().filter(|d| d.handle.tool == Tool::Move).map(|d| (d.handle, d.origin, d.axes.col(d.handle.axis), d.amount));
        if let Some((_, o, axis, _)) = moving {
            if let (Some(a), Some(b)) = (self.game.cam.project(rect, o - axis * 1000.0), self.game.cam.project(rect, o + axis * 1000.0)) {
                painter.with_clip_rect(rect).line_segment([a, b], Stroke::new(1.0, Color32::WHITE));
            }
        }
        for (h, pts) in handles.iter().filter(|_| !self.clean_view) {
            if moving.is_some_and(|(mh, ..)| mh != *h) {
                continue;
            }
            let active =
                hover_handle == Some(*h) && self.drag.is_none();
            let c = if active {
                HOVER_COLOR
            } else {
                AXIS_COLORS[h.axis]
            };
            match h.tool {
                Tool::Move => {
                    painter.line_segment([pts[0], pts[1]], Stroke::new(3.0, c));
                    let d = (pts[1] - pts[0]).normalized();
                    let n = Vec2::new(-d.y, d.x);
                    painter.add(egui::Shape::convex_polygon(
                        vec![pts[1] + d * 12.0, pts[1] + n * 6.0, pts[1] - n * 6.0],
                        c,
                        Stroke::NONE,
                    ));
                }
                Tool::Scale => {
                    painter.line_segment([pts[0], pts[1]], Stroke::new(2.0, c));
                    painter.circle_filled(pts[1], 7.0, c);
                }
                Tool::Rotate => {
                    painter.add(egui::Shape::line(
                        pts.clone(),
                        Stroke::new(if active { 3.5 } else { 2.5 }, c),
                    ));
                }
                Tool::Select | Tool::None | Tool::Transform => {}
            }
        }
        if let Some((mh, ..)) = moving {
            if let Some((_, pts)) = handles.iter().find(|(h, _)| *h == mh) {
                let amount = self.drag.as_ref().map(|d| d.amount).unwrap_or(0.0);
                let text = format!("{}", (amount * 1000.0).round() / 1000.0);
                let tip = pts[pts.len() - 1] + Vec2::new(-35.0, 30.0);
                let galley = painter.layout_no_wrap(text, egui::FontId::proportional(15.0), Color32::WHITE);
                let r = Rect::from_center_size(tip, galley.size() + Vec2::new(10.0, 8.0));
                painter.rect_filled(r, 3.0, Color32::from_rgb(30, 30, 30));
                painter.rect_stroke(r, 3.0, Stroke::new(1.0, Color32::from_rgb(90, 90, 90)), egui::StrokeKind::Inside);
                painter.galley(r.min + Vec2::new(5.0, 4.0), galley, Color32::WHITE);
            }
        }
        // The pivot: a white dot ringed in Select Color at the handles'
        // centre (Move / Scale / Rotate / Transform, also mid free drag).
        if !matches!(self.tool, Tool::Select | Tool::None) {
            if let Some(c) = self.gizmo_frame().and_then(|(o, _, _)| self.game.cam.project(rect, o)) {
                painter.circle_filled(c, 4.5, theme::SELECT);
                painter.circle_filled(c, 2.5, Color32::WHITE);
            }
        }
        if let (Some(start), Some(end), false) = (self.marquee, pointer, self.clean_view) {
            if (end - start).length() > mech::DRAG_SELECT_MIN_PX {
                let r = Rect::from_two_pos(start, end);
                painter.rect_filled(r, 0.0, Color32::from_rgba_unmultiplied(90, 170, 255, 30));
                painter.rect_stroke(
                    r,
                    0.0,
                    Stroke::new(1.0, Color32::from_rgb(90, 170, 255)),
                    egui::StrokeKind::Inside,
                );
            }
        }
        // Studio's Play view shows the game alone: no tool readout, axis
        // compass or border (tools/fidelity side-by-side shots).
        if !self.game.is_playing() && !self.clean_view {
            let _s = rustudio_core::bench::scope("vp_view_selector");
            self.draw_viewport_hud(ui, rect);
            self.draw_drag_hotkeys(&painter, rect);
        }
        // A Toolbox card dragged here: preview and drop (src/toolbox/glue.rs).
        if !self.game.is_playing()
            && self
                .builtin_tools
                .use_native(rustudio_core::rustudio_settings::BuiltinTool::Toolbox)
        {
            self.toolbox_viewport_drop(ui, rect, &resp);
        }
    }

    pub(crate) fn draw_box(
        &self,
        painter: &egui::Painter,
        rect: Rect,
        xf: &Xform,
        size: Vec3,
        stroke: Stroke,
    ) {
        let corners = box_corners(xf, size);
        let pts: Vec<Option<Pos2>> = corners.iter().map(|c| self.game.cam.project(rect, *c)).collect();
        for (a, b) in BOX_EDGES {
            if let (Some(pa), Some(pb)) = (pts[a], pts[b]) {
                painter.line_segment([pa, pb], stroke);
            }
        }
    }

    pub(crate) fn draw_outline(&self, painter: &egui::Painter, rect: Rect, sp: &ScenePart, stroke: Stroke) {
        self.draw_outline_union(painter, rect, &[sp.r], stroke, false);
    }

    fn draw_outline_union(&self, painter: &egui::Painter, rect: Rect, refs: &[Ref], stroke: Stroke, selection: bool) {
        // Nothing to outline: no mask (building and scanning one costs about
        // 50 M instructions at a 952x569 view, and an empty one has no edges).
        if refs.is_empty() {
            return;
        }
        let vp = self.game.cam.view_proj(rect);
        let reuse = !self.game.is_playing();
        // Nothing the per-part key reads has changed since a cached outline
        // of these parts (QuickKey): replay it without walking the parts.
        if reuse {
            let quick = self.outline_quick_key(rect, vp);
            let mut cache = self.outline_cache.borrow_mut();
            if let Some(i) = cache.entries.iter().position(|e| e.2 == Some(quick) && e.0.refs == refs) {
                let _s = rustudio_core::bench::scope("ol_quick_replay");
                let e = cache.entries.remove(i);
                painter.extend(e.1.iter().map(|edge| egui::Shape::line_segment(*edge, stroke)));
                cache.entries.push(e);
                return;
            }
        }
        // A camera turn changes projection, while the selection's meshes and
        // transforms usually stay fixed. Keep the gathered jobs for big selections.
        let prepare = reuse && selection && refs.len() >= 128;
        if prepare {
            let quick = self.outline_quick_key(rect, vp);
            let mut cache = self.outline_cache.borrow_mut();
            let cache = &mut *cache;
            if let Some(prepared) = &mut cache.prepared {
                if prepared.refs == refs
                    && prepared.inputs.same_geometry(quick)
                    && (prepared.inputs.journal == quick.journal || self.only_camera_motion_since(prepared.inputs.journal))
                {
                    if prepared.inputs.vp != quick.vp || prepared.inputs.rect != quick.rect {
                        prepared.edges = visible_outline_edges(
                            rect, vp, self.game.cam.pos, prepared, &mut cache.scratch, &mut cache.extra,
                        );
                    }
                    prepared.inputs = quick;
                    painter.extend(prepared.edges.iter().map(|edge| egui::Shape::line_segment(*edge, stroke)));
                    return;
                }
            }
        }
        let _key_scope = rustudio_core::bench::scope("ol_key");
        // What the outline is made from (OutlineKey): the view, the parts and
        // their drawn transforms, sizes, shapes and loaded meshes, and the
        // document's change journal (Bone, Motor6D and property edits). A
        // still selection reuses last frame's edges. While playing, skinned
        // poses move without journal marks: no reuse then.
        let mut parts = Vec::with_capacity(refs.len());
        let mut sources = Vec::with_capacity(refs.len());
        // (each mesh looked up once per outline, by its id's address: many
        // parts share a mesh, and one not parsed yet is read from disk again
        // on every lookup; equal ids at two addresses are just looked up twice)
        #[allow(clippy::type_complexity)]
        let mut meshes: rustc_hash::FxHashMap<(usize, usize), (Option<std::sync::Arc<Vec<[Vec3; 3]>>>, Option<std::sync::Arc<rustudio_render::skin::Base>>)> =
            rustc_hash::FxHashMap::default();
        for r in refs {
            let Some(sp) = self.game.scene.get(*r) else {
                parts.push(None);
                continue;
            };
            let d = self.game.renderer.as_ref().and_then(|renderer| renderer.item_draws(*r))
                .and_then(|draws| draws.first()).unwrap_or(&sp.draw);
            let (asset, skin) = match &sp.mesh_asset {
                Some(a) => meshes
                    .entry((a.id.as_ptr() as usize, a.id.len()))
                    .or_insert_with(|| (rustudio_core::assets::mesh_triangles(&a.id), rustudio_render::skin::cached_base(&a.id)))
                    .clone(),
                None => (None, None),
            };
            let bits = |v: Vec3| [v.x.to_bits(), v.y.to_bits(), v.z.to_bits()];
            parts.push(Some(PartSig {
                pos: bits(d.xf.pos),
                rot: [bits(d.xf.rot.x_axis), bits(d.xf.rot.y_axis), bits(d.xf.rot.z_axis)],
                size: bits(d.size),
                mesh: d.mesh,
                asset: asset.as_ref().map_or(0, |a| std::sync::Arc::as_ptr(a) as usize),
                skin: skin.as_ref().map_or(0, |b| std::sync::Arc::as_ptr(b) as usize),
            }));
            sources.push((*r, sp, d, asset, skin.is_some()));
        }
        let key = OutlineKey {
            rect: [rect.min.x, rect.min.y, rect.max.x, rect.max.y],
            vp: vp.to_cols_array(),
            refs: refs.to_vec(),
            parts,
            journal: self.game.doc.journal_end(),
        };
        drop(_key_scope);
        // (after the key: reading it can parse a mesh, which bumps the
        // generation, and the key saw that mesh)
        let quick = reuse.then(|| self.outline_quick_key(rect, vp));
        let mut cache = self.outline_cache.borrow_mut();
        let cache = &mut *cache;
        if reuse {
            let _hit_scope = rustudio_core::bench::scope("ol_hit_lookup");
            if let Some(i) = cache.entries.iter().position(|e| e.0 == key) {
                drop(_hit_scope);
                let _s = rustudio_core::bench::scope("ol_hit_replay");
                let mut e = cache.entries.remove(i);
                painter.extend(e.1.iter().map(|edge| egui::Shape::line_segment(*edge, stroke)));
                e.2 = quick;
                cache.entries.push(e);
                return;
            }
        }
        let _mask_scope = rustudio_core::bench::scope("ol_mask");
        // What gets rasterized: each part's triangles, skipping parts wholly
        // outside the view (every triangle would be rejected on its own).
        let mut jobs: Vec<OutlineJob> = Vec::with_capacity(sources.len());
        for (r, sp, d, asset, skinned) in &sources {
            // A skinned mesh: its deformed triangles (Studio's selection
            // box and Highlight trace the drawn shape). Only a mesh with a
            // parsed skinning base can have them (posed_triangles).
            let posed = sp.mesh_asset.as_ref().filter(|_| *skinned).and_then(|a| rustudio_render::skin::posed_triangles(&self.game.doc, *r, &a.id));
            let (bounds, tris) = match posed {
                Some(v) => (triangle_bounds(&v), std::sync::Arc::new(IndexedTris::unshared(&v))),
                None => {
                    let t = asset.as_deref().map(Vec::as_slice).unwrap_or_else(|| rustudio_render::render::outline_triangles(d.mesh));
                    let e = cache.bounds
                        .entry((t.as_ptr() as usize, t.len()))
                        .or_insert_with(|| (triangle_bounds(t), std::sync::Arc::new(IndexedTris::new(t)), asset.clone()));
                    (e.0, e.1.clone())
                }
            };
            if !prepare && bounds.is_some_and(|(lo, hi)| part_outside_frustum(vp, d.xf, d.size, lo, hi)) {
                continue;
            }
            jobs.push((tris, d.xf, d.size));
        }
        drop(_mask_scope);
        if prepare {
            // Posed skinned vertices can read more than the renderer's draw
            // transforms. Only reuse static triangle lists between turns.
            if !sources.iter().any(|(_, _, _, _, skinned)| *skinned) {
                let mut prepared = PreparedOutline {
                    inputs: self.outline_quick_key(rect, vp),
                    refs: refs.to_vec(),
                    jobs,
                    sorted_from: None,
                    edges: Vec::new(),
                };
                prepared.edges = visible_outline_edges(
                    rect, vp, self.game.cam.pos, &mut prepared, &mut cache.scratch, &mut cache.extra,
                );
                painter.extend(prepared.edges.iter().map(|edge| egui::Shape::line_segment(*edge, stroke)));
                cache.prepared = Some(prepared);
                return;
            }
            jobs.retain(|(tris, xf, size)| {
                !tris.bounds.is_some_and(|(lo, hi)| part_outside_frustum(vp, *xf, *size, lo, hi))
            });
        }
        sort_outline_jobs(self.game.cam.pos, &mut jobs);
        let threads = outline_thread_count(jobs.iter().map(|j| j.0.corners.len()).sum());
        let edges = union_edges(rect, vp, &jobs, &mut cache.scratch, &mut cache.extra, threads);

        painter.extend(edges.iter().map(|edge| egui::Shape::line_segment(*edge, stroke)));
        if reuse {
            // The selection's and a few hovered parts' (moving the mouse
            // across parts and back doesn't redo each one).
            if cache.entries.len() >= OUTLINE_CACHE_ENTRIES {
                cache.entries.remove(0);
            }
            cache.entries.push((key, edges, quick));
        }
    }

    fn draw_selection(&self, painter: &egui::Painter, rect: Rect) {
        // Studio Settings > Selection: Selection Color, Selection Line Thickness.
        let refs = self.selected_parts_shared();
        self.draw_outline_union(painter, rect, &refs,
            Stroke::new(crate::studio_settings::selection_outline_px(), crate::studio_settings::color("Studio", "Select Color", theme::SELECT)), true);
    }

    /// Releasing the box keeps what it held (with Shift / Ctrl, added to
    /// the selection it started from).
    fn marquee_select(&mut self, rect: Rect, area: Rect) {
        if area.size().length() < mech::DRAG_SELECT_MIN_PX {
            return;
        }
        let picked = self.marquee_pick(rect, area);
        self.select(picked);
    }

    /// The box's selection, as Studio's DraggerSchemaCore.beginBoxSelect and
    /// DragSelector.updateDrag / getSelectionFrustum: every unlocked part
    /// stands for its selectable (its top Model, else itself), and a
    /// selectable is in the box when its centre — a Model's bounding box
    /// centre, a part's position — is inside the four planes through the
    /// camera and the box's corners (the same as projecting inside the box,
    /// in front of the camera). Added to the base selection (Shift / Ctrl).
    fn marquee_pick(&mut self, rect: Rect, area: Rect) -> Vec<Ref> {
        let _p = rustudio_core::bench::scope("marquee_pick");
        let key = (self.game.doc.id, self.game.doc.journal_end());
        if self.marquee_centres.key != Some(key) || self.game.is_playing() {
            self.marquee_centres = MarqueeCentres {
                key: Some(key),
                targets: self.collect_marquee_centres(),
            };
        }
        let mut picked = self.marquee_base.clone();
        let mut seen: rustc_hash::FxHashSet<_> = picked.iter().copied().collect();
        let view_proj = self.game.cam.view_proj(rect);
        for &(target, centre) in &self.marquee_centres.targets {
            let clip = view_proj * centre.extend(1.0);
            if clip.w <= 1e-4 {
                continue;
            }
            let ndc = clip.truncate() / clip.w;
            let point = egui::pos2(
                rect.min.x + (ndc.x + 1.0) * 0.5 * rect.width(),
                rect.min.y + (1.0 - ndc.y) * 0.5 * rect.height(),
            );
            if area.contains(point) && seen.insert(target) {
                picked.push(target);
            }
        }
        picked
    }

    fn collect_marquee_centres(&self) -> Vec<(Ref, Vec3)> {
        let _p = rustudio_core::bench::scope("marquee_centres");
        // selectable -> (centre if a part, else the box of its parts)
        let mut order: Vec<Ref> = Vec::new();
        let mut centres: std::collections::HashMap<Ref, Result<Vec3, (Vec3, Vec3)>> = std::collections::HashMap::new();
        for sp in &self.game.scene.parts {
            if matches!(self.game.doc.prop(sp.r, "Locked"), Some(Variant::Bool(true))) {
                continue;
            }
            let t = self.selection_target(sp.r);
            if t == sp.r {
                order.push(t);
                centres.insert(t, Ok(sp.xf.pos));
                continue;
            }
            let m = sp.xf.rot;
            let half = (m.x_axis.abs() * sp.size.x + m.y_axis.abs() * sp.size.y + m.z_axis.abs() * sp.size.z) * 0.5;
            let (lo, hi) = (sp.xf.pos - half, sp.xf.pos + half);
            match centres.get_mut(&t) {
                Some(Err((a, b))) => {
                    *a = a.min(lo);
                    *b = b.max(hi);
                }
                _ => {
                    order.push(t);
                    centres.insert(t, Err((lo, hi)));
                }
            }
        }
        order.into_iter().map(|target| {
            let centre = match centres[&target] {
                Ok(centre) => centre,
                Err((lo, hi)) => (lo + hi) * 0.5,
            };
            (target, centre)
        }).collect()
    }

    fn view_selector_rect(rect: Rect) -> Rect {
        Rect::from_min_size(egui::pos2(rect.right() - 100.0, rect.top() + 8.0), Vec2::splat(92.0))
    }

    fn draw_viewport_hud(&mut self, ui: &mut egui::Ui, rect: Rect) {
        let area = Self::view_selector_rect(rect);
        let p = ui.painter().clone();
        if let Some(drag) = &self.drag {
            let unit = if drag.handle.tool == Tool::Rotate { "°" } else { " studs" };
            p.text(rect.min + Vec2::splat(10.0), egui::Align2::LEFT_TOP,
                format!("{}: {:+.2}{unit}", drag.handle.tool.label(), drag.amount),
                egui::FontId::proportional(13.0), theme::TEXT);
        }
        if !self
            .builtin_tools
            .use_native(rustudio_core::rustudio_settings::BuiltinTool::ViewSelector)
            || !self.ui.show_view_selector
        {
            return;
        }
        let view = self.game.cam.view();
        let center = area.center() - Vec2::new(0.0, 5.0);
        let project = |v: Vec3| { let v = view.transform_vector3(v); center + Vec2::new(v.x, -v.y) * 29.0 };
        // The selector's meshes depend only on the view's rotation and where
        // it sits: kept until either changes (SelectorCache).
        let rot = glam::Mat3::from_mat4(view);
        let view_key: [u32; 11] = {
            let c = rot.to_cols_array();
            let mut k = [0u32; 11];
            for (i, v) in c.iter().enumerate() { k[i] = v.to_bits(); }
            k[9] = center.x.to_bits();
            k[10] = center.y.to_bits();
            k
        };
        let pointer = ui.input(|i| i.pointer.hover_pos());
        let selector_resource = |id: &str| {
            rustudio_core::assets::resolve(id, None).filter(|p| p.is_file()).or_else(|| {
                let root = rustudio_core::assets::content_dir().canonicalize().ok()?;
                let p = root.parent()?.join("StudioContent").join(id.strip_prefix("rbxasset://")?);
                p.is_file().then_some(p)
            })
        };
        // Axis.mesh is a 1.06-unit bar scaled by 0.9, centered at the
        // model-relative positions recorded in ViewSelector.rbxm.
        for (i, axis) in Vec3::AXES.iter().enumerate() {
            let mid = [Vec3::new(0.01,-0.48,-0.48), Vec3::new(-0.48,-0.03,-0.48), Vec3::new(-0.48,-0.48,0.01)][i] / 0.475;
            let half = *axis * (0.53*0.9/0.475);
            let end = project(mid+half);
            let color = [Color32::from_rgb(239,65,87),Color32::from_rgb(72,228,133),Color32::from_rgb(50,94,248)][i];
            p.line_segment([project(mid-half),end], Stroke::new(0.76, color));
            p.text(end, egui::Align2::CENTER_CENTER, ["X","Y","Z"][i], egui::FontId::proportional(9.0), color);
        }
        // Render the shipped rounded mesh once. Backface culling prevents
        // translucent rear surfaces and overlapping guessed face fills.
        static SELECTOR: std::sync::OnceLock<rustudio_core::mesh::MeshData> = std::sync::OnceLock::new();
        // An existing content download can gain the selector while this
        // editor is running. Only cache a mesh after a successful read.
        if SELECTOR.get().is_none() {
            if let Some(geometry) = selector_resource("rbxasset://models/ViewSelector/Basic.mesh")
                .and_then(|path| std::fs::read(path).ok())
                .and_then(|bytes| rustudio_core::mesh::parse(&bytes).ok())
            {
                let _ = SELECTOR.set(geometry);
            }
        }
        let Some(geometry) = SELECTOR.get() else { return };
        {
            let cache = &mut self.selector_cache;
            if cache.body.as_ref().is_none_or(|b| b.0 != view_key) {
                // Only the projection and the facing test follow the view;
                // the lit colours and model positions are fixed (SelectorPrep).
                let prep = selector_prep(geometry);
                let mut mesh = egui::Mesh::default();
                mesh.vertices.reserve(prep.body.len() * 3);
                mesh.indices.reserve(prep.body.len() * 3);
                for t in &prep.body {
                    if view.transform_vector3(t.facing).z <= 0.0 { continue; }
                    let start = mesh.vertices.len() as u32;
                    for (q, c) in t.q.iter().zip(t.color) {
                        mesh.colored_vertex(project(*q), c);
                    }
                    mesh.add_triangle(start,start+1,start+2);
                }
                cache.body = Some((view_key, std::sync::Arc::new(mesh)));
            }
            if let Some((_, mesh)) = &cache.body {
                p.add(egui::Shape::Mesh(mesh.clone()));
            }
        }
        let mut faces = Vec::new();
        for (face_index, &(label, key, normal, u, v)) in SELECTOR_FACES.iter().enumerate() {
            let depth = view.transform_vector3(normal).z;
            if depth <= 0.001 { continue; }

            let points = [normal-u-v*0.65, normal-u*0.65-v, normal+u*0.65-v, normal+u-v*0.65, normal+u+v*0.65, normal+u*0.65+v, normal-u*0.65+v, normal-u+v*0.65].map(project);
            faces.push((depth, face_index, label, key, normal, u, v, points));
        }
        faces.sort_by(|a,b| a.0.total_cmp(&b.0));
        for (_depth, face_index, label, key, normal, u, v, points) in faces {
            let bounds = Rect::from_points(&points);
            self.ui_rects.insert(key, bounds);
            let response = ui.interact(bounds, egui::Id::new(key), Sense::click());
            let inside = pointer.is_some_and(|q| {
                let mut sign = 0.0_f32;
                for i in 0..points.len() {
                    let a=points[i]; let b=points[(i+1)%points.len()];
                    let cross=(b.x-a.x)*(q.y-a.y)-(b.y-a.y)*(q.x-a.x);
                    if cross.abs()>0.01 { if sign*cross<0.0 { return false; } sign=cross; }
                }
                true
            });
            // Studio ships the face lettering as a decal, including its contour.
            // Retain that rasterization instead of re-typesetting projected text.
            let texture_key = egui::Id::new(("selector-decal", label, inside));
            let texture = ui.ctx().data_mut(|d| d.get_temp::<egui::TextureHandle>(texture_key));
            let texture = texture.or_else(|| {
                let id = format!("rbxasset://textures/ViewSelector/{}{}.png", label.to_lowercase(), if inside { "_hover" } else { "" });
                let bytes = std::fs::read(selector_resource(&id)?).ok()?;
                let img = image::load_from_memory(&bytes).ok()?.to_rgba8();
                let image = egui::ColorImage::from_rgba_unmultiplied([img.width() as usize,img.height() as usize], img.as_raw());
                let t = ui.ctx().load_texture(id, image, egui::TextureOptions::LINEAR);
                ui.ctx().data_mut(|d| d.insert_temp(texture_key,t.clone()));
                Some(t)
            });
            if let Some(texture) = texture {
                let face_key = (label, texture.id());
                if let Some((_, mesh)) = self.selector_cache.faces.iter().find(|f| f.0 == (face_key, view_key)) {
                    p.add(egui::Shape::Mesh(mesh.clone()));
                } else {
                let mut mesh = egui::Mesh::with_texture(texture.id());
                {
                    // This face's triangles and lettering UVs are fixed
                    // (SelectorPrep); the view decides which face the camera.
                    let _ = (u, v);
                    let prep = selector_prep(geometry);
                    for t in &prep.faces[face_index] {
                        if view.transform_vector3(t.n).z <= 0.0 { continue; }
                        let start = mesh.vertices.len() as u32;
                        for (q, uv) in t.q.iter().zip(t.uv) {
                            mesh.vertices.push(egui::epaint::Vertex { pos: project(*q),
                                uv,
                                color: Color32::from_white_alpha(204) });
                        }
                        mesh.add_triangle(start,start+1,start+2);
                    }
                }
                let mesh = std::sync::Arc::new(mesh);
                let faces = &mut self.selector_cache.faces;
                faces.retain(|f| f.0.0.0 != label || f.0.1 != view_key);
                faces.push(((face_key, view_key), mesh.clone()));
                if faces.len() > 12 {
                    faces.remove(0);
                }
                p.add(egui::Shape::Mesh(mesh));
                }
            }
            if inside {
                ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
                if response.clicked() { self.game.cam.animate_view(normal); }
            }
        }
        // Studio exposes quarter-turn arrows around the aligned face.
        if self.game.cam.forward().abs().max_element() > 0.999 {
            for (offset, direction) in [(Vec2::new(-40.0,0.0), -self.game.cam.right()),
                (Vec2::new(40.0,0.0), self.game.cam.right()),
                (Vec2::new(0.0,-40.0), self.game.cam.up()),
                (Vec2::new(0.0,40.0), -self.game.cam.up())] {
                let c=center+offset;
                let d=offset.normalized(); let t=Vec2::new(-d.y,d.x);
                let r=Rect::from_center_size(c, Vec2::splat(15.0));
                let response=ui.interact(r, egui::Id::new(("view-turn",offset.x as i32,offset.y as i32)),Sense::click());
                p.add(egui::Shape::convex_polygon(vec![c+d*5.0,c-d*4.0+t*4.0,c-d*4.0-t*4.0],
                    if response.hovered() {Color32::from_rgb(95,162,218)} else {Color32::from_gray(190)},Stroke::NONE));
                if response.clicked() {self.game.cam.animate_view(direction);}
            }
        }

    }

}

/// The parts' new (CFrame, Size) for a handle drag of `d` studs (Move,
/// Scale) or degrees (Rotate).
fn gizmo_updates(drag: &GizmoDrag, dir: Vec3, d: f32, symmetric: bool, uniform: bool) -> Vec<(Ref, Xform, Vec3)> {
    let h = drag.handle;
    let mut updates = Vec::new();
    match h.tool {
        Tool::Move => {
            for (r, xf, size) in &drag.parts {
                updates.push((*r, Xform { pos: xf.pos + dir * d, rot: xf.rot }, *size));
            }
        }
        // ExtrudeHandlesImplementation.axesToScale: all three axes when Alt is
        // held, or when the selection is more than one loose part. Then the
        // selection's box scales as one: sizes and offsets by the same factor,
        // about the opposite face (Ctrl: about the centre).
        Tool::Scale if uniform || drag.parts.len() > 1 => {
            let (mut lo, mut hi) = (Vec3::splat(f32::INFINITY), Vec3::splat(f32::NEG_INFINITY));
            for (_, xf, size) in &drag.parts {
                for a in 0..3 {
                    let ax = drag.axes.col(a);
                    let c = (xf.pos - drag.origin).dot(ax);
                    let e: f32 = (0..3).map(|i| xf.rot.col(i).dot(ax).abs() * size[i] * 0.5).sum();
                    lo[a] = lo[a].min(c - e);
                    hi[a] = hi[a].max(c + e);
                }
            }
            let a = h.axis;
            let extent = (hi[a] - lo[a]).max(1e-6);
            let min_dim = drag.parts.iter().map(|(_, _, s)| s.min_element()).fold(f32::INFINITY, f32::min);
            let f = ((extent + d * if symmetric { 2.0 } else { 1.0 }) / extent)
                .max(rustudio_core::mech::MIN_PART_SIZE / min_dim.max(1e-6));
            let centre = drag.origin + drag.axes * ((lo + hi) * 0.5);
            let anchor = if symmetric { centre } else { centre - dir * (extent * 0.5) };
            for (r, xf, size) in &drag.parts {
                updates.push((*r, Xform { pos: anchor + (xf.pos - anchor) * f, rot: xf.rot }, *size * f));
            }
        }
        Tool::Scale => {
            for (r, xf, size) in &drag.parts {
                // Grow the part's own axis closest to the dragged direction, one side only.
                let k = (0..3)
                    .max_by(|a, b| xf.rot.col(*a).dot(dir).abs().total_cmp(&xf.rot.col(*b).dot(dir).abs()))
                    .unwrap_or(0);
                let along = xf.rot.col(k) * xf.rot.col(k).dot(dir).signum();
                let (s, grow) = crate::part_drag::scaled_size(*size, k, d, symmetric, uniform);
                updates.push((
                    *r,
                    Xform { pos: xf.pos + along * grow * if symmetric { 0.0 } else { 0.5 }, rot: xf.rot },
                    s,
                ));
            }
        }
        Tool::Rotate => {
            let rot = Mat3::from_axis_angle(drag.axes.col(h.axis), d.to_radians());
            for (r, xf, size) in &drag.parts {
                let nx = Xform { pos: drag.origin + rot * (xf.pos - drag.origin), rot: rot * xf.rot }.orthonormalized();
                updates.push((*r, nx, *size));
            }
        }
        Tool::Select | Tool::None | Tool::Transform => {}
    }
    updates
}

/// The view selector's faces: label, hit key, outward normal and the
/// lettering's u / v axes.
const SELECTOR_FACES: [(&str, &str, Vec3, Vec3, Vec3); 6] = [
    ("Right", "view.right", Vec3::X, Vec3::NEG_Z, Vec3::Y),
    ("Left", "view.left", Vec3::NEG_X, Vec3::Z, Vec3::Y),
    ("Top", "view.top", Vec3::Y, Vec3::NEG_X, Vec3::NEG_Z),
    ("Bottom", "view.bottom", Vec3::NEG_Y, Vec3::NEG_X, Vec3::NEG_Z),
    ("Back", "view.back", Vec3::Z, Vec3::X, Vec3::Y),
    ("Front", "view.front", Vec3::NEG_Z, Vec3::NEG_X, Vec3::Y),
];

/// One body triangle of the view selector: model positions (/25), the
/// vertex colours and the normal its facing test uses.
struct SelectorBodyTri {
    facing: Vec3,
    q: [Vec3; 3],
    color: [Color32; 3],
}

/// One lettering triangle of a selector face.
struct SelectorFaceTri {
    n: Vec3,
    q: [Vec3; 3],
    uv: [Pos2; 3],
}

/// What the view selector draws that doesn't depend on the view, worked
/// out once from Basic.mesh (the camera turning used to redo the lighting,
/// face membership and UVs of every triangle each frame). Same arithmetic
/// as before, so the meshes are bit-identical.
struct SelectorPrep {
    body: Vec<SelectorBodyTri>,
    faces: [Vec<SelectorFaceTri>; 6],
}

fn selector_prep(geometry: &'static rustudio_core::mesh::MeshData) -> &'static SelectorPrep {
    static PREP: std::sync::OnceLock<SelectorPrep> = std::sync::OnceLock::new();
    PREP.get_or_init(|| {
        let mut body = Vec::new();
        let mut faces: [Vec<SelectorFaceTri>; 6] = Default::default();
        for (tri, ns) in geometry.positions.chunks_exact(3).zip(geometry.normals.chunks_exact(3)) {
            let q = [tri[0] / 25.0, tri[1] / 25.0, tri[2] / 25.0];
            // Model: color (181,187,199), transparency .5; viewport
            // ambient 200/255, directional 140/255 along (-1,-1,-1).
            let color = [0, 1, 2].map(|i| {
                let light = (200.0/255.0 + 140.0/255.0*ns[i].dot(Vec3::ONE.normalize()).max(0.0)).min(1.0);
                Color32::from_rgba_unmultiplied((181.0*light) as u8,(187.0*light) as u8,(199.0*light) as u8,128)
            });
            body.push(SelectorBodyTri { facing: (ns[0]+ns[1]+ns[2])/3.0, q, color });
            let n = (ns[0]+ns[1]+ns[2]).normalize_or_zero();
            for (i, &(_, _, normal, u, v)) in SELECTOR_FACES.iter().enumerate() {
                if n.dot(normal) < n.abs().max_element()-0.0001 { continue; }
                let uv = q.map(|q| egui::pos2((q.dot(u)+1.0)*0.5,(1.0-q.dot(v))*0.5));
                faces[i].push(SelectorFaceTri { n, q, uv });
            }
        }
        SelectorPrep { body, faces }
    })
}

/// The view selector's projected meshes (draw_viewport_hud), for one view
/// rotation and position: the body, and each face's lettering by (label,
/// texture).
#[derive(Default)]
pub struct SelectorCache {
    body: Option<([u32; 11], std::sync::Arc<egui::Mesh>)>,
    #[allow(clippy::type_complexity)]
    faces: Vec<(((&'static str, egui::TextureId), [u32; 11]), std::sync::Arc<egui::Mesh>)>,
}

/// Outlines kept from earlier frames (newest last), at most this many.
const OUTLINE_CACHE_ENTRIES: usize = 6;

/// One outlined part as the outline last saw it (bit-exact floats).
#[derive(PartialEq)]
struct PartSig {
    pos: [u32; 3],
    rot: [[u32; 3]; 3],
    size: [u32; 3],
    mesh: rustudio_render::render::MeshKind,
    /// The loaded mesh / skinning base (their allocation: a new download is a new one).
    asset: usize,
    skin: usize,
}

/// What can change an outline's OutlineKey without walking its parts: the
/// view (bits), the document journal (scene membership and properties),
/// the scene, the renderer's draws (items_generation) and the meshes,
/// outline triangles and skinning bases (assets::mesh_generation). Equal
/// to an entry's (with the same parts) means its OutlineKey would match.
#[derive(Clone, Copy, PartialEq, Eq)]
struct QuickKey {
    doc: u64,
    rect: [u32; 4],
    vp: [u32; 16],
    journal: u64,
    scene: (u64, usize),
    items: u64,
    meshes: u64,
}

impl QuickKey {
    fn same_geometry(self, other: Self) -> bool {
        self.doc == other.doc && self.scene == other.scene && self.items == other.items && self.meshes == other.meshes
    }
}

impl Editor {
    fn outline_quick_key(&self, rect: Rect, vp: Mat4) -> QuickKey {
        QuickKey {
            doc: self.game.doc.id,
            rect: [rect.min.x, rect.min.y, rect.max.x, rect.max.y].map(f32::to_bits),
            vp: vp.to_cols_array().map(f32::to_bits),
            journal: self.game.doc.journal_end(),
            scene: (self.game.scene.doc_id, self.game.scene.parts.len()),
            // (no renderer: no draws to differ)
            items: self.game.renderer.as_ref().map_or(u64::MAX, |r| r.items_generation()),
            meshes: rustudio_core::assets::mesh_generation(),
        }
    }
}

/// Everything a union outline's edges depend on.
#[derive(PartialEq)]
struct OutlineKey {
    rect: [f32; 4],
    vp: [f32; 16],
    refs: Vec<Ref>,
    parts: Vec<Option<PartSig>>,
    journal: u64,
}

/// Outline edges kept between frames, and the mask's buffers (reused, so a
/// recompute neither allocates nor clears a whole view-sized buffer).
#[derive(Default)]
pub struct OutlineCache {
    #[allow(clippy::type_complexity)]
    entries: Vec<(OutlineKey, Vec<[Pos2; 2]>, Option<QuickKey>)>,
    scratch: MaskScratch,
    prepared: Option<PreparedOutline>,
    /// Model-space bounds and shared-corner form of each triangle list the outline has drawn, by
    /// (address, length). Primitives are static; a loaded mesh's Arc is
    /// held here, so its address can't be reused by another mesh.
    #[allow(clippy::type_complexity)]
    bounds: rustc_hash::FxHashMap<(usize, usize), (Option<(Vec3, Vec3)>, std::sync::Arc<IndexedTris>, Option<std::sync::Arc<Vec<[Vec3; 3]>>>)>,
    /// The other rasterizing threads' buffers (each all zeros between uses).
    extra: Vec<MaskScratch>,
}

/// One part's triangles (model space) and how it's drawn.
type OutlineJob = (std::sync::Arc<IndexedTris>, Xform, Vec3);

struct PreparedOutline {
    inputs: QuickKey,
    refs: Vec<Ref>,
    jobs: Vec<OutlineJob>,
    sorted_from: Option<[u32; 3]>,
    edges: Vec<[Pos2; 2]>,
}

fn sort_outline_jobs(eye: Vec3, jobs: &mut [OutlineJob]) {
    // Big, near parts first: coverage of the view lets later parts skip rows.
    jobs.sort_by_cached_key(|(_, xf, size)| {
        let reach = size.x * size.y + size.y * size.z + size.x * size.z;
        std::cmp::Reverse((reach / (xf.pos - eye).length_squared().max(1e-3)).to_bits())
    });
}

fn outline_thread_count(triangles: usize) -> usize {
    if triangles < OUTLINE_PARALLEL_TRIANGLES {
        1
    } else {
        std::thread::available_parallelism().map_or(1, |n| n.get()).clamp(1, outline_threads())
    }
}

fn visible_outline_edges(
    rect: Rect, vp: Mat4, eye: Vec3, prepared: &mut PreparedOutline, main: &mut MaskScratch, extra: &mut Vec<MaskScratch>,
) -> Vec<[Pos2; 2]> {
    let scope = rustudio_core::bench::scope("ol_prepared_cull");
    let eye_key = eye.to_array().map(f32::to_bits);
    if prepared.sorted_from != Some(eye_key) {
        sort_outline_jobs(eye, &mut prepared.jobs);
        prepared.sorted_from = Some(eye_key);
    }
    let jobs: Vec<_> = prepared.jobs.iter().filter(|(tris, xf, size)| {
        !tris.bounds.is_some_and(|(lo, hi)| part_outside_frustum(vp, *xf, *size, lo, hi))
    }).collect();
    let threads = outline_thread_count(jobs.iter().map(|j| j.0.corners.len()).sum());
    drop(scope);
    union_edges(rect, vp, &jobs, main, extra, threads)
}

/// A triangle list with its corners shared: each distinct point (by its
/// exact bits) once, and the triangles as indices into them. The outline
/// projects each point once instead of once per triangle using it, with the
/// same arithmetic, so the projected corners are bit-identical.
pub struct IndexedTris {
    points: Vec<Vec3>,
    corners: Vec<[u32; 3]>,
    /// The box around the points (None: empty or not finite).
    bounds: Option<(Vec3, Vec3)>,
}

impl IndexedTris {
    fn new(triangles: &[[Vec3; 3]]) -> Self {
        let mut index: std::collections::HashMap<[u32; 3], u32> = std::collections::HashMap::new();
        let mut points = Vec::new();
        let corners = triangles
            .iter()
            .map(|t| {
                t.map(|p| {
                    *index.entry([p.x.to_bits(), p.y.to_bits(), p.z.to_bits()]).or_insert_with(|| {
                        points.push(p);
                        points.len() as u32 - 1
                    })
                })
            })
            .collect();
        Self { bounds: triangle_bounds(triangles), points, corners }
    }

    /// Without looking for shared corners (a skinned pose, made each frame).
    fn unshared(triangles: &[[Vec3; 3]]) -> Self {
        Self {
            points: triangles.iter().flatten().copied().collect(),
            corners: (0..triangles.len() as u32).map(|i| [i * 3, i * 3 + 1, i * 3 + 2]).collect(),
            bounds: triangle_bounds(triangles),
        }
    }
}

/// The union outline of `jobs` seen through `vp` (SilhouetteMask). Big
/// outlines rasterize on `threads` threads, each into its own coverage
/// deltas, summed after: the union (coverage > 0), and so the edges and
/// their order, are the same as on one thread.
fn union_edges(rect: Rect, vp: Mat4, jobs: &[impl std::borrow::Borrow<OutlineJob> + Sync], main: &mut MaskScratch, extra: &mut Vec<MaskScratch>, threads: usize) -> Vec<[Pos2; 2]> {
    let _mask_scope = rustudio_core::bench::scope("ol_raster");
    // One job: its triangles, unless every row it could touch is covered
    // already (its triangles would each skip those rows: SilhouetteMask::triangle).
    let draw = |mask: &mut SilhouetteMask<'_>, clip: &mut Vec<glam::Vec4>, (tris, xf, size): &OutlineJob| {
        if let Some((lo, hi)) = tris.bounds {
            if mask.rows_covered(vp, *xf, *size, lo, hi) {
                return;
            }
        }
        clip.clear();
        clip.extend(tris.points.iter().map(|p| vp * (xf.pos + xf.rot * (*p * *size)).extend(1.0)));
        for c in &tris.corners {
            mask.triangle([clip[c[0] as usize], clip[c[1] as usize], clip[c[2] as usize]]);
        }
    };
    // Threads take the jobs (biggest first) one at a time from a shared
    // counter, so each fills its own rows early and skips more.
    let next = std::sync::atomic::AtomicUsize::new(0);
    let raster = |mask: &mut SilhouetteMask<'_>| {
        let mut clip: Vec<glam::Vec4> = Vec::new();
        loop {
            let i = next.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            let Some(job) = jobs.get(i) else { break };
            draw(mask, &mut clip, job.borrow());
        }
    };
    let shared: Vec<std::sync::atomic::AtomicBool> = if threads > 1 {
        (0..SilhouetteMask::rows_for(rect)).map(|_| std::sync::atomic::AtomicBool::new(false)).collect()
    } else {
        Vec::new()
    };
    let shared = &shared[..];
    let mask = if threads <= 1 {
        let mut mask = SilhouetteMask::new(rect, main);
        raster(&mut mask);
        mask
    } else {
        let others = threads - 1;
        if extra.len() < others {
            extra.resize_with(others, Default::default);
        }
        let (mut mask, boxes) = std::thread::scope(|sc| {
            let handles: Vec<_> = extra[..others]
                .iter_mut()
                .map(|scratch| {
                    let raster = &raster;
                    sc.spawn(move || {
                        let mut m = SilhouetteMask::new(rect, scratch);
                        m.shared = Some(shared);
                        raster(&mut m);
                        (m.x0, m.x1, m.y0, m.y1)
                    })
                })
                .collect();
            let mut m = SilhouetteMask::new(rect, main);
            m.shared = Some(shared);
            raster(&mut m);
            let boxes: Vec<_> = handles.into_iter().map(|h| h.join().unwrap_or((usize::MAX, 0, usize::MAX, 0))).collect();
            (m, boxes)
        });
        let _merge = rustudio_core::bench::scope("ol_merge");
        let stride = mask.width + 1;
        for (scratch, &(x0, x1, y0, y1)) in extra.iter_mut().zip(&boxes) {
            if x0 > x1 || y0 >= y1 {
                continue;
            }
            for y in y0..y1 {
                let (a, b) = (y * stride + x0, y * stride + x1 + 1);
                let src = &mut scratch.delta[a..b];
                for (d, s) in mask.s.delta[a..b].iter_mut().zip(src.iter()) {
                    *d += *s;
                }
                src.fill(0);
            }
            mask.x0 = mask.x0.min(x0);
            mask.x1 = mask.x1.max(x1);
            mask.y0 = mask.y0.min(y0);
            mask.y1 = mask.y1.max(y1);
        }
        mask
    };
    drop(_mask_scope);
    let _edges_scope = rustudio_core::bench::scope("ol_edges");
    mask.edges()
}

/// The first and last index where two equally long rows differ (None:
/// equal), comparing eight cells at a time.
fn differing_span(a: &[bool], b: &[bool]) -> Option<(usize, usize)> {
    // SAFETY: bool is one byte (0 or 1); the rows are only read.
    let (a, b): (&[u8], &[u8]) = unsafe {
        (std::slice::from_raw_parts(a.as_ptr().cast(), a.len()), std::slice::from_raw_parts(b.as_ptr().cast(), b.len()))
    };
    let n = a.len().min(b.len());
    let word = |s: &[u8], i: usize| u64::from_ne_bytes(s[i..i + 8].try_into().unwrap_or_default());
    let mut i = 0;
    let first = loop {
        if i + 8 <= n {
            if word(a, i) != word(b, i) {
                break (i..i + 8).find(|&k| a[k] != b[k])?;
            }
            i += 8;
        } else {
            break (i..n).find(|&k| a[k] != b[k])?;
        }
    };
    let mut j = n;
    let last = loop {
        if j >= first + 8 {
            if word(a, j - 8) != word(b, j - 8) {
                break (j - 8..j).rev().find(|&k| a[k] != b[k]).unwrap_or(first);
            }
            j -= 8;
        } else {
            break (first..j).rev().find(|&k| a[k] != b[k]).unwrap_or(first);
        }
    };
    Some((first, last))
}

/// Outlines of at least this many triangles rasterize on several threads.
const OUTLINE_PARALLEL_TRIANGLES: usize = 20_000;
/// At most this many (a low-end laptop has four cores; the main thread is one).
const OUTLINE_THREADS: usize = 4;

/// OUTLINE_THREADS, or RUSTUDIO_OUTLINE_THREADS (1..=16, for measuring).
fn outline_threads() -> usize {
    static N: std::sync::OnceLock<usize> = std::sync::OnceLock::new();
    *N.get_or_init(|| {
        std::env::var("RUSTUDIO_OUTLINE_THREADS").ok().and_then(|v| v.parse().ok()).map_or(OUTLINE_THREADS, |n: usize| n.clamp(1, 16))
    })
}

/// The box around a triangle list's corners (None: empty or not finite).
fn triangle_bounds(triangles: &[[Vec3; 3]]) -> Option<(Vec3, Vec3)> {
    let mut lo = Vec3::splat(f32::INFINITY);
    let mut hi = Vec3::splat(f32::NEG_INFINITY);
    for t in triangles {
        for p in t {
            lo = lo.min(*p);
            hi = hi.max(*p);
        }
    }
    (lo.is_finite() && hi.is_finite()).then_some((lo, hi))
}

/// Whether a part's triangles (inside lo..hi in model space) all lie
/// clearly outside one plane of the clip-space frustum. The plane distance
/// is affine in the model-space point, so all eight box corners outside a
/// plane put every point of the box outside it; the margin keeps rounding
/// in the per-triangle test from ever disagreeing.
fn part_outside_frustum(vp: glam::Mat4, xf: Xform, size: Vec3, lo: Vec3, hi: Vec3) -> bool {
    let mut outside = 0b11_1111;
    for i in 0..8 {
        let c = Vec3::new(
            if i & 1 == 0 { lo.x } else { hi.x },
            if i & 2 == 0 { lo.y } else { hi.y },
            if i & 4 == 0 { lo.z } else { hi.z },
        );
        let p = vp * (xf.pos + xf.rot * (c * size)).extend(1.0);
        if !p.is_finite() {
            return false;
        }
        outside &= clip_outcode(p, -1e-3 * (1.0 + p.w.abs()));
        if outside == 0 {
            return false;
        }
    }
    true
}

/// One bit per WGPU clip plane that the point is outside, using the
/// same plane-distance arithmetic as the polygon clipper.
fn clip_outcode(p: glam::Vec4, margin: f32) -> u32 {
    let sides = glam::Vec4::new(p.x + p.w, p.w - p.x, p.y + p.w, p.w - p.y);
    sides.cmplt(glam::Vec4::splat(margin)).bitmask()
        | (u32::from(p.z < margin) << 4)
        | (u32::from(p.w - p.z < margin) << 5)
}

/// SilhouetteMask's buffers. `delta` is all zeros between uses (each use
/// clears the box it wrote).
#[derive(Default)]
struct MaskScratch {
    delta: Vec<i32>,
    full_rows: Vec<bool>,
    covered_span: Vec<(usize, usize)>,
    spans: Vec<(f32, f32)>,
    prev: Vec<bool>,
    cur: Vec<bool>,
    vstart: Vec<usize>,
}

/// Rasterize the union of clipped triangles and emit coverage boundaries only.
/// The boundaries are found in the box the coverage spans touched.
struct SilhouetteMask<'a> {
    rect: Rect, width: usize, height: usize, remaining_rows: usize,
    s: &'a mut MaskScratch,
    /// Rows another rasterizing thread has found full (they are full in
    /// the union too, so this one needn't add to them). Only ever set.
    shared: Option<&'a [std::sync::atomic::AtomicBool]>,
    /// The covered box (empty: no edges; all of them off screen, or no
    /// triangles at all): columns x0..x1, rows y0..y1.
    x0: usize, x1: usize, y0: usize, y1: usize,
}
impl<'a> SilhouetteMask<'a> {
    fn new(rect: Rect, s: &'a mut MaskScratch) -> Self {
        // Fixed viewport budget; work is O(triangles * height + covered box).
        let scale = (1024.0 / rect.width().max(rect.height()).max(1.0)).min(2.0);
        let width = (rect.width() * scale).ceil().max(1.0) as usize;
        let height = (rect.height() * scale).ceil().max(1.0) as usize;
        let n = (width + 1) * height;
        if s.delta.len() < n {
            s.delta.resize(n, 0);
        }
        s.full_rows.clear();
        s.full_rows.resize(height, false);
        s.covered_span.clear();
        s.covered_span.resize(height, (0, 0));
        Self { rect, width, height, remaining_rows: height, s, shared: None, x0: usize::MAX, x1: 0, y0: usize::MAX, y1: 0 }
    }

    /// The mask's height in rows for `rect` (as `new` sizes it).
    fn rows_for(rect: Rect) -> usize {
        let scale = (1024.0 / rect.width().max(rect.height()).max(1.0)).min(2.0);
        (rect.height() * scale).ceil().max(1.0) as usize
    }

    fn row_full(&self, y: usize) -> bool {
        self.s.full_rows[y] || self.shared.is_some_and(|s| s[y].load(std::sync::atomic::Ordering::Relaxed))
    }
    /// Whether everything a part inside lo..hi (model space) could cover is
    /// covered already: on each row its projected box spans, the row is
    /// full or its covered interval holds the box's columns. Only for a
    /// part wholly in front of the camera: then its points project inside
    /// the hull of its box's projected corners, and a pixel of margin on
    /// every side keeps rounding on the safe side. Its triangles would add
    /// nothing to the union (coverage > 0) the edges come from.
    fn rows_covered(&self, vp: Mat4, xf: Xform, size: Vec3, lo: Vec3, hi: Vec3) -> bool {
        if self.remaining_rows == 0 {
            return true;
        }
        let (mut min, mut max) = (glam::Vec2::splat(f32::INFINITY), glam::Vec2::splat(f32::NEG_INFINITY));
        for i in 0..8 {
            let c = Vec3::new(
                if i & 1 == 0 { lo.x } else { hi.x },
                if i & 2 == 0 { lo.y } else { hi.y },
                if i & 4 == 0 { lo.z } else { hi.z },
            );
            let p = vp * (xf.pos + xf.rot * (c * size)).extend(1.0);
            if !p.is_finite() || p.w <= 1e-4 {
                return false;
            }
            let q = glam::Vec2::new((p.x / p.w + 1.0) * 0.5 * self.width as f32, (1.0 - p.y / p.w) * 0.5 * self.height as f32);
            min = min.min(q);
            max = max.max(q);
        }
        let from = (min.y.floor() - 1.0).max(0.0) as usize;
        let to = ((max.y.ceil() + 1.0).max(0.0) as usize).min(self.height);
        let x0 = (min.x.floor() - 1.0).max(0.0) as usize;
        let x1 = ((max.x.ceil() + 1.0).max(0.0) as usize).min(self.width);
        (from..to).all(|y| {
            let span = self.s.covered_span[y];
            self.row_full(y) || (span.0 != span.1 && span.0 <= x0 && span.1 >= x1)
        })
    }

    fn triangle(&mut self, tri: [glam::Vec4; 3]) {
        if self.remaining_rows == 0 || tri.iter().any(|p| !p.is_finite()) { return; }
        let distance = |plane: usize, p: glam::Vec4| match plane {
            0 => p.x+p.w, 1 => p.w-p.x, 2 => p.y+p.w,
            3 => p.w-p.y, 4 => p.z, _ => p.w-p.z,
        };
        let [a, b, c] = tri.map(|p| clip_outcode(p, 0.0));
        if a & b & c != 0 {
            return;
        }
        // WGPU homogeneous frustum, including the near plane, before divide
        // (a triangle clipped by 6 planes has at most 9 corners).
        let mut poly = [glam::Vec4::ZERO; 9];
        let mut len = 3;
        poly[..3].copy_from_slice(&tri);
        if a | b | c != 0 {
            for plane in 0..6 {
                if poly[..len].iter().all(|p| distance(plane, *p) >= 0.0) { continue; }
                let mut clipped = [glam::Vec4::ZERO; 9];
                let mut n = 0;
                for i in 0..len {
                    let a=poly[i]; let b=poly[(i+1)%len];
                    let da=distance(plane, a); let db=distance(plane, b);
                    if da >= 0.0 && n < 9 { clipped[n] = a; n += 1; }
                    if (da >= 0.0) != (db >= 0.0) && n < 9 { clipped[n] = a+(b-a)*(da/(da-db)); n += 1; }
                }
                poly=clipped; len=n;
                if len<3 { return; }
            }
        }
        let mut points = [egui::Pos2::ZERO; 9];
        for (q, p) in points.iter_mut().zip(&poly[..len]) {
            *q = egui::pos2((p.x/p.w+1.0)*0.5*self.width as f32, (1.0-p.y/p.w)*0.5*self.height as f32);
        }
        let points = &points[..len];
        let lo=points.iter().map(|p| p.y).fold(f32::INFINITY,f32::min).max(0.0) as usize;
        let hi=points.iter().map(|p| p.y).fold(0.0,f32::max).ceil().min(self.height as f32) as usize;
        if hi <= lo { return; }
        // Each convex polygon edge contributes a linear x intersection. Compute
        // slopes once, and step them rather than divide for every row.
        let shared = self.shared;
        let spans = &mut self.s.spans;
        spans.clear();
        spans.resize(hi-lo, (f32::INFINITY, f32::NEG_INFINITY));
        for i in 0..points.len() {
            let mut a=points[i]; let mut b=points[(i+1)%points.len()];
            if a.y > b.y { std::mem::swap(&mut a,&mut b); }
            if a.y == b.y { continue; }
            let start=(a.y-0.5).ceil().clamp(lo as f32,hi as f32) as usize;
            let end=(b.y-0.5).ceil().clamp(lo as f32,hi as f32) as usize;
            let slope=(b.x-a.x)/(b.y-a.y);
            for y in start..end {
                if self.s.full_rows[y] || shared.is_some_and(|s| s[y].load(std::sync::atomic::Ordering::Relaxed)) { continue; }
                let x=a.x+slope*(y as f32+0.5-a.y);
                spans[y-lo].0=spans[y-lo].0.min(x);
                spans[y-lo].1=spans[y-lo].1.max(x);
            }
        }
        for y in lo..hi {
            if self.row_full(y) { continue; }
            let (left,right)=self.s.spans[y-lo];
            if left <= right {
                let start=(left-0.5).ceil().clamp(0.0,self.width as f32) as usize;
                let end=(right-0.5).ceil().clamp(0.0,self.width as f32) as usize;
                self.s.delta[y*(self.width+1)+start]+=1;
                self.s.delta[y*(self.width+1)+end]-=1;
                // (the box holds every delta written, so edges() can clear it)
                self.x0=self.x0.min(start); self.x1=self.x1.max(end);
                self.y0=self.y0.min(y); self.y1=self.y1.max(y+1);
                // A conservative connected covered interval permits saturation
                // across adjacent triangles. Disconnected spans never bridge gaps.
                let span=&mut self.s.covered_span[y];
                if span.0 == span.1 { *span=(start,end); }
                else if start <= span.1 && end >= span.0 {
                    span.0=span.0.min(start); span.1=span.1.max(end);
                }
                if span.0 == 0 && span.1 == self.width {
                    self.s.full_rows[y]=true;
                    self.remaining_rows-=1;
                    if let Some(s) = self.shared {
                        s[y].store(true, std::sync::atomic::Ordering::Relaxed);
                    }
                }
            }
        }
    }

    /// The coverage boundaries as line segments: horizontal ones row by row,
    /// then vertical ones column by column (one pass over the covered box).
    fn edges(self) -> Vec<[Pos2;2]> {
        let (x0, x1, y0, y1) = (self.x0, self.x1, self.y0, self.y1);
        if x0 > x1 || y0 >= y1 {
            return Vec::new();
        }
        let (w, h, rect) = (self.width, self.height, self.rect);
        let s = self.s;
        let stride = w + 1;
        let point=|x: usize,y: usize| rect.min+egui::vec2(x as f32/w as f32*rect.width(),y as f32/h as f32*rect.height());
        let bw = x1 - x0;
        s.prev.clear(); s.prev.resize(bw, false);
        s.cur.clear(); s.cur.resize(bw, false);
        s.vstart.clear(); s.vstart.resize(bw + 1, usize::MAX);
        let mut horizontal = Vec::new();
        let mut vertical: Vec<(usize, usize, [Pos2; 2])> = Vec::new();
        for y in y0..=y1 {
            // Row y's coverage (none past the box), clearing its deltas.
            if y < y1 {
                let row = &mut s.delta[y*stride + x0..=y*stride + x1];
                let mut coverage = 0;
                for (c, d) in s.cur.iter_mut().zip(row.iter()) { coverage += *d; *c = coverage > 0; }
                row.fill(0);
            } else {
                s.cur.fill(false);
            }
            // Only columns where row y differs from row y-1 can start or end
            // a boundary: elsewhere both rows, and so the horizontal (none)
            // and vertical boundaries (as last row), are the same.
            let Some((first, last)) = differing_span(&s.prev, &s.cur) else {
                std::mem::swap(&mut s.prev, &mut s.cur);
                continue;
            };
            // Horizontal boundaries on line y: row y-1 against row y.
            let mut start = None;
            for x in first..=last + 1 {
                let boundary = x < bw && s.prev[x] != s.cur[x];
                if boundary { start.get_or_insert(x); }
                else if let Some(a) = start.take() { horizontal.push([point(x0 + a, y), point(x0 + x, y)]); }
            }
            // Vertical boundaries on column x within row y: x-1 against x.
            for x in first..=last + 1 {
                let boundary = y < y1 && (x > 0 && s.cur[x-1]) != (x < bw && s.cur[x]);
                if boundary {
                    if s.vstart[x] == usize::MAX { s.vstart[x] = y; }
                } else if s.vstart[x] != usize::MAX {
                    let a = std::mem::replace(&mut s.vstart[x], usize::MAX);
                    vertical.push((x, a, [point(x0 + x, a), point(x0 + x, y)]));
                }
            }
            std::mem::swap(&mut s.prev, &mut s.cur);
        }
        vertical.sort_unstable_by_key(|v| (v.0, v.1));
        horizontal.extend(vertical.into_iter().map(|v| v.2));
        horizontal
    }
}

#[cfg(test)]
mod outline_tests {
    use super::*;

    // The silhouette mask before it reused its buffers and scanned only the
    // covered box: the new one must give the same edges in the same order.
    /// Rasterize the union of clipped triangles and emit coverage boundaries only.
    struct RefMask {
        rect: Rect, width: usize, height: usize, delta: Vec<i64>, full_rows: Vec<bool>, covered_span: Vec<(usize,usize)>, remaining_rows: usize,
        /// Whether any triangle covered a pixel: without one the mask is empty
        /// and has no edges (all of them off screen, or no triangles at all).
        covered: bool,
    }
    impl RefMask {
        fn new(rect: Rect) -> Self {
            // Fixed viewport budget; work is O(triangles * height + width * height).
            let scale = (1024.0 / rect.width().max(rect.height()).max(1.0)).min(2.0);
            let width = (rect.width() * scale).ceil().max(1.0) as usize;
            let height = (rect.height() * scale).ceil().max(1.0) as usize;
            Self { rect, width, height, delta: vec![0; (width + 1) * height], full_rows: vec![false; height], covered_span: vec![(0,0); height], remaining_rows: height, covered: false }
        }
        fn triangle(&mut self, tri: [glam::Vec4; 3]) {
            if self.remaining_rows == 0 || tri.iter().any(|p| !p.is_finite()) { return; }
            let mut poly = tri.to_vec();
            // WGPU homogeneous frustum, including the near plane, before divide.
            for plane in 0..6 {
                let distance = |p: glam::Vec4| match plane {
                    0 => p.x+p.w, 1 => p.w-p.x, 2 => p.y+p.w,
                    3 => p.w-p.y, 4 => p.z, _ => p.w-p.z,
                };
                if poly.iter().all(|p| distance(*p) >= 0.0) { continue; }
                let mut clipped = Vec::with_capacity(9);
                for i in 0..poly.len() {
                    let a=poly[i]; let b=poly[(i+1)%poly.len()];
                    let da=distance(a); let db=distance(b);
                    if da >= 0.0 { clipped.push(a); }
                    if (da >= 0.0) != (db >= 0.0) { clipped.push(a+(b-a)*(da/(da-db))); }
                }
                poly=clipped;
                if poly.len()<3 { return; }
            }
            let points: Vec<_> = poly.iter().map(|p| egui::pos2(
                (p.x/p.w+1.0)*0.5*self.width as f32,
                (1.0-p.y/p.w)*0.5*self.height as f32)).collect();
            let lo=points.iter().map(|p| p.y).fold(f32::INFINITY,f32::min).max(0.0) as usize;
            let hi=points.iter().map(|p| p.y).fold(0.0,f32::max).ceil().min(self.height as f32) as usize;
            // Each convex polygon edge contributes a linear x intersection. Compute
            // slopes once, and step them rather than divide for every row.
            let mut spans = vec![(f32::INFINITY, f32::NEG_INFINITY); hi-lo];
            for i in 0..points.len() {
                let mut a=points[i]; let mut b=points[(i+1)%points.len()];
                if a.y > b.y { std::mem::swap(&mut a,&mut b); }
                if a.y == b.y { continue; }
                let start=(a.y-0.5).ceil().clamp(lo as f32,hi as f32) as usize;
                let end=(b.y-0.5).ceil().clamp(lo as f32,hi as f32) as usize;
                let slope=(b.x-a.x)/(b.y-a.y);
                for y in start..end {
                    if self.full_rows[y] { continue; }
                    let x=a.x+slope*(y as f32+0.5-a.y);
                    spans[y-lo].0=spans[y-lo].0.min(x);
                    spans[y-lo].1=spans[y-lo].1.max(x);
                }
            }
            for y in lo..hi {
                if self.full_rows[y] { continue; }
                let (left,right)=spans[y-lo];
                if left <= right {
                    let start=(left-0.5).ceil().clamp(0.0,self.width as f32) as usize;
                    let end=(right-0.5).ceil().clamp(0.0,self.width as f32) as usize;
                    self.delta[y*(self.width+1)+start]+=1;
                    self.delta[y*(self.width+1)+end]-=1;
                    self.covered |= start < end;
                    // A conservative connected covered interval permits saturation
                    // across adjacent triangles. Disconnected spans never bridge gaps.
                    let span=&mut self.covered_span[y];
                    if span.0 == span.1 { *span=(start,end); }
                    else if start <= span.1 && end >= span.0 {
                        span.0=span.0.min(start); span.1=span.1.max(end);
                    }
                    if span.0 == 0 && span.1 == self.width {
                        self.full_rows[y]=true;
                        self.remaining_rows-=1;
                    }
                }
            }
        }

        fn edges(mut self) -> Vec<[Pos2;2]> {
            if !self.covered {
                return Vec::new();
            }
            for y in 0..self.height {
                let mut coverage=0;
                for x in 0..self.width { let i=y*(self.width+1)+x; coverage+=self.delta[i]; self.delta[i]=i64::from(coverage>0); }
            }
            let filled=|x: usize,y: usize| self.delta[y*(self.width+1)+x]>0;
            let point=|x: usize,y: usize| self.rect.min+egui::vec2(x as f32/self.width as f32*self.rect.width(),y as f32/self.height as f32*self.rect.height());
            let mut edges=Vec::new();
            // Join collinear boundary pixels so antialiased line caps do not
            // produce a dotted outline on straight edges.
            for y in 0..=self.height {
                let mut start=None;
                for x in 0..=self.width {
                    let boundary=x<self.width && ((y>0 && filled(x,y-1)) != (y<self.height && filled(x,y)));
                    if boundary { start.get_or_insert(x); }
                    else if let Some(a)=start.take() { edges.push([point(a,y),point(x,y)]); }
                }
            }
            for x in 0..=self.width {
                let mut start=None;
                for y in 0..=self.height {
                    let boundary=y<self.height && ((x>0 && filled(x-1,y)) != (x<self.width && filled(x,y)));
                    if boundary { start.get_or_insert(y); }
                    else if let Some(a)=start.take() { edges.push([point(x,a),point(x,y)]); }
                }
            }
            edges
        }
    }

    fn rng(seed: &mut u64) -> f32 {
        *seed = seed.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
        ((*seed >> 33) as f32) / (1u64 << 31) as f32
    }

    #[test]
    fn bounded_mask_matches_full_scan() {
        let mut seed = 7u64;
        let mut scratch = MaskScratch::default();
        let mut with_edges = 0;
        for case in 0..300 {
            let rect = match case % 4 {
                0 => Rect::from_min_size(egui::pos2(10.0, 50.0), egui::vec2(952.0, 569.0)),
                1 => Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(1930.0, 1010.0)),
                2 => Rect::from_min_size(egui::pos2(3.0, 7.0), egui::vec2(300.0, 700.0)),
                _ => Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(64.0, 40.0)),
            };
            let mut old = RefMask::new(rect);
            let mut new = SilhouetteMask::new(rect, &mut scratch);
            // A cluster of triangles somewhere (often partly off screen, some behind the camera).
            let n = 1 + (rng(&mut seed) * 60.0) as usize;
            let (cx, cy, r) = (rng(&mut seed) * 3.0 - 1.5, rng(&mut seed) * 3.0 - 1.5, rng(&mut seed) * 1.2 + 0.01);
            for _ in 0..n {
                let tri = [0, 1, 2].map(|_| {
                    let w = rng(&mut seed) * 2.0 + 0.2;
                    let z = rng(&mut seed) * 1.4 - 0.2;
                    glam::Vec4::new((cx + (rng(&mut seed) - 0.5) * r) * w, (cy + (rng(&mut seed) - 0.5) * r) * w, z * w, w)
                });
                old.triangle(tri);
                new.triangle(tri);
            }
            let (a, b) = (old.edges(), new.edges());
            with_edges += usize::from(!a.is_empty());
            assert_eq!(a, b, "case {case}");
        }
        assert!(with_edges > 150, "most cases draw an outline ({with_edges})");
        assert!(scratch.delta.iter().all(|d| *d == 0), "the scratch buffer is left clear");
    }

    // A part the frustum test skips draws nothing on its own: skipping it
    // can't change an outline.
    #[test]
    fn skipped_parts_draw_nothing() {
        let mut seed = 5u64;
        let (mut scratch, mut extra) = (MaskScratch::default(), Vec::new());
        let cube = rustudio_render::render::outline_triangles(rustudio_render::render::MeshKind::Block);
        let rect = Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(952.0, 569.0));
        let (mut skipped, mut drawn) = (0, 0);
        for _ in 0..4000 {
            let eye = Vec3::new(rng(&mut seed) * 40.0 - 20.0, rng(&mut seed) * 20.0, 30.0);
            let view = Mat4::look_at_rh(eye, Vec3::new(rng(&mut seed) * 20.0 - 10.0, 0.0, 0.0), Vec3::Y);
            let vp = Mat4::perspective_rh(1.2, rect.width() / rect.height(), 0.1, 300.0) * view;
            let pos = Vec3::new(rng(&mut seed) * 400.0 - 200.0, rng(&mut seed) * 100.0 - 50.0, rng(&mut seed) * 400.0 - 200.0);
            let rot = Mat3::from_euler(glam::EulerRot::YXZ, rng(&mut seed) * 6.0, rng(&mut seed) * 6.0, rng(&mut seed) * 6.0);
            let size = Vec3::new(rng(&mut seed) * 30.0 + 0.1, rng(&mut seed) * 30.0 + 0.1, rng(&mut seed) * 30.0 + 0.1);
            let xf = Xform { pos, rot };
            let (lo, hi) = triangle_bounds(cube).unwrap();
            let edges = union_edges(rect, vp, &[(std::sync::Arc::new(IndexedTris::new(cube)), xf, size)], &mut scratch, &mut extra, 1);
            if part_outside_frustum(vp, xf, size, lo, hi) {
                skipped += 1;
                assert!(edges.is_empty(), "a skipped part drew {} edges", edges.len());
            } else {
                drawn += usize::from(!edges.is_empty());
            }
        }
        assert!(skipped > 500 && drawn > 200, "both kinds happen ({skipped} skipped, {drawn} drawn)");
    }

    #[test]
    fn differing_span_matches_a_plain_scan() {
        let mut seed = 9u64;
        for n in [0usize, 1, 7, 8, 9, 15, 16, 17, 63, 64, 65, 300] {
            for _ in 0..200 {
                let a: Vec<bool> = (0..n).map(|_| rng(&mut seed) < 0.5).collect();
                let mut b = a.clone();
                let flips = (rng(&mut seed) * 4.0) as usize;
                for _ in 0..flips {
                    if n > 0 {
                        let k = ((rng(&mut seed) * n as f32) as usize).min(n - 1);
                        b[k] = !b[k];
                    }
                }
                let first = a.iter().zip(&b).position(|(x, y)| x != y);
                let want = first.map(|f| (f, n - 1 - a.iter().zip(&b).rev().position(|(x, y)| x != y).unwrap()));
                assert_eq!(differing_span(&a, &b), want, "n {n}");
            }
        }
    }

    // Behind a covering ground and wall, a small part's rows are all full
    // (and it is skipped), while one reaching into the sky's rows is not.
    #[test]
    fn covered_rows_skip_parts() {
        let rect = Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(952.0, 569.0));
        let vp = Mat4::perspective_rh(1.2, rect.width() / rect.height(), 0.1, 1000.0)
            * Mat4::look_at_rh(Vec3::new(0.0, 5.0, 60.0), Vec3::new(0.0, -10.0, 0.0), Vec3::Y);
        let cube = rustudio_render::render::outline_triangles(rustudio_render::render::MeshKind::Block);
        let ground = IndexedTris::new(cube);
        let mut scratch = MaskScratch::default();
        let mut mask = SilhouetteMask::new(rect, &mut scratch);
        for t in &ground.corners {
            let xf = Xform { pos: Vec3::new(0.0, -12.0, 0.0), rot: Mat3::IDENTITY };
            mask.triangle(t.map(|i| vp * (xf.pos + xf.rot * (ground.points[i as usize] * Vec3::new(4000.0, 1.0, 4000.0))).extend(1.0)));
        }
        let (lo, hi) = ground.bounds.unwrap();
        let near_ground = Xform { pos: Vec3::new(3.0, -10.0, 20.0), rot: Mat3::IDENTITY };
        assert!(mask.rows_covered(vp, near_ground, Vec3::ONE, lo, hi), "a small part low in the view");
        let tower = Xform { pos: Vec3::new(3.0, 40.0, 0.0), rot: Mat3::IDENTITY };
        assert!(!mask.rows_covered(vp, tower, Vec3::new(1.0, 100.0, 1.0), lo, hi), "a tower into the sky");
        let behind = Xform { pos: Vec3::new(0.0, 5.0, 80.0), rot: Mat3::IDENTITY };
        assert!(!mask.rows_covered(vp, behind, Vec3::splat(4.0), lo, hi), "a part behind the camera is never judged");
        let _ = mask.edges();
    }

    // Rasterizing a big outline on several threads gives the same edges, in
    // the same order, as one thread, and leaves every buffer clear.
    #[test]
    fn threaded_union_matches_one_thread() {
        let mut seed = 11u64;
        let (mut one, mut one_extra) = (MaskScratch::default(), Vec::new());
        let (mut many, mut many_extra) = (MaskScratch::default(), Vec::new());
        let cube = rustudio_render::render::outline_triangles(rustudio_render::render::MeshKind::Block);
        let mut with_edges = 0;
        for case in 0..40 {
            let rect = match case % 3 {
                0 => Rect::from_min_size(egui::pos2(10.0, 50.0), egui::vec2(952.0, 569.0)),
                1 => Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(1930.0, 1010.0)),
                _ => Rect::from_min_size(egui::pos2(3.0, 7.0), egui::vec2(300.0, 700.0)),
            };
            let eye = Vec3::new(rng(&mut seed) * 40.0 - 20.0, rng(&mut seed) * 20.0, 60.0);
            let view = Mat4::look_at_rh(eye, Vec3::ZERO, Vec3::Y);
            let proj = Mat4::perspective_rh(1.2, rect.width() / rect.height(), 0.1, 1000.0);
            let vp = proj * view;
            let n = 1 + (rng(&mut seed) * 400.0) as usize;
            let mut jobs: Vec<OutlineJob> = (0..n)
                .map(|i| {
                    let pos = Vec3::new(rng(&mut seed) * 120.0 - 60.0, rng(&mut seed) * 30.0 - 10.0, rng(&mut seed) * 120.0 - 80.0);
                    let rot = Mat3::from_euler(glam::EulerRot::YXZ, rng(&mut seed) * 6.0, rng(&mut seed) * 6.0, rng(&mut seed) * 6.0);
                    let size = Vec3::new(rng(&mut seed) * 8.0 + 0.2, rng(&mut seed) * 8.0 + 0.2, rng(&mut seed) * 8.0 + 0.2);
                    // Some free triangles too (open shapes, overlapping).
                    let tris: Vec<[Vec3; 3]> = if i % 5 == 0 {
                        (0..8).map(|_| [0, 1, 2].map(|_| Vec3::new(rng(&mut seed) - 0.5, rng(&mut seed) - 0.5, rng(&mut seed) - 0.5))).collect()
                    } else {
                        cube.to_vec()
                    };
                    let tris = if i % 2 == 0 { IndexedTris::new(&tris) } else { IndexedTris::unshared(&tris) };
                    (std::sync::Arc::new(tris), Xform { pos, rot }, size)
                })
                .collect();
            // Often a big ground and wall first, so whole rows fill up and
            // later parts are skipped (rows_covered).
            if case % 2 == 0 {
                for (pos, size) in [(Vec3::new(0.0, -12.0, 0.0), Vec3::new(4000.0, 1.0, 4000.0)), (Vec3::new(0.0, 0.0, -150.0), Vec3::new(4000.0, 300.0, 1.0))] {
                    jobs.insert(0, (std::sync::Arc::new(IndexedTris::new(cube)), Xform { pos, rot: Mat3::IDENTITY }, size));
                }
            }
            let a = union_edges(rect, vp, &jobs, &mut one, &mut one_extra, 1);
            // The way it was drawn before: each triangle's corners projected on their own.
            let mut plain = SilhouetteMask::new(rect, &mut many);
            for (tris, xf, size) in &jobs {
                for c in &tris.corners {
                    let t = c.map(|i| tris.points[i as usize]);
                    plain.triangle(t.map(|p| vp * (xf.pos + xf.rot * (p * *size)).extend(1.0)));
                }
            }
            assert_eq!(a, plain.edges(), "case {case}: shared corners");
            for threads in [2, 3, 4] {
                let b = union_edges(rect, vp, &jobs, &mut many, &mut many_extra, threads);
                assert_eq!(a, b, "case {case}, {threads} threads");
            }
            with_edges += usize::from(!a.is_empty());
        }
        assert!(with_edges > 30, "most cases draw an outline ({with_edges})");
        for s in std::iter::once(&many).chain(&many_extra).chain(std::iter::once(&one)) {
            assert!(s.delta.iter().all(|d| *d == 0), "buffers are left clear");
        }
    }
}
