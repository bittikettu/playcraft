//! Remote control of the running player, used by the MCP bridge (`mp3blaster-rs --mcp`).
//!
//! The player listens on a free localhost port and publishes it with a random token in
//! `control.json` in the per-user state folder; only processes that can read that file can
//! drive the player. A connection carries one JSON line per call, a request
//! `{token, client, tool, args}` answered by `{"Ok": result}` or `{"Err": message}`. Calls
//! are handed to the UI thread, which owns all player state.

use std::io::{BufRead, BufReader, Read, Write};
use std::net::{Ipv4Addr, SocketAddr, TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::time::Duration;

use crossbeam_channel::{Receiver, RecvTimeoutError, Sender, bounded, unbounded};
use serde::{Deserialize, Serialize};
use serde_json::Value;

pub const CONTROL_FILE: &str = "control.json";
/// How long a call waits for the UI thread.
const REPLY_TIMEOUT: Duration = Duration::from_secs(5);
/// Requests are small; anything longer is not a client of ours.
const MAX_LINE: u64 = 1 << 20;

/// Where the running player listens (the contents of `control.json`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ControlInfo {
    pub port: u16,
    pub token: String,
    pub pid: u32,
}

/// A tool call waiting for the UI thread.
pub struct RemoteCall {
    /// Name of the AI client (from MCP `clientInfo`), shown in banners.
    pub client: String,
    pub tool: String,
    pub args: Value,
    pub reply: Sender<Result<Value, String>>,
}

#[derive(Serialize, Deserialize)]
struct Request {
    token: String,
    #[serde(default)]
    client: String,
    tool: String,
    #[serde(default)]
    args: Value,
}

/// The listening side, owned by the player. Removes `control.json` when dropped.
pub struct ControlServer {
    file: PathBuf,
    info: ControlInfo,
}

impl ControlServer {
    /// Listens on a free localhost port and publishes it in `<state_dir>/control.json`.
    pub fn start(state_dir: &Path) -> std::io::Result<(ControlServer, Receiver<RemoteCall>)> {
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0))?;
        let info = ControlInfo {
            port: listener.local_addr()?.port(),
            token: format!("{:032x}", rand::random::<u128>()),
            pid: std::process::id(),
        };
        let file = state_dir.join(CONTROL_FILE);
        crate::session::save_json(&file, &serde_json::to_string(&info)?)?;
        let (calls, received) = unbounded();
        let token = info.token.clone();
        std::thread::Builder::new()
            .name("control".into())
            .spawn(move || {
                for stream in listener.incoming().flatten() {
                    let (calls, token) = (calls.clone(), token.clone());
                    let _ = std::thread::Builder::new()
                        .name("control-client".into())
                        .spawn(move || serve(stream, &token, &calls));
                }
            })?;
        log::info!("remote control listening on 127.0.0.1:{}", info.port);
        Ok((ControlServer { file, info }, received))
    }

    pub fn port(&self) -> u16 {
        self.info.port
    }
}

impl Drop for ControlServer {
    fn drop(&mut self) {
        // A player started later may have taken the file over; leave that one alone.
        if read_info(&self.file).is_some_and(|i| i.token == self.info.token) {
            let _ = std::fs::remove_file(&self.file);
        }
    }
}

/// Reads one line of at most `MAX_LINE` bytes. `None` at the end of the stream.
fn read_line(reader: &mut impl BufRead) -> Option<String> {
    let mut line = String::new();
    match reader.take(MAX_LINE).read_line(&mut line) {
        Ok(0) | Err(_) => None,
        Ok(_) => Some(line),
    }
}

fn serve(stream: TcpStream, token: &str, calls: &Sender<RemoteCall>) {
    let Ok(mut writer) = stream.try_clone() else {
        return;
    };
    let mut reader = BufReader::new(stream);
    while let Some(line) = read_line(&mut reader) {
        let request = serde_json::from_str::<Request>(&line);
        let authorised = matches!(&request, Ok(r) if r.token == token);
        let reply = match request {
            Err(e) => Err(format!("bad request: {e}")),
            Ok(_) if !authorised => Err("wrong token".to_string()),
            Ok(r) => forward(r, calls),
        };
        let text = serde_json::to_string(&reply).unwrap_or_default();
        if writeln!(writer, "{text}").is_err() || !authorised {
            if !authorised {
                log::warn!("remote control: rejected a request without the right token");
            }
            return;
        }
    }
}

fn forward(r: Request, calls: &Sender<RemoteCall>) -> Result<Value, String> {
    let (reply, answer) = bounded(1);
    let call = RemoteCall {
        client: r.client,
        tool: r.tool,
        args: r.args,
        reply,
    };
    if calls.send(call).is_err() {
        return Err("the player is shutting down".into());
    }
    match answer.recv_timeout(REPLY_TIMEOUT) {
        Ok(result) => result,
        Err(RecvTimeoutError::Timeout) => Err("the player did not answer in time".into()),
        Err(RecvTimeoutError::Disconnected) => Err("the player is shutting down".into()),
    }
}

