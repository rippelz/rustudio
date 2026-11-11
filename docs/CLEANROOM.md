# Clean-room rule

rustudio matches Roblox Studio by behaviour, not by copying its code. Studio's
shaders, binaries and decompiled output may be studied to understand what it
does, but the person (or agent) who studies them doesn't write rustudio's
code for that topic. What they learned crosses over only as a written
behaviour spec in [docs/specs/](specs/).

This isn't a certified legal process. The aim is simple: rustudio's code
shouldn't mirror Studio's structure, and it shouldn't reference Studio's
internals (decompiled names, buffer slots, shader programs, addresses).

## Roles

- **Reader**: may study anything about how Studio works, including shader
  packs, binaries, decompiled code and black-box measurements. Writes a spec
  in `docs/specs/<topic>.md` and keeps any dumps or notes in
  `cleanroom-reference/` (gitignored). Doesn't change rustudio's code for
  that topic.
- **Writer**: has never seen Studio's internals for the topic. Works only
  from the spec, rustudio's own code, `api-dump.json`, Roblox's public docs,
  and Studio as a black box (screenshots, measurements). Implements the
  behaviour in rustudio's own structure.
- **Orchestrator** (optional): passes the spec's path from reader to writer
  and carries nothing else across.

The same person can be a reader on one topic and a writer on another, but not
both on the same topic. Black-box work (screenshots, measurements, public
docs) needs no split: anyone can do it and write code from it directly.

## What a spec looks like

Start from [docs/specs/TEMPLATE.md](specs/TEMPLATE.md):

- **Allowed**: prose and maths in your own words; which public properties
  drive the behaviour; equations for quantities, with names that say what
  the quantity is; measurable expected results; edge cases.
- **Not allowed**: code in any language, pseudo-code that follows the source
  line by line, decompiled names (`_1234`, `FUN_…`, `sub_…`), buffer slots
  and shader inputs (`CB0[…]`, `VARYING…`), shader program names, addresses,
  and the source's structure or statement order.
- **Expected results** must be checkable without the internals, e.g. "a
  sunlit face with roughness 1, sun at 45°, lights by at most N levels".

## The lint

```sh
python3 tools/cleanroom/lint.py spec             # every spec in docs/specs/
python3 tools/cleanroom/lint.py source           # src/ and crates/
python3 tools/cleanroom/lint.py source --update-baseline
```

`spec` rejects code, decompiled names, buffer slots, shader program names and
addresses. `source` looks for the same internal names in code and comments.
References that predate this rule are recorded by count only in
`tools/cleanroom/baseline.json`. CI fails if a file gets more of them, and
when you remove some, `--update-baseline` locks that in. The baseline is the
to-do list for rewriting those parts through this process.

## Tainted code

Some of rustudio's code was translated from Studio's internals before this
rule. It's listed in [docs/cleanroom/TAINT.md](cleanroom/TAINT.md) and marked
in the source (`cleanroom-taint-begin/end: <id>`, or
`cleanroom-taint-file: <id>` for a whole file). It gets rewritten with the
same two roles:

1. A **reader** reads the tainted code (it's allowed to) plus Studio as
   needed, marks it (`tools/cleanroom/taint.py mark`), and writes a spec of
   the behaviour and of the interface the rest of rustudio uses. The spec is
   checked against Studio, not just the old code.
2. A **writer**, who can't see tainted code, implements from the spec in new
   code. It sees files through `taint.py redact` (tainted regions withheld)
   and `taint.py grep`, and the hook blocks reading, searching or editing
   tainted lines any other way.
3. `taint.py splice` swaps the region for the new code mechanically, the
   TAINT.md entry is marked done, and the lint baseline goes down.

Removing internal names or markers without rewriting the code doesn't count.
CI runs `taint.py check` (markers balanced, every region documented).

Finding tainted code means a reader **reading** each file and judging from
context whether it looks like a port (leftovers, compiled-code numbers,
decompiled shape, knowledge no black box would give, the project's own
record), not running the lint: most translated code has no leftover names
for a lint to find. Studio's internals, where available, are for
spot-checking unclear cases, not for diffing everything. TAINT.md explains how to review and mark (smallest
regions, interface left visible) and keeps a review log;
`tools/cleanroom/taint.py coverage` shows how much of the source has been
reviewed. The guard only knows marked code, so readers mark generously.

## With Claude Code

The repo ships the clean room ready to use:

- hooks (`.claude/settings.json`): one tells every session its role when it
  starts, the other (`.claude/hooks/cleanroom_guard.py`) enforces the role on
  every tool call;
- two agents, `cleanroom-reader` and `cleanroom-writer`, whose rules each role
  follows;
- `/cleanroom <topic>` for running a topic hands-off.

Add the folders your own Studio install or dumps live in to
`tools/cleanroom/reference_paths.txt`.

### Two sessions you talk to (the normal way)

Open Claude Code in the repo, in any front end (the VS Code extension's chat
panel, the CLI, the desktop app). Before doing anything, the session tells you
it follows the clean room, says whether it's already seen any of Studio's
internals in this conversation, and asks whether it should be the **reader**
or the **writer**. It then claims the role with `tools/cleanroom/role.sh
reader|writer`, and the hook enforces it from then on:

- **writer**: all normal rustudio work; Studio's internals are blocked.
- **reader**: may study anything; writes only `docs/specs/` and
  `cleanroom-reference/` (gitignored, for dumps and notes).
- **no role yet**: no edits in the repo and no internals, until it asks you.

For the other role, open one more chat (a new tab is enough) and answer the
other way. Each is a normal session: ask it things, help it, correct it, look
at its screenshots. Helper agents a session starts (Explore and the like)
share its role. A session that has been the reader can never become the
writer; if a session has seen internals another way (you pasted some, or it
read them before this rule existed), it says so and you start a new one for
the writer. `/resume` brings a session back with its role.

**Fixed roles** instead, if you prefer: `claude --agent cleanroom-reader` /
`claude --agent cleanroom-writer` (or `tools/cleanroom/session.sh reader|writer
<topic>`), or one folder per role with `tools/cleanroom/setup-workspaces.sh`,
which makes every session in a folder start with that role.

In VS Code, the extension can attach your editor selection and open file to
the chat. Open Studio's files only while you're talking to the reader, so they
can't end up in the writer's chat.

How the two sessions talk:

1. The reader writes `docs/specs/<topic>.md`, the spec lint checks it, and
   the reader sends the writer a notice: `spec ready: docs/specs/<topic>.md`.
2. The writer implements from it. When something is unclear, it adds a
   numbered question to the spec's "Questions from the writer" section and
   messages the reader. **The writer may message the reader freely**
   (questions, measurements, what it tried): it has never seen Studio's
   internals, so there's nothing it could leak.
3. The reader answers by improving the spec, marks the questions answered,
   and sends `spec updated: docs/specs/<topic>.md (answered 1, 2)`. **That
   notice is all the reader can send**: the hook blocks any other message
   from it, so what it knows only reaches the writer through the spec.

The messages use Claude Code's messaging between sessions on the same
machine (ListAgents / SendMessage). Sessions are listed by folder name
(`rustudio-<n>`, or `rustudio-cleanroom-reader-<n>` in the reader's folder);
when two chats share a folder, the agent asks you which one is the other
role. A message reaches the other session at its next step; if the two
sessions run in different permission modes, Claude Code may ask you to
approve it first. If the other session isn't open, the agent tells you.

You're the link between the sessions, so the one rule for you is: **anything
that reaches the writer goes through the spec.** You can talk freely with the
reader about what it finds. Don't paste the reader's chat, sources or dumps
into the writer's session. You can tell the writer black-box things you've
seen yourself (a Studio screenshot, a property value, "this looks too
bright"), and you can help it with anything about rustudio.

### Hands-off: `/cleanroom <topic>`

For a topic that doesn't need you along the way, open Claude Code in the repo
(the writer window, if you've set the folders up) and run `/cleanroom <topic> [what to match or fix]`. It starts the reader as a
subagent, lints the spec, then starts a fresh writer subagent that never sees
the reader's sources or the conversation, and reports back. Subagents can't
stop to ask you things, so use the two sessions when a topic needs your help.

### Limits

The hook is a guardrail, not a sandbox. A program the writer runs could still
open files directly. For a hard boundary, run the writer on a machine,
container or account where Studio isn't installed. The hook needs `python3`
on PATH.

## With other tools, or by hand

Follow the same roles:

1. The reader writes `docs/specs/<topic>.md` and runs the spec lint.
2. Someone else, who hasn't looked at the internals for this topic, implements
   it from the spec and runs the source lint.
3. Fill in the clean-room part of the PR template: where the behaviour comes
   from, who read and who wrote, the spec, and any tainted code replaced.

If you use another AI tool, give the writer a fresh session with only the
spec and this file, and keep Studio's install and your dumps out of its reach.
