#![cfg_attr(target_os = "windows", windows_subsystem = "windows")]

#[global_allocator]
static ALLOC: rustudio_core::profiler::alloc::CountingAlloc = rustudio_core::profiler::alloc::CountingAlloc;

fn main() -> eframe::Result<()> {
    rustudio_core::profiler::alloc::CountingAlloc::mark_installed();
    rustudio_player::player_app::run()
}
