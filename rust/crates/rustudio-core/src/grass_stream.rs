/// Matched caller Studio; bounded native length range .1..=1.
pub fn length_factor(length: f32) -> f32 {
    if length > 0.7 { 1.0 + (70.0_f32 * 0.01 - 1.0) * ((length - 0.7) / 0.3) }
    else if length < 0.7 { 1.0 + (200.0_f32 * 0.01 - 1.0) * ((length - 0.7) / -0.5999999642372131) }
    else { 1.0 }
}
pub fn sample_count(length: f32, density: f32, near: bool) -> usize {
    (((if near {16.0} else {13.0}) * (length_factor(length) * density)).trunc() as usize).min(32)
}
// Matched default emitter stream.
// Terrain topology, input triangle order and batch output transposition are
// separate from this stream; source chunk geometry is not native bit parity.
pub fn fold(u: f32, v: f32) -> (f32, f32) {
    let excess = (u + v - 1.0).max(0.0);
    (u - excess, v - excess)
}
pub struct Stream(u64);
impl Stream {
    pub fn from_seed(seed: u64) -> Self {
        Self(seed.wrapping_mul(0x5851_f42d_4c95_7f2d).wrapping_add(0x399d_2694_6951_29de))
    }
    pub fn new(local_first: [f32; 3]) -> Self {
        // Caller sign-extends packed i16, then performs UNSIGNED division.
        let q = local_first.map(|v| ((v * 512.0).trunc() as i16 as i64 as u64) / 2048);
        let seed = q[0].wrapping_add(q[1].wrapping_shl(16)).wrapping_add(q[2].wrapping_shl(32));
        Self(seed.wrapping_mul(0x5851_f42d_4c95_7f2d).wrapping_add(0x399d_2694_6951_29de))
    }
    pub fn next(&mut self) -> u32 {
        let s = self.0;
        self.0 = s.wrapping_mul(0x5851_f42d_4c95_7f2d).wrapping_add(105);
        (((s >> 45) ^ (s >> 27)) as u32).rotate_right((s >> 59) as u32)
    }
    pub fn range(&mut self, lo: f64, hi: f64) -> f32 {
        let low = self.next() as u64;
        let high = self.next() as u64;
        let unit = ((high << 32) | low) as f64 * (1.0 / 18446744073709551616.0);
        ((1.0 - unit) * lo + unit * hi).clamp(lo, hi) as f32
    }
    pub fn shuffle(&mut self, count: usize) {
        self.shuffled_slots(count);
    }
    /// Native output permutation, without changing the geometry PRNG stream.
    pub fn shuffled_slots(&mut self, count: usize) -> Vec<usize> {
        let mut slots: Vec<_> = (0..count).collect();
        for i in 1..count {
            let n = (i + 1) as u64;
            loop {
                let v = self.next() as u64;
                if u32::MAX as u64 / n > v / n || u32::MAX as u64 % n == i as u64 {
                    slots.swap(i, (v % n) as usize);
                    break;
                }
            }
        }
        slots
    }
    pub fn blade(&mut self) -> (f32, f32, usize) {
        let h = self.range(1.65_f32 as f64, 3.8_f32 as f64);
        let w = self.range(0.1_f32 as f64, 0.3_f32 as f64);
        let template = (self.next() >> 22) as usize;
        (h, w, template)
    }
}
/// Producer Studio: hash ordered voxel-space positions, then draw u/v.
/// The hash reads its second qword first (XYZ0 carrier: Z,0,X,Y).
pub fn material_uv(world: [[f32; 3]; 3]) -> (f32, f32) {
    let hashes = world.map(|v| {
        let xyz = v.map(|x| ((x * 0.25) * 0.25).floor());
        let mut h = 0xcf470aac6cb293d2f52f88bf32307f8f_u128;
        for x in [xyz[2], 0.0, xyz[0], xyz[1]] {
            for byte in x.to_le_bytes() {
                h = h.wrapping_mul((1_u128 << 88) + 0x159) ^ byte as u128;
            }
        }
        let h = (h >> 64) as u64 ^ h as u64;
        (h >> 32) ^ (h & 0xffff_ffff)
    });
    let mut seed = hashes[0].wrapping_add(0x9e3779b9);
    for h in &hashes[1..] {
        seed ^= h.wrapping_add(seed << 6).wrapping_add(seed >> 2).wrapping_add(0x9e3779b9);
    }
    let mut stream = Stream(seed.wrapping_mul(0x5851_f42d_4c95_7f2d).wrapping_add(0x399d_2694_6951_29de));
    (stream.range(0.15_f32 as f64, 0.45_f32 as f64), stream.range(0.15_f32 as f64, 0.45_f32 as f64))
}
pub fn material_weight(world: [[f32; 3]; 3], material: [f32; 3]) -> f32 {
    let (u, v) = material_uv(world);
    let w = (1.0_f32 - u) - v;
    let weight = (material[1] * v + material[0] * u) + material[2] * w;
    (weight * 16384.0).trunc() / 16384.0
}
#[cfg(test)] mod tests {
    use super::*;
    #[test] fn captured_mixed_material_sampler() {
        let rows: serde_json::Value = serde_json::from_str(include_str!("../../../tests/fixtures/grass/mixed-material-sampler.json")).unwrap();
        for row in rows.as_array().unwrap() {
            let world = std::array::from_fn(|i| std::array::from_fn(|j| row["world"][i][j].as_f64().unwrap() as f32));
            let indicators = std::array::from_fn(|i| row["indicators"][i].as_f64().unwrap() as f32);
            assert_eq!((material_weight(world, indicators)*16384.0) as i32, row["q14"].as_i64().unwrap() as i32);
        }
    }
    #[test] fn captured_material_sampler() {
        let rows: serde_json::Value = serde_json::from_str(include_str!("../../../tests/fixtures/grass/material-weight-sampler.json")).unwrap();
        for row in rows.as_array().unwrap() {
            let world = std::array::from_fn(|i| std::array::from_fn(|j| row["world"][i][j].as_f64().unwrap() as f32));
            let (u,v) = material_uv(world);
            assert_eq!(u, row["uv"][0].as_f64().unwrap() as f32);
            assert_eq!(v, row["uv"][1].as_f64().unwrap() as f32);
            assert_eq!((material_weight(world, [1.0;3])*16384.0) as i32, row["q14"].as_i64().unwrap() as i32);
        }
    }
    #[test] fn captured_grass_template_stream() {
        for (p, expected) in [([0., -26., 4.], [881,429,776,53,78,260,413,592,448,581,856,385,183]),
                              ([4., -26., 4.], [231,543,780,336,879,610,858,966,291,359,671,896,909])] {
            let mut r = Stream::new(p); r.shuffle(13);
            for id in expected { assert_eq!(r.blade().2, id); }
        }
    }
    #[test] fn captured_grass_roots_and_templates() {
        let rows: serde_json::Value = serde_json::from_str(include_str!("../../../tests/fixtures/grass/placement.json")).unwrap();
        for row in rows.as_array().unwrap() {
            let first: [f32; 3] = std::array::from_fn(|i| row["first"][i].as_f64().unwrap() as f32);
            let near = matches!(row["event"].as_u64().unwrap(), 251 | 257);
            let samples: &[[f32; 2]] = if near { &crate::mech::GRASS_NEAR_SAMPLE_POINTS } else { &crate::mech::GRASS_SAMPLE_POINTS };
            let index = row["sample"].as_u64().unwrap() as usize;
            let mut r = Stream::new(first); r.shuffle(if near {16} else {13});
            for _ in 0..index { r.blade(); }
            let (height, _, template) = r.blade();
            assert_eq!(template, row["template"].as_u64().unwrap() as usize);
            let scale = 1.0275_f32 * if near { 0.85 } else { 1.0 };
            let tip_w = ((height * scale + 0.1) * 512.0).trunc() as i32;
            let tolerance = if matches!(row["event"].as_u64().unwrap(), 248 | 251) { 1 } else { 0 };
            assert!((tip_w - row["native_tip_w"].as_i64().unwrap() as i32).abs() <= tolerance);
            let [u, v] = samples[index].map(|a| a * 0.5 + 0.5);
            let (u, v) = fold(u, v);
            let point = if first[0] == 0.0 { [4.0*u, -26.0, 4.0-4.0*u-4.0*v] }
                        else { [4.0-4.0*v, -26.0, 4.0-4.0*u] };
            // Captured packed terrain endpoints/material weight are not unique
            // from grass alone. Bound the reconstruction by two packing steps.
            for j in 0..3 { assert!((point[j] - row["root"][j].as_f64().unwrap() as f32).abs() < 2.0/512.0); }
        }
    }

}

