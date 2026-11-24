//! The Table tab: the merged call tree (or a flat hot list, or totals by
//! category) over the selection, averaged per frame, summed, or for the
//! worst frame. Sortable columns, a filter, one thread or all.

use std::collections::HashSet;
use std::hash::{Hash, Hasher};
use std::time::Instant;

use egui::{Align, Align2, Layout, Pos2, Rect, Sense, Ui, Vec2};
use rustudio_core::profiler::analysis::{self, CallTree, HotEntry, Stats};
use rustudio_core::profiler::{Capture, Cat, FrameKind, FrameRec};

use crate::theme::*;
use crate::{display_name, ProfilerWindow, Sel};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) enum Col {
    Category,
    Total,
    SelfT,
    Calls,
    AvgCall,
    P95,
    Max,
    Pct,
}

impl Col {
    const ALL: [Col; 8] = [Col::Category, Col::Total, Col::SelfT, Col::Calls, Col::AvgCall, Col::P95, Col::Max, Col::Pct];
    fn title(self, instr: bool) -> &'static str {
        match (self, instr) {
            (Col::Category, _) => "Category",
            (Col::Total, false) => "Total ms",
            (Col::Total, true) => "Total instr",
            (Col::SelfT, false) => "Self ms",
            (Col::SelfT, true) => "Self instr",
            (Col::Calls, _) => "Calls",
            (Col::AvgCall, _) => "Avg / call",
            (Col::P95, _) => "p95",
            (Col::Max, _) => "Max",
            (Col::Pct, _) => "% of frame",
        }
    }
    fn width(self) -> f32 {
        match self {
            Col::Category => 110.0,
            Col::Total | Col::SelfT | Col::AvgCall => 105.0,
            Col::Calls => 80.0,
            Col::P95 | Col::Max => 95.0,
            Col::Pct => 200.0,
        }
    }
    fn left(self) -> bool {
        matches!(self, Col::Category | Col::Pct)
    }
}

struct Cached {
    key: (u64, u64, Option<String>, u8, bool),
    at: Instant,
    tree: CallTree,
    hot: Vec<HotEntry>,
    cats: Vec<(Cat, u64, u64)>,
    /// Frames the values are divided by, and their mean length (ns).
    frames: u64,
    avg_frame: u64,
    span: u64,
}

pub(crate) struct State {
    pub filter: String,
    cached: Option<Cached>,
    selected: Option<u64>,
    expanded: HashSet<u64>,
    collapsed: HashSet<u64>,
    /// One lane by name, or all.
    thread: Option<String>,
    sort: (Option<Col>, bool),
    threads_menu: bool,
    columns_menu: bool,
    hidden: Vec<Col>,
    menu_anchor: [Pos2; 2],
}

impl Default for State {
    fn default() -> Self {
        State {
            filter: String::new(),
            cached: None,
            selected: None,
            expanded: HashSet::new(),
            collapsed: HashSet::new(),
            thread: None,
            sort: (Some(Col::Total), true),
            threads_menu: false,
            columns_menu: false,
            hidden: Vec::new(),
            menu_anchor: [Pos2::ZERO; 2],
        }
    }
}

impl State {
    pub fn invalidate(&mut self) {
        self.cached = None;
    }
}

/// One row on screen.
struct Row {
    depth: usize,
    key: u64,
    /// Its node in the call tree (tree mode).
    node: usize,
    /// Expandable, and open.
    twisty: Option<bool>,
    name: String,
    cat: Cat,
    stats: Stats,
    /// The thread row of the tree (grey, no category).
    thread_row: bool,
}

fn path_key(parent: u64, name: &str) -> u64 {
    let mut h = std::collections::hash_map::DefaultHasher::new();
    parent.hash(&mut h);
    name.hash(&mut h);
    h.finish()
}

