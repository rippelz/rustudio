//! Opt-in live probe: which part of an R6 rig a Team Create server refuses
//! (0x104). Each case runs on a fresh connection: join, make sure
//! `ServerStorage.rustudio_tc_probe` exists, send the case's creates into it
//! (as rustudio's planner sent the rig, 2026-11-21), watch 6 s. Then a last
//! join deletes the probe folder and another checks it is gone. Nothing else
//! in the place is touched. Run it only against a place whose owner agreed.
//!
//!   cargo run --release -p rustudio-net --example tc_rig_probe -- UNIVERSE PLACE [case,...]
use rustudio_net::live::{LiveEvent, LiveSession};
use rustudio_net::model::LiveModel;
use rustudio_net::replicator::PropValue as P;
use std::time::{Duration, Instant};

fn folder_name() -> String {
    std::env::var("RUSTUDIO_PROBE_FOLDER").unwrap_or_else(|_| "rustudio_tc_probe".into())
}
const FOLDER: &str = "rustudio_tc_probe";

struct Live {
    session: LiveSession,
    model: LiveModel,
}

fn join(universe: u64, place: u64) -> Result<Live, String> {
    let mut last = String::new();
    for attempt in 0..3 {
        std::thread::sleep(Duration::from_secs(if attempt == 0 { 8 } else { 20 }));
        match join_once(universe, place) {
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

fn join_once(universe: u64, place: u64) -> Result<Live, String> {
    let session = LiveSession::connect_as(universe, place, None).map_err(|e| e.to_string())?;
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
    fn create(&self, class: &str, parent: u64, props: Vec<(&str, P)>) -> Result<u64, String> {
        let id = self.session.allocate_id().map_err(|e| e.to_string())?;
        let props = props.into_iter().map(|(n, v)| (n.to_string(), v)).collect();
        self.session.send_create(id, class.into(), parent, props).map_err(|e| e.to_string())?;
        Ok(id)
    }
    fn watch(&mut self, secs: u64) -> Result<(), String> {
        let until = Instant::now() + Duration::from_secs(secs);
        while Instant::now() < until {
            match self.session.events.recv_timeout(Duration::from_millis(200)) {
                Ok(LiveEvent::Batch(ops)) => ops.into_iter().for_each(|op| self.model.apply(op)),
                Ok(LiveEvent::Error(e) | LiveEvent::Disconnected(e)) => return Err(e),
                Ok(LiveEvent::EditFailed { id, error }) => println!("  edit {id:#x} not sent: {error}"),
                Ok(LiveEvent::PropertySkipped { name, reason, .. }) => println!("  skipped {name}: {reason}"),
                _ => {}
            }
        }
        Ok(())
    }
}

fn s(v: &str) -> P {
    P::String(v.into())
}
fn cf(pos: [f32; 3], rot: [f32; 9]) -> P {
    P::CFrame { pos, rot }
}
const ID: [f32; 9] = [1., 0., 0., 0., 1., 0., 0., 0., 1.];
const FLIP: [f32; 9] = [-1., 0., 0., 0., 1., 0., 0., 0., -1.];

fn part(name: &str) -> Vec<(&str, P)> {
    vec![
        ("Name", s(name)),
        ("FormFactor", P::Enum(0)),
        ("CFrame", cf([3018.0923, 5.738074, 66.61895], FLIP)),
        ("CanCollide", P::Bool(false)),
        ("Color", P::Color3uint8([127, 127, 127])),
        ("Size", P::Vector3([1.0, 2.0, 1.0])),
    ]
}

/// One case: creates under the probe folder (`f`), each may name earlier ids.
type Case = fn(&Live, u64) -> Result<(), String>;

fn cases() -> Vec<(&'static str, Case)> {
    vec![
        ("model", |l, f| {
            l.create("Model", f, vec![
                ("Name", s("")),
                ("ModelMeshCFrame", cf([3016.5918, -12.259199, 44.121986], [1.0000056, 0., 0., 0., 0.9998659, 0., 0., 0., 0.9998719])),
                ("WorldPivotData", cf([3016.5923, 2.7380738, 66.61895], FLIP)),
            ])?;
            Ok(())
        }),
        ("part", |l, f| {
            l.create("Part", f, part("Left Arm"))?;
            Ok(())
        }),
        ("motor", |l, f| {
            let a = l.create("Part", f, part("Torso"))?;
            let b = l.create("Part", f, part("Head"))?;
            l.create("Motor6D", a, vec![
                ("Name", s("Neck")),
                ("MaxVelocity", P::Float(0.1)),
                ("C0", cf([0., 1., 0.], [-1., 0., 0., 0., 0., 1., 0., 1., 0.])),
                ("C1", cf([0., -0.5, 0.], [-1., 0., 0., 0., 0., 1., 0., 1., 0.])),
                ("Part0", P::Ref(Some(a))),
                ("Part1", P::Ref(Some(b))),
            ])?;
            Ok(())
        }),
        ("motor_name", |l, f| {
            let a = l.create("Part", f, part("Torso"))?;
            l.create("Motor6D", a, vec![("Name", s("Neck"))])?;
            Ok(())
        }),
        ("motor_cframes", |l, f| {
            let a = l.create("Part", f, part("Torso"))?;
            l.create("Motor6D", a, vec![
                ("Name", s("Neck")),
                ("MaxVelocity", P::Float(0.1)),
                ("C0", cf([0., 1., 0.], [-1., 0., 0., 0., 0., 1., 0., 1., 0.])),
                ("C1", cf([0., -0.5, 0.], [-1., 0., 0., 0., 0., 1., 0., 1., 0.])),
            ])?;
            Ok(())
        }),
        ("motor_part0", |l, f| {
            let a = l.create("Part", f, part("Torso"))?;
            l.create("Motor6D", a, vec![("Name", s("Neck")), ("Part0", P::Ref(Some(a)))])?;
            Ok(())
        }),
        ("objectvalue_ref", |l, f| {
            let a = l.create("Part", f, part("Torso"))?;
            l.create("ObjectValue", a, vec![("Name", s("ref")), ("Value", P::Ref(Some(a)))])?;
            Ok(())
        }),
        ("ov_nil", |l, f| {
            l.create("ObjectValue", f, vec![("Name", s("ref_nil")), ("Value", P::Ref(None))])?;
            Ok(())
        }),
        ("ov_server", |l, f| {
            let st = l.storage()?;
            println!("  ServerStorage {st:#x}");
            l.create("ObjectValue", f, vec![("Name", s("ref_server")), ("Value", P::Ref(Some(st)))])?;
            Ok(())
        }),
        ("ov_folder", |l, f| {
            println!("  folder {f:#x}");
            l.create("ObjectValue", f, vec![("Name", s("ref_folder")), ("Value", P::Ref(Some(f)))])?;
            Ok(())
        }),
        ("ov_then_prop", |l, f| {
            let st = l.storage()?;
            let ov = l.create("ObjectValue", f, vec![("Name", s("ref_prop"))])?;
            l.session.send_property(ov, "ObjectValue".into(), "Value".into(), P::Ref(Some(st))).map_err(|e| e.to_string())?;
            Ok(())
        }),
        ("motor_then_prop", |l, f| {
            let a = l.create("Part", f, part("Torso"))?;
            let m = l.create("Motor6D", a, vec![("Name", s("Neck"))])?;
            l.session.send_property(m, "Motor6D".into(), "Part0".into(), P::Ref(Some(a))).map_err(|e| e.to_string())?;
            Ok(())
        }),
        ("look", |l, f| {
            let dom = l.model.dom();
            let r = l.model.reference(f).ok_or("no folder")?;
            for d in dom.descendants_of(r) {
                if matches!(d.class.as_str(), "ObjectValue" | "Motor6D") {
                    let props: Vec<String> = d.properties.iter().filter(|(k, _)| matches!(k.as_str(), "Value" | "Part0" | "Part1")).map(|(k, v)| {
                        let target = if let rbx_types::Variant::Ref(t) = v { dom.get_by_ref(*t).map(|i| format!("{} {}", i.class, i.name)) } else { None };
                        format!("{k}={v:?} -> {target:?}")
                    }).collect();
                    println!("  {} {}: {props:?}", d.class, d.name);
                }
            }
            Ok(())
        }),
        ("cframe_set", |l, f| {
            let a = l.create("Part", f, part("Torso"))?;
            l.session.send_property(a, "Part".into(), "CFrame".into(), cf([3020.0, 6.0, 66.0], FLIP)).map_err(|e| e.to_string())?;
            Ok(())
        }),
        ("cframe_rot", |l, f| {
            let a = l.create("Part", f, part("Torso"))?;
            let (c, sn) = (0.8f32, 0.6f32);
            l.session.send_property(a, "Part".into(), "CFrame".into(), cf([3020.0, 6.0, 66.0], [c, 0., sn, 0., 1., 0., -sn, 0., c])).map_err(|e| e.to_string())?;
            Ok(())
        }),
        ("cframe_burst", |l, f| {
            let parts: Vec<u64> = (0..12).map(|i| l.create("Part", f, part(&format!("p{i}")))).collect::<Result<_, _>>()?;
            for step in 0..60 {
                for &p in &parts {
                    l.session.send_property(p, "Part".into(), "CFrame".into(), cf([3020.0 + step as f32 * 0.1, 6.0, 66.0], FLIP)).map_err(|e| e.to_string())?;
                }
            }
            Ok(())
        }),
        ("guid_twice", |l, f| {
            for n in ["a", "b"] {
                l.create("Script", f, vec![("Name", s(n)), ("Source", s("-- probe\n")), ("ScriptGuid", s("{0B0B0B0B-1111-4222-8333-444455556666}"))])?;
            }
            Ok(())
        }),
        ("guid_existing", |l, f| {
            // A ScriptGuid a script already in the place has.
            let dom = l.model.dom();
            let guid = dom.descendants().find_map(|i| match i.properties.get(&"ScriptGuid".into()) {
                Some(rbx_types::Variant::String(g)) if !g.is_empty() && i.class.as_str() == "Script" => Some(g.clone()),
                _ => None,
            }).ok_or("no script with a guid")?;
            println!("  reusing {guid}");
            l.create("Script", f, vec![("Name", s("dup")), ("Source", s("-- probe\n")), ("ScriptGuid", P::String(guid))])?;
            Ok(())
        }),
        ("source_twice", |l, f| {
            for (n, g) in [("a", "{0C0C0C0C-1111-4222-8333-444455556661}"), ("b", "{0C0C0C0C-1111-4222-8333-444455556662}")] {
                l.create("Script", f, vec![("Name", s(n)), ("Source", s("-- same source\n")), ("ScriptGuid", s(g))])?;
            }
            Ok(())
        }),
        ("sandboxed", |l, f| {
            l.create("Script", f, vec![("Name", s("Respawn")), ("Source", s("-- sandboxed probe\n")), ("Sandboxed", P::Bool(true))])?;
            Ok(())
        }),
        ("sandboxed_set", |l, f| {
            let sc = l.create("Script", f, vec![("Name", s("Respawn2")), ("Source", s("-- sandboxed probe 2\n"))])?;
            l.session.send_property(sc, "Script".into(), "Sandboxed".into(), P::Bool(true)).map_err(|e| e.to_string())?;
            Ok(())
        }),
        ("attachment", |l, f| {
            let p = l.create("Part", f, vec![("Name", s("p"))])?;
            l.create("Attachment", p, vec![("Name", s("HatAttachment")), ("CFrame", cf([0., 0.6, 0.], ID))])?;
            Ok(())
        }),
        ("script", |l, f| {
            l.create("Script", f, vec![
                ("Name", s("Respawn")),
                ("Source", s("-- probe\nprint(1)\n")),
                ("ScriptGuid", s("{FC4009F0-1F59-4E83-8D70-0C5E9E5357A8}")),
                ("SourceAssetId", P::Int64(1491351835)),
            ])?;
            Ok(())
        }),
        ("script_plain", |l, f| {
            l.create("Script", f, vec![("Name", s("Respawn")), ("Source", s("-- probe\nprint(1)\n")), ("ScriptGuid", s("{FC4009F0-1F59-4E83-8D70-0C5E9E5357A9}"))])?;
            Ok(())
        }),
        ("animate", |l, f| {
            let src = format!("-- probe\n{}", "local x = 1 -- padding padding padding padding\n".repeat(320));
            let a = l.create("LocalScript", f, vec![("Name", s("Animate")), ("Source", P::String(src)), ("ScriptGuid", s("{D1ADA20F-E8A6-49FB-A09C-203641CCD9C8}"))])?;
            let idle = l.create("StringValue", a, vec![("Name", s("idle"))])?;
            let anim = l.create("Animation", idle, vec![("Name", s("Animation1")), ("AnimationContent", s("http://www.roblox.com/asset/?id=180435571"))])?;
            l.create("NumberValue", anim, vec![("Name", s("Weight")), ("Value", P::Float(9.0))])?;
            l.create("BindableFunction", a, vec![("Name", s("PlayEmote"))])?;
            Ok(())
        }),
        ("humanoid", |l, f| {
            let h = l.create("Humanoid", f, vec![("Name", s("Humanoid"))])?;
            l.create("Animator", h, vec![("Name", s("Animator"))])?;
            Ok(())
        }),
        ("humdesc", |l, f| {
            let d = l.create("HumanoidDescription", f, vec![("Name", s("HumanoidDescription")), ("BodyTypeScale", P::Float(0.0)), ("ProportionScale", P::Float(0.0))])?;
            l.create("BodyPartDescription", d, vec![("Name", s("BodyPartDescription")), ("BodyPart", P::Enum(1)), ("Color", P::Color3([0.5, 0.5, 0.5]))])?;
            Ok(())
        }),
        ("bodycolors", |l, f| {
            let c = |n| (n, P::Color3([0.5, 0.5, 0.5]));
            l.create("BodyColors", f, vec![("Name", s("Body Colors")), c("HeadColor3"), c("LeftArmColor3"), c("LeftLegColor3"), c("RightArmColor3"), c("RightLegColor3"), c("TorsoColor3")])?;
            Ok(())
        }),
        ("decal_mesh", |l, f| {
            let p = l.create("Part", f, vec![("Name", s("p"))])?;
            l.create("Decal", p, vec![("Name", s("face")), ("TextureContent", s("rbxasset://textures/face.png"))])?;
            l.create("SpecialMesh", p, vec![("Name", s("Mesh")), ("Scale", P::Vector3([1.25, 1.25, 1.25]))])?;
            Ok(())
        }),
    ]
}

fn main() {
    let a: Vec<String> = std::env::args().skip(1).collect();
    let (universe, place): (u64, u64) = (a[0].parse().unwrap(), a[1].parse().unwrap());
    let only: Option<Vec<String>> = a.get(2).map(|s| s.split(',').map(String::from).collect());
    for (name, case) in cases() {
        if only.as_ref().is_some_and(|o| !o.iter().any(|x| x == name)) {
            continue;
        }
        let mut live = match join(universe, place) {
            Ok(l) => l,
            Err(e) => {
                println!("CASE {name}: join failed: {e}");
                continue;
            }
        };
        let result = (|| {
            let storage = live.storage()?;
            let folder = match live.child(storage, "Folder", &folder_name()) {
                Some(f) => f,
                None => {
                    let f = live.create("Folder", storage, vec![("Name", P::String(folder_name()))])?;
                    live.watch(3)?;
                    f
                }
            };
            case(&live, folder)?;
            live.watch(6)
        })();
        match result {
            Ok(()) => println!("CASE {name}: accepted"),
            Err(e) => println!("CASE {name}: SESSION ENDED: {e}"),
        }
    }
    if std::env::var("RUSTUDIO_PROBE_NO_CLEAN").is_ok() { return; }
    // Clean up: delete every ServerStorage.rustudio_tc_probe*, then check.
    for pass in ["delete", "check"] {
        match join(universe, place) {
            Ok(mut live) => {
                let storage = live.storage().unwrap();
                let r = live.model.reference(storage).unwrap();
                let found: Vec<(String, Option<u64>)> = live.model.dom().get_by_ref(r).unwrap().children().iter().filter_map(|c| {
                    let i = live.model.dom().get_by_ref(*c)?;
                    i.name.starts_with(FOLDER).then(|| (i.name.clone(), live.model.server_id(*c)))
                }).collect();
                println!("CLEAN {pass}: {} probe folder(s): {found:x?}", found.len());
                if pass == "delete" {
                    for (_, id) in &found {
                        if let Some(id) = id {
                            live.session.send_delete(*id).unwrap();
                        }
                    }
                    let _ = live.watch(8);
                }
            }
            Err(e) => println!("CLEAN {pass}: join failed {e}"),
        }
    }
}
