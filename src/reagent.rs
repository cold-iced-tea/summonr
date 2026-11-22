//! Reagents: tool capabilities whose authority the core enforces outside the
//! model. Built-ins provide pure data, scoped file/HTTP reads, and fixture mail.

use crate::outcome::Outcome;
use crate::records::Evidence;
use crate::ritual::Grant;
use serde_json::{json, Value};
use std::collections::{BTreeMap, HashSet};
use std::path::{Path, PathBuf};

#[derive(Debug)]
pub enum Fail {
    Exhausted(String),
    Cancelled,
    Invalid(String),
    Denied(String),
    Failed(String),
    /// An effect was attempted but the runtime did not observe the expected
    /// state afterward.
    Unverified(String),
}

impl Fail {
    pub fn outcome(&self) -> Outcome {
        match self {
            Fail::Exhausted(_) => Outcome::Exhausted,
            Fail::Cancelled => Outcome::Cancelled,
            Fail::Invalid(_) => Outcome::Invalid,
            Fail::Denied(_) | Fail::Failed(_) => Outcome::Failed,
            Fail::Unverified(_) => Outcome::Unverified,
        }
    }
    pub fn message(&self) -> String {
        match self {
            Fail::Exhausted(what) => format!("essence exhausted: {what}"),
            Fail::Cancelled => "cancelled by a caller".into(),
            Fail::Invalid(m) => format!("invalid output: {m}"),
            Fail::Denied(m) => format!("not authorized: {m}"),
            Fail::Failed(m) => m.clone(),
            Fail::Unverified(m) => format!("not verified: {m}"),
        }
    }
}

/// Metadata used by validation, contracts and execution.
#[derive(Clone, Copy, Debug)]
pub struct ToolSpec {
    pub reagent: &'static str,
    pub name: &'static str,
    pub effect: &'static str,
    pub determinism: &'static str,
    pub description: &'static str,
}
const DATA_TOOLS: &[ToolSpec] = &[
    ToolSpec {
        reagent: "data",
        name: "arithmetic",
        effect: "pure",
        determinism: "deterministic",
        description: "Checked integer arithmetic",
    },
    ToolSpec {
        reagent: "data",
        name: "compare",
        effect: "pure",
        determinism: "deterministic",
        description: "Explicit typed comparisons",
    },
    ToolSpec {
        reagent: "data",
        name: "get",
        effect: "pure",
        determinism: "deterministic",
        description: "Read a JSON pointer with an explicit missing-value policy",
    },
    ToolSpec {
        reagent: "data",
        name: "keys",
        effect: "pure",
        determinism: "deterministic",
        description: "Sorted object keys",
    },
    ToolSpec {
        reagent: "data",
        name: "concat",
        effect: "pure",
        determinism: "deterministic",
        description: "Concatenate arrays",
    },
    ToolSpec {
        reagent: "data",
        name: "slice",
        effect: "pure",
        determinism: "deterministic",
        description: "Bounded array slice",
    },
    ToolSpec {
        reagent: "data",
        name: "sort",
        effect: "pure",
        determinism: "deterministic",
        description: "Stable typed ordering",
    },
    ToolSpec {
        reagent: "data",
        name: "unique",
        effect: "pure",
        determinism: "deterministic",
        description: "Distinct JSON values",
    },
    ToolSpec {
        reagent: "data",
        name: "trim",
        effect: "pure",
        determinism: "deterministic",
        description: "Trim Unicode whitespace",
    },
    ToolSpec {
        reagent: "data",
        name: "split",
        effect: "pure",
        determinism: "deterministic",
        description: "Literal text splitting",
    },
    ToolSpec {
        reagent: "data",
        name: "join",
        effect: "pure",
        determinism: "deterministic",
        description: "Join strings",
    },
    ToolSpec {
        reagent: "data",
        name: "replace",
        effect: "pure",
        determinism: "deterministic",
        description: "Literal text replacement",
    },
    ToolSpec {
        reagent: "data",
        name: "map",
        effect: "pure",
        determinism: "deterministic",
        description: "Bounded pure transformation",
    },
    ToolSpec {
        reagent: "data",
        name: "filter",
        effect: "pure",
        determinism: "deterministic",
        description: "Bounded predicate selection",
    },
    ToolSpec {
        reagent: "data",
        name: "fold",
        effect: "pure",
        determinism: "deterministic",
        description: "Bounded pure reduction",
    },
    ToolSpec {
        reagent: "data",
        name: "parse_json",
        effect: "pure",
        determinism: "deterministic",
        description: "Parse exact JSON and optionally validate it; no repair",
    },
    ToolSpec {
        reagent: "data",
        name: "project",
        effect: "pure",
        determinism: "deterministic",
        description: "Select named object fields; missing fields fail",
    },
    ToolSpec {
        reagent: "data",
        name: "count",
        effect: "pure",
        determinism: "deterministic",
        description: "Count array elements, object keys, or Unicode scalar values",
    },
    ToolSpec {
        reagent: "data",
        name: "sum_integers",
        effect: "pure",
        determinism: "deterministic",
        description: "Sum signed integers with checked overflow",
    },
    ToolSpec {
        reagent: "data",
        name: "assert",
        effect: "pure",
        determinism: "deterministic",
        description: "Require an explicit boolean condition",
    },
    ToolSpec {
        reagent: "data",
        name: "all",
        effect: "pure",
        determinism: "deterministic",
        description: "True only when every boolean in the array is true",
    },
    ToolSpec {
        reagent: "data",
        name: "any",
        effect: "pure",
        determinism: "deterministic",
        description: "True when at least one boolean in the array is true",
    },
    ToolSpec {
        reagent: "data",
        name: "logic",
        effect: "pure",
        determinism: "deterministic",
        description: "Explicit boolean logic (and, or, xor, not)",
    },
    ToolSpec {
        reagent: "data",
        name: "select",
        effect: "pure",
        determinism: "deterministic",
        description: "Conditional value selection without branching steps",
    },
    ToolSpec {
        reagent: "data",
        name: "min",
        effect: "pure",
        determinism: "deterministic",
        description: "Minimum of a non-empty homogeneous array",
    },
    ToolSpec {
        reagent: "data",
        name: "max",
        effect: "pure",
        determinism: "deterministic",
        description: "Maximum of a non-empty homogeneous array",
    },
    ToolSpec {
        reagent: "data",
        name: "clamp",
        effect: "pure",
        determinism: "deterministic",
        description: "Clamp a signed integer between min and max",
    },
    ToolSpec {
        reagent: "data",
        name: "abs",
        effect: "pure",
        determinism: "deterministic",
        description: "Checked integer absolute value",
    },
    ToolSpec {
        reagent: "data",
        name: "merge",
        effect: "pure",
        determinism: "deterministic",
        description: "Merge two objects with an explicit conflict policy",
    },
    ToolSpec {
        reagent: "data",
        name: "set",
        effect: "pure",
        determinism: "deterministic",
        description: "Return an object with one named field added or replaced",
    },
    ToolSpec {
        reagent: "data",
        name: "omit",
        effect: "pure",
        determinism: "deterministic",
        description: "Return an object with named fields removed",
    },
    ToolSpec {
        reagent: "data",
        name: "entries",
        effect: "pure",
        determinism: "deterministic",
        description: "Sorted key/value entries of an object",
    },
    ToolSpec {
        reagent: "data",
        name: "from_entries",
        effect: "pure",
        determinism: "deterministic",
        description: "Build an object from key/value entries; duplicate keys fail",
    },
    ToolSpec {
        reagent: "data",
        name: "sha256",
        effect: "pure",
        determinism: "deterministic",
        description: "SHA-256 hex digest of UTF-8 text",
    },
    ToolSpec {
        reagent: "data",
        name: "canonical_json",
        effect: "pure",
        determinism: "deterministic",
        description: "Canonical sorted-key JSON string of a value",
    },
    ToolSpec {
        reagent: "data",
        name: "contains",
        effect: "pure",
        determinism: "deterministic",
        description: "Substring or array-element containment check",
    },
    ToolSpec {
        reagent: "data",
        name: "starts_with",
        effect: "pure",
        determinism: "deterministic",
        description: "Literal string prefix check",
    },
    ToolSpec {
        reagent: "data",
        name: "ends_with",
        effect: "pure",
        determinism: "deterministic",
        description: "Literal string suffix check",
    },
    ToolSpec {
        reagent: "data",
        name: "case",
        effect: "pure",
        determinism: "deterministic",
        description: "Unicode lowercase or uppercase conversion",
    },
    ToolSpec {
        reagent: "data",
        name: "lines",
        effect: "pure",
        determinism: "deterministic",
        description: "Split text into bounded lines, optionally numbered",
    },
    ToolSpec {
        reagent: "data",
        name: "coalesce",
        effect: "pure",
        determinism: "deterministic",
        description: "First non-null element, or null",
    },
    ToolSpec {
        reagent: "data",
        name: "unique_by",
        effect: "pure",
        determinism: "deterministic",
        description: "First object for each combination of the named fields",
    },
    ToolSpec {
        reagent: "data",
        name: "flatten",
        effect: "pure",
        determinism: "deterministic",
        description: "Concatenate an array of arrays, one level",
    },
    ToolSpec {
        reagent: "data",
        name: "range",
        effect: "pure",
        determinism: "deterministic",
        description: "Bounded integer sequence from start up to end by step",
    },
    ToolSpec {
        reagent: "data",
        name: "index_of",
        effect: "pure",
        determinism: "deterministic",
        description: "Index of the first equal (or containing) element, or null",
    },
];
const MAIL_TOOLS: &[ToolSpec] = &[
    ToolSpec {
        reagent: "mail",
        name: "search",
        effect: "read",
        determinism: "deterministic over live state",
        description: "List the newest messages in a folder",
    },
    ToolSpec {
        reagent: "mail",
        name: "read",
        effect: "read",
        determinism: "deterministic over live state",
        description: "Read one selected message",
    },
    ToolSpec {
        reagent: "mail",
        name: "move",
        effect: "idempotent write",
        determinism: "deterministic",
        description: "Move one selected message to a folder",
    },
    ToolSpec {
        reagent: "mail",
        name: "locate",
        effect: "read",
        determinism: "deterministic over live state",
        description: "Report which folder a selected message is in now",
    },
];

