# Profiling rustudio (for agents)

Captures are files; you read them with the CLI. The MicroProfiler window
(View > MicroProfiler, Ctrl+F6; Ctrl+F6 in the standalone player)
opens the same files.

## Capture

```sh
rustudio PLACE.rbxl --play --no-ui --headless --profile-capture out.rsprof --seconds 8
rustudio PLACE.rbxl --play --no-ui --headless --profile-capture out.rsprof --seconds 8 --instr
rustudio-player PLACE.rbxl --headless --profile-capture out.rsprof --seconds 8
```

- Recording starts after the `--bench` warm-up (3 s, `RUSTUDIO_BENCH_WARMUP`),
  from Play's start with `--play`, and lasts `--seconds` (default 10).
  `--fixed-dt 1/60` makes the window cover the same simulated frames on any
  machine.
- `.rsprof` is the native format (gzip JSON). Any other name writes Chrome
  trace JSON (Perfetto, chrome://tracing). Both read back everywhere.
- `--instr` (or `RUSTUDIO_BENCH_INSTR=1` / `RUSTUDIO_PROFILE_INSTR=1`): every
  scope also counts the thread's retired instructions. On a loaded,
  shared machine instruction counts barely move while times swing by 15 %
  or more, so use them for before/after work. Linux counts instructions;
  Windows counts CPU cycles (`capture.instr_unit`): never compare
  the two.
- `RUSTUDIO_PROFILE_NO_GPU=1`: no GPU timestamp queries.

## Report

```sh
rustudio --profile-report out.rsprof [TOP] [--spikes N] [--json]
```

Text by default; `--json` is the stable form (schema
`rustudio-profile-report/1`). Every per-frame number is a mean over all
display frames, 0 for frames a scope didn't run in.

| key | what |
|---|---|
| `capture` | `app`, `build` (git sha of the binary; empty if unknown, e.g. rustudio-player), `host`, `platform`, `cpus`, `gpu`, `context` (Edit/Play · place), `load_avg` (1/5/15 min, Linux), `unit` (`ms` or `instr`: the primary unit), `instr_unit`, `alloc_tracked`, `duration_s`, `spans`, `display_frames` |
| `frames.{render,physics,heartbeat,gpu}` | `count`, `per_second`, `mean_ms`, `stddev_ms`, `p50_ms`, `p95_ms`, `p99_ms`, `max_ms`; render adds `main_busy_ms_mean` and, with instructions, `main_busy_minstr_mean`/`_stddev` (the display thread's work per frame: the best single number for A/B) |
| `lanes[]` | `name`, `title`, `kind`, `spans`, `busy_pct`, `dropped`, `display_frames` |
| `scopes[]` | sorted by self cost per frame (primary unit): `name`, `category`, `source` (`crates/…/file.rs:LINE` where the scope opens), `source_sites` (>1: one name, several sites), `lanes`, `calls`, `calls_per_frame`, `frames_present`, `self_ms_per_frame`, `self_ms_stddev`, `self_ms_frame_p95`, `incl_ms_per_frame`, `call_ms_p50`/`_p95`/`_max`, `alloc_bytes_per_frame`; with instructions also `self_minstr_per_frame`, `self_minstr_stddev`, `incl_minstr_per_frame` (M = millions) |
| `spikes[]` | the N worst display frames: `frame`, `t_s`, `ms`, `main_busy_ms`, `culprits[]` = scopes whose self time that frame beat their own p95, by how much (`self_ms`, `incl_ms`, `frame_p95_ms`, `excess_ms`, `source`) |
| `counters[]` | `name`, `category`, `unit`, `min`, `mean`, `max`, `last` |
| `scripts` | the script profiler's top scripts and functions when it ran (else `null`) |

Self = the scope's own time, children excluded; incl = with children.

## Diff

```sh
rustudio --profile-diff before.rsprof after.rsprof [TOP] [--json]
```

Schema `rustudio-profile-diff/1`: `unit`, `notes[]`, `frames.*`
(`a_mean_ms`, `b_mean_ms`, `delta_ms`, `delta_pct`, `verdict`),
`main_lane_busy_per_frame`, `scopes[]` sorted by the size of the change in
self cost per frame (`a_self`, `b_self`, `delta`, `delta_pct`, calls per
frame, `verdict`, `source`), `counters[]` (means and delta).

Instructions are the unit when both captures counted them, else ms.

### The noise rule

`verdict` is `slower` / `faster` only when all three hold:

1. The means differ by more than 3 standard errors. The errors come from
   the means of 10 contiguous blocks of frames per capture: neighbouring
   frames are alike, so single frames would overstate the certainty.
2. ms: at least 15 % and 5 µs. Instructions: at least 3 % and 0.02 M.
   Times also move with the machine's load between runs, which one capture
   can't see (two runs of one build on Windows differed by 16 % in ms; in
   instructions by 0.0 % on the main lane).
3. Otherwise the verdict is `noise`. `new` / `gone`: the scope ran in only
   one of the two.

Before trusting a ms `slower` under ~30 %, repeat both captures or switch to
`--instr`.

## The render thread

The viewport is recorded on the main thread (prepare) and finished and
submitted on `rustudio-render` (perform, `rustudio-render/src/render_thread.rs`;
Roblox's `RenderView::renderPrepare` / `renderPerform`). The scene's passes
are finished there as soon as `render` returns and submitted with the post
chain (`finish_frame`, after the world GUIs and effects) in one submit.
egui's paint and the next frame's first upload join it first, so no frame
of latency is added. Bench sections: `render_thread_perform` (the thread's
time, a call a job) and `render_thread_wait` (the main thread waiting for
it, category Wait). `RUSTUDIO_RENDER_THREAD=0` performs on the main thread.

## Categories

Frame, Render (CPU side), GPU (timestamp queries, on the GPU lane), UI
(editor panels, egui), Physics, Script (Luau), Animation, Network, Assets,
Audio, Input, GUI (in-game GUI layout and text), Pathfinding, Wait (idle,
waiting on a worker; time in a frame no scope covers counts here), Other.

## Adding scopes

```rust
let _s = rustudio_core::profiler::scope("my_step", rustudio_core::profiler::Cat::Physics);
let _s = rustudio_core::bench::scope("my_step"); // category from the name
rustudio_core::profiler::counter("my count", Cat::Physics, n as f64);
```

A scope records its source line by itself (`#[track_caller]`). While the
profiler is off a scope costs one atomic load (about 1 ns); recording, about
60 ns.
