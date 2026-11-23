//! Model backends.
//! - `ollama/<model>` talks to a local Ollama server (`/api/chat`).
//! - `openai/<model>` talks to any OpenAI-compatible server (`/v1/chat/completions`,
//!   including OpenAI, OpenRouter, vLLM, llama-server, LM Studio).
//! - `anthropic/<model>` talks to an Anthropic Messages server (`/v1/messages`).
//! - Named providers in `config.yaml` (`providers.<name>`) map `<name>/<model>` to
//!   any of the three protocol families (`kind: openai | anthropic | ollama`) with
//!   a custom `url` and optional `api_key_env`.
//! - `mock/keyword` is a deterministic stand-in used by tests and dry demos; it
//!   is labeled as a mock everywhere it appears.

use crate::util::ProviderConfig;
use base64::Engine;
use serde_json::{json, Value};
use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::sync::{Mutex, OnceLock, RwLock};
use std::time::{Duration, Instant};

/// One image carried to a vision-capable model.
#[derive(Clone)]
pub struct ImagePart {
    pub sha256: String,
    pub mime: String,
    pub bytes: Vec<u8>,
}

impl std::fmt::Debug for ImagePart {
    /// Never prints the bytes.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ImagePart")
            .field("sha256", &self.sha256)
            .field("mime", &self.mime)
            .field("len", &self.bytes.len())
            .finish()
    }
}

/// `Inline` sends the bytes; `Placeholder` is the record form (digest only).
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum ImageMode {
    Inline,
    Placeholder,
}

fn b64(bytes: &[u8]) -> String {
    base64::engine::general_purpose::STANDARD.encode(bytes)
}

/// Error text for a request that carried images: no image base64 may
/// survive (servers often echo the input) and the text stays bounded.
fn scrub_error(err: String, images: &[ImagePart]) -> String {
    if images.is_empty() {
        return err;
    }
    let mut out = err;
    // Exact copies become sentinels first so the digest marker survives the
    // run elision below (a digest is itself 64 alphabet characters).
    for (n, i) in images.iter().enumerate() {
        out = out.replace(&b64(&i.bytes), &format!("\u{1}{n}\u{1}"));
    }
    // Truncated, `\/`-escaped or line-wrapped copies: elide any long run of
    // base64-looking characters.
    out = elide_base64_runs(&out);
    for (n, i) in images.iter().enumerate() {
        out = out.replace(&format!("\u{1}{n}\u{1}"), &format!("[sight:{}]", i.sha256));
    }
    match out.char_indices().nth(2000) {
        Some((at, _)) => {
            out.truncate(at);
            out.push_str("...[truncated]");
            out
        }
        None => out,
    }
}

/// Replace every run of 64 or more base64-alphabet characters (plus the
/// escape and line-break characters a server may add) with a marker.
fn elide_base64_runs(s: &str) -> String {
    const MARK: &str = "[sight:redacted]";
    let is_b64 = |c: char| c.is_ascii_alphanumeric() || matches!(c, '\\' | '/' | '+' | '=' | '\n' | '\r');
    let mut out = String::with_capacity(s.len());
    let mut run = String::new();
    let flush = |run: &mut String, out: &mut String| {
        if run.chars().count() >= 64 {
            out.push_str(MARK);
        } else {
            out.push_str(run);
        }
        run.clear();
    };
    for c in s.chars() {
        if is_b64(c) {
            run.push(c);
        } else {
            flush(&mut run, &mut out);
            out.push(c);
        }
    }
    flush(&mut run, &mut out);
    out
}

fn placeholder(img: &ImagePart) -> Value {
    json!({"sight": {"sha256": img.sha256, "mime": img.mime, "bytes": img.bytes.len()}})
}

pub struct ChatReq<'a> {
    pub model: &'a str,
    pub system: &'a str,
    pub user: &'a str,
    pub schema: Option<&'a Value>,
    pub settings: &'a BTreeMap<String, Value>,
    /// Remaining time across this summon and all ancestors.
    pub timeout: Option<Duration>,
    /// Remaining total-token budget; generation must never exceed this.
    /// Prompt tokens are reported afterward, so this is not an exact cap on
    /// combined prompt + generation usage.
    pub max_output_tokens: Option<u64>,
    /// Images attached to the user turn, sent before the text.
    pub images: &'a [ImagePart],
}

pub struct ChatResp {
    pub content: String,
    pub done_reason: Option<String>,
    pub prompt_tokens: u64,
    pub output_tokens: u64,
    pub load_ms: u64,
    pub prompt_ms: u64,
    pub eval_ms: u64,
    pub total_ms: u64,
    /// The exact body sent to the backend.
    pub payload: Value,
    /// Whether the backend's decoder enforced the output schema.
    pub constrained: bool,
    /// `exact` when the backend reported token counts, `estimated` otherwise.
    pub token_confidence: &'static str,
    /// Optional provider-reported USD cost converted to microdollars (e.g., OpenRouter `usage.cost`).
    pub provider_cost_microusd: Option<u64>,
}

fn provider_registry() -> &'static RwLock<BTreeMap<String, ProviderConfig>> {
    static REGISTRY: OnceLock<RwLock<BTreeMap<String, ProviderConfig>>> = OnceLock::new();
    REGISTRY.get_or_init(|| RwLock::new(BTreeMap::new()))
}

/// Register custom providers loaded from `config.yaml`.
pub fn register_providers(providers: &BTreeMap<String, ProviderConfig>) {
    if let Ok(mut guard) = provider_registry().write() {
        *guard = providers.clone();
    }
}

fn lookup_provider(name: &str) -> Option<ProviderConfig> {
    provider_registry()
        .read()
        .ok()
        .and_then(|g| g.get(name).cloned())
}

/// Resolve the protocol family (`ollama`, `openai`, `anthropic`, `mock`) and
/// optional API key environment variable for a backend prefix.
fn resolve_protocol(backend: &str) -> Result<(&'static str, Option<String>), String> {
    if let Some(p) = lookup_provider(backend) {
        let kind = match p.kind.as_str() {
            "ollama" => "ollama",
            "openai" => "openai",
            "anthropic" => "anthropic",
            other => {
                return Err(format!(
                    "provider `{backend}` has unsupported kind `{other}`; use openai, anthropic, or ollama"
                ))
            }
        };
        let key = p
            .api_key_env
            .as_deref()
            .and_then(|env_name| std::env::var(env_name).ok())
            .filter(|s| !s.trim().is_empty());
        return Ok((kind, key));
    }
    match backend {
        "ollama" => Ok(("ollama", None)),
        "openai" => {
            let key = std::env::var("SUMMONR_OPENAI_API_KEY")
                .ok()
                .or_else(|| std::env::var("OPENAI_API_KEY").ok())
                .filter(|s| !s.trim().is_empty());
            Ok(("openai", key))
        }
        "anthropic" => {
            let key = std::env::var("SUMMONR_ANTHROPIC_API_KEY")
                .ok()
                .or_else(|| std::env::var("ANTHROPIC_API_KEY").ok())
                .filter(|s| !s.trim().is_empty());
            Ok(("anthropic", key))
        }
        "mock" => Ok(("mock", None)),
        other => Err(format!(
            "backend `{other}` is not supported; use ollama/, openai/, anthropic/, mock/, or a provider configured in config.yaml"
        )),
    }
}