pub(crate) trait Reagent: Sync {
    fn name(&self) -> &'static str;
    fn tools(&self) -> &'static [ToolSpec];
    fn schema(&self, name: &str) -> Option<Value>;
    /// Every check that decides whether `name` is permitted with `args`:
    /// grants, selection, plain names, roots and origins. It performs no
    /// effect and no I/O beyond what a check itself needs. `call` begins
    /// with it, so a refusal here is exactly what a live call would return.
    fn authorize(
        &self,
        root: Option<&str>,
        name: &str,
        args: &Value,
        auth: &Authority,
    ) -> Result<(), Fail>;
    /// Take on the authority side effects of an output that was not produced
    /// by `call` (a replayed or simulated one). Most tools have none.
    fn adopt(&self, _name: &str, _output: &Value, _auth: &Authority) {}
    fn call(
        &self,
        root: Option<&str>,
        name: &str,
        args: &Value,
        auth: &Authority,
    ) -> Result<Value, Fail>;
}
struct Data;
struct Mail;
impl Reagent for Data {
    fn name(&self) -> &'static str {
        "data"
    }
    fn tools(&self) -> &'static [ToolSpec] {
        DATA_TOOLS
    }
    fn schema(&self, name: &str) -> Option<Value> {
        crate::pure::schema(name)
    }
    fn authorize(&self, _: Option<&str>, _: &str, _: &Value, _: &Authority) -> Result<(), Fail> {
        Ok(())
    }
    fn call(
        &self,
        root: Option<&str>,
        name: &str,
        args: &Value,
        auth: &Authority,
    ) -> Result<Value, Fail> {
        self.authorize(root, name, args, auth)?;
        if matches!(name, "map" | "filter" | "fold") {
            crate::stdlib::bounded(args.clone()).map_err(Fail::Invalid)?;
            crate::stdlib::collection(name, args, auth.on_item)
        } else {
            crate::pure::call(name, args).map_err(Fail::Invalid)
        }
    }
}
impl Reagent for Mail {
    fn name(&self) -> &'static str {
        "mail"
    }
    fn tools(&self) -> &'static [ToolSpec] {
        MAIL_TOOLS
    }
    fn schema(&self, name: &str) -> Option<Value> {
        mail_schema(&format!("mail.{name}"))
    }
    fn authorize(
        &self,
        root: Option<&str>,
        name: &str,
        args: &Value,
        auth: &Authority,
    ) -> Result<(), Fail> {
        mail_authorize(root, name, args, auth).map(|_| ())
    }
    fn adopt(&self, name: &str, output: &Value, auth: &Authority) {
        if name == "search" {
            select(
                auth,
                output
                    .as_array()
                    .into_iter()
                    .flatten()
                    .filter_map(|m| m.get("id").and_then(Value::as_str)),
            );
        }
    }
    fn call(
        &self,
        root: Option<&str>,
        name: &str,
        args: &Value,
        auth: &Authority,
    ) -> Result<Value, Fail> {
        mail_call(root, name, args, auth)
    }
}

/// Add message ids to this summon's selection: the only way a message
/// becomes readable or movable.
fn select<'a>(auth: &Authority, ids: impl Iterator<Item = &'a str>) {
    let mut selected = auth.selected.lock().unwrap();
    for id in ids {
        selected.insert(id.to_owned());
    }
}
static REAGENTS: &[&dyn Reagent] = &[
    &Data,
    &Mail,
    &crate::file_reagent::Files,
    &crate::git_reagent::Git,
    &crate::gh_reagent::Gh,
    &crate::trial_reagent::Trial,
    &crate::cmd_reagent::Cmd,
    &crate::http_reagent::Http,
    &crate::sight_reagent::Sight,
];
fn reagent(name: &str) -> Option<&'static dyn Reagent> {
    REAGENTS.iter().copied().find(|r| r.name() == name)
}
pub fn all_tools() -> impl Iterator<Item = &'static ToolSpec> {
    REAGENTS.iter().flat_map(|r| r.tools())
}
#[derive(Clone, Copy, Debug)]
pub struct ToolMeta {
    pub effect: &'static str,
    pub determinism: &'static str,
}

