//! Dialogs a plugin call opens. Studio shows a native dialog and the call
//! yields until it closes (measured in Studio, checks/plugins/services/
//! captures/dialogs.txt); here the call registers a prompt, yields (its
//! Luau side polls each frame), and the editor shows the dialog
//! (glue.rs) or the harness answers it: `--action plugin-prompt:<answer>@S`
//! answers the oldest open prompt. Answers: `cancel`, or the dialog's
//! value: a file path (several joined with `|`) for the file dialogs, a
//! number for the asset-id picker, `ok` for a confirmation.
//! `plugin-prompt:queue:<answer>` answers the oldest open prompt, or, with
//! none open, the next one this thread's plugins open (a run that answers
//! many dialogs in turn without timing them).

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Mutex;

#[derive(Clone, Debug, PartialEq)]
pub enum PromptKind {
    /// StudioService:PromptImportFile(s): an Open dialog (`multi`: several
    /// files), `filters` the extensions asked for (empty: all files).
    ImportFile { filters: Vec<String>, multi: bool },
    /// Plugin:ImportFbxRig / ImportFbxAnimation: Studio's "Open 3D File
    /// (FBX, OBJ, GLB, GLTF)" dialog.
    Import3d { rig: bool },
    /// Plugin:PromptForExistingAssetId: Studio's Asset Configuration
    /// window ("Choose an asset to import").
    ExistingAssetId { asset_type: String },
    /// Plugin:PromptSaveSelection: a Save As dialog for the selection.
    SaveSelection { name: String },
    /// PluginManager():ExportPlace / ExportSelection (Studio's "Export
    /// Place" / "Export Selection" save dialogs, .obj).
    Export { selection: bool },
    /// Plugin:SaveSelectedToRoblox: Studio's Asset Configuration window
    /// (Title = the first selected instance's name, Description, Save /
    /// Cancel). Save uploads the selection as a Model (`model_upload`).
    SaveToRoblox { name: String },
}

#[derive(Clone, Debug, PartialEq)]
pub enum Answer {
    Cancel,
    Paths(Vec<PathBuf>),
    Text(String),
}

struct Open {
    id: u64,
    kind: PromptKind,
    /// Whether the editor took it yet.
    shown: bool,
    /// The thread whose plugin VM opened it: its editor (or harness) shows
    /// and answers it. Plugin VMs never leave the thread that runs them,
    /// so two editors in one process (tests run in parallel) never see or
    /// answer each other's dialogs. Answers by id (a native dialog's
    /// worker thread) are not restricted.
    owner: std::thread::ThreadId,
}

#[derive(Default)]
struct State {
    next: u64,
    /// Open prompts, oldest first.
    open: Vec<Open>,
    answers: HashMap<u64, Answer>,
    /// `queue:` answers given while no prompt was open, oldest first.
    queued: Vec<(std::thread::ThreadId, String)>,
}

fn me() -> std::thread::ThreadId {
    std::thread::current().id()
}

static STATE: Mutex<Option<State>> = Mutex::new(None);

fn with<R>(f: impl FnOnce(&mut State) -> R) -> R {
    let mut g = STATE.lock().unwrap_or_else(|e| e.into_inner());
    f(g.get_or_insert_with(State::default))
}

/// Opens a prompt; poll it with [`poll`].
pub fn open(kind: PromptKind) -> u64 {
    let me = me();
    let (id, queued) = with(|s| {
        s.next += 1;
        let id = s.next;
        s.open.push(Open { id, kind: kind.clone(), shown: false, owner: me });
        let q = s.queued.iter().position(|(t, _)| *t == me).map(|i| s.queued.remove(i).1);
        (id, q)
    });
    if let Some(text) = queued {
        answer(id, answer_for(&kind, &text));
    }
    id
}

/// The answer, once given (the prompt is closed then).
pub fn poll(id: u64) -> Option<Answer> {
    with(|s| s.answers.remove(&id))
}

/// Prompts this thread's plugins opened that its editor hasn't shown yet.
pub fn take_new() -> Vec<(u64, PromptKind)> {
    let me = me();
    with(|s| {
        let mut out = Vec::new();
        for p in s.open.iter_mut().filter(|p| p.owner == me) {
            if !p.shown {
                p.shown = true;
                out.push((p.id, p.kind.clone()));
            }
        }
        out
    })
}

