use egui::{Event, MouseWheelUnit, Pos2, Rect, TouchPhase, Vec2, pos2, vec2};

fn menu_frame(
    ctx: &egui::Context,
    height: f32,
    anchor: Pos2,
    rows: usize,
    events: Vec<Event>,
) -> (Rect, Rect, Rect) {
    let mut result = None;
    let input = egui::RawInput {
        screen_rect: Some(Rect::from_min_size(Pos2::ZERO, vec2(1024.0, height))),
        events,
        ..Default::default()
    };
    let mut output = ctx.run_ui(input, |ui| {
        let menu = egui::Popup::new(egui::Id::new("menu"), ctx.clone(), anchor, ui.layer_id())
            .kind(egui::PopupKind::Menu)
            .width(180.0)
            .show(|ui| {
                let mut last = Rect::NOTHING;
                for _ in 0..rows {
                    last = ui
                        .allocate_exact_size(vec2(180.0, 22.0), egui::Sense::click())
                        .0;
                }
                (last, ui.clip_rect())
            })
            .unwrap();
        result = Some((menu.response.rect, menu.inner.0, menu.inner.1));
    });
    output.textures_delta.clear();
    result.unwrap()
}

#[test]
fn short_menus_keep_their_anchor_or_move_to_fit() {
    for anchor in [pos2(12.0, 20.0), pos2(12.0, 575.0)] {
        let ctx = egui::Context::default();
        let _ = menu_frame(&ctx, 600.0, anchor, 8, vec![]);
        let (menu, last, clip) = menu_frame(&ctx, 600.0, anchor, 8, vec![]);
        assert!(menu.top() >= 0.0 && menu.bottom() <= 600.0, "{menu:?}");
        assert!(clip.contains_rect(last), "{last:?} {clip:?}");
        if anchor.y == 20.0 {
            assert_eq!(menu.min, anchor);
        } else {
            assert!(menu.bottom() <= anchor.y);
        }
    }
}

#[test]
fn tall_menus_scroll_to_the_final_item() {
    for height in [600.0, 768.0, 1080.0] {
        let ctx = egui::Context::default();
        let anchor = pos2(12.0, 20.0);
        let _ = menu_frame(&ctx, height, anchor, 40, vec![]);
        let (menu, last, clip) = menu_frame(&ctx, height, anchor, 40, vec![]);
        assert!(menu.top() >= 0.0 && menu.bottom() <= height, "{menu:?}");
        if height == 1080.0 {
            assert_eq!(menu.min, anchor);
            assert!(clip.contains_rect(last));
            continue;
        }
        assert!(last.bottom() > clip.bottom());
        let _ = menu_frame(
            &ctx,
            height,
            anchor,
            40,
            vec![Event::PointerMoved(menu.center())],
        );
        let _ = menu_frame(
            &ctx,
            height,
            anchor,
            40,
            vec![Event::MouseWheel {
                unit: MouseWheelUnit::Point,
                delta: Vec2::new(0.0, -10000.0),
                modifiers: egui::Modifiers::NONE,
                phase: TouchPhase::Move,
            }],
        );
        let (_, last, clip) = menu_frame(&ctx, height, anchor, 40, vec![]);
        assert!(clip.contains_rect(last), "{last:?} {clip:?}");
    }
}
