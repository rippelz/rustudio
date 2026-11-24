//! HttpService:CreateWebStreamClient's connections. Each client runs on its
//! own thread (tungstenite for the WebSocket type, ureq's streaming body for
//! SSE / RawStream); the services domain drains `poll` once a frame and fires
//! the WebStreamClient's events (Opened, MessageReceived, Error, Closed).
//!
//! Where each behaviour comes from. Nothing below was measured in Studio for
//! WebStreamClient itself: no capture of it exists (checks/parity/services.md:
//! the Studio used for the parity probes was network-isolated;
//! checks/plugins/ref/corpus/rojo.md records Rojo's panel, not a session).
//! - Public API (api-dump.json, Studio 0.747): the types (SSE 0, RawStream 1,
//!   WebSocket 2), the states (Connecting 0, Open 1, Error 2, Closed 3),
//!   Opened(responseStatusCode, headers: string), MessageReceived(message:
//!   string), Error(responseStatusCode, errorMessage), Closed(), Send(data:
//!   string), Close(), the "HttpError: <name>" names (Enum.HttpError).
//! - Public client code written for Studio (Rojo 7.7.1's
//!   plugin/src/ApiContext.lua, the corpus plugin): it passes a ws:// URL;
//!   it decodes MessageReceived's string as msgpack from the binary frames
//!   `rojo serve` sends, so binary frames must arrive byte-exact as strings;
//!   it handles Closed and Error as separate endings and calls Close() to
//!   disconnect. checks/plugins/webstream/check.py runs that plugin
//!   unmodified against the official `rojo serve` 7.7.1.
//! - Measured for RequestAsync / GetAsync, reused here by analogy (not
//!   measured for this API): the option and URL errors, the Roblox-domain
//!   error, "HttpError: ConnectFail" / "DnsResolve", plugins skipping
//!   HttpEnabled, the request headers Studio adds.
//! - rustudio's own choices, each waiting on a Studio measurement (listed in
//!   Studio tests): the headers string's format; Error codes and messages
//!   for transport failures, a refused upgrade, a failed Send ("HttpError:
//!   ConnectionClosed"), a handshake timeout; no Closed after Error; a TCP
//!   drop without a close frame giving Closed; Send's text-vs-binary choice
//!   and its error wording; Send after Close raising; SSE giving each
//!   event's data only; ws:// and http:// both taken for the WebSocket
//!   type; data not yet fired being dropped on Close; no client limit; the
//!   timeouts below; a close frame on unload.

use std::collections::{HashMap, VecDeque};
use std::io::Read;
use std::net::TcpStream;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver, Sender, TryRecvError};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

/// Bound on name resolution + TCP connect (per address) and on the
/// WebSocket handshake. A script's Close, its plugin's unload or its VM
/// going away interrupts everything else at once (SSE / RawStream: the
/// agent's shutdown; WebSocket: within one 15 ms read tick); name
/// resolution and the TCP connect itself are only bounded by this, and the
/// worker then ends without reporting anything.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(30);
/// WebSocket writes blocked this long (a peer that stopped reading) fail
/// the connection: Error(0, "HttpError: TimedOut").
const WRITE_TIMEOUT: Duration = Duration::from_secs(10);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Sse,
    Raw,
    WebSocket,
}

impl Kind {
    /// Enum.WebStreamClientType's value.
    pub fn from_value(v: u32) -> Kind {
        match v {
            0 => Kind::Sse,
            1 => Kind::Raw,
            _ => Kind::WebSocket,
        }
    }
}

#[derive(Debug, PartialEq)]
pub enum Event {
    Opened(u16, String),
    Message(Vec<u8>),
    Error(i64, String),
    Closed,
}

enum Cmd {
    Send(Vec<u8>),
    Close,
}

pub struct Request {
    pub kind: Kind,
    pub method: String,
    pub url: String,
    pub headers: Vec<(String, String)>,
    pub body: Option<Vec<u8>>,
}

/// Stops a worker that is waiting on the network. The SSE / RawStream
/// request's ureq agent is shut down (`Agent::shutdown` ends a request, a
/// response wait or a body read in progress at once). The WebSocket worker
/// never blocks longer than a read tick once connected (handshake and open
/// loop alike) and checks `fired` each tick. (Shutting a duplicated socket
/// handle down from another thread is not reliable on Windows: it can give
/// WSAENOTCONN and leave the blocked read waiting.)
#[derive(Default)]
struct Cancel {
    fired: AtomicBool,
    /// The WebSocket handshake finished (the worker loop serves Close).
    opened: AtomicBool,
    agent: Mutex<Option<ureq::Agent>>,
    /// Tests: the transport's next writes fail (as a reset connection's).
    /// (Set only by `break_writes_for`, which studio's plugin-host test calls:
    /// that test is a crate away once the engine is one, so the switch is
    /// built in every run and stays false outside tests.)
    fail_writes: AtomicBool,
}

impl Cancel {
    fn fire(&self) {
        self.fired.store(true, Ordering::SeqCst);
        if let Some(a) = self.agent.lock().unwrap().take() {
            a.shutdown();
        }
    }

    fn fired(&self) -> bool {
        self.fired.load(Ordering::SeqCst)
    }

