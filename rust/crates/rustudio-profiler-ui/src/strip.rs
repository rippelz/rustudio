//! The frame strip: every display frame in the ring as a bar stacked by
//! category, budget and spike lines, spikes marked red. Click selects a
//! frame, drag selects a range, right-click goes back to following the
//! newest frame.

use egui::{Align2, Pos2, Rect, Sense, Stroke, Ui, Vec2};
use rustudio_core::profiler::{analysis, Capture, Cat, FrameRec};

use crate::theme::*;
use crate::{ProfilerWindow, Sel};

/// Stacking order, bottom up (Wait on top, as idle time).
const STACK: [Cat; 15] = [
    Cat::Script,
    Cat::Render,
    Cat::Physics,
    Cat::Ui,
    Cat::Gui,
    Cat::Animation,
    Cat::Network,
    Cat::Assets,
    Cat::Audio,
    Cat::Input,
    Cat::Pathfinding,
    Cat::Gpu,
    Cat::Other,
    Cat::Frame,
    Cat::Wait,
];

/// A readable scale above `v` ms.
fn nice_max(v: f64) -> f64 {
    for s in [20.0, 25.0, 40.0, 50.0, 60.0, 80.0, 100.0, 150.0, 200.0, 300.0, 500.0, 1000.0, 2000.0, 5000.0] {
        if v <= s {
            return s;
        }
    }
    10_000.0
}

pub(crate) fn breakdown_of(w: &mut ProfilerWindow, cap: &Capture, i: usize, f: FrameRec) -> [f32; Cat::ALL.len()] {
    let n = cap.render_frames().len();
    if let Some(b) = w.breakdown.get(&f.start) {
        return *b;
    }
    let mut out = [0f32; Cat::ALL.len()];
    if let Some(main) = analysis::main_track(cap) {
        let by = analysis::category_breakdown(main, f.start, f.end);
        for (o, v) in out.iter_mut().zip(by) {
            *o = v as f32 / 1e6;
        }
    }
    // (the newest frames may still get spans from late lanes: not cached)
    if i + 2 < n {
        w.breakdown.insert(f.start, out);
    }
    out
}

