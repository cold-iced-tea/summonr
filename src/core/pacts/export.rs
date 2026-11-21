use super::*;

// ------------------------------------------------------- revise, end, verify

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Export {
    pact: String,
    file: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct VerifyFile {
    file: String,
    #[serde(default)]
    key: Option<String>,
}

impl Core {
    /// A revision must replace an agreed pact between the same two cores.
    pub(super) fn check_revision(&self, t: &Terms) -> Result<(), RpcErr> {
        let Some(old) = &t.revises else { return Ok(()) };
        let bad = |why: &str| RpcErr::invalid(format!("revises {old}: {why}"));
        let row = self.pact_row(old).map_err(|_| bad("no such pact here"))?;
        if row.state != "agreed" {
            return Err(bad(&format!("that pact is {}, not agreed", row.state)));
        }
        let prior = terms_of(&self.db.lock().unwrap(), &row.id)?;
        if key_id_of(&prior.caller) != key_id_of(&t.caller) || key_id_of(&prior.callee) != key_id_of(&t.callee) {
            return Err(bad("a revision must be between the same caller and callee"));
        }
        Ok(())
    }

    /// Sign an end entry on `row`; open calls still finish.
    pub(super) fn end_pact(self: &Arc<Self>, row: &Row, data: Value) -> Result<(), RpcErr> {
        let to = if data["superseded_by"].is_string() { "superseded" } else { "ended" };
        if row.side == "callee" {
            let s = {
                let mut db = self.db.lock().unwrap();
                let tx = db.transaction().map_err(sql)?;
                let s = append_own(&tx, &self.key, &row.id, Kind::End, None, data)?;
                tx.commit().map_err(sql)?;
                s
            };
            self.push_to_peers_async(&row.id, s);
        } else {
            match self.caller_append(row, Kind::End, None, data) {
                Ok(_) => {}
                Err(e) if e.exit == 8 => {}
                Err(e) => return Err(e),
            }
        }
        self.db.lock().unwrap().execute("UPDATE pacts SET state=?2 WHERE id=?1 AND state IN ('offered','agreed')", params![row.id, to]).map_err(sql)?;
        Ok(())
    }

    pub(in crate::core) fn pact_end(self: &Arc<Self>, p: Value) -> Result<Value, RpcErr> {
        let Which { pact, reason } = parse(p)?;
        let row = self.pact_row(&pact)?;
        if !matches!(row.state.as_str(), "agreed" | "offered") {
            return Err(RpcErr::conflict(format!("pact `{}` is already {}", row.name, row.state)));
        }
        self.end_pact(&row, json!({"reason": reason}))?;
        Ok(json!({"pact": row.name, "id": row.id, "state": "ended"}))
    }

    /// The thread entry by entry, each re-verified, with any forks.
    pub(in crate::core) fn pact_history(&self, p: Value) -> Result<Value, RpcErr> {
        let Which { pact, .. } = parse(p)?;
        let row = self.pact_row(&pact)?;
        let db = self.db.lock().unwrap();
        let t = terms_of(&db, &row.id)?;
        let other = self.other_key(&t)?;
        let me = self.key.public();
        let entries: Vec<Value> = thread_of(&db, &row.id)?
            .into_iter()
            .map(|(e, s)| {
                let key = if e.by == me.id() { &me } else { &other };
                let verified = pact::open_entry(&s, key).is_ok();
                json!({"n": e.n, "kind": e.kind, "call": e.call, "by": if e.by == me.id() { "this core" } else { row.peer.as_str() }, "at": e.at, "data": e.data, "hash": e.hash, "verified": verified})
            })
            .collect();
        let forks = Self::forks_of(&db, &row.id)?;
        Ok(json!({"pact": row.name, "id": row.id, "state": row.state, "entries": entries, "forks": forks}))
    }

    pub(super) fn forks_of(db: &rusqlite::Connection, id: &str) -> Result<Vec<Value>, RpcErr> {
        let mut q = db.prepare("SELECT a, b, seen FROM pact_forks WHERE pact=?1").map_err(sql)?;
        let rows = q
            .query_map(params![id], |r| {
                Ok(json!({"a": serde_json::from_str::<Value>(&r.get::<_, String>(0)?).unwrap_or_default(), "b": serde_json::from_str::<Value>(&r.get::<_, String>(1)?).unwrap_or_default(), "seen": r.get::<_, i64>(2)?}))
            })
            .map_err(sql)?;
        rows.collect::<rusqlite::Result<Vec<_>>>().map_err(sql)
    }

    /// Write the thread for offline checking by anyone holding both keys.
    pub(in crate::core) fn pact_export(&self, p: Value) -> Result<Value, RpcErr> {
        let Export { pact, file } = parse(p)?;
        let row = self.pact_row(&pact)?;
        let export = {
            let db = self.db.lock().unwrap();
            let t = terms_of(&db, &row.id)?;
            let mut entries: Vec<Signed> = thread_of(&db, &row.id)?.into_iter().map(|(_, s)| s).collect();
            // A fork's second entry is evidence; it travels with the thread.
            for f in Self::forks_of(&db, &row.id)? {
                if let Ok(s) = serde_json::from_value::<Signed>(f["b"].clone()) {
                    entries.push(s);
                }
            }
            json!({"v": 1, "kind": "pact-thread", "terms": t, "caller_key": t.caller, "callee_key": t.callee, "entries": entries})
        };
        std::fs::write(&file, serde_json::to_string_pretty(&export).unwrap()).map_err(|e| RpcErr::invalid(format!("cannot write {file}: {e}")))?;
        Ok(json!({"pact": row.name, "file": file, "entries": export["entries"].as_array().map(|e| e.len())}))
    }

    /// Check an exported thread with keys this core trusts: its own, pinned
    /// peers', or one given. Keys written in the file are never used alone.
    pub(in crate::core) fn pact_verify(&self, p: Value) -> Result<Value, RpcErr> {
        let VerifyFile { file, key } = parse(p)?;
        let export: Value = std::fs::read_to_string(&file)
            .map_err(|e| e.to_string())
            .and_then(|t| serde_json::from_str(&t).map_err(|e| e.to_string()))
            .map_err(|e| RpcErr::invalid(format!("{file}: {e}")))?;
        if export["kind"] != "pact-thread" {
            return Err(RpcErr::invalid(format!("{file} is not a pact thread export")));
        }
        let given = key.as_deref().map(crate::keys::PublicKey::parse).transpose().map_err(RpcErr::invalid)?;
        let trusted = |text: &str| -> Result<crate::keys::PublicKey, RpcErr> {
            let id = key_id_of(text);
            if id == self.key.public().id() {
                return Ok(self.key.public());
            }
            if let Some(k) = given.as_ref().filter(|k| k.id() == id) {
                return Ok(*k);
            }
            let pinned: Option<String> = self.db.lock().unwrap().query_row("SELECT key FROM peers WHERE key_id=?1", params![id], |r| r.get(0)).optional().map_err(sql)?;
            let pinned = pinned.ok_or_else(|| RpcErr::not_found(format!("the core {} is not pinned here; pin it with `peer add` after comparing fingerprints, or give its key", &id[..16.min(id.len())])))?;
            crate::keys::PublicKey::parse(&pinned).map_err(RpcErr::invalid)
        };
        let caller = trusted(export["terms"]["caller"].as_str().unwrap_or(""))?;
        let callee = trusted(export["terms"]["callee"].as_str().unwrap_or(""))?;
        match pact::verify_thread(&export, &caller, &callee) {
            Ok(r) => Ok(json!({
                "ok": r.forks.is_empty(), "entries": r.entries, "calls": r.calls, "epitaphs_checked": r.epitaphs_checked,
                "forks": r.forks.iter().map(|(a, b)| json!({"a": a, "b": b})).collect::<Vec<_>>(),
                "problem": if r.forks.is_empty() { Value::Null } else { json!("a core signed two different entries on the same previous entry: signed evidence that it equivocated") },
            })),
            Err((n, why)) => Ok(json!({"ok": false, "problem": {"entry": n, "reason": why}, "forks": []})),
        }
    }
}
