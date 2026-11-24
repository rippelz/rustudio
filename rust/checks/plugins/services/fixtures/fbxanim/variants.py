#!/usr/bin/env python3
"""FBX animation variants for measuring Studio's Plugin:ImportFbxAnimation (rustudio's own files).
Each variant changes one thing of make_fixtures.py's wave scene, so a Studio capture of the import
isolates one rule. Writes them into OUT_DIR (default: ./variants next to this script).

    python variants.py [OUT_DIR]

  wave.fbx        the base scene (make_fixtures.py)
  ns.fbx          node names "mixamorig:LowerTorso", "a:b:UpperTorso", "rightupperarm" (case),
                  "Antenna"
  stacks.fbx      two AnimationStacks: "Wave" (the base) then "Second" (LowerTorso T.y 100 -> 150
                  over 0..1 s, Takes Current = "Second")
  layers.fbx      the base stack with a second layer "Layer2": UpperTorso R.z 0 -> 40 linear
  meters.fbx      UnitScaleFactor 100 (metres)
  zup.fbx         UpAxis 2 (Z), FrontAxis 1, CoordAxis 0
  scale.fbx       LowerTorso Lcl Scaling x 1 -> 2 (plus its T.y keys)
  fps24.fbx       TimeMode 11 (24 fps)
  ascii.fbx       the base scene as ASCII FBX 7.4
  r6.fbx          nodes Torso / Head / "Right Arm" under HumanoidRootPart, Torso T.y and arm R.x
  root.fbx        HumanoidRootPart itself animated (T.z 0 -> 100) plus LowerTorso R.y
  gaps.fbx        Hips (T.y) -> Mid (unanimated LimbNode, R.z 30) -> Tip (R.x); Hips -> NullMid
                  (unanimated Null) -> Tip2 (R.y); Hips -> Leaf (unanimated LimbNode)
  norange.fbx     the base scene, its AnimationStack without LocalStart / LocalStop
  hrpname.fbx     root.fbx with HumanoidRootPart named "Root"
  rstatic.fbx     Root (no curves) -> LowerTorso (R.y)
  rmove.fbx       Root (T.x 0->40, T.y 0->20, R.y 0->90) -> LowerTorso (a flat T.x curve)
  hmove.fbx       rmove.fbx with Root named HumanoidRootPart
  rmovex.fbx      rmove.fbx with LowerTorso named Torso
  hoff.fbx        HumanoidRootPart at (10, 50, 0) (R.y 0->90) -> LowerTorso (R.y)
  mesh.fbx        one Model, no animation
  notes.txt       not an FBX file
"""
import copy
import os
import sys

HERE = os.path.dirname(os.path.abspath(__file__))
sys.path.insert(0, HERE)
import make_fixtures as mf  # noqa: E402
from make_fixtures import N, S, L, I, D, Arr, P, p70, cname, Curve, LINEAR, CONSTANT, TICKS  # noqa: E402

BASE_NODES = copy.deepcopy(mf.NODES)


def with_nodes(nodes, **glob):
    saved = {k: getattr(mf, k) for k in ("NODES", "UNIT", "BIND")}
    mf.NODES = nodes
    for k, v in glob.items():
        setattr(mf, k, v)
    try:
        return mf.scene()
    finally:
        for k, v in saved.items():
            setattr(mf, k, v)


def top(scene, name):
    return next(n for n in scene if n.name == name)


def set_gs(scene, key, val):
    gs = top(scene, "GlobalSettings")
    props = gs.kids[1]
    for p in props.kids:
        if p.props[0] == key.encode():
            p.props[4] = val
            return
    props.kids.append(P(key, "int", "Integer", "", val))


def renamed(scene, names):
    for o in top(scene, "Objects").kids:
        if o.name == "Model":
            n = bytes(o.props[1]).split(b"\x00")[0].decode()
            if n in names:
                o.props[1] = cname(names[n], "Model")
    return scene


