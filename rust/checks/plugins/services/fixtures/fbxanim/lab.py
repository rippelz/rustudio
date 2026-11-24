#!/usr/bin/env python3
"""Isolating FBX files for Plugin:ImportFbxAnimation (rustudio's own files): every node is a
top-level LimbNode (a Skeleton attribute, DefaultAttributeIndex 0) that tests one thing, so one
Studio import measures many rules at once. variants.py writes them with the other variants.

Layer files lab_<cfg>.fbx: the same nodes, a second AnimationLayer "Layer2" whose properties vary
per file (LAYER_CFGS). Curves run 1 -> 2 s (31 frames at 30 fps).
  rz    base R.x flat 0;        L2 R.z 0 -> 40
  rxx   base R.x 0 -> 30;       L2 R.x 0 -> 40
  rxz   base R.x 0 -> 60;       L2 R.z 0 -> 40
  ryz   base R.y 0 -> 60;       L2 R.z 0 -> 40
  rzx   base R.z 0 -> 60;       L2 R.x 0 -> 40
  rxy   base R.x 0 -> 60;       L2 R.y 0 -> 40
  rzy   base R.z 0 -> 60;       L2 R.y 0 -> 40
  rful  base R.x 0 -> 30, R.y 0 -> 60, d|Z 10;  L2 R.z 0 -> 40  (layers.fbx's UpperTorso)
  rcst  base R flat (30, 60, 10);               L2 R.z 0 -> 40
  rdef  base R.x 0 -> 30;       L2 R curve node d|X/Y/Z (5, 7, 0), R.z 0 -> 40
  rl2   no base curves;         L2 R.z 0 -> 40
  rord  RotationActive, RotationOrder 5 (ZYX); base R.x 0 -> 30, R.y 0 -> 60; L2 R.z 0 -> 40
  txx   base T.x 0 -> 10;       L2 T.x 0 -> 5
  txy   base T.x 0 -> 10;       L2 T.y 0 -> 5
  tdef  base T.x 0 -> 10;       L2 T curve node (1, 2, 3), T.x 0 -> 5
  sp    base S.x 1 -> 2, S.y flat 1; L2 S.x 1 -> 3, S.y flat 2   (scales child sc)
  sc    child of sp at (10, 10, 10), flat T.x 10
Layer 3 (lab_three.fbx only): "Layer3" R.z 0 -> 20, R.x 0 -> 10 on rz / rxz, T.x 0 -> 2 on txx.

pivots.fbx: chains P -> child (child at (10, 0, 0), a flat T.x curve) with P's R.z 0 -> 90 or
S.x 1 -> 2, plus one property each: RotationPivot, RotationOffset, ScalingPivot, ScalingOffset
(with and without RotationActive), and all four with a PreRotation.
fps_<n>.fbx: the base wave scene with TimeMode / CustomFrameRate set.
dai_*.fbx: skeleton nodes without DefaultAttributeIndex / with -1 / mixed.
"""
import copy
import os
import sys

HERE = os.path.dirname(os.path.abspath(__file__))
sys.path.insert(0, HERE)
import make_fixtures as mf  # noqa: E402
from make_fixtures import N, S, L, I, D, P, p70, cname, Curve  # noqa: E402
import variants as v  # noqa: E402

LIN = lambda a, b: Curve([(1.0, a), (2.0, b)])  # noqa: E731