    fn set_agent(&self, a: &ureq::Agent) -> bool {
        let mut g = self.agent.lock().unwrap();
        if self.fired() {
            return false;
        }
        *g = Some(a.clone());
        true
    }
}

struct Entry {
    events: VecDeque<Event>,
    repaint: Option<egui::Context>,
    tx: Sender<Cmd>,
    cancel: Arc<Cancel>,
    /// Closed by the script (SSE / RawStream, or a WebSocket still
    /// connecting: Closed already queued; later events from the worker are
    /// dropped).
    done: bool,
    /// The script called Close (an open WebSocket waits for the server's
    /// close frame): Send is refused from then on, never silently dropped.
    closing: bool,
}

fn table() -> &'static Mutex<HashMap<u64, Entry>> {
    static T: OnceLock<Mutex<HashMap<u64, Entry>>> = OnceLock::new();
    T.get_or_init(|| Mutex::new(HashMap::new()))
}

/// The worker threads still running: id -> URL.
fn workers() -> &'static Mutex<HashMap<u64, String>> {
    static W: OnceLock<Mutex<HashMap<u64, String>>> = OnceLock::new();
    W.get_or_init(|| Mutex::new(HashMap::new()))
}

/// Takes the worker out of `workers` when its thread ends (whatever way).
struct WorkerGuard(u64);

impl Drop for WorkerGuard {
    fn drop(&mut self) {
        workers().lock().unwrap().remove(&self.0);
    }
}

/// How many worker threads are still running for URLs containing `part`
/// (studio's plugin-host test: the connections' threads end with the plugin).
pub fn workers_for(part: &str) -> usize {
    workers().lock().unwrap().values().filter(|u| u.contains(part)).count()
}

/// Tests: from now on the WebSocket writes of the clients whose URL
/// contains `part` fail as on a reset connection (the transport dying under
/// an open connection). (Shutting the real socket's send side down can't
/// stand in for this on Windows: shutdown on the socket can give
/// WSAENOTCONN there.) studio's plugin-host test calls this, so it is built
/// in every run; nothing outside a test calls it.
pub fn break_writes_for(part: &str) -> usize {
    let ids: Vec<u64> = workers().lock().unwrap().iter().filter(|(_, u)| u.contains(part)).map(|(i, _)| *i).collect();
    let t = table().lock().unwrap();
    let mut n = 0;
    for id in ids {
        if let Some(e) = t.get(&id) {
            e.cancel.fail_writes.store(true, Ordering::SeqCst);
            n += 1;
        }
    }
    n
}

/// `ws.send`, or (tests) the injected transport failure.
fn ws_send<S: std::io::Read + std::io::Write>(
    ws: &mut tungstenite::WebSocket<S>,
    m: tungstenite::Message,
    cancel: &Cancel,
) -> tungstenite::Result<()> {
    if cancel.fail_writes.load(Ordering::SeqCst) {
        return Err(tungstenite::Error::Io(std::io::ErrorKind::ConnectionReset.into()));
    }
    let _ = cancel;
    ws.send(m)
}

static NEXT: AtomicU64 = AtomicU64::new(1);

fn push(id: u64, ev: Event) {
    let repaint = {
        let mut entries = table().lock().unwrap();
        let Some(e) = entries.get_mut(&id).filter(|e| !e.done) else { return };
        e.events.push_back(ev);
        e.repaint.clone()
    };
    if let Some(ctx) = repaint {
        ctx.request_repaint_of(egui::ViewportId::ROOT);
    }
}

fn gone(id: u64) -> bool {
    table().lock().unwrap().get(&id).is_none_or(|e| e.done)
}

/// Opens the connection on its own thread; events arrive through [`poll`].
pub fn start(req: Request, repaint: Option<egui::Context>) -> u64 {
    let id = NEXT.fetch_add(1, Ordering::Relaxed);
    let (tx, rx) = mpsc::channel();
    let cancel = Arc::new(Cancel::default());
    table().lock().unwrap().insert(id, Entry { events: VecDeque::new(), repaint, tx, cancel: cancel.clone(), done: false, closing: false });
    workers().lock().unwrap().insert(id, req.url.clone());
    let spawned = std::thread::Builder::new().name("rustudio-webstream".into()).spawn(move || {
        let _guard = WorkerGuard(id);
        match req.kind {
            Kind::WebSocket => ws_run(id, req, rx, &cancel),
            _ => stream_run(id, req, rx, &cancel),
        }
    });
    if spawned.is_err() {
        workers().lock().unwrap().remove(&id);
        push(id, Event::Error(0, "HttpError: Unknown".into()));
    }
    id
}

/// The events since the last call, in order.
pub fn poll(id: u64) -> Vec<Event> {
    match table().lock().unwrap().get_mut(&id) {
        Some(e) => e.events.drain(..).collect(),
        None => Vec::new(),
    }
}

/// Queues a WebSocket message. False (the script's Send raises) when the
/// connection can't take it: Close was called, or the worker has ended
/// (failed or closed) and nothing would ever write it.
pub fn send(id: u64, data: Vec<u8>) -> bool {
    table().lock().unwrap().get(&id).is_some_and(|e| !e.closing && !e.done && e.tx.send(Cmd::Send(data)).is_ok())
}

