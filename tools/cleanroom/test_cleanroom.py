#!/usr/bin/env python3
"""Tests for the clean-room lint and hook (docs/CLEANROOM.md).

  python3 tools/cleanroom/test_cleanroom.py

Standard library only. The hook is run the way Claude Code runs it: JSON on
stdin, exit 2 to block.
"""
import json
import os
import subprocess
import sys
import tempfile
import unittest

HERE = os.path.dirname(os.path.abspath(__file__))
ROOT = os.path.abspath(os.path.join(HERE, "..", ".."))
LINT = os.path.join(HERE, "lint.py")
HOOK = os.path.join(ROOT, ".claude", "hooks", "cleanroom_guard.py")
sys.path.insert(0, HERE)
import lint  # noqa: E402

GOOD_SPEC = """# Sun highlight: behaviour spec

## Scope
How bright the sun's highlight on a part is.

## Inputs
`Lighting.EnvironmentSpecularScale`, `MaterialVariant.RoughnessMap`.

## Behaviour
The highlight narrows as roughness falls. With h the half vector between the
view and sun directions, the lobe's falloff follows 1 / (1 + k (1 - n.h)^2).

## Expected results
A sunlit face at roughness 1 brightens by at most 4 levels (sRGB) over the same
face with the sun's highlight removed.

## Edge cases and open questions
None yet.

## Provenance
- Studio version: version-67c8b1dedbb74fed
- Kinds of source used: black-box screenshots
- Reader: test, 2026-11-10
"""


def lint_text(text, name="spec.md"):
    with tempfile.TemporaryDirectory() as d:
        p = os.path.join(d, name)
        with open(p, "w", encoding="utf-8") as fh:
            fh.write(text)
        r = subprocess.run([sys.executable, LINT, "spec", p], capture_output=True, text=True)
        return r.returncode, r.stdout + r.stderr


START = os.path.join(ROOT, ".claude", "hooks", "cleanroom_start.py")


def hook(agent, tool, tool_input, raw=None, project=ROOT, session="test-session", event="PreToolUse", repo=None):
    data = raw if raw is not None else json.dumps(
        {"agent_type": agent, "tool_name": tool, "tool_input": tool_input, "cwd": project, "session_id": session,
         "hook_event_name": event})
    env = dict(os.environ, CLAUDE_PROJECT_DIR=project)
    if repo:
        env["CLEANROOM_TEST_REPO"] = repo
    r = subprocess.run([sys.executable, HOOK], input=data, capture_output=True, text=True, env=env)
    return r.returncode


def start_context(project, session="test-session", agent=None):
    env = dict(os.environ, CLAUDE_PROJECT_DIR=project)
    data = json.dumps({"session_id": session, "cwd": project, "agent_type": agent, "source": "startup"})
    r = subprocess.run([sys.executable, START], input=data, capture_output=True, text=True, env=env)
    return json.loads(r.stdout)["hookSpecificOutput"]["additionalContext"]