#[test]
#[ignore = "reads local evidence under checks/"]
fn retained_nondefault_ordered_geometry() {
    let base = std::path::Path::new(concat!(env!("CARGO_MANIFEST_DIR"), "/../../checks/grass-full-re/nondefault"));
    let text = std::fs::read_to_string(base.join("capture.jsonl")).unwrap();
    let decode = |s: &str| (0..s.len()).step_by(2).map(|i|u8::from_str_radix(&s[i..i+2],16).unwrap()).collect::<Vec<_>>();
    let templates = std::fs::read(base.join("grass_templates.bin")).unwrap();
    let mut checked=0;
    for line in text.lines() {
        let row: serde_json::Value=serde_json::from_str(line).unwrap();
        if row["kind"]!="emitter" || row["buffers"].as_array().unwrap().is_empty() {continue;}
        let length=row["case"]["length"].as_f64().unwrap() as f32;
        let density=row["case"]["density"].as_f64().unwrap() as f32;
        let header=decode(row["output_header"].as_str().unwrap());
        let count=u32::from_le_bytes(header[76..80].try_into().unwrap()) as usize;
        let near=count==sample_count(length,density,true);
        assert_eq!(count,sample_count(length,density,near));
        let points=if near {crate::mech::GRASS_NEAR_SAMPLE_POINTS}else{crate::mech::GRASS_SAMPLE_POINTS};
        let input=decode(row["input"].as_str().unwrap());let domains=input.len()/28;
        let output=std::fs::read(base.join(row["buffers"][0]["file"].as_str().unwrap())).unwrap();
        for (di,record) in input.chunks_exact(28).enumerate() {
            let f=|i:usize|i16::from_le_bytes(record[i*2..i*2+2].try_into().unwrap()) as f32/512.0;
            let first=std::array::from_fn(|a|f(a));let weight=f(9)*512.0/16384.0;
            let mut stream=Stream::new(first);let order=stream.shuffled_slots(count);
            let real=((count as f32*weight).trunc() as usize).max(3).min(count);
            for si in 0..real {
                let (u,v)=fold(weight*points[si][0]*0.5+0.5,weight*points[si][1]*0.5+0.5);
                let p=std::array::from_fn(|a|(f(a)+f(a+3)*u)+f(a+6)*v);
                let (h,w,id)=stream.blade();let component=|i:usize|f32::from_le_bytes(templates[id*24+i*4..id*24+i*4+4].try_into().unwrap());
                let size=(row["size"].as_f64().unwrap() as f32*if near {0.85}else{1.0})*weight;
                let actual=crate::terrain::grass_pack::emit(p,std::array::from_fn(component),std::array::from_fn(|i|component(i+3)),size*w,size*h);
                let slot=(order[si]*domains+di)*36;
                for v in 0..3 {for a in 0..4 {
                    let off=slot+v*12+a*2;let expected=i16::from_le_bytes(output[off..off+2].try_into().unwrap()) as f32/512.0;
                    assert_eq!(actual[v][a],expected,"length {length} density {density} near {near} domain {di} sample {si} vertex {v} axis {a}");
                }}checked+=1;
            }
            // Native fills unused allocation slots by copying the last real
            // blade. These are duplicates, not additional PRNG draws.
            for si in real..count {
                let slot=(order[si]*domains+di)*36;
                let previous=(order[si-1]*domains+di)*36;
                assert_eq!(&output[slot..slot+36], &output[previous..previous+36]);
            }
        }
    }
    assert!(checked>10000);println!("{checked} ordered native blades matched exactly");
}

