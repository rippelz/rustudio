//! Plugin commands (no window, usable headless):
//!
//! ```text
//! rustudio --list-plugins [--json]
//! rustudio --install-plugin <assetId>      Get (if free and not owned) + download + store
//! rustudio --uninstall-plugin <assetId>    remove from rustudio's store
//! rustudio --enable-plugin <id>            id: cloud_<n>, user_<file>, an asset id or a file name
//! rustudio --disable-plugin <id>
//! rustudio --update-plugin <assetId>       install the newest version
//! rustudio --sync-plugins                  read the account's installed plugins from Roblox
//! rustudio --import-studio-plugins [N]     list Studio installs, or import install N
//! rustudio --search-plugins <words>        Creator Store plugin search
//! rustudio --publish-plugin <file.rbxm|rbxmx> <name> [--overwrite <assetId>] [--dry-run]
//! ```
//! Plugin roots: `RUSTUDIO_PLUGIN_ROOT` overrides `<data>/rustudio`,
//! `RUSTUDIO_PLUGIN_USER` the account id used for installed plugins.

use rustudio_account::plugins as rp;

use super::{import, manager, Store};

/// Runs a plugin command if `argv` has one. Some(exit code) when it did.
pub fn run(argv: &[String]) -> Option<i32> {
    let pos = |f: &str| argv.iter().position(|a| a == f);
    let arg = |f: &str| pos(f).and_then(|i| argv.get(i + 1)).cloned();
    let has = |f: &str| pos(f).is_some();
    let cmds = [
        "--list-plugins", "--install-plugin", "--uninstall-plugin", "--enable-plugin", "--disable-plugin",
        "--update-plugin", "--sync-plugins", "--import-studio-plugins", "--search-plugins", "--publish-plugin",
    ];
    if !cmds.iter().any(|c| has(c)) {
        return None;
    }
    crate::studio_settings::init(false);
    let mut store = Store::open_default();
    let id_arg = |f: &str| -> Result<u64, String> {
        arg(f).and_then(|v| v.parse().ok()).ok_or_else(|| format!("{f} needs an asset id"))
    };
    let r: Result<(), String> = (|| {
        if has("--list-plugins") {
            list(&store, has("--json"));
        } else if has("--enable-plugin") || has("--disable-plugin") {
            let on = has("--enable-plugin");
            let id = arg(if on { "--enable-plugin" } else { "--disable-plugin" }).ok_or("which plugin?")?;
            let p = store.set_enabled(&id, on)?;
            println!("{} {}", p.id, if p.enabled { "enabled" } else { "disabled" });
        } else if has("--uninstall-plugin") {
            let a = id_arg("--uninstall-plugin")?;
            store.uninstall(a)?;
            println!("removed cloud_{a} from rustudio (your Roblox account is unchanged)");
        } else if has("--import-studio-plugins") {
            let found = import::detect();
            match arg("--import-studio-plugins").and_then(|n| n.parse::<usize>().ok()) {
                None => {
                    for (i, s) in found.iter().enumerate() {
                        let items = import::plan(s, store.user_id);
                        println!("{i}: {} ({} item(s) to copy) {:?} {:?}", s.label, items.len(), s.plugin_dirs, s.roblox_dirs);
                    }
                    if found.is_empty() {
                        println!("no Studio installs found");
                    }
                }
                Some(n) => {
                    let s = found.get(n).ok_or("no such install")?;
                    let plan = import::plan(s, store.user_id);
                    let rep = import::run(&mut store, &plan);
                    for c in &rep.copied {
                        println!("copied  {c}");
                    }
                    for c in &rep.skipped {
                        println!("skipped {c}");
                    }
                    for c in &rep.failed {
                        println!("FAILED  {c}");
                    }
                }
            }
        } else {
            // Commands that need the session.
            if std::env::var_os("RUSTUDIO_NO_SESSION").is_some() {
                return Err(rp::Failure::SignedOut.describe());
            }
            let client = rustudio_account::Client::open(rustudio_account::AccountConfig::load().map_err(|e| e.0)?)
                .map_err(|e| e.describe())?;
            let s = &client.session;
            if store.user_id != client.user_id() {
                store = Store::open(store.root.clone(), store.plugins_dir.clone(), client.user_id());
            }
            if has("--sync-plugins") {
                let rows = rp::installed(s).map_err(|f| f.describe())?;
                let ids: Vec<u64> = rows.iter().map(|r| r.plugin_asset_id).collect();
                let info = rp::plugin_info(s, &ids).map_err(|f| f.describe())?;
                let det = rp::item_details(s, &ids).map_err(|f| f.describe())?;
                store.merge_cloud(&rows);
                store.merge_details(&info, &det);
                store.save().map_err(|e| e.to_string())?;
                list(&store, has("--json"));
            } else if has("--install-plugin") || has("--update-plugin") {
                let a = id_arg(if has("--install-plugin") { "--install-plugin" } else { "--update-plugin" })?;
                let (meta, bytes) = manager::fetch_for_install(s, client.user_id(), a, has("--install-plugin")).map_err(|f| f.describe())?;
                let n = super::check_model_bytes(&bytes)?;
                let p = store.install_bytes(meta, &bytes)?;
                println!("installed {} \"{}\" v{} ({} bytes, {n} instances) -> {}", p.id, p.name, p.version.unwrap_or(0), bytes.len(), p.path.display());
            } else if has("--search-plugins") {
                let i = pos("--search-plugins").unwrap();
                let q = argv[i + 1..].iter().take_while(|a| !a.starts_with("--")).cloned().collect::<Vec<_>>().join(" ");
                for h in rp::search(s, &q, None).map_err(|f| f.describe())?.hits {
                    println!("{:>16} {:<5} {:<24} {}", h.id, if h.is_free { "free" } else { "paid" }, h.creator_name, h.name);
                }
            } else if has("--publish-plugin") {
                let i = pos("--publish-plugin").unwrap();
                let file = argv.get(i + 1).ok_or("--publish-plugin FILE NAME")?;
                let name = argv.get(i + 2).ok_or("--publish-plugin FILE NAME")?.clone();
                let bytes = std::fs::read(file).map_err(|e| format!("{file}: {e}"))?;
                super::check_model_bytes(&bytes)?;
                let bytes = to_binary(&bytes)?;
                let overwrite = arg("--overwrite").and_then(|v| v.parse().ok());
                let req = rp::PublishRequest { overwrite, name, description: arg("--description").unwrap_or_default(), group_id: None };
                let (m, u) = req.target();
                println!("{m} {u} request={} ({} bytes of model)", req.request_json(client.user_id()), bytes.len());
                if !has("--dry-run") {
                    let id = rp::publish(s, &req, &bytes).map_err(|f| f.describe())?;
                    println!("published plugin asset {id}");
                }
            }
        }
        Ok(())
    })();
    Some(match r {
        Ok(()) => 0,
        Err(e) => {
            eprintln!("{e}");
            1
        }
    })
}

