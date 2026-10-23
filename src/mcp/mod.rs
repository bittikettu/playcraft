//! The MCP server (`mp3blaster-rs --mcp`): Model Context Protocol over stdio, so AI assistants
//! such as Claude Code and Claude Desktop can control the running player.
//!
//! Messages are newline-delimited JSON-RPC 2.0 on stdin/stdout (stdout carries nothing
//! else). Only tools are offered; each call is forwarded to the player through
//! `remote::call` and runs there on the UI thread.

pub mod tools;

use std::io::{BufRead, Write};
use std::path::PathBuf;

use serde_json::{Value, json};

use crate::remote::{self, CallError};

/// Protocol versions spoken, newest first.
pub const PROTOCOL_VERSIONS: &[&str] = &["2025-11-25", "2025-06-18", "2025-03-26", "2024-11-05"];

pub const NOT_RUNNING: &str = "mp3blaster-rs is not running. Ask the user to start it in a \
terminal, then try again.";

const INSTRUCTIONS: &str = "Controls mp3blaster-rs, a music player running in the user's \
terminal. To play a kind of music (a genre, mood or era): browse_library shows which genres and \
artists exist; search_tracks finds tracks (turn a mood into genres or artists you know, and use \
shuffle for variety); queue_tracks queues them by path, with play_now to start at once. Queued \
tracks play before the normal playlist order, which resumes afterwards. get_status tells what is \
playing and what is queued. set_visualizer switches the visuals and fullscreen (hyperspace is \
the showpiece). If a tool says the player is not running, ask the user to start mp3blaster-rs.";

/// One MCP connection.
#[derive(Debug, Default)]
pub struct Session {
    /// The client's name, shown in the player's banners.
    pub client: String,
}

/// A readable name for a client: its title, a known name, or its name in title case.
fn client_label(info: Option<&Value>) -> String {
    let field = |key| info.and_then(|i| i.get(key)).and_then(Value::as_str);
    if let Some(title) = field("title").filter(|t| !t.trim().is_empty()) {
        return title.to_string();
    }
    match field("name").map(str::trim) {
        Some("claude-code") => "Claude Code".into(),
        Some("claude-ai") => "Claude".into(),
        Some(name) if !name.is_empty() => name
            .split(['-', '_', ' '])
            .filter(|w| !w.is_empty())
            .map(|w| {
                let mut c = w.chars();
                c.next()
                    .map(|f| f.to_uppercase().chain(c).collect::<String>())
                    .unwrap_or_default()
            })
            .collect::<Vec<_>>()
            .join(" "),
        _ => "AI assistant".into(),
    }
}

fn reply(id: Value, result: Value) -> String {
    json!({ "jsonrpc": "2.0", "id": id, "result": result }).to_string()
}

fn error(id: Value, code: i64, message: &str) -> String {
    json!({ "jsonrpc": "2.0", "id": id, "error": { "code": code, "message": message } }).to_string()
}

fn tool_error(message: &str) -> Value {
    json!({ "content": [{ "type": "text", "text": message }], "isError": true })
}

/// Handles one incoming line and returns the line to answer with, if any. `call` runs a tool
/// in the player: `(client, tool, arguments)`.
pub fn handle(
    line: &str,
    session: &mut Session,
    call: &mut impl FnMut(&str, &str, Value) -> Result<Value, CallError>,
) -> Option<String> {
    let message: Value = match serde_json::from_str(line) {
        Ok(v) => v,
        Err(e) => return Some(error(Value::Null, -32700, &format!("parse error: {e}"))),
    };
    let Some(message) = message.as_object() else {
        return Some(error(
            Value::Null,
            -32600,
            "expected a single JSON-RPC message",
        ));
    };
    // Responses (we send no requests) and notifications such as `initialized` need no answer.
    let method = message.get("method").and_then(Value::as_str)?;
    let id = message.get("id").cloned()?;
    let params = message.get("params").cloned().unwrap_or(Value::Null);
    Some(match method {
        "initialize" => {
            let requested = params.get("protocolVersion").and_then(Value::as_str);
            let version = requested
                .filter(|v| PROTOCOL_VERSIONS.contains(v))
                .unwrap_or(PROTOCOL_VERSIONS[0]);
            session.client = client_label(params.get("clientInfo"));
            log::info!("MCP client {} (protocol {version})", session.client);
            reply(
                id,
                json!({
                    "protocolVersion": version,
                    "capabilities": { "tools": { "listChanged": false } },
                    "serverInfo": {
                        "name": "mp3blaster-rs",
                        "title": "MP3Blaster",
                        "version": env!("CARGO_PKG_VERSION"),
                    },
                    "instructions": INSTRUCTIONS,
                }),
            )
        }
        "ping" => reply(id, json!({})),
        "tools/list" => reply(id, json!({ "tools": tools::list() })),
        "tools/call" => {
            let Some(name) = params.get("name").and_then(Value::as_str) else {
                return Some(error(id, -32602, "tools/call needs a tool name"));
            };
            if tools::find(name).is_none() {
                return Some(error(id, -32602, &format!("unknown tool: {name}")));
            }
            let args = params
                .get("arguments")
                .cloned()
                .unwrap_or_else(|| json!({}));
            let result = match call(&session.client, name, args) {
                Ok(result) => {
                    let mut r = json!({
                        "content": [{ "type": "text", "text": result.to_string() }],
                        "isError": false,
                    });
                    if result.is_object() {
                        r["structuredContent"] = result;
                    }
                    r
                }
                Err(CallError::NotRunning) => tool_error(NOT_RUNNING),
                Err(CallError::Failed(message)) => tool_error(&message),
            };
            reply(id, result)
        }
        other => error(id, -32601, &format!("method not found: {other}")),
    })
}

