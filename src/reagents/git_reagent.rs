//! The `git` reagent: a fixed menu of git operations on repositories the
//! owner lists in config.yaml. No shell, no prompts, no force, and never an
//! operation that could lose uncommitted work.

use crate::reagent::{self, Authority, Fail, Reagent, ToolSpec};
use crate::util::{Config, RepoConfig};
use serde_json::{json, Value};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

pub struct Git;

const READ: &str = "read";
const LIVE: &str = "deterministic over live state";

pub(crate) const TOOLS: &[ToolSpec] = &[
    ToolSpec { reagent: "git", name: "repos", effect: READ, determinism: LIVE, description: "The configured repository names" },
    ToolSpec { reagent: "git", name: "status", effect: READ, determinism: LIVE, description: "Branch and uncommitted changes of the main checkout or a listed worktree" },
    ToolSpec { reagent: "git", name: "ahead_behind", effect: READ, determinism: LIVE, description: "Commits only in `from` (ahead) and only in `to` (behind)" },
    ToolSpec { reagent: "git", name: "merged", effect: READ, determinism: LIVE, description: "Whether a branch is fully contained in the trunk" },
    ToolSpec { reagent: "git", name: "head", effect: READ, determinism: LIVE, description: "The commit a local ref points at" },
    ToolSpec { reagent: "git", name: "remote_head", effect: READ, determinism: LIVE, description: "The commit a branch points at on the remote" },
    ToolSpec { reagent: "git", name: "worktrees", effect: READ, determinism: LIVE, description: "Worktrees with their branch, head and whether they are clean" },
    ToolSpec { reagent: "git", name: "branches", effect: READ, determinism: LIVE, description: "Local branches, merged into the trunk or not, and the current one" },
    ToolSpec { reagent: "git", name: "info", effect: READ, determinism: LIVE, description: "A repository's trunk and remote" },
    ToolSpec { reagent: "git", name: "worktree_for", effect: READ, determinism: LIVE, description: "The worktree a branch is checked out in, if any, and whether it is clean" },
    ToolSpec { reagent: "git", name: "branch_exists", effect: READ, determinism: LIVE, description: "Whether a local branch exists" },
    ToolSpec { reagent: "git", name: "cleanup_plan", effect: READ, determinism: LIVE, description: "Which worktrees and branches are merged and clean enough to remove, and why others stay" },
    ToolSpec { reagent: "git", name: "fetch", effect: READ, determinism: LIVE, description: "Update remote-tracking refs; changes no local branch or file" },
    ToolSpec { reagent: "git", name: "merge_ff", effect: "write", determinism: LIVE, description: "Fast-forward the trunk to a branch in a clean main checkout; nothing else" },
    ToolSpec { reagent: "git", name: "push", effect: "write", determinism: LIVE, description: "Push one branch to the remote, never forced" },
    ToolSpec { reagent: "git", name: "delete_branch", effect: "write", determinism: LIVE, description: "Delete a branch fully merged into the trunk" },
    ToolSpec { reagent: "git", name: "remove_worktree", effect: "write", determinism: LIVE, description: "Remove a clean, listed worktree" },
    ToolSpec { reagent: "git", name: "worktree_add", effect: "write", determinism: LIVE, description: "Create a branch at an up-to-date trunk in a new worktree beside the main checkout" },
    ToolSpec { reagent: "git", name: "commit", effect: "write", determinism: LIVE, description: "Commit everything in a branch's own worktree, refusing secrets and large files" },
];

/// Branch, ref and remote names: git's rules plus a conservative alphabet.
pub fn valid_ref_name(s: &str) -> Result<(), Fail> {
    let ok = !s.is_empty()
        && s.len() <= 200
        && s.as_bytes()[0].is_ascii_alphanumeric()
        && s.bytes().all(|b| b.is_ascii_alphanumeric() || b"._/-".contains(&b))
        && !s.contains("..")
        && !s.contains("//")
        && !s.contains("@{")
        && !s.ends_with('/')
        && !s.ends_with(".lock")
        && !s.ends_with('.');
    if ok {
        Ok(())
    } else {
        Err(Fail::Invalid(format!("`{s}` is not an acceptable branch or ref name")))
    }
}

/// A configured repository, or a refusal.
pub(crate) fn repo<'a>(auth: &'a Authority, name: &str) -> Result<&'a RepoConfig, Fail> {
    let cfg = auth.config.ok_or_else(|| Fail::Denied("no repositories are configured".into()))?;
    cfg.repos
        .get(name)
        .ok_or_else(|| Fail::Denied(format!("`{name}` is not a repository in config.yaml `repos:`")))
}

pub(crate) fn config<'a>(auth: &'a Authority) -> Result<&'a Config, Fail> {
    auth.config.ok_or_else(|| Fail::Denied("no repositories are configured".into()))
}

/// A command that cannot prompt, read input, or be redirected to another
/// repository by inherited environment. `git` commands also get a fixed locale
/// for parsing; other programs (the user's tests) keep theirs.
pub(crate) fn command(prog: &str, git: bool) -> Command {
    let mut c = Command::new(prog);
    for var in ["GIT_DIR", "GIT_WORK_TREE", "GIT_INDEX_FILE", "GIT_COMMON_DIR", "GIT_OBJECT_DIRECTORY", "GIT_ALTERNATE_OBJECT_DIRECTORIES", "GIT_NAMESPACE", "GIT_PREFIX"] {
        c.env_remove(var);
    }
    c.env("GIT_TERMINAL_PROMPT", "0")
        .env("GIT_EDITOR", "true")
        .env("GH_PROMPT_DISABLED", "1")
        .env("SSH_ASKPASS_REQUIRE", "never")
        .env("GCM_INTERACTIVE", "never");
    if std::env::var_os("GIT_SSH_COMMAND").is_none() {
        c.env("GIT_SSH_COMMAND", "ssh -oBatchMode=yes");
    }
    if git {
        c.env("LC_ALL", "C");
    }
    c
}

pub(crate) fn run_bounded(prog: &str, args: &[&str], cwd: &Path, deadline: Option<Instant>) -> Result<(i32, String, String), Fail> {
    run_bounded_as(prog, args, cwd, deadline, false)
}

/// Run a program in its own process group with separate arguments (never a
/// shell) and no input. At the deadline the whole group is stopped; when the
/// program exits, anything it left running in its group is stopped too. A
/// child that left the group (`setsid`) and still holds the output open is
/// not waited for beyond a short grace period.
pub(crate) fn run_bounded_as(prog: &str, args: &[&str], cwd: &Path, deadline: Option<Instant>, git: bool) -> Result<(i32, String, String), Fail> {
    run_bounded_inner(prog, args, cwd, deadline, git, None)
}

/// The same, keeping only the last `keep` bytes of stdout and of stderr, so a
/// program that writes without end cannot fill memory.
pub(crate) fn run_bounded_kept(prog: &str, args: &[&str], cwd: &Path, deadline: Option<Instant>, keep: usize) -> Result<(i32, String, String), Fail> {
    run_bounded_inner(prog, args, cwd, deadline, false, Some(keep))
}

/// How long to keep reading after the program ended, for output a child
/// outside its process group may still hold open.
const OUTPUT_GRACE: Duration = Duration::from_secs(2);

/// One output stream, read on its own thread into a shared buffer.
struct Drain {
    buf: std::sync::Arc<std::sync::Mutex<Vec<u8>>>,
    done: std::sync::mpsc::Receiver<()>,
}

impl Drain {
    fn start(mut r: impl std::io::Read + Send + 'static, keep: Option<usize>) -> Drain {
        let buf = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let (tx, done) = std::sync::mpsc::channel();
        let shared = buf.clone();
        std::thread::spawn(move || {
            let mut chunk = [0u8; 8192];
            while let Ok(n) = r.read(&mut chunk) {
                if n == 0 {
                    break;
                }
                let mut b = shared.lock().unwrap_or_else(|e| e.into_inner());
                b.extend_from_slice(&chunk[..n]);
                // Trim only now and then, so keeping the end stays cheap.
                if let Some(k) = keep {
                    if b.len() > 2 * k {
                        let cut = b.len() - k;
                        b.drain(..cut);
                    }
                }
            }
            let _ = tx.send(());
        });
        Drain { buf, done }
    }

