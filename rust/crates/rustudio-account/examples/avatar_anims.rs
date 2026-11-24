//! Lists the signed-in avatar's worn animation packs and saves each package
//! model (read-only). usage: avatar_anims OUTDIR
fn main() {
    let out = std::path::PathBuf::from(std::env::args().nth(1).unwrap());
    let c = rustudio_account::Client::open(rustudio_account::AccountConfig::load().unwrap()).unwrap();
    let v = c.avatar_json().unwrap();
    let info = rustudio_account::avatar::parse_avatar(c.user_id(), &v);
    println!("r15={} assets={}", info.r15, info.assets.len());
    for a in &info.assets {
        println!("{} {} {} ({})", a.id, a.type_id, a.type_name, a.name);
        if a.type_name.ends_with("Animation") {
            let b = c.download_asset(a.id, None).unwrap();
            std::fs::write(out.join(format!("{}.rbxm", a.id)), &b).unwrap();
        }
    }
}
