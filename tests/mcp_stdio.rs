//! `mp3blaster-rs --mcp` end to end: an MCP client talks to the binary over stdio, and tool
//! calls reach a stand-in player through the control socket in the state folder.

use std::io::{BufRead, BufReader, Write};
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};

use mp3blaster_rs::remote::ControlServer;
use serde_json::{Value, json};

struct Bridge {
    child: Child,
    stdin: ChildStdin,
    stdout: BufReader<ChildStdout>,
}

impl Bridge {
    /// Starts the bridge with its state folder under `state_home`.
    fn start(state_home: &std::path::Path) -> Bridge {
        let mut child = Command::new(env!("CARGO_BIN_EXE_mp3blaster-rs"))
            .arg("--mcp")
            .env("XDG_STATE_HOME", state_home)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .expect("start mp3blaster-rs --mcp");
        let stdin = child.stdin.take().unwrap();
        let stdout = BufReader::new(child.stdout.take().unwrap());
        Bridge {
            child,
            stdin,
            stdout,
        }
    }

    fn send(&mut self, message: Value) {
        writeln!(self.stdin, "{message}").unwrap();
        self.stdin.flush().unwrap();
    }

    fn request(&mut self, message: Value) -> Value {
        self.send(message);
        let mut line = String::new();
        self.stdout.read_line(&mut line).unwrap();
        serde_json::from_str(&line).unwrap_or_else(|e| panic!("bad line {line:?}: {e}"))
    }

    fn initialize(&mut self) -> Value {
        let answer = self.request(json!({
            "jsonrpc": "2.0", "id": 1, "method": "initialize",
            "params": {
                "protocolVersion": "2025-06-18",
                "capabilities": {},
                "clientInfo": { "name": "claude-code", "version": "2.1.0" },
            },
        }));
        self.send(json!({ "jsonrpc": "2.0", "method": "notifications/initialized" }));
        answer
    }

    fn call(&mut self, id: u64, tool: &str, args: Value) -> Value {
        self.request(json!({
            "jsonrpc": "2.0", "id": id, "method": "tools/call",
            "params": { "name": tool, "arguments": args },
        }))
    }

    /// Closes stdin, as a client does when it is done; the bridge exits cleanly.
    fn finish(mut self) {
        drop(self.stdin);
        let status = self.child.wait().unwrap();
        assert!(status.success(), "bridge exited with {status}");
        let mut rest = String::new();
        self.stdout.read_line(&mut rest).unwrap();
        assert!(rest.is_empty(), "stray output: {rest:?}");
    }
}

#[test]
fn tool_calls_reach_the_running_player() {
    let home = tempfile::tempdir().unwrap();
    let (_player, calls) = ControlServer::start(&home.path().join("mp3blaster-rs")).unwrap();
    let player = std::thread::spawn(move || {
        let call = calls.recv().unwrap();
        let seen = (call.client.clone(), call.tool.clone(), call.args.clone());
        let _ = call
            .reply
            .send(Ok(json!({ "style": "hyperspace", "fullscreen": true })));
        seen
    });

    let mut bridge = Bridge::start(home.path());
    let init = bridge.initialize();
    assert_eq!(init["result"]["protocolVersion"], "2025-06-18");
    assert_eq!(init["result"]["serverInfo"]["name"], "mp3blaster-rs");
    let list = bridge.request(json!({ "jsonrpc": "2.0", "id": 2, "method": "tools/list" }));
    assert!(list["result"]["tools"].as_array().unwrap().len() >= 10);

    let args = json!({ "style": "hyperspace", "fullscreen": true });
    let result = bridge.call(3, "set_visualizer", args.clone());
    assert_eq!(result["id"], 3);
    assert_eq!(result["result"]["isError"], false);
    assert_eq!(result["result"]["structuredContent"]["fullscreen"], true);
    let seen = player.join().unwrap();
    assert_eq!(
        seen,
        (
            "Claude Code".to_string(),
            "set_visualizer".to_string(),
            args
        )
    );
    bridge.finish();
}

#[test]
fn without_a_player_tools_say_so() {
    let home = tempfile::tempdir().unwrap();
    let mut bridge = Bridge::start(home.path());
    bridge.initialize();
    // Listing tools needs no player.
    let list = bridge.request(json!({ "jsonrpc": "2.0", "id": 2, "method": "tools/list" }));
    assert!(list["result"]["tools"].is_array());
    let result = bridge.call(3, "get_status", json!({}));
    assert_eq!(result["result"]["isError"], true);
    let text = result["result"]["content"][0]["text"].as_str().unwrap();
    assert!(text.contains("not running"), "{text}");
    bridge.finish();
}