    /// What was read, once the stream closed or the grace period ran out.
    fn finish(self, until: Instant, keep: Option<usize>) -> String {
        let _ = self.done.recv_timeout(until.saturating_duration_since(Instant::now()));
        let b = self.buf.lock().unwrap_or_else(|e| e.into_inner());
        let from = keep.map_or(0, |k| b.len().saturating_sub(k));
        String::from_utf8_lossy(&b[from..]).into_owned()
    }
}

fn run_bounded_inner(prog: &str, args: &[&str], cwd: &Path, deadline: Option<Instant>, git: bool, keep: Option<usize>) -> Result<(i32, String, String), Fail> {
    use std::os::unix::process::CommandExt;
    let mut child = command(prog, git)
        .args(args)
        .current_dir(cwd)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .process_group(0)
        .spawn()
        .map_err(|e| Fail::Failed(format!("cannot run {prog}: {e}")))?;
    let group = child.id() as i32;
    let stop_group = || unsafe {
        libc::kill(-group, libc::SIGKILL);
    };
    // Drain output on threads so a full pipe can never stall the wait.
    let out = Drain::start(child.stdout.take().unwrap(), keep);
    let err = Drain::start(child.stderr.take().unwrap(), keep);
    let status = loop {
        match child.try_wait() {
            Ok(Some(st)) => break st,
            Ok(None) => {}
            Err(e) => {
                stop_group();
                let _ = child.wait();
                return Err(Fail::Failed(format!("cannot wait for {prog}: {e}")));
            }
        }
        if deadline.is_some_and(|d| Instant::now() >= d) {
            stop_group();
            let _ = child.wait();
            return Err(Fail::Exhausted(format!("{prog} ran past the time limit")));
        }
        std::thread::sleep(Duration::from_millis(10));
    };
    // The program is done; nothing it started in its group may keep running.
    stop_group();
    let until = Instant::now() + OUTPUT_GRACE;
    let stdout = out.finish(until, keep);
    let stderr = err.finish(until, keep);
    Ok((status.code().unwrap_or(-1), stdout, stderr))
}

/// The most a change summary may hold, so it fits in a ritual's message.
const SUMMARY_BYTES: usize = 300;

/// A short, bounded description of changed paths for messages:
/// `a.txt`, or `42 paths: a.txt, b.txt and 40 more`.
pub(crate) fn change_summary(changed: &[String]) -> String {
    fn cut(s: &str, max: usize) -> &str {
        let mut end = s.len().min(max);
        while !s.is_char_boundary(end) {
            end -= 1;
        }
        &s[..end]
    }
    match changed {
        [] => String::new(),
        [one] => cut(one, SUMMARY_BYTES).to_string(),
        _ => {
            let mut text = format!("{} paths: ", changed.len());
            let mut shown = 0;
            for p in changed {
                let sep = if shown == 0 { "" } else { ", " };
                // Leave room for " and N more".
                if text.len() + sep.len() + p.len() + 16 > SUMMARY_BYTES {
                    break;
                }
                text.push_str(sep);
                text.push_str(p);
                shown += 1;
            }
            if shown == 0 {
                let room = SUMMARY_BYTES - text.len() - 16;
                text.push_str(cut(&changed[0], room));
                shown = 1;
            }
            if shown < changed.len() {
                text.push_str(&format!(" and {} more", changed.len() - shown));
            }
            text
        }
    }
}

/// Run git; a non-zero exit is a failure carrying git's last error line.
pub(crate) fn git(cfg: &Config, cwd: &Path, args: &[&str], deadline: Option<Instant>) -> Result<String, Fail> {
    let prog = cfg.git.as_deref().unwrap_or("git");
    let mut full = vec!["-c", "core.quotepath=false"];
    full.extend_from_slice(args);
    let (code, out, err) = run_bounded_as(prog, &full, cwd, deadline, true)?;
    if code != 0 {
        let last = err.lines().rev().find(|l| !l.trim().is_empty()).unwrap_or("").trim().to_string();
        return Err(Fail::Failed(format!("git {}: {last}", args.first().unwrap_or(&""))));
    }
    Ok(out)
}

/// git, but an exit code of 1 is an answer (`false`), not a failure.
fn git_test(cfg: &Config, cwd: &Path, args: &[&str], deadline: Option<Instant>) -> Result<bool, Fail> {
    let prog = cfg.git.as_deref().unwrap_or("git");
    let (code, _, err) = run_bounded_as(prog, args, cwd, deadline, true)?;
    match code {
        0 => Ok(true),
        1 => Ok(false),
        _ => Err(Fail::Failed(format!("git {}: {}", args.first().unwrap_or(&""), err.trim()))),
    }
}

fn arg<'a>(args: &'a Value, key: &str) -> Result<&'a str, Fail> {
    args[key].as_str().ok_or_else(|| Fail::Invalid(format!("`{key}` is required")))
}

pub(crate) struct Worktree {
    pub path: PathBuf,
    pub branch: Option<String>,
    pub head: String,
    pub main: bool,
    /// Its directory is gone; `git worktree prune` would forget it.
    pub prunable: bool,
    pub locked: bool,
}

pub(crate) fn list_worktrees(cfg: &Config, r: &RepoConfig, deadline: Option<Instant>) -> Result<Vec<Worktree>, Fail> {
    let out = git(cfg, Path::new(&r.path), &["worktree", "list", "--porcelain"], deadline)?;
    let mut list = Vec::new();
    for block in out.split("\n\n").filter(|b| !b.trim().is_empty()) {
        let mut w = Worktree { path: PathBuf::new(), branch: None, head: String::new(), main: list.is_empty(), prunable: false, locked: false };
        for line in block.lines() {
            if let Some(p) = line.strip_prefix("worktree ") {
                w.path = PathBuf::from(p);
            } else if let Some(h) = line.strip_prefix("HEAD ") {
                w.head = h.to_string();
            } else if let Some(b) = line.strip_prefix("branch ") {
                w.branch = Some(b.strip_prefix("refs/heads/").unwrap_or(b).to_string());
            } else if line == "locked" || line.starts_with("locked ") {
                w.locked = true;
            } else if line == "prunable" || line.starts_with("prunable ") {
                w.prunable = true;
            }
        }
        list.push(w);
    }
    Ok(list)
}

/// (branch or None when detached, changed paths) of a checkout.
pub(crate) fn checkout_state(cfg: &Config, path: &Path, deadline: Option<Instant>) -> Result<(Option<String>, Vec<String>), Fail> {
    let prog = cfg.git.as_deref().unwrap_or("git");
    let (code, out, _) = run_bounded_as(prog, &["symbolic-ref", "-q", "--short", "HEAD"], path, deadline, true)?;
    let branch = (code == 0).then(|| out.trim().to_string());
    let changed = status_entries(cfg, path, deadline)?.into_iter().map(|e| e.path).collect();
    Ok((branch, changed))
}

/// One entry of `git status`: its two-letter code, path, and (for a rename
/// or copy) the path it came from. Read with `-z`, so names are exact.
pub(crate) struct Entry {
    pub code: String,
    pub path: String,
    pub from: Option<String>,
}

pub(crate) fn status_entries(cfg: &Config, path: &Path, deadline: Option<Instant>) -> Result<Vec<Entry>, Fail> {
    let out = git(cfg, path, &["status", "--porcelain=v1", "-z", "--untracked-files=all"], deadline)?;
    let mut parts = out.split('\0').filter(|p| !p.is_empty());
    let mut list = Vec::new();
    while let Some(p) = parts.next() {
        if p.len() < 4 {
            continue;
        }
        let code = p[..2].to_string();
        let from = if code.contains('R') || code.contains('C') { parts.next().map(String::from) } else { None };
        list.push(Entry { code, path: p[3..].to_string(), from });
    }
    Ok(list)
}

