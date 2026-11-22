//! Shared drawing and input for the player and Studio's 3D view.

use egui::{Color32, PointerButton, Pos2, Rect, Vec2};
use glam::{Mat3, Vec3};
use rbx_dom_weak::types::{Ref, Variant};
use rustudio_core::{math::Xform, mech};
use rustudio_engine::theme;
use rustudio_render::{render::{Frame, LineVertex, PartDraw, vf::Request}, scene};

use crate::game::Game;

impl Game {
    /// This frame's Highlight instances (highlight.rs): enabled, adorning
    /// (Adornee, else the parent) something in the Workspace; the parts
    /// they cover are the adornee and its descendant parts. The list of
    /// Highlights is kept by a class index fed from the change journal (a
    /// full walk only when the document is new), not by walking the place.
    fn highlight_draws(&mut self) -> Vec<rustudio_render::highlight::HighlightDraw> {
        use rbx_dom_weak::types::Variant as V;
        let refs = {
            let index = self
                .highlight_index
                .get_or_insert_with(|| rustudio_core::doc::ClassIndex::new(rustudio_core::doc::Reader::Highlights));
            index.update(&mut self.doc, |c| c == "Highlight");
            if index.list().is_empty() {
                return Vec::new();
            }
            index.list().to_vec()
        };
        let d = &self.doc;
        let Some(ws) = d.workspace() else { return Vec::new() };
        let color = |r: Ref, k: &str, dflt: [f32; 3]| match d.prop(r, k) {
            Some(V::Color3(c)) => [c.r, c.g, c.b],
            Some(V::Color3uint8(c)) => [c.r as f32 / 255.0, c.g as f32 / 255.0, c.b as f32 / 255.0],
            _ => dflt,
        };
        let num = |r: Ref, k: &str, dflt: f32| match d.prop(r, k) {
            Some(V::Float32(v)) => v,
            Some(V::Float64(v)) => v as f32,
            _ => dflt,
        };
        let mut out = Vec::new();
        for h in refs {
            if !d.exists(h) || matches!(d.prop(h, "Enabled"), Some(V::Bool(false))) {
                continue;
            }
            let adornee = match d.prop(h, "Adornee") {
                Some(V::Ref(a)) if d.exists(a) => a,
                _ => match d.dom.get_by_ref(h) {
                    Some(i) => i.parent(),
                    None => continue,
                },
            };
            if adornee.is_none() || !d.is_ancestor(ws, adornee) {
                continue;
            }
            let items: Vec<Ref> = std::iter::once(adornee)
                .chain(d.dom.descendants_of(adornee).map(|i| i.referent()))
                .filter(|r| self.scene.get(*r).is_some())
                .collect();
            if items.is_empty() {
                continue;
            }
            let fill = color(h, "FillColor", [1.0, 0.0, 0.0]);
            let outline = color(h, "OutlineColor", [1.0, 1.0, 1.0]);
            let fa = (1.0 - num(h, "FillTransparency", 0.5)).clamp(0.0, 1.0);
            let oa = (1.0 - num(h, "OutlineTransparency", 0.0)).clamp(0.0, 1.0);
            let on_top = !matches!(d.prop(h, "DepthMode"), Some(V::Enum(e)) if e.to_u32() == 1);
            out.push(rustudio_render::highlight::HighlightDraw {
                items,
                fill: [fill[0], fill[1], fill[2], fa],
                outline: [outline[0], outline[1], outline[2], oa],
                on_top,
            });
            if out.len() >= rustudio_render::highlight::MAX {
                break;
            }
        }
        out
    }
}

/// A Texture's tiling on a MeshPart (uv scale and offset over the face's
/// 0..1 image coordinates): Studio 0.747 anchors the tiles at the face's
/// centre, u = (x from the centre + OffsetStudsU) / StudsPerTileU (v the
/// same, downward), where a plain Part anchors them at the face's top-left
/// corner (tools/decalprobe.py, two tilings: 1.5 / 0.25, 0.5 and
/// 1.2, 0.9 / 0.4, 0.3, on a MeshPart and a Part side by side).
fn mesh_texture_uvt(tiling: Option<[f32; 4]>, w: f32, h: f32) -> [f32; 4] {
    match tiling {
        Some([su, sv, ou, ov]) => {
            let (su, sv) = (su.max(0.01), sv.max(0.01));
            [w / su, h / sv, ((ou - w * 0.5) / su).rem_euclid(1.0), ((ov - h * 0.5) / sv).rem_euclid(1.0)]
        }
        None => rustudio_render::render::PartDraw::NO_UVT,
    }
}

/// A SurfaceAppearance's composite, made on an asset worker
/// (TextureCache::composite): its colour map; NormalMap xy, RoughnessMap
/// and MetalnessMap (`maps` 0..3, absent ones flat) packed into one map at
/// the colour map's `w` × `h` with its data mip chain; the emissive mask
/// (`maps` 3).
fn surface_composite(
    base: rustudio_core::assets::PixelSource,
    maps: Vec<Option<rustudio_core::assets::PixelSource>>,
    w: u32,
    h: u32,
) -> Option<rustudio_core::assets::Composite> {
    let base = base.get()?;
    let px: Vec<_> = maps.iter().map(|m| m.as_ref().and_then(|m| m.get())).collect();
    let mut packed = vec![0; (w * h * 4) as usize];
    for y in 0..h {
        for x in 0..w {
            let at = ((y * w + x) * 4) as usize;
            packed[at..at + 4].copy_from_slice(&[128, 128, 255, 0]);
            for (k, im) in px.iter().take(3).enumerate() {
                if let Some(im) = im {
                    let q = (((y * im.height / h) * im.width + x * im.width / w) * 4) as usize;
                    if k == 0 {
                        packed[at..at + 2].copy_from_slice(&im.pixels[q..q + 2]);
                    } else {
                        packed[at + 1 + k] = im.pixels[q];
                    }
                }
            }
        }
    }
    let mut c = rustudio_core::assets::Composite::new(std::sync::Arc::unwrap_or_clone(base));
    c.mask = px.get(3).cloned().flatten();
    c.packed = Some((w, h, rustudio_core::texture::data_mip_chain(w, h, &packed)));
    Some(c)
}

/// Terrain water is drawn in pieces of this many consecutive triangles,
/// each frustum-culled (sync_terrain).
const WATER_PIECE_TRIS: usize = 4096;

/// A right-drag camera look in progress: the cursor is hidden and grabbed.
#[derive(Debug, Clone)]
pub struct LookGrab {
    /// Where the cursor was when the drag began; it is put back on release.
    pub start: Pos2,
    pub confined: bool,
    frames: u32,
    /// Consecutive frames the pointer still moved while supposedly locked.
    moving: u32,
}

/// Sum of raw (unaccelerated, unclamped) mouse motion this frame.
fn raw_mouse_delta(ctx: &egui::Context) -> Option<Vec2> {
    ctx.input(|i| {
        let mut sum = Vec2::ZERO;
        let mut any = false;
        for e in &i.raw.events {
            if let egui::Event::MouseMoved(d) = e {
                sum += *d;
                any = true;
            }
        }
        any.then_some(sum)
    })
}

impl Game {
    fn cursor_cmd(&mut self, ctx: &egui::Context, cmd: egui::ViewportCommand, label: &str) {
        self.cursor_log.push(label.to_string());
        if self.cursor_log.len() > 64 {
            self.cursor_log.remove(0);
        }
        if std::env::var_os("RUSTUDIO_CURSOR_LOG").is_some() {
            eprintln!("[cursor] {label} {cmd:?}");
        }
        ctx.send_viewport_cmd(cmd);
    }

    /// Lets a game grab go (mouse_lock::release): the cursor stays where it
    /// is held, never jumping to the middle of the view.
    fn release_game_grab(&mut self, ctx: &egui::Context, g: &LookGrab, label: &str) {
        use egui::viewport::CursorGrab;
        use egui::ViewportCommand as V;
        for c in rustudio_engine::mouse_lock::release(g.start, g.confined) {
            match c {
                rustudio_engine::mouse_lock::Cmd::Release => self.cursor_cmd(ctx, V::CursorGrab(CursorGrab::None), label),
                rustudio_engine::mouse_lock::Cmd::MoveTo(p) => {
                    self.cursor_cmd(ctx, V::CursorPosition(p), &format!("game-grab:restore {:.0},{:.0}", p.x, p.y))
                }
            }
        }
    }

    /// Right-drag look: grabs and hides the cursor while the button is held
    /// (Locked, falling back to Confined when the platform can't lock), reads
    /// raw mouse motion, and puts the cursor back where it was on release.
    /// Returns the look delta in pixels for this frame.
    pub fn update_look_grab(
        &mut self,
        ctx: &egui::Context,
        resp: &egui::Response,
        pointer_delta: Vec2,
    ) -> Option<Vec2> {
        use egui::viewport::CursorGrab;
        use egui::ViewportCommand as V;
        let secondary_down = ctx.input(|i| i.pointer.secondary_down());
        if resp.drag_started_by(PointerButton::Secondary) && self.look_grab.is_none() {
            let start = ctx
                .input(|i| i.pointer.press_origin())
                .or(resp.hover_pos())
                .unwrap_or(resp.rect.center());
            let confined = self.grab_use_confined;
            self.look_grab = Some(LookGrab {
                start,
                confined,
                frames: 0,
                moving: 0,
            });
            let mode = if confined {
                CursorGrab::Confined
            } else {
                CursorGrab::Locked
            };
            self.cursor_cmd(
                ctx,
                V::CursorGrab(mode),
                if confined {
                    "grab:confined"
                } else {
                    "grab:locked"
                },
            );
            self.cursor_cmd(ctx, V::CursorVisible(false), "hide");
        }
        let Some(grab) = self.look_grab.as_mut() else {
            return None;
        };
        if !secondary_down {
            let (start, confined) = (grab.start, grab.confined);
            self.look_grab = None;
            // (a Locked cursor never moved; a Confined one goes back after
            // the release: mouse_lock::release)
            self.cursor_cmd(ctx, V::CursorGrab(CursorGrab::None), "grab:none");
            if confined {
                self.cursor_cmd(ctx, V::CursorPosition(start), "restore-position");
            }
            self.cursor_cmd(ctx, V::CursorVisible(true), "show");
            return None;
        }
        grab.frames += 1;
        // A locked cursor never moves. If it still does after a couple of
        // frames, locking isn't supported here: confine it instead.
        if !grab.confined && grab.frames > 3 && rustudio_engine::mouse_lock::may_fall_back(rustudio_engine::mouse_lock::platform_locks()) {
            let moved = ctx
                .input(|i| i.pointer.latest_pos())
                .is_some_and(|p| (p - grab.start).length() > 24.0);
            if moved {
                grab.confined = true;
                self.grab_use_confined = true;
                self.cursor_cmd(
                    ctx,
                    V::CursorGrab(CursorGrab::Confined),
                    "grab:confined-fallback",
                );
            }
        }
        Some(raw_mouse_delta(ctx).unwrap_or(pointer_delta))
    }

    /// Mouse over the game view while playing: motion and wheel go to the
    /// game (InputChanged / PointerAction); the cursor is grabbed while the
    /// game's UserInputService.MouseBehavior isn't Default (LockCurrentPosition
    /// during a right-drag orbit, LockCenter for shift-lock and first person),
    /// as Roblox does, and comes back where it was.
    pub fn update_game_mouse(
        &mut self,
        ctx: &egui::Context,
        resp: &egui::Response,
        pointer_delta: Vec2,
        notches: f32,
    ) {
        use egui::viewport::CursorGrab;
        use egui::ViewportCommand as V;
        let Some((behavior, icon, menu)) = self
            .play
            .as_ref()
            .map(|s| (s.mouse_behavior, s.mouse_icon, s.game.menu_open))
        else {
            return;
        };
        // The in-game menu frees the cursor (SettingsHub's Modal button and
        // ForceShow icon override): no grab, the arrow shows, no mouse-look.
        // MouseBehavior itself keeps the game's value and applies again on
        // Resume.
        // An editor dialog (egui Modal: "Discard unsaved changes?", …) frees
        // it the same way, so its buttons can be clicked during first person
        // or shift lock; the game's grab comes back when the dialog closes.
        let menu = menu || ctx.memory(|m| m.top_modal_layer()).is_some();
        let (behavior, icon) = if menu { (0, true) } else { (behavior, icon) };
        // A shown GuiButton.Modal lets go of the cursor too, unless the
        // right button is down (gui_modal.rs; asked only while the game
        // wants a lock).
        let behavior = if behavior != 0
            && !ctx.input(|i| i.pointer.secondary_down())
            && self.play.as_ref().is_some_and(|s| s.showing == rustudio_engine::runtime::Side::Client)
            && rustudio_engine::gui_modal::modal_shown(&self.doc, &self.gui.layout)
        {
            0
        } else {
            behavior
        };
        if menu {
            self.free_cursor_for_menu(ctx);
        } else if let Some(s) = self.play.as_mut() {
            s.game.menu.cursor_freed = false;
        }
        // LockCenter holds whatever the pointer is over, as Roblox does; it
        // only needs the pointer inside the game view to begin. A window that
        // lost focus has lost its grab too, so take it again on return.
        let (focused, inside) = ctx.input(|i| {
            (
                i.focused,
                i.pointer.latest_pos().is_some_and(|p| resp.rect.contains(p)),
            )
        });
        if !focused {
            // Ask for a real lock again on return; a confine fallback found
            // earlier is re-detected if the platform still can't lock.
            self.grab_use_confined = !rustudio_engine::mouse_lock::platform_locks();
            if let Some(g) = self.game_grab.take() {
                self.release_game_grab(ctx, &g, "game-grab:unfocused");
            }
        }
        let begin = if behavior == 1 { inside } else { resp.hovered() };
        let want = focused && behavior != 0 && (begin || self.game_grab.is_some());
        if want && self.game_grab.is_none() {
            let start = ctx
                .input(|i| i.pointer.latest_pos())
                .or(resp.hover_pos())
                .unwrap_or(resp.rect.center());
            let confined = self.grab_use_confined;
            self.game_grab = Some(LookGrab {
                start,
                confined,
                frames: 0,
                moving: 0,
            });
            self.cursor_cmd(
                ctx,
                V::CursorGrab(if confined {
                    CursorGrab::Confined
                } else {
                    CursorGrab::Locked
                }),
                "game-grab",
            );
        }
        if !want {
            // (an open menu already let go: free_cursor_for_menu)
            if let Some(g) = self.game_grab.take() {
                self.release_game_grab(ctx, &g, "game-grab:none");
            }
        }
        let grabbed = self.game_grab.is_some();
        let mut fallback = false;
        if let Some(g) = self.game_grab.as_mut() {
            g.frames += 1;
            // A locked cursor never moves. If it keeps moving for several
            // frames in a row, locking isn't supported here: confine it
            // instead. One jump isn't enough: coming back to the window
            // (alt-tab) reports the pointer's travel as a single big delta,
            // and taking that for "can't lock" left the cursor free.
            // (Never on Wayland: see mouse_lock::may_fall_back.)
            if !g.confined && g.frames > 3 && rustudio_engine::mouse_lock::may_fall_back(rustudio_engine::mouse_lock::platform_locks()) {
                g.moving = if pointer_delta.length() > 24.0 { g.moving + 1 } else { 0 };
                if g.moving >= 3 {
                    g.confined = true;
                    fallback = true;
                }
            }
            // The OS cursor stays where it was when the lock began (Roblox's
            // LockCenter mouse is the middle of the view, but that is the
            // game's mouse: mouse_lock::game_pointer, and the icon drawn
            // there). Nothing moves a Locked cursor: a position sent then is a
            // Wayland hint that Hyprland jumps the cursor to on release.
            if let Some(at) = rustudio_engine::mouse_lock::hold_at(g.start, g.confined) {
                ctx.send_viewport_cmd(V::CursorPosition(at));
            }
        }
        if fallback {
            self.grab_use_confined = true;
            self.cursor_cmd(
                ctx,
                V::CursorGrab(CursorGrab::Confined),
                "game-grab:confined-fallback",
            );
        }
        // While LockCenter holds the mouse the system cursor hides: Roblox
        // draws the game's icon in the middle of the view instead (viewport
        // draw, mouse_lock::locked_icon).
        let hidden = !icon || (grabbed && behavior == 1);
        if hidden != self.game_cursor_hidden {
            self.game_cursor_hidden = hidden;
            self.cursor_cmd(
                ctx,
                V::CursorVisible(!hidden),
                if hidden {
                    "game-cursor:hide"
                } else {
                    "game-cursor:show"
                },
            );
        }
        // Roblox reports mouse motion deltas only while the cursor is locked;
        // otherwise InputChanged carries the new position with a zero Delta.
        // (Without raw motion, egui's pointer delta stands in, but only for
        // frames where the pointer really moved: a button press alone can
        // carry the first known position, which would read as one huge
        // flick and throw a first-person camera at the floor.)
        let pointer_moved = ctx.input(|i| {
            i.raw.events.iter().any(|e| matches!(e, egui::Event::PointerMoved(_)))
        });
        // The frame the grab begins carries the pointer's travel into the
        // view (or, with no position known before, its whole position): no
        // look motion from that either.
        let first = self.game_grab.as_ref().is_some_and(|g| g.frames <= 1);
        let d = if grabbed && first {
            Vec2::ZERO
        } else if grabbed {
            raw_mouse_delta(ctx).unwrap_or(if pointer_moved { pointer_delta } else { Vec2::ZERO })
        } else {
            Vec2::ZERO
        };
        let moved = resp.hovered() && pointer_delta != Vec2::ZERO;
        if menu {
            return;
        }
        if let Some(s) = self.play.as_mut() {
            s.mouse_delta += glam::Vec2::new(d.x, d.y);
            s.mouse_moved |= moved || d != Vec2::ZERO;
            if resp.hovered() || grabbed {
                s.wheel += notches * mech::WHEEL_INPUT_PER_NOTCH;
            }
        }
    }

    /// While the in-game menu is open nothing holds the cursor: SettingsHub
    /// frees it (Modal + ForceShow) whatever the game's MouseBehavior. On the
    /// frame it opens the window's grab is let go and the arrow shown even
    /// if no grab is on record (a right-drag look, or a grab the platform
    /// kept), so hovering any page can't find the cursor held; after that a
    /// grab of either kind is dropped the moment one appears.
    fn free_cursor_for_menu(&mut self, ctx: &egui::Context) {
        use egui::viewport::CursorGrab;
        use egui::ViewportCommand as V;
        let opened = self.play.as_mut().is_some_and(|s| !std::mem::replace(&mut s.game.menu.cursor_freed, true));
        let held = self.game_grab.take().is_some() | self.look_grab.take().is_some();
        if opened || held {
            // (the cursor stays where the lock held it, as Roblox's does)
            self.cursor_cmd(ctx, V::CursorGrab(CursorGrab::None), "game-grab:menu");
        }
        if opened {
            self.game_cursor_hidden = false;
            self.cursor_cmd(ctx, V::CursorVisible(true), "game-cursor:show");
        }
    }

    /// LockCenter holds the mouse now (shift lock, first person): the
    /// game's mouse is the middle of the view (mouse_lock::game_pointer).
    pub fn lock_center_held(&self) -> bool {
        self.game_grab.is_some()
            && self
                .play
                .as_ref()
                .is_some_and(|s| s.mouse_behavior == 1 && !s.game.menu_open)
    }

    /// Lets go of a game cursor grab (after Stop).
    pub fn release_game_mouse(&mut self, ctx: &egui::Context) {
        use egui::ViewportCommand as V;
        if let Some(g) = self.game_grab.take() {
            self.release_game_grab(ctx, &g, "game-grab:none");
        }
        if std::mem::take(&mut self.game_cursor_hidden) {
            self.cursor_cmd(ctx, V::CursorVisible(true), "game-cursor:show");
        }
    }