fn refresh(w: &mut ProfilerWindow, cap: &Capture, live: bool) {
    let (t0, t1) = w.stats_range(cap, 60);
    let agg = w.prefs.table_agg;
    let key = (t0, t1, w.table.thread.clone(), agg, w.cfg.instr_mode);
    let fresh = w.table.cached.as_ref().is_some_and(|c| {
        // (following the newest frames live: every half second is plenty)
        c.key == key || (live && w.sel == Sel::Latest && c.key.2 == key.2 && c.key.3 == key.3 && c.at.elapsed().as_millis() < 500)
    });
    if fresh {
        return;
    }
    let mut range = (t0, t1);
    let frames: Vec<FrameRec> = analysis::frames_in(cap, FrameKind::Render, t0, t1).copied().collect();
    if agg == 2 {
        if let Some(worst) = frames.iter().max_by_key(|f| f.dur()) {
            range = (worst.start, worst.end);
        }
    }
    let thread = w.table.thread.clone();
    let keep = |t: &rustudio_core::profiler::Track| thread.as_ref().is_none_or(|n| *n == t.name);
    let tree = analysis::call_tree(cap, range.0, range.1, keep);
    let hot = analysis::hot_list(cap, range.0, range.1, keep);
    let cats = analysis::category_totals(cap, range.0, range.1, keep);
    let n = if agg == 2 { 1 } else { frames.len().max(1) as u64 };
    let avg_frame = if agg == 2 {
        range.1 - range.0
    } else if frames.is_empty() {
        (t1 - t0).max(1)
    } else {
        frames.iter().map(|f| f.dur()).sum::<u64>() / frames.len() as u64
    };
    w.table.cached = Some(Cached { key, at: Instant::now(), tree, hot, cats, frames: n, avg_frame, span: range.1 - range.0 });
}

pub(crate) fn show(w: &mut ProfilerWindow, ui: &mut Ui, cap: &Capture, live: bool) {
    let r = ui.max_rect();
    // (narrow: the bar wraps to a second row instead of overlapping)
    let two_rows = r.width() < 1060.0;
    let bar = Rect::from_min_size(r.left_top(), Vec2::new(r.width(), if two_rows { SUBBAR_H * 2.0 - 8.0 } else { SUBBAR_H }));
    subbar(w, ui, bar, cap, two_rows);
    refresh(w, cap, live);
    let instr = w.cfg.instr_mode;
    let Some(c) = w.table.cached.take() else { return };
    let rows = rows(w, cap, &c);
    let head = Rect::from_min_size(Pos2::new(r.left(), bar.bottom()), Vec2::new(r.width(), 30.0));
    let mut cols: Vec<Col> = Col::ALL.iter().copied().filter(|c| !w.table.hidden.contains(c)).collect();
    // (narrow: the least needed columns go first, so nothing is cut off)
    for drop in [Col::AvgCall, Col::P95, Col::Category, Col::Max, Col::Calls, Col::Pct] {
        if cols.iter().map(|c| c.width()).sum::<f32>() + 220.0 <= r.width() {
            break;
        }
        cols.retain(|c| *c != drop);
    }
    let fixed: f32 = cols.iter().map(|c| c.width()).sum();
    let name_w = (r.width() - fixed).max(220.0);
    // header
    fill(ui, head, PANEL);
    hline(ui, head.left(), head.right(), head.bottom() - 1.0, LINE);
    let mut x = head.left();
    let name_hdr = match w.prefs.table_mode {
        2 => "Category",
        _ => "Name",
    };
    header_cell(w, ui, Rect::from_min_size(Pos2::new(x, head.top()), Vec2::new(name_w, 30.0)), name_hdr, None, true);
    x += name_w;
    for col in &cols {
        let cr = Rect::from_min_size(Pos2::new(x, head.top()), Vec2::new(col.width(), 30.0));
        header_cell(w, ui, cr, col.title(instr), Some(*col), col.left());
        x += col.width();
    }
    // rows
    let body_r = Rect::from_min_max(Pos2::new(r.left(), head.bottom()), r.right_bottom());
    let rh = w.prefs.style.row_height();
    let mut inner = crate::region(ui, body_r, "table-body");
    let mut toggle: Option<(u64, bool)> = None;
    let mut select = None;
    egui::ScrollArea::vertical().auto_shrink([false, false]).show_rows(&mut inner, rh, rows.len(), |ui, range| {
        for row in &rows[range] {
            let (rr, resp) = ui.allocate_exact_size(Vec2::new(body_r.width(), rh), Sense::click());
            let sel = w.table.selected == Some(row.key);
            if sel {
                ui.painter().rect_filled(rr, 0.0, SEL_ROW);
            } else if resp.hovered() {
                ui.painter().rect_filled(rr, 0.0, PANEL.gamma_multiply(0.6));
            }
            hline(ui, rr.left(), rr.right(), rr.bottom() - 1.0, LINE2);
            let p = ui.painter();
            let cy = rr.center().y;
            let mut x = rr.left() + 10.0 + row.depth as f32 * 18.0;
            if let Some(open) = row.twisty {
                p.text(Pos2::new(x + 3.0, cy), Align2::LEFT_CENTER, if open { "▾" } else { "▸" }, small(), FAINT);
                let tw = Rect::from_center_size(Pos2::new(x + 7.0, cy), Vec2::splat(rh));
                if resp.clicked() && ui.input(|i| i.pointer.interact_pos()).is_some_and(|p| tw.contains(p)) {
                    toggle = Some((row.key, !open));
                }
            }
            x += 14.0;
            let color = if row.thread_row { FAINT } else { w.prefs.style.cat_color(row.cat) };
            if !row.thread_row || w.prefs.table_mode == 2 {
                swatch(ui, Pos2::new(x, cy - 5.0), if row.cat == Cat::Wait || row.cat == Cat::Frame { NEUTRAL_BAR } else { color });
                x += 15.0;
            }
            let name_clip = p.with_clip_rect(Rect::from_min_max(rr.left_top(), Pos2::new(rr.left() + name_w - 8.0, rr.bottom())));
            name_clip.text(Pos2::new(x, cy), Align2::LEFT_CENTER, &row.name, body(), if row.thread_row { MUTED } else { TEXT });
            let mut cx = rr.left() + name_w;
            for col in &cols {
                let cr = Rect::from_min_size(Pos2::new(cx, rr.top()), Vec2::new(col.width(), rh));
                cell(w, ui, cr, *col, row, &c, instr);
                cx += col.width();
            }
            if resp.clicked() && toggle.is_none() {
                select = Some(row.key);
            }
            if resp.double_clicked() {
                if let Some(open) = row.twisty {
                    toggle = Some((row.key, !open));
                }
            }
        }
    });
    if rows.is_empty() {
        ui.painter().text(body_r.center(), Align2::CENTER_CENTER, if w.table.filter.is_empty() { "Nothing recorded in this range" } else { "Nothing matches the filter" }, body(), FAINT);
    }
    if let Some((k, open)) = toggle {
        if open {
            w.table.expanded.insert(k);
            w.table.collapsed.remove(&k);
        } else {
            w.table.collapsed.insert(k);
            w.table.expanded.remove(&k);
        }
    }
    if let Some(k) = select {
        w.table.selected = Some(k);
    }
    w.table.cached = Some(c);
    menus(w, ui, cap);
}

