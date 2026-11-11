#!/usr/bin/env python3
"""PreToolUse guard for rustudio's clean room (docs/CLEANROOM.md).

Every Claude Code session in the repo has a clean-room role:
- fixed by the agent it runs as (`claude --agent cleanroom-reader|writer`,
  or a subagent of that type),
- or fixed by the folder ("agent" in .claude/settings.local.json or
  settings.json, set by tools/cleanroom/setup-workspaces.sh),
- or claimed by the session itself with `tools/cleanroom/role.sh
  reader|writer` (this hook records the claim per session; helper agents
  share their session's role),
- or none yet ("unclaimed").

What each role may do:
- writer: anything except Studio's internals (tools/cleanroom/reference_paths.txt).
- reader: anything to read, tainted code included; writes only docs/specs/,
  docs/cleanroom/ and cleanroom-reference/; messages other sessions only with
  "spec ready/updated" notices.
- Tainted code (docs/cleanroom/TAINT.md): everyone but the reader sees it only
  through tools/cleanroom/taint.py (redact, grep) and replaces it with splice.
- unclaimed: no Studio internals and no edits inside the repo, until the
  session has told the user about the clean room and claimed a role.
A session that has been the reader can never become the writer.

Exit 2 blocks the call and tells the agent why. This is a guardrail, not a
sandbox: a program a session runs could still open files on its own.
"""
import json
import os
import re
import sys
import time

WRITER, READER = "cleanroom-writer", "cleanroom-reader"
ROLES = {"writer": WRITER, "reader": READER}
CLAIM = re.compile(r"(?:^|[\s;&|/])role\.(?:sh|py)\s+(reader|writer)\b")
NOTICE = re.compile(r"spec (ready|updated): docs/specs/[a-z0-9][a-z0-9-]*\.md( \(answered [0-9][0-9, ]*\))?")
CLAIM_HELP = ("this repo uses a clean room (docs/CLEANROOM.md). Tell the user you'll follow it, say whether "
              "you've already seen any Studio internals in this conversation, ask whether this session should "
              "be the reader or the writer, then run tools/cleanroom/role.sh reader|writer")


def block(msg):
    print(f"clean-room: {msg} (docs/CLEANROOM.md)", file=sys.stderr)
    sys.exit(2)


try:
    data = json.load(sys.stdin)
except Exception:
    block("couldn't read the hook input, refusing to guess")

root = os.path.realpath(os.environ.get("CLAUDE_PROJECT_DIR") or data.get("cwd") or os.getcwd())
# The repo this hook belongs to (the reader's workspace links to it); the
# tests point it at a scratch repo.
repo = os.environ.get("CLEANROOM_TEST_REPO") or os.path.dirname(os.path.dirname(os.path.dirname(os.path.realpath(__file__))))
state_dir = os.path.join(root, ".claude", "cleanroom-state")
session = re.sub(r"[^A-Za-z0-9_-]", "", str(data.get("session_id") or ""))
tool = data.get("tool_name", "")
inp = data.get("tool_input") or {}


def folder_role():
    for name in ("settings.local.json", "settings.json"):
        try:
            role = json.load(open(os.path.join(root, ".claude", name), encoding="utf-8")).get("agent")
        except (OSError, ValueError):
            continue
        if role in (WRITER, READER):
            return role
    return ""


def claimed_role():
    if not session:
        return ""
    try:
        return json.load(open(os.path.join(state_dir, session + ".json"), encoding="utf-8")).get("role", "")
    except (OSError, ValueError):
        return ""


def resolve(p):
    if not p:
        return ""
    p = os.path.expanduser(p)
    if not os.path.isabs(p):
        p = os.path.join(data.get("cwd") or root, p)
    return os.path.realpath(p)


def inside(path, base):
    return path == base or path.startswith(base.rstrip(os.sep) + os.sep)


agent = data.get("agent_type") or ""
fixed = agent if agent in (WRITER, READER) else folder_role()
role = fixed or claimed_role()

# A role claim (`tools/cleanroom/role.sh reader|writer`). Before it runs,
# refuse claims the session can't make; after it has run (PostToolUse, so a
# claim the user didn't approve counts for nothing), record it for this
# session. The script itself prints the role's rules.
claim = CLAIM.search(str(inp.get("command") or "")) if tool == "Bash" else None
if data.get("hook_event_name") == "PostToolUse":
    if claim and not fixed and session:
        want = ROLES[claim.group(1)]
        if not (role == READER and want == WRITER):
            os.makedirs(state_dir, exist_ok=True)
            with open(os.path.join(state_dir, session + ".json"), "w", encoding="utf-8") as fh:
                json.dump({"role": want, "claimed": time.strftime("%Y-%m-%dT%H:%M:%S")}, fh)
    sys.exit(0)
if claim:
    want = ROLES[claim.group(1)]
    if fixed and want != fixed:
        block(f"this session is fixed as {fixed} (its agent or folder) and can't switch")
    if role == READER and want == WRITER:
        block("this session has been the reader, so it may have seen Studio's internals: it can't become "
              "the writer. Tell the user to start a new session for the writer")
    sys.exit(0)