/// Compare generated selected geometry with actual RenderDoc draw ranges. The
/// emitter output is independently byte-joined to each resident GPU allocation.
#[test]
#[ignore = "reads local evidence under checks/"]
fn retained_gpu_selected_ordinals() {
    let base=std::path::Path::new(concat!(env!("CARGO_MANIFEST_DIR"), "/../../checks/grass-full-re/gpu-ordinal"));
    let joins:serde_json::Value=serde_json::from_slice(&std::fs::read(base.join("emitter-gpu-join.json")).unwrap()).unwrap();
    let observations:serde_json::Value=serde_json::from_slice(&std::fs::read(base.join("resident-comparison.json")).unwrap()).unwrap();
    let templates=std::fs::read(base.join("grass_templates.bin")).unwrap();
    let mut selected=0;
    for batch in 0..4 {
        let row=&joins[batch]["matches"][0];
        let input_hex=row["input"].as_str().unwrap();
        let input:Vec<u8>=(0..input_hex.len()).step_by(2).map(|i|u8::from_str_radix(&input_hex[i..i+2],16).unwrap()).collect();
        let domains=input.len()/28;
        let near=batch<2;
        let count=sample_count(0.7,1.0,near);
        let points=if near {crate::mech::GRASS_NEAR_SAMPLE_POINTS}else{crate::mech::GRASS_SAMPLE_POINTS};
        let native=std::fs::read(base.join(row["file"].as_str().unwrap())).unwrap();
        let mut generated=std::collections::BTreeMap::new();
        for (di,record) in input.chunks_exact(28).enumerate() {
            let f=|i:usize|i16::from_le_bytes(record[i*2..i*2+2].try_into().unwrap()) as f32/512.0;
            let mut stream=Stream::new(std::array::from_fn(|a|f(a)));
            let order=stream.shuffled_slots(count);
            let weight=f(9)*512.0/16384.0;
            let real=((count as f32*weight).trunc() as usize).max(3).min(count);
            for si in 0..real {
                let (u,v)=fold(weight*points[si][0]*0.5+0.5,weight*points[si][1]*0.5+0.5);
                let root=std::array::from_fn(|a|(f(a)+f(a+3)*u)+f(a+6)*v);
                let (h,w,id)=stream.blade();
                let component=|i:usize|f32::from_le_bytes(templates[id*24+i*4..id*24+i*4+4].try_into().unwrap());
                let size=(row["size"].as_f64().unwrap() as f32*if near {0.85}else{1.0})*weight;
                generated.insert(order[si]*domains+di,crate::terrain::grass_pack::emit(root,std::array::from_fn(component),std::array::from_fn(|i|component(i+3)),size*w,size*h));
            }
            for si in real..count {
                let previous=generated[&(order[si-1]*domains+di)];
                generated.insert(order[si]*domains+di,previous);
            }
        }
        for obs in observations[batch]["observations"].as_array().unwrap() {
            let limit=obs["primitives"].as_u64().unwrap() as usize;
            let mut actual=std::collections::BTreeSet::new();
            for (&ordinal,geometry) in generated.range(..limit) {
                for v in 0..3 {for a in 0..4 {
                    let off=ordinal*36+v*12+a*2;
                    let expected=i16::from_le_bytes(native[off..off+2].try_into().unwrap()) as f32/512.0;
                    assert_eq!(geometry[v][a],expected,"batch {batch} ordinal {ordinal} vertex {v} axis {a}");
                }}
                actual.insert(ordinal);selected+=1;
            }
            // Compare allocated ordinals, including native copied fillers.
            assert_eq!(actual,(0..limit).collect(),"selected ordinal set batch {batch}");
        }
    }
    assert!(selected>10000);
    println!("{selected} selected native primitives match generated slots across out/back GPU captures");
}
