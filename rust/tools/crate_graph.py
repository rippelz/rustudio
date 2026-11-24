#!/usr/bin/env python3
"""crate_graph.py - rustudio's module import graph and wrong-way reference check.

Walks every workspace crate's module tree from its crate root (``src/lib.rs``
when it exists, otherwise ``src/main.rs``), follows ``#[path]`` attributes the
way ``mod`` resolution does, and records every ``crate::``, ``super::`` and
``rustudio_<crate>::`` reference between top-level modules.  Top-level modules
are mapped to their workspace owners and upward dependencies are reported.
A reference through a submodule is attributed to its top-level module.

Run from the repository root:

    python3 tools/crate_graph.py                    # lines per crate + wrong-way refs
    python3 tools/crate_graph.py --check            # exit 1 on any wrong-way ref
    python3 tools/crate_graph.py --check core,physics,render
    python3 tools/crate_graph.py --self-test        # scanner fixtures
    python3 tools/crate_graph.py --cycles           # import cycles (SCCs)
    python3 tools/crate_graph.py --editor-impls     # `impl Editor` outside app
    python3 tools/crate_graph.py --json graph.json  # raw graph

``--check`` only fails on references that *start* in the named crates, so you
can check a subset of the workspace.
References inside ``#[cfg(test)]`` code are counted separately and do not fail
a check unless ``--tests`` is given.

This is a source scanner. It follows literal Rust includes and explicit root
imports and recognizes qualified paths. It does not expand macros (including
computed/generated includes), resolve globs or lexical shadowing, infer types
or method calls, or evaluate cfg_attr and target/feature predicates. Repeated
inclusion of a source file is attributed to its first discovered module.
Use the Rust compiler for full resolution.

Python 3 standard library only. Run from the repository root.
"""

import argparse
import tempfile
from pathlib import Path
import bisect
import collections
import json
import os
import re
import sys

# ============================ ASSIGNMENT TABLE ==============================
# Top-level module owners in dependency order. Children share their parent's
# owner. Unlisted modules belong to Studio.

CRATE_MODULES = {
    # ~25k lines: document, math, shared data types, asset cache, terrain data,
    # logging, bench.  May use rbx_*, glam, rustudio-account only.
    'core': (
        'math', 'mech', 'paths', 'platform', 'heap', 'sim_clock', 'load_trace',
        'log', 'bench', 'crash', 'api', 'api_stubs', 'telemetry',
        'collision_groups', 'editable_source',
        'doc', 'mesh', 'mesh_bvh', 'vertex', 'truss', 'fbx', 'fbx_anim', 'cage_rbf',
        'texture', 'texture_pack', 'game_settings', 'rustudio_settings', 'project_values',
        'assets', 'studio_content', 'terrain', 'terrain_region', 'terrain_mat',
        'grass_stream', 'default_scripts', 'rig_type',
        # Attachment frames are read by
        # physics and read/written by the renderer, so they live in core.
        'attachment',
        # Body-part names, rigid joints, humanoid states and
        # joint frames - physics, skin and rig all read them.
        'body',
    ),
    # ~27k lines: solver, rigid bodies, collision, queries, humanoid physics.
    'physics': (
        'solver', 'solver_ticks', 'physics', 'physics_mesh', 'collision',
        'query_shapes', 'humanoid', 'client_joints',
    ),
    # ~23k lines: GPU renderer, scene build, lighting, skinning, effects.
    'render': (
        'render', 'gpu_timing', 'world_gui', 'sun_shadow', 'highlight', 'water',
        'clouds', 'skin', 'skin_gpu', 'wrap', 'hsr', 'lightgrid', 'lightocc',
        'matpack', 'grass_temporal', 'effects', 'lighting', 'scene', 'camera',
    ),
    # ~80k lines: Luau runtime, API domains, replication, GUI objects, audio.
    'engine': (
        'runtime', 'ltypes', 'cmeta', 'dtmeta', 'sharedtable', 'globals_parity',
        'bytecode_cache', 'replicate', 'anim_replicate', 'rig', 'avatar',
        'game_avatar', 'audio', 'audio_dsp', 'audio_capture', 'audio_scene',
        'gui', 'gui_modal', 'mouse_lock', 'ti', 'theme',
        # Section 4: the scene probe casts rays through the physics meshes,
        # so it lives in engine.  Its GPU half, `env_probe_gpu`, is a child
        # of `render` (#[path] in render.rs) and stays there.
        'env_probe',
        # The Lua-facing entry points moved out of core's telemetry/bench/
        # the engine clock interface (no mlua below engine).
        'lua_bridge',
    ),
    # Play sessions, game menu, character control and shared game drawing.
    'player': (
        'play', 'emote_wheel', 'game', 'game_view', 'account', 'asset_report',
        'keys', 'offscreen', 'run_harness', 'player_app', 'gamepad',
    ),
    # ~73k lines: everything else - editor, panels, plugin host, toolbox,
    # terrain tools, project sync, self-test.
    'studio': (),
}

STUDIO = 'studio'
ROOT_MODULE = '<crate>'          # src/lib.rs (or src/main.rs before the split)
CRATE_ORDER = ('core', 'physics', 'render', 'engine', 'player', 'studio')

# Dependency order: core feeds physics and render, which are siblings.
# Engine uses those three, player uses engine and Studio uses player.
# The pre-existing account and net packages are outside this module graph.
ALLOWED = {
    'core': frozenset(),
    'physics': frozenset({'core'}),
    'render': frozenset({'core'}),
    'engine': frozenset({'core', 'physics', 'render'}),
    'player': frozenset({'core', 'physics', 'render', 'engine'}),
    'studio': frozenset({'core', 'physics', 'render', 'engine', 'player'}),
}

