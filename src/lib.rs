//! rustudio: a native editor for Roblox place and model files.
//!
//! This crate is the studio: the editor (`app::Editor`), its panels and
//! viewport, the plugin host and plugin API, the toolbox, the self-test and
//! project sync, plus the play code until it moves to `rustudio-player`.
//! Everything it builds on is a crate under `crates/` — `rustudio-core`
//! (document, math, assets, logging), `rustudio-physics` (solver, bodies,
//! collision, queries), `rustudio-render` (wgpu renderer, scene, lighting),
//! `rustudio-engine` (Luau runtime, API domains, replication, GUI, audio,
//! rigs), `rustudio-account` (the Roblox session) and `rustudio-net` (Team
//! Create) — and every use of them names the crate.
//!
//! What is not added here: code that belongs to a crate below (a domain, a
//! renderer pass, a solver step) and code that only the player needs. An
//! import that points up from one of those crates does not compile, which is
//! the boundary this crate sits on top of.
//!
//! `src/main.rs` is the GUI entry point and calls [`run`].

pub mod anim_editor;
pub mod asset_manager;
pub mod plugin_store;
pub mod toolbox;
pub mod ui_pointer;
pub mod update;
pub mod account_ui;
pub mod char_builder;
pub mod ribbon_quick;
#[cfg(target_os = "macos")]
pub mod macos;
pub mod cloud_ui;
pub mod join_ui;
pub mod app;
mod native_loop;
pub use rustudio_player::{account, asset_report, emote_wheel, game, game_view, keys, offscreen, run_harness, player_app, play};
pub mod frame_watchdog;
pub mod class_icons;
pub mod bin_dump;
#[cfg(unix)]
pub mod control;
pub mod coverage;
pub mod explorer;
pub mod file_dialog;
pub mod hypr;
pub mod studio_settings;
pub mod fidelity;
pub mod engine_tables;
pub mod icons;
pub mod live_drafts;
pub mod live_sync;
pub mod mcp;
pub mod file_sync;
pub mod layout_store;
mod panel_cache;
pub mod panels;
pub mod part_drag;
pub mod output_panel;
pub mod props_layout;
pub mod prop_slider;
pub mod color_editors;
pub mod plugin_host;
pub mod project;
pub mod project_config;
pub mod project_export;
pub mod project_ui;
pub mod project_sync;
pub mod properties;
pub mod script_editor;
pub mod selftest;
pub mod terrain_editor;
pub mod terrain_ui;
pub mod terrain_brush;
pub mod terrain_gen;
pub mod studio_ui;
pub mod studio_actions;
pub mod studio_hooks;
pub mod studio_ribbon;
pub mod service_visibility;
pub mod studio_panels;
pub mod start_page;
pub mod group_icons;
pub mod ui_shots;
pub mod viewport;
pub mod viewport_menu;

use std::path::PathBuf;

const USAGE: &str = "\
rustudio — edit Roblox places (.rbxl/.rbxlx) and models (.rbxm/.rbxmx)

