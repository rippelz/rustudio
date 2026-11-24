//! The Luau ↔ Instance/Enum bridge as scripts observe it (Studio probes;
//! crates/rustudio-engine/src/cmeta.rs): metatables, namecall, method functions, Lua-written
//! methods that yield, Enum types and the index/newindex errors.

use crate::runtime::{RunMode, Runtime, Side, World};
use rbx_dom_weak::InstanceBuilder;
use rustudio_core::doc::Document;

/// Runs `src` as a client LocalScript for `frames` steps; Output lines
/// without their location suffix.
fn run(src: &str, frames: usize) -> Vec<String> {
    let mut d = Document::new_place();
    let sp = d.find_child(d.dom.root_ref(), "StarterPlayer").unwrap();
    let sps = d.find_child(sp, "StarterPlayerScripts").unwrap();
    let s = d.dom.insert(
        sps,
        InstanceBuilder::new("LocalScript").with_name("T").with_property("Source", src.to_string()),
    );
    let rt = Runtime::new().unwrap();
    let mut w = World::new(d, RunMode::Run);
    w.side = Side::Client;
    rt.enter(w);
    rt.bind_globals().unwrap();
    rt.run_script(s);
    for _ in 0..frames {
        rt.step(1.0 / 60.0);
    }
    rt.leave()
        .output
        .iter()
        .map(|(_, t)| t.split("  —").next().unwrap_or("").to_string())
        .collect()
}

fn expect(text: &[String], wants: &[&str]) {
    for want in wants {
        assert!(text.iter().any(|t| t == want), "{want:?} not in {text:#?}");
    }
}

#[test]
fn bridge_signal_connect() {
    let text = run(
        r#"
        local p = Instance.new("Part")
        local n = 0
        local c = p.Changed:Connect(function() n += 1 end)
        p.Name = "X"
        print("conn", typeof(c), tostring(c), c.Connected, n)
        c:Disconnect()
        print("disc", c.Connected)
        "#,
        2,
    );
    expect(&text, &["conn RBXScriptConnection Connection true 1", "disc false"]);
}

/// A Lua prelude for the tests: `e(f, ...)` is pcall's message with any
/// position as "LOC: ", `info(f)` debug.info's s/l/n/a.
const LIB: &str = r#"
local function e(f, ...)
    -- Called from a Lua function, as a script line would (pcall itself
    -- is C, and Luau's argument errors name their caller's position).
    local ok, m = pcall(function(...) return f(...) end, ...)
    if ok then return "ok" end
    return (string.gsub(tostring(m), "^[^:]*:%d+: ", "LOC: "))
end
local function info(f)
    local s, l, n, a, v = debug.info(f, "slna")
    return table.concat({ s, l, n, a, tostring(v) }, " ")
end
local function raiser(f)
    local m
    xpcall(f, function() m = debug.info(2, "f") end)
    return m
end
"#;

fn run_lib(src: &str, frames: usize) -> Vec<String> {
    run(&format!("{LIB}\n{src}"), frames)
}

/// Studio probes 1: every bridge object's metatable is locked.
#[test]
fn bridge_metatables_locked() {
    let text = run_lib(
        r#"
        local p = Instance.new("Part")
        local c = p.Changed:Connect(function() end)
        for _, o in { game, p, p.Changed, c, Enum, Enum.Material, Enum.Material.Plastic } do
            print("mt", typeof(o), getmetatable(o))
        end
        "#,
        1,
    );
    for t in ["Instance", "RBXScriptSignal", "RBXScriptConnection", "Enums", "Enum", "EnumItem"] {
        expect(&text, &[&format!("mt {t} The metatable is locked")]);
    }
}

