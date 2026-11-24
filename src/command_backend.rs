//! Command-backed model providers: `providers.NAME: {kind: command, ...}`
//! runs an owner-configured program (`codex exec`, `claude -p`, a llama.cpp
//! script) as the backend of `NAME/LABEL` model steps.
//!
//! The prompt (instructions, task, and the schema the answer must match) goes
//! to the program's stdin, or into `${prompt}` in its arguments. Its stdout is
//! the answer. Nothing constrains the program's decoder, so the step checks
//! the answer against its schema afterward, exactly as for an HTTP backend:
//! non-JSON or a mismatch makes the step `invalid` (an answer that is
//! exactly one ```` ```json ```` fence is unwrapped first), which `retry` and
//! `on_failure` treat as they do for any model. The program runs without a
//! shell, in its own process group, which is killed on timeout or cancel.
use crate::backend::{ChatReq, ChatResp};
use crate::git_reagent::{run_bounded_launch, Launch};
use crate::reagent::Fail;
use crate::util::{Config, ProviderConfig};
use serde_json::{json, Value};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

/// The most stdout an answer may be.
const MAX_OUTPUT: usize = 4 << 20;
const DEFAULT_TIMEOUT_MS: u64 = 600_000;

fn passes_prompt_on_stdin(p: &ProviderConfig) -> bool {
    p.stdin.as_deref().unwrap_or("prompt") == "prompt"
}

/// Check a command provider when `config.yaml` is loaded.
pub(crate) fn validate(name: &str, p: &ProviderConfig, cfg: &Config) -> Result<(), String> {
    let program = p.program.as_deref().unwrap_or("");
    if program.trim().is_empty() {
        return Err("a command provider needs `program`".into());
    }
    if !p.url.is_empty() || p.api_key_env.is_some() {
        return Err("a command provider takes no `url` or `api_key_env`; pass what the program needs with `env`".into());
    }
    if let Some(cwd) = &p.cwd {
        if !Path::new(cwd).is_absolute() {
            return Err(format!("`cwd` must be an absolute directory, not `{cwd}`"));
        }
    }
    let stdin = p.stdin.as_deref().unwrap_or("prompt");
    if !matches!(stdin, "prompt" | "none") {
        return Err(format!("`stdin` is `prompt` or `none`, not `{stdin}`"));
    }
    let mut prompt_args = 0;
    for a in &p.args {
        let mut rest = a.as_str();
        while let Some(at) = rest.find("${") {
            let after = &rest[at + 2..];
            let end = after.find('}').ok_or_else(|| format!("unclosed `${{` in argument `{a}`"))?;
            match &after[..end] {
                "model" => {}
                "prompt" => prompt_args += 1,
                other => return Err(format!("argument `{a}` names `${{{other}}}`; arguments may use only `${{model}}` and `${{prompt}}`")),
            }
            rest = &after[end + 1..];
        }
    }
    match (stdin, prompt_args) {
        ("prompt", 0) | ("none", 1) => {}
        ("prompt", _) => return Err("with `stdin: prompt` the prompt goes to stdin; remove `${prompt}` from `args` or set `stdin: none`".into()),
        ("none", 0) => return Err("with `stdin: none`, put `${prompt}` in `args` so the program receives the prompt".into()),
        _ => return Err("use `${prompt}` in one argument only".into()),
    }
    if p.timeout_ms == Some(0) {
        return Err("`timeout_ms` must be positive".into());
    }
    if let Some(names) = &p.env {
        // Names only; their values are read when the provider is called.
        for n in names {
            if n.is_empty() || n.contains(['=', '\0']) {
                return Err(format!("invalid `env` name `{n}`; list variable names, not NAME=value"));
            }
            if crate::git_reagent::is_sensitive_env(n, Some(cfg)) {
                return Err(format!("`env` lists `{n}`, a credential Summonr never passes to programs"));
            }
        }
    }
    let _ = name;
    Ok(())
}