def curve_objs(cid, curve, axis, cn, conns):
    objs = [N("AnimationCurve", [L(cid), cname("", "AnimCurve"), S(b"")], [
        N("Default", [D(curve.keys[0][1])]), N("KeyVer", [I(4009)]),
        N("KeyTime", [Arr("l", curve.times())]),
        N("KeyValueFloat", [Arr("f", [k[1] for k in curve.keys])]),
        N("KeyAttrFlags", [Arr("i", [curve.flags])]),
        N("KeyAttrDataFloat", [Arr("f", [0.0, 0.0, 9.419963346924634e-30, 0.0])]),
        N("KeyAttrRefCount", [Arr("i", [len(curve.keys)])])])]
    conns.append(N("C", [S(b"OP"), L(cid), L(cn), S(("d|" + axis.upper()).encode())]))
    return objs


def add_channel(scene, parent_layer, model_id, prop, defaults, axes, nid):
    """A curve node on model_id's `prop` in layer parent_layer with curves axes {axis: Curve}."""
    objs, conns = top(scene, "Objects").kids, top(scene, "Connections").kids
    objs.append(N("AnimationCurveNode", [L(nid), cname(prop[4], "AnimCurveNode"), S(b"")], [p70(
        *[P("d|" + a.upper(), "Number", "", "A", D(defaults["xyz".index(a)])) for a in "xyz"])]))
    conns.append(N("C", [S(b"OO"), L(nid), L(parent_layer)]))
    conns.append(N("C", [S(b"OP"), L(nid), L(model_id), S(prop.encode())]))
    k = nid
    for axis, c in axes.items():
        k += 1
        objs.extend(curve_objs(k, c, axis, nid, conns))


def stacks():
    s = with_nodes(BASE_NODES)
    objs, conns = top(s, "Objects").kids, top(s, "Connections").kids
    objs.append(N("AnimationStack", [L(510), cname("Second", "AnimStack"), S(b"")], [p70(
        P("LocalStart", "KTime", "Time", "", L(0)), P("LocalStop", "KTime", "Time", "", L(TICKS)),
        P("ReferenceStart", "KTime", "Time", "", L(0)), P("ReferenceStop", "KTime", "Time", "", L(TICKS)))]))
    objs.append(N("AnimationLayer", [L(511), cname("BaseLayer", "AnimLayer"), S(b"")]))
    conns.append(N("C", [S(b"OO"), L(511), L(510)]))
    add_channel(s, 511, 101, "Lcl Translation", (0, 100, 0), {"y": Curve([(0.0, 100.0), (1.0, 150.0)])}, 800)
    takes = top(s, "Takes")
    takes.kids[0] = N("Current", [S(b"Second")])
    takes.kids.append(N("Take", [S(b"Second")], [N("FileName", [S(b"Second.tak")]),
                                                 N("LocalTime", [L(0), L(TICKS)]),
                                                 N("ReferenceTime", [L(0), L(TICKS)])]))
    return s


def layers():
    s = with_nodes(BASE_NODES)
    objs, conns = top(s, "Objects").kids, top(s, "Connections").kids
    objs.append(N("AnimationLayer", [L(502), cname("Layer2", "AnimLayer"), S(b"")]))
    conns.append(N("C", [S(b"OO"), L(502), L(500)]))
    add_channel(s, 502, 102, "Lcl Rotation", (0, 0, 0), {"z": Curve([(1.0, 0.0), (2.0, 40.0)])}, 820)
    return s


def scale():
    nodes = copy.deepcopy(BASE_NODES)
    s = with_nodes(nodes)
    add_channel(s, 501, 101, "Lcl Scaling", (1, 1, 1), {"x": Curve([(1.0, 1.0), (2.0, 2.0)])}, 840)
    return s


def r6():
    nodes = {
        "HumanoidRootPart": (100, 0, "LimbNode", (0, 0, 0), (0, 0, 0), (0, 0, 0), 0, {}),
        "Torso": (101, 100, "LimbNode", (0, 0, 0), (0, 0, 0), (0, 0, 0), 0, {
            "T": {"y": Curve([(1.0, 0.0), (2.0, 20.0)])}}),
        "Head": (102, 101, "LimbNode", (0, 150, 0), (0, 0, 0), (0, 0, 0), 0, {
            "R": {"y": Curve([(1.0, 0.0), (2.0, 45.0)])}}),
        "Right Arm": (103, 101, "LimbNode", (150, 50, 0), (0, 0, 0), (0, 0, 0), 0, {
            "R": {"x": Curve([(1.0, 0.0), (2.0, 90.0)])}}),
    }
    return with_nodes(nodes, BIND={})


