//! Disposable copies for drilling write paths. A sandbox clones the owner's
//! configured repositories and copies the file roots a ritual requires into
//! a temporary directory, and rewrites paths from the real locations to the
//! copies. The drill then runs in a throwaway core that knows only the
//! copies, so a write the ritual makes can only land in the sandbox; the
//! directory is deleted when the sandbox is dropped.
use super::*;
use std::fs;
use std::io::ErrorKind;
use std::os::unix::fs::{DirBuilderExt, PermissionsExt};
use std::process::Command;

pub(crate) struct Sandbox {
    pub dir: PathBuf,
    /// Real path prefix -> its copy, longest first.
    map: Vec<(PathBuf, PathBuf)>,
    pub repos: Vec<String>,
    pub roots: Vec<String>,
}

impl Drop for Sandbox {
    fn drop(&mut self) {
        if fs::remove_dir_all(&self.dir).is_err() {
            // Archive copies can preserve read-only directories. Make any
            // remaining directories removable without following copied links.
            let mut pending = vec![self.dir.clone()];
            while let Some(path) = pending.pop() {
                let Ok(meta) = fs::symlink_metadata(&path) else {
                    continue;
                };
                if !meta.is_dir() {
                    continue;
                }
                let _ = fs::set_permissions(
                    &path,
                    fs::Permissions::from_mode(meta.permissions().mode() | 0o700),
                );
                if let Ok(entries) = fs::read_dir(&path) {
                    pending.extend(entries.filter_map(Result::ok).map(|entry| entry.path()));
                }
            }
            let _ = fs::remove_dir_all(&self.dir);
        }
    }
}

