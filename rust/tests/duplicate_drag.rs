//! Duplicate (Ctrl+D) mid-drag must hand the drag to the copies, like Studio's
//! DraggerFramework (DraggingParts/DraggingHandle.processSelectionChanged):
//! the originals stay where they were, the selected copies follow the mouse.
//! Runs the real editor's headless self-test (free drag, Move/Scale/Rotate
//! handles, parts and models, 1 or 2 selected, 3 duplicates per drag).
#[test]
fn duplicate_during_drag_moves_the_selection() {
    let dir = std::env::temp_dir().join(format!("rustudio-dupdrag-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let out = dir.join("result.json");
    let status = std::process::Command::new(env!("CARGO_BIN_EXE_rustudio"))
        .arg("--self-test")
        .arg(&out)
        .env("RUSTUDIO_DUPLICATE_DRAG_REGRESSION", "1")
        .status()
        .expect("run rustudio");
    let json = std::fs::read_to_string(&out).unwrap_or_default();
    let _ = std::fs::remove_dir_all(&dir);
    assert!(status.success(), "self-test exited with {status}");
    assert!(json.contains("\"passed\":true"), "self-test result: {json:?}");
}
