#!/usr/bin/env python3
"""Deterministic FBX animation fixtures for Plugin:ImportFbxAnimation (rustudio's own files: no
third-party or proprietary content). Writes wave.fbx next to this script: binary FBX 7.4 laid out
the way FBX SDK exporters write it (header, FileId / CreationTime / footer that belong together,
GlobalSettings, Definitions, Objects with Model + NodeAttribute / AnimationStack / AnimationLayer /
AnimationCurveNode / AnimationCurve / BindPose, Connections, Takes; zlib-compressed key arrays).
Studio's importer rejects a file ("Import failed") without the Definitions section, with a footer
code that doesn't match the file id and creation time, or without the null record after a
childless object such as AnimationLayer; it treats a LimbNode as a bone only when its
DefaultAttributeIndex points at a Skeleton NodeAttribute.

What Studio makes of the file was measured, not computed here: captures/fbxanim-studio.txt
(studio_fbxanim.py); variants.py writes the other measured files from this scene.

    python checks/plugins/services/fixtures/fbxanim/make_fixtures.py   (rewrites wave.fbx)
"""
import math
import os
import struct
import zlib

HERE = os.path.dirname(os.path.abspath(__file__))
TICKS = 46186158000
FPS = 30


# ---- binary FBX writer -------------------------------------------------------------------------

class N:
    def __init__(self, name, props=(), kids=()):
        self.name, self.props, self.kids = name, list(props), list(kids)


class S(bytes):
    pass


class L(int):
    pass


class I(int):
    pass


class D(float):
    pass


class R(bytes):
    pass


class Arr:
    def __init__(self, code, values, compress=True):
        self.code, self.values, self.compress = code, list(values), compress


def enc_prop(p):
    if isinstance(p, S):
        return b"S" + struct.pack("<I", len(p)) + bytes(p)
    if isinstance(p, R):
        return b"R" + struct.pack("<I", len(p)) + bytes(p)
    if isinstance(p, L):
        return b"L" + struct.pack("<q", p)
    if isinstance(p, I):
        return b"I" + struct.pack("<i", p)
    if isinstance(p, D):
        return b"D" + struct.pack("<d", p)
    if isinstance(p, Arr):
        fmt = {"d": "d", "f": "f", "l": "q", "i": "i"}[p.code]
        raw = struct.pack("<%d%s" % (len(p.values), fmt), *p.values)
        if p.compress:
            data, e = zlib.compress(raw, 9), 1
        else:
            data, e = raw, 0
        return p.code.encode() + struct.pack("<III", len(p.values), e, len(data)) + data
    raise TypeError(p)


def enc_node(n, at, big=False):
    """big: FBX 7.5 layout (64-bit offsets, 25-byte null records)."""
    props = b"".join(enc_prop(p) for p in n.props)
    rec = 25 if big else 13
    head_len = rec + len(n.name)
    body = bytearray()
    o = at + head_len + len(props)
    for k in n.kids:
        b = enc_node(k, o, big)
        body += b
        o += len(b)
    # A null record ends a node with children or no properties, and every object of the Objects
    # section (FBX SDK files write one after a childless AnimationLayer too; without it Studio's
    # importer rejects the file: "Import failed").
    if n.kids or not n.props or getattr(n, "block", False):
        body += b"\0" * rec
    end = at + head_len + len(props) + len(body)
    return struct.pack("<QQQB" if big else "<IIIB", end, len(n.props), len(props), len(n.name)) + n.name.encode() + props + bytes(body)


# The file id, creation time and footer code exporters write together (the values Blender's FBX
# exporter writes). The FBX SDK (Studio's importer) checks the footer code against the file id and
# creation time and rejects the file ("Import failed") when they don't belong together.
FILE_ID = bytes.fromhex("28 b3 2a eb b6 24 cc c2 bf c8 b0 2a a9 2b fc f1")
CREATION_TIME = b"1970-01-01 10:00:00:000"
FOOT_ID = bytes.fromhex("fa bc ab 09 d0 c8 d4 66 b1 76 fb 83 1c f7 26 7e")


def complete(top):
    """The top-level nodes as exporters write them: the header, file id, creation time, creator,
    then the rest with a Definitions section before Objects."""
    head = [n for n in top if n.name == "FBXHeaderExtension"]
    rest = [n for n in top if n.name not in ("FBXHeaderExtension", "FileId", "CreationTime", "Creator")]
    objects = next((n for n in rest if n.name == "Objects"), None)
    if objects is not None and not any(n.name == "Definitions" for n in rest):
        # The object types and counts (Studio's importer rejects a file without Definitions:
        # "Import failed").
        counts = {}
        for o in objects.kids:
            counts[o.name] = counts.get(o.name, 0) + 1
        kinds = [N("ObjectType", [S(b"GlobalSettings")], [N("Count", [I(1)])])] + [
            N("ObjectType", [S(k.encode())], [N("Count", [I(v)])]) for k, v in counts.items()]
        defs = N("Definitions", [], [N("Version", [I(100)]), N("Count", [I(1 + sum(counts.values()))]), *kinds])
        rest.insert(rest.index(objects), defs)
    return head + [N("FileId", [R(FILE_ID)]), N("CreationTime", [S(CREATION_TIME)]),
                   N("Creator", [S(b"rustudio fixture generator")])] + rest


