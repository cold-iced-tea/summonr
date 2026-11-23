//! The `cmd` reagent: execute owner-defined scoped commands from `config.yaml`
//! (`commands:`) with validated parameter bindings, bounded output, process-group
//! timeout enforcement, and optional resource digests for held writes.
//! Never invokes a shell.

use crate::file_reagent::validate_root;
use crate::git_reagent::{is_sensitive_env, run_bounded_launch, Launch};

/// The most output a command keeps (its end); the rest is read and dropped.
const KEEP_BYTES: usize = 1 << 20;
use crate::reagent::{self, Authority, Fail, Reagent, ToolSpec};
use crate::util::{sha256_hex, CommandConfig, Config};
use serde_json::{json, Value};
use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;
use std::time::Instant;

pub struct Cmd;

const MAX_OUTPUT_CHARS: usize = 65_536;

pub(crate) const TOOLS: &[ToolSpec] = &[
    ToolSpec {
        reagent: "cmd",
        name: "list",
        effect: "read",
        determinism: "deterministic over live state",
        description: "List the configured command names, effects, and parameter names",
    },
    ToolSpec {
        reagent: "cmd",
        name: "read",
        effect: "read",
        determinism: "deterministic over live state",
        description: "Run a configured read-only command with validated parameters; never uses a shell",
    },
    ToolSpec {
        reagent: "cmd",
        name: "write",
        effect: "write",
        determinism: "deterministic over live state",
        description: "Run a configured write command with validated parameters; never uses a shell",
    },
];

fn tail(text: &str, n: usize) -> String {
    let lines: Vec<&str> = text.lines().collect();
    lines[lines.len().saturating_sub(n)..].join("\n")
}

fn bounded_text(text: &str) -> String {
    let trimmed = text.trim_end_matches(['\r', '\n']);
    if trimmed.chars().count() > MAX_OUTPUT_CHARS {
        trimmed.chars().take(MAX_OUTPUT_CHARS).collect()
    } else {
        trimmed.to_string()
    }
}

/// Extract `${param}` names from a single argument template.
fn template_params(arg: &str) -> Result<Vec<String>, Fail> {
    let mut out = Vec::new();
    let mut rest = arg;
    while let Some(start) = rest.find("${") {
        let after = &rest[start + 2..];
        let end = after.find('}').ok_or_else(|| {
            Fail::Invalid(format!("unterminated `${{` in command arg template `{arg}`"))
        })?;
        let name = after[..end].trim();
        if name.is_empty()
            || !name
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
        {
            return Err(Fail::Invalid(format!(
                "invalid parameter placeholder `${{{name}}}` in command arg template `{arg}`"
            )));
        }
        out.push(name.to_string());
        rest = &after[end + 1..];
    }
    Ok(out)
}

fn scalar_param(v: &Value, name: &str) -> Result<String, Fail> {
    match v {
        Value::String(s) => Ok(s.clone()),
        Value::Number(n) if n.is_i64() || n.is_u64() => Ok(n.to_string()),
        Value::Bool(b) => Ok(b.to_string()),
        _ => Err(Fail::Invalid(format!(
            "parameter `{name}` must be a string, integer, or boolean"
        ))),
    }
}

/// Validate the configured command and render its argument vector from `params`.
/// A configured command ready to run.
struct Prepared<'a> {
    name: &'a str,
    cfg: &'a CommandConfig,
    args: Vec<String>,
    launch: Launch,
}

impl Prepared<'_> {
    fn run(&self, deadline: Option<Instant>, cfg: &Config) -> Result<(i32, String, String), Fail> {
        let arg_refs: Vec<&str> = self.args.iter().map(String::as_str).collect();
        run_bounded_launch(&self.cfg.program, &arg_refs, Path::new(&self.cfg.cwd), deadline, KEEP_BYTES, Some(cfg), &self.launch)
    }
}

