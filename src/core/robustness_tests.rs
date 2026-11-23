use super::*;
fn paths() -> Paths {
    let root = std::env::temp_dir().join(new_id("summonr-core-test"));
    Paths {
        runtime: root.join("run"),
        state: root.join("state"),
        config: root.join("config"),
    }
}
fn install(core: &Core) {
    let def: Ritual = serde_json::from_value(json!({"ritual":"test","model":"mock/keyword","steps":[{"id":"answer","model":{"prompt":"receipt"}}],"return":{"answer":"${steps.answer.output}"}})).unwrap();
    core.db
        .lock()
        .unwrap()
        .execute(
            "INSERT INTO rituals VALUES ('test',1,?1,?2,0)",
            params![
                ritual::content_hash(&def),
                serde_json::to_string(&def).unwrap()
            ],
        )
        .unwrap();
}
fn install_job_ritual(c: &Core, version: u32) {
    let mut def: Ritual =
        serde_yaml::from_str(include_str!("../../tests/fixtures/rituals/total-receipt.yaml")).unwrap();
    if version == 2 {
        def.ret.insert("total_minor".into(), json!(999));
    }
    c.db.lock()
        .unwrap()
        .execute(
            "INSERT INTO rituals(name,version,hash,def,created) VALUES (?1,?2,?3,?4,0)",
            params![
                def.ritual,
                version,
                ritual::content_hash(&def),
                serde_json::to_string(&def).unwrap()
            ],
        )
        .unwrap();
}
#[test]
fn concurrent_job_workers_execute_pinned_ritual_and_account_for_every_item() {
    let p = paths();
    let c = Core::open(p.clone()).unwrap();
    install_job_ritual(&c, 1);
    let items:Vec<_>=(0..16).map(|i|json!({"id":format!("receipt-{i}"),"version":"snapshot-1","args":{"text":"{\"amounts_minor\":[100,25]}"}})).collect();
    c.dispatch(
        "job_create",
        json!({"job":"receipts","ritual":"total-receipt","items":items}),
    )
    .unwrap();
    install_job_ritual(&c, 2);
    let barrier = Arc::new(std::sync::Barrier::new(3));
    let handles: Vec<_> = (0..2)
        .map(|_| {
            let c = c.clone();
            let barrier = barrier.clone();
            std::thread::spawn(move || {
                barrier.wait();
                c.dispatch(
                    "job_work",
                    json!({"job":"receipts","count":8,"lease_ms":300000}),
                )
                .unwrap()
            })
        })
        .collect();
    barrier.wait();
    for h in handles {
        let v = h.join().unwrap();
        assert_eq!(v["attempted"], 8);
        for item in v["items"].as_array().unwrap() {
            assert_eq!(item["committed"], true);
            assert_eq!(item["record"]["result"]["total_minor"], 125);
        }
    }
    let status = c.dispatch("job_status", json!({"job":"receipts"})).unwrap();
    assert_eq!(status["counts"]["done"], 16);
    let db = c.db.lock().unwrap();
    let (n, requests): (i64, i64) = db
        .query_row("SELECT COUNT(*),SUM(requests) FROM summons", [], |r| {
            Ok((r.get(0)?, r.get(1)?))
        })
        .unwrap();
    assert_eq!((n, requests), (16, 0));
    let attempts: i64 = db
        .query_row(
            "SELECT COUNT(*) FROM job_attempts WHERE summon IS NOT NULL AND state='done'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(attempts, 16);
    drop(db);
    assert!(c
        .dispatch(
            "job_create",
            json!({"job":"receipts","ritual":"total-receipt","items":items})
        )
        .is_err());
    assert!(c
        .dispatch(
            "job_work",
            json!({"job":"receipts","count":1,"owner":"fake"})
        )
        .is_err());
    drop(c);
    std::fs::remove_dir_all(p.state.parent().unwrap()).unwrap();
}
#[test]
fn jobs_reject_writes_and_unbounded_inference() {
    let p = paths();
    let c = Core::open(p.clone()).unwrap();
    install(&c);
    assert!(c
        .dispatch(
            "job_create",
            json!({"job":"bad","ritual":"test","items":[{"id":"1","version":"1","args":{}}]})
        )
        .is_err());
    assert!(reagent::check_job_effect(true, "idempotent write").is_err());
    assert!(reagent::check_job_effect(true, "read").is_ok());
    assert!(reagent::check_job_effect(true, "pure").is_ok());
    drop(c);
    std::fs::remove_dir_all(p.state.parent().unwrap()).unwrap();
}

#[test]
fn second_core_cannot_recover_live_runs() {
    let p = paths();
    let c = Core::open(p.clone()).unwrap();
    c.insert_summon(
        "live",
        None,
        "r",
        1,
        "h",
        None,
        &json!({}),
        &Essence::default(),
        None,
    );
    assert!(Core::open(p.clone()).is_err());
    assert_eq!(c.summon("live").unwrap()["status"], "running");
    drop(c);
    let recovered = Core::open(p.clone()).unwrap();
    assert_eq!(recovered.summon("live").unwrap()["outcome"], "interrupted");
    drop(recovered);
    std::fs::remove_dir_all(p.state.parent().unwrap()).unwrap();
}
#[test]
fn storage_failure_blocks_new_execution() {
    let p = paths();
    let c = Core::open(p.clone()).unwrap();
    install(&c);
    c.db.lock().unwrap().execute_batch("CREATE TRIGGER no_events BEFORE INSERT ON events BEGIN SELECT RAISE(ABORT,'injected disk failure'); END;").unwrap();
    assert!(c.invoke(json!({"target":"test"})).is_err());
    assert!(c.storage_health().is_err());
    let n: i64 =
        c.db.lock()
            .unwrap()
            .query_row("SELECT COUNT(*) FROM events WHERE kind='model'", [], |r| {
                r.get(0)
            })
            .unwrap();
    assert_eq!(n, 0);
    drop(c);
    std::fs::remove_dir_all(p.state.parent().unwrap()).unwrap();
}
#[test]
fn terminal_event_and_state_commit_together() {
    let p = paths();
    let c = Core::open(p.clone()).unwrap();
    c.insert_summon(
        "live",
        None,
        "r",
        1,
        "h",
        None,
        &json!({}),
        &Essence::default(),
        None,
    );
    c.db.lock().unwrap().execute_batch("CREATE TRIGGER no_finish BEFORE UPDATE ON summons BEGIN SELECT RAISE(ABORT,'injected failure'); END;").unwrap();
    c.finish_summon(
        "live",
        &Finished {
            outcome: Outcome::Returned,
            result: json!({}),
            error: None,
        },
        json!({}),
    );
    assert!(c.storage_health().is_err());
    assert_eq!(c.summon("live").unwrap()["status"], "running");
    let n: i64 =
        c.db.lock()
            .unwrap()
            .query_row(
                "SELECT COUNT(*) FROM events WHERE kind='summon.end'",
                [],
                |r| r.get(0),
            )
            .unwrap();
    assert_eq!(n, 0);
    drop(c);
    std::fs::remove_dir_all(p.state.parent().unwrap()).unwrap();
}
#[test]
fn idempotent_submission_reuses_run_and_rejects_changed_inputs() {
    let p = paths();
    let c = Core::open(p.clone()).unwrap();
    install(&c);
    let request = json!({"target":"test","idempotency_key":"task-1"});
    let first = c.invoke(request.clone()).unwrap();
    let again = c.invoke(request.clone()).unwrap();
    assert_eq!(first["summon"], again["summon"]);
    assert_eq!(first["warnings"], again["warnings"]);
    assert_eq!(again["reused"], true);
    assert!(c
        .invoke(json!({"target":"test","idempotency_key":"task-1","args":{"new":1}}))
        .is_err());
    let id = first["summon"].as_str().unwrap();
    c.wait(json!({"summon":id,"timeout_ms":2000})).unwrap();
    drop(c);
    let c = Core::open(p.clone()).unwrap();
    assert_eq!(c.invoke(request).unwrap()["summon"], id);
    // Repeated replay invocations also preserve the initial warnings from summon.start.
    let c_path = p.state.join("empty-cassette.json");
    std::fs::write(&c_path, r#"{"cassette_version":1,"entries":[]}"#).unwrap();
    let rep_req = json!({"target":"test","idempotency_key":"task-replay","replay":c_path.to_str().unwrap()});
    let r1 = c.invoke(rep_req.clone()).unwrap();
    let r2 = c.invoke(rep_req).unwrap();
    assert_eq!(r1["warnings"], json!(["drift not checked during replay"]));
    assert_eq!(r2["warnings"], r1["warnings"]);
    assert_eq!(r2["reused"], true);
    drop(c);
    std::fs::remove_dir_all(p.state.parent().unwrap()).unwrap();
}
#[test]
fn duplicate_petitions_do_not_execute_and_claims_recover_unknown() {
    let p = paths();
    let c = Core::open(p.clone()).unwrap();
    let id = c.propose(
        "s",
        "r",
        "move",
        "mail.move",
        "write",
        &json!({}),
        None,
        &[],
        &json!({}),
        &[],
        None,
        None,
    );
    assert!(c
        .petition_decide(json!({"ids":[id,id],"permit":true}))
        .is_err());
    assert_eq!(
        c.dispatch("petitions", json!({}))
            .unwrap()
            .as_array()
            .unwrap()
            .len(),
        1
    );
    c.db.lock()
        .unwrap()
        .execute("UPDATE petitions SET status='executing'", [])
        .unwrap();
    drop(c);
    let c = Core::open(p.clone()).unwrap();
    assert_eq!(
        c.dispatch("petitions", json!({"status":"unknown"}))
            .unwrap()
            .as_array()
            .unwrap()
            .len(),
        1
    );
    assert!(c
        .petition_decide(json!({"ids":[id],"permit":true}))
        .is_err());
    drop(c);
    std::fs::remove_dir_all(p.state.parent().unwrap()).unwrap();
}
#[test]
fn concurrent_decisions_claim_once() {
    let p = paths();
    let c = Core::open(p.clone()).unwrap();
    let id = c.propose(
        "s",
        "r",
        "move",
        "mail.move",
        "write",
        &json!({}),
        None,
        &[],
        &json!({}),
        &[],
        None,
        None,
    );
    let barrier = Arc::new(std::sync::Barrier::new(3));
    let handles: Vec<_> = (0..2)
        .map(|_| {
            let c = c.clone();
            let id = id.clone();
            let b = barrier.clone();
            std::thread::spawn(move || {
                b.wait();
                c.petition_decide(json!({"ids":[id],"permit":false}))
                    .is_ok()
            })
        })
        .collect();
    barrier.wait();
    assert_eq!(
        handles
            .into_iter()
            .filter_map(|h| h.join().ok())
            .filter(|x| *x)
            .count(),
        1
    );
    drop(c);
    std::fs::remove_dir_all(p.state.parent().unwrap()).unwrap();
}
#[test]
fn caller_hash_and_limits_are_checked_without_inference() {
    let p = paths();
    let c = Core::open(p.clone()).unwrap();
    install(&c);
    assert!(c
        .resolve(&serde_json::from_value(json!({"target":"test","expected_hash":"wrong"})).unwrap())
        .is_err());
    let r = c
        .resolve(
            &serde_json::from_value(
                json!({"target":"test","limits":{"requests":0,"tokens":10,"time":"1s"}}),
            )
            .unwrap(),
        )
        .unwrap();
    assert_eq!(r.def.essence.requests, Some(0));
    assert_eq!(r.def.essence.tokens, Some(10));
    drop(c);
    std::fs::remove_dir_all(p.state.parent().unwrap()).unwrap();
}

#[test]
fn malformed_rpc_fields_never_default_into_actions() {
    let p = paths();
    let c = Core::open(p.clone()).unwrap();
    let bad = vec![
        ("invoke", json!({"targte":"test"})),
        ("invoke", json!({"target":"test","typo":1})),
        ("rehearse", json!({})),
        ("get", json!({"summon":""})),
        ("cancel", json!({"summon":2})),
        ("wait", json!({"summon":"s","timeout_ms":-1})),
        ("wait", json!({"summon":"s","timeout_ms":u64::MAX})),
        ("events", json!({"summon":"s","after":-1})),
        ("events", json!({"summon":"s","limit":-1})),
        ("list", json!({"scope":"actve"})),
        ("list", json!({"scope":"children"})),
        ("list", json!({"limit":10001})),
        ("inscribe", json!({"path":"file","replcae":true})),
        ("ritual_read", json!({"name":"x","version":4294967296u64})),
        (
            "grant",
            json!({"ritual":"x","scopes":[{"reagent":"mail","all":true,"select":["read"]}]}),
        ),
        (
            "revoke",
            json!({"ritual":"x","scopes":[{"reagent":"mail","al":true}]}),
        ),
        ("assay", json!({"ritual":"x","basline":true})),
        ("drill", json!({"ritual":"x","agaisnt":"d.yaml"})),
        ("drill", json!({"ritual":"x"})),
        ("petitions", json!({"status":"pendng"})),
        ("petitions", json!({"summon":""})),
        ("decide", json!({"permit":true,"all":true,"ritual":""})),
        ("petition_decide", json!({"ids":["p"]})),
        (
            "petition_decide",
            json!({"all":true,"ids":["p"],"permit":false}),
        ),
        ("glean", json!({"ritual":"x","count":0})),
        ("contract", json!({"namee":"x"})),
        ("job_work", json!({"job":"j","owner":"fake"})),
        ("job_status", json!({"job":"j","offset":-1})),
        ("job_item", json!({"job":"j"})),
        ("job_retry", json!({"job":"j","item":true})),
        ("ping", json!({"unknown":1})),
        ("tools", json!({"unknown":1})),
        ("rituals", json!({"unknown":1})),
        ("shutdown", json!({"force":"yes"})),
    ];
    for (method, params) in bad {
        let e = c.dispatch(method, params).unwrap_err();
        assert_eq!(e.exit, 2, "{method}: {e:?}");
    }
    assert!(!c.stop.load(Ordering::SeqCst));
    assert_eq!(
        c.db.lock()
            .unwrap()
            .query_row("SELECT COUNT(*) FROM summons", [], |r| r.get::<_, i64>(0))
            .unwrap(),
        0
    );
    drop(c);
    std::fs::remove_dir_all(p.state.parent().unwrap()).unwrap();
}

#[test]
fn corrupt_storage_is_an_error_not_an_empty_result() {
    let p = paths();
    let c = Core::open(p.clone()).unwrap();
    install(&c);
    c.db.lock()
        .unwrap()
        .execute("UPDATE rituals SET def='not json'", [])
        .unwrap();
    assert_eq!(
        c.dispatch("ritual_read", json!({"name":"test"}))
            .unwrap_err()
            .code,
        "storage_failed"
    );
    c.db.lock()
        .unwrap()
        .execute("DROP TABLE summons", [])
        .unwrap();
    assert_eq!(
        c.dispatch("list", json!({})).unwrap_err().code,
        "storage_failed"
    );
    assert_eq!(
        c.dispatch("ping", json!({})).unwrap_err().code,
        "storage_failed"
    );
    drop(c);
    std::fs::remove_dir_all(p.state.parent().unwrap()).unwrap();
}

#[test]
fn cancellation_is_visible_until_terminal_outcome() {
    let p = paths();
    let c = Core::open(p.clone()).unwrap();
    c.insert_summon(
        "waiting",
        None,
        "r",
        1,
        "h",
        None,
        &json!({}),
        &Essence::default(),
        None,
    );
    c.active
        .lock()
        .unwrap()
        .insert("waiting".into(), Arc::new(AtomicBool::new(false)));
    assert_eq!(
        c.summon("waiting").unwrap()["cancellation_requested"],
        false
    );
    c.dispatch("cancel", json!({"summon":"waiting"})).unwrap();
    let record = c.summon("waiting").unwrap();
    assert_eq!(record["cancellation_requested"], true);
    assert_ne!(record["status"], "ended");
    c.db.lock()
        .unwrap()
        .execute(
            "UPDATE summons SET status='ended',outcome='cancelled' WHERE id='waiting'",
            [],
        )
        .unwrap();
    assert_eq!(
        c.summon("waiting").unwrap()["cancellation_requested"],
        false
    );
    drop(c);
    std::fs::remove_dir_all(p.state.parent().unwrap()).unwrap();
}

struct CannedChat;
impl crate::io::Io for CannedChat {
    fn chat(
        &self,
        _base_url: &str,
        _model: &str,
        _step: &str,
        _req: backend::ChatReq,
    ) -> Result<backend::ChatResp, String> {
        Ok(backend::ChatResp {
            content: "hello".into(),
            done_reason: None,
            prompt_tokens: 4,
            output_tokens: 2,
            load_ms: 0,
            prompt_ms: 0,
            eval_ms: 0,
            total_ms: 0,
            payload: json!({"canned": true}),
            constrained: false,
            token_confidence: "exact",
            provider_cost_microusd: None,
        })
    }
    fn call(
        &self,
        _root: Option<&str>,
        _operation: &str,
        _args: &Value,
        _auth: &reagent::Authority,
    ) -> Result<Value, reagent::Fail> {
        Err(reagent::Fail::Failed("unused".into()))
    }
}

#[test]
fn replaying_an_ollama_model_run_makes_no_backend_requests() {
    let p = paths();
    std::fs::create_dir_all(&p.config).unwrap();
    // A listener that counts connections stands in for the model backend.
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let hits = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let counter = hits.clone();
    std::thread::spawn(move || {
        // Answer every request with an empty JSON body and count it.
        for stream in listener.incoming().flatten() {
            use std::io::{Read, Write};
            let mut stream = stream;
            counter.fetch_add(1, Ordering::SeqCst);
            let mut buf = [0u8; 4096];
            let _ = stream.read(&mut buf);
            let _ = stream.write_all(
                b"HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: 2\r\nConnection: close\r\n\r\n{}",
            );
        }
    });
    std::fs::write(
        p.config.join("config.yaml"),
        format!("ollama_url: http://127.0.0.1:{port}\n"),
    )
    .unwrap();
    let c = Core::open(p.clone()).unwrap();
    let def: Ritual = serde_json::from_value(json!({"ritual":"talk","model":"ollama/qwen3:4b","steps":[{"id":"say","model":{"prompt":"hi"}}],"return":{"said":"${steps.say.output}"}})).unwrap();
    c.db.lock()
        .unwrap()
        .execute(
            "INSERT INTO rituals VALUES ('talk',1,?1,?2,0)",
            params![
                ritual::content_hash(&def),
                serde_json::to_string(&def).unwrap()
            ],
        )
        .unwrap();
    let cassette = p.config.join("talk.json");
    // Record against a canned Io (this live-style run may touch the listener).
    let spec: Spec = decode(json!({"target":"talk"})).unwrap();
    let rec = crate::io::Recording::new(Arc::new(CannedChat), cassette.clone()).unwrap();
    let r = c.resolve(&spec).unwrap();
    let (_, run, _) = c.make_run(r, None, None, Some(Arc::new(rec)));
    let fin = c.complete(run, false);
    c.db.lock()
        .unwrap()
        .execute(
            "INSERT INTO baselines (ritual, version, model, set_name, set_hash, set_json, fingerprint, account, created) VALUES ('talk',1,'ollama/qwen3:4b','s','h','{}','\"fp\"','{}',0)",
            [],
        )
        .unwrap();
    assert_eq!(fin.result["said"], "hello", "{:?}", fin.error);
    let before = hits.load(Ordering::SeqCst);
    assert!(before > 0, "the recording run should have unloaded the model");

    let started = c
        .dispatch(
            "invoke",
            json!({"target":"talk","replay":cassette.to_str().unwrap()}),
        )
        .unwrap();
    assert!(started["warnings"]
        .as_array()
        .unwrap()
        .iter()
        .any(|w| w == "drift not checked during replay"));
    let done = c
        .dispatch("wait", json!({"summon": started["summon"], "timeout_ms": 10000}))
        .unwrap();
    assert_eq!(done["result"]["said"], "hello", "{done}");
    assert_eq!(
        done["cleanup"][0]["result"], "not applicable (replay)",
        "{done}"
    );
    std::thread::sleep(Duration::from_millis(300));
    assert_eq!(
        hits.load(Ordering::SeqCst),
        before,
        "replay opened a connection to the model backend"
    );
    drop(c);
    std::fs::remove_dir_all(p.state.parent().unwrap()).unwrap();
}

#[test]
fn core_key_is_stable_across_reopen() {
    let p = paths();
    let c = Core::open(p.clone()).unwrap();
    let first = c.dispatch("identity", json!({})).unwrap();
    drop(c);
    let c = Core::open(p.clone()).unwrap();
    let again = c.dispatch("identity", json!({})).unwrap();
    assert_eq!(first, again);
    assert!(first["key"].as_str().unwrap().starts_with("ed25519:"), "{first}");
    assert_eq!(first["id"].as_str().unwrap().len(), 64);
    assert!(first["certified_by"].is_null());
    assert!(!first.to_string().contains("PRIVATE"), "identity shows only public data");
    use std::os::unix::fs::PermissionsExt;
    let mode = std::fs::metadata(p.config.join("core.key")).unwrap().permissions().mode() & 0o777;
    assert_eq!(mode, 0o600);
    assert_eq!(std::fs::read_to_string(p.config.join("core.pub")).unwrap().trim(), first["key"]);
    drop(c);
    std::fs::remove_dir_all(p.state.parent().unwrap()).unwrap();
}

#[test]
fn principals_from_an_earlier_build_become_familiars() {
    let p = paths();
    let c = Core::open(p.clone()).unwrap();
    drop(c);
    // What earlier builds left: a principals table, a principal column and
    // its key folder.
    {
        let db = rusqlite::Connection::open(p.db()).unwrap();
        db.execute_batch(
            "CREATE TABLE IF NOT EXISTS principals (name TEXT PRIMARY KEY, kind TEXT NOT NULL, key TEXT NOT NULL, key_id TEXT NOT NULL UNIQUE, created INTEGER NOT NULL, revoked INTEGER, allowed TEXT NOT NULL DEFAULT '[]');
             INSERT INTO principals VALUES ('old', 'remote', 'ed25519:x', 'kid', 1, NULL, '[]');
             ALTER TABLE summons ADD COLUMN principal TEXT;
             INSERT INTO summons (id, ritual, version, hash, status, started, principal) VALUES ('s-old', 'r', 1, 'h', 'ended', 1, 'principal:old');",
        )
        .unwrap();
        std::fs::create_dir_all(p.config.join("principals")).unwrap();
        std::fs::write(p.config.join("principals/old.key"), "k").unwrap();
    }
    let c = Core::open(p.clone()).unwrap();
    let list = c.dispatch("familiar_list", json!({})).unwrap();
    assert_eq!(list[0]["name"], "old", "{list}");
    assert_eq!(c.owner_of("s-old"), Some(Some("familiar:old".to_string())));
    assert!(p.config.join("familiars/old.key").exists());
    assert!(!p.config.join("principals").exists());
    drop(c);
    std::fs::remove_dir_all(p.state.parent().unwrap()).unwrap();
}

#[test]
fn patrons_from_an_earlier_build_become_familiars() {
    let p = paths();
    drop(Core::open(p.clone()).unwrap());
    {
        let db = rusqlite::Connection::open(p.db()).unwrap();
        db.execute_batch(
            "CREATE TABLE IF NOT EXISTS patrons (name TEXT PRIMARY KEY, kind TEXT NOT NULL, key TEXT NOT NULL, key_id TEXT NOT NULL UNIQUE, created INTEGER NOT NULL, revoked INTEGER, allowed TEXT NOT NULL DEFAULT '[]');
             INSERT INTO patrons VALUES ('mid', 'local', 'ed25519:y', 'kid2', 2, NULL, '[\"r\"]');
             ALTER TABLE summons ADD COLUMN patron TEXT;
             INSERT INTO summons (id, ritual, version, hash, status, started, patron) VALUES ('s-mid', 'r', 1, 'h', 'ended', 1, 'patron:mid');",
        )
        .unwrap();
        std::fs::create_dir_all(p.config.join("patrons")).unwrap();
        std::fs::write(p.config.join("patrons/mid.key"), "k").unwrap();
    }
    let c = Core::open(p.clone()).unwrap();
    let list = c.dispatch("familiar_list", json!({})).unwrap();
    assert_eq!(list[0]["name"], "mid", "{list}");
    assert_eq!(list[0]["allowed"], json!(["r"]));
    assert_eq!(c.owner_of("s-mid"), Some(Some("familiar:mid".to_string())));
    assert!(p.config.join("familiars/mid.key").exists());
    assert!(!p.config.join("patrons").exists());
    drop(c);
    std::fs::remove_dir_all(p.state.parent().unwrap()).unwrap();
}

#[test]
fn revive_resumes_exhausted_batch_from_checkpoint_without_re_running_completed_items() {
    let p = paths();
    let c = Core::open(p.clone()).unwrap();
    let def_path = p.state.parent().unwrap().join("batch-inc.json");
    std::fs::write(
        &def_path,
        json!({
            "ritual": "batch-inc",
            "reagents": {"data": {"select": ["arithmetic"]}},
            "essence": {"requests": 0, "tool_calls": 2, "time": "10s"},
            "steps": [{
                "id": "batch",
                "each": "x in [10, 20, 30]",
                "steps": [{
                    "id": "inc",
                    "operation": "data.arithmetic",
                    "arguments": {"left": "${x}", "right": 1, "operator": "add"}
                }]
            }],
            "return": {"items": "${steps.batch.output}"}
        })
        .to_string(),
    )
    .unwrap();
    c.dispatch(
        "inscribe",
        json!({
            "path": def_path.to_string_lossy()
        }),
    )
    .unwrap();

    let wait_done = |id: &str| {
        for _ in 0..200 {
            let g = c.dispatch("get", json!({"summon": id})).unwrap();
            if g["status"] == "ended" {
                return g;
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        panic!("summon {id} did not finish");
    };
    let first = c.dispatch("invoke", json!({"target": "batch-inc"})).unwrap();
    let first_id = first["summon"].as_str().unwrap().to_string();
    let first_done = wait_done(&first_id);
    assert_eq!(first_done["outcome"], "exhausted", "{first_done}");
    assert_eq!(
        first_done["result"]["partial"]["batch"]["counts"]["returned"], 2,
        "{first_done}"
    );

    // Reviving resumes from the 2-item checkpoint, so only item 30 needs 1 tool call.
    let revived = c.dispatch("revive", json!({"summon": first_id})).unwrap();
    let revived_id = revived["summon"].as_str().unwrap().to_string();
    assert_eq!(revived["revived_from"], first_id);
    let done = wait_done(&revived_id);
    assert_eq!(done["outcome"], "returned", "{done}");
    let items = done["result"]["items"].as_array().unwrap();
    assert_eq!(items.len(), 3, "{done}");
    assert_eq!(items[0]["outputs"]["inc"], json!(11));
    assert_eq!(items[1]["outputs"]["inc"], json!(21));
    assert_eq!(items[2]["outputs"]["inc"], json!(31));

    // A returned summon cannot be revived.
    let err = c
        .dispatch("revive", json!({"summon": revived_id}))
        .unwrap_err();
    assert_eq!(err.code, "state_conflict", "{err:?}");

    let verified = c.dispatch("chain_verify", json!({})).unwrap();
    assert_eq!(verified["ok"], true, "{verified}");

    drop(c);
    std::fs::remove_dir_all(p.state.parent().unwrap()).unwrap();
}

#[test]
fn recast_branches_from_earlier_step_reusing_prior_checkpoints() {
    let p = paths();
    let c = Core::open(p.clone()).unwrap();
    let def_path = p.state.parent().unwrap().join("two-step.json");
    std::fs::write(
        &def_path,
        json!({
            "language_version": 1,
            "ritual": "two-step",
            "reagents": {"data": {"select": ["arithmetic"]}},
            "essence": {"requests": 0, "tool_calls": 2, "time": "10s"},
            "steps": [
                {
                    "id": "first",
                    "operation": "data.arithmetic",
                    "arguments": {"left": 10, "right": 5, "operator": "add"}
                },
                {
                    "id": "second",
                    "operation": "data.arithmetic",
                    "arguments": {"left": "${steps.first.output}", "right": 3, "operator": "multiply"}
                }
            ],
            "return": {"first": "${steps.first.output}", "second": "${steps.second.output}"}
        })
        .to_string(),
    )
    .unwrap();
    c.dispatch("inscribe", json!({"path": def_path.to_string_lossy()}))
        .unwrap();

    let wait_done = |id: &str| {
        for _ in 0..200 {
            let g = c.dispatch("get", json!({"summon": id})).unwrap();
            if g["status"] == "ended" {
                return g;
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        panic!("summon {id} did not finish");
    };

    // Steps that ran in a summon (pure steps are in its log, not its effects).
    let ops_run = |id: &str| {
        let events = c.dispatch("events", json!({"summon": id})).unwrap();
        events.as_array().unwrap().iter().filter(|e| e["kind"] == "op").count()
    };
    let run1 = c.dispatch("invoke", json!({"target": "two-step"})).unwrap();
    let id1 = run1["summon"].as_str().unwrap().to_string();
    let done1 = wait_done(&id1);
    assert_eq!(done1["outcome"], "returned", "{done1}");
    assert_eq!(done1["result"]["second"], json!(45));
    assert_eq!(ops_run(&id1), 2, "{done1}");

    // Recast from `second`: keeps `first` checkpoint, only executes `second` (1 tool call).
    let recast = c
        .dispatch("recast", json!({"summon": id1, "from_step": "second"}))
        .unwrap();
    let id2 = recast["summon"].as_str().unwrap().to_string();
    assert_eq!(recast["recast_from"], id1);
    assert_eq!(recast["from_step"], "second");
    let done2 = wait_done(&id2);
    assert_eq!(done2["outcome"], "returned", "{done2}");
    assert_eq!(done2["result"]["first"], json!(15));
    assert_eq!(done2["result"]["second"], json!(45));
    assert_eq!(ops_run(&id2), 1, "{done2}");

    // Invalid step name fails cleanly.
    let err = c
        .dispatch("recast", json!({"summon": id1, "from_step": "missing"}))
        .unwrap_err();
    assert_eq!(err.code, "invalid_invocation", "{err:?}");

    drop(c);
    std::fs::remove_dir_all(p.state.parent().unwrap()).unwrap();
}

#[test]
fn non_idempotent_write_is_fenced_on_retry_and_revive_while_recast_clears_fence() {
    let p = paths();
    let c = Core::open(p.clone()).unwrap();
    let root = p.state.parent().unwrap().join("workspace");
    std::fs::create_dir_all(&root).unwrap();
    let root_s = root.to_str().unwrap();
    let log_file = root.join("log.jsonl");
    std::fs::write(&log_file, "{\"n\":0}\n").unwrap();

    let r_path = p.config.join("append-fenced.yaml");
    std::fs::write(
        &r_path,
        format!(
            r#"
ritual: append-fenced
requires:
  - reagent: file
    action: append_jsonl
    on: {{root: "{root_s}"}}
  - reagent: file
    action: read_text
    on: {{root: "{root_s}"}}
essence: {{requests: 0, tool_calls: 10}}
steps:
  - id: append
    operation: file.append_jsonl
    retry: 1
    arguments: {{root: "{root_s}", path: "log.jsonl", record: {{n: 1}}}}
    verify:
      observe: file.read_text
      arguments: {{root: "{root_s}", path: "log.jsonl"}}
      expect: {{text: "never-matches"}}
return: {{out: "${{steps.append.output}}"}}
"#
        ),
    )
    .unwrap();
    c.dispatch("inscribe", json!({"path": r_path.to_str().unwrap()}))
        .unwrap();
    c.dispatch(
        "grant",
        json!({"ritual": "append-fenced", "scopes": [{"reagent": "file", "select": ["append_jsonl", "read_text"]}], "on": {"root": root_s}}),
    )
    .unwrap();

    let id1 = c
        .dispatch("invoke", json!({"target": "append-fenced"}))
        .unwrap()["summon"]
        .as_str()
        .unwrap()
        .to_string();
    let done1 = c.dispatch("wait", json!({"summon": id1, "timeout_ms": 5000})).unwrap();
    assert_eq!(done1["outcome"], "failed", "retry must be blocked by fence: {done1}");
    assert!(done1["error"].as_str().unwrap().contains("is fenced"), "{done1}");
    assert_eq!(
        std::fs::read_to_string(&log_file).unwrap(),
        "{\"n\":0}\n{\"n\":1}\n",
        "non-idempotent write must execute at most once across retry"
    );
    let evs1 = c.dispatch("events", json!({"summon": id1})).unwrap();
    assert!(
        evs1.as_array().unwrap().iter().any(|e| e["kind"] == "op.fenced" && e["data"]["state"] == "blocked"),
        "expected op.fenced blocked event: {evs1}"
    );
    assert!(done1["fences"].as_array().is_some_and(|a| !a.is_empty()), "{done1}");

    // Simulate an interrupted run with a dispatched non-idempotent fence:
    // `revive` must block re-dispatch, while `recast` from `append` clears the fence.
    let r2_path = p.config.join("append-plain.yaml");
    std::fs::write(
        &r2_path,
        format!(
            r#"
ritual: append-plain
requires:
  - reagent: file
    action: append_jsonl
    on: {{root: "{root_s}"}}
essence: {{requests: 0, tool_calls: 4}}
steps:
  - id: append
    operation: file.append_jsonl
    arguments: {{root: "{root_s}", path: "log.jsonl", record: {{n: 2}}}}
return: {{out: "${{steps.append.output}}"}}
"#
        ),
    )
    .unwrap();
    c.dispatch("inscribe", json!({"path": r2_path.to_str().unwrap()}))
        .unwrap();
    c.dispatch(
        "grant",
        json!({"ritual": "append-plain", "scopes": [{"reagent": "file", "select": ["append_jsonl"]}], "on": {"root": root_s}}),
    )
    .unwrap();
    let id2 = "s-interrupted-fence";
    let h2 = c.dispatch("contract", json!({"name": "append-plain"})).unwrap()["hash"]
        .as_str()
        .unwrap()
        .to_string();
    let args2 = json!({"root": root_s, "path": "log.jsonl", "record": {"n": 2}});
    let args_hash = crate::util::canonical_sha256(&args2);
    let fk2 = Core::compute_fence_key(id2, "append", "file.append_jsonl", &args_hash);
    {
        let db = c.db.lock().unwrap();
        db.execute(
            "INSERT INTO summons (id, ritual, version, hash, inputs, status, outcome, started, ended) VALUES (?1,'append-plain',1,?2,'{}','ended','interrupted',1,2)",
            rusqlite::params![id2, h2],
        ).unwrap();
        db.execute(
            "INSERT INTO effect_fence (fence_key, summon, step, operation, effect, args_hash, state, updated) VALUES (?1,?2,'append','file.append_jsonl','write',?3,'dispatched',2)",
            rusqlite::params![fk2, id2, args_hash],
        ).unwrap();
    }

    let rev = c.dispatch("revive", json!({"summon": id2})).unwrap()["summon"]
        .as_str()
        .unwrap()
        .to_string();
    let rev_done = c.dispatch("wait", json!({"summon": rev, "timeout_ms": 5000})).unwrap();
    assert_eq!(rev_done["outcome"], "failed", "{rev_done}");
    assert!(rev_done["error"].as_str().unwrap().contains("is fenced"), "{rev_done}");
    assert_eq!(std::fs::read_to_string(&log_file).unwrap(), "{\"n\":0}\n{\"n\":1}\n");

    let rec = c.dispatch("recast", json!({"summon": id2, "from_step": "append"})).unwrap()["summon"]
        .as_str()
        .unwrap()
        .to_string();
    let rec_done = c.dispatch("wait", json!({"summon": rec, "timeout_ms": 5000})).unwrap();
    assert_eq!(rec_done["outcome"], "returned", "{rec_done}");
    assert_eq!(
        std::fs::read_to_string(&log_file).unwrap(),
        "{\"n\":0}\n{\"n\":1}\n{\"n\":2}\n"
    );

    drop(c);
    std::fs::remove_dir_all(p.state.parent().unwrap()).unwrap();
}

#[test]
fn idempotent_write_returns_cached_result_on_revive_and_allows_redispatch() {
    let p = paths();
    let c = Core::open(p.clone()).unwrap();
    let root = p.state.parent().unwrap().join("workspace");
    std::fs::create_dir_all(&root).unwrap();
    let root_s = root.to_str().unwrap();
    let out_file = root.join("out.txt");

    let r_path = p.config.join("idem-write.yaml");
    std::fs::write(
        &r_path,
        format!(
            r#"
ritual: idem-write
requires:
  - reagent: file
    action: write_text
    on: {{root: "{root_s}"}}
essence: {{requests: 0, tool_calls: 4}}
steps:
  - id: save
    operation: file.write_text
    arguments: {{root: "{root_s}", path: "out.txt", text: "hello"}}
return: {{out: "${{steps.save.output}}"}}
"#
        ),
    )
    .unwrap();
    c.dispatch("inscribe", json!({"path": r_path.to_str().unwrap()}))
        .unwrap();
    c.dispatch(
        "grant",
        json!({"ritual": "idem-write", "scopes": [{"reagent": "file", "select": ["write_text"]}], "on": {"root": root_s}}),
    )
    .unwrap();

    let h = c.dispatch("contract", json!({"name": "idem-write"})).unwrap()["hash"]
        .as_str()
        .unwrap()
        .to_string();
    let args = json!({"root": root_s, "path": "out.txt", "text": "hello"});
    let args_hash = crate::util::canonical_sha256(&args);

    // 1. Applied fence without step checkpoint -> revive returns cached output without overwriting disk.
    let id_applied = "s-idem-applied";
    let fk1 = Core::compute_fence_key(id_applied, "save", "file.write_text", &args_hash);
    std::fs::write(&out_file, "sentinel").unwrap();
    {
        let db = c.db.lock().unwrap();
        db.execute(
            "INSERT INTO summons (id, ritual, version, hash, inputs, status, outcome, started, ended) VALUES (?1,'idem-write',1,?2,'{}','ended','interrupted',1,2)",
            rusqlite::params![id_applied, h],
        ).unwrap();
        db.execute(
            "INSERT INTO effect_fence (fence_key, summon, step, operation, effect, args_hash, state, output, evidence, updated) VALUES (?1,?2,'save','file.write_text','idempotent write',?3,'applied',?4,?5,2)",
            rusqlite::params![
                fk1,
                id_applied,
                args_hash,
                json!({"path":"out.txt","bytes":5}).to_string(),
                serde_json::to_string(&crate::records::Evidence::Reported {
                    observed: json!({"path":"out.txt","bytes":5}),
                }).unwrap(),
            ],
        ).unwrap();
    }
    let rev1 = c.dispatch("revive", json!({"summon": id_applied})).unwrap()["summon"]
        .as_str()
        .unwrap()
        .to_string();
    let done1 = c.dispatch("wait", json!({"summon": rev1, "timeout_ms": 5000})).unwrap();
    assert_eq!(done1["outcome"], "returned", "{done1}");
    assert_eq!(done1["result"]["out"]["bytes"], 5);
    assert_eq!(
        std::fs::read_to_string(&out_file).unwrap(),
        "sentinel",
        "cached applied fence must not re-execute file.write_text"
    );

    // 2. Dispatched idempotent fence -> revive safely re-dispatches and writes.
    let id_disp = "s-idem-dispatched";
    let fk2 = Core::compute_fence_key(id_disp, "save", "file.write_text", &args_hash);
    {
        let db = c.db.lock().unwrap();
        db.execute(
            "INSERT INTO summons (id, ritual, version, hash, inputs, status, outcome, started, ended) VALUES (?1,'idem-write',1,?2,'{}','ended','interrupted',1,2)",
            rusqlite::params![id_disp, h],
        ).unwrap();
        db.execute(
            "INSERT INTO effect_fence (fence_key, summon, step, operation, effect, args_hash, state, updated) VALUES (?1,?2,'save','file.write_text','idempotent write',?3,'dispatched',2)",
            rusqlite::params![fk2, id_disp, args_hash],
        ).unwrap();
    }
    let rev2 = c.dispatch("revive", json!({"summon": id_disp})).unwrap()["summon"]
        .as_str()
        .unwrap()
        .to_string();
    let done2 = c.dispatch("wait", json!({"summon": rev2, "timeout_ms": 5000})).unwrap();
    assert_eq!(done2["outcome"], "returned", "{done2}");
    assert_eq!(std::fs::read_to_string(&out_file).unwrap(), "hello");

    drop(c);
    std::fs::remove_dir_all(p.state.parent().unwrap()).unwrap();
}

#[test]
fn petition_for_missing_authority_grants_when_permitted() {
    let p = paths();
    let c = Core::open(p.clone()).unwrap();
    let root = p.state.parent().unwrap().join("workspace");
    std::fs::create_dir_all(&root).unwrap();
    let root_s = root.to_str().unwrap();

    let r_path = p.config.join("auth-pet.yaml");
    std::fs::write(
        &r_path,
        format!(
            r#"
ritual: auth-pet
requires:
  - reagent: file
    action: write_text
    on: {{root: "{root_s}"}}
essence: {{requests: 0, tool_calls: 2}}
steps:
  - id: w
    operation: file.write_text
    arguments: {{root: "{root_s}", path: "hi.txt", text: "ok"}}
return: {{out: "${{steps.w.output}}"}}
"#
        ),
    )
    .unwrap();
    c.dispatch("inscribe", json!({"path": r_path.to_str().unwrap()}))
        .unwrap();

    // Invoking with petition_missing files an authority.grant petition and refuses.
    let err = c
        .dispatch("invoke", json!({"target": "auth-pet", "petition_missing": true}))
        .unwrap_err();
    assert_eq!(err.code, "not_authorized");
    let pids = err.data["petitions"].as_array().unwrap();
    assert_eq!(pids.len(), 1);
    let pid = pids[0].as_str().unwrap().to_string();

    // Permitting the authority petition grants the scope so invocation now succeeds.
    c.dispatch("petition_decide", json!({"permit": true, "ids": [pid]}))
        .unwrap();
    let id = c.dispatch("invoke", json!({"target": "auth-pet"})).unwrap()["summon"]
        .as_str()
        .unwrap()
        .to_string();
    let done = c.dispatch("wait", json!({"summon": id, "timeout_ms": 5000})).unwrap();
    assert_eq!(done["outcome"], "returned", "{done}");
    assert_eq!(std::fs::read_to_string(root.join("hi.txt")).unwrap(), "ok");

    drop(c);
    std::fs::remove_dir_all(p.state.parent().unwrap()).unwrap();
}

#[test]
fn operator_fence_reconciliation_and_petition_ttl_expiry() {
    let p = paths();
    std::fs::create_dir_all(&p.config).unwrap();
    std::fs::write(p.config.join("config.yaml"), "petition_ttl: \"10s\"\n").unwrap();
    let c = Core::open(p.clone()).unwrap();

    // 1. Fence list & reconcile (apply / clear)
    let fk = Core::compute_fence_key("s-op", "step1", "file.append_jsonl", "abc");
    {
        let db = c.db.lock().unwrap();
        db.execute(
            "INSERT INTO effect_fence (fence_key, summon, step, operation, effect, args_hash, state, updated) VALUES (?1,'s-op','step1','file.append_jsonl','write','abc','dispatched',1)",
            rusqlite::params![fk],
        ).unwrap();
    }
    let list = c.dispatch("fence_list", json!({"summon": "s-op"})).unwrap();
    assert_eq!(list.as_array().unwrap().len(), 1);
    assert_eq!(list[0]["state"], "dispatched");

    let applied = c.dispatch("fence_resolve", json!({"fence_key": fk, "action": "applied"})).unwrap();
    assert_eq!(applied["state"], "applied");
    let list2 = c.dispatch("fence_list", json!({"summon": "s-op"})).unwrap();
    assert_eq!(list2[0]["state"], "applied");

    let cleared = c.dispatch("fence_resolve", json!({"fence_key": fk, "action": "clear"})).unwrap();
    assert_eq!(cleared["state"], "cleared");
    let list3 = c.dispatch("fence_list", json!({"summon": "s-op"})).unwrap();
    assert_eq!(list3.as_array().unwrap().len(), 0);

    // 2. Petition TTL expiry marks overdue petitions expired on list
    let pid = c.propose(
        "s-ttl",
        "r-ttl",
        "step",
        "mail.move",
        "idempotent write",
        &json!({}),
        None,
        &[],
        &json!({}),
        &[],
        None,
        None,
    );
    {
        let db = c.db.lock().unwrap();
        db.execute("UPDATE petitions SET expires = 1 WHERE id=?1", rusqlite::params![pid]).unwrap();
    }
    let pending = c.dispatch("petitions", json!({"status": "pending"})).unwrap();
    assert!(pending.as_array().unwrap().is_empty(), "{pending}");
    let expired = c.dispatch("petitions", json!({"status": "expired"})).unwrap();
    assert_eq!(expired.as_array().unwrap().len(), 1);
    assert_eq!(expired[0]["id"], pid);

    drop(c);
    std::fs::remove_dir_all(p.state.parent().unwrap()).unwrap();
}

#[test]
fn event_subscription_streams_backlog_and_live_events() {
    let h = super::test_home::Home::new("");
    let c = h.core.clone();
    let p = c.paths.clone();
    c.emit("sub-1", "step", "backlog", json!({"n": 1}));
    let srv = c.clone();
    let handle = std::thread::spawn(move || srv.serve());
    for _ in 0..40 {
        if std::os::unix::net::UnixStream::connect(p.socket()).is_ok() {
            break;
        }
        std::thread::sleep(Duration::from_millis(20));
    }

    let rx = crate::rpc::Client::subscribe(&p, Some("sub-1"), 0, false).unwrap();
    let first = rx.recv_timeout(Duration::from_secs(2)).unwrap();
    assert_eq!(first["kind"], "step");
    assert_eq!(first["text"], "backlog");
    assert_eq!(first["data"]["n"], 1);

    c.emit("other", "step", "ignored", json!({}));
    c.emit("sub-1", "done", "live", json!({"n": 2}));
    let second = rx.recv_timeout(Duration::from_secs(2)).unwrap();
    assert_eq!(second["kind"], "done");
    assert_eq!(second["text"], "live");
    assert_eq!(second["data"]["n"], 2);

    drop(rx);
    c.stop.store(true, std::sync::atomic::Ordering::SeqCst);
    let _ = handle.join();
}

#[test]
fn revive_does_not_repeat_non_idempotent_writes_inside_child_rituals() {
    let p = paths();
    let c = Core::open(p.clone()).unwrap();
    let root = p.state.parent().unwrap().join("workspace");
    std::fs::create_dir_all(&root).unwrap();
    let root_s = root.to_str().unwrap();
    let log_file = root.join("child.jsonl");

    let kid_path = p.config.join("kid.yaml");
    std::fs::write(
        &kid_path,
        format!(
            r#"
ritual: kid
requires:
  - reagent: file
    action: append_jsonl
    on: {{root: "{root_s}"}}
essence: {{requests: 0, tool_calls: 4}}
steps:
  - id: first
    operation: file.append_jsonl
    arguments: {{root: "{root_s}", path: "child.jsonl", record: {{step: "first"}}}}
  - id: second
    operation: file.append_jsonl
    arguments: {{root: "{root_s}", path: "child.jsonl", record: {{step: "second"}}}}
return: {{done: true}}
"#
        ),
    )
    .unwrap();
    c.dispatch("inscribe", json!({"path": kid_path.to_str().unwrap()})).unwrap();

    let dad_path = p.config.join("dad.yaml");
    std::fs::write(
        &dad_path,
        format!(
            r#"
ritual: dad
requires:
  - reagent: file
    action: append_jsonl
    on: {{root: "{root_s}"}}
essence: {{requests: 0, tool_calls: 1}}
steps:
  - id: call
    ritual: kid
    arguments: {{}}
return: {{ok: true}}
"#
        ),
    )
    .unwrap();
    c.dispatch("inscribe", json!({"path": dad_path.to_str().unwrap()})).unwrap();
    c.dispatch(
        "grant",
        json!({"ritual": "dad", "scopes": [{"reagent": "file", "select": ["append_jsonl"]}], "on": {"root": root_s}}),
    )
    .unwrap();

    // First run exhausts tool_calls=1 after `kid`'s first append.
    let s1 = c.dispatch("invoke", json!({"target": "dad", "args": {}})).unwrap()["summon"]
        .as_str()
        .unwrap()
        .to_string();
    let d1 = c.dispatch("wait", json!({"summon": s1, "timeout_ms": 5000})).unwrap();
    assert_eq!(d1["outcome"], "exhausted", "{d1}");
    assert_eq!(std::fs::read_to_string(&log_file).unwrap(), "{\"step\":\"first\"}\n");

    // Revive runs again; `kid.first` must hit the copied child fence and NOT append twice.
    let s2 = c.dispatch("revive", json!({"summon": s1})).unwrap()["summon"]
        .as_str()
        .unwrap()
        .to_string();
    let d2 = c.dispatch("wait", json!({"summon": s2, "timeout_ms": 5000})).unwrap();
    assert_eq!(d2["outcome"], "exhausted", "{d2}");
    assert_eq!(
        std::fs::read_to_string(&log_file).unwrap(),
        "{\"step\":\"first\"}\n",
        "child write was repeated on revive"
    );

    drop(c);
    std::fs::remove_dir_all(p.state.parent().unwrap()).unwrap();
}

#[test]
fn identical_writes_in_each_loop_and_idless_steps_all_execute() {
    let p = paths();
    let c = Core::open(p.clone()).unwrap();
    let root = p.state.parent().unwrap().join("workspace");
    std::fs::create_dir_all(&root).unwrap();
    let root_s = root.to_str().unwrap();
    let log_file = root.join("ticks.jsonl");

    let r_path = p.config.join("ticks.yaml");
    std::fs::write(
        &r_path,
        format!(
            r#"
ritual: ticks
inputs:
  items: {{type: array, default: [1, 2, 3]}}
requires:
  - reagent: file
    action: append_jsonl
    on: {{root: "{root_s}"}}
essence: {{requests: 0, tool_calls: 10, items: 10}}
steps:
  - id: loop
    each: x in inputs.items
    steps:
      - id: w
        operation: file.append_jsonl
        arguments: {{root: "{root_s}", path: "ticks.jsonl", record: {{tick: 1}}}}
  - id: w1
    operation: file.append_jsonl
    arguments: {{root: "{root_s}", path: "ticks.jsonl", record: {{tick: 2}}}}
  - id: w2
    operation: file.append_jsonl
    arguments: {{root: "{root_s}", path: "ticks.jsonl", record: {{tick: 2}}}}
return: {{ok: true}}
"#
        ),
    )
    .unwrap();
    c.dispatch("inscribe", json!({"path": r_path.to_str().unwrap()})).unwrap();
    c.dispatch(
        "grant",
        json!({"ritual": "ticks", "scopes": [{"reagent": "file", "select": ["append_jsonl"]}], "on": {"root": root_s}}),
    )
    .unwrap();

    let s = c.dispatch("invoke", json!({"target": "ticks", "args": {}})).unwrap()["summon"]
        .as_str()
        .unwrap()
        .to_string();
    let d = c.dispatch("wait", json!({"summon": s, "timeout_ms": 5000})).unwrap();
    assert_eq!(d["outcome"], "returned", "{d}");
    let lines: Vec<String> = std::fs::read_to_string(&log_file)
        .unwrap()
        .lines()
        .map(String::from)
        .collect();
    assert_eq!(
        lines,
        vec![
            "{\"tick\":1}",
            "{\"tick\":1}",
            "{\"tick\":1}",
            "{\"tick\":2}",
            "{\"tick\":2}",
        ]
    );

    drop(c);
    std::fs::remove_dir_all(p.state.parent().unwrap()).unwrap();
}

#[test]
fn revive_preserves_child_model_limits_and_caller() {
    let p = paths();
    let c = Core::open(p.clone()).unwrap();
    let child_path = p.config.join("child-m.yaml");
    std::fs::write(
        &child_path,
        r#"
ritual: child-m
model: mock/child-model
essence: {requests: 2}
steps:
  - id: ask
    model:
      prompt: "hello"
return: {out: "${steps.ask.output}"}
"#,
    )
    .unwrap();
    c.dispatch("inscribe", json!({"path": child_path.to_str().unwrap()})).unwrap();

    let parent_path = p.config.join("parent-m.yaml");
    std::fs::write(
        &parent_path,
        r#"
ritual: parent-m
model: mock/parent-model
essence: {requests: 2}
steps:
  - id: call
    ritual: child-m
    arguments: {}
return: {out: "${steps.call.output.out}"}
"#,
    )
    .unwrap();
    c.dispatch("inscribe", json!({"path": parent_path.to_str().unwrap()})).unwrap();

    // Invoke with tighter limit (requests: 0) and a familiar caller, NO model override.
    let s1 = c
        .dispatch(
            "invoke",
            json!({"target": "parent-m", "args": {}, "limits": {"requests": 0}, "caller": "familiar:alpha"}),
        )
        .unwrap()["summon"]
        .as_str()
        .unwrap()
        .to_string();
    let d1 = c.dispatch("wait", json!({"summon": s1, "timeout_ms": 5000})).unwrap();
    assert_eq!(d1["outcome"], "exhausted", "{d1}");
    assert_eq!(d1["familiar"], "familiar:alpha");

    // Revive must preserve caller attribution, tighter requests: 0 limit, and
    // child's own `mock/child-model` (not overwritten by `mock/parent-model`).
    let s2 = c.dispatch("revive", json!({"summon": s1})).unwrap()["summon"]
        .as_str()
        .unwrap()
        .to_string();
    let d2 = c.dispatch("wait", json!({"summon": s2, "timeout_ms": 5000})).unwrap();
    assert_eq!(d2["outcome"], "exhausted", "{d2}");
    assert_eq!(d2["familiar"], "familiar:alpha");
    let child_model: String = c
        .db
        .lock()
        .unwrap()
        .query_row(
            "SELECT model FROM summons WHERE parent=?1 LIMIT 1",
            rusqlite::params![s2],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(child_model, "mock/child-model");

    drop(c);
    std::fs::remove_dir_all(p.state.parent().unwrap()).unwrap();
}

#[test]
fn session_bid_charges_tokens_even_when_schema_check_fails() {
    let p = paths();
    let c = Core::open(p.clone()).unwrap();
    let sid = c
        .dispatch(
            "session_open",
            json!({"model": "mock/s", "essence": {"requests": 5, "tokens": 10000}}),
        )
        .unwrap()["session"]
        .as_str()
        .unwrap()
        .to_string();
    // Mock returns `{"x": 0}`, which fails the `minimum: 5` schema check.
    let err = c
        .dispatch(
            "session_bid",
            json!({
                "session": sid,
                "prompt": "give me json",
                "schema": {"type": "object", "required": ["x"], "properties": {"x": {"type": "integer", "minimum": 5}}}
            }),
        )
        .unwrap_err();
    assert_eq!(err.code, "invalid_invocation");
    let state = c.dispatch("session_get", json!({"session": sid})).unwrap();
    assert!(
        state["used_tokens"].as_u64().unwrap_or(0) > 0,
        "tokens must be charged even when schema validation fails: {state}"
    );
    assert_eq!(state["used_requests"], 1);

    drop(c);
    std::fs::remove_dir_all(p.state.parent().unwrap()).unwrap();
}

#[test]
fn cost_budget_fails_closed_on_unpriced_model_before_dispatch() {
    let p = paths();
    let c = Core::open(p.clone()).unwrap();
    let dir = p.state.parent().unwrap();
    let f = dir.join("unpriced.yaml");
    std::fs::write(
        &f,
        r#"
ritual: unpriced-check
model: openai/gpt-4.1-mini
essence: {requests: 1, cost: "$0.01", time: 1m}
steps:
  - id: ask
    model:
      instructions: "reply"
      prompt: "hi"
return:
  ans: "${steps.ask.output}"
"#,
    )
    .unwrap();
    let reh = c
        .dispatch("rehearse", json!({"target": f.to_str().unwrap()}))
        .unwrap();
    assert_eq!(reh["pricing"]["openai/gpt-4.1-mini"], "unpriced");
    assert!(
        reh["warnings"]
            .as_array()
            .unwrap()
            .iter()
            .any(|w| w.as_str().unwrap_or("").contains("pricing:")),
        "rehearse should warn on unpriced model with essence.cost: {reh}"
    );
    let started = c
        .dispatch("invoke", json!({"target": f.to_str().unwrap()}))
        .unwrap();
    let ended = c
        .dispatch("wait", json!({"summon": started["summon"], "timeout_ms": 10_000}))
        .unwrap();
    assert_eq!(ended["outcome"], "failed", "{ended}");
    assert_eq!(ended["requests"], 0, "must fail closed before dispatch: {ended}");
    assert!(
        ended["error"].as_str().unwrap_or("").contains("pricing:"),
        "{ended}"
    );

    drop(c);
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn cost_budget_charges_rate_card_and_exhausts_on_overrun() {
    let p = paths();
    std::fs::create_dir_all(&p.config).unwrap();
    std::fs::write(
        p.config.join("config.yaml"),
        "pricing:\n  \"mock/keyword\":\n    input_per_mtok_usd: 10000.0\n    output_per_mtok_usd: 10000.0\n",
    )
    .unwrap();
    let c = Core::open(p.clone()).unwrap();
    let dir = p.state.parent().unwrap();
    let f = dir.join("priced.yaml");
    std::fs::write(
        &f,
        r#"
ritual: priced-check
model: mock/keyword
essence: {requests: 2, cost: "$0.0001", time: 1m}
steps:
  - id: ask
    model:
      instructions: "pick"
      prompt: "hello world this has several tokens"
      schema:
        type: object
        properties: {choice: {type: string, enum: [hello, other]}}
        required: [choice]
return:
  choice: "${steps.ask.output.choice}"
"#,
    )
    .unwrap();
    let started = c
        .dispatch("invoke", json!({"target": f.to_str().unwrap()}))
        .unwrap();
    let ended = c
        .dispatch("wait", json!({"summon": started["summon"], "timeout_ms": 10_000}))
        .unwrap();
    assert_eq!(ended["outcome"], "exhausted", "{ended}");
    assert!(
        ended["cost_microusd"].as_u64().unwrap_or(0) > 100,
        "expected cost_microusd > 100: {ended}"
    );

    drop(c);
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn session_cost_budget_enforces_pricing_and_cap() {
    let p = paths();
    std::fs::create_dir_all(&p.config).unwrap();
    std::fs::write(
        p.config.join("config.yaml"),
        "pricing:\n  \"mock/keyword\":\n    input_per_mtok_usd: 10000.0\n    output_per_mtok_usd: 10000.0\n",
    )
    .unwrap();
    let c = Core::open(p.clone()).unwrap();
    let opened = c
        .dispatch(
            "session_open",
            json!({"model": "mock/keyword", "essence": {"requests": 5, "cost": "$0.0001"}}),
        )
        .unwrap();
    let sid = opened["session"].as_str().unwrap().to_string();
    let err = c
        .dispatch(
            "session_bid",
            json!({"session": sid, "prompt": "hello world this exceeds budget"}),
        )
        .unwrap_err();
    assert_eq!(err.code, "exhausted", "{err:?}");
    let state = c.dispatch("session_get", json!({"session": sid})).unwrap();
    assert!(
        state["used_cost_microusd"].as_u64().unwrap_or(0) > 100,
        "expected used_cost_microusd > 100: {state}"
    );

    drop(c);
    std::fs::remove_dir_all(p.state.parent().unwrap()).unwrap();
}

#[test]
fn failed_items_in_a_lenient_each_warn_on_the_parent_record() {
    let h = super::test_home::Home::new("");
    h.inscribe_def(
        json!({
            "ritual": "lenient-child", "inputs": {"rows": {"type": "list"}},
            "steps": [{"id": "rows", "each": "r in inputs.rows", "on_failure": "continue", "steps": [
                {"id": "a", "operation": "data.get", "arguments": {"value": "${r}", "path": "/a"}}
            ]}],
            "return": {"n": "${steps.rows.counts.returned}"},
        }),
        false,
    );
    h.inscribe_def(
        json!({
            "ritual": "lenient-parent", "inputs": {"rows": {"type": "list"}},
            "steps": [{"id": "child", "ritual": "lenient-child", "arguments": {"rows": "${inputs.rows}"}}],
            "return": {"n": "${steps.child.output.n}"},
        }),
        false,
    );
    let rec = h.run(json!({"target": "lenient-parent", "args": {"rows": [{"a": 1}, {"b": 2}, {"a": 3}]}}));
    assert_eq!(rec["outcome"], "returned", "{rec}");
    let warnings = rec["warnings"].as_array().cloned().unwrap_or_default();
    assert_eq!(warnings.len(), 1, "{rec}");
    let w = warnings[0].as_str().unwrap();
    assert!(w.contains("rows: 1 of 3 items failed"), "{w}");
    assert!(w.contains("(1)"), "names the first failed item: {w}");
    assert!(w.contains("in lenient-child"), "{w}");
    let brief = h.call("get", json!({"summon": rec["id"], "brief": true})).unwrap();
    assert_eq!(brief["warnings"], json!(warnings));
}

#[test]
fn a_record_lists_effects_but_not_pure_steps() {
    let h = super::test_home::Home::new("");
    h.inscribe_def(
        json!({"ritual": "pure-only", "steps": [
            {"id": "a", "operation": "data.trim", "arguments": {"text": " a "}},
            {"id": "b", "operation": "data.count", "arguments": {"value": "${steps.a.output}"}}],
            "return": {"b": "${steps.b.output}"}}),
        false,
    );
    let rec = h.run(json!({"target": "pure-only"}));
    assert_eq!(rec["outcome"], "returned", "{rec}");
    assert_eq!(rec["effects"], json!([]), "{rec}");
    // The steps are still in the event log.
    let events = h.call("events", json!({"summon": rec["id"]})).unwrap();
    let ops = events.as_array().unwrap().iter().filter(|e| e["kind"] == "op").count();
    assert_eq!(ops, 2, "{events}");
}
