//! RUSTUDIO_GPU_TIMING=1: GPU time per pass group, from timestamp queries.
//!
//! `begin(device, encoder)` writes a timestamp; each `mark(encoder, label)`
//! writes another and books the GPU time since the previous one to `label`.
//! `end_frame` resolves the frame's queries into a read-back buffer (a ring
//! of a few, so nothing waits for the GPU) and collects frames that have
//! finished. `take()` hands the bench the per-label milliseconds and each
//! finished frame's total (the sum of its spans: the timed encoders only, not
//! egui's own pass or the present). Results lag the frame they measure by a
//! frame or two. Needs the adapter's TIMESTAMP_QUERY and
//! TIMESTAMP_QUERY_INSIDE_ENCODERS. Automatic quality uses the scene's
//! encoder start and end timestamps in both timing modes.

use std::sync::atomic::{AtomicBool, AtomicU8, Ordering};
use std::sync::{Arc, Mutex, OnceLock};

use egui_wgpu::wgpu;

const MAX_QUERIES: u32 = 256;
const RING: usize = 4;
const FREE: u8 = 0;
const MAPPING: u8 = 1;
const MAPPED: u8 = 2;

fn benchmark_enabled() -> bool {
    static ON: OnceLock<bool> = OnceLock::new();
    *ON.get_or_init(|| std::env::var_os("RUSTUDIO_GPU_TIMING").is_some())
}

static AUTOMATIC: AtomicBool = AtomicBool::new(false);

pub(crate) fn set_automatic(enabled: bool) {
    AUTOMATIC.store(enabled, Ordering::Relaxed);
}

/// Whether GPU spans are timed now (`end_frame` does nothing otherwise).
pub fn on() -> bool {
    benchmark_enabled() || AUTOMATIC.load(Ordering::Relaxed) || rustudio_core::profiler::gpu_wanted()
}

/// Every labelled span is timed (the bench or the profiler wants them), not
/// only the scene span automatic quality reads.
fn detailed() -> bool {
    benchmark_enabled() || rustudio_core::profiler::gpu_wanted()
}

/// Keep timestamp queries available if Automatic is selected after startup.
pub fn features(adapter: wgpu::Features) -> wgpu::Features {
    adapter & (wgpu::Features::TIMESTAMP_QUERY | wgpu::Features::TIMESTAMP_QUERY_INSIDE_ENCODERS)
}

pub fn last_scene_ms() -> Option<f32> {
    if !on() {
        return None;
    }
    STATE.lock().unwrap_or_else(|e| e.into_inner()).as_mut()?.last_scene_ms.take()
}

struct Slot {
    set: wgpu::QuerySet,
    resolve: wgpu::Buffer,
    read: wgpu::Buffer,
    spans: Vec<(u32, u32, &'static str)>,
    scene_span: Option<(u32, u32)>,
    used: u32,
    state: Arc<AtomicU8>,
    /// The profiler's clock when the frame's work was submitted (where its
    /// GPU spans are placed on the timeline).
    cpu_ns: u64,
}

struct State {
    slots: Vec<Slot>,
    cur: Option<usize>,
    last: Option<u32>,
    encoder_start: Option<u32>,
    period_ns: f64,
    results: Vec<(&'static str, f64)>,
    totals: Vec<f64>,
    last_scene_ms: Option<f32>,
}

static STATE: Mutex<Option<State>> = Mutex::new(None);
static UNSUPPORTED: OnceLock<()> = OnceLock::new();

fn init(device: &wgpu::Device, queue: &wgpu::Queue) -> Option<State> {
    let need = wgpu::Features::TIMESTAMP_QUERY | wgpu::Features::TIMESTAMP_QUERY_INSIDE_ENCODERS;
    if !device.features().contains(need) {
        UNSUPPORTED.get_or_init(|| eprintln!("RUSTUDIO_GPU_TIMING: the adapter has no timestamp queries inside encoders"));
        return None;
    }
    // The bench (core) asks for the timings through this callback.
    rustudio_core::bench::set_gpu_timing(take);
    let bytes = MAX_QUERIES as u64 * 8;
    let slots = (0..RING)
        .map(|_| Slot {
            set: device.create_query_set(&wgpu::QuerySetDescriptor {
                label: Some("gpu timing"),
                ty: wgpu::QueryType::Timestamp,
                count: MAX_QUERIES,
            }),
            resolve: device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("gpu timing resolve"),
                size: bytes,
                usage: wgpu::BufferUsages::QUERY_RESOLVE | wgpu::BufferUsages::COPY_SRC,
                mapped_at_creation: false,
            }),
            read: device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("gpu timing read"),
                size: bytes,
                usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            }),
            spans: Vec::new(),
            scene_span: None,
            used: 0,
            state: Arc::new(AtomicU8::new(FREE)),
            cpu_ns: 0,
        })
        .collect();
    Some(State {
        slots,
        cur: Some(0),
        last: None,
        encoder_start: None,
        period_ns: queue.get_timestamp_period() as f64,
        results: Vec::new(),
        totals: Vec::new(),
        last_scene_ms: None,
    })
}