/// Ignored, untracked entries of a checkout (directories end in `/`).
pub(crate) fn ignored(cfg: &Config, path: &Path, deadline: Option<Instant>) -> Result<Vec<String>, Fail> {
    let out = git(cfg, path, &["ls-files", "--others", "--ignored", "--exclude-standard", "--directory"], deadline)?;
    Ok(out.lines().filter(|l| !l.is_empty()).map(String::from).collect())
}

/// Ignored entries a removal would destroy that the owner hasn't called disposable.
fn precious(cfg: &Config, r: &RepoConfig, path: &Path, deadline: Option<Instant>) -> Result<Vec<String>, Fail> {
    Ok(ignored(cfg, path, deadline)?
        .into_iter()
        .filter(|e| !r.disposable.iter().any(|d| e == d || e.starts_with(d.as_str()) || e.trim_end_matches('/') == d.trim_end_matches('/')))
        .collect())
}

/// Files over this size are refused by `commit`.
const MAX_COMMIT_FILE: u64 = 5 * 1024 * 1024;

/// Names that usually hold credentials; `commit` refuses them.
fn looks_secret(path: &str) -> bool {
    let name = path.rsplit('/').next().unwrap_or(path).to_ascii_lowercase();
    [".env", ".envrc", ".netrc", ".npmrc", ".pypirc", ".pgpass"].contains(&name.as_str())
        || name.starts_with(".env.")
        || name.contains("credentials")
        || [".pem", ".key", ".p12", ".pfx", ".keystore", ".jks", ".gpg"].iter().any(|ext| name.ends_with(ext))
        || ["id_rsa", "id_ed25519", "id_ecdsa", "id_dsa", "secrets.", "service-account"].iter().any(|p| name.starts_with(p))
}

/// The checkout a call names: the main checkout, or a worktree `worktrees` lists.
pub(crate) fn checkout_path(cfg: &Config, r: &RepoConfig, args: &Value, deadline: Option<Instant>) -> Result<PathBuf, Fail> {
    match args["worktree"].as_str() {
        None => Ok(PathBuf::from(&r.path)),
        Some(w) => {
            let want = std::fs::canonicalize(w).unwrap_or_else(|_| PathBuf::from(w));
            list_worktrees(cfg, r, deadline)?
                .into_iter()
                .map(|t| t.path)
                .find(|p| std::fs::canonicalize(p).unwrap_or_else(|_| p.clone()) == want)
                .ok_or_else(|| Fail::Denied(format!("`{w}` is not a worktree of this repository")))
        }
    }
}

fn names_in(name: &str) -> &'static [&'static str] {
    match name {
        "ahead_behind" => &["from", "to"],
        "merged" | "remote_head" | "merge_ff" | "push" | "delete_branch" | "worktree_for" | "branch_exists" | "worktree_add" | "commit" => &["branch"],
        "head" => &["ref"],
        _ => &[],
    }
}

/// What `clean-merged` may remove: worktrees and branches fully merged into
/// the trunk with nothing uncommitted. Everything else stays, with a reason.
fn cleanup_plan(cfg: &Config, r: &RepoConfig, d: Option<Instant>) -> Result<Value, Fail> {
    let main = Path::new(&r.path);
    let merged = |b: &str| git_test(cfg, main, &["merge-base", "--is-ancestor", b, &r.trunk], d);
    let trees = list_worktrees(cfg, r, d)?;
    let mut worktrees = Vec::new();
    for w in trees.iter().filter(|w| !w.main) {
        let (action, reason) = if w.prunable {
            ("keep", "prunable: its directory is gone; run git worktree prune".to_string())
        } else if w.locked {
            ("keep", "locked".to_string())
        } else {
            let (_, changed) = checkout_state(cfg, &w.path, d)?;
            let keep = precious(cfg, r, &w.path, d)?;
            match &w.branch {
                None => ("keep", "detached HEAD".to_string()),
                Some(_) if !changed.is_empty() => ("keep", format!("uncommitted changes: {}", changed.join(", "))),
                Some(_) if !keep.is_empty() => ("keep", format!("ignored files that removal would delete: {}", keep.join(", "))),
                Some(b) if !merged(b)? => ("keep", format!("{b} is not merged into {}", r.trunk)),
                Some(_) => ("remove", "merged and clean".to_string()),
            }
        };
        worktrees.push(json!({"path": w.path, "branch": w.branch, "action": action, "reason": reason}));
    }
    let current = trees.iter().find(|w| w.main).and_then(|w| w.branch.clone());
    let out = git(cfg, main, &["for-each-ref", "--format=%(refname:short)", "refs/heads"], d)?;
    let mut branches = Vec::new();
    for b in out.lines().filter(|l| !l.is_empty()) {
        let in_tree = trees.iter().find(|w| !w.main && w.branch.as_deref() == Some(b));
        let (action, reason) = if b == r.trunk {
            ("keep", "the trunk".to_string())
        } else if current.as_deref() == Some(b) {
            ("keep", "checked out in the main checkout".to_string())
        } else if let Some(w) = in_tree {
            ("keep", format!("checked out in {}", w.path.display()))
        } else if !merged(b)? {
            ("keep", format!("not merged into {}", r.trunk))
        } else {
            ("remove", "merged".to_string())
        };
        branches.push(json!({"name": b, "action": action, "reason": reason}));
    }
    Ok(json!({"worktrees": worktrees, "branches": branches}))
}

