//! Tamper-evident administrative audit log: every policy, grant, identity,
//! config, and governance state change is appended to `audit_log`, hash-chained,
//! and signed with the core's Ed25519 key over `AUDIT_DOMAIN`.
use super::*;
use base64::Engine as _;
use std::cell::RefCell;

pub const AUDIT_DOMAIN: &[u8] = b"summonr-audit-v1\0";
const ZEROS: &str = "0000000000000000000000000000000000000000000000000000000000000000";

thread_local! {
    static CURRENT_ACTOR: RefCell<Option<String>> = const { RefCell::new(None) };
}

/// RAII guard that binds the current thread's actor ID (e.g. from `SO_PEERCRED`)
/// for the lifetime of a socket connection or RPC dispatch.
pub(super) struct ActorGuard {
    prev: Option<String>,
}

impl ActorGuard {
    pub(super) fn enter(actor: String) -> Self {
        let prev = CURRENT_ACTOR.with(|c| c.borrow_mut().replace(actor));
        Self { prev }
    }
}

impl Drop for ActorGuard {
    fn drop(&mut self) {
        let prev = self.prev.take();
        CURRENT_ACTOR.with(|c| {
            *c.borrow_mut() = prev;
        });
    }
}

pub(super) fn current_actor() -> String {
    CURRENT_ACTOR.with(|c| {
        c.borrow()
            .clone()
            .unwrap_or_else(|| "local:owner".to_string())
    })
}

/// Extract `(uid, pid)` from a connected Unix domain socket via `SO_PEERCRED` on Linux.
#[cfg(target_os = "linux")]
pub(super) fn peer_cred_actor(stream: &std::os::unix::net::UnixStream) -> String {
    let mut cred = libc::ucred {
        pid: 0,
        uid: 0,
        gid: 0,
    };
    let mut len = std::mem::size_of::<libc::ucred>() as libc::socklen_t;
    let rc = unsafe {
        libc::getsockopt(
            stream.as_raw_fd(),
            libc::SOL_SOCKET,
            libc::SO_PEERCRED,
            (&mut cred as *mut libc::ucred).cast(),
            &mut len,
        )
    };
    if rc == 0 {
        format!("local:uid={}:pid={}", cred.uid, cred.pid)
    } else {
        "local:owner".to_string()
    }
}

#[cfg(not(target_os = "linux"))]
pub(super) fn peer_cred_actor(_stream: &std::os::unix::net::UnixStream) -> String {
    "local:owner".to_string()
}

#[allow(clippy::too_many_arguments)]
fn canonical_audit_entry(
    seq: i64,
    prev: &str,
    ts: i64,
    actor: &str,
    action: &str,
    target: &str,
    outcome: &str,
    detail: &Value,
) -> String {
    crate::util::canonical_json(&json!({
        "v": 1,
        "seq": seq,
        "prev": prev,
        "ts": ts,
        "actor": actor,
        "action": action,
        "target": target,
        "outcome": outcome,
        "detail": detail,
    }))
}

