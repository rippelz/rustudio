use super::analysis::*;
use super::capture::*;
use super::*;

/// The profiler is global: tests that record run one at a time.
fn serial() -> std::sync::MutexGuard<'static, ()> {
    static L: std::sync::Mutex<()> = std::sync::Mutex::new(());
    let g = L.lock().unwrap_or_else(|e| e.into_inner());
    while is_enabled() {
        disable();
    }
    resume();
    set_config(Config::default());
    clear();
    g
}

fn spin(us: u64) {
    let t = std::time::Instant::now();
    while t.elapsed().as_micros() < us as u128 {
        std::hint::spin_loop();
    }
}

#[test]
fn inactive_records_nothing() {
    let _g = serial();
    clear();
    assert!(!active());
    {
        let _a = scope("never", Cat::Other);
        counter("never_counter", Cat::Other, 1.0);
        mark("never_mark", Cat::Other);
    }
    frame(FrameKind::Render);
    with_capture(|c| assert_eq!(c.span_count(), 0));
}

#[test]
fn nested_scopes_frames_counters_and_tree() {
    let _g = serial();
    set_config(Config::default());
    enable();
    clear();
    frame(FrameKind::Render);
    for i in 0..3 {
        {
            let _outer = scope("outer", Cat::Physics);
            spin(300);
            {
                let _inner = scope("inner", Cat::Script);
                spin(500);
            }
            let l = intern("Workspace.Script");
            let _d = scope_dyn(l, Cat::Script);
            spin(100);
        }
        counter("parts", Cat::Other, i as f64 * 10.0);
        mark("tick", Cat::Input);
        frame(FrameKind::Render);
    }
    // another thread with its own name
    std::thread::spawn(|| {
        thread_name("worker lane");
        let _s = scope("bg_job", Cat::Assets);
        spin(200);
    })
    .join()
    .unwrap();
    frame(FrameKind::Render);
    disable();

    let cap = snapshot();
    assert_eq!(cap.render_frames().len(), 4);
    let main = main_track(&cap).unwrap();
    let outer = main.spans.iter().filter(|s| cap.name(s.name) == "outer").count();
    assert_eq!(outer, 3);
    let inner = main.spans.iter().find(|s| cap.name(s.name) == "inner").unwrap();
    assert_eq!(inner.depth, 1);
    assert_eq!(inner.cat, Cat::Script);
    assert!(cap.track_by_name("worker lane").is_some());
    let parts = cap.counter("parts").unwrap();
    assert_eq!(parts.last(), Some(20.0));
    assert_eq!(main.marks.len(), 3);

    let (t0, t1) = cap.time_range().unwrap();
    let tree = call_tree(&cap, t0, t1, |_| true);
    let o = tree.nodes.iter().position(|n| n.name.is_some_and(|x| cap.name(x) == "outer")).unwrap();
    let st = &tree.nodes[o].stats;
    assert_eq!(st.calls, 3);
    assert!(st.self_ns < st.incl_ns);
    // children: inner + the dyn script scope
    let kids: Vec<&str> = tree.nodes[o].children.iter().map(|&c| tree.name(&cap, c)).collect();
    assert!(kids.contains(&"inner") && kids.contains(&"Workspace.Script"), "{kids:?}");
    let inner_node = &tree.nodes[tree.nodes[o].children.iter().copied().find(|&c| tree.name(&cap, c) == "inner").unwrap()];
    assert!(inner_node.stats.incl_ns >= 3 * 500_000);
    assert_eq!(st.self_ns + tree.nodes[o].children.iter().map(|&c| tree.nodes[c].stats.incl_ns).sum::<u64>(), st.incl_ns);

    let hot = hot_list(&cap, t0, t1, |_| true);
    assert_eq!(cap.name(hot[0].name), "inner");

    let f = cap.render_frames()[1];
    let by = category_breakdown(main, f.start, f.end);
    assert!(by[Cat::Script as usize] > by[Cat::Physics as usize]);
    assert_eq!(by.iter().sum::<u64>(), f.dur());
}

#[test]
fn auto_category_from_name() {
    assert_eq!(Cat::from_name("ph_world_step"), Cat::Physics);
    assert_eq!(Cat::from_name("gui_text_layout"), Cat::Gui);
    assert_eq!(Cat::from_name("vp_render"), Cat::Render);
    assert_eq!(Cat::from_name("explorer_build"), Cat::Ui);
    assert_eq!(Cat::from_name("p4_server_lua"), Cat::Script);
    assert_eq!(Cat::from_name("rep_dispatch"), Cat::Network);
    assert_eq!(Cat::from_name("ribbon"), Cat::Ui);
    assert_eq!(Cat::from_name("native_redraw"), Cat::Ui);
    assert_eq!(Cat::from_name("zzz"), Cat::Other);
}