# The workspace crates whose sources are part of this graph, as
# (source tree relative to the repository root, the crate it holds, package
# name). The account and network crates are outside this graph.
CRATE_TREES = (
    ('src', None, 'rustudio'),
    ('crates/rustudio-core/src', 'core', 'rustudio_core'),
    ('crates/rustudio-physics/src', 'physics', 'rustudio_physics'),
    ('crates/rustudio-render/src', 'render', 'rustudio_render'),
    ('crates/rustudio-engine/src', 'engine', 'rustudio_engine'),
    ('crates/rustudio-player/src', 'player', 'rustudio_player'),
)

WORKSPACE_PACKAGES = tuple(package for _, _, package in CRATE_TREES)

MODULE_CRATE = {}
for _crate, _modules in CRATE_MODULES.items():
    for _module in _modules:
        MODULE_CRATE[_module] = _crate
MODULE_CRATE[ROOT_MODULE] = STUDIO

# External crates counted for the `ext` part of --json.
EXTERNAL_CRATES = (
    'egui', 'eframe', 'egui_dock', 'egui_wgpu', 'wgpu', 'mlua', 'rapier3d',
    'parry3d', 'rbx_dom_weak', 'rbx_binary', 'rbx_xml', 'rbx_reflection',
    'rbx_reflection_database', 'rbx_types', 'rodio', 'ureq', 'tungstenite',
    'rfd', 'notify', 'rustudio_net', 'rustudio_account', 'image', 'glam',
    'serde_json', 'winit', 'draco_core', 'zstd',
)


# --------------------------------- helpers ---------------------------------

_RAW_STRING = re.compile(r'(?:br|rb|r)(#*)"')
_CHAR_LITERAL = re.compile(r"'(?:\\.|[^\\'\n])'")


def mask_source(src):
    """Blank out comments and string/char literal contents, keeping offsets.

    Returns a copy of `src` of the same length and line structure in which
    everything that is not code is spaces.  Regexes and brace matching can then
    run on it without tripping over `crate::` in a doc comment or a `}` inside
    a format string, while offsets still line up with the original text.
    """
    out = list(src)
    n = len(src)
    i = 0

    def blank(a, b):
        for k in range(a, min(b, n)):
            if out[k] != '\n':
                out[k] = ' '

    while i < n:
        c = src[i]
        if c == '/' and src.startswith('//', i):
            j = src.find('\n', i)
            j = n if j < 0 else j
            blank(i, j)
            i = j
        elif c == '/' and src.startswith('/*', i):
            depth = 1
            j = i + 2
            while j < n and depth:
                if src.startswith('/*', j):
                    depth += 1
                    j += 2
                elif src.startswith('*/', j):
                    depth -= 1
                    j += 2
                else:
                    j += 1
            blank(i, j)
            i = j
        elif c == '"':
            j = i + 1
            while j < n:
                if src[j] == '\\':
                    j += 2
                elif src[j] == '"':
                    j += 1
                    break
                else:
                    j += 1
            blank(i, j)
            i = j
        elif c in 'rb' and (i == 0 or not (src[i - 1].isalnum() or src[i - 1] == '_')):
            m = _RAW_STRING.match(src, i)
            if not m:
                i += 1
                continue
            close = '"' + m.group(1)
            j = src.find(close, m.end())
            j = n if j < 0 else j + len(close)
            blank(i, j)
            i = j
        elif c == "'":
            m = _CHAR_LITERAL.match(src, i)
            if m:
                blank(i, m.end())
                i = m.end()
            else:
                i += 1          # a lifetime, not a char literal
        else:
            i += 1
    return ''.join(out)


def cfg_is_test(attrs):
    """True when a cfg is provably false with test=false.

    Other predicates are unknown, so feature- and platform-specific production
    code still counts. Parse only the boolean cfg operators and the test atom.
    """
    def predicate(tokens):
        token = next(tokens, '')
        if token in ('all', 'any', 'not') and next(tokens, '') == '(':
            values = []
            while tokens.peek() != ')':
                values.append(predicate(tokens))
                if tokens.peek() == ')':
                    break
                if next(tokens, '') != ',':
                    raise ValueError
            next(tokens)
            if token == 'not':
                if len(values) != 1:
                    raise ValueError
                return None if values[0] is None else not values[0]
            decisive = token == 'any'
            if decisive in values:
                return decisive
            return None if None in values else not decisive
        if token in ('(', ')', ',', '=', ''):
            raise ValueError
        if token == 'test':
            return False
        if tokens.peek() == '=':
            next(tokens)
            next(tokens, '')
        return None

    class Tokens:
        def __init__(self, text):
            self.tokens = collections.deque(re.findall(r'"(?:[^"\\]|\\.)*"|\w+|[^\s]', text))

        def __next__(self):
            if not self.tokens:
                raise StopIteration
            return self.tokens.popleft()

        def peek(self):
            return self.tokens[0] if self.tokens else None

    for match in _CFG_ATTR.finditer(attrs):
        # The cfg argument is enclosed by the final parenthesis of the attribute.
        text = match.group(1)
        tokens = Tokens(text)
        try:
            if predicate(tokens) is False and tokens.peek() is None:
                return True
        except ValueError:
            pass
    return False


_CFG_ATTR = re.compile(r'#\[\s*cfg\s*\((.*?)\)\s*\]', re.S)


