//! RFC 3161 timestamps, entirely through the system's `openssl`: it builds
//! requests and checks tokens. Summonr parses no certificate formats itself.

use std::io::Read;
use std::path::Path;
use std::process::{Command, Stdio};

const MAX_RESPONSE: u64 = 1_048_576;

/// The configured `openssl`, or the one on PATH.
pub fn program(configured: Option<&str>) -> String {
    configured.unwrap_or("openssl").to_string()
}

pub fn available(prog: &str) -> bool {
    Command::new(prog).arg("version").stdout(Stdio::null()).stderr(Stdio::null()).status().is_ok_and(|s| s.success())
}

fn run(prog: &str, args: &[&str]) -> Result<Vec<u8>, String> {
    let out = Command::new(prog)
        .args(args)
        .stdin(Stdio::null())
        .output()
        .map_err(|e| format!("openssl not found ({prog}): {e}"))?;
    if !out.status.success() {
        return Err(String::from_utf8_lossy(&out.stderr).trim().to_string());
    }
    Ok(out.stdout)
}

fn check_digest(digest_hex: &str) -> Result<(), String> {
    if digest_hex.len() == 64 && digest_hex.bytes().all(|b| b.is_ascii_hexdigit()) {
        Ok(())
    } else {
        Err("a timestamp digest is 64 hex characters (SHA-256)".into())
    }
}

/// A DER TimeStampReq for `digest_hex`, built by `openssl ts -query`.
pub fn query(prog: &str, digest_hex: &str) -> Result<Vec<u8>, String> {
    check_digest(digest_hex)?;
    if !available(prog) {
        return Err(format!("openssl not found ({prog}); RFC 3161 timestamps need it"));
    }
    run(prog, &["ts", "-query", "-sha256", "-digest", digest_hex, "-cert"])
}

/// POST a request to a timestamp authority; the raw response bytes.
pub fn post(url: &str, request: &[u8]) -> Result<Vec<u8>, String> {
    let resp = ureq::AgentBuilder::new()
        .timeout(std::time::Duration::from_secs(30))
        .build()
        .post(url)
        .set("Content-Type", "application/timestamp-query")
        .send_bytes(request)
        .map_err(|e| format!("timestamp authority {url}: {e}"))?;
    let mut body = Vec::new();
    resp.into_reader()
        .take(MAX_RESPONSE)
        .read_to_end(&mut body)
        .map_err(|e| format!("timestamp authority {url}: {e}"))?;
    Ok(body)
}

fn in_temp<T>(files: &[(&str, &[u8])], f: impl FnOnce(&Path) -> Result<T, String>) -> Result<T, String> {
    let dir = std::env::temp_dir().join(crate::util::new_id("summonr-tsa"));
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    let result = (|| {
        for (name, bytes) in files {
            std::fs::write(dir.join(name), bytes).map_err(|e| e.to_string())?;
        }
        f(&dir)
    })();
    let _ = std::fs::remove_dir_all(&dir);
    result
}

/// Check `openssl ts -reply -text` output: the status line itself (the one
/// straight after `Status info:`) says granted, the hash is SHA-256, and the
/// timestamped message is exactly `digest_hex`.
pub fn check_reply_text(text: &str, digest_hex: &str) -> Result<(), String> {
    let lines: Vec<&str> = text.lines().collect();
    let status = lines
        .iter()
        .position(|l| l.trim() == "Status info:")
        .and_then(|i| lines.get(i + 1))
        .map(|l| l.trim())
        .unwrap_or("no status");
    if status != "Status: Granted." {
        return Err(format!("the authority did not grant the timestamp ({status})"));
    }
    if !lines.iter().any(|l| l.trim() == "Hash Algorithm: sha256") {
        return Err("the timestamp is not over a SHA-256 digest".into());
    }
    let start = lines.iter().position(|l| l.trim() == "Message data:").ok_or("the reply carries no message data")?;
    let mut bytes = String::new();
    for l in &lines[start + 1..] {
        // `    0000 - ab ab … ab-ab … ab   ................`
        let Some((offset, rest)) = l.split_once(" - ") else { break };
        if !l.starts_with(' ') || offset.trim().is_empty() {
            break;
        }
        let hex = rest.get(..47).unwrap_or(rest).replace('-', " ");
        bytes.extend(hex.split_whitespace());
    }
    if !bytes.eq_ignore_ascii_case(digest_hex) {
        return Err("the authority timestamped a different digest".into());
    }
    Ok(())
}

