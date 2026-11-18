//! Explicit-origin, read-only HTTP. No redirects, cookies, credentials or ambient proxies.
use crate::reagent::{self, Authority, Fail, Reagent, ToolSpec};
use serde_json::{json, Value};
use std::{
    io::Read,
    time::{Duration, Instant},
};
pub struct Http;
const TOOLS: &[ToolSpec] = &[
    ToolSpec {
        reagent: "http",
        name: "get_text",
        effect: "read",
        determinism: "deterministic over live state",
        description: "GET bounded UTF-8 text from an explicitly granted origin; redirects refused",
    },
    ToolSpec {
        reagent: "http",
        name: "get_json",
        effect: "read",
        determinism: "deterministic over live state",
        description: "GET and parse bounded UTF-8 JSON from an explicitly granted origin; redirects refused",
    },
    ToolSpec {
        reagent: "http",
        name: "post",
        effect: "write",
        determinism: "deterministic over live state",
        description: "Authenticated HTTP POST to an explicitly granted origin; redirects refused",
    },
    ToolSpec {
        reagent: "http",
        name: "put",
        effect: "idempotent write",
        determinism: "deterministic over live state",
        description: "Authenticated idempotent HTTP PUT to an explicitly granted origin; redirects refused",
    },
    ToolSpec {
        reagent: "http",
        name: "patch",
        effect: "write",
        determinism: "deterministic over live state",
        description: "Authenticated HTTP PATCH to an explicitly granted origin; redirects refused",
    },
    ToolSpec {
        reagent: "http",
        name: "delete",
        effect: "idempotent write",
        determinism: "deterministic over live state",
        description: "Authenticated idempotent HTTP DELETE to an explicitly granted origin; redirects refused",
    },
];
pub fn parse_url(text: &str) -> Result<url::Url, String> {
    let u = url::Url::parse(text).map_err(|e| e.to_string())?;
    if !matches!(u.scheme(), "http" | "https")
        || u.host_str().is_none()
        || !u.username().is_empty()
        || u.password().is_some()
        || u.fragment().is_some()
    {
        return Err("URL must be HTTP(S), with a host and no credentials or fragment".into());
    }
    Ok(u)
}
fn request_url(args: &Value) -> Result<url::Url, Fail> {
    let text = args["url"]
        .as_str()
        .ok_or_else(|| Fail::Invalid("http.get_text needs a string argument `url`".into()))?;
    parse_url(text).map_err(Fail::Invalid)
}
fn host_matches(pattern: &str, host: &str) -> bool {
    let p = pattern.trim().to_ascii_lowercase();
    if let Some(suffix) = p.strip_prefix("*.") {
        host == suffix || host.ends_with(&format!(".{suffix}"))
    } else {
        host == p
    }
}

fn is_non_public_ip(ip: std::net::IpAddr) -> bool {
    match ip {
        std::net::IpAddr::V4(v4) => {
            v4.is_loopback()
                || v4.is_private()
                || v4.is_link_local()
                || v4.is_unspecified()
                || v4.is_multicast()
                || v4.is_broadcast()
                || (v4.octets()[0] == 100 && (v4.octets()[1] & 0b1100_0000) == 64)
        }
        std::net::IpAddr::V6(v6) => {
            if let Some(v4) = v6.to_ipv4_mapped() {
                return is_non_public_ip(std::net::IpAddr::V4(v4));
            }
            let seg0 = v6.segments()[0];
            v6.is_loopback()
                || v6.is_unspecified()
                || v6.is_multicast()
                || (seg0 & 0xfe00) == 0xfc00
                || (seg0 & 0xffc0) == 0xfe80
        }
    }
}