def root():
    nodes = {
        "HumanoidRootPart": (100, 0, "LimbNode", (0, 0, 0), (0, 0, 0), (0, 0, 0), 0, {
            "T": {"z": Curve([(1.0, 0.0), (2.0, 100.0)])}}),
        "LowerTorso": (101, 100, "LimbNode", (0, 100, 0), (0, 0, 0), (0, 0, 0), 0, {
            "R": {"y": Curve([(1.0, 0.0), (2.0, 90.0)])}}),
    }
    return with_nodes(nodes, BIND={})


def gaps():
    nodes = {
        "Hips": (100, 0, "LimbNode", (0, 100, 0), (0, 0, 0), (0, 0, 0), 0, {
            "T": {"y": Curve([(1.0, 100.0), (2.0, 110.0)])}}),
        "Mid": (101, 100, "LimbNode", (0, 10, 0), (0, 0, 30), (0, 0, 0), 0, {}),
        "Tip": (102, 101, "LimbNode", (0, 5, 0), (0, 0, 0), (0, 0, 0), 0, {
            "R": {"x": Curve([(1.0, 0.0), (2.0, 90.0)])}}),
        "NullMid": (103, 100, "Null", (5, 0, 0), (0, 0, 0), (0, 0, 0), 0, {}),
        "Tip2": (104, 103, "LimbNode", (0, 0, 5), (0, 0, 0), (0, 0, 0), 0, {
            "R": {"y": Curve([(1.0, 0.0), (2.0, 90.0)])}}),
        "Leaf": (105, 100, "LimbNode", (-5, 0, 0), (0, 0, 0), (0, 0, 0), 0, {}),
    }
    return with_nodes(nodes, BIND={})


def rootvar(top_name, top_anim, child="LowerTorso", child_anim=None, top_t=(0, 0, 0)):
    """A two-node skeleton: top_name (curves top_anim) -> child (LowerTorso by default)."""
    nodes = {
        top_name: (100, 0, "LimbNode", top_t, (0, 0, 0), (0, 0, 0), 0, top_anim),
        child: (101, 100, "LimbNode", (0, 100, 0), (0, 0, 0), (0, 0, 0), 0, child_anim or {
            "R": {"y": Curve([(1.0, 0.0), (2.0, 90.0)])}}),
    }
    return with_nodes(nodes, BIND={})


MOVE = {"T": {"x": Curve([(1.0, 0.0), (2.0, 40.0)]), "y": Curve([(1.0, 0.0), (2.0, 20.0)])},
        "R": {"y": Curve([(1.0, 0.0), (2.0, 90.0)])}}


def rootvar2():
    """Root at the origin turned 30 degrees about Y at rest, T.x 0 -> 40 -> LowerTorso (R.y)."""
    nodes = {
        "Root": (100, 0, "LimbNode", (0, 0, 0), (0, 30, 0), (0, 0, 0), 0, {
            "T": {"x": Curve([(1.0, 0.0), (2.0, 40.0)])}}),
        "LowerTorso": (101, 100, "LimbNode", (0, 37, 0), (0, 0, 0), (0, 0, 0), 0, {
            "R": {"y": Curve([(1.0, 0.0), (2.0, 90.0)])}}),
    }
    return with_nodes(nodes, BIND={})


def r2kids():
    nodes = {
        "Root": (100, 0, "LimbNode", (0, 0, 0), (0, 0, 0), (0, 0, 0), 0, MOVE),
        "LowerTorso": (101, 100, "LimbNode", (0, 100, 0), (0, 0, 0), (0, 0, 0), 0, {
            "R": {"y": Curve([(1.0, 0.0), (2.0, 90.0)])}}),
        "Other": (102, 100, "LimbNode", (20, 60, 0), (0, 0, 0), (0, 0, 0), 0, {
            "R": {"x": Curve([(1.0, 0.0), (2.0, 90.0)])}}),
    }
    return with_nodes(nodes, BIND={})


