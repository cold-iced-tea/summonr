//! Witnesses: a core countersigns a ward's chain and records any fork.
use super::test_home::{repo, Home};
use super::*;
use crate::link::CoreLink;

/// A is the witness (serving its socket and `listen`), B its ward with A
/// pinned as peer `a`. B has `total-receipt` inscribed.
fn pair() -> (Home, Home) {
    let a = Home::new("");
    let b = Home::new("");
    let serving = a.core.clone();
    std::thread::spawn(move || serving.serve());
    for _ in 0..200 {
        if std::os::unix::net::UnixStream::connect(a.core.paths.socket()).is_ok() {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    let paths = a.core.paths.clone();
    std::thread::spawn(move || crate::listen::serve(listener, paths));
    b.call("peer_add", json!({"name": "a", "url": url, "key": a.core.key.public().text()})).unwrap();
    a.call("witness_accept", json!({"name": "b", "key": b.core.key.public().text()})).unwrap();
    b.inscribe(&repo("tests/fixtures/rituals/total-receipt.yaml"), false);
    (a, b)
}

fn run(b: &Home, n: i64) {
    b.run(json!({"target": "total-receipt", "args": {"text": format!("{{\"amounts_minor\":[{n}]}}")}}));
}

fn held(a: &Home) -> (i64, String) {
    a.core.db.lock().unwrap().query_row("SELECT seq, hash FROM witnessed WHERE ward='b'", [], |r| Ok((r.get(0)?, r.get(1)?))).unwrap()
}

fn stop(homes: &[&Home]) {
    for h in homes {
        h.core.stop.store(true, std::sync::atomic::Ordering::SeqCst);
    }
}

#[test]
fn first_visit_records_head_only() {
    let (a, b) = pair();
    run(&b, 1);
    run(&b, 2);
    let out = b.call("witness_to", json!({"peer": "a"})).unwrap();
    assert_eq!(out["seq"], 2, "{out}");
    assert_eq!(held(&a).0, 2);
    let head = b.call("chain_head", json!({})).unwrap();
    assert_eq!(held(&a).1, head["hash"].as_str().unwrap());
    stop(&[&a, &b]);
}

#[test]
fn extension_countersigns_and_stores() {
    let (a, b) = pair();
    run(&b, 1);
    b.call("witness_to", json!({"peer": "a"})).unwrap();
    run(&b, 2);
    run(&b, 3);
    let out = b.call("witness_to", json!({"peer": "a"})).unwrap();
    assert_eq!(out["seq"], 3);
    let (seq, hash, time, sig): (i64, String, i64, String) = b.core.db.lock().unwrap()
        .query_row("SELECT seq, hash, time, sig FROM witnessings WHERE peer='a' ORDER BY seq DESC LIMIT 1", [], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)))
        .unwrap();
    assert_eq!(seq, 3);
    assert!(crate::epitaph::check_countersign(&a.core.key.public(), &b.core.key.public().id(), seq, &hash, time, &sig));
    stop(&[&a, &b]);
}

#[test]
fn second_witness_with_nothing_new_is_a_noop() {
    let (a, b) = pair();
    run(&b, 1);
    let first = b.call("witness_to", json!({"peer": "a"})).unwrap();
    let again = b.call("witness_to", json!({"peer": "a"})).unwrap();
    assert_eq!(first["seq"], again["seq"]);
    assert_eq!(first["hash"], again["hash"]);
    let forks: i64 = a.core.db.lock().unwrap().query_row("SELECT COUNT(*) FROM forks", [], |r| r.get(0)).unwrap();
    assert_eq!(forks, 0);
    stop(&[&a, &b]);
}

/// Submit straight to the witness as B's core, bypassing B's own checks, as
/// a dishonest ward could.
fn raw_submit(a: &Home, b: &Home, from: Value, links: Value) -> Result<Value, RpcErr> {
    let key = crate::keys::load(&b.core.paths.config.join("core.key")).unwrap();
    let paths = a.core.paths.clone();
    let mut link = crate::link::SignedLink::new(key, a.core.key.public(), Box::new(move |f: &Value| crate::link::socket_send(&paths, f)));
    link.call("witness_submit", json!({"from": from, "links": links}))
}