class SpecLint(unittest.TestCase):
    def test_good_spec_passes(self):
        code, out = lint_text(GOOD_SPEC)
        self.assertEqual(code, 0, out)

    def test_template_passes(self):
        r = subprocess.run([sys.executable, LINT, "spec", os.path.join(ROOT, "docs", "specs", "TEMPLATE.md")],
                           capture_output=True, text=True)
        self.assertEqual(r.returncode, 0, r.stdout)

    def bad(self, insert, rule):
        code, out = lint_text(GOOD_SPEC.replace("None yet.", insert))
        self.assertEqual(code, 1, f"{rule} not caught: {insert!r}")
        self.assertIn(rule, out)

    def test_rules(self):
        self.bad("```\nx\n```", "code-fence")
        self.bad("It reads _2293 twice.", "decompiled-temp")
        self.bad("Scaled by CB0[15].", "constant-buffer")
        self.bad("Uses VARYING4 for depth.", "shader-varying")
        self.bad("As in SomeTiledFS.3 here.", "shader-program")
        self.bad("See FUN_146654ff0.", "disassembly-name")
        self.bad("At sub_1407db6c0.", "disassembly-name")
        self.bad("Then vec3(1.0) is added.", "glsl-hlsl")
        self.bad("Then gl_FrontFacing flips it.", "shader-builtin")
        self.bad("Stored at 0x146654ff0.", "address")
        self.bad("x = y * 2;", "code-line")

    def test_prose_is_not_flagged(self):
        for ok in ["The FACS rig and MECHANICS file are fine.", "Roughness 0.5 gives 1 / (1 + k).",
                   "Item 1_000 and snake_case names are fine.", "GetRealPhysicsFPS is an API member."]:
            code, out = lint_text(GOOD_SPEC.replace("None yet.", ok))
            self.assertEqual(code, 0, f"{ok!r} flagged:\n{out}")

    def test_structure(self):
        code, out = lint_text(GOOD_SPEC.replace("## Expected results", "## Notes"))
        self.assertEqual(code, 1)
        self.assertIn("missing section '## Expected results'", out)
        code, out = lint_text(GOOD_SPEC.replace("- Studio version: version-67c8b1dedbb74fed", "- Studio version:"))
        self.assertEqual(code, 1)
        self.assertIn("Studio version", out)
        empty = GOOD_SPEC.replace("How bright the sun's highlight on a part is.", "")
        code, out = lint_text(empty)
        self.assertEqual(code, 1)
        self.assertIn("'## Scope' is empty", out)


class TaintRegisterLint(unittest.TestCase):
    def check(self, text):
        with tempfile.TemporaryDirectory() as d:
            p = os.path.join(d, "TAINT.md")
            with open(p, "w", encoding="utf-8") as fh:
                fh.write(text)
            r = subprocess.run([sys.executable, LINT, "taint", p], capture_output=True, text=True)
            return r.returncode, r.stdout

    def test_register(self):
        ok = ("# Tainted code\n\n## demo\n- Evidence: high: comments cite decompiled function addresses; "
              "the uniform buffer layout matches; values end in a semicolon;\n")
        self.assertEqual(self.check(ok)[0], 0)
        for leak in ["uses _2293", "reads CB0[15]", "from SomeTiledFS.3", "see FUN_146654ff0", "at 0x146654ff0",
                     "gl_FrontFacing flips it"]:
            self.assertEqual(self.check(ok + leak + "\n")[0], 1, leak)

    def test_repo_register_is_clean(self):
        r = subprocess.run([sys.executable, LINT, "taint"], capture_output=True, text=True)
        self.assertEqual(r.returncode, 0, r.stdout + r.stderr)


class SourceLint(unittest.TestCase):
    def test_repo_matches_baseline(self):
        r = subprocess.run([sys.executable, LINT, "source"], capture_output=True, text=True)
        self.assertEqual(r.returncode, 0, r.stdout + r.stderr)

    def test_source_rules(self):
        names = lambda t: {n for n, _, _ in lint.findings(t, "source")}
        self.assertIn("decompiled-temp", names("// matches _1849 in the original"))
        self.assertIn("shader-pack", names("// from shaders_glsl3.pack"))
        self.assertNotIn("glsl-hlsl", names("let v = vec3<f32>(1.0);"))
        self.assertEqual(names("let x = 1_000; // FACS, EPS, MECHANICS"), set())


