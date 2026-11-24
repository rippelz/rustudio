//! `rustudio mcp [--pid PID]`: an MCP server on stdin / stdout for clients
//! that start their servers themselves (`claude mcp add rustudio -- rustudio
//! mcp`). It opens no window: it finds the rustudio windows with MCP on
//! (sessions.rs) and forwards each tool call to one of them over its HTTP
//! endpoint, with the token from the MCP settings file.
//!
//! The tool list is this binary's own catalogue (so it is there even before
//! a window is open) plus two of the bridge's: `rustudio_windows` lists the
//! open windows and `rustudio_use_window` picks one by pid. Without a pick
//! it uses `--pid`, else the only window, else the one updated last.

use std::io::{BufRead, Read, Write};
use std::net::TcpStream;
use std::time::Duration;

use serde_json::{json, Value as J};

use super::server::PROTOCOL_VERSIONS;
use super::{sessions, settings};

struct Bridge {
    pid: Option<u64>,
    token: String,
}

fn result(id: &J, r: J) -> J {
    json!({ "jsonrpc": "2.0", "id": id, "result": r })
}

fn text(t: impl Into<String>, error: bool) -> J {
    json!({ "content": [{ "type": "text", "text": t.into() }], "isError": error })
}

/// POSTs one JSON-RPC message to a window; its JSON reply.
fn post(port: u16, token: &str, body: &J) -> Result<J, String> {
    let mut s = TcpStream::connect(("127.0.0.1", port)).map_err(|e| format!("can't reach 127.0.0.1:{port}: {e}"))?;
    let _ = s.set_read_timeout(Some(Duration::from_secs(25 * 60)));
    let body = body.to_string();
    write!(
        s,
        "POST /mcp HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nAuthorization: Bearer {token}\r\nContent-Type: application/json\r\nAccept: application/json, text/event-stream\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    )
    .map_err(|e| e.to_string())?;
    let mut raw = Vec::new();
    s.read_to_end(&mut raw).map_err(|e| e.to_string())?;
    let text = String::from_utf8_lossy(&raw);
    let (head, body) = text.split_once("\r\n\r\n").ok_or("an empty reply")?;
    let status = head.split_whitespace().nth(1).unwrap_or("");
    if status == "401" {
        return Err("the window refused the token: the bridge reads it from the MCP settings file; restart the bridge after changing it".into());
    }
    if !status.starts_with('2') {
        return Err(format!("HTTP {status}: {body}"));
    }
    serde_json::from_str(body).map_err(|e| format!("a reply that isn't JSON ({e})"))
}

impl Bridge {
    fn window(&self) -> Result<J, String> {
        let list = sessions::list();
        if let Some(pid) = self.pid {
            return list
                .into_iter()
                .find(|v| v["pid"].as_u64() == Some(pid))
                .ok_or_else(|| format!("rustudio pid {pid} has no MCP on (closed, or MCP turned off); rustudio_windows lists the others"));
        }
        list.into_iter().next().ok_or_else(|| "No rustudio window has MCP on. Open rustudio and turn MCP on (the MCP button in the top bar, beside Collaborate).".into())
    }

    fn call(&mut self, name: &str, args: &J) -> J {
        match name {
            "rustudio_windows" => {
                let list = sessions::list();
                let chosen = self.window().ok().and_then(|w| w["pid"].as_u64());
                text(
                    serde_json::to_string_pretty(&json!({ "windows": list, "using_pid": chosen, "how": "rustudio_use_window picks one by pid" })).unwrap_or_default(),
                    false,
                )
            }
            "rustudio_use_window" => {
                let pid = args.get("pid").and_then(|p| p.as_u64());
                match pid {
                    None => {
                        self.pid = None;
                        text("Using the most recently updated window", false)
                    }
                    Some(p) if sessions::list().iter().any(|v| v["pid"].as_u64() == Some(p)) => {
                        self.pid = Some(p);
                        text(format!("Using rustudio pid {p}"), false)
                    }
                    Some(p) => text(format!("No rustudio window with pid {p} has MCP on"), true),
                }
            }
            _ => {
                let w = match self.window() {
                    Ok(w) => w,
                    Err(e) => return text(e, true),
                };
                let port = w["port"].as_u64().unwrap_or(0) as u16;
                let msg = json!({ "jsonrpc": "2.0", "id": 1, "method": "tools/call", "params": { "name": name, "arguments": args } });
                match post(port, &self.token, &msg) {
                    Ok(r) => match (r.get("result"), r.get("error")) {
                        (Some(res), _) => res.clone(),
                        (_, Some(e)) => text(format!("rustudio: {}", e["message"].as_str().unwrap_or("error")), true),
                        _ => text("rustudio sent an empty answer", true),
                    },
                    Err(e) => text(format!("rustudio pid {}: {e}", w["pid"]), true),
                }
            }
        }
    }

    fn handle(&mut self, msg: &J) -> Option<J> {
        let id = msg.get("id")?.clone();
        let method = msg.get("method").and_then(|m| m.as_str()).unwrap_or("");
        let params = msg.get("params").cloned().unwrap_or(J::Null);
        Some(match method {
            "initialize" => {
                let asked = params.get("protocolVersion").and_then(|v| v.as_str()).unwrap_or(PROTOCOL_VERSIONS[0]);
                let version = PROTOCOL_VERSIONS.iter().find(|v| **v == asked).copied().unwrap_or(PROTOCOL_VERSIONS[0]);
                result(&id, json!({
                    "protocolVersion": version,
                    "capabilities": { "tools": { "listChanged": false } },
                    "serverInfo": { "name": "rustudio", "title": "RuStudio", "version": env!("CARGO_PKG_VERSION") },
                    "instructions": "Drives open RuStudio windows (a native Roblox Studio) live: the place tree, properties, scripts, Luau in edit / Play, Output, screenshots, Play / Stop, place files and Team Create. Calls go to one window: rustudio_windows lists them, rustudio_use_window picks one (else the only / most recent). Paths are GetFullName paths. On a live Team Create place writes reach every collaborator; script edits become drafts (team_create_commit_drafts, then team_create_wait).",
                }))
            }
            "ping" => result(&id, json!({})),
            "tools/list" => {
                let mut tools = super::tools::list();
                tools.push(json!({ "name": "rustudio_windows", "description": "Lists the open rustudio windows with MCP on (pid, title, place, mode, url) and which one calls go to.", "inputSchema": { "type": "object", "properties": {} }, "annotations": { "readOnlyHint": true } }));
                tools.push(json!({ "name": "rustudio_use_window", "description": "Sends the following calls to the rustudio window with this pid (omit pid for the most recently updated one).", "inputSchema": { "type": "object", "properties": { "pid": { "type": "integer" } } }, "annotations": { "readOnlyHint": true } }));
                result(&id, json!({ "tools": tools }))
            }
            "tools/call" => {
                let name = params.get("name").and_then(|n| n.as_str()).unwrap_or("");
                let args = params.get("arguments").cloned().unwrap_or_else(|| json!({}));
                let r = self.call(name, &args);
                result(&id, r)
            }
            "resources/list" => result(&id, json!({ "resources": [] })),
            "prompts/list" => result(&id, json!({ "prompts": [] })),
            _ => json!({ "jsonrpc": "2.0", "id": id, "error": { "code": -32601, "message": format!("Method not found: {method}") } }),
        })
    }
}

/// Runs the bridge until stdin closes. `argv`: the arguments after `mcp`.
pub fn run(argv: &[String]) -> i32 {
    let pid = argv.iter().position(|a| a == "--pid").and_then(|i| argv.get(i + 1)).and_then(|p| p.parse().ok());
    let token = std::env::var("RUSTUDIO_MCP_TOKEN").ok().filter(|t| !t.is_empty()).unwrap_or_else(|| settings::McpSettings::load(settings::path().as_deref()).token);
    let mut b = Bridge { pid, token };
    let stdin = std::io::stdin();
    let mut out = std::io::stdout();
    for line in stdin.lock().lines() {
        let Ok(line) = line else { break };
        if line.trim().is_empty() {
            continue;
        }
        let reply = match serde_json::from_str::<J>(&line) {
            Ok(msg) => b.handle(&msg),
            Err(e) => Some(json!({ "jsonrpc": "2.0", "id": null, "error": { "code": -32700, "message": format!("Parse error: {e}") } })),
        };
        if let Some(r) = reply {
            if writeln!(out, "{r}").and_then(|_| out.flush()).is_err() {
                break;
            }
        }
    }
    0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_bridge_lists_tools_without_a_window() {
        let dir = std::env::temp_dir().join(format!("rustudio-mcp-bridge-{}", std::process::id()));
        std::env::set_var("RUSTUDIO_MCP_SESSIONS", &dir);
        let mut b = Bridge { pid: None, token: "x".into() };
        let init = b.handle(&json!({ "jsonrpc": "2.0", "id": 1, "method": "initialize", "params": { "protocolVersion": "2025-06-18" } })).unwrap();
        assert_eq!(init["result"]["protocolVersion"], "2025-06-18");
        let list = b.handle(&json!({ "jsonrpc": "2.0", "id": 2, "method": "tools/list" })).unwrap();
        let names: Vec<&str> = list["result"]["tools"].as_array().unwrap().iter().filter_map(|t| t["name"].as_str()).collect();
        assert!(names.contains(&"run_luau") && names.contains(&"rustudio_windows"));
        let call = b.handle(&json!({ "jsonrpc": "2.0", "id": 3, "method": "tools/call", "params": { "name": "get_tree" } })).unwrap();
        assert_eq!(call["result"]["isError"], true);
        assert!(call["result"]["content"][0]["text"].as_str().unwrap().contains("No rustudio window has MCP on"));
        assert!(b.handle(&json!({ "jsonrpc": "2.0", "method": "notifications/initialized" })).is_none());
    }
}