# name: (base channels, layer-2 channels {(prop, defaults): {axis: Curve}}, static T, extra props)
LAB = {
    "rz": ({"R": {"x": LIN(0, 0)}}, {("R", (0, 0, 0)): {"z": LIN(0, 40)}}),
    "rxx": ({"R": {"x": LIN(0, 30)}}, {("R", (0, 0, 0)): {"x": LIN(0, 40)}}),
    "rxz": ({"R": {"x": LIN(0, 60)}}, {("R", (0, 0, 0)): {"z": LIN(0, 40)}}),
    "ryz": ({"R": {"y": LIN(0, 60)}}, {("R", (0, 0, 0)): {"z": LIN(0, 40)}}),
    "rzx": ({"R": {"z": LIN(0, 60)}}, {("R", (0, 0, 0)): {"x": LIN(0, 40)}}),
    "rxy": ({"R": {"x": LIN(0, 60)}}, {("R", (0, 0, 0)): {"y": LIN(0, 40)}}),
    "rzy": ({"R": {"z": LIN(0, 60)}}, {("R", (0, 0, 0)): {"y": LIN(0, 40)}}),
    "rful": ({"R": {"x": LIN(0, 30), "y": LIN(0, 60)}}, {("R", (0, 0, 0)): {"z": LIN(0, 40)}}, (0, 0, 0), (0, 0, 10)),
    "rcst": ({"R": {"x": LIN(30, 30), "y": LIN(60, 60), "z": LIN(10, 10)}}, {("R", (0, 0, 0)): {"z": LIN(0, 40)}}),
    "rdef": ({"R": {"x": LIN(0, 30)}}, {("R", (5, 7, 0)): {"z": LIN(0, 40)}}),
    "rl2": ({}, {("R", (0, 0, 0)): {"z": LIN(0, 40)}}),
    "rord": ({"R": {"x": LIN(0, 30), "y": LIN(0, 60)}}, {("R", (0, 0, 0)): {"z": LIN(0, 40)}}, (0, 0, 0), (0, 0, 0), 5),
    "txx": ({"T": {"x": LIN(0, 10)}}, {("T", (0, 0, 0)): {"x": LIN(0, 5)}}),
    "txy": ({"T": {"x": LIN(0, 10)}}, {("T", (0, 0, 0)): {"y": LIN(0, 5)}}),
    "tdef": ({"T": {"x": LIN(0, 10)}}, {("T", (1, 2, 3)): {"x": LIN(0, 5)}}),
    "sp": ({}, {("S", (1, 1, 1)): {"x": LIN(1, 3), "y": LIN(2, 2)}}),
    "sc": ({"T": {"x": LIN(10, 10)}}, {}),
}
LAYER3 = {"rz": {("R", (0, 0, 0)): {"z": LIN(0, 20), "x": LIN(0, 10)}},
          "rxz": {("R", (0, 0, 0)): {"z": LIN(0, 20), "x": LIN(0, 10)}},
          "txx": {("T", (0, 0, 0)): {"x": LIN(0, 2)}}}
PROPS = {"T": "Lcl Translation", "R": "Lcl Rotation", "S": "Lcl Scaling"}

# file suffix: (layer-2 properties, base layer properties, three layers)
LAYER_CFGS = {
    "def": ([], [], False),
    "w50": ([("Weight", "Number", "", "A", D(50))], [], False),
    "w0": ([("Weight", "Number", "", "A", D(0))], [], False),
    "over": ([("BlendMode", "enum", "", "", I(1))], [], False),
    "overw50": ([("BlendMode", "enum", "", "", I(1)), ("Weight", "Number", "", "A", D(50))], [], False),
    "pass": ([("BlendMode", "enum", "", "", I(2)), ("Weight", "Number", "", "A", D(50))], [], False),
    "chan": ([("RotationAccumulationMode", "enum", "", "", I(1))], [], False),
    "sadd": ([("ScaleAccumulationMode", "enum", "", "", I(1))], [], False),
    "mute": ([("Mute", "bool", "", "", I(1))], [], False),
    "solo": ([("Solo", "bool", "", "", I(1))], [], False),
    "bw50": ([], [("Weight", "Number", "", "A", D(50))], False),
    "bmute": ([], [("Mute", "bool", "", "", I(1))], False),
    "three": ([], [], True),
}


def lab_nodes():
    nodes = {}
    for k, (name, spec) in enumerate(LAB.items()):
        base = spec[0]
        t = spec[2] if len(spec) > 2 else (0, 0, 0)
        r = spec[3] if len(spec) > 3 else (0, 0, 0)
        order = spec[4] if len(spec) > 4 else 0
        parent = 0
        if name == "sc":
            parent, t = 100 + list(LAB).index("sp"), (10, 10, 10)
        nodes[name] = (100 + k, parent, "LimbNode", t, r, (0, 0, 0), order, base)
    return nodes


