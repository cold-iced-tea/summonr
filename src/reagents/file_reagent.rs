//! Descriptor-relative filesystem access. Never follows symlinks.
use crate::reagent::{self, Authority, Fail, Reagent, ToolSpec};
use serde_json::{json, Value};
use std::{
    ffi::CString,
    fs::File,
    io::{Read, Write},
    os::fd::{AsRawFd, FromRawFd},
    path::{Component, Path},
    time::Instant,
};
pub struct Files;
const TOOLS: &[ToolSpec] = &[
    ToolSpec {
        reagent: "file",
        name: "read_text",
        effect: "read",
        determinism: "deterministic over live state",
        description: "Read bounded UTF-8 text beneath a granted directory; no symlinks",
    },
    ToolSpec {
        reagent: "file",
        name: "read_json",
        effect: "read",
        determinism: "deterministic over live state",
        description: "Read and parse bounded UTF-8 JSON beneath a granted directory; no symlinks",
    },
    ToolSpec {
        reagent: "file",
        name: "stat",
        effect: "read",
        determinism: "deterministic over live state",
        description: "Inspect whether a path beneath a granted directory exists, its kind, size, and SHA-256; no symlinks",
    },
    ToolSpec {
        reagent: "file",
        name: "list",
        effect: "read",
        determinism: "deterministic over live state",
        description: "List one directory, bounded and sorted; no recursive traversal",
    },
    ToolSpec {
        reagent: "file",
        name: "write_text",
        effect: "idempotent write",
        determinism: "deterministic",
        description: "Write bounded UTF-8 text to a regular file beneath a granted directory; no symlinks",
    },
    ToolSpec {
        reagent: "file",
        name: "append_jsonl",
        effect: "write",
        determinism: "deterministic",
        description: "Append one canonical JSON line to a regular file beneath a granted directory; no symlinks",
    },
    ToolSpec {
        reagent: "file",
        name: "delete",
        effect: "write",
        determinism: "deterministic",
        description: "Delete one regular file beneath a granted directory; no symlinks",
    },
];
pub fn validate_root(root: &str) -> Result<(), String> {
    if !Path::new(root).is_absolute()
        || root.contains('\0')
        || root.split('/').any(|x| x == ".." || x == ".")
    {
        return Err("file root must be an absolute directory without dot components".into());
    }
    Ok(())
}
fn segments(path: &str) -> Result<Vec<&str>, Fail> {
    if path.contains('\0')
        || Path::new(path).is_absolute()
        || path.split('/').any(|x| x == ".." || x == ".")
    {
        return Err(Fail::Denied(
            "path must be relative with no dot components".into(),
        ));
    }
    Ok(path.split('/').filter(|s| !s.is_empty()).collect())
}
fn open_at(parent: &File, name: &str, directory: bool) -> Result<File, Fail> {
    let name = CString::new(name).map_err(|_| Fail::Invalid("NUL in path".into()))?;
    let flags = libc::O_RDONLY
        | libc::O_CLOEXEC
        | libc::O_NOFOLLOW
        | libc::O_NONBLOCK
        | if directory { libc::O_DIRECTORY } else { 0 };
    let fd = unsafe { libc::openat(parent.as_raw_fd(), name.as_ptr(), flags) };
    if fd < 0 {
        return Err(Fail::Failed(format!(
            "cannot open path component (symlinks forbidden): {}",
            std::io::Error::last_os_error()
        )));
    }
    Ok(unsafe { File::from_raw_fd(fd) })
}
fn open_root_dir(root: &str) -> Result<File, Fail> {
    validate_root(root).map_err(Fail::Denied)?;
    let mut fd = File::open("/").map_err(|e| Fail::Failed(e.to_string()))?;
    for c in Path::new(root).components() {
        if let Component::Normal(n) = c {
            fd = open_at(
                &fd,
                n.to_str()
                    .ok_or_else(|| Fail::Invalid("non-UTF-8 root".into()))?,
                true,
            )?;
        }
    }
    Ok(fd)
}
fn open_parent_and_leaf<'a>(root: &str, path: &'a str) -> Result<(File, &'a str), Fail> {
    let mut fd = open_root_dir(root)?;
    let parts = segments(path)?;
    let (&leaf, parents) = parts
        .split_last()
        .ok_or_else(|| Fail::Invalid("file operation needs a relative file path".into()))?;
    for part in parents {
        fd = open_at(&fd, part, true)?;
    }
    Ok((fd, leaf))
}
fn open(root: &str, path: &str, directory: bool) -> Result<File, Fail> {
    let mut fd = open_root_dir(root)?;
    let parts = segments(path)?;
    if parts.is_empty() && !directory {
        return Err(Fail::Invalid("read_text needs a file path".into()));
    }
    for (i, part) in parts.iter().enumerate() {
        fd = open_at(&fd, part, directory || i + 1 < parts.len())?;
    }
    Ok(fd)
}
fn read_bounded_bytes(file: &mut File, max: usize, auth: Option<&Authority>) -> Result<Vec<u8>, Fail> {
    if !file
        .metadata()
        .map_err(|e| Fail::Failed(e.to_string()))?
        .is_file()
    {
        return Err(Fail::Denied("operation accepts only regular files".into()));
    }
    let mut data = Vec::new();
    let mut chunk = [0; 8192];
    loop {
        if let Some(a) = auth {
            live(a)?;
        }
        let n = file
            .read(&mut chunk)
            .map_err(|e| Fail::Failed(e.to_string()))?;
        if n == 0 {
            break;
        }
        if data.len() + n > max {
            return Err(Fail::Exhausted(
                "file exceeds max_bytes; no partial text returned".into(),
            ));
        }
        data.extend_from_slice(&chunk[..n]);
    }
    Ok(data)
}
fn stat_leaf(parent: &File, leaf: &str) -> Result<Option<(String, u64)>, Fail> {
    let cname = CString::new(leaf).map_err(|_| Fail::Invalid("NUL in path".into()))?;
    let mut st: libc::stat = unsafe { std::mem::zeroed() };
    let rc = unsafe {
        libc::fstatat(
            parent.as_raw_fd(),
            cname.as_ptr(),
            &mut st,
            libc::AT_SYMLINK_NOFOLLOW,
        )
    };
    if rc < 0 {
        let err = std::io::Error::last_os_error();
        if err.raw_os_error() == Some(libc::ENOENT) {
            return Ok(None);
        }
        return Err(Fail::Failed(format!("cannot stat `{leaf}`: {err}")));
    }
    let kind = match st.st_mode & libc::S_IFMT {
        libc::S_IFLNK => return Err(Fail::Denied("cannot access symlink (symlinks forbidden)".into())),
        libc::S_IFREG => "file",
        libc::S_IFDIR => "directory",
        _ => "other",
    };
    Ok(Some((kind.to_string(), st.st_size as u64)))
}
/// Digest of the target file before a held write (`"missing"` if absent, or `"sha256:<hex>"`).
pub fn target_digest(args: &Value) -> Result<Option<String>, Fail> {
    let (root, path) = root_and_path(args)?;
    let (parent, leaf) = open_parent_and_leaf(root, path)?;
    match stat_leaf(&parent, leaf)? {
        None => Ok(Some("missing".into())),
        Some((kind, _)) if kind != "file" => Err(Fail::Denied(format!(
            "target `{path}` is a {kind}, not a regular file"
        ))),
        Some(_) => {
            let mut f = open_at(&parent, leaf, false)?;
            let bytes = read_bounded_bytes(&mut f, 1048576, None)?;
            Ok(Some(format!("sha256:{}", crate::util::sha256_hex(&bytes))))
        }
    }
}

