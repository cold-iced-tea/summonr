//! Assays: isolation, admission, modes, fingerprints and touchstones.
use super::test_home::Home;
use std::path::Path;
use super::*;

/// One case that moves the fixture receipt for real unless the assay isolates it.
fn write_set(h: &Home) -> String {
    let path = h.path("effects.json");
    std::fs::write(
        &path,
        json!({"assay_set":"effects","cases":[{"id":"write","inputs":{"count":1,"apply":true,"hold":false},"expect":{"unscored":true}}]}).to_string(),
    )
    .unwrap();
    path
}

fn assay(h: &Home, set: &str, extra: Value) -> Result<Value, RpcErr> {
    let mut p = json!({"ritual":"inbox-triage","against":set,"model":"mock/keyword","cwd":"/"});
    for (k, v) in extra.as_object().unwrap() {
        p[k] = v.clone();
    }
    h.call("assay", p)
}

#[test]
fn assay_simulates_writes_by_default() {
    let (h, mail) = Home::triage();
    let set = write_set(&h);
    let out = assay(&h, &set, json!({"baseline": true})).unwrap();
    assert!(mail.join("mail/Inbox/001.eml").exists(), "{out}");
    assert!(!mail.join("mail/Receipts/001.eml").exists());
    assert_eq!(out["mode"], "simulated", "{out}");
    let _ = std::fs::remove_dir_all(mail);
}

#[test]
fn live_assay_performs_writes() {
    let (h, mail) = Home::triage();
    let set = write_set(&h);
    let out = assay(&h, &set, json!({"baseline": true, "live": true})).unwrap();
    assert!(mail.join("mail/Receipts/001.eml").exists(), "{out}");
    assert_eq!(out["mode"], "live", "{out}");
    let _ = std::fs::remove_dir_all(mail);
}

#[test]
fn assay_respects_active_run_limit() {
    let (h, mail) = Home::triage();
    let set = write_set(&h);
    // Occupy the only slot the way a running summon would.
    h.core
        .active
        .lock()
        .unwrap()
        .extend((0..16).map(|i| (format!("s-busy-{i}"), Arc::new(AtomicBool::new(false)))));
    let err = assay(&h, &set, json!({"baseline": true})).unwrap_err();
    assert_eq!(err.code, "busy", "{}", err.message);
    let _ = std::fs::remove_dir_all(mail);
}

#[test]
fn mode_mismatch_is_inconclusive() {
    let (h, mail) = Home::triage();
    let set = write_set(&h);
    assay(&h, &set, json!({"baseline": true})).unwrap();
    let out = assay(&h, &set, json!({"live": true})).unwrap();
    assert_eq!(out["drift"]["verdict"], "inconclusive", "{out}");
    assert!(out["drift"].to_string().contains("mode"), "{out}");
    let _ = std::fs::remove_dir_all(mail);
}

#[test]
fn old_format_baseline_is_inconclusive() {
    let (h, mail) = Home::triage();
    let set = write_set(&h);
    assay(&h, &set, json!({"baseline": true})).unwrap();
    // A baseline recorded before fingerprints covered every model site.
    h.core
        .db
        .lock()
        .unwrap()
        .execute("UPDATE baselines SET fingerprint = '{\"backend\":\"mock\",\"model\":\"keyword\"}'", [])
        .unwrap();
    let out = assay(&h, &set, json!({})).unwrap();
    assert_eq!(out["drift"]["verdict"], "inconclusive", "{out}");
    assert!(out["drift"]["reasons"].to_string().contains("re-record"), "{out}");
    let _ = std::fs::remove_dir_all(mail);
}

#[test]
fn fingerprint_lists_every_model_site() {
    let (h, mail) = Home::triage();
    let set = write_set(&h);
    let out = assay(&h, &set, json!({"baseline": true})).unwrap();
    // inbox-triage's classify step lives in the child ritual classify-message.
    assert!(out["fingerprint"]["sites"]["mock/keyword"].is_object(), "{out}");
    let _ = std::fs::remove_dir_all(mail);
}

// ------------------------------------------------------------- touchstones

