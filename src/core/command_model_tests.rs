//! A command-backed model provider driven through the core: invalid answers
//! are retried like any model's, requests are counted, the fingerprint names
//! the configured command, and a cassette replays without the program.
use super::*;
use super::test_home::Home;
use std::os::unix::fs::PermissionsExt;

fn script(dir: &std::path::Path, name: &str, body: &str) -> String {
    let path = dir.join(name);
    std::fs::write(&path, format!("#!/bin/sh\n{body}\n")).unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
    path.to_str().unwrap().to_string()
}

fn ritual(model: &str, retry: bool) -> Value {
    let mut step = json!({"id": "c", "model": {"name": model, "prompt": "Classify ${inputs.text}",
        "schema": {"type": "object", "required": ["label"], "properties": {"label": {"type": "string", "enum": ["spam", "ham"]}}}}});
    if retry {
        step["retry"] = json!(2);
    }
    json!({"ritual": format!("cmd-{}", model.replace('/', "-")), "inputs": {"text": {"type": "string"}},
           "essence": {"requests": 5, "time": "30s"}, "steps": [step], "return": {"label": "${steps.c.output.label}"}})
}

#[test]
fn a_command_provider_answers_model_steps_and_replays_offline() {
    let dir = std::env::temp_dir().join(new_id("summonr-cmdmodel-core"));
    std::fs::create_dir_all(&dir).unwrap();
    let count = dir.join("count");
    // Answers prose first, then JSON: the retry sees a fresh call.
    let flaky = script(&dir, "flaky.sh", &format!(
        "cat >/dev/null\nn=$(cat {c} 2>/dev/null || echo 0)\necho $((n+1)) > {c}\nif [ \"$n\" = 0 ]; then echo 'Sure! Here you go'; else echo '{{\"label\": \"spam\"}}'; fi",
        c = count.display()));
    let wrong = script(&dir, "wrong.sh", "cat >/dev/null\necho '{\"label\": \"maybe\"}'");
    let h = Home::new(&format!(
        "providers:\n  cmdflaky: {{kind: command, program: {flaky}, timeout_ms: 5000}}\n  cmdwrong: {{kind: command, program: {wrong}}}\n"));
    h.inscribe_def(ritual("cmdflaky/default", true), false);
    h.inscribe_def(ritual("cmdwrong/default", false), false);

    let cassette = h.path("cmd.json");
    let done = h.run(json!({"target": "cmd-cmdflaky-default", "args": {"text": "win money"}, "record": cassette}));
    assert_eq!(done["outcome"], "returned", "{done}");
    assert_eq!(done["result"]["label"], "spam");
    assert_eq!(std::fs::read_to_string(&count).unwrap().trim(), "2", "the invalid answer was retried once");
    assert_eq!(done["requests"], 2, "{done}");

    // A schema mismatch is `invalid`, as for an HTTP backend.
    let bad = h.run(json!({"target": "cmd-cmdwrong-default", "args": {"text": "x"}}));
    assert_eq!(bad["outcome"], "invalid", "{bad}");

    // The record says the decoder did not enforce the schema.
    let events = h.call("events", json!({"summon": done["id"]})).unwrap();
    let model = events.as_array().unwrap().iter().find(|e| e["kind"] == "model").unwrap();
    assert_eq!(model["data"]["structure"], "checked afterward only", "{model}");
    assert_eq!(model["data"]["payload"]["kind"], "command");

    // Replay never runs the program: delete it first.
    std::fs::remove_file(&flaky).unwrap();
    let replayed = h.run(json!({"target": "cmd-cmdflaky-default", "args": {"text": "win money"}, "replay": cassette}));
    assert_eq!(replayed["outcome"], "returned", "{replayed}");
    assert_eq!(replayed["result"]["label"], "spam");

    // The drift fingerprint names the configured command, without running it.
    let sites = std::collections::BTreeSet::from(["cmdwrong/default".to_string()]);
    let print = h.core.full_fingerprint(&sites);
    let site = &print["sites"]["cmdwrong/default"];
    assert_eq!(site["kind"], "command", "{print}");
    assert!(site["digest"].as_str().unwrap().starts_with("command:"), "{print}");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_drill_checks_a_routed_step_and_its_refused_choices() {
    let h = Home::new("");
    h.inscribe_def(json!({
        "ritual": "router",
        "inputs": {"model": {"type": "string"}},
        "essence": {"requests": 1},
        "steps": [{"id": "c", "model": {"one_of": ["ollama/small", "ollama/big"], "choose": "${inputs.model}",
                   "prompt": "Classify", "schema": {"type": "object", "required": ["label"], "properties": {"label": {"type": "string"}}}}}],
        "return": {"label": "${steps.c.output.label}"}
    }), false);
    let file = h.path("router.drill.json");
    std::fs::write(&file, json!({"drill": "routes", "ritual": "router", "cases": [
        {"id": "small", "inputs": {"model": "ollama/small"}, "model": {"c": [{"label": "spam"}]},
         "expect": {"outcome": "returned", "result": {"label": "spam"}, "model_calls": 1}},
        {"id": "outside", "inputs": {"model": "ollama/huge"},
         "expect": {"outcome": "invalid", "model_calls": 0}}
    ]}).to_string()).unwrap();
    let report = h.call("drill", json!({"ritual": "router", "against": file})).unwrap();
    assert_eq!((report["passed"].clone(), report["failed"].clone()), (json!(2), json!(0)), "{report}");
}
