//! Opt-in live probe: which NewObject a Team Create server refuses (0x104,
//! "Error while receiving data"). Each case runs on a fresh connection:
//! join, make sure `ServerStorage.rustudio_tc_probe` exists, send the case's
//! creates into it, then watch the stream for 6 s. At the end the probe
//! folder is deleted and a last join checks it is gone. Nothing else in the
//! place is touched. Run it only against a place whose owner agreed.
//!
//!   cargo run --release -p rustudio-net --example tc_create_probe -- UNIVERSE PLACE [ACCOUNT_ID] [case,...]
use rustudio_net::live::{LiveEvent, LiveSession};
use rustudio_net::model::LiveModel;
use rustudio_net::replicator::PropValue as P;
use std::time::{Duration, Instant};

const FOLDER: &str = "rustudio_tc_probe";

type Creates = Vec<(&'static str, Vec<(String, P)>)>;

fn named(name: &str) -> (String, P) {
    ("Name".into(), P::String(name.into()))
}

fn surface(name: &str, extra: Vec<(&str, P)>) -> (&'static str, Vec<(String, P)>) {
    let mut props = vec![named(name)];
    props.extend(extra.into_iter().map(|(n, v)| (n.to_string(), v)));
    ("SurfaceAppearance", props)
}

/// The cases, each a list of creates under the probe folder.
fn cases() -> Vec<(&'static str, Creates)> {
    let source = |s: &str| ("Source".to_string(), P::String(s.into()));
    let local_ids = (1..=48).map(|i| {
        surface(
            "sa_burst",
            vec![
                ("AlphaMode", P::Enum(1)),
                ("ColorMapContent", P::String(format!("rbxassetid://90002346000000{i:02}"))),
                ("EmissiveStrength", P::Float(0.0)),
            ],
        )
    });
    vec![
        ("module", vec![("ModuleScript", vec![named("probe_module"), source("return 1\n")])]),
        ("folder", vec![("Folder", vec![named("probe_folder")])]),
        ("sa_name", vec![surface("sa_name", vec![])]),
        ("sa_alpha", vec![surface("sa_alpha", vec![("AlphaMode", P::Enum(1))])]),
        ("sa_emissive", vec![surface("sa_emissive", vec![("EmissiveStrength", P::Float(0.0))])]),
        ("sa_color_real", vec![surface("sa_color_real", vec![("ColorMapContent", P::String("rbxassetid://6372755229".into()))])]),
        ("sa_color_local", vec![surface("sa_color_local", vec![("ColorMapContent", P::String("rbxassetid://9000234600000009".into()))])]),
        ("localscript", vec![("LocalScript", vec![named("probe_local"), source("print(1)\n")])]),
        ("localscript_props", vec![("LocalScript", vec![named("probe_local_props"), source("print(1)\n"), ("Disabled".into(), P::Bool(false)), ("RunContext".into(), P::Enum(0))])]),
        ("sa_burst", local_ids.collect()),
    ]
}

struct Live {
    session: LiveSession,
    model: LiveModel,
}

/// Joins, retrying the server's 0x101 refusal (seen on the join right
/// after a session that sent edits).
fn join(universe: u64, place: u64, account: Option<u64>) -> Result<Live, String> {
    let mut last = String::new();
    for attempt in 0..3 {
        std::thread::sleep(Duration::from_secs(if attempt == 0 { 10 } else { 20 }));
        match join_once(universe, place, account) {
            Ok(live) => return Ok(live),
            Err(e) if e.contains("1099511628033") => {
                println!("  join refused (0x101), retrying");
                last = e;
            }
            Err(e) => return Err(e),
        }
    }
    Err(last)
}

fn join_once(universe: u64, place: u64, account: Option<u64>) -> Result<Live, String> {
    let session = LiveSession::connect_as(universe, place, account).map_err(|e| e.to_string())?;
    let mut model = LiveModel::new();
    let until = Instant::now() + Duration::from_secs(240);
    loop {
        if Instant::now() > until {
            return Err("join timed out".into());
        }
        match session.events.recv_timeout(Duration::from_secs(1)) {
            Ok(LiveEvent::Batch(ops)) => ops.into_iter().for_each(|op| model.apply(op)),
            Ok(LiveEvent::Ready) => return Ok(Live { session, model }),
            Ok(LiveEvent::Error(e) | LiveEvent::Disconnected(e)) => return Err(e),
            Ok(LiveEvent::Waiting(n, m)) => eprintln!("  waiting for the server ({n}): {m}"),
            _ => {}
        }
    }
}

impl Live {
    fn child(&self, parent: u64, class: &str, name: &str) -> Option<u64> {
        let dom = self.model.dom();
        let p = if parent == 0 { dom.root_ref() } else { self.model.reference(parent)? };
        dom.get_by_ref(p)?
            .children()
            .iter()
            .filter_map(|c| dom.get_by_ref(*c))
            .find(|i| i.class.as_str() == class && i.name == name)
            .and_then(|i| self.model.server_id(i.referent()))
    }
    fn storage(&self) -> Result<u64, String> {
        self.child(0, "ServerStorage", "ServerStorage").ok_or_else(|| "no ServerStorage".into())
    }
    fn create(&self, class: &str, parent: u64, props: Vec<(String, P)>) -> Result<u64, String> {
        let id = self.session.allocate_id().map_err(|e| e.to_string())?;
        self.session.send_create(id, class.into(), parent, props).map_err(|e| e.to_string())?;
        Ok(id)
    }
    /// Watches the stream; Err(reason) if the server ends the session.
    fn watch(&mut self, secs: u64) -> Result<(), String> {
        let until = Instant::now() + Duration::from_secs(secs);
        while Instant::now() < until {
            match self.session.events.recv_timeout(Duration::from_millis(200)) {
                Ok(LiveEvent::Batch(ops)) => ops.into_iter().for_each(|op| {
                    if std::env::var("RUSTUDIO_PROBE_TRACE").is_ok()
                        && !matches!(op, rustudio_net::replicator::RepOp::Presence { .. } | rustudio_net::replicator::RepOp::Event { .. })
                    {
                        println!("  op: {}", format!("{op:?}").chars().take(200).collect::<String>());
                    }
                    self.model.apply(op)
                }),
                Ok(LiveEvent::Error(e) | LiveEvent::Disconnected(e)) => return Err(e),
                Ok(LiveEvent::EditFailed { id, error }) => println!("  edit {id:#x} not sent: {error}"),
                Ok(LiveEvent::PropertySkipped { name, reason, .. }) => println!("  skipped {name}: {reason}"),
                Ok(LiveEvent::Notice(m)) => println!("  notice: {m}"),
                _ => {}
            }
        }
        Ok(())
    }
}

/// Prints rustudio_net's log lines (debug with RUSTUDIO_PROBE_DEBUG).
struct Stderr;
impl log::Log for Stderr {
    fn enabled(&self, m: &log::Metadata) -> bool {
        m.target().starts_with("rustudio_net")
            && (m.level() <= log::Level::Info || std::env::var("RUSTUDIO_PROBE_DEBUG").is_ok())
    }
    fn log(&self, r: &log::Record) {
        if self.enabled(r.metadata()) {
            eprintln!("  [{}] {}", r.level(), r.args());
        }
    }
    fn flush(&self) {}
}

fn main() {
    let _ = log::set_logger(&Stderr);
    log::set_max_level(log::LevelFilter::Debug);
    let a: Vec<String> = std::env::args().skip(1).collect();
    let (universe, place): (u64, u64) = (a[0].parse().unwrap(), a[1].parse().unwrap());
    let account = a.get(2).and_then(|s| s.parse().ok());
    let only: Option<Vec<&str>> = a.get(3).map(|s| s.split(',').collect());
    for (name, creates) in cases() {
        if only.as_ref().is_some_and(|o| !o.contains(&name)) {
            continue;
        }
        let mut live = match join(universe, place, account) {
            Ok(l) => l,
            Err(e) => {
                println!("CASE {name}: join failed: {e}");
                continue;
            }
        };
        let result = (|| {
            let storage = live.storage()?;
            let folder = match live.child(storage, "Folder", FOLDER) {
                Some(f) => f,
                None => {
                    let f = live.create("Folder", storage, vec![named(FOLDER)])?;
                    live.watch(3)?;
                    f
                }
            };
            for (class, props) in creates {
                live.create(class, folder, props)?;
            }
            live.watch(6)
        })();
        match result {
            Ok(()) => println!("CASE {name}: accepted"),
            Err(e) => println!("CASE {name}: SESSION ENDED: {e}"),
        }
    }
    // RUSTUDIO_PROBE_SEQ=1: create / delete / check across separate joins
    // (see the step labels); everything lives under the probe folders.
    if std::env::var("RUSTUDIO_PROBE_SEQ").is_ok_and(|v| v == "1") {
        let step = |label: &str, f: &mut dyn FnMut(&mut Live) -> Result<(), String>| match join(universe, place, account) {
            Ok(mut live) => match f(&mut live).and_then(|_| live.watch(8)) {
                Ok(()) => println!("{label}: ok"),
                Err(e) => println!("{label}: SESSION ENDED {e}"),
            },
            Err(e) => println!("{label}: join failed {e}"),
        };
        let look = |live: &Live| {
            let storage = live.storage().unwrap();
            let f = live.child(storage, "Folder", FOLDER);
            let f2 = live.child(storage, "Folder", "rustudio_tc_probe2");
            let m = f.and_then(|f| live.child(f, "ModuleScript", "seq_module"));
            let sa = f.and_then(|f| live.child(f, "SurfaceAppearance", "seq_sa"));
            println!("  state: folder {f:x?} folder2 {f2:x?} module {m:x?} surface {sa:x?}");
            (f, f2, m, sa)
        };
        step("A create", &mut |live| {
            let storage = live.storage()?;
            let f = live.create("Folder", storage, vec![named(FOLDER)])?;
            live.create("ModuleScript", f, vec![named("seq_module"), ("Source".into(), P::String("return 1\n".into()))])?;
            live.create("SurfaceAppearance", f, vec![named("seq_sa"), ("AlphaMode".into(), P::Enum(1))])?;
            live.create("Folder", storage, vec![named("rustudio_tc_probe2")])?;
            Ok(())
        });
        step("B delete the surface and folder2", &mut |live| {
            let (_, f2, _, sa) = look(live);
            for id in [sa, f2].into_iter().flatten() {
                live.session.send_delete(id).map_err(|e| e.to_string())?;
            }
            Ok(())
        });
        step("C delete the folder holding the module", &mut |live| {
            let (f, ..) = look(live);
            if let Some(f) = f {
                live.session.send_delete(f).map_err(|e| e.to_string())?;
            }
            Ok(())
        });
        step("D nil-parent the folder if still there", &mut |live| {
            let (f, ..) = look(live);
            if let Some(f) = f {
                println!("  folder survived its delete; setting Parent = nil");
                live.session.send_reparent(f, 0).map_err(|e| e.to_string())?;
            }
            Ok(())
        });
        step("E final check", &mut |live| {
            look(live);
            Ok(())
        });
        return;
    }
    // RUSTUDIO_PROBE_CLEAN=1: delete (or with =check only list) every
    // ServerStorage child named rustudio_tc_probe*.
    if let Ok(mode) = std::env::var("RUSTUDIO_PROBE_CLEAN") {
        match join(universe, place, account) {
            Ok(mut live) => {
                let storage = live.storage().unwrap();
                let r = live.model.reference(storage).unwrap();
                let found: Vec<(String, String, Option<u64>, usize)> = live.model.dom().get_by_ref(r).unwrap().children().iter().filter_map(|c| {
                    let i = live.model.dom().get_by_ref(*c)?;
                    i.name.starts_with("rustudio_tc_probe").then(|| (i.class.to_string(), i.name.clone(), live.model.server_id(*c), live.model.dom().descendants_of(*c).count() - 1))
                }).collect();
                println!("CLEAN: {} probe instance(s) in ServerStorage", found.len());
                for (class, name, id, below) in &found {
                    println!("  {class} {name} {id:x?} ({below} below)");
                    if mode != "check" {
                        if let Some(id) = id {
                            live.session.send_delete(*id).unwrap();
                        }
                    }
                }
                let _ = live.watch(8);
            }
            Err(e) => println!("CLEAN join failed: {e}"),
        }
        return;
    }
    // RUSTUDIO_PROBE_SEQ=3: three childless folders (indices 1, 2, 3 of one
    // session's scope); another session deletes only the middle one.
    if std::env::var("RUSTUDIO_PROBE_SEQ").is_ok_and(|v| v == "3") {
        let names = ["rustudio_tc_probe_p1", "rustudio_tc_probe_p2", "rustudio_tc_probe_p3"];
        let ids = |live: &Live| {
            let storage = live.storage().unwrap();
            names.map(|n| live.child(storage, "Folder", n))
        };
        let run = |label: &str, f: &mut dyn FnMut(&mut Live) -> Result<(), String>| match join(universe, place, account) {
            Ok(mut live) => match f(&mut live).and_then(|_| live.watch(8)) {
                Ok(()) => println!("{label}: ok"),
                Err(e) => println!("{label}: SESSION ENDED {e}"),
            },
            Err(e) => println!("{label}: join failed {e}"),
        };
        if std::env::var("RUSTUDIO_PROBE_SKIP_A").is_err() { run("A create p1 p2 p3", &mut |live| {
            let storage = live.storage()?;
            for n in names {
                let id = live.create("Folder", storage, vec![named(n)])?;
                println!("  {n} {id:#x}");
            }
            Ok(())
        }); }
        run("B delete p2 only", &mut |live| {
            let found = ids(live);
            println!("  before: {found:x?}");
            let target = std::env::var("RUSTUDIO_PROBE_TARGET").ok().and_then(|t| t.parse::<usize>().ok()).unwrap_or(1);
            if let Some(id) = found[target] {
                println!("  deleting {} {id:#x}", names[target]);
                live.session.send_delete(id).map_err(|e| e.to_string())?;
            }
            Ok(())
        });
        run("C check", &mut |live| {
            println!("  after: {:x?}", ids(live));
            Ok(())
        });
        return;
    }
    // RUSTUDIO_PROBE_SEQ=2: is it "had children" or "the session's first
    // referent" that a delete can't remove?
    if std::env::var("RUSTUDIO_PROBE_SEQ").is_ok_and(|v| v == "2") {
        let names = ["rustudio_tc_probe_first", "rustudio_tc_probe_parent"];
        let ids = |live: &Live| {
            let storage = live.storage().unwrap();
            names.map(|n| live.child(storage, "Folder", n))
        };
        let run = |label: &str, f: &mut dyn FnMut(&mut Live) -> Result<(), String>| match join(universe, place, account) {
            Ok(mut live) => match f(&mut live).and_then(|_| live.watch(8)) {
                Ok(()) => println!("{label}: ok"),
                Err(e) => println!("{label}: SESSION ENDED {e}"),
            },
            Err(e) => println!("{label}: join failed {e}"),
        };
        if std::env::var("RUSTUDIO_PROBE_SKIP_A").is_err() { run("A create first (childless) then parent (with a child)", &mut |live| {
            let storage = live.storage()?;
            let first = live.create("Folder", storage, vec![named(names[0])])?;
            let parent = live.create("Folder", storage, vec![named(names[1])])?;
            live.create("ModuleScript", parent, vec![named("seq_child"), ("Source".into(), P::String("return 1\n".into()))])?;
            println!("  first {first:#x}, parent {parent:#x}");
            Ok(())
        }); }
        run("B delete both", &mut |live| {
            let [first, parent] = ids(live);
            println!("  before: first {first:x?} parent {parent:x?}");
            for id in [first, parent].into_iter().flatten() {
                live.session.send_delete(id).map_err(|e| e.to_string())?;
            }
            Ok(())
        });
        run("C check", &mut |live| {
            let [first, parent] = ids(live);
            println!("  after: first {first:x?} parent {parent:x?}");
            Ok(())
        });
        return;
    }
    // RUSTUDIO_PROBE_DELETE=1: delete one child of each class in the probe
    // folder, then report from a fresh join which are still there.
    if std::env::var("RUSTUDIO_PROBE_DELETE").is_ok() {
        let mut targets = Vec::new();
        let first = join(universe, place, account);
        if let Err(e) = &first {
            println!("DELETE join failed: {e}");
        }
        if let Ok(mut live) = first {
            let storage = live.storage().unwrap();
            let folder = live.child(storage, "Folder", FOLDER);
            println!("DELETE probe folder: {folder:x?}");
            if let Some(f) = folder {
                let r = live.model.reference(f).unwrap();
                for c in live.model.dom().get_by_ref(r).unwrap().children() {
                    let i = live.model.dom().get_by_ref(*c).unwrap();
                    println!("  child {} {} {:x?}", i.class, i.name, live.model.server_id(*c));
                }
                for (class, name) in [("SurfaceAppearance", "sa_color_real"), ("SurfaceAppearance", "sa_burst")] {
                    if let Some(id) = live.child(f, class, name) {
                        println!("DELETE {class} {name} {id:#x}");
                        live.session.send_delete(id).unwrap();
                        targets.push((class, name));
                    }
                }
            }
            let _ = live.watch(10);
        }
        if let Ok(live) = join(universe, place, account) {
            let storage = live.storage().unwrap();
            if let Some(f) = live.child(storage, "Folder", FOLDER) {
                let r = live.model.reference(f).unwrap();
                println!("AFTER: probe folder has {} children", live.model.dom().get_by_ref(r).unwrap().children().len());
                for (class, name) in targets {
                    println!("AFTER DELETE {class} {name}: {}", if live.child(f, class, name).is_some() { "STILL THERE" } else { "gone" });
                }
            }
        }
        return;
    }
    // RUSTUDIO_PROBE_CLEANUP=nil sets each probe folder's Parent to nil
    // instead of deleting it.
    if std::env::var("RUSTUDIO_PROBE_CLEANUP").is_ok_and(|v| v == "nil") {
        if let Ok(mut live) = join(universe, place, account) {
            let storage = live.storage().unwrap();
            if let Some(f) = live.child(storage, "Folder", FOLDER) {
                println!("NIL PARENT {f:#x}");
                live.session.send_reparent(f, 0).unwrap();
            }
            let _ = live.watch(6);
        }
    }
    // Clean up: delete every probe folder, then check from a fresh join.
    // RUSTUDIO_PROBE_CLEANUP=children deletes each folder's descendants
    // (deepest first) before the folder itself.
    let children_first = std::env::var("RUSTUDIO_PROBE_CLEANUP").is_ok_and(|v| v == "children");
    for round in 0..3 {
        match join(universe, place, account) {
            Ok(mut live) => {
                let storage = live.storage().unwrap();
                let mut n = 0;
                while let Some(f) = live.child(storage, "Folder", FOLDER) {
                    let r = live.model.reference(f).unwrap();
                    let below: Vec<_> = live.model.dom().descendants_of(r).skip(1).map(|i| i.referent()).collect();
                    println!("CLEANUP round {round}: folder {f:#x} with {} descendant(s)", below.len());
                    if children_first {
                        for d in below.iter().rev() {
                            if let Some(id) = live.model.server_id(*d) {
                                live.session.send_delete(id).unwrap();
                            }
                        }
                    }
                    live.session.send_delete(f).unwrap();
                    for d in below {
                        live.model.unbind(d);
                    }
                    live.model.unbind(r);
                    n += 1;
                }
                if n == 0 {
                    println!("CLEANUP round {round}: no probe folder left");
                    break;
                }
                let _ = live.watch(4);
            }
            Err(e) => println!("CLEANUP join failed: {e}"),
        }
    }
}
