//! Normalized SQLite Role-Based Access Control (RBAC) for reagent management
//! and ritual execution.
//!
//! Built-in roles (`admin`, `policy_author`, `agent_runner`, `reviewer`,
//! `auditor`, plus legacy aliases `author` and `security_admin`) are seeded on
//! `Core::open`. Custom roles, fine-grained resource permissions
//! (`rpc`, `reagent`, `ritual`), and actor bindings are stored in SQLite and
//! evaluated live before reagent execution in `src/exec.rs`.
use super::*;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ResourceType {
    Rpc,
    Reagent,
    Ritual,
}

impl ResourceType {
    pub fn parse(s: &str) -> Result<Self, RpcErr> {
        match s {
            "rpc" => Ok(Self::Rpc),
            "reagent" => Ok(Self::Reagent),
            "ritual" => Ok(Self::Ritual),
            other => Err(RpcErr::invalid(format!(
                "unknown resource_type `{other}`; use rpc, reagent, or ritual"
            ))),
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Rpc => "rpc",
            Self::Reagent => "reagent",
            Self::Ritual => "ritual",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PermissionAction {
    Invoke,
    Read,
    Write,
    Manage,
    Approve,
    Audit,
    All,
}

impl PermissionAction {
    pub fn parse(s: &str) -> Result<Self, RpcErr> {
        match s {
            "invoke" => Ok(Self::Invoke),
            "read" => Ok(Self::Read),
            "write" => Ok(Self::Write),
            "manage" => Ok(Self::Manage),
            "approve" => Ok(Self::Approve),
            "audit" => Ok(Self::Audit),
            "*" => Ok(Self::All),
            other => Err(RpcErr::invalid(format!(
                "unknown permission action `{other}`; use invoke, read, write, manage, approve, audit, or *"
            ))),
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Invoke => "invoke",
            Self::Read => "read",
            Self::Write => "write",
            Self::Manage => "manage",
            Self::Approve => "approve",
            Self::Audit => "audit",
            Self::All => "*",
        }
    }

    pub fn matches(self, requested: Self) -> bool {
        self == Self::All || self == requested
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Permission {
    pub resource_type: ResourceType,
    pub resource_pattern: String,
    pub action: PermissionAction,
}

impl Permission {
    pub fn allows(&self, resource_type: ResourceType, target: &str, action: PermissionAction) -> bool {
        self.resource_type == resource_type
            && self.action.matches(action)
            && pattern_matches(&self.resource_pattern, target)
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ActorContext {
    pub actor_id: String,
    pub roles: Vec<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RbacDecision {
    Allow { matched_role: String },
    Deny { reason: String },
}

/// Match a resource pattern (`*`, `prefix.*`, or exact name) against a target.
pub fn pattern_matches(pattern: &str, target: &str) -> bool {
    if pattern == "*" || pattern == target {
        return true;
    }
    if let Some(prefix) = pattern.strip_suffix(".*") {
        return target
            .strip_prefix(prefix)
            .is_some_and(|rest| rest.starts_with('.'));
    }
    false
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RoleCreate {
    name: String,
    #[serde(default)]
    description: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct PermSpec {
    role: String,
    resource_type: String,
    resource_pattern: String,
    action: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ActorBind {
    actor_id: String,
    role: String,
}

fn parse<T: serde::de::DeserializeOwned>(p: Value) -> Result<T, RpcErr> {
    serde_json::from_value(p).map_err(|e| RpcErr::invalid(e.to_string()))
}

fn sql(e: rusqlite::Error) -> RpcErr {
    RpcErr::new("storage_failed", 11, e.to_string())
}

const BUILTIN_ROLES: &[(&str, &str)] = &[
    ("admin", "Full administrative authority over all RPCs, rituals, and reagents"),
    (
        "policy_author",
        "Authors rituals and configures grants/reagents; cannot perform live reagent writes",
    ),
    (
        "agent_runner",
        "Invokes granted rituals and executes authorized reagents",
    ),
    (
        "reviewer",
        "Approves or declines held petitions and pacts under separation of duties",
    ),
    (
        "auditor",
        "Read-only access to audit logs, proofs, and compliance exports",
    ),
    (
        "author",
        "Legacy combined author + runner role for ritual authoring and execution",
    ),
    (
        "security_admin",
        "Manages grants, operators, familiars, peers, and RBAC bindings",
    ),
];

const BUILTIN_PERMS: &[(&str, &str, &str, &str)] = &[
    // admin: unrestricted
    ("admin", "rpc", "*", "*"),
    ("admin", "reagent", "*", "*"),
    ("admin", "ritual", "*", "*"),
    // policy_author: manage rituals/reagents, read-only reagent calls (no live writes)
    ("policy_author", "reagent", "*", "read"),
    ("policy_author", "reagent", "*", "manage"),
    ("policy_author", "ritual", "*", "manage"),
    ("policy_author", "ritual", "*", "read"),
    // agent_runner: execute rituals and granted reagents (read + write)
    ("agent_runner", "reagent", "*", "read"),
    ("agent_runner", "reagent", "*", "write"),
    ("agent_runner", "ritual", "*", "invoke"),
    ("agent_runner", "ritual", "*", "read"),
    // reviewer: read-only + petition/pact approval
    ("reviewer", "reagent", "*", "read"),
    ("reviewer", "ritual", "*", "approve"),
    ("reviewer", "ritual", "*", "read"),
    // auditor: audit + read-only (no reagent execution)
    ("auditor", "ritual", "*", "audit"),
    ("auditor", "ritual", "*", "read"),
    // author (legacy alias): authoring + execution
    ("author", "reagent", "*", "read"),
    ("author", "reagent", "*", "write"),
    ("author", "ritual", "*", "manage"),
    ("author", "ritual", "*", "invoke"),
    ("author", "ritual", "*", "read"),
    // security_admin: governance & grants (no live reagent writes)
    ("security_admin", "reagent", "*", "manage"),
    ("security_admin", "reagent", "*", "read"),
    ("security_admin", "ritual", "*", "manage"),
    ("security_admin", "ritual", "*", "read"),
];

impl Core {
    /// Seed built-in roles/permissions and backfill any existing active operators
    /// into `rbac_actor_roles`. Idempotent across restarts.
    pub(super) fn seed_rbac(&self) -> Result<(), String> {
        let now = now_ms();
        let db = self.db.lock().unwrap();
        for &(name, desc) in BUILTIN_ROLES {
            db.execute(
                "INSERT OR IGNORE INTO rbac_roles (name, description, builtin, created)
                 VALUES (?1, ?2, 1, ?3)",
                params![name, desc, now],
            )
            .map_err(|e| format!("cannot seed rbac_roles: {e}"))?;
        }
        for &(role, rtype, pattern, action) in BUILTIN_PERMS {
            db.execute(
                "INSERT OR IGNORE INTO rbac_role_permissions
                 (role_name, resource_type, resource_pattern, action)
                 VALUES (?1, ?2, ?3, ?4)",
                params![role, rtype, pattern, action],
            )
            .map_err(|e| format!("cannot seed rbac_role_permissions: {e}"))?;
        }
        // Backfill active rows from `operators` so existing operators automatically
        // have corresponding `rbac_actor_roles` bindings.
        db.execute(
            "INSERT OR IGNORE INTO rbac_actor_roles (actor_id, role_name, granted_by, granted_at, revoked_at)
             SELECT 'operator:' || name, role, 'system:migration', created, revoked
             FROM operators",
            [],
        )
        .map_err(|e| format!("cannot backfill rbac_actor_roles: {e}"))?;
        Ok(())
    }

    pub(super) fn rbac_sync_operator_role(&self, op_name: &str, role: &str) {
        let actor_id = format!("operator:{op_name}");
        let by = super::audit::current_actor();
        let _ = self.db.lock().unwrap().execute(
            "INSERT INTO rbac_actor_roles (actor_id, role_name, granted_by, granted_at, revoked_at)
             VALUES (?1, ?2, ?3, ?4, NULL)
             ON CONFLICT(actor_id, role_name) DO UPDATE SET revoked_at=NULL, granted_by=?3, granted_at=?4",
            params![actor_id, role, by, now_ms()],
        );
    }

    pub(super) fn rbac_revoke_operator_roles(&self, op_name: &str) {
        let actor_id = format!("operator:{op_name}");
        let _ = self.db.lock().unwrap().execute(
            "UPDATE rbac_actor_roles SET revoked_at=?2 WHERE actor_id=?1 AND revoked_at IS NULL",
            params![actor_id, now_ms()],
        );
    }

    /// Load the active role list for an actor id (e.g. `operator:alice`).
    pub(crate) fn actor_context_for(&self, actor_id: &str) -> ActorContext {
        let db = self.db.lock().unwrap();
        let roles = db
            .prepare(
                "SELECT role_name FROM rbac_actor_roles
                 WHERE actor_id=?1 AND revoked_at IS NULL
                 ORDER BY role_name",
            )
            .and_then(|mut st| {
                st.query_map(params![actor_id], |r| r.get::<_, String>(0))
                    .map(|rows| rows.filter_map(Result::ok).collect::<Vec<_>>())
            })
            .unwrap_or_default();
        ActorContext {
            actor_id: actor_id.to_string(),
            roles,
        }
    }

    /// Normalize an audit actor string such as `operator:alice(agent_runner)`
    /// into its canonical actor id `operator:alice`.
    pub(super) fn normalize_actor_id(raw: &str) -> String {
        if raw.starts_with("operator:") {
            if let Some(idx) = raw.find('(') {
                return raw[..idx].to_string();
            }
        }
        raw.to_string()
    }

    /// Pre-execution RBAC policy check for a reagent call (`op` like `mail.send`,
    /// `action` = `Read` or `Write`). Queries live SQLite state so revocations
    /// take effect immediately even mid-run.
    pub(crate) fn authorize_reagent(
        &self,
        actor: Option<&ActorContext>,
        op: &str,
        action: PermissionAction,
    ) -> RbacDecision {
        let Some(ctx) = actor else {
            if self.cfg.rbac.enforce {
                return RbacDecision::Deny {
                    reason: "rbac.enforce is enabled but no authenticated actor context is bound to this run".into(),
                };
            }
            return RbacDecision::Allow {
                matched_role: "local:owner".into(),
            };
        };

        let db = self.db.lock().unwrap();
        let active_roles: Vec<String> = db
            .prepare(
                "SELECT role_name FROM rbac_actor_roles
                 WHERE actor_id=?1 AND revoked_at IS NULL
                 ORDER BY role_name",
            )
            .and_then(|mut st| {
                st.query_map(params![ctx.actor_id], |r| r.get::<_, String>(0))
                    .map(|rows| rows.filter_map(Result::ok).collect::<Vec<_>>())
            })
            .unwrap_or_default();

        if active_roles.is_empty() {
            if !self.cfg.rbac.enforce && ctx.actor_id.starts_with("local:") {
                return RbacDecision::Allow {
                    matched_role: "local:owner".into(),
                };
            }
            return RbacDecision::Deny {
                reason: format!("actor `{}` has no active RBAC roles", ctx.actor_id),
            };
        }

        let perms: Vec<(String, Permission)> = db
            .prepare(
                "SELECT p.role_name, p.resource_type, p.resource_pattern, p.action
                 FROM rbac_role_permissions p
                 JOIN rbac_actor_roles a ON a.role_name = p.role_name
                 WHERE a.actor_id = ?1
                   AND a.revoked_at IS NULL
                   AND p.resource_type = 'reagent'
                 ORDER BY p.role_name",
            )
            .and_then(|mut st| {
                st.query_map(params![ctx.actor_id], |r| {
                    Ok((
                        r.get::<_, String>(0)?,
                        r.get::<_, String>(1)?,
                        r.get::<_, String>(2)?,
                        r.get::<_, String>(3)?,
                    ))
                })
                .map(|rows| {
                    rows.filter_map(Result::ok)
                        .filter_map(|(role_name, rtype, pattern, act)| {
                            Some((
                                role_name,
                                Permission {
                                    resource_type: ResourceType::parse(&rtype).ok()?,
                                    resource_pattern: pattern,
                                    action: PermissionAction::parse(&act).ok()?,
                                },
                            ))
                        })
                        .collect::<Vec<_>>()
                })
            })
            .unwrap_or_default();

        for (role_name, perm) in perms {
            if perm.allows(ResourceType::Reagent, op, action) {
                return RbacDecision::Allow {
                    matched_role: role_name,
                };
            }
        }

        RbacDecision::Deny {
            reason: format!(
                "roles [{}] do not grant reagent `{}` on `{op}`",
                active_roles.join(", "),
                action.as_str()
            ),
        }
    }

    pub(super) fn rbac_role_create(&self, p: Value) -> Result<Value, RpcErr> {
        let RoleCreate { name, description } = parse(p)?;
        super::familiars::valid_name(&name)?;
        let n = self
            .db
            .lock()
            .unwrap()
            .execute(
                "INSERT OR IGNORE INTO rbac_roles (name, description, builtin, created)
                 VALUES (?1, ?2, 0, ?3)",
                params![name, description, now_ms()],
            )
            .map_err(sql)?;
        if n == 0 {
            return Err(RpcErr::conflict(format!("role `{name}` already exists")));
        }
        self.record_audit(
            "rbac.role_create",
            &name,
            "ok",
            json!({"description": description}),
        );
        Ok(json!({"name": name, "description": description}))
    }

    pub(super) fn rbac_permission_grant(&self, p: Value) -> Result<Value, RpcErr> {
        let PermSpec {
            role,
            resource_type,
            resource_pattern,
            action,
        } = parse(p)?;
        let rtype = ResourceType::parse(&resource_type)?;
        let act = PermissionAction::parse(&action)?;
        if resource_pattern.trim().is_empty() {
            return Err(RpcErr::invalid("resource_pattern must not be empty"));
        }
        let exists: i64 = self
            .db
            .lock()
            .unwrap()
            .query_row(
                "SELECT COUNT(*) FROM rbac_roles WHERE name=?1",
                params![role],
                |r| r.get(0),
            )
            .map_err(sql)?;
        if exists == 0 {
            return Err(RpcErr::not_found(format!("unknown role `{role}`")));
        }
        self.db
            .lock()
            .unwrap()
            .execute(
                "INSERT OR IGNORE INTO rbac_role_permissions
                 (role_name, resource_type, resource_pattern, action)
                 VALUES (?1, ?2, ?3, ?4)",
                params![role, rtype.as_str(), resource_pattern, act.as_str()],
            )
            .map_err(sql)?;
        self.record_audit(
            "rbac.permission_grant",
            &role,
            "ok",
            json!({
                "resource_type": rtype.as_str(),
                "resource_pattern": resource_pattern,
                "action": act.as_str(),
            }),
        );
        Ok(json!({
            "role": role,
            "resource_type": rtype.as_str(),
            "resource_pattern": resource_pattern,
            "action": act.as_str(),
        }))
    }

    pub(super) fn rbac_permission_revoke(&self, p: Value) -> Result<Value, RpcErr> {
        let PermSpec {
            role,
            resource_type,
            resource_pattern,
            action,
        } = parse(p)?;
        let rtype = ResourceType::parse(&resource_type)?;
        let act = PermissionAction::parse(&action)?;
        let n = self
            .db
            .lock()
            .unwrap()
            .execute(
                "DELETE FROM rbac_role_permissions
                 WHERE role_name=?1 AND resource_type=?2 AND resource_pattern=?3 AND action=?4",
                params![role, rtype.as_str(), resource_pattern, act.as_str()],
            )
            .map_err(sql)?;
        if n == 0 {
            return Err(RpcErr::not_found(format!(
                "permission ({}, {}, {}) not found on role `{role}`",
                rtype.as_str(),
                resource_pattern,
                act.as_str()
            )));
        }
        self.record_audit(
            "rbac.permission_revoke",
            &role,
            "ok",
            json!({
                "resource_type": rtype.as_str(),
                "resource_pattern": resource_pattern,
                "action": act.as_str(),
            }),
        );
        Ok(json!({
            "role": role,
            "resource_type": rtype.as_str(),
            "resource_pattern": resource_pattern,
            "action": act.as_str(),
            "revoked": true,
        }))
    }

    pub(super) fn rbac_actor_bind(&self, p: Value) -> Result<Value, RpcErr> {
        let ActorBind { actor_id, role } = parse(p)?;
        if actor_id.trim().is_empty() {
            return Err(RpcErr::invalid("actor_id must not be empty"));
        }
        let exists: i64 = self
            .db
            .lock()
            .unwrap()
            .query_row(
                "SELECT COUNT(*) FROM rbac_roles WHERE name=?1",
                params![role],
                |r| r.get(0),
            )
            .map_err(sql)?;
        if exists == 0 {
            return Err(RpcErr::not_found(format!("unknown role `{role}`")));
        }
        let by = super::audit::current_actor();
        self.db
            .lock()
            .unwrap()
            .execute(
                "INSERT INTO rbac_actor_roles (actor_id, role_name, granted_by, granted_at, revoked_at)
                 VALUES (?1, ?2, ?3, ?4, NULL)
                 ON CONFLICT(actor_id, role_name) DO UPDATE SET revoked_at=NULL, granted_by=?3, granted_at=?4",
                params![actor_id, role, by, now_ms()],
            )
            .map_err(sql)?;
        self.record_audit(
            "rbac.actor_bind",
            &actor_id,
            "ok",
            json!({"role": role}),
        );
        Ok(json!({"actor_id": actor_id, "role": role}))
    }

    pub(super) fn rbac_actor_unbind(&self, p: Value) -> Result<Value, RpcErr> {
        let ActorBind { actor_id, role } = parse(p)?;
        let n = self
            .db
            .lock()
            .unwrap()
            .execute(
                "UPDATE rbac_actor_roles SET revoked_at=?3
                 WHERE actor_id=?1 AND role_name=?2 AND revoked_at IS NULL",
                params![actor_id, role, now_ms()],
            )
            .map_err(sql)?;
        if n == 0 {
            return Err(RpcErr::not_found(format!(
                "no active binding for actor `{actor_id}` and role `{role}`"
            )));
        }
        self.record_audit(
            "rbac.actor_unbind",
            &actor_id,
            "ok",
            json!({"role": role, "revoked": true}),
        );
        Ok(json!({"actor_id": actor_id, "role": role, "revoked": true}))
    }

    pub(super) fn rbac_list(&self) -> Result<Value, RpcErr> {
        let db = self.db.lock().unwrap();
        let mut st = db
            .prepare("SELECT name, description, builtin, created FROM rbac_roles ORDER BY name")
            .map_err(sql)?;
        let roles: Vec<Value> = st
            .query_map([], |r| {
                Ok(json!({
                    "name": r.get::<_, String>(0)?,
                    "description": r.get::<_, String>(1)?,
                    "builtin": r.get::<_, i64>(2)? != 0,
                    "created": r.get::<_, i64>(3)?,
                }))
            })
            .map_err(sql)?
            .collect::<Result<_, _>>()
            .map_err(sql)?;

        let mut pst = db
            .prepare(
                "SELECT role_name, resource_type, resource_pattern, action
                 FROM rbac_role_permissions
                 ORDER BY role_name, resource_type, resource_pattern, action",
            )
            .map_err(sql)?;
        let permissions: Vec<Value> = pst
            .query_map([], |r| {
                Ok(json!({
                    "role": r.get::<_, String>(0)?,
                    "resource_type": r.get::<_, String>(1)?,
                    "resource_pattern": r.get::<_, String>(2)?,
                    "action": r.get::<_, String>(3)?,
                }))
            })
            .map_err(sql)?
            .collect::<Result<_, _>>()
            .map_err(sql)?;

        let mut bst = db
            .prepare(
                "SELECT actor_id, role_name, granted_by, granted_at, revoked_at
                 FROM rbac_actor_roles
                 ORDER BY actor_id, role_name",
            )
            .map_err(sql)?;
        let bindings: Vec<Value> = bst
            .query_map([], |r| {
                Ok(json!({
                    "actor_id": r.get::<_, String>(0)?,
                    "role": r.get::<_, String>(1)?,
                    "granted_by": r.get::<_, String>(2)?,
                    "granted_at": r.get::<_, i64>(3)?,
                    "revoked_at": r.get::<_, Option<i64>>(4)?,
                }))
            })
            .map_err(sql)?
            .collect::<Result<_, _>>()
            .map_err(sql)?;

        Ok(json!({
            "roles": roles,
            "permissions": permissions,
            "bindings": bindings,
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::outcome::Outcome;

    fn test_paths(label: &str) -> Paths {
        let dir = std::env::temp_dir().join(format!("summonr-rbac-guard-{label}-{}", new_id("t")));
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
    fn pattern_matching_handles_wildcards_and_exact_names() {
        assert!(pattern_matches("*", "mail.send"));
        assert!(pattern_matches("mail.*", "mail.send"));
        assert!(pattern_matches("mail.*", "mail.locate"));
        assert!(!pattern_matches("mail.*", "mailbox.send"));
        assert!(pattern_matches("mail.locate", "mail.locate"));
        assert!(!pattern_matches("mail.locate", "mail.send"));
    }

    #[test]
    fn custom_role_and_reagent_guard_deny_unauthorized_calls_and_audit() {
        let p = test_paths("reagent-guard");
        let dir = p.state.parent().unwrap().to_path_buf();
        let mail_root = dir.join("mail");
        std::fs::create_dir_all(mail_root.join("Inbox")).unwrap();
        std::fs::write(
            mail_root.join("Inbox/m1.txt"),
            "From: a@example.com\nSubject: Hi\n\nhello",
        )
        .unwrap();
        std::fs::write(
            p.config.join("config.yaml"),
            format!("mail_root: {}\n", mail_root.display()),
        )
        .unwrap();

        let core = Core::open(p).unwrap();

        // Create a narrow custom role that can ONLY call `mail.locate` (read), not `mail.move` (write).
        core.rbac_role_create(json!({
            "name": "mail-locator",
            "description": "Can only locate messages"
        }))
        .unwrap();
        core.rbac_permission_grant(json!({
            "role": "mail-locator",
            "resource_type": "reagent",
            "resource_pattern": "mail.locate",
            "action": "read"
        }))
        .unwrap();
        core.rbac_actor_bind(json!({
            "actor_id": "operator:limited",
            "role": "mail-locator"
        }))
        .unwrap();

        // Policy decision checks:
        assert!(matches!(
            core.authorize_reagent(
                Some(&core.actor_context_for("operator:limited")),
                "mail.locate",
                PermissionAction::Read
            ),
            RbacDecision::Allow { .. }
        ));
        assert!(matches!(
            core.authorize_reagent(
                Some(&core.actor_context_for("operator:limited")),
                "mail.move",
                PermissionAction::Write
            ),
            RbacDecision::Deny { .. }
        ));

        // Now execute a ritual with a standing grant for BOTH mail.locate and mail.move,
        // under `operator:limited`. The exec.rs RBAC guard must block `mail.move` BEFORE
        // touching the mailbox and log `rbac.reagent_denied` to `audit_log`.
        let def: Ritual = serde_json::from_value(json!({
            "ritual": "move-mail",
            "essence": {"requests": 0, "time": "1m"},
            "requires": [
                {"reagent": "mail", "action": "locate"},
                {"reagent": "mail", "action": "move", "into": {"folders": ["Archive"]}}
            ],
            "steps": [
                {"id": "loc", "operation": "mail.locate", "arguments": {"message": "m1"}},
                {"id": "mv", "operation": "mail.move", "arguments": {"message": "m1", "to": "Archive"}}
            ],
            "return": {"ok": true}
        }))
        .unwrap();
        let grants: Vec<Grant> = def
            .requires
            .iter()
            .flat_map(ritual::Requirement::atoms)
            .collect();
        let id = new_id("s");
        core.insert_summon(
            &id,
            None,
            &def.ritual,
            1,
            &ritual::content_hash(&def),
            None,
            &json!({}),
            &def.essence,
            None,
        );
        let mut run = Run {
            job_claim: None,
            core: core.clone(),
            id: id.clone(),
            root_id: id.clone(),
            fence_prefix: String::new(),
            def,
            inputs: json!({}),
            model_override: None,
            settings_override: BTreeMap::new(),
            meters: vec![Arc::new(Meter::new(&Essence {
                requests: Some(0),
                tool_calls: None,
                items: None,
                tokens: None,
                cost: None,
                time: Some("1m".into()),
                cleanup_time: None,
            }))],
            selected: Arc::new(Mutex::new(HashSet::from(["m1".to_string()]))),
            cancel: Arc::new(AtomicBool::new(false)),
            grants: Arc::new(grants),
            depth: 0,
            io: Arc::new(crate::io::Live),
            models_used: Arc::new(Mutex::new(BTreeSet::new())),
            root_ritual: "move-mail".into(),
            actor: Some(core.actor_context_for("operator:limited")),
            done: 0,
            total: 0,
            finally_report: None,
        };

        let fin = run.execute();
        assert_eq!(fin.outcome, Outcome::Failed, "expected Failed, got {:?}", fin.error);
        assert!(
            fin.error.as_deref().unwrap_or("").contains("not permitted to invoke `mail.move`"),
            "unexpected error: {:?}",
            fin.error
        );
        // Side-effect check: m1.txt must still be in Inbox, never moved to Archive.
        assert!(mail_root.join("Inbox/m1.txt").exists());
        assert!(!mail_root.join("Archive/m1.txt").exists());

        // Audit log check: tamper-evident `rbac.reagent_denied` entry must exist and verify.
        let verified = core.audit_verify().unwrap();
        assert_eq!(verified["ok"], true);
        let entries = core.audit_list(json!({"limit": 100})).unwrap();
        assert!(
            entries.as_array().unwrap().iter().any(|e| {
                e["action"] == "rbac.reagent_denied"
                    && e["actor"] == "operator:limited"
                    && e["target"] == "mail.move"
                    && e["outcome"] == "denied"
            }),
            "expected rbac.reagent_denied in audit_log: {entries}"
        );

        // Live revocation: unbinding `mail-locator` immediately blocks even `mail.locate`.
        core.rbac_actor_unbind(json!({
            "actor_id": "operator:limited",
            "role": "mail-locator"
        }))
        .unwrap();
        assert!(matches!(
            core.authorize_reagent(
                Some(&core.actor_context_for("operator:limited")),
                "mail.locate",
                PermissionAction::Read
            ),
            RbacDecision::Deny { .. }
        ));

        drop(core);
        let _ = std::fs::remove_dir_all(dir);
    }
}