#[test]
fn capture_next_freezes_and_spike_capture() {
    let _g = serial();
    enable();
    capture_next(3);
    frame(FrameKind::Render);
    for _ in 0..5 {
        let _s = scope("work", Cat::Other);
        frame(FrameKind::Render);
    }
    assert!(matches!(frozen(), Some(Frozen::Captured { frames: 3 })));
    assert!(!active());
    let n = with_capture(|c| c.render_frames().len());
    assert_eq!(n, 3);

    resume();
    clear();
    set_config(Config { spike_capture: true, spike_ms: 5.0, spike_frames_after: 2, ..Config::default() });
    frame(FrameKind::Render);
    frame(FrameKind::Render);
    spin(8000);
    frame(FrameKind::Render); // spike
    frame(FrameKind::Render);
    assert!(frozen().is_none());
    frame(FrameKind::Render);
    assert!(matches!(frozen(), Some(Frozen::Spike { .. })), "{:?}", frozen());
    resume();
    set_config(Config::default());
    disable();
    clear();
}

fn sample() -> Capture {
    let mut c = Capture::new();
    c.names = vec!["frame".into(), "physics".into(), "gpu: scene".into()];
    c.tracks.push(Track {
        id: 1,
        name: "main".into(),
        kind: TrackKind::Thread,
        spans: vec![
            SpanRec { start: 1_000, end: 4_000, name: 1, cat: Cat::Physics, depth: 1, instr: 77, alloc: 4096, loc: 1 },
            SpanRec { start: 0, end: 10_000, name: 0, cat: Cat::Frame, depth: 0, instr: 0, alloc: 0, loc: 0 },
        ]
        .into(),
        marks: vec![MarkRec { t: 500, name: 1, cat: Cat::Input }].into(),
        max_dur: 10_000,
        dropped: 0,
    });
    c.tracks.push(Track {
        id: 1 << 30,
        name: "GPU".into(),
        kind: TrackKind::Gpu,
        spans: vec![SpanRec { start: 2_000, end: 6_000, name: 2, cat: Cat::Gpu, depth: 0, instr: 0, alloc: 0, loc: 0 }].into(),
        ..Default::default()
    });
    c.frames.insert(FrameKindKey::Render, vec![FrameRec { start: 0, end: 10_000 }].into());
    c.counters.push(CounterTrack { name: "memory".into(), cat: Cat::Other, unit: Unit::Bytes, samples: vec![(5_000, 1024.0)].into() });
    c.render_track = Some(1);
    c.locations = vec!["crates/rustudio-physics/src/physics.rs:3678".into()];
    c.extensions.insert("scripts".into(), serde_json::json!({"x": 1}));
    c
}

#[test]
fn native_round_trip() {
    let c = sample();
    let bytes = export::to_native(&c).unwrap();
    let back = export::from_bytes(&bytes).unwrap();
    assert_eq!(back.tracks[0].spans, c.tracks[0].spans);
    assert_eq!(back.frames, c.frames);
    assert_eq!(back.counters[0].samples, c.counters[0].samples);
    assert_eq!(back.extensions, c.extensions);
}

#[test]
fn chrome_round_trip() {
    let c = sample();
    let v = export::to_chrome(&c);
    let bytes = serde_json::to_vec(&v).unwrap();
    let back = export::from_bytes(&bytes).unwrap();
    let main = back.track_by_name("main").unwrap();
    let mut got: Vec<(u64, u64, String, Cat, u8, u64, u64)> =
        main.spans.iter().map(|s| (s.start, s.end, back.name(s.name).to_string(), s.cat, s.depth, s.instr, s.alloc)).collect();
    got.sort();
    assert_eq!(
        got,
        vec![(0, 10_000, "frame".into(), Cat::Frame, 0, 0, 0), (1_000, 4_000, "physics".into(), Cat::Physics, 1, 77, 4096)]
    );
    assert_eq!(back.render_frames().len(), 1);
    assert_eq!(back.counter("memory").unwrap().unit, Unit::Bytes);
    assert_eq!(back.track_by_name("GPU").unwrap().kind, TrackKind::Gpu);
    assert_eq!(back.extensions, c.extensions);
    assert_eq!(main.marks.len(), 1);
    let phys = main.spans.iter().find(|s| back.name(s.name) == "physics").unwrap();
    assert_eq!(back.location(phys.loc), Some("crates/rustudio-physics/src/physics.rs:3678"));
}