usage: rustudio [FILE] [--demo] [--play] [--screenshot OUT.png] [--self-test OUT.json]
                [--bench OUT.json [--seconds N]]

  FILE                 open a place or model on start
  --demo               start with the sample scene
  --open-roblox ID|URL open a place from Roblox (place id, roblox.com/games/… or
                       create.roblox.com/…/places/… link) once signed in; with
                       --local-copy, a Team Create place opens as a local copy
  --roblox-picker TAB  show File > Open from Roblox (recent, mine, groups, shared),
                       or File > Experience Settings (settings)
  --play               start Play (F5) right after loading
  --team-create U P    join the live Team Create server for universe U, place P
  --no-ui              no rustudio UI: the window (exactly --window WxH) shows the
                       3D view alone, no menus, ribbon, Explorer, Properties,
                       Output or status bar (none of them is built). Without
                       --play: the place in Edit state through the edit camera
                       (--camera), scene only: grid, selection, gizmos, hover,
                       view cube and readouts are off (--overlays keeps them).
                       With --play: the game as a player sees it (ScreenGuis,
                       cursor, the Esc menu); every key goes to the game, F1–F12
                       and Escape included (Escape opens the menu and reaches
                       UserInputService as gameProcessedEvent true); no editor
                       shortcut (F5, Shift+F5, F8, …) and no --action.
                       --click/--move/--ldrag/--wheel X,Y are window pixels.
                       --shots writes one picture per time, FILE-<t>s.png, the
                       view at the window's size (no FILE-<t>s.view.png; the
                       .frames.json / .cursor.json beside it as in the editor)
  --play-only          --no-ui --play
  --overlays           with --no-ui in Edit state: keep the editor's overlays
  --headless           no window, no compositor, no display server (no gamescope
                       or Xvfb needed): frames are drawn offscreen at --window
                       WxH and read back for shots; 60 frames a second (unpaced
                       with --bench). Silent unless --audio. Hides Play Core UI
                       (topbar, leaderboard, chat) by default unless --show-core-ui
                       or RUSTUDIO_SHOW_CORE_UI=1. With --no-ui or --play-only only
  --show-core-ui       show Play Core UI (topbar, leaderboard, chat, backpack)
  --no-core-ui         hide Play Core UI while keeping game UI (PlayerGui) visible
  --no-render          --headless with no GPU at all: scripts, physics, sound
                       state and the harness run, nothing is drawn; --shots
                       write the log and frame times but no picture. The Output
                       log is a rendered run's minus the renderer's own lines
                       (Sky: …)
  --audio              with --headless / --no-render: play sound (default silent)
  --fixed-dt DT        deterministic stepping: every frame advances the game by
                       exactly DT (1/60, 0.0166 or 60hz) and frames run as fast
                       as the machine allows. Harness times (--key, --luau,
                       --shots, --seconds), os.clock, tick, os.time, time,
                       DateTime.now, task.wait, RunService deltas and
                       FrameTime, tweens, animations, sound positions and
                       physics all follow the simulated clock; unseeded
                       math.random / Random.new / BrickColor.Random take seeds
                       in a fixed order. Not deterministic: when downloaded
                       assets arrive, table/function addresses in tostring,
                       GUIDs. tick / os.time / os.date / DateTime.now start at
                       2026-01-01 00:00 UTC (RUSTUDIO_EPOCH=SECONDS to change)
  --screenshot FILE    draw ~120 frames, save the window as PNG, exit
  --self-test FILE     drive the editor with scripted input, write results, exit
  --shot-after S       with --screenshot: take it S seconds after start
  --shots T1,T2,…      with --screenshot: a shot (window and viewport) at each time,
                       the Output log and frame times with each (FILE-<t>s.png …)
  --from-play          harness times (shots, --luau, --key) count from Play start
  --write-corpus DIR   write the fidelity corpus places (demo, control) and exit
  --plugin FILE        load this Studio plugin (.lua/.luau/.rbxm/.rbxmx) once the place
                       is open, as a local plugin (user_<file name>); repeatable
  --plugins-dir DIR    load every plugin file in DIR, in Studio's order (.lua, .luau,
                       .rbxm, .rbxmx, each by name)
  --no-plugins         no plugin loads at all (also RUSTUDIO_NO_PLUGINS=1)
  --bench-vsync        keep the interactive presentation mode with --bench
  --installed-plugin ID:FILE  load FILE as the installed plugin ID (cloud_<ID>)
  --action plugin-button:TOOLBAR/BUTTON@S, --action plugin-action:ACTIONID@S,
  --action plugin-menu:CHOICE@S (answers an open PluginMenu:ShowAsync: N, N.M,
                       an action id or text, or escape)
                       fire a plugin toolbar button's Click / a PluginAction's
                       Triggered at S seconds (works with --no-ui)
  --api-coverage OUT [GAPS…]  measure API coverage against app/api-dump.json
                       (RUSTUDIO_GAPS files from real runs add call gaps)
  --dump-scripts PLACE DIR  write every script's source under DIR
  --log FILE           with --screenshot: write the Output lines to FILE
  --key NAME@S[:HOLD]  press a key at S seconds (held HOLD seconds)
  --shift S:HOLD       hold Shift from S seconds for HOLD seconds
  --luau server|client FILE@S  run the Luau in FILE on that side of Play at S
                       seconds (with --play; see --from-play)
  --action NAME@S      run a Studio menu action by id at S seconds (e.g. panel_output)
  --move X,Y@S         move the pointer to view position X,Y (hover)
  --wheel N@S[:X,Y]    turn the mouse wheel N notches over the 3D view's centre
                       (or at view position X,Y)
  --type TEXT@S        type TEXT at S seconds (into a focused TextBox)
  --rdrag DX,DY@S      right-drag over the 3D view by DX,DY pixels
  --rhold S:HOLD       hold the right mouse button from S seconds for HOLD seconds
  --ldrag X1,Y1>X2,Y2@S  left-drag in the 3D view between view positions
  --lhold X,Y@S:HOLD   press the left button at X,Y at S seconds, let go HOLD seconds later
  --click X,Y@S        click the 3D view at X,Y (pixels) S seconds after start
  --ui-click X,Y@S     click anywhere in the window (panels, menus) at window
                       pixels X,Y S seconds after start (hover, press, release)
  --key NAME@S         press a key (egui name, e.g. Enter, B) S seconds after start
  --window WxH         window size for test runs
  --camera X,Y,Z,YAW,PITCH  start the editor camera there (degrees)
  --bench FILE         measure frame times (after a 3 s warm-up) for N seconds
                       (default 10) at 1265x900 without vsync, write JSON, exit
  --profile-capture OUT  record a profiler capture of N seconds (--seconds,
                       default 10; after the --bench warm-up, from Play's start
                       with --play) and exit. OUT.rsprof opens in the
                       MicroProfiler; any other name is Chrome trace JSON
                       (Perfetto, chrome://tracing)
  --instr              with --profile-capture: scopes also count instructions
                       (Linux; Windows counts CPU cycles); reports then rank by them
  --profile-report FILE [TOP] [--json] [--spikes N]  a capture (.rsprof or
                       Chrome trace) as text or JSON: frames, lanes, the TOP
                       hottest scopes with source lines, the N worst frames and
                       what made them slow, counters, scripts (docs/PROFILING.md)
  --profile-diff A B [TOP] [--json]  compare two captures: frames and scopes,
                       change and whether it is outside run-to-run noise
  --profile-convert IN OUT  rewrite a capture (OUT.rsprof native, else Chrome trace)
  --seconds N          with --bench: how long to measure; with --no-ui (no
                       --bench): end the run at N seconds (from Play's start
                       with --from-play)
  --result FILE        at the end of a test run, one JSON: why it ended (shots,
                       seconds, bench, EndTest), exit code, Edit or Play, the
                       EndTest value, error and warning counts, frame times
                       (count, mean, p50, p99, max, work), the shots written,
                       CPU time and peak RSS, and the whole Output log
  --fail-on-error      exit 1 if the Output has any error line (script errors,
                       --luau errors, engine errors shown in red)
                       A server script ends a --no-ui (or --result) run with
                       game:GetService(\"StudioTestService\"):EndTest(value): Play
                       stops, --log / --result are written, the process exits
  --ui-shots DIR       pictures of the ribbon tabs, menus and panels in DIR, exit

tools (no window; each exits when done):
  --stats PLACE        counts of what is in a place
  --script PLACE PATTERN  print the scripts whose full name contains PATTERN
  --fetch-assets DIR ID…  download public assets by id into DIR
  --install-studio-content  download Studio's built-in content from Roblox's CDN
  --list-plugins [--json]   the local and installed plugins rustudio would load
  --install-plugin ID       Get (free) + download a Creator Store plugin; --update-plugin ID
  --uninstall-plugin ID     remove an installed plugin from rustudio
  --enable-plugin ID / --disable-plugin ID   (cloud_<n>, user_<file>, asset id or file name)
  --sync-plugins            read your installed plugins from Roblox
  --import-studio-plugins [N]  list Studio installs / copy install N's plugins in
  --search-plugins WORDS    Creator Store plugin search
  --publish-plugin FILE NAME [--overwrite ID] [--dry-run]  upload a model as a plugin
  --export-project OUT_DIR PLACE [--lua] [--no-sourcemap] [--containers A,B/C]
                       write a place out as a Rojo / Argon project folder
  --sourcemap FOLDER|FILE.project.json [OUT.json] [--include-non-scripts]
                       write Rojo's sourcemap.json (stdout without OUT)
  --build OUT.rbxl FOLDER|FILE.project.json  save a project folder as one place
  --solver-ticks SCENE.json [--out FILE]  step one physics lab scene tick by tick
                       and print every body's state

physics (environment):
  RUSTUDIO_SOLVER=studio   Studio's own (legacy) solver: the default when unset
  RUSTUDIO_SOLVER=rapier   the old physics (rapier), as before the switch; use it
                       for a place that uses what the Studio solver does not
                       cover yet (rustudio names the class once when it sees one)
";

/// Test runs (--screenshot / --self-test) re-launch themselves inside a headless
/// gamescope so they never open a window on the user's screen.
/// RUSTUDIO_VISIBLE_TESTS=1 keeps them on screen.
#[cfg(target_os = "linux")]
fn rerun_headless() -> Option<i32> {
    use std::os::unix::fs::OpenOptionsExt;
    let argv: Vec<String> = std::env::args().collect();
    let testing = is_test_run(&argv);
    // (--headless / --no-render need no display at all: src/offscreen.rs)
    let offscreen = argv.iter().any(|a| a == "--headless" || a == "--no-render");
    if !testing
        || offscreen
        || std::env::var_os("RUSTUDIO_HEADLESS_CHILD").is_some()
        || std::env::var_os("RUSTUDIO_VISIBLE_TESTS").is_some()
    {
        return None;
    }
    // The benchmark measures at the 1265x900 size the person asked for.
    let h = if argv.iter().any(|a| a == "--bench") {
        "900"
    } else {
        "1376"
    };
    // --window WxH: another window size (e.g. to give the 3D view Studio's size).
    let (w, h) = window_arg(&argv)
        .map(|(w, h)| (w.to_string(), h.to_string()))
        .unwrap_or(("1265".into(), h.into()));
    // gamescope's own lines (and its Xwayland's: swapchain notes, "failed to
    // read Wayland events" at exit) go to a file. gamescope closes inherited
    // descriptors, so rustudio inside writes to two FIFOs (RUSTUDIO_PARENT_OUT
    // / _ERR, see restore_parent_fds) that this process copies to its own
    // stdout / stderr.
    let tmp = std::env::temp_dir().join(format!("rustudio-headless-{}", std::process::id()));
    let _ = std::fs::create_dir_all(&tmp);
    let gs_log = tmp.join("gamescope.log");
    let gs_out = std::fs::File::create(&gs_log).ok();
    rustudio_core::load_trace::mark("headless: starting gamescope");
    let mut cmd = std::process::Command::new("gamescope");
    let mut pumps = Vec::new();
    if let Some(f) = gs_out {
        let fifos = ["out", "err"].map(|n| tmp.join(n));
        let made = fifos.iter().all(|p| {
            let c = std::ffi::CString::new(p.as_os_str().as_encoded_bytes()).unwrap_or_default();
            // SAFETY: mkfifo on a NUL-terminated path.
            unsafe { libc::mkfifo(c.as_ptr(), 0o600) == 0 }
        });
        if let (true, Ok(f2)) = (made, f.try_clone()) {
            cmd.stdout(f)
                .stderr(f2)
                .env("RUSTUDIO_PARENT_OUT", &fifos[0])
                .env("RUSTUDIO_PARENT_ERR", &fifos[1])
                // (lsfg-vk's frame generation layer, when installed, only
                // adds errors about its own config to a test run.)
                .env("DISABLE_LSFGVK", "1");
            for (i, p) in fifos.iter().enumerate() {
                let p = p.clone();
                let (tx, rx) = std::sync::mpsc::channel::<()>();
                std::thread::spawn(move || {
                    // (Line by line, leaving out the notes gamescope's Vulkan
                    // WSI layer prints from inside rustudio's process.)
                    if let Ok(r) = std::fs::File::open(&p) {
                        use std::io::{BufRead, Write};
                        let mut r = std::io::BufReader::new(r);
                        let mut line = Vec::new();
                        let mut wsi = false;
                        while r.read_until(b'\n', &mut line).is_ok_and(|n| n > 0) {
                            // (its blocks continue on indented lines)
                            wsi = line.starts_with(b"[Gamescope WSI]") || (wsi && line.starts_with(b"  "));
                            if !wsi {
                                let _ = if i == 0 {
                                    let mut o = std::io::stdout().lock();
                                    o.write_all(&line).and_then(|_| o.flush())
                                } else {
                                    std::io::stderr().write_all(&line)
                                };
                            }
                            line.clear();
                        }
                    }
                    let _ = tx.send(());
                });
                pumps.push((fifos[i].clone(), rx));
            }
        }
    }
    // gamescope exits 0 even if its client panics or is killed.
    // Without the client status, a run with no screenshot looked successful.
    // A shell inside records rustudio's own exit status (128 + signal when
    // killed); no status at all means the run was killed from outside.
    let status_file = tmp.join("status");
    let status = cmd
        .args([
            "--backend",
            "headless",
            "-W",
            &w,
            "-H",
            &h,
            "-w",
            &w,
            "-h",
            &h,
            "--",
            "sh",
            "-c",
            "\"$0\" \"$@\"; echo $? > \"$RUSTUDIO_HEADLESS_STATUS\"",
        ])
        .args(&argv)
        .env("RUSTUDIO_HEADLESS_CHILD", "1")
        .env("RUSTUDIO_HEADLESS_STATUS", &status_file)
        .status()
        .ok(); // no gamescope: fall back to a normal window
    let inner: Option<i32> = std::fs::read_to_string(&status_file).ok().and_then(|s| s.trim().parse().ok());
    for (fifo, done) in pumps {
        // (A FIFO rustudio never opened: open its write end so the copier's
        // open returns, then let it see the end.)
        let _ = std::fs::OpenOptions::new()
            .write(true)
            .custom_flags(libc::O_NONBLOCK)
            .open(&fifo);
        let _ = done.recv_timeout(std::time::Duration::from_secs(2));
    }
    use std::io::Write;
    let _ = std::io::stdout().flush();
    let gs_text = std::fs::read_to_string(&gs_log).unwrap_or_default();
    let _ = std::fs::remove_dir_all(&tmp);
    let status = status?;
    let code = match (inner, status.code()) {
        (Some(0), _) => 0,
        (Some(c), _) if c > 128 => {
            eprintln!("rustudio: the test run was killed by signal {} (inside the headless gamescope)", c - 128);
            c
        }
        (Some(c), _) => c,
        (None, Some(_)) => {
            eprintln!(
                "rustudio: the test run inside the headless gamescope ended without an exit status \
                 (killed from outside, e.g. a pkill of gamescope / gamescopereaper)"
            );
            1
        }
        (None, None) => {
            eprintln!("rustudio: the headless gamescope was killed by a signal before the test run finished");
            1
        }
    };
    // A failed run shows gamescope's lines after all (it may never have
    // started rustudio).
    if code != 0 && !gs_text.is_empty() {
        eprintln!("gamescope output:\n{gs_text}");
    }
    Some(code)
}

#[cfg(not(target_os = "linux"))]
fn rerun_headless() -> Option<i32> {
    None
}

/// Inside the headless gamescope: write to the launching process's stdout /
/// stderr (FIFOs it copies from, RUSTUDIO_PARENT_OUT / _ERR), not gamescope's.
#[cfg(target_os = "linux")]
fn restore_parent_fds() {
    for (var, to) in [("RUSTUDIO_PARENT_OUT", 1), ("RUSTUDIO_PARENT_ERR", 2)] {
        let Some(p) = std::env::var_os(var) else { continue };
        // (Our own children, e.g. a nested test run, don't inherit it.)
        std::env::remove_var(var);
        if let Ok(f) = std::fs::OpenOptions::new().write(true).open(&p) {
            use std::os::fd::IntoRawFd;
            let fd = f.into_raw_fd();
            // SAFETY: dup2 / close on a descriptor we just opened.
            unsafe {
                libc::dup2(fd, to);
                libc::close(fd);
            }
        }
    }
}

#[cfg(not(target_os = "linux"))]
fn restore_parent_fds() {}

/// A harness run that ends by itself (pictures, a self-test, a benchmark,
/// a CI result, or a --no-ui run with --seconds): no window on screen.
fn is_test_run(argv: &[String]) -> bool {
    let has = |f: &str| argv.iter().any(|a| a == f);
    ["--screenshot", "--self-test", "--bench", "--profile-capture", "--ui-shots", "--result", "--fail-on-error"].iter().any(|f| has(f))
        || ((has("--no-ui") || has("--play-only")) && has("--seconds"))
}

fn window_arg(argv: &[String]) -> Option<(u32, u32)> {
    let i = argv.iter().position(|a| a == "--window")?;
    let (w, h) = argv.get(i + 1)?.split_once('x')?;
    Some((w.parse().ok()?, h.parse().ok()?))
}

/// The whole program: parse the command line and run the editor, a tool
/// command or a harness run (`src/main.rs` calls this).
pub fn run() -> eframe::Result<()> {
    // `rustudio mcp`: the stdio MCP bridge to the open windows (src/mcp/
    // bridge.rs). No window, nothing else on stdout.
    {
        let argv: Vec<String> = std::env::args().collect();
        if argv.get(1).is_some_and(|a| a == "mcp") {
            std::process::exit(mcp::bridge::run(&argv[2..]));
        }
    }
    // Headless runs are silent unless --audio (before any thread starts).
    {
        let argv: Vec<String> = std::env::args().collect();
        let headless = argv.iter().any(|a| a == "--headless" || a == "--no-render");
        if headless && !argv.iter().any(|a| a == "--audio") && std::env::var_os("RUSTUDIO_NO_AUDIO").is_none() {
            std::env::set_var("RUSTUDIO_NO_AUDIO", "1");
        }
        // (lsfg-vk's frame generation layer, when installed, only adds
        // errors about its own config to a run that presents nothing.)
        if headless && std::env::var_os("DISABLE_LSFGVK").is_none() {
            std::env::set_var("DISABLE_LSFGVK", "1");
        }
        // No update check (and so no "Update available" notice in the
        // corner) in headless and harness runs: their captures are compared
        // picture by picture.
        if (headless || is_test_run(&argv)) && std::env::var_os("RUSTUDIO_NO_UPDATE_CHECK").is_none() {
            std::env::set_var("RUSTUDIO_NO_UPDATE_CHECK", "1");
        }
    }
    restore_parent_fds();
    rustudio_core::diagnostics::init();
    log::info!(
        "RusTudio {} ({}) on {}/{}",
        env!("CARGO_PKG_VERSION"),
        option_env!("RUSTUDIO_GIT").unwrap_or("unknown commit"),
        std::env::consts::OS,
        std::env::consts::ARCH
    );
    rustudio_core::load_trace::init();
    // First: a panic or fatal signal from here on leaves a report.
    rustudio_core::crash::install();
    // Studio's installations of the engine's start-up hooks (the plugin API
    // domain; see src/studio_hooks.rs). Before any command below makes a
    // runtime, so the engine's tables have the registered members.
    studio_hooks::install();
    // RUSTUDIO_CRASH_TEST=panic|segv|thread: check the reports themselves.
    match std::env::var("RUSTUDIO_CRASH_TEST").as_deref() {
        Ok("panic") => panic!("RUSTUDIO_CRASH_TEST panic"),
        Ok("thread") => {
            let _ = std::thread::Builder::new()
                .name("crash-test".into())
                .spawn(|| panic!("RUSTUDIO_CRASH_TEST thread panic"))
                .map(|h| h.join());
        }
        // SAFETY: deliberately faults, to test the signal report.
        Ok("segv") => unsafe { libc::raise(libc::SIGSEGV); },
        _ => {}
    }
    // os.clock counts from here, as Studio's does from its process start.
    let _ = rustudio_engine::runtime::process_start();
    // Before any thread makes a Luau VM: the flags are process-global.
    rustudio_engine::runtime::luau_flags();
    rustudio_render::matpack::init_from_env();
    // RUSTUDIO_PLACE_CONTEXT=ID: ask asset delivery in that Roblox place's
    // context for a local file (a copy of a Roblox place).
    if let Some(p) = std::env::var("RUSTUDIO_PLACE_CONTEXT").ok().and_then(|v| v.parse().ok()) {
        rustudio_account::session::set_place_context(p);
    }
    // Plugin store commands (src/plugin_store/cli.rs), no window.
    {
        let argv: Vec<String> = std::env::args().collect();
        if let Some(code) = plugin_store::cli::run(&argv) {
            std::process::exit(code);
        }
        if let Some(code) = toolbox::cli::run(&argv) {
            std::process::exit(code);
        }
    }
    // `--install-studio-content`: download Studio's built-in content from
    // Roblox's CDN into ~/.local/share/rustudio/studio, then exit.
    if std::env::args().any(|a| a == "--install-studio-content") {
        match rustudio_core::studio_content::install(&|s| eprintln!("studio content: {s}")) {
            Ok(_) => std::process::exit(0),
            Err(e) => {
                eprintln!("studio content: {e}");
                std::process::exit(1);
            }
        }
    }
    // `--write-corpus DIR`: the fidelity corpus's place files, then exit.
    let argv: Vec<String> = std::env::args().collect();
    if let Some(i) = argv.iter().position(|a| a == "--write-corpus") {
        let dir = argv.get(i + 1).map(PathBuf::from).unwrap_or_else(|| PathBuf::from("."));
        match fidelity::write_corpus(&dir) {
            Ok(()) => std::process::exit(0),
            Err(e) => {
                eprintln!("write-corpus: {e}");
                std::process::exit(1);
            }
        }
    }
    // `--solver-ticks SCENE.json [--out FILE]`: one compiled physics scene
    // stepped tick by tick, every body's state printed (crates/rustudio-physics/src/solver_ticks.rs).
    if let Some(i) = argv.iter().position(|a| a == "--solver-ticks") {
        let scene = argv.get(i + 1).cloned().unwrap_or_default();
        let out = argv.iter().position(|a| a == "--out").and_then(|j| argv.get(j + 1)).map(|s| s.as_str());
        match rustudio_physics::solver_ticks::run(&scene, out) {
            Ok(()) => std::process::exit(0),
            Err(e) => {
                eprintln!("solver-ticks: {e}");
                std::process::exit(2);
            }
        }
    }
    // `--export-project OUT_DIR PLACE.rbxl [--lua] [--containers A,B/C]`: a
    // place written out as a Rojo / Argon project folder (src/project_export.rs).
    if let Some(i) = argv.iter().position(|a| a == "--export-project") {
        let (Some(out), Some(src)) = (argv.get(i + 1), argv.get(i + 2)) else {
            eprintln!("usage: rustudio --export-project OUT_DIR PLACE.rbxl [--lua] [--no-sourcemap] [--containers ReplicatedStorage,StarterPlayer/StarterPlayerScripts,...]");
            std::process::exit(2);
        };
        let out = PathBuf::from(out);
        let mut o = project_export::ExportOptions {
            name: out.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| "game".into()),
            lua: argv.iter().any(|a| a == "--lua"),
            sourcemap: !argv.iter().any(|a| a == "--no-sourcemap"),
            ..Default::default()
        };
        if let Some(j) = argv.iter().position(|a| a == "--containers") {
            o.containers = argv.get(j + 1).map(|s| s.split(',').map(String::from).collect()).unwrap_or_default();
        }
        let res = rustudio_core::doc::Document::open(std::path::Path::new(src)).and_then(|d| project_export::export(&d, &out, &o));
        match res {
            Ok(r) => {
                for w in &r.warnings {
                    eprintln!("warning: {w}");
                }
                println!(
                    "{}: {} scripts, {} folders, {} models",
                    r.project_file.display(), r.scripts, r.folders, r.models
                );
                std::process::exit(0);
            }
            Err(e) => {
                eprintln!("export-project: {e}");
                std::process::exit(1);
            }
        }
    }
    // `--sourcemap FOLDER [OUT] [--include-non-scripts]`: Rojo's sourcemap.json.
    if let Some(i) = argv.iter().position(|a| a == "--sourcemap") {
        let Some(src) = argv.get(i + 1) else {
            eprintln!("usage: rustudio --sourcemap FOLDER|FILE.project.json [OUT.json] [--include-non-scripts]");
            std::process::exit(2);
        };
        let src = PathBuf::from(src);
        let file = if src.is_dir() { project::project_files(&src).into_iter().next() } else { Some(src.clone()) };
        let all = argv.iter().any(|a| a == "--include-non-scripts");
        let res = file
            .ok_or_else(|| format!("no *.project.json in {}", src.display()))
            .and_then(|f| project_sync::open(&f))
            .map(|(doc, st)| st.sourcemap_bytes(&doc, all));
        match res {
            Ok(bytes) => {
                match argv.get(i + 2).filter(|a| !a.starts_with("--")) {
                    Some(out) => {
                        if let Err(e) = std::fs::write(out, &bytes) {
                            eprintln!("sourcemap: {out}: {e}");
                            std::process::exit(1);
                        }
                    }
                    None => {
                        use std::io::Write;
                        let _ = std::io::stdout().write_all(&bytes);
                    }
                }
                std::process::exit(0);
            }
            Err(e) => {
                eprintln!("sourcemap: {e}");
                std::process::exit(1);
            }
        }
    }
    // `--build OUT.rbxl FOLDER`: a project folder (its default project file,
    // over the place file it names) saved as one place file, then exit.
    if let Some(i) = argv.iter().position(|a| a == "--build") {
        let (Some(out), Some(src)) = (argv.get(i + 1), argv.get(i + 2)) else {
            eprintln!("usage: rustudio --build OUT.rbxl FOLDER|FILE.project.json");
            std::process::exit(2);
        };
        let src = PathBuf::from(src);
        let file = if src.is_dir() {
            project::project_files(&src).into_iter().next()
        } else {
            Some(src.clone())
        };
        let res = file
            .ok_or_else(|| format!("no *.project.json in {}", src.display()))
            .and_then(|f| project_sync::open(&f))
            .and_then(|(mut doc, _)| doc.save_to(std::path::Path::new(out)));
        match res {
            Ok(()) => std::process::exit(0),
            Err(e) => {
                eprintln!("build: {e}");
                std::process::exit(1);
            }
        }
    }
    if let Some(code) = rerun_headless() {
        std::process::exit(code);
    }
    // First launch without a Studio install: fetch Studio's built-in content
    // in the background (never in test runs, which must be reproducible).
    let test_run = is_test_run(&std::env::args().collect::<Vec<_>>());
    if !test_run {
        rustudio_core::studio_content::spawn_background_install_if_missing();
    }
    let mut open: Option<PathBuf> = None;
    let mut screenshot: Option<PathBuf> = None;
    let mut selftest: Option<PathBuf> = None;
    let mut ui_shots: Option<PathBuf> = None;
    let mut demo = false;
    let mut start_play = false;
    let mut bench: Option<PathBuf> = None;
    let mut profile_capture: Option<PathBuf> = None;
    let mut profile_instr = false;
    let mut camera: Option<[f32; 5]> = None;
    let mut seconds = rustudio_core::mech::PERF_SECONDS;
    let mut shot_after: Option<f32> = None;
    let mut shots: Vec<f32> = Vec::new();
    let mut from_play = false;
    let mut bench_vsync = false;
    let mut log_out: Option<PathBuf> = None;
    let mut actions: Vec<(f32, app::HarnessAction)> = Vec::new();
    let mut open_roblox: Option<u64> = None;
    let mut team_create: Option<(u64, u64)> = None;
    let mut roblox_picker: Option<cloud_ui::PickerTab> = None;
    let mut roblox_settings = false;
    let mut local_copy = false;
    let mut no_ui = false;
    let mut overlays = false;
    let mut headless = false;
    let mut no_render = false;
    let mut show_core_ui: Option<bool> = None;
    let mut seconds_given = false;
    let mut fixed_dt: Option<f64> = None;
    let mut fail_on_error = false;
    let mut result: Option<PathBuf> = None;
    let mut plugin_files: Vec<PathBuf> = Vec::new();
    let mut installed_plugins: Vec<(u64, PathBuf)> = Vec::new();
    let mut plugin_dirs: Vec<PathBuf> = Vec::new();
    let mut no_plugins = false;
    let mut args = std::env::args().skip(1);
    while let Some(a) = args.next() {
        match a.as_str() {
            "--screenshot" => screenshot = args.next().map(PathBuf::from),
            "--bench" => bench = args.next().map(PathBuf::from),
            "--profile-capture" => profile_capture = args.next().map(PathBuf::from),
            "--instr" => profile_instr = true,
            "--camera" => {
                let v: Vec<f32> = args
                    .next()
                    .unwrap_or_default()
                    .split(',')
                    .filter_map(|x| x.trim().parse().ok())
                    .collect();
                if v.len() == 5 {
                    camera = Some([v[0], v[1], v[2], v[3], v[4]]);
                }
            }
            "--shot-after" => shot_after = args.next().and_then(|s| s.parse().ok()),
            "--shots" => {
                shots = args
                    .next()
                    .map(|v| v.split(',').filter_map(|t| t.trim().parse().ok()).collect())
                    .unwrap_or_default()
            }
            "--from-play" => from_play = true,
            "--bench-vsync" => bench_vsync = true,
            "--log" => log_out = args.next().map(PathBuf::from),
            "--ui-click" => {
                let v = args.next().unwrap_or_default();
                match app::HarnessAction::parse_ui_click(&v) {
                    Some(x) => actions.extend(x),
                    None => eprintln!("--ui-click: can't read {v:?} (X,Y@S, window pixels)"),
                }
            }
            "--click" | "--key" => {
                let v = args.next().unwrap_or_default();
                match app::HarnessAction::parse(&v, a == "--click") {
                    Some(x) => actions.extend(x),
                    None => eprintln!("{a}: can't read {v:?}"),
                }
            }
            "--dblclick" => {
                let v = args.next().unwrap_or_default();
                match app::HarnessAction::parse_dblclick(&v) {
                    Some(x) => actions.extend(x),
                    None => eprintln!("--dblclick: can't read {v:?} (X,Y@SECONDS)"),
                }
            }
            "--paste" => {
                let v = args.next().unwrap_or_default();
                match app::HarnessAction::parse_paste(&v) {
                    Some(x) => actions.push(x),
                    None => eprintln!("--paste: can't read {v:?} (TEXT@SECONDS)"),
                }
            }
            "--move" => {
                // The pointer to view position X,Y at S (hover, no buttons).
                let v = args.next().unwrap_or_default();
                match v.split_once('@').and_then(|(xy, at)| {
                    let (x, y) = xy.split_once(',')?;
                    Some((
                        at.trim().parse::<f32>().ok()?,
                        app::HarnessAction::PointerAt(x.trim().parse().ok()?, y.trim().parse().ok()?),
                    ))
                }) {
                    Some(x) => actions.push(x),
                    None => eprintln!("--move: can't read {v:?} (X,Y@SECONDS)"),
                }
            }
            "--wheel" => {
                let v = args.next().unwrap_or_default();
                match app::HarnessAction::parse_wheel(&v) {
                    Some(x) => actions.push(x),
                    None => eprintln!("--wheel: can't read {v:?} (NOTCHES@SECONDS or NOTCHES@SECONDS:X,Y)"),
                }
            }
            "--type" => {
                let v = args.next().unwrap_or_default();
                match app::HarnessAction::parse_type(&v) {
                    Some(x) => actions.push(x),
                    None => eprintln!("--type: can't read {v:?} (TEXT@SECONDS)"),
                }
            }
            "--shift" => {
                let v = args.next().unwrap_or_default();
                match v.split_once(':').and_then(|(a, h)| {
                    Some((a.trim().parse::<f32>().ok()?, h.trim().parse::<f32>().ok()?))
                }) {
                    Some((at, hold)) => {
                        actions.push((at, app::HarnessAction::Shift(true)));
                        actions.push((at + hold, app::HarnessAction::Shift(false)));
                    }
                    None => eprintln!("--shift: can't read {v:?} (SECONDS:HOLD)"),
                }
            }
            "--action" => {
                let v = args.next().unwrap_or_default();
                match v.rsplit_once('@').and_then(|(n, at)| Some((at.trim().parse::<f32>().ok()?, n.trim().to_string()))) {
                    Some((at, name)) => actions.push((at, app::HarnessAction::UiAction(name))),
                    None => eprintln!("--action: can't read {v:?} (NAME@SECONDS)"),
                }
            }
            "--ldrag" => {
                let v = args.next().unwrap_or_default();
                match app::HarnessAction::parse_ldrag(&v) {
                    Some(list) => actions.extend(list),
                    None => eprintln!("--ldrag: can't read {v:?} (X1,Y1>X2,Y2@SECONDS)"),
                }
            }
            "--lhold" => {
                let v = args.next().unwrap_or_default();
                match app::HarnessAction::parse_lhold(&v) {
                    Some(list) => actions.extend(list),
                    None => eprintln!("--lhold: can't read {v:?} (X,Y@SECONDS:HOLD)"),
                }
            }
            "--rhold" => {
                let v = args.next().unwrap_or_default();
                match app::HarnessAction::parse_rhold(&v) {
                    Some(x) => actions.extend(x),
                    None => eprintln!("--rhold: can't read {v:?} (SECONDS:HOLD)"),
                }
            }
            "--rdrag" => {
                let v = args.next().unwrap_or_default();
                match app::HarnessAction::parse_rdrag(&v) {
                    Some(x) => actions.extend(x),
                    None => eprintln!("--rdrag: can't read {v:?} (DX,DY@SECONDS)"),
                }
            }
            "--luau" => {
                // --luau server|client FILE@S
                let side = args.next().unwrap_or_default();
                let v = args.next().unwrap_or_default();
                match v.rsplit_once('@').and_then(|(f, at)| {
                    Some((std::fs::read_to_string(f).ok()?, at.parse::<f32>().ok()?))
                }) {
                    Some((code, at)) => {
                        actions.push((at, app::HarnessAction::Luau(side != "client", code)))
                    }
                    None => eprintln!("--luau: can't read {v:?} (FILE@SECONDS)"),
                }
            }
            "--window" => {
                args.next();
            }
            "--seconds" => {
                seconds = args.next().and_then(|s| s.parse().ok()).unwrap_or(seconds);
                seconds_given = true;
            }
            "--fail-on-error" => fail_on_error = true,
            "--fixed-dt" => {
                let v = args.next().unwrap_or_default();
                match rustudio_core::sim_clock::parse_dt(&v) {
                    Some(dt) => fixed_dt = Some(dt),
                    None => {
                        eprintln!("--fixed-dt: can't read {v:?} (1/60, 0.0166 or 60hz)");
                        std::process::exit(2);
                    }
                }
            }
            "--result" => result = args.next().map(PathBuf::from),
            "--plugin" => plugin_files.extend(args.next().map(PathBuf::from)),
            // An installed (Creator Store) plugin: ASSETID:FILE, named cloud_<id>.
            "--installed-plugin" => {
                if let Some((id, f)) = args.next().as_deref().and_then(|v| v.split_once(':')) {
                    if let Ok(id) = id.parse() {
                        installed_plugins.push((id, PathBuf::from(f)));
                    }
                }
            }
            "--plugins-dir" => plugin_dirs.extend(args.next().map(PathBuf::from)),
            "--no-plugins" => no_plugins = true,
            "--self-test" => selftest = args.next().map(PathBuf::from),
            "--ui-shots" => ui_shots = args.next().map(PathBuf::from),
            "--stats" => {
                // Counts of what's in a place (no window).
                let path = args.next().map(PathBuf::from).unwrap_or_default();
                match rustudio_core::doc::Document::open(&path) {
                    Ok(d) => print!("{}", d.stats()),
                    Err(e) => eprintln!("{e}"),
                }
                return Ok(());
            }
            "--profile-report" => {
                // A profiler capture as text or JSON (no window):
                // --profile-report FILE [TOP] [--json] [--spikes N]
                let path = args.next().map(PathBuf::from).unwrap_or_default();
                let (mut top, mut json, mut spikes) = (30usize, false, 5usize);
                while let Some(a) = args.next() {
                    match a.as_str() {
                        "--json" => json = true,
                        "--spikes" => spikes = args.next().and_then(|n| n.parse().ok()).unwrap_or(spikes),
                        n => top = n.parse().unwrap_or(top),
                    }
                }
                match rustudio_core::profiler::export::open(&path) {
                    Ok(cap) if json => println!("{}", serde_json::to_string_pretty(&rustudio_core::profiler::report::report_json(&cap, top, spikes)).unwrap_or_default()),
                    Ok(cap) => print!("{}", rustudio_core::profiler::report::report_text(&cap, top, spikes)),
                    Err(e) => {
                        eprintln!("{e}");
                        std::process::exit(1);
                    }
                }
                return Ok(());
            }
            "--profile-diff" => {
                // Before / after: --profile-diff A B [TOP] [--json]
                let a = args.next().map(PathBuf::from).unwrap_or_default();
                let b = args.next().map(PathBuf::from).unwrap_or_default();
                let (mut top, mut json) = (30usize, false);
                for x in args.by_ref() {
                    match x.as_str() {
                        "--json" => json = true,
                        n => top = n.parse().unwrap_or(top),
                    }
                }
                let open = |p: &PathBuf| rustudio_core::profiler::export::open(p);
                match (open(&a), open(&b)) {
                    (Ok(ca), Ok(cb)) if json => println!("{}", serde_json::to_string_pretty(&rustudio_core::profiler::report::diff_json(&ca, &cb, top)).unwrap_or_default()),
                    (Ok(ca), Ok(cb)) => print!("{}", rustudio_core::profiler::report::diff_text(&ca, &cb, top)),
                    (Err(e), _) | (_, Err(e)) => {
                        eprintln!("{e}");
                        std::process::exit(1);
                    }
                }
                return Ok(());
            }
            "--profile-convert" => {
                // Rewrites a capture in another format (no window): IN OUT,
                // OUT.rsprof native, anything else Chrome trace JSON.
                let input = args.next().map(PathBuf::from).unwrap_or_default();
                let out = args.next().map(PathBuf::from).unwrap_or_default();
                match rustudio_core::profiler::export::open(&input)
                    .and_then(|cap| rustudio_core::profiler::export::save(&cap, &out).map_err(|e| e.to_string()))
                {
                    Ok(()) => println!("{} -> {}", input.display(), out.display()),
                    Err(e) => eprintln!("{e}"),
                }
                return Ok(());
            }
            "--api-coverage" => {
                // Measures API coverage against app/api-dump.json (no window):
                // --api-coverage OUT [GAPS.json …] (real-run gap files).
                let out = args.next().map(PathBuf::from).unwrap_or_else(|| "coverage.json".into());
                let gaps: Vec<PathBuf> = args.by_ref().map(PathBuf::from).collect();
                match coverage::run(&out, &gaps) {
                    Ok(r) => {
                        for k in ["functions", "properties", "events", "callbacks"] {
                            println!(
                                "{k}: exists {} / real {} / {} (game scripts: exists {} / real {} / {})",
                                r[k]["exists"], r[k]["implemented"], r[k]["total"],
                                r[k]["game_scripts_exists"], r[k]["game_scripts_implemented"],
                                r[k]["game_scripts_total"]
                            );
                        }
                    }
                    Err(e) => eprintln!("coverage: {e}"),
                }
                return Ok(());
            }
            "--dump-scripts" => {
                // Writes every script's full source under DIR as <Full.Name>.luau (no window).
                let path = args.next().map(PathBuf::from).unwrap_or_default();
                let dir = args.next().map(PathBuf::from).unwrap_or_default();
                match rustudio_core::doc::Document::open(&path) {
                    Ok(d) => {
                        let _ = std::fs::create_dir_all(&dir);
                        let mut n = 0;
                        for (name, src) in d.all_script_sources() {
                            if std::fs::write(dir.join(format!("{name}.luau")), src).is_ok() {
                                n += 1;
                            }
                        }
                        println!("wrote {n} scripts to {}", dir.display());
                    }
                    Err(e) => eprintln!("{e}"),
                }
                return Ok(());
            }
            "--fetch-assets" => {
                // Downloads public assets by id (rustudio's cache path) into
                // DIR as <id>.<ext>; a texture pack's maps come along. Used to
                // give the offline Studio the terrain packs (round 21).
                let dir = args.next().map(PathBuf::from).unwrap_or_default();
                let _ = std::fs::create_dir_all(&dir);
                let mut queue: Vec<u64> = args.by_ref().filter_map(|a| a.parse().ok()).collect();
                let mut done = std::collections::HashSet::new();
                while let Some(id) = queue.pop() {
                    if !done.insert(id) {
                        continue;
                    }
                    match rustudio_core::assets::public_cached(id) {
                        Ok(b) => {
                            let text = std::str::from_utf8(&b).ok();
                            let ext = if b.starts_with(b"\x89PNG") {
                                "png"
                            } else if b.starts_with(b"\xff\xd8\xff") {
                                "jpg"
                            } else if text.is_some_and(|t| t.contains("texturepack_version")) {
                                "texturepack.xml"
                            } else {
                                "bin"
                            };
                            if let Some(p) = text.and_then(|t| rustudio_core::texture_pack::parse_pack(t).ok()) {
                                queue.extend([p.color, p.normal, p.roughness, p.metalness, p.height].into_iter().flatten());
                            }
                            let out = dir.join(format!("{id}.{ext}"));
                            match std::fs::write(&out, &b[..]) {
                                Ok(()) => println!("{id} {} bytes -> {}", b.len(), out.display()),
                                Err(e) => eprintln!("{id}: {e}"),
                            }
                        }
                        Err(e) => eprintln!("{id}: {e}"),
                    }
                }
                return Ok(());
            }
            "--script" => {
                // Prints the source of scripts whose full name contains PATTERN (no window).
                let path = args.next().map(PathBuf::from).unwrap_or_default();
                let pat = args.next().unwrap_or_default();
                match rustudio_core::doc::Document::open(&path) {
                    Ok(d) => print!("{}", d.script_sources(&pat)),
                    Err(e) => eprintln!("{e}"),
                }
                return Ok(());
            }
            "--team-create" => {
                let universe = args.next().and_then(|v| v.parse::<u64>().ok()).filter(|v| *v != 0);
                let place = args.next().and_then(|v| v.parse::<u64>().ok()).filter(|v| *v != 0);
                match (universe, place) {
                    (Some(u), Some(p)) => team_create = Some((u, p)),
                    _ => { eprintln!("--team-create requires UNIVERSE_ID PLACE_ID"); std::process::exit(2); }
                }
            }
            "--open-roblox" => {
                let v = args.next().unwrap_or_default();
                match rustudio_account::parse_place_ref(&v) {
                    Ok(p) => open_roblox = Some(p.place_id),
                    Err(e) => {
                        eprintln!("--open-roblox: {e}");
                        std::process::exit(2);
                    }
                }
            }
            "--local-copy" => local_copy = true,
            "--roblox-picker" => {
                let v = args.next().unwrap_or_default();
                match cloud_ui::PickerTab::parse(&v) {
                    Some(t) => roblox_picker = Some(t),
                    // File > Experience Settings instead of the picker.
                    None if v == "settings" => roblox_settings = true,
                    None => {
                        eprintln!("--roblox-picker: {v:?} (recent, mine, groups, shared or settings)");
                        std::process::exit(2);
                    }
                }
            }
            "--demo" => demo = true,
            "--play" => start_play = true,
            "--no-ui" => no_ui = true,
            "--play-only" => {
                no_ui = true;
                start_play = true;
            }
            "--overlays" => overlays = true,
            "--headless" => headless = true,
            "--no-render" => {
                headless = true;
                no_render = true;
            }
            "--show-core-ui" => show_core_ui = Some(true),
            "--hide-core-ui" | "--no-core-ui" => show_core_ui = Some(false),
            // (read at the start of main)
            "--audio" => {}
            "-h" | "--help" => {
                print!("{USAGE}");
                return Ok(());
            }
            _ if a.starts_with("--") => {
                eprint!("unknown option {a}\n\n{USAGE}");
                std::process::exit(2);
            }
            _ => open = Some(PathBuf::from(a)),
        }
    }
    if headless && !no_ui {
        eprintln!("--headless / --no-render: only with --no-ui (or --play-only); the editor's own UI needs a window");
        std::process::exit(2);
    }
    if no_ui && (selftest.is_some() || ui_shots.is_some()) {
        eprintln!("--no-ui: --self-test and --ui-shots drive the editor's UI; run them without --no-ui");
        std::process::exit(2);
    }
    // The screenshot shows the editor with something in it to look at.
    if screenshot.is_some() && open.is_none() {
        demo = true;
    }

    // --fixed-dt: simulated clocks from here (crates/rustudio-core/src/sim_clock.rs).
    if let Some(dt) = fixed_dt {
        rustudio_core::sim_clock::enable(dt);
    }
    let (show_core_gui, show_core_gui_explicit) = run_harness::resolve_show_core_gui(
        show_core_ui,
        std::env::var("RUSTUDIO_SHOW_CORE_UI").ok().as_deref(),
        headless,
    );
    let harness = app::Harness {
        run: run_harness::RunHarness {
            shot_after,
            log_out,
            input: run_harness::HarnessInput { actions, pointer: None, shift: false },
            started: rustudio_core::sim_clock::now(),
            held: 0.0,
            screenshot,
            screenshot_requested: false,
            start_play,
            camera,
            shots,
            shot_next: 0,
            shot_lines: Vec::new(),
            from_play,
            play_started: None,
            play_game_time: None,
            frame_ms: Vec::new(),
            last_frame: None,
            no_ui,
            no_render,
            show_core_gui,
            show_core_gui_explicit,
            end_after: (no_ui && seconds_given && bench.is_none() && profile_capture.is_none()).then_some(seconds),
            fail_on_error,
            result,
            end_value: None,
            run_frames: Vec::new(),
            run_last: None,
            run_playing: false,
            finished: false,
            bench: bench.map(|out| {
                let label = open
                    .as_ref()
                    .map(|p| p.display().to_string())
                    .unwrap_or_else(|| "new place".into());
                rustudio_core::bench::Bench::new(Some(out), &label, start_play, seconds)
            }),
            profile: profile_capture.map(|out| rustudio_core::profiler::cli::CliCapture::new(out, seconds, start_play, profile_instr)),
        },
        selftest: selftest.map(selftest::SelfTest::new),
        ui_shots: ui_shots.map(ui_shots::UiShots::new),
        open_roblox: open_roblox.map(|p| (p, local_copy)),
        team_create,
        roblox_picker,
        roblox_settings,
        ui_due: Vec::new(),
        overlays,
        plugin_files,
        installed_plugins,
        plugin_dirs,
        no_plugins,
        plugins_started: false,
    };
    // No vsync when measuring, so frame times show the real cost.
    // (--fixed-dt: as fast as the machine allows)
    let bench_mode = harness.run.bench.is_some() || harness.selftest.is_some() || rustudio_core::sim_clock::on();
    let argv: Vec<String> = std::env::args().collect();
    let size = match window_arg(&argv) {
        Some((w, h)) => [w as f32, h as f32],
        None if harness.run.bench.is_some() => [1265.0, 900.0],
        // The self-test's lighting references were read in a 952×519 view
        // (rounds 7–15); round 16's chrome is 25 px shorter, so 33 px less window.
        None if harness.selftest.is_some() => [1265.0, 867.0],
        None => [1600.0, 900.0],
    };
    // Studio Settings: the user's for plain runs (src/studio_settings.rs).
    studio_settings::init(layout_store::enabled(&harness));
    // rustudio's own options: likewise only for runs a person drives, never
    // under RUSTUDIO_DETERMINISTIC (crates/rustudio-core/src/rustudio_settings.rs).
    rustudio_core::rustudio_settings::init(layout_store::enabled(&harness));
    // (a --profile-capture run keeps the settings it chose)
    if harness.run.profile.is_none() {
        rustudio_core::profiler::load_saved_config();
    }
    // The saved window (src/layout_store.rs), for plain interactive runs.
    let saved_layout = (!harness.run.no_ui && layout_store::enabled(&harness)).then(layout_store::load).flatten();
    let icon = egui::IconData {
        rgba: icons::logo_rgba(64),
        width: 64,
        height: 64,
    };
    let mut viewport = egui::ViewportBuilder::default()
        .with_title("rustudio")
        .with_app_id("rustudio")
        .with_inner_size(size)
        .with_icon(icon);
    // (--no-ui: any size, the view is the whole window)
    if !harness.run.no_ui {
        viewport = viewport.with_min_inner_size([900.0, 560.0]);
    }
    // A test run gets the size it asked for even on a smaller desktop
    // (eframe clamps the first size to the monitor: an 800x600 session
    // desktop made `--window 1024x768` an 800x600 editor, its docked
    // widgets too short for the checks).
    if cfg!(windows) && is_test_run(&argv) {
        viewport = viewport.with_clamp_size_to_monitor_size(false);
    }
    if let Some(l) = &saved_layout {
        viewport = l.window_builder(viewport);
    }
    // Driven for comparison captures (src/control.rs): open without taking
    // focus and stay below other windows, so it never covers the person's
    // work (X11 keeps drawing covered windows, and shots read the frame back).
    if std::env::var_os("RUSTUDIO_CONTROL").is_some() {
        viewport = viewport.with_active(false).with_window_level(egui::WindowLevel::AlwaysOnBottom);
    }
    let wgpu_options = offscreen::wgpu_options(bench_mode && !(bench_vsync && harness.run.bench.is_some()));
    // --headless / --no-render: no window at all (src/offscreen.rs).
    if headless {
        let px = [size[0].round() as u32, size[1].round() as u32];
        let code = offscreen::run(
            wgpu_options,
            // (RUSTUDIO_HEADLESS_HZ paces a bench too: frame work at a display's rate)
            offscreen::Options { size: px, render: !no_render, paced: !bench_mode || std::env::var_os("RUSTUDIO_HEADLESS_HZ").is_some() },
            move |ctx, rs| app::RustudioApp::new_in(ctx, rs, None, open, harness, demo),
            app::RustudioApp::frame,
        );
        rustudio_core::telemetry::flush();
        std::process::exit(code.max(app::EXIT_CODE.load(std::sync::atomic::Ordering::Relaxed)));
    }
    let options = eframe::NativeOptions {
        renderer: eframe::Renderer::Wgpu,
        viewport,
        wgpu_options: wgpu_options.clone(),
        // macOS: rustudio sets the app menu itself (macos.rs).
        #[cfg(target_os = "macos")]
        event_loop_builder: Some(Box::new(|b| {
            use winit::platform::macos::EventLoopBuilderExtMacOS;
            b.with_default_menu(false);
        })),
        ..Default::default()
    };
    // Files opened from Finder arrive as Apple Events, not argv.
    #[cfg(target_os = "macos")]
    macos::install_open_handler();
    let r = native_loop::run(
        "rustudio",
        options,
        Box::new(move |cc: &eframe::CreationContext<'_>| {
            #[cfg(target_os = "macos")]
            {
                macos::install_menu();
                macos::set_wake(&cc.egui_ctx);
            }
            Ok(Box::new(app::RustudioApp::new(cc, open, harness, demo)))
        }),
    );
    // --fail-on-error (app::Editor::finish_run); a windowed test run's exit
    // status reaches the caller through the headless wrapper's status file.
    let code = app::EXIT_CODE.load(std::sync::atomic::Ordering::Relaxed);
    if code != 0 {
        std::process::exit(code);
    }
    r
}