def set_props(node, props):
    if props:
        node.kids = [p70(*[P(*p) for p in props])]


def lab(cfg):
    l2props, baseprops, three = LAYER_CFGS[cfg]
    nodes = lab_nodes()
    s = v.with_nodes(nodes, BIND={})
    objs, conns = v.top(s, "Objects").kids, v.top(s, "Connections").kids
    for o in objs:
        if o.name == "AnimationLayer":
            set_props(o, baseprops)
        if o.name == "Model" and bytes(o.props[1]).startswith(b"rord\x00"):
            o.kids[1].kids.append(P("RotationActive", "bool", "", "", I(1)))
    lay = [(502, "Layer2", l2props, {n: s[1] for n, s in LAB.items()})]
    if three:
        lay.append((503, "Layer3", [], LAYER3))
    nid = 3000
    for lid, lname, props, chans in lay:
        ln = N("AnimationLayer", [L(lid), cname(lname, "AnimLayer"), S(b"")])
        set_props(ln, props)
        objs.append(ln)
        conns.append(N("C", [S(b"OO"), L(lid), L(500)]))
        for name, ch in chans.items():
            for (prop, defaults), axes in ch.items():
                v.add_channel(s, lid, nodes[name][0], PROPS[prop], defaults, axes, nid)
                nid += 10
    return s


def pivots():
    """Chains: top P (one property set) -> child C at (10, 0, 0) (flat T.x 10)."""
    chains = {
        # name: (props, base channels)
        "rp": ([("RotationPivot", "Vector3D", "Vector", "", D(5), D(0), D(0))], "R"),
        "rpA": ([("RotationPivot", "Vector3D", "Vector", "", D(5), D(0), D(0)),
                 ("RotationActive", "bool", "", "", I(1))], "R"),
        "ro": ([("RotationOffset", "Vector3D", "Vector", "", D(0), D(3), D(0))], "R"),
        "roA": ([("RotationOffset", "Vector3D", "Vector", "", D(0), D(3), D(0)),
                 ("RotationActive", "bool", "", "", I(1))], "R"),
        "sp": ([("ScalingPivot", "Vector3D", "Vector", "", D(2), D(0), D(0))], "S"),
        "spA": ([("ScalingPivot", "Vector3D", "Vector", "", D(2), D(0), D(0)),
                 ("RotationActive", "bool", "", "", I(1))], "S"),
        "so": ([("ScalingOffset", "Vector3D", "Vector", "", D(0), D(0), D(4))], "S"),
        "soA": ([("ScalingOffset", "Vector3D", "Vector", "", D(0), D(0), D(4)),
                 ("RotationActive", "bool", "", "", I(1))], "S"),
        "all": ([("RotationPivot", "Vector3D", "Vector", "", D(5), D(1), D(0)),
                 ("RotationOffset", "Vector3D", "Vector", "", D(0), D(3), D(0)),
                 ("ScalingPivot", "Vector3D", "Vector", "", D(2), D(0), D(1)),
                 ("ScalingOffset", "Vector3D", "Vector", "", D(0), D(0), D(4)),
                 ("RotationActive", "bool", "", "", I(1)),
                 ("PreRotation", "Vector3D", "Vector", "", D(0), D(20), D(0))], "RS"),
        "rpT": ([("RotationPivot", "Vector3D", "Vector", "", D(5), D(0), D(0))], "RT"),
    }
    nodes = {}
    i = 100
    for name, (props, ch) in chains.items():
        base = {}
        if "R" in ch:
            base["R"] = {"z": LIN(0, 90)}
        if "T" in ch:
            base["T"] = {"y": LIN(0, 10)}
        nodes["P" + name] = (i, 0, "LimbNode", (0, 0, 0), (0, 0, 0), (0, 0, 0), 0, base)
        nodes["C" + name] = (i + 1, i, "LimbNode", (10, 0, 0), (0, 0, 0), (0, 0, 0), 0, {"T": {"x": LIN(10, 10)}})
        i += 2
    s = v.with_nodes(nodes, BIND={})
    objs = v.top(s, "Objects").kids
    for o in objs:
        if o.name == "Model":
            n = bytes(o.props[1]).split(b"\x00")[0].decode()
            if n.startswith("P"):
                o.kids[1].kids.extend(P(*p) for p in chains[n[1:]][0])
    nid = 4000
    for name, (props, ch) in chains.items():
        if "S" in ch:
            v.add_channel(s, 501, nodes["P" + name][0], "Lcl Scaling", (1, 1, 1), {"x": LIN(1, 2)}, nid)
            nid += 10
    return s


