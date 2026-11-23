//! A minimal HTTP/1.1 client for plain `http://` backends, used while a run
//! can be cancelled. ureq 2 offers no way to reach a request's socket, so a
//! cancelled request could only be abandoned, and a local Ollama kept
//! generating for nobody until it finished. This client owns the socket,
//! reads it in short slices, and on cancel shuts it down: the backend sees
//! the client hang up, which is what Ollama watches to stop generating.
//!
//! It does one request per connection (`Connection: close`), no redirects,
//! no proxies and no TLS; `https://` requests stay on ureq.
use serde_json::Value;
use std::io::{Read, Write};
use std::net::{Shutdown, TcpStream, ToSocketAddrs};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

/// Largest response body read, to bound memory if a server misbehaves.
const MAX_BODY: usize = 64 << 20;
/// How often a blocked read wakes to look at the cancel flag.
const SLICE: Duration = Duration::from_millis(100);

pub(crate) enum Outcome {
    Done(Result<Value, String>),
    Cancelled,
}

struct Target<'a> {
    host: &'a str,
    authority: &'a str,
    port: u16,
    path: &'a str,
}

fn target(url: &str) -> Result<Target<'_>, String> {
    let rest = url
        .strip_prefix("http://")
        .ok_or_else(|| format!("not a plain http URL: {url}"))?;
    let (authority, path) = match rest.find('/') {
        Some(i) => (&rest[..i], &rest[i..]),
        None => (rest, "/"),
    };
    let (host, port) = if let Some(v6) = authority.strip_prefix('[') {
        let (h, after) = v6.split_once(']').ok_or_else(|| format!("bad host in {url}"))?;
        (h, after.strip_prefix(':'))
    } else {
        match authority.rsplit_once(':') {
            Some((h, p)) => (h, Some(p)),
            None => (authority, None),
        }
    };
    let port = match port {
        Some(p) => p.parse().map_err(|_| format!("bad port in {url}"))?,
        None => 80,
    };
    if host.is_empty() || authority.contains('@') {
        return Err(format!("bad host in {url}"));
    }
    Ok(Target { host, authority, port, path })
}

/// Send one request and wait for the answer, giving up when `cancel` is
/// set or `timeout` passes. The socket is shut down before this returns
/// either way.
pub(crate) fn request(
    method: &str,
    url: &str,
    body: Option<&Value>,
    headers: &[(&str, &str)],
    timeout: Option<Duration>,
    cancel: &AtomicBool,
) -> Outcome {
    let deadline = Instant::now() + timeout.unwrap_or(Duration::from_secs(1800));
    let unreachable = |e: String| Outcome::Done(Err(format!("backend unreachable at {url}: {e}")));
    let t = match target(url) {
        Ok(t) => t,
        Err(e) => return unreachable(e),
    };
    let addrs: Vec<_> = match (t.host, t.port).to_socket_addrs() {
        Ok(a) => a.collect(),
        Err(e) => return unreachable(e.to_string()),
    };
    let connect_for = deadline.saturating_duration_since(Instant::now()).min(Duration::from_secs(5));
    let mut last = String::from("no address");
    let mut stream = None;
    for a in &addrs {
        match TcpStream::connect_timeout(a, connect_for.max(Duration::from_millis(1))) {
            Ok(s) => {
                stream = Some(s);
                break;
            }
            Err(e) => last = e.to_string(),
        }
    }
    let Some(mut stream) = stream else { return unreachable(last) };
    let _ = stream.set_nodelay(true);
    let payload = body.map(|b| b.to_string()).unwrap_or_default();
    let mut head = format!(
        "{method} {} HTTP/1.1\r\nHost: {}\r\nAccept: application/json\r\nConnection: close\r\n",
        t.path, t.authority
    );
    if body.is_some() {
        head.push_str(&format!("Content-Type: application/json\r\nContent-Length: {}\r\n", payload.len()));
    }
    for (k, v) in headers {
        if k.contains(['\r', '\n']) || v.contains(['\r', '\n']) {
            return Outcome::Done(Err("a request header contains a line break".into()));
        }
        head.push_str(&format!("{k}: {v}\r\n"));
    }
    head.push_str("\r\n");
    let _ = stream.set_write_timeout(Some(deadline.saturating_duration_since(Instant::now()).max(Duration::from_millis(1))));
    if let Err(e) = stream.write_all(head.as_bytes()).and_then(|_| stream.write_all(payload.as_bytes())) {
        let _ = stream.shutdown(Shutdown::Both);
        return unreachable(e.to_string());
    }
    let _ = stream.set_read_timeout(Some(SLICE));
    let mut raw = Vec::new();
    let mut buf = [0u8; 16 * 1024];
    let read = loop {
        if cancel.load(Ordering::SeqCst) {
            let _ = stream.shutdown(Shutdown::Both);
            return Outcome::Cancelled;
        }
        if Instant::now() >= deadline {
            break Err("timed out waiting for the backend".to_string());
        }
        match stream.read(&mut buf) {
            Ok(0) => break Ok(()),
            Ok(n) => {
                raw.extend_from_slice(&buf[..n]);
                if raw.len() > MAX_BODY {
                    break Err(format!("response larger than {} MiB", MAX_BODY >> 20));
                }
                // A complete Content-Length body need not wait for the close.
                if complete(&raw) {
                    break Ok(());
                }
            }
            Err(e) if matches!(e.kind(), std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut | std::io::ErrorKind::Interrupted) => continue,
            Err(e) => break Err(e.to_string()),
        }
    };
    let _ = stream.shutdown(Shutdown::Both);
    if let Err(e) = read {
        return unreachable(e);
    }
    Outcome::Done(parse(&raw))
}