pub fn check_egress(cfg: Option<&crate::util::Config>, u: &url::Url) -> Result<(), Fail> {
    let Some(egress) = cfg.map(|c| &c.egress) else {
        return Ok(());
    };
    let raw_host = u
        .host_str()
        .ok_or_else(|| Fail::Invalid("URL has no host".into()))?;
    let host = raw_host
        .trim_start_matches('[')
        .trim_end_matches(']')
        .to_ascii_lowercase();
    if egress.deny_hosts.iter().any(|p| host_matches(p, &host)) {
        return Err(Fail::Denied(format!(
            "egress policy blocks host `{host}`"
        )));
    }
    if !egress.allow_hosts.is_empty() && !egress.allow_hosts.iter().any(|p| host_matches(p, &host))
    {
        return Err(Fail::Denied(format!(
            "egress policy does not allow host `{host}`"
        )));
    }
    if egress.block_private_ips {
        if host == "localhost" || host.ends_with(".localhost") {
            return Err(Fail::Denied(format!(
                "egress policy blocks loopback host `{host}`"
            )));
        }
        if let Ok(ip) = host.parse::<std::net::IpAddr>() {
            if is_non_public_ip(ip) {
                return Err(Fail::Denied(format!(
                    "egress policy blocks non-public IP `{ip}`"
                )));
            }
        } else if let Some(port) = u.port_or_known_default() {
            if let Ok(addrs) = std::net::ToSocketAddrs::to_socket_addrs(&(host.as_str(), port)) {
                for addr in addrs {
                    if is_non_public_ip(addr.ip()) {
                        return Err(Fail::Denied(format!(
                            "egress policy blocks host `{host}` resolving to non-public IP `{}`",
                            addr.ip()
                        )));
                    }
                }
            }
        }
    }
    Ok(())
}

