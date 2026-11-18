//! The seam between the executor and the outside world. Every model call and
//! tool call a summon makes goes through `Io`, so a run can be wrapped
//! (recorded, replayed, simulated) without the executor knowing.

use crate::backend;
use crate::cassette::{self, Cassette, Entry};
use crate::reagent;
use serde_json::Value;
use std::collections::{BTreeMap, HashMap, VecDeque};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

pub trait Io: Send + Sync {
    /// One model call made by step `step`.
    fn chat(
        &self,
        base_url: &str,
        model: &str,
        step: &str,
        req: backend::ChatReq,
    ) -> Result<backend::ChatResp, String>;

    /// One tool call, `operation` being `reagent.tool`.
    fn call(
        &self,
        root: Option<&str>,
        operation: &str,
        args: &Value,
        auth: &reagent::Authority,
    ) -> Result<Value, reagent::Fail>;

    /// True when this `Io` never reaches a backend, so the run must not
    /// either (no unload, no drift fingerprint).
    fn offline(&self) -> bool {
        false
    }

    /// True when a call to `operation` is answered with what it would do
    /// rather than done. Decided by the `Io`, never by inspecting output.
    fn simulates(&self, _operation: &str) -> bool {
        false
    }

    /// True when model answers come from a drill script, so no model is
    /// loaded and none needs releasing.
    fn scripted(&self) -> bool {
        false
    }
}

/// The real backends and reagents.
pub struct Live;

impl Io for Live {
    fn chat(
        &self,
        base_url: &str,
        model: &str,
        _step: &str,
        req: backend::ChatReq,
    ) -> Result<backend::ChatResp, String> {
        backend::chat(base_url, model, req)
    }

    fn call(
        &self,
        root: Option<&str>,
        operation: &str,
        args: &Value,
        auth: &reagent::Authority,
    ) -> Result<Value, reagent::Fail> {
        let (reagent, tool) = operation
            .split_once('.')
            .ok_or_else(|| reagent::Fail::Failed(format!("bad operation `{operation}`")))?;
        reagent::call(root, reagent, tool, args, auth)
    }
}

/// Wraps another `Io` so writes are authorized and then answered with what
/// they would do, never performed. Reads, pure tools and model calls pass
/// through. A simulated write never reaches the inner `Io`, so it is neither
/// recorded nor looked up in a cassette.
pub struct Simulate {
    inner: Arc<dyn Io>,
}

impl Simulate {
    pub fn new(inner: Arc<dyn Io>) -> Self {
        Simulate { inner }
    }
}

impl Io for Simulate {
    fn offline(&self) -> bool {
        self.inner.offline()
    }

    fn scripted(&self) -> bool {
        self.inner.scripted()
    }

    fn simulates(&self, operation: &str) -> bool {
        is_write(operation)
    }

    fn chat(
        &self,
        base_url: &str,
        model: &str,
        step: &str,
        req: backend::ChatReq,
    ) -> Result<backend::ChatResp, String> {
        self.inner.chat(base_url, model, step, req)
    }

    fn call(
        &self,
        root: Option<&str>,
        operation: &str,
        args: &Value,
        auth: &reagent::Authority,
    ) -> Result<Value, reagent::Fail> {
        if !is_write(operation) {
            return self.inner.call(root, operation, args, auth);
        }
        reagent::authorize(root, operation, args, auth)?;
        Ok(serde_json::json!({"simulated": true, "operation": operation, "arguments": args}))
    }
}

/// Answers model calls from a drill's script, by step id in call order, and
/// passes tool calls to `tools` (a replay or live reads). It never reaches a
/// model backend.
pub struct Scripted {
    answers: Mutex<HashMap<String, VecDeque<Value>>>,
    missed: Mutex<Vec<String>>,
    tools: Arc<dyn Io>,
}

impl Scripted {
    pub fn new(answers: BTreeMap<String, Vec<Value>>, tools: Arc<dyn Io>) -> Self {
        Scripted {
            answers: Mutex::new(answers.into_iter().map(|(k, v)| (k, v.into())).collect()),
            missed: Mutex::new(Vec::new()),
            tools,
        }
    }

