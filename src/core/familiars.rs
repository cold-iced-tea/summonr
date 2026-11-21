//! familiars service: owner-managed identities (agents and other devices'
//! agents) and pinned peer cores. Private keys only ever live in key files.
use super::*;
use crate::keys::{self, PublicKey};

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Named {
    name: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct AddKey {
    name: String,
    key: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RegisterGrant {
    grant: keys::FamiliarGrant,
    #[serde(default)]
    root_key: Option<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Allow {
    name: String,
    #[serde(default)]
    rituals: Vec<String>,
    /// Pacts the familiar may call under, stored as `pact:NAME`.
    #[serde(default)]
    pacts: Vec<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct AddPeer {
    name: String,
    url: String,
    key: String,
}

fn parse<T: serde::de::DeserializeOwned>(p: Value) -> Result<T, RpcErr> {
    serde_json::from_value(p).map_err(|e| RpcErr::invalid(e.to_string()))
}

/// Names become file names, so they are kept to a small, safe alphabet.
pub(crate) fn valid_name(name: &str) -> Result<(), RpcErr> {
    let ok = !name.is_empty()
        && name.len() <= 32
        && name.bytes().next().is_some_and(|b| b.is_ascii_lowercase() || b.is_ascii_digit())
        && name.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-');
    if ok {
        Ok(())
    } else {
        Err(RpcErr::invalid(format!(
            "`{name}` is not a valid name: use 1-32 lowercase letters, digits or hyphens, starting with a letter or digit"
        )))
    }
}

fn sql(e: rusqlite::Error) -> RpcErr {
    RpcErr::new("storage_failed", 11, e.to_string())
}

impl Core {
    fn familiar_exists(&self, name: &str, key_id: &str) -> Result<bool, RpcErr> {
        self.db
            .lock()
            .unwrap()
            .query_row(
                "SELECT COUNT(*) FROM familiars WHERE name=?1 OR key_id=?2",
                params![name, key_id],
                |r| r.get::<_, i64>(0),
            )
            .map(|n| n > 0)
            .map_err(sql)
    }

    fn insert_familiar(&self, name: &str, kind: &str, key: &PublicKey) -> Result<(), RpcErr> {
        // One guard for the check and the insert, so a concurrent `witness
        // accept` cannot put the same key in both tables.
        let db = self.db.lock().unwrap();
        let ward: i64 = db
            .query_row("SELECT COUNT(*) FROM wards WHERE key_id=?1", params![key.id()], |r| r.get(0))
            .map_err(sql)?;
        if ward > 0 {
            return Err(RpcErr::conflict("that key is a ward here; one key cannot be both a ward and a familiar"));
        }
        let counterparty: i64 = db
            .query_row("SELECT COUNT(*) FROM counterparties WHERE key_id=?1", params![key.id()], |r| r.get(0))
            .map_err(sql)?;
        if counterparty > 0 {
            return Err(RpcErr::conflict("that key is a pact counterparty here; one key cannot be both a counterparty and a familiar"));
        }
        db.execute(
            "INSERT INTO familiars (name, kind, key, key_id, created) VALUES (?1, ?2, ?3, ?4, ?5)",
            params![name, kind, key.text(), key.id(), now_ms()],
        )
        .map(|_| ())
        .map_err(sql)
    }

    pub(super) fn familiar_new(&self, p: Value) -> Result<Value, RpcErr> {
        let Named { name } = parse(p)?;
        valid_name(&name)?;
        let dir = self.paths.config.join("familiars");
        let file = dir.join(format!("{name}.key"));
        if self.familiar_exists(&name, "")? {
            return Err(RpcErr::conflict(format!("a familiar named `{name}` already exists; names are never reused")));
        }
        if file.exists() {
            return Err(RpcErr::conflict(format!(
                "{} exists but is not registered as a familiar; move it away (or delete it if you are sure it is unused), then try again",
                file.display()
            )));
        }
        // Write the key beside its final name, register it, then link it into
        // place; a failure at any step leaves neither a stray key nor a
        // registered familiar without one.
        let k = keys::generate();
        let tmp = dir.join(format!(".{name}.key.{}", new_id("tmp")));
        keys::save_new(&tmp, &k).map_err(|e| RpcErr::new("storage_failed", 11, e))?;
        if let Err(e) = self.insert_familiar(&name, "local", &k.public()) {
            let _ = std::fs::remove_file(&tmp);
            return Err(e);
        }
        let placed = std::fs::hard_link(&tmp, &file);
        let _ = std::fs::remove_file(&tmp);
        if let Err(e) = placed {
            let _ = self.db.lock().unwrap().execute("DELETE FROM familiars WHERE name=?1", params![name]);
            return Err(RpcErr::new("storage_failed", 11, format!("cannot place key file {}: {e}", file.display())));
        }
        let public = k.public();
        Ok(json!({"name": name, "key": public.text(), "id": public.id(), "key_file": file}))
    }

    pub(super) fn familiar_add(&self, p: Value) -> Result<Value, RpcErr> {
        let AddKey { name, key } = parse(p)?;
        valid_name(&name)?;
        let key = PublicKey::parse(&key).map_err(RpcErr::invalid)?;
        if self.familiar_exists(&name, &key.id())? {
            return Err(RpcErr::conflict(format!(
                "a familiar named `{name}` or with this key already exists; names and keys are never reused"
            )));
        }
        self.insert_familiar(&name, "remote", &key)?;
        Ok(json!({"name": name, "id": key.id()}))
    }

    /// Register a remote familiar from an offline-root-signed [`keys::FamiliarGrant`].
    pub(crate) fn familiar_register_grant(&self, params: Value) -> Result<Value, RpcErr> {
        let RegisterGrant { grant, root_key } = parse(params)?;
        let pinned_path = self.paths.config.join("root.pub");
        let pinned_pub = if pinned_path.exists() {
            let text = std::fs::read_to_string(&pinned_path)
                .map_err(|e| RpcErr::invalid(format!("cannot read root.pub: {e}")))?;
            Some(PublicKey::parse(&text).map_err(RpcErr::invalid)?)
        } else {
            None
        };
        let root_pub = match (pinned_pub, root_key) {
            (Some(pinned), Some(supplied)) => {
                let supplied_pub = PublicKey::parse(&supplied).map_err(RpcErr::invalid)?;
                if supplied_pub.id() != pinned.id() {
                    return Err(RpcErr::new(
                        "not_authenticated",
                        4,
                        "supplied root_key does not match pinned config/root.pub",
                    ));
                }
                pinned
            }
            (Some(pinned), None) => pinned,
            (None, _) => {
                return Err(RpcErr::new(
                    "not_authenticated",
                    4,
                    "no pinned config/root.pub; pin the offline root public key before registering root-signed grants",
                ));
            }
        };
        let familiar_pub = grant
            .verify(&root_pub, now_ms())
            .map_err(|e| RpcErr::new("not_authenticated", 4, e))?;
        valid_name(&grant.name)?;
        if self.familiar_exists(&grant.name, &familiar_pub.id())? {
            return Err(RpcErr::conflict(format!(
                "a familiar named `{}` or with this key already exists; names and keys are never reused",
                grant.name
            )));
        }
        let mut allowed: Vec<String> = Vec::new();
        for pact in &grant.pacts {
            let row = self.pact_row(pact).map_err(|_| {
                RpcErr::not_found(format!("no pact `{pact}`; nothing was registered"))
            })?;
            if row.side != "caller" {
                return Err(RpcErr::invalid(format!(
                    "this core is the callee of `{pact}`; familiars call only under pacts where this core is the caller"
                )));
            }
            let entry = format!("pact:{}", row.name);
            if !allowed.contains(&entry) {
                allowed.push(entry);
            }
        }
        for r in &grant.rituals {
            if self.load_ritual(r, None)?.is_none() {
                return Err(RpcErr::not_found(format!(
                    "no ritual `{r}` in the grimoire; nothing was registered"
                )));
            }
            if !allowed.contains(r) {
                allowed.push(r.clone());
            }
        }
        allowed.sort();
        {
            let mut db = self.db.lock().unwrap();
            let tx = db.transaction().map_err(sql)?;
            let exists: i64 = tx
                .query_row(
                    "SELECT COUNT(*) FROM familiars WHERE name=?1 OR key_id=?2",
                    params![grant.name, familiar_pub.id()],
                    |r| r.get(0),
                )
                .map_err(sql)?;
            if exists > 0 {
                return Err(RpcErr::conflict(format!(
                    "a familiar named `{}` or with this key already exists; names and keys are never reused",
                    grant.name
                )));
            }
            let in_other: i64 = tx
                .query_row(
                    "SELECT (SELECT COUNT(*) FROM wards WHERE key_id=?1) + (SELECT COUNT(*) FROM counterparties WHERE key_id=?1)",
                    params![familiar_pub.id()],
                    |r| r.get(0),
                )
                .map_err(sql)?;
            if in_other > 0 {
                return Err(RpcErr::conflict(
                    "that key already belongs to a ward or counterparty",
                ));
            }
            let allowed_json = serde_json::to_string(&allowed).map_err(|e| RpcErr::invalid(e.to_string()))?;
            tx.execute(
                "INSERT INTO familiars(name, kind, key, key_id, created, allowed, expires_at) VALUES(?1, 'remote', ?2, ?3, ?4, ?5, ?6)",
                params![grant.name, familiar_pub.text(), familiar_pub.id(), now_ms(), allowed_json, grant.expires_at_ms],
            )
            .map_err(sql)?;
            tx.commit().map_err(sql)?;
        }
        Ok(json!({
            "name": grant.name,
            "kind": "remote",
            "key": familiar_pub.text(),
            "key_id": familiar_pub.id(),
            "root_key_id": grant.root_key_id,
            "allowed": allowed,
        }))
    }

    pub(super) fn familiar_list(&self) -> Result<Value, RpcErr> {
        let db = self.db.lock().unwrap();
        let mut q = db
            .prepare("SELECT name, kind, key, key_id, created, revoked, allowed FROM familiars ORDER BY created")
            .map_err(sql)?;
        let rows = q
            .query_map([], |r| {
                Ok(json!({
                    "name": r.get::<_, String>(0)?, "kind": r.get::<_, String>(1)?,
                    "key": r.get::<_, String>(2)?, "id": r.get::<_, String>(3)?,
                    "created": r.get::<_, i64>(4)?, "revoked": r.get::<_, Option<i64>>(5)?,
                    "allowed": serde_json::from_str::<Value>(&r.get::<_, String>(6)?).unwrap_or(json!([])),
                }))
            })
            .map_err(sql)?;
        rows.collect::<Result<Vec<_>, _>>().map(Value::Array).map_err(sql)
    }

    fn allowed_of(&self, name: &str) -> Result<Vec<String>, RpcErr> {
        let raw: Option<String> = self
            .db
            .lock()
            .unwrap()
            .query_row("SELECT allowed FROM familiars WHERE name=?1 AND revoked IS NULL", params![name], |r| r.get(0))
            .optional()
            .map_err(sql)?;
        let raw = raw.ok_or_else(|| RpcErr::not_found(format!("no active familiar `{name}`")))?;
        Ok(serde_json::from_str(&raw).unwrap_or_default())
    }

    fn set_allowed(&self, name: &str, allowed: &[String]) -> Result<(), RpcErr> {
        self.db
            .lock()
            .unwrap()
            .execute(
                "UPDATE familiars SET allowed=?2 WHERE name=?1",
                params![name, serde_json::to_string(allowed).unwrap()],
            )
            .map(|_| ())
            .map_err(sql)
    }

    pub(super) fn familiar_allow(&self, p: Value, allow: bool) -> Result<Value, RpcErr> {
        let Allow { name, rituals, pacts } = parse(p)?;
        let mut allowed = self.allowed_of(&name)?;
        for pact in &pacts {
            let entry = format!("pact:{pact}");
            if allow {
                let row = self.pact_row(pact).map_err(|_| RpcErr::not_found(format!("no pact `{pact}`; nothing was allowed")))?;
                if row.side != "caller" {
                    return Err(RpcErr::invalid(format!("this core is the callee of `{pact}`; familiars call only under pacts where this core is the caller")));
                }
                let entry = format!("pact:{}", row.name);
                if !allowed.contains(&entry) {
                    allowed.push(entry);
                }
            } else {
                let resolved = self.pact_row(pact).ok().map(|r| format!("pact:{}", r.name));
                allowed.retain(|a| a != &entry && Some(a) != resolved.as_ref());
            }
        }
        for r in &rituals {
            if allow {
                if self.load_ritual(r, None)?.is_none() {
                    return Err(RpcErr::not_found(format!("no ritual `{r}` in the grimoire; nothing was allowed")));
                }
                if !allowed.contains(r) {
                    allowed.push(r.clone());
                }
            } else {
                allowed.retain(|a| a != r);
            }
        }
        allowed.sort();
        self.set_allowed(&name, &allowed)?;
        Ok(json!({"name": name, "allowed": allowed}))
    }

    pub(super) fn familiar_revoke(&self, p: Value) -> Result<Value, RpcErr> {
        let Named { name } = parse(p)?;
        let n = self
            .db
            .lock()
            .unwrap()
            .execute("UPDATE familiars SET revoked=?2 WHERE name=?1 AND revoked IS NULL", params![name, now_ms()])
            .map_err(sql)?;
        if n == 0 {
            return Err(RpcErr::not_found(format!("no active familiar `{name}`")));
        }
        Ok(json!({"name": name, "revoked": true}))
    }

    /// An active familiar by key id: its name, key and allowed rituals.
    pub(crate) fn familiar_by_key(&self, key_id: &str) -> Option<(String, PublicKey, Vec<String>)> {
        let now = now_ms();
        let row: Option<(String, String, String)> = self
            .db
            .lock()
            .unwrap()
            .query_row(
                "SELECT name, key, allowed FROM familiars WHERE key_id=?1 AND revoked IS NULL AND (expires_at IS NULL OR expires_at >= ?2)",
                params![key_id, now],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .optional()
            .ok()
            .flatten();
        let (name, key, allowed) = row?;
        let key = PublicKey::parse(&key).ok()?;
        Some((name, key, serde_json::from_str(&allowed).unwrap_or_default()))
    }

    pub(super) fn peer_add(&self, p: Value) -> Result<Value, RpcErr> {
        let AddPeer { name, url, key } = parse(p)?;
        valid_name(&name)?;
        if !(url.starts_with("http://") || url.starts_with("https://")) || url::Url::parse(&url).is_err() {
            return Err(RpcErr::invalid(format!("`{url}` is not an http:// or https:// URL")));
        }
        let url = url.trim_end_matches('/').to_string();
        let key = PublicKey::parse(&key).map_err(RpcErr::invalid)?;
        let n = self
            .db
            .lock()
            .unwrap()
            .execute(
                "INSERT OR IGNORE INTO peers (name, url, key, key_id, created) VALUES (?1, ?2, ?3, ?4, ?5)",
                params![name, url, key.text(), key.id(), now_ms()],
            )
            .map_err(sql)?;
        if n == 0 {
            return Err(RpcErr::conflict(format!("a peer named `{name}` already exists; remove it first")));
        }
        Ok(json!({"name": name, "url": url, "id": key.id()}))
    }

    pub(super) fn peer_list(&self) -> Result<Value, RpcErr> {
        let db = self.db.lock().unwrap();
        let mut q = db.prepare("SELECT name, url, key, key_id, created FROM peers ORDER BY name").map_err(sql)?;
        let rows = q
            .query_map([], |r| {
                Ok(json!({"name": r.get::<_, String>(0)?, "url": r.get::<_, String>(1)?, "key": r.get::<_, String>(2)?,
                          "id": r.get::<_, String>(3)?, "created": r.get::<_, i64>(4)?}))
            })
            .map_err(sql)?;
        rows.collect::<Result<Vec<_>, _>>().map(Value::Array).map_err(sql)
    }

    pub(super) fn peer_remove(&self, p: Value) -> Result<Value, RpcErr> {
        let Named { name } = parse(p)?;
        let n = self.db.lock().unwrap().execute("DELETE FROM peers WHERE name=?1", params![name]).map_err(sql)?;
        if n == 0 {
            return Err(RpcErr::not_found(format!("no peer `{name}`")));
        }
        Ok(json!({"name": name, "removed": true}))
    }
}
