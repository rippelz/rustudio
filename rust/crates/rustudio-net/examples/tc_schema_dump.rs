//! Offline: what a captured join stream (RUSTUDIO_TC_STREAM_OUT format) says
//! about some classes. Prints each class's replicated properties and wire
//! types, the props the server's own NewObject carried for the first instance
//! of the class, and SET_GLOBALS' service table with the services that never
//! arrive as NewInstance. Reads a file; sends nothing.
//!
//!   cargo run -p rustudio-net --example tc_schema_dump -- STREAM.bin LocalScript Folder
use rustudio_net::replicator::{packet_id, top_services, Decoder, RepOp};
use std::collections::{HashMap, HashSet};

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let data = std::fs::read(&args[0]).expect("read stream");
    let wanted: HashSet<&str> = args[1..].iter().map(String::as_str).collect();
    let mut dec = Decoder::new();
    let mut services = Vec::new();
    let mut created = HashSet::new();
    let mut children: HashMap<u64, Vec<String>> = HashMap::new();
    let mut shown = HashSet::new();
    let mut p = 7;
    while p + 4 <= data.len() {
        let len = (u32::from_be_bytes(data[p..p + 4].try_into().unwrap()) & 0x7fff_ffff) as usize;
        let frame = &data[p + 4..p + 4 + len];
        p += 4 + len;
        if frame.is_empty() {
            continue;
        }
        if frame[0] == packet_id::SET_GLOBALS {
            if let Some(schema) = &dec.schema {
                services = top_services(schema, &frame[1..]).expect("service table");
            }
        }
        for op in dec.decode_frame(frame[0], &frame[1..]) {
            let RepOp::NewInstance { id, class, parent, props } = op else { continue };
            created.insert(id);
            if id > u32::MAX as u64 {
                println!("peer-scoped NewInstance {class} scope {} index {:#x} parent {parent:x?}", id >> 32, id as u32);
            }
            if let Some(parent) = parent {
                children.entry(parent).or_default().push(class.clone());
            }
            if wanted.contains(class.as_str()) && shown.insert(class.clone()) {
                let names: Vec<_> = props.iter().map(|(n, _)| n.as_str()).collect();
                println!("server NewObject {class}: {}", names.join(" "));
            }
        }
    }
    let schema = dec.schema.expect("schema");
    for name in &args[1..] {
        match schema.classes.iter().find(|c| &c.name == name) {
            Some(c) => println!("{name}: {}", c.props.iter().map(|p| format!("{}:{:#x}", p.name, p.ty)).collect::<Vec<_>>().join(" ")),
            None => println!("{name}: not in schema"),
        }
    }
    println!("{} listed services", services.len());
    for (id, class) in &services {
        if !created.contains(id) {
            let kids = children.get(id).map_or(0, Vec::len);
            println!("  never sent as NewInstance: {class} {id:#x} ({kids} children name it)");
        }
    }
}
