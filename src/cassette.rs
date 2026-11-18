//! Cassettes: a recorded run's model and tool traffic.

use crate::backend::ChatReq;
use crate::reagent::Fail;
use crate::util::canonical_sha256;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::io::Write;
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};

#[derive(Serialize, Deserialize, Debug, Clone)]
#[serde(deny_unknown_fields)]
pub struct Cassette {
    pub cassette_version: u32,
    pub entries: Vec<Entry>,
}

impl Default for Cassette {
    fn default() -> Self {
        Cassette {
            cassette_version: 1,
            entries: Vec::new(),
        }
    }
}

#[derive(Serialize, Deserialize, Debug, Clone)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum Entry {
    Model {
        step: String,
        model: String,
        request_hash: String,
        payload: Value,
        content: String,
        prompt_tokens: u64,
        output_tokens: u64,
    },
    Tool {
        operation: String,
        request_hash: String,
        arguments: Value,
        output: Option<Value>,
        error: Option<RecordedError>,
    },
}

/// A tool failure as recorded; `kind` maps 1:1 onto `reagent::Fail`.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct RecordedError {
    pub kind: ErrorKind,
    pub message: String,
}

/// The closed set of recorded failure kinds; any other kind fails the load.
#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ErrorKind {
    Exhausted,
    Cancelled,
    Invalid,
    Denied,
    Failed,
    Unverified,
}

/// Why a cassette could not be loaded: the file is missing or unreadable
/// (not found), or it does not parse as a cassette (invalid).
#[derive(Debug, Clone, PartialEq)]
pub enum LoadError {
    Unreadable(String),
    Invalid(String),
}

impl std::fmt::Display for LoadError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            LoadError::Unreadable(m) | LoadError::Invalid(m) => f.write_str(m),
        }
    }
}

impl RecordedError {
    pub fn from_fail(f: &Fail) -> Self {
        let (kind, message) = match f {
            Fail::Exhausted(m) => (ErrorKind::Exhausted, m.clone()),
            Fail::Cancelled => (ErrorKind::Cancelled, String::new()),
            Fail::Invalid(m) => (ErrorKind::Invalid, m.clone()),
            Fail::Denied(m) => (ErrorKind::Denied, m.clone()),
            Fail::Failed(m) => (ErrorKind::Failed, m.clone()),
            Fail::Unverified(m) => (ErrorKind::Unverified, m.clone()),
        };
        RecordedError { kind, message }
    }

    pub fn to_fail(&self) -> Fail {
        let m = self.message.clone();
        match self.kind {
            ErrorKind::Exhausted => Fail::Exhausted(m),
            ErrorKind::Cancelled => Fail::Cancelled,
            ErrorKind::Invalid => Fail::Invalid(m),
            ErrorKind::Denied => Fail::Denied(m),
            ErrorKind::Failed => Fail::Failed(m),
            ErrorKind::Unverified => Fail::Unverified(m),
        }
    }
}

/// Hash of what a model sees: timeout and token cap are excluded.
pub fn model_hash(model: &str, req: &ChatReq) -> String {
    canonical_sha256(&json!({
        "model": model,
        "system": req.system,
        "user": req.user,
        "schema": req.schema,
        "settings": req.settings,
    }))
}

pub fn tool_hash(operation: &str, args: &Value) -> String {
    canonical_sha256(&json!({"operation": operation, "arguments": args}))
}

pub fn load(path: &Path) -> Result<Cassette, LoadError> {
    let text = std::fs::read_to_string(path)
        .map_err(|e| LoadError::Unreadable(format!("{}: {e}", path.display())))?;
    let invalid = |e: String| LoadError::Invalid(format!("{}: {e}", path.display()));
    let yaml = matches!(
        path.extension().and_then(|e| e.to_str()),
        Some("yaml") | Some("yml")
    );
    let c: Cassette = if yaml {
        serde_yaml::from_str(&text).map_err(|e| invalid(e.to_string()))?
    } else {
        serde_json::from_str(&text).map_err(|e| invalid(e.to_string()))?
    };
    if c.cassette_version != 1 {
        return Err(invalid(format!(
            "unsupported cassette_version {}",
            c.cassette_version
        )));
    }
    Ok(c)
}

