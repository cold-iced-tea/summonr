//! `summonr listen ADDR`: accept signed requests from other devices over HTTP
//! and hand them, unchanged, to this device's core. Trust comes from the
//! signatures, which the core checks; this server only frames and bounds.

use crate::rpc::Client;
use crate::tls::{load_server_config, NetStream};
use crate::util::{Config, Paths};
use serde_json::{json, Value};
use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpListener;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;

pub const PATH: &str = "/summonr/v1/signed";
/// Bodies over this are refused before any signature work.
pub const MAX_BODY: usize = 1_048_576;
const MAX_CONNECTIONS: usize = 32;

pub fn listen(paths: &Paths, addr: &str) -> i32 {
    let cfg = match Config::load(paths) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("summonr listen: {e}");
            return 2;
        }
    };
    let tls = match cfg.tls {
        Some(ref t) => match load_server_config(&t.cert, &t.key) {
            Ok(sc) => Some(sc),
            Err(e) => {
                eprintln!("summonr listen: {e}");
                return 2;
            }
        },
        None => None,
    };
    let identity = match Client::connect(paths).and_then(|mut c| c.call("identity", json!({}))) {
        Ok(v) => v,
        Err(e) => {
            eprintln!("summonr listen: {}", e.message);
            return 8;
        }
    };
    let listener = match TcpListener::bind(addr) {
        Ok(l) => l,
        Err(e) => {
            eprintln!("summonr listen: cannot listen on {addr}: {e}");
            return 8;
        }
    };
    let scheme = if tls.is_some() { "https" } else { "http" };
    println!("Summonr is listening for signed requests");
    println!("  url          {scheme}://{addr}");
    println!("  core key     {}", identity["key"].as_str().unwrap_or(""));
    println!("  fingerprint  {}", crate::keys::grouped(identity["id"].as_str().unwrap_or("")));
    println!("  Only familiars you added can call it, and only the rituals you allowed.");
    if tls.is_some() {
        println!("  TLS is enabled on this listener.");
    } else {
        println!("  Requests and replies are signed but not encrypted; use only on a network you trust.");
    }
    serve_with_tls(listener, paths.clone(), REQUEST_DEADLINE, tls);
    0
}

/// How long a client has to send a whole request.
pub const REQUEST_DEADLINE: Duration = Duration::from_secs(10);

/// Accept connections until the process ends.
#[allow(dead_code)]
pub fn serve(listener: TcpListener, paths: Paths) {
    serve_with(listener, paths, REQUEST_DEADLINE)
}

#[allow(dead_code)]
pub fn serve_with(listener: TcpListener, paths: Paths, deadline: Duration) {
    serve_with_tls(listener, paths, deadline, None)
}

pub fn serve_with_tls(
    listener: TcpListener,
    paths: Paths,
    deadline: Duration,
    tls: Option<Arc<rustls::ServerConfig>>,
) {
    let open = Arc::new(AtomicUsize::new(0));
    for stream in listener.incoming().flatten() {
        if open.fetch_add(1, Ordering::SeqCst) >= MAX_CONNECTIONS {
            open.fetch_sub(1, Ordering::SeqCst);
            continue;
        }
        let (open, paths, tls) = (open.clone(), paths.clone(), tls.clone());
        std::thread::spawn(move || {
            if let Ok(net) = NetStream::wrap(stream, tls.as_ref()) {
                serve_one(net, &paths, deadline);
            }
            open.fetch_sub(1, Ordering::SeqCst);
        });
    }
}

fn respond(stream: &mut NetStream, status: &str, body: &str) {
    if !status.starts_with("200") {
        let from = stream.peer_addr().map(|a| a.to_string()).unwrap_or_else(|_| "unknown".into());
        eprintln!("summonr listen: {from}: {status}");
    }
    let _ = write!(
        stream,
        "HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    );
    let _ = stream.flush();
}

/// Reads that give each call only what is left of the request's deadline,
/// so a client cannot hold a connection by trickling bytes.
struct Deadline {
    stream: NetStream,
    until: std::time::Instant,
}

impl Read for Deadline {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        let expired = || std::io::Error::new(std::io::ErrorKind::TimedOut, "request deadline passed");
        let left = self
            .until
            .checked_duration_since(std::time::Instant::now())
            .filter(|d| !d.is_zero())
            .ok_or_else(expired)?;
        self.stream.set_read_timeout(Some(left))?;
        self.stream.read(buf)
    }
}

