//! Offline: the property values the server's own records carried for every
//! instance of some classes in a captured join stream (RUSTUDIO_TC_STREAM_OUT
//! format), and the schema properties those records left out (sent at the
//! class's default). Reads a file; sends nothing.
//!
//!   cargo run -p rustudio-net --example tc_props_dump -- STREAM.bin Lighting Sky BloomEffect
use rustudio_net::replicator::{Decoder, RepOp};
use std::collections::HashSet;

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let data = std::fs::read(&args[0]).expect("read stream");
    let wanted: HashSet<&str> = args[1..].iter().map(String::as_str).collect();
    let mut dec = Decoder::new();
    let mut found = Vec::new();
    let mut p = 7;
    while p + 4 <= data.len() {
        let len = (u32::from_be_bytes(data[p..p + 4].try_into().unwrap()) & 0x7fff_ffff) as usize;
        let frame = &data[p + 4..p + 4 + len];
        p += 4 + len;
        if frame.is_empty() {
            continue;
        }
        for op in dec.decode_frame(frame[0], &frame[1..]) {
            match op {
                RepOp::NewInstance { id, class, parent, props } if wanted.contains(class.as_str()) => {
                    found.push((id, class, parent, props));
                }
                RepOp::ChangeProperty { id, name, value } if found.iter().any(|(i, ..)| *i == id) => {
                    println!("change #{id:x}.{name} = {value:?}");
                }
                _ => {}
            }
        }
    }
    let schema = dec.schema.expect("schema");
    for (id, class, parent, props) in &found {
        println!("== {class} #{id:x} parent {parent:x?}");
        for (n, v) in props {
            let mut s = format!("{v:?}");
            if s.len() > 160 {
                s.truncate(160);
                s.push('…');
            }
            println!("   sent {n} = {s}");
        }
        if let Some(c) = schema.classes.iter().find(|c| &c.name == class) {
            let sent: HashSet<&str> = props.iter().map(|(n, _)| n.as_str()).collect();
            let omitted: Vec<&str> = c.props.iter().map(|p| p.name.as_str()).filter(|n| !sent.contains(n)).collect();
            println!("   omitted (schema props not in the record): {}", omitted.join(" "));
        }
    }
}