pub(crate) fn show(w: &mut ProfilerWindow, ui: &mut Ui, cap: &Capture, live: bool) {
    let r = ui.max_rect();
    fill(ui, r, DEEP);
    hline(ui, r.left(), r.right(), r.bottom() - 1.0, LINE);
    let chart = Rect::from_min_max(Pos2::new(r.left() + 46.0, r.top() + 6.0), Pos2::new(r.right() - 10.0, r.bottom() - 16.0));
    let frames = cap.render_frames();
    let n = frames.len();
    let p = ui.painter().clone();
    let foot_y = chart.bottom() + 9.0;
    if n == 0 {
        p.text(chart.center(), Align2::CENTER_CENTER, if live { "Waiting for frames…" } else { "This capture has no display frames" }, small(), FAINT);
        return;
    }
    // scale from the slow end of the ring
    // (the scale changes slowly: re-sorted at most 4 times a second)
    let p99 = match w.tl.strip_p99 {
        Some((at, n0, v)) if at.elapsed().as_millis() < 250 && (live || n0 == n) => v,
        _ => {
            let mut durs: Vec<u64> = frames.iter().map(|f| f.dur()).collect();
            durs.sort_unstable();
            let v = analysis::quantile_sorted(&durs, 0.99) as f64 / 1e6;
            w.tl.strip_p99 = Some((std::time::Instant::now(), n, v));
            v
        }
    };
    let max_ms = nice_max((w.cfg.spike_ms as f64 * 1.4).max(p99 * 1.1));
    let y_of = |ms: f64| chart.bottom() - (ms / max_ms).min(1.0) as f32 * chart.height();

    // columns: one per frame when they are at least 3 px wide, else the
    // slowest frame of each group
    let per = ((n as f32 * 3.0) / chart.width()).ceil().max(1.0) as usize;
    let cols = n.div_ceil(per);
    let bw = chart.width() / cols as f32;
    let gap = if bw >= 3.0 { 1.0 } else { 0.0 };
    let sel_range = match w.sel {
        Sel::Range(a, b) => Some((a, b)),
        _ => None,
    };
    let sel_frame = w.sel_frame(cap).map(|x| x.1.start);
    let mut sel_col = None;
    let spike_ns = (w.cfg.spike_ms as f64 * 1e6) as u64;
    for c in 0..cols {
        let i0 = c * per;
        let i1 = ((c + 1) * per).min(n);
        let (i, f) = (i0..i1).map(|i| (i, frames[i])).max_by_key(|x| x.1.dur()).unwrap_or((i0, frames[i0]));
        if (i0..i1).any(|j| Some(frames[j].start) == sel_frame) {
            sel_col = Some(c);
        }
        let b = breakdown_of(w, cap, i, f);
        let x = chart.left() + c as f32 * bw;
        let dim = sel_range.is_some_and(|(a, z)| f.end <= a || f.start >= z);
        let alpha = if dim { 0.35 } else { 0.92 };
        let mut acc = 0.0f64;
        for cat in STACK {
            if !w.prefs.cat_visible(cat) {
                continue;
            }
            let v = b[cat as usize] as f64;
            if v <= 0.0 {
                continue;
            }
            let y0 = y_of(acc);
            acc += v;
            let y1 = y_of(acc);
            p.rect_filled(Rect::from_min_max(Pos2::new(x, y1), Pos2::new(x + (bw - gap).max(1.0), y0)), 0.0, w.prefs.style.cat_color(cat).gamma_multiply(alpha));
        }
        if f.dur() > spike_ns {
            p.rect_filled(Rect::from_min_size(Pos2::new(x, chart.top()), Vec2::new((bw - gap).max(1.0), 3.0)), 0.0, BAD);
        }
    }
    // budget and spike lines, axis
    let budget = w.cfg.budget_ms as f64;
    let spike = w.cfg.spike_ms as f64;
    dashed_h(&p, chart.left(), chart.right(), y_of(spike), BAD.gamma_multiply(0.55));
    dashed_h(&p, chart.left(), chart.right(), y_of(budget), MUTED.gamma_multiply(0.55));
    // axis labels, bottom up; one too close to a drawn one is left out
    let mut drawn: Vec<f32> = Vec::new();
    let mut label = |y: f32, text: String| {
        if drawn.iter().all(|d| (d - y).abs() >= 11.0) {
            p.text(Pos2::new(r.left() + 6.0, y), Align2::LEFT_CENTER, text, tiny(), FAINT);
            drawn.push(y);
        }
    };
    label(chart.bottom(), "0 ms".into());
    label(y_of(budget), format!("{budget:.1}"));
    label(y_of(spike), format!("{spike:.1}"));
    if max_ms > spike * 1.6 {
        label(chart.top() + 4.0, format!("{max_ms:.0}"));
    }
    // selection
    let col_of_time = |t: u64| -> f32 {
        let i = frames.partition_point(|f| f.start < t).min(n - 1);
        chart.left() + (i / per) as f32 * bw
    };
    if let Some((a, b)) = sel_range {
        let x0 = col_of_time(a);
        let x1 = col_of_time(b.saturating_sub(1)) + bw;
        p.rect_filled(Rect::from_min_max(Pos2::new(x0, chart.top()), Pos2::new(x1, chart.bottom())), 0.0, ACCENT.gamma_multiply(0.10));
        p.rect_filled(Rect::from_min_size(Pos2::new(x0, chart.top()), Vec2::new(1.5, chart.height())), 0.0, ACCENT);
        p.rect_filled(Rect::from_min_size(Pos2::new(x1, chart.top()), Vec2::new(1.5, chart.height())), 0.0, ACCENT);
    } else if let Some(c) = sel_col {
        let x = chart.left() + c as f32 * bw;
        p.rect_stroke(Rect::from_min_size(Pos2::new(x - 1.0, chart.top()), Vec2::new(bw + 1.0, chart.height())), 0.0, Stroke::new(1.5, WHITE), egui::StrokeKind::Middle);
    }

    // footer: span, what's selected, now
    let span_s = frames.back().map(|l| l.end).unwrap_or(0).saturating_sub(frames.front().map(|f| f.start).unwrap_or(0)) as f64 / 1e9;
    p.text(Pos2::new(chart.left(), foot_y), Align2::LEFT_CENTER, format!("−{span_s:.1} s"), tiny(), FAINT);
    p.text(Pos2::new(chart.right(), foot_y), Align2::RIGHT_CENTER, if live { "now" } else { "end" }, tiny(), FAINT);
    let mid = match w.sel {
        Sel::Range(a, b) => {
            let i0 = frames.partition_point(|f| f.start < a);
            let i1 = frames.partition_point(|f| f.start < b);
            format!(
                "Selection: frames {} – {} · {} frames · {:.2} s · right-click to clear",
                crate::spaced(cap.frames_trimmed + i0 as u64),
                crate::spaced(cap.frames_trimmed + i1.saturating_sub(1) as u64),
                i1 - i0,
                (b - a) as f64 / 1e9
            )
        }
        _ => match w.sel_frame(cap) {
            Some((i, f)) => format!(
                "Frame #{} {} · {:.1} ms · click a frame, drag for a range",
                crate::spaced(cap.frames_trimmed + i as u64),
                if w.sel == Sel::Latest && live { "(newest)" } else { "selected" },
                f.ms()
            ),
            None => "click a frame, drag for a range".into(),
        },
    };
    p.text(Pos2::new(chart.center().x, foot_y), Align2::CENTER_CENTER, mid, tiny(), FAINT);

    // input
    let resp = ui.interact(chart, ui.id().with("strip"), Sense::click_and_drag());
    let frame_at = |x: f32| -> usize {
        let c = (((x - chart.left()) / bw).floor().max(0.0) as usize).min(cols - 1);
        let i0 = c * per;
        let i1 = ((c + 1) * per).min(n);
        (i0..i1).max_by_key(|&i| frames[i].dur()).unwrap_or(i0)
    };
    if let Some(pos) = resp.hover_pos() {
        let i = frame_at(pos.x);
        let f = frames[i];
        let b = breakdown_of(w, cap, i, f);
        let x = chart.left() + (i / per) as f32 * bw;
        p.rect_filled(Rect::from_min_size(Pos2::new(x, chart.top()), Vec2::new(bw.max(1.0), chart.height())), 0.0, WHITE.gamma_multiply(0.06));
        if !resp.dragged() {
            resp.clone().on_hover_ui_at_pointer(|ui| {
                ui.label(egui::RichText::new(format!("Frame #{} · {:.2} ms", crate::spaced(cap.frames_trimmed + i as u64), f.ms())).font(strong(13.0, ui)).color(WHITE));
                let mut parts: Vec<(Cat, f32)> = STACK.iter().map(|c| (*c, b[*c as usize])).filter(|x| x.1 > 0.05).collect();
                parts.sort_by(|a, b| b.1.total_cmp(&a.1));
                for (c, v) in parts.into_iter().take(6) {
                    kv_swatch(ui, w.prefs.style.cat_color(c), c.name(), &format!("{v:.2} ms"));
                }
            });
        }
    }
    if resp.drag_started() {
        if let Some(pos) = resp.interact_pointer_pos() {
            w.tl.strip_drag = Some(frame_at(pos.x));
        }
    }
    if resp.dragged() {
        if let (Some(a), Some(pos)) = (w.tl.strip_drag, resp.interact_pointer_pos()) {
            let b = frame_at(pos.x);
            let (lo, hi) = (a.min(b), a.max(b));
            if lo != hi {
                w.sel = Sel::Range(frames[lo].start, frames[hi].end);
            }
        }
    }
    if resp.drag_stopped() {
        w.tl.strip_drag = None;
        w.table.invalidate();
    }
    if resp.clicked() {
        if let Some(pos) = resp.interact_pointer_pos() {
            let i = frame_at(pos.x);
            w.sel = if i + 1 == n && live { Sel::Latest } else { Sel::Frame(frames[i].start) };
            w.tl.pinned = None;
            w.table.invalidate();
        }
    }
    if resp.secondary_clicked() {
        w.sel = Sel::Latest;
        w.table.invalidate();
    }
}
