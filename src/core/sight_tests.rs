//! Records, cassettes, revive, simulation and epitaphs carry image digests
//! only, never image bytes.
use super::epitaph_tests::epitaphs;
use super::test_home::Home;
use super::*;
use crate::reagents::sight_reagent::tests::PNG_1X1;
use base64::Engine;

fn b64(data: &[u8]) -> String {
    base64::engine::general_purpose::STANDARD.encode(data)
}

/// A home with a `scans/a.png` and a ritual that reads it, then asks the
/// model about it after `pad` extra tool steps.
fn sight_home(name: &str, essence: Value, pad: usize) -> (Home, PathBuf) {
    let h = Home::new("");
    let dir = h.root.join("scans");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("a.png"), PNG_1X1).unwrap();
    let root = dir.to_string_lossy().into_owned();
    let mut steps = vec![json!({"id":"page","operation":"sight.read_image","arguments":{"root":&root,"path":"a.png"}})];
    for i in 0..pad {
        steps.push(json!({"id":format!("n{i}"),"operation":"data.arithmetic","arguments":{"left":i,"right":1,"operator":"add"}}));
    }
    steps.push(json!({"id":"look","model":{"prompt":"what is this","images":["${steps.page.output}"]}}));
    h.inscribe_def(
        json!({
            "ritual": name, "model": "mock/keyword", "essence": essence,
            "reagents": {"sight": {"select": ["read_image"]}, "data": {"select": ["arithmetic"]}},
            "requires": [{"reagent":"sight","action":"read_image","on":{"root":&root}}],
            "steps": steps,
            "return": {"seen": "${steps.look.output}"}
        }),
        false,
    );
    h.call("grant", json!({"ritual": name, "scopes": [{"reagent":"sight","select":["read_image"]}], "on": {"root": &root}})).unwrap();
    h.call("grant", json!({"ritual": name, "scopes": [{"reagent":"data","all":true}]})).unwrap();
    (h, dir)
}

fn record_text(h: &Home, run: &str) -> String {
    let events = h.call("events", json!({"summon": run, "limit": 200})).unwrap();
    let got = h.call("get", json!({"summon": run})).unwrap();
    format!("{events}\n{got}")
}

fn assert_digest_only(text: &str) {
    assert!(!text.contains(&b64(PNG_1X1)), "image bytes leaked: {text}");
    assert!(text.contains(&crate::util::sha256_hex(PNG_1X1)), "digest missing: {text}");
}

#[test]
fn cassette_round_trip_with_images() {
    let (h, _) = sight_home("see", json!({"time":"10s"}), 0);
    let cassette = h.path("see.json");
    let rec = h.run(json!({"target":"see","record":cassette}));
    assert_eq!(rec["outcome"], "returned", "{rec}");
    let saved = std::fs::read_to_string(&cassette).unwrap();
    assert_digest_only(&saved);
    let c = crate::cassette::load(std::path::Path::new(&cassette)).unwrap();
    assert_eq!(c.entries.len(), 2, "one read_image tool entry and one model entry");

    let rep = h.run(json!({"target":"see","replay":cassette}));
    assert_eq!(rep["outcome"], "returned", "{rep}");
    assert_eq!(rep["result"], rec["result"]);
    let text = record_text(&h, rep["id"].as_str().unwrap());
    assert_digest_only(&text);
}

#[test]
fn replay_with_changed_image_is_stale_not_matched_by_bytes() {
    let (h, dir) = sight_home("see", json!({"time":"10s"}), 0);
    let cassette = h.path("see.json");
    h.run(json!({"target":"see","record":cassette}));
    // The recorded handle is replayed, but its bytes are re-verified at send.
    std::fs::write(dir.join("a.png"), b"changed").unwrap();
    let rep = h.run(json!({"target":"see","replay":cassette}));
    assert_ne!(rep["outcome"], "returned", "{rep}");
}

#[test]
fn revive_rechecks_image_digest() {
    // tool_calls 2: read_image + n0 fit, n1 exhausts; revive needs only n1.
    let essence = json!({"tool_calls":2,"time":"10s"});
    let model_events = |h: &Home| -> i64 {
        h.core.db.lock().unwrap().query_row("SELECT COUNT(*) FROM events WHERE kind='model'", [], |r| r.get(0)).unwrap()
    };
    let wait = |h: &Home, id: &Value| h.call("wait", json!({"summon": id, "timeout_ms": 20000})).unwrap();

    let (h, dir) = sight_home("see", essence.clone(), 2);
    let first = h.run(json!({"target":"see"}));
    assert_eq!(first["outcome"], "exhausted", "{first}");
    std::fs::write(dir.join("a.png"), b"edited").unwrap();
    let r = h.call("revive", json!({"summon": first["id"]})).unwrap();
    let done = wait(&h, &r["summon"]);
    assert_eq!(done["outcome"], "failed", "{done}");
    assert!(done["error"].as_str().unwrap_or_default().contains("stale image"), "{done}");
    assert_eq!(model_events(&h), 0, "stale image must not be dispatched");

    let (h, _dir) = sight_home("see", essence, 2);
    let first = h.run(json!({"target":"see"}));
    assert_eq!(first["outcome"], "exhausted", "{first}");
    let r = h.call("revive", json!({"summon": first["id"]})).unwrap();
    let done = wait(&h, &r["summon"]);
    assert_eq!(done["outcome"], "returned", "{done}");
    assert_eq!(model_events(&h), 1);
    assert_digest_only(&record_text(&h, r["summon"].as_str().unwrap()));
}

