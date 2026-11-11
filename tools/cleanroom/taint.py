#!/usr/bin/env python3
"""Tainted code in rustudio: code translated from Studio's internals before the
clean-room rule (docs/CLEANROOM.md, "Tainted code"; the register is
docs/cleanroom/TAINT.md).

Tainted code is marked with comment lines around each region:

    // cleanroom-taint-begin: <id>
    ...
    // cleanroom-taint-end: <id>

(`#` or `--` instead of `//` in Python, TOML, Lua; `<!-- ... -->` in
Markdown), or for a whole file, `cleanroom-taint-file: <id>` in its first
lines. An id may cover several regions. Files that can't hold comments (JSON)
are listed whole in tools/cleanroom/taint-extra.txt. A reader may read
tainted code; a writer only sees it through `redact` and `grep`, and replaces
it through `splice`.

  taint.py list                          every tainted region
  taint.py check                         markers balanced, ids documented, index current (CI)
  taint.py index                         rewrite tools/cleanroom/taint-index.json
  taint.py redact FILE                   FILE with tainted regions withheld (line-numbered)
  taint.py grep PATTERN [PATH ...]       search the source, skipping tainted lines
  taint.py mark FILE ID SPEC [SPEC ...]  (reader) mark regions under ID; SPEC is a line range A-B,
                                         `--fn NAME` (a Rust/WGSL function's body; the signature
                                         stays visible), `--fn-doc NAME` (its body and the comment
                                         block above it) or `--file` (the whole file)
  taint.py mark FILE --id ID SPEC... --id ID2 SPEC...
                                         several ids in one go. Line numbers are always as the file
                                         is when you run the command; the tool inserts bottom up.
  taint.py todo [FILE]                   ids whose TAINT.md section says "Continues in: FILE": reuse
                                         them there, and delete the line once that part is reviewed
  taint.py unmark FILE ID                (reader) remove ID's marks, only to re-mark more precisely
  taint.py splice FILE ID --with NEW [--at N]
  taint.py splice FILE ID --delete [--at N]
                                         replace ID's region in FILE (the Nth, when ID has several
                                         there) with NEW's contents, or remove it; for a whole-file
                                         taint, replace or delete the file
  taint.py coverage [--all]              how much a reader has reviewed (TAINT.md's review log),
                                         who's on what, and what's left by folder

Standard library only.
"""
import json
import os
import re
import sys

HERE = os.path.dirname(os.path.abspath(__file__))
ROOT = os.path.abspath(os.path.join(HERE, "..", ".."))
INDEX = os.path.join(HERE, "taint-index.json")
EXTRA = os.path.join(HERE, "taint-extra.txt")
TAINT_DOC = os.path.join(ROOT, "docs", "cleanroom", "TAINT.md")
SOURCE_DIRS = ["src", "crates", "tools", "build.rs", "docs"]
SKIP_DIRS = {"target", "vendor", "node_modules", ".git", "__pycache__"}
# Files that talk about the markers rather than carry them.
SKIP_FILES = {"tools/cleanroom/taint.py", "tools/cleanroom/test_cleanroom.py", "tools/cleanroom/lint.py",
              "docs/CLEANROOM.md"}
SKIP_PREFIXES = ("docs/cleanroom/", "docs/specs/")
MARK = re.compile(r"cleanroom-taint-(begin|end|file):\s*([a-z0-9][a-z0-9-]*)")
FILE_MARK_LINES = 5
COMMENT = {".rs": ("//", ""), ".wgsl": ("//", ""), ".js": ("//", ""), ".ts": ("//", ""), ".c": ("//", ""),
           ".h": ("//", ""), ".py": ("#", ""), ".toml": ("#", ""), ".sh": ("#", ""), ".luau": ("--", ""),
           ".lua": ("--", ""), ".md": ("<!--", " -->")}
# What a full review covers (TAINT.md's review log).
REVIEW_DIRS = ["src", "crates"]
REVIEW_EXT = {".rs", ".wgsl", ".luau", ".lua"}
REVIEW_FILES = ["docs/MECHANICS.md", "mechanics.json"]


def rel(path):
    return os.path.relpath(os.path.abspath(path), ROOT).replace(os.sep, "/")