#[test]
fn foreign_chrome_trace_with_begin_end() {
    let t = serde_json::json!([
        {"ph": "B", "name": "a", "pid": 7, "tid": 3, "ts": 1000.0},
        {"ph": "B", "name": "b", "pid": 7, "tid": 3, "ts": 1001.0},
        {"ph": "E", "pid": 7, "tid": 3, "ts": 1002.0},
        {"ph": "E", "pid": 7, "tid": 3, "ts": 1005.0},
    ]);
    let c = export::from_bytes(&serde_json::to_vec(&t).unwrap()).unwrap();
    let tr = &c.tracks[0];
    assert_eq!(tr.spans.len(), 2);
    let b = tr.spans.iter().find(|s| c.name(s.name) == "b").unwrap();
    assert_eq!((b.start, b.end, b.depth), (1_000, 2_000, 1));
}

#[test]
fn frame_stats_and_formatting() {
    let f: Vec<FrameRec> = (0..100).map(|i| FrameRec { start: i * 16_000_000, end: (i + 1) * 16_000_000 + if i == 99 { 50_000_000 } else { 0 } }).collect();
    let s = frame_stats(&f);
    assert_eq!(s.count, 100);
    assert!((s.max_ms - 66.0).abs() < 1e-6);
    assert!((s.p50_ms - 16.0).abs() < 1e-6);
    assert_eq!(fmt_ns(812), "812 ns");
    assert_eq!(fmt_ns(43_200), "43.2 µs");
    assert_eq!(fmt_ns(3_410_000), "3.41 ms");
    assert_eq!(Unit::Bytes.format(12.0 * 1024.0 * 1024.0), "12.0 MB");
    assert_eq!(Unit::Count.format(1234567.0), "1,234,567");
}

#[test]
fn spans_in_range() {
    let c = sample();
    let t = &c.tracks[0];
    assert_eq!(t.spans_in(4_500, 5_000).count(), 1);
    assert_eq!(t.spans_in(0, 20_000).count(), 2);
    assert_eq!(t.spans_in(20_000, 30_000).count(), 0);
}

#[test]
fn short_lived_threads_share_a_lane() {
    let _g = serial();
    enable();
    frame(FrameKind::Render);
    for _ in 0..3 {
        std::thread::Builder::new()
            .name("oneshot".into())
            .spawn(|| {
                let _s = scope("job", Cat::Network);
                spin(50);
            })
            .unwrap()
            .join()
            .unwrap();
        frame(FrameKind::Render);
    }
    disable();
    let cap = snapshot();
    let lanes: Vec<_> = cap.tracks.iter().filter(|t| t.name == "oneshot").collect();
    assert_eq!(lanes.len(), 1);
    assert_eq!(lanes[0].spans.len(), 3);
}

#[test]
fn rate_counter_is_per_second() {
    let _g = serial();
    enable();
    frame(FrameKind::Render);
    rate("bytes in", Cat::Network, Unit::BytesPerSec, 500.0);
    rate("bytes in", Cat::Network, Unit::BytesPerSec, 250.0);
    frame(FrameKind::Render);
    frame(FrameKind::Render);
    disable();
    let cap = snapshot();
    let c = cap.counter("bytes in").unwrap();
    assert_eq!(c.last(), Some(750.0));
    assert_eq!(c.unit, Unit::BytesPerSec);
}

/// `cargo test --profile agent -p rustudio-core --lib profiler::tests::overhead -- --ignored --nocapture`
#[test]
#[ignore]
fn overhead() {
    let _g = serial();
    const N: u64 = 2_000_000;
    let run = |label: &str| {
        let t = std::time::Instant::now();
        for i in 0..N {
            let _s = scope("bench_scope", Cat::Other);
            std::hint::black_box(i);
            if i % 2000 == 1999 {
                frame(FrameKind::Render);
            }
        }
        let ns = t.elapsed().as_nanos() as f64 / N as f64;
        println!("{label}: {ns:.1} ns per scope");
        ns
    };
    let base = {
        let t = std::time::Instant::now();
        for i in 0..N {
            std::hint::black_box(i);
        }
        t.elapsed().as_nanos() as f64 / N as f64
    };
    println!("empty loop: {base:.1} ns");
    run("profiler off");
    let bench_off = {
        let t = std::time::Instant::now();
        for i in 0..N {
            let _s = crate::bench::scope("bench_scope");
            std::hint::black_box(i);
        }
        t.elapsed().as_nanos() as f64 / N as f64
    };
    println!("bench::scope, profiler off: {bench_off:.1} ns");
    enable();
    run("profiler on (incl. gathering every 2000 scopes)");
    let st = status();
    println!("last gather: {} ns for 2000 spans", st.gather_ns);
    disable();
    clear();
}