    /// Steps with answers left over, and how many, sorted by step.
    pub fn unused(&self) -> Vec<(String, usize)> {
        let mut left: Vec<(String, usize)> = self
            .answers
            .lock()
            .unwrap()
            .iter()
            .filter(|(_, q)| !q.is_empty())
            .map(|(k, q)| (k.clone(), q.len()))
            .collect();
        left.sort();
        left
    }

    /// Steps that asked for an answer the script did not have, in order.
    pub fn missed(&self) -> Vec<String> {
        self.missed.lock().unwrap().clone()
    }
}

impl Io for Scripted {
    fn offline(&self) -> bool {
        self.tools.offline()
    }

    fn scripted(&self) -> bool {
        true
    }

    fn chat(
        &self,
        _base_url: &str,
        _model: &str,
        step: &str,
        _req: backend::ChatReq,
    ) -> Result<backend::ChatResp, String> {
        let answer = self
            .answers
            .lock()
            .unwrap()
            .get_mut(step)
            .and_then(VecDeque::pop_front);
        let Some(answer) = answer else {
            self.missed.lock().unwrap().push(step.to_string());
            return Err(format!("drill gave no answer for model step `{step}`"));
        };
        let content = match answer {
            Value::Object(m) if m.len() == 1 && m.get("error").is_some_and(Value::is_string) => {
                return Err(m["error"].as_str().unwrap_or_default().to_string())
            }
            Value::String(s) => s,
            other => other.to_string(),
        };
        Ok(backend::ChatResp {
            content,
            done_reason: None,
            prompt_tokens: 0,
            output_tokens: 0,
            load_ms: 0,
            prompt_ms: 0,
            eval_ms: 0,
            total_ms: 0,
            payload: serde_json::json!({"backend": "scripted", "step": step}),
            constrained: false,
            token_confidence: "exact",
        })
    }

    fn call(
        &self,
        root: Option<&str>,
        operation: &str,
        args: &Value,
        auth: &reagent::Authority,
    ) -> Result<Value, reagent::Fail> {
        self.tools.call(root, operation, args, auth)
    }
}

/// A known tool whose effect is neither read nor pure. An unknown operation
/// is never guessed to be a write.
fn is_write(operation: &str) -> bool {
    effect(operation).is_some_and(|e| !matches!(e, "read" | "pure"))
}

/// A known tool whose effect is pure: computed from its arguments alone, so
/// it is never recorded and a replay computes it live, metered as live.
fn is_pure(operation: &str) -> bool {
    effect(operation) == Some("pure")
}

fn effect(operation: &str) -> Option<&'static str> {
    operation
        .split_once('.')
        .and_then(|(r, t)| reagent::tool(r, t))
        .map(|t| t.effect)
}

/// Wraps another `Io`, appending every model success and every tool call that
/// is not pure to a cassette that is saved after each entry, so a killed run
/// leaves a usable file.
pub struct Recording {
    inner: Arc<dyn Io>,
    path: PathBuf,
    cassette: Mutex<Cassette>,
}

impl Recording {
    /// Saves the empty cassette at once, so an unwritable path fails before
    /// any step runs. An existing file is replaced only if it is a cassette.
    pub fn new(inner: Arc<dyn Io>, path: PathBuf) -> Result<Self, String> {
        if path.exists() {
            cassette::load(&path)
                .map_err(|e| format!("will not record over a file that is not a cassette: {e}"))?;
        }
        let cassette = Cassette::default();
        cassette::save(&path, &cassette).map_err(|e| format!("cannot record to {e}"))?;
        Ok(Recording {
            inner,
            path,
            cassette: Mutex::new(cassette),
        })
    }

    /// Append and save under one lock so file order matches call order.
    fn append(&self, entry: Entry) -> Result<(), String> {
        let mut c = self.cassette.lock().unwrap();
        c.entries.push(entry);
        cassette::save(&self.path, &c).map_err(|e| format!("cassette could not be saved: {e}"))
    }
}

impl Io for Recording {
    fn offline(&self) -> bool {
        self.inner.offline()
    }

    fn scripted(&self) -> bool {
        self.inner.scripted()
    }

