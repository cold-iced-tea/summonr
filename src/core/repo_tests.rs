//! The repository rituals, end to end against throwaway git repositories.
use super::test_home::{repo as repo_file, Home};
use super::*;
use crate::git_fixture::{commit, dirty, fixture, fixture_at, sh, Fixture};

const RITUALS: [&str; 6] = ["repo-status", "land-branch", "clean-merged", "open-pr", "start-branch", "commit-work"];

/// A home whose config lists the fixture as repo `demo`, with all four
/// rituals inscribed and granted.
fn home(f: &Fixture, test: &str, extra: &str) -> Home {
    let config = format!("repos:\n  demo:\n    path: \"{}\"\n    test: \"{test}\"\n{extra}", f.main().display());
    let h = Home::new(&config);
    for r in RITUALS {
        h.inscribe(&repo_file(&format!("rituals/repo/{r}.yaml")), false);
    }
    for r in RITUALS {
        for reagent in ["git", "gh", "trial", "data"] {
            let _ = h.call("grant", json!({"ritual": r, "scopes": [{"reagent": reagent, "all": true}]}));
        }
    }
    h
}

fn invoke(h: &Home, ritual: &str, args: Value) -> Value {
    h.run(json!({"target": ritual, "args": args}))
}

fn head(path: &std::path::Path, r: &str) -> String {
    sh(path, &["rev-parse", r])
}

#[test]
fn land_branch_happy_path() {
    let f = fixture();
    let h = home(&f, "true", "");
    let done = invoke(&h, "land-branch", json!({"repo": "demo", "branch": "feature"}));
    assert_eq!(done["outcome"], "returned", "{done}");
    let feature = head(&f.feature(), "HEAD");
    assert_eq!(head(&f.main(), "master"), feature);
    assert_eq!(sh(&f.origin(), &["rev-parse", "master"]), feature, "GitHub (origin) has the new commit");
    assert_eq!(done["result"]["commit"], feature.as_str());
    let epitaphs: i64 = h.core.db.lock().unwrap().query_row("SELECT COUNT(*) FROM epitaphs", [], |r| r.get(0)).unwrap();
    assert!(epitaphs >= 1);
    assert_eq!(done["requests"], 0, "no model calls");
}

#[test]
fn land_branch_refuses_dirty_main_and_leaves_it() {
    let f = fixture();
    let h = home(&f, "true", "");
    dirty(&f.main(), "a.txt");
    let before = head(&f.main(), "master");
    let done = invoke(&h, "land-branch", json!({"repo": "demo", "branch": "feature"}));
    assert_ne!(done["outcome"], "returned");
    assert!(done.to_string().contains("uncommitted changes"), "{done}");
    assert_eq!(std::fs::read_to_string(f.main().join("a.txt")).unwrap(), "uncommitted work");
    assert_eq!(head(&f.main(), "master"), before);
}

#[test]
fn land_branch_refuses_detached_main() {
    let f = fixture();
    let h = home(&f, "true", "");
    sh(&f.main(), &["checkout", "-q", "--detach"]);
    let done = invoke(&h, "land-branch", json!({"repo": "demo", "branch": "feature"}));
    assert_ne!(done["outcome"], "returned");
    assert!(done.to_string().contains("not on master"), "{done}");
}

#[test]
fn land_branch_refuses_trunk_as_branch() {
    let f = fixture();
    let h = home(&f, "true", "");
    let done = invoke(&h, "land-branch", json!({"repo": "demo", "branch": "master"}));
    assert_ne!(done["outcome"], "returned");
    assert!(done.to_string().contains("nothing to land"), "{done}");
}

#[test]
fn land_branch_refuses_failing_tests_and_merges_nothing() {
    let f = fixture();
    let h = home(&f, "false", "");
    let before = head(&f.main(), "master");
    let done = invoke(&h, "land-branch", json!({"repo": "demo", "branch": "feature"}));
    assert_ne!(done["outcome"], "returned");
    assert!(done.to_string().contains("tests failed"), "{done}");
    assert_eq!(head(&f.main(), "master"), before);
}

/// Like cargo: a long build log on stderr, then the failure on stdout.
pub(crate) const LOUD_FAILURE: &str = "i=0\nwhile [ $i -lt 40 ]; do echo \"   Compiling crate-$i v1.0.0 (a long line of build noise that pads the log out)\" >&2; i=$((i+1)); done\necho 'test widget::spins ... FAILED'\nexit 101\n";

#[test]
fn land_branch_reports_a_loud_test_failure() {
    let f = fixture();
    std::fs::write(f.feature().join("loud.sh"), LOUD_FAILURE).unwrap();
    sh(&f.feature(), &["add", "loud.sh"]);
    sh(&f.feature(), &["commit", "-q", "-m", "loud tests"]);
    let h = home(&f, "sh loud.sh", "");
    let before = head(&f.main(), "master");
    let done = invoke(&h, "land-branch", json!({"repo": "demo", "branch": "feature"}));
    assert_ne!(done["outcome"], "returned");
    let text = done.to_string();
    assert!(text.contains("tests failed") && text.contains("widget::spins ... FAILED"), "{done}");
    assert_eq!(head(&f.main(), "master"), before);
}

