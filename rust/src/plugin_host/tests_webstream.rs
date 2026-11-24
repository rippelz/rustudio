//! WebStreamClient lifecycle through a plugin: idle live connections end
//! (sockets closed, worker threads gone) when the plugin unloads or its VM
//! goes away, not only on paper; a failed WebSocket send reaches the
//! script as Error. The connections are real (local servers here).

use std::io::{Read, Write};
use std::net::TcpListener;
use std::sync::mpsc::{self, Receiver};
use std::time::{Duration, Instant};

use rbx_dom_weak::{InstanceBuilder, WeakDom};

use super::*;
use rustudio_engine::runtime::domains::services::stream;

fn source(name: &str, src: &str) -> PluginSource {
    let mut dom = WeakDom::new(InstanceBuilder::new("DataModel"));
    let root = dom.root_ref();
    dom.insert(root, InstanceBuilder::new("Script").with_name("Main").with_property("Source", src.to_string()));
    PluginSource::Tree { name: format!("user_{name}.rbxmx"), asset_id: None, dom: std::sync::Arc::new(dom) }
}

/// Runs frames (real time passes: the connections are real) until `until`
/// holds for the output so far, or 10 s.
fn run_until(h: &mut PluginHost, doc: &mut Document, out: &mut Vec<String>, until: impl Fn(&[String]) -> bool) {
    let t = Instant::now();
    while t.elapsed() < Duration::from_secs(10) {
        h.tick(doc, 1.0 / 60.0, true, false);
        out.extend(h.take_log().into_iter().map(|(_, t)| t.split("  —").next().unwrap_or("").to_string()));
        if until(out) {
            return;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
}

fn within(secs: f32, f: impl Fn() -> bool) -> bool {
    let t = Instant::now();
    while t.elapsed() < Duration::from_secs_f32(secs) {
        if f() {
            return true;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    f()
}

/// An HTTP server that answers with a streamed response's headers (and one
/// chunk) and then stays idle. The receiver gets the moment the client's
/// side ended.
fn idle_http(first: &'static [u8]) -> (u16, Receiver<Instant>) {
    let l = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = l.local_addr().unwrap().port();
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let (mut s, _) = l.accept().unwrap();
        let _ = s.set_read_timeout(Some(Duration::from_secs(60)));
        let mut b = [0u8; 4096];
        let _ = s.read(&mut b);
        s.write_all(first).unwrap();
        loop {
            match s.read(&mut b) {
                Ok(0) | Err(_) => break,
                Ok(_) => {}
            }
        }
        let _ = tx.send(Instant::now());
    });
    (port, rx)
}

/// A WebSocket server that stays idle; it reports text messages, the
/// client's close frame and the connection's end.
fn idle_ws() -> (u16, Receiver<String>) {
    let l = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = l.local_addr().unwrap().port();
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let (s, _) = l.accept().unwrap();
        let _ = s.set_read_timeout(Some(Duration::from_secs(60)));
        let mut ws = tungstenite::accept(s).unwrap();
        loop {
            match ws.read() {
                Ok(tungstenite::Message::Text(t)) => {
                    let _ = tx.send(format!("text {t}"));
                }
                Ok(tungstenite::Message::Close(_)) => {
                    let _ = tx.send("close frame".into());
                }
                Ok(_) => {}
                Err(_) => {
                    let _ = tx.send("ended".into());
                    break;
                }
            }
        }
        // Keep the socket (no EOF for the client) for the test's length.
        std::thread::sleep(Duration::from_secs(20));
        drop(ws);
    });
    (port, rx)
}

