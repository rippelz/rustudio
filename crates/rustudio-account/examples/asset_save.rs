//! Downloads one asset with the signed-in session to a file (read-only).
//! usage: asset_save ID OUT
fn main() {
    let mut a = std::env::args().skip(1);
    let id: u64 = a.next().unwrap().parse().unwrap();
    let out = a.next().unwrap();
    let c = rustudio_account::Client::open(rustudio_account::AccountConfig::load().unwrap()).unwrap();
    std::fs::write(&out, c.download_asset(id, None).unwrap()).unwrap();
}