fn verify_expected_digest(args: &Value, path: &str, expected: Option<&str>) -> Result<(), Fail> {
    if let Some(exp) = expected {
        let current = target_digest(args)?.unwrap_or_default();
        if current != exp {
            return Err(Fail::Failed(format!(
                "file `{path}` changed since held (expected {exp}, found {current})"
            )));
        }
    }
    Ok(())
}
fn open_write_leaf(parent: &File, leaf: &str, append: bool) -> Result<File, Fail> {
    if let Some((kind, _)) = stat_leaf(parent, leaf)? {
        if kind != "file" {
            return Err(Fail::Denied(format!(
                "cannot write to `{leaf}`: target is a {kind}"
            )));
        }
    }
    let cname = CString::new(leaf).map_err(|_| Fail::Invalid("NUL in path".into()))?;
    let flags = libc::O_WRONLY
        | libc::O_CREAT
        | libc::O_CLOEXEC
        | libc::O_NOFOLLOW
        | if append { libc::O_APPEND } else { libc::O_TRUNC };
    let fd = unsafe { libc::openat(parent.as_raw_fd(), cname.as_ptr(), flags, 0o600) };
    if fd < 0 {
        return Err(Fail::Failed(format!(
            "cannot open `{leaf}` for writing (symlinks forbidden): {}",
            std::io::Error::last_os_error()
        )));
    }
    Ok(unsafe { File::from_raw_fd(fd) })
}
fn root_and_path(args: &Value) -> Result<(&str, &str), Fail> {
    let text = |key: &str| {
        args[key]
            .as_str()
            .ok_or_else(|| Fail::Invalid(format!("file tools need a string argument `{key}`")))
    };
    Ok((text("root")?, text("path")?))
}
fn live(auth: &Authority) -> Result<(), Fail> {
    if auth.deadline.is_some_and(|d| Instant::now() >= d) {
        Err(Fail::Exhausted("file read deadline".into()))
    } else {
        Ok(())
    }
}
impl Reagent for Files {
    fn name(&self) -> &'static str {
        "file"
    }
    fn tools(&self) -> &'static [ToolSpec] {
        TOOLS
    }
    fn schema(&self, name: &str) -> Option<Value> {
        let mut properties = json!({"root":{"type":"string","minLength":1,"maxLength":4096},"path":{"type":"string","maxLength":4096}});
        let mut required = vec!["root", "path"];
        match name {
            "read_text" => {
                properties["max_bytes"] = json!({"type":"integer","minimum":1,"maximum":1048576})
            }
            "read_json" => {
                properties["max_bytes"] = json!({"type":"integer","minimum":1,"maximum":1048576});
                properties["schema"] = json!({"type":"object"});
            }
            "stat" | "delete" => {}
            "list" => {
                properties["max_entries"] = json!({"type":"integer","minimum":1,"maximum":10000})
            }
            "write_text" => {
                properties["text"] = json!({"type":"string","maxLength":1048576});
                required.push("text");
            }
            "append_jsonl" => {
                properties["record"] = json!({});
                required.push("record");
            }
            _ => return None,
        };
        Some(
            json!({"type":"object","properties":properties,"required":required,"additionalProperties":false}),
        )
    }
    fn authorize(
        &self,
        _: Option<&str>,
        name: &str,
        args: &Value,
        auth: &Authority,
    ) -> Result<(), Fail> {
        let (root, path) = root_and_path(args)?;
        validate_root(root).map_err(Fail::Denied)?;
        reagent::require(auth, reagent::need("file", name, &[("root", root)], &[]))?;
        // Legacy/broad grants do not bypass explicit directory scoping.
        if !auth.grants.iter().any(|g| {
            g.reagent == "file" && g.action == name && g.on.get("root").is_some_and(|r| r == root)
        }) {
            return Err(Fail::Denied(
                "file access requires an explicit root grant".into(),
            ));
        }
        // Path syntax only; `open` checks it again and refuses symlinks
        // component by component while it walks.
        let parts = segments(path)?;
        if parts.is_empty() && name != "list" && name != "stat" {
            return Err(Fail::Invalid(format!("file.{name} needs a relative file path")));
        }
        Ok(())
    }
    fn call(
        &self,
        mail_root: Option<&str>,
        name: &str,
        args: &Value,
        auth: &Authority,
    ) -> Result<Value, Fail> {
        self.authorize(mail_root, name, args, auth)?;
        let (root, path) = root_and_path(args)?;
        live(auth)?;
        match name {
            "read_text" | "read_json" => {
                let mut file = open(root, path, false)?;
                let max = args["max_bytes"].as_u64().unwrap_or(1048576) as usize;
                let data = read_bounded_bytes(&mut file, max, Some(auth))?;
                let hash = crate::util::sha256_hex(&data);
                let bytes = data.len();
                let text = String::from_utf8(data)
                    .map_err(|_| Fail::Invalid("file is not valid UTF-8".into()))?;
                if name == "read_json" {
                    let value: Value = serde_json::from_str(&text)
                        .map_err(|e| Fail::Invalid(format!("invalid JSON in `{path}`: {e}")))?;
                    if let Some(schema) = args.get("schema") {
                        crate::schema::check(schema, &value, "parsed")
                            .map_err(Fail::Invalid)?;
                    }
                    Ok(json!({"root":root,"path":path,"value":value,"bytes":bytes,"sha256":hash}))
                } else {
                    Ok(json!({"root":root,"path":path,"text":text,"bytes":bytes,"sha256":hash}))
                }
            }
            "stat" => {
                let parts = segments(path)?;
                if parts.is_empty() {
                    let _ = open_root_dir(root)?;
                    return Ok(json!({"root":root,"path":path,"exists":true,"kind":"directory","bytes":null,"sha256":null}));
                }
                let (parent, leaf) = open_parent_and_leaf(root, path)?;
                match stat_leaf(&parent, leaf)? {
                    None => Ok(json!({"root":root,"path":path,"exists":false,"kind":null,"bytes":null,"sha256":null})),
                    Some((kind, size)) if kind == "file" => {
                        let mut f = open_at(&parent, leaf, false)?;
                        let data = read_bounded_bytes(&mut f, 1048576, Some(auth))?;
                        let hash = crate::util::sha256_hex(&data);
                        Ok(json!({"root":root,"path":path,"exists":true,"kind":"file","bytes":size,"sha256":hash}))
                    }
                    Some((kind, _)) => Ok(json!({"root":root,"path":path,"exists":true,"kind":kind,"bytes":null,"sha256":null})),
                }
            }
            "write_text" => {
                verify_expected_digest(args, path, auth.expected_digest)?;
                let text = args["text"]
                    .as_str()
                    .ok_or_else(|| Fail::Invalid("write_text needs a string `text`".into()))?;
                if text.len() > 1048576 {
                    return Err(Fail::Invalid("write_text exceeds 1 MiB".into()));
                }
                let (parent, leaf) = open_parent_and_leaf(root, path)?;
                let mut f = open_write_leaf(&parent, leaf, false)?;
                f.write_all(text.as_bytes())
                    .and_then(|_| f.sync_all())
                    .map_err(|e| Fail::Failed(format!("failed writing `{path}`: {e}")))?;
                let hash = crate::util::sha256_hex(text.as_bytes());
                Ok(json!({
                    "root": root,
                    "path": path,
                    "bytes": text.len(),
                    "sha256": hash,
                    "held": null,
                    "petition": null
                }))
            }
            "append_jsonl" => {
                verify_expected_digest(args, path, auth.expected_digest)?;
                let line = format!("{}\n", crate::util::canonical_json(&args["record"]));
                if line.len() > 1048576 {
                    return Err(Fail::Invalid("append_jsonl record exceeds 1 MiB".into()));
                }
                let rec_hash = crate::util::sha256_hex(line.trim_end_matches('\n').as_bytes());
                let (parent, leaf) = open_parent_and_leaf(root, path)?;
                let mut f = open_write_leaf(&parent, leaf, true)?;
                f.write_all(line.as_bytes())
                    .and_then(|_| f.sync_all())
                    .map_err(|e| Fail::Failed(format!("failed appending to `{path}`: {e}")))?;
                drop(f);
                let mut rf = open_at(&parent, leaf, false)?;
                let all_bytes = read_bounded_bytes(&mut rf, 1048576, Some(auth))?;
                let file_hash = crate::util::sha256_hex(&all_bytes);
                Ok(json!({
                    "root": root,
                    "path": path,
                    "bytes_appended": line.len(),
                    "record_sha256": rec_hash,
                    "sha256": file_hash,
                    "held": null,
                    "petition": null
                }))
            }
            "delete" => {
                verify_expected_digest(args, path, auth.expected_digest)?;
                let (parent, leaf) = open_parent_and_leaf(root, path)?;
                match stat_leaf(&parent, leaf)? {
                    None => {
                        return Err(Fail::Failed(format!(
                            "cannot delete `{path}`: no such file"
                        )))
                    }
                    Some((kind, _)) if kind != "file" => {
                        return Err(Fail::Denied(format!(
                            "cannot delete `{path}`: target is a {kind}"
                        )))
                    }
                    Some(_) => {}
                }
                let cname = CString::new(leaf).map_err(|_| Fail::Invalid("NUL in path".into()))?;
                if unsafe { libc::unlinkat(parent.as_raw_fd(), cname.as_ptr(), 0) } != 0 {
                    return Err(Fail::Failed(format!(
                        "failed deleting `{path}`: {}",
                        std::io::Error::last_os_error()
                    )));
                }
                Ok(json!({
                    "root": root,
                    "path": path,
                    "deleted": true,
                    "held": null,
                    "petition": null
                }))
            }
            _ => {
                let file = open(root, path, true)?;
                let max = args["max_entries"].as_u64().unwrap_or(1000) as usize;
                let mut entries = Vec::new();
                // /proc names the already-open directory, so renames cannot redirect traversal.
                for entry in std::fs::read_dir(format!("/proc/self/fd/{}", file.as_raw_fd()))
                    .map_err(|e| Fail::Failed(e.to_string()))?
                {
                    live(auth)?;
                    let e = entry.map_err(|e| Fail::Failed(e.to_string()))?;
                    if entries.len() == max {
                        return Err(Fail::Exhausted(
                            "directory exceeds max_entries; no partial listing returned".into(),
                        ));
                    }
                    let ty = e.file_type().map_err(|e| Fail::Failed(e.to_string()))?;
                    let name = e
                        .file_name()
                        .into_string()
                        .map_err(|_| Fail::Invalid("directory contains non-UTF-8 names".into()))?;
                    entries.push(json!({"name":name,"kind":if ty.is_symlink(){"symlink"}else if ty.is_dir(){"directory"}else if ty.is_file(){"file"}else{"other"}}));
                }
                entries.sort_by(|a, b| a["name"].as_str().cmp(&b["name"].as_str()));
                Ok(json!({"root":root,"path":path,"entries":entries}))
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{collections::HashSet, sync::Mutex};
    #[test]
    fn directory_authority_and_descriptor_walk_fail_closed() {
        let root = std::env::temp_dir().join(crate::util::new_id("summonr-files"));
        std::fs::create_dir_all(root.join("sub")).unwrap();
        std::fs::write(root.join("sub/a.txt"), "é🙂").unwrap();
        std::os::unix::fs::symlink("sub/a.txt", root.join("link")).unwrap();
        std::os::unix::fs::symlink("sub", root.join("dir-link")).unwrap();
        let path = root.to_str().unwrap();
        let selected = Mutex::new(HashSet::new());
        let grants = vec![
            reagent::need("file", "read_text", &[("root", path)], &[]),
            reagent::need("file", "list", &[("root", path)], &[]),
        ];
        let auth = Authority {
            grants: &grants,
            selected: &selected,
            deadline: None,
            on_item: None,
            expected_digest: None,
            config: None,
        };
        let run = |name: &str, args: Value| reagent::call(None, "file", name, &args, &auth);
        let result = run("read_text", json!({"root":path,"path":"sub/a.txt"})).unwrap();
        assert_eq!(result["text"], "é🙂");
        assert_eq!(result["bytes"], 6);
        assert_eq!(result["sha256"], crate::util::sha256_hex("é🙂".as_bytes()));
        for p in ["../a", "/etc/passwd", "link", "dir-link/a.txt", "sub"] {
            assert!(
                run("read_text", json!({"root":path,"path":p})).is_err(),
                "{p}"
            );
        }
        assert!(run("read_text", json!({"root":"/","path":"etc/passwd"})).is_err());
        assert!(matches!(
            run(
                "read_text",
                json!({"root":path,"path":"sub/a.txt","max_bytes":1})
            ),
            Err(Fail::Exhausted(_))
        ));
        assert!(matches!(
            run("list", json!({"root":path,"path":"","max_entries":1})),
            Err(Fail::Exhausted(_))
        ));
        let listing = run("list", json!({"root":path,"path":"sub"})).unwrap();
        assert_eq!(listing["entries"][0]["name"], "a.txt");
        std::fs::write(root.join("bad"), [255]).unwrap();
        assert!(run("read_text", json!({"root":path,"path":"bad"})).is_err());
        let broad = vec![reagent::need("file", "read_text", &[], &[])];
        let denied = Authority {
            grants: &broad,
            ..auth
        };
        assert!(matches!(
            reagent::call(
                None,
                "file",
                "read_text",
                &json!({"root":path,"path":"sub/a.txt"}),
                &denied
            ),
            Err(Fail::Denied(_))
        ));
        // Test stat, write_text, read_json, append_jsonl, and symlink defense on writes
        let grants_all = vec![
            reagent::need("file", "read_text", &[("root", path)], &[]),
            reagent::need("file", "read_json", &[("root", path)], &[]),
            reagent::need("file", "stat", &[("root", path)], &[]),
            reagent::need("file", "list", &[("root", path)], &[]),
            reagent::need("file", "write_text", &[("root", path)], &[]),
            reagent::need("file", "append_jsonl", &[("root", path)], &[]),
            reagent::need("file", "delete", &[("root", path)], &[]),
        ];
        let auth_all = Authority {
            grants: &grants_all,
            ..auth
        };
        let call_all = |name: &str, args: Value| reagent::call(None, "file", name, &args, &auth_all);
        let st_missing = call_all("stat", json!({"root":path,"path":"sub/new.json"})).unwrap();
        assert_eq!(st_missing["exists"], false);
        assert_eq!(
            target_digest(&json!({"root":path,"path":"sub/new.json"})).unwrap(),
            Some("missing".into())
        );
        let written = call_all(
            "write_text",
            json!({"root":path,"path":"sub/new.json","text":"{\"count\":42}"}),
        )
        .unwrap();
        assert_eq!(written["bytes"], 12);
        let parsed = call_all(
            "read_json",
            json!({"root":path,"path":"sub/new.json","schema":{"type":"object","required":["count"],"properties":{"count":{"type":"integer"}}}}),
        )
        .unwrap();
        assert_eq!(parsed["value"]["count"], 42);
        // Writing through a symlink or into a symlinked directory is refused
        assert!(call_all("write_text", json!({"root":path,"path":"link","text":"nope"})).is_err());
        assert!(call_all("write_text", json!({"root":path,"path":"dir-link/x.txt","text":"nope"})).is_err());
        assert!(call_all("stat", json!({"root":path,"path":"link"})).is_err());
        // Append JSONL lines deterministically
        call_all("append_jsonl", json!({"root":path,"path":"sub/log.jsonl","record":{"b":2,"a":1}})).unwrap();
        call_all("append_jsonl", json!({"root":path,"path":"sub/log.jsonl","record":{"b":4,"a":3}})).unwrap();
        let log_txt = call_all("read_text", json!({"root":path,"path":"sub/log.jsonl"})).unwrap();
        assert_eq!(log_txt["text"], "{\"a\":1,\"b\":2}\n{\"a\":3,\"b\":4}\n");
        // Delete regular file, refuse directory or missing target
        let del = call_all("delete", json!({"root":path,"path":"sub/new.json"})).unwrap();
        assert_eq!(del["deleted"], true);
        assert!(!root.join("sub/new.json").exists());
        assert!(call_all("delete", json!({"root":path,"path":"sub"})).is_err());
        assert!(call_all("delete", json!({"root":path,"path":"sub/new.json"})).is_err());
        std::fs::remove_dir_all(root).unwrap();
    }
}
