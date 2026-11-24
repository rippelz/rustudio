use super::*;
use crate::runtime::{RunMode, Runtime, World};
use rustudio_core::log::Level;
use rbx_dom_weak::InstanceBuilder;
use rustudio_core::doc::Document;

/// Aborts the test process if a run takes over 90 s (a hang must never
/// hold a shared build lock); disarmed when dropped.
struct Watchdog(Option<std::sync::mpsc::Sender<()>>);

impl Watchdog {
    fn arm(name: &str) -> Watchdog {
        let (tx, rx) = std::sync::mpsc::channel::<()>();
        let name = name.to_string();
        std::thread::spawn(move || {
            if let Err(std::sync::mpsc::RecvTimeoutError::Timeout) = rx.recv_timeout(std::time::Duration::from_secs(90)) {
                eprintln!("script_profiler test run {name:?} hung for 90 s: aborting");
                std::process::abort();
            }
        });
        Watchdog(Some(tx))
    }
}

impl Drop for Watchdog {
    fn drop(&mut self) {
        if let Some(tx) = self.0.take() {
            let _ = tx.send(());
        }
    }
}

/// Runs `src` as a server Script named `name` for `frames` steps.
fn run(name: &str, src: &str, frames: usize) -> Vec<(Level, String)> {
    let _watchdog = Watchdog::arm(name);
    let mut d = Document::new_place();
    let root = d.dom.root_ref();
    let sss = d.find_child(root, "ServerScriptService").unwrap();
    let s = d.dom.insert(sss, InstanceBuilder::new("Script").with_name(name).with_property("Source", src.to_string()));
    let rt = Runtime::new().unwrap();
    rt.enter(World::new(d, RunMode::Run));
    rt.bind_globals().unwrap();
    rt.run_script(s);
    for _ in 0..frames {
        rt.step(1.0 / 60.0);
    }
    rt.leave().output
}

const HOT: &str = r#"
local function hot(n)
    local x = 0
    for i = 1, n do
        x = x + math.sin(i) * math.cos(i)
    end
    return x
end

local function middle(n)
    return hot(n)
end

local b = Instance.new("BindableEvent")
b.Event:Connect(function(n)
    debug.profilebegin("bind-work")
    middle(n)
    debug.profileend()
end)

-- (bounded by count too: never spin if the clock stalls)
local t0, n = os.clock(), 0
while os.clock() - t0 < 0.06 and n < 400 do
    middle(20000)
    n += 1
end
-- A C function calling back into Lua: sampled with the name its call
-- site used (table.sort, through GETIMPORT).
t0, n = os.clock(), 0
while os.clock() - t0 < 0.03 and n < 400 do
    n += 1
    local t = {}
    for i = 1, 300 do t[i] = (i * 7919) % 301 end
    table.sort(t, function(a, b) return math.sin(a) < math.sin(b) end)
end
for _ = 1, 3 do
    task.wait()
    b:Fire(50000)
end
print("DONE")
"#;