#[test]
fn land_branch_refuses_behind_remote() {
    let f = fixture();
    let h = home(&f, "true", "");
    sh(&f.root, &["clone", "-q", "origin.git", "other"]);
    commit(&f.root.join("other"), "o.txt", "elsewhere");
    sh(&f.root.join("other"), &["push", "-q", "origin", "master"]);
    let done = invoke(&h, "land-branch", json!({"repo": "demo", "branch": "feature"}));
    assert_ne!(done["outcome"], "returned");
    assert!(done.to_string().contains("behind origin"), "{done}");
}

#[test]
fn clean_merged_removes_only_merged_clean() {
    let f = fixture();
    let h = home(&f, "true", "");
    invoke(&h, "land-branch", json!({"repo": "demo", "branch": "feature"}));
    // A second merged worktree that has uncommitted work.
    sh(&f.main(), &["worktree", "add", "-q", "-b", "kept", "../wt-kept"]);
    dirty(&f.root.join("wt-kept"), "a.txt");
    let done = invoke(&h, "clean-merged", json!({"repo": "demo"}));
    assert_eq!(done["outcome"], "returned", "{done}");
    assert!(!f.feature().exists(), "the merged, clean worktree is removed");
    assert!(f.root.join("wt-kept").exists(), "the dirty one is kept");
    assert_eq!(std::fs::read_to_string(f.root.join("wt-kept/a.txt")).unwrap(), "uncommitted work");
    let branches = sh(&f.main(), &["branch", "--format=%(refname:short)"]);
    assert!(!branches.lines().any(|b| b == "feature"), "{branches}");
    assert!(branches.lines().any(|b| b == "kept"), "{branches}");
    assert!(done["result"].to_string().contains("uncommitted"), "the reason it kept one: {done}");
}

#[test]
fn repo_status_reports_each_repo() {
    let f = fixture();
    let h = home(&f, "true", "");
    dirty(&f.main(), "a.txt");
    let done = invoke(&h, "repo-status", json!({}));
    assert_eq!(done["outcome"], "returned", "{done}");
    let r = &done["result"]["repos"][0]["outputs"]["report"];
    assert_eq!(r["repo"], "demo", "{done}");
    assert_eq!(r["status"]["clean"], false, "{done}");
    assert_eq!(r["worktrees"].as_array().unwrap().len(), 2, "{done}");
}

#[test]
fn open_pr_creates_then_reports_existing() {
    let f = fixture();
    let gh = f.root.join("gh");
    let state = f.root.join("pr-open");
    std::fs::write(&gh, format!(
        "#!/bin/sh\nif [ \"$2\" = list ]; then if [ -f '{s}' ]; then printf '[{{\"url\":\"https://github.com/o/r/pull/9\",\"isCrossRepository\":false}}]'; else printf '[]'; fi; else touch '{s}'; echo https://github.com/o/r/pull/9; fi\n",
        s = state.display()
    )).unwrap();
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(&gh, std::fs::Permissions::from_mode(0o755)).unwrap();
    let h = home(&f, "true", &format!("gh: \"{}\"\n", gh.display()));
    let first = invoke(&h, "open-pr", json!({"repo": "demo", "branch": "feature"}));
    assert_eq!(first["outcome"], "returned", "{first}");
    assert_eq!(first["result"]["url"], "https://github.com/o/r/pull/9");
    assert_eq!(first["result"]["existing"], false);
    assert_eq!(sh(&f.origin(), &["rev-parse", "feature"]), head(&f.feature(), "HEAD"), "the branch was pushed");
    let again = invoke(&h, "open-pr", json!({"repo": "demo", "branch": "feature"}));
    assert_eq!(again["result"]["existing"], true, "{again}");
}

#[test]
fn missing_grant_refuses_before_any_effect() {
    let f = fixture();
    let config = format!("repos:\n  demo:\n    path: \"{}\"\n    test: \"true\"\n", f.main().display());
    let h = Home::new(&config);
    h.inscribe(&repo_file("rituals/repo/land-branch.yaml"), false);
    let before = head(&f.main(), "master");
    let e = h.call("invoke", json!({"target": "land-branch", "args": {"repo": "demo", "branch": "feature"}})).unwrap_err();
    assert_eq!(e.code, "not_authorized", "{}", e.message);
    assert_eq!(head(&f.main(), "master"), before);
}

