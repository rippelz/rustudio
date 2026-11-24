//! Prints the signed-in avatar-fetch reply (read-only).
fn main() {
    let c = rustudio_account::Client::open(rustudio_account::AccountConfig::load().unwrap()).unwrap();
    let (st, t) = c.session.get(&rustudio_account::avatar::avatar_fetch_url(0, c.user_id())).unwrap();
    println!("{st} {t}");
}