class Hook(unittest.TestCase):
    PACK = "/home/someone/.var/app/org.vinegarhq.Vinegar/data/vinegar/versions/v/shaders/shaders_glsl3.pack"

    def test_writer_blocked_from_internals(self):
        self.assertEqual(hook("cleanroom-writer", "Read", {"file_path": self.PACK}), 2)
        self.assertEqual(hook("cleanroom-writer", "Read", {"file_path": "C:/Program Files/Roblox/Versions/v/RobloxStudioBeta.exe"}), 2)
        self.assertEqual(hook("cleanroom-writer", "Bash", {"command": "strings shaders_d3d11.pack | less"}), 2)
        self.assertEqual(hook("cleanroom-writer", "Glob", {"pattern": "**/*.pack"}), 2)
        self.assertEqual(hook("cleanroom-writer", "Grep", {"pattern": "x", "path": "cleanroom-reference"}), 2)
        self.assertEqual(hook("cleanroom-writer", "Read", {"file_path": "cleanroom-reference/notes.md"}), 2)

    def test_writer_allowed_normal_work(self):
        # (files that are never tainted: the real repo's marks change as readers work)
        self.assertEqual(hook("cleanroom-writer", "Read", {"file_path": "README.md"}), 0)
        self.assertEqual(hook("cleanroom-writer", "Read", {"file_path": "docs/specs/TEMPLATE.md"}), 0)
        self.assertEqual(hook("cleanroom-writer", "Edit", {"file_path": "README.md"}), 0)
        self.assertEqual(hook("cleanroom-writer", "Bash", {"command": "cargo test --workspace"}), 0)

    def test_reader_writes_only_specs(self):
        self.assertEqual(hook("cleanroom-reader", "Edit", {"file_path": "src/shader.wgsl"}), 2)
        self.assertEqual(hook("cleanroom-reader", "Write", {"file_path": "AGENTS.md"}), 2)
        self.assertEqual(hook("cleanroom-reader", "Write", {"file_path": "docs/specs/x.md"}), 0)
        self.assertEqual(hook("cleanroom-reader", "Write", {"file_path": "cleanroom-reference/dump.txt"}), 0)
        self.assertEqual(hook("cleanroom-reader", "Read", {"file_path": self.PACK}), 0)
        self.assertEqual(hook("cleanroom-reader", "Write", {"file_path": "docs/specs/../../src/x.rs"}), 2)

    def folder(self, role):
        d = tempfile.mkdtemp()
        self.addCleanup(lambda: __import__("shutil").rmtree(d, ignore_errors=True))
        if role:
            os.makedirs(os.path.join(d, ".claude"))
            with open(os.path.join(d, ".claude", "settings.local.json"), "w") as fh:
                json.dump({"agent": role}, fh)
        return d

    def test_unclaimed_session_is_guarded(self):
        d = self.folder(None)
        self.assertEqual(hook("", "Read", {"file_path": self.PACK}, project=d), 2)
        self.assertEqual(hook("Explore", "Read", {"file_path": self.PACK}, project=d), 2)
        self.assertEqual(hook("", "Edit", {"file_path": os.path.join(d, "src", "x.rs")}, project=d), 2)
        self.assertEqual(hook("", "Read", {"file_path": os.path.join(d, "src", "x.rs")}, project=d), 0)
        self.assertEqual(hook("", "Bash", {"command": "cargo build"}, project=d), 0)
        self.assertEqual(hook("", "Write", {"file_path": "/tmp/elsewhere/notes.txt"}, project=d), 0)

    def test_claiming_a_role(self):
        d = self.folder(None)
        def claim(role, sess="s1"):  # as Claude Code does: check before, record after it ran
            cmd = {"command": f"tools/cleanroom/role.sh {role}"}
            pre = hook("", "Bash", cmd, project=d, session=sess)
            if pre == 0:
                hook("", "Bash", cmd, project=d, session=sess, event="PostToolUse")
            return pre
        # a claim that never ran (the user declined it) grants nothing
        self.assertEqual(hook("", "Bash", {"command": "tools/cleanroom/role.sh writer"}, project=d, session="s0"), 0)
        self.assertEqual(hook("", "Edit", {"file_path": os.path.join(d, "src", "x.rs")}, project=d, session="s0"), 2)
        self.assertEqual(claim("writer"), 0)
        self.assertEqual(hook("", "Edit", {"file_path": os.path.join(d, "src", "x.rs")}, project=d, session="s1"), 0)
        self.assertEqual(hook("Explore", "Read", {"file_path": self.PACK}, project=d, session="s1"), 2)
        # another session in the same folder is still unclaimed
        self.assertEqual(hook("", "Edit", {"file_path": os.path.join(d, "src", "x.rs")}, project=d, session="s2"), 2)
        self.assertEqual(claim("reader", "s2"), 0)
        self.assertEqual(hook("", "Read", {"file_path": self.PACK}, project=d, session="s2"), 0)
        self.assertEqual(hook("", "Edit", {"file_path": os.path.join(d, "src", "x.rs")}, project=d, session="s2"), 2)
        # a reader never becomes the writer; a writer may become a reader
        self.assertEqual(claim("writer", "s2"), 2)
        self.assertEqual(claim("reader", "s1"), 0)
        self.assertEqual(claim("writer", "s1"), 2)

    def test_fixed_roles_dont_switch(self):
        w = self.folder("cleanroom-writer")
        self.assertEqual(hook("", "Bash", {"command": "tools/cleanroom/role.sh reader"}, project=w), 2)
        self.assertEqual(hook("cleanroom-reader", "Bash", {"command": "tools/cleanroom/role.sh writer"}), 2)

    def test_helpers_take_the_folders_role(self):
        d = self.folder("cleanroom-writer")
        self.assertEqual(hook("", "Read", {"file_path": self.PACK}, project=d), 2)
        self.assertEqual(hook("Explore", "Read", {"file_path": self.PACK}, project=d), 2)
        self.assertEqual(hook("general-purpose", "Bash", {"command": "cat x/shaders_glsl3.pack"}, project=d), 2)
        self.assertEqual(hook("Explore", "Read", {"file_path": "src/main.rs"}, project=d), 0)
        # /cleanroom's reader keeps its own role inside the writer's folder
        self.assertEqual(hook("cleanroom-reader", "Read", {"file_path": self.PACK}, project=d), 0)
        r = self.folder("cleanroom-reader")
        self.assertEqual(hook("general-purpose", "Write", {"file_path": os.path.join(r, "src", "x.rs")}, project=r), 2)

    def test_session_start_message(self):
        d = self.folder(None)
        text = start_context(d)
        for must in ("no role yet", "Contamination check", "role.sh reader", "role.sh writer", "first reply"):
            self.assertIn(must, text)
        hook("", "Bash", {"command": "tools/cleanroom/role.sh writer"}, project=d, session="s9", event="PostToolUse")
        self.assertIn("this session is the WRITER", start_context(d, session="s9"))
        self.assertIn("this session is the READER", start_context(d, agent="cleanroom-reader"))

    def test_reader_messages_are_notices_only(self):
        send = lambda agent, msg: hook(agent, "SendMessage", {"to": "rustudio-14", "message": msg})
        self.assertEqual(send("cleanroom-reader", "spec ready: docs/specs/sun-specular.md"), 0)
        self.assertEqual(send("cleanroom-reader", "spec updated: docs/specs/sun-specular.md (answered 1, 3)"), 0)
        self.assertEqual(send("cleanroom-reader", "  spec ready: docs/specs/a-b2.md\n"), 0)
        self.assertEqual(send("cleanroom-reader", ""), 0)  # an idle subscription carries nothing
        for leak in ["spec ready: docs/specs/x.md -- the highlight uses a + a^2",
                     "The sun term is divided by 3 lh + 0.5",
                     "spec ready: ../../src/x.md",
                     "spec updated: docs/specs/x.md (answered 1) also see the pack"]:
            self.assertEqual(send("cleanroom-reader", leak), 2, leak)

    def test_writer_messages_freely(self):
        msg = "Question 2 added: on a sunlit face at roughness 1 I measure +6 levels; is that expected?"
        self.assertEqual(hook("cleanroom-writer", "SendMessage", {"to": "rustudio-cleanroom-reader-82", "message": msg}), 0)

    def test_bad_input_blocks(self):
        self.assertEqual(hook(None, None, None, raw="not json"), 2)