fn header_cell(w: &mut ProfilerWindow, ui: &mut Ui, r: Rect, title: &str, col: Option<Col>, left: bool) {
    let resp = ui.interact(r, ui.id().with(("th", title)), Sense::click());
    let sorted = w.table.sort.0 == col;
    let arrow = if sorted { if w.table.sort.1 { " ↓" } else { " ↑" } } else { "" };
    let text = format!("{title}{arrow}");
    let color = if resp.hovered() || sorted { TEXT } else { MUTED };
    let font = egui::FontId::proportional(11.5);
    if left {
        ui.painter().text(Pos2::new(r.left() + 10.0, r.center().y), Align2::LEFT_CENTER, text, font, color);
    } else {
        ui.painter().text(Pos2::new(r.right() - 10.0, r.center().y), Align2::RIGHT_CENTER, text, font, color);
    }
    if resp.clicked() {
        w.table.sort = if sorted { (col, !w.table.sort.1) } else { (col, true) };
    }
}

/// The value shown in `col` (also the sort key).
fn value(col: Col, row: &Row, c: &Cached, instr: bool) -> f64 {
    let st = &row.stats;
    let n = c.frames as f64;
    match col {
        Col::Category => 0.0,
        Col::Total => (if instr { st.incl_instr } else { st.incl_ns }) as f64 / n,
        Col::SelfT => (if instr { st.self_instr } else { st.self_ns }) as f64 / n,
        Col::Calls => st.calls as f64 / n,
        Col::AvgCall => {
            let v = if instr { st.incl_instr } else { st.incl_ns };
            if st.calls == 0 { 0.0 } else { v as f64 / st.calls as f64 }
        }
        Col::P95 => (if st.p95_frame_ns > 0 { st.p95_frame_ns } else { st.p95_ns }) as f64,
        Col::Max => (if st.max_frame_ns > 0 { st.max_frame_ns } else { st.max_ns }) as f64,
        Col::Pct => st.incl_ns as f64 / n / c.avg_frame.max(1) as f64 * 100.0,
    }
}

