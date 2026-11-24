# Using RuStudioAPK

## First playtest

RuStudio downloads movement and camera scripts first. If you tap **Play** while they are still downloading, the playtest starts when they are ready. The progress dialog lets you retry after a failure. Other assets continue downloading in the background.

## Landscape editor

**Editor** opens a panel covering 75% of the workspace. Tap **Expand** or drag the handle upward to fill the workspace. Tap **Viewport** or drag the handle down to return to the scene. When the keyboard is open, the editor fills the available space above it.

Swipe the top row or ribbon to reach off-screen controls. The arrow on the right collapses or expands the ribbon. Explorer, Properties, and File Sync dock on the right. Toolbox and Terrain Editor dock on the left. Output opens below the viewport. Tap **X** in a panel header to close it.

## Play and files

- In Edit mode, use **Select**, **Orbit**, and **Look**. Pinch to zoom and use two fingers to pan.
- In Play mode, drag outside the movement controls to turn the camera. Use the toolbar to open the game menu, chat, or player list, or to stop the playtest.
- Shared files are stored in `/storage/emulated/0/RuStudio`. The app also has private storage.
- Plugin dialogs can open, save, or select multiple files. File Sync can open a Rojo or Argon project. The MicroProfiler can open and export recordings using the built-in file picker.

## Diagnostic logs

RuStudio saves `rustudio-*.log` files under **Internal storage → RuStudio → Logs** when it has access to shared storage at startup. Otherwise, it writes them to `RuStudio/Logs` in private app storage.

Use **Menu → Help → Share Diagnostic Logs** to share recent sessions, including a session that crashed. Each directory keeps up to five sessions, for up to 14 days, with a limit of roughly 4 MiB per file. Review the logs before attaching them to a report.

## Configuration

Set `KEY=VALUE` lines in `rustudio_env.txt` in the shared RuStudio folder or the app's private files directory.

- `RUSTUDIO_RENDER_SCALE=0.6` overrides the Performance panel's 3D render scale (0.25–1.0).
- `RUSTUDIO_TOUCH_TRACE=1` logs Play camera input to logcat.
- `RUSTUDIO_UI_SCALE=1.0` sets the UI scale (0.5–3.0).
- `RUSTUDIO_FULL_SHADERS=1` selects the full graphics profile.
- `RUSTUDIO_MOBILE_GPU_DIAGNOSTICS=1` logs pipeline creation and GPU checkpoints in the first frames of the compatibility profile.

The MicroProfiler collects data while its panel is visible. A manually requested capture can continue when you switch panels. Live script statistics are enabled while the **Scripts** tab is selected.

Android uses simplified shaders and single-sample depth by default. See the [Android changelog](CHANGELOG.md).
