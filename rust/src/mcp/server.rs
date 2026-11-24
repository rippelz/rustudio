//! The MCP transport: Streamable HTTP (MCP 2025-06-18) on 127.0.0.1 only.
//!
//! `POST /mcp` takes one JSON-RPC message and answers with one JSON
//! response (`application/json`; notifications get 202). No server-sent
//! events: every tool answers its own call, and the tool list doesn't
//! change (a disabled group's tools fail with a message instead). `GET
//! /mcp` is 405 as the spec allows; `DELETE /mcp` ends a client's session.
//!
//! Every request needs `Authorization: Bearer <token>` (unless the person
//! turned the token off) and, when the browser sends an Origin, a localhost
//! one (DNS rebinding). Tool calls are handed to the editor's UI thread
//! ([`Job`]) and the connection's thread waits for the answer; the editor
//! runs them between frames (src/mcp/exec.rs).

use std::collections::HashMap;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{channel, Receiver, Sender};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde_json::{json, Value as J};

pub const PROTOCOL_VERSIONS: [&str; 3] = ["2025-06-18", "2025-03-26", "2024-11-05"];
const MAX_BODY: usize = 64 << 20;
/// How long a connection's thread waits for the editor to answer a call.
const CALL_TIMEOUT: Duration = Duration::from_secs(20 * 60);
/// A client not heard from for this long no longer counts as connected.
pub const CLIENT_IDLE: Duration = Duration::from_secs(10 * 60);

/// A tool's answer: MCP content items and whether it failed.
#[derive(Clone, Debug)]
pub struct ToolResult {
    pub content: Vec<J>,
    pub is_error: bool,
}

impl ToolResult {
    pub fn text(t: impl Into<String>) -> Self {
        ToolResult { content: vec![json!({ "type": "text", "text": t.into() })], is_error: false }
    }
    pub fn json(v: &J) -> Self {
        Self::text(serde_json::to_string_pretty(v).unwrap_or_default())
    }
    pub fn error(t: impl Into<String>) -> Self {
        ToolResult { content: vec![json!({ "type": "text", "text": t.into() })], is_error: true }
    }
    /// The first text item (for the activity log).
    pub fn summary(&self) -> String {
        self.content
            .iter()
            .find_map(|c| c.get("text").and_then(|t| t.as_str()).map(String::from))
            .or_else(|| self.content.first().map(|c| format!("[{}]", c["type"].as_str().unwrap_or("?"))))
            .unwrap_or_default()
    }
}

/// A tool call for the UI thread.
pub struct Job {
    pub tool: String,
    pub args: J,
    /// The client's name (initialize's clientInfo), for the activity log.
    pub client: String,
    pub reply: Sender<ToolResult>,
}

#[derive(Clone, Debug)]
pub struct ClientInfo {
    pub name: String,
    pub last_seen: Instant,
}

/// What the connection threads and the editor share.
#[derive(Default)]
pub struct Shared {
    pub token: String,
    pub require_token: bool,
    /// MCP sessions by Mcp-Session-Id.
    pub clients: HashMap<String, ClientInfo>,
    pub ctx: Option<egui::Context>,
    pub info: J,
}

impl Shared {
    /// Clients heard from lately.
    pub fn connected(&self) -> Vec<ClientInfo> {
        let mut v: Vec<ClientInfo> = self.clients.values().filter(|c| c.last_seen.elapsed() < CLIENT_IDLE).cloned().collect();
        v.sort_by_key(|c| std::cmp::Reverse(c.last_seen));
        v
    }
}

pub struct Server {
    pub port: u16,
    pub jobs: Receiver<Job>,
    pub shared: Arc<Mutex<Shared>>,
    stop: Arc<AtomicBool>,
}

impl Drop for Server {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        // Wake the accept loop so it sees the stop.
        let _ = TcpStream::connect_timeout(&([127, 0, 0, 1], self.port).into(), Duration::from_millis(200));
    }
}

impl Server {
    /// Listens on 127.0.0.1:`port`, or the next free one of `tries` ports.
    pub fn start(port: u16, tries: u16, shared: Arc<Mutex<Shared>>) -> Result<Server, String> {
        let mut last = String::new();
        for p in port..port.saturating_add(tries.max(1)) {
            match TcpListener::bind(("127.0.0.1", p)) {
                Ok(listener) => return Ok(Self::serve(listener, p, shared)),
                Err(e) => last = format!("127.0.0.1:{p}: {e}"),
            }
        }
        Err(format!("no free port from {port} ({last})"))
    }