def read(path):
    with open(path, encoding="utf-8", errors="replace") as fh:
        return fh.read()


def write(path, text):
    with open(path, "w", encoding="utf-8") as fh:
        fh.write(text)


def regions(text):
    """[(start, end, id, kind)], 1-based inclusive line numbers, markers
    included; a whole-file taint is (1, n, id, "file"). Raises ValueError on
    bad markers."""
    lines = text.split("\n")
    out, open_ = [], None
    for i, line in enumerate(lines, 1):
        m = MARK.search(line)
        if not m:
            continue
        kind, rid = m.groups()
        if kind == "file":
            if i > FILE_MARK_LINES:
                raise ValueError(f"line {i}: cleanroom-taint-file must be in the first {FILE_MARK_LINES} lines")
            return [(1, len(lines), rid, "file")]
        if kind == "begin":
            if open_:
                raise ValueError(f"line {i}: region {rid!r} begins inside {open_[1]!r}")
            open_ = (i, rid)
        else:
            if not open_ or open_[1] != rid:
                raise ValueError(f"line {i}: end of {rid!r} without its begin")
            out.append((open_[0], i, rid, "region"))
            open_ = None
    if open_:
        raise ValueError(f"line {open_[0]}: region {open_[1]!r} never ends")
    return out


def extra():
    """{relpath: id}: whole files listed in taint-extra.txt (for formats
    without comments)."""
    out = {}
    try:
        for line in read(EXTRA).splitlines():
            line = line.split("#", 1)[0].strip()
            if line:
                path, rid = line.split()
                out[path] = rid
    except OSError:
        pass
    return out


def write_extra(entries):
    head = ("# Whole files that are tainted but can't hold a marker comment (JSON and the like).\n"
            "# One per line: <path from the repo root> <id>. Managed by tools/cleanroom/taint.py.\n")
    write(EXTRA, head + "".join(f"{p} {i}\n" for p, i in sorted(entries.items())))


def regions_for(path, text=None):
    """Regions of a file, counting taint-extra.txt."""
    if text is None:
        text = read(path)
    rid = extra().get(rel(path))
    if rid:
        return [(1, len(text.split("\n")), rid, "file")]
    return regions(text)


def source_files():
    seen = set()
    for d in SOURCE_DIRS:
        p = os.path.join(ROOT, d)
        if os.path.isfile(p):
            seen.add(p)
            continue
        for base, dirs, files in os.walk(p):
            dirs[:] = [x for x in dirs if x not in SKIP_DIRS]
            for f in files:
                path = os.path.join(base, f)
                r = rel(path)
                if os.path.splitext(f)[1] in COMMENT and r not in SKIP_FILES and not r.startswith(SKIP_PREFIXES):
                    seen.add(path)
    seen.update(os.path.join(ROOT, p) for p in extra())
    return sorted(seen)


def scan():
    found, errors = {}, []
    ex = extra()
    for f in source_files():
        r_ = rel(f)
        if r_ in ex:
            if not os.path.exists(f):
                errors.append(f"{r_} is listed in taint-extra.txt but doesn't exist")
                continue
            found[r_] = regions_for(f)
            continue
        try:
            text = read(f)
        except OSError:
            continue
        if "cleanroom-taint-" not in text:
            continue
        try:
            r = regions(text)
        except ValueError as e:
            errors.append(f"{r_}: {e}")
            continue
        if r:
            found[r_] = r
    return found, errors


def documented_ids():
    try:
        text = read(TAINT_DOC)
    except OSError:
        return set()
    return set(re.findall(r"^##\s+([a-z0-9][a-z0-9-]*)\s*$", text, re.M))


def write_index(found):
    write(INDEX, json.dumps({"files": sorted(found)}, indent=1) + "\n")


def cmd_list(_):
    found, errors = scan()
    for f, rs in sorted(found.items()):
        for s, e, rid, kind in rs:
            print(f"{rid}\t{f}\t" + ("whole file" if kind == "file" else f"lines {s}-{e}"))
    for e in errors:
        print("error:", e, file=sys.stderr)
    return 1 if errors else 0


