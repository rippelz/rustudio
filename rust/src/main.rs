//! rustudio's entry point. The program itself is the library (`src/lib.rs`),
//! so `tests/` and `examples/` can use `rustudio::…` instead of including
//! source files by path.
//!
//! This file stays a binary on purpose: the attributes below are the ones that
//! describe how the *executable* is presented to the OS.

// Launch as a GUI application on Windows without opening a console window.
// (Here, not in the library: it is the binary the OS starts.)
#![cfg_attr(target_os = "windows", windows_subsystem = "windows")]

// Counts bytes allocated per profiler scope while the profiler asks for it
// (one atomic load per allocation otherwise).
#[global_allocator]
static ALLOC: rustudio_core::profiler::alloc::CountingAlloc = rustudio_core::profiler::alloc::CountingAlloc;

fn main() -> eframe::Result<()> {
    rustudio_core::profiler::alloc::CountingAlloc::mark_installed();
    rustudio_core::profiler::set_build(option_env!("RUSTUDIO_GIT_SHA").unwrap_or(""));
    let result = rustudio::run();
    match &result {
        Ok(()) => log::info!("RusTudio exited"),
        Err(error) => log::error!("RusTudio startup or event loop failed: {error}"),
    }
    result
}