fn write(st: &mut State, encoder: &mut wgpu::CommandEncoder) -> Option<u32> {
    let slot = &mut st.slots[st.cur?];
    if slot.used >= MAX_QUERIES {
        return None;
    }
    let i = slot.used;
    encoder.write_timestamp(&slot.set, i);
    slot.used += 1;
    Some(i)
}

/// Starts a timed run of work in `encoder` (each encoder that is timed).
pub fn begin(device: &wgpu::Device, queue: &wgpu::Queue, encoder: &mut wgpu::CommandEncoder) {
    if !on() || UNSUPPORTED.get().is_some() {
        return;
    }
    let mut g = STATE.lock().unwrap_or_else(|e| e.into_inner());
    if g.is_none() {
        *g = init(device, queue);
    }
    let Some(st) = g.as_mut() else { return };
    st.last = write(st, encoder);
    st.encoder_start = st.last;
}

/// Books the GPU time since the last `begin` / `mark` in this encoder to `label`.
pub fn mark(encoder: &mut wgpu::CommandEncoder, label: &'static str) {
    if !on() || (!detailed() && label != "gpu: post") {
        return;
    }
    let mut g = STATE.lock().unwrap_or_else(|e| e.into_inner());
    let Some(st) = g.as_mut() else { return };
    let Some(prev) = st.last else { return };
    let Some(i) = write(st, encoder) else { return };
    let cur = st.cur.unwrap_or(0);
    let slot = &mut st.slots[cur];
    slot.spans.push((prev, i, label));
    if label == "gpu: post" {
        slot.scene_span = st.encoder_start.map(|start| (start, i));
    }
    st.last = Some(i);
}

/// Once per frame, after the frame's work is submitted: resolve this frame's
/// queries and collect earlier frames that finished.
pub fn end_frame(device: &wgpu::Device, queue: &wgpu::Queue) {
    if !on() {
        return;
    }
    let mut g = STATE.lock().unwrap_or_else(|e| e.into_inner());
    let Some(st) = g.as_mut() else { return };
    if let Some(cur) = st.cur {
        let slot = &mut st.slots[cur];
        if slot.used > 0 && !slot.spans.is_empty() {
            let mut enc = device.create_command_encoder(&wgpu::CommandEncoderDescriptor { label: Some("gpu timing") });
            enc.resolve_query_set(&slot.set, 0..slot.used, &slot.resolve, 0);
            enc.copy_buffer_to_buffer(&slot.resolve, 0, &slot.read, 0, slot.used as u64 * 8);
            queue.submit([enc.finish()]);
            slot.cpu_ns = rustudio_core::profiler::now_ns();
            slot.state.store(MAPPING, Ordering::Release);
            let state = slot.state.clone();
            slot.read.slice(..slot.used as u64 * 8).map_async(wgpu::MapMode::Read, move |r| {
                state.store(if r.is_ok() { MAPPED } else { FREE }, Ordering::Release);
            });
        } else {
            slot.used = 0;
            slot.spans.clear();
            slot.scene_span = None;
        }
    }
    st.last = None;
    st.encoder_start = None;
    let _ = device.poll(wgpu::PollType::Poll);
    let period = st.period_ns;
    for slot in st.slots.iter_mut() {
        if slot.state.load(Ordering::Acquire) != MAPPED {
            continue;
        }
        if let Ok(data) = slot.read.slice(..slot.used as u64 * 8).get_mapped_range() {
            let ts: Vec<u64> = data.chunks_exact(8).map(|c| u64::from_le_bytes(c.try_into().unwrap_or([0; 8]))).collect();
            if rustudio_core::profiler::active() {
                profile_slot(slot, &ts, period);
            }
            let mut total = 0.0;
            for (a, b, label) in &slot.spans {
                let ms = ts[*b as usize].wrapping_sub(ts[*a as usize]) as f64 * period / 1e6;
                // (a counter that went backwards or a bogus value: dropped)
                if !(0.0..1000.0).contains(&ms) {
                    continue;
                }
                total += ms;
                if benchmark_enabled() {
                    match st.results.iter_mut().find(|e| e.0 == *label) {
                        Some(e) => e.1 += ms,
                        None => st.results.push((label, ms)),
                    }
                }
            }
            // Detailed profiling also times effects, UI and extra submits.
            // Feed the controller the same viewport span in both modes.
            if let Some((a, b)) = slot.scene_span {
                let scene = ts[b as usize].wrapping_sub(ts[a as usize]) as f64 * period / 1e6;
                if scene > 0.0 && scene < 1000.0 {
                    st.last_scene_ms = Some(scene as f32);
                }
            }
            if benchmark_enabled() {
                st.totals.push(total);
            }
        }
        slot.read.unmap();
        slot.used = 0;
        slot.spans.clear();
        slot.scene_span = None;
        slot.state.store(FREE, Ordering::Release);
    }
    // The next free slot records the next frame; none free: that frame isn't timed.
    st.cur = st.slots.iter().position(|s| s.state.load(Ordering::Acquire) == FREE && s.used == 0);
}