pub fn answer(id: u64, a: Answer) {
    with(|s| {
        if let Some(i) = s.open.iter().position(|p| p.id == id) {
            s.open.remove(i);
            s.answers.insert(id, a);
        }
    })
}

/// Whether any prompt this thread's plugins opened is open (the editor
/// keeps redrawing).
pub fn any_open() -> bool {
    let me = me();
    with(|s| s.open.iter().any(|p| p.owner == me))
}

/// The oldest prompt this thread's plugins opened.
fn oldest() -> Option<(u64, PromptKind)> {
    let me = me();
    with(|s| s.open.iter().find(|p| p.owner == me).map(|p| (p.id, p.kind.clone())))
}

/// The harness's `plugin-prompt:<answer>`: answers the oldest prompt this
/// thread's plugins opened.
pub fn answer_oldest(text: &str) {
    let (text, queue) = match text.strip_prefix("queue:") {
        Some(t) => (t, true),
        None => (text, false),
    };
    let open = oldest();
    if queue && open.is_none() {
        let me = me();
        with(|s| s.queued.push((me, text.to_string())));
        return;
    }
    let id = open.as_ref().map(|p| p.0);
    let Some(id) = id else {
        // No dialog of ours: the plugin store's permission prompt (HTTP to a
        // domain, script injection), answered allow / deny.
        if let Ok(mut g) = crate::plugin_store::permissions::global().lock() {
            if !g.prompts.is_empty() {
                let p = g.prompts.remove(0);
                let allow = matches!(text.to_ascii_lowercase().as_str(), "allow" | "yes" | "ok");
                g.answer(&p, allow);
            }
        }
        return;
    };
    let a = answer_for(&open.map(|p| p.1).unwrap_or(PromptKind::Import3d { rig: false }), text);
    answer(id, a);
}