const OPEN_THREE: &str = r#"
local HS = game:GetService("HttpService")
_G.keep = {}
for _, c in { {"raw", Enum.WebStreamClientType.RawStream, "http://127.0.0.1:__RAW__/idle"},
              {"sse", Enum.WebStreamClientType.SSE, "http://127.0.0.1:__SSE__/idle"},
              {"ws", Enum.WebStreamClientType.WebSocket, "ws://127.0.0.1:__WS__/idle"} } do
    local cl = HS:CreateWebStreamClient(c[2], { Url = c[3] })
    table.insert(_G.keep, cl)
    cl.Opened:Connect(function(code) print("opened " .. c[1] .. " " .. code) end)
    cl.Closed:Connect(function() print("closed " .. c[1]) end)
    cl.Error:Connect(function(code, m) print("error " .. c[1] .. " " .. m) end)
end
"#;

struct Three {
    raw: (u16, Receiver<Instant>),
    sse: (u16, Receiver<Instant>),
    ws: (u16, Receiver<String>),
}

impl Three {
    fn new() -> Three {
        Three {
            raw: idle_http(b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n3\r\nabc\r\n"),
            sse: idle_http(b"HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nTransfer-Encoding: chunked\r\n\r\n9\r\ndata: x\n\n\r\n"),
            ws: idle_ws(),
        }
    }

    fn src(&self) -> String {
        OPEN_THREE.replace("__RAW__", &self.raw.0.to_string()).replace("__SSE__", &self.sse.0.to_string()).replace("__WS__", &self.ws.0.to_string())
    }

    fn workers(&self) -> usize {
        [self.raw.0, self.sse.0, self.ws.0].iter().map(|p| stream::workers_for(&format!("127.0.0.1:{p}/"))).sum()
    }

    /// All three connections ended on the server side, and their worker
    /// threads are gone.
    fn assert_all_ended(&self, how: &str) {
        assert!(self.raw.1.recv_timeout(Duration::from_secs(2)).is_ok(), "{how}: RawStream socket still open on the server");
        assert!(self.sse.1.recv_timeout(Duration::from_secs(2)).is_ok(), "{how}: SSE socket still open on the server");
        let ws: Vec<String> = {
            let t = Instant::now();
            let mut v = Vec::new();
            while t.elapsed() < Duration::from_secs(2) && !v.iter().any(|m| m == "ended") {
                if let Ok(m) = self.ws.1.recv_timeout(Duration::from_millis(50)) {
                    v.push(m);
                }
            }
            v
        };
        assert_eq!(ws, vec!["close frame".to_string(), "ended".to_string()], "{how}: the WebSocket");
        assert!(within(2.0, || self.workers() == 0), "{how}: {} worker thread(s) still running", self.workers());
    }
}

fn opened_all(out: &[String]) -> bool {
    ["opened raw 200", "opened sse 200", "opened ws 101"].iter().all(|w| out.iter().any(|l| l == w))
}

#[test]
fn unload_ends_idle_live_connections() {
    super::tests::temp_data();
    let three = Three::new();
    let mut doc = Document::new_place();
    let mut h = PluginHost::default();
    let id = h.load(source("WsUnload", &three.src())).unwrap();
    let mut out = Vec::new();
    run_until(&mut h, &mut doc, &mut out, opened_all);
    assert!(opened_all(&out), "{out:#?}");
    std::thread::sleep(Duration::from_millis(200));
    assert_eq!(three.workers(), 3, "three idle workers blocked on their sockets");
    h.unload(id);
    for _ in 0..3 {
        h.tick(&mut doc, 1.0 / 60.0, true, false);
    }
    three.assert_all_ended("unload");
    out.extend(h.take_log().into_iter().map(|(_, t)| t));
    assert!(!out.iter().any(|l| l.starts_with("closed") || l.starts_with("error")), "no events into an unloaded plugin: {out:#?}");
}

#[test]
fn vm_drop_ends_idle_live_connections() {
    super::tests::temp_data();
    let three = Three::new();
    let mut doc = Document::new_place();
    let mut h = PluginHost::default();
    h.load(source("WsDrop", &three.src())).unwrap();
    let mut out = Vec::new();
    run_until(&mut h, &mut doc, &mut out, opened_all);
    assert!(opened_all(&out), "{out:#?}");
    std::thread::sleep(Duration::from_millis(200));
    assert_eq!(three.workers(), 3);
    drop(h);
    three.assert_all_ended("VM dropped");
}

/// The transport dies under an open WebSocket: the script's next Send
/// (injected: stream::break_writes_for) fails as Error (state Error, no
/// Closed after it) and Send raises from
/// then on, so nothing more is accepted and silently lost.
#[test]
fn failed_send_reaches_the_script() {
    super::tests::temp_data();
    let (port, got) = idle_ws();
    let mut doc = Document::new_place();
    let mut h = PluginHost::default();
    let src = r#"
local HS = game:GetService("HttpService")
local ws = HS:CreateWebStreamClient(Enum.WebStreamClientType.WebSocket, { Url = "ws://127.0.0.1:__PORT__/send" })
local sent, open = 0, false
ws.Opened:Connect(function()
    print("opened")
    open = true
end)
ws.Closed:Connect(function() print("closed") end)
ws.Error:Connect(function(code, m)
    open = false
    print("error " .. code .. " " .. m .. " " .. tostring(ws.ConnectionState))
    local ok, e = pcall(ws.Send, ws, "after")
    print("send after error " .. tostring(ok) .. " " .. tostring(e))
    print("sent " .. sent)
end)
-- Sends every frame while open.
game:GetService("RunService").Heartbeat:Connect(function()
    if open then
        local ok, e = pcall(ws.Send, ws, "tick" .. sent)
        if ok then sent += 1 elseif not tostring(e):find("not connected") then print("send raised " .. tostring(e)) end
    end
end)
"#
    .replace("__PORT__", &port.to_string());
    h.load(source("WsSend", &src)).unwrap();
    let mut out = Vec::new();
    run_until(&mut h, &mut doc, &mut out, |o| o.iter().any(|l| l == "opened"));
    // A few messages go through, then the transport's write side dies.
    // (frames keep running: the script sends from Heartbeat)
    let first = {
        let t = Instant::now();
        loop {
            h.tick(&mut doc, 1.0 / 60.0, true, false);
            if let Ok(m) = got.recv_timeout(Duration::from_millis(10)) {
                break m;
            }
            assert!(t.elapsed() < Duration::from_secs(5), "nothing reached the server");
        }
    };
    assert_eq!(first, "text tick0");
    out.extend(h.take_log().into_iter().map(|(_, t)| t));
    assert_eq!(stream::break_writes_for(&format!("127.0.0.1:{port}/")), 1);
    run_until(&mut h, &mut doc, &mut out, |o| o.iter().any(|l| l.starts_with("sent ")));
    assert!(
        out.iter().any(|l| l == "error 0 HttpError: ConnectionClosed Enum.WebStreamClientState.Error"),
        "{out:#?}"
    );
    assert!(out.iter().any(|l| l == "send after error false WebStreamClient is not connected"), "{out:#?}");
    for _ in 0..10 {
        h.tick(&mut doc, 1.0 / 60.0, true, false);
    }
    out.extend(h.take_log().into_iter().map(|(_, t)| t));
    assert!(!out.iter().any(|l| l.starts_with("closed") || l.starts_with("send raised")), "{out:#?}");
    assert!(within(2.0, || stream::workers_for(&format!("127.0.0.1:{port}/")) == 0), "worker still running");
    // Every Send the script saw succeed before the failure either reached
    // the server or was among those the Error reported (written after the
    // transport died): the server's messages are a prefix, in order.
    let sent: usize = out.iter().find_map(|l| l.strip_prefix("sent ")).unwrap().parse().unwrap();
    let mut server: Vec<String> = vec!["text tick0".into()];
    server.extend(got.try_iter().filter(|m| m.starts_with("text")));
    for (i, m) in server.iter().enumerate() {
        assert_eq!(m, &format!("text tick{i}"));
    }
    assert!(server.len() < sent, "the failed message(s) were reported, not delivered: server {} sent {sent}", server.len());
}