    fn chat(
        &self,
        base_url: &str,
        model: &str,
        step: &str,
        req: backend::ChatReq,
    ) -> Result<backend::ChatResp, String> {
        let hash = cassette::model_hash(model, &req);
        let resp = self.inner.chat(base_url, model, step, req)?;
        self.append(Entry::Model {
            step: step.to_string(),
            model: model.to_string(),
            request_hash: hash,
            payload: resp.payload.clone(),
            content: resp.content.clone(),
            prompt_tokens: resp.prompt_tokens,
            output_tokens: resp.output_tokens,
        })?;
        Ok(resp)
    }

    fn call(
        &self,
        root: Option<&str>,
        operation: &str,
        args: &Value,
        auth: &reagent::Authority,
    ) -> Result<Value, reagent::Fail> {
        if is_pure(operation) {
            return self.inner.call(root, operation, args, auth);
        }
        let result = self.inner.call(root, operation, args, auth);
        let (output, error) = match &result {
            Ok(v) => (Some(v.clone()), None),
            Err(e) => (None, Some(cassette::RecordedError::from_fail(e))),
        };
        self.append(Entry::Tool {
            operation: operation.to_string(),
            request_hash: cassette::tool_hash(operation, args),
            arguments: args.clone(),
            output,
            error,
        })
        .map_err(reagent::Fail::Failed)?;
        result
    }
}

/// Answers model calls and non-pure tool calls from a cassette and never
/// touches a backend or reagent effect. Tool calls are authorized first, so a
/// recording never widens authority. Entries are matched by request hash,
/// first in first out. Pure tools are computed live, through the same
/// metering a live run applies, and any recorded entry for them is ignored.
/// The core always wraps a replay in `Simulate`, so writes are never looked up.
pub struct Replay {
    queues: Mutex<HashMap<String, VecDeque<Entry>>>,
}

impl Replay {
    pub fn new(cassette: Cassette) -> Self {
        let mut queues: HashMap<String, VecDeque<Entry>> = HashMap::new();
        for entry in cassette.entries {
            let key = match &entry {
                Entry::Model { request_hash, .. } | Entry::Tool { request_hash, .. } => {
                    request_hash.clone()
                }
            };
            queues.entry(key).or_default().push_back(entry);
        }
        Replay {
            queues: Mutex::new(queues),
        }
    }

    pub fn load(path: &std::path::Path) -> Result<Self, cassette::LoadError> {
        cassette::load(path).map(Self::new)
    }

    fn pop(&self, hash: &str) -> Option<Entry> {
        self.queues.lock().unwrap().get_mut(hash)?.pop_front()
    }
}

impl Io for Replay {
    fn offline(&self) -> bool {
        true
    }

    fn chat(
        &self,
        _base_url: &str,
        model: &str,
        step: &str,
        req: backend::ChatReq,
    ) -> Result<backend::ChatResp, String> {
        let hash = cassette::model_hash(model, &req);
        match self.pop(&hash) {
            Some(Entry::Model {
                payload,
                content,
                prompt_tokens,
                output_tokens,
                ..
            }) => Ok(backend::ChatResp {
                content,
                done_reason: None,
                prompt_tokens,
                output_tokens,
                load_ms: 0,
                prompt_ms: 0,
                eval_ms: 0,
                total_ms: 0,
                payload,
                constrained: false,
                token_confidence: "exact",
            }),
            _ => Err(format!(
                "no recording for model step `{step}` (request {}); re-record the cassette",
                &hash[..12]
            )),
        }
    }

