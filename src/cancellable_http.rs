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
use std::io::{self, BufRead, BufReader, Read, Write};
use std::net::{Shutdown, TcpStream, ToSocketAddrs};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

/// Largest response body read, to bound memory if a server misbehaves.
const MAX_BODY: usize = 64 << 20;
const MAX_LINE: usize = 8 << 10;
const MAX_HEADERS: usize = 128;
const MAX_HEAD: usize = 64 << 10;
const MAX_CHUNK_METADATA: usize = 1 << 20;
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
        let (h, after) = v6
            .split_once(']')
            .ok_or_else(|| format!("bad host in {url}"))?;
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
    Ok(Target {
        host,
        authority,
        port,
        path,
    })
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
    let now = Instant::now();
    let default_timeout = Duration::from_secs(1800);
    let deadline = now
        .checked_add(timeout.unwrap_or(default_timeout))
        .unwrap_or(now + default_timeout);
    if cancel.load(Ordering::SeqCst) {
        return Outcome::Cancelled;
    }
    let unreachable = |e: String| Outcome::Done(Err(format!("backend unreachable at {url}: {e}")));
    let t = match target(url) {
        Ok(t) => t,
        Err(e) => return unreachable(e),
    };
    let addrs: Vec<_> = match (t.host, t.port).to_socket_addrs() {
        Ok(a) => a.collect(),
        Err(e) => return unreachable(e.to_string()),
    };
    let mut last = String::from("no address");
    let mut stream = None;
    for a in &addrs {
        if cancel.load(Ordering::SeqCst) {
            return Outcome::Cancelled;
        }
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return unreachable("timed out connecting to the backend".into());
        }
        match TcpStream::connect_timeout(a, remaining.min(Duration::from_secs(5))) {
            Ok(s) => {
                stream = Some(s);
                break;
            }
            Err(e) => last = e.to_string(),
        }
    }
    let Some(stream) = stream else {
        return if cancel.load(Ordering::SeqCst) {
            Outcome::Cancelled
        } else {
            unreachable(last)
        };
    };
    let _ = stream.set_nodelay(true);
    let payload = body.map(|b| b.to_string()).unwrap_or_default();
    let mut head = format!(
        "{method} {} HTTP/1.1\r\nHost: {}\r\nAccept: application/json\r\nConnection: close\r\n",
        t.path, t.authority
    );
    if body.is_some() {
        head.push_str(&format!(
            "Content-Type: application/json\r\nContent-Length: {}\r\n",
            payload.len()
        ));
    }
    for (k, v) in headers {
        if k.contains(['\r', '\n']) || v.contains(['\r', '\n']) {
            return Outcome::Done(Err("a request header contains a line break".into()));
        }
        head.push_str(&format!("{k}: {v}\r\n"));
    }
    head.push_str("\r\n");
    let mut socket = RequestSocket {
        stream,
        deadline,
        cancel,
    };
    let result = (|| {
        socket
            .stream
            .set_read_timeout(Some(SLICE))
            .map_err(|e| e.to_string())?;
        socket
            .stream
            .set_write_timeout(Some(SLICE))
            .map_err(|e| e.to_string())?;
        socket
            .write_all(head.as_bytes())
            .map_err(|e| e.to_string())?;
        socket
            .write_all(payload.as_bytes())
            .map_err(|e| e.to_string())?;
        read_response(&mut BufReader::new(&mut socket), MAX_BODY)
    })();
    if cancel.load(Ordering::SeqCst) {
        Outcome::Cancelled
    } else {
        Outcome::Done(result)
    }
}

struct RequestSocket<'a> {
    stream: TcpStream,
    deadline: Instant,
    cancel: &'a AtomicBool,
}

impl RequestSocket<'_> {
    fn transfer<T>(
        &mut self,
        mut operation: impl FnMut(&mut TcpStream) -> io::Result<T>,
    ) -> io::Result<T> {
        loop {
            if self.cancel.load(Ordering::SeqCst) {
                return Err(io::Error::other("backend request cancelled"));
            }
            if Instant::now() >= self.deadline {
                return Err(io::Error::new(
                    io::ErrorKind::TimedOut,
                    "backend request timed out",
                ));
            }
            match operation(&mut self.stream) {
                Err(e)
                    if matches!(
                        e.kind(),
                        io::ErrorKind::WouldBlock
                            | io::ErrorKind::TimedOut
                            | io::ErrorKind::Interrupted
                    ) => {}
                result => return result,
            }
        }
    }
}

