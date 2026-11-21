//! The `trial` reagent: run a repository's test command, as the owner wrote
//! it in config.yaml, split into words and never given to a shell.

use crate::git_reagent::{checkout_path, config, repo, run_bounded_kept};
use crate::reagent::{self, Authority, Fail, Reagent, ToolSpec};
use serde_json::{json, Value};

pub struct Trial;

const TOOLS: &[ToolSpec] = &[ToolSpec {
    reagent: "trial",
    name: "run",
    effect: "read",
    determinism: "deterministic over live state",
    description: "Run the repository's configured test command in its main checkout or a listed worktree",
}];

/// The most a tail may hold, so it fits in a 1024-byte message with room for a prefix.
pub(crate) const TAIL_BYTES: usize = 900;

/// How much of each stream is kept while the tests run.
const KEEP_BYTES: usize = 64 * 1024;

const STDOUT_MARK: &str = "\n--- stdout ---\n";

/// The last `n` lines, then at most the last `max` bytes of those, cut on a
/// character boundary. The end of a test log is where the failure is.
fn tail_within(text: &str, n: usize, max: usize) -> String {
    let lines: Vec<&str> = text.trim_end().lines().collect();
    let joined = lines[lines.len().saturating_sub(n)..].join("\n");
    let mut start = joined.len().saturating_sub(max);
    while !joined.is_char_boundary(start) {
        start += 1;
    }
    joined[start..].to_string()
}

fn tail(text: &str, n: usize) -> String {
    tail_within(text, n, TAIL_BYTES)
}

/// The end of stderr, then the end of stdout. They are kept apart because
/// their order in time is unknowable: a test runner may buffer stdout until
/// exit, after the failure it already wrote to stderr. Each stream gets half
/// the budget, and whatever one leaves unused goes to the other.
fn report(stdout: &str, stderr: &str) -> String {
    let (out, err) = (stdout.trim(), stderr.trim());
    if err.is_empty() {
        return tail(out, 30);
    }
    if out.is_empty() {
        return tail(err, 30);
    }
    let room = TAIL_BYTES - STDOUT_MARK.len();
    let out_t = tail_within(out, 15, room - tail_within(err, 15, room / 2).len());
    let err_t = tail_within(err, 15, room - out_t.len());
    format!("{err_t}{STDOUT_MARK}{out_t}")
}