/// Write `path.tmp` (mode 0600 from creation), fsync, rename over `path`.
pub fn save(path: &Path, c: &Cassette) -> Result<(), String> {
    let mut tmp: PathBuf = path.as_os_str().to_owned().into();
    tmp.as_mut_os_string().push(".tmp");
    let body = serde_json::to_vec_pretty(c).map_err(|e| e.to_string())?;
    let write = || -> std::io::Result<()> {
        let mut f = std::fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .mode(0o600)
            .open(&tmp)?;
        f.write_all(&body)?;
        f.sync_all()?;
        std::fs::rename(&tmp, path)
    };
    write().map_err(|e| {
        let _ = std::fs::remove_file(&tmp);
        format!("{}: {e}", path.display())
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::backend::ChatReq;
    use serde_json::json;
    use std::collections::BTreeMap;
    use std::os::unix::fs::PermissionsExt;

    fn req<'a>(
        user: &'a str,
        settings: &'a BTreeMap<String, serde_json::Value>,
        timeout: Option<std::time::Duration>,
    ) -> ChatReq<'a> {
        ChatReq {
            model: "m",
            system: "s",
            user,
            schema: None,
            settings,
            timeout,
            max_output_tokens: None,
        }
    }

    #[test]
    fn hash_ignores_timeouts_but_not_prompts() {
        let st = BTreeMap::new();
        let a = model_hash("mock/x", &req("hi", &st, None));
        let b = model_hash(
            "mock/x",
            &req("hi", &st, Some(std::time::Duration::from_secs(3))),
        );
        let c = model_hash("mock/x", &req("ho", &st, None));
        assert_eq!(a, b);
        assert_ne!(a, c);
    }

    #[test]
    fn recorded_errors_round_trip_every_variant() {
        for f in [
            Fail::Exhausted("a".into()),
            Fail::Cancelled,
            Fail::Invalid("b".into()),
            Fail::Denied("c".into()),
            Fail::Failed("d".into()),
            Fail::Unverified("e".into()),
        ] {
            let r = RecordedError::from_fail(&f);
            let back = r.to_fail();
            assert_eq!(format!("{f:?}"), format!("{back:?}"));
            assert_eq!(r, RecordedError::from_fail(&back));
        }
        assert_eq!(RecordedError::from_fail(&Fail::Cancelled).message, "");
    }

    #[test]
    fn loads_yaml_cassettes() {
        let dir = std::env::temp_dir().join(format!("cas-{}", crate::util::new_id("t")));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("c.yaml");
        std::fs::write(
            &path,
            "cassette_version: 1\nentries:\n  - kind: tool\n    operation: data.count\n    request_hash: h\n    arguments: {value: [1]}\n    output: 1\n    error: {kind: denied, message: no}\n",
        )
        .unwrap();
        let c = load(&path).unwrap();
        assert_eq!(c.entries.len(), 1);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn unknown_error_kinds_and_missing_files_fail_the_load() {
        let dir = std::env::temp_dir().join(format!("cas-{}", crate::util::new_id("t")));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("c.yaml");
        std::fs::write(
            &path,
            "cassette_version: 1\nentries:\n  - kind: tool\n    operation: data.count\n    request_hash: h\n    arguments: {value: [1]}\n    output: null\n    error: {kind: exploded, message: no}\n",
        )
        .unwrap();
        assert!(matches!(load(&path), Err(LoadError::Invalid(ref m)) if m.contains("exploded")));
        assert!(matches!(
            load(&dir.join("missing.json")),
            Err(LoadError::Unreadable(_))
        ));
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn save_is_atomic_and_private() {
        let dir = std::env::temp_dir().join(format!("cas-{}", crate::util::new_id("t")));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("c.json");
        let c = Cassette {
            cassette_version: 1,
            entries: vec![Entry::Tool {
                operation: "data.count".into(),
                request_hash: tool_hash("data.count", &json!({"value":[1]})),
                arguments: json!({"value":[1]}),
                output: Some(json!(1)),
                error: None,
            }],
        };
        save(&path, &c).unwrap();
        let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600);
        assert!(!dir.join("c.json.tmp").exists());
        let back = load(&path).unwrap();
        assert_eq!(back.entries.len(), 1);
        assert_eq!(back.cassette_version, 1);
        std::fs::remove_dir_all(&dir).ok();
    }
}