#[test]
fn rewritten_history_is_a_recorded_fork() {
    let (a, b) = pair();
    run(&b, 1);
    run(&b, 2);
    b.call("witness_to", json!({"peer": "a"})).unwrap();
    // B re-signs a different seq 2 and offers it as following its seq 1.
    let rows = super::epitaph_tests::epitaphs(&b.core);
    let mut body: Value = serde_json::from_str(&rows[1].3).unwrap();
    body["outcome"] = json!("failed");
    let forged = crate::epitaph::seal(&b.core.key, 2, &rows[0].5, &body);
    let e = raw_submit(&a, &b, json!({"seq": 1, "hash": rows[0].5}), json!([{"seq": 2, "link": forged.link, "sig": forged.sig}])).unwrap_err();
    assert_eq!(e.code, "state_conflict", "{}", e.message);
    let (held_hash, offered): (String, String) = a.core.db.lock().unwrap()
        .query_row("SELECT held_hash, offered_from_hash FROM forks WHERE ward='b'", [], |r| Ok((r.get(0)?, r.get(1)?)))
        .unwrap();
    assert_eq!(held_hash, rows[1].5, "the witness keeps the head it held");
    assert_eq!(offered, rows[0].5);
    assert_eq!(held(&a).1, rows[1].5, "the witnessed head is unchanged");
    stop(&[&a, &b]);
}

#[test]
fn submitter_notices_a_witness_holding_another_history() {
    let (a, b) = pair();
    run(&b, 1);
    run(&b, 2);
    b.call("witness_to", json!({"peer": "a"})).unwrap();
    super::epitaph_tests::reseal_all_core(&b.core, |body| body["outcome"] = json!("failed"));
    let e = b.call("witness_to", json!({"peer": "a"})).unwrap_err();
    assert!(e.message.contains("history differs"), "{}", e.message);
    stop(&[&a, &b]);
}

#[test]
fn non_ward_key_refused() {
    let (a, b) = pair();
    let stranger = Home::new("");
    let paths = a.core.paths.clone();
    let mut link = crate::link::SignedLink::new(crate::keys::generate(), a.core.key.public(), Box::new(move |f: &Value| crate::link::socket_send(&paths, f)));
    assert_eq!(link.call("witness_head", json!({})).unwrap_err().code, "not_authenticated");
    drop(stranger);
    stop(&[&a, &b]);
}

#[test]
fn ward_cannot_call_other_methods() {
    let (a, b) = pair();
    for method in ["rituals", "invoke", "grant", "witness_accept", "chain_verify"] {
        let e = raw_submit_method(&a, &b, method).unwrap_err();
        assert_eq!(e.code, "not_authorized", "{method}");
    }
    stop(&[&a, &b]);
}

fn raw_submit_method(a: &Home, b: &Home, method: &str) -> Result<Value, RpcErr> {
    let key = crate::keys::load(&b.core.paths.config.join("core.key")).unwrap();
    let paths = a.core.paths.clone();
    let mut link = crate::link::SignedLink::new(key, a.core.key.public(), Box::new(move |f: &Value| crate::link::socket_send(&paths, f)));
    link.call(method, json!({}))
}

#[test]
fn key_cannot_be_ward_and_familiar() {
    let (a, b) = pair();
    let ward_key = b.core.key.public().text();
    assert_eq!(a.call("familiar_add", json!({"name": "dup", "key": ward_key})).unwrap_err().exit, 5);
    let f = crate::keys::generate().public().text();
    a.call("familiar_add", json!({"name": "agentf", "key": f})).unwrap();
    assert_eq!(a.call("witness_accept", json!({"name": "dup2", "key": f})).unwrap_err().exit, 5);
    stop(&[&a, &b]);
}

#[test]
fn unpinned_or_unreachable_peer_fails_cleanly() {
    let (a, b) = pair();
    run(&b, 1);
    let e = b.call("witness_to", json!({"peer": "nowhere"})).unwrap_err();
    assert!(e.message.contains("nowhere"), "{}", e.message);
    b.call("peer_add", json!({"name": "dead", "url": "http://127.0.0.1:9", "key": a.core.key.public().text()})).unwrap();
    let e = b.call("witness_to", json!({"peer": "dead"})).unwrap_err();
    assert!(e.message.contains("dead") || e.message.contains("127.0.0.1:9"), "{}", e.message);
    let n: i64 = b.core.db.lock().unwrap().query_row("SELECT COUNT(*) FROM witnessings", [], |r| r.get(0)).unwrap();
    assert_eq!(n, 0);
    stop(&[&a, &b]);
}

