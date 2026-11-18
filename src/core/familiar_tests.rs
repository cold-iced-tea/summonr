//! Familiars and peers: owner-managed identities.
use super::test_home::{repo, Home};
use super::*;
use std::os::unix::fs::PermissionsExt;

fn other_key() -> String {
    crate::keys::generate().public().text()
}

#[test]
fn new_writes_0600_key_and_registers() {
    let h = Home::new("");
    let out = h.call("familiar_new", json!({"name": "codex"})).unwrap();
    let file = PathBuf::from(out["key_file"].as_str().unwrap());
    assert_eq!(file, h.core.paths.config.join("familiars/codex.key"));
    assert_eq!(std::fs::metadata(&file).unwrap().permissions().mode() & 0o777, 0o600);
    let loaded = crate::keys::load(&file).unwrap();
    assert_eq!(out["id"], loaded.public().id());
    assert_eq!(out["key"], loaded.public().text());
    let list = h.call("familiar_list", json!({})).unwrap();
    assert_eq!(list[0]["name"], "codex");
    assert_eq!(list[0]["kind"], "local");
    assert_eq!(list[0]["allowed"], json!([]));
    assert!(h.core.familiar_by_key(&loaded.public().id()).is_some());
}

#[test]
fn new_refuses_bad_names() {
    let h = Home::new("");
    for name in ["../evil", "a/b", "", "UPPER", &"x".repeat(33), "-lead"] {
        let err = h.call("familiar_new", json!({"name": name})).unwrap_err();
        assert_eq!(err.exit, 2, "{name}: {}", err.message);
    }
    assert!(!h.core.paths.config.join("evil.key").exists());
    assert!(std::fs::read_dir(h.core.paths.config.join("familiars")).map(|d| d.count()).unwrap_or(0) == 0);
}

#[test]
fn new_refuses_duplicate_even_revoked_and_keeps_file() {
    let h = Home::new("");
    h.call("familiar_new", json!({"name": "codex"})).unwrap();
    let file = h.core.paths.config.join("familiars/codex.key");
    let before = std::fs::read(&file).unwrap();
    assert_eq!(h.call("familiar_new", json!({"name": "codex"})).unwrap_err().exit, 5);
    h.call("familiar_revoke", json!({"name": "codex"})).unwrap();
    assert_eq!(h.call("familiar_new", json!({"name": "codex"})).unwrap_err().exit, 5);
    assert_eq!(h.call("familiar_add", json!({"name": "codex", "key": other_key()})).unwrap_err().exit, 5);
    assert_eq!(std::fs::read(&file).unwrap(), before, "the key file is never overwritten");
}

#[test]
fn add_registers_remote_and_refuses_duplicate_key_id() {
    let h = Home::new("");
    let k = other_key();
    let out = h.call("familiar_add", json!({"name": "partner", "key": k})).unwrap();
    assert_eq!(out["id"], crate::keys::PublicKey::parse(&k).unwrap().id());
    assert_eq!(h.call("familiar_list", json!({})).unwrap()[0]["kind"], "remote");
    assert_eq!(h.call("familiar_add", json!({"name": "again", "key": k})).unwrap_err().exit, 5);
    assert_eq!(h.call("familiar_add", json!({"name": "bad", "key": "ed25519:nope"})).unwrap_err().exit, 2);
    assert!(!h.core.paths.config.join("familiars/partner.key").exists(), "a remote familiar has no private key here");
}

#[test]
fn allow_requires_existing_ritual_and_disallow_removes() {
    let h = Home::new("");
    h.inscribe(&repo("tests/fixtures/rituals/total-receipt.yaml"), false);
    h.call("familiar_new", json!({"name": "codex"})).unwrap();
    assert_eq!(h.call("familiar_allow", json!({"name": "codex", "rituals": ["nope"]})).unwrap_err().exit, 3);
    h.call("familiar_allow", json!({"name": "codex", "rituals": ["total-receipt", "total-receipt"]})).unwrap();
    assert_eq!(h.call("familiar_list", json!({})).unwrap()[0]["allowed"], json!(["total-receipt"]));
    h.call("familiar_disallow", json!({"name": "codex", "rituals": ["total-receipt"]})).unwrap();
    assert_eq!(h.call("familiar_list", json!({})).unwrap()[0]["allowed"], json!([]));
    assert_eq!(h.call("familiar_allow", json!({"name": "ghost", "rituals": ["total-receipt"]})).unwrap_err().exit, 3);
}

#[test]
fn revoke_hides_from_lookup() {
    let h = Home::new("");
    let out = h.call("familiar_new", json!({"name": "codex"})).unwrap();
    let id = out["id"].as_str().unwrap().to_string();
    h.call("familiar_revoke", json!({"name": "codex"})).unwrap();
    assert!(h.core.familiar_by_key(&id).is_none());
    assert!(h.call("familiar_list", json!({})).unwrap()[0]["revoked"].is_i64());
}