impl Reagent for Git {
    fn name(&self) -> &'static str {
        "git"
    }
    fn tools(&self) -> &'static [ToolSpec] {
        TOOLS
    }
    fn schema(&self, name: &str) -> Option<Value> {
        let s = |props: Value, req: &[&str]| json!({"type": "object", "properties": props, "required": req, "additionalProperties": false});
        let repo = json!({"type": "string", "minLength": 1, "maxLength": 32});
        let name_s = json!({"type": "string", "minLength": 1, "maxLength": 200});
        Some(match name {
            "repos" => s(json!({}), &[]),
            "status" => s(json!({"repo": repo, "worktree": {"type": "string", "minLength": 1, "maxLength": 4096}}), &["repo"]),
            "ahead_behind" => s(json!({"repo": repo, "from": name_s, "to": name_s}), &["repo", "from", "to"]),
            "merge_ff" => s(json!({"repo": repo, "branch": name_s, "expect": {"type": "string", "minLength": 40, "maxLength": 64}}), &["repo", "branch"]),
            "merged" | "remote_head" | "push" | "delete_branch" | "worktree_for" | "branch_exists" => {
                s(json!({"repo": repo, "branch": name_s}), &["repo", "branch"])
            }
            "info" | "cleanup_plan" => s(json!({"repo": repo}), &["repo"]),
            "worktree_add" => s(json!({"repo": repo, "branch": name_s}), &["repo", "branch"]),
            "commit" => s(json!({"repo": repo, "branch": name_s, "message": {"type": "string", "minLength": 1, "maxLength": 10000}}), &["repo", "branch", "message"]),
            "remove_worktree" => s(json!({"repo": repo, "path": {"type": "string", "minLength": 1, "maxLength": 4096}}), &["repo", "path"]),
            "head" => s(json!({"repo": repo, "ref": name_s}), &["repo", "ref"]),
            "worktrees" | "branches" | "fetch" => s(json!({"repo": repo}), &["repo"]),
            _ => return None,
        })
    }
    fn authorize(&self, _: Option<&str>, name: &str, args: &Value, auth: &Authority) -> Result<(), Fail> {
        if name == "repos" {
            config(auth)?;
            return reagent::require(auth, reagent::need("git", "repos", &[], &[]));
        }
        let repo_name = arg(args, "repo")?;
        repo(auth, repo_name)?;
        for key in names_in(name) {
            valid_ref_name(arg(args, key)?)?;
        }
        reagent::require(auth, reagent::need("git", name, &[("repo", repo_name)], &[]))
    }
    fn call(&self, root: Option<&str>, name: &str, args: &Value, auth: &Authority) -> Result<Value, Fail> {
        self.authorize(root, name, args, auth)?;
        let cfg = config(auth)?;
        let d = auth.deadline;
        if name == "repos" {
            return Ok(json!(cfg.repos.keys().collect::<Vec<_>>()));
        }
        let r = repo(auth, arg(args, "repo")?)?;
        let main = Path::new(&r.path);
        match name {
            "status" => {
                let path = checkout_path(cfg, r, args, d)?;
                let (branch, changed) = checkout_state(cfg, &path, d)?;
                Ok(json!({"branch": branch, "clean": changed.is_empty(), "summary": change_summary(&changed), "changed": changed}))
            }
            "ahead_behind" => {
                let range = format!("{}...{}", arg(args, "from")?, arg(args, "to")?);
                let out = git(cfg, main, &["rev-list", "--left-right", "--count", &range], d)?;
                let mut n = out.split_whitespace().map(|x| x.parse::<i64>().unwrap_or(0));
                Ok(json!({"ahead": n.next().unwrap_or(0), "behind": n.next().unwrap_or(0)}))
            }
            "merged" => {
                let merged = git_test(cfg, main, &["merge-base", "--is-ancestor", arg(args, "branch")?, &r.trunk], d)?;
                Ok(json!({"merged": merged}))
            }
            "head" => {
                let spec = format!("{}^{{commit}}", arg(args, "ref")?);
                Ok(json!({"commit": git(cfg, main, &["rev-parse", "--verify", "--quiet", &spec], d)?.trim()}))
            }
            "remote_head" => {
                let refname = format!("refs/heads/{}", arg(args, "branch")?);
                let out = git(cfg, main, &["ls-remote", &r.remote, &refname], d)?;
                Ok(json!({"commit": out.split_whitespace().next()}))
            }
            "worktrees" => {
                let mut list = Vec::new();
                for w in list_worktrees(cfg, r, d)? {
                    let clean = if w.prunable { Value::Null } else { json!(checkout_state(cfg, &w.path, d)?.1.is_empty()) };
                    list.push(json!({"path": w.path, "branch": w.branch, "head": w.head, "clean": clean, "main": w.main, "prunable": w.prunable, "locked": w.locked}));
                }
                Ok(Value::Array(list))
            }
            "branches" => {
                let out = git(cfg, main, &["for-each-ref", "--format=%(refname:short)%09%(HEAD)", "refs/heads"], d)?;
                let mut list = Vec::new();
                for line in out.lines().filter(|l| !l.is_empty()) {
                    let (b, current) = match line.split_once('\t') {
                        Some((b, h)) => (b.to_string(), h.trim() == "*"),
                        None => (line.to_string(), false),
                    };
                    let merged = git_test(cfg, main, &["merge-base", "--is-ancestor", &b, &r.trunk], d)?;
                    list.push(json!({"name": b, "merged": merged, "current": current}));
                }
                Ok(Value::Array(list))
            }
            "info" => Ok(json!({"trunk": r.trunk, "remote": r.remote})),
            "worktree_for" => {
                let branch = arg(args, "branch")?;
                match list_worktrees(cfg, r, d)?.into_iter().find(|w| !w.main && !w.prunable && w.branch.as_deref() == Some(branch)) {
                    Some(w) => {
                        let (_, changed) = checkout_state(cfg, &w.path, d)?;
                        Ok(json!({"path": w.path, "clean": changed.is_empty(), "summary": change_summary(&changed), "changed": changed}))
                    }
                    None => Ok(json!({"path": null, "clean": null, "summary": "", "changed": []})),
                }
            }
            "branch_exists" => {
                let refname = format!("refs/heads/{}", arg(args, "branch")?);
                Ok(json!({"exists": git_test(cfg, main, &["show-ref", "--verify", "--quiet", &refname], d)?}))
            }
            "cleanup_plan" => cleanup_plan(cfg, r, d),
            "fetch" => {
                git(cfg, main, &["fetch", "--quiet", &r.remote], d)?;
                Ok(json!({"fetched": true}))
            }
            "merge_ff" => {
                let branch = arg(args, "branch")?;
                let (on, changed) = checkout_state(cfg, main, d)?;
                if on.as_deref() != Some(r.trunk.as_str()) {
                    return Err(Fail::Invalid(format!("the main checkout is not on {}", r.trunk)));
                }
                if !changed.is_empty() {
                    return Err(Fail::Invalid(format!("the main checkout has uncommitted changes: {}", changed.join(", "))));
                }
                // Merge exactly the commit that was checked, if one was named.
                let tip = git(cfg, main, &["rev-parse", "--verify", "--quiet", &format!("refs/heads/{branch}^{{commit}}")], d)?.trim().to_string();
                if let Some(want) = args["expect"].as_str() {
                    if want != tip {
                        return Err(Fail::Invalid(format!("{branch} moved since it was tested (tested {want}, now {tip})")));
                    }
                }
                if !git_test(cfg, main, &["merge-base", "--is-ancestor", &r.trunk, &tip], d)? {
                    let range = format!("{branch}..{}", r.trunk);
                    let lacks = git(cfg, main, &["rev-list", "--count", &range], d)?;
                    return Err(Fail::Invalid(format!(
                        "{branch} does not fast-forward {} ({} has {} commit(s) it lacks)",
                        r.trunk,
                        r.trunk,
                        lacks.trim()
                    )));
                }
                // An ignored file in the main checkout at a path the branch adds
                // would be silently overwritten by the merge: refuse instead.
                let range = format!("{}..{tip}", r.trunk);
                let added = git(cfg, main, &["diff", "--name-only", "--diff-filter=A", &range], d)?;
                let ignored_here = ignored(cfg, main, d)?;
                let clobbered: Vec<&str> = added
                    .lines()
                    .filter(|a| ignored_here.iter().any(|e| e == a || (e.ends_with('/') && a.starts_with(e.as_str()))))
                    .collect();
                if !clobbered.is_empty() {
                    return Err(Fail::Invalid(format!(
                        "merging {branch} would overwrite ignored files in the main checkout: {}",
                        clobbered.join(", ")
                    )));
                }
                git(cfg, main, &["merge", "--ff-only", "--quiet", &tip], d)?;
                let commit = git(cfg, main, &["rev-parse", "HEAD"], d)?;
                Ok(json!({"trunk": r.trunk, "commit": commit.trim()}))
            }
            "push" => {
                let branch = arg(args, "branch")?;
                let refspec = format!("refs/heads/{branch}:refs/heads/{branch}");
                git(cfg, main, &["push", "--quiet", &r.remote, &refspec], d)?;
                let commit = git(cfg, main, &["rev-parse", branch], d)?;
                Ok(json!({"branch": branch, "commit": commit.trim()}))
            }
            "delete_branch" => {
                let branch = arg(args, "branch")?;
                if branch == r.trunk {
                    return Err(Fail::Invalid(format!("refusing to delete the trunk ({branch})")));
                }
                if list_worktrees(cfg, r, d)?.iter().any(|w| w.branch.as_deref() == Some(branch)) {
                    return Err(Fail::Invalid(format!("`{branch}` is checked out in a worktree")));
                }
                if !git_test(cfg, main, &["merge-base", "--is-ancestor", branch, &r.trunk], d)? {
                    return Err(Fail::Invalid(format!("`{branch}` is not fully merged into {}", r.trunk)));
                }
                git(cfg, main, &["branch", "--delete", branch], d)?;
                Ok(json!({"deleted": branch}))
            }
            "remove_worktree" => {
                let want = arg(args, "path")?;
                let canon = |p: &Path| std::fs::canonicalize(p).unwrap_or_else(|_| p.to_path_buf());
                let target = canon(Path::new(want));
                let w = list_worktrees(cfg, r, d)?
                    .into_iter()
                    .find(|w| canon(&w.path) == target)
                    .ok_or_else(|| Fail::Denied(format!("`{want}` is not a worktree of this repository")))?;
                if w.main {
                    return Err(Fail::Invalid("refusing to remove the main checkout".into()));
                }
                if w.locked {
                    return Err(Fail::Invalid(format!("{} is locked", w.path.display())));
                }
                if w.prunable {
                    return Err(Fail::Invalid(format!("{} is already gone; run git worktree prune", w.path.display())));
                }
                let (_, changed) = checkout_state(cfg, &w.path, d)?;
                if !changed.is_empty() {
                    return Err(Fail::Invalid(format!("{} has uncommitted changes: {}", w.path.display(), changed.join(", "))));
                }
                let keep = precious(cfg, r, &w.path, d)?;
                if !keep.is_empty() {
                    return Err(Fail::Invalid(format!(
                        "{} has ignored files that removal would delete: {} (list build output under the repo's `disposable:` to allow it)",
                        w.path.display(),
                        keep.join(", ")
                    )));
                }
                let path = w.path.to_string_lossy().into_owned();
                git(cfg, main, &["worktree", "remove", &path], d)?;
                Ok(json!({"removed": path}))
            }
            "worktree_add" => {
                let branch = arg(args, "branch")?;
                let (on, _) = checkout_state(cfg, main, d)?;
                if on.as_deref() != Some(r.trunk.as_str()) {
                    return Err(Fail::Invalid(format!("the main checkout is not on {}", r.trunk)));
                }
                if git_test(cfg, main, &["show-ref", "--verify", "--quiet", &format!("refs/heads/{branch}")], d)? {
                    return Err(Fail::Invalid(format!("branch `{branch}` already exists")));
                }
                let dir = main.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
                let path = main.parent().unwrap_or(main).join(format!("{dir}-{}", branch.replace('/', "-")));
                if path.exists() {
                    return Err(Fail::Invalid(format!("{} already exists", path.display())));
                }
                git(cfg, main, &["fetch", "--quiet", &r.remote], d)?;
                let range = format!("{}..{}/{}", r.trunk, r.remote, r.trunk);
                let behind = git(cfg, main, &["rev-list", "--count", &range], d)?;
                if behind.trim() != "0" {
                    return Err(Fail::Invalid(format!(
                        "{} is behind {}/{} by {} commit(s); bring it up to date first",
                        r.trunk,
                        r.remote,
                        r.trunk,
                        behind.trim()
                    )));
                }
                let target = path.to_string_lossy().into_owned();
                git(cfg, main, &["worktree", "add", "--quiet", "-b", branch, &target, &r.trunk], d)?;
                let commit = git(cfg, &path, &["rev-parse", "HEAD"], d)?;
                Ok(json!({"path": path, "branch": branch, "commit": commit.trim()}))
            }
            "commit" => {
                let branch = arg(args, "branch")?;
                let message = arg(args, "message")?;
                if message.trim().is_empty() {
                    return Err(Fail::Invalid("a commit needs a message".into()));
                }
                if message.chars().any(|c| c.is_control() && c != '\n' && c != '\t') {
                    return Err(Fail::Invalid("the commit message contains control characters".into()));
                }
                if branch == r.trunk {
                    return Err(Fail::Invalid(format!("refusing to commit on the trunk ({branch}); commit in a branch's worktree")));
                }
                let w = list_worktrees(cfg, r, d)?
                    .into_iter()
                    .find(|w| !w.main && !w.prunable && w.branch.as_deref() == Some(branch))
                    .ok_or_else(|| Fail::Invalid(format!("`{branch}` has no worktree to commit from")))?;
                // An unfinished merge, cherry-pick, revert or rebase would be
                // committed with its conflicts marked resolved.
                for marker in ["MERGE_HEAD", "CHERRY_PICK_HEAD", "REVERT_HEAD", "rebase-merge", "rebase-apply"] {
                    let at = git(cfg, &w.path, &["rev-parse", "--git-path", marker], d)?;
                    if w.path.join(at.trim()).exists() {
                        return Err(Fail::Invalid(format!("{} has an unfinished merge, rebase, cherry-pick or revert ({marker}); finish it first", w.path.display())));
                    }
                }
                let entries = status_entries(cfg, &w.path, d)?;
                if let Some(c) = entries.iter().find(|e| ["UU", "AA", "DD", "AU", "UA", "DU", "UD"].contains(&e.code.as_str())) {
                    return Err(Fail::Invalid(format!("{} has unresolved conflicts: {}", w.path.display(), c.path)));
                }
                let changed: Vec<String> = entries.iter().map(|e| e.path.clone()).collect();
                if changed.is_empty() {
                    return Err(Fail::Invalid(format!("nothing to commit in {}", w.path.display())));
                }
                // Checked before anything is staged, so a refusal changes nothing.
                // A file that should exist but can't be read is refused too.
                let risky: Vec<String> = entries
                    .iter()
                    .filter(|e| {
                        let deleted = e.code.contains('D');
                        let too_big = match std::fs::symlink_metadata(w.path.join(&e.path)) {
                            Ok(m) => m.is_file() && m.len() > MAX_COMMIT_FILE,
                            Err(_) => !deleted,
                        };
                        looks_secret(&e.path) || e.from.as_deref().is_some_and(looks_secret) || too_big
                    })
                    .map(|e| e.path.clone())
                    .collect();
                if !risky.is_empty() {
                    return Err(Fail::Invalid(format!(
                        "refusing to commit files that look like secrets or are over {} MiB: {}",
                        MAX_COMMIT_FILE / (1024 * 1024),
                        risky.join(", ")
                    )));
                }
                git(cfg, &w.path, &["add", "--all", "--", "."], d)?;
                let msg = format!("--message={message}");
                git(cfg, &w.path, &["commit", "--quiet", &msg], d).map_err(|e| {
                    Fail::Failed(format!("the changes are staged in {} but nothing was committed: {}", w.path.display(), e.message()))
                })?;
                let commit = git(cfg, &w.path, &["rev-parse", "HEAD"], d)?;
                Ok(json!({"commit": commit.trim(), "files": changed}))
            }
            other => Err(Fail::Failed(format!("reagent `git` has no tool `{other}`"))),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::git_fixture::{commit, dirty, fixture, fixture_at, sh, Fixture};
    use serde_json::json;
    use std::collections::HashSet;
    use std::sync::Mutex;

    fn grants_all() -> Vec<crate::ritual::Grant> {
        TOOLS.iter().map(|t| reagent::need("git", t.name, &[], &[])).collect()
    }

    fn call(f: &Fixture, tool: &str, args: Value) -> Result<Value, Fail> {
        let grants = grants_all();
        let selected = Mutex::new(HashSet::new());
        let auth = Authority { grants: &grants, deadline: None, on_item: None, selected: &selected, expected_digest: None, config: Some(&f.cfg) };
        Git.call(None, tool, &args, &auth)
    }

    #[test]
    fn status_reports_clean_and_changed() {
        let f = fixture();
        let s = call(&f, "status", json!({"repo": "demo"})).unwrap();
        assert_eq!(s["branch"], "master");
        assert_eq!(s["clean"], true);
        dirty(&f.main(), "a.txt");
        let s = call(&f, "status", json!({"repo": "demo"})).unwrap();
        assert_eq!(s["clean"], false);
        assert_eq!(s["changed"], json!(["a.txt"]));
        let wt = call(&f, "status", json!({"repo": "demo", "worktree": f.feature().to_str().unwrap()})).unwrap();
        assert_eq!(wt["branch"], "feature");
        assert_eq!(wt["clean"], true);
    }

    #[test]
    fn status_refuses_a_worktree_it_does_not_list() {
        let f = fixture();
        let e = call(&f, "status", json!({"repo": "demo", "worktree": "/tmp"})).unwrap_err();
        assert!(matches!(e, Fail::Denied(_)), "{e:?}");
    }

    #[test]
    fn ahead_behind_counts() {
        let f = fixture();
        let ab = call(&f, "ahead_behind", json!({"repo": "demo", "from": "master", "to": "feature"})).unwrap();
        assert_eq!((ab["ahead"].as_i64(), ab["behind"].as_i64()), (Some(0), Some(1)));
    }

    #[test]
    fn merged_detects_containment() {
        let f = fixture();
        assert_eq!(call(&f, "merged", json!({"repo": "demo", "branch": "feature"})).unwrap()["merged"], false);
        sh(&f.main(), &["merge", "-q", "--ff-only", "feature"]);
        assert_eq!(call(&f, "merged", json!({"repo": "demo", "branch": "feature"})).unwrap()["merged"], true);
    }

    #[test]
    fn head_and_remote_head() {
        let f = fixture();
        let local = call(&f, "head", json!({"repo": "demo", "ref": "master"})).unwrap();
        assert_eq!(local["commit"].as_str().unwrap().len(), 40);
        let remote = call(&f, "remote_head", json!({"repo": "demo", "branch": "master"})).unwrap();
        assert_eq!(remote["commit"], local["commit"]);
        assert_eq!(call(&f, "remote_head", json!({"repo": "demo", "branch": "feature"})).unwrap()["commit"], Value::Null);
    }

    #[test]
    fn worktrees_lists_main_and_feature() {
        let f = fixture();
        let list = call(&f, "worktrees", json!({"repo": "demo"})).unwrap();
        let list = list.as_array().unwrap();
        assert_eq!(list.len(), 2);
        assert_eq!(list[0]["main"], true);
        assert_eq!(list[0]["branch"], "master");
        assert_eq!(list[1]["main"], false);
        assert_eq!(list[1]["branch"], "feature");
        assert_eq!(list[1]["clean"], true);
    }

    #[test]
    fn branches_marks_merged_and_current() {
        let f = fixture();
        let b = call(&f, "branches", json!({"repo": "demo"})).unwrap();
        let by = |n: &str| b.as_array().unwrap().iter().find(|x| x["name"] == n).unwrap().clone();
        assert_eq!(by("master")["current"], true);
        assert_eq!(by("feature")["merged"], false);
    }

    #[test]
    fn repos_lists_configured_names() {
        let f = fixture();
        assert_eq!(call(&f, "repos", json!({})).unwrap(), json!(["demo"]));
    }

    #[test]
    fn unknown_repo_is_denied() {
        let f = fixture();
        assert!(matches!(call(&f, "status", json!({"repo": "nope"})), Err(Fail::Denied(_))));
    }

    #[test]
    fn bad_names_refused() {
        let f = fixture();
        for bad in ["-delete", "../x", "a b", "a..b", "x.lock", "@{u}", "x/", ""] {
            assert!(call(&f, "merged", json!({"repo": "demo", "branch": bad})).is_err(), "{bad}");
            assert!(valid_ref_name(bad).is_err(), "{bad}");
        }
        assert!(valid_ref_name("feature/one-2").is_ok());
    }

    #[test]
    fn paths_with_spaces_and_unicode_work() {
        let f = fixture_at("with space/ü");
        assert_eq!(call(&f, "status", json!({"repo": "demo"})).unwrap()["clean"], true);
        assert_eq!(call(&f, "worktrees", json!({"repo": "demo"})).unwrap().as_array().unwrap().len(), 2);
    }

    #[test]
    fn fetch_changes_no_local_branch() {
        let f = fixture();
        let before = sh(&f.main(), &["rev-parse", "master"]);
        // Someone else pushes to origin.
        let other = f.root.join("other");
        sh(&f.root, &["clone", "-q", "origin.git", "other"]);
        commit(&other, "c.txt", "three");
        sh(&other, &["push", "-q", "origin", "master"]);
        assert_eq!(call(&f, "fetch", json!({"repo": "demo"})).unwrap()["fetched"], true);
        assert_eq!(sh(&f.main(), &["rev-parse", "master"]), before, "local master untouched");
        let ab = call(&f, "ahead_behind", json!({"repo": "demo", "from": "master", "to": "origin/master"})).unwrap();
        assert_eq!(ab["behind"], 1);
    }

    // ------------------------------------------------------------- writes

    #[test]
    fn merge_ff_fast_forwards() {
        let f = fixture();
        let out = call(&f, "merge_ff", json!({"repo": "demo", "branch": "feature"})).unwrap();
        assert_eq!(out["commit"], sh(&f.feature(), &["rev-parse", "HEAD"]).as_str());
        assert_eq!(sh(&f.main(), &["rev-parse", "master"]), sh(&f.feature(), &["rev-parse", "HEAD"]));
    }

    #[test]
    fn merge_ff_refuses_non_ff() {
        let f = fixture();
        commit(&f.main(), "m.txt", "diverge");
        let before = sh(&f.main(), &["rev-parse", "master"]);
        let e = call(&f, "merge_ff", json!({"repo": "demo", "branch": "feature"})).unwrap_err();
        assert!(e.message().contains("does not fast-forward"), "{}", e.message());
        assert_eq!(sh(&f.main(), &["rev-parse", "master"]), before);
    }

    #[test]
    fn merge_ff_refuses_dirty_main() {
        let f = fixture();
        dirty(&f.main(), "a.txt");
        let e = call(&f, "merge_ff", json!({"repo": "demo", "branch": "feature"})).unwrap_err();
        assert!(e.message().contains("uncommitted changes") && e.message().contains("a.txt"), "{}", e.message());
        assert_eq!(std::fs::read_to_string(f.main().join("a.txt")).unwrap(), "uncommitted work");
    }

    #[test]
    fn push_updates_remote() {
        let f = fixture();
        call(&f, "merge_ff", json!({"repo": "demo", "branch": "feature"})).unwrap();
        let out = call(&f, "push", json!({"repo": "demo", "branch": "master"})).unwrap();
        assert_eq!(call(&f, "remote_head", json!({"repo": "demo", "branch": "master"})).unwrap()["commit"], out["commit"]);
    }

    #[test]
    fn push_rejected_by_hook_is_failed() {
        let f = fixture();
        let hook = f.origin().join("hooks/pre-receive");
        std::fs::write(&hook, "#!/bin/sh\necho 'protected branch' >&2\nexit 1\n").unwrap();
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&hook, std::fs::Permissions::from_mode(0o755)).unwrap();
        call(&f, "merge_ff", json!({"repo": "demo", "branch": "feature"})).unwrap();
        let e = call(&f, "push", json!({"repo": "demo", "branch": "master"})).unwrap_err();
        assert!(matches!(e, Fail::Failed(_)), "{e:?}");
    }

    #[test]
    fn delete_branch_refuses_unmerged_and_trunk() {
        let f = fixture();
        assert!(call(&f, "delete_branch", json!({"repo": "demo", "branch": "master"})).unwrap_err().message().contains("trunk"));
        sh(&f.main(), &["branch", "loose", "master"]);
        sh(&f.main(), &["worktree", "add", "-q", "../wt-loose", "loose"]);
        commit(&f.root.join("wt-loose"), "l.txt", "unmerged");
        sh(&f.main(), &["worktree", "remove", "../wt-loose"]);
        assert!(call(&f, "delete_branch", json!({"repo": "demo", "branch": "loose"})).unwrap_err().message().contains("not fully merged"));
        sh(&f.main(), &["branch", "done", "master"]);
        assert_eq!(call(&f, "delete_branch", json!({"repo": "demo", "branch": "done"})).unwrap()["deleted"], "done");
    }

    #[test]
    fn remove_worktree_refuses_dirty_and_main() {
        let f = fixture();
        let main = f.main().to_string_lossy().into_owned();
        assert!(call(&f, "remove_worktree", json!({"repo": "demo", "path": main})).unwrap_err().message().contains("main checkout"));
        dirty(&f.feature(), "b.txt");
        let path = f.feature().to_string_lossy().into_owned();
        assert!(call(&f, "remove_worktree", json!({"repo": "demo", "path": path})).unwrap_err().message().contains("uncommitted changes"));
        assert_eq!(std::fs::read_to_string(f.feature().join("b.txt")).unwrap(), "uncommitted work");
        sh(&f.feature(), &["checkout", "--", "b.txt"]);
        call(&f, "remove_worktree", json!({"repo": "demo", "path": path})).unwrap();
        assert!(!f.feature().exists());
    }

    #[test]
    fn no_forbidden_flags_in_source() {
        let src = include_str!("git_reagent.rs");
        let code = &src[..src.find("#[cfg(test)]").unwrap()];
        // As git arguments; `"clean"` alone is also a JSON field name, so the
        // `clean` command is matched where it would start a git call.
        for bad in ["\"--force\"", "\"-f\"", "\"-D\"", "\"reset\"", "\"stash\"", "&[\"clean\"", "\"rebase\"", "\"switch\"", "\"checkout\""] {
            assert!(!code.contains(bad), "forbidden {bad} in git_reagent.rs");
        }
    }


    // ------------------------------------------------------ final-review fixes

    fn ignore(path: &std::path::Path, pattern: &str) {
        let mut text = std::fs::read_to_string(path.join(".git/info/exclude")).unwrap_or_default();
        text.push_str(pattern);
        text.push('\n');
        std::fs::create_dir_all(path.join(".git/info")).unwrap();
        std::fs::write(path.join(".git/info/exclude"), text).unwrap();
    }

    #[test]
    fn merge_ff_refuses_to_overwrite_an_ignored_file() {
        let f = fixture();
        // The branch commits `.env`; the main checkout has its own, ignored one.
        commit(&f.feature(), ".env", "from the branch");
        ignore(&f.main(), ".env");
        std::fs::write(f.main().join(".env"), "SECRET").unwrap();
        let e = call(&f, "merge_ff", json!({"repo": "demo", "branch": "feature"})).unwrap_err();
        assert!(e.message().contains(".env"), "{}", e.message());
        assert_eq!(std::fs::read_to_string(f.main().join(".env")).unwrap(), "SECRET");
    }

    #[test]
    fn merge_ff_merges_only_the_expected_commit() {
        let f = fixture();
        let tested = sh(&f.feature(), &["rev-parse", "HEAD"]);
        commit(&f.feature(), "late.txt", "after the tests");
        let e = call(&f, "merge_ff", json!({"repo": "demo", "branch": "feature", "expect": tested})).unwrap_err();
        assert!(e.message().contains("moved"), "{}", e.message());
        let now = sh(&f.feature(), &["rev-parse", "HEAD"]);
        let ok = call(&f, "merge_ff", json!({"repo": "demo", "branch": "feature", "expect": now})).unwrap();
        assert_eq!(ok["commit"], now.as_str());
    }

    #[test]
    fn remove_worktree_keeps_ignored_files_unless_disposable() {
        let mut f = fixture();
        ignore(&f.main(), ".env.local");
        ignore(&f.main(), "target/");
        std::fs::write(f.feature().join(".env.local"), "notes").unwrap();
        let path = f.feature().to_string_lossy().into_owned();
        let e = call(&f, "remove_worktree", json!({"repo": "demo", "path": path})).unwrap_err();
        assert!(e.message().contains(".env.local"), "{}", e.message());
        assert!(f.feature().join(".env.local").exists());
        let plan = call(&f, "cleanup_plan", json!({"repo": "demo"})).unwrap();
        assert_eq!(plan["worktrees"][0]["action"], "keep", "{plan}");
        // Build output the owner lists as disposable doesn't block removal.
        std::fs::remove_file(f.feature().join(".env.local")).unwrap();
        std::fs::create_dir_all(f.feature().join("target/debug")).unwrap();
        std::fs::write(f.feature().join("target/debug/x"), "build").unwrap();
        assert!(call(&f, "remove_worktree", json!({"repo": "demo", "path": path})).is_err(), "target/ isn't disposable yet");
        f.cfg.repos.get_mut("demo").unwrap().disposable = vec!["target/".into()];
        sh(&f.main(), &["merge", "-q", "--ff-only", "feature"]);
        call(&f, "remove_worktree", json!({"repo": "demo", "path": path})).unwrap();
        assert!(!f.feature().exists());
    }

    #[test]
    fn prunable_and_locked_worktrees_are_reported_and_kept() {
        let f = fixture();
        sh(&f.main(), &["worktree", "add", "-q", "-b", "gone", "../wt-gone"]);
        std::fs::remove_dir_all(f.root.join("wt-gone")).unwrap();
        sh(&f.main(), &["worktree", "add", "-q", "-b", "held", "../wt-held"]);
        sh(&f.main(), &["worktree", "lock", "../wt-held"]);
        let list = call(&f, "worktrees", json!({"repo": "demo"})).unwrap();
        assert_eq!(list.as_array().unwrap().len(), 4, "{list}");
        let plan = call(&f, "cleanup_plan", json!({"repo": "demo"})).unwrap();
        let reason = |b: &str| plan["worktrees"].as_array().unwrap().iter().find(|w| w["branch"] == b).unwrap()["reason"].as_str().unwrap().to_string();
        assert!(reason("gone").contains("prunable"), "{plan}");
        assert!(reason("held").contains("locked"), "{plan}");
        let path = f.root.join("wt-held").to_string_lossy().into_owned();
        assert!(call(&f, "remove_worktree", json!({"repo": "demo", "path": path})).unwrap_err().message().contains("locked"));
    }

    #[test]
    fn bounded_output_keeps_only_the_end() {
        let script = "head -c 4000000 /dev/zero | tr '\\0' x; printf 'the end'";
        let (code, out, _) = run_bounded_kept("sh", &["-c", script], Path::new("/"), None, 4096).unwrap();
        assert_eq!(code, 0);
        assert!(out.len() <= 4096 && out.ends_with("the end"), "{} bytes", out.len());
    }

    #[test]
    fn change_summary_is_bounded() {
        assert_eq!(change_summary(&[]), "");
        assert_eq!(change_summary(&["a.txt".into()]), "a.txt");
        let many: Vec<String> = (0..200).map(|i| format!("target/generated/module_{i:03}.rs")).collect();
        let s = change_summary(&many);
        assert!(s.len() <= 300 && s.starts_with("200 paths: target/generated/module_000.rs") && s.ends_with(" more"), "{s}");
        let long = vec!["é".repeat(400)];
        assert!(change_summary(&long).len() <= 300);
    }

    #[test]
    fn git_commands_drop_inherited_location_variables() {
        let cmd = command("git", true);
        let envs: std::collections::HashMap<_, _> = cmd.get_envs().map(|(k, v)| (k.to_string_lossy().into_owned(), v.map(|v| v.to_string_lossy().into_owned()))).collect();
        for var in ["GIT_DIR", "GIT_WORK_TREE", "GIT_INDEX_FILE", "GIT_COMMON_DIR", "GIT_OBJECT_DIRECTORY"] {
            assert_eq!(envs.get(var), Some(&None), "{var} is removed");
        }
        assert_eq!(envs.get("SSH_ASKPASS_REQUIRE"), Some(&Some("never".into())));
        let plain = command("cargo", false);
        assert!(!plain.get_envs().any(|(k, _)| k == "LC_ALL"), "the test command keeps the user's locale");
    }

    #[test]
    fn a_background_child_cannot_hang_the_run() {
        let start = Instant::now();
        let dir = std::env::temp_dir();
        let (code, out, _) = run_bounded_as("sh", &["-c", "sleep 31.7 & echo started"], &dir, Some(Instant::now() + Duration::from_secs(20)), false).unwrap();
        assert_eq!(code, 0);
        assert_eq!(out.trim(), "started");
        assert!(start.elapsed() < Duration::from_secs(5), "took {:?}", start.elapsed());
        std::thread::sleep(Duration::from_millis(200));
        let left = Command::new("pgrep").args(["-f", "sleep 31.7"]).output().unwrap();
        assert!(left.stdout.is_empty(), "the background child was stopped");
    }

    #[test]
    fn a_timeout_stops_the_whole_process_group() {
        let dir = std::env::temp_dir();
        let r = run_bounded_as("sh", &["-c", "sleep 32.3; true"], &dir, Some(Instant::now() + Duration::from_millis(500)), false);
        assert!(matches!(r, Err(Fail::Exhausted(_))), "{r:?}");
        std::thread::sleep(Duration::from_millis(200));
        let left = Command::new("pgrep").args(["-f", "sleep 32.3"]).output().unwrap();
        assert!(left.stdout.is_empty(), "the grandchild was stopped too");
    }


    // ---------------------------------------------- start-branch / commit-work

    #[test]
    fn worktree_add_creates_a_sibling_worktree_at_the_trunk() {
        let f = fixture();
        let out = call(&f, "worktree_add", json!({"repo": "demo", "branch": "pacts/one"})).unwrap();
        let path = std::path::PathBuf::from(out["path"].as_str().unwrap());
        assert_eq!(path, f.root.join("main-pacts-one"));
        assert_eq!(sh(&path, &["rev-parse", "--abbrev-ref", "HEAD"]), "pacts/one");
        assert_eq!(out["commit"], sh(&f.main(), &["rev-parse", "master"]).as_str());
    }

    #[test]
    fn worktree_add_refuses_existing_branch_path_and_stale_trunk() {
        let f = fixture();
        assert!(call(&f, "worktree_add", json!({"repo": "demo", "branch": "feature"})).unwrap_err().message().contains("already exists"));
        std::fs::create_dir_all(f.root.join("main-taken")).unwrap();
        assert!(call(&f, "worktree_add", json!({"repo": "demo", "branch": "taken"})).unwrap_err().message().contains("already exists"));
        sh(&f.root, &["clone", "-q", "origin.git", "other"]);
        commit(&f.root.join("other"), "o.txt", "elsewhere");
        sh(&f.root.join("other"), &["push", "-q", "origin", "master"]);
        assert!(call(&f, "worktree_add", json!({"repo": "demo", "branch": "fresh"})).unwrap_err().message().contains("behind"));
    }

    #[test]
    fn commit_commits_everything_in_the_branch_worktree() {
        let f = fixture();
        std::fs::write(f.feature().join("b.txt"), "changed").unwrap();
        std::fs::write(f.feature().join("new.txt"), "new").unwrap();
        let out = call(&f, "commit", json!({"repo": "demo", "branch": "feature", "message": "-starts with a dash\n\nbody"})).unwrap();
        assert_eq!(out["commit"], sh(&f.feature(), &["rev-parse", "HEAD"]).as_str());
        assert_eq!(sh(&f.feature(), &["log", "-1", "--format=%B"]), "-starts with a dash\n\nbody");
        let mut files: Vec<String> = out["files"].as_array().unwrap().iter().map(|v| v.as_str().unwrap().to_string()).collect();
        files.sort();
        assert_eq!(files, ["b.txt", "new.txt"]);
        assert_eq!(call(&f, "status", json!({"repo": "demo", "worktree": f.feature().to_str().unwrap()})).unwrap()["clean"], true);
    }

    #[test]
    fn commit_refuses_secrets_and_leaves_them_untouched() {
        let f = fixture();
        std::fs::write(f.feature().join(".env"), "TOKEN=1").unwrap();
        std::fs::write(f.feature().join("ok.txt"), "fine").unwrap();
        let e = call(&f, "commit", json!({"repo": "demo", "branch": "feature", "message": "m"})).unwrap_err();
        assert!(e.message().contains(".env"), "{}", e.message());
        assert_eq!(std::fs::read_to_string(f.feature().join(".env")).unwrap(), "TOKEN=1");
        assert_eq!(sh(&f.feature(), &["diff", "--cached", "--name-only"]), "", "nothing was staged");
    }

    #[test]
    fn commit_refuses_large_files_empty_commits_trunk_and_main() {
        let f = fixture();
        std::fs::write(f.feature().join("big.bin"), vec![0u8; 5 * 1024 * 1024 + 1]).unwrap();
        assert!(call(&f, "commit", json!({"repo": "demo", "branch": "feature", "message": "m"})).unwrap_err().message().contains("big.bin"));
        std::fs::remove_file(f.feature().join("big.bin")).unwrap();
        assert!(call(&f, "commit", json!({"repo": "demo", "branch": "feature", "message": "m"})).unwrap_err().message().contains("nothing to commit"));
        dirty(&f.main(), "a.txt");
        assert!(call(&f, "commit", json!({"repo": "demo", "branch": "master", "message": "m"})).unwrap_err().message().contains("trunk"));
        assert!(call(&f, "commit", json!({"repo": "demo", "branch": "nowhere", "message": "m"})).unwrap_err().message().contains("worktree"));
        assert!(call(&f, "commit", json!({"repo": "demo", "branch": "feature", "message": "  "})).is_err(), "an empty message");
    }


    #[test]
    fn commit_refuses_an_unfinished_merge() {
        let f = fixture();
        commit(&f.main(), "c.txt", "main side");
        commit(&f.feature(), "c.txt", "branch side");
        let _ = Command::new("git").args(["merge", "-q", "master"]).current_dir(f.feature()).output().unwrap();
        let before = sh(&f.feature(), &["rev-parse", "HEAD"]);
        let e = call(&f, "commit", json!({"repo": "demo", "branch": "feature", "message": "m"})).unwrap_err();
        assert!(e.message().contains("merge"), "{}", e.message());
        assert_eq!(sh(&f.feature(), &["rev-parse", "HEAD"]), before);
    }

    #[test]
    fn commit_checks_oddly_named_files() {
        let f = fixture();
        let big = vec![0u8; 5 * 1024 * 1024 + 1];
        for name in ["q\"x.bin", "a -> b", "tab\tx"] {
            std::fs::write(f.feature().join(name), &big).unwrap();
            let e = call(&f, "commit", json!({"repo": "demo", "branch": "feature", "message": "m"})).unwrap_err();
            assert!(e.message().contains(name), "{name}: {}", e.message());
            std::fs::remove_file(f.feature().join(name)).unwrap();
        }
    }

    #[test]
    fn commit_refuses_more_credential_files() {
        let f = fixture();
        for name in [".git-credentials", ".netrc", ".npmrc", ".pypirc", ".pgpass", ".envrc", "store.jks", "x.gpg", "secrets.yaml", "service-account-prod.json", "sub/dir/.env"] {
            let path = f.feature().join(name);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(&path, "x").unwrap();
            let e = call(&f, "commit", json!({"repo": "demo", "branch": "feature", "message": "m"})).unwrap_err();
            assert!(e.message().contains(name.rsplit('/').next().unwrap()), "{name}: {}", e.message());
            std::fs::remove_file(&path).unwrap();
        }
    }

    #[test]
    fn commit_rejects_control_characters_before_staging() {
        let f = fixture();
        std::fs::write(f.feature().join("ok.txt"), "fine").unwrap();
        assert!(call(&f, "commit", json!({"repo": "demo", "branch": "feature", "message": "a\u{0}b"})).is_err());
        assert_eq!(sh(&f.feature(), &["diff", "--cached", "--name-only"]), "", "nothing was staged");
    }

    #[test]
    fn a_failed_commit_says_the_changes_are_staged() {
        let f = fixture();
        let hooks = f.main().join(".git/hooks");
        std::fs::create_dir_all(&hooks).unwrap();
        std::fs::write(hooks.join("pre-commit"), "#!/bin/sh\necho hook says no >&2\nexit 1\n").unwrap();
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(hooks.join("pre-commit"), std::fs::Permissions::from_mode(0o755)).unwrap();
        std::fs::write(f.feature().join("ok.txt"), "fine").unwrap();
        let e = call(&f, "commit", json!({"repo": "demo", "branch": "feature", "message": "m"})).unwrap_err();
        assert!(e.message().contains("staged") && e.message().contains("nothing was committed"), "{}", e.message());
    }

}