/// The script's Close: an open WebSocket sends its close frame and reports
/// Closed when the server answers (or after 5 s); a WebSocket still
/// connecting and a streamed HTTP response are cut at once (the worker
/// drops its socket and ends) and Closed is reported.
pub fn close(id: u64, kind: Kind) {
    let mut t = table().lock().unwrap();
    let Some(e) = t.get_mut(&id) else { return };
    e.closing = true;
    let _ = e.tx.send(Cmd::Close);
    if kind == Kind::WebSocket && e.cancel.opened.load(Ordering::SeqCst) {
        return;
    }
    e.cancel.fire();
    if !e.done {
        // (data not yet fired is dropped: nothing arrives after Close)
        e.events.retain(|ev| !matches!(ev, Event::Message(_)));
        e.events.push_back(Event::Closed);
        e.done = true;
        let repaint = e.repaint.clone();
        drop(t);
        if let Some(ctx) = repaint {
            ctx.request_repaint_of(egui::ViewportId::ROOT);
        }
    }
}

/// Drops a client (its plugin unloaded, its VM went away, it was
/// collected): the worker ends. An open WebSocket sends its close frame
/// first (its loop notices within a read tick); everything else is cut.
pub fn forget(id: u64) {
    if let Some(e) = table().lock().unwrap().remove(&id) {
        let _ = e.tx.send(Cmd::Close);
        if !e.cancel.opened.load(Ordering::SeqCst) {
            e.cancel.fire();
        }
    }
}

/// Response headers as one string, a "Name: value" line each (CRLF).
fn header_text<'a>(h: impl Iterator<Item = (&'a str, String)>) -> String {
    h.map(|(k, v)| format!("{k}: {v}\r\n")).collect()
}

// ------------------------------------------------------------------ WebSocket

/// The TCP connection for a ws(s) request, address by address (each
/// bounded by CONNECT_TIMEOUT; a cancel is noticed between them).
fn ws_connect(uri: &tungstenite::http::Uri, cancel: &Cancel) -> Result<TcpStream, (i64, String)> {
    use std::net::ToSocketAddrs;
    let host = uri.host().unwrap_or("").trim_start_matches('[').trim_end_matches(']');
    let port = uri.port_u16().unwrap_or(if uri.scheme_str() == Some("wss") { 443 } else { 80 });
    if host.is_empty() {
        return Err((0, "HttpError: InvalidUrl".into()));
    }
    let addrs: Vec<_> = match (host, port).to_socket_addrs() {
        Ok(a) => a.collect(),
        Err(_) => return Err((0, "HttpError: DnsResolve".into())),
    };
    for a in &addrs {
        if cancel.fired() {
            break;
        }
        if let Ok(s) = TcpStream::connect_timeout(a, CONNECT_TIMEOUT) {
            return Ok(s);
        }
    }
    Err((0, if addrs.is_empty() { "HttpError: DnsResolve" } else { "HttpError: ConnectFail" }.into()))
}

/// The WebSocket's TCP stream: a read that times out (the worker's read
/// tick) reports WouldBlock, which tungstenite treats as "try again" in the
/// handshake as in the open loop (Windows reports a timed-out read as
/// TimedOut, which would fail the handshake).
struct Tick(TcpStream);

impl std::io::Read for Tick {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        match self.0.read(buf) {
            Err(e) if e.kind() == std::io::ErrorKind::TimedOut => Err(std::io::ErrorKind::WouldBlock.into()),
            r => r,
        }
    }
}

impl std::io::Write for Tick {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.0.write(buf)
    }
    fn flush(&mut self) -> std::io::Result<()> {
        self.0.flush()
    }
}

