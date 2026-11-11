## What this changes

<!-- One or two sentences, and how you checked it (tests, screenshots against Studio). -->

## Clean room

<!-- rustudio matches Studio by behaviour and never copies its code: docs/CLEANROOM.md.
Tick what applies; delete the lines that don't. -->

Where the behaviour comes from:
- [ ] Black-box only: Studio screenshots or measurements, the API dump, Roblox's public docs (say which)
- [ ] A spec: `docs/specs/<topic>.md`, written by a reader, implemented by a different writer

If it follows a spec:
- Reader (person / AI session):
- Writer (person / AI session), who hasn't seen Studio's internals for this topic:

Tainted code (docs/cleanroom/TAINT.md):
- [ ] Doesn't touch tainted code
- [ ] Replaces tainted region(s) `<id>` with new code from the spec, via `taint.py splice`; TAINT.md updated

Checks:
- [ ] No Studio internal names in code or comments (decompiled names, buffer slots, shader program names, addresses)
- [ ] `python3 tools/cleanroom/lint.py source`, `lint.py spec` and `taint.py check` pass (CI runs them)
- [ ] Tools used for this change (Claude Code, another AI tool, by hand):