/// Reads `control.json`.
pub fn read_info(file: &Path) -> Option<ControlInfo> {
    serde_json::from_str(&std::fs::read_to_string(file).ok()?).ok()
}

/// Why a call to the player did not return a result.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CallError {
    /// No player is running (or it exited without cleaning up).
    NotRunning,
    /// The player answered with an error, or the connection broke.
    Failed(String),
}

/// Sends one tool call to the player whose `control.json` is in `state_dir`.
pub fn call(state_dir: &Path, client: &str, tool: &str, args: Value) -> Result<Value, CallError> {
    let info = read_info(&state_dir.join(CONTROL_FILE)).ok_or(CallError::NotRunning)?;
    let addr = SocketAddr::from((Ipv4Addr::LOCALHOST, info.port));
    // A live player accepts at once; Windows only gives up on a closed port at the timeout.
    let stream = TcpStream::connect_timeout(&addr, Duration::from_millis(500))
        .map_err(|_| CallError::NotRunning)?;
    let broken = |e: std::io::Error| CallError::Failed(format!("connection to the player: {e}"));
    stream
        .set_read_timeout(Some(REPLY_TIMEOUT * 2))
        .map_err(broken)?;
    let request = Request {
        token: info.token,
        client: client.to_string(),
        tool: tool.to_string(),
        args,
    };
    let mut writer = stream.try_clone().map_err(broken)?;
    let text = serde_json::to_string(&request).map_err(|e| CallError::Failed(e.to_string()))?;
    writeln!(writer, "{text}").map_err(broken)?;
    let line = read_line(&mut BufReader::new(stream))
        .ok_or_else(|| CallError::Failed("the player closed the connection".into()))?;
    let reply: Result<Value, String> = serde_json::from_str(&line)
        .map_err(|e| CallError::Failed(format!("unreadable answer: {e}")))?;
    reply.map_err(CallError::Failed)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// Answers calls like the UI thread would, until the server goes away.
    fn answer_calls(calls: Receiver<RemoteCall>) {
        std::thread::spawn(move || {
            for call in calls {
                let result = match call.tool.as_str() {
                    "echo" => Ok(json!({ "client": call.client, "args": call.args })),
                    other => Err(format!("unknown tool {other}")),
                };
                let _ = call.reply.send(result);
            }
        });
    }

    #[test]
    fn calls_reach_the_player_and_come_back() {
        let dir = tempfile::tempdir().unwrap();
        let (server, calls) = ControlServer::start(dir.path()).unwrap();
        answer_calls(calls);
        let info = read_info(&dir.path().join(CONTROL_FILE)).unwrap();
        assert_eq!(info.port, server.port());
        assert_eq!(info.token.len(), 32);

        let got = call(dir.path(), "Claude", "echo", json!({ "x": 1 })).unwrap();
        assert_eq!(got, json!({ "client": "Claude", "args": { "x": 1 } }));
        assert_eq!(
            call(dir.path(), "Claude", "nope", json!({})),
            Err(CallError::Failed("unknown tool nope".into()))
        );

        // Gone with the player.
        drop(server);
        assert!(!dir.path().join(CONTROL_FILE).exists());
        assert_eq!(
            call(dir.path(), "Claude", "echo", json!({})),
            Err(CallError::NotRunning)
        );
    }

    #[test]
    fn a_wrong_token_is_turned_away() {
        let dir = tempfile::tempdir().unwrap();
        let (server, calls) = ControlServer::start(dir.path()).unwrap();
        answer_calls(calls);
        let file = dir.path().join(CONTROL_FILE);
        let mut info = read_info(&file).unwrap();
        info.token = "0".repeat(32);
        std::fs::write(&file, serde_json::to_string(&info).unwrap()).unwrap();
        assert_eq!(
            call(dir.path(), "Mallory", "echo", json!({})),
            Err(CallError::Failed("wrong token".into()))
        );
        drop(server);
    }

    #[test]
    fn a_stale_control_file_means_not_running() {
        let dir = tempfile::tempdir().unwrap();
        // A port nothing listens on any more.
        let port = TcpListener::bind((Ipv4Addr::LOCALHOST, 0))
            .unwrap()
            .local_addr()
            .unwrap()
            .port();
        let info = ControlInfo {
            port,
            token: "x".into(),
            pid: 1,
        };
        std::fs::write(
            dir.path().join(CONTROL_FILE),
            serde_json::to_string(&info).unwrap(),
        )
        .unwrap();
        assert_eq!(
            call(dir.path(), "c", "echo", json!({})),
            Err(CallError::NotRunning)
        );
        assert_eq!(
            call(&dir.path().join("missing"), "c", "echo", json!({})),
            Err(CallError::NotRunning)
        );
    }
}