impl From<&'static ToolSpec> for ToolMeta {
    fn from(s: &'static ToolSpec) -> Self {
        Self {
            effect: s.effect,
            determinism: s.determinism,
        }
    }
}

pub fn tool(reagent_name: &str, name: &str) -> Option<&'static ToolSpec> {
    reagent(reagent_name)?
        .tools()
        .iter()
        .find(|t| t.name == name)
}

pub fn tool_meta(
    cfg: Option<&crate::util::Config>,
    reagent_name: &str,
    name: &str,
) -> Option<ToolMeta> {
    if let Some(t) = tool(reagent_name, name) {
        return Some(ToolMeta::from(t));
    }
    if let Some(mcp) = cfg?.mcp_reagents.get(reagent_name) {
        let t = mcp.tools.get(name)?;
        let effect = match t.effect.as_str() {
            "read" => "read",
            _ => "write",
        };
        return Some(ToolMeta {
            effect,
            determinism: "external",
        });
    }
    let wasm = cfg?.wasm_reagents.get(reagent_name)?;
    let t = wasm.tools.get(name)?;
    let (effect, determinism) = match t.effect.as_str() {
        "pure" => ("pure", "deterministic"),
        "read" => ("read", "deterministic over live state"),
        _ => ("write", "external"),
    };
    Some(ToolMeta {
        effect,
        determinism,
    })
}
pub fn tools_of(name: &str) -> Vec<&'static str> {
    reagent(name)
        .map(|r| r.tools().iter().map(|t| t.name).collect())
        .unwrap_or_default()
}
pub fn tools_of_cfg(cfg: Option<&crate::util::Config>, name: &str) -> Vec<String> {
    let builtin = tools_of(name);
    if !builtin.is_empty() {
        return builtin.into_iter().map(String::from).collect();
    }
    if let Some(tools) = cfg
        .and_then(|c| c.mcp_reagents.get(name))
        .map(|m| m.tools.keys().cloned().collect::<Vec<_>>())
    {
        return tools;
    }
    cfg.and_then(|c| c.wasm_reagents.get(name))
        .map(|w| w.tools.keys().cloned().collect())
        .unwrap_or_default()
}
pub fn input_schema(operation: &str) -> Option<Value> {
    let (r, t) = operation.split_once('.')?;
    reagent(r)?.schema(t)
}
fn mail_schema(operation: &str) -> Option<Value> {
    let text = json!({"type":"string","minLength":1});
    let (properties, required) = match operation {
        "mail.search" => (
            json!({"folder":text,"newest":{"type":"integer","minimum":0,"maximum":10000}}),
            vec!["folder"],
        ),
        "mail.read" => (
            json!({"message":text,"max_chars":{"type":"integer","minimum":0,"maximum":1048576}}),
            vec!["message"],
        ),
        "mail.move" => (
            json!({"message":text,"folder":text}),
            vec!["message", "folder"],
        ),
        "mail.locate" => (json!({"message":text}), vec!["message"]),
        _ => return None,
    };
    Some(
        json!({"type":"object","properties":properties,"required":required,"additionalProperties":false}),
    )
}

fn contains_template(v: &Value) -> bool {
    match v {
        Value::String(s) => s.contains("${"),
        Value::Array(a) => a.iter().any(contains_template),
        Value::Object(m) => m.values().any(contains_template),
        _ => false,
    }
}

/// Validate literal arguments at compilation; templates are checked after rendering.
pub fn validate_arguments(
    operation: &str,
    arguments: &Value,
    rendered: bool,
) -> Result<(), String> {
    if matches!(operation, "data.map" | "data.filter" | "data.fold") {
        crate::stdlib::validate_collection(operation, arguments)?;
    }
    if matches!(
        operation,
        "data.parse_json" | "file.read_json" | "http.get_json"
    ) {
        if let Some(s) = arguments.get("schema") {
            crate::schema::validate(s)?;
        }
    }
    let schema = input_schema(operation).ok_or_else(|| format!("unknown operation {operation}"))?;
    if rendered {
        return crate::schema::check(&schema, arguments, "arguments");
    }
    let args = arguments.as_object().ok_or("arguments must be an object")?;
    for name in schema["required"].as_array().unwrap() {
        if !args.contains_key(name.as_str().unwrap()) {
            return Err(format!("missing required argument {name}"));
        }
    }
    for (name, v) in args {
        let sub = schema["properties"]
            .get(name)
            .ok_or_else(|| format!("unknown argument {name}"))?;
        if contains_template(v) {
            continue;
        }
        crate::schema::check(sub, v, name)?;
    }
    Ok(())
}

pub struct Authority<'a> {
    pub grants: &'a [Grant],
    pub deadline: Option<std::time::Instant>,
    pub on_item: Option<&'a dyn Fn() -> Result<(), Fail>>,
    /// Message IDs this summon's own searches returned. Reads and moves
    /// outside this set are refused whatever the model asked for.
    pub selected: &'a std::sync::Mutex<HashSet<String>>,
    /// For an approved petition: the `target_digest` recorded when it was
    /// held. The write re-checks it against the bytes it is about to act on.
    pub expected_digest: Option<&'a str>,
    /// The owner's configuration, for reagents that act on configured
    /// resources (the git, gh and trial reagents' repositories).
    pub config: Option<&'a crate::util::Config>,
}

pub(crate) fn need(
    reagent: &str,
    action: &str,
    on: &[(&str, &str)],
    into: &[(&str, &str)],
) -> Grant {
    let m = |kv: &[(&str, &str)]| -> BTreeMap<String, String> {
        kv.iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect()
    };
    Grant {
        reagent: reagent.into(),
        action: action.into(),
        on: m(on),
        into: m(into),
    }
}

/// Refuse unless some grant covers `n`.
pub(crate) fn require(auth: &Authority, n: Grant) -> Result<(), Fail> {
    if auth.grants.iter().any(|g| g.covers(&n)) {
        Ok(())
    } else {
        Err(Fail::Denied(format!("no grant covers {}", n.describe())))
    }
}

fn safe_name(s: &str, what: &str) -> Result<(), Fail> {
    if s.is_empty() || s.contains('/') || s.contains('\\') || s.starts_with('.') {
        return Err(Fail::Denied(format!("{what} `{s}` is not a plain name")));
    }
    Ok(())
}

fn str_arg<'a>(args: &'a Value, key: &str, op: &str) -> Result<&'a str, Fail> {
    args.get(key)
        .and_then(Value::as_str)
        .ok_or_else(|| Fail::Failed(format!("{op} needs a string argument `{key}`")))
}

fn headers(text: &str) -> (String, String, String) {
    let mut from = String::new();
    let mut subject = String::new();
    let mut date = String::new();
    for line in text.lines() {
        if line.trim().is_empty() {
            break;
        }
        if let Some((k, v)) = line.split_once(':') {
            match k.to_ascii_lowercase().as_str() {
                "from" => from = v.trim().to_string(),
                "subject" => subject = v.trim().to_string(),
                "date" => date = v.trim().to_string(),
                _ => {}
            }
        }
    }
    (from, subject, date)
}

