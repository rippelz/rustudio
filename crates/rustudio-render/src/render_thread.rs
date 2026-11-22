//! The render thread: Roblox's split of a frame's rendering between the
//! render job and a render thread (local://threading-research.md A.2).
//! `SurfaceController::RenderJob::stepDataModelJob` (0x1013f2bc4) runs
//! `Render::renderStep`, then `FunctionMarshaller::Submit(scheduleRender)`
//! hands the frame over ("TransferControlToRenderThread"); `scheduleRender`
//! (0x1013f40d6) runs `RenderView::renderPrepare` (0x1051eb0fc: the scene
//! walk and culling, under the DataModel lock) and then
//! `RenderView::renderPerform` (0x1051ef2ca: GPU command encoding and
//! submission) while the next jobs run, until "WaitForRenderThread".
//!
//! Here the main thread prepares: it culls, uploads and records the frame's
//! passes. wgpu encodes the recorded passes into the driver's command
//! buffers in `CommandEncoder::finish`, the bulk of a frame's CPU cost, so
//! the perform half is `finish` and `Queue::submit`, on this thread, in the
//! order the work was handed over. `wait` joins it: before egui paints the
//! window (it shows the viewport texture the frame drew, so no frame of
//! latency is added), before a read-back, and so before the next frame's
//! first queue write (writes are staged on the queue and land at its next
//! submit, so one made while a frame's commands wait here would reach them
//! first). Between the hand-over and the join the main thread records the
//! rest of the frame and builds egui's.
//!
//! RUSTUDIO_RENDER_THREAD=0 performs on the calling thread.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Condvar, LazyLock, Mutex};

use egui_wgpu::wgpu;

type Job = Box<dyn FnOnce() + Send>;

struct Done {
    /// Jobs finished (or panicked).
    count: u64,
    /// The first panic of a job, raised again on the thread that waits.
    panic: Option<Box<dyn std::any::Any + Send>>,
    /// The time spent performing since the last `wait` (ms, jobs).
    perform: (f64, u64),
}

struct Thread {
    tx: Mutex<std::sync::mpsc::Sender<Job>>,
    done: Mutex<Done>,
    cv: Condvar,
}

static THREAD: LazyLock<Option<Thread>> = LazyLock::new(|| {
    if std::env::var("RUSTUDIO_RENDER_THREAD").is_ok_and(|v| v == "0") {
        return None;
    }
    // (the window's painter joins this thread before egui's paint)
    egui_wgpu::prewarm::set_before_paint(wait);
    let (tx, rx) = std::sync::mpsc::channel::<Job>();
    let spawned = std::thread::Builder::new().name("rustudio-render".into()).spawn(move || {
        // (waits for this initializer to finish)
        let Some(me) = THREAD.as_ref() else { return };
        for job in rx {
            let t0 = std::time::Instant::now();
            let span = rustudio_core::profiler::scope("render_perform", rustudio_core::profiler::Cat::Render);
            let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(job));
            drop(span);
            let ms = t0.elapsed().as_secs_f64() * 1e3;
            let mut d = me.done.lock().unwrap_or_else(|e| e.into_inner());
            d.count += 1;
            d.perform.0 += ms;
            d.perform.1 += 1;
            if let Err(p) = result {
                d.panic.get_or_insert(p);
            }
            me.cv.notify_all();
        }
    });
    spawned.ok().map(|_| Thread {
        tx: Mutex::new(tx),
        done: Mutex::new(Done { count: 0, panic: None, perform: (0.0, 0) }),
        cv: Condvar::new(),
    })
});
/// Jobs handed to the thread so far.
static SENT: AtomicU64 = AtomicU64::new(0);

/// Runs `job` on the render thread after the work handed over before it
/// (here, now, without one).
pub fn run(job: impl FnOnce() + Send + 'static) {
    let Some(t) = THREAD.as_ref() else {
        job();
        return;
    };
    SENT.fetch_add(1, Ordering::AcqRel);
    let sent = t.tx.lock().unwrap_or_else(|e| e.into_inner()).send(Box::new(job));
    if let Err(std::sync::mpsc::SendError(job)) = sent {
        // (the thread is gone: perform here, and count it done)
        job();
        let mut d = t.done.lock().unwrap_or_else(|e| e.into_inner());
        d.count += 1;
    }
}

