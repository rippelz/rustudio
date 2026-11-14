//! `RUSTUDIO_CONTROL=PATH`: drive a running rustudio over a Unix socket, so
//! comparison captures (Studio vs rustudio, from the same camera) don't
//! restart it for every picture.
//!
//! One command per line, one reply line each:
//!
//! - `ping` → `ok`
//! - `camera X,Y,Z,YAW,PITCH` (degrees, as `--camera`) → `ok`
//! - `fov DEGREES` (vertical field of view) → `ok`
//! - `shot PATH` → `ok PATH WxH` once the view is settled: every mesh,
//!   texture and material of the scene loaded (nothing waiting, no download
//!   queued) and a few frames drawn since the last camera move. The image is
//!   the 3D view only (as `--screenshot`'s `.view.png`): a PNG, or for a PATH
//!   ending in `.ppm` the raw pixels (binary PPM, no compression: much faster
//!   for a caller that re-encodes anyway). After 30 s without settling it is
//!   taken anyway and the reply says `ok PATH WxH unsettled`.
//! - `viewport W,H` → `ok WxH window AxB` once the 3D view is W × H pixels
//!   (the window is resized around it, so a capture can match another
//!   program's view without rescaling either picture; A × B is the window's
//!   inner size in points, for `--window` next time); `error …` if the window
//!   manager won't let it get there.
//! - `quit` → `ok`, then rustudio closes.

use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixListener;
use std::path::PathBuf;
use std::sync::mpsc::{channel, Receiver, Sender};
use std::time::{Duration, Instant};

/// Frames drawn after a camera move before a shot counts as settled.
const SETTLE_FRAMES: u64 = 1;
/// A shot is taken anyway after this long.
const SHOT_TIMEOUT: Duration = Duration::from_secs(30);
/// Frames to wait after resizing the window before measuring the view again.
const FIT_FRAMES: u64 = 4;
/// Resizes tried before `viewport` gives up.
const FIT_TRIES: u32 = 6;

pub struct Control {
    rx: Receiver<(String, Sender<String>)>,
    /// The frame the camera last moved on.
    moved_at: u64,
    shot: Option<Shot>,
    fit: Option<Fit>,
    /// Frames until the window is sent below the others: window managers
    /// ignore the level asked for at creation (Cinnamon/Muffin), not a
    /// request once the window is mapped.
    lower_in: u32,
}

/// A pending `viewport W,H`: the 3D view's size wanted, in pixels.
struct Fit {
    size: [i64; 2],
    reply: Sender<String>,
    /// The frame the window was last resized on.
    resized_at: Option<u64>,
    tries: u32,
}

struct Shot {
    path: PathBuf,
    reply: Sender<String>,
    asked: Instant,
    requested: bool,
    settled: bool,
}

impl Control {
    /// Listens on `path` (a stale socket file there is replaced).
    pub fn listen(path: &str) -> std::io::Result<Control> {
        let _ = std::fs::remove_file(path);
        let listener = UnixListener::bind(path)?;
        let (tx, rx) = channel::<(String, Sender<String>)>();
        std::thread::Builder::new().name("control".into()).spawn(move || {
            for stream in listener.incoming().flatten() {
                let tx = tx.clone();
                std::thread::spawn(move || {
                    let Ok(mut out) = stream.try_clone() else { return };
                    for line in BufReader::new(stream).lines().map_while(Result::ok) {
                        let (reply_tx, reply_rx) = channel();
                        if tx.send((line.trim().to_string(), reply_tx)).is_err() {
                            return;
                        }
                        let reply = reply_rx.recv().unwrap_or_else(|_| "error rustudio closed".into());
                        if writeln!(out, "{reply}").is_err() {
                            return;
                        }
                    }
                });
            }
        })?;
        eprintln!("[control] listening on {path}");
        Ok(Control { rx, moved_at: 0, shot: None, fit: None, lower_in: 2 })
    }
}

/// Writes an image as binary PPM (P6: a short header, then R, G, B bytes per
/// pixel): no compression, so it costs about as much as copying the pixels.
fn save_ppm(image: &egui::ColorImage, path: &std::path::Path) -> Result<(), String> {
    let [w, h] = image.size;
    let mut out = Vec::with_capacity(32 + w * h * 3);
    out.extend_from_slice(format!("P6\n{w} {h}\n255\n").as_bytes());
    for p in &image.pixels {
        out.extend_from_slice(&[p.r(), p.g(), p.b()]);
    }
    std::fs::write(path, out).map_err(|e| format!("{}: {e}", path.display()))
}

