#!/usr/bin/env python3
"""Clean-room lint (see docs/CLEANROOM.md).

  lint.py spec [FILE.md ...]        check behaviour specs (default: docs/specs/*.md)
  lint.py source                    check src/ and crates/ for references to Studio internals
  lint.py source --update-baseline  accept the current source findings as known debt
  lint.py taint                     check docs/cleanroom/TAINT.md (writers read it, so it mustn't leak)

A spec is what a reader hands to a writer, so it must describe behaviour in its
own words: no code, no decompiled names, no shader programs or buffer slots.
In the source, the same internal names must not appear at all (in code or
comments); findings that predate this rule are listed by count only in
baseline.json and may only go down.

Standard library only, so it runs anywhere Python 3.8+ does.
"""
import json
import os
import re
import sys

ROOT = os.path.abspath(os.path.join(os.path.dirname(__file__), "..", ".."))
BASELINE = os.path.join(os.path.dirname(os.path.abspath(__file__)), "baseline.json")
SPEC_DIR = os.path.join(ROOT, "docs", "specs")
SOURCE_DIRS = ["src", "crates"]
SOURCE_EXT = (".rs", ".wgsl", ".luau", ".lua", ".py", ".js", ".ts", ".json", ".toml")

# (name, regex, where): where is the set of things the rule applies to:
# "spec", "source", "taint" (the TAINT.md register).
LEAKS = {"spec", "source", "taint"}
RULES = [
    ("decompiled-temp", r"(?<![\w.$])_\d{2,}\b", LEAKS),         # compiler temporaries
    ("constant-buffer", r"\bCB\d+\s*\[", LEAKS),                  # constant buffer slots
    ("shader-varying", r"\bVARYING\d+\b", LEAKS),                 # shader interpolants
    ("shader-program", r"\b(?:[A-Z][a-z0-9]+){2,}(?:FS|VS|PS|CS)\b(?:\.\d+)?", LEAKS),  # shader program names
    ("disassembly-name", r"\b(?:sub|loc|off|unk|byte|word|dword|qword)_[0-9A-Fa-f]{4,}\b"
                         r"|\b(?:FUN|DAT|LAB|PTR)_[0-9A-Fa-f]{6,}\b", LEAKS),
    ("shader-pack", r"shaders_\w+\.pack", {"source"}),             # citing the pack as a source
    ("code-fence", r"^\s*(?:```|~~~)", {"spec"}),
    ("glsl-hlsl", r"\b(?:i|u|b)?vec[234]\s*\(|\bmat[234](?:x[234])?\s*\(|\buniform\s+\w+"
                  r"|\b(?:mul|saturate|lerp)\s*\(", {"spec"}),
    ("shader-builtin", r"\bgl_[A-Za-z]+\b|\b(?:textureLod|texelFetch|texture2D)\s*\(", {"spec", "taint"}),
    ("address", r"\b0x[0-9A-Fa-f]{7,}\b", {"spec", "taint"}),
    ("code-line", r"[;{}]\s*$", {"spec"}),                         # a line that ends like code
]
COMPILED = [(n, re.compile(rx, re.M), w) for n, rx, w in RULES]
TAINT_DOC = os.path.join(ROOT, "docs", "cleanroom", "TAINT.md")


def findings(text, where):
    out = []
    for name, rx, w in COMPILED:
        if where not in w:
            continue
        for m in rx.finditer(text):
            line = text.count("\n", 0, m.start()) + 1
            out.append((name, line, m.group(0).strip()))
    return out


SECTIONS = ["Scope", "Inputs", "Behaviour", "Expected results", "Provenance"]
PROVENANCE_FIELDS = ["Studio version", "Kinds of source used", "Reader"]


def structure(text, template):
    """Problems with a spec's shape: the template's sections must be there and,
    in a real spec, filled in (Provenance included)."""
    out = []
    body = re.sub(r"<!--.*?-->", "", text, flags=re.S)
    heads = [(m.group(1).strip().lower().replace("behavior", "behaviour"), m.end())
             for m in re.finditer(r"^##\s+(.+?)\s*$", body, re.M)]
    for want in SECTIONS:
        hit = [h for h in heads if h[0].startswith(want.lower())]
        if not hit:
            out.append(f"missing section '## {want}'")
            continue
        if template:
            continue
        start = hit[0][1]
        nxt = [h[1] for h in heads if h[1] > start]
        section = body[start:(min(nxt) if nxt else len(body))]
        content = re.sub(r"^\s*##.*$", "", section, flags=re.M).strip()
        if not content:
            out.append(f"section '## {want}' is empty")
        if want == "Provenance":
            for field in PROVENANCE_FIELDS:
                m = re.search(rf"{field}[ \t]*:[ \t]*(.*)", section)
                if not m or not m.group(1).strip():
                    out.append(f"Provenance: fill in '{field}:'")
    return out