/// Whether a response grants a timestamp for exactly `digest_hex`.
pub fn granted(prog: &str, response: &[u8], digest_hex: &str) -> Result<(), String> {
    in_temp(&[("r.tsr", response)], |dir| {
        let path = dir.join("r.tsr");
        let text = run(prog, &["ts", "-reply", "-in", path.to_str().unwrap_or(""), "-text"])?;
        check_reply_text(&String::from_utf8_lossy(&text), digest_hex)
    })
}

/// Check a stored response for `digest_hex` against a CA certificate file.
pub fn verify(prog: &str, digest_hex: &str, response: &[u8], ca_file: &Path) -> Result<String, String> {
    check_digest(digest_hex)?;
    in_temp(&[("r.tsr", response)], |dir| {
        let path = dir.join("r.tsr");
        let out = run(prog, &["ts", "-verify", "-digest", digest_hex, "-in", path.to_str().unwrap_or(""), "-CAfile", ca_file.to_str().unwrap_or("")])?;
        Ok(String::from_utf8_lossy(&out).trim().to_string())
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn missing_openssl_is_reported() {
        let prog = "/nonexistent/openssl";
        assert!(!available(prog));
        let e = query(prog, &"ab".repeat(32)).unwrap_err();
        assert!(e.contains("openssl not found"), "{e}");
    }

    #[test]
    fn query_builds_der_request() {
        if !available("openssl") {
            eprintln!("skipping: openssl is not installed");
            return;
        }
        let der = query("openssl", &"ab".repeat(32)).unwrap();
        assert_eq!(der.first(), Some(&0x30), "a DER SEQUENCE");
    }

    #[test]
    fn digest_must_be_sha256_hex() {
        assert!(query("openssl", "zz").is_err());
    }

    const REPLY: &str = "Status info:
Status: Granted.
Status description: unspecified
Failure info: unspecified

TST info:
Version: 1
Policy OID: tsa_policy1
Hash Algorithm: sha256
Message data:
    0000 - ab ab ab ab ab ab ab ab-ab ab ab ab ab ab ab ab   ................
    0010 - ab ab ab ab ab ab ab ab-ab ab ab ab ab ab ab ab   ................
Serial number: 0x02
";

    #[test]
    fn a_granted_reply_for_this_digest_passes() {
        assert_eq!(check_reply_text(REPLY, &"ab".repeat(32)), Ok(()));
    }

    #[test]
    fn a_reply_for_another_digest_fails() {
        let e = check_reply_text(REPLY, &"cd".repeat(32)).unwrap_err();
        assert!(e.contains("different digest"), "{e}");
    }

    #[test]
    fn only_the_real_status_line_counts() {
        let rejected = REPLY.replace("Status: Granted.", "Status: Rejected.").replace(
            "Status description: unspecified",
            "Status description: Status: Granted.",
        );
        assert!(check_reply_text(&rejected, &"ab".repeat(32)).is_err());
        let spoofed = "Status info:\nStatus description: x\nStatus: Granted.\n";
        assert!(check_reply_text(spoofed, &"ab".repeat(32)).is_err());
    }

    #[test]
    fn a_reply_with_another_hash_algorithm_fails() {
        assert!(check_reply_text(&REPLY.replace("sha256", "sha1"), &"ab".repeat(32)).is_err());
    }

}
