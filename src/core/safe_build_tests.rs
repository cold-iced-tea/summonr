//! Final-review rulings for the safe build kit, driven through the core RPC.
use super::*;
use std::path::Path;

use super::test_home::{repo, Home};

fn triage(extra: Value) -> Value {
    let mut p = json!({"target":"inbox-triage","model":"mock/keyword",
        "args":{"count":1,"apply":true,"hold":false}});
    for (k, v) in extra.as_object().unwrap() {
        if k == "hold" {
            p["args"]["hold"] = v.clone();
        } else {
            p[k] = v.clone();
        }
    }
    p
}

fn move_effect(done: &Value) -> &Value {
    &done["result"]["triage"][0]["effects"][0]
}

fn restore(mailbox: &Path) {
    if mailbox.join("Receipts/001.eml").exists() {
        std::fs::rename(
            mailbox.join("Receipts/001.eml"),
            mailbox.join("Inbox/001.eml"),
        )
        .unwrap();
    }
}

fn kinds(cassette: &str) -> Vec<String> {
    let c: Value = serde_json::from_str(&std::fs::read_to_string(cassette).unwrap()).unwrap();
    c["entries"]
        .as_array()
        .unwrap()
        .iter()
        .map(|e| {
            e["operation"]
                .as_str()
                .map(String::from)
                .unwrap_or_else(|| format!("model:{}", e["step"].as_str().unwrap()))
        })
        .collect()
}

// ---------------------------------------------- Ruling 25: replay simulates

#[test]
fn plain_replay_of_a_live_write_is_simulated_and_changes_nothing() {
    let (h, mail) = Home::triage();
    let mailbox = mail.join("mail");
    let cassette = h.path("live.json");
    let live = h.run(triage(json!({"record": cassette})));
    assert_eq!(live["outcome"], "returned", "{live}");
    assert_eq!(move_effect(&live)["state"], "applied", "{live}");
    assert!(kinds(&cassette).iter().any(|k| k == "mail.move"));
    restore(&mailbox);

    let replayed = h.run(triage(json!({"replay": cassette})));
    assert_eq!(replayed["outcome"], "returned", "{replayed}");
    assert!(mailbox.join("Inbox/001.eml").exists());
    assert!(!mailbox.join("Receipts/001.eml").exists());
    let effect = move_effect(&replayed);
    assert_eq!(effect["state"], "simulated", "{replayed}");
    assert_eq!(effect["evidence"]["class"], "simulated by the runtime");
    assert!(!replayed.to_string().contains("observed by the runtime"));
    std::fs::remove_dir_all(&mail).ok();
}

#[test]
fn plain_replay_of_a_held_write_files_no_petition() {
    let (h, mail) = Home::triage();
    let cassette = h.path("live.json");
    h.run(triage(json!({"record": cassette})));
    restore(&mail.join("mail"));
    for _ in 0..2 {
        let replayed = h.run(triage(json!({"replay": cassette, "hold": true})));
        assert_eq!(replayed["outcome"], "returned", "{replayed}");
        assert_eq!(move_effect(&replayed)["state"], "simulated", "{replayed}");
        assert_eq!(move_effect(&replayed)["would_hold"], true, "{replayed}");
    }
    assert_eq!(h.petitions(), 0);
    assert!(mail.join("mail/Inbox/001.eml").exists());
    std::fs::remove_dir_all(&mail).ok();
}

#[test]
fn a_cassette_recorded_while_simulating_replays_without_simulating() {
    let (h, mail) = Home::triage();
    let cassette = h.path("sim.json");
    let sim = h.run(triage(json!({"record": cassette, "simulate": true})));
    assert_eq!(move_effect(&sim)["state"], "simulated", "{sim}");
    assert!(!kinds(&cassette).iter().any(|k| k == "mail.move"));
    let replayed = h.run(triage(json!({"replay": cassette})));
    assert_eq!(replayed["outcome"], "returned", "{replayed}");
    assert_eq!(move_effect(&replayed)["state"], "simulated", "{replayed}");
    assert!(mail.join("mail/Inbox/001.eml").exists());
    std::fs::remove_dir_all(&mail).ok();
}