fn message_files(dir: &Path) -> Vec<(String, PathBuf)> {
    let mut out: Vec<(String, PathBuf)> = std::fs::read_dir(dir)
        .into_iter()
        .flatten()
        .flatten()
        .map(|e| e.path())
        .filter(|p| {
            p.is_file()
                && matches!(
                    p.extension().and_then(|e| e.to_str()),
                    Some("eml") | Some("txt")
                )
        })
        .filter_map(|p| Some((p.file_stem()?.to_str()?.to_string(), p)))
        .collect();
    // Newest first; fixture names sort chronologically.
    out.sort_by(|a, b| b.0.cmp(&a.0));
    out
}

/// A digest of the resource a write would act on, recorded when the write is
/// held so that approving it later applies only to what was reviewed. For a
/// mail move: SHA-256 of the message's folder, a newline, and its bytes.
/// Operations without a digest return `Ok(None)` and file unbound petitions.
#[cfg_attr(not(test), allow(dead_code))]
pub fn target_digest(
    mail_root: Option<&str>,
    operation: &str,
    args: &Value,
) -> Result<Option<String>, Fail> {
    target_digest_with_config(mail_root, None, operation, args, None)
}

pub fn target_digest_with_config(
    mail_root: Option<&str>,
    cfg: Option<&crate::util::Config>,
    operation: &str,
    args: &Value,
    deadline: Option<std::time::Instant>,
) -> Result<Option<String>, Fail> {
    match operation {
        "mail.move" => {
            let root = mail_root.ok_or_else(|| Fail::Failed("no mail_root is configured".into()))?;
            let id = str_arg(args, "message", "mail.move")?;
            let (folder, path) = locate(Path::new(root), id)
                .ok_or_else(|| Fail::Failed(format!("message `{id}` no longer exists")))?;
            message_digest(id, &folder, &path).map(Some)
        }
        "cmd.write" => crate::cmd_reagent::target_digest(cfg, args, deadline),
        "file.write_text" | "file.append_jsonl" | "file.delete" => {
            crate::file_reagent::target_digest(args)
        }
        "http.post" | "http.put" | "http.patch" | "http.delete" => {
            crate::http_reagent::target_digest(cfg, args)
        }
        _ => Ok(None),
    }
}

fn message_digest(id: &str, folder: &str, path: &Path) -> Result<String, Fail> {
    let bytes =
        std::fs::read(path).map_err(|e| Fail::Failed(format!("cannot read `{id}`: {e}")))?;
    let mut data = folder.as_bytes().to_vec();
    data.push(b'\n');
    data.extend(bytes);
    Ok(crate::util::sha256_hex(&data))
}

fn locate(root: &Path, id: &str) -> Option<(String, PathBuf)> {
    for entry in std::fs::read_dir(root).ok()?.flatten() {
        let dir = entry.path();
        if !dir.is_dir() {
            continue;
        }
        for ext in ["eml", "txt"] {
            let p = dir.join(format!("{id}.{ext}"));
            if p.is_file() {
                return Some((entry.file_name().to_string_lossy().into_owned(), p));
            }
        }
    }
    None
}

/// A verification with its references already resolved, so it can travel
/// with a held operation and run when that operation is approved.
#[derive(serde::Serialize, serde::Deserialize, Clone, Debug)]
pub struct Check {
    pub observe: String,
    pub arguments: Value,
    pub expect: Value,
}

/// Perform an operation and, when a check is attached, read the state back.
/// Returns the tool's output and the evidence the runtime observed.
pub fn perform(
    mail_root: Option<&str>,
    operation: &str,
    args: &Value,
    check: Option<&Check>,
    auth: &Authority,
    io: &dyn crate::io::Io,
) -> Result<(Value, Evidence), Fail> {
    let (reagent, _) = operation
        .split_once('.')
        .ok_or_else(|| Fail::Failed(format!("bad operation `{operation}`")))?;
    let out = io.call(mail_root, operation, args, auth)?;
    // The Io says whether it simulated this call; its output is never inspected.
    if io.simulates(operation, auth.config) {
        return Ok((out.clone(), Evidence::Simulated { would: out }));
    }
    let Some(c) = check else {
        return Ok((
            out,
            if reagent == "data" {
                Evidence::Computed {
                    observed: Value::Null,
                }
            } else {
                Evidence::Reported {
                    observed: Value::Null,
                }
            },
        ));
    };
    if !c.observe.contains('.') {
        return Err(Fail::Failed(format!("bad verify tool `{}`", c.observe)));
    }
    let seen = io
        .call(mail_root, &c.observe, &c.arguments, auth)
        .map_err(|e| {
            Fail::Unverified(format!(
                "{operation} completed, but verification failed: {}",
                e.message()
            ))
        })?;
    let mismatched: Vec<String> = c
        .expect
        .as_object()
        .into_iter()
        .flatten()
        .filter(|(k, want)| seen.get(k.as_str()) != Some(*want))
        .map(|(k, want)| {
            format!(
                "{k} is {} but {want} was expected",
                seen.get(k.as_str()).unwrap_or(&Value::Null)
            )
        })
        .collect();
    if mismatched.is_empty() {
        Ok((
            out,
            Evidence::Observed {
                by: c.observe.clone(),
                observed: seen,
                expected: c.expect.clone(),
            },
        ))
    } else {
        Err(Fail::Unverified(format!(
            "{operation}: {} (observed by {})",
            mismatched.join("; "),
            c.observe
        )))
    }
}

fn authorize_mcp(
    reagent: &str,
    tool: &str,
    auth: &Authority,
) -> Result<&'static str, Fail> {
    let cfg = auth
        .config
        .and_then(|c| c.mcp_reagents.get(reagent))
        .ok_or_else(|| Fail::Failed(format!("unknown reagent `{reagent}`")))?;
    let t = cfg
        .tools
        .get(tool)
        .ok_or_else(|| Fail::Failed(format!("reagent `{reagent}` has no tool `{tool}`")))?;
    let mut matching = auth
        .grants
        .iter()
        .filter(|g| g.reagent == reagent && g.action == tool)
        .peekable();
    if matching.peek().is_none() {
        return Err(Fail::Denied(format!(
            "{reagent}.{tool} is not authorized by any grant"
        )));
    }
    // A scope on an MCP tool is never checked against its arguments, so a
    // scoped grant (stored before scopes were refused, or narrowed from a
    // ritual's `requires`) authorizes nothing rather than everything.
    if !matching.any(|g| g.on.is_empty() && g.into.is_empty()) {
        return Err(Fail::Denied(format!(
            "{reagent}.{tool} is only granted with a scope, and MCP grants cannot be scoped; \
             grant it without on/into"
        )));
    }
    Ok(if t.effect == "read" { "read" } else { "write" })
}

