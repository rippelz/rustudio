//! The freeform drags measured in Studio, replayed through the real editor
//! (src/selftest/drag_parity.rs; the numbers are part_drag::studio_cases).
#[test]
fn studio_drags_through_the_editor() {
    let dir = std::env::temp_dir().join(format!("rustudio-dragparity-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let out = dir.join("result.json");
    let status = std::process::Command::new(env!("CARGO_BIN_EXE_rustudio"))
        .arg("--self-test")
        .arg(&out)
        .env("RUSTUDIO_DRAG_PARITY_REGRESSION", "1")
        .env("RUSTUDIO_NO_AUDIO", "1")
        .status()
        .expect("run rustudio");
    let json = std::fs::read_to_string(&out).unwrap_or_default();
    let _ = std::fs::remove_dir_all(&dir);
    assert!(status.success(), "self-test exited with {status}");
    assert!(json.contains("\"passed\":true"), "self-test result: {json}");
}
