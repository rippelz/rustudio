# Android changelog

## 0.5.1-android.3

- The Chat button opens and closes the in-game chat and releases text focus when closed.
- Touches on game UI controls reach their buttons without starting a camera gesture. The mobile toolbar includes a player list shortcut.

## 0.5.1-android.2

- The Play viewport shows one toolbar, with the RuStudio icon for guests and the Roblox icon for signed-in accounts.
- The sky shader uses invariant vertex positions for the depth test.

## 0.5.1-android.1

- Updated the Android port to Rustudio 0.5.1.
- Added per-session logs in `RuStudio/Logs` and sharing through the Help menu.

## 0.3.3-android.8

- Terrain grass uses the Grass color from `Terrain.MaterialColors`. The renderer prepares one decoration chunk per frame, starting with the nearest chunk, and skips terrain packs the mobile shader does not use.

## 0.3.3-android.7

- Matched the desktop fog curve and atmospheric colors to fix color inversion in places using Atmosphere and Sky, including Flat Terrain.

## 0.3.3-android.6

- Play downloads movement and camera scripts first, queues Play requests while they download, and retries missing Studio textures and meshes after installation.
- Sized the floating joystick and jump button to the viewport, added a fallback icon font, and suspended the watchdog when Android suspends the window.

## 0.3.3-android.4

- Delivered touch motion and virtual button events in order. After a pinch, the remaining finger resumes camera drag.

## 0.3.3-android.3

- Routed camera drags directly to client Play input and added the RuStudio, chat, and stop icons to the touch toolbar.

## 0.3.3-android.2

- Fixed Android startup by linking Clang's ARM64 runtime for Luau's `__clear_cache` dependency and rejecting unresolved native references.

## 0.3.3-android.1

- Based the port on [Rustudio v0.3.3](https://github.com/rippelz/rustudio/releases/tag/v0.3.3), commit `74fb620821a7d7866048a2cab641fdd09ee09563`. The package ID is `com.rustudio.app`, with version code 9.
- File Sync and the MicroProfiler use the in-app file picker.
- The compatibility profile uses single-sample depth for the viewport and 3D UI, keeps water occlusion, and uses ordinary transparency for glass.
- The Performance panel controls render scale, which defaults to 50% for this release. Presentation defaults to vertical sync. Early GPU checkpoints are available through `RUSTUDIO_MOBILE_GPU_DIAGNOSTICS=1`.
- Preserved gestures, the soft keyboard, private credential storage, and plugin dialogs.

### Notes for this release

The compatibility profile uses simplified lighting and omits SSAO, volumetric clouds, bloom, and depth of field. Device validation should cover opening a place, touch gestures, Play, dialogs, water, and 3D UI. The optional Windows file-picker check uses `cargo test --manifest-path rust/tools/android-dialog-check/Cargo.toml` with Rust and Visual C++ configured.

Play may report `could not resolve alias "rbx"` when a script calls `require("@rbx/AvatarAbilities")`. This release does not implement that alias.