    fn serve(listener: TcpListener, port: u16, shared: Arc<Mutex<Shared>>) -> Server {
        let (tx, jobs) = channel::<Job>();
        let stop = Arc::new(AtomicBool::new(false));
        let stop2 = stop.clone();
        let shared2 = shared.clone();
        let _ = std::thread::Builder::new().name("mcp-listen".into()).spawn(move || {
            for stream in listener.incoming() {
                if stop2.load(Ordering::Acquire) {
                    break;
                }
                let Ok(stream) = stream else { continue };
                let (tx, shared, stop) = (tx.clone(), shared2.clone(), stop2.clone());
                let _ = std::thread::Builder::new().name("mcp-conn".into()).spawn(move || connection(stream, &tx, &shared, &stop));
            }
        });
        log::info!("MCP server listening on 127.0.0.1:{port}");
        Server { port, jobs, shared, stop }
    }
}

struct Request {
    method: String,
    path: String,
    headers: Vec<(String, String)>,
    body: Vec<u8>,
}

impl Request {
    fn header(&self, name: &str) -> Option<&str> {
        self.headers.iter().find(|(k, _)| k.eq_ignore_ascii_case(name)).map(|(_, v)| v.as_str())
    }
}

fn read_request(r: &mut BufReader<TcpStream>) -> Result<Option<Request>, String> {
    let mut line = String::new();
    if r.read_line(&mut line).map_err(|e| e.to_string())? == 0 {
        return Ok(None);
    }
    let mut parts = line.split_whitespace();
    let method = parts.next().unwrap_or("").to_string();
    let path = parts.next().unwrap_or("").to_string();
    let mut headers = Vec::new();
    loop {
        let mut h = String::new();
        if r.read_line(&mut h).map_err(|e| e.to_string())? == 0 {
            return Ok(None);
        }
        let h = h.trim_end();
        if h.is_empty() {
            break;
        }
        if let Some((k, v)) = h.split_once(':') {
            headers.push((k.trim().to_string(), v.trim().to_string()));
        }
        if headers.len() > 100 {
            return Err("too many headers".into());
        }
    }
    let mut req = Request { method, path, headers, body: Vec::new() };
    if req.header("transfer-encoding").is_some_and(|t| t.eq_ignore_ascii_case("chunked")) {
        loop {
            let mut size = String::new();
            r.read_line(&mut size).map_err(|e| e.to_string())?;
            let n = usize::from_str_radix(size.trim().split(';').next().unwrap_or("0"), 16).map_err(|e| e.to_string())?;
            if req.body.len() + n > MAX_BODY {
                return Err("body too large".into());
            }
            let start = req.body.len();
            req.body.resize(start + n, 0);
            r.read_exact(&mut req.body[start..]).map_err(|e| e.to_string())?;
            let mut crlf = String::new();
            r.read_line(&mut crlf).map_err(|e| e.to_string())?;
            if n == 0 {
                break;
            }
        }
    } else if let Some(n) = req.header("content-length").and_then(|v| v.parse::<usize>().ok()) {
        if n > MAX_BODY {
            return Err("body too large".into());
        }
        req.body.resize(n, 0);
        r.read_exact(&mut req.body).map_err(|e| e.to_string())?;
    }
    Ok(Some(req))
}

fn respond(out: &mut TcpStream, status: &str, headers: &[(&str, String)], body: &[u8]) -> std::io::Result<()> {
    let mut head = format!("HTTP/1.1 {status}\r\nContent-Length: {}\r\nCache-Control: no-store\r\n", body.len());
    for (k, v) in headers {
        head += &format!("{k}: {v}\r\n");
    }
    head += "\r\n";
    out.write_all(head.as_bytes())?;
    out.write_all(body)?;
    out.flush()
}

/// An Origin a page on this machine sends (or none: not a browser).
pub fn origin_allowed(origin: Option<&str>) -> bool {
    let Some(o) = origin else { return true };
    let host = o.split("://").nth(1).unwrap_or(o);
    let host = host.split('/').next().unwrap_or(host);
    let host = if host.starts_with('[') { host.split(']').next().map(|h| &h[1..]).unwrap_or(host) } else { host.split(':').next().unwrap_or(host) };
    matches!(host, "localhost" | "127.0.0.1" | "::1")
}

