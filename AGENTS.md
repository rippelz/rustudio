# AGENTS.md

Notes for coding agents (and people) working on rustudio. The README covers
what the project is; this is how to find your way around and check your work.

## Before anything else: the clean room

rustudio matches Roblox Studio by its **behaviour**. It must not copy,
translate or reference Studio's code. Studio's internals (shader packs,
binaries, decompiled or disassembled code) may be studied only by a
**reader**, who writes a code-free behaviour spec in `docs/specs/`. A
**writer**, who has never seen those internals, implements from the spec.
Full rules: [docs/CLEANROOM.md](docs/CLEANROOM.md).

If you're an AI assistant working in this repo, your **first reply** in a
session, before any other work, must:

1. Tell the user this repo uses a clean room and that you'll follow it.
2. Check yourself for contamination. If this conversation already contains
   any of Studio's internals (shader code, decompiled or disassembled code, a
   binary's contents), whether you read it, the user pasted it, or it came
   from work done before this rule existed, say so plainly: this session
   must not write rustudio code for that behaviour. It can only be the
   reader, and the writer needs a new session.
3. Ask the user which role this session takes:
   - **Reader**: studies Studio, writes specs in `docs/specs/`, never edits
     rustudio's code.
   - **Writer**: implements specs and does all other rustudio work; never
     opens Studio's internals (see `tools/cleanroom/reference_paths.txt`).
4. Once they answer, run `tools/cleanroom/role.sh reader` or
   `tools/cleanroom/role.sh writer` and follow the rules it prints.

A session that has been the reader can never become the writer. Never put
Studio's internal names in code or comments (decompiled names, buffer slots,
shader program names, addresses); `python3 tools/cleanroom/lint.py source`
must pass. In Claude Code, the repo's hooks remind you of this at the start
of every session and enforce the role.

## Build and test

```sh
cargo build --release
cargo test --workspace
```

The workspace is the app (`src/`) plus `crates/rustudio-account` (Roblox
session, downloads, publishing) and `crates/rustudio-net` (Team Create /
network protocol). Crates in `vendor/` are third-party; change them only
when there is no way around it.

## Seeing a change work

Unit tests don't cover how things look or feel. rustudio has its own harness
for that, described in [docs/DEVELOPING.md](docs/DEVELOPING.md):

- `--screenshot out.png` draws a scene and saves the window.
- `place.rbxl --play --shots 2,5,10 --screenshot shot.png` plays a place and
  takes timed shots, with `--click`, `--type`, `--key`, `--wheel` and
  `--luau client|server file.luau@T` to drive it.
- `--self-test out.json` drives the editor with scripted input.
- `--bench out.json` measures frame times and the slowest Lua and engine calls.
- `--stats place.rbxl` lists what's in a place.

Look at the screenshots you take. A test that passes while the picture is
wrong isn't a fix.

## Where things are

[docs/DEVELOPING.md](docs/DEVELOPING.md) has a table of the main source files.
The rest of `docs/`:

- [docs/MECHANICS.md](docs/MECHANICS.md): every tuning number and where it
  comes from. The values live in `src/mech.rs` and `mechanics.json`.
- [docs/ENGINE-GAPS.md](docs/ENGINE-GAPS.md): engine API that's still missing,
  measured by playing real places.
- [docs/UI-PARITY.md](docs/UI-PARITY.md), [docs/MATERIALS.md](docs/MATERIALS.md):
  matching Studio's editor UI and materials.
- [docs/TEAMCREATE.md](docs/TEAMCREATE.md), [docs/AUTH.md](docs/AUTH.md):
  Team Create and sign-in.
- `checks/parity/`: per-area notes and results comparing rustudio with Studio.

## How changes should look

- The goal is to behave like Roblox Studio. When you change a behaviour or a
  number, say what it's based on (Studio's behaviour, the API dump, a
  measurement) or mark it as an estimate, the way docs/MECHANICS.md does.
- New tuning values go in `src/mech.rs` / `mechanics.json` with a line in
  docs/MECHANICS.md, not as constants scattered in the code.
- Engine API is checked against Roblox's real API (`api-dump.json`). Only add
  members Roblox actually has, and update docs/ENGINE-GAPS.md when you close
  a gap.
- Keep a change to one thing, match the style of the surrounding code, and
  add a test for a bug fix where it's practical.