def fps(mode, custom=None):
    s = v.with_nodes(v.BASE_NODES)
    v.set_gs(s, "TimeMode", I(mode))
    if custom is not None:
        gs = v.top(s, "GlobalSettings").kids[1]
        gs.kids.append(P("CustomFrameRate", "double", "Number", "", D(custom)))
    return s


def dai(mode):
    """The base scene's skeleton with DefaultAttributeIndex left out ("none"), set to -1 ("neg"),
    or left out on UpperTorso only ("mixed")."""
    s = v.with_nodes(v.BASE_NODES)
    for o in v.top(s, "Objects").kids:
        if o.name != "Model":
            continue
        n = bytes(o.props[1]).split(b"\x00")[0].decode()
        props = o.kids[1].kids
        for p in props:
            if p.props[0] == b"DefaultAttributeIndex":
                if mode == "none" or (mode == "mixed" and n == "UpperTorso"):
                    props.remove(p)
                elif mode == "neg":
                    p.props[4] = I(-1)
                break
    return s


GB = {"x": LIN(0, 30), "y": LIN(0, -50), "z": LIN(0, 20)}
GL = {"x": LIN(0, 15), "y": LIN(0, 25), "z": LIN(0, -35)}
R0 = ("R", (0, 0, 0))
# Second set (lab2_<cfg>.fbx). name: (base channels, layer-2 channels, static T, static R, order,
# RotationActive, PreRotation, static S, parent)
LAB2 = {
    "g0": ({"R": GB}, {R0: GL}),
    **{f"gA{k}": ({"R": GB}, {R0: GL}, (0, 0, 0), (0, 0, 0), k, True) for k in range(1, 6)},
    "gI5": ({"R": GB}, {R0: GL}, (0, 0, 0), (0, 0, 0), 5, False),
    "gI3": ({"R": GB}, {R0: GL}, (0, 0, 0), (0, 0, 0), 3, False),
    "gut": ({"R": {"x": LIN(0, 30), "y": LIN(0, 60)}}, {R0: {"z": LIN(0, 40)}}, (0, 0, 0), (0, 0, 10), 5, False),
    "gpre": ({"R": GB}, {R0: GL}, (0, 0, 0), (0, 0, 0), 0, True, (10, 0, 0)),
    "trot": ({"R": {"z": LIN(0, 90)}, "T": {"x": LIN(0, 10)}}, {("T", (0, 0, 0)): {"y": LIN(0, 5)}}),
    "tstat": ({"R": {"x": LIN(0, 0)}}, {("T", (0, 0, 0)): {"x": LIN(0, 5)}}, (3, 4, 5)),
    "rstat": ({"T": {"x": LIN(0, 0)}}, {R0: {"x": LIN(0, 40)}}, (0, 0, 0), (0, 0, 30)),
    "sb": ({"S": {"x": LIN(1, 2)}}, {("S", (1, 1, 1)): {"x": LIN(1, 3)}}),
    "sbc": ({"T": {"x": LIN(10, 10)}}, {}, (10, 10, 10), (0, 0, 0), 0, False, (0, 0, 0), (1, 1, 1), "sb"),
    "sst": ({"T": {"x": LIN(0, 0)}}, {("S", (1, 1, 1)): {"x": LIN(1, 3)}}, (0, 0, 0), (0, 0, 0), 0, False,
            (0, 0, 0), (2, 2, 2)),
    "sstc": ({"T": {"x": LIN(10, 10)}}, {}, (10, 10, 10), (0, 0, 0), 0, False, (0, 0, 0), (1, 1, 1), "sst"),
    "sy": ({"S": {"x": LIN(1, 2)}}, {("S", (1, 1, 1)): {"y": LIN(2, 2)}}),
    "syc": ({"T": {"x": LIN(10, 10)}}, {}, (10, 10, 10), (0, 0, 0), 0, False, (0, 0, 0), (1, 1, 1), "sy"),
}
LAYER2_CFGS = {
    "def": ([], [], False),
    "w25": ([("Weight", "Number", "", "A", D(25))], [], False),
    "sadd": ([("ScaleAccumulationMode", "enum", "", "", I(1))], [], False),
    "over25": ([("BlendMode", "enum", "", "", I(1)), ("Weight", "Number", "", "A", D(25))], [], False),
    "pass25": ([("BlendMode", "enum", "", "", I(2)), ("Weight", "Number", "", "A", D(25))], [], False),
    "chan": ([("RotationAccumulationMode", "enum", "", "", I(1))], [], False),
}