/// Whether the request carries the token (constant-time compare).
pub fn token_ok(header: Option<&str>, token: &str) -> bool {
    let Some(h) = header else { return false };
    let given = h.strip_prefix("Bearer ").or_else(|| h.strip_prefix("bearer ")).unwrap_or(h).trim();
    given.len() == token.len() && given.bytes().zip(token.bytes()).fold(0u8, |acc, (a, b)| acc | (a ^ b)) == 0
}

fn connection(stream: TcpStream, tx: &Sender<Job>, shared: &Arc<Mutex<Shared>>, stop: &AtomicBool) {
    let _ = stream.set_read_timeout(Some(Duration::from_secs(120)));
    let Ok(mut out) = stream.try_clone() else { return };
    let mut reader = BufReader::new(stream);
    loop {
        if stop.load(Ordering::Acquire) {
            return;
        }
        let req = match read_request(&mut reader) {
            Ok(Some(r)) => r,
            Ok(None) => return,
            Err(e) => {
                let _ = respond(&mut out, "400 Bad Request", &[("Content-Type", "text/plain".into())], e.as_bytes());
                return;
            }
        };
        let close = req.header("connection").is_some_and(|c| c.eq_ignore_ascii_case("close"));
        if handle(&req, &mut out, tx, shared).is_err() || close {
            return;
        }
    }
}

fn handle(req: &Request, out: &mut TcpStream, tx: &Sender<Job>, shared: &Arc<Mutex<Shared>>) -> std::io::Result<()> {
    let path = req.path.split('?').next().unwrap_or("");
    if !origin_allowed(req.header("origin")) {
        return respond(out, "403 Forbidden", &[], b"Origin not allowed");
    }
    let (token, require) = {
        let s = shared.lock().unwrap();
        (s.token.clone(), s.require_token)
    };
    if require && !token_ok(req.header("authorization"), &token) {
        return respond(
            out,
            "401 Unauthorized",
            &[("Content-Type", "application/json".into()), ("WWW-Authenticate", "Bearer".into())],
            br#"{"error":"rustudio MCP: missing or wrong token (Authorization: Bearer <token>; the MCP panel in rustudio's top bar shows it)"}"#,
        );
    }
    if path != "/mcp" && path != "/" {
        return respond(out, "404 Not Found", &[], b"not found");
    }
    let session = req.header("mcp-session-id").map(String::from);
    match req.method.as_str() {
        "GET" => respond(out, "405 Method Not Allowed", &[("Allow", "POST, DELETE".into())], b""),
        "DELETE" => {
            if let Some(id) = session {
                shared.lock().unwrap().clients.remove(&id);
            }
            respond(out, "200 OK", &[], b"")
        }
        "POST" => {
            let msg: J = match serde_json::from_slice(&req.body) {
                Ok(m) => m,
                Err(e) => {
                    let body = json!({ "jsonrpc": "2.0", "id": null, "error": { "code": -32700, "message": format!("Parse error: {e}") } });
                    return respond(out, "400 Bad Request", &[("Content-Type", "application/json".into())], body.to_string().as_bytes());
                }
            };
            let client = session
                .as_ref()
                .and_then(|id| {
                    let mut s = shared.lock().unwrap();
                    let c = s.clients.get_mut(id)?;
                    c.last_seen = Instant::now();
                    Some(c.name.clone())
                })
                .unwrap_or_else(|| "client".into());
            let info = shared.lock().unwrap().info.clone();
            let mut call = |tool: &str, args: J| call_editor(tx, shared, tool, args, &client);
            let reply = handle_message(&msg, &info, &mut call);
            let mut headers = vec![("Content-Type", "application/json".to_string())];
            if let Some(name) = reply.initialized {
                let id = super::settings::new_token();
                shared.lock().unwrap().clients.insert(id.clone(), ClientInfo { name, last_seen: Instant::now() });
                headers.push(("Mcp-Session-Id", id));
            }
            match reply.response {
                Some(r) => respond(out, "200 OK", &headers, r.to_string().as_bytes()),
                None => respond(out, "202 Accepted", &[], b""),
            }
        }
        _ => respond(out, "405 Method Not Allowed", &[("Allow", "POST, DELETE".into())], b""),
    }
}

fn call_editor(tx: &Sender<Job>, shared: &Arc<Mutex<Shared>>, tool: &str, args: J, client: &str) -> ToolResult {
    let (reply, rx) = channel();
    if tx.send(Job { tool: tool.to_string(), args, client: client.to_string(), reply }).is_err() {
        return ToolResult::error("rustudio's MCP server stopped");
    }
    if let Some(ctx) = shared.lock().unwrap().ctx.clone() {
        ctx.request_repaint();
    }
    rx.recv_timeout(CALL_TIMEOUT).unwrap_or_else(|_| ToolResult::error("rustudio didn't answer the call (the window closed, or its frame loop is stuck)"))
}

