//! The detail page's 3D preview: the asset's model is downloaded and
//! decoded on a worker, then put into an orphan ViewportFrame in the open
//! document (Parent nil: not in the DataModel, never saved, never in
//! Explorer or Play, as plugin-owned instances are) and drawn by the 3D
//! view's own ViewportFrame renderer (vf_render.rs) into a texture the
//! page shows, with its own orbit camera (drag to turn, wheel to zoom).
//! Meshes, textures and materials load through the normal asset pipeline.

use std::collections::HashMap;
use std::time::Instant;

use glam::{Mat3, Vec3};
use rbx_dom_weak::types::Ref;
use rbx_dom_weak::{InstanceBuilder, WeakDom};

use super::api::Failure;
use rustudio_core::math::Xform;

/// One row of the instance tree view.
#[derive(Debug, Clone, PartialEq)]
pub struct TreeRow {
    pub depth: usize,
    pub class: String,
    pub name: String,
}

pub enum PState {
    Loading,
    Loaded(Box<WeakDom>),
    Installed { vf: Ref, center: Vec3, radius: f32 },
    Failed(String),
}

#[derive(Debug, Clone, Copy)]
pub struct Orbit {
    pub yaw: f32,
    pub pitch: f32,
    pub zoom: f32,
}

impl Default for Orbit {
    fn default() -> Self {
        // Studio's preview starts looking at the model's front, a little from above.
        Orbit { yaw: 0.6, pitch: -0.35, zoom: 1.0 }
    }
}

#[derive(Default)]
pub struct Previews {
    pub state: HashMap<u64, PState>,
    pub trees: HashMap<u64, Vec<TreeRow>>,
    pub orbit: Orbit,
    /// Set by the page each frame it shows a 3D preview: (asset, pixel size).
    pub shown: Option<(u64, [u32; 2])>,
    last_shown: Option<Instant>,
    /// The texture the renderer drew the shown preview into.
    pub texture: Option<(u64, egui::TextureId, [u32; 2])>,
    doc_id: Option<u64>,
    /// Frames to destroy once the renderer let go of them.
    dying: Vec<(Ref, u32)>,
}

impl Previews {
    pub fn wants(&self, id: u64) -> bool {
        !self.state.contains_key(&id)
    }

    pub fn start(&mut self, id: u64) {
        self.state.insert(id, PState::Loading);
    }

    pub fn loaded(&mut self, id: u64, r: Result<WeakDom, Failure>) {
        match r {
            Ok(dom) => {
                self.trees.insert(id, tree_rows(&dom));
                self.state.insert(id, PState::Loaded(Box::new(dom)));
            }
            Err(e) => {
                self.state.insert(id, PState::Failed(e.describe()));
            }
        }
    }

    pub fn show(&mut self, id: u64, size: [u32; 2]) {
        self.shown = Some((id, size));
        self.last_shown = Some(Instant::now());
    }

