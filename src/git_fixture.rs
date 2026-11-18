//! Throwaway git repositories for tests: a bare `origin.git`, a clone `main`
//! on master with one commit, and a worktree `wt-feature` on branch
//! `feature` one commit ahead. Built with the real git, no network.
use crate::util::{Config, RepoConfig};
use std::path::{Path, PathBuf};
use std::process::Command;

pub struct Fixture {
    pub root: PathBuf,
    pub cfg: Config,
}

impl Fixture {
    pub fn main(&self) -> PathBuf {
        self.root.join("main")
    }
    pub fn feature(&self) -> PathBuf {
        self.root.join("wt-feature")
    }
    pub fn origin(&self) -> PathBuf {
        self.root.join("origin.git")
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

pub fn sh(cwd: &Path, args: &[&str]) -> String {
    let out = Command::new("git")
        .args(args)
        .current_dir(cwd)
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("GIT_AUTHOR_NAME", "t")
        .env("GIT_AUTHOR_EMAIL", "t@example.com")
        .env("GIT_COMMITTER_NAME", "t")
        .env("GIT_COMMITTER_EMAIL", "t@example.com")
        .output()
        .expect("git is installed");
    assert!(out.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&out.stderr));
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

pub fn commit(path: &Path, file: &str, text: &str) {
    std::fs::write(path.join(file), text).unwrap();
    sh(path, &["add", file]);
    sh(path, &["commit", "-q", "-m", &format!("add {file}")]);
}

pub fn dirty(path: &Path, file: &str) {
    std::fs::write(path.join(file), "uncommitted work").unwrap();
}

pub fn fixture() -> Fixture {
    fixture_at("")
}

/// The same, under a subdirectory name (to test awkward paths).
pub fn fixture_at(sub: &str) -> Fixture {
    let root = std::env::temp_dir().join(crate::util::new_id("summonr-git")).join(sub);
    std::fs::create_dir_all(&root).unwrap();
    sh(&root, &["init", "-q", "--bare", "-b", "master", "origin.git"]);
    sh(&root, &["clone", "-q", "origin.git", "main"]);
    let main = root.join("main");
    sh(&main, &["checkout", "-q", "-B", "master"]);
    commit(&main, "a.txt", "one");
    sh(&main, &["push", "-q", "-u", "origin", "master"]);
    sh(&main, &["worktree", "add", "-q", "-b", "feature", "../wt-feature"]);
    commit(&root.join("wt-feature"), "b.txt", "two");
    let mut cfg = Config::default();
    cfg.repos.insert(
        "demo".into(),
        RepoConfig { path: main.to_string_lossy().into_owned(), trunk: "master".into(), remote: "origin".into(), test: Some("true".into()), disposable: vec![] },
    );
    Fixture { root, cfg }
}