def lab2(cfg):
    """LAB2's nodes with layer 2 set as LAYER2_CFGS[cfg]."""
    l2props, baseprops, _ = LAYER2_CFGS[cfg]
    ids = {n: 100 + k for k, n in enumerate(LAB2)}
    nodes, extra = {}, {}
    for name, spec in LAB2.items():
        spec = list(spec) + [None] * (9 - len(spec))
        base, _, t, r, order, active, pre, sc, parent = spec
        nodes[name] = (ids[name], ids[parent] if parent else 0, "LimbNode", t or (0, 0, 0), r or (0, 0, 0),
                       (0, 0, 0), order or 0, {k: v for k, v in base.items() if k != "S"})
        extra[name] = (active, pre, sc, base.get("S"))
    s = v.with_nodes(nodes, BIND={})
    objs, conns = v.top(s, "Objects").kids, v.top(s, "Connections").kids
    for o in objs:
        if o.name == "AnimationLayer":
            set_props(o, baseprops)
        if o.name == "Model":
            n = bytes(o.props[1]).split(b"\x00")[0].decode()
            active, pre, sc, _ = extra[n]
            props = o.kids[1].kids
            if active:
                props.append(P("RotationActive", "bool", "", "", I(1)))
            if pre:
                props.append(P("PreRotation", "Vector3D", "Vector", "", *map(D, pre)))
            if sc:
                for p in props:
                    if p.props[0] == b"Lcl Scaling":
                        p.props[4:] = list(map(D, sc))
    nid = 3000
    for n, (_, _, _, sb) in extra.items():
        if sb:
            v.add_channel(s, 501, ids[n], "Lcl Scaling", (1, 1, 1), sb, nid)
            nid += 10
    ln = N("AnimationLayer", [L(502), cname("Layer2", "AnimLayer"), S(b"")])
    set_props(ln, l2props)
    objs.append(ln)
    conns.append(N("C", [S(b"OO"), L(502), L(500)]))
    for name, spec in LAB2.items():
        for (prop, defaults), axes in spec[1].items():
            v.add_channel(s, 502, ids[name], PROPS[prop], defaults, axes, nid)
            nid += 10
    return s