def write_fbx(top, version=7400):
    out = bytearray(b"Kaydara FBX Binary  \x00\x1a\x00" + struct.pack("<I", version))
    top = complete(top)
    for n in top:
        if n.name == "Objects":
            for o in n.kids:
                o.block = True
        out += enc_node(n, len(out), version >= 7500)
    out += b"\0" * (25 if version >= 7500 else 13)
    # Footer as exporters write it: the footer code, 4 zero bytes, padding to 16 (a whole 16 when
    # already aligned), the version, 120 zero bytes, the magic.
    out += FOOT_ID + b"\0" * 4
    pad = ((len(out) + 15) & ~15) - len(out)
    out += b"\0" * (pad or 16)
    out += struct.pack("<I", version) + b"\0" * 120
    out += bytes.fromhex("f8 5a 8c 6a de f5 d9 7e ec e9 0c e3 75 8f 29 0b")
    return bytes(out)


def P(name, typ, label, flags, *vals):
    return N("P", [S(name.encode()), S(typ.encode()), S(label.encode()), S(flags.encode()), *vals])


def p70(*ps):
    return N("Properties70", [], ps)


def cname(name, cls):
    return S(name.encode() + b"\x00\x01" + cls.encode())


# ---- transform maths (column vectors, m[row][col]) -----------------------------------------------

def mat_mul(a, b):
    return [[sum(a[i][k] * b[k][j] for k in range(4)) for j in range(4)] for i in range(4)]


def ident():
    return [[float(i == j) for j in range(4)] for i in range(4)]


def trans(v):
    m = ident()
    m[0][3], m[1][3], m[2][3] = v
    return m


def scale(v):
    m = ident()
    m[0][0], m[1][1], m[2][2] = v
    return m


def rot_axis(axis, deg):
    r = math.radians(deg)
    c, s = math.cos(r), math.sin(r)
    m = ident()
    i, j = {"x": (1, 2), "y": (2, 0), "z": (0, 1)}[axis]
    m[i][i], m[i][j], m[j][i], m[j][j] = c, -s, s, c
    return m


ORDERS = ["xyz", "xzy", "yzx", "yxz", "zxy", "zyx"]


def euler(deg, order=0):
    """FBX Euler rotation: the first axis of the order is applied first."""
    m = ident()
    for axis in ORDERS[order]:
        m = mat_mul(rot_axis(axis, deg["xyz".index(axis)]), m)
    return m


def inverse_rigid(m):
    r = [[m[j][i] for j in range(3)] for i in range(3)]
    t = [-sum(r[i][k] * m[k][3] for k in range(3)) for i in range(3)]
    return [r[0] + [t[0]], r[1] + [t[1]], r[2] + [t[2]], [0.0, 0.0, 0.0, 1.0]]


def local(t, r, s, pre=(0, 0, 0), post=(0, 0, 0), order=0):
    m = trans(t)
    m = mat_mul(m, euler(pre))
    m = mat_mul(m, euler(r, order))
    m = mat_mul(m, inverse_rigid(euler(post)))
    return mat_mul(m, scale(s))


# ---- curves ----------------------------------------------------------------------------------------

CONSTANT, LINEAR, CUBIC = 0x2, 0x4, 0x8 | 0x400  # cubic with user tangents


class Curve:
    def __init__(self, keys, flags=LINEAR):
        self.keys = keys  # [(seconds, value, right_slope, next_left_slope)] or [(seconds, value)]
        self.flags = flags

    def times(self):
        return [round(k[0] * TICKS) for k in self.keys]

    def at(self, tick):
        ks = self.keys
        t = tick / TICKS
        if t <= ks[0][0]:
            return ks[0][1]
        for a, b in zip(ks, ks[1:]):
            if t < b[0]:
                u = (t - a[0]) / (b[0] - a[0])
                if self.flags == CONSTANT:
                    return a[1]
                if self.flags == LINEAR:
                    return a[1] + (b[1] - a[1]) * u
                dt = b[0] - a[0]
                s0 = a[2] if len(a) > 2 else 0.0
                s1 = a[3] if len(a) > 3 else 0.0
                h00, h10 = 2 * u ** 3 - 3 * u ** 2 + 1, u ** 3 - 2 * u ** 2 + u
                h01, h11 = -2 * u ** 3 + 3 * u ** 2, u ** 3 - u ** 2
                return h00 * a[1] + h10 * dt * s0 + h01 * b[1] + h11 * dt * s1
        return ks[-1][1]