    fn call(
        &self,
        root: Option<&str>,
        operation: &str,
        args: &Value,
        auth: &reagent::Authority,
    ) -> Result<Value, reagent::Fail> {
        if is_pure(operation) {
            return Live.call(root, operation, args, auth);
        }
        reagent::authorize(root, operation, args, auth)?;
        let hash = cassette::tool_hash(operation, args);
        match self.pop(&hash) {
            Some(Entry::Tool {
                output: Some(output),
                error: None,
                ..
            }) => {
                reagent::adopt(operation, &output, auth);
                Ok(output)
            }
            Some(Entry::Tool {
                error: Some(error), ..
            }) => Err(error.to_fail()),
            _ => Err(reagent::Fail::Failed(format!(
                "no recording for {operation} (request {}); re-record the cassette",
                &hash[..12]
            ))),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::backend::ChatReq;
    use serde_json::json;
    use std::collections::{BTreeMap, HashSet};
    use std::sync::{Arc, Mutex};

    #[test]
    fn live_delegates_to_mock_backend_and_data_reagent() {
        let schema = json!({"type":"object","properties":{"folder":{"enum":["Receipts","Action","abstain"]}}});
        let settings = BTreeMap::new();
        let req = ChatReq {
            model: "mock/keyword",
            system: "classify",
            user: "your receipt",
            schema: Some(&schema),
            settings: &settings,
            timeout: None,
            max_output_tokens: None,
        };
        let resp = Live
            .chat("http://unused", "mock/keyword", "s", req)
            .unwrap();
        assert!(resp.content.contains("Receipts"), "{}", resp.content);

        let selected = Mutex::new(HashSet::new());
        let auth = reagent::Authority {
            grants: &[],
            deadline: None,
            on_item: None,
            selected: &selected,
            expected_digest: None,
            config: None,
        };
        let out = Live
            .call(None, "data.count", &json!({"value":[1,2]}), &auth)
            .unwrap();
        assert_eq!(out, json!(2));
    }

    fn chat_req<'a>(
        user: &'a str,
        schema: &'a Value,
        settings: &'a BTreeMap<String, Value>,
    ) -> ChatReq<'a> {
        ChatReq {
            model: "mock/keyword",
            system: "classify",
            user,
            schema: Some(schema),
            settings,
            timeout: None,
            max_output_tokens: None,
        }
    }