pub struct Reply {
    pub response: Option<J>,
    /// An `initialize`: the client's name, for a new session.
    pub initialized: Option<String>,
}

/// One JSON-RPC message: initialize, ping, tools/list, tools/call (through
/// `call`), notifications.
pub fn handle_message(msg: &J, info: &J, call: &mut dyn FnMut(&str, J) -> ToolResult) -> Reply {
    let id = msg.get("id").cloned();
    let method = msg.get("method").and_then(|m| m.as_str()).unwrap_or("");
    let ok = |result: J| Reply { response: Some(json!({ "jsonrpc": "2.0", "id": id.clone(), "result": result })), initialized: None };
    let err = |code: i64, message: String| Reply {
        response: Some(json!({ "jsonrpc": "2.0", "id": id.clone(), "error": { "code": code, "message": message } })),
        initialized: None,
    };
    // A response or a notification: nothing to answer.
    if id.is_none() || method.is_empty() {
        return Reply { response: None, initialized: None };
    }
    let params = msg.get("params").cloned().unwrap_or(J::Null);
    match method {
        "initialize" => {
            let asked = params.get("protocolVersion").and_then(|v| v.as_str()).unwrap_or(PROTOCOL_VERSIONS[0]);
            let version = PROTOCOL_VERSIONS.iter().find(|v| **v == asked).copied().unwrap_or(PROTOCOL_VERSIONS[0]);
            let client = params
                .pointer("/clientInfo/name")
                .and_then(|n| n.as_str())
                .map(|n| match params.pointer("/clientInfo/version").and_then(|v| v.as_str()) {
                    Some(v) => format!("{n} {v}"),
                    None => n.to_string(),
                })
                .unwrap_or_else(|| "client".into());
            let mut r = ok(json!({
                "protocolVersion": version,
                "capabilities": { "tools": { "listChanged": false } },
                "serverInfo": { "name": "rustudio", "title": "RuStudio", "version": env!("CARGO_PKG_VERSION") },
                "instructions": instructions(info),
            }));
            r.initialized = Some(client);
            r
        }
        "ping" => ok(json!({})),
        "tools/list" => ok(json!({ "tools": super::tools::list() })),
        "tools/call" => {
            let Some(name) = params.get("name").and_then(|n| n.as_str()) else {
                return err(-32602, "tools/call needs a name".into());
            };
            if super::tools::find(name).is_none() {
                return err(-32602, format!("Unknown tool: {name}"));
            }
            let args = params.get("arguments").cloned().unwrap_or_else(|| json!({}));
            let r = call(name, args);
            ok(json!({ "content": r.content, "isError": r.is_error }))
        }
        "resources/list" => ok(json!({ "resources": [] })),
        "prompts/list" => ok(json!({ "prompts": [] })),
        _ => err(-32601, format!("Method not found: {method}")),
    }
}

