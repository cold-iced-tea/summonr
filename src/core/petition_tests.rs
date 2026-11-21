//! Petitions bind to the version of the resource a person reviewed.
use super::test_home::Home;
use super::*;

/// Hold the fixture receipt's move and return (home, mail root, petition id).
fn held() -> (Home, PathBuf, String) {
    let (h, root) = Home::triage();
    h.run(json!({"target":"inbox-triage","model":"mock/keyword","args":{"count":1,"apply":true,"hold":true}}));
    let list = h.call("petitions", json!({"status":"pending"})).unwrap();
    let id = list[0]["id"].as_str().unwrap().to_string();
    (h, root.join("mail"), id)
}

fn permit(h: &Home, id: &str) -> Value {
    h.call("petition_decide", json!({"permit": true, "ids": [id]})).unwrap()
}

#[test]
fn petition_records_a_digest() {
    let (h, mail, _) = held();
    let list = h.call("petitions", json!({"status":"pending"})).unwrap();
    assert_eq!(list[0]["digest"].as_str().map(str::len), Some(64), "{list}");
    let _ = std::fs::remove_dir_all(mail.parent().unwrap());
}

#[test]
fn permit_succeeds_when_unchanged() {
    let (h, mail, id) = held();
    let out = permit(&h, &id);
    assert_eq!(out["decided"][0]["status"], "applied", "{out}");
    assert!(mail.join("Receipts/001.eml").exists());
    let _ = std::fs::remove_dir_all(mail.parent().unwrap());
}

#[test]
fn permit_refuses_changed_message() {
    let (h, mail, id) = held();
    std::fs::write(mail.join("Inbox/001.eml"), "From: someone-else@example.com\n\nchanged").unwrap();
    let out = permit(&h, &id);
    assert_eq!(out["decided"][0]["status"], "stale", "{out}");
    assert!(out["decided"][0]["result"]["recorded"].is_string(), "{out}");
    assert!(mail.join("Inbox/001.eml").exists(), "a stale petition must not act");
    let _ = std::fs::remove_dir_all(mail.parent().unwrap());
}

#[test]
fn permit_refuses_message_moved_by_hand() {
    let (h, mail, id) = held();
    std::fs::create_dir_all(mail.join("Elsewhere")).unwrap();
    std::fs::rename(mail.join("Inbox/001.eml"), mail.join("Elsewhere/001.eml")).unwrap();
    let out = permit(&h, &id);
    assert_eq!(out["decided"][0]["status"], "stale", "{out}");
    assert!(mail.join("Elsewhere/001.eml").exists());
    let _ = std::fs::remove_dir_all(mail.parent().unwrap());
}

#[test]
fn permit_refuses_missing_message() {
    let (h, mail, id) = held();
    std::fs::remove_file(mail.join("Inbox/001.eml")).unwrap();
    let out = permit(&h, &id);
    assert_eq!(out["decided"][0]["status"], "stale", "{out}");
    assert!(out["decided"][0]["result"]["now"].is_null(), "{out}");
    let _ = std::fs::remove_dir_all(mail.parent().unwrap());
}

#[test]
fn legacy_petition_without_digest_permits_with_warning() {
    let (h, mail, id) = held();
    h.core
        .db
        .lock()
        .unwrap()
        .execute("UPDATE petitions SET digest = NULL", [])
        .unwrap();
    let out = permit(&h, &id);
    assert_eq!(out["decided"][0]["status"], "applied", "{out}");
    assert!(out["decided"][0]["warning"].as_str().unwrap().contains("not bound"), "{out}");
    let _ = std::fs::remove_dir_all(mail.parent().unwrap());
}

/// A home whose `write-state` command is held, with `read-state` (or the
/// given program) as its digest command, and a ritual `poke` granted it.
fn cmd_home(digest_program: &str, digest_args: &str, essence: &str) -> (Home, PathBuf) {
    let dir = std::env::temp_dir().join(new_id("summonr-held-cmd"));
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("state.txt"), "old").unwrap();
    let d = dir.display();
    let h = Home::new(&format!(
        "commands:\n  read-state: {{program: {digest_program}, args: {digest_args}, cwd: \"{d}\", effect: read}}\n  \
         write-state: {{program: sh, args: [\"-c\", \"echo new > state.txt\"], cwd: \"{d}\", effect: write, digest_command: read-state}}\n"
    ));
    h.inscribe_def(
        json!({"ritual": "poke", "reagents": {"cmd": {"select": ["write"]}}, "requires": [{"reagent": "cmd", "action": "write", "on": {"command": "write-state"}}], "essence": serde_yaml::from_str::<Value>(essence).unwrap(),
               "steps": [{"id": "w", "operation": "cmd.write", "hold": true,
                          "arguments": {"command": "write-state", "params": {}}}],
               "return": {"v": "${steps.w.output}"}}),
        false,
    );
    h.call("grant", json!({"ritual": "poke", "scopes": [{"reagent": "cmd", "select": ["write"]}],
                           "on": {"command": "write-state"}})).unwrap();
    (h, dir)
}

