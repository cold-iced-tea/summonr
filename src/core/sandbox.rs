//! Disposable copies for drilling write paths. A sandbox clones the owner's
//! configured repositories and copies the file roots a ritual requires into
//! a temporary directory, and rewrites paths from the real locations to the
//! copies. The drill then runs in a throwaway core that knows only the
//! copies, so a write the ritual makes can only land in the sandbox; the
//! directory is deleted when the sandbox is dropped.
use super::*;
use std::process::Command;

pub(crate) struct Sandbox {
    pub dir: PathBuf,
    /// Real path prefix -> its copy, longest first.
    map: Vec<(String, String)>,
    pub repos: Vec<String>,
    pub roots: Vec<String>,
}

impl Drop for Sandbox {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

fn git(cwd: &Path, args: &[&str]) -> Result<String, RpcErr> {
    let out = Command::new("git")
        .args(args)
        .current_dir(cwd)
        .env("GIT_TERMINAL_PROMPT", "0")
        .output()
        .map_err(|e| RpcErr::invalid(format!("sandbox: cannot run git: {e}")))?;
    if !out.status.success() {
        return Err(RpcErr::invalid(format!(
            "sandbox: git {} failed: {}",
            args.join(" "),
            String::from_utf8_lossy(&out.stderr).trim()
        )));
    }
    Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
}

impl Sandbox {
    /// `s` with a real path prefix replaced by its copy. Only a whole path
    /// or a path under it is rewritten (`/a/b` and `/a/b/c`, not `/a/bc`).
    pub fn rewrite_str(&self, s: &str) -> String {
        for (real, copy) in &self.map {
            if s == real {
                return copy.clone();
            }
            if let Some(rest) = s.strip_prefix(real.as_str()) {
                if rest.starts_with('/') {
                    return format!("{copy}{rest}");
                }
            }
        }
        s.to_string()
    }

    pub fn rewrite(&self, v: &Value) -> Value {
        match v {
            Value::String(s) => Value::String(self.rewrite_str(s)),
            Value::Array(a) => Value::Array(a.iter().map(|x| self.rewrite(x)).collect()),
            Value::Object(m) => Value::Object(m.iter().map(|(k, x)| (k.clone(), self.rewrite(x))).collect()),
            other => other.clone(),
        }
    }

    fn add_map(&mut self, real: String, copy: String) {
        self.map.push((real, copy));
        self.map.sort_by_key(|(real, _)| std::cmp::Reverse(real.len()));
    }

