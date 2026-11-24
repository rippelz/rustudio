// Rust / lib.rs / cargo test / Android file-selection logic on the host
extern crate self as rustudio_core;
extern crate self as rustudio_engine;
pub mod paths { pub fn data() -> std::path::PathBuf { std::env::temp_dir() } }
pub mod theme {
    pub const WARN: egui::Color32 = egui::Color32::YELLOW;
    pub const ACCENT: egui::Color32 = egui::Color32::BLUE;
}
#[path = "../../../crates/rustudio-profiler-ui/src/android_dialog.rs"]
pub mod android_dialog;