pub fn split_model(id: &str) -> Result<(&str, &str), String> {
    match id.split_once('/') {
        Some((b, m)) if !b.is_empty() && !m.is_empty() => Ok((b, m)),
        _ => Err(format!(
            "model `{id}` must be written backend/model, such as ollama/qwen3 or openai/gpt-4o-mini"
        )),
    }
}

fn agent(timeout: Option<Duration>) -> ureq::Agent {
    let timeout = timeout.unwrap_or(Duration::from_secs(1800));
    ureq::AgentBuilder::new()
        .timeout_connect(timeout.min(Duration::from_secs(5)))
        .timeout(timeout)
        .build()
}

fn http(method: &str, url: &str, body: Option<&Value>) -> Result<Value, String> {
    http_with_headers(method, url, body, &[], None)
}

fn http_with_timeout(
    method: &str,
    url: &str,
    body: Option<&Value>,
    timeout: Option<Duration>,
) -> Result<Value, String> {
    http_with_headers(method, url, body, &[], timeout)
}

/// The message a request returns when its run was cancelled while waiting.
pub(crate) const CANCELLED: &str = "the run was cancelled while the backend was answering";

fn http_with_headers(
    method: &str,
    url: &str,
    body: Option<&Value>,
    headers: &[(&str, &str)],
    timeout: Option<Duration>,
) -> Result<Value, String> {
    let Some(cancel) = crate::cancel::current() else {
        return http_blocking(method, url, body, headers, timeout);
    };
    // ureq 2 cannot abort a request in progress, so it runs on a helper
    // thread and this one returns as soon as the run is cancelled. The
    // abandoned request keeps its connection until the backend answers or
    // the timeout ends it (a server may go on generating until then).
    let (tx, rx) = std::sync::mpsc::channel();
    let (method, url) = (method.to_string(), url.to_string());
    let body = body.cloned();
    let headers: Vec<(String, String)> = headers.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect();
    std::thread::spawn(move || {
        let headers: Vec<(&str, &str)> = headers.iter().map(|(k, v)| (k.as_str(), v.as_str())).collect();
        let _ = tx.send(http_blocking(&method, &url, body.as_ref(), &headers, timeout));
    });
    loop {
        if cancel.load(std::sync::atomic::Ordering::SeqCst) {
            return Err(CANCELLED.into());
        }
        match rx.recv_timeout(Duration::from_millis(100)) {
            Ok(result) => return result,
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => continue,
            Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => {
                return Err("backend request thread ended without an answer".into())
            }
        }
    }
}

fn http_blocking(
    method: &str,
    url: &str,
    body: Option<&Value>,
    headers: &[(&str, &str)],
    timeout: Option<Duration>,
) -> Result<Value, String> {
    let mut req = agent(timeout).request(method, url);
    for (k, v) in headers {
        req = req.set(k, v);
    }
    let res = match body {
        Some(b) => req.send_json(b),
        None => req.call(),
    };
    match res {
        Ok(r) => r
            .into_json::<Value>()
            .map_err(|e| format!("backend sent unreadable JSON: {e}")),
        Err(ureq::Error::Status(code, r)) => Err(format!(
            "backend returned {code}: {}",
            r.into_string().unwrap_or_default().trim()
        )),
        Err(e) => Err(format!("backend unreachable at {url}: {e}")),
    }
}

fn v1_endpoint(base: &str, path: &str) -> String {
    let base = base.trim_end_matches('/');
    let path = path.trim_start_matches('/');
    if base.ends_with("/v1") {
        format!("{base}/{path}")
    } else {
        format!("{base}/v1/{path}")
    }
}

pub fn chat(base_url: &str, model_id: &str, req: ChatReq) -> Result<ChatResp, String> {
    let (backend, model) = split_model(model_id)?;
    let (kind, api_key) = resolve_protocol(backend)?;
    match kind {
        "ollama" => ollama_chat(base_url, model, req),
        "openai" => openai_chat(base_url, model, api_key.as_deref(), req),
        "anthropic" => anthropic_chat(base_url, model, api_key.as_deref(), req),
        "mock" => Ok(mock_chat(req)),
        _ => unreachable!(),
    }
}

/// Build the exact outgoing request payload for `model_id` before dispatch.
pub fn payload(model_id: &str, req: &ChatReq) -> Result<Value, String> {
    let (backend, model) = split_model(model_id)?;
    let (kind, _) = resolve_protocol(backend)?;
    match kind {
        "ollama" => ollama_payload(model, req, ImageMode::Placeholder),
        "openai" => openai_payload(model, req, ImageMode::Placeholder),
        "anthropic" => anthropic_payload(model, req, ImageMode::Placeholder),
        "mock" => {
            let mut p = json!({
                "backend": "mock",
                "model": model_id,
                "system": req.system,
                "user": req.user
            });
            if !req.images.is_empty() {
                p["images"] = json!(req.images.iter().map(|i| &i.sha256).collect::<Vec<_>>());
            }
            Ok(p)
        }
        _ => unreachable!(),
    }
}

/// Whether `model_id`'s backend can take images. Ollama is asked once per
/// `(url, model)` and must list the `vision` capability.
pub fn supports_images(base_url: &str, model_id: &str, timeout: Option<Duration>) -> Result<(), String> {
    let (backend, model) = split_model(model_id)?;
    let (kind, _) = resolve_protocol(backend)?;
    if kind != "ollama" {
        return Ok(());
    }
    static CACHE: OnceLock<Mutex<HashMap<(String, String), bool>>> = OnceLock::new();
    let cache = CACHE.get_or_init(|| Mutex::new(HashMap::new()));
    let key = (base_url.to_string(), model.to_string());
    let known = cache.lock().ok().and_then(|c| c.get(&key).copied());
    let vision = match known {
        Some(v) => v,
        None => {
            let r = http_with_timeout(
                "POST",
                &format!("{base_url}/api/show"),
                Some(&json!({"model": model})),
                timeout,
            )?;
            let v = r["capabilities"]
                .as_array()
                .is_some_and(|c| c.iter().any(|x| x.as_str() == Some("vision")));
            if let Ok(mut c) = cache.lock() {
                c.insert(key, v);
            }
            v
        }
    };
    if vision {
        Ok(())
    } else {
        Err(format!("model `{model_id}` does not support images"))
    }
}

fn ollama_chat(base: &str, model: &str, req: ChatReq) -> Result<ChatResp, String> {
    let timeout = req.timeout;
    let body = ollama_payload(model, &req, ImageMode::Inline)?;
    let recorded = if req.images.is_empty() {
        body.clone()
    } else {
        ollama_payload(model, &req, ImageMode::Placeholder)?
    };
    let started = Instant::now();
    let r = http_with_timeout("POST", &format!("{base}/api/chat"), Some(&body), timeout)
        .map_err(|e| scrub_error(e, req.images))?;
    let ns = |k: &str| r.get(k).and_then(Value::as_u64).unwrap_or(0) / 1_000_000;
    let reported = r.get("prompt_eval_count").and_then(Value::as_u64).is_some()
        && r.get("eval_count").and_then(Value::as_u64).is_some();
    Ok(ChatResp {
        content: r["message"]["content"]
            .as_str()
            .ok_or("backend response has no text content")?
            .to_string(),
        done_reason: r["done_reason"].as_str().map(String::from),
        prompt_tokens: r
            .get("prompt_eval_count")
            .and_then(Value::as_u64)
            .unwrap_or(0),
        output_tokens: r.get("eval_count").and_then(Value::as_u64).unwrap_or(0),
        load_ms: ns("load_duration"),
        prompt_ms: ns("prompt_eval_duration"),
        eval_ms: ns("eval_duration"),
        total_ms: started.elapsed().as_millis() as u64,
        payload: recorded,
        constrained: req.schema.is_some(),
        token_confidence: if reported { "exact" } else { "unknown" },
        provider_cost_microusd: None,
    })
}