    #[test]
    fn recording_saves_after_each_entry_and_skips_pure_operations() {
        let dir = std::env::temp_dir().join(format!("rec-{}", crate::util::new_id("t")));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("c.json");
        let rec = Recording::new(Arc::new(Live), path.clone()).unwrap();
        let selected = Mutex::new(HashSet::new());
        let auth = reagent::Authority {
            grants: &[],
            deadline: None,
            on_item: None,
            selected: &selected,
            expected_digest: None,
            config: None,
        };
        let schema = json!({"type":"object"});
        let settings = BTreeMap::new();
        rec.chat(
            "u",
            "mock/keyword",
            "a",
            chat_req("one", &schema, &settings),
        )
        .unwrap();
        assert_eq!(crate::cassette::load(&path).unwrap().entries.len(), 1);
        rec.chat(
            "u",
            "mock/keyword",
            "b",
            chat_req("two", &schema, &settings),
        )
        .unwrap();
        assert_eq!(crate::cassette::load(&path).unwrap().entries.len(), 2);
        let n = rec
            .call(None, "data.count", &json!({"value":[1,2]}), &auth)
            .unwrap();
        assert_eq!(n, json!(2));
        assert_eq!(crate::cassette::load(&path).unwrap().entries.len(), 2);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn recording_replaces_only_a_cassette() {
        let dir = std::env::temp_dir().join(format!("rec-{}", crate::util::new_id("t")));
        std::fs::create_dir_all(&dir).unwrap();
        let notes = dir.join("notes.txt");
        std::fs::write(&notes, "keep me").unwrap();
        assert!(Recording::new(Arc::new(Live), notes.clone()).is_err());
        assert_eq!(std::fs::read_to_string(&notes).unwrap(), "keep me");
        let path = dir.join("c.json");
        crate::cassette::save(
            &path,
            &Cassette {
                entries: vec![tool_entry("mail.search", json!({}), json!([]))],
                ..Cassette::default()
            },
        )
        .unwrap();
        Recording::new(Arc::new(Live), path.clone()).unwrap();
        assert!(crate::cassette::load(&path).unwrap().entries.is_empty());
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn replay_computes_pure_operations_live_and_metered() {
        let args = json!({"value":[1,2,3]});
        // A stale recorded answer for a pure operation is ignored.
        let replay = Replay::new(Cassette {
            entries: vec![tool_entry("data.count", args.clone(), json!(99))],
            ..Cassette::default()
        });
        let selected = Mutex::new(HashSet::new());
        let out = replay
            .call(None, "data.count", &args, &auth_with(&[], &selected))
            .unwrap();
        assert_eq!(out, json!(3));
        let charged = std::sync::atomic::AtomicUsize::new(0);
        let on_item = || {
            if charged.fetch_add(1, std::sync::atomic::Ordering::SeqCst) >= 2 {
                Err(reagent::Fail::Exhausted("items".into()))
            } else {
                Ok(())
            }
        };
        let auth = reagent::Authority {
            grants: &[],
            deadline: None,
            on_item: Some(&on_item),
            selected: &selected,
            expected_digest: None,
            config: None,
        };
        let map = json!({"values":[1,2,3],"operation":"data.arithmetic",
            "arguments":{"left":"${item}","right":2,"operator":"multiply"}});
        let r = replay.call(None, "data.map", &map, &auth);
        assert!(matches!(r, Err(reagent::Fail::Exhausted(_))), "{r:?}");
        assert_eq!(charged.load(std::sync::atomic::Ordering::SeqCst), 3);
    }

    #[test]
    fn recording_to_a_missing_directory_fails_at_construction() {
        let path = std::env::temp_dir()
            .join(format!("nope-{}", crate::util::new_id("t")))
            .join("c.json");
        assert!(Recording::new(Arc::new(Live), path).is_err());
    }

    #[test]
    fn a_later_save_failure_surfaces_from_call() {
        let dir = std::env::temp_dir().join(format!("rec-{}", crate::util::new_id("t")));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("c.json");
        let rec = Recording::new(Arc::new(Live), path.clone()).unwrap();
        // A directory where the temp file must go makes the next save fail.
        std::fs::create_dir(dir.join("c.json.tmp")).unwrap();
        let selected = Mutex::new(HashSet::new());
        let auth = reagent::Authority {
            grants: &[],
            deadline: None,
            on_item: None,
            selected: &selected,
            expected_digest: None,
            config: None,
        };
        // An ungranted read is refused, and the refusal is recorded.
        match rec.call(
            None,
            "http.get_text",
            &json!({"url":"http://127.0.0.1:9/x"}),
            &auth,
        ) {
            Err(reagent::Fail::Failed(m)) => {
                assert!(m.starts_with("cassette could not be saved"), "{m}")
            }
            other => panic!("expected save failure, got {other:?}"),
        }
        std::fs::remove_dir_all(&dir).ok();
    }

    fn auth_with<'a>(
        grants: &'a [crate::ritual::Grant],
        selected: &'a Mutex<HashSet<String>>,
    ) -> reagent::Authority<'a> {
        reagent::Authority {
            grants,
            deadline: None,
            on_item: None,
            selected,
            expected_digest: None,
            config: None,
        }
    }

    fn tool_entry(op: &str, args: Value, output: Value) -> Entry {
        Entry::Tool {
            operation: op.into(),
            request_hash: cassette::tool_hash(op, &args),
            arguments: args,
            output: Some(output),
            error: None,
        }
    }

    #[test]
    fn replay_refuses_ungranted_tool_even_when_recorded() {
        let args = json!({"url":"http://127.0.0.1:9/x"});
        let c = Cassette {
            entries: vec![tool_entry(
                "http.get_text",
                args.clone(),
                json!({"text":"hi"}),
            )],
            ..Cassette::default()
        };
        let replay = Replay::new(c);
        let selected = Mutex::new(HashSet::new());
        let r = replay.call(None, "http.get_text", &args, &auth_with(&[], &selected));
        assert!(matches!(r, Err(reagent::Fail::Denied(_))), "{r:?}");
    }