/// The one text the program receives: instructions, then the task, then the
/// schema the answer must match.
pub(crate) fn prompt_text(req: &ChatReq) -> String {
    let mut text = String::new();
    if !req.system.trim().is_empty() {
        text.push_str(req.system.trim());
        text.push_str("\n\n");
    }
    text.push_str(req.user);
    if let Some(schema) = req.schema {
        text.push_str("\n\nReply with only one JSON value, and no other text, that matches this JSON schema:\n");
        text.push_str(&schema.to_string());
    }
    text
}

/// The arguments with `${model}` and `${prompt}` filled in. A value that
/// would begin an argument with `-` is refused, so neither a label nor a
/// prompt can turn into an option of the program.
fn render_args(name: &str, label: &str, p: &ProviderConfig, prompt: Option<&str>) -> Result<Vec<String>, String> {
    if label.contains(['\n', '\r', '\0']) {
        return Err(format!("model label `{label}` for provider `{name}` cannot contain line breaks"));
    }
    let mut out = Vec::with_capacity(p.args.len());
    for a in &p.args {
        let prompt = prompt.unwrap_or("${prompt}");
        if a.contains("${prompt}") && prompt.contains(['\n', '\r', '\0']) {
                return Err(format!(
                    "provider `{name}` passes the prompt as an argument, which cannot span lines; \
                     this step's prompt has several lines (its schema and instructions are part of it). \
                     Set `stdin: prompt` if the program can read the prompt from standard input"
                ));
        }
        let mut s = String::new();
        let mut rest = a.as_str();
        while let Some(start) = rest.find("${") {
            let after = &rest[start + 2..];
            let Some(end) = after.find('}') else { break };
            s.push_str(&rest[..start]);
            match &after[..end] {
                "model" => s.push_str(label),
                "prompt" => s.push_str(prompt),
                _ => s.push_str(&rest[start..start + 3 + end]),
            }
            rest = &after[end + 1..];
        }
        s.push_str(rest);
        if s.starts_with('-') && !a.starts_with('-') {
            return Err(format!("provider `{name}`: argument `{a}` would begin with `-` (`{}`), which the program could read as an option", s.chars().take(40).collect::<String>()));
        }
        out.push(s);
    }
    Ok(out)
}

fn cwd_of(p: &ProviderConfig) -> PathBuf {
    p.cwd.as_ref().map(PathBuf::from).unwrap_or_else(std::env::temp_dir)
}

/// The request as recorded before dispatch: the program, its arguments with
/// the prompt left as `${prompt}`, and the prompt text.
pub(crate) fn payload(name: &str, label: &str, p: &ProviderConfig, req: &ChatReq) -> Result<Value, String> {
    let prompt = prompt_text(req);
    Ok(json!({
        "backend": name,
        "kind": "command",
        "model": label,
        "program": p.program,
        "args": render_args(name, label, p, None)?,
        "stdin": if passes_prompt_on_stdin(p) { "prompt" } else { "none" },
        "prompt": prompt,
    }))
}

/// What identifies the backend without running it: the configured command.
/// A change to the program, its arguments, directory, environment names or
/// prompt delivery changes the digest; the program's own version does not
/// (it is not asked).
pub(crate) fn fingerprint(name: &str, label: &str, p: &ProviderConfig) -> Value {
    let described = json!({
        "program": p.program, "args": p.args, "cwd": p.cwd, "env": p.env,
        "stdin": if passes_prompt_on_stdin(p) { "prompt" } else { "none" },
    });
    json!({
        "backend": name,
        "model": label,
        "kind": "command",
        "digest": format!("command:{}", &crate::util::sha256_hex(described.to_string().as_bytes())[..16]),
    })
}

/// The JSON inside an answer that is one Markdown code fence and nothing else
/// (```` ```json ```` or a bare ```` ``` ````), which agent CLIs often print
/// even when told not to. Prose around the fence, a second fence, or another
/// language tag is left alone for the schema check to refuse.
fn unfence(text: &str) -> Option<&str> {
    let body = text.trim().strip_prefix("```")?.strip_suffix("```")?;
    let (tag, inner) = body.split_once('\n')?;
    if !matches!(tag.trim(), "" | "json" | "JSON") || inner.contains("```") {
        return None;
    }
    Some(inner.trim_end_matches(['\n', '\r']))
}

