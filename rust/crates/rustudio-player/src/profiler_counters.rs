//! The profiler's per-frame counters that live in the `Game` (instances,
//! assets, memory, the renderer's own tallies, audio). Engine-side counters
//! (physics, GUI, draws, network, Luau heaps) are set where they happen.

use std::sync::Mutex;
use std::time::Instant;

use rustudio_core::profiler::{self, counter, counter_unit, Cat, Unit};

use crate::game::Game;

struct Slow {
    at: Option<Instant>,
    /// (document id, tree revision) the instance counts were taken at.
    rev: (u64, u64),
    counted: Option<Instant>,
    instances: f64,
    parts: f64,
    rss: f64,
    /// Class → is a BasePart (the reflection lookup is the walk's cost).
    part_class: std::collections::HashMap<rbx_dom_weak::Ustr, bool>,
}

static SLOW: std::sync::LazyLock<Mutex<Slow>> = std::sync::LazyLock::new(|| {
    Mutex::new(Slow {
        at: None,
        rev: (0, 0),
        counted: None,
        instances: 0.0,
        parts: 0.0,
        rss: 0.0,
        part_class: Default::default(),
    })
});

/// Once per display frame, on the thread that draws (no-op while the
/// profiler is off). Costly counts are refreshed at most 4 times a second.
pub fn sample(game: &Game) {
    if !profiler::active() {
        return;
    }
    let _p = profiler::scope("profiler_counters", Cat::Other);
    let mut s = SLOW.lock().unwrap_or_else(|e| e.into_inner());
    let due = s.at.is_none_or(|t| t.elapsed().as_millis() >= 250);
    if due {
        s.at = Some(Instant::now());
        s.rss = rustudio_core::platform::resident_bytes().unwrap_or(0) as f64;
        // (a full walk: ~70 ns an instance, 4-5 ms for a 64k-instance place,
        // so at most every 5 s and only when the tree changed; the timeline
        // shows it as the profiler's own `profiler_count_instances`)
        let rev = (game.doc.id, game.doc.tree_revision);
        if rev != s.rev && s.counted.is_none_or(|t| t.elapsed().as_secs() >= 5) {
            let _c = profiler::scope("profiler_count_instances", Cat::Other);
            s.rev = rev;
            s.counted = Some(Instant::now());
            let d = &game.doc.dom;
            let (mut n, mut parts) = (0usize, 0usize);
            let mut last: Option<(rbx_dom_weak::Ustr, bool)> = None;
            for i in d.descendants_of(d.root_ref()) {
                n += 1;
                let part = match last {
                    Some((c, p)) if c == i.class => p,
                    _ => {
                        let p = *s.part_class.entry(i.class).or_insert_with(|| rustudio_core::doc::is_a(&i.class, "BasePart"));
                        last = Some((i.class, p));
                        p
                    }
                };
                parts += part as usize;
            }
            s.instances = n as f64;
            s.parts = parts as f64;
        }
    }
    counter("instances", Cat::Other, s.instances);
    counter("parts", Cat::Physics, s.parts);
    counter_unit("memory (resident)", Cat::Other, Unit::Bytes, s.rss);
    drop(s);

    counter("asset downloads pending", Cat::Assets, game.assets.pending_count() as f64);
    let (decoding, textures) = game.textures.counts();
    counter("textures decoding", Cat::Assets, decoding as f64);
    counter("textures loaded", Cat::Assets, textures as f64);
    counter("meshes loaded", Cat::Assets, game.meshes.count() as f64);
    if let Some(r) = game.renderer.as_ref() {
        let st = &r.stats;
        counter("render items", Cat::Render, st.items as f64);
        counter("render batches", Cat::Render, st.batches as f64);
        counter("render instances drawn", Cat::Render, st.drawn_instances as f64);
        counter("render batches culled", Cat::Render, st.culled_batches as f64);
    }
    if let Some(p) = game.play.as_ref() {
        counter("audio voices", Cat::Audio, p.game.audio_voices() as f64);
    }
    // what the capture is of (the window's header), and the GPU
    thread_local! { static SEEN: std::cell::RefCell<(String, bool)> = const { std::cell::RefCell::new((String::new(), false)) }; }
    SEEN.with(|seen| {
        let mut seen = seen.borrow_mut();
        let mode = match game.play.as_ref().map(|p| p.mode) {
            Some(m) => format!("{m:?}"),
            None => "Edit".to_string(),
        };
        let context = format!("{mode} · {}", game.doc.title);
        if seen.0 != context {
            profiler::set_context(&context);
            seen.0 = context;
        }
        if !seen.1 {
            if let Some(rs) = game.render_state.as_ref() {
                let info = rs.adapter.get_info();
                profiler::set_gpu_info(&format!("{} · {:?}", info.name, info.backend));
                seen.1 = true;
            }
        }
    });
}

/// A rate counter from a running total (bytes so far): adds what grew since
/// the last call. `last` keeps the total between calls.
pub fn rate_from_total(name: &'static str, cat: Cat, total: u64, last: &mut u64) {
    // (the first call only learns the total: what came before isn't this window's)
    let first = *last == 0;
    let d = total.saturating_sub(*last);
    *last = total;
    if first {
        return;
    }
    if d > 0 && profiler::active() {
        profiler::rate(name, cat, Unit::BytesPerSec, d as f64);
    }
}
