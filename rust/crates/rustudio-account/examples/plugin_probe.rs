//! Probe the plugin endpoints Studio uses, over the signed-in session.
//! Read-only unless `acquire` or `publish` is given.
//!
//! usage:
//!   plugin_probe installed            the account's installed plugins + update state
//!   plugin_probe permissions          plugin permission rows
//!   plugin_probe search QUERY         Creator Store plugin search
//!   plugin_probe details ID...        toolbox item details
//!   plugin_probe download ID [VER] OUT  save a plugin file
//!   plugin_probe owns ID              inventory check
//!   plugin_probe acquire ID           take a FREE plugin into the inventory (writes to the account)
//!   plugin_probe publish-url NAME     print the Upload.ashx request Publish as Plugin would send
use rustudio_account::plugins as p;

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let c = rustudio_account::Client::open(rustudio_account::AccountConfig::load().unwrap()).unwrap_or_else(|e| {
        eprintln!("{}", e.describe());
        std::process::exit(2)
    });
    let s = &c.session;
    let ids = || args[1..].iter().filter_map(|a| a.parse().ok()).collect::<Vec<u64>>();
    match args.first().map(String::as_str) {
        Some("installed") => {
            let list = p::installed(s).unwrap();
            let ids: Vec<u64> = list.iter().map(|x| x.plugin_asset_id).collect();
            let info = p::plugin_info(s, &ids).unwrap_or_default();
            let det = p::item_details(s, &ids).unwrap_or_default();
            for x in &list {
                let cur = info.get(&x.plugin_asset_id).map(|i| i.version_id).unwrap_or(0);
                println!(
                    "{:>15} v{:<15} {:<8} upd={:<5} by {:<20} {}",
                    x.plugin_asset_id,
                    x.asset_version_id,
                    if x.enabled { "enabled" } else { "disabled" },
                    p::has_update(x.asset_version_id, cur),
                    det.get(&x.plugin_asset_id).map(|d| d.creator_name.as_str()).unwrap_or("?"),
                    x.plugin_name
                );
            }
            println!("{} plugins", list.len());
        }
        Some("permissions") => {
            for r in p::permissions(s).unwrap() {
                println!("{:>15} {:<16} {:<5} {:?}", r.plugin_id, r.kind, r.allowed, r.domain);
            }
        }
        Some("search") => {
            for h in p::search(s, &args[1..].join(" "), None).unwrap().hits {
                println!("{:>15} free={:<5} {:<20} {}", h.id, h.is_free, h.creator_name, h.name);
            }
        }
        Some("details") => println!("{:#?}", p::item_details(s, &ids())),
        Some("owns") => println!("{:?}", p::owns(s, c.user_id(), ids()[0])),
        Some("download") => {
            let id: u64 = args[1].parse().unwrap();
            let (ver, out) = if args.len() > 3 { (args[2].parse().ok(), &args[3]) } else { (None, &args[2]) };
            match p::download(s, id, ver) {
                Ok(b) => {
                    std::fs::write(out, &b).unwrap();
                    println!("{} bytes -> {out} (head {:?})", b.len(), String::from_utf8_lossy(&b[..b.len().min(8)]));
                }
                Err(e) => println!("failed: {}", e.describe()),
            }
        }
        Some("acquire") => println!("{:?}", p::acquire_free(s, ids()[0]).map_err(|e| e.describe())),
        Some("publish-url") => {
            let r = p::PublishRequest { overwrite: None, name: args[1..].join(" "), description: String::new(), group_id: None };
            let (m, u) = r.target();
            println!("{m} {u}\nrequest = {}", r.request_json(c.user_id()));
        }
        _ => eprintln!("see the usage at the top of examples/plugin_probe.rs"),
    }
}