    /// Swaps placeholders for loaded mesh geometry where the mesh is local.
    /// Swaps placeholders for loaded meshes, adds mesh textures, classic
    /// clothing on character body parts, and Decal / Texture quads.
    /// Hands the renderer the parts that changed since last frame, with
    /// loaded meshes, textures, classic clothing and Decal / Texture quads
    /// resolved. Parts whose assets are still loading are redone when an
    /// asset arrives.
    /// EditableMesh edits since the last frame (runtime/editable.rs
    /// `sync_live`): value edits go into the loaded copy's vertex buffer in
    /// place; rebuilt ones are dropped from the mesh cache and their parts
    /// returned to be redone.
    fn sync_live_meshes(&mut self, removed: &[Ref]) -> (Vec<Ref>, bool) {
        use rustudio_engine::runtime::editable::{self, LivePatch};
        let _s = rustudio_core::bench::scope("sd_live_mesh");
        editable::live_users_gone(removed);
        let patches = editable::sync_live();
        if patches.is_empty() {
            return (Vec::new(), false);
        }
        let mut redo = Vec::new();
        let mut patched = false;
        let queue = self.render_state.as_ref().map(|rs| rs.queue.clone());
        for (id, p) in patches {
            match p {
                LivePatch::Ranges(runs) => {
                    let ok = match (self.meshes.loaded(&id), self.renderer.as_mut(), &queue) {
                        (Some(m), Some(r), Some(q)) => runs.iter().all(|(first, verts)| r.write_mesh_vertices(q, m.index, *first, verts)),
                        _ => false,
                    };
                    if !ok {
                        self.meshes.forget(&id);
                    }
                    patched = true;
                }
                LivePatch::Rebuild { users, skinned } => {
                    self.meshes.forget(&id);
                    if skinned {
                        rustudio_render::skin::forget_base(&id);
                    }
                    redo.extend(users);
                }
                LivePatch::SkinAttrs { users, colors, uvs } => {
                    // In place: the skin base, the shared rest mesh and
                    // each posed part's slot (its compute pass runs again).
                    // (a part without Bone instances never asked for the
                    // base: parse it now, the bytes are still current)
                    if rustudio_render::skin::cached_base(&id).is_none() {
                        if let Some(bytes) = rustudio_core::assets::generated(&id) {
                            rustudio_render::skin::base(&id, &bytes);
                        }
                    }
                    let done = match (rustudio_render::skin::patch_base(&id, &colors, &uvs), self.renderer.as_mut(), &queue) {
                        (Some(b), Some(r), Some(q)) if !rustudio_render::skin::cpu_skinning() => {
                            let plane = b.extent.extend(0.0).to_array();
                            let shared = self.meshes.loaded(&id).is_none_or(|m| {
                                r.rewrite_mesh(q, m.index, &b.positions, &b.normals, &b.uvs, &b.colors, plane)
                            });
                            shared
                                && users.iter().all(|u| {
                                    rustudio_render::skin::slot_of(*u, b.positions.len()).is_none_or(|slot| {
                                        r.rewrite_mesh(q, slot, &b.positions, &b.normals, &b.uvs, &b.colors, plane)
                                    })
                                })
                        }
                        _ => false,
                    };
                    if done {
                        patched = true;
                    } else {
                        let users = editable::live_resnap(&id);
                        self.meshes.forget(&id);
                        rustudio_render::skin::forget_base(&id);
                        redo.extend(users);
                    }
                }
            }
        }
        (redo, patched)
    }

    pub fn sync_draws(&mut self) {
        // EditableMeshes edited since the last frame: their parts' copies
        // patched in place (or rebuilt and the parts redone), in the world
        // and in ViewportFrames.
        let mut removed = self.scene.peek_removed();
        removed.extend(self.vf_scene.peek_removed());
        let (live_redo, patched) = self.sync_live_meshes(&removed);
        if patched {
            if let Some(r) = self.renderer.as_mut() {
                r.touch_layers();
            }
        }
        self.sync_draws_of(false, &live_redo);
        // ViewportFrames' parts: the same resolution, their own scene,
        // cache and renderer layer.
        std::mem::swap(&mut self.scene, &mut self.vf_scene);
        std::mem::swap(&mut self.draw_cache, &mut self.vf_draw_cache);
        std::mem::swap(&mut self.draws_asset_gen, &mut self.vf_draws_asset_gen);
        let stats = self.draw_stats;
        self.sync_draws_of(true, &live_redo);
        self.draw_stats = stats;
        std::mem::swap(&mut self.scene, &mut self.vf_scene);
        std::mem::swap(&mut self.draw_cache, &mut self.vf_draw_cache);
        std::mem::swap(&mut self.draws_asset_gen, &mut self.vf_draws_asset_gen);
    }

