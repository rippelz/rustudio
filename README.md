<p align="center">
  <img src="assets/icons/rustudio-256.png" width="128" alt="rustudio icon">
</p>

<h1 align="center">rustudio</h1>

<p align="center">
  <a href="https://discord.gg/pdVZpADpWr"><img src="https://img.shields.io/badge/Discord-join%20the%20server-5865F2?logo=discord&logoColor=white" alt="Join the rustudio Discord"></a>
</p>

**A native Linux/Windows/macOS Roblox Studio, written in Rust.** Open, edit and save
Roblox places, press Play and run them with real Luau, physics, characters
and GUIs, without Wine.

Rustudio is very much a beta/prototype, I wouldn't call it a studio replacement
quite yet.

![Editing a place in rustudio](docs/images/editor.png)

![Playing a test place in rustudio](docs/images/play-mode.png)

![The rustudio home screen](docs/images/home.png)

> This project is still early, so please please help!! Contributions are super welcome. Come hang out, ask for help or report bugs on the [Discord](https://discord.gg/pdVZpADpWr).

## Features

- **Editor:** Explorer, Properties, Output, script tabs, move/scale/rotate
  tools, terrain brushes, pop-out panels, undo/redo, and Studio's menus and
  shortcuts.
- **Files:** `.rbxl` / `.rbxlx` places, `.rbxm` / `.rbxmx` models, and
  Rojo/Argon-style project folders (File > Save as Project Folder…).
- **File Sync:** attach a Rojo or Argon project to any open place, live Team
  Create sessions included.
- **Play / Run:** separate server and client, remotes, physics that follows
  Studio's solver, R15/R6 characters, sounds, tweens, 2D and 3D GUIs, and
  teleports between places of the same experience.
- **Luau:** the real Luau VM with most of the engine API real games use
  ([what's missing](docs/ENGINE-GAPS.md)).
- **Rendering:** wgpu, with lighting, shadows, sky, atmosphere, materials,
  meshes and terrain tuned against Studio, plus FBX import.
- **Plugins and Toolbox:** Studio plugins run in the editor (local or from
  the Creator Store), and the Toolbox searches the Creator Store and your
  inventory.
- **Roblox account:** open, save and publish your places, load assets and
  avatars, use DataStores, and join Team Create.
- **MCP server:** AI agents can read and edit an open place, run Luau, press
  Play and send input (`rustudio mcp`).
- **Profiler:** a MicroProfiler window (Ctrl+F6) and command-line captures
  ([docs/PROFILING.md](docs/PROFILING.md)).

## Install

Download the latest build from the
[releases page](https://github.com/rippelz/rustudio/releases):

- **Linux (x86_64):** extract the archive and run `./install.sh`. It installs
  into `~/.local` and adds rustudio to your app launcher. Run it again from a
  newer archive to upgrade; `./install.sh --uninstall` removes it.
- **Windows (x64):** extract the ZIP and double-click `install.cmd` (no admin
  rights needed), or run `rustudio.exe` directly as a portable copy.
  `uninstall.cmd` in the install folder removes it.
- **macOS 11+ (Apple Silicon and Intel):** unzip, move `rustudio.app` to
  Applications, and the first time right-click it > **Open** (it isn't
  notarized).

Uninstalling keeps your projects, settings, sign-in and downloaded content.
Downloaded copies check for updates on start (verified against the release's
`SHA256SUMS`); set `RUSTUDIO_NO_UPDATE_CHECK=1` to turn that off.

### Build from source

You need Rust 1.95 or newer and a GPU with Vulkan or OpenGL (Metal on macOS).

```sh
git clone https://github.com/rippelz/rustudio
cd rustudio
cargo build --release
./target/release/rustudio --demo           # sample scene
./target/release/rustudio my_place.rbxl    # open a place
```

On Linux, `./install.sh` builds and installs it for your user
(`--no-build` installs an existing `target/release/rustudio`; `--help` for
more). On macOS, `tools/macos/bundle.sh` builds the `.app`. The standalone
player isn't in the downloads; build it with `cargo build -p rustudio-player`.

### Studio content

On first launch rustudio downloads Roblox Studio's built-in content (default
characters, player scripts, fonts, textures, sounds; about 60 MB) from
Roblox's CDN. Run `rustudio --install-studio-content` to fetch or update it
ahead of time. If you already have Studio installed (e.g. through
[Vinegar](https://vinegarhq.org), or `RobloxStudio.app` on a Mac), rustudio
can use that instead:

```sh
export RUSTUDIO_STUDIO_DIR=~/.var/app/org.vinegarhq.Vinegar/data/vinegar/versions/version-XXXX
```

## Platforms

| Platform | Status |
|---|---|
| Linux x86_64 (Wayland or X11) | Supported |
| Linux ARM64 | Should build; untested |
| macOS 11+ (Apple Silicon and Intel) | Builds and runs (tested on Apple Silicon) |
| Windows x86_64 (MSVC) | Builds and runs (tested on Windows 11) |

CI builds and tests on Linux, Windows and macOS (ARM and Intel).

- **Windows:** data and caches live in `%LOCALAPPDATA%\rustudio`, settings
  in `%APPDATA%\rustudio`. Video playback is off, and native crashes don't
  write a crash report (panics do).
- **macOS:** data and settings live in `~/Library/Application Support/rustudio`,
  caches in `~/Library/Caches/rustudio`. Shortcuts follow Studio for Mac.
  Video playback needs `brew install libvpx opus`.

## Signing in

Everything above works signed out. To open your own places or private
assets, use the account button at the top right and choose **Sign in with
your browser**, or paste your `.ROBLOSECURITY` cookie. rustudio keeps the
session in your system keyring (the Keychain on macOS). See
[docs/AUTH.md](docs/AUTH.md).

## Docs

- [DEVELOPING.md](docs/DEVELOPING.md): workspace layout, command-line options and tests
- [MECHANICS.md](docs/MECHANICS.md): every tuning number and where it comes from
- [ENGINE-GAPS.md](docs/ENGINE-GAPS.md): engine API that's still missing
- [TEAMCREATE.md](docs/TEAMCREATE.md): Team Create
- [PROFILING.md](docs/PROFILING.md): profiler captures and the CLI that reads them
- [MATERIALS.md](docs/MATERIALS.md): where material textures come from
- [STUDIO-PANELS.md](docs/STUDIO-PANELS.md): panels and pop-out windows
- [UI-PARITY.md](docs/UI-PARITY.md): how the UI was matched to Studio

## Not affiliated with Roblox

rustudio is an independent project. It is not made, endorsed or supported
by Roblox Corporation. "Roblox" and "Roblox Studio" are trademarks of Roblox
Corporation.

This repository doesn't redistribute Roblox Studio or its content. Studio's
built-in content is downloaded from Roblox's own CDN onto your machine on
first launch.

## License

MIT OR Apache-2.0, at your option. Vendored crates in `vendor/` keep their
own licences.