    /// Clone `repos` (from `cfg`) and copy `roots` into a new temporary
    /// directory. The real repositories and roots are only read.
    pub fn build(cfg: &Config, repos: &[String], roots: &[String]) -> Result<Sandbox, RpcErr> {
        let dir = std::env::temp_dir().join(new_id("summonr-sandbox"));
        std::fs::create_dir_all(dir.join("remotes"))
            .map_err(|e| RpcErr::invalid(format!("sandbox: cannot create {}: {e}", dir.display())))?;
        let mut sb = Sandbox { dir: dir.clone(), map: Vec::new(), repos: Vec::new(), roots: Vec::new() };
        // A worktree is made beside its main checkout, so each repository's
        // parent directory maps to a temporary one, shared by repositories
        // that share a parent.
        let mut parents: Vec<(String, PathBuf)> = Vec::new();
        for name in repos {
            let r = cfg.repos.get(name).ok_or_else(|| {
                RpcErr::invalid(format!("sandbox: `{name}` is not a repository in config.yaml `repos:`"))
            })?;
            let real = PathBuf::from(&r.path);
            let (Some(parent), Some(leaf)) = (real.parent(), real.file_name()) else {
                return Err(RpcErr::invalid(format!("sandbox: repository path {} has no parent", r.path)));
            };
            let parent_s = parent.to_string_lossy().into_owned();
            let temp_parent = match parents.iter().find(|(p, _)| *p == parent_s) {
                Some((_, t)) => t.clone(),
                None => {
                    let t = dir.join(format!("p{}", parents.len()));
                    std::fs::create_dir_all(&t).map_err(|e| RpcErr::invalid(format!("sandbox: {e}")))?;
                    parents.push((parent_s.clone(), t.clone()));
                    sb.add_map(parent_s.clone(), t.to_string_lossy().into_owned());
                    t
                }
            };
            // Committed history only: a bare copy stands in for the remote,
            // and the checkout is cloned from it, so pushes stay here too.
            let bare = dir.join("remotes").join(format!("{name}.git"));
            let bare_s = bare.to_string_lossy().into_owned();
            git(&dir, &["clone", "--bare", "--quiet", &r.path, &bare_s])?;
            let main = temp_parent.join(leaf);
            let main_s = main.to_string_lossy().into_owned();
            git(&dir, &["clone", "--quiet", "--origin", &r.remote, "--branch", &r.trunk, &bare_s, &main_s])?;
            for key in ["user.name", "user.email"] {
                if let Ok(v) = git(&real, &["config", "--get", key]) {
                    if !v.is_empty() {
                        git(&main, &["config", key, &v])?;
                    }
                }
            }
            sb.repos.push(name.clone());
        }
        for (i, root) in roots.iter().enumerate() {
            let mapped = sb.rewrite_str(root);
            let copy = if mapped != *root {
                PathBuf::from(mapped)
            } else {
                let leaf = Path::new(root).file_name().map(|l| l.to_os_string()).unwrap_or_else(|| "root".into());
                let copy = dir.join(format!("r{i}")).join(leaf);
                sb.add_map(root.clone(), copy.to_string_lossy().into_owned());
                copy
            };
            // A root that does not exist yet (a worktree the ritual will add)
            // or that is inside a cloned checkout needs no copy.
            if Path::new(root).is_dir() && !copy.exists() {
                if let Some(p) = copy.parent() {
                    std::fs::create_dir_all(p).map_err(|e| RpcErr::invalid(format!("sandbox: {e}")))?;
                }
                let st = Command::new("cp")
                    .args(["-a", "--reflink=auto", root, &copy.to_string_lossy()])
                    .status()
                    .map_err(|e| RpcErr::invalid(format!("sandbox: cannot copy {root}: {e}")))?;
                if !st.success() {
                    return Err(RpcErr::invalid(format!("sandbox: copying {root} failed")));
                }
            }
            sb.roots.push(root.clone());
        }
        Ok(sb)
    }

    /// The configuration the sandbox's core runs with: only the cloned
    /// repositories (at their copies), only commands whose working
    /// directory is inside the sandbox, everything else as configured.
    pub fn config(&self, cfg: &Config) -> Config {
        let mut c = cfg.clone();
        c.repos = cfg
            .repos
            .iter()
            .filter(|(n, _)| self.repos.contains(n))
            .map(|(n, r)| {
                let mut r = r.clone();
                r.path = self.rewrite_str(&r.path);
                (n.clone(), r)
            })
            .collect();
        c.commands = cfg
            .commands
            .iter()
            .filter_map(|(n, cmd)| {
                let cwd = self.rewrite_str(&cmd.cwd);
                (cwd != cmd.cwd).then(|| {
                    let mut cmd = cmd.clone();
                    cmd.cwd = cwd;
                    (n.clone(), cmd)
                })
            })
            .collect();
        c
    }

    pub fn describe(&self) -> Value {
        json!({"repos": self.repos, "roots": self.roots, "deleted": true})
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rewriting_replaces_whole_path_prefixes_only_longest_first() {
        let mut sb = Sandbox { dir: PathBuf::from("/nonexistent-sandbox"), map: Vec::new(), repos: vec![], roots: vec![] };
        sb.add_map("/lab".into(), "/tmp/sb/p0".into());
        sb.add_map("/lab/home".into(), "/tmp/sb/r1/home".into());
        assert_eq!(sb.rewrite_str("/lab"), "/tmp/sb/p0");
        assert_eq!(sb.rewrite_str("/lab/target-repro/src"), "/tmp/sb/p0/target-repro/src");
        assert_eq!(sb.rewrite_str("/lab/home/x"), "/tmp/sb/r1/home/x");
        assert_eq!(sb.rewrite_str("/labs/other"), "/labs/other", "a sibling with a longer name is not under /lab");
        assert_eq!(sb.rewrite_str("see /lab"), "see /lab", "only whole strings that are paths");
        assert_eq!(sb.rewrite(&json!({"a": ["/lab/x", 1]})), json!({"a": ["/tmp/sb/p0/x", 1]}));
    }
}