def touches_internals():
    try:
        lines = open(os.path.join(repo, "tools", "cleanroom", "reference_paths.txt"), encoding="utf-8").read().splitlines()
    except OSError:
        block("tools/cleanroom/reference_paths.txt is missing, so the session can't be guarded")
    pats = [re.compile(l.strip(), re.I) for l in lines if l.strip() and not l.lstrip().startswith("#")]
    targets = []
    for key in ("file_path", "path", "notebook_path"):
        if inp.get(key):
            targets += [inp[key], resolve(inp[key])]
    for key in ("pattern", "glob", "command"):
        if inp.get(key):
            targets.append(str(inp[key]))
    for t in targets:
        for p in pats:
            if p.search(t):
                return f"{p.pattern!r} matched {t!r}"
    return ""


EDITS = ("Write", "Edit", "NotebookEdit", "MultiEdit")


def touches_tainted_code():
    """Tainted code in the repo (docs/cleanroom/TAINT.md) is Studio's code in
    translation: whoever isn't the reader sees it only through
    `taint.py redact` / `taint.py grep` and replaces it with `taint.py splice`."""
    try:
        tainted = json.load(open(os.path.join(repo, "tools", "cleanroom", "taint-index.json"),
                                 encoding="utf-8")).get("files", [])
    except (OSError, ValueError):
        return ""
    if not tainted:
        return ""
    sys.path.insert(0, os.path.join(repo, "tools", "cleanroom"))
    import taint  # noqa: E402
    how = ("read it through `python3 tools/cleanroom/taint.py redact <file>` (then Read only the line ranges "
           "it shows) or `taint.py grep`, and replace tainted code with `taint.py splice`")
    abs_tainted = {os.path.realpath(os.path.join(repo, f)): f for f in tainted}

    def file_regions(path):
        try:
            return taint.regions_for(path)  # markers, or listed whole in taint-extra.txt
        except (OSError, ValueError):
            return [(1, 10 ** 9, "unreadable", "file")]

    if tool == "Bash":
        cmd = str(inp.get("command") or "")
        if "tools/cleanroom/taint.py" in cmd:
            return ""
        for f in tainted:
            if f in cmd or os.path.basename(f) in cmd:
                return f"{f} contains tainted code; don't open it from the shell: {how}"
        return ""
    if tool == "Grep":
        if (inp.get("output_mode") or "files_with_matches") != "content":
            return ""
        target = resolve(inp.get("path") or data.get("cwd") or root)
        if any(inside(t, target) for t in abs_tainted):
            return f"searching {target!r} would show tainted code: use `python3 tools/cleanroom/taint.py grep`"
        return ""
    path = resolve(inp.get("file_path") or inp.get("notebook_path") or "")
    if path not in abs_tainted:
        return ""
    name = abs_tainted[path]
    rs = file_regions(path)
    if any(kind == "file" for *_, kind in rs):
        return f"{name} is tainted as a whole (docs/cleanroom/TAINT.md): rewrite it from its spec in a new file"
    spans = ", ".join(f"{s}-{e} ({rid})" for s, e, rid, _ in rs)
    overlaps = lambda a, b: any(not (b < s or a > e) for s, e, _, _ in rs)
    if tool == "Read":
        start = int(inp.get("offset") or 1)
        end = start + int(inp.get("limit") or 2000) - 1
        if overlaps(start, end):
            return f"lines {spans} of {name} are tainted; {how}"
        return ""
    if tool in ("Write", "NotebookEdit"):
        return f"{name} contains tainted code, so it can't be overwritten: {how}"
    if tool in ("Edit", "MultiEdit"):
        text = open(path, encoding="utf-8", errors="replace").read()
        olds = [inp.get("old_string")] + [e.get("old_string") for e in inp.get("edits") or []]
        for old in filter(None, olds):
            pos = text.find(old)
            while pos >= 0:
                first = text.count("\n", 0, pos) + 1
                if overlaps(first, first + old.count("\n")):
                    return f"that edit touches tainted lines ({spans}) of {name}; {how}"
                pos = text.find(old, pos + 1)
    return ""


if role == WRITER:
    hit = touches_internals()
    if hit:
        block(f"the writer may not touch Studio internals ({hit}); work from the spec in docs/specs/ "
              "and Studio as a black box")
    hit = touches_tainted_code()
    if hit:
        block(f"the writer may not see tainted code: {hit}")
    sys.exit(0)

if role == READER:
    if tool == "SendMessage":
        msg = (inp.get("message") or "").strip()
        if msg and not NOTICE.fullmatch(msg):
            block("the reader may only send 'spec ready: docs/specs/<topic>.md' or 'spec updated: "
                  "docs/specs/<topic>.md (answered 1, 2)'; put everything else in the spec")
        sys.exit(0)
    if tool in EDITS:
        target = resolve(inp.get("file_path") or inp.get("notebook_path") or "")
        allowed = [os.path.realpath(os.path.join(root, d)) for d in ("docs/specs", "docs/cleanroom", "cleanroom-reference")]
        if not any(inside(target, a) for a in allowed):
            block(f"the reader writes only docs/specs/, docs/cleanroom/ and cleanroom-reference/, not {target!r}")
    sys.exit(0)

# Unclaimed: nothing sensitive until the session has a role.
hit = touches_internals()
if hit:
    block(f"Studio internals are off limits until this session is the reader ({hit}); {CLAIM_HELP}")
hit = touches_tainted_code()
if hit:
    block(f"tainted code is off limits until this session is the reader ({hit}); {CLAIM_HELP}")
if tool in EDITS:
    target = resolve(inp.get("file_path") or inp.get("notebook_path") or "")
    if inside(target, root) or inside(target, repo):
        block(f"no edits in the repo before choosing a role: {CLAIM_HELP}")
sys.exit(0)