fn ws_run(id: u64, req: Request, rx: Receiver<Cmd>, cancel: &Cancel) {
    use tungstenite::client::IntoClientRequest;
    use tungstenite::handshake::HandshakeError;
    use tungstenite::http::{HeaderName, HeaderValue};
    use tungstenite::stream::MaybeTlsStream;
    use tungstenite::Message;

    // http(s) URLs go to ws(s) (the same server).
    let url = if let Some(r) = req.url.strip_prefix("http://") {
        format!("ws://{r}")
    } else if let Some(r) = req.url.strip_prefix("https://") {
        format!("wss://{r}")
    } else {
        req.url.clone()
    };
    let mut r = match url.as_str().into_client_request() {
        Ok(r) => r,
        Err(_) => return push(id, Event::Error(0, "HttpError: InvalidUrl".into())),
    };
    for (k, v) in &req.headers {
        if let (Ok(n), Ok(v)) = (HeaderName::from_bytes(k.as_bytes()), HeaderValue::from_str(v)) {
            r.headers_mut().insert(n, v);
        }
    }
    let sock = match ws_connect(r.uri(), cancel) {
        Ok(s) => s,
        Err((code, msg)) => {
            if !cancel.fired() {
                push(id, Event::Error(code, msg));
            }
            return;
        }
    };
    if cancel.fired() {
        return;
    }
    // Short read timeouts, from the TLS / upgrade handshake on: a worker
    // waiting on a silent server notices a cancel within a tick, and the
    // open loop also serves Send / Close.
    let _ = sock.set_read_timeout(Some(Duration::from_millis(15)));
    let _ = sock.set_write_timeout(Some(WRITE_TIMEOUT));
    let started = Instant::now();
    let mut hs = tungstenite::client_tls_with_config(r, Tick(sock), None, None);
    let (mut ws, resp) = loop {
        match hs {
            Ok(x) => break x,
            Err(_) if cancel.fired() => return,
            Err(HandshakeError::Failure(e)) => {
                let (code, msg) = ws_error(&e);
                return push(id, Event::Error(code, msg));
            }
            Err(HandshakeError::Interrupted(mid)) => {
                if started.elapsed() > CONNECT_TIMEOUT {
                    return push(id, Event::Error(0, "HttpError: TimedOut".into()));
                }
                hs = mid.handshake();
            }
        }
    };
    cancel.opened.store(true, Ordering::SeqCst);
    if cancel.fired() {
        return;
    }
    let headers = header_text(resp.headers().iter().map(|(k, v)| (k.as_str(), String::from_utf8_lossy(v.as_bytes()).into_owned())));
    push(id, Event::Opened(resp.status().as_u16(), headers));

    let mut closing: Option<Instant> = None;
    // Since when writes have been blocked (a peer that stopped reading);
    // past WRITE_TIMEOUT the connection fails.
    let mut stalled: Option<Instant> = None;
    loop {
        loop {
            match rx.try_recv() {
                Ok(Cmd::Send(d)) => {
                    // Text when it is UTF-8 (JSON, the common case), else binary.
                    let m = match String::from_utf8(d) {
                        Ok(t) => Message::text(t),
                        Err(e) => Message::binary(e.into_bytes()),
                    };
                    // A message that can't be written ends the connection
                    // with Error: it is never dropped quietly. (A write that
                    // would block stays queued in tungstenite and goes out
                    // with the next flush.)
                    match ws_send(&mut ws, m, cancel) {
                        Ok(()) => stalled = None,
                        Err(e) if would_block(&e) => {
                            stalled.get_or_insert_with(Instant::now);
                        }
                        Err(e) => return push(id, send_failed(&e)),
                    }
                }
                Ok(Cmd::Close) => {
                    if closing.is_none() {
                        let _ = ws.close(None);
                        closing = Some(Instant::now());
                    }
                }
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Disconnected) => {
                    // Forgotten (unload / VM gone / collected): a close
                    // frame if it goes out quickly, then the socket goes.
                    match ws.get_mut() {
                        MaybeTlsStream::Plain(s) => {
                            let _ = s.0.set_write_timeout(Some(Duration::from_secs(1)));
                        }
                        MaybeTlsStream::Rustls(s) => {
                            let _ = s.sock.0.set_write_timeout(Some(Duration::from_secs(1)));
                        }
                        _ => {}
                    }
                    let _ = ws.close(None);
                    let _ = ws.flush();
                    return;
                }
            }
        }
        if closing.is_some_and(|t| t.elapsed() > Duration::from_secs(5)) {
            return push(id, Event::Closed);
        }
        match ws.read() {
            Ok(Message::Text(t)) => push(id, Event::Message(t.as_bytes().to_vec())),
            Ok(Message::Binary(b)) => push(id, Event::Message(b.to_vec())),
            Ok(_) => {} // ping / pong (answered by tungstenite) / close frame
            Err(e) if would_block(&e) => {
                // Writes still pending (a message that would have blocked,
                // pong replies) go out here; failing to is an Error too.
                match ws.flush() {
                    Ok(()) => stalled = None,
                    Err(e) if would_block(&e) => {
                        if stalled.get_or_insert_with(Instant::now).elapsed() > WRITE_TIMEOUT {
                            return push(id, Event::Error(0, "HttpError: TimedOut".into()));
                        }
                    }
                    Err(e) => return push(id, if closing.is_some() { Event::Closed } else { send_failed(&e) }),
                }
            }
            Err(tungstenite::Error::ConnectionClosed | tungstenite::Error::AlreadyClosed) => return push(id, Event::Closed),
            // The server went away without a close handshake: the
            // connection is closed all the same.
            Err(tungstenite::Error::Protocol(tungstenite::error::ProtocolError::ResetWithoutClosingHandshake)) => {
                return push(id, Event::Closed)
            }
            Err(_) if closing.is_some() => return push(id, Event::Closed),
            Err(e) => {
                let (code, msg) = ws_error(&e);
                return push(id, Event::Error(code, msg));
            }
        }
        if gone(id) && closing.is_none() {
            let _ = ws.close(None);
            closing = Some(Instant::now());
        }
    }
}

fn would_block(e: &tungstenite::Error) -> bool {
    matches!(e, tungstenite::Error::Io(io) if matches!(io.kind(), std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut))
}

/// A message (or a queued frame) could not be written: the transport is
/// gone. Error(0, ...) with HttpError's name for a closed connection.
fn send_failed(e: &tungstenite::Error) -> Event {
    use tungstenite::Error as E;
    match e {
        E::Io(_) | E::ConnectionClosed | E::AlreadyClosed | E::Protocol(_) => Event::Error(0, "HttpError: ConnectionClosed".into()),
        other => {
            let (code, msg) = ws_error(other);
            Event::Error(code, msg)
        }
    }
}