def lint_specs(files):
    if not files:
        files = sorted(os.path.join(SPEC_DIR, f) for f in os.listdir(SPEC_DIR) if f.endswith(".md"))
    bad = 0
    for f in files:
        text = open(f, encoding="utf-8").read()
        rel = os.path.relpath(f, ROOT)
        for name, line, tok in findings(text, "spec"):
            print(f"{rel}:{line}: {name}: {tok!r}")
            bad += 1
        for msg in structure(text, os.path.basename(f) == "TEMPLATE.md"):
            print(f"{rel}: {msg}")
            bad += 1
    if bad:
        print(f"\n{bad} problem(s). A spec describes behaviour in prose and maths, in its own words;"
              " see docs/CLEANROOM.md.", file=sys.stderr)
        return 1
    print(f"clean-room lint: {len(files)} spec(s) clean")
    return 0


def lint_taint(path=TAINT_DOC):
    """TAINT.md names rustudio's code and is read by writers: it may describe
    why code counts as a port, but must not carry Studio's internals."""
    text = open(path, encoding="utf-8").read()
    bad = findings(text, "taint")
    for name, line, tok in bad:
        print(f"{os.path.relpath(path, ROOT)}:{line}: {name}: {tok!r}")
    if bad:
        print(f"\n{len(bad)} problem(s): the register is read by writers. Describe the evidence in words "
              "(\"comments cite decompiled names\"), don't quote it; see docs/cleanroom/TAINT.md.", file=sys.stderr)
        return 1
    print("clean-room lint: TAINT.md clean")
    return 0


def source_files():
    for d in SOURCE_DIRS:
        for base, dirs, files in os.walk(os.path.join(ROOT, d)):
            dirs[:] = [x for x in dirs if x not in ("target", "vendor", "node_modules")]
            for f in files:
                if f.endswith(SOURCE_EXT):
                    yield os.path.join(base, f)


def lint_source(update):
    counts, where = {}, {}
    for f in source_files():
        try:
            text = open(f, encoding="utf-8", errors="replace").read()
        except OSError:
            continue
        rel = os.path.relpath(f, ROOT).replace(os.sep, "/")
        for name, line, tok in findings(text, "source"):
            key = f"{rel}|{name}"
            counts[key] = counts.get(key, 0) + 1
            where.setdefault(key, []).append((line, tok))
    if update:
        json.dump(dict(sorted(counts.items())), open(BASELINE, "w"), indent=1)
        open(BASELINE, "a").write("\n")
        print(f"baseline: {sum(counts.values())} finding(s) in {len({k.split('|')[0] for k in counts})} file(s)")
        return 0
    base = json.load(open(BASELINE)) if os.path.exists(BASELINE) else {}
    new = 0
    for key, n in sorted(counts.items()):
        allowed = base.get(key, 0)
        if n > allowed:
            rel, name = key.split("|")
            for line, tok in where[key]:
                print(f"{rel}:{line}: {name}: {tok!r}")
            print(f"  ^ {rel}: {n} {name} finding(s), baseline allows {allowed}")
            new += n - allowed
    total = sum(counts.values())
    if new:
        print(f"\n{new} new reference(s) to Studio internals. Describe the behaviour instead"
              " (docs/CLEANROOM.md).", file=sys.stderr)
        return 1
    gone = sum(max(0, n - counts.get(k, 0)) for k, n in base.items())
    print(f"clean-room lint: no new findings ({total} known, baseline {sum(base.values())}"
          + (f"; {gone} fixed — run with --update-baseline to lock that in" if gone else "") + ")")
    return 0


def main(a):
    if not a or a[0] in ("-h", "--help"):
        print(__doc__)
        return 0
    if a[0] == "spec":
        return lint_specs(a[1:])
    if a[0] == "source":
        return lint_source("--update-baseline" in a)
    if a[0] == "taint":
        return lint_taint(*a[1:2])
    print(__doc__, file=sys.stderr)
    return 2


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
