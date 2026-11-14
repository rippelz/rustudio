/// Studio: mulss, cvttss2si, low signed word; no clamp.
/// Call with chunk-local coordinates, before adding the chunk translation.
pub fn quantize(value: f32) -> f32 {
    ((value * 512.0).trunc() as i32 as i16) as f32 / 512.0
}

pub fn emit(p: [f32; 3], base: [f32; 3], tip: [f32; 3], width: f32, height: f32) -> [[f32; 4]; 3] {
    let mut out = [[0.0; 4]; 3];
    for j in 0..3 {
        out[0][j] = quantize(p[j] - base[j] * width);
        out[1][j] = quantize(p[j] + base[j] * width);
        out[2][j] = quantize(p[j] + tip[j] * height);
    }
    out[0][3] = quantize(0.1);
    out[1][3] = quantize(0.1);
    out[2][3] = quantize(height + 0.1);
    out
}

#[test]
fn grass_native_same_input() {
    let data = include_bytes!("../tests/fixtures/grass/quantization.bin");
    for row in data.chunks_exact(68) {
        let f: Vec<f32> = row[..44].chunks_exact(4).map(|b| f32::from_le_bytes(b.try_into().unwrap())).collect();
        let actual = emit(f[0..3].try_into().unwrap(), f[3..6].try_into().unwrap(), f[6..9].try_into().unwrap(), f[9], f[10]);
        for (a,b) in actual.iter().flatten().zip(row[44..].chunks_exact(2)) {
            assert_eq!(*a, i16::from_le_bytes(b.try_into().unwrap()) as f32 / 512.0);
        }
    }
    assert!(data.len() >= 68 * 100);
    assert_eq!(quantize(-0.1009), -51.0/512.0);
    assert_eq!(quantize(64.0), -64.0); // low word, no saturation
}