#[test]
fn verify_lists_witness_age_and_witness_list_shows_wards() {
    let (a, b) = pair();
    run(&b, 1);
    b.call("witness_to", json!({"peer": "a"})).unwrap();
    let v = b.call("chain_verify", json!({})).unwrap();
    assert_eq!(v["witnesses"][0]["peer"], "a", "{v}");
    assert_eq!(v["witnesses"][0]["seq"], 1);
    assert_eq!(v["witnesses"][0]["valid"], true);
    let wards = a.call("witness_list", json!({})).unwrap();
    assert_eq!(wards[0]["name"], "b");
    assert_eq!(wards[0]["seq"], 1);
    assert_eq!(wards[0]["forks"], 0);
    stop(&[&a, &b]);
}

#[test]
fn missing_openssl_refuses_and_verify_reports_unchecked() {
    let h = Home::new("openssl: /nonexistent/openssl\n");
    h.inscribe(&repo("tests/fixtures/rituals/total-receipt.yaml"), false);
    run(&h, 1);
    let e = h.call("witness_tsa", json!({"url": "http://127.0.0.1:9"})).unwrap_err();
    assert!(e.message.contains("openssl not found"), "{}", e.message);
    // A stored timestamp with no way to check it is unchecked, not a failure.
    h.core.db.lock().unwrap().execute(
        "INSERT INTO timestamps (seq, hash, url, response, time) VALUES (1, (SELECT hash FROM epitaphs WHERE seq=1), 'http://tsa', x'00', 1)", [],
    ).unwrap();
    let v = h.call("chain_verify", json!({"tsa_ca": "/tmp/ca.pem"})).unwrap();
    assert_eq!(v["ok"], true, "{v}");
    assert!(v["timestamps"][0]["verdict"].as_str().unwrap().starts_with("unchecked"), "{v}");
}

// ------------------------------------------------------ final-review fixes

#[test]
fn a_late_concurrent_submission_is_not_a_fork() {
    let (a, b) = pair();
    run(&b, 1);
    b.call("witness_to", json!({"peer": "a"})).unwrap();
    run(&b, 2);
    run(&b, 3);
    // Two submitters both read head #1; the first moves the witness to #3.
    let rows = super::epitaph_tests::epitaphs(&b.core);
    let links: Vec<Value> = rows[1..].iter().map(|r| json!({"seq": r.0, "link": r.2, "sig": r.4})).collect();
    raw_submit(&a, &b, json!({"seq": 1, "hash": rows[0].5}), json!(links)).unwrap();
    let late = raw_submit(&a, &b, json!({"seq": 1, "hash": rows[0].5}), json!(links)).unwrap();
    assert_eq!(late["seq"], 3);
    let forks: i64 = a.core.db.lock().unwrap().query_row("SELECT COUNT(*) FROM forks", [], |r| r.get(0)).unwrap();
    assert_eq!(forks, 0, "an honest late submitter is not a rewrite");
    stop(&[&a, &b]);
}

#[test]
fn a_fork_keeps_the_signed_links_offered() {
    let (a, b) = pair();
    run(&b, 1);
    run(&b, 2);
    b.call("witness_to", json!({"peer": "a"})).unwrap();
    let rows = super::epitaph_tests::epitaphs(&b.core);
    let mut body: Value = serde_json::from_str(&rows[1].3).unwrap();
    body["outcome"] = json!("failed");
    let forged = crate::epitaph::seal(&b.core.key, 2, &rows[0].5, &body);
    let _ = raw_submit(&a, &b, json!({"seq": 1, "hash": rows[0].5}), json!([{"seq": 2, "link": forged.link, "sig": forged.sig}]));
    let offered: String = a.core.db.lock().unwrap().query_row("SELECT offered FROM forks", [], |r| r.get(0)).unwrap();
    let offered: Value = serde_json::from_str(&offered).unwrap();
    assert_eq!(offered[0]["link"], forged.link.as_str());
    assert_eq!(offered[0]["sig"], forged.sig.as_str());
    // The fork alone proves the rewrite: both links verify with the ward's key at the same seq.
    assert!(crate::epitaph::check_link(&forged.link, &forged.sig, &b.core.key.public()).is_ok());
    stop(&[&a, &b]);
}

