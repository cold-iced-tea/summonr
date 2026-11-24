//! Small shared helpers: time, identifiers, hashing, paths, configuration.

use serde::Deserialize;
use std::io::Read;
use std::path::PathBuf;
#[cfg(test)]
use std::sync::atomic::{AtomicI64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

#[cfg(test)]
static CLOCK_OFFSET_MS: AtomicI64 = AtomicI64::new(0);

#[cfg(test)]
pub fn set_clock_offset_ms(offset_ms: i64) {
    CLOCK_OFFSET_MS.store(offset_ms, Ordering::SeqCst);
}

#[cfg(test)]
fn dynamic_clock_offset_ms() -> i64 {
    let base = CLOCK_OFFSET_MS.load(Ordering::SeqCst);
    let env_off = std::env::var("SUMMONR_CLOCK_OFFSET_MS")
        .ok()
        .and_then(|s| s.trim().parse::<i64>().ok())
        .unwrap_or(0);
    base.saturating_add(env_off)
}

#[cfg(not(test))]
fn dynamic_clock_offset_ms() -> i64 {
    0
}

pub fn now_ms() -> i64 {
    let wall = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0);
    wall.saturating_add(dynamic_clock_offset_ms())
}

pub(crate) fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

pub fn new_id(prefix: &str) -> String {
    let mut b = [0u8; 16];
    std::fs::File::open("/dev/urandom")
        .and_then(|mut f| f.read_exact(&mut b))
        .expect("OS randomness is required for unique execution identifiers");
    format!("{prefix}-{}", hex(&b))
}

pub fn sha256_hex(data: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    let d = Sha256::digest(data);
    hex(d.as_slice())
}

/// SHA-256 hex of `value` serialized with object keys sorted at every depth.
pub fn canonical_sha256(value: &serde_json::Value) -> String {
    sha256_hex(canonical_json(value).as_bytes())
}

/// `value` as JSON with object keys sorted at every depth and no whitespace.
pub fn canonical_json(value: &serde_json::Value) -> String {
    fn write(v: &serde_json::Value, out: &mut String) {
        use serde_json::Value;
        match v {
            Value::Object(m) => {
                let mut keys: Vec<&String> = m.keys().collect();
                keys.sort();
                out.push('{');
                for (i, k) in keys.into_iter().enumerate() {
                    if i > 0 {
                        out.push(',');
                    }
                    out.push_str(&Value::String(k.clone()).to_string());
                    out.push(':');
                    write(&m[k], out);
                }
                out.push('}');
            }
            Value::Array(a) => {
                out.push('[');
                for (i, x) in a.iter().enumerate() {
                    if i > 0 {
                        out.push(',');
                    }
                    write(x, out);
                }
                out.push(']');
            }
            other => out.push_str(&other.to_string()),
        }
    }
    let mut out = String::new();
    write(value, &mut out);
    out
}

/// Parse a monetary amount such as `"$0.25"`, `"0.25"`, `"25c"`, or `"$1.50"`
/// into integer microdollars (1 USD = 1_000_000 µ$). Up to 6 decimal places.
pub fn parse_cost_microusd(s: &str) -> Result<u64, String> {
    let raw = s.trim();
    if raw.is_empty() {
        return Err("cost must not be empty (e.g. \"$0.25\" or \"25c\")".into());
    }
    if raw.starts_with('-') {
        return Err(format!("cost `{s}` must not be negative"));
    }
    if let Some(cents_str) = raw.strip_suffix('c') {
        let cents_str = cents_str.trim();
        let micro_cents = parse_decimal_scaled(cents_str, 4)
            .map_err(|e| format!("cost `{s}`: {e}"))?;
        return Ok(micro_cents);
    }
    let dollars = raw.strip_prefix('$').unwrap_or(raw).trim();
    parse_decimal_scaled(dollars, 6).map_err(|e| format!("cost `{s}`: {e}"))
}