impl Reagent for Trial {
    fn name(&self) -> &'static str {
        "trial"
    }
    fn tools(&self) -> &'static [ToolSpec] {
        TOOLS
    }
    fn schema(&self, name: &str) -> Option<Value> {
        (name == "run").then(|| {
            json!({"type": "object", "properties": {"repo": {"type": "string", "minLength": 1, "maxLength": 32},
                   "worktree": {"type": "string", "minLength": 1, "maxLength": 4096}}, "required": ["repo"], "additionalProperties": false})
        })
    }
    fn authorize(&self, _: Option<&str>, _: &str, args: &Value, auth: &Authority) -> Result<(), Fail> {
        let name = args["repo"].as_str().ok_or_else(|| Fail::Invalid("`repo` is required".into()))?;
        let r = repo(auth, name)?;
        if r.test.as_deref().map(str::trim).unwrap_or("").is_empty() {
            return Err(Fail::Invalid(format!("repo {name} has no test command in config.yaml")));
        }
        reagent::require(auth, reagent::need("trial", "run", &[("repo", name)], &[]))
    }
    fn call(&self, root: Option<&str>, name: &str, args: &Value, auth: &Authority) -> Result<Value, Fail> {
        self.authorize(root, name, args, auth)?;
        let cfg = config(auth)?;
        let r = repo(auth, args["repo"].as_str().unwrap_or(""))?;
        let cwd = checkout_path(cfg, r, args, auth.deadline)?;
        let words: Vec<&str> = r.test.as_deref().unwrap_or("").split_whitespace().collect();
        match run_bounded_kept(words[0], &words[1..], &cwd, auth.deadline, KEEP_BYTES, Some(cfg)) {
            Ok((code, out, err)) => Ok(json!({"passed": code == 0, "code": code, "tail": report(&out, &err)})),
            Err(Fail::Exhausted(_)) => Ok(json!({"passed": false, "code": null, "tail": "stopped: the tests ran past the time limit"})),
            Err(e) => Err(e),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::git_fixture::{fixture, Fixture};
    use serde_json::json;
    use std::collections::HashSet;
    use std::sync::Mutex;
    use std::time::{Duration, Instant};

    fn run(f: &Fixture, test: Option<&str>, deadline: Option<Instant>) -> Result<Value, Fail> {
        let mut cfg = f.cfg.clone();
        cfg.repos.get_mut("demo").unwrap().test = test.map(String::from);
        let grants = vec![reagent::need("trial", "run", &[], &[])];
        let selected = Mutex::new(HashSet::new());
        let auth = Authority { grants: &grants, deadline, on_item: None, selected: &selected, expected_digest: None, config: Some(&cfg) };
        Trial.call(None, "run", &json!({"repo": "demo"}), &auth)
    }

    #[test]
    fn trial_passes_and_fails_by_exit_code() {
        let f = fixture();
        assert_eq!(run(&f, Some("true"), None).unwrap()["passed"], true);
        let failed = run(&f, Some("false"), None).unwrap();
        assert_eq!(failed["passed"], false);
        assert_eq!(failed["code"], 1);
    }

    #[test]
    fn trial_tail_keeps_output_order_and_stays_short() {
        let f = fixture();
        std::fs::write(f.main().join("loud.sh"), crate::core::repo_tests::LOUD_FAILURE).unwrap();
        let out = run(&f, Some("sh loud.sh"), None).unwrap();
        assert_eq!(out["code"], 101);
        let tail = out["tail"].as_str().unwrap();
        assert!(tail.ends_with("test widget::spins ... FAILED"), "{tail}");
        assert!(tail.len() <= TAIL_BYTES, "{} bytes", tail.len());
    }

    #[test]
    fn tail_cuts_on_a_character_boundary() {
        // An odd length puts the byte cut in the middle of an `é`.
        let text = "é".repeat(TAIL_BYTES) + "a";
        let t = tail(&text, 30);
        assert!(t.len() <= TAIL_BYTES && t.ends_with('a'), "{} bytes", t.len());
        assert!(t.trim_end_matches('a').chars().all(|c| c == 'é'));
    }

    #[test]
    fn trial_keeps_a_stderr_failure_behind_buffered_stdout() {
        // A buffered test runner's stdout arrives at exit, after the failure
        // it already wrote to stderr.
        let f = fixture();
        let script = "echo 'FAIL: widget spins backwards' >&2\ni=0\nwhile [ $i -lt 40 ]; do echo \"checking case $i ok, with padding to make the line longer\"; i=$((i+1)); done\nexit 1\n";
        std::fs::write(f.main().join("quiet.sh"), script).unwrap();
        let out = run(&f, Some("sh quiet.sh"), None).unwrap();
        let tail = out["tail"].as_str().unwrap();
        assert!(tail.contains("FAIL: widget spins backwards"), "{tail}");
        assert!(tail.contains("checking case 39 ok"), "{tail}");
        assert!(tail.len() <= TAIL_BYTES, "{} bytes", tail.len());
    }

    #[test]
    fn trial_does_not_wait_for_a_child_that_left_its_group() {
        let f = fixture();
        std::fs::write(f.main().join("escape.sh"), "setsid sleep 30 &\necho $! > sleeper.pid\necho tests done\n").unwrap();
        let start = Instant::now();
        let out = run(&f, Some("sh escape.sh"), None).unwrap();
        let took = start.elapsed();
        if let Ok(pid) = std::fs::read_to_string(f.main().join("sleeper.pid")) {
            if let Ok(pid) = pid.trim().parse::<i32>() {
                unsafe { libc::kill(pid, libc::SIGKILL) };
            }
        }
        assert_eq!(out["passed"], true, "{out}");
        assert!(out["tail"].as_str().unwrap().contains("tests done"), "{out}");
        assert!(took < Duration::from_secs(10), "waited {took:?}");
    }

    #[test]
    fn trial_is_killed_at_the_deadline() {
        let f = fixture();
        let start = Instant::now();
        let out = run(&f, Some("sleep 30"), Some(Instant::now() + Duration::from_secs(1))).unwrap();
        assert_eq!(out["passed"], false);
        assert!(out["tail"].as_str().unwrap().contains("time limit"), "{out}");
        assert!(start.elapsed() < Duration::from_secs(3));
    }

    #[test]
    fn trial_without_test_command_is_invalid() {
        let f = fixture();
        let e = run(&f, None, None).unwrap_err();
        assert!(matches!(e, Fail::Invalid(_)) && e.message().contains("no test command"), "{e:?}");
    }

    #[test]
    fn trial_never_uses_a_shell() {
        let f = fixture();
        let out = run(&f, Some("echo $HOME;"), None).unwrap();
        assert!(out["tail"].as_str().unwrap().contains("$HOME;"), "{out}");
    }
}