def cmd_check(_):
    found, errors = scan()
    ids = {}
    for f, rs in found.items():
        for _, _, rid, _ in rs:
            ids.setdefault(rid, f)
    for rid in sorted(set(ids) - documented_ids()):
        errors.append(f"id {rid!r} ({ids[rid]}) has no '## {rid}' section in docs/cleanroom/TAINT.md")
    try:
        indexed = json.loads(read(INDEX)).get("files", [])
    except (OSError, ValueError):
        indexed = []
    if sorted(indexed) != sorted(found):
        errors.append("tools/cleanroom/taint-index.json is out of date: run python3 tools/cleanroom/taint.py index")
    for e in errors:
        print("taint:", e, file=sys.stderr)
    if not errors:
        n = sum(len(r) for r in found.values())
        print(f"taint: {n} tainted region(s) under {len(ids)} id(s) in {len(found)} file(s), "
              "all marked and documented")
    return 1 if errors else 0


def cmd_index(_):
    found, errors = scan()
    for e in errors:
        print("taint:", e, file=sys.stderr)
    if errors:
        return 1
    write_index(found)
    print(f"taint index: {len(found)} file(s)")
    return 0


def cmd_redact(a):
    if len(a) != 1:
        return usage()
    text = read(a[0])
    rs = regions_for(a[0], text)
    lines = text.split("\n")
    i = 1
    for s, e, rid, _ in rs + [(len(lines) + 1, len(lines) + 1, None, None)]:
        while i < s:
            print(f"{i:5d}| {lines[i - 1]}")
            i += 1
        if rid:
            print(f"     | [cleanroom-taint {rid}: lines {s}-{e} withheld; see docs/cleanroom/TAINT.md#{rid}]")
            i = e + 1
    return 0


def cmd_grep(a):
    if not a:
        return usage()
    rx = re.compile(a[0])
    paths = [os.path.abspath(p) for p in a[1:]] or [ROOT]
    files = []
    for p in paths:
        if os.path.isfile(p):
            files.append(p)
        else:
            files += [f for f in source_files() if p == ROOT or os.path.abspath(f).startswith(p.rstrip(os.sep) + os.sep)]
    hits = 0
    for f in sorted(set(files)):
        try:
            text = read(f)
            rs = regions_for(f, text)
        except (OSError, ValueError):
            continue
        hidden = set()
        for s, e, _, _ in rs:
            hidden.update(range(s, e + 1))
        for n, line in enumerate(text.split("\n"), 1):
            if n not in hidden and rx.search(line):
                print(f"{rel(f)}:{n}: {line}")
                hits += 1
    return 0 if hits else 1


def fn_body(text, name):
    """(first, last) lines of a Rust/WGSL function's body, between its braces,
    so the signature stays outside the mark."""
    found = [m for m in re.finditer(rf"\bfn\s+{re.escape(name)}\b", text)]
    if len(found) != 1:
        raise SystemExit(f"taint: found {len(found)} functions named {name!r}; mark it with a line range")
    i = text.find("{", found[0].end())
    if i < 0:
        raise SystemExit(f"taint: {name!r} has no body")
    depth, j, n = 0, i, len(text)
    while j < n:
        c = text[j]
        if text.startswith("//", j):
            j = text.find("\n", j)
            j = n if j < 0 else j
            continue
        if text.startswith("/*", j):
            k = text.find("*/", j + 2)
            j = n if k < 0 else k + 2
            continue
        if c == '"':
            j += 1
            while j < n and text[j] != '"':
                j += 2 if text[j] == "\\" else 1
        elif c == "{":
            depth += 1
        elif c == "}":
            depth -= 1
            if depth == 0:
                break
        j += 1
    if depth != 0:
        raise SystemExit(f"taint: couldn't find the end of {name!r}; mark it with a line range")
    first, last = text.count("\n", 0, i) + 2, text.count("\n", 0, j)
    if last < first:
        raise SystemExit(f"taint: {name!r}'s body shares a line with its braces; mark it with a line range")
    return first, last


def doc_above(text, line):
    """Lines (first, last) of the comment block right above a line (attribute
    lines in between stay outside), or None."""
    lines = text.split("\n")
    i = line - 2
    while i >= 0 and lines[i].strip().startswith("#["):
        i -= 1
    last = i
    while i >= 0 and lines[i].strip().startswith("//"):
        i -= 1
    return (i + 2, last + 1) if i < last else None