/// Hands a finished frame's spans to the profiler's GPU lane. GPU and CPU
/// clocks aren't related: the frame's first timestamp is placed at the time
/// its work was submitted, the rest keep their offsets from it.
fn profile_slot(slot: &Slot, ts: &[u64], period: f64) {
    use rustudio_core::profiler::{frame_at, lane_span, Cat, FrameKind};
    let Some(base) = slot.spans.iter().map(|(a, _, _)| ts[*a as usize]).min() else { return };
    let at = |t: u64| slot.cpu_ns + (t.wrapping_sub(base) as f64 * period) as u64;
    frame_at(FrameKind::Gpu, slot.cpu_ns);
    let mut spans: Vec<_> = slot.spans.iter().filter(|(a, b, _)| ts[*b as usize] >= ts[*a as usize]).collect();
    spans.sort_by_key(|(_, b, _)| ts[*b as usize]);
    for (a, b, label) in spans {
        let (s, e) = (at(ts[*a as usize]), at(ts[*b as usize]));
        // (a counter that went backwards or a bogus value: dropped)
        if e.saturating_sub(s) < 1_000_000_000 {
            lane_span("GPU", label, Cat::Gpu, s, e);
        }
    }
}

/// The GPU milliseconds per label since the last call, and each finished
/// frame's total.
pub fn take() -> (Vec<(&'static str, f64)>, Vec<f64>) {
    if !on() {
        return (Vec::new(), Vec::new());
    }
    let mut g = STATE.lock().unwrap_or_else(|e| e.into_inner());
    match g.as_mut() {
        Some(st) => (std::mem::take(&mut st.results), std::mem::take(&mut st.totals)),
        None => (Vec::new(), Vec::new()),
    }
}

/// The timestamp encoders around a submit timed as `label`: (before,
/// after), None with the switch off. Doesn't disturb a `begin`/`mark` run
/// being recorded in another encoder meanwhile.
fn bracket(device: &wgpu::Device, queue: &wgpu::Queue, label: &'static str) -> Option<(wgpu::CommandEncoder, wgpu::CommandEncoder)> {
    if !detailed() || UNSUPPORTED.get().is_some() {
        return None;
    }
    let saved = STATE.lock().unwrap_or_else(|e| e.into_inner())
        .as_ref().map(|s| (s.last, s.encoder_start));
    let mut a = device.create_command_encoder(&wgpu::CommandEncoderDescriptor { label: Some("gpu timing") });
    begin(device, queue, &mut a);
    let mut b = device.create_command_encoder(&wgpu::CommandEncoderDescriptor { label: Some("gpu timing") });
    mark(&mut b, label);
    if let Some(st) = STATE.lock().unwrap_or_else(|e| e.into_inner()).as_mut() {
        (st.last, st.encoder_start) = saved.unwrap_or_default();
    }
    Some((a, b))
}

/// `encoders` finished and submitted on the render thread (render_thread.rs),
/// timed as `label` (a timestamp before and after, in their own command
/// buffers) with the switch on.
pub fn submit(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    label: &'static str,
    encoders: impl IntoIterator<Item = wgpu::CommandEncoder>,
) {
    let all = match bracket(device, queue, label) {
        Some((a, b)) => std::iter::once(a).chain(encoders).chain(std::iter::once(b)).collect(),
        None => encoders.into_iter().collect(),
    };
    crate::render_thread::perform(queue, all, None);
}

/// `queue.submit(bufs)` here and now, timed as `label` (after
/// `render_thread::wait`: egui's paint offscreen).
pub fn submit_finished(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    label: &'static str,
    bufs: impl IntoIterator<Item = wgpu::CommandBuffer>,
) {
    match bracket(device, queue, label) {
        Some((a, b)) => queue.submit(std::iter::once(a.finish()).chain(bufs).chain(std::iter::once(b.finish()))),
        None => queue.submit(bufs),
    };
}