/// One request per connection, which must arrive whole within `deadline`.
fn serve_one(stream: NetStream, paths: &Paths, deadline: Duration) {
    let until = std::time::Instant::now() + deadline;
    let _ = stream.set_write_timeout(Some(Duration::from_secs(360)));
    let mut reader = BufReader::new(Deadline { stream, until });
    let mut line = String::new();
    if reader.by_ref().take(8193).read_line(&mut line).unwrap_or(0) == 0 || line.len() > 8192 {
        return;
    }
    let mut parts = line.split_whitespace();
    let (method, target) = (parts.next().unwrap_or("").to_string(), parts.next().unwrap_or("").to_string());
    let (mut length, mut length_seen, mut invalid, mut origin, mut header_bytes) = (0usize, false, false, false, 0usize);
    loop {
        let mut h = String::new();
        let n = reader.by_ref().take(8193).read_line(&mut h).unwrap_or(0);
        header_bytes += n;
        if n == 0 || h.len() > 8192 || header_bytes > 16384 {
            return;
        }
        if h.trim().is_empty() {
            break;
        }
        if let Some((k, v)) = h.split_once(':') {
            match k.trim().to_ascii_lowercase().as_str() {
                "content-length" => {
                    invalid |= length_seen;
                    length_seen = true;
                    match v.trim().parse() {
                        Ok(n) => length = n,
                        Err(_) => invalid = true,
                    }
                }
                "transfer-encoding" => invalid = true,
                "origin" => origin = true,
                _ => {}
            }
        }
    }
    if target.split('?').next() != Some(PATH) {
        return respond(&mut reader.get_mut().stream, "404 Not Found", r#"{"error":"not found"}"#);
    }
    if method != "POST" {
        return respond(&mut reader.get_mut().stream, "405 Method Not Allowed", r#"{"error":"POST only"}"#);
    }
    if origin {
        return respond(&mut reader.get_mut().stream, "403 Forbidden", r#"{"error":"browser requests are refused"}"#);
    }
    if invalid || !length_seen {
        return respond(&mut reader.get_mut().stream, "400 Bad Request", r#"{"error":"bad request"}"#);
    }
    if length > MAX_BODY {
        return respond(&mut reader.get_mut().stream, "413 Payload Too Large", r#"{"error":"request too large"}"#);
    }
    let mut body = vec![0u8; length];
    let mut filled = 0;
    while filled < length {
        match reader.read(&mut body[filled..]) {
            Ok(0) | Err(_) => return,
            Ok(n) => filled += n,
        }
    }
    let mut stream = reader.into_inner().stream;
    let frame: Value = match serde_json::from_slice(&body) {
        Ok(v) => v,
        Err(_) => return respond(&mut stream, "400 Bad Request", r#"{"error":"not JSON"}"#),
    };
    // Only signed envelopes are forwarded; the core verifies them.
    if !frame.get("signed").is_some_and(Value::is_object) {
        return respond(&mut stream, "400 Bad Request", r#"{"error":"only signed requests are accepted"}"#);
    }
    match Client::connect(paths).and_then(|mut c| c.send_frame(json!({"signed": frame["signed"]}))) {
        Ok(reply) => respond(&mut stream, "200 OK", &json!({"signed": reply["signed"]}).to_string()),
        Err(e) => {
            eprintln!("summonr listen: {}", e.message);
            respond(&mut stream, "503 Service Unavailable", r#"{"error":"core unavailable"}"#)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::Core;
    use crate::util::new_id;
    use serde_json::json;

    /// A core with `total-receipt` allowed for `alpha`, and a listener in front.
    fn serving() -> (Paths, Arc<Core>, String) {
        let root = std::env::temp_dir().join(new_id("summonr-listen"));
        let p = Paths { runtime: root.join("run"), state: root.join("state"), config: root.join("config") };
        let core = Core::open(p.clone()).unwrap();
        let ritual = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/rituals/total-receipt.yaml");
        core.dispatch("inscribe", json!({"path": ritual.to_str().unwrap()})).unwrap();
        core.dispatch("familiar_new", json!({"name": "alpha"})).unwrap();
        core.dispatch("familiar_allow", json!({"name": "alpha", "rituals": ["total-receipt"]})).unwrap();
        let c = core.clone();
        std::thread::spawn(move || c.serve());
        for _ in 0..200 {
            if std::os::unix::net::UnixStream::connect(p.socket()).is_ok() {
                break;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let paths = p.clone();
        std::thread::spawn(move || serve(listener, paths));
        (p, core, url)
    }

    fn done(p: &Paths, core: &Core) {
        core.stop.store(true, std::sync::atomic::Ordering::SeqCst);
        let _ = std::fs::remove_dir_all(p.state.parent().unwrap());
    }

    fn raw(url: &str, request: &str) -> String {
        let mut s = std::net::TcpStream::connect(url.trim_start_matches("http://")).unwrap();
        s.write_all(request.as_bytes()).unwrap();
        let mut out = String::new();
        let _ = s.read_to_string(&mut out);
        out
    }

    #[test]
    fn oversized_body_is_413() {
        let (p, core, url) = serving();
        let r = raw(&url, "POST /summonr/v1/signed HTTP/1.1\r\nHost: x\r\nContent-Length: 2000000\r\n\r\n");
        assert!(r.starts_with("HTTP/1.1 413"), "{r}");
        done(&p, &core);
    }

    #[test]
    fn wrong_path_404_and_wrong_method_405() {
        let (p, core, url) = serving();
        assert!(raw(&url, "POST /mcp HTTP/1.1\r\nHost: x\r\nContent-Length: 2\r\n\r\n{}").starts_with("HTTP/1.1 404"));
        assert!(raw(&url, "GET /summonr/v1/signed HTTP/1.1\r\nHost: x\r\n\r\n").starts_with("HTTP/1.1 405"));
        done(&p, &core);
    }

    #[test]
    fn origin_refused() {
        let (p, core, url) = serving();
        let r = raw(&url, "POST /summonr/v1/signed HTTP/1.1\r\nHost: x\r\nOrigin: http://evil\r\nContent-Length: 2\r\n\r\n{}");
        assert!(r.starts_with("HTTP/1.1 403"), "{r}");
        done(&p, &core);
    }

    #[test]
    fn unsigned_body_is_400() {
        let (p, core, url) = serving();
        let body = r#"{"method":"grant","params":{}}"#;
        let r = raw(&url, &format!("POST /summonr/v1/signed HTTP/1.1\r\nHost: x\r\nContent-Length: {}\r\n\r\n{body}", body.len()));
        assert!(r.starts_with("HTTP/1.1 400"), "unsigned requests never reach the core: {r}");
        done(&p, &core);
    }

    #[test]
    fn forwards_and_returns_signed_reply() {
        let (p, core, url) = serving();
        let key = crate::keys::load(&p.config.join("familiars/alpha.key")).unwrap();
        let target = url.clone();
        let mut link = crate::link::SignedLink::new(key, core.key.public(), Box::new(move |f: &Value| crate::link::http_send(&target, f)));
        use crate::link::CoreLink;
        let listed = link.call("rituals", json!({})).unwrap();
        assert_eq!(listed.as_array().unwrap().len(), 1, "{listed}");
        assert_eq!(link.call("grant", json!({})).unwrap_err().code, "not_authorized");
        done(&p, &core);
    }

    #[test]
    fn http_link_round_trip_through_a_pinned_peer() {
        let (p, core, url) = serving();
        // The same home pins its own core as a peer, as another device would.
        core.dispatch("peer_add", json!({"name": "self", "url": url, "key": core.key.public().text()})).unwrap();
        let mut link = crate::link::link_for(&p, Some("alpha"), Some("self")).unwrap();
        let started = link
            .call("invoke", json!({"target": "total-receipt", "args": {"text": "{\"amounts_minor\":[100,25]}"}}))
            .unwrap();
        let ended = link.call("wait", json!({"summon": started["summon"], "timeout_ms": 10000})).unwrap();
        assert_eq!(ended["result"]["total_minor"], 125, "{ended}");
        done(&p, &core);
    }

    #[test]
    fn slow_headers_are_cut_off() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let root = std::env::temp_dir().join(new_id("summonr-slow"));
        let p = Paths { runtime: root.join("run"), state: root.join("state"), config: root.join("config") };
        std::thread::spawn(move || serve_with(listener, p, Duration::from_millis(300)));
        let mut s = std::net::TcpStream::connect(addr).unwrap();
        s.write_all(b"POST /summonr/v1/signed HTTP/1.1\r\n").unwrap();
        // Trickle a header one byte at a time, each well inside a per-read timeout.
        let start = std::time::Instant::now();
        let mut closed = false;
        for b in b"X-Slow: aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa" {
            std::thread::sleep(Duration::from_millis(50));
            if s.write_all(&[*b]).is_err() {
                closed = true;
                break;
            }
            let mut buf = [0u8; 64];
            s.set_read_timeout(Some(Duration::from_millis(1))).unwrap();
            if let Ok(0) = s.read(&mut buf) {
                closed = true;
                break;
            }
        }
        assert!(closed, "a request that trickles in must be cut off at the deadline");
        assert!(start.elapsed() < Duration::from_secs(2));
    }

}