pub fn validate_origin(text: &str) -> Result<(), String> {
    let u = parse_url(text)?;
    if u.origin().ascii_serialization() != text {
        return Err(
            "origin must be canonical scheme://host[:port], with no path, query, or trailing slash"
                .into(),
        );
    }
    Ok(())
}
impl Reagent for Http {
    fn name(&self) -> &'static str {
        "http"
    }
    fn tools(&self) -> &'static [ToolSpec] {
        TOOLS
    }
    fn schema(&self, name: &str) -> Option<Value> {
        let mut props = json!({"url":{"type":"string","minLength":1,"maxLength":8192},"max_bytes":{"type":"integer","minimum":1,"maximum":1048576},"timeout_ms":{"type":"integer","minimum":1,"maximum":30000}});
        match name {
            "get_text" => {}
            "get_json" => {
                props["schema"] = json!({"type":"object"});
            }
            "post" | "put" | "patch" => {
                props["body"] = json!({});
                props["headers"] = json!({"type":"object"});
                props["auth"] = json!({"type":"string","minLength":1,"maxLength":64});
                props["idempotency_key"] = json!({"type":"string","minLength":1,"maxLength":200});
            }
            "delete" => {
                props["headers"] = json!({"type":"object"});
                props["auth"] = json!({"type":"string","minLength":1,"maxLength":64});
                props["idempotency_key"] = json!({"type":"string","minLength":1,"maxLength":200});
            }
            _ => return None,
        }
        Some(json!({"type":"object","properties":props,"required":["url"],"additionalProperties":false}))
    }
    fn authorize(
        &self,
        _: Option<&str>,
        name: &str,
        args: &Value,
        auth: &Authority,
    ) -> Result<(), Fail> {
        let u = request_url(args)?;
        check_egress(auth.config, &u)?;
        let origin = u.origin().ascii_serialization();
        reagent::require(
            auth,
            reagent::need("http", name, &[("origin", &origin)], &[]),
        )?;
        if !auth.grants.iter().any(|g| {
            g.reagent == "http" && g.action == name && g.on.get("origin") == Some(&origin)
        }) {
            return Err(Fail::Denied(
                "HTTP access requires an explicit origin grant".into(),
            ));
        }
        if let Some(auth_name) = args.get("auth").and_then(Value::as_str) {
            let cfg = auth
                .config
                .ok_or_else(|| Fail::Denied("no HTTP auth configuration available".into()))?;
            let entry = cfg.http_auth.get(auth_name).ok_or_else(|| {
                Fail::Denied(format!("unknown http_auth credential `{auth_name}`"))
            })?;
            if entry.origin != origin {
                return Err(Fail::Denied(format!(
                    "http_auth `{auth_name}` is pinned to `{}`, not `{origin}`",
                    entry.origin
                )));
            }
            let token = std::env::var(&entry.token_env).unwrap_or_default();
            if token.trim().is_empty() {
                return Err(Fail::Denied(format!(
                    "http_auth `{auth_name}` requires env var `{}`",
                    entry.token_env
                )));
            }
        }
        if let Some(hdrs) = args.get("headers").and_then(Value::as_object) {
            for (k, v) in hdrs {
                if !v.is_string() || k.contains('\r') || k.contains('\n') {
                    return Err(Fail::Invalid(format!("invalid HTTP header `{k}`")));
                }
            }
        }
        Ok(())
    }
    fn call(
        &self,
        mail_root: Option<&str>,
        name: &str,
        args: &Value,
        auth: &Authority,
    ) -> Result<Value, Fail> {
        self.authorize(mail_root, name, args, auth)?;
        // The same parse `authorize` just accepted; the request uses it.
        let u = request_url(args)?;
        let mut timeout = Duration::from_millis(args["timeout_ms"].as_u64().unwrap_or(5000));
        if let Some(d) = auth.deadline {
            timeout = timeout.min(d.saturating_duration_since(Instant::now()));
        }
        if timeout.is_zero() {
            return Err(Fail::Exhausted("HTTP deadline".into()));
        }
        let deadline = Instant::now() + timeout;
        let agent = ureq::AgentBuilder::new()
            .try_proxy_from_env(false)
            .redirects(0)
            .timeout(timeout)
            .build();
        let is_write = matches!(name, "post" | "put" | "patch" | "delete");
        if is_write {
            if let Some(want) = auth.expected_digest {
                if target_digest(auth.config, args)?.as_deref() != Some(want) {
                    return Err(Fail::Denied(
                        "HTTP resource changed after this petition was filed".into(),
                    ));
                }
            }
        }
        let mut req = match name {
            "get_text" | "get_json" => agent
                .get(u.as_str())
                .set("Accept", "text/plain, application/json, text/*;q=0.9"),
            "post" => agent.post(u.as_str()),
            "put" => agent.put(u.as_str()),
            "patch" => agent.patch(u.as_str()),
            "delete" => agent.delete(u.as_str()),
            other => return Err(Fail::Failed(format!("reagent `http` has no tool `{other}`"))),
        };
        if let Some(auth_name) = args.get("auth").and_then(Value::as_str) {
            let entry = &auth.config.expect("authorize checked config").http_auth[auth_name];
            let token = std::env::var(&entry.token_env).unwrap_or_default();
            req = req.set(&entry.header, &format!("{}{}", entry.prefix, token));
        }
        if let Some(hdrs) = args.get("headers").and_then(Value::as_object) {
            for (k, v) in hdrs {
                if let Some(vs) = v.as_str() {
                    req = req.set(k, vs);
                }
            }
        }
        if let Some(idem) = args.get("idempotency_key").and_then(Value::as_str) {
            req = req.set("Idempotency-Key", idem);
        }
        let response = if matches!(name, "post" | "put" | "patch") {
            match args.get("body") {
                Some(Value::String(s)) => req.send_string(s),
                Some(v) if !v.is_null() => req.send_json(v.clone()),
                _ => req.call(),
            }
        } else {
            req.call()
        }
        .map_err(|e| Fail::Failed(format!("HTTP {} failed: {e}", name.to_uppercase())))?;
        let status = response.status();
        if !(200..300).contains(&status) {
            return Err(Fail::Failed(format!(
                "HTTP status {status}; redirects are not followed"
            )));
        }
        let etag = response.header("ETag").map(str::to_string);
        let content_type = response.header("Content-Type").unwrap_or("").to_string();
        let max = args["max_bytes"].as_u64().unwrap_or(1048576) as usize;
        let mut body = response.into_reader();
        let mut data = Vec::new();
        let mut chunk = [0; 8192];
        loop {
            if Instant::now() >= deadline {
                return Err(Fail::Exhausted("HTTP deadline".into()));
            }
            let n = body
                .read(&mut chunk)
                .map_err(|e| Fail::Failed(format!("HTTP body read failed: {e}")))?;
            if n == 0 {
                break;
            }
            if data.len() + n > max {
                return Err(Fail::Exhausted(
                    "HTTP body exceeds max_bytes; no partial text returned".into(),
                ));
            }
            data.extend_from_slice(&chunk[..n]);
        }
        let hash = crate::util::sha256_hex(&data);
        let bytes = data.len();
        let text = String::from_utf8(data)
            .map_err(|_| Fail::Invalid("HTTP body is not valid UTF-8".into()))?;
        if name == "get_json" {
            let value: Value = serde_json::from_str(&text)
                .map_err(|e| Fail::Invalid(format!("invalid JSON from `{}`: {e}", u.as_str())))?;
            if let Some(schema) = args.get("schema") {
                crate::schema::check(schema, &value, "parsed").map_err(Fail::Invalid)?;
            }
            Ok(json!({"url":u.as_str(),"status":status,"content_type":content_type,"value":value,"bytes":bytes,"sha256":hash}))
        } else if is_write {
            let parsed_json: Value = serde_json::from_str(&text).unwrap_or(Value::Null);
            Ok(json!({
                "url": u.as_str(),
                "status": status,
                "etag": etag,
                "content_type": content_type,
                "text": text,
                "json": parsed_json,
                "bytes": bytes,
                "sha256": hash,
            }))
        } else {
            Ok(
                json!({"url":u.as_str(),"status":status,"content_type":content_type,"text":text,"bytes":bytes,"sha256":hash}),
            )
        }
    }
}