fn split_head(raw: &[u8]) -> Option<(&str, &[u8])> {
    let end = raw.windows(4).position(|w| w == b"\r\n\r\n")?;
    Some((std::str::from_utf8(&raw[..end]).ok()?, &raw[end + 4..]))
}

fn header<'a>(head: &'a str, name: &str) -> Option<&'a str> {
    head.lines().skip(1).find_map(|l| {
        let (k, v) = l.split_once(':')?;
        k.trim().eq_ignore_ascii_case(name).then(|| v.trim())
    })
}

fn complete(raw: &[u8]) -> bool {
    let Some((head, body)) = split_head(raw) else { return false };
    match header(head, "content-length").and_then(|v| v.parse::<usize>().ok()) {
        Some(n) => body.len() >= n,
        None => false,
    }
}

fn dechunk(mut body: &[u8]) -> Result<Vec<u8>, String> {
    let mut out = Vec::new();
    loop {
        let line_end = body
            .windows(2)
            .position(|w| w == b"\r\n")
            .ok_or("backend sent a truncated chunked body")?;
        let size_text = std::str::from_utf8(&body[..line_end]).map_err(|_| "bad chunk size")?;
        let size = usize::from_str_radix(size_text.split(';').next().unwrap_or("").trim(), 16)
            .map_err(|_| format!("bad chunk size `{size_text}`"))?;
        body = &body[line_end + 2..];
        if size == 0 {
            return Ok(out);
        }
        if body.len() < size + 2 {
            return Err("backend sent a truncated chunked body".into());
        }
        out.extend_from_slice(&body[..size]);
        body = &body[size + 2..];
    }
}

/// The JSON body of a complete response, or the same errors ureq gives.
fn parse(raw: &[u8]) -> Result<Value, String> {
    let (head, body) = split_head(raw).ok_or("backend closed the connection without an answer")?;
    let status: u16 = head
        .split_whitespace()
        .nth(1)
        .and_then(|c| c.parse().ok())
        .ok_or("backend sent a malformed status line")?;
    let body = if header(head, "transfer-encoding").is_some_and(|v| v.eq_ignore_ascii_case("chunked")) {
        dechunk(body)?
    } else if let Some(n) = header(head, "content-length").and_then(|v| v.parse::<usize>().ok()) {
        body.get(..n).ok_or("backend sent a truncated body")?.to_vec()
    } else {
        body.to_vec()
    };
    if !(200..300).contains(&status) {
        return Err(format!("backend returned {status}: {}", String::from_utf8_lossy(&body).trim()));
    }
    serde_json::from_slice(&body).map_err(|e| format!("backend sent unreadable JSON: {e}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn urls_split_into_host_port_and_path() {
        let t = target("http://172.17.0.1:11434/api/chat").unwrap();
        assert_eq!((t.host, t.port, t.path, t.authority), ("172.17.0.1", 11434, "/api/chat", "172.17.0.1:11434"));
        let t = target("http://[::1]:8080").unwrap();
        assert_eq!((t.host, t.port, t.path), ("::1", 8080, "/"));
        assert_eq!(target("http://localhost/x").unwrap().port, 80);
        assert!(target("https://x/").is_err());
        assert!(target("http://user@x/").is_err());
    }

    #[test]
    fn responses_parse_by_length_chunks_or_close_and_errors_read_like_ureq() {
        let by_len = b"HTTP/1.1 200 OK\r\nContent-Length: 8\r\n\r\n{\"a\":1}\n";
        assert!(complete(by_len));
        assert_eq!(parse(by_len).unwrap(), json!({"a": 1}));
        let chunked = b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n4\r\n{\"a\"\r\n3;x=y\r\n:1}\r\n0\r\n\r\n";
        assert!(!complete(chunked));
        assert_eq!(parse(chunked).unwrap(), json!({"a": 1}));
        assert_eq!(parse(b"HTTP/1.0 200 OK\r\n\r\n[1,2]").unwrap(), json!([1, 2]));
        assert_eq!(parse(b"HTTP/1.1 404 Not Found\r\nContent-Length: 17\r\n\r\nmodel not found\r\n").unwrap_err(), "backend returned 404: model not found");
        assert!(parse(b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n4\r\n{\"").unwrap_err().contains("truncated"));
        assert!(parse(b"").unwrap_err().contains("without an answer"));
    }
}