def fn_doc(text, name):
    """The function's body and the comment block above its signature, as
    separate ranges: the signature stays visible between them."""
    first, last = fn_body(text, name)
    sig = text.count("\n", 0, re.search(rf"\bfn\s+{re.escape(name)}\b", text).start()) + 1
    doc = doc_above(text, sig)
    return ([doc] if doc else []) + [(first, last)]


def parse_mark(text, a):
    """[(id, [(start, end), ...]), ...] from `ID SPEC...` or `--id ID SPEC... --id ID SPEC...`."""
    if a and a[0] != "--id":
        a = ["--id"] + a
    groups, k = [], 0
    while k < len(a):
        if a[k] != "--id" or k + 1 >= len(a):
            raise SystemExit(f"taint: expected --id ID, got {a[k]!r}")
        rid, k = a[k + 1], k + 2
        if not re.fullmatch(r"[a-z0-9][a-z0-9-]*", rid):
            raise SystemExit("taint: ids are lowercase letters, digits and dashes")
        ranges = []
        while k < len(a) and a[k] != "--id":
            if a[k] in ("--fn", "--fn-doc") and k + 1 < len(a):
                ranges += [fn_body(text, a[k + 1])] if a[k] == "--fn" else fn_doc(text, a[k + 1])
                k += 2
                continue
            m = re.fullmatch(r"(\d+)-(\d+)", a[k])
            if not m:
                raise SystemExit(f"taint: don't understand {a[k]!r} (a range A-B, --fn NAME or --fn-doc NAME)")
            ranges.append((int(m.group(1)), int(m.group(2))))
            k += 1
        if not ranges:
            raise SystemExit(f"taint: nothing to mark for {rid!r}")
        groups.append((rid, ranges))
    return groups


def cmd_mark(a):
    # Older forms: mark FILE START END ID / mark FILE --file ID.
    if len(a) == 4 and a[1].isdigit() and a[2].isdigit():
        a = [a[0], a[3], f"{a[1]}-{a[2]}"]
    elif len(a) == 3 and a[1] == "--file":
        a = [a[0], a[2], "--file"]
    if len(a) < 3:
        return usage()
    path = a[0]
    ext = os.path.splitext(path)[1]
    if a[2:] == ["--file"]:
        rid = a[1]
        if not re.fullmatch(r"[a-z0-9][a-z0-9-]*", rid):
            raise SystemExit("taint: ids are lowercase letters, digits and dashes")
        print("taint: a whole-file mark hides the file's interface from the writer too (its types, "
              "signatures, names), so the spec must restate all of it. If only bodies or values are "
              "translated, mark those regions instead.", file=sys.stderr)
        if ext not in COMMENT:
            ex = extra()
            ex[rel(path)] = rid
            write_extra(ex)
            print(f"taint: {rel(path)} can't hold a comment, so it's listed in tools/cleanroom/taint-extra.txt",
                  file=sys.stderr)
        else:
            pre, post = COMMENT[ext]
            text = read(path)
            if "cleanroom-taint-" in text:
                raise SystemExit("taint: file already has taint markers; unmark them first")
            write(path, f"{pre} cleanroom-taint-file: {rid}{post}\n" + text)
        ids = [rid]
    else:
        if ext not in COMMENT:
            raise SystemExit(f"taint: {path} can't hold marker comments; mark it whole with --file")
        pre, post = COMMENT[ext]
        text = read(path)
        lines = text.split("\n")
        groups = parse_mark(text, a[1:])
        spans = sorted((s, e, rid) for rid, ranges in groups for s, e in ranges)
        existing = [(s, e) for s, e, _, _ in regions(text)]
        for n, (s, e, rid) in enumerate(spans):
            if not (1 <= s <= e <= len(lines)):
                raise SystemExit(f"taint: lines {s}-{e} are outside the file (1-{len(lines)})")
            if n and s <= spans[n - 1][1]:
                raise SystemExit(f"taint: ranges {spans[n - 1][0]}-{spans[n - 1][1]} and {s}-{e} overlap")
            if any(not (e < s2 or s > e2) for s2, e2 in existing):
                raise SystemExit(f"taint: lines {s}-{e} overlap an existing mark")
        for s, e, rid in reversed(spans):  # bottom up, so the given line numbers stay valid
            indent = re.match(r"\s*", lines[s - 1]).group(0)
            lines[e:e] = [f"{indent}{pre} cleanroom-taint-end: {rid}{post}"]
            lines[s - 1:s - 1] = [f"{indent}{pre} cleanroom-taint-begin: {rid}{post}"]
        new = "\n".join(lines)
        regions(new)
        write(path, new)
        ids = [rid for rid, _ in groups]
        print(f"taint: marked {len(spans)} region(s) under {len(ids)} id(s) in {rel(path)}", file=sys.stderr)
    for rid in [i for i in ids if i not in documented_ids()]:
        print(f"taint: now add a '## {rid}' section to docs/cleanroom/TAINT.md", file=sys.stderr)
    return cmd_index([])


