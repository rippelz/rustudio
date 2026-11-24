//! Asset thumbnails for the Toolbox: thumbnails.roblox.com in batches,
//! the PNGs from the CDN, decoded on worker threads, kept on disk
//! (`<cache>/rustudio/toolbox-thumbs/<id>_<size>.png`, a day) and in
//! memory as egui textures (least recently used dropped past a cap). The
//! UI thread only asks (`get`) and uploads a few decoded images a frame.

use std::collections::{HashMap, HashSet, VecDeque};
use std::path::PathBuf;
use std::sync::mpsc::{channel, Receiver, Sender};
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant, SystemTime};

use super::api::{self, Http};
use super::model;

pub const SIZE: u32 = 150;
/// Textures kept in memory.
const MEM_CAP: usize = 600;
/// Decoded images turned into textures per frame (keeps frames smooth).
const UPLOADS_PER_FRAME: usize = 10;
const DISK_TTL: Duration = Duration::from_secs(24 * 3600);
const BATCH: usize = 30;
const WORKERS: usize = 3;

#[derive(Clone)]
pub enum Thumb {
    Loading,
    Ready(egui::TextureHandle),
    Failed,
}

struct Decoded {
    id: u64,
    size: u32,
    image: Option<egui::ColorImage>,
    /// Roblox is still rendering it (state "Pending"): ask again later.
    pending: bool,
}

#[derive(Default)]
struct Queue {
    want: VecDeque<(u64, u32)>,
    stop: bool,
}

pub struct Thumbs {
    mem: HashMap<(u64, u32), (Thumb, u64)>,
    tick: u64,
    queue: Arc<(Mutex<Queue>, Condvar)>,
    rx: Receiver<Decoded>,
    tx: Sender<Decoded>,
    http: Arc<Mutex<Arc<dyn Http>>>,
    started: bool,
    retry: Vec<(Instant, u64, u32)>,
    inflight: HashSet<(u64, u32)>,
    dir: PathBuf,
    /// Decoded but not yet uploaded (spread over frames).
    ready: VecDeque<Decoded>,
    pub uploaded_last_frame: usize,
    /// Times a still-rendering ("Pending") thumbnail was asked again.
    pending_tries: HashMap<(u64, u32), u8>,
}

impl Default for Thumbs {
    fn default() -> Self {
        let (tx, rx) = channel();
        Thumbs {
            mem: HashMap::new(),
            tick: 0,
            queue: Arc::new((Mutex::new(Queue::default()), Condvar::new())),
            rx,
            tx,
            http: Arc::new(Mutex::new(Arc::new(api::AnonHttp))),
            started: false,
            retry: Vec::new(),
            inflight: HashSet::new(),
            dir: rustudio_core::paths::cache().join("rustudio").join("toolbox-thumbs"),
            ready: VecDeque::new(),
            uploaded_last_frame: 0,
            pending_tries: HashMap::new(),
        }
    }
}

impl Drop for Thumbs {
    fn drop(&mut self) {
        if let Ok(mut q) = self.queue.0.lock() {
            q.stop = true;
        }
        self.queue.1.notify_all();
    }
}

impl Thumbs {
    pub fn set_http(&mut self, h: Arc<dyn Http>) {
        if let Ok(mut g) = self.http.lock() {
            *g = h;
        }
    }

    fn start(&mut self) {
        if self.started {
            return;
        }
        self.started = true;
        let _ = std::fs::create_dir_all(&self.dir);
        for i in 0..WORKERS {
            let q = self.queue.clone();
            let tx = self.tx.clone();
            let http = self.http.clone();
            let dir = self.dir.clone();
            let _ = std::thread::Builder::new().name(format!("toolbox-thumbs-{i}")).spawn(move || worker(q, tx, http, dir));
        }
    }

    /// The thumbnail of `id` at `size` (asks for it the first time).
    pub fn get(&mut self, id: u64, size: u32) -> Thumb {
        self.tick += 1;
        let tick = self.tick;
        if let Some((t, used)) = self.mem.get_mut(&(id, size)) {
            *used = tick;
            return t.clone();
        }
        self.start();
        self.mem.insert((id, size), (Thumb::Loading, tick));
        self.inflight.insert((id, size));
        let (lock, cv) = &*self.queue;
        if let Ok(mut q) = lock.lock() {
            q.want.push_back((id, size));
        }
        cv.notify_one();
        Thumb::Loading
    }

    /// Forget a failure so the next `get` asks again (Retry).
    pub fn retry_failed(&mut self) {
        self.mem.retain(|_, (t, _)| !matches!(t, Thumb::Failed));
    }

    pub fn loading(&self) -> usize {
        self.inflight.len()
    }