/// A command's `env:` list, resolved from the core's environment.
fn resolve_env(cmd_name: &str, names: &[String], cfg: &Config) -> Result<Vec<(String, String)>, Fail> {
    let mut out = Vec::with_capacity(names.len());
    for name in names {
        if name.is_empty() || name.contains(['=', '\0']) {
            return Err(Fail::Invalid(format!(
                "command `{cmd_name}` has an invalid `env` name `{name}` in config.yaml; list variable names, not NAME=value"
            )));
        }
        if is_sensitive_env(name, Some(cfg)) {
            return Err(Fail::Invalid(format!(
                "command `{cmd_name}` lists `{name}` in `env`, a credential Summonr never passes to commands"
            )));
        }
        let Some(value) = std::env::var_os(name) else {
            return Err(Fail::Failed(format!(
                "command `{cmd_name}` needs `{name}`, which is not set in the core's environment (the core keeps the environment it was started with; set it there and run `summonr core restart`)"
            )));
        };
        out.push((name.clone(), value.to_string_lossy().into_owned()));
    }
    Ok(out)
}

fn prepare_command<'a>(
    cfg: &'a Config,
    tool: &str,
    args: &'a Value,
) -> Result<Prepared<'a>, Fail> {
    let cmd_name = args["command"]
        .as_str()
        .ok_or_else(|| Fail::Invalid("`command` is required".into()))?;
    let cmd_cfg = cfg.commands.get(cmd_name).ok_or_else(|| {
        Fail::Denied(format!(
            "`{cmd_name}` is not a configured command in config.yaml `commands:`"
        ))
    })?;
    if !matches!(cmd_cfg.effect.as_str(), "read" | "write") {
        return Err(Fail::Invalid(format!(
            "command `{cmd_name}` has invalid effect `{}` in config.yaml; use `read` or `write`",
            cmd_cfg.effect
        )));
    }
    if cmd_cfg.effect != tool {
        return Err(Fail::Denied(format!(
            "command `{cmd_name}` is configured with effect `{}`, not `{tool}`; use cmd.{}",
            cmd_cfg.effect, cmd_cfg.effect
        )));
    }
    if cmd_cfg.program.trim().is_empty()
        || cmd_cfg.program.contains('\0')
        || cmd_cfg.program.contains('\n')
    {
        return Err(Fail::Invalid(format!(
            "command `{cmd_name}` has an invalid `program` in config.yaml"
        )));
    }
    validate_root(&cmd_cfg.cwd)
        .map_err(|e| Fail::Denied(format!("command `{cmd_name}` cwd: {e}")))?;

    let mut needed = BTreeSet::new();
    for tpl in &cmd_cfg.args {
        for p in template_params(tpl)? {
            needed.insert(p);
        }
    }
    let stdin_param = cmd_cfg.stdin.as_deref();
    if let Some(p) = stdin_param {
        if needed.contains(p) {
            return Err(Fail::Invalid(format!(
                "command `{cmd_name}` uses `{p}` both as stdin and in `args`; use one"
            )));
        }
        needed.insert(p.to_string());
    }
    let supplied = match args.get("params") {
        None | Some(Value::Null) => serde_json::Map::new(),
        Some(Value::Object(m)) => m.clone(),
        Some(_) => return Err(Fail::Invalid("`params` must be an object".into())),
    };
    for k in supplied.keys() {
        if !needed.contains(k) && !cmd_cfg.params.contains_key(k) {
            return Err(Fail::Invalid(format!(
                "command `{cmd_name}` has no parameter `{k}`"
            )));
        }
    }
    let mut validated: BTreeMap<String, String> = BTreeMap::new();
    for name in needed.union(&cmd_cfg.params.keys().cloned().collect()) {
        let Some(raw) = supplied.get(name) else {
            if needed.contains(name) {
                return Err(Fail::Invalid(format!(
                    "command `{cmd_name}` requires parameter `{name}`"
                )));
            }
            continue;
        };
        let val = scalar_param(raw, name)?;
        // Only stdin may span lines; NUL is never allowed.
        let multi_line = stdin_param == Some(name.as_str());
        if val.contains('\0') || (!multi_line && (val.contains('\n') || val.contains('\r'))) {
            return Err(Fail::Denied(format!(
                "parameter `{name}` may not contain NUL or newline characters"
            )));
        }
        let spec = cmd_cfg.params.get(name);
        let max_len = spec.and_then(|s| s.max_length).unwrap_or(1024);
        if val.chars().count() > max_len {
            return Err(Fail::Invalid(format!(
                "parameter `{name}` exceeds max_length of {max_len}"
            )));
        }
        if let Some(options) = spec.and_then(|s| s.options.as_ref()) {
            if !options.contains(&val) {
                return Err(Fail::Denied(format!(
                    "parameter `{name}` value `{val}` is not in allowed enum {options:?}"
                )));
            }
        }
        validated.insert(name.clone(), val);
    }

    let mut rendered_args = Vec::with_capacity(cmd_cfg.args.len());
    for tpl in &cmd_cfg.args {
        let mut out = String::new();
        let mut rest = tpl.as_str();
        let starts_with_placeholder = rest.starts_with("${");
        while let Some(start) = rest.find("${") {
            let after = &rest[start + 2..];
            let end = after.find('}').unwrap();
            let pname = after[..end].trim();
            let val = &validated[pname];
            let allow_dash = cmd_cfg
                .params
                .get(pname)
                .is_some_and(|s| s.allow_leading_dash);
            if start == 0 && starts_with_placeholder && val.starts_with('-') && !allow_dash {
                return Err(Fail::Denied(format!(
                    "parameter `{pname}` may not start with `-` when used at the start of an argument"
                )));
            }
            out.push_str(&rest[..start]);
            out.push_str(val);
            rest = &after[end + 1..];
        }
        out.push_str(rest);
        rendered_args.push(out);
    }

    if tool == "write" {
        if let Some(ref dname) = cmd_cfg.digest_command {
            let dcfg = cfg.commands.get(dname).ok_or_else(|| {
                Fail::Invalid(format!(
                    "command `{cmd_name}` names unknown digest_command `{dname}`"
                ))
            })?;
            if dcfg.effect != "read" {
                return Err(Fail::Invalid(format!(
                    "digest_command `{dname}` must have effect `read`"
                )));
            }
        }
    }

    let env = match &cmd_cfg.env {
        Some(names) => Some(resolve_env(cmd_name, names, cfg)?),
        None => None,
    };
    let stdin = stdin_param.map(|p| validated[p].clone());
    Ok(Prepared {
        name: cmd_name,
        cfg: cmd_cfg,
        args: rendered_args,
        launch: Launch { env, stdin },
    })
}

