//! rustudio: large textures made and cleared ahead of need on another
//! thread. Making a texture is cheap; on integrated GPUs the first use of a
//! large one (the driver allocating and zeroing its memory) costs the
//! submitting thread tens of milliseconds. A texture taken from here holds
//! transparent black (all zero) in every mip level, as a new texture does.

use std::sync::Mutex;

/// What a pooled texture is (2D, one layer, one sample).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Key {
    pub width: u32,
    pub height: u32,
    pub mip_levels: u32,
    pub format: wgpu::TextureFormat,
    /// The usages asked for (RENDER_ATTACHMENT is added for the clear).
    pub usage: wgpu::TextureUsages,
}

struct Pool {
    ready: Vec<(Key, wgpu::Texture)>,
    making: Vec<Key>,
    /// Keys already handed out: not made again (`prewarm_once`).
    served: Vec<Key>,
}

static POOL: Mutex<Pool> = Mutex::new(Pool { ready: Vec::new(), making: Vec::new(), served: Vec::new() });

fn pool() -> std::sync::MutexGuard<'static, Pool> {
    POOL.lock().unwrap_or_else(|e| e.into_inner())
}

/// Starts making a texture for `key` on a thread (once: nothing happens
/// while one is being made or waits).
pub fn prewarm(device: &wgpu::Device, queue: &wgpu::Queue, key: Key) {
    prewarm_inner(device, queue, key, false);
}

/// `prewarm`, unless a texture for `key` was already taken once.
pub fn prewarm_once(device: &wgpu::Device, queue: &wgpu::Queue, key: Key) {
    prewarm_inner(device, queue, key, true);
}

fn prewarm_inner(device: &wgpu::Device, queue: &wgpu::Queue, key: Key, once: bool) {
    {
        let mut p = pool();
        if p.making.contains(&key) || p.ready.iter().any(|(k, _)| *k == key) || (once && p.served.contains(&key)) {
            return;
        }
        p.making.push(key);
    }
    let (device, queue) = (device.clone(), queue.clone());
    let spawned = std::thread::Builder::new().name("gpu-prewarm".into()).spawn(move || {
        let texture = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("prewarmed"),
            size: wgpu::Extent3d { width: key.width, height: key.height, depth_or_array_layers: 1 },
            mip_level_count: key.mip_levels,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: key.format,
            usage: key.usage | wgpu::TextureUsages::RENDER_ATTACHMENT,
            view_formats: &[],
        });
        let mut enc = device.create_command_encoder(&wgpu::CommandEncoderDescriptor { label: Some("prewarm clear") });
        for level in 0..key.mip_levels {
            let view = texture.create_view(&wgpu::TextureViewDescriptor {
                base_mip_level: level,
                mip_level_count: Some(1),
                ..Default::default()
            });
            enc.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("prewarm clear"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &view,
                    resolve_target: None,
                    depth_slice: None,
                    ops: wgpu::Operations { load: wgpu::LoadOp::Clear(wgpu::Color::TRANSPARENT), store: wgpu::StoreOp::Store },
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
                multiview_mask: None,
            });
        }
        queue.submit([enc.finish()]);
        let mut p = pool();
        p.making.retain(|k| *k != key);
        p.ready.push((key, texture));
    });
    if spawned.is_err() {
        pool().making.retain(|k| *k != key);
    }
}

/// A made texture for `key`, if one is ready.
pub fn take(key: Key) -> Option<wgpu::Texture> {
    let mut p = pool();
    let i = p.ready.iter().position(|(k, _)| *k == key)?;
    if !p.served.contains(&key) {
        p.served.push(key);
    }
    if std::env::var_os("RUSTUDIO_ATLAS_TRACE").is_some() {
        eprintln!("[prewarm] took {}x{} ({} levels)", key.width, key.height, key.mip_levels);
    }
    Some(p.ready.swap_remove(i).1)
}

/// Drops ready textures no one took that are not for `keep`.
pub fn drop_except(keep: &[Key]) {
    let mut gone = Vec::new();
    pool().ready.retain_mut(|(k, t)| {
        let keep = keep.contains(k);
        if !keep {
            gone.push(t.clone());
        }
        keep
    });
    drop_later(gone);
}

/// Drops `value` (textures, views, bind groups no longer used) on a thread:
/// freeing a large texture's memory can cost an integrated GPU's driver
/// tens of milliseconds.
pub fn drop_later<T: Send + 'static>(value: T) {
    type Job = Box<dyn Send>;
    static DROPPER: Mutex<Option<std::sync::mpsc::Sender<Job>>> = Mutex::new(None);
    let mut d = DROPPER.lock().unwrap_or_else(|e| e.into_inner());
    if d.is_none() {
        let (tx, rx) = std::sync::mpsc::channel::<Job>();
        if std::thread::Builder::new().name("gpu-drop".into()).spawn(move || for job in rx { drop(job) }).is_ok() {
            *d = Some(tx);
        }
    }
    match d.as_ref() {
        Some(tx) => {
            if let Err(e) = tx.send(Box::new(value)) {
                drop(e);
            }
        }
        None => drop(value),
    }
}

/// Destroys `textures` (and drops `rest`) on the drop thread: `destroy`
/// frees the memory there, instead of when wgpu next collects resources on
/// the thread that submits. Only for textures nothing will draw with again.
pub fn destroy_later<T: Send + 'static>(textures: Vec<wgpu::Texture>, rest: T) {
    struct Destroy(Vec<wgpu::Texture>);
    impl Drop for Destroy {
        fn drop(&mut self) {
            for t in &self.0 {
                t.destroy();
            }
        }
    }
    drop_later((Destroy(textures), rest));
}

/// What the window painter runs before it touches the queue: rustudio's
/// render thread join (the viewport texture egui shows was drawn there).
static BEFORE_PAINT: std::sync::OnceLock<fn()> = std::sync::OnceLock::new();

pub fn set_before_paint(f: fn()) {
    let _ = BEFORE_PAINT.set(f);
}

#[cfg_attr(not(feature = "winit"), allow(dead_code))]
pub(crate) fn before_paint() {
    if let Some(f) = BEFORE_PAINT.get() {
        f();
    }
}