fn route_form_home() -> Home {
    let h = Home::new("");
    h.inscribe(&super::test_home::repo("examples/04-route-form/route-form.yaml"), false);
    h.inscribe(&super::test_home::repo("examples/04-route-form/route-form-touchstone.yaml"), false);
    h
}

fn form(category: &str) -> Value {
    json!({"form": json!({"category": category, "message": "hello there"}).to_string()})
}

fn assay_route_form(h: &Home, set: Value) -> Result<Value, RpcErr> {
    let path = h.path(&format!("{}.json", new_id("set")));
    std::fs::write(&path, set.to_string()).unwrap();
    h.call(
        "assay",
        json!({"ritual":"route-form","against":path,"model":"mock/keyword","cwd":"/","baseline":true}),
    )
}

#[test]
fn assay_set_with_touchstone_scores_cases() {
    let h = route_form_home();
    let out = assay_route_form(
        &h,
        json!({"assay_set":"t","touchstone":"route-form-touchstone","cases":[
            {"id":"a","inputs":form("support")},
            {"id":"b","inputs":form("sales")}
        ]}),
    )
    .unwrap();
    assert_eq!(out["account"]["correct"], 2, "{out}");
    assert_eq!(out["account"]["touchstone_errors"], 0, "{out}");
    assert_eq!(out["touchstone"]["name"], "route-form-touchstone", "{out}");
}

#[test]
fn assay_touchstone_error_is_inconclusive() {
    let h = route_form_home();
    assay_route_form(
        &h,
        json!({"assay_set":"t","touchstone":"route-form-touchstone","cases":[{"id":"a","inputs":form("support")}]}),
    )
    .unwrap();
    // A case the touchstone's schema rejects, so it cannot compute an answer.
    let bad = h.path("bad.json");
    std::fs::write(
        &bad,
        json!({"assay_set":"t","touchstone":"route-form-touchstone","cases":[{"id":"a","inputs":{"form":"not json"}}]}).to_string(),
    )
    .unwrap();
    let out = h
        .call("assay", json!({"ritual":"route-form","against":bad,"model":"mock/keyword","cwd":"/"}))
        .unwrap();
    assert_eq!(out["account"]["touchstone_errors"], 1, "{out}");
    assert_eq!(out["drift"]["verdict"], "inconclusive", "{out}");
    assert!(out["drift"]["reasons"].to_string().contains("touchstone"), "{out}");
}

#[test]
fn assay_refuses_mixed_touchstone_and_expect() {
    let h = route_form_home();
    let err = assay_route_form(
        &h,
        json!({"assay_set":"t","touchstone":"route-form-touchstone","cases":[
            {"id":"a","inputs":form("support"),"expect":{"from_form":"support"}}
        ]}),
    )
    .unwrap_err();
    assert_eq!(err.exit, 2, "{}", err.message);
    assert!(err.message.contains("touchstone"), "{}", err.message);
}

#[test]
fn gleaned_cases_score_against_touchstone() {
    let h = route_form_home();
    for c in ["support", "press"] {
        h.run(json!({"target":"route-form","model":"mock/keyword","args":form(c)}));
    }
    let gleaned = h.call("glean", json!({"ritual":"route-form","count":5})).unwrap();
    assert_eq!(gleaned["cases"].as_array().unwrap().len(), 2, "{gleaned}");
    let out = assay_route_form(
        &h,
        json!({"assay_set":"g","touchstone":"route-form-touchstone","cases":gleaned["cases"]}),
    )
    .unwrap();
    assert_eq!(out["account"]["correct"], 2, "{out}");
}

// ------------------------------------------------------ final-review fixes

#[test]
fn changed_touchstone_makes_comparison_inconclusive() {
    let h = route_form_home();
    assay_route_form(
        &h,
        json!({"assay_set":"t","touchstone":"route-form-touchstone","cases":[{"id":"a","inputs":form("support")}]}),
    )
    .unwrap();
    // A new touchstone version with different logic.
    let v2 = h.path("touchstone-v2.yaml");
    std::fs::write(
        &v2,
        "ritual: route-form-touchstone\ninputs: {form: {type: string}}\nreagents: {data: {select: [trim]}}\nessence: {requests: 0}\nsteps:\n  - id: t\n    operation: data.trim\n    arguments: {text: x}\nreturn: {from_form: sales}\n",
    )
    .unwrap();
    h.inscribe(Path::new(&v2), true);
    let out = h
        .call("assay", json!({"ritual":"route-form","model":"mock/keyword","cwd":"/"}))
        .unwrap();
    assert_eq!(out["drift"]["verdict"], "inconclusive", "{out}");
    assert!(out["drift"]["reasons"].to_string().contains("touchstone changed"), "{out}");
}