fn parse_provider_cost_microusd(v: &Value) -> Option<u64> {
    let usd = v.as_f64()?;
    if !usd.is_finite() || usd < 0.0 {
        return None;
    }
    let micro = (usd * 1_000_000.0).ceil();
    if micro > u64::MAX as f64 {
        None
    } else {
        Some(micro as u64)
    }
}

fn openai_chat(
    base: &str,
    model: &str,
    api_key: Option<&str>,
    req: ChatReq,
) -> Result<ChatResp, String> {
    let timeout = req.timeout;
    let body = openai_payload(model, &req, ImageMode::Inline)?;
    let recorded = if req.images.is_empty() {
        body.clone()
    } else {
        openai_payload(model, &req, ImageMode::Placeholder)?
    };
    let started = Instant::now();
    let auth_val = api_key.map(|k| format!("Bearer {k}"));
    let mut headers = Vec::new();
    if let Some(ref a) = auth_val {
        headers.push(("Authorization", a.as_str()));
    }
    let url = v1_endpoint(base, "chat/completions");
    let r = http_with_headers("POST", &url, Some(&body), &headers, timeout)
        .map_err(|e| scrub_error(e, req.images))?;
    let choice = r["choices"]
        .as_array()
        .and_then(|c| c.first())
        .ok_or("backend response has no choices")?;
    let content = choice["message"]["content"]
        .as_str()
        .ok_or("backend response has no text content")?
        .to_string();
    let done_reason = choice["finish_reason"].as_str().map(String::from);
    let prompt_tokens = r["usage"]["prompt_tokens"].as_u64();
    let output_tokens = r["usage"]["completion_tokens"].as_u64();
    let reported = prompt_tokens.is_some() && output_tokens.is_some();
    let provider_cost_microusd = parse_provider_cost_microusd(&r["usage"]["cost"]);
    Ok(ChatResp {
        content,
        done_reason,
        prompt_tokens: prompt_tokens.unwrap_or(0),
        output_tokens: output_tokens.unwrap_or(0),
        load_ms: 0,
        prompt_ms: 0,
        eval_ms: 0,
        total_ms: started.elapsed().as_millis() as u64,
        payload: recorded,
        constrained: req.schema.is_some(),
        token_confidence: if reported { "exact" } else { "unknown" },
        provider_cost_microusd,
    })
}

fn anthropic_chat(
    base: &str,
    model: &str,
    api_key: Option<&str>,
    req: ChatReq,
) -> Result<ChatResp, String> {
    let timeout = req.timeout;
    let body = anthropic_payload(model, &req, ImageMode::Inline)?;
    let recorded = if req.images.is_empty() {
        body.clone()
    } else {
        anthropic_payload(model, &req, ImageMode::Placeholder)?
    };
    let started = Instant::now();
    let mut headers = vec![("anthropic-version", "2023-06-01")];
    if let Some(k) = api_key {
        headers.push(("x-api-key", k));
    }
    let url = v1_endpoint(base, "messages");
    let r = http_with_headers("POST", &url, Some(&body), &headers, timeout)
        .map_err(|e| scrub_error(e, req.images))?;
    let content = r["content"]
        .as_array()
        .and_then(|blocks| {
            blocks
                .iter()
                .find_map(|b| b.get("text").and_then(Value::as_str))
        })
        .ok_or("backend response has no text content")?
        .to_string();
    let done_reason = r["stop_reason"].as_str().map(|s| {
        if s == "max_tokens" {
            "length".to_string()
        } else {
            s.to_string()
        }
    });
    let prompt_tokens = r["usage"]["input_tokens"].as_u64();
    let output_tokens = r["usage"]["output_tokens"].as_u64();
    let reported = prompt_tokens.is_some() && output_tokens.is_some();
    Ok(ChatResp {
        content,
        done_reason,
        prompt_tokens: prompt_tokens.unwrap_or(0),
        output_tokens: output_tokens.unwrap_or(0),
        load_ms: 0,
        prompt_ms: 0,
        eval_ms: 0,
        total_ms: started.elapsed().as_millis() as u64,
        payload: recorded,
        // Anthropic Messages API without forced tool-use checks schema afterward.
        constrained: false,
        token_confidence: if reported { "exact" } else { "unknown" },
        provider_cost_microusd: None,
    })
}

pub fn validate_settings(settings: &BTreeMap<String, Value>) -> Result<(), String> {
    crate::schema::check(
        &json!({"type":"object","additionalProperties":false,"properties":{
            "think":{"type":"boolean"},"temperature":{"type":"number","minimum":0},
            "seed":{"type":"integer"},"num_predict":{"type":"integer","minimum":-2},
            "max_tokens":{"type":"integer","minimum":1},
            "num_ctx":{"type":"integer","minimum":1},"top_k":{"type":"integer","minimum":0},
            "top_p":{"type":"number","minimum":0,"maximum":1}
        }}),
        &serde_json::to_value(settings).map_err(|e| e.to_string())?,
        "settings",
    )
}

pub(crate) fn resolve_generation_cap(
    settings: &BTreeMap<String, Value>,
    max_output_tokens: Option<u64>,
) -> Result<Option<u64>, String> {
    let explicit = match (settings.get("max_tokens"), settings.get("num_predict")) {
        (Some(v), _) => {
            let n = v.as_i64().ok_or("max_tokens must be an integer")?;
            (n >= 1).then_some(n as u64)
        }
        (None, Some(v)) => {
            let n = v.as_i64().ok_or("num_predict must be an integer")?;
            (n >= 0).then_some(n as u64)
        }
        (None, None) => None,
    };
    match (max_output_tokens, explicit) {
        (Some(0), _) | (_, Some(0)) => Err("no token essence remains for generation".into()),
        (Some(rem), Some(exp)) => Ok(Some(rem.min(exp).min(i32::MAX as u64))),
        (Some(rem), None) => Ok(Some(rem.min(i32::MAX as u64))),
        (None, Some(exp)) => Ok(Some(exp.min(i32::MAX as u64))),
        (None, None) => Ok(None),
    }
}

/// `think: false` asks for what a backend without a thinking switch already
/// does, so it is accepted and sent nowhere; `think: true` is refused rather
/// than silently dropped.
fn thinking_off(backend: &str, v: &Value) -> Result<(), String> {
    if v.as_bool() == Some(false) {
        Ok(())
    } else {
        Err(format!(
            "setting `think: true` is not supported by the {backend} backend; only the ollama backend forwards `think`"
        ))
    }
}