/// A model file as binary (.rbxm), what Upload.ashx takes.
pub fn to_binary(bytes: &[u8]) -> Result<Vec<u8>, String> {
    if bytes.starts_with(b"<roblox!") {
        return Ok(bytes.to_vec());
    }
    let dom = rbx_xml::from_reader_default(bytes).map_err(|e| e.to_string())?;
    let mut out = Vec::new();
    rbx_binary::to_writer(&mut out, &dom, dom.root().children()).map_err(|e| e.to_string())?;
    Ok(out)
}

fn list(store: &Store, json: bool) {
    let list = store.list();
    if json {
        let v: Vec<serde_json::Value> = list
            .iter()
            .map(|p| {
                serde_json::json!({
                    "id": p.id, "name": p.name, "kind": p.kind_label(), "path": p.path,
                    "enabled": p.enabled, "present": p.present, "assetId": p.asset_id, "version": p.version,
                    "latestVersion": p.latest_version, "creator": p.creator, "updated": p.updated,
                    "hasUpdate": p.has_update(),
                })
            })
            .collect();
        println!("{}", serde_json::to_string_pretty(&v).unwrap());
        return;
    }
    println!("local Plugins folder: {}", store.plugins_dir.display());
    println!("installed plugins:    {}", store.installed_dir().display());
    for p in &list {
        println!(
            "{:<3} {:<34} {:<9} {:<8} {:<16} {:<18} {}{}",
            if p.enabled { "on" } else { "off" },
            p.id,
            p.kind_label(),
            if p.present { "" } else { "missing" },
            p.version.map(|v| format!("v{v}")).unwrap_or_default(),
            p.creator.clone().unwrap_or_default(),
            p.name,
            if p.has_update() { "  [update]" } else { "" }
        );
    }
    println!("{} plugin(s), {} enabled", list.len(), list.iter().filter(|p| p.enabled).count());
}