#[test]
fn simulate_with_images_reads_for_real() {
    let (h, _) = sight_home("see", json!({"time":"10s"}), 0);
    let done = h.run(json!({"target":"see","simulate":true}));
    assert_eq!(done["outcome"], "returned", "{done}");
    let id = done["id"].as_str().unwrap();
    let events = h.call("events", json!({"summon": id, "limit": 200})).unwrap();
    let text = events.to_string();
    // The read is real: the handle names the file's true digest and size.
    assert!(text.contains(&crate::util::sha256_hex(PNG_1X1)), "{text}");
    assert!(text.contains(&format!("\"bytes\":{}", PNG_1X1.len())), "{text}");
    assert_digest_only(&record_text(&h, id));
}

#[test]
fn epitaph_body_has_no_image_bytes() {
    let (h, _) = sight_home("see", json!({"time":"10s"}), 0);
    let done = h.run(json!({"target":"see"}));
    assert_eq!(done["outcome"], "returned", "{done}");
    let all = epitaphs(&h.core);
    assert_eq!(all.len(), 1);
    let (_, _, link, body, _, _) = &all[0];
    for text in [link, body] {
        assert!(!text.contains(&b64(PNG_1X1)), "image bytes leaked into an epitaph");
    }
}

#[test]
fn revive_never_adopts_a_fallback_made_handle() {
    let h = Home::new("");
    let dir = h.root.join("scans");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("a.png"), PNG_1X1).unwrap();
    let root = dir.to_string_lossy().into_owned();
    // The read fails (no such file); its model fallback forges a handle for
    // the real a.png. tool_calls 2 lets the read and n0 run, then n1 exhausts.
    let forge_schema = json!({"type":"object","properties":{
        "kind":{"enum":["image"]},"root":{"enum":[&root]},"path":{"enum":["a.png"]},
        "sha256":{"enum":[crate::util::sha256_hex(PNG_1X1)]}}});
    h.inscribe_def(json!({
        "ritual":"forge","model":"mock/keyword","essence":{"tool_calls":2,"time":"10s"},
        "reagents":{"sight":{"select":["read_image"]},"data":{"select":["arithmetic"]}},
        "requires":[{"reagent":"sight","action":"read_image","on":{"root":&root}}],
        "steps":[
            {"id":"page","operation":"sight.read_image","arguments":{"root":&root,"path":"missing.png"},
             "fallback":[{"id":"fake","model":{"prompt":"make a handle","schema":forge_schema}}]},
            {"id":"n0","operation":"data.arithmetic","arguments":{"left":1,"right":1,"operator":"add"}},
            {"id":"n1","operation":"data.arithmetic","arguments":{"left":2,"right":1,"operator":"add"}},
            {"id":"look","model":{"prompt":"what is this","images":["${steps.page.output}"]}}
        ],
        "return":{"seen":"${steps.look.output}"}
    }), false);
    h.call("grant", json!({"ritual":"forge","scopes":[{"reagent":"sight","select":["read_image"]}],"on":{"root":&root}})).unwrap();
    h.call("grant", json!({"ritual":"forge","scopes":[{"reagent":"data","all":true}]})).unwrap();
    let first = h.run(json!({"target":"forge"}));
    assert_eq!(first["outcome"], "exhausted", "{first}");
    let model_events = |h: &Home| -> i64 {
        h.core.db.lock().unwrap().query_row("SELECT COUNT(*) FROM events WHERE kind='model'", [], |r| r.get(0)).unwrap()
    };
    let before = model_events(&h);
    assert_eq!(before, 1, "only the forging fallback ran");
    let r = h.call("revive", json!({"summon": first["id"]})).unwrap();
    let done = h.call("wait", json!({"summon": r["summon"], "timeout_ms": 20000})).unwrap();
    assert_ne!(done["outcome"], "returned", "{done}");
    assert!(done["error"].as_str().unwrap_or_default().contains("not read by sight"), "{done}");
    assert_eq!(model_events(&h), before, "the image step must not reach the model");
}