/// Compute a resource digest for a `cmd.write` operation when its configured
/// command specifies `digest_command`.
pub fn target_digest(
    cfg: Option<&Config>,
    args: &Value,
    deadline: Option<Instant>,
) -> Result<Option<String>, Fail> {
    let Some(cfg) = cfg else {
        return Ok(None);
    };
    let cmd_cfg = prepare_command(cfg, "write", args)?.cfg;
    let Some(ref dname) = cmd_cfg.digest_command else {
        return Ok(None);
    };
    let dcfg = cfg.commands.get(dname).ok_or_else(|| {
        Fail::Invalid(format!("unknown digest_command `{dname}`"))
    })?;
    let mut dneeded = BTreeSet::new();
    for tpl in &dcfg.args {
        for p in template_params(tpl)? {
            dneeded.insert(p);
        }
    }
    dneeded.extend(dcfg.params.keys().cloned());
    let mut dparams = serde_json::Map::new();
    if let Some(m) = args.get("params").and_then(Value::as_object) {
        for (k, v) in m {
            if dneeded.contains(k) {
                dparams.insert(k.clone(), v.clone());
            }
        }
    }
    let dargs = json!({"command": dname, "params": Value::Object(dparams)});
    let digest = prepare_command(cfg, "read", &dargs)?;
    let (code, out, err) = digest.run(deadline, cfg)?;
    if code != 0 && !digest.cfg.allow_nonzero {
        let last = err
            .lines()
            .rev()
            .find(|l| !l.trim().is_empty())
            .unwrap_or("")
            .trim();
        return Err(Fail::Failed(format!(
            "digest_command `{dname}` failed (exit {code}): {last}"
        )));
    }
    let payload = format!("code={code}\nstdout={out}\nstderr={err}");
    Ok(Some(sha256_hex(payload.as_bytes())))
}

