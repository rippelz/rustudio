//! `rustudio --solver-ticks SCENE.json [--out FILE]`: builds a compiled scene
//! (checks/humanoid-physics/ticks/solver/scene.py) in a fresh place, steps the
//! physics one 240 Hz tick at a time with whichever solver RUSTUDIO_SOLVER picks,
//! and prints after every tick, per tracked body, the columns Studio's
//! `workspace:StepPhysics(1/240, parts)` lab prints:
//!
//! `tick name px py pz r00 r01 r02 r10 r11 r12 r20 r21 r22 vx vy vz wx wy wz`
//!
//! (position relative to the scene's origin, rotation matrix rows, the
//! assembly's linear velocity at its centre of mass and angular velocity).

use std::collections::HashMap;
use std::io::Write;

use glam::Vec3;
use rbx_dom_weak::types::{CFrame, CustomPhysicalProperties, Enum, Matrix3, PhysicalProperties, Ref, Variant, Vector2, Vector3};
use rbx_dom_weak::{ustr, InstanceBuilder};
use serde_json::Value;

use rustudio_core::doc::{self, Document};
use crate::physics::Physics;

fn f(v: &Value) -> f32 {
    v.as_f64().unwrap_or(0.0) as f32
}

fn v3(v: &Value) -> Vector3 {
    Vector3::new(f(&v[0]), f(&v[1]), f(&v[2]))
}

fn cframe(v: &Value, origin: [f64; 3]) -> CFrame {
    let g = |i: usize| f(&v[i]);
    // (x, y, z, R00 .. R22) as CFrame.new takes them; the translation in f64
    // plus the origin, then to f32 as Studio stores it.
    let pos = |i: usize| (v[i].as_f64().unwrap_or(0.0) + origin[i]) as f32;
    CFrame::new(
        Vector3::new(pos(0), pos(1), pos(2)),
        Matrix3::new(Vector3::new(g(3), g(4), g(5)), Vector3::new(g(6), g(7), g(8)), Vector3::new(g(9), g(10), g(11))),
    )
}

/// A JSON value as a property (the typed forms are scene.py's).
fn variant(v: &Value, refs: &HashMap<String, Ref>) -> Option<Variant> {
    Some(match v {
        Value::Bool(b) => Variant::Bool(*b),
        Value::Number(n) => Variant::Float32(n.as_f64()? as f32),
        Value::String(s) => Variant::String(s.clone()),
        Value::Object(o) => {
            if let Some(x) = o.get("v3") {
                Variant::Vector3(v3(x))
            } else if let Some(x) = o.get("v2") {
                Variant::Vector2(Vector2::new(f(&x[0]), f(&x[1])))
            } else if let Some(x) = o.get("enum") {
                Variant::Enum(Enum::from_u32(x[2].as_u64()? as u32))
            } else if let Some(x) = o.get("phys") {
                Variant::PhysicalProperties(PhysicalProperties::Custom(CustomPhysicalProperties::new(f(&x[0]), f(&x[1]), f(&x[2]), f(&x[3]), f(&x[4]), 1.0)))
            } else if let Some(x) = o.get("ref") {
                Variant::Ref(*refs.get(x.as_str()?)?)
            } else if let Some(x) = o.get("cf") {
                Variant::CFrame(cframe(x, [0.0; 3]))
            } else if let Some(x) = o.get("int") {
                Variant::Int32(x.as_i64()? as i32)
            } else if let Some(x) = o.get("f64") {
                Variant::Float64(x.as_f64()?)
            } else {
                return None;
            }
        }
        _ => return None,
    })
}