    #[test]
    fn replay_miss_names_step_and_hash() {
        let schema = json!({"type":"object"});
        let settings = BTreeMap::new();
        let mk = |user| ChatReq {
            model: "mock/keyword",
            system: "s",
            user,
            schema: Some(&schema),
            settings: &settings,
            timeout: None,
            max_output_tokens: None,
        };
        let hash = cassette::model_hash("mock/keyword", &mk("original"));
        let c = Cassette {
            entries: vec![Entry::Model {
                step: "classify".into(),
                model: "mock/keyword".into(),
                request_hash: hash,
                payload: json!({"p":1}),
                content: "{\"a\":1}".into(),
                prompt_tokens: 7,
                output_tokens: 3,
            }],
            ..Cassette::default()
        };
        let replay = Replay::new(c);
        let ok = replay
            .chat("http://unused", "mock/keyword", "classify", mk("original"))
            .unwrap();
        assert_eq!(ok.content, "{\"a\":1}");
        assert_eq!((ok.prompt_tokens, ok.output_tokens), (7, 3));
        assert_eq!(ok.token_confidence, "exact");
        // FIFO: the single entry is consumed.
        assert!(replay
            .chat("http://unused", "mock/keyword", "classify", mk("original"))
            .is_err());
        let err = replay
            .chat("http://unused", "mock/keyword", "classify", mk("changed"))
            .err()
            .unwrap();
        assert!(err.contains("`classify`"), "{err}");
        let h = err
            .split("(request ")
            .nth(1)
            .unwrap()
            .split(')')
            .next()
            .unwrap();
        assert_eq!(h.len(), 12);
        assert!(h.chars().all(|c| c.is_ascii_hexdigit()), "{err}");
    }