def _skip_attrs(code, i):
    """Move past whitespace and any `#[...]` attributes."""
    n = len(code)
    while i < n:
        while i < n and code[i].isspace():
            i += 1
        if code.startswith('#[', i):
            depth = 0
            j = i + 1
            while j < n:
                if code[j] == '[':
                    depth += 1
                elif code[j] == ']':
                    depth -= 1
                    if depth == 0:
                        j += 1
                        break
                j += 1
            i = j
            continue
        return i
    return i


def _block_end(code, brace):
    """Index one past the `}` matching the `{` at `brace` (code is masked)."""
    depth = 0
    for j in range(brace, len(code)):
        if code[j] == '{':
            depth += 1
        elif code[j] == '}':
            depth -= 1
            if depth == 0:
                return j + 1
    return len(code)


def item_end(code, i):
    """End offset of the item starting at `i`: its `}` or its `;`."""
    n = len(code)
    i = _skip_attrs(code, i)
    depth = 0
    for j in range(i, n):
        c = code[j]
        if c in '([':
            depth += 1
        elif c in ')]':
            depth = max(0, depth - 1)
        elif depth == 0 and c == '{':
            return _block_end(code, j)
        elif depth == 0 and c == ';':
            return j + 1
    return n


def cfg_test_ranges(code):
    """Merged (start, end) ranges of items carrying `#[cfg(test)]`."""
    spans = []
    for m in _CFG_ATTR.finditer(code):
        if cfg_is_test(m.group()):
            spans.append((m.start(), item_end(code, m.end())))
    spans.sort()
    merged = []
    for start, end in spans:
        if merged and start <= merged[-1][1]:
            merged[-1] = (merged[-1][0], max(merged[-1][1], end))
        else:
            merged.append((start, end))
    return merged


class TestSpans:
    """Fast `is this offset inside #[cfg(test)]?` lookup for one file."""

    __slots__ = ('ranges', 'starts')

    def __init__(self, ranges):
        self.ranges = ranges
        self.starts = [s for s, _ in ranges]

    def __contains__(self, pos):
        idx = bisect.bisect_right(self.starts, pos) - 1
        return idx >= 0 and pos < self.ranges[idx][1]

    def line_count(self, code):
        total = 0
        for start, end in self.ranges:
            total += code.count('\n', start, end)
        return total


# ------------------------------- module walk -------------------------------

_MOD_DECL = re.compile(
    r'^[ \t]*((?:#\[[^\]]*\]\s*)*)'
    r'(?:pub(?:\s*\([^)]*\))?\s+)?(?:unsafe\s+)?'
    r'mod\s+([A-Za-z_][A-Za-z_0-9]*)\s*;', re.M)
_PATH_ATTR = re.compile(r'#\[path\s*=\s*"([^"]+)"\]')


_INLINE_MOD = re.compile(r'\bmod\s+([A-Za-z_]\w*)\s*\{')
_INCLUDE = re.compile(r'\binclude\s*!\s*\(')
_LITERAL_INCLUDE = re.compile(r'\s*"([^"\\]+)"\s*\)')
_ROOT_ITEM = re.compile(r'\b(fn|type|struct|enum|trait|const|static|union)\s+([A-Za-z_]\w*)')
_ROOT_USE = re.compile(r'\buse\s+([^;]+);')


def inline_modules(code):
    return [(m.end(), _block_end(code, m.end() - 1), m[1]) for m in _INLINE_MOD.finditer(code)]


def inline_path(modules, pos):
    return tuple(name for start, end, name in modules if start <= pos < end)


def root_scope(code):
    """Brace depth for recognizing root declarations rather than associated items."""
    braces = [m.start() for m in re.finditer(r'[{}]', code)]
    depths = [0]
    for pos in braces:
        depths.append(depths[-1] + (1 if code[pos] == '{' else -1))
    return lambda pos: depths[bisect.bisect_left(braces, pos)] == 0


def root_module(crate):
    return f'<{crate} root>' if crate else ROOT_MODULE



def crate_roots(src):
    """Files that are compilation roots, primary first.

    `src/lib.rs` when it exists, else `src/main.rs`; both are walked when both
    exist, so module declarations in either file count.
    """
    roots = []
    for name in ('lib.rs', 'main.rs'):
        path = os.path.join(src, name)
        if os.path.exists(path):
            roots.append(path)
    return roots


