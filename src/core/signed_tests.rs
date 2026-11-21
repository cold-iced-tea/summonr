//! Signed requests: verification, separation between familiars, signed replies.
use super::test_home::{repo, Home};
use super::*;
use crate::envelope::{new_nonce, open_reply, seal_request};
use crate::keys::Keypair;

struct Agent {
    key: Keypair,
}

fn agent(h: &Home, name: &str, allow: &[&str]) -> Agent {
    h.call("familiar_new", json!({"name": name})).unwrap();
    if !allow.is_empty() {
        h.call("familiar_allow", json!({"name": name, "rituals": allow})).unwrap();
    }
    let file = h.core.paths.config.join(format!("familiars/{name}.key"));
    Agent { key: crate::keys::load(&file).unwrap() }
}

/// Send one signed request through the core and open its signed reply.
fn signed_at(h: &Home, a: &Agent, method: &str, params: Value, ts: i64, nonce: &str) -> Result<Value, RpcErr> {
    let frame = seal_request(&a.key, &h.core.key.public().id(), method, &params, ts, nonce);
    let reply = h.core.handle_signed(&frame);
    open_reply(&reply, &h.core.key.public(), nonce).expect("replies are signed by the core and bound to the nonce")
}

fn signed(h: &Home, a: &Agent, method: &str, params: Value) -> Result<Value, RpcErr> {
    signed_at(h, a, method, params, now_ms(), &new_nonce())
}

fn receipt(h: &Home, a: &Agent, extra: Value) -> Value {
    let mut p = json!({"target":"total-receipt","args":{"text":"{\"amounts_minor\":[100,25]}"}});
    for (k, v) in extra.as_object().unwrap() {
        p[k] = v.clone();
    }
    let started = signed(h, a, "invoke", p).unwrap();
    signed(h, a, "wait", json!({"summon": started["summon"], "timeout_ms": 10000})).unwrap();
    started
}

/// The demo triage, with its classifier pinned by the owner to the mock model:
/// a signed agent may not choose the model itself.
fn mock_triage() -> (Home, PathBuf) {
    let (h, mail) = Home::triage();
    let text = std::fs::read_to_string(repo("tests/fixtures/rituals/classify-message.yaml")).unwrap();
    let mocked: String = text
        .lines()
        .map(|l| if l.starts_with("model:") { "model: mock/keyword".to_string() } else { l.to_string() })
        .collect::<Vec<_>>()
        .join("\n");
    let file = h.root.join("classify-message.yaml");
    std::fs::write(&file, mocked).unwrap();
    std::fs::copy(repo("tests/fixtures/rituals/rules.md"), h.root.join("rules.md")).unwrap();
    h.inscribe(&file, true);
    // The parent pins its child at inscription, so inscribe it again.
    h.inscribe(&repo("tests/fixtures/rituals/inbox-triage.yaml"), true);
    (h, mail)
}

fn home() -> Home {
    let h = Home::new("");
    h.inscribe(&repo("tests/fixtures/rituals/total-receipt.yaml"), false);
    h
}

#[test]
fn signed_invoke_records_familiar() {
    let h = home();
    let a = agent(&h, "alpha", &["total-receipt"]);
    let started = receipt(&h, &a, json!({}));
    let id = started["summon"].as_str().unwrap();
    assert_eq!(h.core.owner_of(id), Some(Some("familiar:alpha".to_string())));
    let got = signed(&h, &a, "get", json!({"summon": id})).unwrap();
    assert_eq!(got["result"]["total_minor"], 125, "{got}");
}

#[test]
fn b_cannot_get_wait_or_cancel_a_run() {
    let h = home();
    let a = agent(&h, "alpha", &["total-receipt"]);
    let b = agent(&h, "beta", &["total-receipt"]);
    let id = receipt(&h, &a, json!({}))["summon"].as_str().unwrap().to_string();
    let missing = signed(&h, &b, "get", json!({"summon": "s-missing"})).unwrap_err();
    for (method, params) in [
        ("get", json!({"summon": id})),
        ("wait", json!({"summon": id, "timeout_ms": 100})),
        ("cancel", json!({"summon": id})),
    ] {
        let e = signed(&h, &b, method, params).unwrap_err();
        assert_eq!(e.code, "not_found", "{method}");
        assert_eq!(e.message, missing.message.replace("s-missing", &id), "{method}: same answer as a missing run");
    }
    // The owner still sees it.
    assert!(h.call("get", json!({"summon": id})).is_ok());
}