#[test]
fn baseline_with_touchstone_errors_is_refused() {
    let h = route_form_home();
    let err = assay_route_form(
        &h,
        json!({"assay_set":"t","touchstone":"route-form-touchstone","cases":[{"id":"a","inputs":{"form":"not json"}}]}),
    )
    .unwrap_err();
    assert!(err.message.contains("touchstone"), "{}", err.message);
    let n: i64 = h.core.db.lock().unwrap().query_row("SELECT COUNT(*) FROM baselines", [], |r| r.get(0)).unwrap();
    assert_eq!(n, 0, "no baseline may be recorded over unscored cases");
}

#[test]
fn refuse_policy_with_old_baseline_says_re_record() {
    let h = Home::new("");
    h.inscribe_def(
        json!({"ritual":"guarded","model":"mock/keyword","drift":{"on_mismatch":"refuse"},"inputs":{"text":{"type":"string"}},
               "steps":[{"id":"c","model":{"prompt":"${inputs.text}"}}],"return":{"c":"${steps.c.output}"}}),
        false,
    );
    h.core
        .db
        .lock()
        .unwrap()
        .execute(
            "INSERT INTO baselines (ritual, version, model, set_name, set_hash, set_json, fingerprint, account, created) VALUES ('guarded',1,'mock/keyword','s','h','{}','{\"backend\":\"mock\"}','{}',0)",
            [],
        )
        .unwrap();
    let err = h
        .call("invoke", json!({"target":"guarded","args":{"text":"hi"}}))
        .unwrap_err();
    assert!(err.message.contains("re-record"), "{}", err.message);
}

#[test]
fn assay_compare_paired_contingency_and_deltas() {
    let h = Home::new("");
    let a = json!([
        {"case": "c1", "correct": true,  "ms": 100, "tokens": 10},
        {"case": "c2", "correct": true,  "ms": 200, "tokens": 20},
        {"case": "c3", "correct": false, "ms": 300, "tokens": 30},
        {"case": "c4", "correct": false, "ms": 400, "tokens": 40},
        {"case": "only_in_a", "correct": true, "ms": 50, "tokens": 5}
    ]);
    let b = json!([
        {"case": "c1", "correct": true,  "ms": 120, "tokens": 12},
        {"case": "c2", "correct": false, "ms": 220, "tokens": 22},
        {"case": "c3", "correct": true,  "ms": 320, "tokens": 32},
        {"case": "c4", "correct": false, "ms": 420, "tokens": 42},
        {"case": "only_in_b1", "correct": false, "ms": 60, "tokens": 6},
        {"case": "only_in_b2", "correct": true, "ms": 70, "tokens": 7}
    ]);
    let out = h.core.assay_compare(json!({"a": a, "b": b})).unwrap();
    assert_eq!(out["matched"], 4);
    assert_eq!(out["unmatched_a"], 1);
    assert_eq!(out["unmatched_b"], 2);
    assert_eq!(out["both_correct"], 1);
    assert_eq!(out["only_a_correct"], 1);
    assert_eq!(out["only_b_correct"], 1);
    assert_eq!(out["both_wrong"], 1);
    assert!((out["accuracy_a"].as_f64().unwrap() - 0.5).abs() < 1e-9);
    assert!((out["accuracy_b"].as_f64().unwrap() - 0.5).abs() < 1e-9);
    assert!((out["accuracy_delta"].as_f64().unwrap() - 0.0).abs() < 1e-9);
    assert!((out["mean_ms_delta"].as_f64().unwrap() - 20.0).abs() < 1e-9);
    assert_eq!(out["token_delta"], 8);
    assert!(!out["mcnemar"]["significant_at_05"].as_bool().unwrap());
}