// The core test binary counts allocations like the app binaries do.
#[global_allocator]
static ALLOC: alloc::CountingAlloc = alloc::CountingAlloc;

#[test]
fn allocation_per_scope() {
    let _g = serial();
    alloc::CountingAlloc::mark_installed();
    set_config(Config::default());
    enable();
    frame(FrameKind::Render);
    {
        let _s = scope("allocates", Cat::Other);
        let v: Vec<u8> = Vec::with_capacity(100_000);
        std::hint::black_box(&v);
    }
    frame(FrameKind::Render);
    disable();
    let cap = snapshot();
    assert!(cap.meta.alloc_tracked);
    let s = main_track(&cap).unwrap().spans.iter().find(|s| cap.name(s.name) == "allocates").copied().unwrap();
    assert!((100_000..200_000).contains(&s.alloc), "{}", s.alloc);
}

#[test]
fn per_frame_stats_gpu_passes_and_categories() {
    let mut c = sample();
    // three display frames; "physics" runs in two of them
    c.frames.insert(FrameKindKey::Render, vec![
        FrameRec { start: 0, end: 10_000 },
        FrameRec { start: 10_000, end: 20_000 },
        FrameRec { start: 20_000, end: 30_000 },
    ].into());
    c.tracks[0].spans.push_back(SpanRec { start: 21_000, end: 29_000, name: 1, cat: Cat::Physics, depth: 0, instr: 0, alloc: 0, loc: 0 });
    c.tracks[0].max_dur = 10_000;
    let tree = call_tree(&c, 0, 30_000, |_| true);
    let p = tree.nodes.iter().find(|n| n.name == Some(1) && n.parent.is_some_and(|p| tree.nodes[p].name.is_none() && p != 0)).unwrap();
    assert_eq!(p.stats.frames_present, 1);
    assert_eq!(p.per_frame, vec![(2, 8_000)]);
    let nested = tree.nodes.iter().find(|n| n.name == Some(1) && n.parent.is_some_and(|p| tree.nodes[p].name == Some(0))).unwrap();
    assert_eq!(nested.stats.alloc_bytes, 4096);

    c.frames.insert(FrameKindKey::Gpu, vec![FrameRec { start: 0, end: 10_000 }, FrameRec { start: 10_000, end: 20_000 }].into());
    let (passes, per_frame) = gpu_passes(&c, 0, 20_000);
    assert_eq!(passes.len(), 1);
    assert_eq!(passes[0].history, vec![4_000, 0]);
    assert_eq!(passes[0].avg_ns, 2_000);
    assert_eq!(per_frame, 2_000);
    assert!((passes[0].share - 1.0).abs() < 1e-9);

    let cats = category_totals(&c, 0, 30_000, |t| t.name == "main");
    assert_eq!(cats[0].0, Cat::Physics);
    assert_eq!(cats[0].1, 3_000 + 8_000);
    assert_eq!(lane_info("physics-par-0"), ("Physics 1".to_string(), "solver workers", 3001));
    assert_eq!(lane_info("main").2, 0);
}

/// Analysis speed on a real capture: RUSTUDIO_PROFILE_FILE=X.rsprof
/// `cargo test --profile agent -p rustudio-core --lib profiler::tests::analysis_speed -- --ignored --nocapture`
#[test]
#[ignore]
fn analysis_speed() {
    let Some(path) = std::env::var_os("RUSTUDIO_PROFILE_FILE") else { return };
    let t = std::time::Instant::now();
    let cap = export::open(std::path::Path::new(&path)).unwrap();
    println!("open: {:?} ({} spans)", t.elapsed(), cap.span_count());
    let (t0, t1) = cap.time_range().unwrap();
    let t = std::time::Instant::now();
    let tree = call_tree(&cap, t0, t1, |_| true);
    println!("call tree, whole capture: {:?} ({} nodes)", t.elapsed(), tree.nodes.len());
    let t = std::time::Instant::now();
    let hot = hot_list(&cap, t0, t1, |_| true);
    println!("hot list, whole capture: {:?} ({} rows)", t.elapsed(), hot.len());
    let f = cap.render_frames()[cap.render_frames().len() / 2];
    let t = std::time::Instant::now();
    let _ = call_tree(&cap, f.start, f.end, |_| true);
    println!("call tree, one frame: {:?}", t.elapsed());
    let main = main_track(&cap).unwrap();
    let t = std::time::Instant::now();
    for f in cap.render_frames() {
        std::hint::black_box(category_breakdown(main, f.start, f.end));
    }
    println!("category breakdown, every frame ({}): {:?}", cap.render_frames().len(), t.elapsed());
    let t = std::time::Instant::now();
    let _ = gpu_passes(&cap, t0, t1);
    println!("gpu passes: {:?}", t.elapsed());
}