fn git(cwd: &Path, args: &[&str]) -> Result<String, RpcErr> {
    let out = crate::git_reagent::command("git", true, None)
        .args(args)
        .current_dir(cwd)
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

fn fs_error(error: std::io::Error) -> RpcErr {
    RpcErr::invalid(format!("sandbox: {error}"))
}

fn absolute_path(path: &str) -> Result<PathBuf, RpcErr> {
    crate::file_reagent::validate_root(path)
        .map_err(|error| RpcErr::invalid(format!("sandbox: {path:?}: {error}")))?;
    Ok(Path::new(path).components().collect())
}

fn refuse_symlink(path: &Path) -> RpcErr {
    RpcErr::invalid(format!("sandbox: symlink at {}", path.display()))
}

/// Check each existing component. A missing suffix can be created later
/// by the drill, for example when the root is a planned Git worktree.
fn existing_directory(path: &Path) -> Result<bool, RpcErr> {
    let mut prefix = PathBuf::new();
    for component in path.components() {
        prefix.push(component);
        match fs::symlink_metadata(&prefix) {
            Ok(meta) if meta.file_type().is_symlink() => return Err(refuse_symlink(&prefix)),
            Ok(meta) if meta.is_dir() => {}
            Ok(_) => {
                return Err(RpcErr::invalid(format!(
                    "sandbox: {} is not a directory",
                    prefix.display()
                )))
            }
            Err(e) if e.kind() == ErrorKind::NotFound => return Ok(false),
            Err(e) => return Err(fs_error(e)),
        }
    }
    Ok(true)
}

fn check_copied_tree(root: &Path) -> Result<(), RpcErr> {
    if !existing_directory(root)? {
        return Err(RpcErr::invalid(format!(
            "sandbox: missing copy at {}",
            root.display()
        )));
    }
    let mut dirs = vec![root.to_path_buf()];
    while let Some(dir) = dirs.pop() {
        for entry in fs::read_dir(dir).map_err(fs_error)? {
            let entry = entry.map_err(fs_error)?;
            let kind = entry.file_type().map_err(fs_error)?;
            if kind.is_symlink() {
                return Err(refuse_symlink(&entry.path()));
            }
            if kind.is_dir() {
                dirs.push(entry.path());
            }
        }
    }
    Ok(())
}

impl Sandbox {
    /// `s` with a real path prefix replaced by its copy. Only a whole path
    /// or a path under it is rewritten (`/a/b` and `/a/b/c`, not `/a/bc`).
    pub fn rewrite_str(&self, s: &str) -> String {
        absolute_path(s)
            .ok()
            .and_then(|path| self.rewrite_path(&path))
            .and_then(|path| path.into_os_string().into_string().ok())
            .unwrap_or_else(|| s.to_string())
    }

    fn rewrite_path(&self, path: &Path) -> Option<PathBuf> {
        self.map.iter().find_map(|(real, copy)| {
            let rest = path.strip_prefix(real).ok()?;
            Some(if rest.as_os_str().is_empty() {
                copy.clone()
            } else {
                copy.join(rest)
            })
        })
    }

    pub fn rewrite(&self, v: &Value) -> Value {
        match v {
            Value::String(s) => Value::String(self.rewrite_str(s)),
            Value::Array(a) => Value::Array(a.iter().map(|x| self.rewrite(x)).collect()),
            Value::Object(m) => Value::Object(
                m.iter()
                    .map(|(k, x)| (k.clone(), self.rewrite(x)))
                    .collect(),
            ),
            other => other.clone(),
        }
    }

    fn add_map(&mut self, real: PathBuf, copy: PathBuf) {
        self.map.push((real, copy));
        self.map
            .sort_by_key(|(real, _)| std::cmp::Reverse(real.components().count()));
    }

    /// Clone `repos` (from `cfg`) and copy `roots` into a new temporary
    /// directory. The real repositories and roots are only read.
    pub fn build(cfg: &Config, repos: &[String], roots: &[String]) -> Result<Sandbox, RpcErr> {
        Self::build_in(cfg, repos, roots, &std::env::temp_dir())
    }

    fn build_in(
        cfg: &Config,
        repos: &[String],
        roots: &[String],
        temp: &Path,
    ) -> Result<Sandbox, RpcErr> {
        let temp = temp.canonicalize().map_err(fs_error)?;
        if temp.to_str().is_none() {
            return Err(RpcErr::invalid(
                "sandbox: temporary parent path must be UTF-8",
            ));
        }
        let dir = temp.join(new_id("summonr-sandbox"));
        fs::DirBuilder::new()
            .mode(0o700)
            .create(&dir)
            .map_err(fs_error)?;
        let mut sb = Sandbox {
            dir,
            map: Vec::new(),
            repos: Vec::new(),
            roots: Vec::new(),
        };
        let dir = sb.dir.clone();
        fs::create_dir(dir.join("remotes")).map_err(fs_error)?;
        sb.repos = repos.to_vec();
        sb.roots = roots.to_vec();
        let mut repo_paths = Vec::new();
        for name in repos {
            let r = cfg.repos.get(name).ok_or_else(|| {
                RpcErr::invalid(format!(
                    "sandbox: `{name}` is not a repository in config.yaml `repos:`"
                ))
            })?;
            let real = absolute_path(&r.path)?;
            if !existing_directory(&real)? {
                return Err(RpcErr::invalid(format!(
                    "sandbox: repository {} does not exist",
                    r.path
                )));
            }
            repo_paths.push((real, r));
        }
        // A worktree is made beside its checkout. Map parent directories
        // shallowest first so a nested repo shares the outer repo's mapping.
        repo_paths.sort_by_key(|(real, _)| real.components().count());
        for (i, (real, r)) in repo_paths.into_iter().enumerate() {
            let (Some(parent), Some(leaf)) = (real.parent(), real.file_name()) else {
                return Err(RpcErr::invalid(format!(
                    "sandbox: repository path {} has no parent",
                    r.path
                )));
            };
            let temp_parent = match sb.rewrite_path(parent) {
                Some(mapped) => mapped,
                None => {
                    let mapped = dir.join(format!("p{i}"));
                    sb.add_map(parent.to_path_buf(), mapped.clone());
                    mapped
                }
            };
            fs::create_dir_all(&temp_parent).map_err(fs_error)?;
            // Committed history only: a bare copy stands in for the remote,
            // and the checkout is cloned from it, so pushes stay here too.
            let bare = dir.join("remotes").join(format!("{i}.git"));
            let bare_s = bare.to_str().unwrap();
            git(
                &dir,
                &[
                    "clone",
                    "--bare",
                    "--quiet",
                    "--no-local",
                    "--",
                    real.to_str().unwrap(),
                    bare_s,
                ],
            )?;
            let main = temp_parent.join(leaf);
            let main_s = main.to_str().unwrap();
            git(
                &dir,
                &[
                    "clone", "--quiet", "--origin", &r.remote, "--branch", &r.trunk, "--", bare_s,
                    main_s,
                ],
            )?;
            check_copied_tree(&main)?;
            for key in ["user.name", "user.email"] {
                if let Ok(v) = git(&real, &["config", "--get", key]) {
                    if !v.is_empty() {
                        git(&main, &["config", key, &v])?;
                    }
                }
            }
        }
        let mut file_roots = roots
            .iter()
            .map(|root| absolute_path(root))
            .collect::<Result<Vec<_>, _>>()?;
        // A copied parent already contains its child roots. Process it first
        // so overlapping declarations see the same files in the drill.
        file_roots.sort_by_key(|root| root.components().count());
        for (i, root) in file_roots.into_iter().enumerate() {
            let exists = existing_directory(&root)?;
            let copy = match sb.rewrite_path(&root) {
                Some(mapped) => mapped,
                None => {
                    let leaf = root
                        .file_name()
                        .unwrap_or_else(|| std::ffi::OsStr::new("root"));
                    let copy = dir.join(format!("r{i}")).join(leaf);
                    sb.add_map(root.clone(), copy.clone());
                    copy
                }
            };
            // A missing root can be a worktree that the drill will create.
            if exists && !existing_directory(&copy)? {
                if copy.starts_with(&root) {
                    return Err(RpcErr::invalid(format!(
                        "sandbox: copy is inside source root {}",
                        root.display()
                    )));
                }
                if let Some(parent) = copy.parent() {
                    fs::create_dir_all(parent).map_err(fs_error)?;
                }
                let status = Command::new("cp")
                    .args(["-a", "--reflink=auto", "--no-target-directory", "--"])
                    .arg(&root)
                    .arg(&copy)
                    .status()
                    .map_err(fs_error)?;
                if !status.success() {
                    return Err(RpcErr::invalid(format!(
                        "sandbox: copying {} failed",
                        root.display()
                    )));
                }
                check_copied_tree(&copy)?;
            }
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
        let mut sb = test_dir();
        sb.add_map("/lab".into(), "/tmp/sb/p0".into());
        sb.add_map("/lab/home".into(), "/tmp/sb/r1/home".into());
        assert_eq!(sb.rewrite_str("/lab"), "/tmp/sb/p0");
        assert_eq!(
            sb.rewrite_str("/lab/target-repro/src"),
            "/tmp/sb/p0/target-repro/src"
        );
        assert_eq!(sb.rewrite_str("/lab/home/x"), "/tmp/sb/r1/home/x");
        assert_eq!(
            sb.rewrite_str("/labs/other"),
            "/labs/other",
            "a sibling with a longer name is not under /lab"
        );
        assert_eq!(
            sb.rewrite_str("see /lab"),
            "see /lab",
            "only whole strings that are paths"
        );
        assert_eq!(
            sb.rewrite(&json!({"a": ["/lab/x", 1]})),
            json!({"a": ["/tmp/sb/p0/x", 1]})
        );
    }

    fn test_dir() -> Sandbox {
        let dir = std::env::temp_dir().join(new_id("summonr-sandbox-test"));
        std::fs::create_dir(&dir).unwrap();
        Sandbox {
            dir,
            map: Vec::new(),
            repos: vec![],
            roots: vec![],
        }
    }

    #[test]
    fn path_rewriting_handles_trailing_slashes_and_empty_strings() {
        let mut sb = test_dir();
        sb.add_map(absolute_path("/lab/").unwrap(), "/tmp/copy".into());
        sb.add_map(
            absolute_path("/lab/project//").unwrap(),
            "/tmp/project".into(),
        );
        assert!(absolute_path("").is_err());
        assert_eq!(sb.rewrite_str("/lab/project//src"), "/tmp/project/src");
        assert_eq!(sb.rewrite_str("/lab/project/"), "/tmp/project");
        assert_eq!(sb.rewrite_str("/lab/"), "/tmp/copy");
        assert_eq!(sb.rewrite_str("/other"), "/other");
        assert_eq!(sb.rewrite_str(""), "");
        assert_eq!(sb.rewrite_str("/lab/../elsewhere"), "/lab/../elsewhere");
        sb.add_map("/".into(), "/tmp/root".into());
        assert_eq!(sb.rewrite_str("/other"), "/tmp/root/other");
    }

    #[test]
    fn rejects_linked_roots_ancestors_and_nested_symlinks() {
        use std::os::unix::fs::symlink;
        let fixture = test_dir();
        let source = fixture.dir.join("source");
        std::fs::create_dir_all(source.join("child")).unwrap();
        let alias = fixture.dir.join("alias");
        symlink(&source, &alias).unwrap();
        let dangling = fixture.dir.join("dangling");
        symlink(fixture.dir.join("missing"), &dangling).unwrap();
        for root in [alias.clone(), alias.join("child"), dangling, source.clone()] {
            if root == source {
                symlink("../missing", source.join("link")).unwrap();
            }
            let error = Sandbox::build(&Config::default(), &[], &[root.to_str().unwrap().into()])
                .err()
                .expect("symlink should be refused");
            assert!(error.message.contains("symlink"), "{}", error.message);
        }

        let cfg: Config = serde_json::from_value(json!({
            "repos": {"test": {"path": alias}}
        }))
        .unwrap();
        let error = Sandbox::build(&cfg, &["test".into()], &[])
            .err()
            .expect("linked repository should be refused");
        assert!(error.message.contains("symlink"), "{}", error.message);
    }

    #[test]
    fn copies_non_utf8_entries_under_a_utf8_root() {
        use std::ffi::OsStr;
        use std::os::unix::ffi::OsStrExt;
        let fixture = test_dir();
        let source = fixture.dir.join("source");
        std::fs::create_dir(&source).unwrap();
        let entry = OsStr::from_bytes(b"entry-\xff");
        std::fs::write(source.join(entry), b"content").unwrap();
        let root = format!("{}/", source.display());
        let sb = Sandbox::build(&Config::default(), &[], std::slice::from_ref(&root)).unwrap();
        let copied = PathBuf::from(sb.rewrite_str(&root));
        assert_eq!(std::fs::read(copied.join(entry)).unwrap(), b"content");
        assert!(sb
            .rewrite_str(&format!("{root}child"))
            .starts_with(copied.to_str().unwrap()));
    }
    #[test]
    fn temp_directory_is_removed_on_build_error_and_unwinding() {
        let fixture = test_dir();
        let source = test_dir();
        std::os::unix::fs::symlink("missing", source.dir.join("dangling")).unwrap();
        let result = Sandbox::build_in(
            &Config::default(),
            &[],
            &[source.dir.to_str().unwrap().into()],
            &fixture.dir,
        );
        assert!(result.err().unwrap().message.contains("symlink"));
        assert_eq!(fs::read_dir(&fixture.dir).unwrap().count(), 0);

        let clean = Sandbox::build_in(&Config::default(), &[], &[], &fixture.dir).unwrap();
        let dir = clean.dir.clone();
        let readonly = dir.join("readonly");
        fs::create_dir(&readonly).unwrap();
        fs::write(readonly.join("entry"), "data").unwrap();
        fs::set_permissions(readonly, fs::Permissions::from_mode(0o500)).unwrap();
        let result = std::panic::catch_unwind(move || {
            let _keep_until_unwind = clean;
            panic!("test unwinding");
        });
        assert!(result.is_err());
        assert!(!dir.exists());
    }

    #[test]
    fn rejects_a_non_utf8_temporary_parent_before_creating_a_copy() {
        use std::ffi::OsStr;
        use std::os::unix::ffi::OsStrExt;
        let fixture = test_dir();
        let tmp = fixture.dir.join(OsStr::from_bytes(b"temp-\xff"));
        fs::create_dir(&tmp).unwrap();
        let error = Sandbox::build_in(&Config::default(), &[], &[], &tmp)
            .err()
            .expect("sandbox paths must fit in string-valued configuration");
        assert!(error.message.contains("UTF-8"));
        assert_eq!(fs::read_dir(tmp).unwrap().count(), 0);
    }

    fn commit_fixture(repo: &Path) {
        git(repo, &["add", "."]).unwrap();
        git(
            repo,
            &[
                "-c",
                "user.name=Fixture",
                "-c",
                "user.email=fixture@example.invalid",
                "-c",
                "commit.gpgSign=false",
                "commit",
                "--quiet",
                "-m",
                "Fixture",
            ],
        )
        .unwrap();
    }

    #[test]
    fn overlapping_repo_and_file_roots_share_the_same_copies() {
        let fixture = test_dir();
        let outer = fixture.dir.join("outer");
        fs::create_dir_all(outer.join("files/child")).unwrap();
        git(&outer, &["init", "--quiet", "--initial-branch=master"]).unwrap();
        fs::write(outer.join("files/child/data"), "outer").unwrap();
        commit_fixture(&outer);

        let inner = outer.join("inner");
        fs::create_dir(&inner).unwrap();
        git(&inner, &["init", "--quiet", "--initial-branch=master"]).unwrap();
        fs::write(inner.join("data"), "inner").unwrap();
        commit_fixture(&inner);

        let cfg: Config = serde_json::from_value(json!({
            "repos": {"outer": {"path": outer}, "inner": {"path": inner}}
        }))
        .unwrap();
        let sb = Sandbox::build(
            &cfg,
            &["inner".into(), "outer".into()],
            &[
                outer.join("files/child").to_str().unwrap().into(),
                outer.join("files").to_str().unwrap().into(),
            ],
        )
        .unwrap();
        let mapped = sb.config(&cfg);
        let outer_copy = Path::new(&mapped.repos["outer"].path);
        let inner_copy = Path::new(&mapped.repos["inner"].path);
        assert!(outer_copy.join(".git").is_dir());
        assert_eq!(inner_copy, outer_copy.join("inner"));
        assert_eq!(
            fs::read_to_string(inner_copy.join("data")).unwrap(),
            "inner"
        );
        assert_eq!(
            fs::read_to_string(outer_copy.join("files/child/data")).unwrap(),
            "outer"
        );

        // A freshly committed symbolic link in the checkout is refused.
        std::os::unix::fs::symlink("../files", inner.join("linked")).unwrap();
        commit_fixture(&inner);
        let error = Sandbox::build(&cfg, &["inner".into()], &[])
            .err()
            .expect("linked checkout entry should be refused");
        assert!(error.message.contains("symlink"), "{}", error.message);
    }
}