/// Command buffers finished ahead of their submit (`finish`) and what runs
/// after that submit: the next `perform` submits them first, in the same
/// `Queue::submit` (a submit costs wgpu ~0.5–1 ms of bookkeeping whatever
/// it carries: bench `r_submit` was 0.9 ms a frame for the scene alone).
/// Only the render thread (or, without one, the caller) touches it.
struct Held {
    queue: Option<wgpu::Queue>,
    bufs: Vec<wgpu::CommandBuffer>,
    after: Vec<Job>,
}

static HELD: Mutex<Held> = Mutex::new(Held { queue: None, bufs: Vec::new(), after: Vec::new() });

/// Submits what is held for `queue` (another queue's on its own first)
/// with `bufs`, then runs the held and the given `after`s.
fn submit_with_held(queue: &wgpu::Queue, bufs: Vec<wgpu::CommandBuffer>, after: Option<Job>) {
    let mut h = HELD.lock().unwrap_or_else(|e| e.into_inner());
    let held_queue = h.queue.take();
    let held = std::mem::take(&mut h.bufs);
    let afters = std::mem::take(&mut h.after);
    drop(h);
    let all = match held_queue {
        Some(q) if q != *queue => {
            q.submit(held);
            bufs
        }
        _ => held.into_iter().chain(bufs).collect(),
    };
    queue.submit(all);
    afters.into_iter().chain(after).for_each(|a| a());
}

/// Finishes `encoders` on the render thread and holds them for the next
/// `perform`'s submit; `after` runs after that submit.
pub fn finish(queue: &wgpu::Queue, encoders: Vec<wgpu::CommandEncoder>, after: Option<Job>) {
    let queue = queue.clone();
    run(move || {
        let bufs: Vec<_> = encoders.into_iter().map(wgpu::CommandEncoder::finish).collect();
        let mut h = HELD.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(q) = h.queue.take().filter(|q| *q != queue) {
            let held = std::mem::take(&mut h.bufs);
            let afters = std::mem::take(&mut h.after);
            q.submit(held);
            afters.into_iter().for_each(|a| a());
        }
        h.queue = Some(queue);
        h.bufs.extend(bufs);
        h.after.extend(after);
    });
}

/// Finishes `encoders` and submits them, after anything held, as one batch
/// on the render thread, then runs `after` there (what must follow the
/// submit: a read-back's `map_async`).
pub fn perform(queue: &wgpu::Queue, encoders: Vec<wgpu::CommandEncoder>, after: Option<Job>) {
    let queue = queue.clone();
    run(move || {
        let bufs: Vec<_> = encoders.into_iter().map(wgpu::CommandEncoder::finish).collect();
        submit_with_held(&queue, bufs, after);
    });
}

/// Blocks until everything handed over has been performed ("WaitForRenderThread"),
/// then submits anything still held (a `finish` no `perform` followed).
/// A job's panic is raised again here.
pub fn wait() {
    let sent = SENT.load(Ordering::Acquire);
    // (`sent` is 0 until a job went to the thread: no thread made here)
    if let Some(t) = (sent > 0).then(|| THREAD.as_ref()).flatten() {
        let mut d = t.done.lock().unwrap_or_else(|e| e.into_inner());
        if d.count < sent {
            let _w = rustudio_core::bench::scope("render_thread_wait");
            while d.count < sent {
                d = t.cv.wait(d).unwrap_or_else(|e| e.into_inner());
            }
        }
        let (ms, jobs) = std::mem::take(&mut d.perform);
        let panic = d.panic.take();
        drop(d);
        if jobs > 0 {
            // (the render thread's own time, in the bench as one call a job)
            rustudio_core::bench::add("render_thread_perform", ms, jobs);
        }
        if let Some(p) = panic {
            std::panic::resume_unwind(p);
        }
    }
    let queue = HELD.lock().unwrap_or_else(|e| e.into_inner()).queue.clone();
    if let Some(queue) = queue {
        submit_with_held(&queue, Vec::new(), None);
    }
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    #[test]
    fn jobs_run_in_order_and_wait_joins_them() {
        let seen = Arc::new(Mutex::new(Vec::new()));
        for i in 0..50 {
            let s = seen.clone();
            super::run(move || {
                std::thread::sleep(std::time::Duration::from_micros(50));
                s.lock().unwrap().push(i);
            });
        }
        super::wait();
        assert_eq!(*seen.lock().unwrap(), (0..50).collect::<Vec<_>>());
    }
}