# ---- the scene ---------------------------------------------------------------------------------------

START, STOP = 1.0, 2.0
UNIT = 1.0  # UnitScaleFactor: the file counts centimetres

# name: (id, parent id, type, static T, static R, pre-rotation, rotation order, {channel: {axis: Curve}})
NODES = {
    "Armature": (100, 0, "Null", (0, 0, 0), (0, 0, 0), (0, 0, 0), 0, {}),
    "LowerTorso": (101, 100, "LimbNode", (0, 100, 0), (0, 0, 0), (0, 0, 0), 0, {
        # one key on X (a constant 5 cm offset), Y with a key past LocalStop
        "T": {"x": Curve([(1.0, 5.0)]), "y": Curve([(1.0, 100.0), (1.5, 110.0), (2.5, 90.0)])},
    }),
    "UpperTorso": (102, 101, "LimbNode", (0, 20, 0), (0, 0, 10), (0, 0, 0), 5, {
        # ZYX order; Z has no curve (its curve node default, 10, is the static value)
        "R": {"x": Curve([(1.0, 0.0), (2.0, 30.0)]), "y": Curve([(1.0, 0.0), (2.0, 60.0)])},
    }),
    "RightUpperArm": (103, 102, "LimbNode", (15, 10, 0), (0, 0, 0), (0, 0, 90), 0, {
        "R": {"x": Curve([(1.0, 0.0, 0.0, 0.0), (2.0, 90.0, 0.0, 0.0)], CUBIC)},
    }),
    "Antenna": (104, 102, "LimbNode", (0, 30, 0), (0, 0, 30), (0, 0, 0), 0, {
        "R": {"z": Curve([(1.0, 0.0), (1.25, 20.0), (1.75, 45.0), (2.0, 45.0)], CONSTANT)},
    }),
    "LeftUpperArm": (105, 102, "LimbNode", (-15, 10, 0), (0, 0, 0), (0, 0, 0), 0, {}),
    "Prop": (106, 100, "Null", (0, 0, 0), (0, 0, 0), (0, 0, 0), 0, {
        "T": {"x": Curve([(1.0, 0.0), (2.0, 50.0)])},
    }),
}
# Antenna's bind pose: rotation 0 (the file's static Lcl Rotation, 30, is the pose it was saved in).
BIND = {"Antenna": None}


def static_local(name):
    _, _, _, t, r, pre, order, _ = NODES[name]
    return local(t, r, (1, 1, 1), pre, (0, 0, 0), order)


def by_id(i):
    return next(n for n, v in NODES.items() if v[0] == i)


def static_world(name):
    pid = NODES[name][1]
    m = static_local(name)
    return m if pid == 0 else mat_mul(static_world(by_id(pid)), m)


def bind_world(name):
    """Antenna rests unrotated where its static translation puts it."""
    pid = NODES[name][1]
    _, _, _, t, _, pre, _, _ = NODES[name]
    return mat_mul(static_world(by_id(pid)), local(t, (0, 0, 0), (1, 1, 1), pre))