# Third set (lab3.fbx, default Layer2): what decides the odd-parity rotation path for nodes
# without RotationActive, translation layers on children / ordered nodes, static scale.
T0 = ("T", (0, 0, 0))
TY = {"T": {"y": LIN(0, 10)}}
LAB3 = {
    # name: (base, layer2, static T, static R, order, active, pre, static S, parent, type)
    "Arm": ({}, {}, (0, 0, 0), (0, 0, 0), 0, False, None, None, None, "Null"),
    "aP": (TY, {}, (0, 0, 0), (0, 0, 0), 0, False, None, None, "Arm"),
    "aI5": ({"R": {"x": LIN(0, 30), "y": LIN(0, 60)}}, {R0: {"z": LIN(0, 40)}}, (0, 20, 0), (0, 0, 10), 5, False,
            None, None, "aP"),
    "cP": (TY, {}),
    "cI5": ({"R": {"x": LIN(0, 30), "y": LIN(0, 60)}}, {R0: {"z": LIN(0, 40)}}, (0, 20, 0), (0, 0, 10), 5, False,
            None, None, "cP"),
    "cI5g": ({"R": GB}, {R0: GL}, (0, 5, 0), (0, 0, 0), 5, False, None, None, "cP"),
    "cI0": ({"R": GB}, {R0: GL}, (5, 0, 0), (0, 0, 0), 0, False, None, None, "cP"),
    "cI2": ({"R": GB}, {R0: GL}, (0, 0, 5), (0, 0, 0), 2, False, None, None, "cP"),
    "cA2": ({"R": GB}, {R0: GL}, (-5, 0, 0), (0, 0, 0), 2, True, None, None, "cP"),
    "cA3": ({"R": GB}, {R0: GL}, (0, -5, 0), (0, 0, 0), 3, True, None, None, "cP"),
    "cT": ({"T": {"x": LIN(0, 10)}}, {T0: {"x": LIN(0, 5), "z": LIN(0, 3)}}, (0, 0, -5), (0, 0, 0), 0, False, None,
           None, "cP"),
    "sP": ({}, {}, (0, 0, 0), (0, 0, 0), 0, False, None, None, None, "Null"),
    "sI5": ({"R": GB}, {R0: GL}, (0, 0, 0), (0, 0, 0), 5, False, None, None, "sP"),
    "kI5": ({"R": GB}, {R0: GL}, (0, 0, 0), (0, 0, 0), 5, False),
    "kK": ({"T": {"x": LIN(10, 10)}}, {}, (10, 0, 0), (0, 0, 0), 0, False, None, None, "kI5"),
    "kI5a": ({"R": GB}, {R0: GL}, (0, 0, 0), (0, 0, 0), 5, False),
    "kKa": ({"T": {"x": LIN(10, 10)}}, {}, (10, 0, 0), (0, 0, 0), 0, True, (0, 0, 90), None, "kI5a"),
    "tA5": ({"R": GB, "T": {"x": LIN(0, 10)}}, {T0: {"x": LIN(0, 5), "z": LIN(0, 3)}}, (0, 0, 0), (0, 0, 0), 5, True),
    "tA2": ({"R": GB, "T": {"x": LIN(0, 10)}}, {T0: {"x": LIN(0, 5), "z": LIN(0, 3)}}, (0, 0, 0), (0, 0, 0), 2, True),
    "ss": ({"T": {"x": LIN(0, 0)}}, {}, (0, 0, 0), (0, 0, 0), 0, False, None, (2, 3, 4)),
    "ssc": ({"T": {"x": LIN(10, 10)}}, {}, (10, 10, 10), (0, 0, 0), 0, False, None, None, "ss"),
    "ss2": ({"S": {"x": LIN(1, 3)}}, {}, (0, 0, 0), (0, 0, 0), 0, False, None, (2, 3, 4)),
    "ss2c": ({"T": {"x": LIN(10, 10)}}, {}, (10, 10, 10), (0, 0, 0), 0, False, None, None, "ss2"),
}


def build_set(spec_map, l2props=()):
    """Nodes from a LAB2-style map (+ type) with one extra layer "Layer2"."""
    ids = {n: 100 + k for k, n in enumerate(spec_map)}
    nodes, extra = {}, {}
    for name, spec in spec_map.items():
        spec = list(spec) + [None] * (10 - len(spec))
        base, _, t, r, order, active, pre, sc, parent, typ = spec
        nodes[name] = (ids[name], ids[parent] if parent else 0, typ or "LimbNode", t or (0, 0, 0), r or (0, 0, 0),
                       (0, 0, 0), order or 0, {k: v for k, v in base.items() if k != "S"})
        extra[name] = (active, pre, sc, base.get("S"))
    s = v.with_nodes(nodes, BIND={})
    objs, conns = v.top(s, "Objects").kids, v.top(s, "Connections").kids
    for o in objs:
        if o.name == "Model":
            n = bytes(o.props[1]).split(b"\x00")[0].decode()
            active, pre, sc, _ = extra[n]
            props = o.kids[1].kids
            if active:
                props.append(P("RotationActive", "bool", "", "", I(1)))
            if pre:
                props.append(P("PreRotation", "Vector3D", "Vector", "", *map(D, pre)))
            if sc:
                for p in props:
                    if p.props[0] == b"Lcl Scaling":
                        p.props[4:] = list(map(D, sc))
    nid = 3000
    for n, (_, _, _, sb) in extra.items():
        if sb:
            v.add_channel(s, 501, ids[n], "Lcl Scaling", (1, 1, 1), sb, nid)
            nid += 10
    ln = N("AnimationLayer", [L(502), cname("Layer2", "AnimLayer"), S(b"")])
    set_props(ln, list(l2props))
    objs.append(ln)
    conns.append(N("C", [S(b"OO"), L(502), L(500)]))
    for name, spec in spec_map.items():
        for (prop, defaults), axes in spec[1].items():
            v.add_channel(s, 502, ids[name], PROPS[prop], defaults, axes, nid)
            nid += 10
    return s