pub fn target_digest(cfg: Option<&crate::util::Config>, args: &Value) -> Result<Option<String>, Fail> {
    let u = request_url(args)?;
    check_egress(cfg, &u)?;
    let agent = ureq::AgentBuilder::new()
        .try_proxy_from_env(false)
        .redirects(0)
        .timeout(Duration::from_millis(args["timeout_ms"].as_u64().unwrap_or(5000)))
        .build();
    let mut req = agent.get(u.as_str());
    if let Some(auth_name) = args.get("auth").and_then(Value::as_str) {
        if let Some(entry) = cfg.and_then(|c| c.http_auth.get(auth_name)) {
            if entry.origin == u.origin().ascii_serialization() {
                if let Ok(token) = std::env::var(&entry.token_env) {
                    if !token.trim().is_empty() {
                        req = req.set(&entry.header, &format!("{}{}", entry.prefix, token));
                    }
                }
            }
        }
    }
    match req.call() {
        Ok(resp) => {
            let status = resp.status();
            let etag = resp.header("ETag").unwrap_or("").to_string();
            let body = resp.into_reader();
            let mut data = Vec::new();
            let _ = body.take(65536).read_to_end(&mut data);
            let mut buf = format!("status={status}\netag={etag}\n").into_bytes();
            buf.extend_from_slice(&data);
            Ok(Some(crate::util::sha256_hex(&buf)))
        }
        Err(ureq::Error::Status(404, _)) => Ok(Some("missing".into())),
        Err(e) => Err(Fail::Failed(format!("HTTP digest probe failed: {e}"))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn origins_are_explicit_and_credentials_are_rejected() {
        for origin in [
            "https://example.com",
            "http://127.0.0.1:8000",
            "http://[::1]:8080",
        ] {
            validate_origin(origin).unwrap();
        }
        for origin in [
            "https://example.com/",
            "https://example.com/path",
            "https://example.com?q=1",
            "file:///etc/passwd",
            "https://u:p@example.com",
            "https://example.com#fragment",
            "HTTPS://EXAMPLE.COM",
        ] {
            assert!(validate_origin(origin).is_err(), "{origin}");
        }
    }

    #[test]
    fn egress_policy_blocks_denied_unlisted_and_private_ips() {
        let cfg = crate::util::Config {
            egress: crate::util::EgressConfig {
                allow_hosts: vec!["*.example.com".into(), "169.254.169.254".into()],
                deny_hosts: vec!["bad.example.com".into()],
                block_private_ips: true,
            },
            ..Default::default()
        };
        assert!(check_egress(Some(&cfg), &parse_url("https://api.example.com/v1").unwrap()).is_ok());
        assert!(check_egress(Some(&cfg), &parse_url("https://bad.example.com/v1").unwrap()).is_err());
        assert!(check_egress(Some(&cfg), &parse_url("https://other.org/v1").unwrap()).is_err());
        assert!(check_egress(Some(&cfg), &parse_url("http://169.254.169.254/latest").unwrap()).is_err());
        assert!(check_egress(Some(&cfg), &parse_url("http://127.0.0.1:8080/").unwrap()).is_err());
        assert!(check_egress(Some(&cfg), &parse_url("http://localhost:8080/").unwrap()).is_err());
    }

    #[test]
    fn authenticated_http_write_enforces_origin_pinning_idempotency_and_digest() {
        use std::io::{BufRead, BufReader, Write};
        use std::net::TcpListener;

        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let origin = format!("http://{addr}");
        let url = format!("{origin}/items/1");

        std::thread::spawn(move || {
            for stream in listener.incoming().flatten().take(3) {
                let mut reader = BufReader::new(stream);
                let mut req_line = String::new();
                reader.read_line(&mut req_line).unwrap();
                let mut headers = Vec::new();
                loop {
                    let mut h = String::new();
                    reader.read_line(&mut h).unwrap();
                    if h.trim().is_empty() {
                        break;
                    }
                    headers.push(h);
                }
                let out = reader.get_mut();
                if req_line.starts_with("GET ") {
                    let body = b"{\"v\":1}";
                    let _ = write!(
                        out,
                        "HTTP/1.1 200 OK\r\nETag: \"v1\"\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                        body.len()
                    );
                    let _ = out.write_all(body);
                } else {
                    let has_auth = headers.iter().any(|h| h.to_ascii_lowercase().starts_with("authorization: bearer secret-tok"));
                    let has_idem = headers.iter().any(|h| h.to_ascii_lowercase().starts_with("idempotency-key: ik-1"));
                    let body = json!({"ok": has_auth && has_idem}).to_string();
                    let _ = write!(
                        out,
                        "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                        body.len(),
                        body
                    );
                }
                let _ = out.flush();
            }
        });

        std::env::set_var("SUMMONR_TEST_HTTP_TOK", "secret-tok");
        let mut cfg = crate::util::Config::default();
        cfg.http_auth.insert(
            "api".into(),
            crate::util::HttpAuthConfig {
                origin: origin.clone(),
                header: "Authorization".into(),
                prefix: "Bearer ".into(),
                token_env: "SUMMONR_TEST_HTTP_TOK".into(),
            },
        );
        cfg.http_auth.insert(
            "other".into(),
            crate::util::HttpAuthConfig {
                origin: "https://other.example.com".into(),
                header: "Authorization".into(),
                prefix: "Bearer ".into(),
                token_env: "SUMMONR_TEST_HTTP_TOK".into(),
            },
        );
        let grants = vec![reagent::need("http", "put", &[("origin", &origin)], &[])];
        let selected = std::sync::Mutex::new(std::collections::HashSet::new());

        // 1. Mismatched origin on http_auth is refused.
        let auth_base = Authority {
            grants: &grants,
            deadline: None,
            on_item: None,
            selected: &selected,
            expected_digest: None,
            config: Some(&cfg),
        };
        let err = reagent::call(
            None,
            "http",
            "put",
            &json!({"url": url, "auth": "other", "body": {"x": 1}}),
            &auth_base,
        )
        .unwrap_err();
        assert!(matches!(err, Fail::Denied(_)), "{err:?}");

        // 2. Digest precondition + auth + idempotency key succeed.
        let digest = target_digest(Some(&cfg), &json!({"url": url, "auth": "api"}))
            .unwrap()
            .unwrap();
        let auth_dig = Authority {
            expected_digest: Some(&digest),
            ..auth_base
        };
        let res = reagent::call(
            None,
            "http",
            "put",
            &json!({"url": url, "auth": "api", "idempotency_key": "ik-1", "body": {"x": 1}}),
            &auth_dig,
        )
        .unwrap();
        assert_eq!(res["status"], 200);
        assert_eq!(res["json"]["ok"], true);
    }
}