class TaintedCode(unittest.TestCase):
    """taint.py and the hook's tainted-code rules, on a scratch repo."""

    MIXED = "\n".join(["fn clean_a() {}", "",
                       "// cleanroom-taint-begin: demo-shading",
                       "fn translated() {", "    let secret = 2293;", "}",
                       "// cleanroom-taint-end: demo-shading",
                       "", "fn clean_b() {}", ""])

    def setUp(self):
        import shutil
        self.repo = tempfile.mkdtemp()
        self.addCleanup(lambda: shutil.rmtree(self.repo, ignore_errors=True))
        tools = os.path.join(self.repo, "tools", "cleanroom")
        os.makedirs(tools)
        for f in ("taint.py", "reference_paths.txt"):
            shutil.copy(os.path.join(HERE, f), tools)
        os.makedirs(os.path.join(self.repo, "src"))
        os.makedirs(os.path.join(self.repo, "docs", "cleanroom"))
        self.write("src/mixed.rs", self.MIXED)
        self.write("src/whole.rs", "// cleanroom-taint-file: demo-grid\nfn port() {}\n")
        self.write("src/clean.rs", "fn fine() {}\n")
        self.write("docs/cleanroom/TAINT.md", "# Tainted code\n\n## demo-shading\n\n## demo-grid\n")
        self.assertEqual(self.taint("index").returncode, 0)

    def write(self, path, text):
        with open(os.path.join(self.repo, path), "w", encoding="utf-8") as fh:
            fh.write(text)

    def taint(self, *args):
        return subprocess.run([sys.executable, os.path.join(self.repo, "tools", "cleanroom", "taint.py")] + list(args),
                              capture_output=True, text=True, cwd=self.repo)

    def h(self, agent, tool, tool_input, session="t1"):
        return hook(agent, tool, tool_input, project=self.repo, repo=self.repo, session=session)

    def p(self, path):
        return os.path.join(self.repo, path)

    def rd(self, path):
        with open(self.p(path), encoding="utf-8") as fh:
            return fh.read()

    def test_taint_tool(self):
        self.assertEqual(self.taint("check").returncode, 0, self.taint("check").stderr)
        red = self.taint("redact", self.p("src/mixed.rs")).stdout
        self.assertIn("clean_a", red)
        self.assertIn("clean_b", red)
        self.assertIn("lines 3-7 withheld", red)
        self.assertNotIn("secret", red)
        g = self.taint("grep", "fn|secret").stdout
        self.assertIn("clean_a", g)
        self.assertNotIn("secret", g)
        self.assertNotIn("port", g)
        self.write("docs/cleanroom/TAINT.md", "# Tainted code\n\n## demo-grid\n")
        self.assertEqual(self.taint("check").returncode, 1)  # demo-shading undocumented
        self.write("src/new.rs.part", "fn rewritten() {}\n")
        self.assertEqual(self.taint("splice", self.p("src/mixed.rs"), "demo-shading", "--with", self.p("src/new.rs.part")).returncode, 0)
        text = self.rd("src/mixed.rs")
        self.assertIn("fn rewritten() {}", text)
        self.assertNotIn("secret", text)
        self.assertNotIn("cleanroom-taint", text)
        self.assertEqual(self.taint("splice", self.p("src/whole.rs"), "demo-grid", "--delete").returncode, 0)
        self.assertFalse(os.path.exists(self.p("src/whole.rs")))
        self.assertEqual(json.loads(self.rd("tools/cleanroom/taint-index.json"))["files"], [])

    def test_coverage(self):
        self.write("docs/cleanroom/TAINT.md", "# Tainted code\n\n## demo-shading\n\n## demo-grid\n\n"
                   "## Review log\n\n- `src/mixed.rs`: tainted (demo-shading) — r, today\n"
                   "- `src/clean.rs`: clean — r, today\n- `src/whole.rs`: partly (rest) — r, today\n")
        out = self.taint("coverage").stdout
        self.assertIn("reviewed: 2 of 3 files", out)
        self.assertIn("1 tainted, 1 clean", out)
        self.assertIn("partly reviewed: src/whole.rs", out)
        self.assertIn("src/whole.rs", self.taint("coverage", "--all").stdout)

    def test_mark_several_ranges_one_id(self):
        self.write("src/long.rs", "\n".join(f"line{i}" for i in range(1, 11)) + "\n")
        self.write("docs/cleanroom/TAINT.md", self.rd("docs/cleanroom/TAINT.md") + "\n## demo-multi\n")
        r = self.taint("mark", self.p("src/long.rs"), "demo-multi", "6-7", "2-3")  # any order, original numbers
        self.assertEqual(r.returncode, 0, r.stderr)
        text = self.rd("src/long.rs").split("\n")
        self.assertEqual(text[1], "// cleanroom-taint-begin: demo-multi")
        self.assertEqual(text[2:4], ["line2", "line3"])
        self.assertEqual(text[4:6], ["// cleanroom-taint-end: demo-multi", "line4"])
        self.assertEqual(text[7:11], ["// cleanroom-taint-begin: demo-multi", "line6", "line7",
                                      "// cleanroom-taint-end: demo-multi"])
        self.assertEqual(self.taint("check").returncode, 0)
        self.assertEqual(self.taint("mark", self.p("src/long.rs"), "demo-x", "2-4").returncode, 1)  # overlaps
        # splicing one of several regions needs --at
        self.write("src/part.txt", "fresh\n")
        self.assertNotEqual(self.taint("splice", self.p("src/long.rs"), "demo-multi", "--with", self.p("src/part.txt")).returncode, 0)
        self.assertEqual(self.taint("splice", self.p("src/long.rs"), "demo-multi", "--with", self.p("src/part.txt"), "--at", "2").returncode, 0)
        after = self.rd("src/long.rs")
        self.assertIn("fresh", after)
        self.assertNotIn("line6", after)
        self.assertIn("line2", after)  # region 1 untouched
        self.assertEqual(self.taint("splice", self.p("src/long.rs"), "demo-multi", "--delete").returncode, 0)
        self.assertNotIn("cleanroom-taint", self.rd("src/long.rs"))

    def test_mark_function_body(self):
        self.write("src/f.rs", "pub fn helper(x: u32) -> u32 {\n    let y = x + 1; // {\n    y\n}\nfn other() {}\n")
        self.write("docs/cleanroom/TAINT.md", self.rd("docs/cleanroom/TAINT.md") + "\n## demo-fn\n")
        r = self.taint("mark", self.p("src/f.rs"), "demo-fn", "--fn", "helper")
        self.assertEqual(r.returncode, 0, r.stderr)
        red = self.taint("redact", self.p("src/f.rs")).stdout
        self.assertIn("pub fn helper(x: u32) -> u32 {", red)  # signature stays visible
        self.assertNotIn("let y", red)
        self.assertIn("fn other", red)

    def test_mark_several_ids_and_fn_doc(self):
        self.write("src/m.rs", "\n".join([
            "/// Ported from the internal routine.",      # 1
            "/// Second doc line.",                       # 2
            "#[inline]",                                  # 3
            "pub fn glow(x: f32) -> f32 {",               # 4
            "    x * 2.0",                                # 5
            "}",                                          # 6
            "const A: f32 = 1.0;",                        # 7
            "const B: f32 = 2.0;",                        # 8
            ""]))
        self.write("docs/cleanroom/TAINT.md", self.rd("docs/cleanroom/TAINT.md") + "\n## demo-glow\n\n## demo-consts\n")
        r = self.taint("mark", self.p("src/m.rs"), "--id", "demo-consts", "7-8", "--id", "demo-glow", "--fn-doc", "glow")
        self.assertEqual(r.returncode, 0, r.stderr)
        red = self.taint("redact", self.p("src/m.rs")).stdout
        self.assertIn("#[inline]", red)                         # attribute stays visible
        self.assertIn("pub fn glow(x: f32) -> f32 {", red)      # signature stays visible
        self.assertNotIn("Ported", red)                         # doc comment withheld
        self.assertNotIn("x * 2.0", red)                        # body withheld
        self.assertNotIn("const A", red)
        listing = self.taint("list").stdout
        self.assertEqual(listing.count("demo-glow"), 2)
        self.assertEqual(listing.count("demo-consts"), 1)
        self.assertEqual(self.taint("check").returncode, 0)

    def test_todo_for_ids_spanning_files(self):
        self.write("src/shade.wgsl", "fn neon() -> f32 {\n    return 1.0;\n}\n")
        self.write("docs/cleanroom/TAINT.md", self.rd("docs/cleanroom/TAINT.md").replace(
            "## demo-shading\n", "## demo-shading\n- Continues in: `src/shade.wgsl` (the neon part)\n"))
        out = self.taint("todo", self.p("src/shade.wgsl")).stdout
        self.assertIn("demo-shading", out)
        self.assertIn("pending continuations: 1", self.taint("coverage").stdout)
        self.assertEqual(self.taint("mark", self.p("src/shade.wgsl"), "demo-shading", "--fn", "neon").returncode, 0)
        # a partial mark doesn't clear the reminder; only the reader deleting the line does
        out = self.taint("todo", self.p("src/shade.wgsl")).stdout
        self.assertIn("demo-shading", out)
        self.assertIn("partly marked", out)
        self.write("docs/cleanroom/TAINT.md", self.rd("docs/cleanroom/TAINT.md").replace(
            "- Continues in: `src/shade.wgsl` (the neon part)\n", ""))
        self.assertNotIn("demo-shading", self.taint("todo", self.p("src/shade.wgsl")).stdout)
        self.assertEqual(self.taint("check").returncode, 0)

    def test_markdown_and_json(self):
        self.write("docs/MECHANICS.md", "# Mechanics\n\n- fog start: 0.25\n- other: 1\n")
        self.write("mechanics.json", '{"fog_start": 0.25}\n')
        self.write("docs/cleanroom/TAINT.md", self.rd("docs/cleanroom/TAINT.md") + "\n## demo-md\n\n## demo-json\n")
        self.assertEqual(self.taint("mark", self.p("docs/MECHANICS.md"), "demo-md", "3-3").returncode, 0)
        self.assertIn("<!-- cleanroom-taint-begin: demo-md -->", self.rd("docs/MECHANICS.md"))
        self.assertEqual(self.taint("mark", self.p("mechanics.json"), "demo-json", "--file").returncode, 0)
        self.assertIn("mechanics.json demo-json", self.rd("tools/cleanroom/taint-extra.txt"))
        files = json.loads(self.rd("tools/cleanroom/taint-index.json"))["files"]
        self.assertIn("docs/MECHANICS.md", files)
        self.assertIn("mechanics.json", files)
        self.assertEqual(self.taint("check").returncode, 0)
        self.assertNotIn("0.25", self.taint("redact", self.p("mechanics.json")).stdout)
        W = "cleanroom-writer"
        self.assertEqual(self.h(W, "Read", {"file_path": self.p("mechanics.json")}), 2)
        self.assertEqual(self.h(W, "Read", {"file_path": self.p("docs/MECHANICS.md")}), 2)
        self.assertEqual(self.h(W, "Read", {"file_path": self.p("docs/MECHANICS.md"), "offset": 1, "limit": 2}), 0)
        self.assertEqual(self.taint("splice", self.p("mechanics.json"), "demo-json", "--delete").returncode, 0)
        self.assertNotIn("mechanics.json", self.rd("tools/cleanroom/taint-extra.txt"))

    def test_coverage_shows_claims(self):
        self.write("docs/cleanroom/TAINT.md", self.rd("docs/cleanroom/TAINT.md") +
                   "\n## In progress\n\n- `src/`: reader tab 2, today\n\n## Review log\n\n")
        self.assertIn("in progress: src/ (reader tab 2, today)", self.taint("coverage").stdout)

    def test_bad_markers(self):
        self.write("src/broken.rs", "// cleanroom-taint-begin: oops\nfn x() {}\n")
        self.assertEqual(self.taint("check").returncode, 1)

    def test_unmark_to_remark(self):
        self.assertEqual(self.taint("unmark", self.p("src/whole.rs"), "demo-grid").returncode, 0)
        self.assertNotIn("cleanroom-taint", self.rd("src/whole.rs"))
        self.assertIn("fn port", self.rd("src/whole.rs"))  # the code itself stays
        self.assertEqual(self.taint("mark", self.p("src/whole.rs"), "1", "1", "demo-grid").returncode, 0)
        self.assertEqual(self.taint("check").returncode, 0)

    def test_mark(self):
        self.assertEqual(self.taint("mark", self.p("src/clean.rs"), "1", "1", "demo-new").returncode, 0)
        self.assertIn("cleanroom-taint-begin: demo-new", self.rd("src/clean.rs"))
        self.assertIn("src/clean.rs", json.loads(self.rd("tools/cleanroom/taint-index.json"))["files"])

    def test_writer_never_sees_tainted_code(self):
        W = "cleanroom-writer"
        mixed, whole = self.p("src/mixed.rs"), self.p("src/whole.rs")
        self.assertEqual(self.h(W, "Read", {"file_path": mixed}), 2)                       # whole file read
        self.assertEqual(self.h(W, "Read", {"file_path": mixed, "offset": 1, "limit": 2}), 0)
        self.assertEqual(self.h(W, "Read", {"file_path": mixed, "offset": 8, "limit": 3}), 0)
        self.assertEqual(self.h(W, "Read", {"file_path": mixed, "offset": 2, "limit": 3}), 2)
        self.assertEqual(self.h(W, "Read", {"file_path": whole, "offset": 2, "limit": 1}), 2)
        self.assertEqual(self.h(W, "Edit", {"file_path": mixed, "old_string": "fn clean_b() {}", "new_string": "x"}), 0)
        self.assertEqual(self.h(W, "Edit", {"file_path": mixed, "old_string": "let secret", "new_string": "x"}), 2)
        self.assertEqual(self.h(W, "Write", {"file_path": mixed, "content": "x"}), 2)
        self.assertEqual(self.h(W, "Bash", {"command": "cat src/mixed.rs"}), 2)
        self.assertEqual(self.h(W, "Bash", {"command": "git diff -- whole.rs"}), 2)
        self.assertEqual(self.h(W, "Bash", {"command": "python3 tools/cleanroom/taint.py redact src/mixed.rs"}), 0)
        self.assertEqual(self.h(W, "Bash", {"command": "cargo test"}), 0)
        self.assertEqual(self.h(W, "Grep", {"pattern": "fn", "path": self.p("src"), "output_mode": "content"}), 2)
        self.assertEqual(self.h(W, "Grep", {"pattern": "fn", "path": self.p("src")}), 0)  # file names only
        self.assertEqual(self.h(W, "Read", {"file_path": self.p("src/clean.rs")}), 0)
        # an unclaimed session is held to the same, the reader isn't
        self.assertEqual(self.h("", "Read", {"file_path": mixed}), 2)
        self.assertEqual(self.h("cleanroom-reader", "Read", {"file_path": mixed}), 0)
        self.assertEqual(self.h("cleanroom-reader", "Write", {"file_path": self.p("docs/cleanroom/TAINT.md")}), 0)


if __name__ == "__main__":
    unittest.main(verbosity=1)
