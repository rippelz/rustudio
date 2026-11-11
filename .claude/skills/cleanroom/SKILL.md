---
name: cleanroom
description: Run rustudio's clean-room process for one topic. A reader agent studies how Roblox Studio does it and writes a code-free spec in docs/specs/, the clean-room lint checks the spec, then a fresh writer agent that has never seen Studio's internals implements it. Use when matching Studio needs knowledge of its internals (shaders, binaries, decompiled code), or to rewrite code that `tools/cleanroom/lint.py source` flags.
argument-hint: <topic> [what to match or fix]
---

# /cleanroom

You are the ORCHESTRATOR (docs/CLEANROOM.md). You pass work between a reader
and a writer and never carry knowledge of Studio's internals between them.

Rules for you:
- You're on the writer's side. If this session has no clean-room role yet,
  do the startup check from AGENTS.md first (tell the user, check for
  contamination) and claim the writer role with `tools/cleanroom/role.sh
  writer`. If this session is or has been the reader, stop: tell the user
  to run /cleanroom from a new session.
- Don't open Studio's shader packs, binaries, decompiled output or
  `cleanroom-reference/` yourself.
- If you have already read Studio internals on this topic in this
  conversation, you must not edit the code for it: leave all edits to the
  writer.
- Pass the writer only the spec's path, the user's own words, and black-box
  facts (measurements, screenshots, public docs). Never paraphrase anything
  the reader saw.

Steps, with `$ARGUMENTS` as the topic and any notes:

1. Choose a short kebab-case slug for the topic. If `docs/specs/<slug>.md`
   already exists and the user didn't ask for a new one, skip to step 4.
2. Start the `cleanroom-reader` agent with: the topic, what the user wants
   matched or fixed (their words), and the target path
   `docs/specs/<slug>.md`. It replies with just the path.
3. Run `python3 tools/cleanroom/lint.py spec docs/specs/<slug>.md`. If it
   fails, send the reader the lint output and have it fix the spec (continue
   the same reader agent; don't fix the spec yourself). Repeat until clean.
   You may read the spec to check it's useful (it is clean by design); if
   it's vague or lacks measurable expected results, send it back.
4. Start a NEW `cleanroom-writer` agent (never a fork, never the reader) with:
   "Implement docs/specs/<slug>.md in rustudio", the user's own notes, and
   any black-box tools or measurements they gave you.
5. When it's done, run `python3 tools/cleanroom/lint.py source` and
   `python3 tools/cleanroom/lint.py spec`, and report to the user: the spec
   path, what the writer changed, how it was checked, and anything left open.
   If the writer has questions about the spec, take them to the reader (the
   reader answers by updating the spec), then hand the updated spec to the
   writer.