fn parse_decimal_scaled(s: &str, scale: usize) -> Result<u64, String> {
    if s.is_empty() {
        return Err("expected a number".into());
    }
    let (whole_s, frac_s) = match s.split_once('.') {
        Some((w, f)) => (w, f),
        None => (s, ""),
    };
    if frac_s.contains('.') {
        return Err("invalid decimal number".into());
    }
    if whole_s.is_empty() && frac_s.is_empty() {
        return Err("expected digits".into());
    }
    if !whole_s.bytes().all(|b| b.is_ascii_digit()) || !frac_s.bytes().all(|b| b.is_ascii_digit()) {
        return Err("must contain only digits and an optional decimal point".into());
    }
    if frac_s.len() > scale {
        return Err(format!("supports at most {scale} decimal places"));
    }
    let whole: u64 = if whole_s.is_empty() {
        0
    } else {
        whole_s.parse().map_err(|_| "number overflow".to_string())?
    };
    let mult = 10u64.pow(scale as u32);
    let mut frac: u64 = 0;
    for b in frac_s.bytes() {
        frac = frac * 10 + (b - b'0') as u64;
    }
    let frac_scaled = frac * 10u64.pow((scale - frac_s.len()) as u32);
    whole
        .checked_mul(mult)
        .and_then(|w| w.checked_add(frac_scaled))
        .ok_or_else(|| "cost overflow".into())
}

/// Format integer microdollars as human-readable USD (e.g. `$0.0042`, `$0.25`, `$1.50`).
pub fn fmt_cost_microusd(microusd: u64) -> String {
    let whole = microusd / 1_000_000;
    let frac = microusd % 1_000_000;
    let frac_s = format!("{frac:06}");
    let trimmed = frac_s.trim_end_matches('0');
    if trimmed.len() <= 2 {
        format!("${whole}.{:02}", frac / 10_000)
    } else {
        format!("${whole}.{trimmed}")
    }
}

/// Parse a duration such as `500ms`, `30s`, `10m`, `2h` into milliseconds.
pub fn parse_duration(s: &str) -> Result<u64, String> {
    let s = s.trim();
    let (num, unit) = match s.find(|c: char| !c.is_ascii_digit()) {
        Some(i) => s.split_at(i),
        None => return Err(format!("duration `{s}` needs a unit: ms, s, m, or h")),
    };
    let n: u64 = num
        .parse()
        .map_err(|_| format!("duration `{s}` must start with a whole number"))?;
    match unit {
        "ms" => Ok(n),
        "s" => n
            .checked_mul(1000)
            .ok_or_else(|| "duration overflow".into()),
        "m" => n
            .checked_mul(60_000)
            .ok_or_else(|| "duration overflow".into()),
        "h" => n
            .checked_mul(3_600_000)
            .ok_or_else(|| "duration overflow".into()),
        _ => Err(format!(
            "duration `{s}` has unknown unit `{unit}`; use ms, s, m, or h"
        )),
    }
}

pub fn fmt_dur(ms: i64) -> String {
    let s = (ms.max(0) / 1000) as u64;
    if s >= 3600 {
        format!("{}:{:02}:{:02}", s / 3600, (s % 3600) / 60, s % 60)
    } else {
        format!("{:02}:{:02}", s / 60, s % 60)
    }
}

pub fn fmt_offset(ms: i64) -> String {
    let ms = ms.max(0);
    format!(
        "+{:02}:{:02}.{}",
        ms / 60_000,
        (ms / 1000) % 60,
        (ms % 1000) / 100
    )
}

/// A UTC time as `YYYY-MM-DDTHH:MMZ`, for marks people read and paste.
pub fn iso_minute(ms: i64) -> String {
    let secs = ms.div_euclid(1000);
    let (days, rem) = (secs.div_euclid(86_400), secs.rem_euclid(86_400));
    // Civil date from days since 1970-01-01 (Howard Hinnant's algorithm).
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + i64::from(month <= 2);
    format!("{year:04}-{month:02}-{day:02}T{:02}:{:02}Z", rem / 3600, rem % 3600 / 60)
}

pub fn now_iso() -> String {
    iso_minute(now_ms())
}

pub fn fmt_ago(ms: i64) -> String {
    let s = ((now_ms() - ms).max(0) / 1000) as u64;
    match s {
        0..=59 => format!("{s}s ago"),
        60..=3599 => format!("{}m ago", s / 60),
        3600..=86_399 => format!("{}h ago", s / 3600),
        _ => format!("{}d ago", s / 86_400),
    }
}