impl Read for RequestSocket<'_> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        self.transfer(|stream| stream.read(buf))
    }
}

impl Write for RequestSocket<'_> {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.transfer(|stream| stream.write(buf))
    }

    fn flush(&mut self) -> io::Result<()> {
        self.stream.flush()
    }
}

impl Drop for RequestSocket<'_> {
    fn drop(&mut self) {
        let _ = self.stream.shutdown(Shutdown::Both);
    }
}

fn read_line(reader: &mut impl BufRead, limit: usize, line: &mut Vec<u8>) -> Result<(), String> {
    line.clear();
    reader
        .take((limit + 1) as u64)
        .read_until(b'\n', line)
        .map_err(|e| e.to_string())?;
    if line.len() > limit {
        return Err("backend response metadata exceeds its size limit".into());
    }
    if !line.ends_with(b"\r\n") {
        return Err("backend sent a truncated or malformed response line".into());
    }
    line.truncate(line.len() - 2);
    Ok(())
}

fn read_headers(
    reader: &mut impl BufRead,
    mut remaining: usize,
    mut field: impl FnMut(&str, &str) -> Result<(), String>,
) -> Result<(), String> {
    let mut line = Vec::new();
    for count in 0..=MAX_HEADERS {
        read_line(reader, MAX_LINE.min(remaining), &mut line)?;
        remaining -= line.len() + 2;
        if line.is_empty() {
            return Ok(());
        }
        if count == MAX_HEADERS {
            break;
        }
        let (name, value) = std::str::from_utf8(&line)
            .ok()
            .and_then(|s| s.split_once(':'))
            .ok_or("backend sent a malformed response header")?;
        if name.is_empty()
            || !name
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b"!#$%&'*+-.^`_|~".contains(&b))
            || value.bytes().any(|b| b.is_ascii_control() && b != b'\t')
        {
            return Err("backend sent a malformed response header".into());
        }
        field(name, value.trim_matches([' ', '\t']))?;
    }
    Err("backend sent too many response headers".into())
}

fn append_body(reader: &mut impl Read, size: usize, out: &mut Vec<u8>) -> Result<(), String> {
    let count = reader
        .take(size as u64)
        .read_to_end(out)
        .map_err(|e| e.to_string())?;
    if count != size {
        return Err("backend sent a truncated body".into());
    }
    Ok(())
}

fn read_chunks(reader: &mut impl BufRead, limit: usize) -> Result<Vec<u8>, String> {
    let mut out = Vec::new();
    let mut line = Vec::new();
    let mut metadata = MAX_CHUNK_METADATA;
    loop {
        read_line(reader, MAX_LINE.min(metadata), &mut line)?;
        metadata -= line.len() + 2;
        let size_text = line.split(|&b| b == b';').next().unwrap_or_default();
        if size_text.is_empty()
            || !size_text.iter().all(u8::is_ascii_hexdigit)
            || line.iter().any(u8::is_ascii_control)
        {
            return Err("backend sent a malformed chunk size".into());
        }
        let size = usize::from_str_radix(std::str::from_utf8(size_text).unwrap(), 16)
            .map_err(|_| "backend sent an invalid chunk size")?;
        if size == 0 {
            read_headers(reader, metadata.min(MAX_HEAD), |name, _| {
                if name.eq_ignore_ascii_case("content-length")
                    || name.eq_ignore_ascii_case("transfer-encoding")
                {
                    return Err("backend sent a framing header in the chunk trailers".into());
                }
                Ok(())
            })?;
            return Ok(out);
        }
        if size > limit - out.len() {
            return Err("backend response body exceeds its size limit".into());
        }
        append_body(reader, size, &mut out)?;
        let mut ending = [0; 2];
        reader.read_exact(&mut ending).map_err(|e| e.to_string())?;
        if ending != *b"\r\n" {
            return Err("backend sent an invalid chunk terminator".into());
        }
    }
}