#[test]
fn familiar_can_land_via_signed_call() {
    let f = fixture_at("signed");
    let h = home(&f, "true", "");
    h.call("familiar_new", json!({"name": "claude"})).unwrap();
    h.call("familiar_allow", json!({"name": "claude", "rituals": ["land-branch"]})).unwrap();
    let key = crate::keys::load(&h.core.paths.config.join("familiars/claude.key")).unwrap();
    let send = |method: &str, params: Value| {
        let nonce = crate::envelope::new_nonce();
        let frame = crate::envelope::seal_request(&key, &h.core.key.public().id(), method, &params, now_ms(), &nonce);
        crate::envelope::open_reply(&h.core.handle_signed(&frame), &h.core.key.public(), &nonce).unwrap()
    };
    let started = send("invoke", json!({"target": "land-branch", "args": {"repo": "demo", "branch": "feature"}})).unwrap();
    let done = send("wait", json!({"summon": started["summon"], "timeout_ms": 20000})).unwrap();
    assert_eq!(done["outcome"], "returned", "{done}");
    assert_eq!(sh(&f.origin(), &["rev-parse", "master"]), head(&f.feature(), "HEAD"));
}

#[test]
fn start_branch_then_commit_work() {
    let f = fixture();
    let h = home(&f, "true", "");
    let started = invoke(&h, "start-branch", json!({"repo": "demo", "branch": "next"}));
    assert_eq!(started["outcome"], "returned", "{started}");
    let path = std::path::PathBuf::from(started["result"]["path"].as_str().unwrap());
    assert_eq!(path, f.root.join("main-next"));
    std::fs::write(path.join("n.txt"), "work").unwrap();
    let done = invoke(&h, "commit-work", json!({"repo": "demo", "branch": "next", "message": "Add n"}));
    assert_eq!(done["outcome"], "returned", "{done}");
    assert_eq!(done["result"]["files"], json!(["n.txt"]));
    assert_eq!(done["result"]["commit"], head(&path, "HEAD").as_str());
    assert_eq!(done["requests"], 0);
}

#[test]
fn commit_work_refuses_a_secret_and_commits_nothing() {
    let f = fixture();
    let h = home(&f, "true", "");
    let before = head(&f.feature(), "HEAD");
    std::fs::write(f.feature().join("id_ed25519"), "PRIVATE").unwrap();
    let done = invoke(&h, "commit-work", json!({"repo": "demo", "branch": "feature", "message": "oops"}));
    assert_ne!(done["outcome"], "returned", "{done}");
    assert!(done.to_string().contains("id_ed25519"), "{done}");
    assert_eq!(head(&f.feature(), "HEAD"), before);
    assert_eq!(std::fs::read_to_string(f.feature().join("id_ed25519")).unwrap(), "PRIVATE");
}

#[test]
fn commit_work_refuses_a_branch_without_a_worktree() {
    let f = fixture();
    let h = home(&f, "true", "");
    sh(&f.main(), &["branch", "loose"]);
    let done = invoke(&h, "commit-work", json!({"repo": "demo", "branch": "loose", "message": "m"}));
    assert_ne!(done["outcome"], "returned", "{done}");
    assert!(done.to_string().contains("start-branch"), "{done}");
}

#[test]
fn familiar_runs_the_whole_lifecycle() {
    let f = fixture_at("cycle");
    let h = home(&f, "true", "");
    h.call("familiar_new", json!({"name": "claude"})).unwrap();
    h.call("familiar_allow", json!({"name": "claude", "rituals": ["start-branch", "commit-work", "land-branch", "clean-merged"]})).unwrap();
    let key = crate::keys::load(&h.core.paths.config.join("familiars/claude.key")).unwrap();
    let run = |ritual: &str, args: Value| {
        let send = |method: &str, params: Value| {
            let nonce = crate::envelope::new_nonce();
            let frame = crate::envelope::seal_request(&key, &h.core.key.public().id(), method, &params, now_ms(), &nonce);
            crate::envelope::open_reply(&h.core.handle_signed(&frame), &h.core.key.public(), &nonce).unwrap()
        };
        let started = send("invoke", json!({"target": ritual, "args": args})).unwrap();
        let done = send("wait", json!({"summon": started["summon"], "timeout_ms": 20000})).unwrap();
        assert_eq!(done["outcome"], "returned", "{ritual}: {done}");
        done
    };
    let started = run("start-branch", json!({"repo": "demo", "branch": "cycle"}));
    let path = std::path::PathBuf::from(started["result"]["path"].as_str().unwrap());
    std::fs::write(path.join("c.txt"), "cycle").unwrap();
    let committed = run("commit-work", json!({"repo": "demo", "branch": "cycle", "message": "Add c"}));
    run("land-branch", json!({"repo": "demo", "branch": "cycle"}));
    assert_eq!(sh(&f.origin(), &["rev-parse", "master"]), committed["result"]["commit"].as_str().unwrap());
    run("clean-merged", json!({"repo": "demo"}));
    assert!(!path.exists(), "the finished worktree is gone");
    assert!(!sh(&f.main(), &["branch", "--format=%(refname:short)"]).lines().any(|b| b == "cycle"));
}