pub fn thousands(n: u64) -> String {
    let s = n.to_string();
    let mut out = String::new();
    for (i, c) in s.chars().enumerate() {
        if i > 0 && (s.len() - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(c);
    }
    out
}

#[derive(Clone, Debug)]
pub struct Paths {
    pub runtime: PathBuf,
    pub state: PathBuf,
    pub config: PathBuf,
}

impl Paths {
    /// The single home directory when one is in use (`SUMMONR_HOME` or
    /// `--home`), or `None` for the XDG layout.
    pub fn home(&self) -> Option<PathBuf> {
        let parent = self.state.parent()?;
        (self.config.parent() == Some(parent)
            && self.state.file_name() == Some("state".as_ref())
            && self.config.file_name() == Some("config".as_ref()))
        .then(|| parent.to_path_buf())
    }

    /// `SUMMONR_HOME` keeps everything under one directory (used by tests and
    /// the demo). Otherwise the XDG locations apply. There is no shared /tmp
    /// fallback for the socket.
    pub fn resolve() -> Paths {
        if let Ok(home) = std::env::var("SUMMONR_HOME") {
            let h = PathBuf::from(home);
            // A Unix socket path is limited to about 100 bytes. A deep home
            // keeps its socket in the user's runtime directory instead, under
            // a name derived from the home so two homes never share a core.
            let mut runtime = h.join("run");
            if runtime.join("core.sock").as_os_str().len() > 100 {
                if let Ok(r) = std::env::var("XDG_RUNTIME_DIR") {
                    runtime = PathBuf::from(r).join(format!(
                        "summonr-{}",
                        &sha256_hex(h.to_string_lossy().as_bytes())[..12]
                    ));
                }
            }
            return Paths {
                runtime,
                state: h.join("state"),
                config: h.join("config"),
            };
        }
        let home = PathBuf::from(std::env::var("HOME").unwrap_or_else(|_| ".".into()));
        let xdg = |var: &str, fallback: &str| {
            std::env::var(var)
                .map(PathBuf::from)
                .unwrap_or_else(|_| home.join(fallback))
                .join("summonr")
        };
        let state = xdg("XDG_STATE_HOME", ".local/state");
        let runtime = match std::env::var("XDG_RUNTIME_DIR") {
            Ok(r) => PathBuf::from(r).join("summonr"),
            Err(_) => state.join("run"),
        };
        Paths {
            runtime,
            state,
            config: xdg("XDG_CONFIG_HOME", ".config"),
        }
    }

    pub fn socket(&self) -> PathBuf {
        self.runtime.join("core.sock")
    }
    pub fn db(&self) -> PathBuf {
        self.state.join("summonr.db")
    }
}

#[derive(Deserialize, Default, Clone, Debug)]
#[serde(deny_unknown_fields)]
pub struct Config {
    /// `keyword` or `flag`: the spelling used in help and suggestions.
    pub style: Option<String>,
    pub ollama_url: Option<String>,
    pub openai_url: Option<String>,
    pub anthropic_url: Option<String>,
    /// Named model providers (`<provider>/<model>`), each with a protocol `kind`,
    /// `url`, and optional `api_key_env`.
    #[serde(default)]
    pub providers: std::collections::BTreeMap<String, ProviderConfig>,
    /// Owner-configured per-model token pricing (`<provider>/<model>` or `<provider>/*`).
    #[serde(default)]
    pub pricing: std::collections::BTreeMap<String, PricingConfig>,
    /// Root directory of the fixture mailbox used by the `mail` reagent.
    pub mail_root: Option<String>,
    pub max_depth: Option<u32>,
    pub max_active_runs: Option<usize>,
    /// The `openssl` program used for RFC 3161 timestamps (default: `openssl` on PATH).
    pub openssl: Option<String>,
    /// Repositories the git, gh and trial reagents may touch, by name.
    #[serde(default)]
    pub repos: std::collections::BTreeMap<String, RepoConfig>,
    /// Scoped commands the `cmd` reagent may execute, by name.
    #[serde(default)]
    pub commands: std::collections::BTreeMap<String, CommandConfig>,
    /// External MCP stdio servers configured as reagents, by name.
    #[serde(default)]
    pub mcp_reagents: std::collections::BTreeMap<String, McpReagentConfig>,
    /// Capability-sandboxed WebAssembly (WASI) reagents pinned by SHA-256, by name.
    #[serde(default)]
    pub wasm_reagents: std::collections::BTreeMap<String, WasmReagentConfig>,
    /// Outbound HTTP egress policy (host allow/deny lists and private-IP blocking).
    #[serde(default)]
    pub egress: EgressConfig,
    /// Named authenticated HTTP credential bindings (`http_auth.<name>`).
    #[serde(default)]
    pub http_auth: std::collections::BTreeMap<String, HttpAuthConfig>,
    /// Optional default TTL for pending petitions (e.g. `"24h"`).
    pub petition_ttl: Option<String>,
    /// Optional TLS certificate/key paths for `summonr listen` and `summonr mcp http`.
    pub tls: Option<TlsConfig>,
    /// Optional OAuth 2.1 Bearer JWT authentication for `summonr mcp http`.
    pub mcp_auth: Option<McpAuthConfig>,
    /// Additional allowed `Host` header values (host or host:port) for `summonr mcp http`.
    #[serde(default)]
    pub mcp_allowed_hosts: Vec<String>,
    /// Optional Role-Based Access Control (RBAC) enforcement settings.
    #[serde(default)]
    pub rbac: RbacConfig,
    /// The `git` and `gh` programs (default: the ones on PATH).
    pub git: Option<String>,
    pub gh: Option<String>,
    /// Lowered limits for images sent to model steps (`max_bytes`, `max_per_step`).
    pub sight: Option<SightConfig>,
}

/// Owner limits for `sight` images. They may only lower the defaults.
#[derive(Deserialize, Default, Clone, Debug, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct SightConfig {
    pub max_bytes: Option<u64>,
    pub max_per_step: Option<usize>,
}