pub fn call(
    mail_root: Option<&str>,
    reagent: &str,
    tool: &str,
    args: &Value,
    auth: &Authority,
) -> Result<Value, Fail> {
    if let Some(implementation) = self::reagent(reagent) {
        validate_arguments(&format!("{reagent}.{tool}"), args, true).map_err(Fail::Invalid)?;
        return implementation.call(mail_root, tool, args, auth);
    }
    if auth
        .config
        .is_some_and(|c| c.wasm_reagents.contains_key(reagent))
    {
        authorize_wasm(reagent, tool, auth)?;
        let wasm_cfg = auth
            .config
            .and_then(|c| c.wasm_reagents.get(reagent))
            .expect("authorized above");
        let timeout = auth
            .deadline
            .map(|d| d.saturating_duration_since(std::time::Instant::now()));
        return crate::wasm_reagent::call_tool(wasm_cfg, tool, args, timeout).map_err(Fail::Failed);
    }
    authorize_mcp(reagent, tool, auth)?;
    let mcp_cfg = auth
        .config
        .and_then(|c| c.mcp_reagents.get(reagent))
        .expect("authorized above");
    let timeout = auth
        .deadline
        .map(|d| d.saturating_duration_since(std::time::Instant::now()));
    crate::mcp_reagent::call_tool(mcp_cfg, tool, args, timeout, auth.config).map_err(Fail::Failed)
}

/// Run every check a live `call` of `operation` would make before its
/// effect, without performing the effect: the same argument validation, then
/// the reagent's own authorization. A refusal carries the same `Fail` variant
/// the live call would return.
pub fn authorize(
    mail_root: Option<&str>,
    operation: &str,
    args: &Value,
    auth: &Authority,
) -> Result<(), Fail> {
    let (reagent, tool) = operation
        .split_once('.')
        .ok_or_else(|| Fail::Failed(format!("bad operation `{operation}`")))?;
    if let Some(implementation) = self::reagent(reagent) {
        validate_arguments(operation, args, true).map_err(Fail::Invalid)?;
        return implementation.authorize(mail_root, tool, args, auth);
    }
    if auth
        .config
        .is_some_and(|c| c.wasm_reagents.contains_key(reagent))
    {
        return authorize_wasm(reagent, tool, auth).map(|_| ());
    }
    authorize_mcp(reagent, tool, auth).map(|_| ())
}

fn authorize_wasm(
    reagent: &str,
    tool: &str,
    auth: &Authority,
) -> Result<&'static str, Fail> {
    let cfg = auth
        .config
        .and_then(|c| c.wasm_reagents.get(reagent))
        .ok_or_else(|| Fail::Failed(format!("unknown wasm reagent `{reagent}`")))?;
    let t = cfg
        .tools
        .get(tool)
        .ok_or_else(|| Fail::Failed(format!("wasm reagent `{reagent}` has no tool `{tool}`")))?;
    if t.effect == "pure" {
        return Ok("pure");
    }
    let mut matching = auth
        .grants
        .iter()
        .filter(|g| g.reagent == reagent && g.action == tool)
        .peekable();
    if matching.peek().is_none() {
        return Err(Fail::Denied(format!(
            "{reagent}.{tool} is not authorized by any grant"
        )));
    }
    if !matching.any(|g| g.on.is_empty() && g.into.is_empty()) {
        return Err(Fail::Denied(format!(
            "{reagent}.{tool} is only granted with a scope, and wasm grants cannot be scoped; grant it without on/into"
        )));
    }
    Ok(if t.effect == "read" { "read" } else { "write" })
}

/// Apply the authority side effects of an output that did not come from a
/// live call, as the live call would have (a `mail.search` selects its ids).
pub fn adopt(operation: &str, output: &Value, auth: &Authority) {
    if let Some((reagent, tool)) = operation.split_once('.') {
        if let Some(implementation) = self::reagent(reagent) {
            implementation.adopt(tool, output, auth);
        }
    }
}

fn mail_root(mail_root: Option<&str>) -> Result<PathBuf, Fail> {
    let root = PathBuf::from(mail_root.ok_or_else(|| {
        Fail::Failed("the mail reagent has no mailbox; set `mail_root` in config.yaml".into())
    })?);
    let root_meta = std::fs::symlink_metadata(&root)
        .map_err(|e| Fail::Failed(format!("mailbox unavailable: {e}")))?;
    if root_meta.file_type().is_symlink() {
        return Err(Fail::Denied("mailbox root may not be a symlink".into()));
    }
    Ok(root)
}

fn require_selected(auth: &Authority, id: &str) -> Result<(), Fail> {
    if auth.selected.lock().unwrap().contains(id) {
        Ok(())
    } else {
        Err(Fail::Denied(format!(
            "message `{id}` is outside this summon's selection"
        )))
    }
}

/// Find the folder holding message `id`, for the grant check. Refuses the
/// same symlinks and duplicate identifiers the mailbox scan in `mail_call`
/// refuses, so an authorization alone never resolves an ambiguous message.
fn find(root: &Path, id: &str) -> Result<Option<(String, PathBuf)>, Fail> {
    let mut found = None;
    let mut seen = 0;
    for folder in std::fs::read_dir(root).map_err(|e| Fail::Failed(e.to_string()))? {
        let folder = folder.map_err(|e| Fail::Failed(e.to_string()))?;
        let ty = folder
            .file_type()
            .map_err(|e| Fail::Failed(e.to_string()))?;
        if ty.is_symlink() {
            return Err(Fail::Denied("symlink in fixture mailbox".into()));
        }
        if !ty.is_dir() {
            continue;
        }
        for ext in ["eml", "txt"] {
            let p = folder.path().join(format!("{id}.{ext}"));
            let Ok(meta) = std::fs::symlink_metadata(&p) else {
                continue;
            };
            if meta.file_type().is_symlink() {
                return Err(Fail::Denied("symlink in fixture mailbox".into()));
            }
            seen += 1;
            if meta.is_file() && found.is_none() {
                found = Some((folder.file_name().to_string_lossy().into_owned(), p));
            }
        }
    }
    if seen > 1 {
        return Err(Fail::Denied(
            "ambiguous duplicate message identifier in fixture mailbox".into(),
        ));
    }
    Ok(found)
}

type Located = Option<(String, PathBuf)>;

/// Every authority check for a mail tool. Returns the mailbox root and, for
/// `read` and `move`, the message's current location: the location the grant
/// was checked against and the one the effect must use.
fn mail_authorize(
    mail_root: Option<&str>,
    tool: &str,
    args: &Value,
    auth: &Authority,
) -> Result<(PathBuf, Located), Fail> {
    let root = self::mail_root(mail_root)?;
    let located = match tool {
        "search" => {
            let folder = str_arg(args, "folder", "mail.search")?;
            safe_name(folder, "folder")?;
            require(auth, need("mail", "search", &[("folder", folder)], &[]))?;
            None
        }
        "read" => {
            let id = str_arg(args, "message", "mail.read")?;
            safe_name(id, "message id")?;
            require_selected(auth, id)?;
            let (folder, path) = find(&root, id)?
                .ok_or_else(|| Fail::Failed(format!("message `{id}` no longer exists")))?;
            require(auth, need("mail", "read", &[("folder", &folder)], &[]))?;
            Some((folder, path))
        }
        "locate" => {
            let id = str_arg(args, "message", "mail.locate")?;
            safe_name(id, "message id")?;
            require_selected(auth, id)?;
            require(auth, need("mail", "locate", &[], &[]))?;
            None
        }
        "move" => {
            let id = str_arg(args, "message", "mail.move")?;
            let dest = str_arg(args, "folder", "mail.move")?;
            safe_name(id, "message id")?;
            safe_name(dest, "folder")?;
            require_selected(auth, id)?;
            let (src, path) = find(&root, id)?
                .ok_or_else(|| Fail::Failed(format!("message `{id}` no longer exists")))?;
            // Idempotent: a move that is already complete needs no grant,
            // because it changes nothing.
            if src != dest {
                require(
                    auth,
                    need("mail", "move", &[("folder", &src)], &[("folder", dest)]),
                )?;
                // Also checked again by the effect, just before the rename.
                refuse_overwrite(&root, dest, &path)?;
            }
            Some((src, path))
        }
        other => {
            return Err(Fail::Failed(format!(
                "reagent `mail` has no tool `{other}`"
            )))
        }
    };
    Ok((root, located))
}