def midroot(mid):
    """Hips (T.y) -> mid (MOVE) -> LowerTorso (R.y)."""
    nodes = {
        "Hips": (99, 0, "LimbNode", (0, 0, 0), (0, 0, 0), (0, 0, 0), 0, {
            "T": {"y": Curve([(1.0, 0.0), (2.0, 10.0)])}}),
        mid: (100, 99, "LimbNode", (0, 0, 0), (0, 0, 0), (0, 0, 0), 0, MOVE),
        "LowerTorso": (101, 100, "LimbNode", (0, 100, 0), (0, 0, 0), (0, 0, 0), 0, {
            "R": {"y": Curve([(1.0, 0.0), (2.0, 90.0)])}}),
    }
    return with_nodes(nodes, BIND={})


def restrot(order=0, active=False, curve_default=None, anim=True):
    """Hips (T.y) -> A (T (0,100,0), static R (0,0,10), R.x curve) -> B (R.y). order / active: A's
    RotationOrder / RotationActive; curve_default: A's curve node d|X/Y/Z (default: the static)."""
    nodes = {
        "Hips": (99, 0, "LimbNode", (0, 0, 0), (0, 0, 0), (0, 0, 0), 0, {
            "T": {"y": Curve([(1.0, 0.0), (2.0, 10.0)])}}),
        "A": (100, 99, "LimbNode", (0, 100, 0), (0, 0, 10), (0, 0, 0), order,
              {"R": {"x": Curve([(1.0, 0.0), (2.0, 60.0)])}} if anim else {}),
        "B": (101, 100, "LimbNode", (0, 50, 0), (0, 0, 0), (0, 0, 0), 0, {
            "R": {"y": Curve([(1.0, 0.0), (2.0, 90.0)])}}),
    }
    s = with_nodes(nodes, BIND={})
    for o in top(s, "Objects").kids:
        if o.name == "Model" and bytes(o.props[1]).startswith(b"A\x00"):
            props = o.kids[1].kids
            if active:
                props.append(P("RotationActive", "bool", "", "", I(1)))
        if o.name == "AnimationCurveNode" and curve_default is not None and bytes(o.props[1]).startswith(b"R\x00"):
            pass
    if curve_default is not None:
        # A's rotation curve node is the first "R" curve node after Hips' "T".
        cns = [o for o in top(s, "Objects").kids if o.name == "AnimationCurveNode"]
        a_r = cns[1]
        a_r.kids = [p70(*[P("d|" + a.upper(), "Number", "", "A", D(curve_default["xyz".index(a)])) for a in "xyz"])]
    return s


def odd():
    """The base scene with LocalStart 1.01 s and LocalStop 2.05 s (not whole frames)."""
    s = with_nodes(BASE_NODES)
    for o in top(s, "Objects").kids:
        if o.name == "AnimationStack":
            o.kids = [p70(P("LocalStart", "KTime", "Time", "", L(round(1.01 * TICKS))),
                          P("LocalStop", "KTime", "Time", "", L(round(2.05 * TICKS))))]
    return s


def noanim():
    s = with_nodes(BASE_NODES)
    drop = ("AnimationStack", "AnimationLayer", "AnimationCurveNode", "AnimationCurve")
    objs = top(s, "Objects")
    ids = {o.props[0] for o in objs.kids if o.name in drop}
    objs.kids = [o for o in objs.kids if o.name not in drop]
    conns = top(s, "Connections")
    conns.kids = [c for c in conns.kids if c.props[1] not in ids and c.props[2] not in ids]
    return s


def nobones():
    """Only Null nodes animated (no Skeleton attribute anywhere)."""
    nodes = {
        "Box": (100, 0, "Null", (0, 0, 0), (0, 0, 0), (0, 0, 0), 0, {"T": {"x": Curve([(1.0, 0.0), (2.0, 30.0)])}}),
        "Lid": (101, 100, "Null", (0, 10, 0), (0, 0, 0), (0, 0, 0), 0, {"R": {"x": Curve([(1.0, 0.0), (2.0, 90.0)])}}),
    }
    return with_nodes(nodes, BIND={})


