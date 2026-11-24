//! The renderer on DX12 where there is one (Windows), FXC compiling every
//! pass, and on WARP, the software DX12 adapter an HD 4600 without a DX12
//! driver falls back to. Its own test binary: on an RTX 3070 a D3D12
//! device made in a process that has already made and dropped another
//! backend's device came back "Parent device is lost".

use egui_wgpu::wgpu;

#[test]
fn renderer_builds_on_dx12_and_warp() {
    // (no debug flags: FXC then optimizes, as in release builds, and only
    // an optimized compile runs its UAV race check)
    let instance = wgpu::Instance::new(wgpu::InstanceDescriptor {
        backends: wgpu::Backends::DX12,
        flags: wgpu::InstanceFlags::empty(),
        ..wgpu::InstanceDescriptor::new_without_display_handle()
    });
    for force_fallback_adapter in [false, true] {
        let options = wgpu::RequestAdapterOptions { force_fallback_adapter, ..Default::default() };
        let Ok(adapter) = rustudio_render::gpu_caps::block_on(instance.request_adapter(&options)) else {
            eprintln!("no DX12 adapter here (fallback {force_fallback_adapter}): skipped");
            continue;
        };
        rustudio_render::render::gpu_check::build_and_draw(&instance, &adapter, None);
    }
}