class Graph:
    def __init__(self, root):
        self.root = root
        self.trees = [(os.path.join(root, tree), crate, package)
                      for tree, crate, package in CRATE_TREES]
        self.files = {}            # abs path -> module path tuple
        self.tree_of = {}          # abs path -> (tree abs path, crate or None, package)
        self.raw = {}
        self.code = {}
        self.testonly = set()      # files inside a `#[cfg(test)] mod`
        self.testspans = {}
        self.inline = {}
        self.root_items = collections.defaultdict(dict)
        self.root_aliases = collections.defaultdict(dict)
        self.lines = collections.Counter()       # top module -> lines
        self.test_lines = collections.Counter()  # top module -> cfg(test) lines
        self.edges = collections.Counter()       # (from, to) -> refs
        self.edges_test = collections.Counter()  # ... inside #[cfg(test)]
        self.items = collections.defaultdict(collections.Counter)
        self.items_test = collections.defaultdict(collections.Counter)
        self.file_edges = collections.defaultdict(collections.Counter)
        self.ext = collections.defaultdict(collections.Counter)
        self.unresolved = []
        self.misplaced = []        # (path, expected crate) where the tree and the table disagree
        self.tops = []

    # -- discovery ---------------------------------------------------------
    def build(self):
        self.roots = []
        for tree, crate, package in self.trees:
            roots = crate_roots(tree)
            if not roots:
                raise SystemExit('crate_graph: no src/lib.rs or src/main.rs under '
                                 + tree)
            self.roots.extend(roots)
            for root in roots:
                self._walk(root, (), False, (tree, crate, package))
        self.tops = sorted({mp[0] for mp in self.files.values() if mp})
        topset = set(self.tops)
        for path, mp in self.files.items():
            if not mp:
                self._root_names(path)
        for path, mp in self.files.items():
            self._scan(path, mp, topset)
        self.orphans = sorted(set().union(*(
            self._all_sources(tree) for tree, _, _ in self.trees)) - set(self.files))
        # The tree a module lives in and the table must agree: core's modules
        # are under crates/rustudio-core and no longer under the root src/.
        moved = {crate for _, crate, _ in self.trees if crate}
        for path, mp in self.files.items():
            if not mp:
                continue            # a crate's own lib.rs/main.rs
            top = mp[0]
            here = self.crate_of(top)
            _, crate, _ = self.tree_of[path]
            if crate and here != crate:
                self.misplaced.append((path, crate))
            elif not crate and here in moved:
                self.misplaced.append((path, here))
        return self

    def _all_sources(self, src):
        found = set()
        for dirpath, _dirnames, filenames in os.walk(src):
            for name in filenames:
                if name.endswith('.rs'):
                    found.add(os.path.normpath(os.path.join(dirpath, name)))
        return found

    def _walk(self, path, mp, is_test, tree):
        path = os.path.normpath(path)
        if path in self.files:
            return
        self.files[path] = mp
        self.tree_of[path] = tree
        if is_test:
            self.testonly.add(path)
        try:
            with open(path, encoding='utf8') as fh:
                text = fh.read()
        except OSError:
            self.unresolved.append((path, mp))
            return
        code = mask_source(text)
        self.raw[path] = text
        self.code[path] = code
        modules = self.inline[path] = inline_modules(code)
        spans = TestSpans(cfg_test_ranges(code))
        directory = os.path.dirname(path)
        stem = os.path.basename(path)[:-3]
        base = directory if stem in ('mod', 'main', 'lib') else os.path.join(directory, stem)
        for m in _MOD_DECL.finditer(code):
            attrs = text[m.start(1):m.end(1)]          # raw: keeps #[path] strings
            name = m.group(2)
            inside = inline_path(modules, m.start())
            child_test = is_test or cfg_is_test(attrs) or m.start(2) in spans
            path_attr = _PATH_ATTR.search(attrs)
            if path_attr:
                # `#[path]` is relative to the directory of the current file.
                candidates = [os.path.join(directory, path_attr.group(1))]
            else:
                candidates = [os.path.join(base, *inside, name + '.rs'),
                              os.path.join(base, *inside, name, 'mod.rs')]
            for candidate in candidates:
                if os.path.exists(candidate):
                    self._walk(candidate, mp + inside + (name,), child_test, tree)
                    break
            else:
                self.unresolved.append((path, name))

        for m in _INCLUDE.finditer(code):
            literal = _LITERAL_INCLUDE.match(text, m.end())
            if literal:
                self._walk(os.path.join(directory, literal[1]),
                           mp + inline_path(modules, m.start()),
                           is_test or m.start() in spans, tree)

    def _root_names(self, path):
        code = self.code[path]
        package = self.tree_of[path][2]
        at_root = root_scope(code)
        for m in _ROOT_ITEM.finditer(code):
            if at_root(m.start()):
                self.root_items[package][m[2]] = root_module(self.tree_of[path][1])
        for m in _ROOT_USE.finditer(code):
            if not at_root(m.start()):
                continue
            for imported in use_paths(m[1]):
                names, _, alias = imported.partition(' as ')
                tokens = re.findall(r'[A-Za-z_]\w*', names)
                if not tokens:
                    continue
                if tokens[0] in ('crate', 'self'):
                    tokens.pop(0)
                target_package = package
                if tokens and tokens[0] in WORKSPACE_PACKAGES:
                    target_package = tokens.pop(0)
                if tokens and tokens[-1] == 'self':
                    tokens.pop()
                if tokens:
                    self.root_aliases[package][alias or tokens[-1]] = (target_package, tokens)

    # -- references --------------------------------------------------------
    def _scan(self, path, mp, topset):
        code = self.code[path]
        top = mp[0] if mp else root_module(self.tree_of[path][1])
        text = self.raw[path]
        self.lines[top] += text.count('\n')
        spans = TestSpans(cfg_test_ranges(code))
        self.testspans[path] = spans
        if path in self.testonly:
            self.test_lines[top] += text.count('\n')
        else:
            self.test_lines[top] += spans.line_count(code)

        for target, item, pos in self._refs(path, code, mp, topset):
            is_test = path in self.testonly or pos in spans
            key = (top, target)
            if is_test:
                self.edges_test[key] += 1
                self.items_test[key][item] += 1
            else:
                self.edges[key] += 1
                self.items[key][item] += 1
            self.file_edges[self.rel(path)][target] += 1

        for ext_crate in EXTERNAL_CRATES:
            count = len(re.findall(r'(?<![\w:])' + ext_crate + r'::', code))
            if count:
                self.ext[top][ext_crate] += count

    def _refs(self, path, code, mp, topset):
        """Qualified top-level references as (target, label, offset)."""
        _, crate, package = self.tree_of[path]
        top = mp[0] if mp else root_module(crate)
        out = []

        def add(target_package, tokens, pos):
            seen = set()
            while (target_package, tokens[0]) not in seen:
                key = target_package, tokens[0]
                alias = self.root_aliases[target_package].get(tokens[0])
                if alias is None:
                    break
                seen.add(key)
                target_package, prefix = alias
                tokens = prefix + tokens[1:]
            first = tokens[0]
            if first in self.root_items[target_package]:
                target = self.root_items[target_package][first]
                label = '::'.join(tokens)
            elif first in topset:
                target = first
                label = _label(tokens)
            else:
                return
            if target != top:
                out.append((target, label, pos))

        roots = '|'.join(('crate', *WORKSPACE_PACKAGES))
        prefix = re.compile(rf'(?<![\w])({roots})\s*::\s*|\b((?:super\s*::\s*)+)')
        for m in prefix.finditer(code):
            if m[2]:
                depth = len(mp) + len(inline_path(self.inline[path], m.start()))
                if m[2].count('::') != depth:
                    continue
            target_package = m[1] if m[1] in WORKSPACE_PACKAGES else package
            if code.startswith('{', m.end()):
                paths = _expand_group(code, m.end())
            else:
                match = re.match(r'[A-Za-z_]\w*(?:\s*::\s*[A-Za-z_]\w*)*', code[m.end():])
                paths = [re.findall(r'[A-Za-z_]\w*', match.group())] if match else []
            for tokens in paths:
                add(target_package, tokens, m.start())
        return out

    # -- classification ----------------------------------------------------
    def crate_of(self, module):
        for crate in CRATE_ORDER:
            if module == root_module(crate):
                return crate
        return MODULE_CRATE.get(module, STUDIO)

    def wrong_way(self, include_tests=False):
        """{(src_crate, dst_crate): [(refs, module, module, items), ...]}.

        A reference is wrong-way when the destination crate is not in
        ALLOWED[source crate]; sibling crates (physics <-> render) count.
        """
        grouped = collections.defaultdict(list)
        for (src, dst), count in self._edge_pairs(include_tests).items():
            src_crate, dst_crate = self.crate_of(src), self.crate_of(dst)
            if src_crate == dst_crate:
                continue
            if dst_crate in ALLOWED[src_crate]:
                continue
            items = collections.Counter()
            for table in (self.items, self.items_test):
                if include_tests or table is self.items:
                    items.update(table.get((src, dst), {}))
            grouped[(src_crate, dst_crate)].append((count, src, dst, items))
        for rows in grouped.values():
            rows.sort(key=lambda row: (-row[0], row[1]))
        return grouped

    def test_only_wrong_way(self):
        """Wrong-way references made inside `#[cfg(test)]` code."""
        grouped = collections.defaultdict(list)
        for (src, dst), count in self.edges_test.items():
            src_crate, dst_crate = self.crate_of(src), self.crate_of(dst)
            if src_crate == dst_crate or dst_crate in ALLOWED[src_crate]:
                continue
            grouped[(src_crate, dst_crate)].append(
                (count, src, dst, self.items_test.get((src, dst), {})))
        for rows in grouped.values():
            rows.sort(key=lambda row: (-row[0], row[1]))
        return grouped

    def _edge_pairs(self, include_tests):
        pairs = collections.Counter(self.edges)
        if include_tests:
            pairs.update(self.edges_test)
        return pairs

    def lines_per_crate(self):
        totals = collections.Counter()
        test = collections.Counter()
        files = collections.Counter()
        for top in list(self.lines) + ([ROOT_MODULE] if ROOT_MODULE not in self.lines else []):
            crate = self.crate_of(top)
            totals[crate] += self.lines.get(top, 0)
            test[crate] += self.test_lines.get(top, 0)
        for path, mp in self.files.items():
            files[self.crate_of(mp[0] if mp else root_module(self.tree_of[path][1]))] += 1
        return totals, test, files

    def rel(self, path):
        # (relative to the repository root: files now live under src/ and
        # under crates/*/src/)
        return os.path.relpath(path, self.root)


