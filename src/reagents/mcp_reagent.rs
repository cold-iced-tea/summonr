//! MCP-backed reagent client: spawns an owner-configured stdio MCP server
//! (never via a shell), performs the JSON-RPC 2.0 handshake, calls one tool,
//! and tears the process down cleanly.

use crate::util::McpReagentConfig;
use serde_json::{json, Value};
use std::io::{BufRead, BufReader, Read, Write};
use std::process::{Child, Command, Stdio};
use std::sync::mpsc;
use std::time::{Duration, Instant};

const MAX_LINE_BYTES: usize = 4 * 1024 * 1024;
const DEFAULT_TIMEOUT: Duration = Duration::from_secs(30);

struct ChildGuard(Child);

impl Drop for ChildGuard {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

fn spawn_line_reader(stdout: impl std::io::Read + Send + 'static) -> mpsc::Receiver<Result<String, String>> {
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let mut reader = BufReader::new(stdout);
        loop {
            let mut buf = Vec::new();
            let mut limited = (&mut reader).take(MAX_LINE_BYTES as u64 + 1);
            match limited.read_until(b'\n', &mut buf) {
                Ok(0) => {
                    let _ = tx.send(Err("mcp reagent closed stdout before replying".into()));
                    break;
                }
                Ok(n) if n > MAX_LINE_BYTES => {
                    let _ = tx.send(Err("mcp reagent response exceeded 4 MiB limit".into()));
                    break;
                }
                Ok(_) => match String::from_utf8(buf) {
                    Ok(s) => {
                        if tx.send(Ok(s)).is_err() {
                            break;
                        }
                    }
                    Err(e) => {
                        let _ = tx.send(Err(format!("mcp reagent sent non-UTF-8 output: {e}")));
                        break;
                    }
                },
                Err(e) => {
                    let _ = tx.send(Err(format!("mcp reagent read failed: {e}")));
                    break;
                }
            }
        }
    });
    rx
}

fn read_response(
    rx: &mpsc::Receiver<Result<String, String>>,
    expected_id: u64,
    deadline: Instant,
) -> Result<Value, String> {
    loop {
        let now = Instant::now();
        if now >= deadline {
            return Err("mcp reagent timed out".into());
        }
        let line = rx
            .recv_timeout(deadline - now)
            .map_err(|_| "mcp reagent timed out".to_string())??;
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        let msg: Value = serde_json::from_str(trimmed)
            .map_err(|e| format!("mcp reagent sent invalid JSON: {e}"))?;
        if msg.get("id").and_then(Value::as_u64) == Some(expected_id) {
            return Ok(msg);
        }
    }
}