/// Studio probes 2: the Instance __namecall is its own unnamed C function,
/// methods only; argument checks as Luau's.
#[test]
fn bridge_namecall() {
    let text = run_lib(
        r#"
        local p = Instance.new("Part")
        local idx = raiser(function() return p.____________ end)
        local nc = raiser(function() p:____________() end)
        print("nc", info(nc), nc ~= idx, raiser(function() game:XYZ() end) == nc)
        print("empty", e(nc))
        print("self", e(nc, p))
        print("prop", e(function() workspace:Name() end))
        print("event", e(function() p:Changed() end))
        print("case", e(function() game:getservice("Players") end))
        print("child", e(function() local f = Instance.new("Folder"); Instance.new("Folder", f).Name = "K"; f:K() end))
        print("ok", game:GetService("Players").ClassName, p:IsA("BasePart"), workspace:FindFirstChild("nope"))
        print("game", e(function() game:XYZ() end))
        "#,
        1,
    );
    expect(
        &text,
        &[
            "nc [C] -1  0 true true true",
            "empty LOC: missing argument #1 (Instance expected)",
            // The VM's last namecall atom (game:XYZ() above), as Studio's.
            "self XYZ is not a valid member of Part \"Part\"",
            "prop Name is not a valid member of Workspace \"Workspace\"",
            "event Changed is not a valid member of Part \"Part\"",
            "case getservice is not a valid member of DataModel \"Game\"",
            "child K is not a valid member of Folder \"Folder\"",
            "ok Players true nil",
            "game XYZ is not a valid member of DataModel \"Game\"",
        ],
    );
}

/// Studio probes 3: method functions are named vararg C functions, one per
/// descriptor; bad self is "Expected ':' not '.'"; GetService converts.
#[test]
fn bridge_method_functions() {
    let text = run_lib(
        r#"
        local p = Instance.new("Part")
        print("info", info(game.GetService), info(p.WaitForChild))
        print("id", game.GetService == game.GetService, p.IsA == game.IsA, p.FindFirstChild == workspace.FindFirstChild)
        print("noself", e(game.GetService))
        print("wrongself", e(game.GetService, {}))
        print("wrongclass", e(game.GetService, workspace))
        print("args", e(game.GetService, game, {}), e(game.GetService, game, true), e(game.GetService, game, 3))
        print("missing", e(game.GetService, game))
        print("abstract", game:GetService("WorldRoot"))
        "#,
        1,
    );
    expect(
        &text,
        &[
            "info [C] -1 GetService 0 true [C] -1 WaitForChild 0 true",
            "id true true true",
            "noself Expected ':' not '.' calling member function GetService",
            "wrongself Expected ':' not '.' calling member function GetService",
            "wrongclass Expected ':' not '.' calling member function GetService",
            "args '{}' is not a valid Service name 'true' is not a valid Service name '3' is not a valid Service name",
            "missing Argument 1 missing or nil",
            "abstract nil",
        ],
    );
}

/// Studio probes 6: Lua-written methods are named C functions that check
/// self first and still yield, called as methods, as functions, inside
/// pcall and inside coroutines.
#[test]
fn bridge_lua_methods_yield() {
    let text = run_lib(
        r#"
        local f = Instance.new("Folder", workspace)
        f.Name = "F"
        task.delay(0.05, function() Instance.new("Part", f).Name = "Late" end)
        print("wfc", f:WaitForChild("Late").Name)
        local bf = Instance.new("BindableFunction")
        bf.OnInvoke = function(x) task.wait(0.02) return x * 2 end
        print("invoke", bf:Invoke(21), bf.Invoke(bf, 4))
        print("pcall", pcall(bf.Invoke, bf, 5))
        -- A coroutine of the script's own, which the wait yields: the
        -- scheduler resumes it through the method's continuation.
        local co = coroutine.create(function() print("co", f:WaitForChild("Late2").Name, f.WaitForChild(f, "Late2").Name) end)
        print("resume", coroutine.resume(co), coroutine.status(co))
        task.delay(0.05, function() Instance.new("Part", f).Name = "Late2" end)
        task.wait(0.2)
        print("timeout", f:WaitForChild("Never", 0.05))
        print("noself", e(f.WaitForChild), e(f.WaitForChild, {}))
        print("noname", e(f.WaitForChild, f), e(function() f:WaitForChild() end))
        print("rf", e(Instance.new("RemoteFunction").InvokeServer, workspace))
        print("info", info(bf.Invoke), info(Instance.new("RemoteFunction").InvokeServer))
        local sig = f.ChildAdded
        print("sig", info(sig.Connect), sig.Connect == sig.Connect)
        print("sigself", e(sig.Connect), e(sig.Connect, {}), e(sig.Connect, workspace))
        task.delay(0.05, function() Instance.new("Part", f) end)
        print("wait", sig:Wait().ClassName)
        print("done")
        "#,
        60,
    );
    expect(
        &text,
        &[
            "wfc Late",
            "invoke 42 8",
            "pcall true 10",
            "resume true suspended",
            "co Late2 Late2",
            "timeout nil",
            "noself Expected ':' not '.' calling member function WaitForChild Expected ':' not '.' calling member function WaitForChild",
            "noname Argument 1 missing or nil Argument 1 missing or nil",
            "rf Expected ':' not '.' calling member function InvokeServer",
            "info [C] -1 Invoke 0 true [C] -1 InvokeServer 0 true",
            "sig [C] -1 Connect 0 true false",
            "sigself LOC: missing argument #1 to 'Connect' (RBXScriptSignal expected) LOC: invalid argument #1 to 'Connect' (RBXScriptSignal expected, got table) LOC: invalid argument #1 to 'Connect' (RBXScriptSignal expected, got Instance)",
            "wait Part",
            "done",
        ],
    );
}