#[test]
fn records_scripts_functions_entries_markers_and_heap() {
    start_recording(false);
    set_sample_hz(4000);
    let out = run("ProfHot", HOT, 12);
    assert!(out.iter().any(|(_, s)| s.contains("DONE")), "{out:?}");
    let snap = snapshot();
    let row = snap
        .scripts
        .iter()
        .find(|s| s.path.ends_with("ServerScriptService.ProfHot"))
        .unwrap_or_else(|| panic!("no script row: {:#?}", snap.scripts));
    assert_eq!(row.vm, VmKind::Server);
    assert!(row.self_ms > 40.0, "{row:?}");
    assert!(row.resumes >= 4, "{row:?}");
    assert!(row.max_ms >= row.avg_ms);
    let kinds: Vec<EntryKind> = row.entries.iter().map(|e| e.kind).collect();
    assert!(kinds.contains(&EntryKind::Start), "{kinds:?}");
    assert!(kinds.contains(&EntryKind::Timer), "{kinds:?}");
    let ev = row.entries.iter().find(|e| e.kind == EntryKind::Event).expect("event entry");
    assert_eq!(ev.calls, 3);
    assert_eq!(ev.event.as_deref(), Some("Event"));
    // The handler resumes nest inside the timer resume: self excludes them.
    assert!(row.total_ms >= row.self_ms);

    // Sampled functions: `hot` with its defining line and a hot line in it.
    let hot = snap
        .functions
        .iter()
        .find(|f| f.script.ends_with("ProfHot") && &*f.name == "hot")
        .unwrap_or_else(|| panic!("no hot fn: {:#?}", &snap.functions[..snap.functions.len().min(10)]));
    assert_eq!(hot.line, 2);
    assert!(hot.self_samples > 10, "{hot:?}");
    assert!(hot.hot_lines.first().is_some_and(|(l, _)| (3..=7).contains(l)), "{hot:?}");
    let middle = snap.functions.iter().find(|f| f.script.ends_with("ProfHot") && &*f.name == "middle").unwrap();
    assert!(middle.total_samples >= hot.self_samples / 2, "{middle:?} vs {hot:?}");
    assert!(middle.total_ms >= middle.self_ms);

    // Tree: middle is a parent of hot under the ProfHot root.
    let i_hot = snap
        .tree
        .iter()
        .position(|n| n.script.ends_with("ProfHot") && &*n.name == "hot" && n.parent.is_some())
        .expect("hot in tree");
    let parent = snap.tree[snap.tree[i_hot].parent.unwrap() as usize].clone();
    assert_eq!(&*parent.name, "middle");

    let mark = snap.markers.iter().find(|m| &*m.label == "bind-work").expect("marker");
    assert_eq!(mark.calls, 3);
    assert!(mark.script.as_deref().is_some_and(|s| s.ends_with("ProfHot")), "{mark:?}");

    // A sampled C frame keeps the name its call site used.
    assert!(
        snap.functions.iter().any(|f| f.c && &*f.name == "table.sort" && f.total_samples > 0),
        "{:#?}",
        snap.functions.iter().filter(|f| f.c).collect::<Vec<_>>()
    );
    let folded = snap.folded();
    assert!(folded.lines().any(|l| l.contains("middle (") && l.contains(";hot (")), "{folded}");
}

#[test]
fn heap_is_polled_per_script() {
    set_live(true);
    // Poll every frame: a fast machine runs these 40 frames inside one 500 ms interval.
    super::HEAP_POLL_MS.store(0, std::sync::atomic::Ordering::Relaxed);
    let out = run(
        "ProfHeap",
        r#"
        _G.keep = {}
        for i = 1, 20000 do _G.keep[i] = {i, tostring(i)} end
        local keep2 = {}
        for i = 1, 20000 do keep2[i] = string.rep("x", 40) .. i end
        while true do task.wait() if #keep2 < 0 then break end end
        "#,
        40,
    );
    assert!(out.iter().all(|(l, _)| *l != Level::Error), "{out:?}");
    let snap = snapshot();
    let row = snap.scripts.iter().find(|s| s.path.ends_with("ProfHeap")).expect("row");
    assert!(row.heap_bytes > 500_000, "{row:?}");
    assert!(snap.vms.iter().any(|v| v.vm == VmKind::Server && v.heap_bytes >= row.heap_bytes));
}

#[test]
fn native_directive() {
    assert!(source_is_native("--!strict\n--!native\nlocal x = 1"));
    assert!(source_is_native("-- header\n--!native"));
    assert!(!source_is_native("local x = 1\n--!native"));
    assert!(!source_is_native("--!strict"));
}

#[test]
fn native_functions_are_flagged() {
    start_recording(false);
    set_sample_hz(4000);
    let out = run(
        "ProfNative",
        r#"--!native
-- (vararg: --!native compiles at -O2, which would inline a small function)
local function crunch(n, ...)
    local x = 0
    for i = 1, n do x = x + (i % 7) * 0.5 end
    return x
end
-- Deferred --!native codegen drains at the start of the first Runtime::step.
task.wait()
local t0 = os.clock()
local n = 0
while os.clock() - t0 < 0.05 and n < 400 do crunch(100000) n += 1 end
print("NDONE")
"#,
        3,
    );
    assert!(out.iter().any(|(_, s)| s.contains("NDONE")), "{out:?}");
    let snap = snapshot();
    let row = snap.scripts.iter().find(|s| s.path.ends_with("ProfNative")).expect("row");
    assert!(row.native);
    let f = snap.functions.iter().find(|f| f.script.ends_with("ProfNative") && &*f.name == "crunch").unwrap_or_else(|| {
        panic!(
            "{:#?}\n{:#?}",
            snap.functions.iter().filter(|f| f.script.contains("ProfNative")).collect::<Vec<_>>(),
            snap.tree.iter().filter(|n| n.script.contains("ProfNative")).collect::<Vec<_>>()
        )
    });
    // Native code needs codegen support on this CPU (and no RUSTUDIO_NO_NATIVE).
    if std::env::var_os("RUSTUDIO_NO_NATIVE").is_none() {
        assert!(f.native, "{f:?}");
    }
}