_LOWER_IDENT = re.compile(r'[a-z_][a-z_0-9]*\Z')


def _label(tokens):
    """Name one reference: the item, prefixed by the submodules it goes through.

    `crate::runtime::domains::world::part::part_mass` -> `domains::world::part::part_mass`;
    `crate::solver::SolverKind::Rapier` -> `SolverKind` (the enum, not the
    variant); `crate::mesh` -> `(mod)`.
    """
    rest = tokens[1:]
    if not rest:
        return '(mod)'
    for i, token in enumerate(rest):
        if not _LOWER_IDENT.match(token):
            return '::'.join(rest[:i + 1])
    return '::'.join(rest)


def split_group(body):
    segments = []
    depth = 0
    current = ''
    for ch in body:
        if ch in '{[(':
            depth += 1
        elif ch in '}])':
            depth -= 1
        if ch == ',' and depth == 0:
            segments.append(current)
            current = ''
        else:
            current += ch
    segments.append(current)
    return segments


def use_paths(text):
    """Flatten explicit imports, including nested groups and renamed leaves."""
    head, brace, group = text.partition('{')
    if not brace:
        yield re.sub(r'\s+as\s+', ' as ', head.strip())
        return
    end = _block_end(text, len(head))
    for leaf in split_group(text[len(head) + 1:end - 1]):
        for path in use_paths(leaf):
            yield head + path


def _expand_group(code, brace):
    """`crate::{a, b::C}` -> [['a'], ['b', 'C']]."""
    end = _block_end(code, brace)
    segments = split_group(code[brace + 1:max(brace + 1, end - 1)])
    result = []
    for segment in segments:
        # `x::y::{A, B}` imports x::y as a whole: keep the module path only.
        cut = segment.split('{', 1)[0]
        tokens = re.findall(r'[A-Za-z_0-9]+', cut)
        if tokens:
            result.append(tokens)
    return result


# -------------------------------- reporting --------------------------------