impl Reagent for Cmd {
    fn name(&self) -> &'static str {
        "cmd"
    }
    fn tools(&self) -> &'static [ToolSpec] {
        TOOLS
    }
    fn schema(&self, name: &str) -> Option<Value> {
        match name {
            "list" => Some(json!({
                "type": "object",
                "properties": {},
                "required": [],
                "additionalProperties": false
            })),
            "read" | "write" => Some(json!({
                "type": "object",
                "properties": {
                    "command": {"type": "string", "minLength": 1, "maxLength": 64},
                    "params": {"type": "object"}
                },
                "required": ["command"],
                "additionalProperties": false
            })),
            _ => None,
        }
    }
    fn authorize(
        &self,
        _: Option<&str>,
        name: &str,
        args: &Value,
        auth: &Authority,
    ) -> Result<(), Fail> {
        let cfg = auth
            .config
            .ok_or_else(|| Fail::Denied("no commands are configured".into()))?;
        match name {
            "list" => reagent::require(auth, reagent::need("cmd", "list", &[], &[])),
            "read" | "write" => {
                let cmd_name = prepare_command(cfg, name, args)?.name;
                reagent::require(
                    auth,
                    reagent::need("cmd", name, &[("command", cmd_name)], &[]),
                )
            }
            other => Err(Fail::Failed(format!("reagent `cmd` has no tool `{other}`"))),
        }
    }
    fn call(
        &self,
        root: Option<&str>,
        name: &str,
        args: &Value,
        auth: &Authority,
    ) -> Result<Value, Fail> {
        self.authorize(root, name, args, auth)?;
        let cfg = auth.config.expect("authorize checked config");
        match name {
            "list" => {
                let list: Vec<Value> = cfg
                    .commands
                    .iter()
                    .map(|(k, c)| {
                        let mut params = BTreeSet::new();
                        for tpl in &c.args {
                            if let Ok(ps) = template_params(tpl) {
                                params.extend(ps);
                            }
                        }
                        params.extend(c.params.keys().cloned());
                        json!({
                            "command": k,
                            "effect": c.effect,
                            "params": params.into_iter().collect::<Vec<_>>()
                        })
                    })
                    .collect();
                Ok(Value::Array(list))
            }
            "read" | "write" => {
                let prepared = prepare_command(cfg, name, args)?;
                let (cmd_name, cmd_cfg) = (prepared.name, prepared.cfg);
                if name == "write" {
                    if let Some(want) = auth.expected_digest {
                        if target_digest(Some(cfg), args, auth.deadline)?.as_deref() != Some(want) {
                            return Err(Fail::Denied(format!(
                                "resource state for command `{cmd_name}` changed after its petition was filed"
                            )));
                        }
                    }
                }
                match prepared.run(auth.deadline, cfg) {
                    Ok((code, out, err)) => {
                        if code != 0 && !cmd_cfg.allow_nonzero {
                            // Both streams' ends, as trial reports them: the
                            // cause is usually on stderr, not the last line.
                            return Err(Fail::Failed(format!(
                                "cmd {cmd_name} (exit {code}): {}",
                                crate::trial_reagent::report(&out, &err)
                            )));
                        }
                        let stdout = bounded_text(&out);
                        let stderr = bounded_text(&err);
                        let parsed_json: Value = {
                            let t = stdout.trim();
                            if (t.starts_with('{') && t.ends_with('}'))
                                || (t.starts_with('[') && t.ends_with(']'))
                            {
                                serde_json::from_str(t).unwrap_or(Value::Null)
                            } else {
                                Value::Null
                            }
                        };
                        Ok(json!({
                            "command": cmd_name,
                            "passed": code == 0,
                            "code": code,
                            "stdout": stdout,
                            "stderr": stderr,
                            "tail": tail(&format!("{out}{err}"), 30),
                            "json": parsed_json,
                        }))
                    }
                    Err(e) => Err(e),
                }
            }
            other => Err(Fail::Failed(format!("reagent `cmd` has no tool `{other}`"))),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::util::{new_id, CommandParam};
    use std::collections::HashSet;
    use std::sync::Mutex;

    fn test_cfg(dir: &Path) -> Config {
        let mut commands = BTreeMap::new();
        commands.insert(
            "echo-json".into(),
            CommandConfig {
                program: "printf".into(),
                args: vec![r#"{"service":"%s","ok":true}"#.into(), "${service}".into()],
                cwd: dir.to_string_lossy().into_owned(),
                effect: "read".into(),
                params: BTreeMap::from([(
                    "service".into(),
                    CommandParam {
                        options: Some(vec!["api".into(), "worker".into()]),
                        max_length: Some(32),
                        allow_leading_dash: false,
                    },
                )]),
                allow_nonzero: false,
                digest_command: None,
                env: None,
                stdin: None,
            },
        );
        commands.insert(
            "read-state".into(),
            CommandConfig {
                program: "cat".into(),
                args: vec!["state.txt".into()],
                cwd: dir.to_string_lossy().into_owned(),
                effect: "read".into(),
                params: BTreeMap::new(),
                allow_nonzero: false,
                digest_command: None,
                env: None,
                stdin: None,
            },
        );
        commands.insert(
            "write-state".into(),
            CommandConfig {
                program: "sh".into(),
                args: vec![
                    "-c".into(),
                    "printf '%s' \"$1\" > state.txt".into(),
                    "sh".into(),
                    "${value}".into(),
                ],
                cwd: dir.to_string_lossy().into_owned(),
                effect: "write".into(),
                params: BTreeMap::new(),
                allow_nonzero: false,
                digest_command: Some("read-state".into()),
                env: None,
                stdin: None,
            },
        );
        Config {
            commands,
            ..Config::default()
        }
    }

    /// Run one configured command (given as config.yaml text) with `params`.
    fn run_one(yaml: &str, params: Value) -> Result<Value, Fail> {
        let dir = std::env::temp_dir().join(new_id("summonr-cmd"));
        std::fs::create_dir_all(&dir).unwrap();
        let text = yaml.replace("CWD", &dir.to_string_lossy());
        let cmd: CommandConfig = serde_yaml::from_str(&text).unwrap();
        let cfg = Config {
            commands: BTreeMap::from([("c".to_string(), cmd)]),
            ..Config::default()
        };
        let grants = vec![reagent::need("cmd", "read", &[("command", "c")], &[])];
        let selected = Mutex::new(HashSet::new());
        let auth = Authority {
            grants: &grants,
            deadline: None,
            on_item: None,
            selected: &selected,
            expected_digest: None,
            config: Some(&cfg),
        };
        let out = Cmd.call(None, "read", &json!({"command": "c", "params": params}), &auth);
        std::fs::remove_dir_all(&dir).ok();
        out
    }

    #[test]
    fn a_failed_command_reports_its_exit_code_and_both_tails() {
        let err = run_one(
            "program: sh\nargs: ['-c', 'echo progress; echo boom-on-stderr >&2; echo last-stdout; exit 3']\ncwd: CWD\n",
            json!({}),
        )
        .unwrap_err();
        let msg = err.message();
        assert!(matches!(err, Fail::Failed(_)), "{err:?}");
        assert!(msg.contains("exit 3"), "{msg}");
        assert!(msg.contains("boom-on-stderr"), "{msg}");
        assert!(msg.contains("last-stdout"), "{msg}");
        assert!(msg.len() <= 1024, "{} bytes", msg.len());
    }

    #[test]
    fn env_passes_exactly_the_listed_variables() {
        std::env::set_var("SUMMONR_CMD_TEST_KEEP", "kept");
        std::env::set_var("SUMMONR_CMD_TEST_DROP", "dropped");
        let out = run_one(
            "program: sh\nargs: ['-c', 'printf \"%s|%s|%s\" \"$SUMMONR_CMD_TEST_KEEP\" \"$SUMMONR_CMD_TEST_DROP\" \"$HOME\"']\ncwd: CWD\nenv: [SUMMONR_CMD_TEST_KEEP]\n",
            json!({}),
        )
        .unwrap();
        assert_eq!(out["stdout"], "kept||");
        let missing = run_one(
            "program: 'true'\ncwd: CWD\nenv: [SUMMONR_CMD_TEST_NEVER_SET]\n",
            json!({}),
        )
        .unwrap_err();
        assert!(
            missing.message().contains("SUMMONR_CMD_TEST_NEVER_SET"),
            "{}",
            missing.message()
        );
        let bad = run_one("program: 'true'\ncwd: CWD\nenv: ['A=B']\n", json!({})).unwrap_err();
        assert!(matches!(bad, Fail::Invalid(_)), "{bad:?}");
    }

    #[test]
    fn stdin_carries_a_multi_line_parameter() {
        let yaml = "program: cat\ncwd: CWD\nstdin: prompt\nparams:\n  prompt: {max_length: 100}\n";
        let out = run_one(yaml, json!({"prompt": "line one\nline two"})).unwrap();
        assert_eq!(out["stdout"], "line one\nline two");
        let too_long = run_one(yaml, json!({"prompt": "x".repeat(101)})).unwrap_err();
        assert!(matches!(too_long, Fail::Invalid(_)), "{too_long:?}");
        // Newlines stay refused in arguments.
        let arg = run_one(
            "program: printf\nargs: ['${p}']\ncwd: CWD\nstdin: prompt\n",
            json!({"p": "a\nb", "prompt": "x"}),
        )
        .unwrap_err();
        assert!(matches!(arg, Fail::Denied(_)), "{arg:?}");
        // The stdin parameter may not also be an argument.
        let both = run_one(
            "program: printf\nargs: ['${prompt}']\ncwd: CWD\nstdin: prompt\n",
            json!({"prompt": "x"}),
        )
        .unwrap_err();
        assert!(matches!(both, Fail::Invalid(_)), "{both:?}");
    }

    #[test]
    fn cmd_read_enforces_grants_enums_and_flag_injection() {
        let dir = std::env::temp_dir().join(new_id("summonr-cmd"));
        std::fs::create_dir_all(&dir).unwrap();
        let cfg = test_cfg(&dir);
        let grants = vec![reagent::need(
            "cmd",
            "read",
            &[("command", "echo-json")],
            &[],
        )];
        let selected = Mutex::new(HashSet::new());
        let auth = Authority {
            grants: &grants,
            deadline: None,
            on_item: None,
            selected: &selected,
            expected_digest: None,
            config: Some(&cfg),
        };
        let out = Cmd
            .call(
                None,
                "read",
                &json!({"command": "echo-json", "params": {"service": "api"}}),
                &auth,
            )
            .unwrap();
        assert_eq!(out["passed"], true);
        assert_eq!(out["json"]["service"], "api");

        // Disallowed enum value is refused.
        let bad_enum = Cmd.call(
            None,
            "read",
            &json!({"command": "echo-json", "params": {"service": "db"}}),
            &auth,
        );
        assert!(matches!(bad_enum, Err(Fail::Denied(_))), "{bad_enum:?}");

        // Extra undeclared parameter is rejected.
        let extra = Cmd.call(
            None,
            "read",
            &json!({"command": "echo-json", "params": {"service": "api", "extra": "x"}}),
            &auth,
        );
        assert!(matches!(extra, Err(Fail::Invalid(_))), "{extra:?}");

        // Calling a write command via cmd.read is refused.
        let wrong_effect = Cmd.call(
            None,
            "read",
            &json!({"command": "write-state", "params": {"value": "v2"}}),
            &auth,
        );
        assert!(
            matches!(wrong_effect, Err(Fail::Denied(_))),
            "{wrong_effect:?}"
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn cmd_write_verifies_resource_digest_against_stale_mutations() {
        let dir = std::env::temp_dir().join(new_id("summonr-cmd-digest"));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("state.txt"), "v1").unwrap();
        let cfg = test_cfg(&dir);
        let args = json!({"command": "write-state", "params": {"value": "v2"}});
        let digest = target_digest(Some(&cfg), &args, None).unwrap().unwrap();

        let grants = vec![reagent::need(
            "cmd",
            "write",
            &[("command", "write-state")],
            &[],
        )];
        let selected = Mutex::new(HashSet::new());

        // Mutate state.txt before permitting -> must refuse as stale!
        std::fs::write(dir.join("state.txt"), "tampered").unwrap();
        let stale_auth = Authority {
            grants: &grants,
            deadline: None,
            on_item: None,
            selected: &selected,
            expected_digest: Some(&digest),
            config: Some(&cfg),
        };
        let res = Cmd.call(None, "write", &args, &stale_auth);
        assert!(matches!(res, Err(Fail::Denied(_))), "{res:?}");
        assert_eq!(std::fs::read_to_string(dir.join("state.txt")).unwrap(), "tampered");

        // Restore v1 -> permit succeeds!
        std::fs::write(dir.join("state.txt"), "v1").unwrap();
        let ok_auth = Authority {
            expected_digest: Some(&digest),
            ..stale_auth
        };
        let out = Cmd.call(None, "write", &args, &ok_auth).unwrap();
        assert_eq!(out["passed"], true);
        assert_eq!(std::fs::read_to_string(dir.join("state.txt")).unwrap(), "v2");
        std::fs::remove_dir_all(&dir).ok();
    }
}