/// Serves MCP on stdin/stdout until stdin closes. Tool calls go to the player whose
/// `control.json` is in `state_dir`.
pub fn run_stdio(state_dir: Option<PathBuf>) -> std::io::Result<()> {
    let mut stdout = std::io::stdout().lock();
    let mut session = Session::default();
    let mut call = |client: &str, tool: &str, args: Value| match &state_dir {
        Some(dir) => remote::call(dir, client, tool, args),
        None => Err(CallError::NotRunning),
    };
    for line in std::io::stdin().lock().lines() {
        let line = line?;
        if line.trim().is_empty() {
            continue;
        }
        if let Some(answer) = handle(&line, &mut session, &mut call) {
            writeln!(stdout, "{answer}")?;
            stdout.flush()?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(
        lines: &[Value],
        mut call: impl FnMut(&str, &str, Value) -> Result<Value, CallError>,
    ) -> Vec<Value> {
        let mut session = Session::default();
        lines
            .iter()
            .filter_map(|l| handle(&l.to_string(), &mut session, &mut call))
            .map(|out| serde_json::from_str(&out).unwrap())
            .collect()
    }

    fn not_running(_: &str, _: &str, _: Value) -> Result<Value, CallError> {
        Err(CallError::NotRunning)
    }

    fn init(version: &str) -> Value {
        json!({
            "jsonrpc": "2.0", "id": 1, "method": "initialize",
            "params": {
                "protocolVersion": version,
                "capabilities": {},
                "clientInfo": { "name": "claude-code", "version": "2.0" },
            },
        })
    }

    #[test]
    fn negotiates_the_protocol_version() {
        let out = run(&[init("2025-06-18")], not_running);
        assert_eq!(out[0]["result"]["protocolVersion"], "2025-06-18");
        assert_eq!(out[0]["result"]["serverInfo"]["name"], "mp3blaster-rs");
        assert!(out[0]["result"]["capabilities"]["tools"].is_object());
        let out = run(&[init("1999-01-01")], not_running);
        assert_eq!(out[0]["result"]["protocolVersion"], PROTOCOL_VERSIONS[0]);
    }

    #[test]
    fn lists_tools_with_object_schemas() {
        let out = run(
            &[
                init("2025-11-25"),
                json!({ "jsonrpc": "2.0", "method": "notifications/initialized" }),
                json!({ "jsonrpc": "2.0", "id": 2, "method": "tools/list" }),
            ],
            not_running,
        );
        assert_eq!(out.len(), 2, "notifications get no answer");
        let tools = out[1]["result"]["tools"].as_array().unwrap();
        assert_eq!(tools.len(), tools::TOOLS.len());
        for t in tools {
            assert_eq!(t["inputSchema"]["type"], "object", "{}", t["name"]);
            assert!(t["description"].as_str().unwrap().len() > 20);
        }
        let names: Vec<&str> = tools.iter().map(|t| t["name"].as_str().unwrap()).collect();
        for wanted in [
            "search_tracks",
            "queue_tracks",
            "set_visualizer",
            "get_status",
        ] {
            assert!(names.contains(&wanted), "{wanted} missing");
        }
    }

    #[test]
    fn tool_calls_are_forwarded_with_the_client_name() {
        let mut seen = Vec::new();
        let out = run(
            &[
                init("2025-06-18"),
                json!({
                    "jsonrpc": "2.0", "id": 3, "method": "tools/call",
                    "params": { "name": "set_visualizer", "arguments": { "fullscreen": true } },
                }),
            ],
            |client, tool, args| {
                seen.push((client.to_string(), tool.to_string(), args));
                Ok(json!({ "fullscreen": true }))
            },
        );
        assert_eq!(
            seen,
            [(
                "Claude Code".to_string(),
                "set_visualizer".to_string(),
                json!({ "fullscreen": true })
            )]
        );
        let result = &out[1]["result"];
        assert_eq!(result["isError"], false);
        assert_eq!(result["structuredContent"]["fullscreen"], true);
        assert_eq!(result["content"][0]["type"], "text");
    }

    #[test]
    fn errors_are_reported_the_mcp_way() {
        let call = |tool: &str| {
            json!({
                "jsonrpc": "2.0", "id": 9, "method": "tools/call",
                "params": { "name": tool, "arguments": {} },
            })
        };
        let out = run(
            &[
                call("get_status"),
                call("make_coffee"),
                json!({ "jsonrpc": "2.0", "id": 10, "method": "resources/list" }),
                json!([1, 2]),
            ],
            not_running,
        );
        // The player being away is a tool result the model can read, not a protocol error.
        assert_eq!(out[0]["result"]["isError"], true);
        assert_eq!(out[0]["result"]["content"][0]["text"], NOT_RUNNING);
        assert_eq!(out[1]["error"]["code"], -32602);
        assert_eq!(out[2]["error"]["code"], -32601);
        assert_eq!(out[3]["error"]["code"], -32600);
        let mut session = Session::default();
        let bad = handle("{not json", &mut session, &mut not_running).unwrap();
        assert!(bad.contains("-32700"));
    }

    #[test]
    fn client_names_read_well_in_banners() {
        assert_eq!(
            client_label(Some(&json!({ "name": "claude-ai" }))),
            "Claude"
        );
        assert_eq!(
            client_label(Some(&json!({ "name": "my_cool-client" }))),
            "My Cool Client"
        );
        assert_eq!(
            client_label(Some(&json!({ "name": "x", "title": "Inspector" }))),
            "Inspector"
        );
        assert_eq!(client_label(None), "AI assistant");
    }
}