def _count(n, word):
    return '%d %s' % (n, word if n == 1 else word + 's')


def _pair_key(pair):
    return (CRATE_ORDER.index(pair[0]), CRATE_ORDER.index(pair[1]))


def _item_summary(items, limit=12):
    parts = []
    for name, count in sorted(items.items(), key=lambda kv: (-kv[1], kv[0]))[:limit]:
        parts.append('%s×%d' % (name or '(mod)', count))
    rest = len(items) - limit
    if rest > 0:
        parts.append('+%d more' % rest)
    return ', '.join(parts)


def print_groups(groups, out=sys.stdout):
    total = 0
    for pair in sorted(groups, key=_pair_key):
        rows = groups[pair]
        refs = sum(row[0] for row in rows)
        total += refs
        print('\n## %s -> %s: %s, %s'
              % (pair[0], pair[1], _count(refs, 'ref'),
                 _count(len(rows), 'module pair')), file=out)
        for count, src, dst, items in rows:
            print('  %s -> %s (%d): %s' % (src, dst, count, _item_summary(items)),
                  file=out)
    return total


def report(graph, out=sys.stdout):
    print('crate root: %s' % ', '.join(
        os.path.relpath(r, graph.root) for r in graph.roots), file=out)
    print('%s, %s lines, %s, %s'
          % (_count(len(graph.files), 'file'), '{:,}'.format(sum(graph.lines.values())),
             _count(len(graph.tops), 'top-level module'),
             _count(len(graph.orphans), 'orphan file')), file=out)

    totals, test, files = graph.lines_per_crate()
    print('\n%-10s %10s %8s %7s' % ('crate', 'lines', 'test', 'files'), file=out)
    print('-' * 38, file=out)
    for crate in CRATE_ORDER:
        print('%-10s %10s %8s %7d'
              % (crate, '{:,}'.format(totals[crate]), '{:,}'.format(test[crate]),
                 files[crate]), file=out)
    print('-' * 38, file=out)
    print('%-10s %10s %8s %7d'
          % ('total', '{:,}'.format(sum(totals.values())),
             '{:,}'.format(sum(test.values())), sum(files.values())), file=out)

    wrong = graph.wrong_way()
    test_wrong = graph.test_only_wrong_way()
    total = sum(sum(row[0] for row in rows) for rows in wrong.values())
    test_total = sum(sum(row[0] for row in rows) for rows in test_wrong.values())
    print('\nwrong-way references: %d (%d more inside #[cfg(test)])'
          % (total, test_total), file=out)
    print_groups(wrong, out)
    if test_wrong:
        print('\n# #[cfg(test)] references, counted separately; these fail '
              '--check only with --tests:', file=out)
        print_groups(test_wrong, out)
    if graph.orphans:
        print('\nwarning: %d .rs files are not reachable from the crate root:'
              % len(graph.orphans), file=out)
        for path in graph.orphans[:10]:
            print('  ' + os.path.relpath(path, graph.root), file=out)
    if graph.misplaced:
        print('\nwarning: %d files are in a tree that does not match the table:'
              % len(graph.misplaced), file=out)
        for path, crate in graph.misplaced[:10]:
            print('  %s (belongs to %s)' % (graph.rel(path), crate), file=out)
    print('\n(default run reports only; use --check to fail on these)')
    return 0


def report_check(graph, crates, include_tests, out=sys.stdout):
    wrong = graph.wrong_way(include_tests=include_tests)
    everything = collections.Counter()
    for pair, rows in wrong.items():
        everything[pair] = sum(row[0] for row in rows)
    selected = {pair: rows for pair, rows in wrong.items() if pair[0] in crates}
    total = sum(everything.values())
    checked = sum(everything[pair] for pair in selected)
    scope = ', '.join(c for c in CRATE_ORDER if c in crates)
    suffix = ' (including #[cfg(test)])' if include_tests else ''
    print('crate_graph --check %s%s: %d wrong-way references'
          % (scope, suffix, checked), file=out)
    if total != checked:
        print('(the whole tree has %d; %d start in crates not being checked)'
              % (total, total - checked), file=out)
    if not selected:
        print('PASS: no wrong-way reference starts in %s' % scope, file=out)
        return 0
    print_groups(selected, out)
    print('\nFAIL: %d wrong-way references start in %s'
          % (checked, scope), file=out)
    return 1