fn read_response(reader: &mut impl BufRead, limit: usize) -> Result<Value, String> {
    let mut line = Vec::new();
    read_line(reader, MAX_LINE, &mut line)?;
    let mut parts = line.splitn(3, |&b| b == b' ');
    let version = parts.next().unwrap_or_default();
    let code = parts.next().unwrap_or_default();
    if !matches!(version, b"HTTP/1.0" | b"HTTP/1.1")
        || code.len() != 3
        || !code.iter().all(u8::is_ascii_digit)
    {
        return Err("backend sent a malformed status line".into());
    }
    let status = u16::from(code[0] - b'0') * 100
        + u16::from(code[1] - b'0') * 10
        + u16::from(code[2] - b'0');
    let mut length = None;
    let mut chunked = false;
    read_headers(reader, MAX_HEAD - line.len() - 2, |name, value| {
        if name.eq_ignore_ascii_case("content-length") {
            if length.is_some() || value.is_empty() || !value.bytes().all(|b| b.is_ascii_digit()) {
                return Err("backend sent an invalid Content-Length".into());
            }
            length = Some(
                value
                    .parse::<usize>()
                    .map_err(|_| "backend sent an invalid Content-Length")?,
            );
        } else if name.eq_ignore_ascii_case("transfer-encoding") {
            if chunked || !value.eq_ignore_ascii_case("chunked") {
                return Err("backend sent an unsupported Transfer-Encoding".into());
            }
            chunked = true;
        }
        Ok(())
    })?;
    if chunked && length.is_some() {
        return Err("backend sent conflicting response framing".into());
    }
    let mut body = Vec::new();
    if chunked {
        body = read_chunks(reader, limit)?;
    } else if let Some(size) = length {
        if size > limit {
            return Err("backend response body exceeds its size limit".into());
        }
        append_body(reader, size, &mut body)?;
    } else {
        reader
            .take(limit as u64)
            .read_to_end(&mut body)
            .map_err(|e| e.to_string())?;
        if body.len() == limit && !reader.fill_buf().map_err(|e| e.to_string())?.is_empty() {
            return Err("backend response body exceeds its size limit".into());
        }
    }
    if !(200..300).contains(&status) {
        return Err(format!(
            "backend returned {status}: {}",
            String::from_utf8_lossy(&body).trim()
        ));
    }
    serde_json::from_slice(&body).map_err(|e| format!("backend sent unreadable JSON: {e}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn parse(mut raw: &[u8]) -> Result<Value, String> {
        read_response(&mut raw, MAX_BODY)
    }

    #[test]
    fn urls_split_into_host_port_and_path() {
        let t = target("http://172.17.0.1:11434/api/chat").unwrap();
        assert_eq!(
            (t.host, t.port, t.path, t.authority),
            ("172.17.0.1", 11434, "/api/chat", "172.17.0.1:11434")
        );
        let t = target("http://[::1]:8080").unwrap();
        assert_eq!((t.host, t.port, t.path), ("::1", 8080, "/"));
        assert_eq!(target("http://localhost/x").unwrap().port, 80);
        assert!(target("https://x/").is_err());
        assert!(target("http://user@x/").is_err());
    }

    #[test]
    fn responses_parse_by_length_chunks_or_close_and_errors_read_like_ureq() {
        let by_len = b"HTTP/1.1 200 OK\r\nContent-Length: 8\r\n\r\n{\"a\":1}\n";
        assert_eq!(parse(by_len).unwrap(), json!({"a": 1}));
        let chunked = b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n4\r\n{\"a\"\r\n3;x=y\r\n:1}\r\n0\r\n\r\n";
        assert_eq!(parse(chunked).unwrap(), json!({"a": 1}));
        assert_eq!(
            parse(b"HTTP/1.0 200 OK\r\n\r\n[1,2]").unwrap(),
            json!([1, 2])
        );
        assert_eq!(
            parse(b"HTTP/1.1 404 Not Found\r\nContent-Length: 17\r\n\r\nmodel not found\r\n")
                .unwrap_err(),
            "backend returned 404: model not found"
        );
        assert!(
            parse(b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n4\r\n{\"")
                .unwrap_err()
                .contains("truncated")
        );
        assert!(parse(b"").unwrap_err().contains("truncated"));
    }

    #[test]
    fn rejects_invalid_response_framing() {
        for framing in [
            "Content-Length: +2",
            "Content-Length: invalid",
            "Content-Length: 999999999999999999999999999",
            "Content-Length: 2\r\nContent-Length: 3",
            "Transfer-Encoding: gzip",
            "Transfer-Encoding: chunked\r\nContent-Length: 2",
        ] {
            let raw = format!(
                "HTTP/1.1 200 OK\r\n{framing}\r\n\r\n{}",
                if framing.starts_with("Transfer-Encoding: chunked") {
                    "2\r\n{}\r\n0\r\n\r\n"
                } else {
                    "{}"
                }
            );
            assert!(parse(raw.as_bytes()).is_err(), "{framing}");
        }
        assert!(parse(b"HTTP/1.1 200 OK\r\nContent-Length: 3\r\n\r\n{}").is_err());
        for chunks in ["2\r\n{}xx0\r\n\r\n", "2\r\n{}\r\n0\r\n", "3\r\n{}"] {
            let raw = format!("HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n{chunks}");
            assert!(parse(raw.as_bytes()).is_err(), "{chunks:?}");
        }
    }

    #[test]
    fn rejects_excessive_response_metadata() {
        let long_line = format!(
            "HTTP/1.1 200 OK\r\nX-Test: {}\r\n\r\n{{}}",
            "a".repeat(8192)
        );
        let many_headers = format!("HTTP/1.1 200 OK\r\n{}\r\n{{}}", "X: a\r\n".repeat(129));
        let big_head = format!(
            "HTTP/1.1 200 OK\r\n{}\r\n{{}}",
            format!("X: {}\r\n", "a".repeat(2000)).repeat(40)
        );
        for response in [long_line, many_headers, big_head] {
            assert!(parse(response.as_bytes()).is_err());
        }
    }

    #[test]
    fn body_read_stops_at_the_limit_for_every_framing() {
        for (headers, body) in [
            ("Content-Length: 5\r\n", "12345"),
            (
                "Transfer-Encoding: chunked\r\n",
                "3\r\n123\r\n2\r\n45\r\n0\r\n\r\n",
            ),
            ("", "12345"),
        ] {
            let prefix = format!("HTTP/1.1 200 OK\r\n{headers}\r\n");
            let raw = format!("{prefix}{body}");
            let mut reader = io::Cursor::new(raw);
            assert!(read_response(&mut reader, 4)
                .unwrap_err()
                .contains("size limit"));
            let body_read = reader.position() as usize - prefix.len();
            assert_eq!(
                body_read,
                match headers {
                    "" => 4,
                    "Content-Length: 5\r\n" => 0,
                    _ => "3\r\n123\r\n2\r\n".len(),
                }
            );
        }
        assert_eq!(
            read_response(&mut &b"HTTP/1.1 200 OK\r\n\r\ntrue"[..], 4).unwrap(),
            json!(true)
        );
    }

    #[test]
    fn socket_closes_on_cancellation_and_after_complete_chunked_response() {
        use std::net::TcpListener;
        use std::thread;

        for cancel_request in [false, true] {
            let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
            let url = format!("http://{}/", listener.local_addr().unwrap());
            let cancel = AtomicBool::new(false);
            thread::scope(|scope| {
                let server = scope.spawn(|| {
                    let (mut peer, _) = listener.accept().unwrap();
                    peer.set_read_timeout(Some(Duration::from_secs(2))).unwrap();
                    let mut line = Vec::new();
                    let mut request = BufReader::new(&mut peer);
                    loop {
                        line.clear();
                        assert_ne!(request.read_until(b'\n', &mut line).unwrap(), 0);
                        if line == b"\r\n" {
                            break;
                        }
                    }
                    peer.write_all(b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n")
                        .unwrap();
                    if cancel_request {
                        // Leave the client waiting for the rest of a line.
                        peer.write_all(b"2").unwrap();
                        thread::sleep(Duration::from_millis(50));
                        cancel.store(true, Ordering::SeqCst);
                    } else {
                        peer.write_all(b"2\r\n{}\r\n0\r\nX-Test: yes\r\n\r\n")
                            .unwrap();
                    }
                    assert_eq!(peer.read(&mut [0]).unwrap(), 0);
                });

                let start = Instant::now();
                let outcome = request(
                    "GET",
                    &url,
                    None,
                    &[],
                    Some(Duration::from_secs(3)),
                    &cancel,
                );
                assert!(start.elapsed() < Duration::from_secs(2));
                if cancel_request {
                    assert!(matches!(outcome, Outcome::Cancelled));
                } else {
                    assert!(matches!(outcome, Outcome::Done(Ok(value)) if value == json!({})));
                }
                server.join().unwrap();
            });
        }
    }
}
