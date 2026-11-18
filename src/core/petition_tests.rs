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