/// Configuration for a pinned, capability-sandboxed WebAssembly (WASI) reagent.
#[derive(Deserialize, Clone, Debug, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct WasmReagentConfig {
    /// Absolute path to the `.wasm` module binary.
    pub module: String,
    /// Required 64-character lowercase hex SHA-256 of the `.wasm` module.
    pub sha256: String,
    /// WASI runtime binary (default: `wasmtime`).
    #[serde(default)]
    pub runtime: Option<String>,
    /// Declared tools exposed by this Wasm reagent.
    #[serde(default)]
    pub tools: std::collections::BTreeMap<String, WasmToolConfig>,
    /// Explicit preopened directories (`--dir`); empty by default (no filesystem access).
    #[serde(default)]
    pub preopen_dirs: Vec<String>,
    /// Optional linear-memory limit in MiB.
    #[serde(default)]
    pub max_memory_mb: Option<u64>,
    /// Optional deterministic WASI fuel limit.
    #[serde(default)]
    pub fuel: Option<u64>,
    /// Hard wall-clock timeout in milliseconds (default: 10000).
    #[serde(default)]
    pub timeout_ms: Option<u64>,
    /// Maximum stdout bytes accepted (default: 262144).
    #[serde(default)]
    pub max_output_bytes: Option<usize>,
}

#[derive(Deserialize, Clone, Debug, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct WasmToolConfig {
    /// `"pure"`, `"read"`, or `"write"` (default `"write"` — fail closed).
    #[serde(default = "default_wasm_effect")]
    pub effect: String,
    #[serde(default)]
    pub description: String,
}

fn default_wasm_effect() -> String {
    "write".into()
}

/// Role-Based Access Control settings for operator separation of duties.
#[derive(Deserialize, Default, Clone, Debug, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct RbacConfig {
    /// When true, unsigned local socket calls are refused (operators must sign).
    #[serde(default)]
    pub enforce: bool,
    /// When true, the operator who started a summon cannot permit its held petitions.
    #[serde(default)]
    pub require_independent_reviewer: bool,
}

/// Owner-configured API pricing for a model or `<provider>/*` wildcard.
#[derive(Deserialize, serde::Serialize, Clone, Debug, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct PricingConfig {
    pub input_per_mtok_usd: f64,
    pub output_per_mtok_usd: f64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cached_input_per_mtok_usd: Option<f64>,
}

impl PricingConfig {
    /// Exact integer microdollar cost (ceiling-rounded) for a single model call.
    pub fn cost_microusd(&self, prompt_tokens: u64, output_tokens: u64) -> Result<u64, String> {
        if !self.input_per_mtok_usd.is_finite()
            || self.input_per_mtok_usd < 0.0
            || !self.output_per_mtok_usd.is_finite()
            || self.output_per_mtok_usd < 0.0
        {
            return Err("pricing rates must be finite and non-negative".into());
        }
        let in_rate = (self.input_per_mtok_usd * 1_000_000.0).round() as u128;
        let out_rate = (self.output_per_mtok_usd * 1_000_000.0).round() as u128;
        let total = (prompt_tokens as u128)
            .saturating_mul(in_rate)
            .saturating_add((output_tokens as u128).saturating_mul(out_rate));
        let micro = total.div_ceil(1_000_000);
        u64::try_from(micro).map_err(|_| "cost overflow".into())
    }
}

/// Outbound HTTP egress policy enforced before any `http` reagent request.
#[derive(Deserialize, Default, Clone, Debug, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct EgressConfig {
    #[serde(default)]
    pub allow_hosts: Vec<String>,
    #[serde(default)]
    pub deny_hosts: Vec<String>,
    #[serde(default)]
    pub block_private_ips: bool,
}

fn default_auth_header() -> String {
    "Authorization".into()
}
fn default_auth_prefix() -> String {
    "Bearer ".into()
}