pub fn run(path: &str, out: Option<&str>) -> Result<(), String> {
    let scene: Value = serde_json::from_str(&std::fs::read_to_string(path).map_err(|e| format!("{path}: {e}"))?).map_err(|e| format!("{path}: {e}"))?;
    if scene.get("humanoid").is_some_and(|h| !h.is_null()) {
        return Err("humanoid scenes run in Play: ticks/solver/run_rustudio.py --play".into());
    }
    let name = scene["name"].as_str().unwrap_or("scene").to_string();
    let ticks = scene["ticks"].as_u64().unwrap_or(240) as usize;
    let o = &scene["origin"];
    let origin = [o[0].as_f64().unwrap_or(0.0), o[1].as_f64().unwrap_or(0.0), o[2].as_f64().unwrap_or(0.0)];

    let mut d = Document::new_place();
    let ws = d.workspace().ok_or("no workspace")?;
    for n in ["Baseplate", "SpawnLocation"] {
        if let Some(r) = d.find_named(ws, n) {
            d.dom.destroy(r);
        }
    }
    if let Some(g) = scene.get("gravity").and_then(|g| g.as_f64()) {
        d.set_prop_live(ws, ustr("Gravity"), Variant::Float32(g as f32));
    }
    let lab = d.dom.insert(ws, InstanceBuilder::new("Folder").with_name("StepLab"));
    let mut refs: HashMap<String, Ref> = HashMap::new();
    let insts = scene["instances"].as_array().cloned().unwrap_or_default();
    // Instances in order (a parent before its children), then the props.
    for i in &insts {
        let class = i["class"].as_str().ok_or("instance without class")?;
        let iname = i["name"].as_str().unwrap_or(class);
        let parent = i["parent"].as_str().and_then(|p| refs.get(p).copied()).unwrap_or(lab);
        let r = d.dom.insert(parent, InstanceBuilder::new(class).with_name(iname));
        refs.insert(iname.to_string(), r);
    }
    for i in &insts {
        let r = refs[i["name"].as_str().unwrap_or(i["class"].as_str().unwrap_or(""))];
        let class = i["class"].as_str().unwrap_or("");
        if let Some(cf) = i.get("cf") {
            let base = doc::is_a(class, "BasePart");
            let c = cframe(cf, if base { origin } else { [0.0; 3] });
            d.dom.get_by_ref_mut(r).unwrap().properties.insert(ustr("CFrame"), Variant::CFrame(c));
        }
        if let Some(props) = i["props"].as_object() {
            for (k, v) in props {
                let var = variant(v, &refs).ok_or_else(|| format!("{}.{k}: can't read {v}", i["name"]))?;
                d.dom.get_by_ref_mut(r).unwrap().properties.insert(ustr(k), var);
            }
        }
    }
    let mut p = Physics::new(&mut d);
    p.sync(&mut d);
    let mut track: Vec<(String, Ref)> = Vec::new();
    let names: Vec<String> = match scene["track"].as_array() {
        Some(a) => a.iter().filter_map(|x| x.as_str().map(String::from)).collect(),
        None => insts
            .iter()
            .filter(|i| doc::is_a(i["class"].as_str().unwrap_or(""), "BasePart") && !i["props"]["Anchored"].as_bool().unwrap_or(false))
            .filter_map(|i| i["name"].as_str().map(String::from))
            .collect(),
    };
    for n in names {
        let r = *refs.get(&n).ok_or_else(|| format!("track: no {n}"))?;
        track.push((n, r));
    }
    if let Some(init) = scene["init"].as_object() {
        for (n, v) in init {
            let r = *refs.get(n).ok_or_else(|| format!("init: no {n}"))?;
            if let Some(x) = v.get("vel") {
                let x = v3(x);
                p.set_velocity(r, Vec3::new(x.x, x.y, x.z));
            }
            if let Some(x) = v.get("angvel") {
                let x = v3(x);
                p.set_angular_velocity(r, Vec3::new(x.x, x.y, x.z));
            }
        }
    }
    let mut w: Box<dyn Write> = match out {
        Some(o) => Box::new(std::io::BufWriter::new(std::fs::File::create(o).map_err(|e| format!("{o}: {e}"))?)),
        None => Box::new(std::io::BufWriter::new(std::io::stdout())),
    };
    let solver = crate::solver::kind().name();
    writeln!(w, "# scene {name} engine rustudio solver {solver} ticks {ticks}").ok();
    let print = |w: &mut Box<dyn Write>, t: usize, d: &Document, p: &Physics| {
        for (n, r) in &track {
            let Some(cf) = (match d.prop(*r, "CFrame") {
                Some(Variant::CFrame(c)) => Some(c),
                _ => None,
            }) else {
                continue;
            };
            let (v, wv) = p.part_motion(*r).unwrap_or((Vec3::ZERO, Vec3::ZERO));
            let m = cf.orientation;
            let pp = cf.position;
            writeln!(
                w,
                "{t} {n} {:.9e} {:.9e} {:.9e} {:.9e} {:.9e} {:.9e} {:.9e} {:.9e} {:.9e} {:.9e} {:.9e} {:.9e} {:.9e} {:.9e} {:.9e} {:.9e} {:.9e} {:.9e}",
                pp.x as f64 - origin[0],
                pp.y as f64 - origin[1],
                pp.z as f64 - origin[2],
                m.x.x, m.x.y, m.x.z, m.y.x, m.y.y, m.y.z, m.z.x, m.z.y, m.z.z,
                v.x, v.y, v.z, wv.x, wv.y, wv.z
            )
            .ok();
        }
    };
    print(&mut w, 0, &d, &p);
    let h = 1.0 / rustudio_core::mech::PHYSICS_HZ;
    let per_tick = std::env::var_os("RUSTUDIO_BENCH_INSTR").is_some();
    let mut last = 0.0;
    // `at`: Luau run before a tick's step in Studio's lab. The one form the scenes use,
    // `I["Name"].AssemblyLinearVelocity / AssemblyAngularVelocity = Vector3.new(x, y, z)`,
    // is applied as the property write (the body's wake path included).
    let mut at: Vec<(usize, Ref, bool, Vec3)> = Vec::new();
    if let Some(o) = scene["at"].as_object() {
        for (k, code) in o {
            let (Ok(t), Some(code)) = (k.parse::<usize>(), code.as_str()) else { continue };
            // Statements are separated by ';' or only by whitespace (lab_arb_rise).
            for stmt in code.split(';').flat_map(|s| s.split("I[\"").skip(1)) {
                let parse = || -> Option<(Ref, bool, Vec3)> {
                    let rest = stmt.trim();
                    let (n, rest) = rest.split_once("\"].")?;
                    let (prop, rest) = rest.split_once('=')?;
                    let ang = match prop.trim() {
                        "AssemblyLinearVelocity" => false,
                        "AssemblyAngularVelocity" => true,
                        _ => return None,
                    };
                    let args = rest.trim().strip_prefix("Vector3.new(")?.strip_suffix(')')?;
                    let x: Vec<f32> = args.split(',').filter_map(|a| a.trim().parse().ok()).collect();
                    if x.len() != 3 {
                        return None;
                    }
                    Some((*refs.get(n)?, ang, Vec3::new(x[0], x[1], x[2])))
                };
                match parse() {
                    Some((r, ang, v)) => at.push((t, r, ang, v)),
                    None => eprintln!("solver-ticks: `at` {t}: not understood: {stmt}"),
                }
            }
        }
    }
    // Harness: RUSTUDIO_SOLVER_REPLAY=FILE:T[,T2..] puts every tracked body at the
    // recorded state of tick T (a Studio run's columns) before the step to T+1.
    let replay: Option<(HashMap<(usize, String), Vec<f64>>, Vec<usize>)> = std::env::var("RUSTUDIO_SOLVER_REPLAY").ok().and_then(|v| {
        let (file, ts) = v.rsplit_once(':')?;
        let ts: Vec<usize> = ts.split(',').filter_map(|x| x.parse().ok()).collect();
        let raw = std::fs::read(file).ok()?;
        let text = if file.ends_with(".gz") {
            use std::io::Read;
            let mut s = String::new();
            flate2::read::GzDecoder::new(&raw[..]).read_to_string(&mut s).ok()?;
            s
        } else {
            String::from_utf8(raw).ok()?
        };
        let mut m = HashMap::new();
        for l in text.lines().filter(|l| !l.starts_with('#')) {
            let f: Vec<&str> = l.split_whitespace().collect();
            if f.len() < 20 {
                continue;
            }
            let Ok(t) = f[0].parse::<usize>() else { continue };
            if ts.contains(&t) {
                m.insert((t, f[1].to_string()), f[2..20].iter().filter_map(|x| x.parse().ok()).collect());
            }
        }
        Some((m, ts))
    });
    for t in 1..=ticks {
        for &(_, r, ang, v) in at.iter().filter(|a| a.0 == t) {
            if ang {
                p.set_angular_velocity(r, v);
            } else {
                p.set_velocity(r, v);
            }
        }
        if let Some((m, ts)) = &replay {
            if ts.contains(&(t - 1)) {
                for (n, r) in &track {
                    let Some(c) = m.get(&(t - 1, n.clone())) else { continue };
                    let pos = Vec3::new((c[0] + origin[0]) as f32, (c[1] + origin[1]) as f32, (c[2] + origin[2]) as f32);
                    let g = |i: usize| c[3 + i] as f32;
                    let rot = glam::Mat3::from_cols(Vec3::new(g(0), g(3), g(6)), Vec3::new(g(1), g(4), g(7)), Vec3::new(g(2), g(5), g(8)));
                    let xf = rustudio_core::math::Xform { pos, rot };
                    p.harness_set_state(*r, &xf, Vec3::new(c[12] as f32, c[13] as f32, c[14] as f32), Vec3::new(c[15] as f32, c[16] as f32, c[17] as f32));
                }
            }
        }
        if std::env::var_os("RUSTUDIO_SOLVER_CONTACT_TRACE").is_some() {
            eprintln!("# harness tick {t}");
        }
        p.step(&mut d, h);
        if per_tick {
            let (v, _) = rustudio_core::bench::section_total("sv_collide");
            let (a, _) = rustudio_core::bench::section_total("ph_step_thread");
            if v > 0.0 {
                writeln!(w, "# tick {t} sv_collide {:.3}", v - last).ok();
                last = v;
            } else {
                writeln!(w, "# tick {t} step {:.3}", a - last).ok();
                last = a;
            }
        }
        print(&mut w, t, &d, &p);
    }
    // RUSTUDIO_BENCH_INSTR=1 (with RUSTUDIO_SOLVER_SERIAL=1): the world steps'
    // cost, and the Studio path's stages.
    if std::env::var_os("RUSTUDIO_BENCH_INSTR").is_some() {
        let mut line = String::from("# cost (M instr per tick)");
        for k in ["ph_step_thread", "sv_collide", "sv_gather", "sv_islands", "sv_solve", "sv_integrate"] {
            let (v, n) = rustudio_core::bench::section_total(k);
            if n > 0 {
                line += &format!(" {k} {:.4}", v / n as f64);
            }
        }
        writeln!(w, "{line}").ok();
    }
    if let Some(st) = p.studio_stats() {
        writeln!(w, "# studio stats {st:?}").ok();
    }
    w.flush().ok();
    Ok(())
}