    /// `sync_draws` for one scene: the world's, or (`layered`) the parts
    /// in ViewportFrames, each in its frame's renderer layer.
    fn sync_draws_of(&mut self, layered: bool, live_redo: &[Ref]) {
        use rustudio_core::assets::{clothing_role, composite_over, template_box, ClothingLayer, Composed, Composite, PixelSource};
        use rustudio_render::render::{PartDraw, TexMode};
        let mut changes = self.scene.take_changes();
        changes.changed.extend(live_redo.iter().copied().filter(|r| self.scene.get(*r).is_some()));
        if layered {
            // (the world's light grid doesn't see these)
        } else if changes.full || !changes.changed.is_empty() || !changes.removed.is_empty() || !changes.restyled.is_empty() {
            self.lighting.lightgrid_dirty = true;
            self.lighting.lightocc_dirty = true;
        } else if !changes.moved.is_empty() {
            // Moving parts (characters, zombies, physics) refresh the grid
            // on a slower clock: a rebuild gathers every part around it.
            self.lighting.lightgrid_moves = true;
            self.lighting.lightocc_dirty = true;
        }
        let regen = self.asset_gen != self.draws_asset_gen;
        if regen && !layered {
            self.lighting.lightgrid_dirty = true;
        }
        if !changes.full
            && changes.changed.is_empty()
            && changes.removed.is_empty()
            && changes.moved.is_empty()
            && changes.restyled.is_empty()
            && !regen
            && self.draw_cache.deferred.is_empty()
        {
            return;
        }
        self.draws_asset_gen = self.asset_gen;
        let doc_dir = self
            .doc
            .path
            .as_ref()
            .and_then(|p| p.parent())
            .map(|p| p.to_path_buf());
        let Game { scene, assets, render_state, renderer, meshes, textures, clothing_meshes, draw_cache, .. } = self;
        let (Some(renderer), Some(rs)) = (renderer.as_mut(), render_state.as_ref()) else {
            return;
        };
        let (device, queue) = (rs.device.clone(), rs.queue.clone());
        let dir = doc_dir.as_deref();
        let use2022 = scene::use_2022_materials(&self.doc);
        if changes.full {
            if layered {
                renderer.clear_layered_items();
            } else {
                renderer.clear_items();
            }
            *draw_cache = DrawCache::default();
        }
        for r in &changes.removed {
            if layered {
                renderer.remove_layered_item(*r);
            } else {
                renderer.remove_item(*r);
            }
            draw_cache.forget(*r);
            rustudio_render::skin::forget_part(*r);
        }
        // Moved parts: carry their resolved draws along rigidly.
        for r in &changes.moved {
            let (Some(sp), Some(old)) = (scene.get(*r), draw_cache.xf.get(r).copied()) else {
                continue;
            };
            // A CFrame written with the value it already had.
            if sp.xf.pos.abs_diff_eq(old.pos, 1e-5) && sp.xf.rot.abs_diff_eq(old.rot, 1e-6) {
                continue;
            }
            if renderer.move_item(*r, old, sp.xf) {
                draw_cache.xf.insert(*r, sp.xf);
            }
        }
        // Restyled parts (Transparency, Color, Reflectance, CastShadow):
        // their one draw is patched in place, as Studio updates a part's
        // instance data; anything more involved is resolved again below.
        let mut restyle_full = Vec::new();
        if !changes.full {
            let _s = rustudio_core::bench::scope("sd_restyle");
            for r in &changes.restyled {
                let patched = match (scene.get(*r), draw_cache.style.get(r), renderer.item_draws(*r)) {
                    (Some(sp), Some(&(mul, alpha_forced)), Some([old])) => {
                        let mut d = *old;
                        d.color = [0, 1, 2].map(|c| sp.draw.color[c] * mul[c]);
                        if !alpha_forced {
                            d.alpha = sp.draw.alpha;
                        }
                        d.reflectance = sp.draw.reflectance;
                        d.cast_shadow = sp.draw.cast_shadow;
                        if d.surface_tint.is_some() {
                            d.surface_tint = sp.draw.surface_tint;
                        }
                        Some(d)
                    }
                    _ => None,
                };
                match patched {
                    Some(d) => renderer.set_item(*r, vec![d]),
                    None => restyle_full.push(*r),
                }
            }
        }
        // What the last frame's budget left goes first.
        let mut todo: Vec<Ref> = std::mem::take(&mut draw_cache.deferred);
        if changes.full {
            todo.extend(scene.parts.iter().map(|p| p.r));
        } else {
            todo.extend(changes.changed);
        }
        todo.extend(restyle_full);
        if regen {
            // A part waiting only on textures is redone when one of them can
            // get further (downloaded, decoded or failed), not each time any
            // asset arrives: Place B's first minute re-resolved ~150 parts a
            // frame that way while its textures streamed in.
            let ready: Vec<Ref> = draw_cache
                .waiting
                .iter()
                .copied()
                .filter(|r| match draw_cache.wait_tex.get(r) {
                    Some(ids) => ids.iter().any(|id| textures.can_progress(id, assets)),
                    None => true,
                })
                .collect();
            for r in &ready {
                draw_cache.waiting.remove(r);
            }
            todo.extend(ready);
        }
        // A layered garment is drawn by a body part: what redraws the
        // garment (its SurfaceAppearance's Color, say) redraws that part.
        let drawers: Vec<Ref> = todo
            .iter()
            .filter(|r| rustudio_render::wrap::is_layered_handle(&self.doc, **r))
            .filter_map(|r| {
                let model = self.doc.dom.get_by_ref(*r).and_then(|h| self.doc.dom.get_by_ref(h.parent())).map(|a| a.parent())?;
                match rustudio_render::wrap::fitted(&self.doc, model) {
                    rustudio_render::wrap::Lookup::Ready(fit) => fit.garments.iter().find(|g| g.handle == *r).map(|g| g.owner),
                    _ => None,
                }
            })
            .collect();
        todo.extend(drawers);
        let mut seen = std::collections::HashSet::new();
        todo.retain(|r| seen.insert(*r));
        // A whole new scene resolves at once; otherwise at most a budget's
        // worth a frame (characters and their meshes, textures and
        // clothing arriving together took tens of ms of one frame), the
        // rest next frame, as Roblox streams such parts in.
        // Runs on the simulated clock (--fixed-dt captures) resolve all.
        let budget = (!changes.full && !rustudio_core::sim_clock::on())
            .then(|| std::time::Duration::from_secs_f32(rustudio_core::mech::DRAW_SYNC_BUDGET_MS / 1000.0));
        let started = std::time::Instant::now();
        let mut todo = todo.into_iter();
        while let Some(r) = todo.next() {
            if budget.is_some_and(|b| started.elapsed() >= b) {
                draw_cache.deferred.push(r);
                draw_cache.deferred.extend(todo);
                rustudio_core::bench::add("count: sd_deferred", draw_cache.deferred.len() as f64, 1);
                break;
            }
            let Some(sp) = scene.get(r) else { continue };
            renderer.set_layer(r, sp.layer);
            // Layered garments are drawn with the body parts they fit.
            if rustudio_render::wrap::is_layered_handle(&self.doc, r) {
                renderer.set_item(r, Vec::new());
                continue;
            }
            let mut d = sp.draw;
            let mut extra = Vec::new();
            let mut waiting = false;
            // How the drawn colour relates to the part's (material packs
            // divide out their fallback albedo) and whether its opacity is
            // the part's own: what a restyle patches with.
            let mut color_mul = [1.0f32; 3];
            let mut alpha_forced = false;
            // Textures still on their way (the part is redone when one can
            // get further); `waiting` is for everything else.
            let mut tex_wait: Vec<String> = Vec::new();
            // A file mesh with no mesh id (an empty SpecialMesh FileMesh or
            // MeshPart): nothing to draw, as in Studio (Place B's striped
            // ground cover, round 23).
            if d.placeholder && sp.mesh_asset.is_none() {
                d.alpha = 0.0;
                d.placeholder = false;
                alpha_forced = true;
            }
            // A body part under layered clothing drops the faces its
            // garments hide (hsr.rs), drawn from a trimmed copy of its mesh.
            let mut hsr_wait = false;
            let hsr_id = match &sp.mesh_asset {
                Some(asset) if asset.fit_to_part && self.doc.class_of(sp.r).is_some_and(|c| c == "MeshPart") => {
                    match rustudio_render::hsr::body_mesh(&self.doc, sp.r, &asset.id) {
                        rustudio_render::hsr::Look::Ready(id) => Some(id),
                        rustudio_render::hsr::Look::Pending => {
                            hsr_wait = true;
                            None
                        }
                        rustudio_render::hsr::Look::None => None,
                    }
                }
                _ => None,
            };
            // A skinned mesh (its id and base) and this frame's pose (None
            // inside: at rest), for its decals.
            let mut skin_base: Option<(String, std::sync::Arc<rustudio_render::skin::Base>)> = None;
            let mut skin_pose: Option<Option<(Vec<Xform>, Vec3, Vec3)>> = None;
            // The drawn mesh has UVs of its own (MeshCache::authored_uvs).
            let mut mesh_uvs = true;
            if let Some(asset) = &sp.mesh_asset {
                waiting = true;
                let sd = rustudio_core::bench::scope("sd_mesh_get");
                // (a WrapDeformer's result shown in place of the part's mesh)
                let shown = rustudio_engine::runtime::editable::shown_mesh(sp.r);
                let mesh_id = shown.as_deref().or(hsr_id.as_deref()).unwrap_or(&asset.id);
                rustudio_engine::runtime::editable::live_user(mesh_id, sp.r);
                let got = meshes.live(mesh_id, &mut |v| renderer.add_mesh_vertices(&device, v)).or_else(|| {
                    meshes.get_colored(mesh_id, asset.vertex_colors, dir, assets, &mut |p, n, u, c, planes, lods| {
                        renderer.add_mesh_lods(&device, p, n, u, c, planes, lods)
                    })
                });
                drop(sd);
                if let Some(m) = got {
                    waiting = false;
                    mesh_uvs = meshes.authored_uvs(mesh_id);
                    d.custom_mesh = Some(m.index);
                    d.placeholder = false;
                    if { static TRACE: std::sync::OnceLock<bool> = std::sync::OnceLock::new(); *TRACE.get_or_init(|| std::env::var_os("RUSTUDIO_MESH_DENSITY_TRACE").is_some()) } && self.doc.class_of(sp.r).is_some_and(|c| c == "MeshPart") {
                        static SEEN: std::sync::OnceLock<std::sync::Mutex<std::collections::HashSet<String>>> = std::sync::OnceLock::new();
                        if SEEN.get_or_init(Default::default).lock().unwrap().insert(format!("{:?}", sp.r)) {
                            eprintln!("MESHPART_SOURCE {:?} Size={:?} MeshSize={:?} InitialSize={:?} bounds_extent={:?} uv={}", sp.r, d.size, self.doc.prop(sp.r,"MeshSize"), self.doc.prop(sp.r,"InitialSize"), m.extent, if sp.texture_id.is_some() { "authored" } else { "built-in stud projection" });
                        }
                    }
                    // Studio: source mesh units
                    // scale by current Size / stored InitialSize (MeshSize).
                    // The upload normalized by extent, so restore that factor.
                    if asset.fit_to_part && self.doc.class_of(sp.r).is_some_and(|c| c == "MeshPart") {
                        if let Some(rbx_dom_weak::types::Variant::Vector3(v)) = self.doc.prop(sp.r, "InitialSize") {
                            let initial = glam::Vec3::new(v.x, v.y, v.z);
                            if initial.is_finite() && initial.min_element() > 1e-4 {
                                d.size *= m.extent / initial;
                            }
                        }
                    }
                    if !asset.fit_to_part {
                        d.size = m.extent * asset.scale;
                        d.xf.pos += d.xf.rot * asset.offset;
                        if asset.origin {
                            d.xf.pos += d.xf.rot * (m.center * asset.scale);
                        }
                    }
                    // A MeshPart with Bones: its skinned mesh, deformed by
                    // the Bones' current frames (skin.rs).
                    let _skin_scope = rustudio_core::bench::scope("sd_skin");
                    if asset.fit_to_part && rustudio_render::skin::part_has_bones(&self.doc, sp.r, &asset.id) {
                        // (the parsed base once there is one: no fetch of the
                        // bytes per redo; parsed on the asset workers, the
                        // part drawn at rest until then)
                        let base = rustudio_render::skin::cached_base(&asset.id).or_else(|| match assets.content(&asset.id, dir) {
                            rustudio_core::assets::Fetch::Ready(bytes) => rustudio_render::skin::base_async(&asset.id, bytes).unwrap_or_else(|| {
                                waiting = true;
                                None
                            }),
                            _ => None,
                        });
                        {
                            if let Some(base) = base {
                                skin_base = Some((asset.id.clone(), base.clone()));
                                if rustudio_render::skin::cpu_skinning() {
                                    if let Some(idx) = rustudio_render::skin::skinned_mesh(&self.doc, sp.r, &base, &mut |slot, p, n, u, c| {
                                        renderer.set_mesh(&device, &queue, slot, p, n, u, c)
                                    }) {
                                        d.custom_mesh = Some(idx);
                                    }
                                } else if let Some((palette, scale, origin)) = rustudio_render::skin::pose(&self.doc, sp.r, &base) {
                                    skin_pose = Some(Some((palette.clone(), scale, origin)));
                                    // GPU skinning (skin_gpu.rs): only the palette goes up.
                                    let corners = base.positions.len();
                                    let have = rustudio_render::skin::slot_of(sp.r, corners);
                                    // Redone with the pose its slot already holds
                                    // (another reason, or a Bone it doesn't use):
                                    // no skinning pass, no upload.
                                    let idx = match have {
                                        Some(i) if rustudio_render::skin::same_pose(sp.r, 0, &base, &palette, scale, origin) => i,
                                        _ => {
                                            if have.is_none() {
                                                rustudio_render::skin::same_pose(sp.r, 0, &base, &palette, scale, origin);
                                            }
                                            let req = rustudio_render::skin_gpu::Request { key: &asset.id, base: &base, palette: &palette, scale, origin };
                                            renderer.skin_mesh(&device, &queue, have, &req)
                                        }
                                    };
                                    rustudio_render::skin::set_slot(sp.r, idx, corners);
                                    d.custom_mesh = Some(idx);
                                }
                            }
                        }
                    }
                } else {
                    // No mesh yet (downloading) or none to have (Roblox refused
                    // the download, or it doesn't parse): Studio draws such a
                    // part as nothing until its mesh is in (tools/fidelity
                    // Place B), not a striped box. A pending one is redone when
                    // it arrives.
                    if matches!(meshes.state(&asset.id), Some(Err(_))) {
                        waiting = false;
                    }
                    d.alpha = 0.0;
                    d.placeholder = false;
                    alpha_forced = true;
                }
            }
            let sd_tex = rustudio_core::bench::scope("sd_texture");
            if let Some(tid) = &sp.texture_id {
                // A SurfaceAppearance's composite is made from its images'
                // pixels: kept past their upload until it is made (other
                // textures' pixels go once they are on the GPU).
                let surface = rustudio_render::scene::surface_maps(&self.doc, r).map(|ids| {
                    let mut ids = ids.to_vec();
                    ids.push(sp.emissive.as_ref().map(|(id, _)| id.clone()));
                    let key = format!("surface|{tid}|{ids:?}");
                    (ids, key)
                });
                let emissive_key = match (&surface, &sp.emissive) {
                    (None, Some((mask_id, _))) => Some(format!("emissive|{tid}|{mask_id}")),
                    _ => None,
                };
                if let Some((ids, key)) = &surface {
                    if textures.composite_index(key).is_none() {
                        textures.want_pixels(tid);
                        for id in ids.iter().flatten() {
                            textures.want_pixels(id);
                        }
                    }
                }
                if let (Some(key), Some((mask_id, _))) = (&emissive_key, &sp.emissive) {
                    if textures.composite_index(key).is_none() {
                        textures.want_pixels(tid);
                        textures.want_pixels(mask_id);
                    }
                }
                match textures.image(tid, dir, assets, &mut |img, slot| {
                    rustudio_render::render::reuse_texture_slot(slot);
                    renderer.add_texture(&device, &queue, img.width, img.height, &img.pixels)
                }) {
                    Some(t) => {
                        d.texture = Some(t.index);
                        d.tex_mode = sp.tex_kind;
                        if let Some((ids, key)) = surface {
                            let mut maps = Vec::new();
                            for id in &ids {
                                maps.push(id.as_ref().and_then(|id| textures.image(id, dir, assets, &mut |img, slot| {
                                    rustudio_render::render::reuse_texture_slot(slot);
                                    renderer.add_texture(&device, &queue, img.width, img.height, &img.pixels)
                                })));
                            }
                            waiting |= ids.iter().zip(&maps).any(|(id, im)| id.as_ref().is_some_and(|id| !textures.failed(id)) && im.is_none());
                            let (w, h) = (t.width.max(2), t.height.max(2));
                            if !waiting {
                                // The colour map with its packed normal /
                                // roughness / metalness map and emissive mask,
                                // made on an asset worker (`surface_composite`);
                                // the plain colour map is drawn until then.
                                let composed = match textures.composite_index(&key) {
                                    Some(i) => Composed::Ready(i),
                                    None => {
                                        let base = textures.pixel_source(tid, dir, assets);
                                        let px: Vec<Option<PixelSource>> = ids.iter().zip(&maps)
                                            .map(|(id, m)| id.as_ref().filter(|_| m.is_some()).and_then(|id| textures.pixel_source(id, dir, assets)))
                                            .collect();
                                        let inputs: Vec<&str> = std::iter::once(tid.as_str()).chain(ids.iter().flatten().map(|i| i.as_str())).collect();
                                        let c = textures.composite(&key, &inputs, move || surface_composite(base?, px, w, h), &mut |c, slot| {
                                            rustudio_render::render::reuse_texture_slot(slot);
                                            renderer.add_texture_surface(&device, &queue, c.image.width, c.image.height, &c.image.pixels,
                                                c.mask.as_ref().map(|m| (m.width, m.height, m.pixels.as_slice())),
                                                c.packed.as_ref().map(|(w, h, p)| (*w, *h, p.as_slice())))
                                        });
                                        textures.release_pixels(inputs);
                                        c
                                    }
                                };
                                waiting |= composed.pending();
                                if let Some(index) = composed.index() { d.texture = Some(index); }
                                if let Some((_, strength)) = &sp.emissive { d.emissive = *strength; }
                            }
                        }
                        // An emissive SurfaceAppearance: the same image with its mask.
                        if let Some((mask_id, strength)) = sp.emissive.as_ref().filter(|_| rustudio_render::scene::surface_maps(&self.doc, r).is_none()) {
                            match textures.image(mask_id, dir, assets, &mut |img, slot| {
                                rustudio_render::render::reuse_texture_slot(slot);
                                renderer.add_texture(
                                    &device,
                                    &queue,
                                    img.width,
                                    img.height,
                                    &img.pixels,
                                )
                            }) {
                                Some(_) => {
                                    let key = format!("emissive|{tid}|{mask_id}");
                                    let composed = match textures.composite_index(&key) {
                                        Some(i) => Composed::Ready(i),
                                        None => {
                                            let base = textures.pixel_source(tid, dir, assets);
                                            let mask = textures.pixel_source(mask_id, dir, assets);
                                            let c = textures.composite(
                                                &key,
                                                &[tid.as_str(), mask_id.as_str()],
                                                move || {
                                                    let mut c = Composite::new(std::sync::Arc::unwrap_or_clone(base?.get()?));
                                                    c.mask = Some(mask?.get()?);
                                                    Some(c)
                                                },
                                                &mut |c, slot| {
                                                    rustudio_render::render::reuse_texture_slot(slot);
                                                    renderer.add_texture_emissive(
                                                        &device,
                                                        &queue,
                                                        c.image.width,
                                                        c.image.height,
                                                        &c.image.pixels,
                                                        c.mask.as_ref().map(|m| (m.width, m.height, m.pixels.as_slice())),
                                                    )
                                                },
                                            );
                                            textures.release_pixels([tid.as_str(), mask_id.as_str()]);
                                            c
                                        }
                                    };
                                    waiting |= composed.pending();
                                    if let Some(i) = composed.index() {
                                        d.texture = Some(i);
                                        d.emissive = *strength;
                                    }
                                }
                                None => {
                                    if !textures.failed(mask_id) {
                                        tex_wait.push(mask_id.clone());
                                    }
                                }
                            }
                        }
                    }
                    None => {
                        if !textures.failed(tid) {
                            tex_wait.push(tid.clone());
                        }
                    }
                }
            }
            drop(sd_tex);
            let _sd_rest = rustudio_core::bench::scope("sd_rest");
            // A mesh body part keeps its mesh under clothing, its UVs
            // re-projected onto the template (assets::body_template_uvs):
            // its own UVs are for Roblox's composited clothing atlas.
            let has_mesh = sp.mesh_asset.is_some() && d.custom_mesh.is_some();
            if let Some((name, shirt, pants)) = &sp.clothing {
                if let Some(role) = clothing_role(name) {
                    let _sc = rustudio_core::bench::scope("sd_clothing");
                    let box_mesh = *clothing_meshes.entry(name.clone()).or_insert_with(|| {
                        let (p, n, u) = template_box(&role);
                        renderer.add_mesh(&device, &p, &n, &u)
                    });
                    let mut mesh = (!has_mesh).then_some(box_mesh);
                    // R15 body MeshParts: their own UVs into their cell of
                    // the composited atlas (assets::r15_composite), as Studio.
                    let r15 = sp.r15_look.filter(|_| has_mesh);
                    // (made on the asset workers: `waiting` until uploaded,
                    // the part drawn without its clothing meanwhile)
                    if let (Some(_), Some(asset)) = (r15, &sp.mesh_asset) {
                        let key = format!("r15-clothing|{name}|{}", asset.id);
                        let part = name.clone();
                        let made = meshes.derived(&key, &asset.id, dir, assets, move |bytes| {
                            let (p, n, uv) = rustudio_core::assets::mesh_for_upload(bytes)?;
                            let uv = rustudio_core::assets::r15_atlas_uvs(&part, &uv)?;
                            Some((p, n, uv))
                        }, &mut |p, n, uv| renderer.add_mesh(&device, p, n, uv));
                        mesh = made.map(|m| m.unwrap_or(box_mesh));
                    } else if let (true, Some(asset)) = (has_mesh, &sp.mesh_asset) {
                        let key = format!("body-clothing|{name}|{}", asset.id);
                        let part = name.clone();
                        let made = meshes.derived(&key, &asset.id, dir, assets, move |bytes| {
                            let (p, n, _) = rustudio_core::assets::mesh_for_upload(bytes)?;
                            let uv = rustudio_core::assets::body_template_uvs(&part, &p)?;
                            Some((p, n, uv))
                        }, &mut |p, n, uv| renderer.add_mesh(&device, p, n, uv));
                        // Unreadable mesh: the template box.
                        mesh = made.map(|m| m.unwrap_or(box_mesh));
                    }
                    let mut img = |id: &Option<String>| {
                        id.as_ref().and_then(|id| {
                            textures.image(id, dir, assets, &mut |im, slot| {
                                rustudio_render::render::reuse_texture_slot(slot);
                                renderer
                                    .add_texture(&device, &queue, im.width, im.height, &im.pixels)
                            })
                        })
                    };
                    let (s, p) = (img(shirt), img(pants));
                    let inputs: Vec<&str> = shirt.iter().chain(pants.iter()).map(|i| i.as_str()).collect();
                    let mut upload = |c: &rustudio_core::assets::Composite, slot: Option<u32>| {
                        rustudio_render::render::reuse_texture_slot(slot);
                        renderer.add_texture(&device, &queue, c.image.width, c.image.height, &c.image.pixels)
                    };
                    let r15_tex = r15.and_then(|look| {
                        // Both images settled (loaded, or absent) first.
                        if (shirt.is_some() && s.is_none()) || (pants.is_some() && p.is_none()) {
                            return None;
                        }
                        let key = format!("r15|{shirt:?}|{pants:?}|{}", look.key());
                        if let Some(i) = textures.composite_index(&key) {
                            return Some(Composed::Ready(i));
                        }
                        let spx = s.and(shirt.as_ref()).and_then(|id| textures.pixel_source(id, dir, assets));
                        let ppx = p.and(pants.as_ref()).and_then(|id| textures.pixel_source(id, dir, assets));
                        Some(textures.composite(
                            &key,
                            &inputs,
                            move || {
                                let (s, p) = (spx.and_then(|x| x.get()), ppx.and_then(|x| x.get()));
                                rustudio_core::assets::r15_composite(&look, s.as_deref(), p.as_deref()).map(Composite::new)
                            },
                            &mut upload,
                        ))
                    });
                    let tex = if r15.is_some() { r15_tex } else { match role.layer {
                        ClothingLayer::Shirt => s.as_ref().map(|t| Composed::Ready(t.index)),
                        ClothingLayer::Pants => p.as_ref().map(|t| Composed::Ready(t.index)),
                        ClothingLayer::Torso => {
                            let key = format!("torso|{shirt:?}|{pants:?}");
                            Some(match textures.composite_index(&key) {
                                Some(i) => Composed::Ready(i),
                                None => {
                                    let spx = s.and(shirt.as_ref()).and_then(|id| textures.pixel_source(id, dir, assets));
                                    let ppx = p.and(pants.as_ref()).and_then(|id| textures.pixel_source(id, dir, assets));
                                    textures.composite(
                                        &key,
                                        &inputs,
                                        move || {
                                            let (s, p) = (spx.and_then(|x| x.get()), ppx.and_then(|x| x.get()));
                                            composite_over(p.as_deref(), s.as_deref()).map(Composite::new)
                                        },
                                        &mut upload,
                                    )
                                }
                            })
                        }
                    } };
                    let tex = tex.and_then(|t| {
                        waiting |= t.pending();
                        t.index()
                    });
                    // (a mesh body part's clothing UVs come with its clothing mesh)
                    if tex.is_none() || (mesh.is_none() && has_mesh) {
                        waiting = true;
                    }
                    if let (Some(tex), true) = (tex, mesh.is_some() || !has_mesh) {
                        if let Some(mesh) = mesh {
                            d.custom_mesh = Some(mesh);
                        }
                        d.texture = Some(tex);
                        d.tex_mode = TexMode::Over;
                        d.surface_tint = None;
                        d.placeholder = false;
                    }
                }
            }
            // A MeshPart draws single-sided unless DoubleSided, as Studio:
            // the Moon Phases Waist Display's inverted-hull outline shells
            // show in Studio only as dark rims around their spheres.
            // File meshes (MeshPart, SpecialMesh FileMesh) draw single-sided,
            // as Studio, unless a MeshPart is DoubleSided: the Moon Phases
            // Waist Display's inverted-hull outline shells show in Studio only
            // as dark rims around their spheres. CSG keeps two-sided.
            if let (Some(i), Some(_)) = (d.custom_mesh, &sp.mesh_asset) {
                let class = self.doc.class_of(sp.r);
                let csg = class.as_ref().is_some_and(|c| matches!(c.as_str(), "UnionOperation" | "NegateOperation" | "IntersectOperation"));
                if !csg {
                    use rustudio_render::render::MeshFidelity;
                    let mesh_part = class.as_ref().is_some_and(|c| c == "MeshPart");
                    let double = mesh_part
                        && matches!(self.doc.prop(sp.r, "DoubleSided"), Some(rbx_dom_weak::types::Variant::Bool(true)));
                    let fidelity = if mesh_part {
                        match self.doc.prop(sp.r, "RenderFidelity") {
                            Some(rbx_dom_weak::types::Variant::Enum(value)) => MeshFidelity::from_enum(value.to_u32()),
                            _ => MeshFidelity::Automatic,
                        }
                    } else {
                        MeshFidelity::Precise
                    };
                    d.custom_mesh = Some(renderer.mesh_cull_slot(i, !double, fidelity));
                }
            }
            // A MaterialVariant draws through the part material shader, like
            // Studio (per-face UVs, slopes included; normal / roughness /
            // metalness maps). Its six ColorMap decals stay only as the
            // fallback when the pack can't be built.
            let variant_maps = match sp.variant_pack {
                Some((pack, _)) if d.texture.is_none() && !d.placeholder && d.material < 0.5 => {
                    match rustudio_render::matpack::pack_maps(pack, 0xffff_ffff) {
                        rustudio_render::matpack::Lookup::Ready(m) if renderer.material_layer(&device, &queue, &m) >= 0 => Some(Some(m)),
                        // (loading, or its released maps on their way back)
                        rustudio_render::matpack::Lookup::Ready(_) | rustudio_render::matpack::Lookup::Pending => {
                            waiting = true;
                            Some(None)
                        }
                        rustudio_render::matpack::Lookup::Unavailable(_) => None,
                    }
                }
                _ => None,
            };
            // LocalTransparencyModifier fades a part's Decals and Textures too
            // (Studio: the first-person camera hides the head's face this way),
            // while the part's own Transparency leaves them showing.
            let ltm_keep = 1.0
                - scene::f32_of(self.doc.prop(sp.r, "LocalTransparencyModifier"))
                    .unwrap_or(0.0)
                    .clamp(0.0, 1.0);
            let sd_decals = rustudio_core::bench::scope("sd_decals");
            for dec in &sp.decals {
                if dec.base_surface && variant_maps.is_some() {
                    continue;
                }
                let Some(t) = textures.image(&dec.id, dir, assets, &mut |img, slot| {
                    rustudio_render::render::reuse_texture_slot(slot);
                    renderer.add_texture(&device, &queue, img.width, img.height, &img.pixels)
                }) else {
                    if !textures.failed(&dec.id) {
                        tex_wait.push(dec.id.clone());
                    }
                    continue;
                };
                let (axis, sign) = match dec.face {
                    0 => (0, 1.0),
                    1 => (1, 1.0),
                    2 => (2, 1.0),
                    3 => (0, -1.0),
                    4 => (1, -1.0),
                    _ => (2, -1.0),
                };
                // Project onto the actual front-facing mesh triangles rather than
                // drawing a closed slab in front of the head. Built-in wedges
                // too: Studio clips a wedge's Front / Top Texture to its slope
                // and a Right / Left one to the triangle (Pirate Island's bow).
                let shape = (sp.mesh_asset.is_none() && d.custom_mesh.is_none())
                    .then(|| rustudio_render::render::shape_triangles(sp.draw.mesh).map(|t| (format!("{:?}", sp.draw.mesh), std::sync::Arc::new(t))))
                    .flatten();
                let is_shape = shape.is_some();
                // Textures too: Studio projects a tiled Texture on a
                // MeshPart onto the same triangles as a Decal.
                let tris = shape.or_else(|| {
                    sp.mesh_asset.as_ref().and_then(|a| rustudio_core::assets::mesh_triangles(&a.id).map(|t| (a.id.clone(), t)))
                });
                let ext = sp
                    .mesh_asset
                    .as_ref()
                    .and_then(|a| rustudio_core::assets::mesh_extent(&a.id))
                    .unwrap_or(Vec3::ONE);
                // A dynamic head (a MeshPart with FaceControls): Studio 0.747
                // draws its Front Decal through the mesh's own UVs over the
                // whole head, like a TextureID (tools/facsprobe.py: the grid
                // as a Front Decal and as TextureID give the same picture),
                // on the skinned (FACS-posed) mesh.
                let face_head = dec.face == 5
                    && dec.tiling.is_none()
                    && skin_base.is_some()
                    && self.doc.dom.get_by_ref(sp.r).is_some_and(|i| {
                        i.children().iter().any(|c| self.doc.class_of(*c).is_some_and(|k| k == "FaceControls"))
                    });
                if face_head {
                    extra.push(PartDraw {
                        texture: Some(t.index),
                        tex_mode: TexMode::Decal, color: dec.color,
                        alpha: ((1.0 - dec.transparency) * ltm_keep).clamp(0.0, 1.0),
                        material: 0.0, material_id: 256, mat_layer: -1,
                        surfaces: 0, cast_shadow: false, surface_tint: None, uvt: PartDraw::NO_UVT,
                        emissive: [0.0; 3], reflectance: 0.0,
                        ..d
                    });
                    continue;
                }
                // A skinned MeshPart: the projected copy is skinned with it.
                let skinned = (!is_shape)
                    .then(|| skin_base.as_ref())
                    .flatten()
                    .and_then(|(id, b)| rustudio_render::skin::decal_base(b, id, dec.face).map(|db| (b.clone(), db)));
                if let Some((mesh_base, db)) = skinned {
                    let corners = db.positions.len();
                    let tag = rustudio_render::skin::DECAL_TAG + dec.face;
                    let pose = skin_pose.get_or_insert_with(|| rustudio_render::skin::pose(&self.doc, sp.r, &mesh_base));
                    let index = match pose {
                        Some((palette, scale, origin)) => {
                            let have = rustudio_render::skin::slot_of_tag(sp.r, tag, corners);
                            let idx = if rustudio_render::skin::cpu_skinning() {
                                let (p, n) = rustudio_render::skin::deform(&db, palette, *scale, *origin);
                                renderer.set_mesh(&device, &queue, have, &p, &n, &db.uvs, &[])
                            } else {
                                let dkey = format!("{}|decal{}", skin_base.as_ref().map(|s| s.0.as_str()).unwrap_or(""), dec.face);
                                let req = rustudio_render::skin_gpu::Request { key: &dkey, base: &db, palette, scale: *scale, origin: *origin };
                                renderer.skin_mesh(&device, &queue, have, &req)
                            };
                            rustudio_render::skin::set_slot_tag(sp.r, tag, idx, corners);
                            idx
                        }
                        None => *clothing_meshes
                            .entry(format!("decal-skinned|{}|{}", skin_base.as_ref().map(|s| s.0.as_str()).unwrap_or(""), dec.face))
                            .or_insert_with(|| renderer.add_mesh(&device, &db.positions, &db.normals.iter().map(|n| *n / db.extent).collect::<Vec<_>>(), &db.uvs)),
                    };
                    let s = sp.decal_size;
                    let (w, h) = match axis {
                        0 => (s.z, s.y),
                        1 => (s.x, s.z),
                        _ => (s.x, s.y),
                    };
                    let uvt = mesh_texture_uvt(dec.tiling, w, h);
                    extra.push(PartDraw {
                        custom_mesh: Some(index), texture: Some(t.index),
                        tex_mode: TexMode::Decal, color: dec.color,
                        alpha: ((1.0 - dec.transparency) * if dec.base_surface { d.alpha } else { ltm_keep }).clamp(0.0, 1.0),
                        material: 0.0, material_id: 256, mat_layer: -1,
                        surfaces: 0, cast_shadow: false, surface_tint: None, uvt,
                        emissive: [0.0; 3], reflectance: 0.0,
                        ..d
                    });
                    continue;
                }
                if let Some((source, tris)) = tris {
                    let key = format!("decal-projection2|{}|{}", source, dec.face);
                    let index = *clothing_meshes.entry(key).or_insert_with(|| {
                        let mut positions = Vec::new();
                        let mut normals = Vec::new();
                        let mut uvs = Vec::new();
                        for tri in tris.iter() {
                            let normal = (tri[1] - tri[0]).cross(tri[2] - tri[0]).normalize_or_zero();
                            if is_shape {
                                if normal[axis] * sign <= 0.01 { continue; }
                            } else {
                                // A mesh: the face its normal (mesh space)
                                // points most along (decal_face_of).
                                let geo = (tri[1] - tri[0]).cross(tri[2] - tri[0])
                                    * Vec3::new(ext.y * ext.z, ext.x * ext.z, ext.x * ext.y);
                                if rustudio_render::scene::decal_face_of(geo) != Some(dec.face) { continue; }
                            }
                            for v in tri {
                                positions.push(*v + normal * 0.0005);
                                normals.push(normal);
                                // A wedge's slope takes its Front or Top image in
                                // its own frame, the same for both (measured against
                                // Studio with a Texture on each face of a WedgePart).
                                let slope = is_shape && normal.y > 0.01 && normal.z < -0.01;
                                let uv = if slope {
                                    glam::Vec2::new(0.5 - v.x, 0.5 - v.z)
                                } else {
                                    rustudio_render::scene::decal_face_uv(dec.face, *v)
                                };
                                uvs.push(uv);
                            }
                        }
                        renderer.add_mesh(&device, &positions, &normals, &uvs)
                    });
                    let s = sp.decal_size;
                    let (w, h) = match axis {
                        0 => (s.z, s.y),
                        1 => (s.x, s.z),
                        _ => (s.x, s.y),
                    };
                    let uvt = if is_shape {
                        match dec.tiling {
                            // Offsets wrapped into one tile: a negative uvt.w means
                            // mesh-projected UVs to the shader (part_out).
                            Some([su, sv, ou, ov]) => [w / su.max(0.01), h / sv.max(0.01), (ou / su.max(0.01)).rem_euclid(1.0), (ov / sv.max(0.01)).rem_euclid(1.0)],
                            None => PartDraw::NO_UVT,
                        }
                    } else {
                        mesh_texture_uvt(dec.tiling, w, h)
                    };
                    extra.push(PartDraw {
                        custom_mesh: Some(index), texture: Some(t.index),
                        tex_mode: TexMode::Decal, color: dec.color,
                        alpha: ((1.0 - dec.transparency) * if dec.base_surface { d.alpha } else { ltm_keep }).clamp(0.0, 1.0),
                        material: 0.0, material_id: 256, mat_layer: -1,
                        surfaces: 0, cast_shadow: false, surface_tint: None, uvt,
                        emissive: [0.0; 3], reflectance: 0.0,
                        ..d
                    });
                    continue;
                }
                let mut s = sp.decal_size;
                let mut n = Vec3::ZERO;
                n[axis] = sign;
                // The plane floats DECAL_LIFT off its face, so at an outside
                // edge two Textures' planes leave a sliver where the part's
                // own colour shows (Pirate Island's hull: white seams on parts
                // Color 248,248,248). Studio draws Textures on the face itself:
                // widen a Texture's plane by the lift so neighbours meet at
                // the corner, keeping its tiling where it was.
                const DECAL_LIFT: f32 = 0.012;
                let grow = if dec.tiling.is_some() { DECAL_LIFT } else { 0.0 };
                for k in 0..3 {
                    if k != axis {
                        s[k] += 2.0 * grow;
                    }
                }
                let (w, h) = match axis {
                    0 => (s.z, s.y),
                    1 => (s.x, s.z),
                    _ => (s.x, s.y),
                };
                let uvt = match dec.tiling {
                    Some([su, sv, ou, ov]) => [
                        w / su.max(0.01),
                        h / sv.max(0.01),
                        // Wrapped into one tile (the image repeats): a negative
                        // uvt.w tells the shader to project mesh UVs instead
                        // (part_out), which squeezed the bow rail's planks.
                        ((ou - grow) / su.max(0.01)).rem_euclid(1.0),
                        ((ov - grow) / sv.max(0.01)).rem_euclid(1.0),
                    ],
                    None => PartDraw::NO_UVT,
                };
                // A decal has one surface. A thin closed Block draws the
                // same image on both sides, visibly doubling it at oblique
                // angles because the decal pass does not write depth.
                let key = format!("decal-plane2|{}", dec.face);
                let index = *clothing_meshes.entry(key).or_insert_with(|| {
                    // (as decal_face_uv: the directions u and v grow in)
                    let (u, v) = match dec.face {
                        0 => (-Vec3::Z, -Vec3::Y),
                        1 => (-Vec3::X, -Vec3::Z),
                        2 => (Vec3::X, -Vec3::Y),
                        3 => (Vec3::Z, -Vec3::Y),
                        4 => (Vec3::X, -Vec3::Z),
                        _ => (-Vec3::X, -Vec3::Y),
                    };
                    let points = [(-u-v)*0.5, (u-v)*0.5, (u+v)*0.5, (-u+v)*0.5];
                    let ids = [0, 1, 2, 0, 2, 3];
                    let uv = [glam::Vec2::ZERO, glam::Vec2::X, glam::Vec2::ONE, glam::Vec2::Y];
                    renderer.add_mesh(&device, &ids.map(|i| points[i]), &[n; 6], &ids.map(|i| uv[i]))
                });
                let xf = sp.xf;
                extra.push(PartDraw {
                    xf: Xform {
                        pos: xf.pos + xf.rot * (sp.decal_offset + n * (s[axis] * 0.5 + DECAL_LIFT)),
                        rot: xf.rot,
                    },
                    size: s,
                    color: dec.color,
                    alpha: ((1.0 - dec.transparency) * if dec.base_surface { sp.draw.alpha } else { ltm_keep }).clamp(0.0, 1.0),
                    mesh: rustudio_render::render::MeshKind::Block,
                    material: 0.0,
                    placeholder: false,
                    hover: false,
                    reflectance: 0.0,
                    custom_mesh: Some(index),
                    texture: Some(t.index),
                    tex_mode: TexMode::Decal,
                    uvt,
                    emissive: [0.0; 3],
                    tex_albedo: 1.0,
                    surfaces: 0,
                    material_id: 256,
                    mat_layer: -1,
                    surface_tint: None,
                    cast_shadow: false,
                });
            }
            drop(sd_decals);
            // Layered clothing fitted onto this body part (wrap.rs).
            let is_target = self.doc.dom.get_by_ref(r).is_some_and(|i| {
                i.children()
                    .iter()
                    .any(|c| self.doc.class_of(*c).is_some_and(|k| k == "WrapTarget"))
            });
            if is_target {
                let _sw = rustudio_core::bench::scope("sd_wrap");
                let model = self
                    .doc
                    .dom
                    .get_by_ref(r)
                    .map(|i| i.parent())
                    .unwrap_or_else(Ref::none);
                match rustudio_render::wrap::fitted(&self.doc, model) {
                    rustudio_render::wrap::Lookup::Ready(fit) => {
                        // Each garment skinned to the body parts (wrap.rs), drawn with its main part.
                        for gm in fit.garments.iter().filter(|g| g.owner == r) {
                            let Some(palette) = rustudio_render::wrap::garment_palette(&self.doc, gm) else { continue };
                            let corners = gm.base.positions.len();
                            let slot_key = gm.handle;
                            let have = rustudio_render::skin::slot_of(slot_key, corners);
                            let mesh = if rustudio_render::skin::cpu_skinning() {
                                let (p, n) = rustudio_render::skin::deform(&gm.base, &palette, Vec3::ONE, Vec3::ZERO);
                                renderer.set_mesh(&device, &queue, have, &p, &n, &gm.base.uvs, &[])
                            } else {
                                let req = rustudio_render::skin_gpu::Request { key: &gm.key, base: &gm.base, palette: &palette, scale: Vec3::ONE, origin: Vec3::ZERO };
                                renderer.skin_mesh(&device, &queue, have, &req)
                            };
                            rustudio_render::skin::set_slot(slot_key, mesh, corners);
                            let (centre, extent) = (gm.base.center, gm.base.extent);
                            let Some(hsp) = scene.get(gm.handle) else {
                                continue;
                            };
                            let mut g = hsp.draw;
                            g.xf = Xform {
                                pos: sp.xf.pos + sp.xf.rot * centre,
                                rot: sp.xf.rot,
                            };
                            g.size = extent;
                            g.custom_mesh = Some(mesh);
                            g.placeholder = false;
                            g.surfaces = 0;
                            g.mat_layer = -1;
                            if let Some(tid) = &hsp.texture_id {
                                match textures.image(tid, dir, assets, &mut |img, slot| {
                                    rustudio_render::render::reuse_texture_slot(slot);
                                    renderer.add_texture(
                                        &device,
                                        &queue,
                                        img.width,
                                        img.height,
                                        &img.pixels,
                                    )
                                }) {
                                    Some(t) => {
                                        g.texture = Some(t.index);
                                        g.tex_mode = hsp.tex_kind;
                                    }
                                    None => waiting |= !textures.failed(tid),
                                }
                            }
                            extra.push(g);
                        }
                    }
                    rustudio_render::wrap::Lookup::Pending => waiting = true,
                    rustudio_render::wrap::Lookup::Failed(e) => {
                        if self.wrap_warned.insert(e.clone()) {
                            textures
                                .messages
                                .push(format!("Layered clothing couldn't be fitted: {e}"));
                        }
                    }
                    rustudio_render::wrap::Lookup::None => {}
                }
            }
            if let (Some(Some(m)), Some((_, studs))) = (&variant_maps, sp.variant_pack) {
                d.mat_layer = renderer.material_layer(&device, &queue, m);
                // The variant's ColorMap × Color (the table albedo is not applied).
                let fb = rustudio_render::matpack::fallback_albedo(d.material_id, use2022);
                for c in 0..3 {
                    if fb[c] > 0.0 {
                        d.color[c] /= fb[c];
                        color_mul[c] /= fb[c];
                    }
                }
                d.tex_albedo = 1.0;
                // Studio: a variant's uv scale is 1 / StudsPerTile on
                // every shape; render.rs leaves its uvOffset at zero.
                let k = 1.0 / studs.max(0.01);
                let union_projection = self.doc.class_of(sp.r).is_some_and(|c|
                    matches!(c.as_str(), "UnionOperation" | "NegateOperation" | "PartOperation"));
                let meshpart_projection = d.custom_mesh.is_some() && self.doc.class_of(sp.r).is_some_and(|c| c == "MeshPart");
                let union_authored = union_projection && mesh_uvs && match self.doc.prop(sp.r, "InitialSize") {
                    Some(rbx_dom_weak::types::Variant::Vector3(v)) =>
                        d.size == glam::Vec3::new(v.x, v.y, v.z),
                    _ => false,
                };
                d.uvt = [k, k, 0.0, if union_authored { -3.0 } else if meshpart_projection { -2.0 } else if union_projection { -1.0 } else { 0.0 }];
                // Plain Tiled shader, textureTiling 1, no detail pack or detiling.
                renderer.set_material_params(&queue, d.mat_layer, [[-1.0, 0.0, 0.0, 1.0], [0.0, 0.0, 0.0, 0.0]]);
            }
            // Material texture packs (matpack.rs) on untextured parts.
            if d.texture.is_none() && !d.placeholder && !sp.variant && d.material < 0.5 {
                if let Some(e) = rustudio_render::matpack::entry(d.material_id, use2022) {
                    match rustudio_render::matpack::maps(e) {
                        // (a pack whose maps were released comes back like a loading one)
                        rustudio_render::matpack::Lookup::Ready(m) if renderer.material_layer(&device, &queue, &m) < 0 => waiting = true,
                        rustudio_render::matpack::Lookup::Ready(m) => {
                            d.mat_layer = renderer.material_layer(&device, &queue, &m);
                            // The maps carry the material's colour now.
                            let fb = rustudio_render::matpack::fallback_albedo(d.material_id, use2022);
                            for c in 0..3 {
                                if fb[c] > 0.0 {
                                    d.color[c] /= fb[c];
                                    color_mul[c] /= fb[c];
                                }
                            }
                            d.tex_albedo = 1.0;
                            // Box faces: one repeat per the material's studs
                            // per tile (Studio's uvScale). Untextured MeshParts use studs too.
                            let c = rustudio_render::matpack::consts(d.material_id, use2022);
                            let union_projection = self.doc.class_of(sp.r).is_some_and(|c|
                                matches!(c.as_str(), "UnionOperation" | "NegateOperation" | "PartOperation"));
                            let meshpart_projection = d.custom_mesh.is_some() && self.doc.class_of(sp.r).is_some_and(|c| c == "MeshPart");
                            // Studio only regenerates CSG UVs when Size
                            // differs from InitialSize; unchanged unions retain authored UVs.
                            // A union with no UVs (every SolidMesh: its UV array is
                            // empty) has none to keep and takes the plane projection
                            // at any Size, as Studio draws it: a test place's Graveyard Slate
                            // rock and Ground cliff unions show their material maps in
                            // Studio 0.747 (agent Vinegar, 2026-11-15) and drew one
                            // flat texel here.
                            let union_authored = union_projection && mesh_uvs && match self.doc.prop(sp.r, "InitialSize") {
                                Some(rbx_dom_weak::types::Variant::Vector3(v)) =>
                                    d.size == glam::Vec3::new(v.x, v.y, v.z),
                                _ => false,
                            };
                            let stud_projection = union_projection || meshpart_projection;
                            let k = if d.custom_mesh.is_some() && !stud_projection {
                                m.tiling
                            } else {
                                m.tiling / c[5]
                            };
                            // Negative w selects shared mesh stud-space projection in the vertex shader.
                            d.uvt = [k, k, 0.0, if union_authored { -3.0 } else if meshpart_projection { -2.0 } else if union_projection { -1.0 } else { 0.0 }];
                            if meshpart_projection && std::env::var_os("RUSTUDIO_MESH_DENSITY_TRACE").is_some() {
                                static SEEN: std::sync::OnceLock<std::sync::Mutex<std::collections::HashSet<String>>> = std::sync::OnceLock::new();
                                if SEEN.get_or_init(Default::default).lock().unwrap().insert(format!("{:?}", sp.r)) {
                                    eprintln!("MESHPART_DENSITY {:?} drawn_extent={:?} MeshSize={:?} InitialSize={:?} uv=stud-projection repeats_per_stud={} map_width={} texels_per_stud={}", sp.r, d.size, self.doc.prop(sp.r,"MeshSize"), self.doc.prop(sp.r,"InitialSize"), k, rustudio_render::matpack::LAYER_SIZE, k * rustudio_render::matpack::LAYER_SIZE as f32);
                                }
                            }
                            if union_projection && std::env::var_os("RUSTUDIO_UNION_DENSITY_TRACE").is_some() {
                                static SEEN: std::sync::OnceLock<std::sync::Mutex<std::collections::HashSet<String>>> = std::sync::OnceLock::new();
                                let key = format!("{:?}:{:?}", sp.r, d.size);
                                if SEEN.get_or_init(Default::default).lock().unwrap().insert(key) {
                                    eprintln!("UNION_UV {:?} Size={:?} actual_uvt={:?} pack_tiling={} studs_per_tile={}", sp.r, d.size, d.uvt, m.tiling, c[5]);
                                }
                            }
                            // The detail normal pack, drawn once it's loaded.
                            let mut detail = -1.0;
                            if e.detail != 0 && c[1] * c[2] != 0.0 {
                                match rustudio_render::matpack::pack_maps(e.detail, 0xffff_ffff) {
                                    rustudio_render::matpack::Lookup::Ready(dm) => {
                                        detail = renderer.material_layer(&device, &queue, &dm) as f32;
                                        waiting |= detail < 0.0;
                                    }
                                    rustudio_render::matpack::Lookup::Pending => waiting = true,
                                    rustudio_render::matpack::Lookup::Unavailable(_) => {}
                                }
                            }
                            renderer.set_material_params(
                                &queue,
                                d.mat_layer,
                                [[detail, c[1], c[2], c[0]], [c[3], c[4], 0.0, 0.0]],
                            );
                        }
                        rustudio_render::matpack::Lookup::Pending => waiting = true,
                        rustudio_render::matpack::Lookup::Unavailable(_) => {}
                    }
                }
            }
            let flags = [
                (sp.mesh_asset.is_some() && d.custom_mesh.is_some()) as u32,
                matches!(
                    d.tex_mode,
                    TexMode::Over | TexMode::Opaque | TexMode::OpaqueTinted | TexMode::Cutout | TexMode::TintMask
                ) as u32,
                extra.len() as u32,
                (sp.clothing.is_some() && d.texture.is_some()) as u32,
            ];
            waiting |= hsr_wait;
            draw_cache.set(r, sp.xf, flags, waiting || !tex_wait.is_empty());
            let tex_only = !waiting && !tex_wait.is_empty();
            if tex_only {
                draw_cache.wait_tex.insert(r, tex_wait);
            } else {
                draw_cache.wait_tex.remove(&r);
            }
            if extra.is_empty() && !waiting && !tex_only && sp.clothing.is_none() {
                draw_cache.style.insert(r, (color_mul, alpha_forced));
            } else {
                draw_cache.style.remove(&r);
            }
            let _si = rustudio_core::bench::scope("sd_set_item");
            let mut all = Vec::with_capacity(1 + extra.len());
            all.push(d);
            all.extend(extra);
            renderer.set_item(r, all);
        }
        self.draw_stats = self.draw_cache.totals();
        let msgs: Vec<String> = std::mem::take(&mut self.meshes.messages)
            .into_iter()
            .chain(std::mem::take(&mut self.textures.messages))
            .collect();
        for msg in msgs {
            self.warn(msg);
        }
    }