def dai_scene(which):
    """Small skeletons for DefaultAttributeIndex: nodes listed in `which` have none."""
    nodes = {
        "Top": (100, 0, "LimbNode", (0, 0, 0), (0, 0, 0), (0, 0, 0), 0, {"R": {"z": LIN(0, 90)}}),
        "Kid": (101, 100, "LimbNode", (0, 10, 0), (0, 0, 0), (0, 0, 0), 0, {"R": {"x": LIN(0, 90)}}),
        "Other": (102, 0, "LimbNode", (5, 0, 0), (0, 0, 0), (0, 0, 0), 0, {"T": {"y": LIN(0, 10)}}),
    }
    s = v.with_nodes(nodes, BIND={})
    for o in v.top(s, "Objects").kids:
        if o.name == "Model" and bytes(o.props[1]).split(b"\x00")[0].decode() in which:
            o.kids[1].kids = [p for p in o.kids[1].kids if p.props[0] != b"DefaultAttributeIndex"]
    return s


def dai_arm():
    """The base scene with only the top Null (Armature) lacking DefaultAttributeIndex."""
    s = v.with_nodes(v.BASE_NODES)
    for o in v.top(s, "Objects").kids:
        if o.name == "Model" and bytes(o.props[1]).startswith(b"Armature\x00"):
            o.kids[1].kids = [p for p in o.kids[1].kids if p.props[0] != b"DefaultAttributeIndex"]
    return s


def rest_scene(mid_type, dai):
    """Hips (T.y) -> Mid (static R (0, 0, 25), curve node d|Z 10, R.x 20 -> 50; type mid_type,
    DefaultAttributeIndex or none) -> Tip (R.y 0 -> 90); Hips -> Still (static R (0, 0, 25), no
    curves) -> Tip2 (R.x 0 -> 90): what rest an animated node that is no Pose has."""
    nodes = {
        "Hips": (100, 0, "LimbNode", (0, 0, 0), (0, 0, 0), (0, 0, 0), 0, {"T": {"y": LIN(0, 10)}}),
        "Mid": (101, 100, mid_type, (0, 10, 0), (0, 0, 25), (0, 0, 0), 0, {"R": {"x": LIN(20, 50)}}),
        "Tip": (102, 101, "LimbNode", (0, 5, 0), (0, 0, 0), (0, 0, 0), 0, {"R": {"y": LIN(0, 90)}}),
        "Still": (103, 100, "LimbNode", (5, 0, 0), (0, 0, 25), (0, 0, 0), 0, {}),
        "Tip2": (104, 103, "LimbNode", (0, 5, 0), (0, 0, 0), (0, 0, 0), 0, {"R": {"x": LIN(0, 90)}}),
    }
    s = v.with_nodes(nodes, BIND={})
    for o in v.top(s, "Objects").kids:
        if o.name == "AnimationCurveNode" and o is [c for c in v.top(s, "Objects").kids
                                                   if c.name == "AnimationCurveNode"][1]:
            o.kids = [p70(*[P("d|" + a.upper(), "Number", "", "A", D(x)) for a, x in zip("xyz", (0, 0, 10))])]
        if o.name == "Model" and bytes(o.props[1]).startswith(b"Mid\x00") and not dai:
            o.kids[1].kids = [p for p in o.kids[1].kids if p.props[0] != b"DefaultAttributeIndex"]
    return s