def report_cycles(graph, out=sys.stdout):
    edges = {top: set() for top in graph.tops + [ROOT_MODULE]}
    for (src, dst) in graph._edge_pairs(include_tests=True):
        edges.setdefault(src, set()).add(dst)
        edges.setdefault(dst, set())
    lines = dict(graph.lines)
    lines.setdefault(ROOT_MODULE, 0)

    index = {}
    low = {}
    stack = []
    on_stack = set()
    components = []
    counter = [0]
    sys.setrecursionlimit(100000)

    def strongconnect(v):
        index[v] = low[v] = counter[0]
        counter[0] += 1
        stack.append(v)
        on_stack.add(v)
        for w in edges[v]:
            if w not in index:
                strongconnect(w)
                low[v] = min(low[v], low[w])
            elif w in on_stack:
                low[v] = min(low[v], index[w])
        if low[v] == index[v]:
            component = []
            while True:
                w = stack.pop()
                on_stack.discard(w)
                component.append(w)
                if w == v:
                    break
            components.append(component)

    for node in sorted(edges):
        if node not in index:
            strongconnect(node)

    big = [c for c in components if len(c) > 1]
    print('import cycles: %d with more than one module '
          '(all references, including #[cfg(test)])' % len(big), file=out)
    for component in sorted(big, key=len, reverse=True):
        size = sum(lines.get(m, 0) for m in component)
        print('\n## %d modules, %s lines' % (len(component), '{:,}'.format(size)),
              file=out)
        print('   ' + ' '.join(sorted(component, key=lambda m: -lines.get(m, 0))),
              file=out)

    biggest = set(max(big, key=len)) if big else set()
    memo = {}

    def reaches_cycle(v):
        if v in biggest:
            return True
        if v in memo:
            return memo[v]
        memo[v] = False
        result = any(reaches_cycle(w) for w in edges.get(v, ()))
        memo[v] = result
        return result

    singles = [c[0] for c in components if len(c) == 1]
    below = sorted((m for m in singles if not reaches_cycle(m)),
                   key=lambda m: -lines.get(m, 0))
    above = sorted((m for m in singles if reaches_cycle(m)),
                   key=lambda m: -lines.get(m, 0))
    print('\nbelow the cycle: %d modules, %s lines'
          % (len(below), '{:,}'.format(sum(lines.get(m, 0) for m in below))),
          file=out)
    print('   ' + ' '.join('%s(%s)' % (m, lines.get(m, 0)) for m in below),
          file=out)
    print('above the cycle: %d modules, %s lines'
          % (len(above), '{:,}'.format(sum(lines.get(m, 0) for m in above))),
          file=out)
    print('   ' + ' '.join('%s(%s)' % (m, lines.get(m, 0)) for m in above),
          file=out)
    return 0


_IMPL = re.compile(
    r'^[ \t]*(?:pub(?:\s*\([^)]*\))?\s+)?(?:unsafe\s+)?(?:default\s+)?'
    r'impl(?:\s*<[^>]*>)?\s+(?:[A-Za-z_0-9]+\s*::\s*)*'
    r'([A-Z][A-Za-z_0-9]*)(?:\s*<[^>{]*>)?\s*(?:where[^{]*)?\{', re.M)
_DEF = re.compile(
    r'^[ \t]*(?:pub(?:\s*\([^)]*\))?\s+)?(?:struct|enum|union)\s+([A-Z][A-Za-z_0-9]*)',
    re.M)


def editor_impls(graph, out=sys.stdout):
    """`impl Editor` blocks outside `app`.

    Also lists every other type that has inherent `impl` blocks in a top-level
    module other than the one that defines it, which is the same rule the
    compiler will enforce once `Editor` lives in a different crate.
    """
    defs = collections.defaultdict(set)
    for path, mp in graph.files.items():
        top = mp[0] if mp else ROOT_MODULE
        for m in _DEF.finditer(graph.code[path]):
            defs[m.group(1)].add(top)

    blocks = collections.defaultdict(list)    # (type, def, where) -> [(file, lines, start)]
    for path, mp in graph.files.items():
        code = graph.code[path]
        top = mp[0] if mp else ROOT_MODULE
        for m in _IMPL.finditer(code):
            name = m.group(1)
            where_defined = defs.get(name)
            if not where_defined or len(where_defined) != 1:
                continue
            if top in where_defined:
                continue
            brace = m.end() - 1
            end = _block_end(code, brace)
            block_lines = code.count('\n', m.start(), end) + 1
            blocks[(name, tuple(sorted(where_defined)), top)].append(
                (graph.rel(path), block_lines))

    editor = {k: v for k, v in blocks.items() if k[0] == 'Editor'}
    total_blocks = sum(len(v) for v in editor.values())
    total_lines = sum(n for v in editor.values() for _, n in v)
    print('impl Editor outside app: %s, %s lines'
          % (_count(total_blocks, 'block'), '{:,}'.format(total_lines)), file=out)
    for (name, defined, where), rows in sorted(editor.items(),
                                               key=lambda kv: (-sum(n for _, n in kv[1]), kv[0][2])):
        print('  %s (defined in %s): %s, %s lines'
              % (where, '/'.join(defined), _count(len(rows), 'block'),
                 '{:,}'.format(sum(n for _, n in rows))), file=out)
        for rel, n in sorted(rows):
            print('    %s: %s' % (rel, _count(n, 'line')), file=out)

    others = {k: v for k, v in blocks.items() if k[0] != 'Editor'}
    if others:
        print('\nother inherent impls outside their defining module '
              '(the compiler will reject these across a crate edge):', file=out)
        for (name, defined, where), rows in sorted(others.items()):
            print('  impl %s (def %s) in %s ×%d'
                  % (name, '/'.join(defined), where, len(rows)), file=out)
    return 0


def dump_json(graph, path):
    payload = {
        'root': os.path.abspath(graph.root),
        'crate_roots': [os.path.relpath(r, graph.root) for r in graph.roots],
        'tops': graph.tops,
        'lines': dict(graph.lines),
        'testlines': dict(graph.test_lines),
        'edges': {a: dict(b) for a, b in
                  _by_source(graph.edges).items()},
        'edges_test': {a: dict(b) for a, b in
                       _by_source(graph.edges_test).items()},
        'items': {'%s>%s' % key: dict(value) for key, value in graph.items.items()},
        'items_test': {'%s>%s' % key: dict(value)
                       for key, value in graph.items_test.items()},
        'crate_of': {module: graph.crate_of(module)
                     for module in graph.tops + [ROOT_MODULE]},
        'allowed': {crate: sorted(targets) for crate, targets in ALLOWED.items()},
        'ext': {top: dict(value) for top, value in graph.ext.items()},
        'orphans': [graph.rel(p) for p in graph.orphans],
        'files': {graph.rel(p): list(mp) for p, mp in graph.files.items()},
        'fileedges': {name: dict(value) for name, value in graph.file_edges.items()},
        'testonly': [graph.rel(p) for p in sorted(graph.testonly)],
    }
    with open(path, 'w', encoding='utf8') as fh:
        json.dump(payload, fh, indent=1, sort_keys=False)
        fh.write('\n')
    print('wrote %s' % path)
    return 0