    /// Meshes Workspace.Terrain's voxels (SmoothGrid): the solid surface by
    /// 32³ chunk with Studio's terrain shading (only the chunks an edit
    /// changed, and their neighbours, are re-meshed: round 21), the water as
    /// one see-through mesh.
    fn sync_terrain(&mut self) {
        let terrain = self
            .doc
            .workspace()
            .and_then(|ws| self.doc.find_child(ws, "Terrain"));
        // A Terrain Editor stroke hands its voxels and touched chunks over
        // (no decode, no full compare).
        let brushed = self.terrain_brushed.take();
        let (voxels, brushed) = match brushed {
            Some((v, dirty, water)) => (Some(v), Some((dirty, water))),
            None => match rustudio_core::terrain::Voxels::from_dom(&self.doc.dom) {
                Some(Ok(v)) => (Some(v), None),
                Some(Err(e)) => {
                    self.warn(format!("Terrain couldn't be read: {e}"));
                    return;
                }
                // A Terrain that was never drawn on has no SmoothGrid yet
                // (every new place): it is empty terrain, which the Terrain
                // Editor's brushes draw into. Without it they did nothing.
                None if terrain.is_some() => (Some(rustudio_core::terrain::Voxels::empty()), None),
                None => (None, None),
            },
        };
        let (Some(renderer), Some(rs)) = (self.renderer.as_mut(), self.render_state.as_ref())
        else {
            return;
        };
        let device = rs.device.clone();
        let (Some(terrain), Some(voxels)) = (terrain, voxels) else {
            renderer.clear_terrain();
            if let Some((_, old, _)) = self.terrain_drawn.take() {
                renderer.remove_item(old);
            }
            self.terrain_tris = 0;
            return;
        };
        let _p = rustudio_core::bench::scope("terrain_remesh");
        // Another place (or Terrain): start over.
        let same = self
            .terrain_drawn
            .as_ref()
            .is_some_and(|(id, r, _)| *id == self.doc.id && *r == terrain);
        if !same {
            renderer.clear_terrain();
            if let Some((_, old, _)) = self.terrain_drawn.take() {
                renderer.remove_item(old);
            }
            self.terrain_grass_chunks.clear();
            self.terrain_grass_built.clear();
        }
        let old = self.terrain_drawn.take().map(|(_, _, v)| v);
        let use2022 = scene::use_2022_materials(&self.doc);
        rustudio_core::terrain::set_use2022(use2022);
        let cube = |m: u8| rustudio_core::terrain_mat::material(m, use2022).is_some_and(|t| t.cube);
        let dirty = match &brushed {
            Some((d, _)) => d.clone(),
            None => rustudio_core::terrain::dirty_chunks(old.as_ref(), &voxels),
        };
        // Meshed a batch at a time on several threads, applied in order.
        let meshed = dirty
            .chunks(256)
            .flat_map(|batch| batch.iter().zip(rustudio_core::terrain::chunk_triangles_many(&voxels, batch, &cube)));
        for (key, verts) in meshed {
            if rustudio_core::terrain::chunk_has_grass_top(&verts) {
                self.terrain_grass_chunks.insert(*key);
            } else {
                self.terrain_grass_chunks.remove(key);
            }
            // Its blades are made again (sync_terrain_grass).
            if self.terrain_grass_built.remove(key) {
                renderer.set_terrain_grass(&device, *key, &[]);
            }
            renderer.set_terrain_chunk(&device, *key, &verts);
        }
        // Water: one mesh, rebuilt when the water or a chunk beside it changed.
        let water_dirty = match &brushed {
            Some((_, water)) => *water || dirty.iter().any(|k| voxels.has_water_near(*k)),
            None => rustudio_core::terrain::water_changed(old.as_ref(), &voxels),
        } || dirty.iter().any(|k| voxels.has_water_near(*k));
        // The Terrain's water look (round 21): WaterColor, WaterTransparency,
        // WaterReflectance (fs_part kind 5).
        let num = |name: &str, d: f32| match self.doc.prop(terrain, name) {
            Some(Variant::Float32(f)) => f,
            Some(Variant::Float64(f)) => f as f32,
            _ => d,
        };
        let water_rgb = match self.doc.prop(terrain, "WaterColor") {
            Some(Variant::Color3(c)) => [c.r, c.g, c.b].map(|x| (x.clamp(0.0, 1.0) * 255.0).round() as u8),
            Some(Variant::Color3uint8(c)) => [c.r, c.g, c.b],
            _ => mech::WATER_COLOR_DEFAULT,
        };
        let transparency = num("WaterTransparency", mech::WATER_TRANSPARENCY_DEFAULT).clamp(0.0, 1.0);
        let reflectance = num("WaterReflectance", mech::WATER_REFLECTANCE_DEFAULT).clamp(0.0, 1.0);
        self.water_waves.wave_size = num("WaterWaveSize", mech::WATER_WAVE_SIZE_DEFAULT).max(0.0);
        self.water_waves.wave_speed = num("WaterWaveSpeed", mech::WATER_WAVE_SPEED_DEFAULT).max(0.0);
        self.water_waves.transparency = transparency;
        let water_dirty = water_dirty
            || self.terrain_water_look != Some((water_rgb, transparency, reflectance));
        self.terrain_water_look = Some((water_rgb, transparency, reflectance));
        if water_dirty {
            renderer.remove_item(terrain);
            let mut draws = Vec::new();
            for m in rustudio_core::terrain::build_water_meshes(&voxels) {
                if m.indices.is_empty() {
                    continue;
                }
                // Studio draws the water where its rays meet it, half a voxel
                // under the surface-nets mesh (WATER_RAY_DROP): plates at
                // 14.25 / 14.75 by Pirate Island's dock (water voxel 62 %
                // full, mesh at 16.48) sit under / over Studio's water, and
                // its dock (15.5–16.5) stays dry.
                let drop = Vec3::Y * rustudio_core::terrain::WATER_RAY_DROP;
                let m_pos: Vec<Vec3> = m.positions.iter().map(|p| Vec3::from(*p) - drop).collect();
                // Fit the mesh into the unit box the renderer instances.
                let (mut lo, mut hi) = (Vec3::splat(f32::MAX), Vec3::splat(f32::MIN));
                for p in &m_pos {
                    lo = lo.min(*p);
                    hi = hi.max(*p);
                }
                let center = (lo + hi) * 0.5;
                let extent = (hi - lo).max(Vec3::splat(0.01));
                let mut pos = Vec::with_capacity(m.indices.len());
                let mut nrm = Vec::with_capacity(m.indices.len());
                for i in &m.indices {
                    pos.push((m_pos[*i as usize] - center) / extent);
                    nrm.push(Vec3::from(m.normals[*i as usize]) * extent);
                }
                // Per triangle: its three corners' random bytes and which
                // corner each vertex is, packed into the UVs (fs_water:
                // x = 4·(r0 + 256·r1) + [corner 0], y = 4·r2 + [corner 1];
                // the interpolated fractions are the barycentric weights).
                let uvs: Vec<glam::Vec2> = m
                    .indices
                    .chunks_exact(3)
                    .flat_map(|t| {
                        let r = [t[0], t[1], t[2]].map(|i| m.rand.get(i as usize).copied().unwrap_or(0) as f32);
                        let base = glam::Vec2::new(4.0 * (r[0] + 256.0 * r[1]), 4.0 * r[2]);
                        [base + glam::Vec2::X, base + glam::Vec2::Y, base]
                    })
                    .collect();
                // In pieces of consecutive triangles (the mesh's order, so
                // overlapping water blends as before), each its own mesh in
                // the whole water's box (the same vertex arithmetic), so the
                // renderer culls the water out of view: Place B's 333k
                // triangle sea was drawn whole every frame (a low-end laptop: ~5 ms).
                let piece = 3 * WATER_PIECE_TRIS;
                for at in (0..pos.len()).step_by(piece) {
                let end = (at + piece).min(pos.len());
                let mesh = renderer.add_mesh(&device, &pos[at..end], &nrm[at..end], &uvs[at..end]);
                let rgb = water_rgb;
                draws.push(rustudio_render::render::PartDraw {
                    xf: Xform {
                        pos: center,
                        rot: Mat3::IDENTITY,
                    },
                    size: extent,
                    color: [
                        scene::srgb_to_linear(rgb[0]),
                        scene::srgb_to_linear(rgb[1]),
                        scene::srgb_to_linear(rgb[2]),
                    ],
                    // Blended (under 0.999, the opaque pass); fs_part kind 5
                    // sets the real alpha. Water casts no shadow, as in Studio.
                    alpha: 0.99,
                    mesh: rustudio_render::render::MeshKind::Block,
                    material: 5.0,
                    placeholder: false,
                    hover: false,
                    reflectance: 0.0,
                    custom_mesh: Some(mesh),
                    texture: None,
                    tex_mode: rustudio_render::render::TexMode::None,
                    uvt: rustudio_render::render::PartDraw::NO_UVT,
                    emissive: [reflectance, transparency, 0.0],
                    tex_albedo: 1.0,
                    surfaces: 0,
                    material_id: 256,
                    mat_layer: -1,
                    surface_tint: None,
                    cast_shadow: false,
                });
                }
            }
            renderer.set_item(terrain, draws);
        }
        self.terrain_tris = renderer.terrain_triangles();
        self.terrain_present = rustudio_core::terrain::materials_present(&voxels);
        self.terrain_params_dirty = true;
        if !same && voxels.count() > 0 {
            self.info(format!(
                "Terrain: {} voxels, {} triangles",
                voxels.count(),
                self.terrain_tris
            ));
        }
        if !same || !dirty.is_empty() {
            self.lighting.lightgrid_dirty = true;
            self.lighting.lightgrid_terrain = None;
        }
        self.terrain_drawn = Some((self.doc.id, terrain, voxels));
    }

