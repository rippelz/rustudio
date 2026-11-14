//! `RUSTUDIO_DRAG_PARITY_REGRESSION=1 rustudio --self-test OUT.json`: the
//! drags measured in Studio (part_drag::studio_cases) replayed through the
//! editor itself — viewport pick, selection, the box basis, model pivots,
//! R / T, undo and Join Surfaces — on a 862x675 view with Studio's camera.

use egui::{Pos2, Rect, Vec2};
use glam::{Mat3, Vec3};
use rbx_dom_weak::types::{Ref, Variant};
use rbx_dom_weak::{ustr, InstanceBuilder};

use crate::app::{Editor, Tool};
use crate::math::Xform;
use crate::part_drag::studio_cases::{scene, CASES};
use crate::render::MeshKind;

impl Editor {
    fn drag_parity_camera(&mut self) {
        self.cam = crate::camera::Camera::default();
        self.cam.pos = Vec3::new(0.0, 20.0, 20.0);
        let f = (Vec3::new(0.0, 2.0, 0.0) - self.cam.pos).normalize();
        let r = f.cross(Vec3::Y).normalize();
        self.cam.set_rotation(Mat3::from_cols(r, r.cross(f), -f));
        self.cam.fov_deg = 70.0;
    }

    /// Builds a case's place; returns name → Ref and what to select.
    fn drag_parity_place(&mut self, name: &str) -> (Vec<(&'static str, Ref)>, Vec<Ref>) {
        self.doc = crate::doc::Document::new_place();
        let ws = self.doc.workspace().unwrap();
        let old: Vec<Ref> = self.doc.dom.get_by_ref(ws).unwrap().children().to_vec();
        for c in old {
            if self.doc.class_of(c).is_some_and(|k| crate::doc::is_a(&k, "BasePart") && k != "Terrain") {
                self.doc.dom.destroy(c);
            }
        }
        let (solids, sel, pivot, _) = scene(name);
        let model = name.starts_with("model").then(|| {
            let mut b = InstanceBuilder::new("Model").with_name("M");
            if !name.starts_with("model_nopp") && !name.starts_with("model_primary") && !name.starts_with("model_tiltpp") {
                b = b.with_property("WorldPivotData", Variant::OptionalCFrame(Some(pivot.to_cframe())));
            }
            self.doc.dom.insert(ws, b)
        });
        let mut refs = Vec::new();
        for (n, s) in &solids {
            let shape = if s.kind == MeshKind::Wedge { 3 } else { 1 };
            let parent = if sel.contains(n) { model.unwrap_or(ws) } else { ws };
            let r = self.doc.dom.insert(parent, crate::doc::new_part(n, shape, s.xf.pos, s.size, [163, 162, 165]));
            self.doc.set_prop_live(r, ustr("CFrame"), Variant::CFrame(s.xf.to_cframe()));
            if *n == "Baseplate" {
                self.doc.set_prop_live(r, ustr("Locked"), Variant::Bool(true));
            }
            refs.push((*n, r));
        }
        if let Some(m) = model {
            if name.starts_with("model_primary") || name.starts_with("model_tiltpp") {
                let b = refs.iter().find(|(n, _)| *n == "B").unwrap().1;
                self.doc.set_prop_live(m, ustr("PrimaryPart"), Variant::Ref(b));
            }
            return (refs, vec![m]);
        }
        let select = refs.iter().filter(|(n, _)| sel.contains(n)).map(|(_, r)| *r).collect();
        (refs, select)
    }

    /// Runs every case; returns (cases, failures).
    pub(crate) fn regression_drag_parity(&mut self) -> (usize, Vec<String>) {
        let rect = Rect::from_min_size(Pos2::ZERO, Vec2::new(862.0, 675.0));
        let mut bad = Vec::new();
        for (name, press, release, grid, keys, expect) in CASES {
            self.drag_parity_camera();
            self.tool = Tool::Select;
            self.snap_move = grid.is_some();
            self.move_inc = grid.unwrap_or(1.0);
            self.join_surfaces = false;
            // (Measured with these off: see part_drag::studio_cases.)
            self.align_dragged = false;
            self.part_snap = false;
            self.use_bounding_boxes = false;
            self.physical_drag = false;
            let (refs, select) = self.drag_parity_place(name);
            self.scene = crate::scene::Scene::default();
            self.select(select);
            self.scene.sync(&mut self.doc);
            self.doc.clear_history();
            let before: Vec<_> = refs.iter().map(|(_, r)| self.doc.prop(*r, "CFrame")).collect();
            let (a, b) = (Pos2::new(press.0, press.1), Pos2::new(release.0, release.1));
            if !self.start_part_drag(rect, a, false) {
                bad.push(format!("{name}: no drag started"));
                continue;
            }
            for k in 1..=12 {
                self.update_part_drag(rect, a + (b - a) * (k as f32 / 12.0));
                self.scene.sync(&mut self.doc);
            }
            for c in keys.chars() {
                self.rotate_part_drag(c == 't');
                self.scene.sync(&mut self.doc);
            }
            self.finish_part_drag();
            let model = self.doc.selection.first().copied();
            for (part, want) in expect.iter() {
                let got = if *part == "M" {
                    model.and_then(|m| crate::part_drag::editor_model_pivot(&self.doc, m))
                } else {
                    refs.iter()
                        .find(|(n, _)| n == part)
                        .and_then(|(_, r)| match self.doc.prop(*r, "CFrame") {
                            Some(Variant::CFrame(cf)) => Some(Xform::from_cframe(&cf)),
                            _ => None,
                        })
                };
                let Some(xf) = got else {
                    bad.push(format!("{name}/{part}: missing"));
                    continue;
                };
                let pos_err = (xf.pos - Vec3::new(want[0], want[1], want[2])).abs().max_element();
                let mut rot_err = 0.0f32;
                for i in 0..3 {
                    for j in 0..3 {
                        rot_err = rot_err.max((xf.rot.col(j)[i] - want[3 + i * 3 + j]).abs());
                    }
                }
                if pos_err > 0.001 || rot_err > 2e-5 {
                    bad.push(format!("{name}/{part}: got {:?} pos err {pos_err} rot err {rot_err}", xf.pos));
                }
            }
            // One undo puts everything back.
            self.doc.undo();
            let after: Vec<_> = refs.iter().map(|(_, r)| self.doc.prop(*r, "CFrame")).collect();
            if before != after {
                bad.push(format!("{name}: undo didn't restore"));
            }
        }
        // Join Surfaces: the snapped baseplate drop welds Probe to Baseplate.
        self.drag_parity_camera();
        self.snap_move = true;
        self.move_inc = 1.0;
        self.join_surfaces = true;
        let (refs, select) = self.drag_parity_place("flat_snap1");
        self.scene = crate::scene::Scene::default();
        self.select(select);
        self.scene.sync(&mut self.doc);
        self.doc.clear_history();
        let probe = refs.iter().find(|(n, _)| *n == "Probe").unwrap().1;
        let base = refs.iter().find(|(n, _)| *n == "Baseplate").unwrap().1;
        self.start_part_drag(rect, Pos2::new(441.0, 319.0), false);
        self.update_part_drag(rect, Pos2::new(538.0, 298.0));
        self.finish_part_drag();
        self.join_surfaces = false;
        let welds: Vec<Ref> = self.doc.dom.get_by_ref(probe).unwrap().children().iter().copied()
            .filter(|c| self.doc.class_of(*c).is_some_and(|k| k == "Weld")).collect();
        let ok = welds.len() == 1
            && self.doc.prop(welds[0], "Part1") == Some(Variant::Ref(base))
            && matches!(self.doc.prop(welds[0], "C1"), Some(Variant::CFrame(cf)) if (crate::math::v3(cf.position) - Vec3::new(6.0, 2.0, -7.0)).length() < 1e-3);
        if !ok {
            bad.push(format!("join: {} welds", welds.len()));
        }
        self.doc.undo();
        if self.doc.dom.get_by_ref(probe).unwrap().children().iter().any(|c| self.doc.class_of(*c).is_some_and(|k| k == "Weld")) {
            bad.push("join: undo kept the weld".into());
        }
        let n = self.regression_dragger_modes(&mut bad);
        (CASES.len() + 1 + n, bad)
    }

    /// A mode_cases scene as a place; the Probe is returned selected.
    fn dragger_mode_place(&mut self, name: &str, anchored: bool) -> Vec<(&'static str, Ref)> {
        self.doc = crate::doc::Document::new_place();
        let ws = self.doc.workspace().unwrap();
        let old: Vec<Ref> = self.doc.dom.get_by_ref(ws).unwrap().children().to_vec();
        for c in old {
            if self.doc.class_of(c).is_some_and(|k| crate::doc::is_a(&k, "BasePart") && k != "Terrain") {
                self.doc.dom.destroy(c);
            }
        }
        let mut refs = Vec::new();
        for (n, s) in crate::part_drag::mode_cases::scene(name) {
            let shape = if s.kind == MeshKind::Wedge { 3 } else { 1 };
            let r = self.doc.dom.insert(ws, crate::doc::new_part(n, shape, s.xf.pos, s.size, [163, 162, 165]));
            self.doc.set_prop_live(r, ustr("CFrame"), Variant::CFrame(s.xf.to_cframe()));
            self.doc.set_prop_live(r, ustr("Anchored"), Variant::Bool(n != "Probe" || anchored));
            if n == "Baseplate" {
                self.doc.set_prop_live(r, ustr("Locked"), Variant::Bool(true));
            }
            refs.push((n, r));
        }
        self.scene = crate::scene::Scene::default();
        self.scene.sync(&mut self.doc);
        self.doc.clear_history();
        refs
    }

    /// Drags the Probe from `a` to `b` on the 1506x842 mode_cases view;
    /// returns its CFrame after.
    fn dragger_mode_drag(&mut self, refs: &[(&'static str, Ref)], a: Pos2, b: Pos2) -> Option<Xform> {
        let rect = Rect::from_min_size(Pos2::ZERO, Vec2::new(crate::part_drag::mode_cases::W, crate::part_drag::mode_cases::H));
        self.cam = crate::part_drag::mode_cases::camera();
        // Studio's measured mouse rays, as pixels of this view.
        let px = |p: Pos2| -> Pos2 {
            crate::part_drag::mode_cases::studio_ray(p.x, p.y)
                .and_then(|d| self.cam.project(rect, self.cam.pos + d * 10.0))
                .unwrap_or(p)
        };
        let (a, b) = (px(a), px(b));
        self.tool = Tool::Select;
        let probe = refs.iter().find(|(n, _)| *n == "Probe")?.1;
        self.select(vec![probe]);
        if !self.start_part_drag(rect, a, false) {
            return None;
        }
        for k in 1..=20 {
            self.update_part_drag(rect, a + (b - a) * (k as f32 / 20.0));
            self.scene.sync(&mut self.doc);
        }
        self.finish_part_drag();
        match self.doc.prop(probe, "CFrame") {
            Some(Variant::CFrame(cf)) => Some(Xform::from_cframe(&cf)),
            _ => None,
        }
    }

    /// The dragger settings menu and Align Dragged Objects against the
    /// drags measured in Studio (part_drag::mode_cases; Physical mode with
    /// Studio's IK results, which lag the camera-plane target by ~2 %).
    fn regression_dragger_modes(&mut self, bad: &mut Vec<String>) -> usize {
        let mut n = 0;
        self.physical_drag = false;
        self.join_surfaces = false;
        for (name, press, release, grid, align, bbox, snap, want) in crate::part_drag::mode_cases::CASES {
            n += 1;
            self.snap_move = grid.is_some();
            self.move_inc = grid.unwrap_or(1.0);
            self.align_dragged = *align;
            self.use_bounding_boxes = *bbox;
            self.part_snap = *snap;
            // Collisions don't change a geometric free drag (target_overlap).
            self.collisions = *name == "target_overlap";
            let refs = self.dragger_mode_place(name, true);
            let Some(xf) = self.dragger_mode_drag(&refs, Pos2::new(press.0, press.1), Pos2::new(release.0, release.1)) else {
                bad.push(format!("modes/{name}: no drag"));
                continue;
            };
            let pos_err = (xf.pos - Vec3::new(want[0], want[1], want[2])).abs().max_element();
            let mut rot_err = 0.0f32;
            for i in 0..3 {
                for j in 0..3 {
                    rot_err = rot_err.max((xf.rot.col(j)[i] - want[3 + i * 3 + j]).abs());
                }
            }
            if pos_err > 0.002 || rot_err > 2e-5 {
                bad.push(format!("modes/{name}: got {:?} pos err {pos_err} rot err {rot_err}", xf.pos));
            }
        }
        // Physical: (scene, anchored, collisions, release, Studio's result, tolerance).
        self.snap_move = false;
        self.part_snap = true;
        self.align_dragged = true;
        self.use_bounding_boxes = false;
        self.physical_drag = true;
        let phys: [(&str, bool, bool, (f32, f32), [f32; 3], f32); 4] = [
            // Lifted off the ground in the camera plane (Studio IK: −2.104, 5.697, −2.957).
            ("plain", false, false, (700.0, 380.0), [-2.1445, 5.7915, -3.0332], 0.12),
            // Anchored: Studio's physical dragger doesn't move it.
            ("plain", true, false, (700.0, 380.0), [0.0, 2.0, 0.0], 1e-4),
            // Through the wall without Collisions (Studio: 7.809, 2, 0).
            ("wall", false, false, (951.0, 500.0), [8.01, 2.0, 0.0], 0.25),
            // Stopped by it with Collisions (Studio: 3.024, 1.990, 0.990).
            ("wall", false, true, (951.0, 500.0), [3.0, 2.0, 0.0], 0.05),
        ];
        for (scene, anchored, collisions, release, want, tol) in phys {
            n += 1;
            self.collisions = collisions;
            let refs = self.dragger_mode_place(scene, anchored);
            let got = self.dragger_mode_drag(&refs, Pos2::new(753.0, 500.0), Pos2::new(release.0, release.1));
            let label = format!("physical/{scene}{}{}", if anchored { "/anchored" } else { "" }, if collisions { "/collisions" } else { "" });
            match got {
                Some(xf) => {
                    let err = (xf.pos - Vec3::from(want)).abs().max_element();
                    if err > tol {
                        bad.push(format!("{label}: got {:?}, want {want:?} ±{tol}", xf.pos));
                    }
                }
                None => bad.push(format!("{label}: no drag")),
            }
        }
        // Back to the defaults.
        self.physical_drag = false;
        self.collisions = false;
        self.align_dragged = crate::mech::ALIGN_DRAGGED_BY_DEFAULT;
        self.part_snap = crate::mech::PART_SNAP_ON_BY_DEFAULT;
        n
    }
}