fn cell(w: &ProfilerWindow, ui: &Ui, r: Rect, col: Col, row: &Row, c: &Cached, instr: bool) {
    let p = ui.painter();
    let cy = r.center().y;
    let v = value(col, row, c, instr);
    let sum = w.prefs.table_agg == 1;
    let num = |x: f64| -> String {
        if instr {
            analysis::fmt_instr(x as u64)
        } else {
            format!("{:.2}", x / 1e6)
        }
    };
    // (a thread's row: only its total and share mean something)
    if row.thread_row && matches!(col, Col::Calls | Col::AvgCall | Col::P95 | Col::Max | Col::SelfT) {
        return;
    }
    let text = match col {
        Col::Category => {
            if !row.thread_row {
                p.text(Pos2::new(r.left() + 10.0, cy), Align2::LEFT_CENTER, row.cat.name(), body(), MUTED);
            }
            return;
        }
        Col::Calls => {
            if sum || c.frames == 1 || (v - v.round()).abs() < 0.05 && v >= 0.95 {
                format!("{}", v.round() as u64)
            } else if v >= 10.0 {
                format!("{v:.0}")
            } else if v >= 0.1 {
                format!("{v:.1}")
            } else {
                format!("{v:.2}")
            }
        }
        Col::AvgCall => {
            if instr { analysis::fmt_instr(v as u64) } else { format!("{:.3}", v / 1e6) }
        }
        Col::P95 | Col::Max => {
            if instr { "—".into() } else { format!("{:.2}", v / 1e6) }
        }
        Col::Pct => {
            let pct = if sum { row.stats.incl_ns as f64 * 100.0 / c.span.max(1) as f64 } else { v };
            let bw = (pct / 100.0 * 120.0).clamp(0.0, 120.0) as f32;
            let color = if row.thread_row { FAINT } else if matches!(row.cat, Cat::Wait | Cat::Frame) { NEUTRAL_BAR.gamma_multiply(1.6) } else { w.prefs.style.cat_color(row.cat) };
            p.rect_filled(Rect::from_min_size(Pos2::new(r.left() + 10.0, cy - 3.0), Vec2::new(bw.max(2.0), 6.0)), 3.0, color);
            p.text(Pos2::new(r.left() + 16.0 + bw, cy), Align2::LEFT_CENTER, format!("{pct:.1} %"), mono(), TEXT);
            return;
        }
        Col::Total | Col::SelfT => {
            if sum {
                num(v * c.frames as f64)
            } else {
                num(v)
            }
        }
    };
    p.text(Pos2::new(r.right() - 10.0, cy), Align2::RIGHT_CENTER, text, mono(), TEXT);
}

