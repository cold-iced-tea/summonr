//! Tamper-evident administrative audit log: every policy, grant, identity,
//! config, and governance state change is appended to `audit_log`, hash-chained,
//! and signed with the core's Ed25519 key over `AUDIT_DOMAIN`.
use super::*;
use base64::Engine as _;
use std::cell::RefCell;

pub const AUDIT_DOMAIN: &[u8] = b"summonr-audit-v1\0";
pub const BUNDLE_DOMAIN: &[u8] = b"summonr-bundle-v1\0";
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

#[derive(Deserialize, Default)]
#[serde(deny_unknown_fields)]
struct AuditExportArgs {
    #[serde(default)]
    file: Option<String>,
    #[serde(default)]
    format: Option<String>,
    #[serde(default)]
    from_ms: Option<i64>,
    #[serde(default)]
    to_ms: Option<i64>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct AuditPruneArgs {
    days: u64,
    #[serde(default)]
    dry_run: bool,
}

impl Core {
    /// Record an administrative/policy audit entry attributed to the current actor.
    pub(super) fn record_audit(&self, action: &str, target: &str, outcome: &str, detail: Value) {
        let actor = current_actor();
        self.record_audit_as(&actor, action, target, outcome, detail);
    }

    /// Record an administrative/policy audit entry with an explicit actor string.
    pub(crate) fn record_audit_as(
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
                    "wasm_reagents": self.cfg.wasm_reagents.iter().map(|(k, v)| (k, &v.sha256)).collect::<BTreeMap<_, _>>(),
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

    /// Export a self-contained, Ed25519-signed compliance evidence bundle
    /// containing the `audit_log`, `epitaphs` chain + Merkle root, peer witnessings,
    /// RFC 3161 timestamps, and active governance controls summary.
    pub(super) fn audit_export(&self, p: Value) -> Result<Value, RpcErr> {
        let args: AuditExportArgs = if p.is_object() && p.as_object().unwrap().is_empty() {
            AuditExportArgs::default()
        } else {
            serde_json::from_value(p).map_err(|e| RpcErr::invalid(e.to_string()))?
        };
        let fmt = args.format.as_deref().unwrap_or("json");
        if !matches!(fmt, "json" | "ndjson") {
            return Err(RpcErr::invalid(format!(
                "unknown export format `{fmt}`; use `json` or `ndjson`"
            )));
        }
        let audit_ver = self.audit_verify()?;
        let chain_ver = self.chain_verify(json!({}))?;
        let exported_at = now_ms();

        let (audit_rows, epitaph_rows, epitaph_hashes, witnessings, timestamps, controls) = {
            let db = self.db.lock().unwrap();
            let mut ast = db
                .prepare(
                    "SELECT seq, ts, actor, action, target, outcome, detail, prev_hash, hash, sig
                     FROM audit_log ORDER BY seq",
                )
                .map_err(storage_error)?;
            let audit_rows: Vec<Value> = ast
                .query_map([], |r| {
                    let ts: i64 = r.get(1)?;
                    let d: String = r.get(6)?;
                    Ok((
                        ts,
                        json!({
                            "seq": r.get::<_, i64>(0)?,
                            "ts": ts,
                            "actor": r.get::<_, String>(2)?,
                            "action": r.get::<_, String>(3)?,
                            "target": r.get::<_, String>(4)?,
                            "outcome": r.get::<_, String>(5)?,
                            "detail": serde_json::from_str::<Value>(&d).unwrap_or(Value::Null),
                            "prev_hash": r.get::<_, String>(7)?,
                            "hash": r.get::<_, String>(8)?,
                            "sig": r.get::<_, String>(9)?,
                        }),
                    ))
                })
                .map_err(storage_error)?
                .collect::<Result<Vec<_>, _>>()
                .map_err(storage_error)?
                .into_iter()
                .filter(|(ts, _)| {
                    args.from_ms.is_none_or(|f| *ts >= f) && args.to_ms.is_none_or(|t| *ts <= t)
                })
                .map(|(_, v)| v)
                .collect();

            let mut est = db
                .prepare("SELECT seq, run, link, body, sig, hash FROM epitaphs ORDER BY seq")
                .map_err(storage_error)?;
            let mut ep_rows = Vec::new();
            let mut ep_hashes = Vec::new();
            for row in est
                .query_map([], |r| {
                    Ok((
                        r.get::<_, i64>(0)?,
                        r.get::<_, String>(1)?,
                        r.get::<_, String>(2)?,
                        r.get::<_, String>(3)?,
                        r.get::<_, String>(4)?,
                        r.get::<_, String>(5)?,
                    ))
                })
                .map_err(storage_error)?
            {
                let (seq, run, link, body, sig, hash) = row.map_err(storage_error)?;
                ep_hashes.push(hash.clone());
                ep_rows.push(json!({
                    "seq": seq,
                    "run": run,
                    "link": link,
                    "body": body,
                    "sig": sig,
                    "hash": hash,
                }));
            }

            let mut wst = db
                .prepare("SELECT peer, seq, hash, time, sig FROM witnessings ORDER BY seq, peer")
                .map_err(storage_error)?;
            let witnessings: Vec<Value> = wst
                .query_map([], |r| {
                    Ok(json!({
                        "peer": r.get::<_, String>(0)?,
                        "seq": r.get::<_, i64>(1)?,
                        "hash": r.get::<_, String>(2)?,
                        "time": r.get::<_, i64>(3)?,
                        "sig": r.get::<_, String>(4)?,
                    }))
                })
                .map_err(storage_error)?
                .collect::<Result<_, _>>()
                .map_err(storage_error)?;

            let mut tst = db
                .prepare("SELECT seq, hash, url, time FROM timestamps ORDER BY seq")
                .map_err(storage_error)?;
            let timestamps: Vec<Value> = tst
                .query_map([], |r| {
                    Ok(json!({
                        "seq": r.get::<_, i64>(0)?,
                        "hash": r.get::<_, String>(1)?,
                        "url": r.get::<_, String>(2)?,
                        "time": r.get::<_, i64>(3)?,
                    }))
                })
                .map_err(storage_error)?
                .collect::<Result<_, _>>()
                .map_err(storage_error)?;

            let operators: i64 = db
                .query_row(
                    "SELECT COUNT(*) FROM operators WHERE revoked IS NULL",
                    [],
                    |r| r.get(0),
                )
                .unwrap_or(0);
            let familiars: i64 = db
                .query_row(
                    "SELECT COUNT(*) FROM familiars WHERE revoked IS NULL",
                    [],
                    |r| r.get(0),
                )
                .unwrap_or(0);
            let grants: i64 = db
                .query_row("SELECT COUNT(*) FROM grants", [], |r| r.get(0))
                .unwrap_or(0);
            let cfg_hash = std::fs::read(self.paths.config.join("config.yaml"))
                .map(|b| sha256_hex(&b))
                .unwrap_or_else(|_| sha256_hex(b""));
            let controls = json!({
                "rbac_enforce": self.cfg.rbac.enforce,
                "require_independent_reviewer": self.cfg.rbac.require_independent_reviewer,
                "active_operators": operators,
                "active_familiars": familiars,
                "active_grants": grants,
                "config_sha256": cfg_hash,
            });
            (audit_rows, ep_rows, ep_hashes, witnessings, timestamps, controls)
        };

        let merkle_root = crate::epitaph::merkle_root(&epitaph_hashes);
        let body = json!({
            "format": "summonr-evidence-bundle-v1",
            "core_key": self.key.public().text(),
            "core_id": self.key.public().id(),
            "exported_at": exported_at,
            "window": {"from_ms": args.from_ms, "to_ms": args.to_ms},
            "controls": controls,
            "audit_verification": audit_ver,
            "chain_verification": chain_ver,
            "epitaph_merkle_root": merkle_root,
            "audit_log": audit_rows,
            "epitaphs": epitaph_rows,
            "witnessings": witnessings,
            "timestamps": timestamps,
        });
        let canonical = crate::util::canonical_json(&body);
        let bundle_hash = sha256_hex(canonical.as_bytes());
        let sig_bytes = self.key.sign(BUNDLE_DOMAIN, canonical.as_bytes());
        let sig = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(sig_bytes);
        let envelope = json!({
            "bundle": body,
            "bundle_hash": bundle_hash,
            "signature": sig,
        });
        let audit_count = body["audit_log"].as_array().map(|a| a.len()).unwrap_or(0);
        let epitaph_count = body["epitaphs"].as_array().map(|a| a.len()).unwrap_or(0);

        let rendered_ndjson = if fmt == "ndjson" {
            let mut lines = Vec::with_capacity(audit_count + 1);
            if let Some(entries) = body["audit_log"].as_array() {
                for entry in entries {
                    lines.push(serde_json::to_string(entry).unwrap_or_default());
                }
            }
            lines.push(
                serde_json::to_string(&json!({
                    "record_type": "summonr-siem-manifest-v1",
                    "core_id": body["core_id"],
                    "exported_at": exported_at,
                    "audit_entries": audit_count,
                    "epitaphs": epitaph_count,
                    "epitaph_merkle_root": merkle_root,
                    "bundle_hash": bundle_hash,
                    "signature": sig,
                }))
                .unwrap_or_default(),
            );
            Some(lines.join("\n") + "\n")
        } else {
            None
        };

        if let Some(ref file) = args.file {
            let content = match &rendered_ndjson {
                Some(nd) => nd.clone(),
                None => serde_json::to_string_pretty(&envelope).unwrap_or_default() + "\n",
            };
            std::fs::write(file, content)
                .map_err(|e| RpcErr::invalid(format!("cannot write {file}: {e}")))?;

            self.record_audit(
                "audit.export",
                file,
                "ok",
                json!({"format": fmt, "bundle_hash": bundle_hash, "audit_entries": audit_count, "epitaphs": epitaph_count}),
            );
            Ok(json!({
                "file": file,
                "format": fmt,
                "bundle_hash": bundle_hash,
                "audit_entries": audit_count,
                "epitaphs": epitaph_count,
                "epitaph_merkle_root": merkle_root,
            }))
        } else {
            self.record_audit(
                "audit.export",
                "stdout",
                "ok",
                json!({"format": fmt, "bundle_hash": bundle_hash, "audit_entries": audit_count, "epitaphs": epitaph_count}),
            );
            if let Some(nd) = rendered_ndjson {
                Ok(json!({
                    "format": "ndjson",
                    "ndjson": nd,
                    "bundle_hash": bundle_hash,
                    "audit_entries": audit_count,
                    "epitaphs": epitaph_count,
                    "epitaph_merkle_root": merkle_root,
                }))
            } else {
                Ok(envelope)
            }
        }
    }

    /// Prune plaintext elegy payloads (`summons`, `events`, `epitaph_salts`) for
    /// ended runs older than `days` that have no pending petitions, while preserving
    /// the signed `epitaphs` chain, Merkle root, and `audit_log` intact.
    pub(super) fn audit_prune(&self, p: Value) -> Result<Value, RpcErr> {
        let args: AuditPruneArgs =
            serde_json::from_value(p).map_err(|e| RpcErr::invalid(e.to_string()))?;
        let window_ms = args
            .days
            .checked_mul(86_400_000)
            .ok_or_else(|| RpcErr::invalid("days overflow"))?;
        let cutoff_ms = now_ms().saturating_sub(window_ms as i64);

        let victims: Vec<String> = {
            let db = self.db.lock().unwrap();
            let mut st = db
                .prepare(
                    "SELECT s.id FROM summons s
                     WHERE s.status = 'ended' AND s.ended IS NOT NULL AND s.ended <= ?1
                       AND NOT EXISTS (
                         SELECT 1 FROM petitions p WHERE p.summon = s.id AND p.status = 'pending'
                       )
                     ORDER BY s.ended",
                )
                .map_err(storage_error)?;
            let rows = st
                .query_map(params![cutoff_ms], |r| r.get::<_, String>(0))
                .map_err(storage_error)?;
            rows.collect::<Result<_, _>>().map_err(storage_error)?
        };

        if args.dry_run {
            return Ok(json!({
                "dry_run": true,
                "cutoff_ms": cutoff_ms,
                "would_prune": victims.len(),
                "summons": victims,
            }));
        }

        {
            let mut db = self.db.lock().unwrap();
            let tx = db.transaction().map_err(storage_error)?;
            for id in &victims {
                tx.execute("DELETE FROM events WHERE summon=?1", params![id])
                    .map_err(storage_error)?;
                tx.execute("DELETE FROM epitaph_salts WHERE run=?1", params![id])
                    .map_err(storage_error)?;
                tx.execute("DELETE FROM summons WHERE id=?1", params![id])
                    .map_err(storage_error)?;
            }
            tx.commit().map_err(storage_error)?;
        }

        self.record_audit(
            "elegy.pruned",
            "summons",
            "ok",
            json!({"days": args.days, "cutoff_ms": cutoff_ms, "pruned": victims.len()}),
        );
        Ok(json!({
            "dry_run": false,
            "cutoff_ms": cutoff_ms,
            "pruned": victims.len(),
            "summons": victims,
        }))
    }
}

/// Offline verification of an exported compliance evidence bundle against an
/// explicitly trusted core `PublicKey`.
pub(crate) fn verify_evidence_bundle(
    file: &str,
    trusted_key: &crate::keys::PublicKey,
) -> Result<Value, RpcErr> {
    let text = std::fs::read_to_string(file)
        .map_err(|e| RpcErr::not_found(format!("cannot read {file}: {e}")))?;
    let env: Value = serde_json::from_str(&text)
        .map_err(|e| RpcErr::invalid(format!("invalid bundle JSON: {e}")))?;
    let body = &env["bundle"];
    if body["format"].as_str() != Some("summonr-evidence-bundle-v1") {
        return Err(RpcErr::new(
            "verification_failed",
            15,
            "unsupported evidence bundle format",
        ));
    }
    if body["core_key"].as_str() != Some(trusted_key.text().as_str()) {
        return Err(RpcErr::new(
            "verification_failed",
            15,
            "bundle core_key does not match trusted key",
        ));
    }
    let canonical = crate::util::canonical_json(body);
    let computed_hash = sha256_hex(canonical.as_bytes());
    if env["bundle_hash"].as_str() != Some(computed_hash.as_str()) {
        return Err(RpcErr::new(
            "verification_failed",
            15,
            "bundle_hash mismatch",
        ));
    }
    let sig_s = env["signature"].as_str().unwrap_or("");
    let raw_sig = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(sig_s)
        .map_err(|_| RpcErr::new("verification_failed", 15, "invalid bundle signature encoding"))?;
    let sig_arr: [u8; 64] = raw_sig
        .try_into()
        .map_err(|_| RpcErr::new("verification_failed", 15, "invalid bundle signature length"))?;
    if !trusted_key.verify(BUNDLE_DOMAIN, canonical.as_bytes(), &sig_arr) {
        return Err(RpcErr::new(
            "verification_failed",
            15,
            "bundle Ed25519 signature verification failed",
        ));
    }

    if body["audit_verification"]["ok"] != true {
        return Err(RpcErr::new(
            "verification_failed",
            15,
            "bundle audit_verification status is not ok",
        ));
    }
    if body["chain_verification"]["ok"] != true {
        return Err(RpcErr::new(
            "verification_failed",
            15,
            "bundle chain_verification status is not ok",
        ));
    }

    // Re-verify every audit_log entry's sequence continuity, prev_hash linkage,
    // canonical SHA-256 hash, and Ed25519 signature.
    let audit_entries = body["audit_log"].as_array().cloned().unwrap_or_default();
    let unfiltered_start = body["window"]["from_ms"].is_null();
    let mut expected_prev: Option<(i64, String)> = None;
    for entry in &audit_entries {
        let seq = entry["seq"].as_i64().unwrap_or(0);
        let ts = entry["ts"].as_i64().unwrap_or(0);
        let actor = entry["actor"].as_str().unwrap_or("");
        let action = entry["action"].as_str().unwrap_or("");
        let target = entry["target"].as_str().unwrap_or("");
        let outcome = entry["outcome"].as_str().unwrap_or("");
        let prev = entry["prev_hash"].as_str().unwrap_or("");
        let hash = entry["hash"].as_str().unwrap_or("");
        let sig = entry["sig"].as_str().unwrap_or("");
        match &expected_prev {
            None => {
                if unfiltered_start && (seq != 1 || prev != ZEROS) {
                    return Err(RpcErr::new(
                        "verification_failed",
                        15,
                        format!("unfiltered audit_log must start at seq 1 with zero prev_hash (saw seq {seq})"),
                    ));
                }
            }
            Some((p_seq, p_hash)) => {
                if seq != p_seq + 1 {
                    return Err(RpcErr::new(
                        "verification_failed",
                        15,
                        format!("audit_log sequence gap: expected {}, saw {seq}", p_seq + 1),
                    ));
                }
                if prev != p_hash {
                    return Err(RpcErr::new(
                        "verification_failed",
                        15,
                        format!("audit entry #{seq} prev_hash does not link to entry #{p_seq}"),
                    ));
                }
            }
        }
        let payload =
            canonical_audit_entry(seq, prev, ts, actor, action, target, outcome, &entry["detail"]);
        if sha256_hex(payload.as_bytes()) != hash {
            return Err(RpcErr::new(
                "verification_failed",
                15,
                format!("audit entry #{seq} hash mismatch"),
            ));
        }
        let rs = base64::engine::general_purpose::URL_SAFE_NO_PAD
            .decode(sig)
            .map_err(|_| {
                RpcErr::new(
                    "verification_failed",
                    15,
                    format!("audit entry #{seq} invalid signature"),
                )
            })?;
        let arr: [u8; 64] = rs.try_into().map_err(|_| {
            RpcErr::new(
                "verification_failed",
                15,
                format!("audit entry #{seq} invalid signature length"),
            )
        })?;
        if !trusted_key.verify(AUDIT_DOMAIN, payload.as_bytes(), &arr) {
            return Err(RpcErr::new(
                "verification_failed",
                15,
                format!("audit entry #{seq} signature verification failed"),
            ));
        }
        expected_prev = Some((seq, hash.to_string()));
    }

    // Re-verify epitaph links and Merkle root.
    let epitaphs = body["epitaphs"].as_array().cloned().unwrap_or_default();
    let mut hashes = Vec::with_capacity(epitaphs.len());
    for ep in &epitaphs {
        let link = ep["link"].as_str().unwrap_or("");
        let sig = ep["sig"].as_str().unwrap_or("");
        let hash = ep["hash"].as_str().unwrap_or("");
        crate::epitaph::check_link(link, sig, trusted_key)
            .map_err(|e| RpcErr::new("verification_failed", 15, format!("epitaph link: {e}")))?;
        if crate::epitaph::hash(link) != hash {
            return Err(RpcErr::new(
                "verification_failed",
                15,
                "epitaph hash column mismatch",
            ));
        }
        hashes.push(hash.to_string());
    }
    let expected_merkle = crate::epitaph::merkle_root(&hashes);
    if body["epitaph_merkle_root"].as_str() != Some(expected_merkle.as_str()) {
        return Err(RpcErr::new(
            "verification_failed",
            15,
            "epitaph Merkle root mismatch",
        ));
    }

    Ok(json!({
        "ok": true,
        "file": file,
        "core_id": trusted_key.id(),
        "bundle_hash": computed_hash,
        "audit_entries": audit_entries.len(),
        "epitaphs": epitaphs.len(),
        "epitaph_merkle_root": expected_merkle,
    }))
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

    #[test]
    fn audit_export_verify_bundle_and_prune_preserve_chain() {
        let p = test_paths("bundle-prune");
        let core = Core::open(p.clone()).unwrap();
        let dir = p.state.parent().unwrap();

        // Run a pure ritual so we have an ended summon + epitaph + audit entries.
        let rf = dir.join("pure.yaml");
        std::fs::write(
            &rf,
            "ritual: pure1\nessence: {requests: 0, time: 1m}\nsteps:\n  - id: s\n    operation: data.trim\n    arguments: {text: \"  secret  \"}\nreturn:\n  out: \"${steps.s.output}\"\n",
        )
        .unwrap();
        core.inscribe(json!({"path": rf.to_str().unwrap()})).unwrap();
        let started = core.invoke(json!({"target": "pure1", "cwd": "/"})).unwrap();
        let sid = started["summon"].as_str().unwrap().to_string();
        let ended = core.wait(json!({"summon": sid, "timeout_ms": 10_000})).unwrap();
        assert_eq!(ended["outcome"], "returned");

        // Export signed compliance evidence bundle and verify offline.
        let bundle_file = dir.join("evidence.bundle.json");
        let exp = core
            .audit_export(json!({"file": bundle_file.to_str().unwrap()}))
            .unwrap();
        assert_eq!(exp["epitaphs"], 1);
        let verified =
            verify_evidence_bundle(bundle_file.to_str().unwrap(), &core.key.public()).unwrap();
        assert_eq!(verified["ok"], true, "{verified}");
        assert_eq!(verified["epitaphs"], 1);

        // Tampering with the exported bundle must be caught offline.
        let mut bad: Value =
            serde_json::from_str(&std::fs::read_to_string(&bundle_file).unwrap()).unwrap();
        bad["bundle"]["controls"]["rbac_enforce"] = json!(true);
        let bad_file = dir.join("bad.bundle.json");
        std::fs::write(&bad_file, serde_json::to_string(&bad).unwrap()).unwrap();
        let err = verify_evidence_bundle(bad_file.to_str().unwrap(), &core.key.public())
            .unwrap_err();
        assert_eq!(err.exit, 15);

        // Prune ended summons (days=0 -> prune everything ended <= now) and confirm
        // plaintext summon/events are removed while epitaph chain + audit_log still verify ok!
        let dry = core.audit_prune(json!({"days": 0, "dry_run": true})).unwrap();
        assert_eq!(dry["would_prune"], 1);
        let pruned = core.audit_prune(json!({"days": 0})).unwrap();
        assert_eq!(pruned["pruned"], 1);
        let cv = core.chain_verify(json!({})).unwrap();
        assert_eq!(cv["ok"], true, "chain_verify must remain ok after pruning: {cv}");
        assert_eq!(cv["not_checkable"].as_array().unwrap().len(), 1);
        assert_eq!(core.audit_verify().unwrap()["ok"], true);

        drop(core);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn execution_record_chaining_siem_export_and_tamper_detection() {
        let p = test_paths("siem-tamper");
        let core = Core::open(p.clone()).unwrap();
        let dir = p.state.parent().unwrap().to_path_buf();

        // 1. Run a ritual so `finish_summon` cross-anchors `summon.finished` into `audit_log`.
        let rf = dir.join("pure.yaml");
        std::fs::write(
            &rf,
            "ritual: pure-audit\nessence: {requests: 0, time: 1m}\nsteps:\n  - id: s\n    operation: data.trim\n    arguments: {text: \"  ok  \"}\nreturn:\n  out: \"${steps.s.output}\"\n",
        )
        .unwrap();
        core.inscribe(json!({"path": rf.to_str().unwrap()})).unwrap();
        let started = core.invoke(json!({"target": "pure-audit", "cwd": "/"})).unwrap();
        let sid = started["summon"].as_str().unwrap().to_string();
        let ended = core.wait(json!({"summon": sid, "timeout_ms": 10_000})).unwrap();
        assert_eq!(ended["outcome"], "returned");

        // 2. Also record a policy denial so both execution records & denials are chained.
        core.record_audit_as(
            "operator:eve",
            "rbac.reagent_denied",
            "mail.send",
            "denied",
            json!({"reason": "no write permission"}),
        );

        let before = core.audit_verify().unwrap();
        assert_eq!(before["ok"], true, "{before}");

        // 3. Export clean JSON (stdout mode) & NDJSON (SIEM mode) and verify contents.
        let json_out = core.audit_export(json!({"format": "json"})).unwrap();
        let entries = json_out["bundle"]["audit_log"].as_array().unwrap();
        let exec_entry = entries
            .iter()
            .find(|e| e["action"] == "summon.finished" && e["target"] == sid)
            .expect("summon.finished must be hash-chained in audit_log");
        assert_eq!(exec_entry["outcome"], "returned");
        assert!(!exec_entry["detail"]["epitaph_hash"].as_str().unwrap_or("").is_empty());
        assert!(entries.iter().any(|e| e["action"] == "rbac.reagent_denied" && e["outcome"] == "denied"));
        let exec_seq = exec_entry["seq"].as_i64().unwrap();

        let ndjson_out = core.audit_export(json!({"format": "ndjson"})).unwrap();
        let lines: Vec<&str> = ndjson_out["ndjson"].as_str().unwrap().lines().collect();
        assert!(lines.len() >= 3);
        let manifest: Value = serde_json::from_str(lines.last().unwrap()).unwrap();
        assert_eq!(manifest["record_type"], "summonr-siem-manifest-v1");

        // 4. Simulate tampering with the execution record row in SQLite and verify detection.
        {
            let db = core.db.lock().unwrap();
            db.execute_batch(&format!(
                "DROP TRIGGER IF EXISTS audit_log_no_update;
                 UPDATE audit_log SET outcome='failed' WHERE seq={exec_seq};"
            ))
            .unwrap();
        }
        let after = core.audit_verify().unwrap();
        assert_eq!(after["ok"], false, "expected tamper detection: {after}");
        assert_eq!(after["problem"]["seq"], exec_seq, "{after}");

        drop(core);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn verify_evidence_bundle_rejects_omitted_audit_entries() {
        let p = test_paths("bundle-gap");
        let core = Core::open(p.clone()).unwrap();
        let dir = p.state.parent().unwrap().to_path_buf();

        core.record_audit("test.one", "t1", "ok", json!({}));
        core.record_audit("test.two", "t2", "ok", json!({}));

        let bundle_file = dir.join("orig.bundle.json");
        core.audit_export(json!({"file": bundle_file.to_str().unwrap()}))
            .unwrap();

        // Remove the middle audit entry and re-sign the bundle with the core's key
        // to simulate a compromised exporter omitting a row.
        let mut env: Value =
            serde_json::from_str(&std::fs::read_to_string(&bundle_file).unwrap()).unwrap();
        let entries = env["bundle"]["audit_log"].as_array_mut().unwrap();
        assert!(entries.len() >= 3);
        entries.remove(1);
        let canonical = crate::util::canonical_json(&env["bundle"]);
        let new_hash = sha256_hex(canonical.as_bytes());
        let new_sig = base64::engine::general_purpose::URL_SAFE_NO_PAD
            .encode(core.key.sign(BUNDLE_DOMAIN, canonical.as_bytes()));
        env["bundle_hash"] = json!(new_hash);
        env["signature"] = json!(new_sig);

        let gap_file = dir.join("gap.bundle.json");
        std::fs::write(&gap_file, serde_json::to_string(&env).unwrap()).unwrap();
        let err = verify_evidence_bundle(gap_file.to_str().unwrap(), &core.key.public())
            .unwrap_err();
        assert_eq!(err.exit, 15);
        assert!(
            err.message.contains("sequence gap"),
            "expected sequence gap error, got: {}",
            err.message
        );

        drop(core);
        let _ = std::fs::remove_dir_all(dir);
    }
}