CONTINUES = re.compile(r"^- Continues in:\s*`?([^`\s(,]+)`?(.*)$", re.M)


def continuations():
    """[(id, path, note, partly_marked)]: parts of an id that a TAINT.md
    section says live in another file. They stay listed until a reader has
    reviewed that part and deletes the line: a partial mark doesn't clear it."""
    try:
        text = read(TAINT_DOC)
    except OSError:
        return []
    found, _ = scan()
    marked = {(f, rid) for f, rs in found.items() for _, _, rid, _ in rs}
    out = []
    for m in re.finditer(r"^##\s+([a-z0-9][a-z0-9-]*)\s*$(.*?)(?=^## |\Z)", text, re.M | re.S):
        rid, body = m.groups()
        for path, note in CONTINUES.findall(body):
            out.append((rid, path, note.strip(), (path, rid) in marked))
    return out


def cmd_todo(a):
    """Ids whose TAINT.md section says "Continues in: FILE": reuse them when
    marking FILE, and delete the line once that part is reviewed."""
    want = rel(a[0]) if a else None
    todo = [t for t in continuations() if want is None or t[1] == want]
    for rid, path, note, partly in todo:
        print(f"{path}\t{rid}\t{note}" + ("\t(partly marked there already)" if partly else ""))
    if not todo:
        print("taint: nothing pending" + (f" for {want}" if want else ""), file=sys.stderr)
    return 0


def cmd_unmark(a):
    """Drop an id's marks in a file (the code stays). Only for a reader
    re-marking more precisely: unmarked translated code isn't clean code."""
    if len(a) != 2:
        return usage()
    path, rid = a
    ex = extra()
    if ex.get(rel(path)) == rid:
        del ex[rel(path)]
        write_extra(ex)
    else:
        lines = read(path).split("\n")
        keep = [l for l in lines if not ((m := MARK.search(l)) and m.group(2) == rid)]
        if len(keep) == len(lines):
            raise SystemExit(f"taint: no markers for {rid!r} in {path}")
        write(path, "\n".join(keep))
    print(f"taint: removed the marks of {rid!r} from {rel(path)}; mark it again more precisely now "
          "(unmarked translated code isn't clean)", file=sys.stderr)
    return cmd_index([])


def cmd_splice(a):
    if len(a) < 3:
        return usage()
    path, rid, rest = a[0], a[1], a[2:]
    at = None
    if "--at" in rest:
        k = rest.index("--at")
        at = int(rest[k + 1])
        rest = rest[:k] + rest[k + 2:]
    if rest == ["--delete"]:
        new_part = None
    elif len(rest) == 2 and rest[0] == "--with":
        new_part = read(rest[1])
    else:
        return usage()
    text = read(path)
    match = [r for r in regions_for(path, text) if r[2] == rid]
    if not match:
        raise SystemExit(f"taint: no region {rid!r} in {path}")
    if match[0][3] == "file":
        ex = extra()
        if ex.pop(rel(path), None):
            write_extra(ex)
        if new_part is None:
            os.remove(path)
            print(f"taint: deleted {rel(path)} (whole-file taint {rid})")
        else:
            write(path, new_part)
            print(f"taint: replaced {rel(path)} with {rest[1]} (whole-file taint {rid})")
    else:
        if at is not None:
            if not 1 <= at <= len(match):
                raise SystemExit(f"taint: {rid!r} has {len(match)} region(s) in {path}; --at takes 1-{len(match)}")
            match = [match[at - 1]]
        elif len(match) > 1 and new_part is not None:
            spans = ", ".join(f"{n}: lines {s}-{e}" for n, (s, e, _, _) in enumerate(match, 1))
            raise SystemExit(f"taint: {rid!r} has {len(match)} regions in {path} ({spans}); pick one with --at N")
        lines = text.split("\n")
        repl = [] if new_part is None else new_part.rstrip("\n").split("\n")
        for s, e, _, _ in reversed(match):
            lines[s - 1:e] = repl
        write(path, "\n".join(lines))
        print(f"taint: {'removed' if new_part is None else 'replaced'} {len(match)} region(s) of {rid} "
              f"in {rel(path)}")
    print(f"taint: when all of {rid!r} is gone, mark its section done in docs/cleanroom/TAINT.md and lower "
          "the lint baseline (python3 tools/cleanroom/lint.py source --update-baseline)", file=sys.stderr)
    return cmd_index([])