/// (responseStatusCode, errorMessage) for a failed WebSocket: the server's
/// HTTP status when it answered the upgrade with one, else 0 and an
/// "HttpError: <Enum.HttpError name>" message.
fn ws_error(e: &tungstenite::Error) -> (i64, String) {
    use tungstenite::Error as E;
    match e {
        E::Http(resp) => {
            let s = resp.status();
            (s.as_u16() as i64, format!("HTTP {} ({})", s.as_u16(), s.canonical_reason().unwrap_or("")))
        }
        E::Io(io) if io.kind() == std::io::ErrorKind::ConnectionRefused => (0, "HttpError: ConnectFail".into()),
        E::Io(io) if io.to_string().contains("lookup address") || io.to_string().contains("No such host") => {
            (0, "HttpError: DnsResolve".into())
        }
        E::Io(io)
            if matches!(
                io.kind(),
                std::io::ErrorKind::ConnectionReset | std::io::ErrorKind::ConnectionAborted | std::io::ErrorKind::BrokenPipe
            ) =>
        {
            (0, "HttpError: ConnectionClosed".into())
        }
        E::Url(tungstenite::error::UrlError::UnableToConnect(_)) => (0, "HttpError: ConnectFail".into()),
        E::Url(_) => (0, "HttpError: InvalidUrl".into()),
        E::Tls(_) => (0, "HttpError: SslConnectFail".into()),
        other => (0, format!("HttpError: Unknown ({other})")),
    }
}

// ------------------------------------------------------------------ SSE / RawStream

fn stream_run(id: u64, req: Request, rx: Receiver<Cmd>, cancel: &Cancel) {
    let agent: ureq::Agent = ureq::Agent::config_builder()
        .timeout_resolve(Some(CONNECT_TIMEOUT))
        .timeout_connect(Some(CONNECT_TIMEOUT))
        .http_status_as_error(false)
        .user_agent("RobloxStudio/WinInet RobloxApp/0.746.0.7461269 (GlobalDist; RobloxDirectDownload)")
        .build()
        .into();
    // From here a cancel shuts the agent down: the request, the wait for
    // the response and a body read in progress all return at once.
    if !cancel.set_agent(&agent) {
        return;
    }
    let mut b = ureq::http::Request::builder().method(req.method.as_str()).uri(&req.url);
    for (k, v) in &req.headers {
        b = b.header(k.as_str(), v.as_str());
    }
    if req.kind == Kind::Sse && !req.headers.iter().any(|(k, _)| k.eq_ignore_ascii_case("Accept")) {
        b = b.header("Accept", "text/event-stream");
    }
    let resp = match req.body {
        Some(body) => b.body(body).map_err(|e| e.to_string()).map(|r| agent.run(r)),
        None => b.body(()).map_err(|e| e.to_string()).map(|r| agent.run(r)),
    };
    if cancel.fired() {
        return;
    }
    let resp = match resp {
        Ok(Ok(r)) => r,
        Ok(Err(e)) => return push(id, Event::Error(0, super::http::describe(e))),
        Err(_) => return push(id, Event::Error(0, "HttpError: InvalidUrl".into())),
    };
    let status = resp.status();
    let headers = header_text(resp.headers().iter().map(|(k, v)| (k.as_str(), String::from_utf8_lossy(v.as_bytes()).into_owned())));
    let mut reader = resp.into_body().into_reader();
    if status.as_u16() >= 400 {
        let mut body = Vec::new();
        let _ = reader.by_ref().take(64 * 1024).read_to_end(&mut body);
        if cancel.fired() {
            return;
        }
        let msg = if body.is_empty() { status.canonical_reason().unwrap_or("").to_string() } else { String::from_utf8_lossy(&body).into_owned() };
        return push(id, Event::Error(status.as_u16() as i64, msg));
    }
    push(id, Event::Opened(status.as_u16(), headers));
    let mut sse = Sse::default();
    let mut buf = vec![0u8; 16 * 1024];
    loop {
        if cancel.fired() || matches!(rx.try_recv(), Ok(Cmd::Close) | Err(TryRecvError::Disconnected)) || gone(id) {
            return;
        }
        let n = reader.read(&mut buf);
        // (a cancel wakes the read with an error or a false end of body)
        if cancel.fired() {
            return;
        }
        match n {
            Ok(0) => {
                if req.kind == Kind::Sse {
                    for m in sse.feed(b"\n\n") {
                        push(id, Event::Message(m));
                    }
                }
                return push(id, Event::Closed);
            }
            Ok(n) => {
                if req.kind == Kind::Raw {
                    push(id, Event::Message(buf[..n].to_vec()));
                } else {
                    for m in sse.feed(&buf[..n]) {
                        push(id, Event::Message(m));
                    }
                }
            }
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {}
            Err(e) => return push(id, Event::Error(0, format!("HttpError: Unknown ({e})"))),
        }
    }
}

/// text/event-stream: each event's `data` lines (joined with "\n") make one
/// message; comments and other fields are skipped.
#[derive(Default)]
struct Sse {
    line: Vec<u8>,
    data: Option<Vec<u8>>,
}

