//! CPU work for assets off the render path: mesh parsing, skinned mesh
//! bases and the clothing / SurfaceAppearance composites. The render path
//! asks, gets "pending" and holds the part back (a mesh) or draws it
//! without (a composite); the finished result is uploaded later under a
//! per-frame budget (assets.rs `MeshCache`, `TextureCache`).
//!
//! As Roblox (Studio's code): its renderer's meshes are
//! parsed by a `TaskScheduler::submitAsyncTask` (0x102bbe0f4) task started
//! from `MeshManager2::onMeshContentReceived` (0x10517b03a, `ReadFileMesh`
//! on the RSL async pool), and only their buffers are made on the render
//! thread (`uploadPendingMeshesToGpu` 0x10517c6a6); textures decode the same
//! way and upload in `TextureManager2::processPendingImages` (0x10538b0a2)
//! until 4 ms are spent. Roblox composites clothing on the GPU
//! (`TextureCompositor::render` 0x10535d64c, at most 8 active jobs); here
//! the composite stays the CPU one it was (assets.rs `r15_composite`,
//! `composite_over`), made on these workers instead of in the frame.

use std::sync::{mpsc, Arc, LazyLock, Mutex};

type Job = Box<dyn FnOnce() + Send>;

/// The pool: half the cores (at least one, at most four), shared by every
/// cache. Decodes have their own (texture.rs `decode_async`).
static POOL: LazyLock<Mutex<mpsc::Sender<Job>>> = LazyLock::new(|| {
    let (tx, rx) = mpsc::channel::<Job>();
    let rx = Arc::new(Mutex::new(rx));
    let n = std::thread::available_parallelism().map(|n| n.get()).unwrap_or(4) / 2;
    for i in 0..n.clamp(1, crate::mech::ASSET_WORK_THREADS) {
        let rx = rx.clone();
        let _ = std::thread::Builder::new().name(format!("asset-work-{i}")).spawn(move || loop {
            let job = rx.lock().ok().and_then(|r| r.recv().ok());
            let Some(job) = job else { return };
            let _p = crate::profiler::scope("asset_work", crate::profiler::Cat::Assets);
            job();
        });
    }
    Mutex::new(tx)
});

/// Runs `job` on the asset workers.
pub fn spawn(job: impl FnOnce() + Send + 'static) {
    if let Ok(tx) = POOL.lock() {
        let _ = tx.send(Box::new(job));
    }
}

/// Results of background jobs keyed by what asked for them, each with the
/// ticket it was started under: a result whose key was forgotten (or
/// started again) since is dropped on arrival.
pub struct Jobs<T> {
    running: std::collections::HashMap<String, u64>,
    next: u64,
    done: std::collections::HashMap<String, T>,
    chan: (mpsc::Sender<(String, u64, T)>, mpsc::Receiver<(String, u64, T)>),
}

impl<T> Default for Jobs<T> {
    fn default() -> Self {
        Jobs { running: Default::default(), next: 0, done: Default::default(), chan: mpsc::channel() }
    }
}

impl<T: Send + 'static> Jobs<T> {
    /// Starts `work` for `key` unless it is running or finished already.
    pub fn start(&mut self, key: &str, work: impl FnOnce() -> T + Send + 'static) {
        if self.running.contains_key(key) || self.done.contains_key(key) {
            return;
        }
        self.next += 1;
        let ticket = self.next;
        self.running.insert(key.to_string(), ticket);
        let tx = self.chan.0.clone();
        let key = key.to_string();
        spawn(move || {
            let _ = tx.send((key, ticket, work()));
        });
    }

    /// Collects finished jobs; true when any arrived.
    pub fn poll(&mut self) -> bool {
        let mut any = false;
        while let Ok((key, ticket, r)) = self.chan.1.try_recv() {
            if self.running.get(&key) == Some(&ticket) {
                self.running.remove(&key);
                self.done.insert(key, r);
                any = true;
            }
        }
        any
    }

    pub fn running(&self, key: &str) -> bool {
        self.running.contains_key(key)
    }

    /// A finished result, taken.
    pub fn take(&mut self, key: &str) -> Option<T> {
        self.done.remove(key)
    }

    /// Puts a taken result back (its upload waits for the next frame).
    pub fn put_back(&mut self, key: String, r: T) {
        self.done.insert(key, r);
    }

    /// Drops `key`'s job and result: a running one's result is ignored.
    pub fn forget(&mut self, key: &str) {
        self.running.remove(key);
        self.done.remove(key);
    }

    /// Keys running or finished and not taken.
    pub fn len(&self) -> usize {
        self.running.len() + self.done.len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn wait<T: Send + 'static>(jobs: &mut Jobs<T>) {
        let t0 = std::time::Instant::now();
        while !jobs.running.is_empty() && t0.elapsed().as_secs() < 10 {
            jobs.poll();
            std::thread::sleep(std::time::Duration::from_millis(1));
        }
    }

    #[test]
    fn a_forgotten_job_result_is_dropped() {
        let mut jobs: Jobs<u32> = Jobs::default();
        let (go, gate) = mpsc::channel::<()>();
        jobs.start("a", move || {
            let _ = gate.recv();
            1
        });
        jobs.forget("a");
        jobs.start("a", || 2);
        let _ = go.send(());
        wait(&mut jobs);
        // (the first job's 1 arrives under an old ticket)
        assert_eq!(jobs.take("a"), Some(2));
        assert!(jobs.is_empty());
    }
}