// ------------------------------------- Ruling 27: pure operations are live

fn counting(values: Value, items: u64) -> Value {
    json!({"ritual":"counting","model":"mock/keyword",
        "reagents":{"data":{"select":["count","map","arithmetic"]}},
        "essence":{"requests":1,"tool_calls":10,"items":items,"time":"10s"},
        "steps":[
            {"id":"answer","model":{"prompt":"your receipt"}},
            {"id":"n","operation":"data.count","arguments":{"value":values}},
            {"id":"doubled","operation":"data.map","arguments":{"values":values,
                "operation":"data.arithmetic","arguments":{"left":"${item}","right":2,"operator":"multiply"}}}],
        "return":{"answer":"${steps.answer.output}","n":"${steps.n.output}","doubled":"${steps.doubled.output}"}})
}

#[test]
fn replay_computes_edited_data_steps_and_records_none() {
    let h = Home::new("");
    h.inscribe_def(counting(json!([1, 2, 3]), 10), false);
    let cassette = h.path("count.json");
    let live = h.run(json!({"target":"counting","record": cassette}));
    assert_eq!(live["outcome"], "returned", "{live}");
    assert_eq!(live["result"]["n"], 3);
    assert_eq!(kinds(&cassette), vec!["model:answer".to_string()]);

    // A different data argument after recording: still replays, and the
    // new value is computed.
    h.inscribe_def(counting(json!([1, 2, 3, 4]), 10), true);
    let replayed = h.run(json!({"target":"counting","replay": cassette}));
    assert_eq!(replayed["outcome"], "returned", "{replayed}");
    assert_eq!(replayed["result"]["n"], 4);
    assert_eq!(replayed["result"]["doubled"], json!([2, 4, 6, 8]));
    assert_eq!(replayed["result"]["answer"], live["result"]["answer"]);
}

#[test]
fn replayed_data_map_is_metered_as_live() {
    let h = Home::new("");
    h.inscribe_def(counting(json!([1, 2, 3]), 10), false);
    let cassette = h.path("count.json");
    h.run(json!({"target":"counting","record": cassette}));
    // Fewer items than the map needs: exhausted live, and so on replay.
    h.inscribe_def(counting(json!([1, 2, 3]), 2), true);
    let live = h.run(json!({"target":"counting"}));
    assert_eq!(live["outcome"], "exhausted", "{live}");
    let replayed = h.run(json!({"target":"counting","replay": cassette}));
    assert_eq!(replayed["outcome"], "exhausted", "{replayed}");
    assert_eq!(replayed["error"], live["error"]);
}

// ------------------------------- Ruling 26: the mode is part of the request

#[test]
fn an_idempotency_key_never_reuses_a_run_across_modes() {
    let h = Home::new("");
    h.inscribe_def(counting(json!([1]), 10), false);
    let first = h
        .core
        .dispatch("invoke", json!({"target":"counting","idempotency_key":"k"}))
        .unwrap();
    let again = h
        .core
        .dispatch("invoke", json!({"target":"counting","idempotency_key":"k"}))
        .unwrap();
    assert_eq!(again["reused"], true);
    let sim = h
        .core
        .dispatch(
            "invoke",
            json!({"target":"counting","idempotency_key":"k","simulate":true}),
        )
        .unwrap_err();
    assert_eq!(sim.exit, 5, "{sim:?}");
    // And the reverse: a simulated run is never handed to a live request.
    h.core
        .dispatch(
            "invoke",
            json!({"target":"counting","idempotency_key":"s","simulate":true}),
        )
        .unwrap();
    let live = h
        .core
        .dispatch("invoke", json!({"target":"counting","idempotency_key":"s"}))
        .unwrap_err();
    assert_eq!(live.exit, 5, "{live:?}");
    h.core
        .dispatch(
            "wait",
            json!({"summon": first["summon"], "timeout_ms": 10000}),
        )
        .unwrap();
}

// ------------------------------------------- Ruling 28: --record PATH guard

