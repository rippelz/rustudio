//! Renders the profiler window on a capture file and saves a screenshot:
//! `cargo run --profile agent -p rustudio-profiler-ui --example preview --
//!   CAPTURE.rsprof OUT.png [TAB] [FRAME|A-B] [pin] [customize] [WxH]`
//! (TAB: timeline, table, counters, gpu, scripts; FRAME: a frame index or
//! a range of them; `pin`: pin the frame's longest script-or-other scope).

use rustudio_profiler_ui::prefs::TabId;
use rustudio_profiler_ui::ProfilerWindow;

struct App {
    w: ProfilerWindow,
    out: std::path::PathBuf,
    frame: u32,
    asked: bool,
}

impl eframe::App for App {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        egui::CentralPanel::default().frame(egui::Frame::NONE).show_inside(ui, |ui| self.w.show(ui));
        self.frame += 1;
        if self.frame == 8 && !self.asked {
            self.asked = true;
            ctx.send_viewport_cmd(egui::ViewportCommand::Screenshot(Default::default()));
        }
        let shot = ctx.input(|i| {
            i.raw.events.iter().find_map(|e| match e {
                egui::Event::Screenshot { image, .. } => Some(image.clone()),
                _ => None,
            })
        });
        if let Some(img) = shot {
            let buf = image::RgbaImage::from_raw(img.size[0] as u32, img.size[1] as u32, img.as_raw().to_vec()).expect("image");
            buf.save(&self.out).expect("save");
            println!("wrote {}", self.out.display());
            std::process::exit(0);
        }
        ctx.request_repaint();
    }
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let cap = std::path::PathBuf::from(&args[0]);
    let out = std::path::PathBuf::from(&args[1]);
    let tab = match args.get(2).map(String::as_str) {
        Some("table") => TabId::Table,
        Some("counters") => TabId::Counters,
        Some("gpu") => TabId::Gpu,
        Some("scripts") => TabId::Scripts,
        _ => TabId::Timeline,
    };
    let sel = args.get(3).cloned();
    let pin = args.iter().any(|a| a == "pin");
    let customize = args.iter().any(|a| a == "customize");
    let size = args
        .iter()
        .find_map(|a| a.split_once('x').and_then(|(w, h)| Some([w.parse::<f32>().ok()?, h.parse::<f32>().ok()?])))
        .unwrap_or([1440.0, 900.0]);
    let opts = eframe::NativeOptions {
        renderer: eframe::Renderer::Wgpu,
        viewport: egui::ViewportBuilder::default().with_inner_size(size).with_title("profiler preview"),
        ..Default::default()
    };
    eframe::run_native(
        "profiler-preview",
        opts,
        Box::new(move |cc| {
            rustudio_engine::ti::install(&cc.egui_ctx);
            cc.egui_ctx.set_pixels_per_point(1.0);
            let mut w = ProfilerWindow::new();
            w.prefs = Default::default();
            w.prefs.tab = tab;
            w.open_file(&cap);
            w.preview_setup(sel.as_deref(), pin, customize);
            Ok(Box::new(App { w, out, frame: 0, asked: false }))
        }),
    )
    .expect("eframe");
}