/// Owner-configured credential binding for `http` reagent calls (`auth: <name>`).
#[derive(Deserialize, Clone, Debug, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct HttpAuthConfig {
    /// Origin this credential is pinned to (e.g. `"https://api.example.com"`).
    pub origin: String,
    #[serde(default = "default_auth_header")]
    pub header: String,
    #[serde(default = "default_auth_prefix")]
    pub prefix: String,
    /// Environment variable holding the secret token.
    pub token_env: String,
}

/// TLS server certificate and private key (PEM files) for `listen` and `mcp http`.
#[derive(Deserialize, Clone, Debug, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct TlsConfig {
    pub cert: String,
    pub key: String,
}

/// OAuth 2.1 Bearer JWT verification settings for `summonr mcp http`.
#[derive(Deserialize, Clone, Debug, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct McpAuthConfig {
    /// Expected token issuer (`iss` claim).
    pub issuer: String,
    /// Expected token audience (`aud` claim).
    pub audience: String,
    /// Authorization server metadata URLs advertised at `/.well-known/oauth-protected-resource`.
    #[serde(default)]
    pub authorization_servers: Vec<String>,
    /// Required OAuth scopes (`scope` / `scp` claims).
    #[serde(default)]
    pub required_scopes: Vec<String>,
    /// EdDSA (`ed25519:<base64url>`) public key for JWT signature verification.
    pub public_key: Option<String>,
    /// Environment variable holding an HS256 shared secret (never stored raw in config.yaml).
    pub hs256_secret_env: Option<String>,
    /// Clock skew tolerance in seconds (default: 60).
    #[serde(default = "default_clock_skew_secs")]
    pub clock_skew_secs: u64,
}

fn default_clock_skew_secs() -> u64 {
    60
}

/// An external MCP stdio server configured by the owner as a reagent.
#[derive(Deserialize, Clone, Debug, Default, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct McpReagentConfig {
    pub command: Vec<String>,
    #[serde(default)]
    pub cwd: Option<String>,
    #[serde(default)]
    pub env: std::collections::BTreeMap<String, String>,
    #[serde(default)]
    pub tools: std::collections::BTreeMap<String, McpToolConfig>,
}

#[derive(Deserialize, Clone, Debug, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct McpToolConfig {
    /// Declared effect class (`read` or `write`; defaults to `write`).
    #[serde(default = "default_mcp_effect")]
    pub effect: String,
    #[serde(default)]
    pub description: Option<String>,
}

fn default_mcp_effect() -> String {
    "write".into()
}

/// A named model backend provider configured by the owner.
#[derive(Deserialize, Clone, Debug, PartialEq, Default)]
#[serde(deny_unknown_fields)]
pub struct ProviderConfig {
    /// Protocol family: `openai` (default), `anthropic`, `ollama`, or
    /// `command` (a local program that answers a prompt).
    #[serde(default = "default_provider_kind")]
    pub kind: String,
    /// The server's URL. Required for every kind but `command`.
    #[serde(default)]
    pub url: String,
    /// Name of the environment variable holding the API key, if required.
    /// Summonr never stores raw API keys in config.yaml.
    pub api_key_env: Option<String>,
    /// `kind: command`: the program to run (never through a shell).
    #[serde(default)]
    pub program: Option<String>,
    /// `kind: command`: arguments; `${model}` is the label after the
    /// provider name, `${prompt}` the prompt when `stdin: none`.
    #[serde(default)]
    pub args: Vec<String>,
    /// `kind: command`: absolute working directory (default: the system
    /// temporary directory).
    #[serde(default)]
    pub cwd: Option<String>,
    /// `kind: command`: environment variables passed through by name, as for
    /// `commands:`; Summonr's credentials are refused.
    #[serde(default)]
    pub env: Option<Vec<String>>,
    /// `kind: command`: `prompt` (default) writes the prompt to stdin;
    /// `none` passes it as `${prompt}` in `args` instead.
    #[serde(default)]
    pub stdin: Option<String>,
    /// `kind: command`: the most one call may take (default 600000).
    #[serde(default)]
    pub timeout_ms: Option<u64>,
}

fn default_provider_kind() -> String {
    "openai".into()
}

