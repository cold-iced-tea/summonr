//! Operator RBAC: Ed25519-authenticated human/service operators with explicit
//! roles (`author`, `security_admin`, `reviewer`, `auditor`, `admin`) and
//! separation-of-duties enforcement.
use super::*;
use crate::keys::{self, PublicKey};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum OperatorRole {
    Author,
    SecurityAdmin,
    Reviewer,
    Auditor,
    Admin,
}

impl OperatorRole {
    pub(crate) fn parse(s: &str) -> Result<Self, RpcErr> {
        match s {
            "author" => Ok(Self::Author),
            "security_admin" => Ok(Self::SecurityAdmin),
            "reviewer" => Ok(Self::Reviewer),
            "auditor" => Ok(Self::Auditor),
            "admin" => Ok(Self::Admin),
            other => Err(RpcErr::invalid(format!(
                "unknown operator role `{other}`; use author, security_admin, reviewer, auditor, or admin"
            ))),
        }
    }

    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::Author => "author",
            Self::SecurityAdmin => "security_admin",
            Self::Reviewer => "reviewer",
            Self::Auditor => "auditor",
            Self::Admin => "admin",
        }
    }

    /// Explicit role-to-RPC allowlist. Any method not listed for a role is refused.
    pub(crate) fn allows(self, method: &str) -> bool {
        let read_only = matches!(
            method,
            "ping"
                | "identity"
                | "rituals"
                | "ritual_read"
                | "contract"
                | "tools"
                | "get"
                | "wait"
                | "events"
                | "list"
                | "petitions"
                | "fence_list"
                | "familiar_list"
                | "peer_list"
                | "operator_list"
                | "pact_list"
                | "pact_show"
                | "pact_history"
                | "pact_call_status"
                | "reliquary_list"
                | "reliquary_show"
                | "rite_list"
                | "rite_show"
                | "rite_events"
                | "epitaph_get"
                | "epitaph_export"
                | "epitaph_prove"
                | "chain_head"
                | "chain_verify"
                | "audit_list"
                | "audit_verify"
                | "witness_list"
                | "witness_compare"
                | "job_status"
                | "job_item"
                | "session_list"
                | "session_get"
        );
        match self {
            Self::Admin => true,
            Self::Auditor => read_only,
            Self::Author => {
                read_only
                    || matches!(
                        method,
                        "inscribe"
                            | "compile"
                            | "rehearse"
                            | "invoke"
                            | "cancel"
                            | "revive"
                            | "recast"
                            | "drill"
                            | "drill_inline"
                            | "assay"
                            | "assay_compare"
                            | "glean"
                            | "experiment_sweep"
                            | "experiment_judge"
                            | "experiment_weigh"
                            | "reliquary_publish"
                            | "reliquary_retract"
                            | "reliquary_export"
                            | "rite_rehearse"
                            | "rite_start"
                            | "rite_advance"
                            | "session_open"
                            | "session_close"
                            | "session_soul"
                            | "session_bind"
                            | "session_imbue"
                            | "session_bid"
                            | "session_lethe"
                            | "session_inscribe"
                            | "job_create"
                            | "job_work"
                            | "job_retry"
                    )
            }
            Self::SecurityAdmin => {
                read_only
                    || matches!(
                        method,
                        "grant"
                            | "revoke"
                            | "familiar_new"
                            | "familiar_add"
                            | "familiar_register_grant"
                            | "familiar_allow"
                            | "familiar_disallow"
                            | "familiar_revoke"
                            | "peer_add"
                            | "peer_remove"
                            | "operator_new"
                            | "operator_add"
                            | "operator_revoke"
                            | "pact_admit"
                            | "pact_unadmit"
                            | "pact_limit"
                            | "reliquary_add_peer"
                            | "reliquary_remove_peer"
                            | "reliquary_inscribe"
                            | "reliquary_import"
                            | "witness_accept"
                            | "witness_drop"
                            | "witness_to"
                            | "witness_tsa"
                            | "fence_resolve"
                    )
            }
            Self::Reviewer => {
                read_only
                    || matches!(
                        method,
                        "petition_decide"
                            | "pact_agree"
                            | "pact_decline"
                            | "pact_accept"
                            | "pact_reject"
                            | "pact_end"
                            | "rite_falsify"
                            | "rite_assert"
                    )
            }
        }
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct NewOp {
    name: String,
    role: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct AddOp {
    name: String,
    role: String,
    key: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct NamedOp {
    name: String,
}

fn parse<T: serde::de::DeserializeOwned>(p: Value) -> Result<T, RpcErr> {
    serde_json::from_value(p).map_err(|e| RpcErr::invalid(e.to_string()))
}

fn sql(e: rusqlite::Error) -> RpcErr {
    RpcErr::new("storage_failed", 11, e.to_string())
}

impl Core {
    fn operator_exists(&self, name: &str, key_id: &str) -> Result<bool, RpcErr> {
        self.db
            .lock()
            .unwrap()
            .query_row(
                "SELECT COUNT(*) FROM operators WHERE name=?1 OR key_id=?2",
                params![name, key_id],
                |r| r.get::<_, i64>(0),
            )
            .map(|n| n > 0)
            .map_err(sql)
    }

    pub(super) fn operator_new(&self, p: Value) -> Result<Value, RpcErr> {
        let NewOp { name, role } = parse(p)?;
        super::familiars::valid_name(&name)?;
        let role = OperatorRole::parse(&role)?;
        let dir = self.paths.config.join("operators");
        let file = dir.join(format!("{name}.key"));
        if self.operator_exists(&name, "")? {
            return Err(RpcErr::conflict(format!(
                "an operator named `{name}` already exists; names are never reused"
            )));
        }
        if file.exists() {
            return Err(RpcErr::conflict(format!(
                "{} already exists on disk; remove or move it first",
                file.display()
            )));
        }
        let k = keys::generate();
        let pubkey = k.public();
        let tmp = dir.join(format!(".{name}.key.{}", new_id("tmp")));
        keys::save_new(&tmp, &k).map_err(|e| RpcErr::new("storage_failed", 11, e))?;
        let ins = self.db.lock().unwrap().execute(
            "INSERT INTO operators (name, role, key, key_id, created) VALUES (?1, ?2, ?3, ?4, ?5)",
            params![name, role.as_str(), pubkey.text(), pubkey.id(), now_ms()],
        );
        if let Err(e) = ins {
            let _ = std::fs::remove_file(&tmp);
            return Err(sql(e));
        }
        let placed = std::fs::hard_link(&tmp, &file);
        let _ = std::fs::remove_file(&tmp);
        if let Err(e) = placed {
            let _ = self
                .db
                .lock()
                .unwrap()
                .execute("DELETE FROM operators WHERE name=?1", params![name]);
            return Err(RpcErr::new(
                "storage_failed",
                11,
                format!("cannot place key file {}: {e}", file.display()),
            ));
        }
        self.record_audit(
            "operator.new",
            &name,
            "ok",
            json!({"role": role.as_str(), "id": pubkey.id()}),
        );
        Ok(json!({
            "name": name,
            "role": role.as_str(),
            "key": pubkey.text(),
            "id": pubkey.id(),
            "key_file": file,
        }))
    }

    pub(super) fn operator_add(&self, p: Value) -> Result<Value, RpcErr> {
        let AddOp { name, role, key } = parse(p)?;
        super::familiars::valid_name(&name)?;
        let role = OperatorRole::parse(&role)?;
        let pubkey = PublicKey::parse(&key).map_err(RpcErr::invalid)?;
        if self.operator_exists(&name, &pubkey.id())? {
            return Err(RpcErr::conflict(format!(
                "an operator named `{name}` or with this key already exists"
            )));
        }
        self.db
            .lock()
            .unwrap()
            .execute(
                "INSERT INTO operators (name, role, key, key_id, created) VALUES (?1, ?2, ?3, ?4, ?5)",
                params![name, role.as_str(), pubkey.text(), pubkey.id(), now_ms()],
            )
            .map_err(sql)?;
        self.record_audit(
            "operator.add",
            &name,
            "ok",
            json!({"role": role.as_str(), "id": pubkey.id()}),
        );
        Ok(json!({"name": name, "role": role.as_str(), "id": pubkey.id()}))
    }

    pub(super) fn operator_list(&self) -> Result<Value, RpcErr> {
        let db = self.db.lock().unwrap();
        let mut st = db
            .prepare("SELECT name, role, key, key_id, created, revoked FROM operators ORDER BY name")
            .map_err(sql)?;
        let rows: Vec<Value> = st
            .query_map([], |r| {
                Ok(json!({
                    "name": r.get::<_, String>(0)?,
                    "role": r.get::<_, String>(1)?,
                    "key": r.get::<_, String>(2)?,
                    "id": r.get::<_, String>(3)?,
                    "created": r.get::<_, i64>(4)?,
                    "revoked": r.get::<_, Option<i64>>(5)?,
                }))
            })
            .map_err(sql)?
            .collect::<Result<_, _>>()
            .map_err(sql)?;
        Ok(Value::Array(rows))
    }

    pub(super) fn operator_revoke(&self, p: Value) -> Result<Value, RpcErr> {
        let NamedOp { name } = parse(p)?;
        let n = self
            .db
            .lock()
            .unwrap()
            .execute(
                "UPDATE operators SET revoked=?2 WHERE name=?1 AND revoked IS NULL",
                params![name, now_ms()],
            )
            .map_err(sql)?;
        if n == 0 {
            return Err(RpcErr::not_found(format!("no active operator `{name}`")));
        }
        self.record_audit("operator.revoke", &name, "ok", json!({"revoked": true}));
        Ok(json!({"name": name, "revoked": true}))
    }

    pub(super) fn operator_by_key(&self, key_id: &str) -> Option<(String, OperatorRole, PublicKey)> {
        let row: Option<(String, String, String)> = self
            .db
            .lock()
            .unwrap()
            .query_row(
                "SELECT name, role, key FROM operators WHERE key_id=?1 AND revoked IS NULL",
                params![key_id],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .optional()
            .ok()
            .flatten();
        let (name, role_s, key_s) = row?;
        let role = OperatorRole::parse(&role_s).ok()?;
        let pk = PublicKey::parse(&key_s).ok()?;
        Some((name, role, pk))
    }

    /// Dispatch a signed RPC from an authenticated operator under their role's allowlist.
    pub(super) fn dispatch_operator(
        self: &Arc<Self>,
        name: &str,
        role: OperatorRole,
        method: &str,
        mut params: Value,
    ) -> Result<Value, RpcErr> {
        let actor = format!("operator:{name}({})", role.as_str());
        if !role.allows(method) {
            self.record_audit_as(
                &actor,
                "rbac.denied",
                method,
                "denied",
                json!({"role": role.as_str()}),
            );
            return Err(RpcErr::new(
                "not_authorized",
                4,
                format!(
                    "operator `{name}` with role `{}` is not authorized for `{method}`",
                    role.as_str()
                ),
            ));
        }
        // Stamp operator caller identity on `invoke` so separation-of-duties can check it.
        if method == "invoke" {
            if let Some(m) = params.as_object_mut() {
                m.insert("caller".into(), json!(format!("operator:{name}")));
            }
        }
        let _guard = super::audit::ActorGuard::enter(actor);
        self.dispatch(method, params)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::envelope::{new_nonce, open_reply, seal_request};

    fn test_paths(label: &str) -> Paths {
        let dir = std::env::temp_dir().join(format!("summonr-rbac-{label}-{}", new_id("t")));
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

    fn signed_call(core: &Arc<Core>, key: &keys::Keypair, method: &str, params: Value) -> Result<Value, RpcErr> {
        let nonce = new_nonce();
        let frame = seal_request(key, &core.key.public().id(), method, &params, now_ms(), &nonce);
        let reply = core.handle_signed(&frame);
        open_reply(&reply, &core.key.public(), &nonce).unwrap()
    }

    #[test]
    fn rbac_enforces_roles_and_logs_denials_to_audit_log() {
        let p = test_paths("roles");
        let core = Core::open(p.clone()).unwrap();
        let dir = p.state.parent().unwrap();

        let author_key = keys::generate();
        let sec_key = keys::generate();
        let auditor_key = keys::generate();

        core.operator_add(json!({"name": "alice", "role": "author", "key": author_key.public().text()})).unwrap();
        core.operator_add(json!({"name": "bob", "role": "security_admin", "key": sec_key.public().text()})).unwrap();
        core.operator_add(json!({"name": "carol", "role": "auditor", "key": auditor_key.public().text()})).unwrap();

        // Author can inscribe a ritual, but CANNOT grant authority.
        let rf = dir.join("r.yaml");
        std::fs::write(
            &rf,
            "ritual: r1\nessence: {requests: 0, time: 1m}\nrequires:\n  - reagent: mail\n    action: locate\nsteps:\n  - id: s\n    operation: mail.locate\n    arguments: {message: \"m1\"}\nreturn:\n  ok: true\n",
        )
        .unwrap();
        signed_call(&core, &author_key, "inscribe", json!({"path": rf.to_str().unwrap()})).unwrap();
        let err = signed_call(
            &core,
            &author_key,
            "grant",
            json!({"ritual": "r1", "scopes": [{"reagent": "mail", "select": ["locate"]}]}),
        )
        .unwrap_err();
        assert_eq!(err.code, "not_authorized");

        // Security admin CAN grant, but CANNOT inscribe.
        signed_call(
            &core,
            &sec_key,
            "grant",
            json!({"ritual": "r1", "scopes": [{"reagent": "mail", "select": ["locate"]}]}),
        )
        .unwrap();
        let err2 = signed_call(&core, &sec_key, "inscribe", json!({"path": rf.to_str().unwrap()})).unwrap_err();
        assert_eq!(err2.code, "not_authorized");

        // Auditor can read and verify audit_log, but cannot mutate anything.
        let av = signed_call(&core, &auditor_key, "audit_verify", json!({})).unwrap();
        assert_eq!(av["ok"], true);
        let list = signed_call(&core, &auditor_key, "audit_list", json!({})).unwrap();
        assert!(
            list.as_array()
                .unwrap()
                .iter()
                .any(|e| e["action"] == "rbac.denied" && e["actor"] == "operator:alice(author)"),
            "expected rbac.denied in audit_log: {list}"
        );
        let err3 = signed_call(&core, &auditor_key, "revoke", json!({"ritual": "r1", "scopes": []})).unwrap_err();
        assert_eq!(err3.code, "not_authorized");

        drop(core);
        let _ = std::fs::remove_dir_all(dir);
    }
}
