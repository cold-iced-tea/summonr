//! Pacts: storage, anchoring, transport, agreement, calls, holds, delivery
//! and acceptance between two cores.
use super::pacts::{append_own, store_received, StoreOutcome};
use super::test_home::Home;
use super::*;
use crate::pact::{self, Kind, Signed};

/// A pact row on `h` between `h`'s core (as caller) and `other` (callee),
/// stored directly, with the offer signed by `h`.
pub(super) fn seeded(h: &Home, other: &crate::keys::Keypair) -> (pact::Terms, String) {
    let yaml = format!(
        "pact: p\ncaller: {}\ncallee: {}\nritual: {{name: r, hash: {}}}\nceilings: {{deadline: 1h}}\nexpires: 2099-01-01\n",
        h.core.key.public().text(),
        other.public().text(),
        "a".repeat(64)
    );
    let t = pact::parse_terms(&yaml).unwrap();
    let id = pact::pact_id(&t);
    h.core
        .db
        .lock()
        .unwrap()
        .execute(
            "INSERT INTO pacts (id, name, peer, side, terms, state, created) VALUES (?1, 'p', 'b', 'caller', ?2, 'offered', 0)",
            params![id, pact::terms_bytes(&t)],
        )
        .unwrap();
    (t, id)
}

fn own(h: &Home, id: &str, kind: Kind, data: Value) -> Signed {
    let mut db = h.core.db.lock().unwrap();
    let tx = db.transaction().unwrap();
    let s = append_own(&tx, &h.core.key, id, kind, None, data).unwrap();
    tx.commit().unwrap();
    s
}

fn received(h: &Home, id: &str, s: &Signed, by: &crate::keys::PublicKey) -> Result<StoreOutcome, RpcErr> {
    let mut db = h.core.db.lock().unwrap();
    let tx = db.transaction().unwrap();
    let out = store_received(&tx, id, s, by);
    tx.commit().unwrap();
    out
}

fn count(h: &Home, sql: &str) -> i64 {
    h.core.db.lock().unwrap().query_row(sql, [], |r| r.get(0)).unwrap()
}

#[test]
fn own_entry_writes_epitaph_same_tx() {
    let h = Home::new("");
    let other = crate::keys::generate();
    let (t, id) = seeded(&h, &other);
    own(&h, &id, Kind::Offer, serde_json::to_value(&t).unwrap());
    assert_eq!(count(&h, "SELECT COUNT(*) FROM pact_entries"), 1);
    assert_eq!(count(&h, "SELECT COUNT(*) FROM epitaphs WHERE run LIKE 'pact:%'"), 1);
}

#[test]
fn pact_epitaph_has_no_content() {
    let h = Home::new("");
    let other = crate::keys::generate();
    let (_, id) = seeded(&h, &other);
    // Only the offer is a valid first entry; its content is the marker here.
    let marker = "unique-marker-7f3c";
    let mut db = h.core.db.lock().unwrap();
    let tx = db.transaction().unwrap();
    append_own(&tx, &h.core.key, &id, Kind::Offer, None, json!({"note": marker})).unwrap();
    tx.commit().unwrap();
    let (link, body): (String, String) = db.query_row("SELECT link, body FROM epitaphs WHERE run LIKE 'pact:%'", [], |r| Ok((r.get(0)?, r.get(1)?))).unwrap();
    assert!(!link.contains(marker) && !body.contains(marker), "{body}");
    let b: Value = serde_json::from_str(&body).unwrap();
    assert_eq!(b["kind"], "pact");
    assert_eq!(b["n"], 1);
}

#[test]
fn chain_verify_rechecks_pact_epitaphs() {
    let h = Home::new("");
    let other = crate::keys::generate();
    let (t, id) = seeded(&h, &other);
    own(&h, &id, Kind::Offer, serde_json::to_value(&t).unwrap());
    let ok = h.call("chain_verify", json!({})).unwrap();
    assert_eq!(ok["ok"], true, "{ok}");
    h.core.db.lock().unwrap().execute("UPDATE pact_entries SET bytes = replace(bytes, '\"n\":1', '\"n\":1 ')", []).unwrap();
    let bad = h.call("chain_verify", json!({})).unwrap();
    assert_eq!(bad["ok"], false, "{bad}");
    assert_eq!(bad["problem"]["seq"], 1);
}

#[test]
fn duplicate_received_is_idempotent() {
    let h = Home::new("");
    let other = crate::keys::generate();
    let (t, id) = seeded(&h, &other);
    let offer = pact::seal_entry(&other, &id, 1, crate::epitaph::ZEROS, Kind::Offer, None, 1, serde_json::to_value(&t).unwrap());
    assert!(matches!(received(&h, &id, &offer, &other.public()).unwrap(), StoreOutcome::Stored));
    assert!(matches!(received(&h, &id, &offer, &other.public()).unwrap(), StoreOutcome::Duplicate));
    assert_eq!(count(&h, "SELECT COUNT(*) FROM pact_entries"), 1);
}

#[test]
fn stale_received_reports_head() {
    let h = Home::new("");
    let other = crate::keys::generate();
    let (t, id) = seeded(&h, &other);
    let offer = own(&h, &id, Kind::Offer, serde_json::to_value(&t).unwrap());
    let agree = pact::seal_entry(&other, &id, 2, &offer.hash, Kind::Agree, None, 2, json!({}));
    received(&h, &id, &agree, &other.public()).unwrap();
    own(&h, &id, Kind::Call, json!({"inputs": {}}));
    // The other side signs on top of the agree while this side's call is
    // already there: a different signer, so stale, not a fork.
    let late = pact::seal_entry(&other, &id, 3, &agree.hash, Kind::Deliver, Some(1), 3, json!({}));
    match received(&h, &id, &late, &other.public()) {
        Ok(StoreOutcome::Stale { head }) => assert_eq!(head, 3),
        other => panic!("{:?}", other.map(|_| ())),
    }
}

#[test]
fn fork_received_freezes_and_keeps_both() {
    let h = Home::new("");
    let other = crate::keys::generate();
    let (t, id) = seeded(&h, &other);
    let offer = own(&h, &id, Kind::Offer, serde_json::to_value(&t).unwrap());
    let agree = pact::seal_entry(&other, &id, 2, &offer.hash, Kind::Agree, None, 2, json!({"a": 1}));
    received(&h, &id, &agree, &other.public()).unwrap();
    let twin = pact::seal_entry(&other, &id, 2, &offer.hash, Kind::Agree, None, 2, json!({"a": 2}));
    let err = received(&h, &id, &twin, &other.public()).unwrap_err();
    assert_eq!(err.code, "state_conflict");
    assert_eq!(count(&h, "SELECT COUNT(*) FROM pact_forks"), 1);
    let state: String = h.core.db.lock().unwrap().query_row("SELECT state FROM pacts WHERE id=?1", params![id], |r| r.get(0)).unwrap();
    assert_eq!(state, "frozen");
}

// ------------------------------------------------------------ transport