/// One scoped command the owner lets rituals invoke via the `cmd` reagent.
#[derive(Deserialize, Clone, Debug, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct CommandConfig {
    /// The executable program (never passed to a shell).
    pub program: String,
    /// Argument templates. Plain arguments are passed verbatim; `${param}`
    /// or `--flag=${param}` placeholders are filled from validated `params`.
    #[serde(default)]
    pub args: Vec<String>,
    /// Working directory (must be an absolute directory path).
    pub cwd: String,
    /// `read` (default) or `write`. `cmd.read` refuses `write` commands;
    /// `cmd.write` refuses `read` commands.
    #[serde(default = "default_cmd_effect")]
    pub effect: String,
    /// Parameter constraints keyed by parameter name.
    #[serde(default)]
    pub params: std::collections::BTreeMap<String, CommandParam>,
    /// When true, non-zero exit codes return `passed: false` instead of failing.
    #[serde(default)]
    pub allow_nonzero: bool,
    /// Optional configured `read` command name used to compute a resource
    /// digest when a `write` command is held as a petition.
    pub digest_command: Option<String>,
    /// Environment variables passed through from the core, by name. When
    /// set, the command gets exactly these (and never Summonr's secrets);
    /// when absent it inherits the core's environment minus those secrets.
    #[serde(default)]
    pub env: Option<Vec<String>>,
    /// A parameter written to the command's standard input instead of an
    /// argument. Unlike an argument it may span lines; it may not also
    /// appear in `args`, and it is required.
    #[serde(default)]
    pub stdin: Option<String>,
}

/// Constraints on one parameter of a configured `cmd` command.
#[derive(Deserialize, Clone, Debug, Default, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct CommandParam {
    /// Closed set of permitted values, if any.
    #[serde(rename = "enum", default)]
    pub options: Option<Vec<String>>,
    /// Maximum UTF-8 character length (default: 1024).
    pub max_length: Option<usize>,
    /// Allow values starting with `-` when passed as a standalone argument.
    #[serde(default)]
    pub allow_leading_dash: bool,
}

fn default_cmd_effect() -> String {
    "read".into()
}

/// One repository the owner lets rituals work on.
#[derive(Deserialize, Clone, Debug)]
#[serde(deny_unknown_fields)]
pub struct RepoConfig {
    /// The main checkout.
    pub path: String,
    #[serde(default = "default_trunk")]
    pub trunk: String,
    #[serde(default = "default_remote")]
    pub remote: String,
    /// The test command `trial.run` may run, split into words (never a shell).
    pub test: Option<String>,
    /// Ignored paths a worktree may lose when it is removed (e.g. `target/`).
    /// Any other ignored file keeps the worktree.
    #[serde(default)]
    pub disposable: Vec<String>,
}

fn default_trunk() -> String {
    "master".into()
}

fn default_remote() -> String {
    "origin".into()
}

fn normalize_url(raw: &str) -> String {
    let raw = raw.trim().trim_end_matches('/').to_string();
    if raw.starts_with("http://") || raw.starts_with("https://") {
        raw
    } else {
        format!("http://{raw}")
    }
}

impl Config {
    pub fn load(paths: &Paths) -> Result<Config, String> {
        let p = paths.config.join("config.yaml");
        let cfg: Config = match std::fs::read_to_string(&p) {
            Ok(s) if s.trim().is_empty() => Config::default(),
            Ok(s) => serde_yaml::from_str(&s).map_err(|e| format!("{}: {e}", p.display()))?,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Config::default(),
            Err(e) => return Err(format!("cannot read {}: {e}", p.display())),
        };
        if let Some(auth) = &cfg.mcp_auth {
            if auth.issuer.trim().is_empty() || auth.audience.trim().is_empty() {
                return Err(format!("{}: mcp_auth requires non-empty issuer and audience", p.display()));
            }
            if auth.public_key.is_none() && auth.hs256_secret_env.is_none() {
                return Err(format!("{}: mcp_auth requires public_key or hs256_secret_env", p.display()));
            }
            if let Some(pk) = &auth.public_key {
                crate::keys::PublicKey::parse(pk)
                    .map_err(|e| format!("{}: invalid mcp_auth.public_key: {e}", p.display()))?;
            }
        }
        if let Some(sight) = &cfg.sight {
            use crate::reagents::sight_reagent::{DEFAULT_MAX_BYTES, DEFAULT_MAX_PER_STEP};
            if sight.max_bytes.is_some_and(|b| b > DEFAULT_MAX_BYTES)
                || sight.max_per_step.is_some_and(|n| n > DEFAULT_MAX_PER_STEP)
            {
                return Err(format!(
                    "{}: sight limits can only be lowered (max_bytes ≤ {DEFAULT_MAX_BYTES}, max_per_step ≤ {DEFAULT_MAX_PER_STEP})",
                    p.display()
                ));
            }
        }
        for (name, provider) in &cfg.providers {
            if provider.kind == "command" {
                crate::command_backend::validate(name, provider, &cfg)
                    .map_err(|e| format!("{}: providers.{name}: {e}", p.display()))?;
            } else if provider.url.trim().is_empty() {
                return Err(format!("{}: providers.{name}: `url` is required for kind `{}`", p.display(), provider.kind));
            }
        }
        crate::backend::register_config(&cfg);
        Ok(cfg)
    }