fn refuse_overwrite(root: &Path, dest: &str, path: &Path) -> Result<(), Fail> {
    if root.join(dest).join(path.file_name().unwrap()).exists() {
        return Err(Fail::Denied(
            "destination already exists; refusing overwrite".into(),
        ));
    }
    Ok(())
}

/// Refuse a mailbox containing symlinks or duplicate identifiers.
fn scan_mailbox(root: &Path) -> Result<(), Fail> {
    let mut ids = HashSet::new();
    for folder in std::fs::read_dir(root).map_err(|e| Fail::Failed(e.to_string()))? {
        let folder = folder.map_err(|e| Fail::Failed(e.to_string()))?;
        let ty = folder
            .file_type()
            .map_err(|e| Fail::Failed(e.to_string()))?;
        if ty.is_symlink() {
            return Err(Fail::Denied("symlink in fixture mailbox".into()));
        }
        if !ty.is_dir() {
            continue;
        }
        for file in std::fs::read_dir(folder.path()).map_err(|e| Fail::Failed(e.to_string()))? {
            let file = file.map_err(|e| Fail::Failed(e.to_string()))?;
            if file
                .file_type()
                .map_err(|e| Fail::Failed(e.to_string()))?
                .is_symlink()
            {
                return Err(Fail::Denied("symlink in fixture mailbox".into()));
            }
            let path = file.path();
            if matches!(
                path.extension().and_then(|x| x.to_str()),
                Some("eml" | "txt")
            ) {
                let id = path
                    .file_stem()
                    .and_then(|x| x.to_str())
                    .ok_or_else(|| Fail::Invalid("invalid message identifier".into()))?;
                if !ids.insert(id.to_owned()) {
                    return Err(Fail::Denied(
                        "ambiguous duplicate message identifier in fixture mailbox".into(),
                    ));
                }
            }
        }
    }
    Ok(())
}

fn mail_call(
    mail_root: Option<&str>,
    tool: &str,
    args: &Value,
    auth: &Authority,
) -> Result<Value, Fail> {
    let (root, located) = mail_authorize(mail_root, tool, args, auth)?;
    scan_mailbox(&root)?;
    match tool {
        "search" => {
            let folder = str_arg(args, "folder", "mail.search")?;
            let newest = args.get("newest").and_then(Value::as_u64).unwrap_or(50) as usize;
            let mut out = Vec::new();
            for (id, path) in message_files(&root.join(folder)).into_iter().take(newest) {
                let text = std::fs::read_to_string(&path).unwrap_or_default();
                let (from, subject, date) = headers(&text);
                out.push(json!({"id": id, "from": from, "subject": subject, "date": date}));
            }
            let out = Value::Array(out);
            Mail.adopt("search", &out, auth);
            Ok(out)
        }
        "read" => {
            let id = str_arg(args, "message", "mail.read")?;
            let (folder, path) = located.expect("mail_authorize locates a read");
            let text = std::fs::read_to_string(&path)
                .map_err(|e| Fail::Failed(format!("cannot read `{id}`: {e}")))?;
            let (from, subject, _) = headers(&text);
            let max = args
                .get("max_chars")
                .and_then(Value::as_u64)
                .unwrap_or(6000) as usize;
            let truncated = text.chars().count() > max;
            let body: String = text.chars().take(max).collect();
            Ok(
                json!({"id": id, "folder": folder, "from": from, "subject": subject, "text": body, "truncated": truncated}),
            )
        }
        "locate" => {
            let id = str_arg(args, "message", "mail.locate")?;
            match locate(&root, id) {
                Some((folder, _)) => Ok(json!({"id": id, "folder": folder, "exists": true})),
                None => Ok(json!({"id": id, "folder": null, "exists": false})),
            }
        }
        "move" => {
            let id = str_arg(args, "message", "mail.move")?;
            let dest = str_arg(args, "folder", "mail.move")?;
            let (src, path) = located.expect("mail_authorize locates a move");
            // An approved petition acts only on the message that was reviewed,
            // checked against the bytes about to be moved.
            if let Some(want) = auth.expected_digest {
                if message_digest(id, &src, &path)? != want {
                    return Err(Fail::Denied(format!(
                        "message `{id}` changed after its petition was filed"
                    )));
                }
            }
            // Idempotent: repeating a completed move changes nothing.
            if src == dest {
                return Ok(
                    json!({"id": id, "from": src, "to": dest, "moved": false, "already_there": true}),
                );
            }
            let dir = root.join(dest);
            std::fs::create_dir_all(&dir)
                .map_err(|e| Fail::Failed(format!("cannot open folder `{dest}`: {e}")))?;
            refuse_overwrite(&root, dest, &path)?;
            let target = dir.join(path.file_name().unwrap());
            #[cfg(target_os = "linux")]
            {
                use std::ffi::CString;
                use std::os::unix::ffi::OsStrExt;
                let src_c = CString::new(path.as_os_str().as_bytes())
                    .map_err(|_| Fail::Failed("NUL in source path".into()))?;
                let dst_c = CString::new(target.as_os_str().as_bytes())
                    .map_err(|_| Fail::Failed("NUL in target path".into()))?;
                let rc = unsafe {
                    libc::renameat2(
                        libc::AT_FDCWD,
                        src_c.as_ptr(),
                        libc::AT_FDCWD,
                        dst_c.as_ptr(),
                        libc::RENAME_NOREPLACE,
                    )
                };
                if rc != 0 {
                    let err = std::io::Error::last_os_error();
                    if err.raw_os_error() == Some(libc::EEXIST) {
                        return Err(Fail::Failed(format!(
                            "destination `{dest}` already holds `{}`",
                            path.file_name().unwrap().to_string_lossy()
                        )));
                    }
                    return Err(Fail::Failed(format!("cannot move `{id}`: {err}")));
                }
            }
            #[cfg(not(target_os = "linux"))]
            {
                std::fs::rename(&path, &target)
                    .map_err(|e| Fail::Failed(format!("cannot move `{id}`: {e}")))?;
            }
            Ok(json!({"id": id, "from": src, "to": dest, "moved": true}))
        }
        other => Err(Fail::Failed(format!(
            "reagent `mail` has no tool `{other}`"
        ))),
    }
}

/// A job lease protects result ownership, not an external filesystem/service.
/// Enforce this independently of model prompts and even for held writes.
pub fn check_job_effect(job_bound: bool, effect: &str) -> Result<(), Fail> {
    if job_bound && !matches!(effect, "read" | "pure") {
        Err(Fail::Denied(
            "shared job workers cannot perform or propose external writes".into(),
        ))
    } else {
        Ok(())
    }
}