#[test]
fn petitions_are_scoped_to_owner() {
    let (h, mail) = mock_triage();
    let a = agent(&h, "alpha", &["inbox-triage"]);
    let b = agent(&h, "beta", &["inbox-triage"]);
    let started = signed(&h, &a, "invoke", json!({"target":"inbox-triage","args":{"count":1,"apply":true}})).unwrap();
    signed(&h, &a, "wait", json!({"summon": started["summon"], "timeout_ms": 10000})).unwrap();
    assert_eq!(h.petitions(), 1);
    let mine = signed(&h, &a, "petitions", json!({"status": "all"})).unwrap();
    assert_eq!(mine.as_array().unwrap().len(), 1, "{mine}");
    assert_eq!(mine[0]["status"], "pending");
    let theirs = signed(&h, &b, "petitions", json!({"status": "all"})).unwrap();
    assert_eq!(theirs, json!([]));
    let _ = std::fs::remove_dir_all(mail);
}

#[test]
fn idempotency_is_per_familiar() {
    let h = home();
    let a = agent(&h, "alpha", &["total-receipt"]);
    let b = agent(&h, "beta", &["total-receipt"]);
    let one = receipt(&h, &a, json!({"idempotency_key": "job-1"}));
    let again = receipt(&h, &a, json!({"idempotency_key": "job-1"}));
    let other = receipt(&h, &b, json!({"idempotency_key": "job-1"}));
    assert_eq!(one["summon"], again["summon"]);
    assert_ne!(one["summon"], other["summon"]);
}

#[test]
fn unallowed_ritual_is_not_found_everywhere() {
    let h = home();
    h.inscribe(&repo("tests/fixtures/rituals/classify-message.yaml"), false);
    let a = agent(&h, "alpha", &["total-receipt"]);
    let listed = signed(&h, &a, "rituals", json!({})).unwrap();
    let names: Vec<&str> = listed.as_array().unwrap().iter().map(|r| r["ritual"].as_str().unwrap()).collect();
    assert_eq!(names, ["total-receipt"]);
    for (method, params) in [
        ("contract", json!({"name": "classify-message"})),
        ("rehearse", json!({"target": "classify-message"})),
        ("invoke", json!({"target": "classify-message", "args": {"text": "x"}})),
        ("invoke", json!({"target": repo("tests/fixtures/rituals/total-receipt.yaml").to_str().unwrap()})),
    ] {
        assert_eq!(signed(&h, &a, method, params).unwrap_err().code, "not_found", "{method}");
    }
}

#[test]
fn new_familiar_sees_no_rituals() {
    let h = home();
    let a = agent(&h, "alpha", &[]);
    assert_eq!(signed(&h, &a, "rituals", json!({})).unwrap(), json!([]));
}

#[test]
fn owner_only_methods_refused() {
    let h = home();
    let a = agent(&h, "alpha", &["total-receipt"]);
    for method in ["grant", "inscribe", "petition_decide", "familiar_new", "familiar_allow", "peer_add", "assay", "drill", "glean", "job_create", "shutdown", "list", "identity"] {
        let e = signed(&h, &a, method, json!({})).unwrap_err();
        assert_eq!(e.code, "not_authorized", "{method}");
    }
}

#[test]
fn smuggled_caller_ignored() {
    let h = home();
    let a = agent(&h, "alpha", &["total-receipt"]);
    let id = receipt(&h, &a, json!({"caller": ""}))["summon"].as_str().unwrap().to_string();
    assert_eq!(h.core.owner_of(&id), Some(Some("familiar:alpha".to_string())));
}

#[test]
fn agent_restrictions_apply() {
    let (h, mail) = Home::triage();
    let a = agent(&h, "alpha", &["inbox-triage"]);
    let e = signed(&h, &a, "invoke", json!({"target":"inbox-triage","model":"mock/keyword","args":{"count":1,"hold":false}})).unwrap_err();
    assert_eq!(e.code, "not_authorized", "user_only input: {}", e.message);
    let e = signed(&h, &a, "invoke", json!({"target":"inbox-triage","record":"/tmp/nope.json"})).unwrap_err();
    assert_eq!(e.code, "not_authorized", "arbitrary host record path: {}", e.message);
    let _ = std::fs::remove_dir_all(mail);
}

#[test]
fn refusal_is_generic_and_logged() {
    let h = home();
    let stranger = Agent { key: crate::keys::generate() };
    let e = signed(&h, &stranger, "rituals", json!({})).unwrap_err();
    assert_eq!((e.code.as_str(), e.message.as_str(), e.exit), ("not_authenticated", crate::envelope::UNBOUND_REFUSAL, 4));
    assert!(h.core.last_refusal().unwrap().contains("unknown"), "{:?}", h.core.last_refusal());
}