#[test]
fn assay_compare_mcnemar_threshold_and_zero_discordant() {
    let h = Home::new("");
    // 6 discordant pairs (0 vs 6): chi2 = (6 - 1)^2 / 6 = 25/6 = 4.1667 >= 3.841
    let a6: Vec<Value> = (0..6)
        .map(|i| json!({"case": format!("c{i}"), "correct": false}))
        .collect();
    let b6: Vec<Value> = (0..6)
        .map(|i| json!({"case": format!("c{i}"), "correct": true}))
        .collect();
    let sig = h.core.assay_compare(json!({"a": a6, "b": b6})).unwrap();
    let chi2 = sig["mcnemar"]["chi2"].as_f64().unwrap();
    assert!((chi2 - 25.0 / 6.0).abs() < 1e-9, "chi2={chi2}");
    assert_eq!(sig["mcnemar"]["significant_at_05"], true);

    // 5 discordant pairs (0 vs 5): chi2 = (5 - 1)^2 / 5 = 3.2 < 3.841
    let a5: Vec<Value> = (0..5)
        .map(|i| json!({"case": format!("c{i}"), "correct": false}))
        .collect();
    let b5: Vec<Value> = (0..5)
        .map(|i| json!({"case": format!("c{i}"), "correct": true}))
        .collect();
    let nonsig = h.core.assay_compare(json!({"a": a5, "b": b5})).unwrap();
    assert!((nonsig["mcnemar"]["chi2"].as_f64().unwrap() - 3.2).abs() < 1e-9);
    assert_eq!(nonsig["mcnemar"]["significant_at_05"], false);

    // 0 discordant pairs: chi2 = 0.0, significant_at_05 = false
    let same = json!([{"case": "c0", "correct": true}, {"case": "c1", "correct": false}]);
    let zero = h.core.assay_compare(json!({"a": same, "b": same})).unwrap();
    assert_eq!(zero["mcnemar"]["chi2"], 0.0);
    assert_eq!(zero["mcnemar"]["significant_at_05"], false);

    // Equal discordant pairs (1 vs 1): Yates-corrected chi2 must be 0.0, not 0.5.
    let eq_a = json!([{"case": "c0", "correct": true}, {"case": "c1", "correct": false}]);
    let eq_b = json!([{"case": "c0", "correct": false}, {"case": "c1", "correct": true}]);
    let tied = h.core.assay_compare(json!({"a": eq_a, "b": eq_b})).unwrap();
    assert_eq!(tied["mcnemar"]["chi2"], 0.0);
    assert_eq!(tied["mcnemar"]["significant_at_05"], false);
}

#[test]
fn assay_compare_accepts_saved_series_ids() {
    let h = route_form_home();
    let set = json!({"assay_set":"t","touchstone":"route-form-touchstone","cases":[
        {"id":"a","inputs":form("support")},
        {"id":"b","inputs":form("sales")}
    ]});
    let out_a = assay_route_form(&h, set.clone()).unwrap();
    let out_b = assay_route_form(&h, set).unwrap();
    let cmp = h
        .core
        .assay_compare(json!({"a": out_a["series"], "b": out_b["series"]}))
        .unwrap();
    assert_eq!(cmp["matched"], 2, "{cmp}");
    assert_eq!(cmp["both_correct"], 2, "{cmp}");
    assert_eq!(cmp["unmatched_a"], 0, "{cmp}");
    assert_eq!(cmp["unmatched_b"], 0, "{cmp}");
}

#[test]
fn laboratory_batch_gate_example_scores_the_two_cases() {
    let h = Home::new("{}");
    let examples = Path::new(env!("CARGO_MANIFEST_DIR")).join("examples/10-bughunt");
    h.inscribe(&examples.join("batch-ready.yaml"), false);
    let report = h.call("assay", json!({
        "ritual": "bughunt-batch-ready",
        "against": examples.join("batch-ready.assay.yaml"),
        "live": false
    })).unwrap();
    assert_eq!(report["account"]["correct"], 2, "{report}");
    assert_eq!(report["trials"][0]["got"]["pass"], true);
    assert_eq!(report["trials"][1]["got"]["pass"], false);
}