/// A synthetic capture: `frames` display frames of 16 ms on "main", each
/// with a "work" scope of `work_ns` (± a little) and an "idle" one.
fn synthetic(frames: u64, work_ns: u64, jitter: u64, instr: bool) -> Capture {
    let mut c = Capture::new();
    c.names = vec!["work".into(), "idle".into()];
    c.meta.instr_mode = instr;
    c.locations = vec!["crates/x/src/work.rs:10".into()];
    let mut spans = std::collections::VecDeque::new();
    let mut f = std::collections::VecDeque::new();
    for i in 0..frames {
        let t = i * 16_000_000;
        let w = work_ns + (i * 7919 % 13) * jitter / 13;
        spans.push_back(SpanRec { start: t, end: t + w, name: 0, cat: Cat::Physics, depth: 0, instr: if instr { w / 10 } else { 0 }, alloc: 0, loc: 1 });
        spans.push_back(SpanRec { start: t + w, end: t + w + 1_000_000, name: 1, cat: Cat::Other, depth: 0, instr: if instr { 100_000 } else { 0 }, alloc: 0, loc: 0 });
        f.push_back(FrameRec { start: t, end: t + 16_000_000 });
    }
    c.tracks.push(Track { id: 1, name: "main".into(), kind: TrackKind::Thread, spans, max_dur: 16_000_000, ..Default::default() });
    c.frames.insert(FrameKindKey::Render, f);
    c.render_track = Some(1);
    c
}

#[test]
fn report_json_schema_and_spikes() {
    let mut c = synthetic(200, 5_000_000, 200_000, false);
    // frame 100: the work scope takes 12 ms
    let s = c.tracks[0].spans.iter_mut().find(|s| s.start == 100 * 16_000_000 && s.name == 0).unwrap();
    s.end = s.start + 12_000_000;
    // (and the frame itself ran long)
    c.frames.get_mut(&FrameKindKey::Render).unwrap()[100].end += 8_000_000;
    let v = report::report_json(&c, 10, 3);
    assert_eq!(v["schema"], report::REPORT_SCHEMA);
    assert_eq!(v["capture"]["unit"], "ms");
    let work = &v["scopes"][0];
    assert_eq!(work["name"], "work");
    assert_eq!(work["source"], "crates/x/src/work.rs:10");
    assert!((work["calls_per_frame"].as_f64().unwrap() - 1.0).abs() < 1e-9);
    let spike = v["spikes"].as_array().unwrap().iter().find(|s| s["frame"] == 100).expect("frame 100 in the worst frames");
    assert_eq!(spike["culprits"][0]["name"], "work");
    assert!(report::report_text(&c, 10, 3).contains("work.rs:10"));
}

#[test]
fn diff_tells_change_from_noise() {
    let a = synthetic(300, 5_000_000, 300_000, true);
    let same = synthetic(300, 5_000_000, 300_000, true);
    let slower = synthetic(300, 6_000_000, 300_000, true);
    let d = report::diff_json(&a, &same, 10);
    assert_eq!(d["unit"], "instr");
    assert!(d["scopes"].as_array().unwrap().iter().all(|s| s["verdict"] == "noise"), "{d}");
    let d = report::diff_json(&a, &slower, 10);
    let work = d["scopes"].as_array().unwrap().iter().find(|s| s["name"] == "work").unwrap();
    assert_eq!(work["verdict"], "slower", "{work}");
    assert!((work["delta_pct"].as_f64().unwrap() - 20.0).abs() < 1.0);
    // ms: a 20 % change passes, a 5 % one is below the ms floor
    let ms_a = synthetic(300, 5_000_000, 300_000, false);
    let ms_small = synthetic(300, 5_250_000, 300_000, false);
    let d = report::diff_json(&ms_a, &ms_small, 10);
    let work = d["scopes"].as_array().unwrap().iter().find(|s| s["name"] == "work").unwrap();
    assert_eq!(work["verdict"], "noise");
    // a scope only in B is new
    let mut b = synthetic(300, 5_000_000, 300_000, true);
    b.names[1] = "fresh".into();
    let d = report::diff_json(&a, &b, 10);
    assert!(d["scopes"].as_array().unwrap().iter().any(|s| s["name"] == "fresh" && s["verdict"] == "new"));
    assert!(d["scopes"].as_array().unwrap().iter().any(|s| s["name"] == "idle" && s["verdict"] == "gone"));
}