    /// Per frame: textures for what the workers decoded (a few a frame),
    /// pending thumbnails asked again, old textures dropped.
    pub fn tick(&mut self, ctx: &egui::Context) -> bool {
        while let Ok(d) = self.rx.try_recv() {
            self.ready.push_back(d);
        }
        let mut n = 0;
        while n < UPLOADS_PER_FRAME {
            let Some(d) = self.ready.pop_front() else { break };
            self.inflight.remove(&(d.id, d.size));
            let key = (d.id, d.size);
            let t = match d.image {
                Some(img) => {
                    n += 1;
                    Thumb::Ready(ctx.load_texture(format!("toolbox-thumb-{}-{}", d.id, d.size), img, egui::TextureOptions::LINEAR))
                }
                None if d.pending && *self.pending_tries.entry(key).or_insert(0) < 8 => {
                    *self.pending_tries.entry(key).or_insert(0) += 1;
                    self.retry.push((Instant::now() + Duration::from_secs(2), d.id, d.size));
                    Thumb::Loading
                }
                None => Thumb::Failed,
            };
            let used = self.mem.get(&key).map(|e| e.1).unwrap_or(self.tick);
            self.mem.insert(key, (t, used));
        }
        self.uploaded_last_frame = n;
        let now = Instant::now();
        let due: Vec<(u64, u32)> = self.retry.iter().filter(|r| r.0 <= now).map(|r| (r.1, r.2)).collect();
        if !due.is_empty() {
            self.retry.retain(|r| r.0 > now);
            let (lock, cv) = &*self.queue;
            if let Ok(mut q) = lock.lock() {
                for d in due {
                    self.inflight.insert(d);
                    q.want.push_back(d);
                }
            }
            cv.notify_all();
        }
        if self.mem.len() > MEM_CAP {
            let mut by_age: Vec<((u64, u32), u64)> = self.mem.iter().map(|(k, v)| (*k, v.1)).collect();
            by_age.sort_by_key(|e| e.1);
            for (k, _) in by_age.into_iter().take(self.mem.len() - MEM_CAP) {
                if !self.inflight.contains(&k) {
                    self.mem.remove(&k);
                }
            }
        }
        !self.ready.is_empty() || !self.inflight.is_empty() || !self.retry.is_empty()
    }
}

fn disk_path(dir: &std::path::Path, id: u64, size: u32) -> PathBuf {
    dir.join(format!("{id}_{size}.png"))
}

fn decode(bytes: &[u8]) -> Option<egui::ColorImage> {
    let img = image::load_from_memory(bytes).ok()?.to_rgba8();
    let (w, h) = img.dimensions();
    if w == 0 || h == 0 || w > 2048 || h > 2048 {
        return None;
    }
    Some(egui::ColorImage::from_rgba_unmultiplied([w as usize, h as usize], img.as_raw()))
}

fn worker(q: Arc<(Mutex<Queue>, Condvar)>, tx: Sender<Decoded>, http: Arc<Mutex<Arc<dyn Http>>>, dir: PathBuf) {
    loop {
        // A batch of the same size.
        let batch: Vec<(u64, u32)> = {
            let (lock, cv) = &*q;
            let Ok(mut g) = lock.lock() else { return };
            while g.want.is_empty() && !g.stop {
                g = match cv.wait(g) {
                    Ok(g) => g,
                    Err(_) => return,
                };
            }
            if g.stop {
                return;
            }
            let size = g.want.front().map(|w| w.1).unwrap_or(SIZE);
            let mut b = Vec::new();
            let mut rest = VecDeque::new();
            while let Some(w) = g.want.pop_front() {
                if w.1 == size && b.len() < BATCH {
                    b.push(w);
                } else {
                    rest.push_back(w);
                }
            }
            g.want = rest;
            b
        };
        let h = http.lock().map(|g| g.clone()).unwrap_or_else(|_| Arc::new(api::AnonHttp));
        let mut need = Vec::new();
        for (id, size) in &batch {
            let p = disk_path(&dir, *id, *size);
            let fresh = std::fs::metadata(&p)
                .ok()
                .and_then(|m| m.modified().ok())
                .and_then(|t| SystemTime::now().duration_since(t).ok())
                .is_some_and(|age| age < DISK_TTL);
            if fresh {
                if let Some(img) = std::fs::read(&p).ok().and_then(|b| decode(&b)) {
                    let _ = tx.send(Decoded { id: *id, size: *size, image: Some(img), pending: false });
                    continue;
                }
            }
            need.push((*id, *size));
        }
        if need.is_empty() {
            continue;
        }
        let size = need[0].1;
        let ids: Vec<u64> = need.iter().map(|n| n.0).collect();
        let listed = api::get_bytes(&*h, &api::thumbnails_url(&ids, size), false, &|| false)
            .ok()
            .and_then(|b| serde_json::from_slice::<serde_json::Value>(&b).ok())
            .map(|v| model::parse_thumbnails(&v))
            .unwrap_or_default();
        for id in ids {
            let row = listed.iter().find(|r| r.0 == id);
            let (image, pending) = match row {
                Some((_, state, Some(url))) if state == "Completed" => {
                    let img = api::get_bytes(&*h, url, false, &|| false).ok().and_then(|b| {
                        let d = decode(&b);
                        if d.is_some() {
                            let _ = std::fs::write(disk_path(&dir, id, size), &b);
                        }
                        d
                    });
                    (img, false)
                }
                Some((_, state, _)) if state == "Pending" => (None, true),
                _ => (None, false),
            };
            let _ = tx.send(Decoded { id, size, image, pending });
        }
    }
}