def timing(take=None, span=None, stack=None, takes=True, stack_name=None, take_name=None):
    """The base scene with the Takes LocalTime, the GlobalSettings TimeSpan and the stack's
    LocalStart/LocalStop set separately (seconds), or the Takes section left out; names apart."""
    s = with_nodes(BASE_NODES)
    tk = lambda x: L(round(x * TICKS))
    for o in top(s, "Objects").kids:
        if o.name == "AnimationStack":
            if stack:
                o.kids = [p70(P("LocalStart", "KTime", "Time", "", tk(stack[0])),
                              P("LocalStop", "KTime", "Time", "", tk(stack[1])))]
            if stack_name:
                o.props[1] = cname(stack_name, "AnimStack")
    if span:
        set_gs(s, "TimeSpanStart", tk(span[0]))
        set_gs(s, "TimeSpanStop", tk(span[1]))
    t = top(s, "Takes")
    if take:
        t.kids[1].kids[1] = N("LocalTime", [tk(take[0]), tk(take[1])])
    if take_name:
        t.kids[0] = N("Current", [S(take_name.encode())])
        t.kids[1].props = [S(take_name.encode())]
    if not takes:
        s.remove(t)
    return s


def norange():
    s = with_nodes(BASE_NODES)
    for o in top(s, "Objects").kids:
        if o.name == "AnimationStack":
            o.kids = []
    return s


def mesh():
    return [N("Objects", [], [N("Model", [L(1), cname("Crate", "Model"), S(b"Null")])]),
            N("Connections", [], [N("C", [S(b"OO"), L(1), L(0)])])]


# ---- ASCII FBX -------------------------------------------------------------------------------------

def a_val(p):
    if isinstance(p, S):
        b = bytes(p)
        if b"\x00\x01" in b:
            name, cls = b.split(b"\x00\x01", 1)
            b = cls + b"::" + name
        return '"' + b.decode("latin-1").replace('"', "&quot;") + '"'
    if isinstance(p, mf.R):
        return '"' + bytes(p).hex() + '"'
    if isinstance(p, (L, I)):
        return str(int(p))
    if isinstance(p, D):
        return repr(float(p))
    raise TypeError(p)


def a_node(n, ind):
    pad = "\t" * ind
    arr = [p for p in n.props if isinstance(p, Arr)]
    if arr:
        a = arr[0]
        vals = ",".join(repr(float(v)) if a.code in "df" else str(int(v)) for v in a.values)
        return f"{pad}{n.name}: *{len(a.values)} {{\n{pad}\ta: {vals}\n{pad}}}\n"
    head = f"{pad}{n.name}: " + ", ".join(a_val(p) for p in n.props)
    if not n.kids:
        return head.rstrip() + ("" if n.props else " {\n" + pad + "}") + "\n"
    return head + " {\n" + "".join(a_node(k, ind + 1) for k in n.kids) + pad + "}\n"


def write_ascii(scene):
    out = "; FBX 7.4.0 project file\n; rustudio fixture generator\n\n"
    for n in mf.complete(scene):
        if n.name in ("FileId", "CreationTime", "Creator"):
            continue
        out += a_node(n, 0) + "\n"
    return out.encode()


