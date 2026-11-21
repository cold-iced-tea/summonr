//! The `trial` reagent: run a repository's test command, as the owner wrote
//! it in config.yaml, split into words and never given to a shell.

use crate::git_reagent::{checkout_path, config, repo, run_bounded_merged};
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

/// The last `n` lines, then at most the last `TAIL_BYTES` of those, cut on a
/// character boundary. The end of a test log is where the failure is.
fn tail(text: &str, n: usize) -> String {
    let lines: Vec<&str> = text.trim_end().lines().collect();
    let joined = lines[lines.len().saturating_sub(n)..].join("\n");
    let mut start = joined.len().saturating_sub(TAIL_BYTES);
    while !joined.is_char_boundary(start) {
        start += 1;
    }
    joined[start..].to_string()
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
        match run_bounded_merged(words[0], &words[1..], &cwd, auth.deadline) {
            Ok((code, out)) => Ok(json!({"passed": code == 0, "code": code, "tail": tail(&out, 30)})),
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
        let text = "é".repeat(TAIL_BYTES);
        let t = tail(&text, 30);
        assert!(t.len() <= TAIL_BYTES && t.chars().all(|c| c == 'é'));
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
