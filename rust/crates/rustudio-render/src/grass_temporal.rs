//! Opt-in consecutive-frame acceptance recorder. No state in ordinary runs.
use std::sync::{Mutex, OnceLock};
struct State { started: std::time::Instant, frame: usize }
static STATE: OnceLock<Mutex<State>> = OnceLock::new();
static INDEX: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(usize::MAX);
pub fn index() -> Option<usize> { let n=INDEX.load(std::sync::atomic::Ordering::Relaxed); (n!=usize::MAX).then_some(n) }
pub fn tick(cam: &mut crate::camera::Camera, ctx: &egui::Context) {
 let Ok(dir)=std::env::var("RUSTUDIO_GRASS_TEMPORAL") else {return};
 let mut s=STATE.get_or_init(||Mutex::new(State{started:std::time::Instant::now(),frame:0})).lock().unwrap();
 std::fs::create_dir_all(&dir).unwrap();
 ctx.input(|i| for e in &i.raw.events { if let egui::Event::Screenshot{image,user_data,..}=e { if let Some(n)=user_data.data.as_ref().and_then(|v|v.downcast_ref::<usize>()) { let bytes:Vec<u8>=image.pixels.iter().flat_map(|p|p.to_array()).collect(); image::save_buffer(format!("{dir}/frame-{n:05}.png"),&bytes,image.size[0] as u32,image.size[1] as u32,image::ColorType::Rgba8).unwrap(); } } });
 if s.started.elapsed().as_secs_f32()<8.0 {ctx.request_repaint();return}
 // Hold an exact recorder frame for GPU capture; opt-in only.
 let hold=std::env::var("RUSTUDIO_GRASS_TEMPORAL_HOLD").ok().and_then(|v|v.parse::<usize>().ok());
 let n=hold.map_or(s.frame, |h|s.frame.min(h)); s.frame+=1;
 let count=std::env::var("RUSTUDIO_GRASS_TEMPORAL_COUNT").ok().and_then(|v|v.parse().ok()).unwrap_or(480usize);
 if n>=count {INDEX.store(count-1,std::sync::atomic::Ordering::Relaxed); if n>count+4 {ctx.send_viewport_cmd(egui::ViewportCommand::Close);} ctx.request_repaint();return}
 INDEX.store(n,std::sync::atomic::Ordering::Relaxed);
 let a=std::env::var("RUSTUDIO_GRASS_TEMPORAL_PATH").unwrap();
 let points:Vec<Vec<f32>>=a.split(';').map(|p|p.split(',').map(|v|v.parse().unwrap()).collect()).collect();
 let u= if std::env::var("RUSTUDIO_GRASS_TEMPORAL_CONTROL").as_deref()==Ok("wind") {0.0} else {n as f32/(count-1) as f32*(points.len()-1) as f32};
 let k=(u.floor() as usize).min(points.len()-2); let t=u-k as f32;
 let v:Vec<f32>=(0..5).map(|j|points[k][j]*(1.0-t)+points[k+1][j]*t).collect();
 cam.pos=glam::Vec3::new(v[0],v[1],v[2]);cam.yaw=v[3].to_radians();cam.pitch=v[4].to_radians();cam.basis=None;
 ctx.send_viewport_cmd(egui::ViewportCommand::Screenshot(egui::UserData::new(n)));ctx.request_repaint();
}
pub fn time(n:usize)->f32 {if std::env::var("RUSTUDIO_GRASS_TEMPORAL_CONTROL").as_deref()==Ok("camera") {4.0} else {4.0+n as f32/60.0}}
