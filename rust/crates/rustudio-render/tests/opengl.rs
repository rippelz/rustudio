//! The renderer on this machine's OpenGL adapter, where there is one (Mesa
//! on Linux, the Windows GL driver of an HD 4600): what WGPU_BACKEND=gl runs.
//! Its own test binary: on an NVIDIA driver, once a GL device has
//! been dropped, the next Vulkan or D3D12 device made in the same process
//! hangs or comes back lost.

use egui_wgpu::wgpu;

#[test]
fn renderer_builds_on_opengl() {
    let instance = wgpu::Instance::new(wgpu::InstanceDescriptor {
        backends: wgpu::Backends::GL,
        ..wgpu::InstanceDescriptor::new_without_display_handle()
    });
    let Ok(adapter) = rustudio_render::gpu_caps::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions::default())) else {
        eprintln!("no OpenGL adapter here: skipped");
        return;
    };
    rustudio_render::render::gpu_check::build_and_draw(&instance, &adapter, None);
}