#[test]
fn clock_skew_reason_logged() {
    let h = home();
    let a = agent(&h, "alpha", &["total-receipt"]);
    let e = signed_at(&h, &a, "rituals", json!({}), now_ms() - 300_000, &new_nonce()).unwrap_err();
    assert_eq!(e.code, "not_authenticated");
    assert!(h.core.last_refusal().unwrap().contains("clock skew"), "{:?}", h.core.last_refusal());
}

#[test]
fn replay_after_restart_refused() {
    let root = std::env::temp_dir().join(new_id("summonr-replay"));
    let paths = Paths { runtime: root.join("run"), state: root.join("state"), config: root.join("config") };
    let c = Core::open(paths.clone()).unwrap();
    c.dispatch("familiar_new", json!({"name": "alpha"})).unwrap();
    let key = crate::keys::load(&paths.config.join("familiars/alpha.key")).unwrap();
    let nonce = new_nonce();
    let frame = seal_request(&key, &c.key.public().id(), "rituals", &json!({}), now_ms(), &nonce);
    open_reply(&c.handle_signed(&frame), &c.key.public(), &nonce).unwrap().unwrap();
    drop(c);
    // A restarted core still remembers the nonce.
    let c = Core::open(paths).unwrap();
    let e = open_reply(&c.handle_signed(&frame), &c.key.public(), &nonce).unwrap().unwrap_err();
    assert_eq!(e.code, "not_authenticated");
    assert!(c.last_refusal().unwrap().contains("replay"), "{:?}", c.last_refusal());
    drop(c);
    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn replies_verify_with_core_key() {
    let h = home();
    let a = agent(&h, "alpha", &["total-receipt"]);
    let nonce = new_nonce();
    let frame = seal_request(&a.key, &h.core.key.public().id(), "rituals", &json!({}), now_ms(), &nonce);
    let reply = h.core.handle_signed(&frame);
    assert!(open_reply(&reply, &crate::keys::generate().public(), &nonce).is_err(), "only the core's key verifies its replies");
    assert!(open_reply(&reply, &h.core.key.public(), &nonce).is_ok());
}

#[test]
fn revoked_familiar_refused_next_request() {
    let h = home();
    let a = agent(&h, "alpha", &["total-receipt"]);
    assert!(signed(&h, &a, "rituals", json!({})).is_ok());
    h.call("familiar_revoke", json!({"name": "alpha"})).unwrap();
    assert_eq!(signed(&h, &a, "rituals", json!({})).unwrap_err().code, "not_authenticated");
}

#[test]
fn no_private_key_material_outside_key_files() {
    let (h, mail) = mock_triage();
    let a = agent(&h, "alpha", &["inbox-triage"]);
    let started = signed(&h, &a, "invoke", json!({"target":"inbox-triage","args":{"count":1,"apply":true}})).unwrap();
    signed(&h, &a, "wait", json!({"summon": started["summon"], "timeout_ms": 10000})).unwrap();
    let _ = signed(&h, &Agent { key: crate::keys::generate() }, "rituals", json!({}));
    let secrets: Vec<String> = ["core.key", "familiars/alpha.key"]
        .iter()
        .map(|f| std::fs::read_to_string(h.core.paths.config.join(f)).unwrap())
        .map(|pem| pem.lines().filter(|l| !l.starts_with("-----")).collect())
        .collect();
    let mut stack = vec![h.root.clone()];
    while let Some(dir) = stack.pop() {
        for entry in std::fs::read_dir(&dir).unwrap().flatten() {
            let p = entry.path();
            if p.is_dir() {
                stack.push(p);
                continue;
            }
            if p.extension().is_some_and(|e| e == "key") {
                continue;
            }
            let bytes = std::fs::read(&p).unwrap_or_default();
            let text = String::from_utf8_lossy(&bytes);
            assert!(!text.contains("PRIVATE KEY"), "{}", p.display());
            for s in &secrets {
                assert!(!text.contains(s.as_str()), "secret key material in {}", p.display());
            }
        }
    }
    let _ = std::fs::remove_dir_all(mail);
}

#[test]
fn run_records_show_the_familiar() {
    let h = home();
    let a = agent(&h, "alpha", &["total-receipt"]);
    let id = receipt(&h, &a, json!({}))["summon"].as_str().unwrap().to_string();
    assert_eq!(h.call("get", json!({"summon": id})).unwrap()["familiar"], "familiar:alpha");
    let ended = h.call("list", json!({"scope": "ended"})).unwrap();
    assert_eq!(ended[0]["familiar"], "familiar:alpha", "{ended}");
}

// ------------------------------------------------------ final-review fixes

#[test]
fn signed_caller_cannot_choose_model_settings_or_version() {
    let h = home();
    let a = agent(&h, "alpha", &["total-receipt"]);
    let args = json!({"text": "{\"amounts_minor\":[1]}"});
    for (method, extra) in [
        ("invoke", json!({"model": "ollama/elsewhere"})),
        ("invoke", json!({"settings": {"temperature": 1}})),
        ("invoke", json!({"version": 1})),
        ("invoke", json!({"record": "/tmp/x.json"})),
        ("rehearse", json!({"model": "ollama/elsewhere"})),
    ] {
        let mut p = json!({"target": "total-receipt", "args": args});
        for (k, v) in extra.as_object().unwrap() {
            p[k] = v.clone();
        }
        let e = signed(&h, &a, method, p).unwrap_err();
        assert_eq!(e.code, "not_authorized", "{method} {extra}: {}", e.message);
    }
    // What an agent may send still works, including the cwd MCP adds.
    let ok = signed(&h, &a, "invoke", json!({"target": "total-receipt", "args": args, "cwd": "/home/someone",
        "expected_hash": null, "idempotency_key": "k", "limits": {"requests": 0}}));
    assert!(ok.is_ok(), "{ok:?}");
}

#[test]
fn signed_wait_longer_than_300s_refused() {
    let h = home();
    let a = agent(&h, "alpha", &["total-receipt"]);
    let id = receipt(&h, &a, json!({}))["summon"].clone();
    let e = signed(&h, &a, "wait", json!({"summon": id, "timeout_ms": 300_001})).unwrap_err();
    assert_eq!(e.code, "invalid_invocation", "{}", e.message);
    assert!(signed(&h, &a, "wait", json!({"summon": id, "timeout_ms": 300_000})).is_ok());
}

#[test]
fn refusal_log_names_the_claimed_key() {
    let h = home();
    let stranger = Agent { key: crate::keys::generate() };
    let _ = signed(&h, &stranger, "rituals", json!({}));
    let id = stranger.key.public().id();
    let logged = h.core.last_refusal().unwrap();
    assert!(logged.contains(&format!("claimed key {}", &id[..16])), "{logged}");
}

#[test]
fn signed_fence_list_isolates_familiars() {
    let h = home();
    let alpha = agent(&h, "alpha", &["total-receipt"]);
    let beta = agent(&h, "beta", &["total-receipt"]);
    let a_id = receipt(&h, &alpha, json!({}))["summon"].as_str().unwrap().to_string();
    let b_id = receipt(&h, &beta, json!({}))["summon"].as_str().unwrap().to_string();
    let own_id = h
        .call("invoke", json!({"target": "total-receipt", "args": {"text": "{\"amounts_minor\":[1]}"}}))
        .unwrap()["summon"]
        .as_str()
        .unwrap()
        .to_string();

    for (sid, step) in [(&a_id, "s-a"), (&b_id, "s-b"), (&own_id, "s-own")] {
        let _ = h.core.fence_acquire(sid, step, "file.write", "write", &json!({"path": "x"})).unwrap();
    }

    let alpha_fences = signed(&h, &alpha, "fence_list", json!({})).unwrap();
    let alpha_arr = alpha_fences.as_array().unwrap();
    assert_eq!(alpha_arr.len(), 1, "{alpha_fences}");
    assert_eq!(alpha_arr[0]["summon"], a_id);

    let alpha_cross = signed(&h, &alpha, "fence_list", json!({"summon": b_id})).unwrap();
    assert_eq!(alpha_cross.as_array().unwrap().len(), 0, "{alpha_cross}");
}

/// `guarded` (allowed) echoes a user-only input; `hidden` (not allowed)
/// returns a secret, and is fit to be a touchstone for `guarded`.
fn drill_home() -> (Home, Agent) {
    let h = home();
    h.inscribe_def(
        json!({"ritual": "guarded",
               "inputs": {"scope": {"type": "string", "default": "public", "user_only": true}},
               "steps": [{"id": "s", "operation": "data.trim", "arguments": {"text": "${inputs.scope}"}}],
               "reagents": {"data": {"select": ["trim"]}},
               "return": {"scope": "${steps.s.output}"}}),
        false,
    );
    h.inscribe_def(
        json!({"ritual": "hidden",
               "inputs": {"scope": {"type": "string"}},
               "steps": [{"id": "s", "operation": "data.trim", "arguments": {"text": "owner-secret-42"}}],
               "reagents": {"data": {"select": ["trim"]}},
               "return": {"scope": "${steps.s.output}"}}),
        false,
    );
    let a = agent(&h, "alpha", &["guarded"]);
    (h, a)
}

fn drill_case(inputs: Value) -> Value {
    json!({"drill": "d", "ritual": "guarded",
           "cases": [{"id": "c1", "inputs": inputs, "expect": {"outcome": "returned"}}]})
}

#[test]
fn a_familiar_drill_is_its_own_run_and_keeps_user_only_inputs() {
    let (h, a) = drill_home();
    let e = signed(&h, &a, "drill_inline", json!({"ritual": "guarded", "cases": drill_case(json!({"scope": "private"}))}));
    let msg = format!("{e:?}");
    assert!(msg.contains("only the user can change"), "{msg}");
    let ok = signed(&h, &a, "drill_inline", json!({"ritual": "guarded", "cases": drill_case(json!({}))})).unwrap();
    assert_eq!(ok["passed"], 1, "{ok}");
    // The drill's run is the familiar's, not the owner's.
    let owner: Option<String> = h.core.db.lock().unwrap()
        .query_row("SELECT familiar FROM summons ORDER BY rowid DESC LIMIT 1", [], |r| r.get(0)).unwrap();
    assert_eq!(owner.as_deref(), Some("familiar:alpha"));
}

#[test]
fn a_familiar_drill_cannot_pick_a_version_or_a_hidden_touchstone() {
    let (h, a) = drill_home();
    let e = signed(&h, &a, "drill_inline", json!({"ritual": "guarded", "version": 1, "cases": drill_case(json!({}))})).unwrap_err();
    assert_eq!(e.code, "not_authorized", "{}", e.message);
    let mut cases = drill_case(json!({}));
    cases["touchstone"] = json!("hidden");
    cases["cases"][0]["expect"] = json!({"outcome": "returned"});
    let e = signed(&h, &a, "drill_inline", json!({"ritual": "guarded", "cases": cases})).unwrap_err();
    assert_eq!(e.code, "not_found", "{}", e.message);
    assert!(!e.message.contains("owner-secret"), "{}", e.message);
}

#[test]
fn a_familiar_compile_sees_only_its_allowed_rituals() {
    let (h, a) = drill_home();
    let calling = |name: &str| {
        json!({"definition": {"ritual": "probe",
            "steps": [{"id": "x", "ritual": name, "arguments": {"scope": "a"}}],
            "return": {"v": "${steps.x.output}"}}})
    };
    let hidden = signed(&h, &a, "compile", calling("hidden")).unwrap_err();
    let missing = signed(&h, &a, "compile", calling("nosuch")).unwrap_err();
    assert_eq!(
        hidden.message.replace("hidden", "NAME"),
        missing.message.replace("nosuch", "NAME"),
        "a hidden ritual answers like a missing one"
    );
    // The owner still compiles against the whole grimoire.
    assert!(h.call("compile", calling("hidden")).is_ok());
}

#[test]
fn a_refusal_never_claims_to_answer_an_unverified_request() {
    // A replayed copy of a frame that ran is refused. The refusal must not be
    // bound to that frame's nonce, or anyone on the path could hand the
    // sender an authentic "not authenticated" for a request that ran.
    let h = home();
    let a = agent(&h, "alpha", &["total-receipt"]);
    let nonce = new_nonce();
    let frame = seal_request(&a.key, &h.core.key.public().id(), "rituals", &json!({}), now_ms(), &nonce);
    assert!(open_reply(&h.core.handle_signed(&frame), &h.core.key.public(), &nonce).unwrap().is_ok());
    let refusal = h.core.handle_signed(&frame);
    let payload: Value = serde_json::from_slice(
        &base64::Engine::decode(&base64::engine::general_purpose::STANDARD, refusal["signed"]["payload"].as_str().unwrap()).unwrap(),
    )
    .unwrap();
    assert_ne!(payload["in_reply_to"], json!(nonce), "{payload}");
    let e = open_reply(&refusal, &h.core.key.public(), &nonce).unwrap().unwrap_err();
    assert_eq!(e.code, "not_authenticated");
    assert!(e.message.contains("cannot say"), "{}", e.message);
}