    /// Terrain.Decoration grass on the Grass-topped chunks near the camera:
    /// one chunk's blades made per frame, far ones dropped (round 21).
    fn sync_terrain_grass(&mut self) {
        let Some((_, terrain, voxels)) = self.terrain_drawn.as_ref() else {
            return;
        };
        let terrain = *terrain;
        let detail = self.quality_profile().map_or_else(
            || rustudio_core::rustudio_settings::performance_choice("grass"),
            |profile| profile.grass(),
        );
        let radius_scale = if detail == 1 { 0.5 } else { 1.0 };
        let on = detail != 2 && matches!(self.doc.prop(terrain, "Decoration"), Some(Variant::Bool(true)));
        let length = match self.doc.prop(terrain, "GrassLength") {
            Some(Variant::Float32(f)) => f,
            _ => mech::GRASS_LENGTH_DEFAULT,
        };
        // Explicit generation-job diagnostics, matching the native injected
        // job boundary. These do not imply a public Terrain property route.
        let length = std::env::var("RUSTUDIO_GRASS_JOB_LENGTH").ok()
            .and_then(|s| s.parse::<f32>().ok()).filter(|v| v.is_finite()).unwrap_or(length);
        let density = std::env::var("RUSTUDIO_GRASS_JOB_DENSITY").ok()
            .and_then(|s| s.parse::<f32>().ok()).filter(|v| v.is_finite()).unwrap_or(1.0).clamp(0.0, 1.0);
        let (Some(renderer), Some(rs)) = (self.renderer.as_mut(), self.render_state.as_ref())
        else {
            return;
        };
        let device = rs.device.clone();
        let density = density * radius_scale;
        if !on || density != self.terrain_grass_density || length.to_bits() != self.terrain_grass_length.to_bits() {
            for k in self.terrain_grass_built.drain() {
                renderer.set_terrain_grass(&device, k, &[]);
            }
            self.terrain_grass_density = density;
            self.terrain_grass_length = length;
            if !on {
                return;
            }
        }
        let chunk = rustudio_core::terrain::VOXEL * 32.0;
        let near = |k: &[i32; 3], r: f32| {
            let lo = Vec3::new(k[0] as f32, k[1] as f32, k[2] as f32) * chunk;
            let d = (self.cam.pos - self.cam.pos.clamp(lo, lo + Vec3::splat(chunk))).length();
            d <= r
        };
        let far: Vec<[i32; 3]> = self
            .terrain_grass_built
            .iter()
            .filter(|k| !near(k, mech::GRASS_EVICTION_RADIUS * radius_scale))
            .copied()
            .collect();
        for k in far {
            self.terrain_grass_built.remove(&k);
            renderer.set_terrain_grass(&device, k, &[]);
        }
        let mut pending: Vec<_> = self
            .terrain_grass_chunks
            .iter()
            .filter(|k| !self.terrain_grass_built.contains(*k) && near(k, mech::GRASS_RADIUS * radius_scale))
            .copied()
            .collect();
        // Complete the visible working set before drawing this camera. Building
        // one chunk per frame leaves holes after a teleport or a length edit.
        pending.sort_unstable();
        for k in pending {
            let blades = rustudio_core::terrain::grass_geometry_density(voxels, k, length, density);
            renderer.set_terrain_grass_geometry(&device, k, blades);
            self.terrain_grass_built.insert(k);
        }
    }

    /// The terrain shader's per-material parameters (textures, tiling,
    /// blending, MaterialColors recolour): rebuilt after a terrain change and
    /// while texture packs are still arriving.
    fn sync_terrain_params(&mut self) {
        let generation = rustudio_core::terrain_mat::palette_generation(&self.doc, rustudio_render::matpack::generation());
        if !self.terrain_params_dirty && generation == self.terrain_params_gen {
            return;
        }
        let Some((_, terrain, _)) = self.terrain_drawn.as_ref() else {
            return;
        };
        let terrain = *terrain;
        let colors = self
            .doc
            .prop(terrain, "MaterialColors")
            .as_ref()
            .and_then(rustudio_core::terrain_mat::material_colors_bytes);
        let use2022 = scene::use_2022_materials(&self.doc);
        let (Some(renderer), Some(rs)) = (self.renderer.as_mut(), self.render_state.as_ref())
        else {
            return;
        };
        let (device, queue) = (rs.device.clone(), rs.queue.clone());
        let mut params = vec![[0.0f32; 4]; rustudio_render::render::TERRAIN_PALETTE * 4];
        let mut waiting = false;
        for m in 0..23u8 {
            let Some(t) = rustudio_core::terrain_mat::material(m, use2022) else {
                continue;
            };
            let rgb = match &colors {
                Some(c) if m >= 2 && (m as usize) * 3 + 3 <= c.len() => {
                    let i = m as usize * 3;
                    [c[i], c[i + 1], c[i + 2]]
                }
                _ if m >= 2 => t.default_color,
                _ => rustudio_core::terrain::material_color(m),
            };
            let unit = |c: [u8; 3]| c.map(|x| x as f32 / 255.0);
            // At the default MaterialColor the texture shows as it is (Studio's
            // grey Concrete and Slate pads); another colour turns its luma and
            // chroma from the default's to it.
            let [k, cs, sn] = rustudio_core::terrain_mat::recolor(unit(t.default_color), unit(rgb));
            for face in 0..3u8 {
                let i = rustudio_core::terrain::palette(m, face) as usize * 4;
                let mut layer = -1.0f32;
                // A MaterialService override's loaded ColorMap: its mean is
                // the decoration grass blade base (matpack::mean_albedo).
                let mut blade_base = None;
                let ovr = rustudio_core::terrain_mat::terrain_override(&self.doc, m, use2022);
                let is_override = ovr.is_some();
                let tex = ovr.or_else(|| t.faces[face as usize].clone()).unwrap_or_default();
                if self.terrain_present[m as usize] && tex.pack != 0 {
                    match rustudio_render::matpack::terrain_pack_maps(tex.pack, 0xff80_8080) {
                        rustudio_render::matpack::Lookup::Ready(maps) => {
                            layer = renderer.material_layer(&device, &queue, &maps) as f32;
                            waiting |= layer < 0.0;
                            if is_override {
                                blade_base = Some(rustudio_render::matpack::mean_albedo(&maps));
                            }
                        }
                        rustudio_render::matpack::Lookup::Pending => waiting = true,
                        rustudio_render::matpack::Lookup::Unavailable(_) => {}
                    }
                }
                let range = (tex.blend_max - tex.blend_min).abs().max(1e-3);
                params[i] = [tex.tiling, tex.detiling, tex.rotation, layer];
                params[i + 1] = [1.0 / range, -tex.blend_min / range, tex.blend_scale.max(1e-3), 0.0];
                params[i + 2] = [k, cs, sn, 0.0];
                // The average albedo (sRGB) a material shows without its pack.
                // (Measured at the default MaterialColors only: a place's own
                // colour recolours avgAlbedo, as nova-terrain's custom colours
                // match Studio better that way.)
                let fallback = (!use2022 && rgb == t.default_color)
                    .then(|| rustudio_core::mech::TERRAIN_FALLBACK_ALBEDO.iter().find(|f| f.0 == m).map(|f| f.1))
                    .flatten();
                let avg = if m >= 2 { unit(fallback.unwrap_or(t.avg_albedo)) } else { unit(rgb) };
                params[i + 3] = [avg[0], avg[1], avg[2], 0.0];
                if let Some(b) = blade_base {
                    // Row 3 .w = 1: the shader's tint-form blades use .rgb as
                    // their base instead of the built-in 2022 Grass base.
                    params[i + 3] = [b[0], b[1], b[2], 1.0];
                }
                rustudio_core::terrain_mat::set_emissive(&mut params, i, m);
                rustudio_core::terrain_mat::set_override_tint(&mut params, i, &self.doc, m, use2022, rgb);
            }
        }
        if std::env::var_os("RUSTUDIO_PALETTE_DEBUG").is_some() {
            eprintln!("GRASS_PALETTE| modern={use2022} rows={:?}", &params[24..28]);
        }
        renderer.set_terrain_params(&queue, &params);
        self.terrain_params_dirty = waiting;
        self.terrain_params_gen = generation;
    }

    /// The part (resolved to its top-level Model unless Alt is held) under a point.

    pub fn sync_view_draws(&mut self, ctx: &egui::Context) {
        // (the last frame's commands are submitted before this one writes
        // the buffers they read: egui's paint joined it already, unless
        // nothing was painted)
        rustudio_render::render_thread::wait();
        {
            let _s = rustudio_core::bench::scope("vp_sync_draws");
            self.sync_draws();
        }
        if std::mem::take(&mut self.scene.terrain_dirty) {
            self.sync_terrain();
        }
        self.sync_terrain_params();
        self.sync_terrain_grass();
        // Decoration sways in Edit mode too, even when input is idle.
        if !self.terrain_grass_built.is_empty() && (rustudio_core::rustudio_settings::performance_choice("idle") == 0 || self.is_playing()) {
            ctx.request_repaint_after(std::time::Duration::from_millis(16));
        }
    }

    fn quality_profile(&self) -> Option<rustudio_render::quality::Profile> {
        (rustudio_core::rustudio_settings::performance_value("quality") == "automatic")
            .then(|| self.automatic_quality.profile())
    }

    pub fn draw_world(
        &mut self,
        ctx: &egui::Context,
        painter: &egui::Painter,
        rect: Rect,
        mut lines: Vec<LineVertex>,
        overlay: &[PartDraw],
        started: std::time::Instant,
    ) {
        let playing = self.is_playing();
        let quality_start = self.quality_profile().map(|_| std::time::Instant::now());
        if let Some(now) = quality_start {
            let gpu_ms = rustudio_render::gpu_timing::last_scene_ms();
            if let Some((last, was_playing)) = self.quality_frame_at.replace((now, playing)) {
                if playing == was_playing {
                    let ms = now.duration_since(last).as_secs_f32() * 1_000.0;
                    let render_ms = gpu_ms.map(|gpu| gpu.max(self.quality_cpu_ms));
                    self.automatic_quality.observe(if playing { ms.min(500.0) } else { ms }, render_ms);
                }
            }
        } else {
            self.quality_frame_at = None;
        }
        let quality = self.quality_profile();
        // WireframeHandleAdornments (domains/world_misc.rs).
        let (wire_lines, wire_texts) = rustudio_engine::runtime::domains::world::misc::wire_geometry(&self.doc);
        lines.extend(wire_lines.into_iter().map(|(pos, color)| LineVertex { pos, color }));
        let ppp = ctx.pixels_per_point();
        let mut size = [
            (rect.width() * ppp).round().max(1.0) as u32,
            (rect.height() * ppp).round().max(1.0) as u32,
        ];
        // (a fixed render size: the camera takes its aspect)
        let rrect = match self.view_render_size {
            Some(s) => {
                size = s;
                Rect::from_min_size(rect.min, egui::vec2(s[0] as f32, s[1] as f32) / ppp)
            }
            None => rect,
        };
        let scale = rustudio_core::rustudio_settings::performance_value("resolution")
            .parse::<f32>()
            .unwrap_or(100.0)
            / 100.0;
        size = size.map(|n| (n as f32 * scale).round().max(1.0) as u32);
        let (globals, lights, shadows, sun_cascades) = {
            let _l = rustudio_core::bench::scope("lighting");
            self.frame_lighting(rrect)
        };
        // Studio's scene environment probe (env_probe.rs): the PBR
        // technologies' 128 px indoor reflection cube.
        let probe = (quality.is_none_or(|p| p.detail() >= 2.0) && globals.env_spec[2] > 0.5 && std::env::var_os("RUSTUDIO_NO_ENV_PROBE").is_none()).then(|| {
            let _p = rustudio_core::bench::scope("env_probe");
            let revision = self.renderer.as_ref().map_or(0, |r| r.static_gen());
            let now = rustudio_core::sim_clock::since(started).as_secs_f64();
            let dt = ctx.input(|i| i.unstable_dt);
            let (doc, scene) = (&self.doc, &self.scene);
            let ray = |o: Vec3, d: Vec3, len: f32| rustudio_engine::env_probe::world_ray(doc, scene, o, d, len);
            self.env_probe.update(self.cam.pos, self.cam.forward(), dt, now, revision, &ray)
        });
        let vp_render = rustudio_core::bench::scope("vp_render");
        let tex = match (self.renderer.as_mut(), self.render_state.as_ref()) {
            (Some(r), Some(rs)) => r.render(
                rs,
                size,
                &Frame {
                    globals,
                    quality,
                    cull_view_proj: self.cam.view_proj(rrect),
                    fov_y: self.cam.fov_deg.to_radians(),
                    lights: &lights,
                    shadows,
                    sun_cascades,
                    overlay,
                    lines: &lines,
                    sky: self.cam.ortho.is_none(),
                    water: rustudio_render::water::WaterLook {
                        time: std::env::var("RUSTUDIO_CAPTURE_WATER_TIME").ok().and_then(|s| s.parse().ok()).unwrap_or_else(|| rustudio_core::sim_clock::since(started).as_secs_f32()),
                        ..self.water_waves
                    },
                    clouds: quality.is_none_or(|p| p.clouds())
                        .then(|| rustudio_render::clouds::look(&self.doc)).flatten(),
                    probe,
                },
            ),
            _ => None,
        };
        drop(vp_render);
        if let Some(start) = quality_start {
            self.quality_cpu_ms = start.elapsed().as_secs_f32() * 1_000.0;
        }
        // The 3D view behind the GUI (UIBlur samples it).
        rustudio_engine::gui::share().scene = tex.map(|id| (id, rect));
        match tex {
            Some(id) => {
                painter.image(
                    id,
                    rect,
                    Rect::from_min_max(Pos2::ZERO, Pos2::new(1.0, 1.0)),
                    Color32::WHITE,
                );
                self.viewport_drawn = true;
            }
            None => {
                painter.rect_filled(rect, 0.0, theme::BG);
                painter.text(
                    rect.center(),
                    egui::Align2::CENTER_CENTER,
                    "3D viewport unavailable (no wgpu device)",
                    egui::FontId::proportional(16.0),
                    theme::TEXT_DIM,
                );
            }
        }

        // Wireframe text: monospace, baseline-left on the point (Studio).
        for (p, text, size, c) in wire_texts {
            if let Some(at) = self.cam.project(rect, p) {
                let col = Color32::from_rgba_unmultiplied(
                    (c[0].clamp(0.0, 1.0) * 255.0) as u8,
                    (c[1].clamp(0.0, 1.0) * 255.0) as u8,
                    (c[2].clamp(0.0, 1.0) * 255.0) as u8,
                    (c[3] * 255.0) as u8,
                );
                painter.text(at, egui::Align2::LEFT_BOTTOM, text, egui::FontId::monospace(size), col);
            }
        }

    }