#[test]
fn recording_refuses_to_replace_a_file_that_is_not_a_cassette() {
    let h = Home::new("");
    h.inscribe_def(counting(json!([1]), 10), false);
    let notes = h.path("notes.txt");
    std::fs::write(&notes, "my notes").unwrap();
    let e = h
        .core
        .dispatch("invoke", json!({"target":"counting","record": notes}))
        .unwrap_err();
    assert_eq!(e.exit, 2, "{e:?}");
    assert_eq!(std::fs::read_to_string(&notes).unwrap(), "my notes");

    // An existing cassette is replaced.
    let cassette = h.path("c.json");
    h.run(json!({"target":"counting","record": cassette}));
    let done = h.run(json!({"target":"counting","record": cassette}));
    assert_eq!(done["outcome"], "returned", "{done}");
    assert_eq!(kinds(&cassette), vec!["model:answer".to_string()]);
}

// ------------------------------------------ Minor 5 and 6: cassette loading

#[test]
fn missing_cassettes_are_not_found_and_bad_ones_invalid() {
    let h = Home::new("");
    h.inscribe_def(counting(json!([1]), 10), false);
    let missing = h
        .core
        .dispatch(
            "invoke",
            json!({"target":"counting","replay": h.path("missing.json")}),
        )
        .unwrap_err();
    assert_eq!(missing.exit, 3, "{missing:?}");
    let garbage = h.path("garbage.json");
    std::fs::write(&garbage, "not a cassette").unwrap();
    let bad = h
        .core
        .dispatch("invoke", json!({"target":"counting","replay": garbage}))
        .unwrap_err();
    assert_eq!(bad.exit, 2, "{bad:?}");
    let unknown = h.path("unknown.json");
    std::fs::write(
        &unknown,
        json!({"cassette_version":1,"entries":[{"kind":"tool","operation":"http.get_text",
            "request_hash":"h","arguments":{},"output":null,
            "error":{"kind":"exploded","message":"m"}}]})
        .to_string(),
    )
    .unwrap();
    let e = h
        .core
        .dispatch("invoke", json!({"target":"counting","replay": unknown}))
        .unwrap_err();
    assert_eq!(e.exit, 2, "{e:?}");
    assert!(e.message.contains("exploded"), "{e:?}");
}

// --------------------------------------- Minor 8: owner-only modes in core

#[test]
fn an_agent_caller_cannot_use_arbitrary_host_cassette_paths_but_can_simulate_and_use_named_cassettes() {
    let h = Home::new("");
    h.inscribe_def(counting(json!([1]), 10), false);
    for mode in [
        json!({"record": h.path("a.json")}),
        json!({"replay": h.path("a.json")}),
    ] {
        let mut p = json!({"target":"counting","caller":"agent"});
        for (k, v) in mode.as_object().unwrap() {
            p[k] = v.clone();
        }
        let e = h.core.dispatch("invoke", p).unwrap_err();
        assert_eq!((e.code.as_str(), e.exit), ("not_authorized", 4), "{e:?}");
    }
    assert!(!Path::new(&h.path("a.json")).exists());
    let sim = h.run(json!({"target":"counting","caller":"agent","simulate":true}));
    assert_eq!(sim["outcome"], "returned");
    let rec = h.run(json!({"target":"counting","caller":"agent","cassette_record":"agent-c"}));
    assert_eq!(rec["outcome"], "returned");
    let rep = h.run(json!({"target":"counting","caller":"agent","cassette_replay":"agent-c"}));
    assert_eq!(rep["outcome"], "returned");
}

// ------------------------------------------ Minor 9: drills are admitted

#[test]
fn drills_respect_the_active_run_limit() {
    let h = Home::new("max_active_runs: 1\n");
    h.inscribe(&repo("examples/04-route-form/route-form.yaml"), false);
    let against = repo("examples/04-route-form/route-form.drill.yaml");
    let drill = || {
        h.core.dispatch(
            "drill",
            json!({"ritual":"route-form","against": against.to_str().unwrap()}),
        )
    };
    h.core
        .active
        .lock()
        .unwrap()
        .insert("busy".into(), Arc::new(AtomicBool::new(false)));
    let e = drill().unwrap_err();
    assert_eq!(e.exit, 5, "{e:?}");
    h.core.active.lock().unwrap().remove("busy");
    assert_eq!(drill().unwrap()["passed"], 3);
}