    /// `(max_bytes, max_per_step)` for images sent to a model step.
    pub fn sight_limits(&self) -> (u64, usize) {
        use crate::reagents::sight_reagent::{DEFAULT_MAX_BYTES, DEFAULT_MAX_PER_STEP};
        let s = self.sight.clone().unwrap_or_default();
        (
            s.max_bytes.map_or(DEFAULT_MAX_BYTES, |b| b.min(DEFAULT_MAX_BYTES)),
            s.max_per_step.map_or(DEFAULT_MAX_PER_STEP, |n| n.min(DEFAULT_MAX_PER_STEP)),
        )
    }

    pub fn ollama_url(&self) -> String {
        let raw = self
            .ollama_url
            .clone()
            .or_else(|| std::env::var("SUMMONR_OLLAMA_URL").ok())
            .or_else(|| std::env::var("OLLAMA_HOST").ok())
            .unwrap_or_else(|| "http://127.0.0.1:11434".into());
        normalize_url(&raw)
    }

    pub fn openai_url(&self) -> String {
        let raw = self
            .openai_url
            .clone()
            .or_else(|| self.providers.get("openai").map(|p| p.url.clone()))
            .or_else(|| std::env::var("SUMMONR_OPENAI_URL").ok())
            .or_else(|| std::env::var("OPENAI_BASE_URL").ok())
            .unwrap_or_else(|| "https://api.openai.com/v1".into());
        normalize_url(&raw)
    }

    pub fn anthropic_url(&self) -> String {
        let raw = self
            .anthropic_url
            .clone()
            .or_else(|| self.providers.get("anthropic").map(|p| p.url.clone()))
            .or_else(|| std::env::var("SUMMONR_ANTHROPIC_URL").ok())
            .or_else(|| std::env::var("ANTHROPIC_BASE_URL").ok())
            .unwrap_or_else(|| "https://api.anthropic.com/v1".into());
        normalize_url(&raw)
    }

    pub fn backend_url_for(&self, model_id: &str) -> String {
        match crate::backend::split_model(model_id) {
            Ok((b, _)) => match self.providers.get(b) {
                Some(p) => normalize_url(&p.url),
                None => match b {
                    "openai" => self.openai_url(),
                    "anthropic" => self.anthropic_url(),
                    _ => self.ollama_url(),
                },
            },
            Err(_) => self.ollama_url(),
        }
    }

    pub fn max_depth(&self) -> u32 {
        self.max_depth.unwrap_or(3)
    }

    /// Resolve owner-configured pricing for a model (`exact` -> `<provider>/*` -> `mock/*` $0 default).
    pub fn price_for(&self, model: &str) -> Option<PricingConfig> {
        if let Some(exact) = self.pricing.get(model) {
            return Some(exact.clone());
        }
        if let Some((provider, _)) = model.split_once('/') {
            if let Some(wild) = self.pricing.get(&format!("{provider}/*")) {
                return Some(wild.clone());
            }
            if provider == "mock" {
                return Some(PricingConfig {
                    input_per_mtok_usd: 0.0,
                    output_per_mtok_usd: 0.0,
                    cached_input_per_mtok_usd: None,
                });
            }
        }
        None
    }
}

#[cfg(test)]
mod pricing_tests {
    use super::*;

    #[test]
    fn parse_and_format_costs() {
        assert_eq!(parse_cost_microusd("$0.25").unwrap(), 250_000);
        assert_eq!(parse_cost_microusd("0.25").unwrap(), 250_000);
        assert_eq!(parse_cost_microusd("25c").unwrap(), 250_000);
        assert_eq!(parse_cost_microusd("$1.000001").unwrap(), 1_000_001);
        assert_eq!(parse_cost_microusd("0").unwrap(), 0);
        assert!(parse_cost_microusd("-1").is_err());
        assert!(parse_cost_microusd("").is_err());
        assert!(parse_cost_microusd("$0.1234567").is_err());
        assert_eq!(fmt_cost_microusd(250_000), "$0.25");
        assert_eq!(fmt_cost_microusd(4_200), "$0.0042");
        assert_eq!(fmt_cost_microusd(1_500_000), "$1.50");
    }