    pub fn draw_view_gui(
        &mut self,
        ctx: &egui::Context,
        painter: &egui::Painter,
        rect: Rect,
        resp: &egui::Response,
        show_gui: bool,
    ) -> bool {
        // GUIs over the 3D view: ScreenGuis (the player's while playing,
        // StarterGui's while editing), SurfaceGuis and BillboardGuis.
        // Effects share these batches with world GUIs. Clear every frame,
        // including when Show UI / RUSTUDIO_NO_GUI skips Gui::draw; otherwise
        // live particles and beams accumulate all previous frames' geometry.
        self.gui.world_batches.clear();
        let playing = self.play.is_some();
        self.gui.pointer_override = self.lock_center_held().then(|| rect.center());
        {
            let doc_dir = self
                .doc
                .path
                .as_ref()
                .and_then(|p| p.parent())
                .map(|p| p.to_path_buf());
            let Game { doc, cam, assets, play, gui, game_grab, fixed_dt, .. } = self;
            // (RUSTUDIO_NO_GUI hides every GUI, to tell GUI from 3D in tests.)
            if std::env::var_os("RUSTUDIO_NO_GUI").is_none() && show_gui {
                let _g = rustudio_core::bench::scope("gui_draw");
                gui.sync_roots(doc);
                if playing {
                    gui.draw_messages(&ctx, &painter, assets, doc, rect);
                }
                gui.draw(
                    &ctx,
                    &painter,
                    rustudio_engine::gui::DrawIn {
                        render_state: self.render_state.as_ref(),
                        doc,
                        cam,
                        rect,
                        store: assets,
                        doc_dir: doc_dir.as_deref(),
                        playing,
                        fixed_step: fixed_dt.is_some(),
                        roots: rustudio_engine::gui::Roots::View,
                    },
                );
                // The game's cursor image over the view. While LockCenter
                // holds the mouse (shift lock, first person) Roblox draws it
                // in the middle of the view, where its mouse is, at the
                // image's own size (MouseLockedCursor for shift lock, its
                // arrow when the game set none); the system cursor is hidden
                // then (update_game_mouse). Other grabs (a right-drag look)
                // show nothing.
                let menu = play.as_ref().is_some_and(|s| s.game.menu_open);
                let locked_center = gui.pointer_override.is_some();
                let (image, at) = if locked_center {
                    (
                        play.as_ref().and_then(|s| {
                            rustudio_engine::mouse_lock::locked_icon(s.mouse_icon, s.cursor_image.as_deref())
                        }),
                        Some(rect.center()),
                    )
                } else {
                    (
                        play.as_ref()
                            .filter(|s| s.mouse_icon && game_grab.is_none() && !menu)
                            .and_then(|s| s.cursor_image.clone()),
                        ctx.input(|i| i.pointer.latest_pos()).filter(|p| rect.contains(*p)),
                    )
                };
                if let (Some(id), Some(at)) = (image, at) {
                    if gui.draw_cursor(&ctx, &painter, assets, doc_dir.as_deref(), &id, at)
                        && !locked_center
                    {
                        ctx.set_cursor_icon(egui::CursorIcon::None);
                    }
                }
            }
            // (The open in-game menu covers the game's GUIs: they get no input.)
            if playing && !play.as_ref().is_some_and(|s| s.game.menu_open) {
                let took = gui.input(&ctx, &resp, rect);
                gui.keyboard(&ctx, doc);
                gui.navigate(&ctx, doc);
                took
            } else {
                false
            }
        }
    }

    pub fn render_viewport_frames(&mut self, extra: &std::collections::HashMap<Ref, Request>) {
        // ViewportFrames the GUI drew: their parts through this renderer
        // (vf_render.rs), before the world GUI pass and egui composite them.
        if let (Some(r), Some(rs)) = (self.renderer.as_mut(), self.render_state.as_ref()) {
            let _g = rustudio_core::bench::scope("vf_render");
            let mut reqs = self.gui.vf_requests(&self.doc);
            reqs.extend(extra.iter().map(|(k, v)| (*k, *v)));
            r.render_viewport_frames(rs, &reqs);
            self.gui.set_vf_textures(|vf| r.vf_texture(vf));
        }
    }

    pub fn finish_view_gui(&mut self, ctx: &egui::Context, rect: Rect, dt: f32) {
        // ParticleEmitters, Beams and Trails: triangles in the same pass, after
        // the GUIs (round 25).
        // (RUSTUDIO_NO_FX leaves them out, to measure their cost.)
        if self.renderer.is_some() && std::env::var_os("RUSTUDIO_NO_FX").is_none() {
            let quality = self.quality_profile();
            let doc_dir = self.doc.path.as_ref().and_then(|p| p.parent()).map(|p| p.to_path_buf());
            let Game { doc, cam, scene, assets, play, fixed_dt, gui, effects, lighting, .. } = self;
            // (round 26: see-through parts dim the effects behind them; the
            // server's :Emit reaches the client's view)
            let glass = if effects.is_empty() { Vec::new() } else { effects.glass(scene, cam) };
            let server = play.as_ref().filter(|s| s.showing == rustudio_engine::runtime::Side::Client).and_then(|s| {
                let id = s.other_doc_id()?;
                Some((id, &s.map.s2c))
            });
            let s2c = server.map(|(_, m)| move |r: Ref| m.get(&r).copied());
            let live = effects.frame(
                rustudio_render::effects::FrameIn {
                    doc,
                    cam,
                    rect,
                    dt: fixed_dt.unwrap_or(dt),
                    quality,
                    texture: &mut |id| gui.texture(&ctx, assets, doc_dir.as_deref(), id).map(|t| t.0),
                    light: &lighting.fx,
                    glass: &glass,
                    server: server.zip(s2c.as_ref()).map(|((id, _), f)| (id, f as &dyn Fn(Ref) -> Option<Ref>)),
                },
            );
            if live {
                ctx.request_repaint();
            }
        }
        // SurfaceGuis / BillboardGuis behind the world stay behind it (round 23).
        let mut hl = self.highlight_draws();
        if !self.extra_highlights.is_empty() {
            let room = rustudio_render::highlight::MAX.saturating_sub(hl.len());
            hl.extend(self.extra_highlights.iter().take(room).cloned());
        }
        if let (Some(r), Some(rs)) = (self.renderer.as_mut(), self.render_state.as_ref()) {
            let _g = rustudio_core::bench::scope("gui_world_pass");
            // The same tone curve rule as the 3D view (lighting::tone_curve_on).
            let curve = rustudio_render::lighting::tone_curve_on(&self.doc).then_some(mech::TONE_CURVE);
            r.draw_world_gui(rs, ctx.font_texture_update(), &self.gui.world_batches, self.effects.drawn(), curve);
            // The post chain over the scene and its effects, on to the
            // render thread with the rest of the frame's commands.
            r.finish_frame(rs);
            // 3D viewport adornments (Team Create collaborator camera gizmos, adorn.rs).
            r.draw_adorn(rs, &self.extra_adorn);
            // Highlights last, over the finished image (highlight.rs).
            r.draw_highlights(rs, &hl);
        }
        if let Some(s) = self
            .play
            .as_mut()
            .filter(|s| s.showing == rustudio_engine::runtime::Side::Client)
        {
            s.game.gui_events.append(&mut self.gui.events);
            // (handed over only when it changed: the game keeps the last one)
            if self.gui.take_layout_change() {
                s.game.gui_layout = self.gui.layout.clone();
                s.game.gui_layout_changed = true;
            }
        } else {
            self.gui.events.clear();
        }

    }
}

impl LightingState {
    /// The skybox faces are still downloading or loading (the sky light in
    /// use is the previous one's until they arrive).
    pub fn sky_pending(&self) -> bool {
        self.sky_failed || self.sky_job.is_some()
    }

    /// Rebuild the light grid next frame, whatever the time since the last.
    pub fn refresh_lightgrid_now(&mut self) {
        self.lightgrid_dirty = true;
        self.lightgrid_at = None;
    }

    /// Parts or terrain changed since the light grid on the GPU was built.
    pub fn lightgrid_pending(&self) -> bool {
        self.lightgrid_dirty
            || self.lightgrid_moves
            || self.lightgrid_job.is_some()
            || self.lightgrid_local_dirty
            || self.lightgrid_local_job.is_some()
    }

}

/// Lighting the viewport keeps between frames.
#[derive(Default)]
pub struct LightingState {
    pub lights: rustudio_render::lighting::LightCache,
    /// Skybox face ids in use ("" = none yet) and whether one failed to load.
    sky_key: String,
    sky_failed: bool,
    /// The sky being prepared on a worker (for that face key).
    #[allow(clippy::type_complexity)]
    sky_job: Option<(String, std::sync::mpsc::Receiver<Option<(u32, Vec<u8>, [Vec3; 6])>>)>,
    /// Studio's studs.dds was looked for (loaded when found).
    studs_tried: bool,
    /// Light grid (lightgrid.rs): where it is (chunks, Studio's camera
    /// follow), the grid last asked for (first voxel, document) and the one
    /// on the GPU, last rebuild, parts or terrain changed since, the
    /// terrain's occupancy for the current place of the grid, and the
    /// worker filling the next.
    lightgrid_place: Option<[i32; 3]>,
    lightgrid_key: Option<([i32; 3], u64)>,
    lightgrid_sun: [i32; 3],
    lightgrid_shown: Option<[i32; 3]>,
    lightgrid_at: Option<std::time::Instant>,
    lightgrid_dirty: bool,
    /// Only parts moved since (refreshed every LIGHTGRID_MOVES_REFRESH_S).
    lightgrid_moves: bool,
    /// Hull bounds per (mesh id, CollisionFidelity) at unit size.
    lightgrid_hull_boxes: std::collections::HashMap<(String, u32), Vec<(Vec3, Vec3)>>,
    lightgrid_terrain: Option<([i32; 3], Option<std::sync::Arc<Vec<u8>>>)>,
    /// The last full build's inputs (its key and terrain plane), to skip a
    /// rebuild that would come out the same.
    lightgrid_inputs: Option<(Vec<u32>, Option<std::sync::Arc<Vec<u8>>>)>,
    lightgrid_lights: Vec<rustudio_render::render::LightRaw>,
    #[allow(clippy::type_complexity)]
    lightgrid_job: Option<std::sync::mpsc::Receiver<(rustudio_render::lightgrid::GridBuild, Option<rustudio_render::lightgrid_gpu::GridUpload>, f64, Vec<((String, u32), Vec<(Vec3, Vec3)>)>, bool)>>,
    /// The last full build's state (lightgrid::GridState): the next build
    /// redoes only what changed parts touch.
    lightgrid_state: Option<rustudio_render::lightgrid::GridState>,
    /// The shown grid's voxels and its local channel as kept
    /// (lightgrid::LocalLights): a change of lights alone (a muzzle flash's
    /// PointLight flicked on for 35 ms, moving with the camera) re-lights
    /// just the boxes of the lights that changed, in the same frame, as
    /// Studio's grid shows it within a frame or two; a rebuild of the whole
    /// channel (throttled full builds, then ~30 ms re-lights on a worker)
    /// showed a flash late, kept it lit after it was off and skipped shots.
    /// `lightgrid_local_dirty`: the lights changed since `lightgrid_locals`
    /// last caught up; `lightgrid_local_job`: a whole re-light on a worker
    /// (when the lights that changed reach more than
    /// LIGHTGRID_LOCAL_PATCH_MAX voxels).
    lightgrid_occ: Option<std::sync::Arc<rustudio_render::lightgrid::Occupancy>>,
    lightgrid_locals: Option<rustudio_render::lightgrid::LocalLights>,
    lightgrid_local_dirty: bool,
    #[allow(clippy::type_complexity)]
    lightgrid_local_job: Option<std::sync::mpsc::Receiver<(rustudio_render::lightgrid::LocalLights, Option<rustudio_render::lightgrid_gpu::GridUpload>, f64)>>,
    /// Parts blocking local lights (lightocc.rs): the grid last asked for
    /// (first voxel, studs per voxel, document) and the one on the GPU,
    /// last rebuild, parts changed since, and the worker filling the next.
    lightocc_origin: Option<([i32; 3], f32, u64)>,
    lightocc_shown: Option<([i32; 3], f32)>,
    lightocc_at: Option<std::time::Instant>,
    lightocc_dirty: bool,
    lightocc_job: Option<std::sync::mpsc::Receiver<rustudio_render::lightocc::OccGrid>>,
    /// Lights drawn last frame (for the self-test and bench).
    pub drawn_lights: usize,
    /// Skybox irradiance per axis (+X, −X, +Y, −Y, +Z, −Z).
    pub sky_irr: [Vec3; 6],
    /// Studio's indoor environment (textures/sky/indoor512): its irradiance
    /// per axis, and whether its faces were looked for.
    pub indoor_irr: [Vec3; 6],
    indoor_tried: bool,
    /// The light particles, beams and trails take (round 26, effects.rs):
    /// this frame's terms, local lights, roof map and light grid.
    pub fx: rustudio_render::effects::SceneLight,
}

impl Game {
    /// Studio's indoor environment cube (PrefilteredEnvIndoorTexture) and its
    /// irradiance (AmbientCube[6..11] before EnvironmentDiffuseScale), from
    /// PlatformContent textures/sky/indoor512_*.tex, once. Same face order and
    /// mirroring as the skybox (sync_sky).
    fn sync_indoor(&mut self) {
        if self.lighting.indoor_tried {
            return;
        }
        let (Some(renderer), Some(rs)) = (self.renderer.as_mut(), self.render_state.as_ref()) else {
            return;
        };
        self.lighting.indoor_tried = true;
        let mut imgs = Vec::new();
        for f in ["bk", "dn", "ft", "lf", "rt", "up"] {
            let id = format!("{}{f}.tex", mech::ENV_INDOOR_FACES);
            let Some(img) = rustudio_core::assets::resolve(&id, None)
                .and_then(|p| std::fs::read(p).ok())
                .and_then(|b| rustudio_core::texture::decode(&b).ok())
            else {
                return;
            };
            imgs.push(img);
        }
        let size = imgs.iter().map(|i| i.width.min(i.height)).min().unwrap_or(1).clamp(1, 2048);
        let fit = |img: &rustudio_core::texture::Rgba| -> Vec<u8> {
            if img.width == size && img.height == size {
                return img.pixels.clone();
            }
            let Some(src) = image::RgbaImage::from_raw(img.width, img.height, img.pixels.clone()) else {
                return vec![0; (size * size * 4) as usize];
            };
            image::imageops::resize(&src, size, size, image::imageops::FilterType::Triangle).into_raw()
        };
        let flip = |mut px: Vec<u8>| -> Vec<u8> {
            let s = size as usize;
            for row in px.chunks_mut(s * 4) {
                for x in 0..s / 2 {
                    for c in 0..4 {
                        row.swap(x * 4 + c, (s - 1 - x) * 4 + c);
                    }
                }
            }
            px
        };
        let cube = [
            flip(fit(&imgs[3])),
            flip(fit(&imgs[4])),
            flip(fit(&imgs[5])),
            flip(fit(&imgs[1])),
            flip(fit(&imgs[0])),
            flip(fit(&imgs[2])),
        ];
        renderer.set_indoor(&rs.device, &rs.queue, size, &cube);
        self.lighting.indoor_irr = rustudio_render::lighting::sky_irradiance(&cube, size);
    }

