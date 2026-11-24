//! Renders the Scripts tab from a saved script profile and screenshots it
//! (for comparing with the design side by side):
//! `cargo run -p rustudio-profiler-ui --example scripts_preview -- PROFILE.json OUT.png [W H]`
//! PROFILE.json: `script_profiler::write_capture` output (or an Export).

use std::sync::Arc;

use rustudio_profiler_ui::scripts::{show, ScriptsTabState};
use rustudio_profiler_ui::{Style, TabCtx};

struct App {
    capture: rustudio_core::profiler::Capture,
    state: ScriptsTabState,
    style: Style,
    out: std::path::PathBuf,
    frame: u32,
    asked: bool,
}

impl eframe::App for App {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        let ctx = &ctx;
        if self.frame == 0 {
            rustudio_engine::ti::install(ctx);
        }
        self.frame += 1;
        let tab = TabCtx { capture: &self.capture, range_ns: None, live: false, style: &self.style };
        show(ui, &mut self.state, &tab);
        let shot = ctx.input(|i| {
            i.events.iter().find_map(|e| match e {
                egui::Event::Screenshot { image, .. } => Some(image.clone()),
                _ => None,
            })
        });
        if let Some(img) = shot {
            save(&img, &self.out);
            std::process::exit(0);
        }
        if self.frame > 8 && !self.asked {
            self.asked = true;
            ctx.send_viewport_cmd(egui::ViewportCommand::Screenshot(egui::UserData::default()));
        }
        ctx.request_repaint();
    }
}

fn save(img: &Arc<egui::ColorImage>, out: &std::path::Path) {
    let [w, h] = img.size;
    let mut buf = image::RgbaImage::new(w as u32, h as u32);
    for (i, p) in img.pixels.iter().enumerate() {
        buf.put_pixel((i % w) as u32, (i / w) as u32, image::Rgba(p.to_array()));
    }
    buf.save(out).expect("write png");
    eprintln!("wrote {}", out.display());
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let profile = std::fs::read_to_string(&args[1]).expect("profile json");
    let out = std::path::PathBuf::from(&args[2]);
    let w: f32 = args.get(3).and_then(|s| s.parse().ok()).unwrap_or(1440.0);
    let h: f32 = args.get(4).and_then(|s| s.parse().ok()).unwrap_or(796.0);
    let mut capture = rustudio_core::profiler::Capture::default();
    capture.extensions.insert("scripts".into(), serde_json::from_str(&profile).expect("json"));
    let mut state = ScriptsTabState::default();
    // RUSTUDIO_PREVIEW_SELECT=<script suffix>: select that script first.
    if let Ok(sel) = std::env::var("RUSTUDIO_PREVIEW_SELECT") {
        let snap: rustudio_engine::script_profiler::Snapshot = serde_json::from_str(&profile).unwrap();
        state.selected = snap.scripts.iter().find(|s| s.path.ends_with(&sel)).map(|s| (s.vm, s.path.to_string()));
    }
    if std::env::var_os("RUSTUDIO_PREVIEW_EXACT").is_some() {
        state.exact = true;
    }
    let opts = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default().with_inner_size([w, h]).with_title("rustudio-scripts-preview"),
        ..Default::default()
    };
    eframe::run_native(
        "rustudio-scripts-preview",
        opts,
        Box::new(move |_| Ok(Box::new(App { capture, state, style: Style::default(), out, frame: 0, asked: false }))),
    )
    .unwrap();
}