def files():
    out = {f"lab_{k}.fbx": mf.write_fbx(lab(k)) for k in LAYER_CFGS}
    out.update({f"lab2_{k}.fbx": mf.write_fbx(lab2(k)) for k in LAYER2_CFGS})
    out["pivots.fbx"] = mf.write_fbx(pivots())
    out["lab3.fbx"] = mf.write_fbx(build_set(LAB3))
    for k, which in [("top", ("Top",)), ("kid", ("Kid",)), ("other", ("Other",)), ("all", ("Top", "Kid", "Other")),
                     ("topother", ("Top", "Other"))]:
        out[f"dai3_{k}.fbx"] = mf.write_fbx(dai_scene(which))
    out["dai_arm.fbx"] = mf.write_fbx(dai_arm())
    out["rest_nodai.fbx"] = mf.write_fbx(rest_scene("LimbNode", False))
    out["rest_null.fbx"] = mf.write_fbx(rest_scene("Null", True))
    for name, custom in [("fps_c7_4", 7.4), ("fps_c7_5", 7.5), ("fps_c7_6", 7.6), ("fps_c0_4", 0.4), ("fps_cm5", -5.0),
                         ("fps_c48", 48.0), ("fps_c29_97", 29.97),
                         ("fps_c0_5", 0.5), ("fps_c0_6", 0.6), ("fps_c1", 1.0), ("fps_c1_5", 1.5), ("fps_c2_5", 2.5),
                         ("fps_c3_5", 3.5), ("fps_c8_5", 8.5), ("fps_c9_5", 9.5), ("fps_c10_5", 10.5),
                         ("fps_c11_5", 11.5), ("fps_c13_5", 13.5), ("fps_c12_4", 12.4), ("fps_c6_9", 6.9),
                         ("fps_c0_99", 0.99), ("fps_c0_9", 0.9)]:
        out[name + ".fbx"] = mf.write_fbx(fps(14, custom))
    for name, mode, custom in [("fps_c12", 14, 12.5), ("fps_c10", 14, 10.0), ("fps_c0", 14, 0.0),
                               ("fps_cnone", 14, None), ("fps60", 3, None), ("fps25", 10, None),
                               ("fps2997", 9, None), ("fps0", 0, None), ("fps120", 1, None),
                               ("fps_c24m", 11, 12.5)]:
        out[name + ".fbx"] = mf.write_fbx(fps(mode, custom))
    l3 = build_set(LAB3)
    v.set_gs(l3, "TimeMode", I(14))
    out["fps_l3none.fbx"] = mf.write_fbx(l3)
    half = v.timing(take=(1.0, 1.5))
    v.set_gs(half, "TimeMode", I(14))
    out["fps_halfnone.fbx"] = mf.write_fbx(half)
    for name, take, mode, custom in [("fps_frac30", (1.0, 2.04), 6, None), ("fps_frac75", (1.0, 1.37), 14, 7.5),
                                     ("fps_fb19", (1.0, 2.9), 14, None), ("fps_fb3", (1.0, 4.0), 14, None)]:
        f = v.timing(take=take)
        v.set_gs(f, "TimeMode", I(mode))
        if custom is not None:
            v.top(f, "GlobalSettings").kids[1].kids.append(P("CustomFrameRate", "double", "Number", "", D(custom)))
        out[name + ".fbx"] = mf.write_fbx(f)
    for m in ("none", "neg", "mixed"):
        out[f"dai_{m}.fbx"] = mf.write_fbx(dai(m))
    return out


if __name__ == "__main__":
    o = sys.argv[1] if len(sys.argv) > 1 else os.path.join(HERE, "variants")
    os.makedirs(o, exist_ok=True)
    for name, data in files().items():
        with open(os.path.join(o, name), "wb") as f:
            f.write(data)
    print("wrote", len(files()), "lab files to", o)