/// Append one signed, hash-chained audit entry inside an open SQLite transaction.
pub(super) fn append_audit_tx(
    tx: &rusqlite::Transaction,
    core: &crate::keys::Keypair,
    actor: &str,
    action: &str,
    target: &str,
    outcome: &str,
    detail: &Value,
) -> rusqlite::Result<String> {
    let (last_seq, prev): (i64, String) = tx
        .query_row(
            "SELECT seq, hash FROM audit_log ORDER BY seq DESC LIMIT 1",
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()?
        .unwrap_or((0, ZEROS.to_string()));
    let seq = last_seq + 1;
    let ts = now_ms();
    let payload = canonical_audit_entry(seq, &prev, ts, actor, action, target, outcome, detail);
    let hash = sha256_hex(payload.as_bytes());
    let sig_bytes = core.sign(AUDIT_DOMAIN, payload.as_bytes());
    let sig = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(sig_bytes);
    tx.execute(
        "INSERT INTO audit_log (seq, ts, actor, action, target, outcome, detail, prev_hash, hash, sig)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
        params![
            seq,
            ts,
            actor,
            action,
            target,
            outcome,
            detail.to_string(),
            prev,
            hash,
            sig
        ],
    )?;
    Ok(hash)
}

#[derive(Deserialize, Default)]
#[serde(deny_unknown_fields)]
struct AuditListArgs {
    #[serde(default)]
    action: Option<String>,
    #[serde(default)]
    actor: Option<String>,
    #[serde(default)]
    limit: Option<i64>,
}

impl Core {
    /// Record an administrative/policy audit entry attributed to the current actor.
    pub(super) fn record_audit(&self, action: &str, target: &str, outcome: &str, detail: Value) {
        let actor = current_actor();
        self.record_audit_as(&actor, action, target, outcome, detail);
    }

    /// Record an administrative/policy audit entry with an explicit actor string.
    pub(super) fn record_audit_as(
        &self,
        actor: &str,
        action: &str,
        target: &str,
        outcome: &str,
        detail: Value,
    ) {
        let mut db = self.db.lock().unwrap();
        let res = db.transaction().and_then(|tx| {
            append_audit_tx(&tx, &self.key, actor, action, target, outcome, &detail)?;
            tx.commit()
        });
        if let Err(e) = res {
            self.note_storage_error(format!("audit_log write failed: {e}"));
        }
    }

    /// Hash `config.yaml` at startup and anchor it into `audit_log` whenever it changes.
    pub(super) fn anchor_config_on_boot(&self) {
        let cfg_path = self.paths.config.join("config.yaml");
        let (cfg_hash, present) = match std::fs::read(&cfg_path) {
            Ok(bytes) => (sha256_hex(&bytes), true),
            Err(_) => (sha256_hex(b""), false),
        };
        let last_hash: Option<String> = self
            .db
            .lock()
            .unwrap()
            .query_row(
                "SELECT detail FROM audit_log WHERE action='config.loaded' ORDER BY seq DESC LIMIT 1",
                [],
                |r| r.get::<_, String>(0),
            )
            .optional()
            .ok()
            .flatten()
            .and_then(|s| serde_json::from_str::<Value>(&s).ok())
            .and_then(|v| v["config_sha256"].as_str().map(str::to_string));
        if last_hash.as_deref() != Some(cfg_hash.as_str()) {
            self.record_audit_as(
                "core:boot",
                "config.loaded",
                "config.yaml",
                "ok",
                json!({
                    "config_sha256": cfg_hash,
                    "present": present,
                    "mcp_reagents": self.cfg.mcp_reagents.keys().collect::<Vec<_>>(),
                    "commands": self.cfg.commands.keys().collect::<Vec<_>>(),
                    "pricing_rules": self.cfg.pricing.len(),
                }),
            );
        }
    }

    /// Verify both `epitaphs` and `audit_log` integrity on startup; if either chain
    /// was tampered with while the core was offline, fail closed via `storage_error`.
    pub(super) fn verify_integrity_on_startup(&self) {
        if let Ok(av) = self.audit_verify() {
            if av["ok"] != true {
                self.note_storage_error(format!(
                    "startup audit_log verification failed: {}",
                    av["problem"]
                ));
                return;
            }
        }
        if let Ok(cv) = self.chain_verify(json!({})) {
            if cv["ok"] != true {
                self.note_storage_error(format!(
                    "startup epitaph chain verification failed: {}",
                    cv["problem"]
                ));
            }
        }
    }

    pub(super) fn audit_verify(&self) -> Result<Value, RpcErr> {
        let key = self.key.public();
        let db = self.db.lock().unwrap();
        let mut st = db
            .prepare(
                "SELECT seq, ts, actor, action, target, outcome, detail, prev_hash, hash, sig
                 FROM audit_log ORDER BY seq",
            )
            .map_err(storage_error)?;
        struct AuditRow {
            seq: i64,
            ts: i64,
            actor: String,
            action: String,
            target: String,
            outcome: String,
            detail_s: String,
            prev_hash: String,
            hash: String,
            sig: String,
        }
        let rows: Vec<AuditRow> = st
            .query_map([], |r| {
                Ok(AuditRow {
                    seq: r.get(0)?,
                    ts: r.get(1)?,
                    actor: r.get(2)?,
                    action: r.get(3)?,
                    target: r.get(4)?,
                    outcome: r.get(5)?,
                    detail_s: r.get(6)?,
                    prev_hash: r.get(7)?,
                    hash: r.get(8)?,
                    sig: r.get(9)?,
                })
            })
            .map_err(storage_error)?
            .collect::<Result<_, _>>()
            .map_err(storage_error)?;

        let mut prev = ZEROS.to_string();
        let mut checked = 0usize;
        let mut problem: Option<(i64, String)> = None;
        for (expected_seq, row) in (1i64..).zip(rows) {
            let AuditRow {
                seq,
                ts,
                actor,
                action,
                target,
                outcome,
                detail_s,
                prev_hash,
                hash,
                sig,
            } = row;
            if seq != expected_seq {
                problem = Some((
                    expected_seq,
                    format!("audit entry {expected_seq} is missing or out of order (saw {seq})"),
                ));
                break;
            }
            if prev_hash != prev {
                problem = Some((seq, "prev_hash does not link to previous audit entry".into()));
                break;
            }
            let detail: Value = match serde_json::from_str(&detail_s) {
                Ok(v) => v,
                Err(e) => {
                    problem = Some((seq, format!("corrupt detail JSON: {e}")));
                    break;
                }
            };
            let payload =
                canonical_audit_entry(seq, &prev, ts, &actor, &action, &target, &outcome, &detail);
            let computed = sha256_hex(payload.as_bytes());
            if computed != hash {
                problem = Some((seq, "stored hash does not match entry content".into()));
                break;
            }
            let raw_sig = match base64::engine::general_purpose::URL_SAFE_NO_PAD.decode(&sig) {
                Ok(b) => b,
                Err(e) => {
                    problem = Some((seq, format!("invalid signature encoding: {e}")));
                    break;
                }
            };
            let sig_arr: [u8; 64] = match raw_sig.try_into() {
                Ok(a) => a,
                Err(_) => {
                    problem = Some((seq, "invalid signature length".into()));
                    break;
                }
            };
            if !key.verify(AUDIT_DOMAIN, payload.as_bytes(), &sig_arr) {
                problem = Some((seq, "Ed25519 signature verification failed".into()));
                break;
            }
            prev = hash;
            checked += 1;
        }
        Ok(json!({
            "ok": problem.is_none(),
            "checked": checked,
            "head": if checked == 0 { Value::Null } else { json!(prev) },
            "problem": problem.map(|(seq, reason)| json!({"seq": seq, "reason": reason})),
        }))
    }

    pub(super) fn audit_list(&self, p: Value) -> Result<Value, RpcErr> {
        let args: AuditListArgs = if p.is_object() && p.as_object().unwrap().is_empty() {
            AuditListArgs::default()
        } else {
            serde_json::from_value(p).map_err(|e| RpcErr::invalid(e.to_string()))?
        };
        let limit = args.limit.unwrap_or(100).clamp(1, 1000);
        let action = args.action.as_deref().unwrap_or("");
        let actor = args.actor.as_deref().unwrap_or("");
        let db = self.db.lock().unwrap();
        let mut st = db
            .prepare(
                "SELECT seq, ts, actor, action, target, outcome, detail, hash
                 FROM audit_log
                 WHERE (?1 = '' OR action = ?1) AND (?2 = '' OR actor = ?2)
                 ORDER BY seq DESC LIMIT ?3",
            )
            .map_err(storage_error)?;
        let rows: Vec<Value> = st
            .query_map(params![action, actor, limit], |r| {
                let d: String = r.get(6)?;
                Ok(json!({
                    "seq": r.get::<_, i64>(0)?,
                    "ts": r.get::<_, i64>(1)?,
                    "actor": r.get::<_, String>(2)?,
                    "action": r.get::<_, String>(3)?,
                    "target": r.get::<_, String>(4)?,
                    "outcome": r.get::<_, String>(5)?,
                    "detail": serde_json::from_str::<Value>(&d).unwrap_or(Value::Null),
                    "hash": r.get::<_, String>(7)?,
                }))
            })
            .map_err(storage_error)?
            .collect::<Result<_, _>>()
            .map_err(storage_error)?;
        Ok(Value::Array(rows))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_paths(label: &str) -> Paths {
        let dir = std::env::temp_dir().join(format!("summonr-audit-{label}-{}", new_id("t")));
        let p = Paths {
            config: dir.join("config"),
            state: dir.join("state"),
            runtime: dir.join("run"),
        };
        std::fs::create_dir_all(&p.config).unwrap();
        std::fs::create_dir_all(&p.state).unwrap();
        std::fs::create_dir_all(&p.runtime).unwrap();
        p
    }

    #[test]
    fn audit_log_chains_admin_mutations_and_blocks_update_delete() {
        let p = test_paths("chain");
        let core = Core::open(p.clone()).unwrap();

        // Boot should have anchored config.loaded.
        let list = core.audit_list(json!({})).unwrap();
        assert!(
            list.as_array()
                .unwrap()
                .iter()
                .any(|e| e["action"] == "config.loaded"),
            "expected config.loaded audit entry: {list}"
        );

        // Inscribe + grant + revoke + familiar_new should all append signed audit rows.
        let dir = p.state.parent().unwrap();
        let rf = dir.join("r.yaml");
        std::fs::write(
            &rf,
            "ritual: r1\nessence: {requests: 0, time: 1m}\nrequires:\n  - reagent: mail\n    action: locate\nsteps:\n  - id: s\n    operation: mail.locate\n    arguments: {message: \"m1\"}\nreturn:\n  ok: true\n",
        )
        .unwrap();
        core.inscribe(json!({"path": rf.to_str().unwrap()})).unwrap();
        core.grant(json!({"ritual": "r1", "scopes": [{"reagent": "mail", "select": ["locate"]}]})).unwrap();
        core.revoke(json!({"ritual": "r1", "scopes": [{"reagent": "mail", "select": ["locate"]}]})).unwrap();
        core.familiar_new(json!({"name": "bot"})).unwrap();

        let verified = core.audit_verify().unwrap();
        assert_eq!(verified["ok"], true, "{verified}");
        assert!(verified["checked"].as_u64().unwrap() >= 5, "{verified}");

        // Immutability triggers must reject UPDATE and DELETE on audit_log.
        let db = core.db.lock().unwrap();
        assert!(
            db.execute("UPDATE audit_log SET outcome='tampered' WHERE seq=1", [])
                .is_err(),
            "UPDATE on audit_log must be blocked by trigger"
        );
        assert!(
            db.execute("DELETE FROM audit_log WHERE seq=1", []).is_err(),
            "DELETE on audit_log must be blocked by trigger"
        );
        drop(db);

        drop(core);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn startup_verification_fails_closed_on_offline_tampering() {
        let p = test_paths("tamper");
        {
            let core = Core::open(p.clone()).unwrap();
            assert_eq!(core.audit_verify().unwrap()["ok"], true);
        }
        // Simulate offline attacker dropping the trigger and mutating row 1.
        {
            let conn = Connection::open(p.db()).unwrap();
            conn.execute_batch(
                "DROP TRIGGER IF EXISTS audit_log_no_update;
                 UPDATE audit_log SET outcome='forged' WHERE seq=1;",
            )
            .unwrap();
        }
        // Re-opening Core must detect the broken chain and fail closed on new RPCs,
        // while still allowing `audit_verify` to report the exact tampered seq.
        let core = Core::open(p.clone()).unwrap();
        let err = core.dispatch("rituals", json!({})).unwrap_err();
        assert_eq!(err.code, "storage_failed", "{err:?}");
        let av = core.dispatch("audit_verify", json!({})).unwrap();
        assert_eq!(av["ok"], false, "{av}");
        assert_eq!(av["problem"]["seq"], 1, "{av}");

        drop(core);
        let _ = std::fs::remove_dir_all(p.state.parent().unwrap());
    }
}