    #[test]
    fn price_for_precedence_and_integer_math() {
        let mut cfg = Config::default();
        cfg.pricing.insert(
            "openai/*".into(),
            PricingConfig {
                input_per_mtok_usd: 1.0,
                output_per_mtok_usd: 2.0,
                cached_input_per_mtok_usd: None,
            },
        );
        cfg.pricing.insert(
            "openai/gpt-4.1-mini".into(),
            PricingConfig {
                input_per_mtok_usd: 0.40,
                output_per_mtok_usd: 1.60,
                cached_input_per_mtok_usd: None,
            },
        );
        let exact = cfg.price_for("openai/gpt-4.1-mini").unwrap();
        assert_eq!(exact.input_per_mtok_usd, 0.40);
        // 1_000 input ($0.0004 = 400 µ$) + 500 output ($0.0008 = 800 µ$) = 1_200 µ$
        assert_eq!(exact.cost_microusd(1_000, 500).unwrap(), 1_200);
        let wild = cfg.price_for("openai/gpt-4.1").unwrap();
        assert_eq!(wild.input_per_mtok_usd, 1.0);
        assert_eq!(cfg.price_for("mock/keyword").unwrap().input_per_mtok_usd, 0.0);
        assert!(cfg.price_for("anthropic/claude-sonnet-4").is_none());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn durations() {
        assert_eq!(parse_duration("10m").unwrap(), 600_000);
        assert_eq!(parse_duration("500ms").unwrap(), 500);
        assert!(parse_duration("10").is_err());
        assert!(parse_duration("5 parsecs").is_err());
        assert!(parse_duration("18446744073709551615h").is_err());
        assert_eq!(new_id("s").len(), 34);
    }

    fn load_yaml(yaml: &str) -> Result<Config, String> {
        let dir = std::env::temp_dir().join(new_id("summonr-cfg"));
        let paths = Paths { runtime: dir.join("run"), state: dir.join("state"), config: dir.join("config") };
        std::fs::create_dir_all(&paths.config).unwrap();
        std::fs::write(paths.config.join("config.yaml"), yaml).unwrap();
        let r = Config::load(&paths);
        let _ = std::fs::remove_dir_all(&dir);
        r
    }

    #[test]
    fn config_lowers_limits() {
        use crate::reagents::sight_reagent::{DEFAULT_MAX_BYTES, DEFAULT_MAX_PER_STEP};
        assert_eq!(Config::default().sight_limits(), (DEFAULT_MAX_BYTES, DEFAULT_MAX_PER_STEP));
        let cfg = load_yaml("sight: {max_bytes: 4194304, max_per_step: 4}\n").unwrap();
        assert_eq!(cfg.sight_limits(), (4_194_304, 4));
        let cfg = load_yaml("sight: {max_per_step: 2}\n").unwrap();
        assert_eq!(cfg.sight_limits(), (DEFAULT_MAX_BYTES, 2));
    }

    #[test]
    fn config_cannot_raise_limits() {
        for yaml in ["sight: {max_bytes: 10485761}\n", "sight: {max_per_step: 9}\n"] {
            let e = load_yaml(yaml).unwrap_err();
            assert!(e.contains("sight limits can only be lowered"), "{e}");
        }
        load_yaml("sight: {max_bytes: 10485760, max_per_step: 8}\n").unwrap();
    }

    #[test]
    fn formatting() {
        assert_eq!(fmt_dur(72_000), "01:12");
        assert_eq!(thousands(1234567), "1,234,567");
    }

    #[test]
    fn clock_offset_shifts_now_ms() {
        let before = now_ms();
        set_clock_offset_ms(3_600_000);
        let shifted = now_ms();
        set_clock_offset_ms(0);
        assert!(shifted - before >= 3_599_000, "{before} -> {shifted}");
    }
}

#[cfg(test)]
mod canonical_tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn canonical_sha256_ignores_key_order() {
        let a = json!({"a":1,"b":{"y":2,"x":3}});
        let b = json!({"b":{"x":3,"y":2},"a":1});
        assert_eq!(canonical_sha256(&a), canonical_sha256(&b));
        assert_ne!(
            canonical_sha256(&json!([1, 2])),
            canonical_sha256(&json!([2, 1]))
        );
    }
}

#[cfg(test)]
mod iso_tests {
    #[test]
    fn iso_minute_formats_utc() {
        assert_eq!(super::iso_minute(0), "1970-01-01T00:00Z");
        assert_eq!(super::iso_minute(1_794_733_320_000), "2026-11-15T09:02Z");
        assert_eq!(super::iso_minute(951_782_400_000), "2000-02-29T00:00Z");
    }
}