def main():
    out = sys.argv[1] if len(sys.argv) > 1 else os.path.join(HERE, "variants")
    os.makedirs(out, exist_ok=True)
    files = {
        "wave.fbx": mf.write_fbx(with_nodes(BASE_NODES)),
        "ns.fbx": mf.write_fbx(renamed(with_nodes(BASE_NODES), {
            "LowerTorso": "mixamorig:LowerTorso", "UpperTorso": "a:b:UpperTorso",
            "RightUpperArm": "rightupperarm"})),
        "stacks.fbx": mf.write_fbx(stacks()),
        "layers.fbx": mf.write_fbx(layers()),
        "meters.fbx": mf.write_fbx(with_nodes(BASE_NODES, UNIT=100.0)),
        "scale.fbx": mf.write_fbx(scale()),
        "r6.fbx": mf.write_fbx(r6()),
        "root.fbx": mf.write_fbx(root()),
        "mesh.fbx": mf.write_fbx(mesh()),
        "gaps.fbx": mf.write_fbx(gaps()),
        "norange.fbx": mf.write_fbx(norange()),
        "hrpname.fbx": mf.write_fbx(renamed(root(), {"HumanoidRootPart": "Root"})),
        "rstatic.fbx": mf.write_fbx(rootvar("Root", {})),
        "rmove.fbx": mf.write_fbx(rootvar("Root", MOVE, child_anim={"T": {"x": Curve([(1.0, 0.0), (2.0, 0.0)])}})),
        "hmove.fbx": mf.write_fbx(rootvar("HumanoidRootPart", MOVE,
                                          child_anim={"T": {"x": Curve([(1.0, 0.0), (2.0, 0.0)])}})),
        "rmovex.fbx": mf.write_fbx(rootvar("Root", MOVE, child="Torso",
                                           child_anim={"T": {"x": Curve([(1.0, 0.0), (2.0, 0.0)])}})),
        "rhigh.fbx": mf.write_fbx(rootvar("Root", MOVE, top_t=(0, 100, 0))),
        "rrest.fbx": mf.write_fbx(rootvar2()),
        "r2kids.fbx": mf.write_fbx(r2kids()),
        "hstatic.fbx": mf.write_fbx(rootvar("HumanoidRootPart", {}, top_t=(0, 37, 0))),
        **{f"name_{k}.fbx": mf.write_fbx(rootvar(n, MOVE)) for k, n in [
            ("lroot", "root"), ("uroot", "ROOT"), ("rootnode", "RootNode"), ("root001", "Root.001"),
            ("nsroot", "mixamorig:Root"), ("armature", "Armature"), ("hrpl", "humanoidrootpart"),
            ("hrp", "HumanoidRootPart")]},
        "midroot.fbx": mf.write_fbx(midroot("Root")),
        "midhrp.fbx": mf.write_fbx(midroot("HumanoidRootPart")),
        "rr_plain.fbx": mf.write_fbx(restrot()),
        "rr_order.fbx": mf.write_fbx(restrot(order=5)),
        "rr_active.fbx": mf.write_fbx(restrot(order=5, active=True)),
        "rr_cn0.fbx": mf.write_fbx(restrot(curve_default=(0, 0, 0))),
        "rr_static.fbx": mf.write_fbx(restrot(anim=False)),
        "odd.fbx": mf.write_fbx(odd()),
        "noanim.fbx": mf.write_fbx(noanim()),
        "nobones.fbx": mf.write_fbx(nobones()),
        "t_take.fbx": mf.write_fbx(timing(take=(1.0, 1.5))),
        "t_span.fbx": mf.write_fbx(timing(span=(1.0, 1.5))),
        "t_notakes.fbx": mf.write_fbx(timing(stack=(1.0, 1.5), takes=False)),
        "t_names.fbx": mf.write_fbx(timing(stack_name="StackName", take_name="TakeName")),
        "hoff.fbx": mf.write_fbx(rootvar("HumanoidRootPart", {"R": {"y": Curve([(1.0, 0.0), (2.0, 90.0)])}},
                                         top_t=(10, 50, 0))),
        "ascii.fbx": write_ascii(with_nodes(BASE_NODES)),
        "notes.txt": b"not a model\n",
    }
    z = with_nodes(BASE_NODES)
    set_gs(z, "UpAxis", I(2))
    set_gs(z, "FrontAxis", I(1))
    set_gs(z, "CoordAxis", I(0))
    files["zup.fbx"] = mf.write_fbx(z)
    f24 = with_nodes(BASE_NODES)
    set_gs(f24, "TimeMode", I(11))
    files["fps24.fbx"] = mf.write_fbx(f24)
    import lab  # the isolating files (lab.py imports this module)
    files.update(lab.files())
    for name, data in files.items():
        with open(os.path.join(out, name), "wb") as f:
            f.write(data)
    print("wrote", len(files), "files to", out)


if __name__ == "__main__":
    main()
