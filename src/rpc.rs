//! JSON-RPC 2.0 over a private Unix socket with length-prefixed frames.

use crate::util::Paths;
use serde_json::{json, Value};
use std::io::{Read, Write};
use std::os::unix::net::UnixStream;
use std::os::unix::process::CommandExt;
use std::time::{Duration, Instant};

const MAX_FRAME: usize = 64 * 1024 * 1024;

pub fn write_frame(s: &mut UnixStream, v: &Value) -> std::io::Result<()> {
    let body = serde_json::to_vec(v)?;
    s.write_all(&(body.len() as u32).to_be_bytes())?;
    s.write_all(&body)?;
    s.flush()
}

pub fn read_frame(s: &mut UnixStream) -> std::io::Result<Option<Value>> {
    let mut len = [0u8; 4];
    match s.read_exact(&mut len) {
        Ok(()) => {}
        Err(e) if e.kind() == std::io::ErrorKind::UnexpectedEof => return Ok(None),
        Err(e) => return Err(e),
    }
    let n = u32::from_be_bytes(len) as usize;
    if n > MAX_FRAME {
        return Err(std::io::Error::other("frame too large"));
    }
    let mut body = vec![0u8; n];
    s.read_exact(&mut body)?;
    serde_json::from_slice(&body)
        .map(Some)
        .map_err(std::io::Error::other)
}

/// A failure with a stable code and the process exit code a CLI should use.
#[derive(Debug, Clone)]
pub struct RpcErr {
    pub code: String,
    pub message: String,
    pub exit: i32,
    pub data: Value,
}

impl RpcErr {
    pub fn new(code: &str, exit: i32, message: impl Into<String>) -> RpcErr {
        RpcErr {
            code: code.into(),
            message: message.into(),
            exit,
            data: Value::Null,
        }
    }
    pub fn invalid(m: impl Into<String>) -> RpcErr {
        RpcErr::new("invalid_invocation", 2, m)
    }
    pub fn not_found(m: impl Into<String>) -> RpcErr {
        RpcErr::new("not_found", 3, m)
    }
    pub fn conflict(m: impl Into<String>) -> RpcErr {
        RpcErr::new("state_conflict", 5, m)
    }
    pub fn with(mut self, data: Value) -> RpcErr {
        self.data = data;
        self
    }
}

pub struct Client {
    stream: UnixStream,
    next: u64,
}

impl Client {
    /// Observe without restarting a daemon the user deliberately stopped.
    pub fn connect_existing(paths: &Paths) -> Result<Client, RpcErr> {
        let stream = UnixStream::connect(paths.socket()).map_err(|e| {
            RpcErr::new(
                "core_unavailable",
                8,
                format!("core is not running or cannot be reached: {e}"),
            )
        })?;
        stream
            .set_read_timeout(Some(Duration::from_secs(2)))
            .map_err(|e| {
                RpcErr::new(
                    "core_unavailable",
                    8,
                    format!("core is not running or cannot be reached: {e}"),
                )
            })?;
        stream
            .set_write_timeout(Some(Duration::from_secs(2)))
            .map_err(|e| {
                RpcErr::new(
                    "core_unavailable",
                    8,
                    format!("core is not running or cannot be reached: {e}"),
                )
            })?;
        Ok(Client { stream, next: 1 })
    }

    /// Connect to the core, starting it if no socket answers.
    pub fn connect(paths: &Paths) -> Result<Client, RpcErr> {
        let sock = paths.socket();
        if let Ok(stream) = UnixStream::connect(&sock) {
            return Ok(Client { stream, next: 1 });
        }
        let exe = std::env::current_exe().map_err(|e| {
            RpcErr::new(
                "backend_unavailable",
                8,
                format!("cannot find own binary: {e}"),
            )
        })?;
        std::fs::create_dir_all(&paths.state).ok();
        let log = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(paths.state.join("core.log"))
            .map_err(|e| {
                RpcErr::new(
                    "backend_unavailable",
                    8,
                    format!("cannot open core log: {e}"),
                )
            })?;
        std::process::Command::new(exe)
            .arg("core")
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(log)
            .process_group(0)
            .spawn()
            .map_err(|e| {
                RpcErr::new(
                    "backend_unavailable",
                    8,
                    format!("cannot start the core: {e}"),
                )
            })?;
        let deadline = Instant::now() + Duration::from_secs(5);
        while Instant::now() < deadline {
            if let Ok(stream) = UnixStream::connect(&sock) {
                return Ok(Client { stream, next: 1 });
            }
            std::thread::sleep(Duration::from_millis(40));
        }
        Err(RpcErr::new(
            "backend_unavailable",
            8,
            format!(
                "the core did not answer on {}; see {}",
                sock.display(),
                paths.state.join("core.log").display()
            ),
        ))
    }

    /// Send a whole frame (such as a signed envelope) and return the raw reply
    /// frame, uninterpreted.
    pub fn send_frame(&mut self, mut frame: Value) -> Result<Value, RpcErr> {
        frame["jsonrpc"] = json!("2.0");
        frame["id"] = json!(self.next);
        self.next += 1;
        let lost = |e: std::io::Error| RpcErr::new("backend_unavailable", 8, format!("lost the core: {e}"));
        write_frame(&mut self.stream, &frame).map_err(lost)?;
        read_frame(&mut self.stream)
            .map_err(lost)?
            .ok_or_else(|| RpcErr::new("backend_unavailable", 8, "the core closed the connection"))
    }

    pub fn call(&mut self, method: &str, params: Value) -> Result<Value, RpcErr> {
        let id = self.next;
        self.next += 1;
        let lost = |e: std::io::Error| {
            RpcErr::new("backend_unavailable", 8, format!("lost the core: {e}"))
        };
        write_frame(
            &mut self.stream,
            &json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params}),
        )
        .map_err(lost)?;
        let reply = read_frame(&mut self.stream).map_err(lost)?.ok_or_else(|| {
            RpcErr::new("backend_unavailable", 8, "the core closed the connection")
        })?;
        if let Some(e) = reply.get("error") {
            return Err(RpcErr {
                code: e["data"]["code"].as_str().unwrap_or("error").to_string(),
                message: e["message"].as_str().unwrap_or("unknown error").to_string(),
                exit: e["data"]["exit"].as_i64().unwrap_or(1) as i32,
                data: e["data"]["detail"].clone(),
            });
        }
        Ok(reply["result"].clone())
    }

    /// Open a dedicated streaming subscription for live emitted events.
    /// Dropping the returned receiver closes the subscription.
    pub fn subscribe(
        paths: &Paths,
        summon: Option<&str>,
        after: i64,
        slim: bool,
    ) -> Result<std::sync::mpsc::Receiver<Value>, RpcErr> {
        let mut client = Client::connect_existing(paths)?;
        // Clear read timeout so idle streams wait without disconnecting.
        client.stream.set_read_timeout(None).ok();
        client.call(
            "subscribe",
            json!({"summon": summon, "after": after, "slim": slim}),
        )?;
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            while let Ok(Some(frame)) = read_frame(&mut client.stream) {
                if frame["method"] == "event" && tx.send(frame["params"].clone()).is_err() {
                    break;
                }
            }
        });
        Ok(rx)
    }
}