/// A harness answer's text as the answer to a prompt of `kind`.
fn answer_for(kind: &PromptKind, text: &str) -> Answer {
    if text.eq_ignore_ascii_case("cancel") {
        return Answer::Cancel;
    }
    match kind {
        // (SaveToRoblox: the harness never uploads; any answer but
        // cancel is "save" with the request built and logged by the host.)
        PromptKind::ImportFile { .. } | PromptKind::Import3d { .. } | PromptKind::SaveSelection { .. } | PromptKind::Export { .. } => {
            Answer::Paths(text.split('|').filter(|p| !p.is_empty()).map(PathBuf::from).collect())
        }
        _ => Answer::Text(text.to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn save_to_roblox_builds_a_model_upload() {
        let (m, url, req) = model_upload("My Model", "desc", 42);
        assert_eq!(m, "POST");
        assert_eq!(url, "https://apis.roblox.com/assets/user-auth/v1/assets");
        assert_eq!(req["assetType"], "Model");
        assert_eq!(req["displayName"], "My Model");
        assert_eq!(req["creationContext"]["creator"]["userId"], "42");
        assert_eq!(req["creationContext"]["expectedPrice"], 0);
    }

    #[test]
    fn prompts_answer_in_order() {
        let a = open(PromptKind::ExistingAssetId { asset_type: "Animation".into() });
        let b = open(PromptKind::ImportFile { filters: vec![], multi: false });
        assert!(poll(a).is_none());
        answer_oldest("123");
        assert_eq!(poll(a), Some(Answer::Text("123".into())));
        answer_oldest("C:/x.png|C:/y.txt");
        assert_eq!(poll(b), Some(Answer::Paths(vec!["C:/x.png".into(), "C:/y.txt".into()])));
        assert!(poll(b).is_none());
    }
}

// ------------------------------------------------------------ the editor

use std::cell::RefCell;

thread_local! {
    /// Open asset-id pickers: (prompt, asset type, text typed).
    static PICKERS: RefCell<Vec<(u64, String, String)>> = const { RefCell::new(Vec::new()) };
    /// Open Asset Configuration windows (SaveSelectedToRoblox).
    static CONFIGS: RefCell<Vec<AssetConfig>> = const { RefCell::new(Vec::new()) };
}

struct AssetConfig {
    id: u64,
    title: String,
    description: String,
    sel: Vec<rbx_dom_weak::types::Ref>,
    status: Option<std::sync::mpsc::Receiver<Result<u64, String>>>,
}

/// Pages plugins asked the browser for (OpenWikiPage), opened by `ui`.
static URLS: Mutex<Vec<String>> = Mutex::new(Vec::new());

pub fn open_url_later(url: String) {
    URLS.lock().unwrap_or_else(|e| e.into_inner()).push(url);
}

/// The upload SaveSelectedToRoblox's Save makes (the Toolbox's asset upload
/// over the session, as Publish as Plugin's, with assetType Model): the
/// method, URL and `request` JSON. Kept off sale (expectedPrice 0).
pub fn model_upload(title: &str, description: &str, user_id: u64) -> (&'static str, String, serde_json::Value) {
    (
        "POST",
        rustudio_account::plugins::USER_AUTH_ASSETS_URL.to_string(),
        serde_json::json!({
            "assetType": "Model",
            "displayName": title,
            "description": description,
            "creationContext": {"creator": {"userId": user_id.to_string()}, "expectedPrice": 0},
        }),
    )
}

/// The Asset Configuration windows: Save uploads the selection on a worker
/// thread (only from a person's click: harness answers never upload).
fn asset_config_ui(ctx: &egui::Context, doc: &rustudio_core::doc::Document) {
    CONFIGS.with(|c| {
        let mut close = Vec::new();
        for cfg in c.borrow_mut().iter_mut() {
            if !with(|s| s.open.iter().any(|o| o.id == cfg.id)) && cfg.status.is_none() {
                close.push(cfg.id);
                continue;
            }
            egui::Window::new("Asset Configuration")
                .id(egui::Id::new(("plugin_asset_config", cfg.id)))
                .collapsible(false)
                .show(ctx, |ui| {
                    ui.horizontal(|ui| {
                        ui.label("Title");
                        ui.text_edit_singleline(&mut cfg.title);
                    });
                    ui.label("Description");
                    ui.text_edit_multiline(&mut cfg.description);
                    ui.label("Content Type: Development Item");
                    if let Some(rx) = &cfg.status {
                        match rx.try_recv() {
                            Ok(Ok(asset)) => {
                                ui.label(format!("Saved as asset {asset}"));
                                close.push(cfg.id);
                            }
                            Ok(Err(e)) => {
                                ui.label(format!("Upload failed: {e}"));
                                cfg.status = None;
                            }
                            Err(_) => {
                                ui.label("Uploading…");
                            }
                        }
                        return;
                    }
                    ui.horizontal(|ui| {
                        let ok = !cfg.title.trim().is_empty();
                        if ui.add_enabled(ok, egui::Button::new("Save")).clicked() {
                            let bytes = doc.encode(&cfg.sel, false).unwrap_or_default();
                            let (title, desc) = (cfg.title.clone(), cfg.description.clone());
                            let (tx, rx) = std::sync::mpsc::channel();
                            let ctx2 = ctx.clone();
                            let _ = std::thread::Builder::new().name("save-to-roblox".into()).spawn(move || {
                                let r = upload_model(&title, &desc, &bytes);
                                let _ = tx.send(r);
                                ctx2.request_repaint();
                            });
                            cfg.status = Some(rx);
                            answer(cfg.id, Answer::Text("save".into()));
                        }
                        if ui.button("Cancel").clicked() {
                            answer(cfg.id, Answer::Cancel);
                            close.push(cfg.id);
                        }
                    });
                });
        }
        c.borrow_mut().retain(|x| !close.contains(&x.id));
    });
}

/// The upload itself (multipart `request` + `fileContent`, then the
/// operation is polled). Not run by any check: it publishes to the account.
fn upload_model(title: &str, description: &str, rbxm: &[u8]) -> Result<u64, String> {
    let config = rustudio_account::AccountConfig::load().map_err(|e| e.to_string())?;
    let c = rustudio_account::Client::open(config).map_err(|e| e.describe())?;
    let (method, url, request) = model_upload(title, description, c.session.profile.id);
    const B: &str = "----rustudio-model-upload-3a91";
    let mut body = Vec::new();
    body.extend_from_slice(format!("--{B}\r\nContent-Disposition: form-data; name=\"request\"\r\nContent-Type: application/json\r\n\r\n").as_bytes());
    body.extend_from_slice(request.to_string().as_bytes());
    body.extend_from_slice(format!("\r\n--{B}\r\nContent-Disposition: form-data; name=\"fileContent\"; filename=\"asset.rbxm\"\r\nContent-Type: model/x-rbxm\r\n\r\n").as_bytes());
    body.extend_from_slice(rbxm);
    body.extend_from_slice(format!("\r\n--{B}--\r\n").as_bytes());
    let (st, text) = c
        .session
        .send_csrf(method, &url, &format!("multipart/form-data; boundary={B}"), &body)
        .map_err(|e| e.to_string())?;
    if !(200..300).contains(&st) {
        return Err(format!("HTTP {st}: {}", text.chars().take(200).collect::<String>()));
    }
    let op: serde_json::Value = serde_json::from_str(&text).map_err(|e| e.to_string())?;
    let id = op.pointer("/response/assetId").and_then(|v| v.as_str().map(str::to_string).or_else(|| v.as_u64().map(|n| n.to_string())));
    match id.and_then(|s| s.parse().ok()) {
        Some(n) => Ok(n),
        None => Err(format!("upload answered: {}", text.chars().take(200).collect::<String>())),
    }
}

/// Shows the dialogs plugins opened (call once a frame with the UI's
/// context; `doc` answers ExportPlace / ExportSelection). File dialogs are
/// native (rfd), awaited off the UI thread; the asset-id picker is a
/// window like Studio's Asset Configuration.
pub fn ui(ctx: &egui::Context, doc: &rustudio_core::doc::Document) {
    for (id, kind) in take_new() {
        match kind {
            PromptKind::ImportFile { filters, multi } => {
                let mut d = crate::file_dialog::AsyncFileDialog::new().set_title("Open");
                if !filters.is_empty() {
                    let f: Vec<&str> = filters.iter().map(String::as_str).collect();
                    d = d.add_filter("Files", &f);
                }
                spawn(ctx, id, d, if multi { Pick::Files } else { Pick::File }, None);
            }
            PromptKind::Import3d { .. } => {
                let d = crate::file_dialog::AsyncFileDialog::new()
                    .set_title("Open 3D File (FBX, OBJ, GLB, GLTF)")
                    .add_filter("3D Files", &["fbx", "obj", "glb", "gltf"]);
                spawn(ctx, id, d, Pick::File, None);
            }
            PromptKind::SaveSelection { name } => {
                let mut d = crate::file_dialog::AsyncFileDialog::new().set_title("Save As").add_filter("Roblox Model", &["rbxm", "rbxmx"]);
                if !name.is_empty() {
                    d = d.set_file_name(format!("{name}.rbxm"));
                }
                spawn(ctx, id, d, Pick::Save, None);
            }
            PromptKind::Export { selection } => {
                let title = if selection { "Export Selection" } else { "Export Place" };
                let d = crate::file_dialog::AsyncFileDialog::new().set_title(title).add_filter("OBJ", &["obj"]);
                let refs = if selection {
                    doc.selection.clone()
                } else {
                    doc.workspace().map(|w| vec![w]).unwrap_or_default()
                };
                let job = ObjJob(obj_text(doc, &refs));
                spawn(ctx, id, d, Pick::Save, Some(job));
            }
            PromptKind::ExistingAssetId { asset_type } => PICKERS.with(|p| p.borrow_mut().push((id, asset_type, String::new()))),
            PromptKind::SaveToRoblox { name } => CONFIGS.with(|c| {
                c.borrow_mut().push(AssetConfig { id, title: name, description: String::new(), sel: doc.selection.clone(), status: None })
            }),
        }
    }
    for url in std::mem::take(&mut *URLS.lock().unwrap_or_else(|e| e.into_inner())) {
        ctx.open_url(egui::OpenUrl::new_tab(url));
    }
    asset_config_ui(ctx, doc);
    PICKERS.with(|p| {
        let mut done = Vec::new();
        for (id, ty, text) in p.borrow_mut().iter_mut() {
            egui::Window::new("Asset Configuration")
                .id(egui::Id::new(("plugin_asset_picker", *id)))
                .collapsible(false)
                .resizable(false)
                .show(ctx, |ui| {
                    ui.label("Choose an asset to import");
                    ui.horizontal(|ui| {
                        ui.label(format!("{ty} ID"));
                        ui.text_edit_singleline(text);
                    });
                    ui.horizontal(|ui| {
                        if ui.button("Import").clicked() {
                            done.push((*id, Answer::Text(text.trim().to_string())));
                        }
                        if ui.button("Cancel").clicked() || ui.input(|i| i.key_pressed(egui::Key::Escape)) {
                            done.push((*id, Answer::Cancel));
                        }
                    });
                });
        }
        for (id, a) in done {
            p.borrow_mut().retain(|x| x.0 != id);
            answer(id, a);
        }
        // (Answered by the harness meanwhile: the window goes.)
        p.borrow_mut().retain(|x| with(|s| s.open.iter().any(|o| o.id == x.0)));
    });
}

enum Pick {
    File,
    Files,
    Save,
}

/// What a save dialog writes once a path is chosen.
struct ObjJob(String);

fn spawn(ctx: &egui::Context, id: u64, d: crate::file_dialog::AsyncFileDialog, pick: Pick, job: Option<ObjJob>) {
    type Fut = std::pin::Pin<Box<dyn std::future::Future<Output = Vec<PathBuf>> + Send>>;
    let fut: Fut = match pick {
        Pick::File => Box::pin(async move { d.pick_file().await.map(|h| vec![h.path().to_path_buf()]).unwrap_or_default() }),
        Pick::Files => Box::pin(async move {
            d.pick_files().await.map(|v| v.iter().map(|h| h.path().to_path_buf()).collect()).unwrap_or_default()
        }),
        Pick::Save => Box::pin(async move { d.save_file().await.map(|h| vec![h.path().to_path_buf()]).unwrap_or_default() }),
    };
    let ctx = ctx.clone();
    let _ = std::thread::Builder::new().name("plugin-dialog".into()).spawn(move || {
        let paths = pollster::block_on(fut);
        if let (Some(ObjJob(text)), Some(p)) = (&job, paths.first()) {
            let _ = std::fs::write(p, text);
        }
        answer(id, if paths.is_empty() { Answer::Cancel } else { Answer::Paths(paths) });
        ctx.request_repaint();
    });
}

/// Wavefront OBJ of the parts under `refs` (each part as its box; a
/// MeshPart / union as its bounding box).
pub fn obj_text(doc: &rustudio_core::doc::Document, refs: &[rbx_dom_weak::types::Ref]) -> String {
    use rbx_dom_weak::types::Variant;
    let mut out = String::from("# Exported by rustudio\n");
    let mut base = 1;
    let mut seen = std::collections::HashSet::new();
    for r in refs {
        for i in doc.dom.descendants_of(*r) {
            if !seen.insert(i.referent()) || !rustudio_core::doc::is_a(i.class.as_str(), "BasePart") {
                continue;
            }
            let (Some(Variant::CFrame(cf)), Some(Variant::Vector3(size))) =
                (doc.prop(i.referent(), "CFrame"), doc.prop(i.referent(), "Size"))
            else {
                continue;
            };
            out.push_str(&format!("o {}\n", i.name));
            let h = [size.x / 2.0, size.y / 2.0, size.z / 2.0];
            let m = cf.orientation;
            for k in 0..8 {
                let l = [
                    if k & 1 == 0 { -h[0] } else { h[0] },
                    if k & 2 == 0 { -h[1] } else { h[1] },
                    if k & 4 == 0 { -h[2] } else { h[2] },
                ];
                let w = [
                    cf.position.x + m.x.x * l[0] + m.x.y * l[1] + m.x.z * l[2],
                    cf.position.y + m.y.x * l[0] + m.y.y * l[1] + m.y.z * l[2],
                    cf.position.z + m.z.x * l[0] + m.z.y * l[1] + m.z.z * l[2],
                ];
                out.push_str(&format!("v {} {} {}\n", w[0], w[1], w[2]));
            }
            for f in [[0, 2, 3, 1], [4, 5, 7, 6], [0, 1, 5, 4], [2, 6, 7, 3], [0, 4, 6, 2], [1, 3, 7, 5]] {
                out.push_str(&format!("f {} {} {} {}\n", base + f[0], base + f[1], base + f[2], base + f[3]));
            }
            base += 8;
        }
    }
    out
}