REVIEW = re.compile(r"^- `?([^`:\s]+)`?:\s*(clean|tainted|partly)\b", re.M)
CLAIM = re.compile(r"^- `?([^`:\s]+)`?:\s*(.+)$", re.M)


def doc_section(title):
    try:
        text = read(TAINT_DOC)
    except OSError:
        return ""
    m = re.search(rf"^## {re.escape(title)}\s*$(.*?)(?=^## |\Z)", text, re.M | re.S)
    return m.group(1) if m else ""


def review_files():
    files = {}
    for d in REVIEW_DIRS:
        for base, dirs, names in os.walk(os.path.join(ROOT, d)):
            dirs[:] = [x for x in dirs if x not in SKIP_DIRS]
            for n in names:
                if os.path.splitext(n)[1] in REVIEW_EXT:
                    path = os.path.join(base, n)
                    files[rel(path)] = read(path).count("\n") + 1
    for f in REVIEW_FILES:
        path = os.path.join(ROOT, f)
        if os.path.exists(path):
            files[f] = read(path).count("\n") + 1
    return files


def cmd_coverage(a):
    log = dict(REVIEW.findall(doc_section("Review log")))
    files = review_files()
    done = {f: n for f, n in files.items() if log.get(f) in ("clean", "tainted")}
    total, lines_done = sum(files.values()), sum(done.values())
    print(f"reviewed: {len(done)} of {len(files)} files, {lines_done} of {total} lines "
          f"({100 * lines_done / max(total, 1):.0f}%); "
          f"{sum(1 for f in done if log[f] == 'tainted')} tainted, {sum(1 for f in done if log[f] == 'clean')} clean")
    partly = sorted(f for f in files if log.get(f) == "partly")
    if partly:
        print("partly reviewed: " + ", ".join(partly))
    claims = CLAIM.findall(doc_section("In progress"))
    if claims:
        print("in progress: " + "; ".join(f"{p} ({who.strip()})" for p, who in claims))
    pending = continuations()
    if pending:
        print(f"pending continuations: {len(pending)} id part(s) noted in other files, not yet reviewed there "
              "(taint.py todo)")
    unknown = sorted(set(log) - set(files))
    if unknown:
        print("in the log but not found: " + ", ".join(unknown))
    left = {f: n for f, n in files.items() if f not in done}
    if "--all" in a:
        for f in sorted(left, key=lambda f: -left[f]):
            print(f"  {left[f]:7d}  {f}")
        return 0
    by_dir = {}
    for f, n in left.items():
        d = os.path.dirname(f) or "."
        c, l = by_dir.get(d, (0, 0))
        by_dir[d] = (c + 1, l + n)
    print("left to review, by folder (lines, files):")
    for d, (c, l) in sorted(by_dir.items(), key=lambda x: -x[1][1]):
        print(f"  {l:7d} lines  {c:4d} files  {d}/")
    return 0


def usage():
    print(__doc__, file=sys.stderr)
    return 2


def main(a):
    cmds = {"list": cmd_list, "check": cmd_check, "index": cmd_index, "redact": cmd_redact, "grep": cmd_grep,
            "mark": cmd_mark, "unmark": cmd_unmark, "splice": cmd_splice, "coverage": cmd_coverage, "todo": cmd_todo}
    if not a or a[0] not in cmds:
        return usage()
    return cmds[a[0]](a[1:])


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