impl crate::app::Editor {
    /// Commands from the control socket, and a pending shot. Runs every frame.
    pub fn control_tick(&mut self, ctx: &egui::Context) {
        let Some(mut c) = self.control.take() else { return };
        // Stay under the person's windows (sent twice: once the window is
        // mapped, and again a little later in case the first was early).
        if c.lower_in > 0 && self.frame_no >= 1 {
            c.lower_in -= 1;
            if c.lower_in == 0 || self.frame_no == 1 {
                ctx.send_viewport_cmd(egui::ViewportCommand::WindowLevel(egui::WindowLevel::AlwaysOnBottom));
            }
        }
        while let Ok((line, reply)) = c.rx.try_recv() {
            let (cmd, arg) = line.split_once(' ').unwrap_or((line.as_str(), ""));
            match cmd {
                "ping" => { let _ = reply.send("ok".into()); }
                "camera" => {
                    let v: Vec<f32> = arg.split(',').filter_map(|s| s.trim().parse().ok()).collect();
                    if v.len() == 5 {
                        self.cam.pos = glam::Vec3::new(v[0], v[1], v[2]);
                        self.cam.yaw = v[3].to_radians();
                        self.cam.pitch = v[4].to_radians();
                        self.cam.focus = self.cam.pos + self.cam.forward() * 50.0;
                        c.moved_at = self.frame_no;
                        let _ = reply.send("ok".into());
                    } else {
                        let _ = reply.send("error camera wants X,Y,Z,YAW,PITCH".into());
                    }
                }
                "fov" => match arg.trim().parse::<f32>() {
                    // Vertical field of view in degrees (Camera.FieldOfView).
                    Ok(v) if v > 0.0 && v < 180.0 => {
                        self.cam.fov_deg = v;
                        c.moved_at = self.frame_no;
                        let _ = reply.send("ok".into());
                    }
                    _ => {
                        let _ = reply.send("error fov wants degrees between 0 and 180".into());
                    }
                },
                "shot" if !arg.is_empty() => {
                    if let Some(old) = c.shot.take() {
                        let _ = old.reply.send("error superseded by another shot".into());
                    }
                    c.shot = Some(Shot { path: PathBuf::from(arg), reply, asked: Instant::now(), requested: false, settled: false });
                }
                "viewport" => {
                    let v: Vec<i64> = arg.split(',').filter_map(|s| s.trim().parse().ok()).collect();
                    match v[..] {
                        [w, h] if w > 0 && h > 0 => {
                            if let Some(old) = c.fit.take() {
                                let _ = old.reply.send("error superseded by another viewport".into());
                            }
                            c.fit = Some(Fit { size: [w, h], reply, resized_at: None, tries: 0 });
                        }
                        _ => {
                            let _ = reply.send("error viewport wants W,H".into());
                        }
                    }
                }
                "quit" => {
                    let _ = reply.send("ok".into());
                    ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                }
                _ => { let _ = reply.send(format!("error unknown command {line:?}")); }
            }
        }
        // Grow or shrink the window by what the view is off by, until it
        // fits (the panels around the view keep their size).
        if let Some(f) = c.fit.as_mut() {
            let ppp = ctx.pixels_per_point();
            let r = self.viewport_rect;
            // The size a shot will have (whole pixels, as `view_pixels` crops).
            let (_, _, vw, vh) = crate::app::view_pixels(r, ppp, [usize::MAX, usize::MAX]);
            let now = [vw as i64, vh as i64];
            let waited = f.resized_at.map_or(true, |at| self.frame_no >= at + FIT_FRAMES);
            if waited && r.is_positive() {
                let off = [f.size[0] - now[0], f.size[1] - now[1]];
                let inner = ctx.input(|i| i.viewport().inner_rect);
                if off == [0, 0] {
                    // With the window's inner size (points, as `--window`
                    // takes it), so a caller can start the next rustudio at
                    // that size and skip the fitting.
                    let window = inner.map(|r| format!(" window {}x{}", r.width().round(), r.height().round())).unwrap_or_default();
                    let _ = f.reply.send(format!("ok {}x{}{window}", now[0], now[1]));
                    c.fit = None;
                } else if f.tries >= FIT_TRIES || inner.is_none() {
                    let _ = f.reply.send(format!("error the view stays {}x{} (rect {:?}, {} px per point)", now[0], now[1], r, ppp));
                    c.fit = None;
                } else if let Some(inner) = inner {
                    let size = inner.size() + egui::vec2(off[0] as f32, off[1] as f32) / ppp;
                    ctx.send_viewport_cmd(egui::ViewportCommand::InnerSize(size));
                    f.resized_at = Some(self.frame_no);
                    f.tries += 1;
                }
            }
        }
        if let Some(s) = c.shot.as_mut() {
            if !s.requested {
                let settled = self.frame_no >= c.moved_at + SETTLE_FRAMES
                    && self.viewport_rect.is_positive()
                    && !self.place_loading()
                    && self.draw_cache.waiting.is_empty()
                    && crate::assets::content_queue_size() == 0
                    && !self.assets.busy()
                    && !self.textures.decoding();
                if std::env::var_os("RUSTUDIO_CONTROL_DEBUG").is_some() {
                    eprintln!(
                        "[control] {:.2}s frame {} moved {} view {} loading {} waiting {} queue {} assets {} decoding {}",
                        s.asked.elapsed().as_secs_f32(),
                        self.frame_no,
                        c.moved_at,
                        self.viewport_rect.is_positive(),
                        self.place_loading(),
                        self.draw_cache.waiting.len(),
                        crate::assets::content_queue_size(),
                        self.assets.busy(),
                        self.textures.decoding()
                    );
                }
                if settled || s.asked.elapsed() > SHOT_TIMEOUT {
                    s.requested = true;
                    s.settled = settled;
                    // The 3D view as last drawn, read straight from the
                    // renderer (this tick runs after the view is drawn): no
                    // window screenshot, which took two or three more frames.
                    let t_read = Instant::now();
                    let direct = self
                        .renderer
                        .as_ref()
                        .zip(self.render_state.as_ref())
                        .and_then(|(r, rs)| r.read_frame(rs));
                    if std::env::var_os("RUSTUDIO_CONTROL_DEBUG").is_some() {
                        eprintln!(
                            "[control] read the frame in {:.0} ms ({})",
                            t_read.elapsed().as_secs_f64() * 1000.0,
                            if direct.is_some() { "direct" } else { "none: window screenshot instead" }
                        );
                    }
                    if let Some((w, h, rgba)) = direct {
                        let s = c.shot.take().expect("pending shot");
                        let pixels = rgba
                            .chunks_exact(4)
                            .map(|p| egui::Color32::from_rgb(p[0], p[1], p[2]))
                            .collect();
                        let view = egui::ColorImage::new([w as usize, h as usize], pixels);
                        let saved = if s.path.extension().is_some_and(|e| e.eq_ignore_ascii_case("ppm")) {
                            save_ppm(&view, &s.path)
                        } else {
                            crate::app::save_png(&view, &s.path)
                        };
                        let reply = match saved {
                            Ok(()) => format!("ok {} {w}x{h}{}", s.path.display(), if s.settled { "" } else { " unsettled" }),
                            Err(e) => format!("error {e}"),
                        };
                        if std::env::var_os("RUSTUDIO_CONTROL_DEBUG").is_some() {
                            let epoch = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap_or_default();
                            eprintln!("[control] shot saved {:.0} ms after it was read, reply sent at {:.3}", t_read.elapsed().as_secs_f64() * 1000.0, epoch.as_secs_f64());
                        }
                        let _ = s.reply.send(reply);
                    } else {
                        ctx.send_viewport_cmd(egui::ViewportCommand::Screenshot(Default::default()));
                    }
                }
            } else if let Some(image) = ctx.input(|i| i.raw.events.iter().find_map(|e| match e {
                egui::Event::Screenshot { image, .. } => Some(image.clone()),
                _ => None,
            })) {
                let s = c.shot.take().expect("pending shot");
                let ppp = ctx.pixels_per_point();
                let (x0, y0, w, h) = crate::app::view_pixels(self.viewport_rect, ppp, image.size);
                let pixels = (0..h)
                    .flat_map(|y| (0..w).map(move |x| (x, y)))
                    .map(|(x, y)| image.pixels[(y0 + y) * image.size[0] + x0 + x])
                    .collect();
                let view = egui::ColorImage::new([w, h], pixels);
                let saved = if s.path.extension().is_some_and(|e| e.eq_ignore_ascii_case("ppm")) {
                    save_ppm(&view, &s.path)
                } else {
                    crate::app::save_png(&view, &s.path)
                };
                let reply = match saved {
                    Ok(()) => format!("ok {} {w}x{h}{}", s.path.display(), if s.settled { "" } else { " unsettled" }),
                    Err(e) => format!("error {e}"),
                };
                let _ = s.reply.send(reply);
            }
        }
        // Keep frames coming while a shot is pending; otherwise poll often:
        // the socket is read once a frame, and waiting up to 50 ms for each
        // command was a large share of a capture's time.
        if c.shot.is_some() || c.fit.is_some() {
            ctx.request_repaint();
        } else {
            ctx.request_repaint_after(Duration::from_millis(2));
        }
        self.control = Some(c);
    }
}