fn rows(w: &ProfilerWindow, cap: &Capture, c: &Cached) -> Vec<Row> {
    let needle = w.table.filter.to_lowercase();
    let instr = w.cfg.instr_mode;
    let sort = w.table.sort;
    let mut out = Vec::new();
    let visible_cat = |cat: Cat| w.prefs.cat_visible(cat);
    let sort_rows = |v: &mut Vec<Row>| {
        if let (Some(col), desc) = sort {
            if col == Col::Category {
                v.sort_by(|a, b| a.cat.name().cmp(b.cat.name()));
            } else {
                v.sort_by(|a, b| value(col, a, c, instr).total_cmp(&value(col, b, c, instr)));
            }
            if desc {
                v.reverse();
            }
        }
    };
    match w.prefs.table_mode {
        1 => {
            let mut v: Vec<Row> = c
                .hot
                .iter()
                .filter(|e| visible_cat(e.cat))
                .map(|e| Row { depth: 0, key: e.name as u64, node: 0, twisty: None, name: display_name(cap.name(e.name)).to_string(), cat: e.cat, stats: e.stats.clone(), thread_row: false })
                .filter(|r| needle.is_empty() || r.name.to_lowercase().contains(&needle))
                .collect();
            sort_rows(&mut v);
            out = v;
        }
        2 => {
            let mut v: Vec<Row> = c
                .cats
                .iter()
                .filter(|e| visible_cat(e.0))
                .map(|&(cat, ns, calls)| Row {
                    depth: 0,
                    key: cat as u64,
                    node: 0,
                    twisty: None,
                    name: cat.name().to_string(),
                    cat,
                    stats: Stats { incl_ns: ns, self_ns: ns, calls, ..Default::default() },
                    thread_row: false,
                })
                .filter(|r| needle.is_empty() || r.name.to_lowercase().contains(&needle))
                .collect();
            sort_rows(&mut v);
            out = v;
        }
        _ => {
            let t = &c.tree;
            // which nodes match the filter (themselves or below)
            let n = t.nodes.len();
            let mut hit = vec![needle.is_empty(); n];
            if !needle.is_empty() {
                for i in (0..n).rev() {
                    let own = t.name(cap, i).to_lowercase().contains(&needle);
                    if own || t.nodes[i].children.iter().any(|&k| hit[k]) {
                        hit[i] = true;
                    }
                }
            }
            let one_thread = w.table.thread.is_some();
            #[allow(clippy::too_many_arguments)]
            fn walk(w: &ProfilerWindow, cap: &Capture, t: &CallTree, i: usize, depth: usize, parent: u64, hit: &[bool], filtering: bool, out: &mut Vec<Row>, sort_rows: &dyn Fn(&mut Vec<Row>)) {
                let mut kids: Vec<Row> = t.nodes[i]
                    .children
                    .iter()
                    .filter(|&&k| hit[k] && (t.nodes[k].name.is_none() || w.prefs.cat_visible(t.nodes[k].cat)))
                    .map(|&k| {
                        let node = &t.nodes[k];
                        let label = match node.name {
                            Some(_) => display_name(t.name(cap, k)).to_string(),
                            None => analysis::lane_info(&node.label).0,
                        };
                        Row { depth, key: path_key(parent, &label), node: k, twisty: None, name: label, cat: node.cat, stats: node.stats.clone(), thread_row: node.name.is_none() }
                    })
                    .collect();
                sort_rows(&mut kids);
                for mut row in kids {
                    let k = row.node;
                    let has_kids = !t.nodes[k].children.is_empty();
                    let key = row.key;
                    let open = has_kids && (filtering || w.table.expanded.contains(&key) || (!w.table.collapsed.contains(&key) && depth < 2));
                    row.twisty = has_kids.then_some(open);
                    out.push(row);
                    if open {
                        walk(w, cap, t, k, depth + 1, key, hit, filtering, out, sort_rows);
                    }
                }
            }
            let start = if one_thread { t.nodes.first().and_then(|r| r.children.first().copied()) } else { Some(0) };
            if let Some(s) = start {
                walk(w, cap, t, s, 0, 0, &hit, !needle.is_empty(), &mut out, &sort_rows);
            }
        }
    }
    out
}

fn subbar(w: &mut ProfilerWindow, ui: &mut Ui, r: Rect, cap: &Capture, two_rows: bool) {
    hline(ui, r.left(), r.right(), r.bottom() - 1.0, LINE2);
    let row1 = Rect::from_min_size(r.left_top(), Vec2::new(r.width(), SUBBAR_H));
    let row2 = if two_rows { Rect::from_min_max(Pos2::new(r.left(), row1.bottom() - 8.0), r.right_bottom()) } else { row1 };
    let mut c = crate::region(ui, row1.shrink2(Vec2::new(10.0, 0.0)), "table-bar");
    c.with_layout(Layout::left_to_right(Align::Center), |ui| {
        ui.spacing_mut().item_spacing.x = 10.0;
        if let Some(i) = seg(ui, &["Tree", "Hot list", "By category"], w.prefs.table_mode as usize) {
            w.prefs.table_mode = i as u8;
        }
        let sw = (ui.available_width() - if two_rows { 0.0 } else { 520.0 }).clamp(120.0, 220.0);
        search(ui, &mut w.table.filter, "Filter… (name, script, category)", sw);
        if !two_rows {
            if let Some(i) = seg(ui, &["Average", "Sum", "Max frame"], w.prefs.table_agg as usize) {
                w.prefs.table_agg = i as u8;
                w.table.invalidate();
            }
        }
    });
    let mut c = crate::region(ui, row2.shrink2(Vec2::new(10.0, 0.0)), "table-bar2");
    c.with_layout(Layout::left_to_right(Align::Center), |ui| {
        ui.spacing_mut().item_spacing.x = 10.0;
        if two_rows {
            if let Some(i) = seg(ui, &["Average", "Sum", "Max frame"], w.prefs.table_agg as usize) {
                w.prefs.table_agg = i as u8;
                w.table.invalidate();
            }
        } else {
            // (row 1's left part is drawn above: leave its room)
            ui.add_space((ui.available_width() - 300.0).max(0.0));
        }
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            ui.spacing_mut().item_spacing.x = 8.0;
            let cr = button(ui, "Columns ▾");
            if cr.clicked() {
                w.table.columns_menu = !w.table.columns_menu;
                w.table.threads_menu = false;
            }
            w.table.menu_anchor[1] = cr.rect.left_bottom();
            let label = match &w.table.thread {
                Some(n) => format!("{} ▾", analysis::lane_info(n).0),
                None => "All threads ▾".to_string(),
            };
            let tr = button(ui, &label);
            if tr.clicked() {
                w.table.threads_menu = !w.table.threads_menu;
                w.table.columns_menu = false;
            }
            w.table.menu_anchor[0] = tr.rect.left_bottom();
            ui.label(egui::RichText::new("Thread").font(body()).color(MUTED));
        });
    });
    let _ = cap;
}