pub(super) fn serve(h: &Home) -> String {
    let serving = h.core.clone();
    std::thread::spawn(move || serving.serve());
    for _ in 0..200 {
        if std::os::unix::net::UnixStream::connect(h.core.paths.socket()).is_ok() {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    let paths = h.core.paths.clone();
    std::thread::spawn(move || crate::listen::serve(listener, paths));
    url
}

/// A is the caller, B the callee serving `listen`. Each pins the other as a
/// peer (`a`, `b`); B admits A as counterparty `a` when `admit`.
pub(super) fn pair(admit: bool) -> (Home, Home) {
    pair_with(admit, Home::new(""))
}

pub(super) fn pair_with(admit: bool, b: Home) -> (Home, Home) {
    let a = Home::new("");
    let url = serve(&b);
    a.call("peer_add", json!({"name": "b", "url": url, "key": b.core.key.public().text()})).unwrap();
    b.call("peer_add", json!({"name": "a", "url": "http://127.0.0.1:9", "key": a.core.key.public().text()})).unwrap();
    if admit {
        b.call("pact_admit", json!({"name": "a", "key": a.core.key.public().text()})).unwrap();
    }
    (a, b)
}

pub(super) fn stop(homes: &[&Home]) {
    for h in homes {
        h.core.stop.store(true, std::sync::atomic::Ordering::SeqCst);
    }
}

#[test]
fn unadmitted_core_refused_generic() {
    let (a, b) = pair(false);
    let err = a.core.pact_send("b", "pact_sync", json!({"pact": "x", "after": 0})).unwrap_err();
    assert_eq!(err.code, "not_authenticated", "{}", err.message);
    stop(&[&a, &b]);
}

#[test]
fn counterparty_limited_to_pact_methods() {
    let (a, b) = pair(true);
    let err = a.core.pact_send("b", "invoke", json!({"target": "x"})).unwrap_err();
    assert_eq!(err.code, "not_authorized", "{}", err.message);
    let err = a.core.pact_send("b", "witness_head", json!({})).unwrap_err();
    assert_eq!(err.code, "not_authorized", "{}", err.message);
    stop(&[&a, &b]);
}

#[test]
fn key_cannot_be_familiar_and_counterparty() {
    let h = Home::new("");
    let k = crate::keys::generate().public().text();
    h.call("familiar_add", json!({"name": "f", "key": k})).unwrap();
    assert!(h.call("pact_admit", json!({"name": "c", "key": k})).is_err());
    let k2 = crate::keys::generate().public().text();
    h.call("pact_admit", json!({"name": "c2", "key": k2})).unwrap();
    assert!(h.call("familiar_add", json!({"name": "f2", "key": k2})).is_err());
}

#[test]
fn replayed_post_is_duplicate_not_fork() {
    let (a, b) = pair(true);
    // B holds a pact row with A as caller; A signs the offer and posts it twice.
    let yaml = format!(
        "pact: p\ncaller: {}\ncallee: {}\nritual: {{name: r, hash: {}}}\nceilings: {{deadline: 1h}}\nexpires: 2099-01-01\n",
        a.core.key.public().text(), b.core.key.public().text(), "a".repeat(64)
    );
    let t = pact::parse_terms(&yaml).unwrap();
    let id = pact::pact_id(&t);
    b.core.db.lock().unwrap().execute(
        "INSERT INTO pacts (id, name, peer, side, terms, state, created) VALUES (?1, 'p', 'a', 'callee', ?2, 'offered', 0)",
        params![id, pact::terms_bytes(&t)],
    ).unwrap();
    let offer = pact::seal_entry(&a.core.key, &id, 1, crate::epitaph::ZEROS, Kind::Offer, None, 1, serde_json::to_value(&t).unwrap());
    let first = a.core.pact_send("b", "pact_post", json!({"pact": id, "entry": offer})).unwrap();
    assert_eq!(first["stored"], "stored", "{first}");
    let again = a.core.pact_send("b", "pact_post", json!({"pact": id, "entry": offer})).unwrap();
    assert_eq!(again["stored"], "duplicate", "{again}");
    let synced = a.core.pact_send("b", "pact_sync", json!({"pact": id, "after": 0})).unwrap();
    assert_eq!(synced["entries"].as_array().unwrap().len(), 1);
    stop(&[&a, &b]);
}

#[test]
fn counterparty_cannot_touch_another_cores_pact() {
    let (a, b) = pair(true);
    let other = crate::keys::generate();
    let (_, id) = seeded(&b, &other); // B is caller, `other` callee: A is not a party
    let err = a.core.pact_send("b", "pact_sync", json!({"pact": id, "after": 0})).unwrap_err();
    assert_eq!(err.code, "not_found", "{}", err.message);
    stop(&[&a, &b]);
}

// ------------------------------------------------------------ offer and agree

pub(super) const MOVE_ONE: &str = "language_version: 1
ritual: move-one
description: Move the newest inbox message to Receipts; flags amounts over a threshold.
inputs:
  amount: {type: integer}
  threshold: {type: integer}
  note: {type: string, default: none}
reagents:
  mail: {select: [search, move, locate]}
  data: {select: [compare]}
requires:
  - {reagent: mail, action: search, on: {folder: Inbox}}
  - {reagent: mail, action: move, on: {folder: Inbox}, into: {folder: [Receipts]}}
  - {reagent: mail, action: locate}
essence: {requests: 0, tool_calls: 20, time: 30s}
steps:
  - id: over
    operation: data.compare
    arguments: {left: \"${inputs.amount}\", right: \"${inputs.threshold}\", operator: gt}
  - id: pick
    operation: mail.search
    arguments: {folder: Inbox, newest: 1}
  - id: each
    each: m in steps.pick.output
    steps:
      - id: commit
        operation: mail.move
        arguments: {message: \"${m.id}\", folder: Receipts}
        verify:
          observe: mail.locate
          arguments: {message: \"${m.id}\"}
          expect: {folder: Receipts}
return:
  amount: \"${inputs.amount}\"
output_schema:
  type: object
  required: [amount]
  properties:
    amount: {type: integer}
";

pub(super) const CHECK: &str = "language_version: 1
ritual: check-quote
inputs:
  delivery: {type: object}
reagents:
  data: {select: [compare]}
essence: {requests: 0, tool_calls: 2, time: 5s}
steps:
  - id: small
    operation: data.compare
    arguments: {left: \"${inputs.delivery.amount}\", right: 1000, operator: le}
return:
  accept: \"${steps.small.output}\"
  reason: amount checked against 1000
";

/// A callee home with one message in Inbox and `move-one` inscribed and granted.
pub(super) fn callee_home() -> Home {
    callee_home_with(MOVE_ONE)
}

/// As `callee_home`, with the commit step also held for the callee's owner.
pub(super) fn callee_home_holding() -> Home {
    callee_home_with(&MOVE_ONE.replace("        arguments: {message: \"${m.id}\", folder: Receipts}\n", "        arguments: {message: \"${m.id}\", folder: Receipts}\n        hold: true\n"))
}

pub(super) fn callee_home_with(source: &str) -> Home {
    let root = std::env::temp_dir().join(new_id("summonr-pact-mail"));
    for f in ["Inbox", "Receipts"] {
        std::fs::create_dir_all(root.join("mail").join(f)).unwrap();
    }
    std::fs::write(root.join("mail/Inbox/001.eml"), "From: a@example.com\nSubject: order\n\nOrder 7").unwrap();
    let h = Home::new(&format!("mail_root: \"{}\"\n", root.join("mail").display()));
    let file = h.root.join("move-one.yaml");
    std::fs::write(&file, source).unwrap();
    h.inscribe(&file, false);
    h.call("grant", json!({"ritual": "move-one", "scopes": [{"reagent": "mail", "all": true}], "on": {"folder": "Inbox"}})).unwrap();
    h.call("grant", json!({"ritual": "move-one", "scopes": [{"reagent": "mail", "select": ["locate"]}]})).unwrap();
    h
}

pub(super) fn move_one_hash(b: &Home) -> String {
    b.call("contract", json!({"name": "move-one"})).unwrap()["hash"].as_str().unwrap().to_string()
}

/// Terms for A (caller) and B (callee) over `move-one`, with `extra` lines.
pub(super) fn terms_file(dir: &Home, a: &Home, b: &Home, hash: &str, extra: &str) -> String {
    let check_hash = crate::ritual::content_hash(&crate::ritual::parse_file(CHECK).unwrap());
    let indented: String = CHECK.lines().map(|l| format!("      {l}\n")).collect();
    let yaml = format!(
        "pact: order-parts\ncaller: {}\ncallee: {}\nritual: {{name: move-one, hash: {hash}}}\nfixed: {{threshold: 1000}}\ninputs: [amount]\nceilings:\n  per_call: {{tool_calls: 20, time: 30s}}\n  total: {{calls: 5}}\n  deadline: 1h\nheld_by_caller:\n  - {{step: commit, when: \"${{steps.over.output}}\"}}\nchecks:\n  schema: true\n  ritual:\n    name: check-quote\n    hash: {check_hash}\n    source: |\n{indented}expires: 2099-01-01\n{extra}",
        a.core.key.public().text(),
        b.core.key.public().text(),
    );
    let path = dir.root.join(format!("{}.yaml", new_id("terms")));
    std::fs::write(&path, yaml).unwrap();
    path.to_str().unwrap().to_string()
}

pub(super) fn state(h: &Home, pact: &str) -> String {
    h.call("pact_show", json!({"pact": pact})).unwrap()["state"].as_str().unwrap().to_string()
}

/// A offers, B agrees, A syncs: an agreed pact named `order-parts` on both.
pub(super) fn agreed(extra: &str) -> (Home, Home) {
    agreed_with(extra, &[])
}

/// As `agreed`, with textual replacements applied to the terms first.
pub(super) fn agreed_with(extra: &str, swaps: &[(&str, &str)]) -> (Home, Home) {
    agreed_on(callee_home(), extra, swaps)
}

pub(super) fn agreed_on(b: Home, extra: &str, swaps: &[(&str, &str)]) -> (Home, Home) {
    let (a, b) = pair_with(true, b);
    let file = terms_file(&a, &a, &b, &move_one_hash(&b), extra);
    let mut text = std::fs::read_to_string(&file).unwrap();
    for (from, to) in swaps {
        assert!(text.contains(from), "{from}");
        text = text.replace(from, to);
    }
    std::fs::write(&file, text).unwrap();
    a.call("pact_offer", json!({"peer": "b", "file": file})).unwrap();
    b.call("pact_agree", json!({"pact": "order-parts"})).unwrap();
    a.call("pact_poll", json!({})).unwrap();
    assert_eq!(state(&a, "order-parts"), "agreed");
    (a, b)
}

#[test]
fn offer_validation_errors() {
    let b = callee_home();
    let a = Home::new("");
    b.call("peer_add", json!({"name": "a", "url": "http://127.0.0.1:9", "key": a.core.key.public().text()})).unwrap();
    let hash = move_one_hash(&b);
    let offer = |file: String| b.call("pact_offer", json!({"peer": "a", "file": file}));
    let e = offer(terms_file(&b, &a, &b, &"c".repeat(64), "")).unwrap_err();
    assert!(e.message.contains("hash"), "{}", e.message);
    let read_held = terms_file(&b, &a, &b, &hash, "").replace("step: commit", "step: pick");
    let f = b.path("t1.yaml");
    std::fs::write(&f, std::fs::read_to_string(&read_held).unwrap().replace("step: commit", "step: pick")).unwrap();
    let e = offer(f.clone()).unwrap_err();
    assert!(e.message.contains("writes"), "{}", e.message);
    let text = std::fs::read_to_string(terms_file(&b, &a, &b, &hash, "")).unwrap();
    std::fs::write(&f, text.replace("${steps.over.output}", "${inputs.note}")).unwrap();
    let e = offer(f.clone()).unwrap_err();
    assert!(e.message.contains("boolean"), "{}", e.message);
    std::fs::write(&f, text.replace("inputs: [amount]", "inputs: []")).unwrap();
    let e = offer(f.clone()).unwrap_err();
    assert!(e.message.contains("amount"), "{}", e.message);
    std::fs::write(&f, text.replace("step: commit", "step: nowhere")).unwrap();
    assert!(offer(f).is_err());
}

#[test]
fn agree_signed_by_other_side_only() {
    let (a, b) = pair_with(true, callee_home());
    let file = terms_file(&a, &a, &b, &move_one_hash(&b), "");
    let out = a.call("pact_offer", json!({"peer": "b", "file": file})).unwrap();
    assert_eq!(out["state"], "offered", "{out}");
    assert_eq!(state(&b, "order-parts"), "offered");
    assert!(a.call("pact_agree", json!({"pact": "order-parts"})).is_err());
    b.call("pact_agree", json!({"pact": "order-parts"})).unwrap();
    assert_eq!(state(&b, "order-parts"), "agreed");
    a.call("pact_poll", json!({})).unwrap();
    assert_eq!(state(&a, "order-parts"), "agreed");
    stop(&[&a, &b]);
}

#[test]
fn no_call_entry_before_agree() {
    let (a, b) = pair_with(true, callee_home());
    let file = terms_file(&a, &a, &b, &move_one_hash(&b), "");
    let id = a.call("pact_offer", json!({"peer": "b", "file": file})).unwrap()["id"].as_str().unwrap().to_string();
    let mut db = b.core.db.lock().unwrap();
    let tx = db.transaction().unwrap();
    assert!(append_own(&tx, &b.core.key, &id, Kind::Deliver, Some(1), json!({})).is_err());
    drop(tx);
    drop(db);
    stop(&[&a, &b]);
}

#[test]
fn callee_offer_fetched_by_caller() {
    let (a, b) = pair_with(true, callee_home());
    let file = terms_file(&b, &a, &b, &move_one_hash(&b), "");
    b.call("pact_offer", json!({"peer": "a", "file": file})).unwrap();
    let fetched = a.call("pact_fetch", json!({"peer": "b"})).unwrap();
    assert_eq!(fetched["fetched"], 1, "{fetched}");
    assert_eq!(state(&a, "order-parts"), "offered");
    assert!(b.call("pact_agree", json!({"pact": "order-parts"})).is_err(), "the offerer cannot agree");
    a.call("pact_agree", json!({"pact": "order-parts"})).unwrap();
    assert_eq!(state(&b, "order-parts"), "agreed");
    stop(&[&a, &b]);
}

#[test]
fn show_includes_both_rehearsals() {
    let (a, b) = agreed("");
    for h in [&a, &b] {
        let shown = h.call("pact_show", json!({"pact": "order-parts"})).unwrap();
        assert_eq!(shown["ritual"]["ritual"], "move-one", "{shown}");
        assert!(shown["ritual"]["effects"].as_array().is_some_and(|e| !e.is_empty()), "{shown}");
        assert_eq!(shown["acceptance"]["ritual"], "check-quote", "{shown}");
        assert!(shown["acceptance"]["source"].as_str().unwrap().contains("delivery"));
    }
    let listed = a.call("pact_list", json!({})).unwrap();
    assert_eq!(listed[0]["name"], "order-parts", "{listed}");
    stop(&[&a, &b]);
}

#[test]
fn peer_key_change_refuses_entries() {
    let (a, b) = agreed("");
    let url = a.call("peer_list", json!({})).unwrap()[0]["url"].as_str().unwrap().to_string();
    a.call("peer_remove", json!({"name": "b"})).unwrap();
    a.call("peer_add", json!({"name": "b", "url": url, "key": crate::keys::generate().public().text()})).unwrap();
    let shown = a.call("pact_show", json!({"pact": "order-parts"})).unwrap();
    assert!(shown["warnings"].as_array().unwrap().iter().any(|w| w.as_str().unwrap().contains("key")), "{shown}");
    let polled = a.call("pact_poll", json!({})).unwrap();
    assert!(polled["errors"].as_array().is_some_and(|e| !e.is_empty()), "{polled}");
    stop(&[&a, &b]);
}

// ------------------------------------------------------------ calls

/// The thread on `h`, decoded.
pub(super) fn entries(h: &Home, pact: &str) -> Vec<pact::Entry> {
    let id = h.call("pact_show", json!({"pact": pact})).unwrap()["id"].as_str().unwrap().to_string();
    super::pacts::thread_of(&h.core.db.lock().unwrap(), &id).unwrap().into_iter().map(|(e, _)| e).collect()
}

/// The run B started for call `n`, waited to its end.
pub(super) fn callee_run(b: &Home, n: i64) -> Value {
    let run: String = b.core.db.lock().unwrap().query_row("SELECT run FROM pact_calls WHERE call=?1", params![n], |r| r.get(0)).unwrap();
    b.call("wait", json!({"summon": run, "timeout_ms": 20000})).unwrap()
}

#[test]
fn call_starts_callee_run_as_pact_caller() {
    let (a, b) = agreed("");
    let out = a.call("pact_call", json!({"pact": "order-parts", "args": {"amount": 10}})).unwrap();
    assert_eq!(out["call"], 1, "{out}");
    let done = callee_run(&b, 1);
    assert_eq!(done["outcome"], "returned", "{done}");
    let caller: String = b.core.db.lock().unwrap().query_row("SELECT familiar FROM summons WHERE id=?1", params![done["id"].as_str().unwrap()], |r| r.get(0)).unwrap();
    assert_eq!(caller, "pact:order-parts");
    stop(&[&a, &b]);
}

#[test]
fn call_runs_under_callee_grants_only() {
    let (a, b) = agreed("");
    b.call("revoke", json!({"ritual": "move-one", "scopes": [{"reagent": "mail", "all": true}], "on": {"folder": "Inbox"}})).unwrap();
    // The caller holding a grant for the same ritual changes nothing.
    a.call("pact_call", json!({"pact": "order-parts", "args": {"amount": 10}})).unwrap();
    let last = entries(&b, "order-parts").pop().unwrap();
    assert_eq!(last.kind, Kind::Deliver, "{last:?}");
    assert_eq!(last.data["outcome"], "refused", "{last:?}");
    stop(&[&a, &b]);
}

#[test]
fn fixed_inputs_cannot_be_overridden() {
    let (a, b) = agreed("");
    let e = a.call("pact_call", json!({"pact": "order-parts", "args": {"amount": 1, "threshold": 0}})).unwrap_err();
    assert!(e.message.contains("threshold"), "{}", e.message);
    stop(&[&a, &b]);
}

#[test]
fn unknown_input_refused() {
    let (a, b) = agreed("");
    let e = a.call("pact_call", json!({"pact": "order-parts", "args": {"amount": 1, "bogus": 2}})).unwrap_err();
    assert!(e.message.contains("bogus"), "{}", e.message);
    stop(&[&a, &b]);
}

#[test]
fn per_call_limit_is_minimum() {
    // Ritual essence 20 tool calls, the pact's per-call 6, the callee's local 3.
    let (a, b) = agreed_with("", &[("per_call: {tool_calls: 20", "per_call: {tool_calls: 6")]);
    assert!(b.call("pact_limit", json!({"pact": "order-parts", "limits": {"tool_calls": 9}})).is_err(), "looser than the pact");
    b.call("pact_limit", json!({"pact": "order-parts", "limits": {"tool_calls": 3}})).unwrap();
    a.call("pact_call", json!({"pact": "order-parts", "args": {"amount": 1}})).unwrap();
    let done = callee_run(&b, 1);
    let essence: String = b.core.db.lock().unwrap().query_row("SELECT essence FROM summons WHERE id=?1", params![done["id"].as_str().unwrap()], |r| r.get(0)).unwrap();
    let essence: Value = serde_json::from_str(&essence).unwrap();
    assert_eq!(essence["tool_calls"], 3, "{essence}");
    stop(&[&a, &b]);
}

#[test]
fn total_calls_ceiling_refused_both_sides() {
    let (a, b) = agreed_with("", &[("total: {calls: 5}", "total: {calls: 1}")]);
    a.call("pact_call", json!({"pact": "order-parts", "args": {"amount": 1}})).unwrap();
    let e = a.call("pact_call", json!({"pact": "order-parts", "args": {"amount": 2}})).unwrap_err();
    assert!(e.message.contains("calls"), "{}", e.message);
    // A caller that skips its own check is refused by the callee too.
    let row = a.core.pact_row("order-parts").unwrap();
    a.core.caller_append(&row, Kind::Call, Some(2), json!({"inputs": {"amount": 2}, "asked_by": ""})).unwrap();
    let last = entries(&b, "order-parts").pop().unwrap();
    assert_eq!((last.kind, last.data["outcome"].as_str()), (Kind::Deliver, Some("refused")), "{last:?}");
    stop(&[&a, &b]);
}

#[test]
fn familiar_needs_pact_allow() {
    let (a, b) = agreed("");
    a.call("familiar_add", json!({"name": "f", "key": crate::keys::generate().public().text()})).unwrap();
    let call = |allowed: &[String]| a.core.dispatch_as("familiar:f", allowed, "pact_call", json!({"pact": "order-parts", "args": {"amount": 1}}));
    assert_eq!(call(&[]).unwrap_err().code, "not_found");
    let allowed = a.call("familiar_allow", json!({"name": "f", "rituals": [], "pacts": ["order-parts"]})).unwrap();
    let list: Vec<String> = serde_json::from_value(allowed["allowed"].clone()).unwrap();
    assert!(list.contains(&"pact:order-parts".to_string()), "{allowed}");
    assert_eq!(call(&list).unwrap()["call"], 1);
    stop(&[&a, &b]);
}

#[test]
fn familiar_sees_only_own_calls() {
    let (a, b) = agreed("");
    let allowed = vec!["pact:order-parts".to_string()];
    a.core.dispatch_as("familiar:f", &allowed, "pact_call", json!({"pact": "order-parts", "args": {"amount": 1}})).unwrap();
    a.core.dispatch_as("familiar:g", &allowed, "pact_call", json!({"pact": "order-parts", "args": {"amount": 2}})).unwrap();
    let shown = a.core.dispatch_as("familiar:f", &allowed, "pact_show", json!({"pact": "order-parts"})).unwrap();
    let calls = shown["calls"].as_array().unwrap();
    assert_eq!(calls.len(), 1, "{shown}");
    assert_eq!(calls[0]["asked_by"], "familiar:f");
    let listed = a.core.dispatch_as("familiar:f", &[], "pact_list", json!({})).unwrap();
    assert_eq!(listed.as_array().unwrap().len(), 0, "unallowed pacts are hidden");
    stop(&[&a, &b]);
}

#[test]
fn pact_caller_is_agent() {
    assert!(super::signed::is_agent("pact:order-parts"));
}

// ------------------------------------------------------------ holds

fn inbox_count(b: &Home) -> usize {
    let mail = b.core.cfg.mail_root.clone().unwrap();
    std::fs::read_dir(std::path::Path::new(&mail).join("Inbox")).unwrap().count()
}

fn pending(h: &Home) -> Vec<Value> {
    h.call("petitions", json!({})).unwrap().as_array().unwrap().clone()
}

fn decide(h: &Home, id: &str, permit: bool) -> Result<Value, RpcErr> {
    h.call("petition_decide", json!({"ids": [id], "permit": permit}))
}

fn petition_status(h: &Home, id: &str) -> String {
    h.core.db.lock().unwrap().query_row("SELECT status FROM petitions WHERE id=?1", params![id], |r| r.get(0)).unwrap()
}

/// Make call 1 for `amount`, let B's run end, and let A see the delivery.
fn held_call(a: &Home, b: &Home, amount: i64) -> pact::Entry {
    a.call("pact_call", json!({"pact": "order-parts", "args": {"amount": amount}})).unwrap();
    callee_run(b, 1);
    a.call("pact_poll", json!({})).unwrap();
    entries(b, "order-parts").into_iter().find(|e| e.kind == Kind::Deliver).expect("delivered")
}

#[test]
fn caller_hold_when_true_raises_caller_petition() {
    let (a, b) = agreed("");
    let delivered = held_call(&a, &b, 5000);
    let held = delivered.data["held"].as_array().unwrap();
    assert_eq!(held.len(), 1, "{delivered:?}");
    assert_eq!(held[0]["needs_caller"], true);
    assert_eq!(held[0]["needs_callee"], false);
    assert_eq!(inbox_count(&b), 1, "nothing moved yet");
    let mine = pending(&a);
    assert_eq!(mine.len(), 1, "{mine:?}");
    assert_eq!(mine[0]["operation"], "pact.decide");
    stop(&[&a, &b]);
}

#[test]
fn caller_hold_when_false_runs_unheld() {
    let (a, b) = agreed("");
    let delivered = held_call(&a, &b, 10);
    assert!(delivered.data["held"].as_array().unwrap().is_empty(), "{delivered:?}");
    assert_eq!(inbox_count(&b), 0, "moved");
    assert!(pending(&a).iter().all(|p| p["operation"] != "pact.decide"), "no held action for the caller");
    stop(&[&a, &b]);
}

#[test]
fn both_holds_need_both_permits() {
    let (a, b) = agreed_on(callee_home_holding(), "", &[]);
    held_call(&a, &b, 5000);
    let theirs = pending(&b);
    assert_eq!(theirs.len(), 1, "{theirs:?}");
    let out = decide(&b, theirs[0]["id"].as_str().unwrap(), true).unwrap();
    assert_eq!(petition_status(&b, theirs[0]["id"].as_str().unwrap()), "pending", "{out}");
    assert_eq!(inbox_count(&b), 1, "waits for the caller too");
    let mine = pending(&a);
    decide(&a, mine[0]["id"].as_str().unwrap(), true).unwrap();
    assert_eq!(inbox_count(&b), 0, "performed after the second permit");
    a.call("pact_poll", json!({})).unwrap();
    let settled = entries(&a, "order-parts").into_iter().find(|e| e.kind == Kind::Settle).expect("settled");
    assert_eq!(settled.data["state"], "applied", "{settled:?}");
    stop(&[&a, &b]);
}

#[test]
fn deny_from_either_is_final() {
    let (a, b) = agreed_on(callee_home_holding(), "", &[]);
    held_call(&a, &b, 5000);
    let mine = pending(&a);
    decide(&a, mine[0]["id"].as_str().unwrap(), false).unwrap();
    let theirs: Vec<String> = b.core.db.lock().unwrap().prepare("SELECT id FROM petitions").unwrap().query_map([], |r| r.get(0)).unwrap().map(Result::unwrap).collect();
    assert_eq!(petition_status(&b, &theirs[0]), "denied");
    assert!(decide(&b, &theirs[0], true).is_err(), "a deny is final");
    assert_eq!(inbox_count(&b), 1);
    stop(&[&a, &b]);
}

#[test]
fn callee_owner_cannot_decide_caller_only_hold() {
    let (a, b) = agreed("");
    held_call(&a, &b, 5000);
    let theirs = pending(&b);
    let e = decide(&b, theirs[0]["id"].as_str().unwrap(), true).unwrap_err();
    assert_eq!(e.code, "not_authorized", "{}", e.message);
    assert_eq!(inbox_count(&b), 1);
    stop(&[&a, &b]);
}

#[test]
fn deadline_expires_without_caller_contact() {
    let (a, b) = agreed("");
    held_call(&a, &b, 5000);
    let theirs = pending(&b);
    let id = theirs[0]["id"].as_str().unwrap().to_string();
    // The caller is gone; the callee's own clock passes the deadline.
    b.core.expire_due(now_ms() + 2 * 3_600_000);
    assert_eq!(petition_status(&b, &id), "expired");
    assert_eq!(inbox_count(&b), 1, "never performed");
    let last = entries(&b, "order-parts").pop().unwrap();
    assert_eq!((last.kind, last.data["state"].as_str()), (Kind::Settle, Some("expired")), "{last:?}");
    a.call("pact_poll", json!({})).unwrap();
    let mine: String = a.core.db.lock().unwrap().query_row("SELECT status FROM petitions WHERE operation='pact.decide'", [], |r| r.get(0)).unwrap();
    assert_eq!(mine, "expired");
    assert!(entries(&a, "order-parts").iter().any(|e| e.kind == Kind::Settle));
    stop(&[&a, &b]);
}

#[test]
fn caller_hold_cannot_exceed_callee_grants() {
    let (a, b) = agreed("");
    held_call(&a, &b, 5000);
    b.call("revoke", json!({"ritual": "move-one", "scopes": [{"reagent": "mail", "all": true}], "on": {"folder": "Inbox"}})).unwrap();
    let mine = pending(&a);
    decide(&a, mine[0]["id"].as_str().unwrap(), true).unwrap();
    assert_eq!(inbox_count(&b), 1, "the callee's grants still bound the write");
    let last = entries(&b, "order-parts").pop().unwrap();
    assert_eq!(last.kind, Kind::Settle);
    assert_ne!(last.data["state"], "applied", "{last:?}");
    stop(&[&a, &b]);
}

// ------------------------------------------------------------ delivery

/// Restart B and point A at its new `listen` address.
fn restart_callee(a: &Home, b: Home) -> Home {
    let b = b.restart();
    let url = serve(&b);
    a.call("peer_remove", json!({"name": "b"})).unwrap();
    a.call("peer_add", json!({"name": "b", "url": url, "key": b.core.key.public().text()})).unwrap();
    b
}

#[test]
fn delivery_carries_verifiable_epitaph() {
    let (a, b) = agreed("");
    held_call(&a, &b, 10);
    let t = pact::parse_terms(&serde_json::to_string(&a.call("pact_show", json!({"pact": "order-parts"})).unwrap()["terms"]).unwrap()).unwrap();
    let d = entries(&a, "order-parts").into_iter().find(|e| e.kind == Kind::Deliver).expect("the caller holds the delivery");
    pact::check_delivery_epitaph(&d.data, &b.core.key.public(), &t.ritual.hash).unwrap();
    let body: Value = serde_json::from_str(d.data["epitaph"]["body"].as_str().unwrap()).unwrap();
    assert!(body["fingerprints"]["inputs"].is_string(), "{body}");
    assert_eq!(d.data["outcome"], "returned");
    stop(&[&a, &b]);
}

#[test]
fn bad_epitaph_signature_rejected_by_caller() {
    let (a, b) = agreed("");
    held_call(&a, &b, 10);
    let id = a.call("pact_show", json!({"pact": "order-parts"})).unwrap()["id"].as_str().unwrap().to_string();
    let forged = crate::epitaph::seal(&crate::keys::generate(), 1, crate::epitaph::ZEROS, &json!({"v": 1}));
    {
        let mut db = b.core.db.lock().unwrap();
        let tx = db.transaction().unwrap();
        append_own(&tx, &b.core.key, &id, Kind::Deliver, Some(1), json!({"outcome": "returned", "run": "s-x", "held": [],
            "epitaph": {"link": forged.link, "body": forged.body, "sig": forged.sig}})).unwrap();
        tx.commit().unwrap();
    }
    let polled = a.call("pact_poll", json!({})).unwrap();
    assert!(!polled["errors"].as_array().unwrap().is_empty(), "{polled}");
    assert_eq!(state(&a, "order-parts"), "frozen");
    let shown = a.call("pact_show", json!({"pact": "order-parts"})).unwrap();
    assert!(shown["warnings"].to_string().contains("epitaph"), "{shown}");
    assert_eq!(entries(&a, "order-parts").iter().filter(|e| e.kind == Kind::Deliver).count(), 1, "the forged delivery is not stored");
    stop(&[&a, &b]);
}

#[test]
fn interrupted_pact_run_delivered_failed_after_restart() {
    let (a, b) = agreed("");
    let id = b.call("pact_show", json!({"pact": "order-parts"})).unwrap()["id"].as_str().unwrap().to_string();
    let hash = move_one_hash(&b);
    {
        let db = b.core.db.lock().unwrap();
        db.execute("INSERT INTO summons (id, ritual, version, hash, status, started, inputs, familiar) VALUES ('s-lost', 'move-one', 1, ?1, 'running', ?2, '{}', 'pact:order-parts')", params![hash, now_ms()]).unwrap();
        db.execute("INSERT INTO invocations (caller, key, request_hash, summon) VALUES ('pact:order-parts', ?1, 'x', 's-lost')", params![format!("pact:{id}:1")]).unwrap();
        db.execute("INSERT INTO pact_calls (pact, call, state, run, deadline) VALUES (?1, 1, 'running', 's-lost', ?2)", params![id, now_ms() + 3_600_000]).unwrap();
    }
    let b = restart_callee(&a, b);
    let d = entries(&b, "order-parts").into_iter().find(|e| e.kind == Kind::Deliver).expect("delivered on recovery");
    assert_eq!(d.data["outcome"], "failed", "{d:?}");
    assert_eq!(d.call, Some(1));
    stop(&[&a, &b]);
}

#[test]
fn held_petition_survives_callee_restart_and_settles() {
    let (a, b) = agreed("");
    held_call(&a, &b, 5000);
    let b = restart_callee(&a, b);
    let mine = pending(&a);
    decide(&a, mine[0]["id"].as_str().unwrap(), true).unwrap();
    assert_eq!(inbox_count(&b), 0, "performed after the restart");
    let last = entries(&b, "order-parts").pop().unwrap();
    assert_eq!((last.kind, last.data["state"].as_str()), (Kind::Settle, Some("applied")), "{last:?}");
    stop(&[&a, &b]);
}

#[test]
fn poll_backs_off_when_idle() {
    use super::pacts::next_poll_ms;
    let mut t = 15_000;
    let mut seen = vec![];
    for _ in 0..6 {
        t = next_poll_ms(t, false);
        seen.push(t);
    }
    assert_eq!(seen, vec![30_000, 60_000, 120_000, 240_000, 300_000, 300_000]);
    assert_eq!(next_poll_ms(300_000, true), 15_000);
}

// ------------------------------------------------------------ acceptance

fn call_state(h: &Home, n: i64) -> String {
    h.core.db.lock().unwrap().query_row("SELECT state FROM pact_calls WHERE call=?1", params![n], |r| r.get(0)).unwrap()
}

fn acceptance_petitions(h: &Home) -> Vec<Value> {
    pending(h).into_iter().filter(|p| p["operation"] == "pact.accept").collect()
}

#[test]
fn checks_mode_accepts_on_pass() {
    let (a, b) = agreed("accept: checks\n");
    held_call(&a, &b, 10);
    assert_eq!(call_state(&a, 1), "accepted");
    let last = entries(&b, "order-parts").pop().unwrap();
    assert_eq!((last.kind, last.data["decided_by"].as_str()), (Kind::Accept, Some("checks")), "{last:?}");
    assert_eq!(call_state(&b, 1), "accepted");
    stop(&[&a, &b]);
}

#[test]
fn checks_mode_rejects_when_ritual_says_no() {
    let (a, b) = agreed_with("accept: checks\n", &[("fixed: {threshold: 1000}", "fixed: {threshold: 100000}")]);
    held_call(&a, &b, 1500);
    assert_eq!(call_state(&a, 1), "rejected");
    let last = entries(&a, "order-parts").pop().unwrap();
    assert_eq!(last.kind, Kind::Reject, "{last:?}");
    assert!(last.data["reason"].as_str().unwrap().contains("1000"), "{last:?}");
    stop(&[&a, &b]);
}

#[test]
fn owner_mode_raises_acceptance_petition() {
    let (a, b) = agreed("");
    held_call(&a, &b, 10);
    let asks = acceptance_petitions(&a);
    assert_eq!(asks.len(), 1, "{asks:?}");
    assert_eq!(asks[0]["because"]["checks"]["ritual"]["accept"], true, "{asks:?}");
    assert_eq!(call_state(&a, 1), "settled");
    a.call("pact_accept", json!({"pact": "order-parts", "call": 1})).unwrap();
    assert_eq!(call_state(&a, 1), "accepted");
    assert!(acceptance_petitions(&a).is_empty());
    let last = entries(&b, "order-parts").pop().unwrap();
    assert_eq!((last.kind, last.data["decided_by"].as_str()), (Kind::Accept, Some("owner")));
    stop(&[&a, &b]);
}

#[test]
fn owner_rejects_with_reason() {
    let (a, b) = agreed("");
    held_call(&a, &b, 10);
    a.call("pact_reject", json!({"pact": "order-parts", "call": 1, "reason": "late"})).unwrap();
    assert_eq!(call_state(&a, 1), "rejected");
    let last = entries(&b, "order-parts").pop().unwrap();
    assert_eq!((last.kind, last.data["reason"].as_str()), (Kind::Reject, Some("late")));
    stop(&[&a, &b]);
}

#[test]
fn only_owner_accepts() {
    let (a, b) = agreed("");
    held_call(&a, &b, 10);
    let e = a.core.dispatch_as("familiar:f", &["pact:order-parts".to_string()], "pact_accept", json!({"pact": "order-parts", "call": 1})).unwrap_err();
    assert_eq!(e.code, "not_authorized");
    stop(&[&a, &b]);
}

#[test]
fn acceptance_waits_for_settle() {
    let (a, b) = agreed("");
    held_call(&a, &b, 5000);
    assert!(acceptance_petitions(&a).is_empty(), "a held action is still open");
    let decide_id = pending(&a)[0]["id"].as_str().unwrap().to_string();
    decide(&a, &decide_id, true).unwrap();
    a.call("pact_poll", json!({})).unwrap();
    assert_eq!(acceptance_petitions(&a).len(), 1);
    stop(&[&a, &b]);
}

#[test]
fn oversized_or_malformed_result_rejected_cleanly() {
    let (a, b) = agreed("");
    let t = pact::parse_terms(&serde_json::to_string(&a.call("pact_show", json!({"pact": "order-parts"})).unwrap()["terms"]).unwrap()).unwrap();
    let schema = json!({"type": "object", "required": ["amount"], "properties": {"amount": {"type": "integer"}}});
    let big = json!("x".repeat(1 << 20));
    let checks = a.core.acceptance_checks(&t, Some(&schema), &big);
    assert_eq!(checks["accept"], false, "{}", &checks.to_string()[..200]);
    let checks = a.core.acceptance_checks(&t, Some(&schema), &json!({"amount": "ten"}));
    assert_eq!(checks["accept"], false);
    assert!(checks["reason"].as_str().unwrap().contains("schema") || checks["reason"].as_str().unwrap().contains("amount"), "{checks}");
    stop(&[&a, &b]);
}

#[test]
fn failed_delivery_skips_acceptance() {
    let (a, b) = agreed("");
    b.call("revoke", json!({"ritual": "move-one", "scopes": [{"reagent": "mail", "all": true}], "on": {"folder": "Inbox"}})).unwrap();
    a.call("pact_call", json!({"pact": "order-parts", "args": {"amount": 10}})).unwrap();
    a.call("pact_poll", json!({})).unwrap();
    assert_eq!(call_state(&a, 1), "closed");
    assert!(acceptance_petitions(&a).is_empty());
    stop(&[&a, &b]);
}

#[test]
fn returned_delivery_without_epitaph_freezes() {
    let (a, b) = agreed("");
    let id = a.call("pact_show", json!({"pact": "order-parts"})).unwrap()["id"].as_str().unwrap().to_string();
    {
        let mut db = b.core.db.lock().unwrap();
        let tx = db.transaction().unwrap();
        append_own(&tx, &b.core.key, &id, Kind::Deliver, Some(1), json!({"outcome": "returned", "result": {"amount": 1}, "held": []})).unwrap();
        tx.commit().unwrap();
    }
    a.call("pact_poll", json!({})).unwrap();
    assert_eq!(state(&a, "order-parts"), "frozen");
    stop(&[&a, &b]);
}

// ------------------------------------------------------------ revise, end, verify

fn pact_id_of(h: &Home, name: &str) -> String {
    h.call("pact_show", json!({"pact": name})).unwrap()["id"].as_str().unwrap().to_string()
}

#[test]
fn revision_supersedes_and_old_calls_finish() {
    let (a, b) = agreed("");
    let old = pact_id_of(&a, "order-parts");
    held_call(&a, &b, 5000); // call 1 stays open on a caller hold
    let file = terms_file(&a, &a, &b, &move_one_hash(&b), &format!("revises: {old}\n"));
    let text = std::fs::read_to_string(&file).unwrap().replace("total: {calls: 5}", "total: {calls: 7}");
    std::fs::write(&file, text).unwrap();
    let rev = a.call("pact_offer", json!({"peer": "b", "file": file, "name": "order-parts-2"})).unwrap();
    b.call("pact_agree", json!({"pact": rev["id"]})).unwrap();
    assert_eq!(state(&b, &old), "superseded");
    a.call("pact_poll", json!({})).unwrap();
    assert_eq!(state(&a, &old), "superseded");
    assert_eq!(state(&a, "order-parts-2"), "agreed");
    assert!(a.call("pact_call", json!({"pact": old, "args": {"amount": 1}})).is_err(), "no new calls on the old pact");
    // The call already open on the old pact still finishes.
    let mine = pending(&a).into_iter().find(|p| p["operation"] == "pact.decide").unwrap();
    decide(&a, mine["id"].as_str().unwrap(), true).unwrap();
    assert_eq!(inbox_count(&b), 0);
    a.call("pact_poll", json!({})).unwrap();
    assert!(entries(&a, &old).iter().any(|e| e.kind == Kind::Settle), "settled after the end");
    stop(&[&a, &b]);
}

#[test]
fn revision_must_name_same_cores() {
    let (a, b) = agreed("");
    let file = terms_file(&a, &a, &b, &move_one_hash(&b), &format!("revises: {}\n", "d".repeat(64)));
    let e = a.call("pact_offer", json!({"peer": "b", "file": file, "name": "x"})).unwrap_err();
    assert!(e.message.contains("revises"), "{}", e.message);
    stop(&[&a, &b]);
}

#[test]
fn ended_pact_refuses_calls() {
    let (a, b) = agreed("");
    a.call("pact_end", json!({"pact": "order-parts", "reason": "done"})).unwrap();
    assert_eq!(state(&a, "order-parts"), "ended");
    assert_eq!(state(&b, "order-parts"), "ended");
    assert!(a.call("pact_call", json!({"pact": "order-parts", "args": {"amount": 1}})).is_err());
    stop(&[&a, &b]);
}

#[test]
fn expired_pact_refuses_calls() {
    let (a, b) = agreed_with("", &[("expires: 2099-01-01", "expires: 2001-01-01")]);
    let e = a.call("pact_call", json!({"pact": "order-parts", "args": {"amount": 1}})).unwrap_err();
    assert!(e.message.contains("expired"), "{}", e.message);
    stop(&[&a, &b]);
}

#[test]
fn export_verifies_on_other_side_identically() {
    let (a, b) = agreed("accept: checks\n");
    held_call(&a, &b, 10);
    a.call("pact_poll", json!({})).unwrap();
    let fa = a.path("a.json");
    let fb = b.path("b.json");
    a.call("pact_export", json!({"pact": "order-parts", "file": fa})).unwrap();
    b.call("pact_export", json!({"pact": "order-parts", "file": fb})).unwrap();
    let ea: Value = serde_json::from_str(&std::fs::read_to_string(&fa).unwrap()).unwrap();
    let eb: Value = serde_json::from_str(&std::fs::read_to_string(&fb).unwrap()).unwrap();
    assert_eq!(ea["entries"], eb["entries"], "both hold the same thread");
    let checked = b.call("pact_verify", json!({"file": fa})).unwrap();
    assert_eq!(checked["ok"], true, "{checked}");
    assert_eq!(checked["epitaphs_checked"], 1, "{checked}");
    let history = a.call("pact_history", json!({"pact": "order-parts"})).unwrap();
    assert!(history["entries"].as_array().unwrap().iter().all(|e| e["verified"] == true), "{history}");
    stop(&[&a, &b]);
}

#[test]
fn verify_export_flags_fork() {
    let (a, b) = agreed("");
    let f = a.path("a.json");
    a.call("pact_export", json!({"pact": "order-parts", "file": f})).unwrap();
    let mut export: Value = serde_json::from_str(&std::fs::read_to_string(&f).unwrap()).unwrap();
    let id = pact_id_of(&a, "order-parts");
    let first: pact::Signed = serde_json::from_value(export["entries"][0].clone()).unwrap();
    // A second offer by the same core on the same (empty) start.
    let twin = pact::seal_entry(&a.core.key, &id, 1, crate::epitaph::ZEROS, Kind::Offer, None, 7, json!({"other": true}));
    assert_ne!(twin.hash, first.hash);
    export["entries"].as_array_mut().unwrap().push(serde_json::to_value(&twin).unwrap());
    std::fs::write(&f, export.to_string()).unwrap();
    let checked = b.call("pact_verify", json!({"file": f})).unwrap();
    assert_eq!(checked["ok"], false, "{checked}");
    assert_eq!(checked["forks"].as_array().unwrap().len(), 1, "{checked}");
    stop(&[&a, &b]);
}

#[test]
fn verify_never_trusts_file_keys() {
    let (a, b) = agreed("");
    let f = a.path("a.json");
    a.call("pact_export", json!({"pact": "order-parts", "file": f})).unwrap();
    let stranger = Home::new("");
    let e = stranger.call("pact_verify", json!({"file": f})).unwrap_err();
    assert!(e.message.contains("pin"), "{}", e.message);
    stop(&[&a, &b]);
}

#[test]
fn anonymous_agent_cannot_use_pacts() {
    let (a, b) = agreed("");
    for (method, params) in [
        ("pact_list", json!({"caller": "agent"})),
        ("pact_show", json!({"pact": "order-parts", "caller": "agent"})),
        ("pact_call", json!({"pact": "order-parts", "args": {"amount": 1}, "caller": "agent"})),
    ] {
        let e = a.call(method, params).unwrap_err();
        assert_eq!(e.code, "not_authorized", "{method}: {}", e.message);
    }
    stop(&[&a, &b]);
}

#[test]
fn multiple_queued_outbox_entries_flush_in_order_without_fork() {
    let (a, b) = agreed("");
    let real_url: String = a.core.db.lock().unwrap().query_row("SELECT url FROM peers WHERE name='b'", [], |r| r.get(0)).unwrap();
    // Point peer b to an unreachable port so two calls queue in pact_outbox.
    a.core.db.lock().unwrap().execute("UPDATE peers SET url='http://127.0.0.1:9' WHERE name='b'", []).unwrap();
    let c1 = a.call("pact_call", json!({"pact": "order-parts", "args": {"amount": 10}})).unwrap();
    let c2 = a.call("pact_call", json!({"pact": "order-parts", "args": {"amount": 20}})).unwrap();
    assert_eq!(c1["state"], "queued");
    assert_eq!(c2["state"], "queued");
    // Restore peer b URL and poll: both queued calls must flush sequentially without a fork.
    a.core.db.lock().unwrap().execute("UPDATE peers SET url=?1 WHERE name='b'", params![real_url]).unwrap();
    let polled = a.call("pact_poll", json!({})).unwrap();
    assert_eq!(polled["synced"], 1, "{polled}");
    assert_eq!(state(&a, "order-parts"), "agreed");
    assert_eq!(state(&b, "order-parts"), "agreed");
    let forks_b: i64 = b.core.db.lock().unwrap().query_row("SELECT COUNT(*) FROM pact_forks", [], |r| r.get(0)).unwrap();
    assert_eq!(forks_b, 0, "honest queued flushes must never fork");
    stop(&[&a, &b]);
}

#[test]
fn expired_or_denied_hold_closes_without_acceptance_even_under_checks() {
    let (a, b) = agreed("accept: checks\n");
    held_call(&a, &b, 5000);
    let mine = pending(&a);
    assert_eq!(mine.len(), 1);
    decide(&a, mine[0]["id"].as_str().unwrap(), false).unwrap();
    a.call("pact_poll", json!({})).unwrap();
    assert_eq!(call_state(&a, 1), "closed", "denied hold must close, never accept");
    assert_eq!(call_state(&b, 1), "closed", "callee call must also close");
    assert!(!entries(&a, "order-parts").iter().any(|e| matches!(e.kind, Kind::Accept | Kind::Reject)));
    stop(&[&a, &b]);
}

#[test]
fn removed_peer_on_callee_refuses_further_entries() {
    let (a, b) = agreed("");
    b.call("peer_remove", json!({"name": "a"})).unwrap();
    let res = a.call("pact_call", json!({"pact": "order-parts", "args": {"amount": 10}})).unwrap_err();
    assert_eq!(res.code, "not_authorized", "{res:?}");
    stop(&[&a, &b]);
}

#[test]
fn multiparty_pact_with_witness_and_callee_push() {
    let a = Home::new("");
    let b = callee_home();
    let w = Home::new("");
    let url_a = serve(&a);
    let url_b = serve(&b);
    let url_w = serve(&w);

    for (me, peer, url, other) in [
        (&a, "b", &url_b, &b),
        (&a, "w", &url_w, &w),
        (&b, "a", &url_a, &a),
        (&b, "w", &url_w, &w),
        (&w, "a", &url_a, &a),
        (&w, "b", &url_b, &b),
    ] {
        me.call("peer_add", json!({"name": peer, "url": url, "key": other.core.key.public().text()})).unwrap();
        me.call("pact_admit", json!({"name": peer, "key": other.core.key.public().text()})).unwrap();
    }

    let hash = b.call("contract", json!({"name": "move-one"}))
        .unwrap()["hash"]
        .as_str()
        .unwrap()
        .to_string();
    let f = terms_file(
        &a,
        &a,
        &b,
        &hash,
        &format!("witnesses:\n  - {}\n", w.core.key.public().text()),
    );
    a.call("pact_offer", json!({"peer": "b", "file": f})).unwrap();
    b.call("pact_agree", json!({"pact": "order-parts"})).unwrap();

    // Callee push delivers `Agree` to both caller `a` and witness `w` without polling.
    for _ in 0..40 {
        if state(&a, "order-parts") == "agreed" && state(&w, "order-parts") == "agreed" {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(25));
    }
    assert_eq!(state(&a, "order-parts"), "agreed");
    assert_eq!(state(&w, "order-parts"), "agreed");
    stop(&[&a, &b, &w]);
}
