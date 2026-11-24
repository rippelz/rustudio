//! The GPU driver's pipeline cache, kept between runs. A Vulkan driver
//! compiles each render pipeline's shaders when the pipeline is made
//! (`ViewportPipelines::new` and the lazy builders: dozens of them at the
//! first frame), and a phone's driver does it slowly: seconds of black
//! window on an Adreno or Mali, where the vendor's driver keeps no cache
//! of its own between processes (Mesa's drivers keep a disk shader cache
//! of their own, so they gain less here). With the `PIPELINE_CACHE` feature (Vulkan only) wgpu wraps a
//! `VkPipelineCache`; its blob is written to the asset cache folder once
//! the pipelines are built and handed back to the driver at the next
//! start, which then skips the compiles it recognises.
//!
//! One cache per process ([`init`] by whoever makes the device: one
//! device here); every pipeline descriptor passes [`current`] as its
//! `cache`. wgpu checks the blob's header (adapter, driver version) and
//! starts empty when it doesn't match (`fallback`), so a driver update
//! costs one slow start, not a crash. `RUSTUDIO_NO_PIPELINE_CACHE=1`
//! turns it off.
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Mutex, OnceLock};
use std::time::Instant;

use egui_wgpu::wgpu;

struct Cache {
    cache: wgpu::PipelineCache,
    path: PathBuf,
    /// Pipelines made ([`current`] calls) when the blob was last written,
    /// and its length then.
    saved: Mutex<(u64, usize)>,
}

static CACHE: OnceLock<Option<Cache>> = OnceLock::new();
/// Pipeline descriptors that asked for the cache ([`current`] calls).
static MADE: AtomicU64 = AtomicU64::new(0);
/// When the last one asked, as milliseconds since [`epoch`].
static LAST_MADE_MS: AtomicU64 = AtomicU64::new(0);

fn epoch() -> Instant {
    static T0: OnceLock<Instant> = OnceLock::new();
    *T0.get_or_init(Instant::now)
}

fn now_ms() -> u64 {
    epoch().elapsed().as_millis() as u64
}

/// The blob's file for this adapter under `dir` (None: a backend without a
/// pipeline cache, OpenGL and the rest).
fn file_for(dir: &std::path::Path, info: &wgpu::AdapterInfo) -> Option<PathBuf> {
    let key = wgpu::util::pipeline_cache_key(info)?;
    Some(dir.join("pipelines").join(format!("{key}.bin")))
}

/// Makes the process's cache for `device`, primed with the blob a previous
/// run left under `dir` (the asset cache folder). Nothing happens without
/// the device feature (the descriptor asks for it where the adapter has
/// it: offscreen.rs `wgpu_options`), or a second time.
pub fn init(device: &wgpu::Device, info: &wgpu::AdapterInfo, dir: PathBuf) {
    let _ = CACHE.get_or_init(|| {
        if std::env::var_os("RUSTUDIO_NO_PIPELINE_CACHE").is_some() {
            return None;
        }
        if !device.features().contains(wgpu::Features::PIPELINE_CACHE) {
            log::info!("pipeline cache: not kept ({:?} has no PIPELINE_CACHE feature)", info.backend);
            return None;
        }
        let path = file_for(&dir, info)?;
        let data = std::fs::read(&path).ok();
        let t0 = Instant::now();
        // SAFETY: the data is what `get_data` of a cache made here returned
        // (`save`), or anything else on disk: wgpu validates its header and
        // the driver its own, and `fallback` makes an empty cache instead
        // of using a blob it doesn't recognise.
        let cache = unsafe {
            device.create_pipeline_cache(&wgpu::PipelineCacheDescriptor {
                label: Some("rustudio pipeline cache"),
                data: data.as_deref(),
                fallback: true,
            })
        };
        let primed = data.as_ref().map_or(0, |d| d.len());
        log::info!(
            "pipeline cache: {} ({}, {} ms)",
            path.display(),
            if primed > 0 { format!("primed with {} KB from the last run", primed / 1024) } else { "empty: the first run on this driver".to_string() },
            t0.elapsed().as_millis()
        );
        if rustudio_core::load_trace::startup_trace() {
            eprintln!("[startup] pipeline cache {}", if primed > 0 { format!("primed with {} KB", primed / 1024) } else { "empty (first run on this driver)".to_string() });
        }
        Some(Cache { cache, path, saved: Mutex::new((0, primed)) })
    });
}

/// The cache for a pipeline descriptor's `cache` field (None before
/// [`init`], without the feature, or when switched off).
pub fn current() -> Option<&'static wgpu::PipelineCache> {
    let c = CACHE.get()?.as_ref()?;
    MADE.fetch_add(1, Ordering::Relaxed);
    LAST_MADE_MS.store(now_ms(), Ordering::Relaxed);
    Some(&c.cache)
}

/// Writes the blob when pipelines were made since the last write and none
/// for the last two seconds (a set is built in a burst; one write covers
/// it), or at once with `force`. Cheap otherwise: called once a frame.
pub fn save(force: bool) {
    let Some(c) = CACHE.get().and_then(|c| c.as_ref()) else { return };
    let made = MADE.load(Ordering::Relaxed);
    let Ok(mut saved) = c.saved.try_lock() else { return };
    if made == saved.0 {
        return;
    }
    if !force && now_ms().saturating_sub(LAST_MADE_MS.load(Ordering::Relaxed)) < 2000 {
        return;
    }
    let t0 = Instant::now();
    let Some(data) = c.cache.get_data() else {
        saved.0 = made;
        return;
    };
    // (the driver may hand back the same blob when nothing new compiled:
    // a primed run whose pipelines were all known)
    if data.len() == saved.1 && saved.0 != 0 {
        saved.0 = made;
        return;
    }
    let write = || -> std::io::Result<()> {
        if let Some(parent) = c.path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let tmp = c.path.with_extension(format!("tmp{}", std::process::id()));
        std::fs::write(&tmp, &data)?;
        std::fs::rename(&tmp, &c.path)
    };
    match write() {
        Ok(()) => log::info!(
            "pipeline cache: {} KB written after {} pipeline(s) ({} ms)",
            data.len() / 1024,
            made - saved.0,
            t0.elapsed().as_millis()
        ),
        Err(e) => log::warn!("pipeline cache: writing {}: {e}", c.path.display()),
    }
    *saved = (made, data.len());
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nothing_before_init() {
        // (another test may have initialised the process's cache: only the
        // file naming is checked here)
        let info = wgpu::AdapterInfo {
            name: "x".into(),
            vendor: 0x5143,
            device: 0x6030001,
            device_type: wgpu::DeviceType::IntegratedGpu,
            device_pci_bus_id: String::new(),
            driver: String::new(),
            driver_info: String::new(),
            backend: wgpu::Backend::Vulkan,
            subgroup_min_size: 0,
            subgroup_max_size: 0,
            transient_saves_memory: None,
            limit_bucket: None,
        };
        let f = file_for(std::path::Path::new("/c"), &info).unwrap();
        assert_eq!(f, PathBuf::from("/c/pipelines/wgpu_pipeline_cache_vulkan_20803_100859905.bin"));
        let mut gl = info.clone();
        gl.backend = wgpu::Backend::Gl;
        assert!(file_for(std::path::Path::new("/c"), &gl).is_none());
    }
}