def _by_source(pairs):
    out = collections.defaultdict(collections.Counter)
    for (src, dst), count in pairs.items():
        out[src][dst] += count
    return out


# ----------------------------------- main -----------------------------------

def find_root(start):
    path = os.path.abspath(start)
    for _ in range(20):
        src = os.path.join(path, 'src')
        if (os.path.exists(os.path.join(src, 'lib.rs'))
                or os.path.exists(os.path.join(src, 'main.rs'))):
            return path
        parent = os.path.dirname(path)
        if parent == path:
            break
        path = parent
    return None


def self_test():
    sources = {
        'spaced': ('fn f() { crate :: app :: Editor; }', True, True),
        'inline': ('mod child { fn f() { super::super::app::Editor; } }', True, True),
        'include': ('include!("fixture.inc.rs");', True, True),
        'root function': ('fn f() { crate::studio_function(); }', True, True),
        'root alias': ('fn f() { crate::EditorAlias; }', True, True),
        'root alias chain': ('fn f() { crate::Aliases::Editor; }', True, True),
        'negated cfg': ('#[cfg(not(test))] fn f() { crate::app::Editor; }', True, True),
        'mixed cfg': ('#[cfg(any(unix, test))] fn f() { crate::app::Editor; }', True, True),
        'test cfg': ('#[cfg(test)] fn f() { crate::app::Editor; }', False, True),
        'nested cfg': ('#[cfg(all(unix, any(test, test)))] fn f() { crate::app::Editor; }', False, True),
        'cfg name is a string': ('#[cfg(feature = "test")] fn f() { crate::app::Editor; }', True, True),
        'comment/string': ('// crate::app::Editor\nconst X: &str = "crate::app::Editor";', False, False),
    }
    with tempfile.TemporaryDirectory() as temp:
        root = Path(temp)
        for tree, _, _ in CRATE_TREES:
            src = root / tree
            src.mkdir(parents=True)
            (src / 'lib.rs').write_text('')
        studio = root / 'src'
        (studio / 'lib.rs').write_text("""
mod app;
mod doc;
fn studio_function() {}
use crate::{app::{Editor as EditorAlias}};
use crate::app as StudioApp;
use crate::StudioApp as Aliases;
""")
        (studio / 'app.rs').write_text('pub struct Editor;')
        (studio / 'fixture.inc.rs').write_text('fn f() { crate::app::Editor; }')
        for name, (source, prod, tests) in sources.items():
            (studio / 'doc.rs').write_text(source)
            graph = Graph(temp).build()
            assert bool(graph.wrong_way().get(('core', 'studio'))) == prod, name
            assert bool(graph.wrong_way(True).get(('core', 'studio'))) == tests, name
        (studio / 'doc.rs').write_text('')
        core = root / 'crates/rustudio-core/src/lib.rs'
        core.write_text('fn boundary() { rustudio::studio_function(); }')
        assert Graph(temp).build().wrong_way().get(('core', 'studio')), 'root caller'
    print(f'{len(sources) + 1} scanner fixtures passed')
    return 0


def main(argv=None):
    parser = argparse.ArgumentParser(
        description='rustudio module import graph and wrong-way reference check.')
    parser.add_argument('--root', metavar='DIR',
                        help='repository root (default: found from the cwd)')
    parser.add_argument('--check', nargs='?', metavar='CRATES', const='',
                        help='exit 1 on a wrong-way reference; optionally only '
                             'for references starting in CRATES '
                             '(comma separated: core,physics,render)')
    parser.add_argument('--tests', action='store_true',
                        help='with --check, count #[cfg(test)] references too')
    parser.add_argument('--cycles', action='store_true',
                        help='print the import cycles (strongly connected '
                             'components) of the module graph')
    parser.add_argument('--editor-impls', action='store_true',
                        help='print impl Editor blocks outside app, and other '
                             'inherent impls outside their defining module')
    parser.add_argument('--json', metavar='FILE',
                        help='write the raw graph as JSON')
    parser.add_argument('--self-test', action='store_true', help='run scanner fixtures')
    args = parser.parse_args(argv)
    if args.self_test:
        return self_test()

    root = args.root or find_root(os.getcwd())
    if not root:
        parser.error('no src/lib.rs or src/main.rs found; run from the repo root '
                     'or pass --root')

    graph = Graph(root).build()
    if graph.unresolved:
        for path, name in graph.unresolved[:10]:
            print('warning: unresolved module %r in %s'
                  % (name, os.path.relpath(path, root)), file=sys.stderr)

    selected = None
    if args.check is not None:
        if args.check.strip():
            selected = [c.strip() for c in args.check.split(',') if c.strip()]
            unknown = [c for c in selected if c not in CRATE_ORDER]
            if unknown:
                parser.error('unknown crate(s): %s (known: %s)'
                             % (', '.join(unknown), ', '.join(CRATE_ORDER)))
        else:
            selected = list(CRATE_ORDER)

    modes = args.check is not None or args.cycles or args.editor_impls or args.json
    status = 0
    if not modes:
        status = report(graph)
    if args.check is not None:
        status |= report_check(graph, selected, args.tests)
    if args.cycles:
        report_cycles(graph)
    if args.editor_impls:
        editor_impls(graph)
    if args.json:
        dump_json(graph, args.json)
    return status


if __name__ == '__main__':
    sys.exit(main())