#[test]
fn verify_flags_witnesses_and_timestamps_the_chain_no_longer_matches() {
    let (a, b) = pair();
    run(&b, 1);
    b.call("witness_to", json!({"peer": "a"})).unwrap();
    b.core.db.lock().unwrap().execute(
        "INSERT INTO timestamps (seq, hash, url, response, time) VALUES (1, (SELECT hash FROM epitaphs WHERE seq=1), 'http://tsa', x'00', 1)", [],
    ).unwrap();
    assert_eq!(b.call("chain_verify", json!({})).unwrap()["ok"], true);
    super::epitaph_tests::reseal_all_core(&b.core, |body| body["outcome"] = json!("failed"));
    let v = b.call("chain_verify", json!({})).unwrap();
    assert_eq!(v["ok"], false, "{v}");
    assert_eq!(v["witnesses"][0]["matches"], false, "{v}");
    assert_eq!(v["timestamps"][0]["matches"], false, "{v}");
    stop(&[&a, &b]);
}

#[test]
fn dropping_a_ward_forgets_its_head() {
    let (a, b) = pair();
    run(&b, 1);
    b.call("witness_to", json!({"peer": "a"})).unwrap();
    a.call("witness_drop", json!({"name": "b"})).unwrap();
    let other = crate::keys::generate().public().text();
    a.call("witness_accept", json!({"name": "b", "key": other})).unwrap();
    let list = a.call("witness_list", json!({})).unwrap();
    assert_eq!(list[0]["seq"], 0, "a new ward under an old name starts fresh: {list}");
    assert_eq!(list[0]["forks"], 0);
    stop(&[&a, &b]);
}

// -------------------------------------------- witness-to-witness comparison

#[test]
fn compare_records_agreeing_witnesses() {
    let records = vec![
        super::witness::WitnessRecord {
            label: "p1".into(),
            seq: 3,
            hash: "abc".into(),
            merkle_root: Some("mr1".into()),
            valid: true,
        },
        super::witness::WitnessRecord {
            label: "p2".into(),
            seq: 3,
            hash: "abc".into(),
            merkle_root: Some("mr1".into()),
            valid: true,
        },
    ];
    let out = super::witness::compare_witness_records(&records);
    assert_eq!(out["ok"], true, "{out}");
    assert_eq!(out["split_view"], false, "{out}");
    assert_eq!(out["conflicts"].as_array().unwrap().len(), 0);
    assert_eq!(out["observations"].as_array().unwrap().len(), 2);
}

#[test]
fn compare_records_detects_split_view_hash_and_merkle_and_invalid() {
    let records = vec![
        super::witness::WitnessRecord {
            label: "p1".into(),
            seq: 2,
            hash: "hash_one".into(),
            merkle_root: None,
            valid: true,
        },
        super::witness::WitnessRecord {
            label: "p2".into(),
            seq: 2,
            hash: "hash_two".into(),
            merkle_root: None,
            valid: true,
        },
    ];
    let out = super::witness::compare_witness_records(&records);
    assert_eq!(out["ok"], false, "{out}");
    assert_eq!(out["split_view"], true, "{out}");
    let c = &out["conflicts"][0];
    assert_eq!(c["seq"], 2);
    assert_eq!(c["a"], "p1");
    assert_eq!(c["b"], "p2");
    assert_eq!(c["hash_a"], "hash_one");
    assert_eq!(c["hash_b"], "hash_two");
    assert_eq!(c["reason"], "split_view");

    // Different merkle_root at same seq & hash also flags split_view.
    let merkle_records = vec![
        super::witness::WitnessRecord {
            label: "p1".into(),
            seq: 2,
            hash: "same".into(),
            merkle_root: Some("root_a".into()),
            valid: true,
        },
        super::witness::WitnessRecord {
            label: "p2".into(),
            seq: 2,
            hash: "same".into(),
            merkle_root: Some("root_b".into()),
            valid: true,
        },
    ];
    let out_m = super::witness::compare_witness_records(&merkle_records);
    assert_eq!(out_m["ok"], false, "{out_m}");
    assert_eq!(out_m["split_view"], true, "{out_m}");
    assert_eq!(out_m["conflicts"][0]["reason"], "split_view");

    // Invalid signature without hash conflict -> ok: false, split_view: false.
    let invalid_records = vec![super::witness::WitnessRecord {
        label: "p1".into(),
        seq: 1,
        hash: "h1".into(),
        merkle_root: None,
        valid: false,
    }];
    let out_inv = super::witness::compare_witness_records(&invalid_records);
    assert_eq!(out_inv["ok"], false, "{out_inv}");
    assert_eq!(out_inv["split_view"], false, "{out_inv}");
    assert_eq!(out_inv["invalid"], 1);
}