fn menus(w: &mut ProfilerWindow, ui: &mut Ui, cap: &Capture) {
    if w.table.threads_menu {
        let mut pick: Option<Option<String>> = None;
        let resp = popup(ui, "threads", w.table.menu_anchor[0], 220.0, |ui| {
            let mut names: Vec<Option<String>> = vec![None];
            names.extend(crate::timeline::lanes(w, cap).iter().map(|t| Some(t.name.clone())));
            for n in names {
                let label = n.as_ref().map(|x| analysis::lane_info(x).0).unwrap_or_else(|| "All threads".into());
                let on = w.table.thread == n;
                let (rr, resp) = ui.allocate_exact_size(Vec2::new(220.0, 26.0), Sense::click());
                if resp.hovered() || on {
                    ui.painter().rect_filled(rr, 3.0, if on { SEG_ON } else { PANEL2 });
                }
                ui.painter().text(Pos2::new(rr.left() + 8.0, rr.center().y), Align2::LEFT_CENTER, label, body(), TEXT);
                if resp.clicked() {
                    pick = Some(n);
                }
            }
        });
        if let Some(n) = pick {
            w.table.thread = n;
            w.table.threads_menu = false;
            w.table.invalidate();
        } else if resp {
            w.table.threads_menu = false;
        }
    }
    if w.table.columns_menu {
        let mut flip = None;
        let resp = popup(ui, "columns", w.table.menu_anchor[1], 180.0, |ui| {
            for col in Col::ALL {
                if checkbox(ui, !w.table.hidden.contains(&col), col.title(w.cfg.instr_mode), None) {
                    flip = Some(col);
                }
            }
        });
        if let Some(col) = flip {
            if let Some(i) = w.table.hidden.iter().position(|c| *c == col) {
                w.table.hidden.remove(i);
            } else {
                w.table.hidden.push(col);
            }
        } else if resp {
            w.table.columns_menu = false;
        }
    }
}

/// A small dropdown under `anchor`; true when the user clicked outside it.
fn popup(ui: &mut Ui, id: &str, anchor: Pos2, width: f32, add: impl FnOnce(&mut Ui)) -> bool {
    let at = Pos2::new((anchor.x).min(ui.ctx().content_rect().right() - width - 24.0), anchor.y + 4.0);
    let r = egui::Area::new(ui.id().with(("popup", id))).fade_in(false).order(egui::Order::Foreground).fixed_pos(at).show(ui.ctx(), |ui| {
        egui::Frame::new().fill(POP_BG).stroke(egui::Stroke::new(1.0, POP_LINE)).corner_radius(5).inner_margin(8).show(ui, |ui| {
            ui.set_width(width);
            ui.spacing_mut().item_spacing.y = 2.0;
            add(ui);
        });
    });
    let outside = ui.input(|i| i.pointer.any_pressed()) && ui.input(|i| i.pointer.interact_pos()).is_some_and(|p| !r.response.rect.contains(p) && p.y > anchor.y + 2.0);
    outside
}
