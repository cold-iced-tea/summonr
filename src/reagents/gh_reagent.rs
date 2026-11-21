//! The `gh` reagent: find or open a pull request with the user's own `gh`
//! login. Summonr stores no GitHub token.

use crate::git_reagent::{config, repo, run_bounded_kept, valid_ref_name};
use crate::reagent::{self, Authority, Fail, Reagent, ToolSpec};
use serde_json::{json, Value};
use std::path::Path;

pub struct Gh;

const TOOLS: &[ToolSpec] = &[
    ToolSpec { reagent: "gh", name: "pr_for", effect: "read", determinism: "deterministic over live state", description: "The open pull request for a branch, if any" },
    ToolSpec { reagent: "gh", name: "pr_open", effect: "idempotent write", determinism: "deterministic over live state", description: "The open pull request for a branch, opening one into the trunk if there is none" },
    ToolSpec { reagent: "gh", name: "pr_create", effect: "write", determinism: "deterministic over live state", description: "Open a pull request from a branch into the trunk" },
];

fn gh(auth: &Authority, cwd: &Path, args: &[&str]) -> Result<String, Fail> {
    let cfg = config(auth)?;
    let prog = cfg.gh.clone().unwrap_or_else(|| "gh".into());
    let (code, out, err) = run_bounded_kept(&prog, args, cwd, auth.deadline, 1 << 20, Some(cfg))?;
    if code != 0 {
        let last = err.lines().rev().find(|l| !l.trim().is_empty()).unwrap_or("").trim().to_string();
        return Err(Fail::Failed(format!("gh {}: {last}", args.first().unwrap_or(&""))));
    }
    Ok(out)
}

fn open_pr(auth: &Authority, main: &Path, branch: &str) -> Result<Option<String>, Fail> {
    // Only this repository's own branch: a fork's pull request with the same
    // branch name is someone else's.
    let head = format!("--head={branch}");
    let out = gh(auth, main, &["pr", "list", &head, "--state=open", "--json=url,isCrossRepository", "--limit=20"])?;
    let list: Value = serde_json::from_str(out.trim()).map_err(|_| Fail::Failed("gh pr list: unexpected output".into()))?;
    Ok(list
        .as_array()
        .into_iter()
        .flatten()
        .find(|p| p["isCrossRepository"] == false)
        .and_then(|p| p["url"].as_str())
        .map(String::from))
}

fn create_pr(auth: &Authority, main: &Path, trunk: &str, branch: &str, title: &str, body: &str) -> Result<String, Fail> {
    // `--flag=value`, so a value beginning with `-` can never be read as a flag.
    let (b, hd, t, bo) = (format!("--base={trunk}"), format!("--head={branch}"), format!("--title={title}"), format!("--body={body}"));
    let out = gh(auth, main, &["pr", "create", &b, &hd, &t, &bo])?;
    out.lines()
        .rev()
        .map(str::trim)
        .find(|l| l.starts_with("https://"))
        .map(String::from)
        .ok_or_else(|| Fail::Failed("gh pr create: no pull request URL in its output".into()))
}