fn instructions(info: &J) -> String {
    let place = info.get("title").and_then(|t| t.as_str()).unwrap_or("?");
    format!(
        "This is one open RuStudio window (pid {}, \"{place}\"): a native Roblox Studio. Tools act on it live: \
         the Explorer tree, properties, scripts and Luau in the edit DataModel, Play / Run and their DataModels, Output, \
         screenshots, and a live Team Create session when one is open. Paths are GetFullName paths (\"Workspace.Map.Door\"). \
         Writes are undoable (undo). On a live Team Create place, writes reach every collaborator and script edits become \
         drafts to commit (team_create_commit_drafts), then team_create_wait. Tool groups the person turned off fail with a \
         message saying so. session_info lists the other rustudio windows.",
        info.get("pid").and_then(|p| p.as_u64()).unwrap_or(0)
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rpc(msg: J) -> Reply {
        let mut calls = Vec::new();
        let mut call = |tool: &str, args: J| {
            calls.push((tool.to_string(), args.clone()));
            ToolResult::text(format!("{tool} {args}"))
        };
        handle_message(&msg, &json!({ "pid": 1, "title": "Place1" }), &mut call)
    }

    #[test]
    fn initialize_lists_and_calls() {
        let r = rpc(json!({ "jsonrpc": "2.0", "id": 1, "method": "initialize", "params": { "protocolVersion": "2025-03-26", "clientInfo": { "name": "claude-code", "version": "2.0" } } }));
        let res = r.response.unwrap();
        assert_eq!(res["result"]["protocolVersion"], "2025-03-26");
        assert_eq!(res["result"]["serverInfo"]["name"], "rustudio");
        assert_eq!(r.initialized.as_deref(), Some("claude-code 2.0"));
        // An unknown version gets the newest.
        let r = rpc(json!({ "jsonrpc": "2.0", "id": 2, "method": "initialize", "params": { "protocolVersion": "1999-01-01" } }));
        assert_eq!(r.response.unwrap()["result"]["protocolVersion"], PROTOCOL_VERSIONS[0]);
        let r = rpc(json!({ "jsonrpc": "2.0", "method": "notifications/initialized" }));
        assert!(r.response.is_none());
        let list = rpc(json!({ "jsonrpc": "2.0", "id": "a", "method": "tools/list" })).response.unwrap();
        assert!(list["result"]["tools"].as_array().unwrap().iter().any(|t| t["name"] == "run_luau"));
        let call = rpc(json!({ "jsonrpc": "2.0", "id": 3, "method": "tools/call", "params": { "name": "get_tree", "arguments": { "path": "Workspace" } } })).response.unwrap();
        assert_eq!(call["id"], 3);
        assert_eq!(call["result"]["isError"], false);
        assert!(call["result"]["content"][0]["text"].as_str().unwrap().starts_with("get_tree"));
        let bad = rpc(json!({ "jsonrpc": "2.0", "id": 4, "method": "tools/call", "params": { "name": "nope" } })).response.unwrap();
        assert_eq!(bad["error"]["code"], -32602);
        let unknown = rpc(json!({ "jsonrpc": "2.0", "id": 5, "method": "sampling/whatever" })).response.unwrap();
        assert_eq!(unknown["error"]["code"], -32601);
    }

    #[test]
    fn origins_and_tokens() {
        assert!(origin_allowed(None));
        assert!(origin_allowed(Some("http://localhost:3000")));
        assert!(origin_allowed(Some("http://127.0.0.1")));
        assert!(origin_allowed(Some("http://[::1]:8080")));
        assert!(!origin_allowed(Some("https://evil.example")));
        assert!(!origin_allowed(Some("http://localhost.evil.example")));
        assert!(token_ok(Some("Bearer abc123"), "abc123"));
        assert!(!token_ok(Some("Bearer abc124"), "abc123"));
        assert!(!token_ok(Some("Bearer abc"), "abc123"));
        assert!(!token_ok(None, "abc123"));
    }

    #[test]
    fn http_round_trip_with_token() {
        let shared = Arc::new(Mutex::new(Shared { token: "t0k".into(), require_token: true, ..Default::default() }));
        let mut server = Server::start(47100, 50, shared.clone()).unwrap();
        let port = server.port;
        // Answer calls from a stand-in UI thread.
        let jobs = std::mem::replace(&mut server.jobs, channel().1);
        std::thread::spawn(move || {
            while let Ok(job) = jobs.recv() {
                let _ = job.reply.send(ToolResult::text(format!("ran {}", job.tool)));
            }
        });
        let post = |auth: Option<&str>, body: &str| -> (String, String) {
            let mut s = TcpStream::connect(("127.0.0.1", port)).unwrap();
            let auth = auth.map(|a| format!("Authorization: Bearer {a}\r\n")).unwrap_or_default();
            write!(s, "POST /mcp HTTP/1.1\r\nHost: x\r\n{auth}Content-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len()).unwrap();
            let mut text = String::new();
            s.read_to_string(&mut text).unwrap();
            let (head, body) = text.split_once("\r\n\r\n").unwrap();
            (head.to_string(), body.to_string())
        };
        let (head, _) = post(None, "{}");
        assert!(head.starts_with("HTTP/1.1 401"), "{head}");
        let (head, body) = post(Some("t0k"), r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"clientInfo":{"name":"test"}}}"#);
        assert!(head.starts_with("HTTP/1.1 200") && head.contains("Mcp-Session-Id"), "{head}");
        assert!(body.contains("rustudio"));
        assert_eq!(shared.lock().unwrap().connected().len(), 1);
        let (_, body) = post(Some("t0k"), r#"{"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":"session_info"}}"#);
        assert!(body.contains("ran session_info"), "{body}");
        let (head, _) = post(Some("t0k"), r#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#);
        assert!(head.starts_with("HTTP/1.1 202"), "{head}");
    }
}