pub(crate) fn ollama_payload(model: &str, req: &ChatReq, mode: ImageMode) -> Result<Value, String> {
    validate_settings(req.settings)?;
    let mut options = serde_json::Map::new();
    let mut think = Value::Bool(false);
    for (k, v) in req.settings {
        match k.as_str() {
            "think" => think = v.clone(),
            "temperature" | "seed" | "num_predict" | "num_ctx" | "top_p" | "top_k" => {
                options.insert(k.clone(), v.clone());
            }
            "max_tokens" => {
                options.insert("num_predict".into(), v.clone());
            }
            other => {
                return Err(format!(
                    "setting `{other}` is not supported by the ollama backend"
                ))
            }
        }
    }
    if let Some(capped) = resolve_generation_cap(req.settings, req.max_output_tokens)? {
        options.insert("num_predict".into(), json!(capped));
    }
    let mut messages = Vec::new();
    if !req.system.is_empty() {
        messages.push(json!({"role": "system", "content": req.system}));
    }
    let mut user = json!({"role": "user", "content": req.user});
    if !req.images.is_empty() {
        user["images"] = Value::Array(
            req.images
                .iter()
                .map(|i| match mode {
                    ImageMode::Inline => json!(b64(&i.bytes)),
                    ImageMode::Placeholder => placeholder(i),
                })
                .collect(),
        );
    }
    messages.push(user);
    let mut body = json!({
        "model": model,
        "messages": messages,
        "stream": false,
        "think": think,
        "options": options,
        "keep_alive": "10m",
    });
    if let Some(s) = req.schema {
        body["format"] = s.clone();
    }
    Ok(body)
}

pub(crate) fn openai_payload(model: &str, req: &ChatReq, mode: ImageMode) -> Result<Value, String> {
    validate_settings(req.settings)?;
    let mut messages = Vec::new();
    if !req.system.is_empty() {
        messages.push(json!({"role": "system", "content": req.system}));
    }
    let content = if req.images.is_empty() {
        json!(req.user)
    } else {
        let mut parts: Vec<Value> = req
            .images
            .iter()
            .map(|i| match mode {
                ImageMode::Inline => json!({"type": "image_url", "image_url": {
                    "url": format!("data:{};base64,{}", i.mime, b64(&i.bytes))}}),
                ImageMode::Placeholder => placeholder(i),
            })
            .collect();
        parts.push(json!({"type": "text", "text": req.user}));
        Value::Array(parts)
    };
    messages.push(json!({"role": "user", "content": content}));
    let mut body = json!({
        "model": model,
        "messages": messages,
        "stream": false,
    });
    for (k, v) in req.settings {
        match k.as_str() {
            "temperature" | "top_p" | "seed" => {
                body[k.as_str()] = v.clone();
            }
            "max_tokens" | "num_predict" => {}
            "think" => thinking_off("openai", v)?,
            other => {
                return Err(format!(
                    "setting `{other}` is not supported by the openai backend"
                ))
            }
        }
    }
    if let Some(capped) = resolve_generation_cap(req.settings, req.max_output_tokens)? {
        body["max_tokens"] = json!(capped);
    }
    if let Some(s) = req.schema {
        body["response_format"] = json!({
            "type": "json_schema",
            "json_schema": {
                "name": "summonr_output",
                "strict": true,
                "schema": s,
            }
        });
    }
    Ok(body)
}

pub(crate) fn anthropic_payload(
    model: &str,
    req: &ChatReq,
    mode: ImageMode,
) -> Result<Value, String> {
    validate_settings(req.settings)?;
    let capped = resolve_generation_cap(req.settings, req.max_output_tokens)?.unwrap_or(4096);
    let content = if req.images.is_empty() {
        json!(req.user)
    } else {
        let mut parts: Vec<Value> = req
            .images
            .iter()
            .map(|i| match mode {
                ImageMode::Inline => json!({"type": "image", "source": {
                    "type": "base64", "media_type": i.mime, "data": b64(&i.bytes)}}),
                ImageMode::Placeholder => placeholder(i),
            })
            .collect();
        parts.push(json!({"type": "text", "text": req.user}));
        Value::Array(parts)
    };
    let mut body = json!({
        "model": model,
        "max_tokens": capped,
        "messages": [{"role": "user", "content": content}],
        "stream": false,
    });
    if !req.system.is_empty() {
        body["system"] = json!(req.system);
    }
    for (k, v) in req.settings {
        match k.as_str() {
            "temperature" | "top_p" | "top_k" => {
                body[k.as_str()] = v.clone();
            }
            "max_tokens" | "num_predict" => {}
            "think" => thinking_off("anthropic", v)?,
            other => {
                return Err(format!(
                    "setting `{other}` is not supported by the anthropic backend"
                ))
            }
        }
    }
    Ok(body)
}

/// Ask the backend to unload a model. Returns a short description of what
/// happened for the elegy.
pub fn unload(base_url: &str, model_id: &str) -> String {
    let Ok((backend, model)) = split_model(model_id) else {
        return "not applicable".into();
    };
    match resolve_protocol(backend) {
        Ok(("ollama", _)) => {
            let body = json!({"model": model, "keep_alive": 0});
            match http("POST", &format!("{base_url}/api/generate"), Some(&body)) {
                Ok(_) => "unload requested".into(),
                Err(e) => format!("unload failed: {e}"),
            }
        }
        Ok(("openai" | "anthropic", _)) => "not applicable (stateless HTTP backend)".into(),
        Ok(("mock", _)) => "not applicable (mock backend)".into(),
        _ => "not applicable".into(),
    }
}

/// Whatever identifies the model the backend would actually run. Used to
/// notice that a ritual's baseline no longer describes its backend.
pub fn fingerprint(base_url: &str, model_id: &str) -> Value {
    let Ok((backend, model)) = split_model(model_id) else {
        return json!({"backend": "unknown", "model": model_id, "digest": "unknown"});
    };
    match resolve_protocol(backend) {
        Ok(("ollama", _)) => {
            let tags = http("GET", &format!("{base_url}/api/tags"), None).ok();
            let version = http("GET", &format!("{base_url}/api/version"), None)
                .ok()
                .and_then(|v| v["version"].as_str().map(String::from));
            ollama_print(backend, model, tags.as_ref(), version.as_deref())
        }
        Ok(("openai", api_key)) => {
            let models = fetch_v1_models(base_url, api_key.as_deref(), false);
            openai_print(backend, model, models.as_ref())
        }
        Ok(("anthropic", api_key)) => {
            let models = fetch_v1_models(base_url, api_key.as_deref(), true);
            openai_print(backend, model, models.as_ref())
        }
        Ok(("mock", _)) => json!({"backend": "mock", "model": model, "digest": "mock"}),
        _ => json!({"backend": "unknown", "model": model_id, "digest": "unknown"}),
    }
}

fn fetch_v1_models(base_url: &str, api_key: Option<&str>, anthropic: bool) -> Option<Value> {
    let url = v1_endpoint(base_url, "models");
    let auth_val = api_key.map(|k| format!("Bearer {k}"));
    let mut headers = Vec::new();
    if anthropic {
        headers.push(("anthropic-version", "2023-06-01"));
        if let Some(k) = api_key {
            headers.push(("x-api-key", k));
        }
    } else if let Some(ref a) = auth_val {
        headers.push(("Authorization", a.as_str()));
    }
    http_with_headers("GET", &url, None, &headers, Some(Duration::from_secs(5))).ok()
}