/// Studio probes 5: Enums, Enum and EnumItem.
#[test]
fn bridge_enums() {
    let text = run_lib(
        r#"
        local m, it = Enum.Material, Enum.Material.Plastic
        print("str", tostring(Enum), tostring(m), tostring(it))
        print("get", #Enum:GetEnums() > 600, info(Enum.GetEnums), Enum.GetEnums == Enum.GetEnums)
        print("from", m:FromName("Plastic"), m:FromValue(256), m:FromName("Nope"), m:FromValue(-3))
        print("eq", Enum.Material == Enum.Material, it == Enum.Material.Plastic, it == Enum.Material.Wood)
        print("cmp", e(function() return it < Enum.Material.Wood end))
        print("fields", it.Name, it.Value, it.EnumType == m)
        print("wname", e(function() it.Name = 3 end), e(function() it.XYZ = 3 end))
        print("rxyz", e(function() return it.XYZ end), e(function() return it[1] end))
        print("enum", e(function() return m.Nope end), e(function() return Enum.Nope end))
        print("items", #m:GetEnumItems(), m:GetEnumItems()[1] == it)
        print("self", e(m.FromName), e(Enum.GetEnums, {}), e(it.IsA, workspace))
        print("isa", it:IsA("Material"), it:IsA("KeyCode"))
        "#,
        1,
    );
    expect(
        &text,
        &[
            "str Enums Material Enum.Material.Plastic",
            // (`Enum.GetEnums` written out twice is one import, resolved when
            // the script loads: the same function, as in Studio 0.748 in a
            // ModuleScript. A fresh closure per read shows through a local:
            // `bridge_imports`.)
            "get true [C] -1 GetEnums 0 true true",
            "from Enum.Material.Plastic Enum.Material.Plastic nil nil",
            "eq true true false",
            "cmp LOC: attempt to compare EnumItem < EnumItem",
            "fields Plastic 256 true",
            "wname Name is not a valid member of EnumItem XYZ is not a valid member of EnumItem",
            "rxyz XYZ is not a valid member of \"Enum.Material.Plastic\" 1 is not a valid member of \"Enum.Material.Plastic\"",
            "enum Nope is not a valid member of \"Enum.Material\" Nope is not a valid member of \"Enum\"",
            "items 45 true",
            "self LOC: missing argument #1 to 'FromName' (Enum expected) LOC: invalid argument #1 to 'GetEnums' (Enums expected, got table) LOC: invalid argument #1 to 'IsA' (EnumItem expected, got Instance)",
            "isa true false",
        ],
    );
}

/// Studio probes 4: index/newindex argument and assignment errors.
#[test]
fn bridge_index_errors() {
    let text = run_lib(
        r#"
        local p = Instance.new("Part")
        local idx = raiser(function() return p.____________ end)
        local nidx = raiser(function() p.____________ = 1 end)
        print("i", e(idx), e(idx, p), e(idx, p, {}), e(idx, p, nil), e(idx, {}, "Name"))
        print("n", e(nidx), e(nidx, p), e(nidx, p, {}), e(function() p[nil] = 1 end))
        print("num", e(function() return p[1] end))
        print("ro", e(function() p.ClassName = "X" end))
        print("name", e(function() p.Name = {} end))
        print("parent", e(function() p.Parent = 3 end))
        print("anch", e(function() p.Anchored = {} end), p.Anchored)
        local c = p.Changed:Connect(function() end)
        print("conn", e(function() return c.Nope end), e(function() c.Nope = 1 end))
        print("sig", e(function() return p.Changed.Nope end), e(function() p.Changed.Nope = 1 end))
        "#,
        1,
    );
    expect(
        &text,
        &[
            "i LOC: missing argument #1 (Instance expected) LOC: missing argument #2 (string expected) LOC: invalid argument #2 (string expected, got table) LOC: invalid argument #2 (string expected, got nil) LOC: invalid argument #1 (Instance expected, got table)",
            "n LOC: missing argument #1 (Instance expected) LOC: missing argument #2 (string expected) LOC: invalid argument #2 (string expected, got table) LOC: invalid argument #2 (string expected, got nil)",
            "num 1 is not a valid member of Part \"Part\"",
            "ro Unable to assign property ClassName. Property is read only",
            "name Unable to assign property Name. string expected, got table",
            "parent LOC: invalid argument #3 (Instance expected, got number)",
            "anch ok true",
            "conn Nope is not a valid member of RBXScriptConnection Nope is not a valid member of RBXScriptConnection",
            "sig Nope is not a valid member of RBXScriptSignal Nope is not a valid member of RBXScriptSignal",
        ],
    );
}

/// Errors inside a Lua-written method: a script pcall that catches one in
/// an OnInvoke (this crashed the resume when the method's C function
/// called into Lua nested), one that escapes through the method, and one
/// after a yield.
#[test]
fn bridge_lua_method_errors() {
    let text = run_lib(
        r#"
        local bf = Instance.new("BindableFunction")
        bf.OnInvoke = function(k)
            if k == "catch" then return pcall(function() error("x", 0) end) end
            if k == "nil" then local y = nil; return y.z end
            task.wait(0.02)
            error("late", 0)
        end
        print("catch", bf:Invoke("catch"))
        print("pcallerr", pcall(function() return pcall(error, "y", 0) end))
        print("escape", e(function() return bf:Invoke("nil") end))
        print("late", e(function() return bf:Invoke("late") end))
        print("after", bf.Invoke(bf, "catch"))
        "#,
        30,
    );
    expect(
        &text,
        &[
            "catch false x",
            "pcallerr true false y",
            "escape LOC: attempt to index nil with 'z'",
            "late late",
            "after false x",
        ],
    );
}

/// Studio probes RBXScriptSignal.values: every read of an event is a new
/// object (`==`, not rawequal, distinct table keys) and connections made
/// through any of them work.
#[test]
fn bridge_signal_reads_are_fresh() {
    let text = run(
        r#"
        local p = Instance.new("Part")
        local a, b = p.Changed, p.Changed
        local t = { [a] = 1 }
        local n = 0
        a:Connect(function() n += 1 end)
        b:Connect(function() n += 10 end)
        p.Name = "X"
        print("sig", a == b, rawequal(a, b), t[b], a == p.AncestryChanged, n)
        "#,
        2,
    );
    expect(&text, &["sig true false nil false 11"]);
}

/// Studio probes RaycastResult rows: read-only userdata, Studio's tostring,
/// `==` by content.
#[test]
fn bridge_raycast_result_userdata() {
    let text = run(
        r#"
        local part = Instance.new("Part")
        part.Anchored = true
        part.Position = Vector3.new(10000, 10000, 10000)
        part.Parent = workspace
        local function cast() return workspace:Raycast(Vector3.new(10000, 10010, 10000), Vector3.new(0, -20, 0)) end
        local r, r2 = cast(), cast()
        print("ray", type(r), typeof(r), tostring(r), getmetatable(r))
        print("ray2", r == r2, rawequal(r, r2), r.Instance == part, r.Distance, r.Material)
        print("ray3", select(2, pcall(function() r.Distance = 1 end)), select(2, pcall(function() return r.Nope end)))
        "#,
        2,
    );
    expect(
        &text,
        &[
            "ray userdata RaycastResult RaycastResult{Part @ 10000, 10000.5996, 10000; normal = 0, 1, 0; material = Plastic} The metatable is locked",
            "ray2 true false true 9.399999618530273 Enum.Material.Plastic",
            "ray3 Distance member of RaycastResult is read-only and cannot be assigned to 'Nope' is not a valid member of RaycastResult",
        ],
    );
}

/// Studio probes misc.stats / misc.UserSettings / misc.DebuggerManager.
#[test]
fn bridge_settings_globals_are_instances() {
    let text = run(
        r#"
        local us = UserSettings()
        local ugs = us:GetService("UserGameSettings")
        print("us", typeof(us), tostring(us), us.ClassName, us:IsA("Instance"), us == UserSettings())
        print("ugs", typeof(ugs), ugs.ClassName, ugs.Parent == us, us.GameSettings == ugs, ugs.MouseSensitivity)
        local changed
        ugs.Changed:Connect(function(k) changed = k end)
        ugs.MouseSensitivity = 1
        task.wait()
        print("ugs2", ugs.MouseSensitivity, changed, us:IsUserFeatureEnabled("UserNewPoppercam4"), select(2, pcall(us.IsUserFeatureEnabled, us, "Bogus")))
        print("stats", typeof(stats()), tostring(stats()), stats() == game:GetService("Stats"))
        print("dm", pcall(DebuggerManager))
        "#,
        3,
    );
    expect(
        &text,
        &[
            "us Instance GlobalBasicSettings UserSettings true true",
            "ugs Instance UserGameSettings true true 0.52",
            "ugs2 1 MouseSensitivity true Flag Bogus does not exist",
            "stats Instance Stats true",
            "dm false The current thread cannot call 'DebuggerManager' (lacking capability Plugin)",
        ],
    );
}

/// Global chains (`Enum.GetEnums`, `CFrame.new`) resolve once when a script
/// loads, through its environment (vendor lvmload.cpp resolveImportSafe);
/// reads through a local still call `__index` each time; chains through
/// the globals scripts change (`_G`, `shared`, `script`, `workspace`) stay
/// run-time reads. Studio 0.748, a ModuleScript required from the command
/// bar: `imp true | imp3 true | dyn false | fn true`, and `2 2 M2 W2` for
/// values changed after the module loaded.
#[test]
fn bridge_imports() {
    let mut d = Document::new_place();
    let sp = d.find_child(d.dom.root_ref(), "StarterPlayer").unwrap();
    let sps = d.find_child(sp, "StarterPlayerScripts").unwrap();
    let src = r#"
        _G.zzq = 1 shared.zzq = 1
        local m = script.M
        local g = require(m)
        _G.zzq = 2 shared.zzq = 2 m.Name = "M2" workspace.Name = "W2"
        print("mut", g())
        local o = Enum
        local function f() return Enum.GetEnums end
        print("imp", Enum.GetEnums == Enum.GetEnums, Enum.Material.FromName == Enum.Material.FromName,
            o.GetEnums == o.GetEnums, f() == f(), CFrame.new == CFrame.new)
    "#;
    let s = d.dom.insert(sps, InstanceBuilder::new("LocalScript").with_name("T").with_property("Source", src.to_string()));
    d.dom.insert(
        s,
        InstanceBuilder::new("ModuleScript").with_name("M").with_property(
            "Source",
            "return function() return _G.zzq, shared.zzq, script.Name, workspace.Name end".to_string(),
        ),
    );
    let rt = Runtime::new().unwrap();
    let mut w = World::new(d, RunMode::Run);
    w.side = Side::Client;
    rt.enter(w);
    rt.bind_globals().unwrap();
    rt.run_script(s);
    rt.step(1.0 / 60.0);
    let text: Vec<String> =
        rt.leave().output.iter().map(|(_, t)| t.split("  —").next().unwrap_or("").to_string()).collect();
    expect(&text, &["mut 2 2 M2 W2", "imp true true false true true"]);
}

/// Property writes answered from the typed fast path (runtime.rs
/// `typed_write`: numbers, booleans, Vector3, CFrame, Color3, UDim2…)
/// convert, clamp, orthonormalize and fire exactly as the general path;
/// the expected lines are the general path's (the build before it).
#[test]
fn bridge_typed_writes() {
    let text = run(
        r#"
local out = {}
local function o(...) local t = table.pack(...) for i = 1, t.n do t[i] = tostring(t[i]) end table.insert(out, table.concat(t, " ")) end
local p = Instance.new("Part") p.Anchored = true p.Parent = workspace
p.CFrame = CFrame.new(1, 2, 3) * CFrame.Angles(0, 0.5, 0)
p.Position = Vector3.new(4, 5, 6)
o("pos", p.Position, p.CFrame)
local n, names = 0, {}
p.Changed:Connect(function(k) n += 1 table.insert(names, k) end)
local pc = 0
p:GetPropertyChangedSignal("CFrame"):Connect(function() pc += 1 end)
p.Transparency = 0.5 p.Transparency = 0.5
p.Color = Color3.new(1, 0, 0) p.Color = Color3.new(1, 0, 0)
p.Position = Vector3.new(7, 8, 9)
p.CFrame = CFrame.new(7, 8, 9) * CFrame.Angles(0, 0.5, 0)
p.CFrame = CFrame.new(1, 1, 1)
p.Anchored = false p.Anchored = true
p.Size = Vector3.new(2, 2, 2)
task.wait()
o("changed", n, pc, table.concat(names, ","))
o("color", p.Color, p.BrickColor, p.Transparency)
local l = Instance.new("ImageLabel")
l.ImageColor3 = Color3.new(0.5, 0.25, 1)
l.Size = UDim2.new(0.5, 10, 0, 20)
l.Position = UDim2.fromOffset(3, 4)
l.Visible = false
l.ImageTransparency = 0.25
o("gui", l.ImageColor3, l.Size, l.Position, l.Visible, l.ImageTransparency)
local b = Instance.new("Bone") b.Parent = p
b.CFrame = CFrame.new(1, 2, 3, 2, 0, 0, 0, 1, 0, 0, 0, 1)
o("ortho", b.CFrame)
b.Transform = CFrame.Angles(0.1, 0, 0)
o("transform", b.Transform)
local m6 = Instance.new("Motor6D") m6.Parent = p
m6.C0 = CFrame.new(0, 1, 0) m6.Transform = CFrame.Angles(0, 0, 0.2)
o("motor", m6.C0, m6.Transform)
local nv = Instance.new("NumberValue") nv.Value = 3 local iv = Instance.new("IntValue") iv.Value = 7.9
local vv = Instance.new("Vector3Value") vv.Value = Vector3.new(1, 2, 3)
o("values", nv.Value, iv.Value, vv.Value)
local ok, e = pcall(function() p.Size = CFrame.new() end)
o("err", ok, e)
ok, e = pcall(function() l.Size = 5 end)
o("err2", ok, e)
for _, l in out do print(l) end

        "#,
        3,
    );
    expect(
        &text,
        &[
            r#"pos 4, 5, 6 4, 5, 6, 0.87758255, 0, 0.47942555, 0, 1, -0, -0.47942555, 0, 0.87758255"#,
            r#"changed 10 2 Transparency,Color,CFrame,Position,CFrame,Anchored,Anchored,Size,AssemblyMass,Mass"#,
            r#"color 1, 0, 0 Really red 0.5"#,
            r#"gui 0.5, 0.25, 1 {0.5, 10}, {0, 20} {0, 3}, {0, 4} false 0.25"#,
            r#"ortho 1, 2, 3, 1, 0, 0, 0, 1, 0, 0, 0, 1"#,
            r#"transform 0, 0, 0, 1, -0, 0, 0, 0.995004177, -0.0998334214, 0, 0.0998334214, 0.995004177"#,
            r#"motor 0, 1, 0, 1, 0, 0, 0, 1, 0, 0, 0, 1 0, 0, 0, 0.980066597, -0.198669329, 0, 0.198669329, 0.980066597, -0, 0, 0, 1"#,
            r#"values 3 7 1, 2, 3"#,
            r#"err false Unable to assign property Size. Vector3 expected, got CFrame"#,
            r#"err2 false Unable to assign property Size. UDim2 expected, got number"#,
        ],
    );
}

/// CFrames are tagged Luau userdata (ltypes.rs `CFRAME_TAG`), not mlua
/// userdata: the same typeof, equality, identity as a table key, members,
/// operators, argument errors, attributes and values as before.
#[test]
fn bridge_native_cframe() {
    let text = run(
        r#"
local out = {}
local function o(...) local t = table.pack(...) for i = 1, t.n do t[i] = tostring(t[i]) end table.insert(out, table.concat(t, " ")) end
local a, b = CFrame.new(1, 2, 3), CFrame.new(1, 2, 3)
o("type", typeof(a), type(a), a == b, rawequal(a, b), a ~= CFrame.new(), getmetatable(a))
local t = {} t[a] = 1 t[b] = 2 local n = 0 for _ in t do n += 1 end o("keys", n)
o("ops", a * b, a * Vector3.new(1, 0, 0), a + Vector3.new(1, 1, 1), a - Vector3.new(1, 1, 1), a:Inverse())
o("members", a.Position, a.X, a.LookVector, a.Rotation, a:GetComponents())
o("lerp", a:Lerp(CFrame.Angles(0, 1, 0), 0.5), a:ToObjectSpace(b), a:PointToWorldSpace(Vector3.one))
local function e(f) local ok, m = pcall(f) return (tostring(m):gsub("^[^:]*:%d+: ", "")) end
o("err", e(function() return a:Lerp(1, 0.5) end), e(function() return a.Lerp(nil, a, 0.5) end), e(function() return a * "x" end), e(function() return a.Nope end), e(function() a.X = 1 end))
o("err2", e(function() return a:Lerp(a) end), e(function() return CFrame.new("a") end), e(function() return a:ToWorldSpace(5) end))
local p = Instance.new("Part")
p:SetAttribute("C", a)
o("attr", typeof(p:GetAttribute("C")), p:GetAttribute("C") == a)
local v = Instance.new("CFrameValue") v.Value = a * CFrame.Angles(0.1, 0.2, 0.3)
o("value", v.Value)
o("euler", a:ToEulerAnglesXYZ()) o("orient", CFrame.Angles(0.1, 0.2, 0.3):ToOrientation()) o("axis", CFrame.Angles(0.1, 0.2, 0.3):ToAxisAngle())
for _, l in out do print(l) end

        "#,
        1,
    );
    expect(
        &text,
        &[
            r#"type CFrame userdata true false true The metatable is locked"#,
            r#"keys 2"#,
            r#"ops 2, 4, 6, 1, 0, 0, 0, 1, 0, 0, 0, 1 2, 2, 3 2, 3, 4, 1, 0, 0, 0, 1, 0, 0, 0, 1 0, 1, 2, 1, 0, 0, 0, 1, 0, 0, 0, 1 -1, -2, -3, 1, 0, 0, 0, 1, 0, 0, 0, 1"#,
            r#"members 1, 2, 3 1 -0, -0, -1 0, 0, 0, 1, 0, 0, 0, 1, 0, 0, 0, 1 1 2 3 1 0 0 0 1 0 0 0 1"#,
            r#"err invalid argument #2 to 'Lerp' (CFrame expected, got number) Argument 1 missing or nil invalid argument #2 (Vector3 expected, got string) Nope is not a valid member of CFrame X member of CFrame is read-only and cannot be assigned to"#,
            r#"err2 missing argument #3 to 'Lerp' (number expected) invalid argument #1 to 'new' (Vector3 expected, got string) invalid argument #2 to 'ToWorldSpace' (CFrame expected, got number)"#,
            r#"attr CFrame true"#,
            r#"value 1, 2, 3, 0.936293423, -0.289629489, 0.198669359, 0.312991828, 0.944702566, -0.0978433937, -0.15934509, 0.153791994, 0.975170493"#,
            r#"euler -0 0 -0"#,
            r#"orient 0.0980001837015152 0.2009774148464203 0.319930762052536"#,
            r#"axis 0.3378807008266449, 0.4807199537754059, 0.8091631531715393 0.381564736366272"#,
        ],
    );
    // Studio's value (x86). An aarch64 build fuses the slerp's multiply-adds,
    // which leaves sin(0.5) one f32 step higher (0.479425579).
    let lerp = r#"lerp 0.5, 1, 1.5, 0.87758255, 0, 0.47942555, 0, 1, 0, -0.47942555, 0, 0.87758255 0, 0, 0, 1, 0, 0, 0, 1, 0, 0, 0, 1 2, 3, 4"#;
    let fused = lerp.replace("0.47942555", "0.479425579");
    assert!(
        text.iter().any(|t| t == lerp || (cfg!(target_arch = "aarch64") && *t == fused)),
        "{lerp:?} not in {text:#?}"
    );
}

/// A --!native script runs as native code (vendor mlua: native modules
/// only, as Roblox) with the interpreter's results, errors and positions.
#[test]
fn bridge_native_module() {
    let text = run(
        r#"--!native
        local function f(n)
            local s, v = 0, Vector3.new(1, 2, 3)
            for i = 1, n do s += math.sin(i * 0.01) * (i % 7) v = v * 0.5 + Vector3.yAxis end
            return s, v
        end
        local s, v = f(1000)
        print("native", string.format("%.6f", s), v, CFrame.new(1, 2, 3) * CFrame.Angles(0, 0.5, 0))
        print("err", select(2, pcall(function() local t = nil return t.x end)))
        print("err2", select(2, pcall(function() return 1 + {} end)))
        "#,
        1,
    );
    expect(
        &text,
        &[
            // (The interpreter's output too: RUSTUDIO_NO_NATIVE=1.)
            "native 548.727801 0, 2, 0 1, 2, 3, 0.87758255, 0, 0.47942555, 0, 1, 0, -0.47942555, 0, 0.87758255",
            "err StarterPlayer.StarterPlayerScripts.T:9: attempt to index nil with 'x'",
            "err2 StarterPlayer.StarterPlayerScripts.T:10: attempt to perform arithmetic (add) on number and table",
        ],
    );
}

/// Color3.fromRGB, UDim2.new / fromScale / fromOffset and Vector2.new on
/// plain numbers (dtmeta `small_ctor_fast`) and Color3 / Vector2 component
/// reads give what the implementations give (UDim2.new(1, 2) takes two
/// UDims: zeros; fromOffset truncates toward zero).
#[test]
fn bridge_small_constructors() {
    let text = run(
        r#"
        print("vals", UDim2.new(0.5, 10.7, 1, -3), UDim2.new(1, 2), UDim2.fromOffset(2.9, -1.5), UDim2.fromScale(0.25),
            Color3.fromRGB(255, 128, 0), Vector2.new(1), Color3.new(0.1, 0.2, 0.3).G, Vector2.new(3, 4).Y, UDim2.new(), Vector2.new())
        print("types", typeof(UDim2.new(0, 1, 0, 1)), typeof(Color3.fromRGB(1, 2, 3)), typeof(Vector2.new(1, 2)), UDim2.new("1", 2, 3, 4))
        local a, b = Vector2.new(1/3, 2/3), Vector2.new(3, 5)
        print("v2", a + b, a - b, a * b, a * 2.5, 2.5 * a, a / 7, a / b, -a, a // 0.1)
        "#,
        1,
    );
    expect(
        &text,
        &[
            "vals {0.5, 10}, {1, -3} {0, 0}, {0, 0} {0, 2}, {0, -1} {0.25, 0}, {0, 0} 1, 0.501961, 0 1, 0 0.20000000298023224 4 {0, 0}, {0, 0} 0, 0",
            "types UDim2 Color3 Vector2 {1, 2}, {3, 4}",
            "v2 3.33333325, 5.66666651 -2.66666675, -4.33333349 1, 3.33333349 0.833333373, 1.66666675 0.833333373, 1.66666675 0.0476190522, 0.0952381045 0.111111112, 0.13333334 -0.333333343, -0.666666687 3, 6",
        ],
    );
}
