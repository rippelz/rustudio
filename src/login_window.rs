//! Sign in to Roblox in a window of rustudio's own (`--roblox-login`):
//! roblox.com's login page in a webview that keeps no profile (incognito):
//! WebKitGTK on Linux, WebView2 on Windows. Once Roblox sets the
//! `.ROBLOSECURITY` cookie, its value goes to stdout and the window closes. A
//! separate process, because the webview needs its own event loop (GTK's on
//! Linux) and the editor runs its own; the cookie reaches the editor over a
//! pipe, never a command line.

use std::io::Write;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use tao::event::{Event, StartCause, WindowEvent};
use tao::event_loop::{ControlFlow, EventLoop};
use tao::window::WindowBuilder;
use wry::WebViewBuilder;

const LOGIN: &str = "https://www.roblox.com/login";
const COOKIE: &str = ".ROBLOSECURITY";

/// The window itself: exits 0 after printing the cookie, 1 if closed first.
pub fn run() -> ! {
    // WebKitGTK's DMA-BUF renderer dies with a Wayland protocol error
    // (Error 71) on some GPUs and drivers; the shared-memory path works everywhere.
    #[cfg(target_os = "linux")]
    if std::env::var_os("WEBKIT_DISABLE_DMABUF_RENDERER").is_none() {
        std::env::set_var("WEBKIT_DISABLE_DMABUF_RENDERER", "1");
    }
    let event_loop = EventLoop::new();
    let window = WindowBuilder::new()
        .with_title("Sign in to Roblox - rustudio")
        .with_inner_size(tao::dpi::LogicalSize::new(480.0, 720.0))
        .build(&event_loop)
        .expect("login window");
    let builder = WebViewBuilder::new().with_url(LOGIN).with_incognito(true);
    #[cfg(any(windows, target_os = "macos"))]
    let webview = builder.build(&window).expect("login webview");
    #[cfg(not(any(windows, target_os = "macos")))]
    let webview = {
        use tao::platform::unix::WindowExtUnix;
        use wry::WebViewBuilderExtUnix;
        builder
            .build_gtk(window.default_vbox().expect("login window has no GTK box"))
            .expect("login webview")
    };
    event_loop.run(move |event, _, flow| {
        // Roblox sets the cookie from a script, so look for it twice a second
        // rather than on page loads.
        if let Event::NewEvents(StartCause::Init | StartCause::ResumeTimeReached { .. }) = event {
            *flow = ControlFlow::WaitUntil(Instant::now() + Duration::from_millis(500));
            let found = webview.cookies_for_url("https://www.roblox.com").ok().and_then(|cs| {
                cs.into_iter()
                    .find(|c| c.name() == COOKIE && !c.value().is_empty())
            });
            if let Some(c) = found {
                let mut out = std::io::stdout();
                let _ = writeln!(out, "{}", c.value());
                let _ = out.flush();
                std::process::exit(0);
            }
        }
        if let Event::WindowEvent { event: WindowEvent::CloseRequested, .. } = event {
            std::process::exit(1);
        }
    })
}

/// Opens the window and waits: the cookie once the person has signed in,
/// None if they closed the window.
pub fn sign_in() -> std::io::Result<Option<String>> {
    let out = Command::new(std::env::current_exe()?)
        .arg("--roblox-login")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .output()?;
    let raw = String::from_utf8_lossy(&out.stdout).trim().to_string();
    Ok((out.status.success() && !raw.is_empty()).then_some(raw))
}