impl Reagent for Gh {
    fn name(&self) -> &'static str {
        "gh"
    }
    fn tools(&self) -> &'static [ToolSpec] {
        TOOLS
    }
    fn schema(&self, name: &str) -> Option<Value> {
        let repo = json!({"type": "string", "minLength": 1, "maxLength": 32});
        let branch = json!({"type": "string", "minLength": 1, "maxLength": 200});
        Some(match name {
            "pr_open" => json!({"type": "object", "properties": {"repo": repo, "branch": branch,
                "title": {"type": "string", "maxLength": 256}, "body": {"type": "string", "maxLength": 65536}},
                "required": ["repo", "branch"], "additionalProperties": false}),
            "pr_for" => json!({"type": "object", "properties": {"repo": repo, "branch": branch}, "required": ["repo", "branch"], "additionalProperties": false}),
            "pr_create" => json!({"type": "object", "properties": {"repo": repo, "branch": branch,
                "title": {"type": "string", "minLength": 1, "maxLength": 256}, "body": {"type": "string", "maxLength": 65536}},
                "required": ["repo", "branch", "title", "body"], "additionalProperties": false}),
            _ => return None,
        })
    }
    fn authorize(&self, _: Option<&str>, name: &str, args: &Value, auth: &Authority) -> Result<(), Fail> {
        let repo_name = args["repo"].as_str().ok_or_else(|| Fail::Invalid("`repo` is required".into()))?;
        repo(auth, repo_name)?;
        valid_ref_name(args["branch"].as_str().unwrap_or(""))?;
        reagent::require(auth, reagent::need("gh", name, &[("repo", repo_name)], &[]))
    }
    fn call(&self, root: Option<&str>, name: &str, args: &Value, auth: &Authority) -> Result<Value, Fail> {
        self.authorize(root, name, args, auth)?;
        let r = repo(auth, args["repo"].as_str().unwrap_or(""))?;
        let main = Path::new(&r.path);
        let branch = args["branch"].as_str().unwrap_or("");
        match name {
            "pr_for" => Ok(json!({"url": open_pr(auth, main, branch)?})),
            // One grant, one write: the open pull request, or a new one.
            "pr_open" => {
                if let Some(url) = open_pr(auth, main, branch)? {
                    return Ok(json!({"url": url, "existing": true}));
                }
                let title = args["title"].as_str().filter(|t| !t.trim().is_empty()).unwrap_or(branch);
                let url = create_pr(auth, main, &r.trunk, branch, title, args["body"].as_str().unwrap_or(""))?;
                Ok(json!({"url": url, "existing": false}))
            }
            "pr_create" => {
                let url = create_pr(auth, main, &r.trunk, branch, args["title"].as_str().unwrap_or(""), args["body"].as_str().unwrap_or(""))?;
                Ok(json!({"url": url}))
            }
            other => Err(Fail::Failed(format!("reagent `gh` has no tool `{other}`"))),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::git_fixture::{fixture, Fixture};
    use serde_json::json;
    use std::collections::HashSet;
    use std::os::unix::fs::PermissionsExt;
    use std::sync::Mutex;

    /// A stand-in `gh` that logs its arguments and prints `reply`, exiting `code`.
    fn stub(f: &Fixture, reply: &str, code: i32) -> (crate::util::Config, std::path::PathBuf) {
        let log = f.root.join("gh.log");
        let script = f.root.join("gh");
        std::fs::write(&script, format!("#!/bin/sh\nfor a in \"$@\"; do printf '%s\\n' \"$a\"; done >> '{}'\nprintf '%s' '{reply}'\nexit {code}\n", log.display())).unwrap();
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
        let mut cfg = f.cfg.clone();
        cfg.gh = Some(script.to_string_lossy().into_owned());
        (cfg, log)
    }

    fn call(cfg: &crate::util::Config, tool: &str, args: Value) -> Result<Value, Fail> {
        let grants: Vec<_> = TOOLS.iter().map(|t| reagent::need("gh", t.name, &[], &[])).collect();
        let selected = Mutex::new(HashSet::new());
        let auth = Authority { grants: &grants, deadline: None, on_item: None, selected: &selected, expected_digest: None, config: Some(cfg) };
        Gh.call(None, tool, &args, &auth)
    }

    #[test]
    fn pr_for_parses_url() {
        let f = fixture();
        let (cfg, _) = stub(&f, r#"[{"url":"https://github.com/o/r/pull/7","isCrossRepository":false}]"#, 0);
        assert_eq!(call(&cfg, "pr_for", json!({"repo": "demo", "branch": "feature"})).unwrap()["url"], "https://github.com/o/r/pull/7");
        let (cfg, _) = stub(&f, "[]", 0);
        assert_eq!(call(&cfg, "pr_for", json!({"repo": "demo", "branch": "feature"})).unwrap()["url"], Value::Null);
    }

    #[test]
    fn pr_create_passes_exact_args() {
        let f = fixture();
        let (cfg, log) = stub(&f, "https://github.com/o/r/pull/8\n", 0);
        let out = call(&cfg, "pr_create", json!({"repo": "demo", "branch": "feature", "title": "Add b; rm -rf /", "body": "line one"})).unwrap();
        assert_eq!(out["url"], "https://github.com/o/r/pull/8");
        let args: Vec<String> = std::fs::read_to_string(log).unwrap().lines().map(String::from).collect();
        assert_eq!(args, ["pr", "create", "--base=master", "--head=feature", "--title=Add b; rm -rf /", "--body=line one"]);
    }

    #[test]
    fn gh_failure_is_clear() {
        let f = fixture();
        let (cfg, _) = stub(&f, "", 4);
        std::fs::write(f.root.join("gh"), "#!/bin/sh\necho 'You are not logged in to any GitHub hosts' >&2\nexit 4\n").unwrap();
        let e = call(&cfg, "pr_for", json!({"repo": "demo", "branch": "feature"})).unwrap_err();
        assert!(e.message().contains("not logged in"), "{}", e.message());
    }

    #[test]
    fn missing_gh_is_clear() {
        let f = fixture();
        let mut cfg = f.cfg.clone();
        cfg.gh = Some("/nonexistent/gh".into());
        let e = call(&cfg, "pr_for", json!({"repo": "demo", "branch": "feature"})).unwrap_err();
        assert!(e.message().contains("cannot run /nonexistent/gh"), "{}", e.message());
    }

    #[test]
    fn pr_for_ignores_pull_requests_from_forks() {
        let f = fixture();
        let (cfg, _) = stub(&f, r#"[{"url":"https://github.com/fork/r/pull/3","isCrossRepository":true}]"#, 0);
        assert_eq!(call(&cfg, "pr_for", json!({"repo": "demo", "branch": "feature"})).unwrap()["url"], Value::Null);
    }

}
