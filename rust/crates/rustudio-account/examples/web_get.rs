//! GET a Roblox web URL with the signed-in session and print status + body (read-only).
fn main() {
    let url = std::env::args().nth(1).unwrap();
    let c = rustudio_account::Client::open(rustudio_account::AccountConfig::load().unwrap()).unwrap();
    let (st, t) = c.session.get(&url).unwrap();
    println!("{st} {t}");
}