    /// The skybox: the Sky's six faces (or Studio's default sky), uploaded
    /// when they change.
    fn sync_sky(&mut self) {
        // A prepared sky from the worker (decode, resize, flips and the
        // irradiance took ~145 ms on the main thread when Place A swapped skies).
        if let Some((key, rx)) = &self.lighting.sky_job {
            match rustudio_core::sim_clock::job_recv(rx) {
                Ok(done) => {
                    let key = key.clone();
                    self.lighting.sky_job = None;
                    if key == self.lighting.sky_key {
                        self.apply_sky(done);
                    }
                }
                Err(std::sync::mpsc::TryRecvError::Empty) => {}
                Err(std::sync::mpsc::TryRecvError::Disconnected) => self.lighting.sky_job = None,
            }
        }
        let faces = rustudio_render::lighting::sky_faces(&self.doc);
        let orient = rustudio_render::lighting::sky_orientation(&self.doc);
        // Whether scripts run is part of the key: a local sky loaded in Edit
        // isn't place content in Play (assets::place_content_allowed).
        let key = format!("{}|{:?}|{}", faces.join("|"), orient.to_array(), rustudio_core::assets::scripts_running());
        if key == self.lighting.sky_key && !self.lighting.sky_failed {
            return;
        }
        let doc_dir = self
            .doc
            .path
            .as_ref()
            .and_then(|p| p.parent())
            .map(|p| p.to_path_buf());
        // Every face's bytes first: nothing is decoded while one downloads.
        let mut bytes = Vec::new();
        for id in &faces {
            match self.assets.content(id, doc_dir.as_deref()) {
                rustudio_core::assets::Fetch::Ready(b) => bytes.push(b),
                // Still downloading: try again next frame.
                rustudio_core::assets::Fetch::Pending => {
                    self.lighting.sky_failed = true;
                    return;
                }
                rustudio_core::assets::Fetch::Unavailable(_) => break,
            }
        }
        self.lighting.sky_key = key.clone();
        self.lighting.sky_failed = false;
        if bytes.len() != 6 {
            self.lighting.sky_job = None;
            self.apply_sky(None);
            return;
        }
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let _ = tx.send(prepare_sky(&bytes, orient));
        });
        self.lighting.sky_job = Some((key, rx));
    }

    /// Puts a prepared sky (or, with None, the gradient sky) on the GPU.
    fn apply_sky(&mut self, sky: Option<(u32, Vec<u8>, [Vec3; 6])>) {
        let (Some(renderer), Some(rs)) = (self.renderer.as_mut(), self.render_state.as_ref())
        else {
            return;
        };
        let Some((size, cube, irr)) = sky else {
            renderer.set_sky(&rs.device, &rs.queue, 0, None);
            // The gradient sky: its colours, squared.
            let top = rustudio_render::lighting::sq(mech::SKY_GRADIENT_TOP);
            let hor = rustudio_render::lighting::sq(mech::SKY_GRADIENT_HORIZON);
            let side = (top + hor) * 0.5;
            self.lighting.sky_irr = [side, side, top, hor * 0.5, side, side];
            return;
        };
        renderer.set_sky(&rs.device, &rs.queue, size, Some(&cube));
        self.lighting.sky_irr = irr;
        let up = self.lighting.sky_irr[2];
        self.info(format!(
            "Sky: {size}px faces, light from above {:.3} {:.3} {:.3}",
            up.x, up.y, up.z
        ));
    }

    /// Keeps Studio's light grid (lightgrid.rs) around the camera: it moves
    /// as Studio's does (a chunk of hysteresis), and is rebuilt when it
    /// moved or parts / terrain changed (at most every LIGHTGRID_REFRESH_S):
    /// the parts in it and the terrain are gathered here, voxelized and lit
    /// on a worker, and uploaded when done; the GPU keeps the last one (with
    /// its own origin) meanwhile. Returns its globals entry (w 0 = none yet).
    fn sync_lightgrid(&mut self, lights: &[rustudio_render::render::LightRaw], sun: Vec3, shadows: bool, voxel_lights: bool) -> [f32; 4] {
        let lights = if voxel_lights || rustudio_render::lighting::soft_style(&self.doc) { lights } else { &[] };
        let st = &mut self.lighting;
        if bytemuck::cast_slice::<_, u8>(&st.lightgrid_lights) != bytemuck::cast_slice::<_, u8>(lights) {
            st.lightgrid_lights = lights.to_vec();
            // With the voxels at hand only the local channel needs redoing
            // (a full build running brings its own lights; the ones after
            // are caught up when it lands).
            if st.lightgrid_occ.is_some() || st.lightgrid_job.is_some() {
                st.lightgrid_local_dirty = true;
            } else {
                st.lightgrid_dirty = true;
            }
        }
        if let Some(rx) = &st.lightgrid_local_job {
            match rx.try_recv() {
                Ok((locals, upload, ms)) => {
                    // (the worker's own time, in the bench as a call per job)
                    rustudio_core::bench::add("lightgrid_local_job", ms, 1);
                    // (no full build starts while this runs, so the grid it
                    // re-lit is still the one shown; if not, rebuild)
                    if st.lightgrid_shown == Some(locals.origin) {
                        if let (Some(renderer), Some(up)) = (self.renderer.as_mut(), upload) {
                            renderer.upload_lightgrid(up);
                        }
                        st.lightgrid_local_dirty = !locals.matches_lights(&st.lightgrid_lights);
                        st.lightgrid_locals = Some(locals);
                    } else {
                        st.lightgrid_dirty = true;
                    }
                    st.lightgrid_local_job = None;
                }
                Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                    st.lightgrid_local_job = None;
                    st.lightgrid_dirty = true;
                }
                Err(std::sync::mpsc::TryRecvError::Empty) => {}
            }
        }
        if let Some(rx) = &st.lightgrid_job {
            match rustudio_core::sim_clock::job_recv(rx) {
                Ok((build, upload, ms, learnt, pending)) => {
                    // (the upload was made ready on the job's thread: taking
                    // it here only queues copies for the next frame)
                    let _apply = rustudio_core::bench::scope("lt_grid_apply");
                    rustudio_core::bench::add("lightgrid_job", ms, 1);
                    // (hulls the job worked out; one still computing on the
                    // physics workers asks for another build later)
                    st.lightgrid_hull_boxes.extend(learnt);
                    if pending {
                        st.lightgrid_dirty = true;
                    }
                    rustudio_core::bench::add(if build.incremental { "lightgrid_job_incremental" } else { "lightgrid_job_full" }, 0.0, 1);
                    let rustudio_render::lightgrid::GridBuild { grid, occ, locals, state, .. } = build;
                    st.lightgrid_occ = Some(occ);
                    st.lightgrid_state = Some(state);
                    if let Ok(points) = std::env::var("RUSTUDIO_LIGHTGRID_DEBUG") {
                        for point in points.split(';') {
                            let p: Vec<f32> = point.split(',').filter_map(|x| x.parse().ok()).collect();
                            if p.len() != 3 { continue; }
                            let pos = Vec3::new(p[0], p[1], p[2]);
                            let v = (pos * 0.25).floor().as_ivec3().to_array();
                            eprintln!("LIGHTGRID camera={:?} origin={:?} pos={:?} sky={:?}", self.cam.pos, grid.origin, pos, grid.sky_at(v));
                        }
                    }
                    if let (Some(renderer), Some(up)) = (self.renderer.as_mut(), upload) {
                        renderer.upload_lightgrid(up);
                        st.lightgrid_shown = Some(grid.origin);
                        // (the last grid's bytes are let go on the drop thread)
                        if let Some(old) = st.fx.grid.replace(std::sync::Arc::new(grid)) {
                            egui_wgpu::prewarm::drop_later(old);
                        }
                        st.lightgrid_local_dirty = !locals.matches_lights(&st.lightgrid_lights);
                        st.lightgrid_locals = Some(locals);
                    }
                    st.lightgrid_job = None;
                }
                Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                    st.lightgrid_job = None;
                    st.lightgrid_state = None;
                }
                Err(std::sync::mpsc::TryRecvError::Empty) => {}
            }
        }
        let place = rustudio_render::lightgrid::follow(st.lightgrid_place, self.cam.pos);
        st.lightgrid_place = Some(place);
        let ch = rustudio_render::lightgrid::CHUNK;
        let origin = [place[0] * ch[0] as i32, place[1] * ch[1] as i32, place[2] * ch[2] as i32];
        // The sun channel follows the sun: rebuilt when it turns (as Studio
        // re-dirties its chunks' sun flag), quantised so a moving clock
        // doesn't rebuild every frame. No sun (GlobalShadows off, or no
        // light): the channel stays 255.
        let sun_key = if shadows { (sun * 512.0).round().as_ivec3().to_array() } else { [0; 3] };
        if st.lightgrid_sun != sun_key {
            st.lightgrid_sun = sun_key;
            st.lightgrid_dirty = true;
        }
        // (the build takes the sun as the key holds it: a rebuild for parts
        // alone then keeps the last build's sun channel instead of sweeping
        // it again for a sun a hair away)
        let sun = if shadows { (Vec3::from_array(sun_key.map(|v| v as f32)) / 512.0).normalize_or(sun) } else { sun };
        let key = (origin, self.doc.id);
        let moved = st.lightgrid_key != Some(key);
        let since = st.lightgrid_at.map(|t| rustudio_core::sim_clock::since(t).as_secs_f32());
        let stale = (st.lightgrid_dirty && since.is_none_or(|t| t >= mech::LIGHTGRID_REFRESH_S))
            || (st.lightgrid_moves && since.is_none_or(|t| t >= mech::LIGHTGRID_MOVES_REFRESH_S));
        if (moved || stale) && st.lightgrid_job.is_none() && st.lightgrid_local_job.is_none() {
            let _b = rustudio_core::bench::scope("lightgrid");
            // The terrain's share, read again only when the grid moved or
            // the terrain changed.
            let scope = rustudio_core::bench::scope("lg_terrain");
            if st.lightgrid_terrain.as_ref().is_none_or(|(o, _)| *o != origin) {
                let plane = self
                    .terrain_drawn
                    .as_ref()
                    .and_then(|(_, _, v)| rustudio_render::lightgrid::terrain_plane(v, origin))
                    .map(std::sync::Arc::new);
                st.lightgrid_terrain = Some((origin, plane));
            }
            let terrain = st.lightgrid_terrain.as_ref().and_then(|(_, p)| p.clone());
            drop(scope);
            let scope = rustudio_core::bench::scope("lg_gather");
            let [nx, ny, nz] = rustudio_render::lightgrid::dims();
            let lo = Vec3::new(origin[0] as f32, origin[1] as f32, origin[2] as f32) * 4.0;
            let hi = lo + Vec3::new(nx as f32, ny as f32, nz as f32) * 4.0;
            let pending_meshes = false;
            let hull_boxes = &st.lightgrid_hull_boxes;
            // Meshes whose hull boxes aren't known yet: (prim index, mesh,
            // fidelity, size). The worker works them out (a hull is computed
            // on the spot, which held the frame when the grid moved on to
            // many new meshes) before it builds, so the grid is the same.
            let mut unresolved: Vec<(usize, String, u32, Vec3)> = Vec::new();
            let mut index = 0usize;
            let prims: Vec<_> = self
                .scene
                .parts
                .iter()
                .zip(&self.scene.bounds)
                .filter(|(_, b)| {
                    b.x + b.w >= lo.x
                        && b.y + b.w >= lo.y
                        && b.z + b.w >= lo.z
                        && b.x - b.w <= hi.x
                        && b.y - b.w <= hi.y
                        && b.z - b.w <= hi.z
                })
                .filter_map(|(sp, _)| {
                    let p = sp.light?;
                    let mut boxes = None;
                    if p.kind == rustudio_render::lightgrid::Kind::Mesh {
                        let fid = rustudio_render::scene::enum_u32(self.doc.prop(sp.r, "CollisionFidelity"))
                            .unwrap_or(rustudio_physics::collision::FIDELITY_DEFAULT);
                        if fid != rustudio_physics::collision::FIDELITY_BOX {
                            if let Some(mesh) = &sp.mesh_asset {
                                let scale = |unit: &Vec<(Vec3, Vec3)>| {
                                    // Hull points scale per axis by Size, so their
                                    // unit-size bounds do too (Size is positive).
                                    unit.iter().map(|(lo, hi)| (*lo * sp.size, *hi * sp.size)).collect::<Vec<_>>()
                                };
                                if let Some(unit) = hull_boxes.get(&(mesh.id.clone(), fid)) {
                                    boxes = Some(scale(unit));
                                // Don't poison the physics cache with a missing download.
                                } else if self.meshes.state(&mesh.id).is_some_and(|s| s.is_ok()) {
                                    unresolved.push((index, mesh.id.clone(), fid, sp.size));
                                }
                            }
                        }
                    }
                    index += 1;
                    Some((p, boxes))
                })
                .collect();
            // Everything the build below reads, bit for bit: the same inputs
            // give the same grid, so a part change that left them alone (a
            // recolour, a script touching a lamp) rebuilds nothing. The
            // local lights are kept up by their own path either way.
            drop(scope);
            let scope = rustudio_core::bench::scope("lg_inputs");
            let mut inputs: Vec<u32> = Vec::with_capacity(prims.len() * 18 + 8);
            inputs.extend(origin.map(|v| v as u32));
            inputs.push(u32::from(shadows));
            inputs.extend(sun_key.map(|v| v as u32));
            for (i, (p, boxes)) in prims.iter().enumerate() {
                if unresolved.binary_search_by_key(&i, |u| u.0).is_ok() {
                    // (not known yet: never the same as a build that knew it)
                    inputs.push(u32::MAX - 1);
                }
                inputs.push(p.kind as u32);
                inputs.extend(p.size.to_array().map(f32::to_bits));
                inputs.extend(p.rot.map(f32::to_bits));
                inputs.extend(p.pos.to_array().map(f32::to_bits));
                inputs.push(p.transparency.to_bits());
                match boxes {
                    None => inputs.push(u32::MAX),
                    Some(b) => {
                        inputs.push(b.len() as u32);
                        for (lo, hi) in b {
                            inputs.extend(lo.to_array().into_iter().chain(hi.to_array()).map(f32::to_bits));
                        }
                    }
                }
            }
            let same_terrain = match (&terrain, &st.lightgrid_inputs) {
                (None, Some((_, None))) => true,
                (Some(a), Some((_, Some(b)))) => std::sync::Arc::ptr_eq(a, b),
                _ => false,
            };
            drop(scope);
            'build: {
            if unresolved.is_empty() && same_terrain && st.lightgrid_inputs.as_ref().is_some_and(|(k, _)| *k == inputs) && st.lightgrid_shown == Some(origin) {
                rustudio_core::bench::add("lightgrid_unchanged", 0.0, 1);
                st.lightgrid_key = Some(key);
                st.lightgrid_at = Some(rustudio_core::sim_clock::now());
                st.lightgrid_dirty = pending_meshes;
                st.lightgrid_moves = false;
                break 'build;
            }
            st.lightgrid_inputs = Some((inputs, terrain.clone()));
            let local_lights = lights.to_vec();
            // The last build's state, occupancy and local lights go to the
            // job (no local job runs alongside one, so nothing else needs
            // them meanwhile); it builds on them when only parts changed.
            let kept = match (st.lightgrid_state.take(), st.lightgrid_occ.take(), st.lightgrid_locals.take()) {
                (Some(s), Some(o), Some(l)) if st.lightgrid_shown == Some(origin) => Some((s, o, l)),
                _ => None,
            };
            if kept.is_none() {
                // (why a build starts from scratch: the grid moved on, or
                // there is no last build; the job's own full builds less
                // these changed terrain or GlobalShadows)
                rustudio_core::bench::add(if st.lightgrid_shown.is_some_and(|o| o != origin) { "lightgrid_full_moved" } else { "lightgrid_full_first" }, 0.0, 1);
            }
            let (tx, rx) = std::sync::mpsc::channel();
            let mut prims = prims;
            // (the GPU-ready bytes are written on the job's thread too)
            let device = self.render_state.as_ref().map(|rs| rs.device.clone());
            std::thread::spawn(move || {
                let t0 = std::time::Instant::now();
                // The hull boxes the gather didn't know, as it worked them out.
                let mut pending = false;
                let mut learnt: Vec<((String, u32), Vec<(Vec3, Vec3)>)> = Vec::new();
                for (i, id, fid, size) in unresolved {
                    let unit = match learnt.iter().find(|l| l.0 .0 == id && l.0 .1 == fid) {
                        Some(l) => Some(l.1.clone()),
                        None => match rustudio_physics::collision::hulls(&id, fid) {
                            rustudio_physics::collision::Lookup::Ready(hulls) => {
                                let unit: Vec<(Vec3, Vec3)> = hulls.iter().filter_map(|h| {
                                    let first = *h.first()?;
                                    Some(h.iter().fold((first, first), |(lo, hi), v| (lo.min(*v), hi.max(*v))))
                                }).collect();
                                learnt.push(((id.clone(), fid), unit.clone()));
                                Some(unit)
                            }
                            rustudio_physics::collision::Lookup::Pending => {
                                pending = true;
                                None
                            }
                            rustudio_physics::collision::Lookup::Unavailable => None,
                        },
                    };
                    if let Some(unit) = unit {
                        // Hull points scale per axis by Size, so their
                        // unit-size bounds do too (Size is positive).
                        prims[i].1 = Some(unit.iter().map(|(lo, hi)| (*lo * size, *hi * size)).collect());
                    }
                }
                let mut build = rustudio_render::lightgrid::build_grid(origin, &prims, terrain, shadows, sun, &local_lights, kept);
                let upload = device.map(|d| rustudio_render::lightgrid_gpu::GridUpload::of_build(&d, &mut build));
                let _ = tx.send((build, upload, t0.elapsed().as_secs_f64() * 1e3, learnt, pending));
            });
            st.lightgrid_job = Some(rx);
            st.lightgrid_key = Some(key);
            st.lightgrid_at = Some(rustudio_core::sim_clock::now());
            st.lightgrid_dirty = pending_meshes;
            st.lightgrid_moves = false;
            }
        }
        // Lights changed, voxels didn't: re-light the boxes of the lights
        // that changed now, or the whole channel on a worker when they're big.
        // Only one job can publish a grid at a time, including a full rebuild.
        if st.lightgrid_local_dirty && st.lightgrid_local_job.is_none() && st.lightgrid_job.is_none() {
            if let (Some(occ), Some(locals)) = (st.lightgrid_occ.clone(), st.lightgrid_locals.as_mut()) {
                if Some(locals.origin) == st.lightgrid_shown {
                    let _b = rustudio_core::bench::scope("lightgrid_local");
                    match locals.update(&occ, &st.lightgrid_lights, mech::LIGHTGRID_LOCAL_PATCH_MAX) {
                        Ok(patches) => {
                            if let (Some(renderer), Some(rs)) = (self.renderer.as_mut(), self.render_state.as_ref()) {
                                renderer.upload_lightgrid(rustudio_render::lightgrid_gpu::GridUpload::of_patches(&rs.device, &patches));
                            }
                            st.lightgrid_local_dirty = false;
                        }
                        Err(()) => {
                            // The same update on a worker (it keeps the
                            // lights' fields), handed back with its patches.
                            let lights = st.lightgrid_lights.clone();
                            let mut locals = st.lightgrid_locals.take().expect("locals");
                            let device = self.render_state.as_ref().map(|rs| rs.device.clone());
                            let (tx, rx) = std::sync::mpsc::channel();
                            std::thread::spawn(move || {
                                let t0 = std::time::Instant::now();
                                let patches = locals.update(&occ, &lights, usize::MAX).unwrap_or_default();
                                let upload = device.map(|d| rustudio_render::lightgrid_gpu::GridUpload::of_patches(&d, &patches));
                                let _ = tx.send((locals, upload, t0.elapsed().as_secs_f64() * 1e3));
                            });
                            st.lightgrid_local_job = Some(rx);
                            st.lightgrid_local_dirty = false;
                        }
                    }
                }
            }
        }
        match st.lightgrid_shown {
            Some(o) => [o[0] as f32, o[1] as f32, o[2] as f32, mech::LIGHTGRID_NORMAL_OFFSET],
            None => [0.0; 4],
        }
    }

    /// Rebuilds the grid of what blocks local lights (lightocc.rs) when the
    /// camera moved it on, the LightingStyle changed its voxels, or parts
    /// changed (at most every LIGHTOCC_REFRESH_S): the parts in it are
    /// gathered here and the grid is filled on a worker; the GPU keeps the
    /// last one (with its own origin) until the new one arrives. Returns its
    /// globals entry (w 0 = no grid yet, or no shadowed light to block).
    fn sync_lightocc(&mut self, lights: &[rustudio_render::render::LightRaw]) -> [f32; 4] {
        if rustudio_render::lighting::soft_style(&self.doc) || !lights.iter().any(|l| l.extra[2] > 0.5) {
            return [0.0; 4];
        }
        let st = &mut self.lighting;
        if let Some(rx) = &st.lightocc_job {
            match rustudio_core::sim_clock::job_recv(rx) {
                Ok(grid) => {
                    let _apply = rustudio_core::bench::scope("lt_occ_apply");
                    if let (Some(renderer), Some(rs)) = (self.renderer.as_mut(), self.render_state.as_ref()) {
                        renderer.set_lightocc(&rs.queue, &grid.data);
                        st.lightocc_shown = Some((grid.origin, grid.voxel));
                    }
                    st.lightocc_job = None;
                }
                Err(std::sync::mpsc::TryRecvError::Disconnected) => st.lightocc_job = None,
                Err(std::sync::mpsc::TryRecvError::Empty) => {}
            }
        }
        let voxel = rustudio_render::lightocc::voxel_size(rustudio_render::lighting::soft_style(&self.doc));
        let origin = rustudio_render::lightocc::origin_for(self.cam.pos, self.cam.forward(), voxel);
        let key = (origin, voxel, self.doc.id);
        let moved = st.lightocc_origin != Some(key);
        let stale = st.lightocc_dirty
            && st
                .lightocc_at
                .is_none_or(|t| rustudio_core::sim_clock::since(t).as_secs_f32() >= mech::LIGHTOCC_REFRESH_S);
        if (moved || stale) && st.lightocc_job.is_none() {
            let _b = rustudio_core::bench::scope("lightocc");
            let hosts = st.lights.hosts(&mut self.doc);
            let (lo, hi) = rustudio_render::lightocc::Gathered::bounds(origin, voxel);
            let mut solids = Vec::new();
            for (sp, b) in self.scene.parts.iter().zip(&self.scene.bounds) {
                if sp.draw.alpha < mech::LIGHTOCC_MIN_ALPHA
                    || b.x + b.w < lo.x
                    || b.y + b.w < lo.y
                    || b.z + b.w < lo.z
                    || b.x - b.w > hi.x
                    || b.y - b.w > hi.y
                    || b.z - b.w > hi.z
                    || hosts.contains(&sp.r)
                    // MeshParts and unions don't block: their box is no guide
                    // to their shape (Place A's chapel went dark behind its meshes).
                    || sp.mesh_asset.is_some()
                    || sp.draw.custom_mesh.is_some()
                {
                    continue;
                }
                solids.push(rustudio_render::lightocc::Solid {
                    pos: sp.draw.xf.pos,
                    rot: sp.draw.xf.rot,
                    half: sp.draw.size * 0.5,
                    fill: rustudio_render::lightocc::fill_of(sp.draw.mesh),
                });
            }
            let gathered = rustudio_render::lightocc::Gathered { origin, voxel, solids };
            let (tx, rx) = std::sync::mpsc::channel();
            std::thread::spawn(move || {
                let _ = tx.send(rustudio_render::lightocc::fill(gathered));
            });
            st.lightocc_job = Some(rx);
            st.lightocc_origin = Some(key);
            st.lightocc_at = Some(rustudio_core::sim_clock::now());
            st.lightocc_dirty = false;
        }
        match st.lightocc_shown {
            Some((o, v)) => [o[0] as f32, o[1] as f32, o[2] as f32, v],
            None => [0.0; 4],
        }
    }

    /// Studio's legacy surface atlas, read once from the Studio install
    /// (PlatformContent/pc/textures/studs.dds, read only). Without it surfaces
    /// draw smooth.
    fn sync_studs(&mut self) {
        if std::mem::replace(&mut self.lighting.studs_tried, true) {
            return;
        }
        let Some(dir) = rustudio_core::default_scripts::studio_dir() else {
            return;
        };
        let path = dir.join("PlatformContent/pc/textures/studs.dds");
        let img = match std::fs::read(&path)
            .map_err(|e| e.to_string())
            .and_then(|b| rustudio_core::texture::decode(&b))
        {
            Ok(img) => img,
            Err(e) => {
                self.warn(format!(
                    "Legacy surfaces drawn smooth: {}: {e}",
                    path.display()
                ));
                return;
            }
        };
        if let (Some(renderer), Some(rs)) = (self.renderer.as_mut(), self.render_state.as_ref()) {
            renderer.set_studs(&rs.device, &rs.queue, img.width, img.height, &img.pixels);
        }
    }

    /// This frame's lighting: shader constants, local lights, and whether
    /// to draw the sun's shadow map.
    pub fn frame_lighting(
        &mut self,
        rect: Rect,
    ) -> (rustudio_render::render::Globals, Vec<rustudio_render::render::LightRaw>, bool, [rustudio_render::sun_shadow::Cascade; rustudio_render::sun_shadow::CASCADES]) {
        {
            let _s = rustudio_core::bench::scope("lt_sky");
            self.sync_sky();
        }
        {
            let _s = rustudio_core::bench::scope("lt_indoor");
            self.sync_indoor();
        }
        self.sync_studs();
        let planes = rustudio_render::render::frustum(self.cam.view_proj(rect));
        let lt_gather = rustudio_core::bench::scope("lt_gather");
        let quality = self.quality_profile();
        let voxel_lights = quality.is_some_and(|profile| profile.detail() < 2.0);
        let mut lights = self.lighting.lights.gather(&mut self.doc, &self.cam, &|c, r| {
            rustudio_render::render::sphere_visible(&planes, c, r)
        }, voxel_lights);
        drop(lt_gather);
        self.lighting.drawn_lights = lights.len();
        let skybox = self.renderer.as_ref().is_some_and(|r| r.sky_size > 0);
        let mut fl = rustudio_render::lighting::globals(
            &self.doc,
            &self.cam,
            rect,
            lights.len(),
            skybox,
            &self.lighting.sky_irr,
            &self.lighting.indoor_irr,
        );
        if let Some(profile) = quality {
            fl.globals.perf[0] = profile.detail();
            fl.globals.perf[3] = 1.0;
            if !profile.depth_of_field() {
                fl.globals.dof = [0.0; 4];
            }
        }
        let shadow_divisor = quality.map_or_else(
            || [1, 2, 0][rustudio_core::rustudio_settings::performance_choice("shadows")],
            |profile| profile.shadow_tile_divisor(),
        );
        let grid_sun = fl.shadows && (shadow_divisor != 0 || quality.is_some());
        if shadow_divisor == 0 {
            fl.shadows = false;
            fl.globals.bloom[2] = 0.0;
        }
        if shadow_divisor == 0 {
            for light in &mut lights { light.extra[3] = -1.0; }
        }
        if quality.map_or_else(
            || rustudio_core::rustudio_settings::performance_choice("bloom") == 1,
            |profile| !profile.bloom(),
        ) {
            fl.globals.cc_tint[3] = 0.0;
            fl.globals.bloom[1] = -1.0;
        }
        fl.globals.lightgrid = self.sync_lightgrid(&lights, fl.sun_true, grid_sun, voxel_lights);
        fl.globals.lightocc = if voxel_lights { [0.0; 4] } else {
            let _s = rustudio_core::bench::scope("lt_occ");
            self.sync_lightocc(&lights)
        };

        {
            // The effects' light (effects::SceneLight): the same terms.
            let g = &fl.globals;
            let v3 = |a: [f32; 4]| Vec3::new(a[0], a[1], a[2]);
            let fx = &mut self.lighting.fx;
            fx.ambient = v3(g.ambient);
            fx.outdoor = v3(g.outdoor);
            // (at night the light is the moon, which lights particles
            // brighter than parts: effects::SceneLight::sun)
            let moon = g.fog[3] < 0.5;
            fx.sun = v3(g.sun_color)
                * g.sun_dir[3]
                * if moon { mech::FX_MOON_PER_BRIGHTNESS / mech::MOON_IRRADIANCE_PER_BRIGHTNESS } else { 1.0 };
            fx.soft = g.sun_color[3] > 0.5;
            fx.env = g.env.iter().map(|e| v3(*e)).sum::<Vec3>() / 6.0 * v3(g.sky_tint);
            fx.lights.clone_from(&lights);
        }
        // The camera in the Terrain's water: Studio fogs the view toward the
        // lit WaterColor (fog.z 2 = the underwater curve, docs/MECHANICS.md
        // "Underwater"); the shader lights the colour.
        if let Some((_, _, v)) = self.terrain_drawn.as_ref() {
            if v.under_drawn_water(self.cam.pos.to_array()) {
                let rgb = self.terrain_water_look.map(|l| l.0).unwrap_or(mech::WATER_COLOR_DEFAULT);
                fl.globals.fog[0] = mech::UNDERWATER_FOG[0];
                fl.globals.fog[1] = mech::UNDERWATER_FOG[1];
                fl.globals.fog[2] = 2.0;
                fl.globals.fog_color = rustudio_render::lighting::sq(rgb).extend(1.0).to_array();
            }
        }
        // Terrain.Clouds' sun cover (clouds.rs): it softens and dims the sun
        // (shader.wgsl reads it from outdoor.w).
        let cover = rustudio_render::clouds::sun_cover(quality.is_none_or(|p| p.clouds())
            && rustudio_render::clouds::look(&self.doc).is_some());
        fl.globals.outdoor[3] = cover;
        fl.shadow_softness = rustudio_render::clouds::shadow_softness(fl.shadow_softness, cover);
        // The sun's shadow cascades (sun_shadow.rs), around this camera.
        let aspect = (rect.width() / rect.height().max(1.0)).max(0.01);
        let tan_y = (self.cam.fov_deg.to_radians() * 0.5).tan();
        let mut cascades = rustudio_render::sun_shadow::cascades(
            self.cam.pos,
            self.cam.forward(),
            [tan_y * aspect, tan_y],
            fl.sun,
            fl.shadow_softness,
        );
        if shadow_divisor > 1 {
            for cascade in &mut cascades { cascade.tile /= shadow_divisor; }
        }
        (fl.globals.sun_casc, fl.globals.sun_sphere, fl.globals.sun_rect) = rustudio_render::sun_shadow::uniforms(&cascades);
        (fl.globals, lights, fl.shadows, cascades)
    }
}