#[test]
fn off_records_nothing_new() {
    // A script that never ran while profiling has no row.
    // (Tests run in parallel: only meaningful when nothing turned it on
    // before or during the run.)
    if on() {
        return;
    }
    run("ProfOff", "print(1)", 1);
    if on() {
        return;
    }
    assert!(!snapshot().scripts.iter().any(|s| s.path.ends_with("ProfOff")));
}

#[test]
fn exact_mode_counts_calls_and_times_functions() {
    set_exact(true);
    let out = run(
        "ProfExact",
        r#"
local function leaf(i)
    return math.sqrt(i) + 1
end
local function mid(n)
    local x = 0
    for i = 1, n do x = x + leaf(i) end
    return x
end
local part = Instance.new("Part")
for _ = 1, 10 do
    mid(100)
    part:FindFirstChild("Nope")
end
local ok = pcall(function() mid(5) error("boom") end)
task.wait()
mid(7)
print("XDONE", ok)
"#,
        3,
    );
    assert!(out.iter().any(|(_, s)| s.contains("XDONE")), "{out:?}");
    let snap = snapshot();
    assert!(snap.exact);
    let f = |name: &str| {
        snap.functions
            .iter()
            .find(|f| f.script.ends_with("ProfExact") && &*f.name == name)
            .unwrap_or_else(|| panic!("no {name}: {:#?}", snap.functions.iter().filter(|f| f.calls > 0).take(20).collect::<Vec<_>>()))
            .clone()
    };
    let mid = f("mid");
    assert_eq!(mid.calls, 12, "{mid:?}");
    let leaf = f("leaf");
    assert_eq!(leaf.calls, 1012, "{leaf:?}");
    assert!(mid.exact_total_ms >= leaf.exact_total_ms, "{mid:?} {leaf:?}");
    assert!(mid.exact_self_ms > 0.0 && mid.max_ms > 0.0);
    // The engine method through __namecall, by name.
    let ffc = snap.functions.iter().find(|f| f.c && &*f.name == "FindFirstChild" && f.vm == VmKind::Server);
    assert!(ffc.is_some_and(|f| f.calls >= 10), "{:#?}", snap.functions.iter().filter(|f| f.c).collect::<Vec<_>>());
    // Tree: leaf under mid under the script root.
    let i = snap.exact_tree.iter().position(|n| n.script.ends_with("ProfExact") && &*n.name == "leaf").unwrap();
    let p = &snap.exact_tree[snap.exact_tree[i].parent.unwrap() as usize];
    assert_eq!(&*p.name, "mid");
    set_exact(false);
}

#[test]
fn resumes_and_markers_are_script_spans_on_the_timeline() {
    use rustudio_core::profiler as p;
    p::enable();
    let out = run(
        "ProfTimeline",
        r#"
        for _ = 1, 3 do
            debug.profilebegin("tl-mark")
            local x = 0
            for i = 1, 20000 do x = x + i end
            debug.profileend()
            task.wait()
        end
        print("TLDONE")
        "#,
        6,
    );
    p::frame(p::FrameKind::Render);
    let cap = p::snapshot();
    p::disable();
    assert!(out.iter().any(|(_, s)| s.contains("TLDONE")), "{out:?}");
    let name_of = |n: u32| cap.names.get(n as usize).map(String::as_str).unwrap_or("");
    let spans: Vec<_> = cap.tracks.iter().flat_map(|t| t.spans.iter()).collect();
    let script = spans
        .iter()
        .filter(|s| s.cat == p::Cat::Script && name_of(s.name) == "ServerScriptService.ProfTimeline")
        .count();
    assert!(script >= 4, "{script} script spans");
    let marks: Vec<_> = spans.iter().filter(|s| name_of(s.name) == "tl-mark").collect();
    assert_eq!(marks.len(), 3);
    assert!(marks.iter().all(|m| m.cat == p::Cat::Script && m.depth >= 1));
    // The capture carries the script profile.
    assert!(cap.extensions.contains_key("scripts"));
}
