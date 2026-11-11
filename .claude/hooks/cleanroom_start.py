#!/usr/bin/env python3
"""SessionStart hook for rustudio's clean room (docs/CLEANROOM.md).

Tells every new or resumed Claude Code session, before its first reply,
which clean-room role it has and what to tell the user. Roles are explained
in cleanroom_guard.py.
"""
import json
import os
import re
import sys

WRITER, READER = "cleanroom-writer", "cleanroom-reader"

try:
    data = json.load(sys.stdin)
except Exception:
    data = {}
root = os.path.realpath(os.environ.get("CLAUDE_PROJECT_DIR") or data.get("cwd") or os.getcwd())
session = re.sub(r"[^A-Za-z0-9_-]", "", str(data.get("session_id") or ""))


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
    try:
        return json.load(open(os.path.join(root, ".claude", "cleanroom-state", session + ".json"),
                              encoding="utf-8")).get("role", "")
    except (OSError, ValueError):
        return ""


agent = data.get("agent_type") or ""
fixed = agent if agent in (WRITER, READER) else folder_role()
role = fixed or (claimed_role() if session else "")
name = {WRITER: "WRITER", READER: "READER"}.get(role, "")

CONTAMINATION = (
    "Contamination check: if this conversation already contains any of Roblox Studio's internals "
    "(its shader code, decompiled or disassembled code, or a binary's contents, whether you read it, the "
    "user pasted it, or it came from work done before this repo had the clean-room rule), this session "
    "must not write rustudio code for that behaviour. Say so plainly: it can only be the reader, and the "
    "writer needs a new session.")

if role:
    how = "fixed by its agent or folder" if fixed else "claimed earlier with tools/cleanroom/role.sh"
    text = (f"rustudio clean room (docs/CLEANROOM.md): this session is the {name} ({how}). "
            f"In your first reply, tell the user you're the clean-room {name.lower()} and will follow "
            f"docs/CLEANROOM.md. " + (CONTAMINATION if role == WRITER else
            "Only the spec in docs/specs/ may carry what you learn to the writer."))
else:
    text = (
        "rustudio clean room (docs/CLEANROOM.md): this repository may only match Roblox Studio by its "
        "behaviour. Studio's internals (shaders, binaries, decompiled code) may be studied only by a READER, "
        "who writes a code-free spec in docs/specs/; a WRITER, who never sees them, implements from the "
        "spec. This session has no role yet. Before doing any other work, in your first reply: "
        "(1) tell the user this repo uses a clean room and that you'll follow it; "
        "(2) " + CONTAMINATION + " "
        "(3) ask the user whether this session should be the READER (studies Studio, writes specs, can't "
        "edit code) or the WRITER (implements specs and does all other rustudio work, never sees Studio's "
        "internals); "
        "(4) once they answer, run `tools/cleanroom/role.sh reader` or `tools/cleanroom/role.sh writer` "
        "and follow what it prints. Until then, edits in the repo and Studio's internals are blocked.")

print(json.dumps({"hookSpecificOutput": {"hookEventName": "SessionStart", "additionalContext": text}}))
