//! Epitaphs are written when runs end, chained, and hold no contents.
use super::test_home::{repo, Home};
use super::*;

const ZEROS: &str = "0000000000000000000000000000000000000000000000000000000000000000";

/// (seq, run, link, body, sig, hash) for every epitaph, in chain order.
pub(super) fn epitaphs(core: &Core) -> Vec<(i64, String, String, String, String, String)> {
    let db = core.db.lock().unwrap();
    let mut q = db.prepare("SELECT seq, run, link, body, sig, hash FROM epitaphs ORDER BY seq").unwrap();
    let rows = q
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?, r.get(5)?)))
        .unwrap();
    rows.collect::<Result<Vec<_>, _>>().unwrap()
}

fn receipt(h: &Home, text: &str) -> String {
    let done = h.run(json!({"target": "total-receipt", "args": {"text": text}}));
    done["id"].as_str().unwrap().to_string()
}

fn home() -> Home {
    let h = Home::new("");
    h.inscribe(&repo("tests/fixtures/rituals/total-receipt.yaml"), false);
    h
}

#[test]
fn every_run_gets_one_epitaph() {
    let h = home();
    let id = receipt(&h, r#"{"amounts_minor":[1,2]}"#);
    let all = epitaphs(&h.core);
    assert_eq!(all.len(), 1);
    assert_eq!(all[0].1, id);
    let body: Value = serde_json::from_str(&all[0].3).unwrap();
    assert_eq!(body["outcome"], "returned");
    assert_eq!(body["ritual"]["name"], "total-receipt");
    assert_eq!(body["run"], id.as_str());
    assert!(body["fingerprints"]["events"]["count"].as_i64().unwrap() > 0);
}

#[test]
fn epitaph_verifies_with_core_key() {
    let h = home();
    receipt(&h, r#"{"amounts_minor":[1]}"#);
    let (_, _, link, body, sig, hash) = epitaphs(&h.core).remove(0);
    let parsed = crate::epitaph::check_link(&link, &sig, &h.core.key.public()).unwrap();
    crate::epitaph::check_body(&parsed, &body).unwrap();
    assert_eq!(hash, crate::epitaph::hash(&link));
}

#[test]
fn chain_links_in_order() {
    let h = home();
    for n in 1..=3 {
        receipt(&h, &format!(r#"{{"amounts_minor":[{n}]}}"#));
    }
    let all = epitaphs(&h.core);
    assert_eq!(all.iter().map(|e| e.0).collect::<Vec<_>>(), [1, 2, 3]);
    let prevs: Vec<String> = all
        .iter()
        .map(|e| serde_json::from_str::<Value>(&e.2).unwrap()["prev"].as_str().unwrap().to_string())
        .collect();
    assert_eq!(prevs[0], ZEROS);
    assert_eq!(prevs[1], all[0].5);
    assert_eq!(prevs[2], all[1].5);
}

#[test]
fn children_are_listed_by_parent() {
    let (h, mail) = Home::triage();
    let done = h.run(json!({"target":"inbox-triage","model":"mock/keyword","args":{"count":1,"apply":false}}));
    let parent = done["id"].as_str().unwrap().to_string();
    let all = epitaphs(&h.core);
    let parent_body: Value = serde_json::from_str(&all.iter().find(|e| e.1 == parent).unwrap().3).unwrap();
    let child_hashes: Vec<String> = all.iter().filter(|e| e.1 != parent).map(|e| e.5.clone()).collect();
    assert!(!child_hashes.is_empty(), "inbox-triage runs a child ritual");
    assert_eq!(parent_body["children"], json!(child_hashes));
    let parent_seq = all.iter().find(|e| e.1 == parent).unwrap().0;
    assert!(all.iter().filter(|e| e.1 != parent).all(|e| e.0 < parent_seq), "children end first");
    let _ = std::fs::remove_dir_all(mail);
}

#[test]
fn epitaph_holds_no_contents() {
    let h = home();
    receipt(&h, r#"{"amounts_minor":[1],"note":"MARKER-7f3a"}"#);
    for e in epitaphs(&h.core) {
        assert!(!e.2.contains("MARKER-7f3a") && !e.3.contains("MARKER-7f3a"), "contents leaked into an epitaph");
    }
}

#[test]
fn interrupted_runs_get_epitaphs_and_large_batches_stay_small() {
    let root = std::env::temp_dir().join(new_id("summonr-epitaph-recover"));
    let p = Paths { runtime: root.join("run"), state: root.join("state"), config: root.join("config") };
    drop(Core::open(p.clone()).unwrap());
    {
        // A run the core was in the middle of, with a thousand events.
        let db = rusqlite::Connection::open(p.db()).unwrap();
        db.execute(
            "INSERT INTO summons (id, ritual, version, hash, status, started, inputs, essence) VALUES ('s-big', 'r', 1, 'h', 'running', 1, '{}', '{}')",
            [],
        )
        .unwrap();
        for seq in 1..=1000 {
            db.execute(
                "INSERT INTO events (summon, seq, ts, kind, text, data) VALUES ('s-big', ?1, 1, 'op', 'x', '{\"n\":1}')",
                params![seq],
            )
            .unwrap();
        }
    }
    let c = Core::open(p.clone()).unwrap();
    let all = epitaphs(&c);
    assert_eq!(all.len(), 1);
    let body: Value = serde_json::from_str(&all[0].3).unwrap();
    assert_eq!(body["outcome"], "interrupted");
    assert_eq!(body["fingerprints"]["events"]["count"], 1001, "a thousand events plus the recovery's end");
    assert!(all[0].3.len() < 4096, "body is {} bytes", all[0].3.len());
    drop(c);
    let _ = std::fs::remove_dir_all(root);
}

// ------------------------------------------------------------------ verify

fn four_runs() -> (Home, Vec<String>) {
    let h = home();
    let ids = (1..=4).map(|n| receipt(&h, &format!(r#"{{"amounts_minor":[{n}]}}"#))).collect();
    (h, ids)
}

fn problem_seq(h: &Home) -> i64 {
    let v = h.call("chain_verify", json!({})).unwrap();
    assert_eq!(v["ok"], false, "{v}");
    v["problem"]["seq"].as_i64().unwrap()
}

fn exec(h: &Home, sql: &str) {
    h.core.db.lock().unwrap().execute_batch(sql).unwrap();
}

#[test]
fn chain_verify_ok_on_an_honest_chain() {
    let (h, _) = four_runs();
    let v = h.call("chain_verify", json!({})).unwrap();
    assert_eq!(v["ok"], true, "{v}");
    assert_eq!(v["checked"], 4);
    assert_eq!(v["not_checkable"], json!([]));
}

#[test]
fn altered_link_is_caught() {
    let (h, _) = four_runs();
    exec(&h, "UPDATE epitaphs SET link = replace(link, '\"chain\":1', '\"chain\":1 ') WHERE seq = 3");
    assert_eq!(problem_seq(&h), 3);
}

#[test]
fn deleted_middle_epitaph_is_caught() {
    let (h, _) = four_runs();
    exec(&h, "DELETE FROM epitaphs WHERE seq = 2");
    assert_eq!(problem_seq(&h), 2);
}

#[test]
fn reordered_epitaphs_are_caught() {
    let (h, _) = four_runs();
    exec(&h, "UPDATE epitaphs SET seq = 99 WHERE seq = 2; UPDATE epitaphs SET seq = 2 WHERE seq = 3; UPDATE epitaphs SET seq = 3 WHERE seq = 99;");
    assert_eq!(problem_seq(&h), 2);
}

#[test]
fn edited_elegy_event_is_caught() {
    let (h, ids) = four_runs();
    exec(&h, &format!("UPDATE events SET data = '{{\"edited\":true}}' WHERE summon = '{}' AND seq = 1", ids[1]));
    assert_eq!(problem_seq(&h), 2);
}

#[test]
fn deleted_elegy_is_not_checkable_not_tampered() {
    let (h, ids) = four_runs();
    exec(&h, &format!("DELETE FROM events WHERE summon = '{0}'; DELETE FROM summons WHERE id = '{0}';", ids[2]));
    let v = h.call("chain_verify", json!({})).unwrap();
    assert_eq!(v["ok"], true, "{v}");
    assert_eq!(v["not_checkable"], json!([ids[2]]));
    assert_eq!(v["checked"], 4);
}

/// Rewrite the whole chain with the real key, as a dishonest owner could.
fn reseal_all(h: &Home, change_first: impl Fn(&mut Value)) {
    reseal_all_core(&h.core, change_first)
}

pub(super) fn reseal_all_core(core: &Core, change_first: impl Fn(&mut Value)) {
    let rows = epitaphs(core);
    let mut prev = ZEROS.to_string();
    for (i, (seq, run, _, body, _, _)) in rows.iter().enumerate() {
        let mut body: Value = serde_json::from_str(body).unwrap();
        if i == 0 {
            change_first(&mut body);
        }
        let s = crate::epitaph::seal(&core.key, *seq, &prev, &body);
        core.db.lock().unwrap().execute(
            "UPDATE epitaphs SET link=?2, body=?3, sig=?4, hash=?5 WHERE seq=?1",
            params![seq, s.link, s.body, s.sig, s.hash],
        ).unwrap();
        let _ = run;
        prev = s.hash;
    }
}

#[test]
fn mark_survives_appends_and_fails_after_rewrite() {
    let (h, _) = four_runs();
    let head = h.call("chain_head", json!({})).unwrap();
    let mark = crate::epitaph::mark(2, epitaphs(&h.core)[1].5.as_str());
    assert_eq!(head["seq"], 4);
    receipt(&h, r#"{"amounts_minor":[9]}"#);
    let ok = h.call("chain_verify", json!({"mark": mark})).unwrap();
    assert_eq!(ok["mark"]["added_since"], 3, "{ok}");
    // Every epitaph re-signed after editing the first: the chain itself verifies...
    reseal_all(&h, |b| b["outcome"] = json!("failed"));
    let v = h.call("chain_verify", json!({"mark": mark})).unwrap();
    // ...but the mark shared earlier no longer matches.
    assert_eq!(v["ok"], false, "{v}");
    assert!(v["mark"]["error"].as_str().unwrap().contains("seq 2"), "{v}");
}

#[test]
fn reveal_verifies_and_changed_value_fails() {
    let (h, ids) = four_runs();
    for part in ["inputs", "event:2"] {
        let r = h.call("epitaph_reveal", json!({"run": ids[0], "part": part})).unwrap();
        let e = &r["epitaph"];
        let link = crate::epitaph::check_link(e["link"].as_str().unwrap(), e["sig"].as_str().unwrap(), &h.core.key.public()).unwrap();
        let body = crate::epitaph::check_body(&link, e["body"].as_str().unwrap()).unwrap();
        crate::epitaph::verify_reveal(&body, &r["reveal"]).unwrap();
        let mut forged = r["reveal"].clone();
        forged["bytes"] = json!("{\"amounts_minor\":[999]}");
        assert!(crate::epitaph::verify_reveal(&body, &forged).is_err(), "{part}");
        let mut wrong_salt = r["reveal"].clone();
        wrong_salt["salt"] = json!("00".repeat(16));
        assert!(crate::epitaph::verify_reveal(&body, &wrong_salt).is_err(), "{part}");
    }
}

#[test]
fn export_verifies_offline_with_only_the_key() {
    let (h, _) = four_runs();
    let export = h.call("epitaph_export", json!({"from": 1, "to": 4})).unwrap();
    assert_eq!(crate::epitaph::verify_export(&export, &h.core.key.public()), Ok(4));
    assert_eq!(crate::epitaph::verify_export(&export, &crate::keys::generate().public()).unwrap_err().0, 1);
    let mut cut = export.clone();
    cut["epitaphs"].as_array_mut().unwrap().remove(1);
    assert_eq!(crate::epitaph::verify_export(&cut, &h.core.key.public()).unwrap_err().0, 2, "the missing epitaph is named");
    let mark = crate::epitaph::mark(2, epitaphs(&h.core)[1].5.as_str());
    assert_eq!(crate::epitaph::verify_mark(&export, &crate::epitaph::parse_mark(&mark).unwrap()), Ok(2));
}

#[test]
fn epitaph_get_shows_one_run() {
    let (h, ids) = four_runs();
    let e = h.call("epitaph_get", json!({"run": ids[3]})).unwrap();
    assert_eq!(e["seq"], 4);
    assert_eq!(e["verifies"], true);
    assert_eq!(e["core_key"], h.core.key.public().text());
    assert_eq!(h.call("epitaph_get", json!({"run": "s-none"})).unwrap_err().exit, 3);
}

// ------------------------------------------------------ final-review fixes

#[test]
fn a_null_piece_written_later_is_caught() {
    let (h, ids) = four_runs();
    exec(&h, &format!("UPDATE summons SET error = 'forged failure' WHERE id = '{}'", ids[1]));
    assert_eq!(problem_seq(&h), 2);
}

#[test]
fn the_runs_recorded_claims_must_match_its_epitaph() {
    for (field, value) in [("outcome", "'failed'"), ("ritual", "'other'"), ("version", "99"), ("ended", "1"), ("familiar", "'familiar:x'")] {
        let (h, ids) = four_runs();
        exec(&h, &format!("UPDATE summons SET {field} = {value} WHERE id = '{}'", ids[2]));
        assert_eq!(problem_seq(&h), 3, "{field}");
    }
}

#[test]
fn relabelling_an_epitaphs_run_is_caught() {
    let (h, ids) = four_runs();
    exec(&h, &format!("UPDATE epitaphs SET run = 'nope' WHERE seq = 2; UPDATE summons SET result = '{{}}' WHERE id = '{}'", ids[1]));
    assert_eq!(problem_seq(&h), 2);
}

#[test]
fn deleting_every_event_of_a_present_run_is_tampering() {
    let (h, ids) = four_runs();
    exec(&h, &format!("DELETE FROM events WHERE summon = '{}'", ids[1]));
    assert_eq!(problem_seq(&h), 2);
}

#[test]
fn a_malformed_salt_is_a_problem_not_a_crash() {
    let (h, ids) = four_runs();
    exec(&h, &format!("UPDATE epitaph_salts SET salt = 'ab' WHERE run = '{}' AND part = 'inputs'", ids[0]));
    assert_eq!(problem_seq(&h), 1);
    assert!(h.call("chain_head", json!({})).is_ok(), "the core keeps working");
}

#[test]
fn child_epitaphs_record_who_asked_for_the_root() {
    let (h, mail) = Home::triage();
    let started = h.call("invoke", json!({"target":"inbox-triage","model":"mock/keyword","caller":"agent","args":{"count":1,"apply":false}})).unwrap();
    h.call("wait", json!({"summon": started["summon"], "timeout_ms": 10000})).unwrap();
    let all = epitaphs(&h.core);
    assert!(all.len() > 1);
    for e in &all {
        let body: Value = serde_json::from_str(&e.3).unwrap();
        assert_eq!(body["caller"], "agent", "{}", e.1);
    }
    assert_eq!(h.call("chain_verify", json!({})).unwrap()["ok"], true);
    let _ = std::fs::remove_dir_all(mail);
}

#[test]
fn an_event_reveal_binds_its_number() {
    let (h, ids) = four_runs();
    let r = h.call("epitaph_reveal", json!({"run": ids[0], "part": "event:2"})).unwrap();
    let e = &r["epitaph"];
    let link = crate::epitaph::check_link(e["link"].as_str().unwrap(), e["sig"].as_str().unwrap(), &h.core.key.public()).unwrap();
    let body = crate::epitaph::check_body(&link, e["body"].as_str().unwrap()).unwrap();
    // Relabel: claim event 2's bytes are event 3, swapping the labels on their fingerprints.
    let mut relabelled = r["reveal"].clone();
    relabelled["part"] = json!("event:3");
    // Keep the fingerprints in order (so the root still matches) and swap only their labels.
    let fps = relabelled["fps"].as_array_mut().unwrap();
    fps[1]["seq"] = json!(3);
    fps[2]["seq"] = json!(2);
    assert!(crate::epitaph::verify_reveal(&body, &relabelled).is_err());
}

#[test]
fn a_recovered_parent_lists_its_recovered_children() {
    let root = std::env::temp_dir().join(new_id("summonr-epitaph-tree"));
    let p = Paths { runtime: root.join("run"), state: root.join("state"), config: root.join("config") };
    drop(Core::open(p.clone()).unwrap());
    {
        // A parent inserted before its child, both cut off by a crash.
        let db = rusqlite::Connection::open(p.db()).unwrap();
        db.execute_batch(
            "INSERT INTO summons (id, ritual, version, hash, status, started, inputs, essence) VALUES ('s-a-parent', 'r', 1, 'h', 'running', 1, '{}', '{}');
             INSERT INTO summons (id, parent, ritual, version, hash, status, started, inputs, essence) VALUES ('s-b-child', 's-a-parent', 'c', 1, 'h', 'running', 2, '{}', '{}');",
        )
        .unwrap();
    }
    let c = Core::open(p.clone()).unwrap();
    let all = epitaphs(&c);
    let child = all.iter().find(|e| e.1 == "s-b-child").unwrap().clone();
    let parent = all.iter().find(|e| e.1 == "s-a-parent").unwrap().clone();
    let body: Value = serde_json::from_str(&parent.3).unwrap();
    assert_eq!(body["children"], json!([child.5]), "the child is sealed first and listed");
    assert!(child.0 < parent.0);
    drop(c);
    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn a_marks_count_comes_from_signed_positions() {
    let (h, _) = four_runs();
    let mut export = h.call("epitaph_export", json!({"from": 1, "to": 4})).unwrap();
    // Relabel the unsigned positions in the file.
    for e in export["epitaphs"].as_array_mut().unwrap() {
        e["seq"] = json!(e["seq"].as_i64().unwrap() + 100);
    }
    let mark = crate::epitaph::parse_mark(&crate::epitaph::mark(2, epitaphs(&h.core)[1].5.as_str())).unwrap();
    assert_eq!(crate::epitaph::verify_mark(&export, &mark), Ok(2));
}

#[test]
fn post_run_petition_decisions_are_sealed_in_the_epitaph_chain() {
    let h = Home::new("");
    let dir = h.root.join("files");
    std::fs::create_dir_all(&dir).unwrap();
    let root = dir.to_str().unwrap();
    h.inscribe_def(json!({
        "ritual": "held-move",
        "reagents": {"file": {"select": ["write_text"]}},
        "requires": [{"reagent": "file", "action": "write_text", "on": {"root": root}}],
        "steps": [{
            "id": "m",
            "operation": "file.write_text",
            "arguments": {"root": root, "path": "note.txt", "text": "hi"},
            "hold": true
        }]
    }), false);
    h.call("grant", json!({"ritual": "held-move", "scopes": [{"reagent": "file", "select": ["write_text"]}], "on": {"root": root}})).unwrap();
    let run = h.run(json!({"target": "held-move", "args": {}}));
    assert_eq!(run["outcome"], "returned");
    let before = epitaphs(&h.core);
    assert_eq!(before.len(), 1);

    let pending = h.call("petitions", json!({"status": "pending"})).unwrap();
    let pid = pending[0]["id"].as_str().unwrap().to_string();
    h.call("petition_decide", json!({"ids": [pid], "permit": true})).unwrap();

    let after = epitaphs(&h.core);
    assert_eq!(after.len(), 2);
    assert_eq!(after[1].1, format!("petition:{pid}"));
    let verify = h.call("chain_verify", json!({})).unwrap();
    assert_eq!(verify["ok"], true, "{verify}");

    // Tampering with the stored petition status is caught by chain_verify.
    h.core.db.lock().unwrap().execute(
        "UPDATE petitions SET status='denied' WHERE id=?1",
        rusqlite::params![pid],
    ).unwrap();
    let tampered = h.call("chain_verify", json!({})).unwrap();
    assert_eq!(tampered["ok"], false, "{tampered}");
    assert_eq!(tampered["problem"]["seq"], 2);
}

#[test]
fn merkle_inclusion_proofs_verify_and_detect_tampering() {
    let (h, runs) = four_runs();
    let head = h.call("chain_head", json!({})).unwrap();
    let root = head["merkle_root"].as_str().unwrap();
    assert_eq!(head["count"], 4);

    for run in &runs {
        let p = h.call("epitaph_prove", json!({"run": run})).unwrap();
        let leaf = p["leaf_hash"].as_str().unwrap();
        let proof: Vec<crate::epitaph::MerkleStep> =
            serde_json::from_value(p["proof"].clone()).unwrap();
        assert!(crate::epitaph::verify_merkle_proof(leaf, root, &proof));
        assert!(!crate::epitaph::verify_merkle_proof(
            "0000000000000000000000000000000000000000000000000000000000000000",
            root,
            &proof
        ));
    }
}

#[test]
fn brief_wait_and_get_drop_the_bulk_but_keep_the_result() {
    let (h, _mail) = Home::triage();
    let started = h.call("invoke", json!({"target":"inbox-triage","model":"mock/keyword","args":{"count":1,"apply":false}})).unwrap();
    let full = h.call("wait", json!({"summon": started["summon"], "timeout_ms": 10000})).unwrap();
    assert!(full.get("effects").is_some());
    for brief in [
        h.call("wait", json!({"summon": started["summon"], "timeout_ms": 10000, "brief": true})).unwrap(),
        h.call("get", json!({"summon": started["summon"], "brief": true})).unwrap(),
    ] {
        assert_eq!(brief["id"], full["id"]);
        assert_eq!(brief["outcome"], full["outcome"]);
        assert_eq!(brief["result"], full["result"]);
        assert_eq!(brief["requests"], full["requests"]);
        assert!(brief.get("effects").is_none(), "{brief}");
        assert!(brief.get("children").is_some(), "child run ids are listed");
    }
}