    /// Editor side, once a frame before the 3D view draws: installs a
    /// loaded model into the document, forgets frames of a closed page,
    /// and returns the frames to draw with their requests.
    pub fn frames(&mut self, doc: &mut rustudio_core::doc::Document) -> HashMap<Ref, rustudio_render::render::vf::Request> {
        if self.doc_id != Some(doc.id) {
            // A new document: the old one's frames went with it.
            self.doc_id = Some(doc.id);
            self.dying.clear();
            for s in self.state.values_mut() {
                if matches!(s, PState::Installed { .. }) {
                    *s = PState::Failed("Reopen the page to see the preview.".into());
                }
            }
            self.state.retain(|_, s| !matches!(s, PState::Failed(_)));
        }
        // Frames no longer shown for a moment: destroyed (two frames after the
        // renderer stopped asking for them, so the scene dropped their parts).
        self.dying.retain_mut(|(r, n)| {
            *n += 1;
            if *n > 2 {
                if doc.dom.get_by_ref(*r).is_some() {
                    doc.dom.destroy(*r);
                }
                false
            } else {
                true
            }
        });
        let stale = self.last_shown.is_none_or(|t| t.elapsed().as_secs_f32() > 0.5);
        if stale {
            self.shown = None;
        }
        let shown = self.shown.map(|s| s.0);
        let ids: Vec<u64> = self.state.keys().copied().collect();
        for id in ids {
            if Some(id) == shown {
                continue;
            }
            if let Some(PState::Installed { vf, .. }) = self.state.get(&id) {
                self.dying.push((*vf, 0));
                self.state.remove(&id);
            }
        }
        let mut out = HashMap::new();
        let Some((id, size)) = self.shown else { return out };
        if let Some(PState::Loaded(_)) = self.state.get(&id) {
            let Some(PState::Loaded(dom)) = self.state.remove(&id) else { return out };
            let mut dom = *dom;
            let vf = doc.dom.insert(Ref::none(), InstanceBuilder::new("ViewportFrame").with_name("ToolboxPreview"));
            let roots = dom.root().children().to_vec();
            for r in roots {
                dom.transfer(r, &mut doc.dom, vf);
            }
            let (center, radius) = match super::insert::aabb(&doc.dom, &[vf]) {
                Some((lo, hi)) => ((lo + hi) * 0.5, ((hi - lo).length() * 0.5).max(0.5)),
                None => (Vec3::ZERO, 2.0),
            };
            self.orbit = Orbit::default();
            self.state.insert(id, PState::Installed { vf, center, radius });
        }
        if let Some(PState::Installed { vf, center, radius }) = self.state.get(&id) {
            let fov = 70f32.to_radians();
            let dist = radius / (fov * 0.5).tan() * 1.05 * self.orbit.zoom;
            let rot = Mat3::from_rotation_y(self.orbit.yaw) * Mat3::from_rotation_x(self.orbit.pitch);
            let pos = *center + rot * Vec3::new(0.0, 0.0, dist);
            let fwd = (*center - pos).normalize_or_zero();
            let right = fwd.cross(Vec3::Y).normalize_or_zero();
            let up = right.cross(fwd);
            let cam = Xform { pos, rot: Mat3::from_cols(right, up, -fwd) };
            out.insert(
                *vf,
                rustudio_render::render::vf::Request {
                    size: [size[0].max(16), size[1].max(16)],
                    cam,
                    fov_y: fov,
                    ambient: [0.62, 0.62, 0.64],
                    light: [0.62, 0.62, 0.62],
                    light_dir: Vec3::new(-0.4, -1.0, -0.6).normalize(),
                    background: [0.0, 0.0, 0.0, 0.0],
                },
            );
        }
        out
    }

    /// After the renderer drew: remember the shown frame's texture.
    pub fn set_texture(&mut self, tex: impl Fn(Ref) -> Option<(egui::TextureId, [u32; 2])>) {
        self.texture = None;
        let Some((id, _)) = self.shown else { return };
        if let Some(PState::Installed { vf, .. }) = self.state.get(&id) {
            if let Some((t, s)) = tex(*vf) {
                self.texture = Some((id, t, s));
            }
        }
    }
}

/// The model's instances, depth first (the detail page's tree view).
pub fn tree_rows(dom: &WeakDom) -> Vec<TreeRow> {
    let mut out = Vec::new();
    let mut stack: Vec<(Ref, usize)> = dom.root().children().iter().rev().map(|r| (*r, 0)).collect();
    while let Some((r, d)) = stack.pop() {
        let Some(i) = dom.get_by_ref(r) else { continue };
        out.push(TreeRow { depth: d, class: i.class.to_string(), name: i.name.clone() });
        if out.len() >= 2000 {
            break;
        }
        for c in i.children().iter().rev() {
            stack.push((*c, d + 1));
        }
    }
    out
}
