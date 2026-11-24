//! Saves a local .rbxl as a Saved (unpublished) version of a place you own,
//! through the same path as File > Save to Roblox. Writes to Roblox: point
//! it only at a scratch place. usage: place_upload UNIVERSE PLACE FILE
fn main() {
    let mut a = std::env::args().skip(1);
    let universe: u64 = a.next().unwrap().parse().unwrap();
    let place: u64 = a.next().unwrap().parse().unwrap();
    let bytes = std::fs::read(a.next().unwrap()).unwrap();
    let c = rustudio_account::Client::open(rustudio_account::AccountConfig::load().unwrap()).unwrap();
    let api = rustudio_account::cloud::CloudApi { session: c.session, ep: Default::default() };
    let t = std::time::Instant::now();
    let r = api.upload_place(universe, place, &bytes, false, rustudio_account::VersionType::Saved);
    println!("{} bytes -> {r:?} in {:?}", bytes.len(), t.elapsed());
}