#[test]
fn peer_add_validates_url_and_key() {
    let h = Home::new("");
    let k = other_key();
    assert_eq!(h.call("peer_add", json!({"name": "b", "url": "ftp://h", "key": k})).unwrap_err().exit, 2);
    assert_eq!(h.call("peer_add", json!({"name": "b", "url": "http://h:1", "key": "x"})).unwrap_err().exit, 2);
    assert_eq!(h.call("peer_add", json!({"name": "../b", "url": "http://h:1", "key": k})).unwrap_err().exit, 2);
    h.call("peer_add", json!({"name": "b", "url": "http://h:1/", "key": k})).unwrap();
    let pinned = &h.call("peer_list", json!({})).unwrap()[0];
    assert_eq!(pinned["url"], "http://h:1");
    assert_eq!(pinned["key"], k);
    assert_eq!(h.call("peer_add", json!({"name": "b", "url": "http://h:2", "key": k})).unwrap_err().exit, 5);
    assert_eq!(h.call("peer_list", json!({})).unwrap()[0]["name"], "b");
    h.call("peer_remove", json!({"name": "b"})).unwrap();
    assert_eq!(h.call("peer_list", json!({})).unwrap(), json!([]));
}

#[test]
fn no_private_key_in_rpc_results() {
    let h = Home::new("");
    let out = h.call("familiar_new", json!({"name": "codex"})).unwrap();
    let pem = std::fs::read_to_string(h.core.paths.config.join("familiars/codex.key")).unwrap();
    let body: String = pem.lines().filter(|l| !l.starts_with("-----")).collect();
    let list = h.call("familiar_list", json!({})).unwrap();
    for shown in [out.to_string(), list.to_string()] {
        assert!(!shown.contains("PRIVATE KEY") && !shown.contains(&body), "{shown}");
    }
}

#[test]
fn orphan_key_file_gets_a_clear_message_and_new_keys_leave_no_temp_files() {
    let h = Home::new("");
    let dir = h.core.paths.config.join("familiars");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("ghost.key"), "left over").unwrap();
    let e = h.call("familiar_new", json!({"name": "ghost"})).unwrap_err();
    assert!(e.message.contains("not registered") && e.message.contains("ghost.key"), "{}", e.message);
    h.call("familiar_new", json!({"name": "codex"})).unwrap();
    let names: Vec<String> = std::fs::read_dir(&dir).unwrap().flatten().map(|e| e.file_name().to_string_lossy().into_owned()).collect();
    assert!(names.iter().all(|n| n.ends_with(".key")), "{names:?}");
}

#[test]
fn register_grant_valid_registers_remote_with_allowed_rituals() {
    let h = Home::new("");
    h.inscribe(&repo("tests/fixtures/rituals/total-receipt.yaml"), false);
    let root = crate::keys::generate();
    let fam = crate::keys::generate();
    let now = crate::util::now_ms();
    let grant = crate::keys::FamiliarGrant::sign(
        &root,
        "delegated-bot".into(),
        &fam.public(),
        vec!["total-receipt".into(), "total-receipt".into()],
        vec![],
        now - 1_000,
        Some(now + 60_000),
    );
    let out = h
        .core
        .familiar_register_grant(json!({
            "grant": grant,
            "root_key": root.public().text(),
        }))
        .unwrap();
    assert_eq!(out["name"], "delegated-bot");
    assert_eq!(out["kind"], "remote");
    assert_eq!(out["key"], fam.public().text());
    assert_eq!(out["key_id"], fam.public().id());
    assert_eq!(out["root_key_id"], root.public().id());
    assert_eq!(out["allowed"], json!(["total-receipt"]));

    let (name, key, allowed) = h.core.familiar_by_key(&fam.public().id()).unwrap();
    assert_eq!(name, "delegated-bot");
    assert_eq!(key, fam.public());
    assert_eq!(allowed, vec!["total-receipt".to_string()]);
}

#[test]
fn register_grant_rejects_tampered_and_expired_grants() {
    let h = Home::new("");
    h.inscribe(&repo("tests/fixtures/rituals/total-receipt.yaml"), false);
    let root = crate::keys::generate();
    let fam = crate::keys::generate();
    let now = crate::util::now_ms();

    let mut tampered = crate::keys::FamiliarGrant::sign(
        &root,
        "good-name".into(),
        &fam.public(),
        vec!["total-receipt".into()],
        vec![],
        now - 1_000,
        Some(now + 60_000),
    );
    tampered.name = "evil-name".into();
    let err = h
        .core
        .familiar_register_grant(json!({
            "grant": tampered,
            "root_key": root.public().text(),
        }))
        .unwrap_err();
    assert_eq!(err.exit, 4);
    assert!(h.core.familiar_by_key(&fam.public().id()).is_none());

    let expired = crate::keys::FamiliarGrant::sign(
        &root,
        "expired-bot".into(),
        &fam.public(),
        vec!["total-receipt".into()],
        vec![],
        now - 10_000,
        Some(now - 1),
    );
    let err = h
        .core
        .familiar_register_grant(json!({
            "grant": expired,
            "root_key": root.public().text(),
        }))
        .unwrap_err();
    assert_eq!(err.exit, 4);
    assert!(err.message.contains("expired"), "{}", err.message);
    assert!(h.core.familiar_by_key(&fam.public().id()).is_none());

    // Pinned config/root.pub cannot be overridden by a caller-supplied root_key.
    std::fs::write(h.core.paths.config.join("root.pub"), format!("{}\n", root.public().text())).unwrap();
    let rogue_root = crate::keys::generate();
    let rogue_grant = crate::keys::FamiliarGrant::sign(
        &rogue_root,
        "rogue-bot".into(),
        &fam.public(),
        vec!["total-receipt".into()],
        vec![],
        now - 1_000,
        Some(now + 60_000),
    );
    let err = h
        .core
        .familiar_register_grant(json!({
            "grant": rogue_grant,
            "root_key": rogue_root.public().text(),
        }))
        .unwrap_err();
    assert_eq!(err.exit, 4);
    assert!(err.message.contains("root.pub"), "{}", err.message);
}