    #[test]
    fn replayed_search_selects_ids() {
        let root = std::env::temp_dir().join(crate::util::new_id("summonr-replay"));
        std::fs::create_dir_all(root.join("Inbox")).unwrap();
        std::fs::write(
            root.join("Inbox/001.eml"),
            "From: a@x\nSubject: one\n\nbody",
        )
        .unwrap();
        let rootstr = root.to_str().unwrap();
        let grants = vec![
            reagent::need("mail", "search", &[("folder", "Inbox")], &[]),
            reagent::need("mail", "read", &[("folder", "Inbox")], &[]),
        ];
        let search = json!({"folder":"Inbox"});
        let read = json!({"message":"001"});
        let c = Cassette {
            entries: vec![
                tool_entry("mail.search", search.clone(), json!([{"id":"001"}])),
                tool_entry("mail.read", read.clone(), json!({"id":"001","body":"body"})),
            ],
            ..Cassette::default()
        };
        let replay = Replay::new(c);
        let selected = Mutex::new(HashSet::new());
        let auth = auth_with(&grants, &selected);
        // Not yet selected: refused even though recorded.
        assert!(replay
            .call(Some(rootstr), "mail.read", &read, &auth)
            .is_err());
        replay
            .call(Some(rootstr), "mail.search", &search, &auth)
            .unwrap();
        assert!(selected.lock().unwrap().contains("001"));
        replay
            .call(Some(rootstr), "mail.read", &read, &auth)
            .unwrap();
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn replayed_denied_error_stays_denied() {
        let args = json!({"url":"http://127.0.0.1:9/x"});
        let c = Cassette {
            entries: vec![Entry::Tool {
                operation: "http.get_text".into(),
                request_hash: cassette::tool_hash("http.get_text", &args),
                arguments: args.clone(),
                output: None,
                error: Some(cassette::RecordedError {
                    kind: cassette::ErrorKind::Denied,
                    message: "nope".into(),
                }),
            }],
            ..Cassette::default()
        };
        let grants = vec![reagent::need(
            "http",
            "get_text",
            &[("origin", "http://127.0.0.1:9")],
            &[],
        )];
        let selected = Mutex::new(HashSet::new());
        let r = Replay::new(c).call(None, "http.get_text", &args, &auth_with(&grants, &selected));
        assert!(
            matches!(r, Err(reagent::Fail::Denied(ref m)) if m == "nope"),
            "{r:?}"
        );
    }

    fn move_fixture() -> (PathBuf, Vec<crate::ritual::Grant>) {
        let root = std::env::temp_dir().join(crate::util::new_id("summonr-sim"));
        std::fs::create_dir_all(root.join("Inbox")).unwrap();
        std::fs::create_dir_all(root.join("Receipts")).unwrap();
        std::fs::write(
            root.join("Inbox/001.eml"),
            "From: a@x\nSubject: one\n\nbody",
        )
        .unwrap();
        let grants = vec![reagent::need(
            "mail",
            "move",
            &[("folder", "Inbox")],
            &[("folder", "Receipts")],
        )];
        (root, grants)
    }

    #[test]
    fn simulate_never_writes() {
        let (root, grants) = move_fixture();
        let selected = Mutex::new(HashSet::from(["001".to_string()]));
        let auth = auth_with(&grants, &selected);
        let sim = Simulate::new(Arc::new(Live));
        let args = json!({"message":"001","folder":"Receipts"});
        let out = sim.call(root.to_str(), "mail.move", &args, &auth).unwrap();
        assert_eq!(out["simulated"], true);
        assert!(sim.simulates("mail.move"));
        assert!(!sim.simulates("mail.read"));
        assert!(!sim.simulates("data.count"));
        assert!(root.join("Inbox/001.eml").exists());
        assert!(!root.join("Receipts/001.eml").exists());
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn simulate_still_authorizes() {
        let (root, grants) = move_fixture();
        let selected = Mutex::new(HashSet::from(["001".to_string()]));
        let auth = auth_with(&grants, &selected);
        let sim = Simulate::new(Arc::new(Live));
        let r = sim.call(
            root.to_str(),
            "mail.move",
            &json!({"message":"001","folder":"Private"}),
            &auth,
        );
        assert!(matches!(r, Err(reagent::Fail::Denied(_))), "{r:?}");
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn simulate_delegates_reads_unknowns_and_offline() {
        let selected = Mutex::new(HashSet::new());
        let auth = auth_with(&[], &selected);
        let sim = Simulate::new(Arc::new(Live));
        let out = sim
            .call(None, "data.count", &json!({"value":[1,2]}), &auth)
            .unwrap();
        assert_ne!(out["simulated"], true);
        assert!(sim.call(None, "nosuch.tool", &json!({}), &auth).is_err());
        assert!(!sim.simulates("nosuch.tool"));
        assert!(!sim.offline());
        assert!(Simulate::new(Arc::new(Replay::new(Cassette::default()))).offline());
    }

    fn scripted_req<'a>(schema: &'a Value, settings: &'a BTreeMap<String, Value>) -> ChatReq<'a> {
        ChatReq {
            model: "m",
            system: "s",
            user: "u",
            schema: Some(schema),
            settings,
            timeout: None,
            max_output_tokens: None,
        }
    }

    #[test]
    fn scripted_answers_by_step_and_tracks_leftovers() {
        let schema = json!({"type":"object"});
        let settings = BTreeMap::new();
        let answers = BTreeMap::from([
            (
                "a".to_string(),
                vec![json!({"x": 1}), json!("raw text"), json!({"error": "down"})],
            ),
            ("b".to_string(), vec![json!(true)]),
        ]);
        let s = Scripted::new(answers, Arc::new(Replay::new(Cassette::default())));
        assert!(s.offline());
        let r = s
            .chat("u", "ollama/m", "a", scripted_req(&schema, &settings))
            .unwrap();
        assert_eq!(r.content, r#"{"x":1}"#);
        assert_eq!((r.prompt_tokens, r.output_tokens), (0, 0));
        assert_eq!(r.token_confidence, "exact");
        assert_eq!(r.payload, json!({"backend":"scripted","step":"a"}));
        let r = s
            .chat("u", "ollama/m", "a", scripted_req(&schema, &settings))
            .unwrap();
        assert_eq!(r.content, "raw text");
        let e = s
            .chat("u", "ollama/m", "a", scripted_req(&schema, &settings))
            .err()
            .unwrap();
        assert_eq!(e, "down");
        let e = s
            .chat("u", "ollama/m", "a", scripted_req(&schema, &settings))
            .err()
            .unwrap();
        assert_eq!(e, "drill gave no answer for model step `a`");
        assert_eq!(s.missed(), vec!["a".to_string()]);
        assert_eq!(s.unused(), vec![("b".to_string(), 1)]);
        assert!(!Scripted::new(BTreeMap::new(), Arc::new(Live)).offline());
        let sim = Simulate::new(Arc::new(Scripted::new(BTreeMap::new(), Arc::new(Live))));
        assert!(sim.scripted() && !sim.offline());
        assert!(!Simulate::new(Arc::new(Live)).scripted());
    }

    #[test]
    fn offline_flags() {
        assert!(!Live.offline());
        assert!(Replay::new(Cassette::default()).offline());
    }
}