/// Fingerprints for several models, asking an Ollama or `/v1/models` server
/// once per endpoint rather than once per model.
#[cfg_attr(not(test), allow(dead_code))]
pub fn fingerprints(
    base_url: &str,
    sites: &BTreeSet<String>,
) -> serde_json::Map<String, Value> {
    fingerprints_with_resolver(sites, |_| base_url.to_string())
}

/// Fingerprints for several models using each site's configured endpoint URL.
pub fn fingerprints_for_cfg(
    cfg: &crate::util::Config,
    sites: &BTreeSet<String>,
) -> serde_json::Map<String, Value> {
    fingerprints_with_resolver(sites, |m| cfg.backend_url_for(m))
}

fn fingerprints_with_resolver(
    sites: &BTreeSet<String>,
    resolve_url: impl Fn(&str) -> String,
) -> serde_json::Map<String, Value> {
    let mut ollama_cache: BTreeMap<String, (Option<Value>, Option<String>)> = BTreeMap::new();
    let mut v1_cache: BTreeMap<(String, String), Option<Value>> = BTreeMap::new();
    sites
        .iter()
        .map(|m| {
            let url = resolve_url(m);
            let print = match split_model(m).and_then(|(b, model)| Ok((b, model, resolve_protocol(b)?))) {
                Ok((b, model, ("ollama", _))) => {
                    let (tags, version) = ollama_cache.entry(url.clone()).or_insert_with(|| {
                        (
                            http("GET", &format!("{url}/api/tags"), None).ok(),
                            http("GET", &format!("{url}/api/version"), None)
                                .ok()
                                .and_then(|v| v["version"].as_str().map(String::from)),
                        )
                    });
                    ollama_print(b, model, tags.as_ref(), version.as_deref())
                }
                Ok((b, model, ("openai", api_key))) => {
                    let models = v1_cache
                        .entry((b.to_string(), url.clone()))
                        .or_insert_with(|| fetch_v1_models(&url, api_key.as_deref(), false));
                    openai_print(b, model, models.as_ref())
                }
                Ok((b, model, ("anthropic", api_key))) => {
                    let models = v1_cache
                        .entry((b.to_string(), url.clone()))
                        .or_insert_with(|| fetch_v1_models(&url, api_key.as_deref(), true));
                    openai_print(b, model, models.as_ref())
                }
                _ => fingerprint(&url, m),
            };
            (m.clone(), print)
        })
        .collect()
}

fn ollama_print(backend: &str, model: &str, tags: Option<&Value>, version: Option<&str>) -> Value {
    let entry = tags.and_then(|t| {
        t["models"].as_array().and_then(|ms| {
            ms.iter()
                .find(|m| {
                    let n = m["name"].as_str().unwrap_or("");
                    n == model || n == format!("{model}:latest")
                })
                .cloned()
        })
    });
    match entry {
        Some(m) => json!({
            "backend": backend,
            "model": model,
            "digest": m["digest"],
            "quantization": m["details"]["quantization_level"],
            "parameter_size": m["details"]["parameter_size"],
            "server_version": version,
        }),
        None => json!({"backend": backend, "model": model, "digest": "unknown", "server_version": version}),
    }
}

fn openai_print(backend: &str, model: &str, list: Option<&Value>) -> Value {
    let entry = list.and_then(|l| {
        l["data"].as_array().and_then(|ms| {
            ms.iter()
                .find(|m| m["id"].as_str() == Some(model))
                .cloned()
        })
    });
    match entry {
        Some(m) => json!({
            "backend": backend,
            "model": model,
            "digest": m.get("id").cloned().unwrap_or(json!(model)),
            "owned_by": m.get("owned_by").cloned().unwrap_or(Value::Null),
            "created": m.get("created").cloned().unwrap_or(Value::Null),
        }),
        None => json!({"backend": backend, "model": model, "digest": "unknown"}),
    }
}

