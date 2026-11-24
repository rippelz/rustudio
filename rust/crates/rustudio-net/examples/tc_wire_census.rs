//! Offline: how the server itself encodes the values of some classes in a
//! captured join stream (RUSTUDIO_TC_STREAM_OUT format). Prints, per class,
//! one decoded NewInstance (property = value) and the decoder's error count.
//! Reads a file; sends nothing.
//!
//!   cargo run -p rustudio-net --example tc_wire_census -- STREAM.bin Motor6D Decal
use rustudio_net::replicator::{Decoder, RepOp};
use std::collections::HashMap;

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let data = std::fs::read(&args[0]).expect("read stream");
    let wanted: Vec<&str> = args[1..].iter().map(String::as_str).collect();
    let mut dec = Decoder::new();
    let mut seen: HashMap<String, usize> = HashMap::new();
    let mut p = 7;
    let mut total = 0usize;
    let mut rotated = 0usize;
    while p + 4 <= data.len() {
        let len = (u32::from_be_bytes(data[p..p + 4].try_into().unwrap()) & 0x7fff_ffff) as usize;
        let frame = &data[p + 4..p + 4 + len];
        p += 4 + len;
        if frame.is_empty() {
            continue;
        }
        for op in dec.decode_frame(frame[0], &frame[1..]) {
            let RepOp::NewInstance { class, props, .. } = op else { continue };
            total += 1;
            for (_, v) in &props {
                if let rustudio_net::replicator::PropValue::CFrame { rot, .. } = v {
                    if rot.iter().any(|f| f.abs() > 1e-4 && (f.abs() - 1.0).abs() > 1e-4) {
                        rotated += 1;
                    }
                }
            }
            let n = seen.entry(class.clone()).or_default();
            *n += 1;
            if *n == 1 && wanted.contains(&class.as_str()) {
                println!("== {class}");
                for (name, v) in &props {
                    println!("   {name} = {}", format!("{v:?}").chars().take(140).collect::<String>());
                }
            }
        }
    }
    println!("{total} instances, {rotated} non-axis CFrames, decoder errors: {:?}", dec.errors.len());
}