#[test]
fn compare_records_notes_prefix_comparability() {
    let records = vec![
        super::witness::WitnessRecord {
            label: "p1".into(),
            seq: 1,
            hash: "h1".into(),
            merkle_root: None,
            valid: true,
        },
        super::witness::WitnessRecord {
            label: "p2".into(),
            seq: 2,
            hash: "h2".into(),
            merkle_root: None,
            valid: true,
        },
        super::witness::WitnessRecord {
            label: "local".into(),
            seq: 1,
            hash: "h1".into(),
            merkle_root: None,
            valid: true,
        },
        super::witness::WitnessRecord {
            label: "local".into(),
            seq: 2,
            hash: "h2".into(),
            merkle_root: None,
            valid: true,
        },
    ];
    let out = super::witness::compare_witness_records(&records);
    assert_eq!(out["ok"], true, "{out}");
    assert_eq!(out["split_view"], false, "{out}");
    let notes = out["prefix_notes"].as_array().unwrap();
    assert!(notes.iter().any(|n| n["lower"] == "p1" && n["higher"] == "p2" && n["local_bridged"] == true), "{out}");
}

fn add_witness(b: &Home, name: &str) -> Home {
    let w = Home::new("");
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    let paths = w.core.paths.clone();
    std::thread::spawn(move || crate::listen::serve(listener, paths));
    let serving = w.core.clone();
    std::thread::spawn(move || serving.serve());
    for _ in 0..200 {
        if std::os::unix::net::UnixStream::connect(w.core.paths.socket()).is_ok() {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    b.call("peer_add", json!({"name": name, "url": url, "key": w.core.key.public().text()})).unwrap();
    w.call("witness_accept", json!({"name": "b", "key": b.core.key.public().text()})).unwrap();
    w
}

#[test]
fn witness_compare_agreeing_offline_and_live() {
    let (a1, b) = pair();
    let a2 = add_witness(&b, "a2");
    run(&b, 1);
    run(&b, 2);
    b.call("witness_to", json!({"peer": "a"})).unwrap();
    b.call("witness_to", json!({"peer": "a2"})).unwrap();

    let offline = b.core.witness_compare(json!({})).unwrap();
    assert_eq!(offline["ok"], true, "{offline}");
    assert_eq!(offline["split_view"], false, "{offline}");

    let live = b.core.witness_compare(json!({"peers": ["a", "a2"], "live": true})).unwrap();
    assert_eq!(live["ok"], true, "{live}");
    assert_eq!(live["split_view"], false, "{live}");

    stop(&[&a1, &a2, &b]);
}

#[test]
fn witness_compare_detects_split_view_offline_and_live() {
    let (a1, b) = pair();
    let a2 = add_witness(&b, "a2");
    run(&b, 1);
    run(&b, 2);
    b.call("witness_to", json!({"peer": "a"})).unwrap();
    let original_hash = b.call("chain_head", json!({})).unwrap()["hash"].as_str().unwrap().to_string();

    // Reseal b's chain so a2 witnesses a conflicting history at seq 2.
    super::epitaph_tests::reseal_all_core(&b.core, |body| body["outcome"] = json!("failed"));
    b.call("witness_to", json!({"peer": "a2"})).unwrap();
    let forked_hash = b.call("chain_head", json!({})).unwrap()["hash"].as_str().unwrap().to_string();
    assert_ne!(original_hash, forked_hash);

    let offline = b.core.witness_compare(json!({})).unwrap();
    assert_eq!(offline["ok"], false, "{offline}");
    assert_eq!(offline["split_view"], true, "{offline}");
    let conflicts = offline["conflicts"].as_array().unwrap();
    assert!(
        conflicts.iter().any(|c| c["seq"] == 2 && c["reason"] == "split_view" && c["hash_a"] != c["hash_b"]),
        "{offline}"
    );

    let live = b.core.witness_compare(json!({"live": true})).unwrap();
    assert_eq!(live["ok"], false, "{live}");
    assert_eq!(live["split_view"], true, "{live}");
    let live_conflicts = live["conflicts"].as_array().unwrap();
    assert!(
        live_conflicts.iter().any(|c| c["seq"] == 2 && c["reason"] == "split_view" && c["hash_a"] != c["hash_b"]),
        "{live}"
    );

    stop(&[&a1, &a2, &b]);
}
