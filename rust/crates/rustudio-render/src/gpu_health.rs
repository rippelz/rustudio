//! Says which GPU rustudio runs on and why it lost it. A lost device (driver
//! reset, a Windows TDR on a long frame, out of GPU memory) used to surface
//! as an egui-wgpu staging-buffer panic with no cause; now the reason goes to
//! the diagnostic log and the crash report.

use egui_wgpu::wgpu;
use std::sync::OnceLock;

static ADAPTER: OnceLock<String> = OnceLock::new();
static LOST: OnceLock<String> = OnceLock::new();
/// The first GPU error nothing captured (out of memory, validation), for the
/// editor's Output; taken once.
static UNCAPTURED: std::sync::Mutex<Option<String>> = std::sync::Mutex::new(None);
static UNCAPTURED_COUNT: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

fn describe(info: &wgpu::AdapterInfo) -> String {
    format!(
        "{} ({:?}, {:?}, driver {} {})",
        info.name, info.backend, info.device_type, info.driver, info.driver_info
    )
}

/// Log the adapter and watch the device for loss. Call once per device.
pub fn watch(adapter: &wgpu::Adapter, device: &wgpu::Device) {
    let gpu = describe(&adapter.get_info());
    log::info!(target: "rustudio::gpu", "GPU: {gpu}");
    let _ = ADAPTER.set(gpu);
    // wgpu's default for an error no error scope catches is a panic: an
    // allocation the GPU refused (a 2.4M-instance place on an RTX 4070 Ti,
    // 2026-11-22: "wgpu error: Out of Memory" in Device::create_texture)
    // took the whole editor down. Log it with what failed and keep going;
    // the resource that failed just doesn't draw.
    device.on_uncaptured_error(std::sync::Arc::new(|error: wgpu::Error| {
        let n = UNCAPTURED_COUNT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let gpu = ADAPTER.get().map(String::as_str).unwrap_or("unknown GPU");
        let kind = match &error {
            wgpu::Error::OutOfMemory { .. } => "out of GPU memory",
            wgpu::Error::Validation { .. } => "GPU validation error",
            wgpu::Error::Internal { .. } => "internal GPU error",
        };
        // Every one goes to the diagnostic log (the first 200 in full).
        if n < 200 {
            log::error!(target: "rustudio::gpu", "{kind} on {gpu}: {error:?}");
        }
        if n == 0 {
            let detail = error.to_string();
            let first = detail.lines().next().unwrap_or("").to_string();
            *UNCAPTURED.lock().unwrap() = Some(format!(
                "{kind} on {gpu}: {first}. Some parts or textures may not draw; the diagnostic log has the details."
            ));
        }
    }));
    device.set_device_lost_callback(|reason, message| {
        if matches!(reason, wgpu::DeviceLostReason::Destroyed) {
            return;
        }
        #[cfg(target_os = "android")]
        crate::gpu_compat::note_device_lost();
        let gpu = ADAPTER.get().map(String::as_str).unwrap_or("unknown GPU");
        let text = format!("GPU device lost on {gpu}: {message}");
        log::error!(target: "rustudio::gpu", "{text}");
        let _ = LOST.set(text);
    });
}

/// The first uncaptured GPU error, once (for a warning in Output).
pub fn take_uncaptured() -> Option<String> {
    UNCAPTURED.lock().ok()?.take()
}

/// Why the device was lost, once it has been.
pub fn lost() -> Option<&'static str> {
    LOST.get().map(String::as_str)
}

/// Call at the top of a frame: once the device is lost nothing more can be
/// drawn, so stop with the reason in the crash report (shown on the next
/// start) instead of a frozen window.
pub fn check() {
    if let Some(why) = lost() {
        panic!("{why}. Update the graphics driver; WGPU_BACKEND=dx12 or WGPU_BACKEND=vulkan picks the other graphics API.");
    }
}
