# Tainted code

Code in rustudio that was translated from Studio's internals before the
clean-room rule ([docs/CLEANROOM.md](../CLEANROOM.md)). It's marked in the
source (`cleanroom-taint-begin/end/file: <id>`) and listed here, one section
per id. This file names rustudio's code only, so anyone may read it.

How a region gets rewritten:

1. A **reader** reads the tainted code (and Studio, as needed) and writes a
   spec in `docs/specs/` covering the behaviour and the interface the rest of
   rustudio uses. It checks the spec against Studio, not just the old code.
2. A **writer**, who never sees the tainted body (the hook blocks it; it
   works from `taint.py redact` / `taint.py grep`), writes the replacement as
   new code from the spec and checks it against Studio.
3. `taint.py splice` swaps the region for the new code. The section below is
   marked done, and `lint.py source --update-baseline` locks in the drop.

Deleting markers or internal names from comments without rewriting the code
doesn't count: it only hides the problem.

Section format, one per id:

    ## <id>
    - Where: <file> (<function / region>)
    - Evidence: <confidence (high | medium)>: <why it counts as translated>
    - Status: marked | spec written (docs/specs/<topic>.md) | rewritten
    - Notes: <the interface the rest of rustudio uses, anything a spec needs>
    - Continues in: `<file>` (<where>)
      (one line per other file this behaviour also lives in; `taint.py
      todo` lists these until a reader has reviewed that part and deletes
      the line: a partial mark doesn't clear it)

## Reviewing the repo for taint

Finding tainted code is a reader's job, done by **reading the code** file by
file and judging, from context, whether it looks like a port of Studio's
internals or like rustudio's own work. Diffing the whole codebase against
Studio isn't practical, so context is the main tool; Studio's internals,
where the reader has them, are for spot-checking unclear cases. The source
lint only catches leftover references, so its findings are a hint, never the
review.

Signs of a port (any one strong sign is enough):
- Leftovers in comments: decompiled names (an underscore and digits), function addresses,
  buffer slots, shader program names, "read from Studio's ...", "matches
  Studio's code", or Studio-internal type, field or record names that aren't
  in the public API (check `api-dump.json`).
- Numbers that came out of compiled code: float literals carrying float32
  rounding (`0.0033333334`, `0.08900000154972`), or long chains of exact
  constants nobody would measure or choose.
- Shape of decompiled or translated code: SSA-style temporaries, odd
  expansions (`x*x*x*x`, repeated identical clamps), branches for internal
  states no API exposes, steps in an order only the original would dictate.
- Knowledge no black box would give: internal processing order, unpublished
  data formats, behaviour in cases Studio never shows, comments explaining
  why Studio does something internally.
- The project's own record: docs (e.g. docs/MECHANICS.md) or commit
  messages saying a value or routine was taken from Studio's shaders, binary
  or decompiled code (`git log`, `git blame` on the file).

Signs of rustudio's own work: values cited to measurements, screenshots,
public docs or the API dump; values marked "estimated" or "fitted"; tests
built from captures; code shaped like the rest of rustudio rather than like
a translation.

Confidence, for each finding:
- **high**: leftovers, compiled-code numbers, or the project's own record
  says it was ported. Mark it.
- **medium**: knowledge or shape that's hard to explain without the
  internals. Mark it, and say so in the section.
- **low**: could be a port, could be careful measurement. Don't mark; add it
  to "Suspects" below with a line on why, for a spot-check with the internals
  (or a closer look) later.

How to mark and log:
- Scope: every file in `src/` and `crates/` (`vendor/` is third-party and
  out of scope), plus `docs/MECHANICS.md` and `mechanics.json`, which repeat
  constants. Largest risk first: rendering, lighting, terrain, particles,
  physics, networking.
- Splitting the work: a careful review covers a few thousand lines per
  session, so several reader sessions can run side by side on different
  folders. Before starting a folder or file, add it to "In progress" below
  (path, who, date) and check nobody else has it; remove the line when it's
  logged as done.
- Mark the smallest regions that are translated, usually function bodies.
  Leave the interface outside the marks (types, struct fields, function
  signatures), so the writer can still see how the code connects. If the
  interface itself copies Studio's (a buffer layout, a data structure), mark
  it too and say so in the section. Mark a whole file only when all of it,
  interface included, is translated.
- Constants files (`src/mech.rs`, `docs/MECHANICS.md`): the value is what's
  tainted and the name is the interface, but a mark hides whole lines. Mark
  the definitions, and list the constant names (and what each is for) in the
  section, so the writer knows what it has to provide. `mechanics.json` can't
  hold a marker: mark it whole with `--file` and list its keys the same way.
- Marking commands (line numbers are as the file is when you run it, so
  give all of a file's ranges in one command):
  `taint.py mark FILE <id> 120-180 240-260` marks several regions under one
  id; `taint.py mark FILE <id> --fn name` marks a Rust or WGSL function's
  body and leaves its signature visible; `--file` marks the whole file. One
  id may cover several regions when they're one piece of behaviour (say,
  scattered through one long function); use separate ids for separate
  behaviours, since each id becomes one spec and one rewrite.
- Taint that spans files: one behaviour is one id, in every file it lives
  in. If part of it is in a file you haven't reviewed, add a "Continues in"
  line to its section. Before reviewing a file, run `taint.py todo <file>`
  to see ids expected there, and mark those parts under the same id instead
  of a new one. Delete the line yourself once you've reviewed all of that
  part; marking some of it doesn't clear it.
- `--fn-doc name` marks a function's body and the comment block above it
  (two regions, signature still visible), for when the doc comment names
  internals too. `--id` groups let one command mark several ids in a file:
  `taint.py mark FILE --id a 10-20 --id b --fn-doc name`.
- To narrow an earlier mark (say a whole-file mark into regions), use
  `taint.py unmark FILE <id>` and mark again straight away. Never unmark to
  clear code: only a rewrite does that.
- Each id gets a section (format above, with its confidence in Evidence).
  Writers read this file, so describe evidence in words ("comments cite
  decompiled function addresses"); never quote Studio's names or code here.
  `python3 tools/cleanroom/lint.py taint` checks it.
- Log every file you finish in the review log below, clean ones included,
  so the review can be picked up by another session.
  `python3 tools/cleanroom/taint.py coverage` shows how much is reviewed,
  who's on what, and what's left.

## In progress

Folders or files a reader session is reviewing right now. One line each:
`` - `<path>`: <reader>, <date> ``

## Review log

One line per file whose review is finished (`clean`, or `tainted` with its
ids), or `partly` with what's left. Format:
`` - `<path>`: clean | tainted (<ids>) | partly (<what's left>) — <reader>, <date> ``

## Suspects

Low-confidence findings: not marked, waiting for a closer look or a
spot-check against Studio's internals. One line each:
`` - `<path>` (<function / lines>): <why it might be a port> — <reader>, <date> ``

## Candidates (from the source lint, not yet reviewed)

Files with leftover references to Studio internals
(`tools/cleanroom/baseline.json`): likely tainted, but only part of the
picture; most translated code has no leftovers, so every other file still
needs reading too.

- `src/shader.wgsl`: 39 finding(s)
- `src/mech.rs`: 21 finding(s)
- `src/lightgrid.rs`: 19 finding(s)
- `src/effects.rs`: 10 finding(s)
- `src/world_gui.rs`: 10 finding(s)
- `src/render.rs`: 5 finding(s)
- `src/lighting.rs`: 3 finding(s)
- `src/scene.rs`: 3 finding(s)
- `src/selftest/material.rs`: 2 finding(s)
- `crates/rustudio-account/src/avatar.rs`: 1 finding(s)
- `src/avatar.rs`: 1 finding(s)
- `src/matpack.rs`: 1 finding(s)