/// External read capabilities always require an explicit resource boundary.
pub fn validate_grant_scope(
    reagent: &str,
    on: &BTreeMap<String, String>,
    into: &BTreeMap<String, String>,
) -> Result<(), String> {
    let key = match reagent {
        "file" | "sight" => "root",
        "http" => "origin",
        _ if self::reagent(reagent).is_none() => {
            // An MCP reagent: nothing maps a scope key to its arguments.
            if on.is_empty() && into.is_empty() {
                return Ok(());
            }
            return Err(format!(
                "{reagent} is an MCP reagent and its grants cannot be scoped; \
                 grant the tool without on/into, or not at all"
            ));
        }
        _ => return Ok(()),
    };
    if !into.is_empty() || on.len() != 1 || !on.contains_key(key) {
        return Err(format!(
            "{reagent} grants require exactly on {key}=VALUE and no destination scope"
        ));
    }
    match reagent {
        "file" | "sight" => crate::file_reagent::validate_root(&on[key]),
        _ => crate::http_reagent::validate_origin(&on[key]),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    fn mailbox() -> PathBuf {
        let root = std::env::temp_dir().join(format!("summonr-test-{}", crate::util::new_id("mb")));
        std::fs::create_dir_all(root.join("Inbox")).unwrap();
        std::fs::create_dir_all(root.join("Private")).unwrap();
        std::fs::write(
            root.join("Inbox/001.eml"),
            "From: a@x\nSubject: one\n\nbody one",
        )
        .unwrap();
        std::fs::write(
            root.join("Inbox/002.eml"),
            "From: b@x\nSubject: two\n\nbody two",
        )
        .unwrap();
        std::fs::write(
            root.join("Private/900.eml"),
            "From: c@x\nSubject: secret\n\nprivate",
        )
        .unwrap();
        root
    }

    fn empty_auth<'a>(grants: &'a [Grant], selected: &'a Mutex<HashSet<String>>) -> Authority<'a> {
        Authority {
            grants,
            deadline: None,
            on_item: None,
            selected,
            expected_digest: None,
            config: None,
        }
    }

    #[test]
    fn simulated_write_skips_verify() {
        let root = mailbox();
        std::fs::create_dir_all(root.join("Receipts")).unwrap();
        let grants = vec![need(
            "mail",
            "move",
            &[("folder", "Inbox")],
            &[("folder", "Receipts")],
        )];
        let selected = Mutex::new(HashSet::from(["001".to_string()]));
        let auth = empty_auth(&grants, &selected);
        let check = Check {
            observe: "mail.locate".into(),
            arguments: json!({"message": "001"}),
            expect: json!({"folder": "Receipts"}),
        };
        let io = crate::io::Simulate::new(std::sync::Arc::new(crate::io::Live));
        let (out, evidence) = perform(
            root.to_str(),
            "mail.move",
            &json!({"message": "001", "folder": "Receipts"}),
            Some(&check),
            &auth,
            &io,
        )
        .unwrap();
        assert_eq!(out["simulated"], true);
        assert!(
            matches!(evidence, Evidence::Simulated { .. }),
            "{evidence:?}"
        );
        assert_eq!(
            serde_json::to_value(&evidence).unwrap()["class"],
            "simulated by the runtime"
        );
        assert!(root.join("Inbox/001.eml").exists());
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn authorize_refuses_without_io() {
        let selected = Mutex::new(HashSet::new());
        let auth = empty_auth(&[], &selected);
        let started = std::time::Instant::now();
        let r = authorize(
            None,
            "http.get_text",
            &json!({"url":"http://127.0.0.1:9/x"}),
            &auth,
        );
        assert!(matches!(r, Err(Fail::Denied(_))), "{r:?}");
        assert!(started.elapsed() < std::time::Duration::from_millis(100));
    }

    #[test]
    fn authorize_file_rejects_dot_paths() {
        let root = std::env::temp_dir().join(crate::util::new_id("summonr-authz"));
        std::fs::create_dir_all(&root).unwrap();
        let path = root.to_str().unwrap();
        let grants = vec![need("file", "read_text", &[("root", path)], &[])];
        let selected = Mutex::new(HashSet::new());
        let auth = empty_auth(&grants, &selected);
        let r = authorize(
            None,
            "file.read_text",
            &json!({"root": path, "path": "../x"}),
            &auth,
        );
        assert!(matches!(r, Err(Fail::Denied(_))), "{r:?}");
        std::fs::remove_dir_all(root).ok();
    }

    #[test]
    fn adopt_mail_search_selects_ids() {
        let selected = Mutex::new(HashSet::new());
        let auth = empty_auth(&[], &selected);
        adopt("mail.search", &json!([{"id":"001"}]), &auth);
        assert!(auth.selected.lock().unwrap().contains("001"));
    }

    /// `authorize` refuses exactly what a live call refuses, with the same
    /// variant, and permits what the live call permits.
    #[test]
    fn authorize_agrees_with_live_calls() {
        fn kind(r: &Result<impl std::fmt::Debug, Fail>) -> String {
            match r {
                Ok(_) => "ok".into(),
                Err(f) => format!("{f:?}").split('(').next().unwrap().to_string(),
            }
        }
        let root = mailbox();
        std::fs::create_dir_all(root.join("Receipts")).unwrap();
        std::fs::write(root.join("Receipts/003.eml"), "Subject: three\n\n").unwrap();
        let r = root.to_str();
        let files = std::env::temp_dir().join(crate::util::new_id("summonr-parity"));
        std::fs::create_dir_all(&files).unwrap();
        std::fs::write(files.join("a.txt"), "a").unwrap();
        let fp = files.to_str().unwrap();
        let grants = vec![
            need("mail", "search", &[("folder", "Inbox")], &[]),
            need("mail", "read", &[("folder", "Inbox")], &[]),
            need("file", "read_text", &[("root", fp)], &[]),
            need("file", "list", &[], &[]),
        ];
        let selected = Mutex::new(HashSet::from(["001".into(), "003".into(), "900".into()]));
        let auth = empty_auth(&grants, &selected);
        let cases = [
            ("mail.search", json!({"folder": "Private"})),
            ("mail.search", json!({"folder": "../Private"})),
            ("mail.search", json!({"folder": "Inbox"})),
            ("mail.read", json!({"message": "002"})),
            ("mail.read", json!({"message": "../Private/900"})),
            ("mail.read", json!({"message": "900"})),
            ("mail.read", json!({"message": "777"})),
            ("mail.locate", json!({"message": "001"})),
            ("mail.move", json!({"message": "001", "folder": "Trash"})),
            ("mail.move", json!({"message": "003", "folder": "Receipts"})),
            ("mail.move", json!({"message": "003", "folder": ".."})),
            ("mail.read", json!({"message": "001", "extra": 1})),
            ("file.read_text", json!({"root": fp, "path": "a.txt"})),
            ("file.read_text", json!({"root": fp, "path": "../a.txt"})),
            ("file.read_text", json!({"root": fp, "path": "/etc/passwd"})),
            ("file.read_text", json!({"root": "/", "path": "etc/passwd"})),
            (
                "file.read_text",
                json!({"root": "relative", "path": "a.txt"}),
            ),
            ("file.list", json!({"root": fp, "path": ""})),
            ("http.get_text", json!({"url": "http://127.0.0.1:9/x"})),
            ("http.get_text", json!({"url": "http://u:p@127.0.0.1:9/x"})),
            ("http.get_text", json!({"url": "ftp://127.0.0.1/x"})),
            ("data.count", json!({"value": [1]})),
        ];
        for (op, args) in cases {
            let (rg, tool) = op.split_once('.').unwrap();
            let checked = authorize(r, op, &args, &auth);
            let live = call(r, rg, tool, &args, &auth);
            assert_eq!(
                kind(&checked),
                kind(&live),
                "{op} {args}: {checked:?} vs {live:?}"
            );
        }
        // A duplicated identifier is refused before any grant is consulted.
        std::fs::write(root.join("Private/001.eml"), "x").unwrap();
        let args = json!({"message": "001"});
        assert!(matches!(
            authorize(r, "mail.read", &args, &auth),
            Err(Fail::Denied(_))
        ));
        assert!(matches!(
            call(r, "mail", "read", &args, &auth),
            Err(Fail::Denied(_))
        ));
        std::fs::remove_dir_all(root).ok();
        std::fs::remove_dir_all(files).ok();
    }

    #[test]
    fn authority_is_enforced_outside_the_model() {
        let root = mailbox();
        let r = root.to_str();
        let grants = vec![
            need("mail", "search", &[("folder", "Inbox")], &[]),
            need("mail", "read", &[("folder", "Inbox")], &[]),
            need(
                "mail",
                "move",
                &[("folder", "Inbox")],
                &[("folder", "Receipts")],
            ),
        ];
        let selected = Mutex::new(HashSet::new());
        let auth = Authority {
            grants: &grants,
            deadline: None,
            on_item: None,
            selected: &selected,
            expected_digest: None,
            config: None,
        };

        // Nothing is readable before a search selects it.
        assert!(matches!(
            call(r, "mail", "read", &json!({"message": "001"}), &auth),
            Err(Fail::Denied(_))
        ));

        let found = call(
            r,
            "mail",
            "search",
            &json!({"folder": "Inbox", "newest": 1}),
            &auth,
        )
        .unwrap();
        assert_eq!(found[0]["id"], "002", "newest first");
        assert_eq!(found.as_array().unwrap().len(), 1);

        // Unselected, ungranted, and escaping targets are all refused.
        assert!(matches!(
            call(r, "mail", "read", &json!({"message": "001"}), &auth),
            Err(Fail::Denied(_))
        ));
        assert!(matches!(
            call(r, "mail", "search", &json!({"folder": "Private"}), &auth),
            Err(Fail::Denied(_))
        ));
        assert!(matches!(
            call(
                r,
                "mail",
                "read",
                &json!({"message": "../Private/900"}),
                &auth,
            ),
            Err(Fail::Denied(_))
        ));
        assert!(matches!(
            call(
                r,
                "mail",
                "move",
                &json!({"message": "002", "folder": "Trash"}),
                &auth,
            ),
            Err(Fail::Denied(_))
        ));

        // A granted move works, and repeating it is harmless.
        let moved = call(
            r,
            "mail",
            "move",
            &json!({"message": "002", "folder": "Receipts"}),
            &auth,
        )
        .unwrap();
        assert_eq!(moved["moved"], true);
        assert!(root.join("Receipts/002.eml").exists());
        let again = call(
            r,
            "mail",
            "move",
            &json!({"message": "002", "folder": "Receipts"}),
            &auth,
        )
        .unwrap();
        assert_eq!(again["already_there"], true);

        // Locating needs its own grant, then reports the observed folder.
        assert!(matches!(
            call(r, "mail", "locate", &json!({"message": "002"}), &auth),
            Err(Fail::Denied(_))
        ));
        let mut grants = grants.clone();
        grants.push(need("mail", "locate", &[], &[]));
        let auth = Authority {
            grants: &grants,
            deadline: None,
            on_item: None,
            selected: &selected,
            expected_digest: None,
            config: None,
        };
        let check = |folder: &str| Check {
            observe: "mail.locate".into(),
            arguments: json!({"message": "002"}),
            expect: json!({"folder": folder}),
        };
        let (_, evidence) = perform(
            r,
            "mail.move",
            &json!({"message": "002", "folder": "Receipts"}),
            Some(&check("Receipts")),
            &auth,
            &crate::io::Live,
        )
        .unwrap();
        assert_eq!(
            serde_json::to_value(&evidence).unwrap()["class"],
            "observed by the runtime"
        );
        assert_eq!(evidence.observation().unwrap().1["folder"], "Receipts");
        // A state that does not match what was expected is never reported as success.
        assert!(matches!(
            perform(
                r,
                "mail.move",
                &json!({"message": "002", "folder": "Receipts"}),
                Some(&check("Action")),
                &auth,
                &crate::io::Live
            ),
            Err(Fail::Unverified(_))
        ));
        let (_, evidence) = perform(
            r,
            "mail.move",
            &json!({"message": "002", "folder": "Receipts"}),
            None,
            &auth,
            &crate::io::Live,
        )
        .unwrap();
        assert_eq!(
            serde_json::to_value(&evidence).unwrap()["class"],
            "reported by the tool"
        );
        std::fs::remove_dir_all(root).ok();
    }

    #[test]
    fn move_refuses_when_message_differs_from_expected_digest() {
        let root = mailbox();
        let r = root.to_str();
        let grants = vec![need("mail", "move", &[("folder", "Inbox")], &[("folder", "Receipts")])];
        let selected = Mutex::new(HashSet::from(["001".to_string()]));
        let args = json!({"message": "001", "folder": "Receipts"});
        let wrong = "0".repeat(64);
        let auth = Authority { grants: &grants, deadline: None, on_item: None, selected: &selected, expected_digest: Some(&wrong), config: None };
        assert!(matches!(perform(r, "mail.move", &args, None, &auth, &crate::io::Live), Err(Fail::Denied(_))));
        assert!(root.join("Inbox/001.eml").exists(), "a mismatched digest must not move");
        let right = target_digest(r, "mail.move", &args).unwrap().unwrap();
        let auth = Authority { expected_digest: Some(&right), ..auth };
        perform(r, "mail.move", &args, None, &auth, &crate::io::Live).unwrap();
        assert!(root.join("Receipts/001.eml").exists());
        std::fs::remove_dir_all(root).ok();
    }


    #[test]
    fn reagent_grant_scope_for_sight_is_root() {
        let m = |k: &str, v: &str| BTreeMap::from([(k.to_string(), v.to_string())]);
        let none = BTreeMap::new();
        assert!(validate_grant_scope("sight", &none, &none).is_err());
        assert!(validate_grant_scope("sight", &m("root", "pictures"), &none).is_err());
        assert!(validate_grant_scope("sight", &m("root", "/a/../b"), &none).is_err());
        assert!(validate_grant_scope("sight", &m("origin", "/a"), &none).is_err());
        assert!(validate_grant_scope("sight", &m("root", "/a"), &m("root", "/b")).is_err());
        assert!(validate_grant_scope("sight", &m("root", "/home/u/pics"), &none).is_ok());
    }
}
