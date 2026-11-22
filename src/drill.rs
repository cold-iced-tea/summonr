//! Drills: ritual unit tests. Each case runs the saved ritual with scripted
//! model answers, writes simulated, and tools replayed from a cassette or
//! read live under the ritual's real grants.

use crate::exec::Finished;
use crate::io::Scripted;
use crate::outcome::Outcome;
use crate::ritual;
use serde::{Deserialize, Serialize};
use crate::touchstone::Expected;
use serde_json::{json, Map, Value};
use std::collections::{BTreeMap, HashSet};

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DrillFile {
    pub drill: String,
    pub ritual: String,
    /// A saved pure ritual that computes each case's expected result.
    #[serde(default)]
    pub touchstone: Option<String>,
    pub cases: Vec<Case>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Case {
    pub id: String,
    #[serde(default)]
    pub inputs: Map<String, Value>,
    /// Model step id -> answers in call order.
    #[serde(default)]
    pub model: BTreeMap<String, Vec<Value>>,
    /// Model step id -> the answer for every call after its listed ones.
    #[serde(default)]
    pub model_default: BTreeMap<String, Value>,
    /// Tools are replayed from this cassette, relative to the drill file.
    pub cassette: Option<String>,
    pub expect: Expect,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Expect {
    /// Defaults to `returned`, so a case never passes on a failed run unnoticed.
    pub outcome: Option<Outcome>,
    /// Dotted path into the result -> expected value (objects match as subsets).
    pub result: Option<Map<String, Value>>,
    pub counts: Option<Counts>,
    pub model_calls: Option<u64>,
}

/// Effects across the whole run tree, root and children. A write the ritual
/// would hold counts as both `held` and `simulated`.
#[derive(Deserialize, Serialize, Default, Clone, Copy, PartialEq, Debug)]
#[serde(deny_unknown_fields)]
pub struct Counts {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub applied: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub simulated: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub held: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub observed: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub unverified: Option<u64>,
}

impl Counts {
    fn fields(&self) -> [(&'static str, Option<u64>); 5] {
        [
            ("applied", self.applied),
            ("simulated", self.simulated),
            ("held", self.held),
            ("observed", self.observed),
            ("unverified", self.unverified),
        ]
    }
}

/// Parse a drill file (YAML or JSON). Unknown keys anywhere are errors.
pub fn parse(text: &str) -> Result<DrillFile, String> {
    let file: DrillFile = serde_yaml::from_str(text).map_err(|e| e.to_string())?;
    validate_file(file)
}

/// Parse an inline drill definition from JSON. Unknown keys anywhere are errors.
pub fn parse_value(v: Value) -> Result<DrillFile, String> {
    let file: DrillFile = serde_json::from_value(v).map_err(|e| e.to_string())?;
    validate_file(file)
}

fn validate_file(file: DrillFile) -> Result<DrillFile, String> {
    if file.cases.is_empty() {
        return Err("the drill has no cases".into());
    }
    let mut seen = HashSet::new();
    for c in &file.cases {
        if c.id.trim().is_empty() {
            return Err("a case has an empty `id`".into());
        }
        if !seen.insert(c.id.as_str()) {
            return Err(format!("case id `{}` appears more than once", c.id));
        }
    }
    Ok(file)
}

/// Tally effects from a summon record's `effects` (the events of its run
/// tree): writes applied or simulated, holds, observations, failed checks.
pub fn tally(effects: &Value) -> Counts {
    let mut c = Counts {
        applied: Some(0),
        simulated: Some(0),
        held: Some(0),
        observed: Some(0),
        unverified: Some(0),
    };
    let bump = |n: &mut Option<u64>| *n = Some(n.unwrap_or(0) + 1);
    for e in effects.as_array().into_iter().flatten() {
        let data = &e["data"];
        match e["kind"].as_str() {
            Some("op") if !matches!(data["effect"].as_str(), Some("read" | "pure")) => {
                match data["state"].as_str() {
                    Some("simulated") => {
                        bump(&mut c.simulated);
                        // The ritual asked to hold this write; a simulation
                        // files no petition, but the intent is a hold.
                        if data["would_hold"] == true {
                            bump(&mut c.held);
                        }
                    }
                    Some("applied") => {
                        bump(&mut c.applied);
                        if data["evidence"]["class"] == "observed by the runtime" {
                            bump(&mut c.observed);
                        }
                    }
                    _ => {}
                }
            }
            Some("held") => bump(&mut c.held),
            Some("check.failed") => bump(&mut c.unverified),
            _ => {}
        }
    }
    c
}

/// `want` matches `got` when equal, or, for objects, when every key of
/// `want` matches the same key of `got`.
fn subset(want: &Value, got: &Value) -> bool {
    match (want, got) {
        (Value::Object(w), Value::Object(g)) => w
            .iter()
            .all(|(k, v)| g.get(k).is_some_and(|gv| subset(v, gv))),
        _ => want == got,
    }
}

/// Judge one finished case. `record` is the summon record (with `effects`
/// and `requests`). Returns the case's report; `passed` is false when any
/// expectation mismatches or the script was not used exactly.
pub fn judge(
    case: &Case,
    fin: &Finished,
    record: &Value,
    script: &Scripted,
    expected: Option<&Expected>,
) -> Value {
    let mut reasons = Vec::new();
    let missed = script.missed();
    let mut named = HashSet::new();
    for step in &missed {
        if named.insert(step) {
            reasons.push(format!("drill gave no answer for model step `{step}`"));
        }
    }
    let unused = script.unused();
    if !unused.is_empty() {
        let left: Vec<String> = unused
            .iter()
            .map(|(step, n)| format!("`{step}` ({n})"))
            .collect();
        reasons.push(format!("unused scripted answers: {}", left.join(", ")));
    }
    let e = &case.expect;
    let mut touchstone_error = None;
    {
        let want = e.outcome.unwrap_or(Outcome::Returned);
        if want != fin.outcome {
            let mut r = format!("expected outcome {want}, got {}", fin.outcome);
            if let (Some(err), true) = (&fin.error, missed.is_empty()) {
                r.push_str(&format!(" ({err})"));
            }
            reasons.push(r);
        }
    }
    // A touchstone describes the returned result; it is consulted only for
    // cases expected to return.
    let computed: Option<Map<String, Value>> = match expected {
        Some(Expected::TouchstoneError(err)) => {
            touchstone_error = Some(err.clone());
            None
        }
        Some(Expected::Value(v)) if e.outcome.unwrap_or(Outcome::Returned) == Outcome::Returned => {
            match v {
                Value::Object(m) => Some(m.clone()),
                other => {
                    touchstone_error = Some(format!("touchstone returned {other}, not an object"));
                    None
                }
            }
        }
        _ => None,
    };
    for (path, want) in e.result.iter().flatten().chain(computed.iter().flatten()) {
        let segs: Vec<String> = path.split('.').map(String::from).collect();
        match ritual::descend(fin.result.clone(), &segs, &segs) {
            Ok(got) if subset(want, &got) => {}
            Ok(got) => reasons.push(format!("result.{path}: expected {want}, got {got}")),
            Err(err) => reasons.push(format!("result.{path}: {err}")),
        }
    }
    let counts = tally(&record["effects"]);
    if let Some(want) = &e.counts {
        for ((name, want), (_, got)) in want.fields().into_iter().zip(counts.fields()) {
            if let Some(want) = want {
                let got = got.unwrap_or(0);
                if want != got {
                    reasons.push(format!("counts.{name}: expected {want}, got {got}"));
                }
            }
        }
    }
    let calls = record["requests"].as_u64().unwrap_or(0);
    if let Some(want) = e.model_calls {
        if want != calls {
            reasons.push(format!("model_calls: expected {want}, got {calls}"));
        }
    }
    json!({
        "id": case.id, "passed": reasons.is_empty() && touchstone_error.is_none(),
        "reasons": reasons, "touchstone_error": touchstone_error,
        "summon": record["id"], "outcome": fin.outcome, "result": fin.result,
        "error": fin.error, "counts": counts, "model_calls": calls,
    })
}

#[cfg(test)]
mod tests {
    use crate::core::Core;
    use crate::rpc::RpcErr;
    use crate::util::{new_id, Paths};
    use serde_json::{json, Value};
    use std::path::{Path, PathBuf};
    use std::sync::Arc;

    struct Home {
        core: Arc<Core>,
        root: PathBuf,
    }

    impl Home {
        fn new() -> Self {
            Self::with_config("")
        }

        fn with_config(config: &str) -> Self {
            let root = std::env::temp_dir().join(new_id("summonr-drill"));
            let paths = Paths {
                runtime: root.join("run"),
                state: root.join("state"),
                config: root.join("config"),
            };
            std::fs::create_dir_all(&paths.config).unwrap();
            std::fs::write(paths.config.join("config.yaml"), config).unwrap();
            let core = Core::open(paths).unwrap();
            Home { core, root }
        }

        fn inscribe(&self, path: &Path) {
            self.core
                .dispatch("inscribe", json!({"path": path.to_str().unwrap()}))
                .unwrap();
        }

        fn drill(&self, ritual: &str, text: &str) -> Result<Value, RpcErr> {
            let file = self.root.join(format!("{}.drill.yaml", new_id("d")));
            std::fs::write(&file, text).unwrap();
            self.core.dispatch(
                "drill",
                json!({"ritual": ritual, "against": file.to_str().unwrap()}),
            )
        }
    }

    impl Drop for Home {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.root);
        }
    }

    fn repo(rel: &str) -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR")).join(rel)
    }

    fn route_form() -> Home {
        let h = Home::new();
        h.inscribe(&repo("examples/04-route-form/route-form.yaml"));
        h
    }

    fn case(out: &Value, i: usize) -> &Value {
        &out["cases"][i]
    }

    fn reasons(out: &Value, i: usize) -> String {
        case(out, i)["reasons"].to_string()
    }

    const OTHER: &str = r#"'{"category":"other","message":"I write for a magazine"}'"#;

    #[test]
    fn passing_case_passes() {
        let h = route_form();
        let out = h
            .drill(
                "route-form",
                &format!(
                    "drill: route-form-cases
ritual: route-form
cases:
  - id: other-goes-to-model
    inputs: {{form: {OTHER}}}
    model: {{judged: [{{category: press}}]}}
    expect:
      outcome: returned
      result: {{from_model: press}}
      counts: {{simulated: 0}}
      model_calls: 1
"
                ),
            )
            .unwrap();
        assert_eq!(case(&out, 0)["passed"], true, "{out}");
        assert_eq!(
            (out["passed"].clone(), out["failed"].clone()),
            (json!(1), json!(0))
        );
        assert!(out["series"].as_str().unwrap().starts_with("series"));
        // A scripted model was never loaded, so nothing is unloaded.
        let release: String = h
            .core
            .db
            .lock()
            .unwrap()
            .query_row(
                "SELECT text FROM events WHERE summon=?1 AND kind='release'",
                [case(&out, 0)["summon"].as_str().unwrap()],
                |r| r.get(0),
            )
            .unwrap();
        assert!(release.ends_with("not applicable (scripted)"), "{release}");
    }

    #[test]
    fn unused_answer_fails_case() {
        let h = route_form();
        let out = h
            .drill(
                "route-form",
                &format!(
                    "drill: d
ritual: route-form
cases:
  - id: two-answers
    inputs: {{form: {OTHER}}}
    model: {{judged: [{{category: press}}, {{category: sales}}]}}
    expect: {{outcome: returned, result: {{from_model: press}}}}
"
                ),
            )
            .unwrap();
        assert_eq!(case(&out, 0)["passed"], false, "{out}");
        assert!(reasons(&out, 0).contains("unused"), "{out}");
        assert_eq!(out["failed"], 1);
    }

    #[test]
    fn missing_answer_fails_case() {
        let h = route_form();
        let out = h
            .drill(
                "route-form",
                &format!(
                    "drill: d
ritual: route-form
cases:
  - id: no-script
    inputs: {{form: {OTHER}}}
    expect: {{outcome: returned}}
"
                ),
            )
            .unwrap();
        assert_eq!(case(&out, 0)["passed"], false, "{out}");
        assert!(
            reasons(&out, 0).contains("no answer for model step"),
            "{out}"
        );
    }

    #[test]
    fn scripted_error_and_mismatches_are_reported() {
        let h = route_form();
        let out = h
            .drill(
                "route-form",
                &format!(
                    "drill: d
ritual: route-form
cases:
  - id: backend-down
    inputs: {{form: {OTHER}}}
    model: {{judged: [{{error: backend unavailable}}]}}
    expect: {{outcome: failed}}
  - id: wrong-guess
    inputs: {{form: {OTHER}}}
    model: {{judged: ['{{\"category\":\"sales\"}}']}}
    expect: {{outcome: returned, result: {{from_model: press}}, model_calls: 0}}
"
                ),
            )
            .unwrap();
        assert_eq!(case(&out, 0)["passed"], true, "{out}");
        assert!(case(&out, 0)["error"]
            .as_str()
            .unwrap()
            .contains("backend unavailable"));
        assert_eq!(case(&out, 1)["passed"], false, "{out}");
        let r = reasons(&out, 1);
        assert!(r.contains("from_model") && r.contains("model_calls"), "{r}");
    }

    #[test]
    fn malformed_drills_are_invalid() {
        let h = route_form();
        let unknown = h
            .drill(
                "route-form",
                "drill: d\nritual: route-form\ncases:\n  - id: a\n    expect: {outcome: returned, extra: 1}\n",
            )
            .unwrap_err();
        assert_eq!(unknown.exit, 2, "{unknown:?}");
        let other = h
            .drill(
                "route-form",
                "drill: d\nritual: classify-message\ncases:\n  - id: a\n    expect: {}\n",
            )
            .unwrap_err();
        assert_eq!(other.exit, 2, "{other:?}");
        let missing = h
            .drill("no-such", "drill: d\nritual: no-such\ncases: []\n")
            .unwrap_err();
        assert_eq!(missing.exit, 3, "{missing:?}");
        // A cassette path resolves against the drill file's directory.
        let tape = h
            .drill(
                "route-form",
                "drill: d\nritual: route-form\ncases:\n  - id: a\n    inputs: {form: x}\n    cassette: tapes/none.json\n    expect: {}\n",
            )
            .unwrap_err();
        assert_eq!(tape.exit, 3, "{tape:?}");
        let want = h.root.join("tapes/none.json");
        assert!(tape.message.contains(want.to_str().unwrap()), "{tape:?}");
        // A drill file that is not there is not found; one that does not parse is invalid.
        let gone = h
            .core
            .dispatch(
                "drill",
                json!({"ritual": "route-form", "against": h.root.join("gone.drill.yaml").to_str().unwrap()}),
            )
            .unwrap_err();
        assert_eq!(gone.exit, 3, "{gone:?}");
        let junk = h.drill("route-form", "[not a drill").unwrap_err();
        assert_eq!(junk.exit, 2, "{junk:?}");
    }

    fn triage_home() -> (Home, PathBuf) {
        triage_home_with(1, "")
    }

    /// A mailbox with `messages` receipts, plus extra config lines.
    fn triage_home_with(messages: usize, config: &str) -> (Home, PathBuf) {
        let root = std::env::temp_dir().join(new_id("summonr-drill-mail"));
        let mailbox = root.join("mail");
        std::fs::create_dir_all(mailbox.join("Inbox")).unwrap();
        std::fs::create_dir_all(mailbox.join("Receipts")).unwrap();
        for i in 1..=messages {
            std::fs::write(
                mailbox.join(format!("Inbox/{i:03}.eml")),
                format!("From: shop@example.com\nSubject: receipt {i}\n\nTotal 12.00"),
            )
            .unwrap();
        }
        let h = Home::with_config(&format!("mail_root: \"{}\"\n{config}", mailbox.display()));
        h.inscribe(&repo("tests/fixtures/rituals/classify-message.yaml"));
        h.inscribe(&repo("tests/fixtures/rituals/inbox-triage.yaml"));
        (h, root)
    }

    const TRIAGE: &str = "drill: triage
ritual: inbox-triage
cases:
  - id: receipt-would-move
    inputs: {count: 1, apply: true, hold: false}
    model: {classify: [{folder: Receipts, reason: a receipt}]}
    expect:
      outcome: returned
      counts: {simulated: 1, applied: 0, held: 0}
      model_calls: 1
";

    fn grant_triage(h: &Home) {
        h.core
            .dispatch(
                "grant",
                json!({"ritual":"inbox-triage","scopes":[{"reagent":"mail","all":true}],"on":{"folder":"Inbox"}}),
            )
            .unwrap();
        h.core
            .dispatch(
                "grant",
                json!({"ritual":"inbox-triage","scopes":[{"reagent":"mail","select":["locate"]}]}),
            )
            .unwrap();
    }

    fn petitions(h: &Home) -> i64 {
        h.core
            .db
            .lock()
            .unwrap()
            .query_row("SELECT COUNT(*) FROM petitions", [], |r| r.get(0))
            .unwrap()
    }

    #[test]
    fn writes_are_simulated() {
        let (h, mail) = triage_home();
        grant_triage(&h);
        let out = h.drill("inbox-triage", TRIAGE).unwrap();
        assert_eq!(case(&out, 0)["passed"], true, "{out}");
        assert!(mail.join("mail/Inbox/001.eml").exists());
        assert!(!mail.join("mail/Receipts/001.eml").exists());
        let petitions: i64 = h
            .core
            .db
            .lock()
            .unwrap()
            .query_row("SELECT COUNT(*) FROM petitions", [], |r| r.get(0))
            .unwrap();
        assert_eq!(petitions, 0);
        std::fs::remove_dir_all(&mail).ok();
    }

    #[test]
    fn missing_grants_refuse_before_any_case() {
        let (h, mail) = triage_home();
        let e = h.drill("inbox-triage", TRIAGE).unwrap_err();
        assert_eq!(e.exit, 4, "{e:?}");
        let runs: i64 = h
            .core
            .db
            .lock()
            .unwrap()
            .query_row("SELECT COUNT(*) FROM summons", [], |r| r.get(0))
            .unwrap();
        assert_eq!(runs, 0);
        std::fs::remove_dir_all(&mail).ok();
    }

    #[test]
    fn a_held_write_counts_as_held_and_simulated() {
        let (h, mail) = triage_home();
        grant_triage(&h);
        let out = h
            .drill(
                "inbox-triage",
                "drill: triage
ritual: inbox-triage
cases:
  - id: receipt-would-be-held
    inputs: {count: 1, apply: true, hold: true}
    model: {classify: [{folder: Receipts, reason: a receipt}]}
    expect:
      counts: {held: 1, simulated: 1, applied: 0}
",
            )
            .unwrap();
        assert_eq!(case(&out, 0)["passed"], true, "{out}");
        assert_eq!(petitions(&h), 0);
        assert!(mail.join("mail/Inbox/001.eml").exists());
        std::fs::remove_dir_all(&mail).ok();
    }

    #[test]
    fn empty_expect_means_returned() {
        let h = route_form();
        let out = h
            .drill(
                "route-form",
                &format!(
                    "drill: d
ritual: route-form
cases:
  - id: backend-down
    inputs: {{form: {OTHER}}}
    model: {{judged: [{{error: backend unavailable}}]}}
    expect: {{}}
"
                ),
            )
            .unwrap();
        assert_eq!(case(&out, 0)["passed"], false, "{out}");
        let r = reasons(&out, 0);
        assert!(r.contains("expected outcome returned, got failed"), "{r}");
    }

    #[test]
    fn model_calls_count_the_whole_run_tree() {
        let (h, mail) = triage_home_with(2, "");
        grant_triage(&h);
        let out = h
            .drill(
                "inbox-triage",
                "drill: triage
ritual: inbox-triage
cases:
  - id: two-children
    inputs: {count: 2, apply: true, hold: false}
    model: {classify: [{folder: Receipts, reason: one}, {folder: abstain, reason: two}]}
    expect:
      counts: {simulated: 1, applied: 0, held: 0}
      model_calls: 2
",
            )
            .unwrap();
        assert_eq!(case(&out, 0)["passed"], true, "{out}");
        // The calls were made by two child summons, not by the root.
        let children: i64 = h
            .core
            .db
            .lock()
            .unwrap()
            .query_row(
                "SELECT COUNT(*) FROM summons WHERE parent=?1 AND requests=1",
                [case(&out, 0)["summon"].as_str().unwrap()],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(children, 2);
        std::fs::remove_dir_all(&mail).ok();
    }

    #[test]
    fn drills_never_touch_a_backend_even_with_a_baseline() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let hits = Arc::new(AtomicUsize::new(0));
        let counter = hits.clone();
        std::thread::spawn(move || {
            for conn in listener.incoming() {
                counter.fetch_add(1, Ordering::SeqCst);
                drop(conn);
            }
        });
        let h = Home::with_config(&format!("ollama_url: \"http://127.0.0.1:{port}\"\n"));
        h.inscribe(&repo("examples/04-route-form/route-form.yaml"));
        h.core
            .db
            .lock()
            .unwrap()
            .execute(
                "INSERT INTO baselines (ritual, version, model, set_name, set_hash, set_json, fingerprint, account, created)
                 VALUES ('route-form', 1, 'ollama/qwen3:4b', 's', 'h', '{}', '{\"old\":true}', '{}', 0)",
                [],
            )
            .unwrap();
        let out = h
            .drill(
                "route-form",
                &format!(
                    "drill: d
ritual: route-form
cases:
  - id: other-goes-to-model
    inputs: {{form: {OTHER}}}
    model: {{judged: [{{category: press}}]}}
    expect: {{result: {{from_model: press}}}}
"
                ),
            )
            .unwrap();
        assert_eq!(case(&out, 0)["passed"], true, "{out}");
        assert_eq!(
            hits.load(Ordering::SeqCst),
            0,
            "a drill reached the backend"
        );
    }

    // ------------------------------------------------------------ touchstones

    fn touchstone_home() -> Home {
        let h = route_form();
        h.inscribe(&repo("examples/04-route-form/route-form-touchstone.yaml"));
        h
    }

    #[test]
    fn touchstone_supplies_expected_results() {
        let h = touchstone_home();
        let text = std::fs::read_to_string(repo("examples/04-route-form/route-form.touchstone-drill.yaml")).unwrap();
        let out = h.drill("route-form", &text).unwrap();
        assert_eq!(out["passed"], 3, "{out}");
        assert_eq!(out["touchstone_errors"], 0, "{out}");
        assert_eq!(out["touchstone"]["name"], "route-form-touchstone", "{out}");
    }

    #[test]
    fn touchstone_disagreement_fails_the_case() {
        let h = Home::new();
        // A candidate that ignores the form and always says `sales`.
        let wrong = h.root.join("wrong.yaml");
        std::fs::write(&wrong, "ritual: route-form\ninputs: {form: {type: string}}\nreagents: {data: {select: [trim]}}\nessence: {requests: 0}\nsteps:\n  - id: t\n    operation: data.trim\n    arguments: {text: x}\nreturn: {from_form: sales}\n").unwrap();
        h.inscribe(&wrong);
        h.inscribe(&repo("examples/04-route-form/route-form-touchstone.yaml"));
        let out = h
            .drill(
                "route-form",
                r#"drill: d
ritual: route-form
touchstone: route-form-touchstone
cases:
  - id: support
    inputs: {form: '{"category":"support","message":"help"}'}
    expect: {}
"#,
            )
            .unwrap();
        assert_eq!(out["failed"], 1, "{out}");
        assert!(reasons(&out, 0).contains("from_form"), "{out}");
    }

    #[test]
    fn touchstone_and_hand_labels_cannot_mix() {
        let h = touchstone_home();
        let err = h
            .drill(
                "route-form",
                r#"drill: d
ritual: route-form
touchstone: route-form-touchstone
cases:
  - id: labelled
    inputs: {form: '{"category":"support","message":"help"}'}
    expect: {result: {from_form: support}}
"#,
            )
            .unwrap_err();
        assert!(err.message.contains("touchstone") && err.message.contains("expect.result"), "{}", err.message);
    }

    #[test]
    fn ineligible_touchstone_is_refused_before_any_case() {
        let h = route_form();
        let err = h
            .drill(
                "route-form",
                "drill: d\nritual: route-form\ntouchstone: route-form\ncases:\n  - id: a\n    inputs: {form: x}\n    expect: {}\n",
            )
            .unwrap_err();
        assert_eq!(err.exit, 2, "{}", err.message);
        assert!(err.message.contains("touchstone"), "{}", err.message);
    }

    #[test]
    fn touchstone_failure_is_not_a_case_failure() {
        let h = route_form();
        // A touchstone that refuses `press`, so one case has no expected value.
        let t = h.root.join("t.yaml");
        std::fs::write(
            &t,
            r#"ritual: picky
inputs: {form: {type: string}}
reagents: {data: {select: [parse_json, assert]}}
essence: {requests: 0}
steps:
  - id: f
    operation: data.parse_json
    arguments: {text: "${inputs.form}"}
  - id: ok
    operation: data.assert
    arguments: {condition: false, message: "picky touchstone"}
return: {from_form: "${steps.f.output.category}"}
"#,
        )
        .unwrap();
        h.inscribe(&t);
        let out = h
            .drill(
                "route-form",
                &format!("drill: d\nritual: route-form\ntouchstone: picky\ncases:\n  - id: a\n    inputs: {{form: {OTHER}}}\n    model: {{judged: [{{category: press}}]}}\n    expect: {{}}\n"),
            )
            .unwrap();
        assert_eq!(out["touchstone_errors"], 1, "{out}");
        assert_eq!(out["failed"], 0, "{out}");
        assert!(case(&out, 0)["touchstone_error"].as_str().unwrap().contains("picky"), "{out}");
    }

    #[test]
    fn touchstone_is_pinned_at_load() {
        let h = touchstone_home();
        let text = std::fs::read_to_string(repo("examples/04-route-form/route-form.touchstone-drill.yaml")).unwrap();
        let out = h.drill("route-form", &text).unwrap();
        let entry = h
            .core
            .dispatch("contract", json!({"name": "route-form-touchstone"}))
            .unwrap();
        assert_eq!(out["touchstone"]["version"], entry["version"], "{out}");
        assert_eq!(out["touchstone"]["hash"], entry["hash"], "{out} {entry}");
    }


    #[test]
    fn touchstone_is_not_consulted_for_non_returning_cases() {
        let h = touchstone_home();
        // Malformed input: the ritual fails as expected, and the touchstone,
        // which would also fail, is not asked.
        let out = h
            .drill(
                "route-form",
                "drill: d\nritual: route-form\ntouchstone: route-form-touchstone\ncases:\n  - id: bad\n    inputs: {form: 'not json'}\n    expect: {outcome: invalid}\n",
            )
            .unwrap();
        assert_eq!(out["touchstone_errors"], 0, "{out}");
        assert_eq!(out["passed"], 1, "{out}");
    }


    #[test]
    fn real_mismatch_beside_a_touchstone_error_counts_as_failed() {
        let h = route_form();
        let t = h.root.join("t.yaml");
        std::fs::write(
            &t,
            "ritual: picky\ninputs: {form: {type: string}}\nreagents: {data: {select: [assert]}}\nessence: {requests: 0}\nsteps:\n  - id: ok\n    operation: data.assert\n    arguments: {condition: false, message: picky}\nreturn: {}\n",
        )
        .unwrap();
        h.inscribe(&t);
        let out = h
            .drill(
                "route-form",
                &format!("drill: d\nritual: route-form\ntouchstone: picky\ncases:\n  - id: a\n    inputs: {{form: {OTHER}}}\n    model: {{judged: [{{category: press}}]}}\n    expect: {{model_calls: 5}}\n"),
            )
            .unwrap();
        assert_eq!(out["failed"], 1, "{out}");
        assert_eq!(out["touchstone_errors"], 0, "{out}");
    }

}