/// What the viewport last handed the renderer for each part.
#[derive(Default)]
pub struct DrawCache {
    /// The part's CFrame when its draws were made (moves are applied relative to it).
    pub xf: std::collections::HashMap<Ref, Xform>,
    /// Parts with a mesh or texture still loading.
    pub waiting: std::collections::HashSet<Ref>,
    /// Parts to resolve that the last frame's budget (`DRAW_SYNC_BUDGET_MS`)
    /// left for the next one, oldest first.
    deferred: Vec<Ref>,
    /// Per part: (loaded mesh, textured, decals, clothed) for the self-test.
    flags: std::collections::HashMap<Ref, [u32; 4]>,
    /// Parts drawn as one instance a restyle can patch: the factor from the
    /// part's colour to the drawn one, and whether the drawn opacity is
    /// forced (0 while a mesh is missing) rather than the part's.
    style: std::collections::HashMap<Ref, ([f32; 3], bool)>,
    /// Waiting parts held up only by these textures.
    wait_tex: std::collections::HashMap<Ref, Vec<String>>,
    totals: [usize; 4],
}

impl DrawCache {
    fn set(&mut self, r: Ref, xf: Xform, flags: [u32; 4], waiting: bool) {
        if let Some(old) = self.flags.insert(r, flags) {
            for i in 0..4 {
                self.totals[i] -= old[i] as usize;
            }
        }
        for i in 0..4 {
            self.totals[i] += flags[i] as usize;
        }
        self.xf.insert(r, xf);
        if waiting {
            self.waiting.insert(r);
        } else {
            self.waiting.remove(&r);
        }
    }

    fn forget(&mut self, r: Ref) {
        if let Some(old) = self.flags.remove(&r) {
            for i in 0..4 {
                self.totals[i] -= old[i] as usize;
            }
        }
        self.xf.remove(&r);
        self.waiting.remove(&r);
        self.style.remove(&r);
        self.wait_tex.remove(&r);
    }

    pub fn totals(&self) -> [usize; 4] {
        self.totals
    }
}

/// Decodes the six skybox faces (Bk, Dn, Ft, Lf, Rt, Up) into the cube
/// rustudio draws, with its irradiance; None if any face doesn't decode.
fn prepare_sky(bytes: &[std::sync::Arc<Vec<u8>>], orient: Vec3) -> Option<(u32, Vec<u8>, [Vec3; 6])> {
    let mut imgs = Vec::new();
    for b in bytes {
        imgs.push(rustudio_core::texture::decode(b).ok()?);
    }
        // All faces square at the smallest face's size.
        let size = imgs
            .iter()
            .map(|i| i.width.min(i.height))
            .min()
            .unwrap_or(1)
            .clamp(1, 2048);
        let fit = |img: &rustudio_core::texture::Rgba| -> Vec<u8> {
            if img.width == size && img.height == size {
                return img.pixels.clone();
            }
            let Some(src) = image::RgbaImage::from_raw(img.width, img.height, img.pixels.clone())
            else {
                return vec![255; (size * size * 4) as usize];
            };
            image::imageops::resize(&src, size, size, image::imageops::FilterType::Triangle)
                .into_raw()
        };
        // Sky order Bk, Dn, Ft, Lf, Rt, Up → cube +X Lf, −X Rt, +Y Up, −Y Dn, +Z Bk, −Z Ft.
        // Round 26: Studio's sky is this cube mirrored left to right (x → −x):
        // looking down its SkyboxDn shows flipped side to side, looking at
        // SkyboxFt too (tools/fidelity skydown), and at 45° between faces it
        // matches with Lf and Rt swapped as well. Unmirrored, the Dn face's
        // brighter edge met the side faces at 45° down as a lighter band
        // (terrain T9). So each face is flipped horizontally, Rt goes to −X
        // and Lf to +X.
        let flip = |mut px: Vec<u8>| -> Vec<u8> {
            let s = size as usize;
            for row in px.chunks_mut(s * 4) {
                for x in 0..s / 2 {
                    for c in 0..4 {
                        row.swap(x * 4 + c, (s - 1 - x) * 4 + c);
                    }
                }
            }
            px
        };
        // The top face turned 90° clockwise and the bottom 90° counter-clockwise
        // (2026-11-05, user: both were a quarter turn off Studio's).
        let rot_cw = |px: Vec<u8>| -> Vec<u8> {
            let s = size as usize;
            let mut out = vec![0u8; px.len()];
            for y in 0..s {
                for x in 0..s {
                    let (nx, ny) = (s - 1 - y, x);
                    out[(ny * s + nx) * 4..(ny * s + nx) * 4 + 4].copy_from_slice(&px[(y * s + x) * 4..(y * s + x) * 4 + 4]);
                }
            }
            out
        };
        let rot_ccw = |px: Vec<u8>| -> Vec<u8> {
            let s = size as usize;
            let mut out = vec![0u8; px.len()];
            for y in 0..s {
                for x in 0..s {
                    let (nx, ny) = (y, s - 1 - x);
                    out[(ny * s + nx) * 4..(ny * s + nx) * 4 + 4].copy_from_slice(&px[(y * s + x) * 4..(y * s + x) * 4 + 4]);
                }
            }
            out
        };
        let cube = [
            flip(fit(&imgs[3])),
            flip(fit(&imgs[4])),
            rot_cw(flip(fit(&imgs[5]))),
            rot_ccw(flip(fit(&imgs[1]))),
            flip(fit(&imgs[0])),
            flip(fit(&imgs[2])),
        ];
        let cube = orient_sky(cube, size, orient);
        let irr = rustudio_render::lighting::sky_irradiance(&cube, size);
    Some((size, rustudio_render::render::cube_mips(size, &cube), irr))
}

/// Turns a sky cube (+X, −X, +Y, −Y, +Z, −Z faces, wgpu's layout) by
/// Sky.SkyboxOrientation (degrees), as Studio does: each texel toward d takes
/// the unturned sky toward R⁻¹·d, R = Rz·Rx·Ry (Roblox's axis rotations),
/// bilinear within a face. Quarter turns land on texel centres, so they are
/// an exact reshuffle. Done here once, so the drawn sky, its reflections and
/// its irradiance all turn together.
fn orient_sky(cube: [Vec<u8>; 6], size: u32, orient: Vec3) -> [Vec<u8>; 6] {
    if orient == Vec3::ZERO {
        return cube;
    }
    // Measured (docs/MECHANICS.md "Skybox orientation"): one axis at a time
    // turns the sky by that axis's Roblox rotation; (30, 60, 20) matches only
    // with Z·X·Y (Y·X·Z, CFrame.fromOrientation's order, is 50 levels off).
    let r = glam::Mat3::from_rotation_z(orient.z.to_radians())
        * glam::Mat3::from_rotation_x(orient.x.to_radians())
        * glam::Mat3::from_rotation_y(orient.y.to_radians());
    let inv = r.transpose();
    let s = size as usize;
    let fetch = |f: usize, x: usize, y: usize| -> [f32; 4] {
        let o = (y * s + x) * 4;
        let p = &cube[f][o..o + 4];
        [p[0] as f32, p[1] as f32, p[2] as f32, p[3] as f32]
    };
    let sample = |d: Vec3| -> [u8; 4] {
        let a = d.abs();
        // Direction → face and (u, v) in −1..1: the inverse of
        // lighting::sky_irradiance's texel → direction table.
        let (f, u, v) = if a.x >= a.y && a.x >= a.z {
            if d.x > 0.0 { (0, -d.z / a.x, -d.y / a.x) } else { (1, d.z / a.x, -d.y / a.x) }
        } else if a.y >= a.z {
            if d.y > 0.0 { (2, d.x / a.y, d.z / a.y) } else { (3, d.x / a.y, -d.z / a.y) }
        } else if d.z > 0.0 {
            (4, d.x / a.z, -d.y / a.z)
        } else {
            (5, -d.x / a.z, -d.y / a.z)
        };
        let fx = ((u + 1.0) * 0.5 * size as f32 - 0.5).clamp(0.0, size as f32 - 1.0);
        let fy = ((v + 1.0) * 0.5 * size as f32 - 0.5).clamp(0.0, size as f32 - 1.0);
        let (x0, y0) = (fx.floor() as usize, fy.floor() as usize);
        let (x1, y1) = ((x0 + 1).min(s - 1), (y0 + 1).min(s - 1));
        let (tx, ty) = (fx - x0 as f32, fy - y0 as f32);
        let (p00, p10, p01, p11) = (fetch(f, x0, y0), fetch(f, x1, y0), fetch(f, x0, y1), fetch(f, x1, y1));
        let mut out = [0u8; 4];
        for c in 0..4 {
            let top = p00[c] + (p10[c] - p00[c]) * tx;
            let bot = p01[c] + (p11[c] - p01[c]) * tx;
            out[c] = (top + (bot - top) * ty).round().clamp(0.0, 255.0) as u8;
        }
        out
    };
    std::array::from_fn(|f| {
        let mut px = vec![0u8; s * s * 4];
        for y in 0..s {
            for x in 0..s {
                let u = (x as f32 + 0.5) / size as f32 * 2.0 - 1.0;
                let v = (y as f32 + 0.5) / size as f32 * 2.0 - 1.0;
                let d = match f {
                    0 => Vec3::new(1.0, -v, -u),
                    1 => Vec3::new(-1.0, -v, u),
                    2 => Vec3::new(u, 1.0, v),
                    3 => Vec3::new(u, -1.0, -v),
                    4 => Vec3::new(u, -v, 1.0),
                    _ => Vec3::new(-u, -v, -1.0),
                };
                let o = (y * s + x) * 4;
                px[o..o + 4].copy_from_slice(&sample(inv * d));
            }
        }
        px
    })
}


pub fn wheel_notches(ctx: &egui::Context) -> f32 {
        // Raw wheel notches (not egui's smoothed scroll) so each click moves at once.
        ctx.input(|i| {
            i.raw
                .events
                .iter()
                .filter_map(|e| match e {
                    egui::Event::MouseWheel { unit, delta, .. } => Some(match unit {
                        egui::MouseWheelUnit::Line => delta.y,
                        egui::MouseWheelUnit::Point => delta.y / mech::WHEEL_POINTS_PER_NOTCH,
                        egui::MouseWheelUnit::Page => delta.y * 3.0,
                    }),
                    _ => None,
                })
                .sum()
        })

}

impl Game {
    pub fn player_viewport(&mut self, ui: &mut egui::Ui, started: std::time::Instant) {
        let rect = ui.available_rect_before_wrap();
        self.viewport_rect = rect;
        let ctx = ui.ctx().clone();
        rustudio_render::grass_temporal::tick(&mut self.cam, &ctx);
        let resp = ui.allocate_rect(rect, egui::Sense::click_and_drag());
        if self.play_game_input() {
            let delta = ctx.input(|i| i.pointer.delta());
            self.update_game_mouse(&ctx, &resp, delta, wheel_notches(&ctx));
        } else {
            self.release_game_mouse(&ctx);
        }
        {
            let _scope = rustudio_core::bench::scope("vp_scene_sync");
            self.scene.sync(&mut self.doc);
            self.vf_scene.set_viewport_frames(&self.doc, self.gui.viewport_frames());
            self.vf_scene.sync(&mut self.doc);
        }
        let painter = ui.painter_at(rect);
        self.sync_view_draws(&ctx);
        self.draw_world(&ctx, &painter, rect, Vec::new(), &[], started);
        let gui_took = self.draw_view_gui(&ctx, &painter, rect, &resp, true);
        self.render_viewport_frames(&Default::default());
        let dt = self.fixed_dt.unwrap_or_else(|| ctx.input(|i| i.stable_dt).min(0.1));
        self.finish_view_gui(&ctx, rect, dt);
        self.game_viewport(ui, rect, &resp, gui_took);
    }
}