#[test]
fn a_held_write_on_an_ungranted_target_is_refused_before_any_probe() {
    let dir = std::env::temp_dir().join(new_id("summonr-held-file"));
    std::fs::create_dir_all(dir.join("granted")).unwrap();
    std::fs::create_dir_all(dir.join("other")).unwrap();
    let h = Home::new("");
    h.inscribe_def(
        json!({"ritual": "w", "reagents": {"file": {"select": ["write_text"]}},
               "requires": [{"reagent": "file", "action": "write_text", "on": {"root": dir.join("granted").to_str().unwrap()}}],
               "steps": [{"id": "w", "operation": "file.write_text", "hold": true,
                          "arguments": {"root": dir.join("other").to_str().unwrap(), "path": "x.txt", "text": "hi"}}],
               "return": {"v": "${steps.w.output}"}}),
        false,
    );
    h.call("grant", json!({"ritual": "w", "scopes": [{"reagent": "file", "select": ["write_text"]}],
                           "on": {"root": dir.join("granted").to_str().unwrap()}})).unwrap();
    let out = h.run(json!({"target": "w"}));
    assert_ne!(out["outcome"], "returned", "{out}");
    assert_eq!(h.petitions(), 0, "no petition for a write no grant covers");
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn a_held_write_whose_target_cannot_be_digested_is_not_held() {
    let (h, dir) = cmd_home("\"false\"", "[]", "{}");
    let out = h.run(json!({"target": "poke"}));
    assert_ne!(out["outcome"], "returned", "{out}");
    assert_eq!(h.petitions(), 0, "an unbound petition would let approval write whatever is there now");
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn a_digest_command_keeps_to_the_run_time_limit() {
    let (h, dir) = cmd_home("sleep", "[\"5\"]", "{time: 1s}");
    let started = std::time::Instant::now();
    let out = h.run(json!({"target": "poke"}));
    assert!(started.elapsed() < std::time::Duration::from_secs(4), "{out}");
    assert_ne!(out["outcome"], "returned", "{out}");
    assert_eq!(h.petitions(), 0);
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn a_held_write_on_a_large_file_is_bound_to_its_digest() {
    let dir = std::env::temp_dir().join(new_id("summonr-held-big"));
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("big.txt"), vec![b'a'; 1_100_000]).unwrap();
    let h = Home::new("");
    h.inscribe_def(
        json!({"ritual": "w", "reagents": {"file": {"select": ["write_text"]}},
               "requires": [{"reagent": "file", "action": "write_text", "on": {"root": dir.to_str().unwrap()}}],
               "steps": [{"id": "w", "operation": "file.write_text", "hold": true,
                          "arguments": {"root": dir.to_str().unwrap(), "path": "big.txt", "text": "replaced"}}],
               "return": {"v": "${steps.w.output}"}}),
        false,
    );
    h.call("grant", json!({"ritual": "w", "scopes": [{"reagent": "file", "select": ["write_text"]}],
                           "on": {"root": dir.to_str().unwrap()}})).unwrap();
    h.run(json!({"target": "w"}));
    let list = h.call("petitions", json!({"status": "pending"})).unwrap();
    assert!(list[0]["digest"].as_str().is_some_and(|d| d.starts_with("sha256:")), "{list}");
    std::fs::write(dir.join("big.txt"), "changed by hand").unwrap();
    let out = permit(&h, list[0]["id"].as_str().unwrap());
    assert_eq!(out["decided"][0]["status"], "stale", "{out}");
    assert_eq!(std::fs::read_to_string(dir.join("big.txt")).unwrap(), "changed by hand");
    let _ = std::fs::remove_dir_all(dir);
}