impl Sse {
    fn feed(&mut self, bytes: &[u8]) -> Vec<Vec<u8>> {
        let mut out = Vec::new();
        for &c in bytes {
            if c != b'\n' {
                self.line.push(c);
                continue;
            }
            let mut line = std::mem::take(&mut self.line);
            if line.last() == Some(&b'\r') {
                line.pop();
            }
            if line.is_empty() {
                if let Some(d) = self.data.take() {
                    out.push(d);
                }
                continue;
            }
            if line[0] == b':' {
                continue;
            }
            let (field, value) = match line.iter().position(|&c| c == b':') {
                Some(i) => {
                    let v = &line[i + 1..];
                    (&line[..i], v.strip_prefix(b" ").unwrap_or(v))
                }
                None => (&line[..], &b""[..]),
            };
            if field == b"data" {
                match &mut self.data {
                    Some(d) => {
                        d.push(b'\n');
                        d.extend_from_slice(value);
                    }
                    None => self.data = Some(value.to_vec()),
                }
            }
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use std::net::TcpListener;

    fn start(req: Request) -> u64 {
        super::start(req, None)
    }

    #[test]
    fn sse_parsing() {
        let mut s = Sse::default();
        let mut got = s.feed(b": hi\r\ndata: one\r\n\r\nevent: x\ndata: two\ndata:three\n");
        got.extend(s.feed(b"\nid: 3\n\n"));
        assert_eq!(got, vec![b"one".to_vec(), b"two\nthree".to_vec()]);
    }

    fn wait(id: u64, mut until: impl FnMut(&[Event]) -> bool) -> Vec<Event> {
        let mut all = Vec::new();
        let t = Instant::now();
        while t.elapsed() < Duration::from_secs(10) {
            all.extend(poll(id));
            if until(&all) {
                break;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        all
    }

    #[test]
    fn websocket_message_wakes_idle_viewport() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let (send_message, ready) = mpsc::channel();
        let (finish, done) = mpsc::channel();
        let server = std::thread::spawn(move || {
            let (socket, _) = listener.accept().unwrap();
            let mut ws = tungstenite::accept(socket).unwrap();
            ready.recv_timeout(Duration::from_secs(5)).unwrap();
            ws.send(tungstenite::Message::text("wake")).unwrap();
            let _ = done.recv_timeout(Duration::from_secs(5));
        });

        let ctx = egui::Context::default();
        let (wake, repaint) = mpsc::channel();
        ctx.set_request_repaint_callback(move |info| {
            let _ = wake.send((info.viewport_id, info.delay));
        });
        let id = super::start(req(Kind::WebSocket, format!("ws://127.0.0.1:{port}/")), Some(ctx.clone()));
        let timeout = Duration::from_secs(5);
        assert_eq!(repaint.recv_timeout(timeout).unwrap(), (egui::ViewportId::ROOT, Duration::ZERO));
        assert!(matches!(poll(id).as_slice(), [Event::Opened(101, _)]));

        // Consume both passes from the initial repaint, then let the worker
        // receive a message while no UI pass is running.
        for _ in 0..2 {
            let _ = ctx.run_logic(&egui::RawInput::default(), |_| {});
        }
        while repaint.try_recv().is_ok() {}
        send_message.send(()).unwrap();
        assert_eq!(repaint.recv_timeout(timeout).unwrap(), (egui::ViewportId::ROOT, Duration::ZERO));
        assert_eq!(poll(id), vec![Event::Message(b"wake".to_vec())]);
        forget(id);
        finish.send(()).unwrap();
        server.join().unwrap();
    }

    #[test]
    fn websocket_echo_close_and_refused() {
        let l = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = l.local_addr().unwrap().port();
        std::thread::spawn(move || {
            let (s, _) = l.accept().unwrap();
            let mut ws = tungstenite::accept(s).unwrap();
            ws.send(tungstenite::Message::binary(vec![0u8, 1, 255])).unwrap();
            loop {
                match ws.read() {
                    Ok(m) if m.is_text() || m.is_binary() => ws.send(m).unwrap(),
                    Ok(_) => {}
                    Err(_) => break,
                }
            }
        });
        let id = start(Request { kind: Kind::WebSocket, method: "GET".into(), url: format!("ws://127.0.0.1:{port}/x"), headers: vec![], body: None });
        let ev = wait(id, |e| e.len() >= 2);
        assert!(matches!(&ev[0], Event::Opened(101, h) if h.to_ascii_lowercase().contains("upgrade: websocket")), "{ev:?}");
        assert_eq!(ev[1], Event::Message(vec![0, 1, 255]));
        assert!(send(id, b"hello".to_vec()));
        let ev = wait(id, |e| !e.is_empty());
        assert_eq!(ev, vec![Event::Message(b"hello".to_vec())]);
        close(id, Kind::WebSocket);
        let ev = wait(id, |e| !e.is_empty());
        assert_eq!(ev, vec![Event::Closed]);
        forget(id);

        let dead = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = dead.local_addr().unwrap().port();
        drop(dead);
        let id = start(Request { kind: Kind::WebSocket, method: "GET".into(), url: format!("ws://127.0.0.1:{port}/"), headers: vec![], body: None });
        let ev = wait(id, |e| !e.is_empty());
        assert_eq!(ev, vec![Event::Error(0, "HttpError: ConnectFail".into())]);
    }

    #[test]
    fn raw_stream_chunks_then_closed() {
        let l = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = l.local_addr().unwrap().port();
        std::thread::spawn(move || {
            let (mut s, _) = l.accept().unwrap();
            let mut b = [0u8; 2048];
            let _ = s.read(&mut b);
            s.write_all(b"HTTP/1.1 200 OK\r\nContent-Type: text/plain\r\nTransfer-Encoding: chunked\r\n\r\n").unwrap();
            for part in ["abc", "def"] {
                write!(s, "{:x}\r\n{part}\r\n", part.len()).unwrap();
                s.flush().unwrap();
                std::thread::sleep(Duration::from_millis(100));
            }
            s.write_all(b"0\r\n\r\n").unwrap();
        });
        let id = start(Request { kind: Kind::Raw, method: "GET".into(), url: format!("http://127.0.0.1:{port}/"), headers: vec![], body: None });
        let ev = wait(id, |e| e.last() == Some(&Event::Closed));
        assert!(matches!(&ev[0], Event::Opened(200, _)), "{ev:?}");
        let data: Vec<u8> = ev.iter().filter_map(|e| if let Event::Message(m) = e { Some(m.clone()) } else { None }).flatten().collect();
        assert_eq!(data, b"abcdef");
        assert_eq!(ev.last(), Some(&Event::Closed));
    }

    // ---------------------------------------------------------- lifecycle

    /// Waits up to `secs` for `f`.
    fn within(secs: f32, mut f: impl FnMut() -> bool) -> bool {
        let t = Instant::now();
        while t.elapsed() < Duration::from_secs_f32(secs) {
            if f() {
                return true;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        f()
    }

    /// A server that answers the first connection per `answer` and then
    /// holds it idle (sends nothing more, never closes). The receiver gets
    /// the moment the client's side of the socket ended (EOF or error on
    /// the server's read).
    fn idle_server(answer: &'static str) -> (u16, Receiver<Instant>) {
        idle_server_accepting(answer).0
    }

    /// `idle_server`, plus a receiver that gets () once the connection is
    /// accepted.
    fn idle_server_accepting(answer: &'static str) -> ((u16, Receiver<Instant>), Receiver<()>) {
        let l = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = l.local_addr().unwrap().port();
        let (tx, rx) = mpsc::channel();
        let (atx, arx) = mpsc::channel();
        std::thread::spawn(move || {
            let (mut s, _) = l.accept().unwrap();
            let _ = atx.send(());
            let _ = s.set_read_timeout(Some(Duration::from_secs(60)));
            let mut b = [0u8; 4096];
            match answer {
                // Accept the TCP connection and say nothing at all.
                "silent" => {}
                // Read the request, then say nothing.
                "no-response" => {
                    let _ = s.read(&mut b);
                }
                "raw" => {
                    let _ = s.read(&mut b);
                    s.write_all(b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n3\r\nabc\r\n").unwrap();
                }
                "sse" => {
                    let _ = s.read(&mut b);
                    s.write_all(b"HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nTransfer-Encoding: chunked\r\n\r\n").unwrap();
                    s.write_all(b"9\r\ndata: x\n\n\r\n").unwrap();
                }
                _ => unreachable!(),
            }
            let _ = s.flush();
            // Idle until the client's side goes away.
            loop {
                match s.read(&mut b) {
                    Ok(0) | Err(_) => break,
                    Ok(_) => {}
                }
            }
            let _ = tx.send(Instant::now());
        });
        ((port, rx), arx)
    }

    fn req(kind: Kind, url: String) -> Request {
        Request { kind, method: "GET".into(), url, headers: vec![], body: None }
    }

    /// An idle SSE / RawStream (headers and one message in, then nothing):
    /// Close reports Closed, the socket is shut (the server sees it go)
    /// and the worker thread ends, instead of staying blocked in its read.
    #[test]
    fn close_interrupts_idle_streams() {
        for (kind, answer) in [(Kind::Raw, "raw"), (Kind::Sse, "sse")] {
            let (port, ended) = idle_server(answer);
            let url = format!("http://127.0.0.1:{port}/idle");
            let id = start(req(kind, url.clone()));
            let ev = wait(id, |e| e.iter().any(|e| matches!(e, Event::Message(_))));
            assert!(matches!(&ev[0], Event::Opened(200, _)), "{kind:?} {ev:?}");
            std::thread::sleep(Duration::from_millis(200));
            assert_eq!(workers_for(&url), 1, "{kind:?}: the worker sits in its read");
            let t = Instant::now();
            close(id, kind);
            assert_eq!(poll(id), vec![Event::Closed], "{kind:?}");
            let gone_at = ended.recv_timeout(Duration::from_secs(5)).expect("server never saw the client go");
            assert!(gone_at.duration_since(t) < Duration::from_secs(2), "{kind:?}: {:?}", gone_at.duration_since(t));
            assert!(within(2.0, || workers_for(&url) == 0), "{kind:?}: worker still running");
            forget(id);
        }
    }

    /// The plugin unloads / the VM goes away (`forget`) with idle streams
    /// open: the same, no events.
    #[test]
    fn forget_interrupts_idle_streams() {
        for (kind, answer) in [(Kind::Raw, "raw"), (Kind::Sse, "sse")] {
            let (port, ended) = idle_server(answer);
            let url = format!("http://127.0.0.1:{port}/forget");
            let id = start(req(kind, url.clone()));
            wait(id, |e| e.iter().any(|e| matches!(e, Event::Message(_))));
            forget(id);
            assert!(ended.recv_timeout(Duration::from_secs(2)).is_ok(), "{kind:?}: server never saw the client go");
            assert!(within(2.0, || workers_for(&url) == 0), "{kind:?}: worker still running");
            assert!(poll(id).is_empty());
        }
    }

    /// Close / forget while still connecting: a WebSocket whose server
    /// never answers the upgrade, a stream whose server never sends its
    /// response. Closed at once (Close) and the worker ends.
    #[test]
    fn close_while_connecting() {
        for (kind, scheme, answer) in
            [(Kind::WebSocket, "ws", "silent"), (Kind::WebSocket, "ws", "no-response"), (Kind::Raw, "http", "no-response"), (Kind::Sse, "http", "silent")]
        {
            for use_forget in [false, true] {
                let ((port, ended), accepted) = idle_server_accepting(answer);
                let url = format!("{scheme}://127.0.0.1:{port}/connecting");
                let id = start(req(kind, url.clone()));
                // Connected, now inside the handshake / waiting for the
                // response (a cancel before the TCP connect means none is made).
                accepted.recv_timeout(Duration::from_secs(5)).expect("never connected");
                std::thread::sleep(Duration::from_millis(200));
                assert!(poll(id).is_empty(), "{kind:?} {answer}: still connecting");
                assert_eq!(workers_for(&url), 1);
                if use_forget {
                    forget(id);
                } else {
                    close(id, kind);
                    assert_eq!(poll(id), vec![Event::Closed], "{kind:?} {answer}");
                }
                assert!(ended.recv_timeout(Duration::from_secs(2)).is_ok(), "{kind:?} {answer} forget={use_forget}: server never saw it go");
                assert!(within(2.0, || workers_for(&url) == 0), "{kind:?} {answer} forget={use_forget}: worker still running");
                std::thread::sleep(Duration::from_millis(50));
                assert!(poll(id).is_empty(), "nothing after Closed");
                forget(id);
            }
        }
    }

    /// A WebSocket server that reads and counts messages and never sends.
    /// Reports each text message, then when the connection ended (and how).
    fn counting_ws_server() -> (u16, Receiver<String>) {
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
                    Err(e) => {
                        let _ = tx.send(format!("ended {e}"));
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

    /// The client's writes break (the transport dies under it: injected,
    /// see `break_writes_for`): the next Send's failure is reported
    /// as Error (not swallowed, no Closed after it), the worker ends, and
    /// later Sends are refused instead of being queued for nobody.
    #[test]
    fn websocket_failed_send_is_an_error() {
        let (port, got) = counting_ws_server();
        let url = format!("ws://127.0.0.1:{port}/send-fails");
        let id = start(req(Kind::WebSocket, url.clone()));
        let ev = wait(id, |e| !e.is_empty());
        assert!(matches!(&ev[0], Event::Opened(101, _)), "{ev:?}");
        assert!(send(id, b"one".to_vec()));
        assert_eq!(got.recv_timeout(Duration::from_secs(5)).unwrap(), "text one");
        // The transport's writes start failing.
        assert_eq!(break_writes_for(&url), 1);
        assert!(send(id, b"two".to_vec()), "queued (the worker hasn't failed yet)");
        let ev = wait(id, |e| !e.is_empty());
        assert_eq!(ev, vec![Event::Error(0, "HttpError: ConnectionClosed".into())]);
        assert!(within(2.0, || workers_for(&url) == 0), "worker still running");
        assert!(!send(id, b"three".to_vec()), "Send after the failure is refused");
        std::thread::sleep(Duration::from_millis(100));
        assert!(poll(id).is_empty(), "no Closed after Error");
        // The server never got "two" (or anything after it).
        let rest: Vec<String> = got.try_iter().collect();
        assert!(!rest.iter().any(|m| m.starts_with("text ")), "{rest:?}");
        forget(id);
    }

    /// An open, idle WebSocket: Send after Close is refused (not dropped);
    /// `forget` (unload / VM gone) sends a close frame and the worker ends.
    #[test]
    fn websocket_close_and_forget_end_the_worker() {
        let (port, got) = counting_ws_server();
        let url = format!("ws://127.0.0.1:{port}/forget");
        let id = start(req(Kind::WebSocket, url.clone()));
        wait(id, |e| !e.is_empty());
        std::thread::sleep(Duration::from_millis(200));
        assert_eq!(workers_for(&url), 1);
        forget(id);
        assert_eq!(got.recv_timeout(Duration::from_secs(2)).unwrap(), "close frame");
        assert!(within(2.0, || workers_for(&url) == 0), "worker still running");

        let (port, got) = counting_ws_server();
        let url = format!("ws://127.0.0.1:{port}/close");
        let id = start(req(Kind::WebSocket, url.clone()));
        wait(id, |e| !e.is_empty());
        close(id, Kind::WebSocket);
        assert!(!send(id, b"late".to_vec()), "Send after Close is refused");
        assert_eq!(got.recv_timeout(Duration::from_secs(2)).unwrap(), "close frame");
        let ev = wait(id, |e| !e.is_empty());
        assert_eq!(ev, vec![Event::Closed]);
        assert!(within(2.0, || workers_for(&url) == 0), "worker still running");
        forget(id);
    }
}