def scene():
    objects, conns = [], []
    for name, (i, pid, typ, t, r, pre, order, _) in NODES.items():
        props = [P("Lcl Translation", "Lcl Translation", "", "A", *map(D, t)),
                 P("Lcl Rotation", "Lcl Rotation", "", "A", *map(D, r)),
                 P("Lcl Scaling", "Lcl Scaling", "", "A", D(1), D(1), D(1)),
                 # Which connected NodeAttribute is the node's (the FBX SDK's default is none: a
                 # LimbNode without it is no bone to Studio's importer).
                 P("DefaultAttributeIndex", "int", "Integer", "", I(0))]
        if pre != (0, 0, 0):
            props += [P("RotationActive", "bool", "", "", I(1)),
                      P("PreRotation", "Vector3D", "Vector", "", *map(D, pre))]
        if order:
            props.append(P("RotationOrder", "enum", "", "", I(order)))
        objects.append(N("Model", [L(i), cname(name, "Model"), S(typ.encode())],
                         [N("Version", [I(232)]), p70(*props)]))
        conns.append(N("C", [S(b"OO"), L(i), L(pid)]))
        # The node's attribute, as exporters write it: a Skeleton for LimbNodes (Studio's importer
        # finds no animation in a skeleton made of LimbNode Models without one: "Import failed"),
        # a Null for Nulls.
        objects.append(N("NodeAttribute", [L(i + 10000), cname("", "NodeAttribute"), S(typ.encode())],
                         [N("TypeFlags", [S(b"Skeleton" if typ == "LimbNode" else b"Null")])]))
        conns.append(N("C", [S(b"OO"), L(i + 10000), L(i)]))
    # Bind pose (global matrices, as FbxAMatrix: translation in elements 12..14).
    pose_nodes = []
    for name in BIND:
        m = bind_world(name)
        flat = [m[r][c] for c in range(4) for r in range(4)]
        pose_nodes.append(N("PoseNode", [], [N("Node", [L(NODES[name][0])]),
                                              N("Matrix", [Arr("d", flat, compress=False)])]))
    objects.append(N("Pose", [L(900), cname("BindPose", "Pose"), S(b"BindPose")],
                     [N("Type", [S(b"BindPose")]), N("Version", [I(100)]), N("NbPoseNodes", [I(len(pose_nodes))]),
                      *pose_nodes]))
    stack, layer = 500, 501
    objects.append(N("AnimationStack", [L(stack), cname("Wave", "AnimStack"), S(b"")], [p70(
        P("LocalStart", "KTime", "Time", "", L(round(START * TICKS))),
        P("LocalStop", "KTime", "Time", "", L(round(STOP * TICKS))),
        P("ReferenceStart", "KTime", "Time", "", L(round(START * TICKS))),
        P("ReferenceStop", "KTime", "Time", "", L(round(STOP * TICKS))))]))
    objects.append(N("AnimationLayer", [L(layer), cname("BaseLayer", "AnimLayer"), S(b"")]))
    conns.append(N("C", [S(b"OO"), L(layer), L(stack)]))
    nid = 600
    for name, (i, _, _, t, r, _, _, ch) in NODES.items():
        for c, axes in ch.items():
            nid += 1
            cn = nid
            static = {"T": t, "R": r}[c]
            objects.append(N("AnimationCurveNode", [L(cn), cname(c, "AnimCurveNode"), S(b"")], [p70(
                *[P("d|" + a.upper(), "Number", "", "A", D(static["xyz".index(a)])) for a in "xyz"])]))
            conns.append(N("C", [S(b"OO"), L(cn), L(layer)]))
            conns.append(N("C", [S(b"OP"), L(cn), L(i), S({"T": b"Lcl Translation", "R": b"Lcl Rotation"}[c])]))
            for axis, curve in axes.items():
                nid += 1
                kids = [N("Default", [D(curve.keys[0][1])]), N("KeyVer", [I(4009)]),
                        N("KeyTime", [Arr("l", curve.times())]),
                        N("KeyValueFloat", [Arr("f", [k[1] for k in curve.keys])]),
                        N("KeyAttrFlags", [Arr("i", [curve.flags])]),
                        N("KeyAttrDataFloat", [Arr("f", [curve.keys[0][2] if len(curve.keys[0]) > 2 else 0.0,
                                                         curve.keys[0][3] if len(curve.keys[0]) > 3 else 0.0,
                                                         9.419963346924634e-30, 0.0])]),
                        N("KeyAttrRefCount", [Arr("i", [len(curve.keys)])])]
                objects.append(N("AnimationCurve", [L(nid), cname("", "AnimCurve"), S(b"")], kids))
                conns.append(N("C", [S(b"OP"), L(nid), L(cn), S(("d|" + axis.upper()).encode())]))
    gs = N("GlobalSettings", [], [N("Version", [I(1000)]), p70(
        P("UpAxis", "int", "Integer", "", I(1)), P("UpAxisSign", "int", "Integer", "", I(1)),
        P("FrontAxis", "int", "Integer", "", I(2)), P("CoordAxis", "int", "Integer", "", I(0)),
        P("UnitScaleFactor", "double", "Number", "", D(UNIT)),
        P("TimeMode", "enum", "", "", I(6)),  # 30 frames per second
        P("TimeSpanStart", "KTime", "Time", "", L(round(START * TICKS))),
        P("TimeSpanStop", "KTime", "Time", "", L(round(STOP * TICKS))))])
    header = N("FBXHeaderExtension", [], [N("FBXHeaderVersion", [I(1003)]), N("FBXVersion", [I(7400)]),
                                          N("Creator", [S(b"rustudio fixture generator")])])
    takes = N("Takes", [], [N("Current", [S(b"Wave")]), N("Take", [S(b"Wave")], [
        N("FileName", [S(b"Wave.tak")]),
        N("LocalTime", [L(round(START * TICKS)), L(round(STOP * TICKS))]),
        N("ReferenceTime", [L(round(START * TICKS)), L(round(STOP * TICKS))])])])
    return [header, gs, N("Objects", [], objects), N("Connections", [], conns), takes]


def main():
    with open(os.path.join(HERE, "wave.fbx"), "wb") as f:
        f.write(write_fbx(scene()))
    print("wrote wave.fbx")


if __name__ == "__main__":
    main()
