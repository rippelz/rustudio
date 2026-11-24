//! The GPU tab: each timed pass's mean and worst time per GPU frame over
//! the selection, its share of the GPU's timed work, and its history.
//! Timestamp queries run while the profiler records.

use egui::{Align, Align2, Layout, Pos2, Rect, Sense, Ui, Vec2};
use rustudio_core::profiler::{analysis, Capture, Cat, FrameKind};

use crate::theme::*;
use crate::{display_name, ProfilerWindow, Sel};

/// "gpu: scene (opaque, terrain, …)" → "Scene (opaque, terrain, …)".
fn pass_title(name: &str) -> String {
    let words: Vec<String> = display_name(name)
        .split(' ')
        .map(|w| match w {
            "ssao" => "SSAO".to_string(),
            "gui" => "GUI".to_string(),
            "ui" => "UI".to_string(),
            "egui" => "egui".to_string(),
            w => w.to_string(),
        })
        .collect();
    let n = words.join(" ");
    let mut c = n.chars();
    match c.next() {
        Some(f) if f.is_lowercase() && !n.starts_with("egui") => f.to_uppercase().collect::<String>() + c.as_str(),
        _ => n,
    }
}

pub(crate) fn show(w: &mut ProfilerWindow, ui: &mut Ui, cap: &Capture) {
    let r = ui.max_rect();
    // the selection, or the last two seconds of GPU frames
    let (t0, t1) = match w.sel {
        Sel::Range(a, b) => (a, b),
        Sel::Frame(_) => w.sel_range(cap),
        Sel::Latest => {
            let f = cap.frames_of(FrameKind::Gpu);
            match (f.len().checked_sub(120.min(f.len())), f.back()) {
                (Some(i), Some(l)) if !f.is_empty() => (f[i].start, l.end),
                _ => cap.time_range().unwrap_or((0, 1)),
            }
        }
    };
    let (passes, per_frame) = analysis::gpu_passes(cap, t0, t1);
    let bar = Rect::from_min_size(r.left_top(), Vec2::new(r.width(), SUBBAR_H));
    hline(ui, bar.left(), bar.right(), bar.bottom() - 1.0, LINE2);
    {
        let mut c = crate::region(ui, bar.shrink2(Vec2::new(10.0, 0.0)), "gpu-bar");
        c.with_layout(Layout::left_to_right(Align::Center), |ui| {
            ui.spacing_mut().item_spacing.x = 6.0;
            ui.label(egui::RichText::new(format!("GPU frame {:.1} ms", per_frame as f64 / 1e6)).font(strong(13.0, ui)).color(WHITE));
            let gpu = if cap.meta.gpu.is_empty() { "GPU unknown".to_string() } else { cap.meta.gpu.clone() };
            let frames = analysis::frames_in(cap, FrameKind::Gpu, t0, t1).count();
            let hint = "Results lag 2 frames (timestamp queries)";
            let hint_w = text_width(ui, hint, &body()) + 20.0;
            let info = format!("· {gpu} · {frames} GPU frames");
            let room = ui.available_width();
            let show_hint = room > text_width(ui, &info, &body()) + hint_w;
            let info_w = (room - if show_hint { hint_w } else { 0.0 }).max(40.0);
            ui.allocate_ui_with_layout(Vec2::new(info_w, 20.0), Layout::left_to_right(Align::Center), |ui| {
                ui.set_width(info_w);
                ui.add(egui::Label::new(egui::RichText::new(info).font(body()).color(MUTED)).truncate());
            });
            if show_hint {
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    ui.label(egui::RichText::new(hint).font(body()).color(MUTED));
                });
            }
        });
    }
    let body = Rect::from_min_max(Pos2::new(r.left(), bar.bottom()), r.right_bottom());
    if passes.is_empty() {
        let msg = if w.cfg.gpu_timing {
            "No GPU timings in this range yet (the adapter needs timestamp queries inside encoders)"
        } else {
            "GPU timing is off (Customize > Display)"
        };
        ui.painter().text(body.center(), Align2::CENTER_CENTER, msg, self::body_font(), FAINT);
        return;
    }
    // (the share column takes what the window can spare; history goes first)
    let share_w = (body.width() - 240.0 - 110.0 - 150.0 - 150.0).clamp(150.0, 600.0);
    let hist_w = if body.width() >= 240.0 + 110.0 + 150.0 + share_w + 150.0 { 150.0 } else { 0.0 };
    let cols = [("Pass", 0.0), ("Avg ms", 110.0), ("Max ms", 150.0), ("Share of GPU frame", share_w), ("History", hist_w)];
    let fixed: f32 = cols.iter().map(|c| c.1).sum();
    let name_w = (body.width() - fixed).max(240.0);
    let head = Rect::from_min_size(body.left_top(), Vec2::new(body.width(), 30.0));
    fill(ui, head, PANEL);
    hline(ui, head.left(), head.right(), head.bottom() - 1.0, LINE);
    let hf = egui::FontId::proportional(11.5);
    let p = ui.painter();
    let mut x = head.left();
    for (i, (t, wd)) in cols.iter().enumerate() {
        let wd = if i == 0 { name_w } else { *wd };
        if wd <= 0.0 {
            continue;
        }
        match i {
            1 | 2 => p.text(Pos2::new(x + wd - 10.0, head.center().y), Align2::RIGHT_CENTER, *t, hf.clone(), MUTED),
            _ => p.text(Pos2::new(x + 10.0, head.center().y), Align2::LEFT_CENTER, *t, hf.clone(), MUTED),
        };
        x += wd;
    }
    let rh = w.prefs.style.row_height() + 6.0;
    let color = w.prefs.style.cat_color(Cat::Gpu);
    let mut inner = crate::region(ui, Rect::from_min_max(Pos2::new(body.left(), head.bottom()), body.right_bottom()), "gpu-rows");
    egui::ScrollArea::vertical().auto_shrink([false, false]).show(&mut inner, |ui| {
        for pass in &passes {
            let (rr, resp) = ui.allocate_exact_size(Vec2::new(body.width(), rh), Sense::hover());
            if resp.hovered() {
                ui.painter().rect_filled(rr, 0.0, PANEL.gamma_multiply(0.6));
            }
            hline(ui, rr.left(), rr.right(), rr.bottom() - 1.0, LINE2);
            let p = ui.painter();
            let cy = rr.center().y;
            swatch(ui, Pos2::new(rr.left() + 10.0, cy - 5.0), color);
            p.text(Pos2::new(rr.left() + 25.0, cy), Align2::LEFT_CENTER, pass_title(cap.name(pass.name)), body_font(), TEXT);
            let mut x = rr.left() + name_w;
            let off = pass.frames_present == 0;
            let avg = if off { "off".to_string() } else { format!("{:.2}", pass.avg_ns as f64 / 1e6) };
            p.text(Pos2::new(x + 110.0 - 10.0, cy), Align2::RIGHT_CENTER, avg, mono(), if off { FAINT } else { TEXT });
            x += 110.0;
            p.text(Pos2::new(x + 150.0 - 10.0, cy), Align2::RIGHT_CENTER, format!("{:.2}", pass.max_ns as f64 / 1e6), mono(), TEXT);
            x += 150.0;
            let share = pass.share as f32;
            let bw = share * (share_w - 80.0).max(40.0);
            p.rect_filled(Rect::from_min_size(Pos2::new(x + 10.0, cy - 3.0), Vec2::new(bw.max(2.0), 6.0)), 3.0, color);
            p.text(Pos2::new(x + 16.0 + bw, cy), Align2::LEFT_CENTER, format!("{:.1} %", share * 100.0), mono(), TEXT);
            x += share_w;
            // (the newest GPU frame is usually still incomplete: left out)
            let n = pass.history.len().saturating_sub(1).max(1).min(pass.history.len());
            let hist: Vec<f64> = pass.history[..n].iter().rev().take(60).rev().map(|v| *v as f64).collect();
            if hist_w > 0.0 {
                sparkline(p, Rect::from_min_size(Pos2::new(x + 10.0, rr.top() + 7.0), Vec2::new(120.0, rh - 14.0)), &hist, color);
            }
            let tip = format!(
                "{}\nmean {:.3} ms · worst {:.3} ms · in {} of {} GPU frames",
                pass_title(cap.name(pass.name)),
                pass.avg_ns as f64 / 1e6,
                pass.max_ns as f64 / 1e6,
                pass.frames_present,
                pass.history.len()
            );
            resp.on_hover_text(tip);
        }
    });
}

fn body_font() -> egui::FontId {
    body()
}