pub fn call_tool(
    cfg: &McpReagentConfig,
    tool_name: &str,
    args: &Value,
    timeout: Option<Duration>,
    root_cfg: Option<&crate::util::Config>,
) -> Result<Value, String> {
    let deadline = Instant::now() + timeout.unwrap_or(DEFAULT_TIMEOUT);
    let (prog, rest) = cfg
        .command
        .split_first()
        .ok_or_else(|| "mcp reagent `command` must not be empty".to_string())?;
    let mut cmd = Command::new(prog);
    crate::git_reagent::strip_sensitive_env(&mut cmd, root_cfg);
    cmd.args(rest)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    if let Some(cwd) = &cfg.cwd {
        cmd.current_dir(cwd);
    }
    if !cfg.env.is_empty() {
        cmd.envs(&cfg.env);
    }
    let child = cmd
        .spawn()
        .map_err(|e| format!("cannot start mcp reagent `{prog}`: {e}"))?;
    let mut guard = ChildGuard(child);
    let mut stdin = guard
        .0
        .stdin
        .take()
        .ok_or_else(|| "mcp reagent stdin unavailable".to_string())?;
    let stdout = guard
        .0
        .stdout
        .take()
        .ok_or_else(|| "mcp reagent stdout unavailable".to_string())?;
    let rx = spawn_line_reader(stdout);

    let init_req = json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "initialize",
        "params": {
            "protocolVersion": "2025-03-26",
            "capabilities": {},
            "clientInfo": {"name": "summonr", "version": env!("CARGO_PKG_VERSION")},
        }
    });
    writeln!(stdin, "{init_req}").map_err(|e| format!("mcp reagent write failed: {e}"))?;
    stdin
        .flush()
        .map_err(|e| format!("mcp reagent flush failed: {e}"))?;
    let init_resp = read_response(&rx, 1, deadline)?;
    if let Some(err) = init_resp.get("error") {
        return Err(format!("mcp reagent initialize failed: {err}"));
    }

    let notif = json!({"jsonrpc": "2.0", "method": "notifications/initialized"});
    writeln!(stdin, "{notif}").map_err(|e| format!("mcp reagent write failed: {e}"))?;

    let call_req = json!({
        "jsonrpc": "2.0",
        "id": 2,
        "method": "tools/call",
        "params": {
            "name": tool_name,
            "arguments": if args.is_null() { json!({}) } else { args.clone() },
        }
    });
    writeln!(stdin, "{call_req}").map_err(|e| format!("mcp reagent write failed: {e}"))?;
    stdin
        .flush()
        .map_err(|e| format!("mcp reagent flush failed: {e}"))?;
    let call_resp = read_response(&rx, 2, deadline)?;
    drop(stdin);

    if let Some(err) = call_resp.get("error") {
        let msg = err["message"]
            .as_str()
            .map(String::from)
            .unwrap_or_else(|| err.to_string());
        return Err(format!("mcp tool `{tool_name}` failed: {msg}"));
    }
    let result = &call_resp["result"];
    if result["isError"].as_bool() == Some(true) {
        let msg = result["content"]
            .as_array()
            .and_then(|a| a.first())
            .and_then(|c| c["text"].as_str())
            .map(String::from)
            .unwrap_or_else(|| result.to_string());
        return Err(format!("mcp tool `{tool_name}` failed: {msg}"));
    }
    if let Some(sc) = result.get("structuredContent").filter(|v| !v.is_null()) {
        return Ok(sc.clone());
    }
    if let Some(text) = result["content"]
        .as_array()
        .and_then(|a| a.first())
        .and_then(|c| c["text"].as_str())
    {
        if let Ok(parsed) = serde_json::from_str::<Value>(text) {
            return Ok(parsed);
        }
        return Ok(Value::String(text.to_string()));
    }
    Ok(result.clone())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::reagent::{self, Authority, Fail};
    use crate::ritual::Grant;
    use crate::util::{Config, McpToolConfig};
    use std::collections::{BTreeMap, HashSet};
    use std::sync::Mutex;

    fn mock_mcp_config() -> Config {
        let script = r#"
import json, sys
for line in sys.stdin:
    req = json.loads(line)
    m = req.get("method")
    if m == "initialize":
        print(json.dumps({"jsonrpc":"2.0","id":req["id"],"result":{"protocolVersion":"2025-03-26","capabilities":{},"serverInfo":{"name":"mock","version":"1.0"}}}), flush=True)
    elif m == "tools/call":
        args = req["params"]["arguments"]
        total = args["a"] + args["b"]
        print(json.dumps({"jsonrpc":"2.0","id":req["id"],"result":{"content":[{"type":"text","text":json.dumps({"sum":total})}],"structuredContent":{"sum":total},"isError":False}}), flush=True)
"#;
        let mut tools = BTreeMap::new();
        tools.insert(
            "add".into(),
            McpToolConfig {
                effect: "read".into(),
                description: Some("Add two numbers".into()),
            },
        );
        let mut mcp_reagents = BTreeMap::new();
        mcp_reagents.insert(
            "calc".into(),
            McpReagentConfig {
                command: vec!["python3".into(), "-c".into(), script.into()],
                cwd: None,
                env: BTreeMap::new(),
                tools,
            },
        );
        Config {
            mcp_reagents,
            ..Default::default()
        }
    }

    #[test]
    fn mcp_grants_cannot_be_scoped() {
        // Nothing maps a scope key to an MCP tool's arguments, so a scoped
        // grant would read as a limit it never enforces.
        let on: BTreeMap<String, String> = [("account".to_string(), "sandbox".to_string())].into();
        let err = reagent::validate_grant_scope("calc", &on, &BTreeMap::new()).unwrap_err();
        assert!(err.contains("cannot be scoped"), "{err}");
        assert!(reagent::validate_grant_scope("calc", &BTreeMap::new(), &on).is_err());
        assert!(reagent::validate_grant_scope("calc", &BTreeMap::new(), &BTreeMap::new()).is_ok());

        // A scoped grant already stored (or narrowed from `requires`) covers nothing.
        let cfg = mock_mcp_config();
        let selected = Mutex::new(HashSet::new());
        let scoped = vec![crate::ritual::Grant {
            reagent: "calc".into(),
            action: "add".into(),
            on,
            into: BTreeMap::new(),
        }];
        let auth = Authority {
            deadline: None,
            on_item: None,
            grants: &scoped,
            selected: &selected,
            expected_digest: None,
            config: Some(&cfg),
        };
        let r = reagent::authorize(None, "calc.add", &json!({"a": 2, "b": 3, "account": "production"}), &auth);
        assert!(matches!(&r, Err(Fail::Denied(m)) if m.contains("scoped")), "{r:?}");
    }

    #[test]
    fn mcp_reagent_enforces_grants_and_calls_stdio_server() {
        let cfg = mock_mcp_config();
        let selected = Mutex::new(HashSet::new());

        // Without grant: denied.
        let no_grants = vec![];
        let auth_denied = Authority {
            deadline: None,
            on_item: None,
            grants: &no_grants,
            selected: &selected,
            expected_digest: None,
            config: Some(&cfg),
        };
        assert!(matches!(
            reagent::authorize(None, "calc.add", &json!({"a": 2, "b": 3}), &auth_denied),
            Err(Fail::Denied(_))
        ));

        // With grant: succeeds and returns structuredContent.
        let grants = vec![Grant {
            reagent: "calc".into(),
            action: "add".into(),
            on: BTreeMap::new(),
            into: BTreeMap::new(),
        }];
        let auth_ok = Authority {
            deadline: None,
            on_item: None,
            grants: &grants,
            selected: &selected,
            expected_digest: None,
            config: Some(&cfg),
        };
        let out = reagent::call(None, "calc", "add", &json!({"a": 2, "b": 3}), &auth_ok).unwrap();
        assert_eq!(out, json!({"sum": 5}));

        // Undeclared tool: refused.
        assert!(reagent::call(None, "calc", "nope", &json!({}), &auth_ok).is_err());

        // Ritual validation via validate_with_cfg accepts configured MCP reagents.
        let def = crate::ritual::parse_file(
            r#"
ritual: use-calc
requires:
  - {reagent: calc, action: add}
steps:
  - id: s
    operation: calc.add
    arguments: {a: 2, b: 3}
return:
  sum: $steps.s.sum
"#,
        )
        .unwrap();
        let none = |_: &str, _: Option<u32>| None;
        crate::ritual::validate_with_cfg(&def, &none, &cfg, 3).unwrap();
    }

    #[test]
    fn hung_mcp_server_times_out_cleanly() {
        let mut tools = BTreeMap::new();
        tools.insert(
            "hang".into(),
            McpToolConfig {
                effect: "read".into(),
                description: None,
            },
        );
        let srv = McpReagentConfig {
            command: vec![
                "python3".into(),
                "-c".into(),
                "import time; time.sleep(10)".into(),
            ],
            cwd: None,
            env: BTreeMap::new(),
            tools,
        };
        let err = call_tool(&srv, "hang", &json!({}), Some(Duration::from_millis(120)), None).unwrap_err();
        assert!(err.contains("timed out") || err.contains("closed stdout"), "{err}");
    }
}