fn estimate_tokens(text: &str) -> u64 {
    (text.len() as u64).div_ceil(4)
}

/// Run the program for one model call.
pub(crate) fn chat(name: &str, label: &str, p: &ProviderConfig, cfg: Option<&Config>, req: ChatReq) -> Result<ChatResp, String> {
    let fallback = Config::default();
    let cfg = cfg.unwrap_or(&fallback);
    let prompt = prompt_text(&req);
    let payload = payload(name, label, p, &req)?;
    let on_stdin = passes_prompt_on_stdin(p);
    let args = render_args(name, label, p, (!on_stdin).then_some(prompt.as_str()))?;
    let env = match &p.env {
        Some(names) => Some(crate::cmd_reagent::resolve_env(name, names, cfg).map_err(|e| e.message())?),
        None => None,
    };
    let mut limit = Duration::from_millis(p.timeout_ms.unwrap_or(DEFAULT_TIMEOUT_MS));
    if let Some(t) = req.timeout {
        limit = limit.min(t);
    }
    let started = Instant::now();
    let program = p.program.as_deref().unwrap_or_default();
    let arg_refs: Vec<&str> = args.iter().map(String::as_str).collect();
    let launch = Launch { env, stdin: on_stdin.then(|| prompt.clone()) };
    let (code, stdout, stderr) = run_bounded_launch(program, &arg_refs, &cwd_of(p), Some(started + limit), MAX_OUTPUT + 1, Some(cfg), &launch)
        .map_err(|f| match f {
            Fail::Exhausted(_) => format!("provider `{name}` ran past its time limit of {}ms (process group killed)", limit.as_millis()),
            Fail::Cancelled => format!("provider `{name}` stopped: the run was cancelled (process group killed)"),
            other => format!("provider `{name}`: {}", other.message()),
        })?;
    if code != 0 {
        let tail: String = stderr.trim().lines().rev().take(5).collect::<Vec<_>>().into_iter().rev().collect::<Vec<_>>().join(" | ");
        return Err(format!("provider `{name}` exited with code {code}: {}", if tail.is_empty() { "(no stderr)" } else { &tail }));
    }
    if stdout.len() > MAX_OUTPUT {
        return Err(format!("provider `{name}` wrote more than {} MiB to stdout", MAX_OUTPUT >> 20));
    }
    let mut content = stdout.trim_end_matches(['\n', '\r']).to_string();
    if req.schema.is_some() {
        if let Some(inner) = unfence(&content) {
            content = inner.to_string();
        }
    }
    let total_ms = started.elapsed().as_millis() as u64;
    Ok(ChatResp {
        prompt_tokens: estimate_tokens(&prompt),
        output_tokens: estimate_tokens(&content),
        content,
        done_reason: None,
        load_ms: 0,
        prompt_ms: 0,
        eval_ms: total_ms,
        total_ms,
        payload,
        constrained: false,
        token_confidence: "estimated",
        provider_cost_microusd: None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;
    use std::os::unix::fs::PermissionsExt;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::Arc;

    #[test]
    fn rendered_arguments_preserve_literal_placeholder_text_in_values() {
        let p = ProviderConfig {
            args: vec!["${model}/${prompt}".into()],
            ..Default::default()
        };
        assert_eq!(
            render_args("local", "literal-${prompt}", &p, Some("hello")).unwrap(),
            ["literal-${prompt}/hello"],
        );
    }

    fn script(dir: &Path, name: &str, body: &str) -> String {
        let path = dir.join(name);
        std::fs::write(&path, format!("#!/bin/sh\n{body}\n")).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
        path.to_str().unwrap().to_string()
    }

    fn provider(program: &str) -> ProviderConfig {
        ProviderConfig { kind: "command".into(), program: Some(program.into()), ..Default::default() }
    }

    fn ask(p: &ProviderConfig, schema: Option<&Value>, user: &str) -> Result<ChatResp, String> {
        let settings = BTreeMap::new();
        let req = ChatReq {
            model: "default", system: "Be brief.", user, schema, settings: &settings,
            timeout: None, max_output_tokens: None, images: &[],
        };
        chat("mycli", "default", p, None, req)
    }

    trait Errs {
        fn err_text(self) -> String;
    }
    impl Errs for Result<ChatResp, String> {
        fn err_text(self) -> String {
            match self {
                Ok(r) => panic!("expected an error, got {}", r.content),
                Err(e) => e,
            }
        }
    }

    fn tmp() -> PathBuf {
        let d = std::env::temp_dir().join(crate::util::new_id("summonr-cmdmodel"));
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn the_prompt_goes_to_stdin_and_stdout_is_the_answer() {
        let d = tmp();
        let schema = json!({"type": "object", "required": ["label"], "properties": {"label": {"type": "string"}}});
        let echo = provider(&script(&d, "echo.sh", "cat"));
        let r = ask(&echo, Some(&schema), "Classify: hello").unwrap();
        assert!(r.content.starts_with("Be brief.\n\nClassify: hello\n\nReply with only one JSON value"), "{}", r.content);
        assert!(r.content.ends_with(&schema.to_string()));
        assert!(!r.constrained);
        assert_eq!(r.token_confidence, "estimated");
        assert!(r.prompt_tokens > 0 && r.output_tokens > 0);
        assert_eq!(r.payload["kind"], "command");
        assert_eq!(r.payload["prompt"], r.content.as_str());
        let answer = provider(&script(&d, "json.sh", "cat >/dev/null\necho '{\"label\": \"spam\"}'"));
        assert_eq!(ask(&answer, Some(&schema), "x").unwrap().content, "{\"label\": \"spam\"}");
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn an_answer_wrapped_in_one_code_fence_is_unwrapped() {
        let d = tmp();
        let schema = json!({"type": "object"});
        let answer = |name: &str, body: &str| {
            let p = provider(&script(&d, name, &format!("cat >/dev/null\nprintf '%s' '{body}'")));
            ask(&p, Some(&schema), "x").unwrap().content
        };
        assert_eq!(answer("json.sh", "```json\n{\"a\": 1}\n```\n"), "{\"a\": 1}");
        assert_eq!(answer("bare.sh", "\n```\n{\"a\": \"x\\ny\"}\n```"), "{\"a\": \"x\\ny\"}");
        // Prose around the fence, or several fences, stay as they are: the
        // schema check then refuses them.
        assert_eq!(answer("prose.sh", "Here:\n```json\n{}\n```"), "Here:\n```json\n{}\n```");
        assert_eq!(answer("two.sh", "```\n{}\n```\n```\n{}\n```"), "```\n{}\n```\n```\n{}\n```");
        assert_eq!(answer("rust.sh", "```rust\n{}\n```"), "```rust\n{}\n```");
        // Without a schema the answer is text and is left alone.
        let p = provider(&script(&d, "text.sh", "cat >/dev/null\nprintf '%s' '```\nhi\n```'"));
        assert_eq!(ask(&p, None, "x").unwrap().content, "```\nhi\n```");
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn arguments_carry_the_label_and_a_one_line_prompt_but_never_an_option() {
        let d = tmp();
        let mut p = provider(&script(&d, "args.sh", "printf '%s|%s' \"$1\" \"$2\""));
        p.args = vec!["-m=${model}".into(), "${prompt}".into()];
        p.stdin = Some("none".into());
        let settings = BTreeMap::new();
        let req = |user: &'static str| ChatReq { model: "m", system: "", user, schema: None, settings: &settings, timeout: None, max_output_tokens: None, images: &[] };
        assert_eq!(chat("c", "fast", &p, None, req("say hi")).unwrap().content, "-m=fast|say hi");
        assert!(chat("c", "fast", &p, None, req("two\nlines")).err_text().contains("cannot span lines"));
        assert!(chat("c", "fast", &p, None, req("--yolo")).err_text().contains("option"));
        // The recorded payload never carries the prompt inside the arguments.
        assert_eq!(payload("c", "fast", &p, &req("say hi")).unwrap()["args"], json!(["-m=fast", "${prompt}"]));
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn a_failing_or_slow_program_is_an_error_with_its_stderr() {
        let d = tmp();
        let fail = provider(&script(&d, "fail.sh", "echo 'not logged in' >&2\nexit 3"));
        let e = ask(&fail, None, "x").err_text();
        assert!(e.contains("exited with code 3") && e.contains("not logged in"), "{e}");
        let mut slow = provider(&script(&d, "slow.sh", "sleep 30"));
        slow.timeout_ms = Some(200);
        let started = Instant::now();
        assert!(ask(&slow, None, "x").err_text().contains("time limit"));
        assert!(started.elapsed() < Duration::from_secs(5));
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn cancelling_kills_the_whole_process_group() {
        let d = tmp();
        let pidfile = d.join("child.pid");
        let p = provider(&script(&d, "spawn.sh", &format!("sleep 60 &\necho $! > {}\nwait", pidfile.display())));
        let flag = Arc::new(AtomicBool::new(false));
        let f2 = flag.clone();
        let worker = std::thread::spawn(move || {
            let _g = crate::cancel::enter(f2);
            ask(&p, None, "x")
        });
        for _ in 0..200 {
            if pidfile.exists() && !std::fs::read_to_string(&pidfile).unwrap().trim().is_empty() {
                break;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        let child: i32 = std::fs::read_to_string(&pidfile).unwrap().trim().parse().unwrap();
        let started = Instant::now();
        flag.store(true, Ordering::SeqCst);
        let e = worker.join().unwrap().err_text();
        assert!(e.contains("cancelled"), "{e}");
        assert!(started.elapsed() < Duration::from_secs(3), "{:?}", started.elapsed());
        // The grandchild in the group is gone too.
        let mut alive = true;
        for _ in 0..100 {
            alive = unsafe { libc::kill(child, 0) } == 0 && !std::fs::read_to_string(format!("/proc/{child}/stat")).unwrap_or_default().contains(") Z ");
            if !alive {
                break;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        assert!(!alive, "process {child} survived the cancel");
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn the_environment_is_an_allowlist_without_credentials() {
        let d = tmp();
        std::env::set_var("SUMMONR_TEST_CMDMODEL_VAR", "visible");
        let mut p = provider(&script(&d, "env.sh", "cat >/dev/null\nprintf '%s|%s' \"$SUMMONR_TEST_CMDMODEL_VAR\" \"$HOME\""));
        p.env = Some(vec!["SUMMONR_TEST_CMDMODEL_VAR".into()]);
        assert_eq!(ask(&p, None, "x").unwrap().content, "visible|");
        let cfg = Config::default();
        p.env = Some(vec!["OPENAI_API_KEY".into()]);
        assert!(validate("c", &p, &cfg).unwrap_err().contains("credential"));
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn configuration_is_checked_and_fingerprinted_without_running() {
        let cfg = Config::default();
        let ok = provider("/nonexistent/runner");
        validate("c", &ok, &cfg).unwrap();
        let bad = |f: &dyn Fn(&mut ProviderConfig), why: &str| {
            let mut p = provider("/bin/true");
            f(&mut p);
            let e = validate("c", &p, &cfg).unwrap_err();
            assert!(e.contains(why), "{why}: {e}");
        };
        bad(&|p| p.program = None, "needs `program`");
        bad(&|p| p.url = "http://x".into(), "no `url`");
        bad(&|p| p.cwd = Some("rel".into()), "absolute");
        bad(&|p| p.stdin = Some("file".into()), "`prompt` or `none`");
        bad(&|p| p.args = vec!["${prompt}".into()], "remove `${prompt}`");
        bad(&|p| p.stdin = Some("none".into()), "put `${prompt}`");
        bad(&|p| p.args = vec!["${secret}".into()], "only `${model}`");
        // The digest follows the configured command; the program is never run.
        let a = fingerprint("c", "x", &ok);
        let mut changed = ok.clone();
        changed.args = vec!["--fast".into()];
        assert_eq!(a, fingerprint("c", "x", &ok));
        assert_ne!(a["digest"], fingerprint("c", "x", &changed)["digest"]);
        assert_eq!(a["kind"], "command");
    }
}
