use crate::app::{Editor, Tool};
use crate::viewport::Handle;
use egui::{Pos2, Rect, Vec2};
use glam::Vec3;

impl Editor {
    /// Headless integration regression: real drag updates and Duplicate action.
    pub(crate) fn regression_duplicate_during_drag(&mut self) -> usize {
        let rect = Rect::from_min_size(Pos2::ZERO, Vec2::new(1000.0, 700.0));
        let mut cases = 0;
        for tool in [Tool::Select, Tool::Move, Tool::Scale, Tool::Rotate] {
            for model in [false, true] {
              for count in [1, 2] {
                self.game.doc = rustudio_core::doc::Document::new_place();
                self.game.scene = rustudio_render::scene::Scene::default();
                self.game.cam = rustudio_render::camera::Camera::default();
                self.game.cam.pos = Vec3::new(0.0, 20.0, 30.0);
                self.game.cam.yaw = 0.0;
                self.game.cam.pitch = -0.5;
                self.game.snap_move = false;
                self.game.snap_rotate = false;
                self.tool = tool;
                let parent = self.game.doc.workspace().unwrap();
                let mut selected = Vec::new();
                let mut parts = Vec::new();
                for i in 0..count {
                    let x = i as f32 * 6.0;
                    let root = if model { self.game.doc.insert(parent, rbx_dom_weak::InstanceBuilder::new("Model")) } else { parent };
                    let r = self.game.doc.insert(root, rustudio_core::doc::new_part("DragRegression", 1,
                        Vec3::new(x, 3.0, 0.0), Vec3::splat(2.0), [160, 160, 160]));
                    parts.push(r);
                    selected.push(if model { root } else { r });
                }
                self.select(selected);
                self.game.scene.sync(&mut self.game.doc);
                let initial_parts = parts.clone();
                let initial_props: Vec<_> = parts.iter().map(|r| self.game.doc.dom.get_by_ref(*r).unwrap().properties.clone()).collect();
                self.game.doc.clear_history();
                let p = self.game.cam.project(rect, Vec3::new(0.0, 3.0, 0.0)).unwrap();
                if tool == Tool::Select {
                    assert!(self.start_part_drag(rect, p, false));
                } else {
                    self.start_drag(rect, Handle { tool, axis: 0, sign: 1.0 }, p);
                    assert!(self.drag.is_some());
                }
                let mut last_baseline = Vec::new();
                for repeat in 0..3 {
                    let q = p + Vec2::new(35.0 + repeat as f32 * 50.0, -20.0 - repeat as f32 * 25.0);
                    if tool == Tool::Select { self.update_part_drag(rect, q); }
                    else { self.update_drag(rect, q, false, false); }
                    let originals = parts.clone();
                    let frozen: Vec<_> = originals.iter().map(|r| self.game.doc.dom.get_by_ref(*r).unwrap().properties.clone()).collect();
                    if std::env::var_os("RUSTUDIO_DUPLICATE_DRAG_BEFORE").is_some() {
                        // Reproduce the original action, which only replaced selection.
                        let refs = self.game.doc.duplicate(&self.game.doc.selection.clone());
                        self.select(refs);
                    } else {
                        self.duplicate();
                    }
                    self.game.scene.sync(&mut self.game.doc);
                    parts = self.selected_parts();
                    assert_eq!(parts.len(), count, "multi-selection/model descendants");
                    assert!(parts.iter().all(|r| !originals.contains(r)));
                    let copy_before: Vec<_> = parts.iter().map(|r| self.game.doc.dom.get_by_ref(*r).unwrap().properties.clone()).collect();
                    last_baseline = copy_before.clone();
                    let next = q + Vec2::new(30.0, -35.0);
                    if tool == Tool::Select { self.update_part_drag(rect, next); }
                    else { self.update_drag(rect, next, false, false); }
                    for (i, old) in originals.iter().enumerate() {
                        assert_eq!(self.game.doc.dom.get_by_ref(*old).unwrap().properties, frozen[i], "unselected original moved: {tool:?}");
                        assert_ne!(self.game.doc.dom.get_by_ref(parts[i]).unwrap().properties, copy_before[i], "selected copy stayed still: {tool:?}");
                    }
                    cases += 1;
                }
                self.finish_part_drag();
                self.finish_drag();
                assert_eq!(self.game.doc.undo_len(), 7, "three duplicates and four motion segments: {tool:?}");
                assert!(self.game.doc.undo().is_some());
                for (r, baseline) in parts.iter().zip(&last_baseline) {
                    assert_eq!(&self.game.doc.dom.get_by_ref(*r).unwrap().properties, baseline,
                        "undo must return copies to duplication point: {tool:?}");
                }
                assert_eq!(self.game.doc.undo().as_deref(), Some("Duplicate"));
                assert!(parts.iter().all(|r| !self.game.doc.exists(*r)));
                while self.game.doc.undo().is_some() {}
                for (r, props) in initial_parts.iter().zip(&initial_props) {
                    assert_eq!(&self.game.doc.dom.get_by_ref(*r).unwrap().properties, props, "undo original motion: {tool:?}");
                }
              }
            }
        }
        cases
    }

}