/// Deterministic stand-in: for each enumerated field, choose the first option
/// whose name appears in the prompt, else `abstain` when that is an option.
fn mock_chat(req: ChatReq) -> ChatResp {
    let text = req.user.to_lowercase();
    let content = match req
        .schema
        .and_then(|s| s.get("properties"))
        .and_then(Value::as_object)
    {
        Some(props) => {
            let mut out = serde_json::Map::new();
            for (name, spec) in props {
                let v = match spec.get("enum").and_then(Value::as_array) {
                    Some(options) => options
                        .iter()
                        .find(|o| {
                            let w = o.as_str().unwrap_or("").to_lowercase();
                            let stem = w.trim_end_matches('s');
                            !stem.is_empty() && w != "abstain" && text.contains(stem)
                        })
                        .or_else(|| options.iter().find(|o| o.as_str() == Some("abstain")))
                        .or_else(|| options.first())
                        .cloned()
                        .unwrap_or(Value::Null),
                    None => match spec.get("type").and_then(Value::as_str) {
                        Some("boolean") => json!(false),
                        Some("integer") | Some("number") => json!(0),
                        _ => json!("mock"),
                    },
                };
                out.insert(name.clone(), v);
            }
            Value::Object(out).to_string()
        }
        None => "mock".to_string(),
    };
    let estimate = |s: &str| (s.len() as u64) / 4 + 1;
    let mut payload = json!({"backend": "mock", "model": req.model, "system": req.system, "user": req.user, "format": req.schema, "options": req.settings});
    if !req.images.is_empty() {
        payload["images"] = json!(req.images.iter().map(|i| &i.sha256).collect::<Vec<_>>());
    }
    ChatResp {
        done_reason: Some("stop".into()),
        prompt_tokens: estimate(req.system) + estimate(req.user) + 256 * req.images.len() as u64,
        output_tokens: estimate(&content),
        content,
        load_ms: 0,
        prompt_ms: 0,
        eval_ms: 0,
        total_ms: 0,
        payload,
        constrained: false,
        token_confidence: "estimated",
        provider_cost_microusd: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn generation_cap_respects_remaining_essence_and_explicit_smaller_limits() {
        for (requested, expected) in [(None, 7), (Some(-1), 7), (Some(100), 7), (Some(3), 3)] {
            let settings = requested
                .map(|n| BTreeMap::from([("num_predict".into(), json!(n))]))
                .unwrap_or_default();
            let req = ChatReq {
                model: "test",
                system: "",
                user: "hello",
                schema: None,
                settings: &settings,
                timeout: Some(Duration::from_millis(10)),
                max_output_tokens: Some(7),
                images: &[],
            };
            let body = ollama_payload("test", &req, ImageMode::Inline).unwrap();
            assert_eq!(body["options"]["num_predict"], expected);
            let oai = openai_payload("test", &req, ImageMode::Inline).unwrap();
            assert_eq!(oai["max_tokens"], expected);
            let ant = anthropic_payload("test", &req, ImageMode::Inline).unwrap();
            assert_eq!(ant["max_tokens"], expected);
        }
    }

    #[test]
    fn think_is_forwarded_to_ollama_ignored_when_false_elsewhere_and_refused_when_true() {
        let req_with = |settings: &BTreeMap<String, Value>, f: &dyn Fn(&ChatReq)| {
            f(&ChatReq {
                model: "m", system: "", user: "hi", schema: None, settings,
                timeout: None, max_output_tokens: None, images: &[],
            })
        };
        let on = BTreeMap::from([("think".to_string(), json!(true))]);
        let off = BTreeMap::from([("think".to_string(), json!(false))]);
        req_with(&on, &|req| {
            assert_eq!(ollama_payload("m", req, ImageMode::Inline).unwrap()["think"], json!(true));
            for e in [openai_payload("m", req, ImageMode::Inline).unwrap_err(),
                      anthropic_payload("m", req, ImageMode::Inline).unwrap_err()] {
                assert!(e.contains("`think: true`") && e.contains("only the ollama backend"), "{e}");
            }
        });
        req_with(&off, &|req| {
            assert_eq!(ollama_payload("m", req, ImageMode::Inline).unwrap()["think"], json!(false));
            let o = openai_payload("m", req, ImageMode::Inline).unwrap();
            let a = anthropic_payload("m", req, ImageMode::Inline).unwrap();
            assert!(o.get("think").is_none() && a.get("think").is_none(), "nothing to turn off");
        });
    }

    #[test]
    fn exhausted_generation_is_rejected_before_dispatch() {
        let settings = BTreeMap::new();
        let req = ChatReq {
            model: "test",
            system: "",
            user: "hello",
            schema: None,
            settings: &settings,
            timeout: None,
            max_output_tokens: Some(0),
            images: &[],
        };
        assert!(ollama_payload("test", &req, ImageMode::Inline)
            .unwrap_err()
            .contains("no token essence"));
        assert!(openai_payload("test", &req, ImageMode::Inline)
            .unwrap_err()
            .contains("no token essence"));
        assert!(anthropic_payload("test", &req, ImageMode::Inline)
            .unwrap_err()
            .contains("no token essence"));
    }

    #[test]
    fn openai_and_anthropic_chat_round_trip_and_enforced_schemas() {
        use std::io::{BufRead, BufReader, Read, Write};
        let server = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}", server.local_addr().unwrap());
        std::thread::spawn(move || {
            for mut s in server.incoming().flatten() {
                let mut line = String::new();
                let mut r = BufReader::new(s.try_clone().unwrap());
                r.read_line(&mut line).unwrap();
                let mut content_len = 0usize;
                loop {
                    let mut h = String::new();
                    if r.read_line(&mut h).unwrap() <= 2 {
                        break;
                    }
                    if let Some((k, v)) = h.split_once(':') {
                        if k.trim().eq_ignore_ascii_case("content-length") {
                            content_len = v.trim().parse().unwrap_or(0);
                        }
                    }
                }
                let mut req_body = vec![0u8; content_len];
                let _ = r.read_exact(&mut req_body);
                let body = if line.contains("/v1/chat/completions") {
                    r#"{"choices":[{"message":{"content":"{\"intent\":\"receipt\"}"},"finish_reason":"stop"}],"usage":{"prompt_tokens":11,"completion_tokens":5}}"#
                } else if line.contains("/v1/messages") {
                    r#"{"content":[{"type":"text","text":"{\"intent\":\"receipt\"}"}],"stop_reason":"end_turn","usage":{"input_tokens":14,"output_tokens":6}}"#
                } else {
                    r#"{"data":[{"id":"gpt-4o-mini","owned_by":"openai","created":123}]}"#
                };
                let _ = write!(
                    s,
                    "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                );
            }
        });

        let schema = json!({"type": "object", "required": ["intent"], "properties": {"intent": {"type": "string"}}});
        let settings = BTreeMap::from([("temperature".into(), json!(0.2))]);
        let req = ChatReq {
            model: "gpt-4o-mini",
            system: "be concise",
            user: "receipt #1",
            schema: Some(&schema),
            settings: &settings,
            timeout: Some(Duration::from_secs(5)),
            max_output_tokens: Some(50),
            images: &[],
        };
        let oai = chat(&url, "openai/gpt-4o-mini", req).unwrap();
        assert_eq!(oai.content, r#"{"intent":"receipt"}"#);
        assert_eq!((oai.prompt_tokens, oai.output_tokens), (11, 5));
        assert_eq!(oai.token_confidence, "exact");
        assert!(oai.constrained);
        assert_eq!(oai.payload["response_format"]["type"], "json_schema");
        assert_eq!(oai.payload["max_tokens"], 50);

        let req_ant = ChatReq {
            model: "claude-sonnet-4",
            system: "be concise",
            user: "receipt #1",
            schema: Some(&schema),
            settings: &settings,
            timeout: Some(Duration::from_secs(5)),
            max_output_tokens: Some(64),
            images: &[],
        };
        let ant = chat(&url, "anthropic/claude-sonnet-4", req_ant).unwrap();
        assert_eq!(ant.content, r#"{"intent":"receipt"}"#);
        assert_eq!((ant.prompt_tokens, ant.output_tokens), (14, 6));
        assert_eq!(ant.token_confidence, "exact");
        assert!(!ant.constrained);
        assert_eq!(ant.payload["max_tokens"], 64);

        let fp = fingerprint(&url, "openai/gpt-4o-mini");
        assert_eq!(fp["backend"], "openai");
        assert_eq!(fp["owned_by"], "openai");
    }

    #[test]
    fn custom_provider_in_registry_routes_to_openai_protocol() {
        register_providers(&BTreeMap::from([(
            "vllm".to_string(),
            ProviderConfig {
                kind: "openai".to_string(),
                url: "http://127.0.0.1:8000/v1".to_string(),
                api_key_env: None,
            },
        )]));
        let settings = BTreeMap::new();
        let req = ChatReq {
            model: "qwen3-8b",
            system: "sys",
            user: "usr",
            schema: None,
            settings: &settings,
            timeout: None,
            max_output_tokens: Some(32),
            images: &[],
        };
        let p = payload("vllm/qwen3-8b", &req).unwrap();
        assert_eq!(p["model"], "qwen3-8b");
        assert_eq!(p["max_tokens"], 32);
    }

    #[test]
    fn fingerprints_ask_the_server_once_for_every_site() {
        use std::io::{BufRead, BufReader, Write};
        let server = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}", server.local_addr().unwrap());
        let hits = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let counted = hits.clone();
        std::thread::spawn(move || {
            for mut s in server.incoming().flatten() {
                let mut line = String::new();
                let mut r = BufReader::new(s.try_clone().unwrap());
                r.read_line(&mut line).unwrap();
                loop {
                    let mut h = String::new();
                    if r.read_line(&mut h).unwrap() <= 2 {
                        break;
                    }
                }
                counted.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                let body = if line.contains("/api/tags") {
                    r#"{"models":[{"name":"a:latest","digest":"da","details":{}},{"name":"b","digest":"db","details":{}}]}"#
                } else {
                    r#"{"version":"0.9"}"#
                };
                let _ = write!(
                    s,
                    "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                );
            }
        });
        let sites: std::collections::BTreeSet<String> = ["ollama/a", "ollama/b", "mock/x"]
            .into_iter()
            .map(String::from)
            .collect();
        let prints = fingerprints(&url, &sites);
        assert_eq!(
            hits.load(std::sync::atomic::Ordering::SeqCst),
            2,
            "one tags and one version request in all"
        );
        assert_eq!(prints["ollama/a"]["digest"], "da");
        assert_eq!(prints["ollama/b"]["digest"], "db");
        assert_eq!(prints["ollama/b"]["server_version"], "0.9");
        assert_eq!(prints["mock/x"], fingerprint(&url, "mock/x"));
        assert_eq!(
            prints["ollama/a"],
            fingerprint(&url, "ollama/a"),
            "same shape as a single fingerprint"
        );
    }

    fn img(bytes: &[u8]) -> ImagePart {
        use sha2::{Digest, Sha256};
        ImagePart {
            sha256: Sha256::digest(bytes)
                .iter()
                .map(|b| format!("{b:02x}"))
                .collect(),
            mime: "image/png".into(),
            bytes: bytes.to_vec(),
        }
    }

    fn with_req<T>(images: &[ImagePart], f: impl FnOnce(&ChatReq) -> T) -> T {
        let settings = BTreeMap::from([("temperature".into(), json!(0.2))]);
        let schema = json!({"type": "object"});
        let req = ChatReq {
            model: "m",
            system: "sys",
            user: "usr",
            schema: Some(&schema),
            settings: &settings,
            timeout: None,
            max_output_tokens: Some(50),
            images,
        };
        f(&req)
    }

    #[test]
    fn no_image_payloads_and_hashes_unchanged() {
        with_req(&[], |req| {
            assert_eq!(
                ollama_payload("m", req, ImageMode::Inline)
                    .unwrap()
                    .to_string(),
                r#"{"format":{"type":"object"},"keep_alive":"10m","messages":[{"content":"sys","role":"system"},{"content":"usr","role":"user"}],"model":"m","options":{"num_predict":50,"temperature":0.2},"stream":false,"think":false}"#
            );
            assert_eq!(
                openai_payload("m", req, ImageMode::Inline)
                    .unwrap()
                    .to_string(),
                r#"{"max_tokens":50,"messages":[{"content":"sys","role":"system"},{"content":"usr","role":"user"}],"model":"m","response_format":{"json_schema":{"name":"summonr_output","schema":{"type":"object"},"strict":true},"type":"json_schema"},"stream":false,"temperature":0.2}"#
            );
            assert_eq!(
                anthropic_payload("m", req, ImageMode::Inline)
                    .unwrap()
                    .to_string(),
                r#"{"max_tokens":50,"messages":[{"content":"usr","role":"user"}],"model":"m","stream":false,"system":"sys","temperature":0.2}"#
            );
            assert_eq!(
                payload("mock/m", req).unwrap().to_string(),
                r#"{"backend":"mock","model":"mock/m","system":"sys","user":"usr"}"#
            );
            assert_eq!(
                crate::cassette::model_hash("m", req),
                "03af21556803bbb07ae39b451877d963961a36cb9cc183c81a46a7b8fa03f262"
            );
        });
    }

    #[test]
    fn ollama_inline_and_placeholder_shapes() {
        let images = [img(b"\x89PNG-bytes")];
        let enc = b64(&images[0].bytes);
        with_req(&images, |req| {
            let inline = ollama_payload("m", req, ImageMode::Inline).unwrap();
            assert_eq!(inline["messages"][1]["images"], json!([enc]));
            assert_eq!(inline["messages"][1]["content"], "usr");
            let ph = ollama_payload("m", req, ImageMode::Placeholder).unwrap();
            assert_eq!(
                ph["messages"][1]["images"],
                json!([{"sight": {"sha256": images[0].sha256, "mime": "image/png", "bytes": 10}}])
            );
            assert!(!ph.to_string().contains(&enc));
            assert_eq!(payload("ollama/m", req).unwrap(), ph);
        });
    }

    #[test]
    fn openai_inline_and_placeholder_shapes() {
        let images = [img(b"\x89PNG-bytes"), img(b"second")];
        let enc = b64(&images[0].bytes);
        with_req(&images, |req| {
            let inline = openai_payload("m", req, ImageMode::Inline).unwrap();
            let c = &inline["messages"][1]["content"];
            assert_eq!(c[0]["type"], "image_url");
            assert_eq!(
                c[0]["image_url"]["url"],
                format!("data:image/png;base64,{enc}")
            );
            assert_eq!(c[2], json!({"type": "text", "text": "usr"}));
            let ph = openai_payload("m", req, ImageMode::Placeholder).unwrap();
            let c = &ph["messages"][1]["content"];
            assert_eq!(c[0]["sight"]["sha256"], images[0].sha256);
            assert_eq!(c[1]["sight"]["bytes"], 6);
            assert_eq!(c[2], json!({"type": "text", "text": "usr"}));
            assert!(!ph.to_string().contains(&enc));
            assert_eq!(payload("openai/m", req).unwrap(), ph);
        });
    }

    #[test]
    fn anthropic_inline_and_placeholder_shapes() {
        let images = [img(b"\x89PNG-bytes")];
        let enc = b64(&images[0].bytes);
        with_req(&images, |req| {
            let inline = anthropic_payload("m", req, ImageMode::Inline).unwrap();
            let c = &inline["messages"][0]["content"];
            assert_eq!(
                c[0],
                json!({"type": "image", "source": {"type": "base64", "media_type": "image/png", "data": enc}})
            );
            assert_eq!(c[1], json!({"type": "text", "text": "usr"}));
            let ph = anthropic_payload("m", req, ImageMode::Placeholder).unwrap();
            assert_eq!(
                ph["messages"][0]["content"][0]["sight"]["sha256"],
                images[0].sha256
            );
            assert!(!ph.to_string().contains(&enc));
            assert_eq!(payload("anthropic/m", req).unwrap(), ph);
        });
    }

    #[test]
    fn model_hash_covers_image_digests_not_bytes() {
        let a = img(b"one");
        let mut a_same_digest = a.clone();
        a_same_digest.bytes = b"different bytes, same claimed digest".to_vec();
        let b = img(b"two");
        let none = with_req(&[], |r| crate::cassette::model_hash("m", r));
        let ha = with_req(std::slice::from_ref(&a), |r| {
            crate::cassette::model_hash("m", r)
        });
        let ha2 = with_req(&[a_same_digest], |r| crate::cassette::model_hash("m", r));
        let hb = with_req(&[b], |r| crate::cassette::model_hash("m", r));
        assert_ne!(none, ha);
        assert_eq!(ha, ha2);
        assert_ne!(ha, hb);
    }

    #[test]
    fn mock_carries_image_digests_and_estimates_256_tokens_each() {
        let images = [img(b"one"), img(b"two")];
        let settings = BTreeMap::new();
        let mk = |images: &[ImagePart]| {
            mock_chat(ChatReq {
                model: "keyword",
                system: "",
                user: "hi",
                schema: None,
                settings: &settings,
                timeout: None,
                max_output_tokens: None,
                images,
            })
        };
        let plain = mk(&[]);
        let r = mk(&images);
        assert!(plain.payload.get("images").is_none());
        assert_eq!(
            r.payload["images"],
            json!([images[0].sha256, images[1].sha256])
        );
        assert_eq!(r.prompt_tokens, plain.prompt_tokens + 512);
        assert_eq!(r.token_confidence, "estimated");
    }

    /// Serves `/api/show` with the given capabilities; counts requests.
    fn show_server(caps: &'static str) -> (String, std::sync::Arc<std::sync::atomic::AtomicUsize>) {
        use std::io::{BufRead, BufReader, Read, Write};
        let server = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}", server.local_addr().unwrap());
        let hits = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let counted = hits.clone();
        std::thread::spawn(move || {
            for mut s in server.incoming().flatten() {
                let mut line = String::new();
                let mut r = BufReader::new(s.try_clone().unwrap());
                r.read_line(&mut line).unwrap();
                let mut len = 0usize;
                loop {
                    let mut h = String::new();
                    if r.read_line(&mut h).unwrap() <= 2 {
                        break;
                    }
                    if let Some((k, v)) = h.split_once(':') {
                        if k.trim().eq_ignore_ascii_case("content-length") {
                            len = v.trim().parse().unwrap_or(0);
                        }
                    }
                }
                let mut body = vec![0u8; len];
                let _ = r.read_exact(&mut body);
                if line.starts_with("POST /api/show") {
                    counted.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                }
                let out = format!(r#"{{"capabilities":{caps}}}"#);
                let _ = write!(
                    s,
                    "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{out}",
                    out.len()
                );
            }
        });
        (url, hits)
    }

    #[test]
    fn ollama_without_vision_is_refused() {
        let (url, hits) = show_server(r#"["completion"]"#);
        let err = supports_images(&url, "ollama/text-only", None).unwrap_err();
        assert!(err.contains("does not support images"), "{err}");
        assert!(supports_images(&url, "ollama/text-only", None).is_err());
        assert_eq!(hits.load(std::sync::atomic::Ordering::SeqCst), 1, "cached");
    }

    #[test]
    fn ollama_with_vision_is_allowed() {
        let (url, hits) = show_server(r#"["completion","vision"]"#);
        supports_images(&url, "ollama/seer", None).unwrap();
        supports_images(&url, "ollama/seer", None).unwrap();
        assert_eq!(hits.load(std::sync::atomic::Ordering::SeqCst), 1, "cached");
        supports_images("http://127.0.0.1:1", "openai/x", None).unwrap();
        supports_images("http://127.0.0.1:1", "mock/keyword", None).unwrap();
    }

    /// Records each request body; answers `status` with `reply`, or echoes
    /// the request body when `reply` is None.
    fn recording_server(
        status: &'static str,
        reply: Option<&'static str>,
    ) -> (String, std::sync::Arc<Mutex<Vec<String>>>) {
        use std::io::{BufRead, BufReader, Read, Write};
        let server = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}", server.local_addr().unwrap());
        let seen = std::sync::Arc::new(Mutex::new(Vec::new()));
        let log = seen.clone();
        std::thread::spawn(move || {
            for mut s in server.incoming().flatten() {
                let mut line = String::new();
                let mut r = BufReader::new(s.try_clone().unwrap());
                r.read_line(&mut line).unwrap();
                let mut len = 0usize;
                loop {
                    let mut h = String::new();
                    if r.read_line(&mut h).unwrap() <= 2 {
                        break;
                    }
                    if let Some((k, v)) = h.split_once(':') {
                        if k.trim().eq_ignore_ascii_case("content-length") {
                            len = v.trim().parse().unwrap_or(0);
                        }
                    }
                }
                let mut body = vec![0u8; len];
                let _ = r.read_exact(&mut body);
                let body = String::from_utf8(body).unwrap();
                let out = reply.map(String::from).unwrap_or_else(|| body.clone());
                log.lock().unwrap().push(body);
                let _ = write!(
                    s,
                    "HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{out}",
                    out.len()
                );
            }
        });
        (url, seen)
    }

    fn chat_with_images(url: &str, model: &str, images: &[ImagePart]) -> Result<ChatResp, String> {
        let settings = BTreeMap::new();
        chat(
            url,
            model,
            ChatReq {
                model: "m",
                system: "sys",
                user: "usr",
                schema: None,
                settings: &settings,
                timeout: Some(Duration::from_secs(5)),
                max_output_tokens: Some(50),
                images,
            },
        )
    }

    #[test]
    fn chat_sends_inline_bytes_but_returns_the_placeholder_payload() {
        let images = [img(b"\x89PNG-bytes")];
        let enc = b64(&images[0].bytes);
        let cases = [
            ("ollama/m", r#"{"message":{"content":"ok"}}"#),
            ("openai/m", r#"{"choices":[{"message":{"content":"ok"}}]}"#),
            ("anthropic/m", r#"{"content":[{"type":"text","text":"ok"}]}"#),
        ];
        for (model, reply) in cases {
            let (url, seen) = recording_server("200 OK", Some(reply));
            let resp = chat_with_images(&url, model, &images).unwrap();
            let sent: Value = serde_json::from_str(&seen.lock().unwrap()[0]).unwrap();
            assert!(sent.to_string().contains(&enc), "{model}: server must get the bytes");
            let at = match model {
                "ollama/m" => &sent["messages"][1]["images"][0],
                "openai/m" => &sent["messages"][1]["content"][0]["image_url"]["url"],
                _ => &sent["messages"][0]["content"][0]["source"]["data"],
            };
            assert!(at.as_str().unwrap().ends_with(&enc), "{model}: wrong position");
            let recorded = resp.payload.to_string();
            assert!(!recorded.contains(&enc), "{model}: base64 leaked: {recorded}");
            assert!(recorded.contains(&images[0].sha256), "{model}");
        }
    }

    #[test]
    fn image_request_errors_are_scrubbed_and_bounded() {
        // A large image so the echoed body far exceeds the bound.
        let images = [img(&vec![7u8; 6000])];
        let enc = b64(&images[0].bytes);
        for model in ["ollama/m", "openai/m", "anthropic/m"] {
            let (url, _) = recording_server("422 Unprocessable Entity", None);
            let err = chat_with_images(&url, model, &images).err().unwrap();
            assert!(!err.contains(&enc), "{model}: base64 in error");
            assert!(!err.contains(&enc[..64]), "{model}: base64 fragment in error");
            assert!(err.chars().count() <= 2000 + 64, "{model}: unbounded ({})", err.len());
        }
        // The marker shows when the echo is short enough to survive truncation.
        let small = [img(b"tiny")];
        let (url, _) = recording_server("422 Unprocessable Entity", None);
        let err = chat_with_images(&url, "openai/m", &small).err().unwrap();
        assert!(err.contains(&format!("[sight:{}]", small[0].sha256)), "{err}");
        assert!(!err.contains(&b64(b"tiny")), "{err}");
        // Escaped and truncated copies are elided too.
        let big = [img(&vec![9u8; 600])];
        let enc = b64(&big[0].bytes);
        let escaped = enc.replace('/', "\\/");
        let reply: &'static str = format!("bad image: {}...", &escaped[..400]).leak();
        let (url, _) = recording_server("422 Unprocessable Entity", Some(reply));
        let err = chat_with_images(&url, "openai/m", &big).err().unwrap();
        assert!(err.contains("[sight:redacted]"), "{err}");
        assert!(!err.contains(&enc[..64]), "{err}");
        let reply: &'static str = format!("invalid image: {}", &enc[..120]).leak();
        let (url, _) = recording_server("422 Unprocessable Entity", Some(reply));
        let err = chat_with_images(&url, "openai/m", &big).err().unwrap();
        assert!(err.contains("[sight:redacted]") && !err.contains(&enc[..64]), "{err}");
        // No images: error text is left alone.
        let (url, _) = recording_server("422 Unprocessable Entity", Some("x".repeat(3000).leak()));
        let err = chat_with_images(&url, "openai/m", &[]).err().unwrap();
        assert!(err.len() > 3000);
    }
}
