//! File > Studio Settings survives a restart and takes effect: the real
//! editor's headless self-test changes settings through the dialog (clicks
//! and typing) and closes it, then a second rustudio on the same settings
//! file reads them back and checks the camera, selection outline, Output
//! and script editor follow them (src/selftest/studio_settings.rs).
#[test]
fn studio_settings_persist_across_restart_and_take_effect() {
    let dir = std::env::temp_dir().join(format!("rustudio-studio-settings-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let file = dir.join("studio-settings.json");
    let run = |phase: &str| {
        let out = dir.join(format!("{phase}.json"));
        let status = std::process::Command::new(env!("CARGO_BIN_EXE_rustudio"))
            .arg("--self-test")
            .arg(&out)
            .env("RUSTUDIO_STUDIO_SETTINGS", &file)
            .env("RUSTUDIO_STUDIO_SETTINGS_REGRESSION", phase)
            .env("RUSTUDIO_NO_AUDIO", "1")
            .status()
            .expect("run rustudio");
        let json = std::fs::read_to_string(&out).unwrap_or_default();
        assert!(status.success(), "{phase}: self-test exited with {status}");
        assert!(json.contains("\"passed\":true"), "{phase}: {json}");
        json
    };
    run("write");
    let saved = std::fs::read_to_string(&file).unwrap_or_default();
    for key in ["Studio.Camera Speed", "Studio.Camera Zoom to Mouse Position", "Studio.Selection Line Thickness"] {
        assert!(saved.contains(key), "{key} not saved: {saved}");
    }
    let check = run("check");
    eprintln!("{check}");
    let _ = std::fs::remove_dir_all(&dir);
}
