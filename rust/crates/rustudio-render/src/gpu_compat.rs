//! Android shader compatibility and pipeline failure diagnostics.
use egui_wgpu::wgpu;
use std::sync::OnceLock;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

static DEVICE_LOST: AtomicBool = AtomicBool::new(false);
static VIEWPORT_FRAMES: AtomicUsize = AtomicUsize::new(0);

pub fn diagnostic_frame() -> bool {
    enabled()
        && diagnostics_enabled()
        && VIEWPORT_FRAMES.fetch_add(1, Ordering::Relaxed)
            < rustudio_core::mech::MOBILE_GPU_DIAGNOSTIC_FRAMES
}

pub fn checkpoint(device: &wgpu::Device, queue: &wgpu::Queue, label: &str) {
    eprintln!("rustudio GPU execution BEGIN: {label}");
    assert!(!DEVICE_LOST.load(Ordering::SeqCst), "GPU already lost before {label}");
    let submission = queue.submit([]);
    let result = device.poll(wgpu::PollType::Wait {
        submission_index: Some(submission),
        timeout: Some(std::time::Duration::from_secs(rustudio_core::mech::MOBILE_GPU_DIAGNOSTIC_TIMEOUT_SECS)),
    });
    assert!(!DEVICE_LOST.load(Ordering::SeqCst), "GPU lost executing {label}");
    match result {
        Ok(_) => eprintln!("rustudio GPU execution OK: {label}"),
        Err(error) => eprintln!("rustudio GPU execution INCOMPLETE: {label}: {error:?}"),
    }
}

pub fn enabled() -> bool {
    static COMPATIBILITY: OnceLock<bool> = OnceLock::new();
    cfg!(target_os = "android")
        && *COMPATIBILITY.get_or_init(|| std::env::var("RUSTUDIO_FULL_SHADERS").as_deref() != Ok("1"))
}

pub(crate) fn diagnostics_enabled() -> bool {
    static DIAGNOSTICS: OnceLock<bool> = OnceLock::new();
    cfg!(target_os = "android")
        && *DIAGNOSTICS.get_or_init(|| std::env::var_os("RUSTUDIO_MOBILE_GPU_DIAGNOSTICS").is_some())
}

pub fn note_device_lost() {
    DEVICE_LOST.store(true, Ordering::SeqCst);
}

pub fn pipeline(device: &wgpu::Device, desc: &wgpu::RenderPipelineDescriptor<'_>) -> wgpu::RenderPipeline {
    let label = desc.label.unwrap_or("unnamed");
    let diagnostics = diagnostics_enabled();
    if diagnostics {
        eprintln!("rustudio GPU pipeline BEGIN: {label}; fragment={:?}", desc.fragment.as_ref().and_then(|stage| stage.entry_point));
    }
    assert!(!DEVICE_LOST.load(Ordering::SeqCst), "GPU lost before pipeline {label}");
    let pipeline = device.create_render_pipeline(desc);
    assert!(!DEVICE_LOST.load(Ordering::SeqCst), "GPU lost creating pipeline {label}");
    if diagnostics {
        eprintln!("rustudio GPU pipeline OK: {label}");
    }
    pipeline
}

pub fn viewport_source(source: &str) -> String {
    if !enabled() { return source.to_owned(); }
    let mut source = source.to_owned();
    for name in ["fs_part", "fs_terrain", "fs_water", "fs_post"] {
        source = source.replace(&format!("fn {name}("), &format!("fn {name}_desktop("));
    }
    source.push_str(include_str!("shader_mobile.wgsl"));
    // These desktop blur pipelines are built even when depth of field is off.
    // The mobile final pass does not use DoF; avoid compiling the costly kernels.
    for name in ["fs_dof_h", "fs_dof_v", "fs_near_h", "fs_near_v", "fs_near_mask0",
        "fs_near_mask1", "fs_near_mask2", "fs_near_mask3", "fs_near_mask4",
        "fs_near_mask5", "fs_near_edge", "fs_near_bokeh", "fs_near_tent"] {
        source = source.replace(&format!("fn {name}("), &format!("fn {name}_desktop("));
        source.push_str(&format!("\n@fragment fn {name}(in: PostOut) -> @location(0) vec4<f32> {{ return textureSampleLevel(hdr_tex, hdr_sampler, in.uv, 0.0); }}\n"));
    }
    source
}

pub fn probe_source(source: &str) -> String {
    if !enabled() { return source.to_owned(); }
    let mut source = source.replace("fn fs_probe_conv(", "fn fs_probe_conv_desktop(");
    source.push_str(r#"
@fragment
fn fs_probe_conv(in: ProbeOut) -> @location(0) vec4<f32> {
    let dir = face_dir(i32(pp.p.y), in.uv.x * 2.0 - 1.0, 1.0 - in.uv.y * 2.0);
    return vec4<f32>(textureSampleLevel(env, env_sampler, dir, clamp(pp.p.x + 1.0, 0.0, 5.0)).rgb, 1.0);
}
"#);
    source
}

pub fn render_scale_override() -> Option<f32> {
    static SCALE: std::sync::OnceLock<Option<f32>> = std::sync::OnceLock::new();
    *SCALE.get_or_init(|| {
        std::env::var("RUSTUDIO_RENDER_SCALE")
            .ok()
            .and_then(|value| value.trim().parse::<f32>().ok())
            .filter(|scale| scale.is_finite())
            .map(|scale| scale.clamp(0.25, 1.0))
    })
}

