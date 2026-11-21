use super::*;

// ---------------------------------------------------------------- agreement

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Offer {
    peer: String,
    file: String,
    #[serde(default)]
    name: Option<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Peer {
    peer: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Which {
    pub(super) pact: String,
    #[serde(default)]
    pub(super) reason: Option<String>,
}

/// A pact row.
#[derive(Clone, Debug)]
pub(in crate::core) struct Row {
    pub id: String,
    pub name: String,
    pub peer: String,
    pub side: String,
    pub state: String,
}

/// Find a step by id anywhere in `steps`.
fn find_step<'a>(steps: &'a mut [crate::ritual::Step], id: &str) -> Option<&'a mut crate::ritual::Step> {
    for s in steps.iter_mut() {
        if s.id.as_deref() == Some(id) {
            return Some(s);
        }
        for inner in [s.steps.as_mut(), s.then.as_mut(), s.otherwise.as_mut()].into_iter().flatten() {
            if let Some(found) = find_step(inner, id) {
                return Some(found);
            }
        }
    }
    None
}

impl Core {
    pub(in crate::core) fn pact_row(&self, which: &str) -> Result<Row, RpcErr> {
        self.db
            .lock()
            .unwrap()
            .query_row(
                "SELECT id, name, peer, side, state FROM pacts WHERE name=?1 OR id=?1",
                params![which],
                |r| Ok(Row { id: r.get(0)?, name: r.get(1)?, peer: r.get(2)?, side: r.get(3)?, state: r.get(4)? }),
            )
            .optional()
            .map_err(sql)?
            .ok_or_else(|| RpcErr::not_found(format!("no pact `{which}`")))
    }

    pub(super) fn pinned_peer(&self, peer: &str) -> Result<crate::keys::PublicKey, RpcErr> {
        let key: String = self
            .db
            .lock()
            .unwrap()
            .query_row("SELECT key FROM peers WHERE name=?1", params![peer], |r| r.get(0))
            .optional()
            .map_err(sql)?
            .ok_or_else(|| RpcErr::not_found(format!("no pinned peer `{peer}`; add it with `summonr peer add {peer} url URL key K`")))?;
        crate::keys::PublicKey::parse(&key).map_err(RpcErr::invalid)
    }

    pub(super) fn peer_named_by_key(&self, key: &crate::keys::PublicKey) -> Option<String> {
        self.db.lock().unwrap().query_row("SELECT name FROM peers WHERE key_id=?1", params![key.id()], |r| r.get(0)).optional().ok().flatten()
    }

    /// The other party's key, from the terms (never from the peer table).
    pub(in crate::core) fn other_key(&self, t: &Terms) -> Result<crate::keys::PublicKey, RpcErr> {
        let me = self.key.public().id();
        let other = if key_id_of(&t.caller) == me { &t.callee } else { &t.caller };
        crate::keys::PublicKey::parse(other).map_err(RpcErr::invalid)
    }

    /// Check the pinned ritual here, the callee: the exact hash exists, the
    /// caller holds sit on writes with boolean conditions, and the inputs are
    /// covered. Returns what both sides are shown.
    pub(in crate::core) fn validate_on_callee(&self, t: &Terms) -> Result<Value, RpcErr> {
        let row: Option<(u32, String)> = self
            .db
            .lock()
            .unwrap()
            .query_row("SELECT version, def FROM rituals WHERE name=?1 AND hash=?2", params![t.ritual.name, t.ritual.hash], |r| Ok((r.get(0)?, r.get(1)?)))
            .optional()
            .map_err(sql)?;
        let (version, def) = row.ok_or_else(|| {
            RpcErr::invalid(format!("no ritual `{}` with hash {} in this grimoire; the pact must pin a version saved here", t.ritual.name, t.ritual.hash))
        })?;
        let def: crate::ritual::Ritual = serde_json::from_str(&def).map_err(|e| RpcErr::new("storage_failed", 11, e.to_string()))?;
        let mut problems = Vec::new();
        for (name, decl) in &def.inputs {
            let given = t.fixed.contains_key(name) || t.inputs.contains(name);
            if decl.default.is_none() && !given {
                problems.push(format!("input `{name}` is required but neither fixed nor given by calls"));
            }
            if decl.user_only && given {
                problems.push(format!("input `{name}` belongs to the owner (user_only); a pact cannot set it"));
            }
        }
        for name in t.fixed.keys().chain(t.inputs.iter()) {
            if !def.inputs.contains_key(name) {
                problems.push(format!("input `{name}` is not declared by `{}`", t.ritual.name));
            }
        }
        let mut held = def.clone();
        for h in &t.held_by_caller {
            match find_step(&mut held.steps, &h.step) {
                None => problems.push(format!("held_by_caller: no step `{}` in `{}`", h.step, t.ritual.name)),
                Some(step) => step.hold = Some(h.when.clone()),
            }
        }
        if !problems.is_empty() {
            return Err(RpcErr::invalid(problems.join("; ")));
        }
        // The same checks a `hold` gets: a write, and a boolean reference.
        let effects = crate::ritual::validate(&held, &self.resolver(), &reagent::tool, self.cfg.max_depth())
            .map_err(|errors| RpcErr::invalid(format!("held_by_caller: {}", errors.join("; "))))?;
        let grants = self.grants_for(&t.ritual.name)?;
        let missing: Vec<String> = def
            .requires
            .iter()
            .flat_map(Requirement::atoms)
            .filter(|n| !grants.iter().any(|g| g.covers(n)))
            .map(|g| g.describe())
            .collect();
        Ok(json!({
            "ritual": t.ritual.name, "version": version, "hash": t.ritual.hash, "description": def.description,
            "inputs": def.inputs, "returns": def.ret.keys().collect::<Vec<_>>(), "output_schema": def.output_schema, "essence": def.essence,
            "effects": effects.iter().map(|e| json!({"step": e.step, "what": e.what, "effect": e.effect, "determinism": e.determinism, "notes": e.notes})).collect::<Vec<_>>(),
            "model_sites": effects.iter().filter(|e| e.kind == "model").map(|e| json!({"step": e.step, "model": e.what})).collect::<Vec<_>>(),
            "authority": if missing.is_empty() { json!("granted") } else { json!({"missing": missing}) },
        }))
    }

    /// The caller's acceptance ritual, compiled from the source in the terms.
    pub(in crate::core) fn validate_acceptance(&self, t: &Terms) -> Result<Value, RpcErr> {
        let Some(r) = &t.checks.ritual else { return Ok(Value::Null) };
        let def = crate::ritual::parse_file(&r.source).map_err(|e| RpcErr::invalid(format!("acceptance ritual: {e}")))?;
        let effects = crate::ritual::validate(&def, &self.resolver(), &reagent::tool, self.cfg.max_depth())
            .map_err(|errors| RpcErr::invalid(format!("acceptance ritual: {}", errors.join("; "))))?;
        if !def.requires.is_empty() || effects.iter().any(|e| e.effect == "write") {
            return Err(RpcErr::invalid("acceptance ritual must not require external grants or perform writes"));
        }
        let inputs: Vec<&String> = def.inputs.keys().collect();
        if inputs != vec!["delivery"] || def.inputs["delivery"].ty != "object" {
            return Err(RpcErr::invalid("acceptance ritual must declare exactly one input, `delivery`, of type object"));
        }
        if !def.ret.contains_key("accept") || !def.ret.contains_key("reason") {
            return Err(RpcErr::invalid("acceptance ritual must return `accept` and `reason`"));
        }
        Ok(json!({
            "ritual": r.name, "hash": r.hash, "source": r.source,
            "effects": effects.iter().map(|e| json!({"step": e.step, "what": e.what, "effect": e.effect})).collect::<Vec<_>>(),
        }))
    }

    pub(super) fn unique_name(&self, wanted: &str, id: &str) -> String {
        let taken = |n: &str| self.db.lock().unwrap().query_row("SELECT COUNT(*) FROM pacts WHERE name=?1", params![n], |r| r.get::<_, i64>(0)).unwrap_or(1) > 0;
        if taken(wanted) { format!("{wanted}-{}", &id[..8]) } else { wanted.to_string() }
    }

    pub(super) fn insert_pact(&self, id: &str, name: &str, peer: &str, side: &str, t: &Terms) -> Result<(), RpcErr> {
        self.db
            .lock()
            .unwrap()
            .execute(
                "INSERT INTO pacts (id, name, peer, side, terms, state, created) VALUES (?1, ?2, ?3, ?4, ?5, 'offered', ?6)",
                params![id, name, peer, side, pact::terms_bytes(t), now_ms()],
            )
            .map_err(|e| match e {
                rusqlite::Error::SqliteFailure(f, _) if f.code == rusqlite::ErrorCode::ConstraintViolation => RpcErr::conflict("this pact or its name already exists here"),
                e => sql(e),
            })?;
        Ok(())
    }

    pub(in crate::core) fn pact_offer(self: &Arc<Self>, p: Value) -> Result<Value, RpcErr> {
        let Offer { peer, file, name } = parse(p)?;
        let text = std::fs::read_to_string(&file).map_err(|e| RpcErr::invalid(format!("cannot read {file}: {e}")))?;
        let t = pact::parse_terms(&text).map_err(RpcErr::invalid)?;
        let errors = pact::check_shape(&t);
        if !errors.is_empty() {
            return Err(RpcErr::invalid(errors.join("; ")));
        }
        let me = self.key.public().id();
        let side = if key_id_of(&t.caller) == me {
            "caller"
        } else if key_id_of(&t.callee) == me {
            "callee"
        } else {
            return Err(RpcErr::invalid("these terms do not name this core as caller or callee"));
        };
        if self.pinned_peer(&peer)?.id() != self.other_key(&t)?.id() {
            return Err(RpcErr::invalid(format!("peer `{peer}` is not the other core these terms name")));
        }
        if side == "callee" {
            self.validate_on_callee(&t)?;
        }
        self.validate_acceptance(&t)?;
        self.check_revision(&t)?;
        let id = pact::pact_id(&t);
        let name = self.unique_name(name.as_deref().unwrap_or(&t.pact), &id);
        self.insert_pact(&id, &name, &peer, side, &t)?;
        let data = serde_json::to_value(&t).expect("terms serialise");
        let entry = if side == "callee" {
            let mut db = self.db.lock().unwrap();
            let tx = db.transaction().map_err(sql)?;
            let s = append_own(&tx, &self.key, &id, Kind::Offer, None, data)?;
            tx.commit().map_err(sql)?;
            s
        } else {
            let s = pact::seal_entry(&self.key, &id, 1, epitaph::ZEROS, Kind::Offer, None, now_ms(), data);
            if let Err(e) = self.pact_send(&peer, "pact_offer", json!({"entry": s})) {
                self.db.lock().unwrap().execute("DELETE FROM pacts WHERE id=?1", params![id]).map_err(sql)?;
                return Err(e);
            }
            self.confirm_own(&id, &s)?;
            s
        };
        self.push_offer_to_witnesses_async(&t, entry.clone());
        Ok(json!({"pact": name, "id": id, "side": side, "state": "offered", "entry": entry.hash}))
    }

    /// A caller (or callee) core offers terms to this core (as callee or witness).
    pub(super) fn offered_to_me(self: &Arc<Self>, key: &crate::keys::PublicKey, entry: &Signed) -> Result<Value, RpcErr> {
        let e = pact::open_entry(entry, key).map_err(|w| RpcErr::new("not_authorized", 4, format!("offer refused: {w}")))?;
        let t: Terms = serde_json::from_value(e.data.clone()).map_err(|err| RpcErr::invalid(format!("offer carries no valid terms: {err}")))?;
        let errors = pact::check_shape(&t);
        if !errors.is_empty() {
            return Err(RpcErr::invalid(errors.join("; ")));
        }
        let id = pact::pact_id(&t);
        if e.kind != Kind::Offer || e.n != 1 || e.pact != id {
            return Err(RpcErr::invalid("not an offer of these terms"));
        }
        let me = self.key.public().id();
        let is_witness = t.witnesses.iter().any(|w| key_id_of(w) == me);
        let side = if key_id_of(&t.caller) == key.id() && key_id_of(&t.callee) == me {
            "callee"
        } else if is_witness && (key_id_of(&t.caller) == key.id() || key_id_of(&t.callee) == key.id()) {
            "witness"
        } else {
            return Err(RpcErr::invalid("an offer must name its sender as caller/callee and this core as callee or witness"));
        };
        let peer = self.peer_named_by_key(key).ok_or_else(|| RpcErr::invalid("this core has not pinned the offering core as a peer"))?;
        if side == "callee" {
            self.validate_on_callee(&t)?;
        }
        self.validate_acceptance(&t)?;
        self.check_revision(&t)?;
        if self.pact_row(&id).is_err() {
            let name = self.unique_name(&t.pact, &id);
            self.insert_pact(&id, &name, &peer, side, &t)?;
        }
        self.receive(&id, entry, key)
    }

    /// Offers this core made to `key`'s core that are still open.
    pub(super) fn offers_for(&self, key: &crate::keys::PublicKey) -> Result<Value, RpcErr> {
        let db = self.db.lock().unwrap();
        let mut q = db.prepare("SELECT p.id, p.terms, e.bytes, e.sig, e.hash FROM pacts p JOIN pact_entries e ON e.pact=p.id AND e.n=1 WHERE p.side='callee' AND p.state='offered'").map_err(sql)?;
        let rows = q
            .query_map([], |r| Ok((r.get::<_, String>(1)?, Signed { bytes: r.get(2)?, sig: r.get(3)?, hash: r.get(4)? })))
            .map_err(sql)?
            .collect::<rusqlite::Result<Vec<_>>>()
            .map_err(sql)?;
        let mine = self.key.public().id();
        let offers: Vec<Signed> = rows
            .into_iter()
            .filter(|(terms, s)| {
                let t: Option<Terms> = serde_json::from_str(terms).ok();
                let by_me = serde_json::from_str::<Value>(&s.bytes).ok().is_some_and(|v| v["by"] == mine);
                by_me && t.is_some_and(|t| key_id_of(&t.caller) == key.id())
            })
            .map(|(_, s)| s)
            .collect();
        Ok(json!({"offers": offers}))
    }

    pub(in crate::core) fn pact_fetch(self: &Arc<Self>, p: Value) -> Result<Value, RpcErr> {
        let Peer { peer } = parse(p)?;
        let peer_key = self.pinned_peer(&peer)?;
        let got = self.pact_send(&peer, "pact_offers", json!({}))?;
        let offers: Vec<Signed> = serde_json::from_value(got["offers"].clone()).map_err(|e| RpcErr::invalid(e.to_string()))?;
        let mut fetched = 0;
        for s in offers {
            let e = pact::open_entry(&s, &peer_key).map_err(RpcErr::invalid)?;
            let Ok(t) = serde_json::from_value::<Terms>(e.data.clone()) else { continue };
            let id = pact::pact_id(&t);
            if e.pact != id || key_id_of(&t.caller) != self.key.public().id() || key_id_of(&t.callee) != peer_key.id() || !pact::check_shape(&t).is_empty() {
                continue;
            }
            if self.pact_row(&id).is_ok() {
                continue;
            }
            let name = self.unique_name(&t.pact, &id);
            self.insert_pact(&id, &name, &peer, "caller", &t)?;
            self.receive(&id, &s, &peer_key)?;
            fetched += 1;
        }
        Ok(json!({"fetched": fetched}))
    }

    pub(in crate::core) fn pact_agree(self: &Arc<Self>, p: Value, agree: bool) -> Result<Value, RpcErr> {
        let Which { pact, reason } = parse(p)?;
        let row = self.pact_row(&pact)?;
        if row.state != "offered" {
            return Err(RpcErr::conflict(format!("pact `{}` is {}, not offered", row.name, row.state)));
        }
        let offer_by: String = {
            let db = self.db.lock().unwrap();
            thread_of(&db, &row.id)?.first().map(|(e, _)| e.by.clone()).unwrap_or_default()
        };
        if offer_by == self.key.public().id() {
            return Err(RpcErr::new("not_authorized", 4, "this core made the offer; the other party must agree or decline it"));
        }
        let t = terms_of(&self.db.lock().unwrap(), &row.id)?;
        let (kind, data) = match (agree, row.side.as_str()) {
            (true, "callee") => {
                self.validate_acceptance(&t)?;
                (Kind::Agree, json!({"rehearsal": self.validate_on_callee(&t)?}))
            }
            (true, _) => {
                self.validate_acceptance(&t)?;
                // What the callee showed, kept on the thread for schema checks.
                let seen = self.pact_send(&row.peer, "pact_rehearse", json!({"pact": row.id}))?;
                (Kind::Agree, json!({"rehearsal": seen}))
            }
            (false, _) => (Kind::Decline, json!({"reason": reason})),
        };
        if row.side == "callee" {
            let s = {
                let mut db = self.db.lock().unwrap();
                let tx = db.transaction().map_err(sql)?;
                let s = append_own(&tx, &self.key, &row.id, kind, None, data)?;
                tx.commit().map_err(sql)?;
                s
            };
            self.push_to_peers_async(&row.id, s);
        } else {
            self.caller_append(&row, kind, None, data)?;
        }
        let state = if agree { "agreed" } else { "declined" };
        self.db.lock().unwrap().execute("UPDATE pacts SET state=?2 WHERE id=?1 AND state='offered'", params![row.id, state]).map_err(sql)?;
        if let (true, Some(old)) = (agree, &t.revises) {
            let old = self.pact_row(old)?;
            self.end_pact(&old, json!({"superseded_by": row.id, "reason": "superseded by a revised pact"}))?;
        }
        Ok(json!({"pact": row.name, "id": row.id, "state": state}))
    }

    pub(in crate::core) fn pact_list(&self) -> Result<Value, RpcErr> {
        let db = self.db.lock().unwrap();
        let mut q = db
            .prepare("SELECT p.name, p.id, p.peer, p.side, p.state, (SELECT COUNT(*) FROM pact_calls c WHERE c.pact=p.id AND c.state NOT IN ('accepted','rejected','closed')) FROM pacts p ORDER BY p.created, p.name")
            .map_err(sql)?;
        let rows = q
            .query_map([], |r| {
                Ok(json!({"name": r.get::<_, String>(0)?, "id": r.get::<_, String>(1)?, "peer": r.get::<_, String>(2)?,
                    "side": r.get::<_, String>(3)?, "state": r.get::<_, String>(4)?, "calls_open": r.get::<_, i64>(5)?}))
            })
            .map_err(sql)?
            .collect::<rusqlite::Result<Vec<_>>>()
            .map_err(sql)?;
        Ok(Value::Array(rows))
    }

    pub(in crate::core) fn pact_show(self: &Arc<Self>, p: Value) -> Result<Value, RpcErr> {
        let Which { pact, .. } = parse(p)?;
        let row = self.pact_row(&pact)?;
        let (t, thread, forks) = {
            let db = self.db.lock().unwrap();
            let forks: i64 = db.query_row("SELECT COUNT(*) FROM pact_forks WHERE pact=?1", params![row.id], |r| r.get(0)).map_err(sql)?;
            (terms_of(&db, &row.id)?, thread_of(&db, &row.id)?, forks)
        };
        let mut warnings = Vec::new();
        let problem: Option<String> = self.db.lock().unwrap().query_row("SELECT problem FROM pacts WHERE id=?1", params![row.id], |r| r.get(0)).map_err(sql)?;
        warnings.extend(problem);
        match self.pinned_peer(&row.peer) {
            Ok(k) if k.id() == self.other_key(&t)?.id() => {}
            Ok(_) => warnings.push(format!("peer `{}` is now pinned with a different key than this pact names; its entries will be refused", row.peer)),
            Err(e) => warnings.push(e.message),
        }
        let agreed = thread.iter().find(|(e, _)| e.kind == Kind::Agree && !e.data["rehearsal"].is_null()).map(|(e, _)| e.data["rehearsal"].clone());
        let ritual = if row.side == "callee" {
            self.validate_on_callee(&t).unwrap_or_else(|e| json!({"ritual": t.ritual.name, "error": e.message}))
        } else if let Some(r) = agreed {
            r
        } else {
            self.pact_send(&row.peer, "pact_rehearse", json!({"pact": row.id})).unwrap_or_else(|e| json!({"ritual": t.ritual.name, "error": e.message}))
        };
        let acceptance = self.validate_acceptance(&t).unwrap_or_else(|e| json!({"error": e.message}));
        let (calls, used_totals): (Vec<Value>, (u64, u64, u64, u64)) = {
            let db = self.db.lock().unwrap();
            let mut q = db.prepare("SELECT call, state, run, asked_by, deadline FROM pact_calls WHERE pact=?1 ORDER BY call").map_err(sql)?;
            let rows = q
                .query_map(params![row.id], |r| {
                    Ok(json!({"call": r.get::<_, i64>(0)?, "state": r.get::<_, String>(1)?, "run": r.get::<_, Option<String>>(2)?, "asked_by": r.get::<_, Option<String>>(3)?, "deadline": r.get::<_, i64>(4)?}))
                })
                .map_err(sql)?;
            (rows.collect::<rusqlite::Result<Vec<_>>>().map_err(sql)?, used(&db, &row.id)?)
        };
        Ok(json!({
            "name": row.name, "id": row.id, "peer": row.peer, "side": row.side, "state": row.state,
            "terms": t, "entries": thread.len(), "forks": forks, "ritual": ritual, "acceptance": acceptance,
            "used": {"calls": used_totals.0, "requests": used_totals.1, "tokens": used_totals.2, "tool_calls": used_totals.3},
            "calls": calls, "warnings": warnings,
        }))
    }

    // ------------------------------------------------------------ caller side

    /// Move an own entry the callee has stored into the thread, anchored.
    pub(super) fn confirm_own(&self, pact: &str, s: &Signed) -> Result<(), RpcErr> {
        let mut db = self.db.lock().unwrap();
        let tx = db.transaction().map_err(sql)?;
        let known: i64 = tx.query_row("SELECT COUNT(*) FROM pact_entries WHERE pact=?1 AND hash=?2", params![pact, s.hash], |r| r.get(0)).map_err(sql)?;
        if known == 0 {
            let mut e = pact::open_entry(s, &self.key.public()).map_err(RpcErr::invalid)?;
            e.hash = s.hash.clone();
            let terms = terms_of(&tx, pact)?;
            let thread: Vec<Entry> = thread_of(&tx, pact)?.into_iter().map(|(x, _)| x).collect();
            pact::check_next(&thread, &e, &terms).map_err(RpcErr::conflict)?;
            insert(&tx, pact, &e, s).map_err(sql)?;
            super::epitaphs::write_pact_epitaph(&tx, &self.key, pact, e.n, s.bytes.as_bytes()).map_err(sql)?;
        }
        tx.execute("DELETE FROM pact_outbox WHERE pact=?1 AND hash=?2", params![pact, s.hash]).map_err(sql)?;
        tx.commit().map_err(sql)?;
        Ok(())
    }

    pub(super) fn local_head(&self, pact: &str) -> Result<(i64, String), RpcErr> {
        let db = self.db.lock().unwrap();
        Ok(thread_of(&db, pact)?.last().map(|(e, _)| (e.n, e.hash.clone())).unwrap_or((0, epitaph::ZEROS.to_string())))
    }

    /// Sign an entry as the caller and hand it to the callee, who orders the
    /// thread. It waits in the outbox until the callee has stored it; if the
    /// callee's head moved, it is signed again on top.
    pub(in crate::core) fn caller_append(self: &Arc<Self>, row: &Row, kind: Kind, call: Option<i64>, data: Value) -> Result<Signed, RpcErr> {
        static OUTBOX: Mutex<()> = Mutex::new(());
        let _guard = OUTBOX.lock().unwrap();
        let waiting: i64 = self.db.lock().unwrap().query_row("SELECT COUNT(*) FROM pact_outbox WHERE pact=?1", params![row.id], |r| r.get(0)).map_err(sql)?;
        let initial = if waiting == 0 {
            let (n, prev) = self.local_head(&row.id)?;
            pact::seal_entry(&self.key, &row.id, n + 1, &prev, kind, call, now_ms(), data.clone())
        } else {
            Signed { bytes: String::new(), sig: String::new(), hash: String::new() }
        };
        let ord = {
            let db = self.db.lock().unwrap();
            db.execute(
                "INSERT INTO pact_outbox (pact, kind, call, data, bytes, sig, hash) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
                params![row.id, serde_json::to_string(&kind).unwrap(), call, data.to_string(), initial.bytes, initial.sig, initial.hash],
            )
            .map_err(sql)?;
            db.last_insert_rowid()
        };
        self.flush(row)?;
        let still: i64 = self.db.lock().unwrap().query_row("SELECT COUNT(*) FROM pact_outbox WHERE pact=?1 AND ord=?2", params![row.id, ord], |r| r.get(0)).map_err(sql)?;
        if still > 0 {
            return Err(RpcErr::new("unavailable", 8, format!("the entry is signed and waiting to reach peer `{}`; it will be sent on the next poll", row.peer)));
        }
        let db = self.db.lock().unwrap();
        thread_of(&db, &row.id)?.last().map(|(_, s)| s.clone()).ok_or_else(|| RpcErr::conflict("entry was not stored"))
    }

    /// Send waiting own entries in order.
    pub(super) fn flush(self: &Arc<Self>, row: &Row) -> Result<(), RpcErr> {
        for _ in 0..8 {
            let next: Option<(i64, String, Option<i64>, String, Signed)> = self
                .db
                .lock()
                .unwrap()
                .query_row("SELECT ord, kind, call, data, bytes, sig, hash FROM pact_outbox WHERE pact=?1 ORDER BY ord LIMIT 1", params![row.id], |r| {
                    Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, Signed { bytes: r.get(4)?, sig: r.get(5)?, hash: r.get(6)? }))
                })
                .optional()
                .map_err(sql)?;
            let Some((ord, kind, call, data, mut s)) = next else { return Ok(()) };
            let (head_n, head_prev) = self.local_head(&row.id)?;
            let matches_head = !s.bytes.is_empty()
                && serde_json::from_str::<Entry>(&s.bytes)
                    .ok()
                    .is_some_and(|e| e.n == head_n + 1 && e.prev == head_prev);
            if !matches_head {
                let kind_val: Kind = serde_json::from_str(&kind).map_err(|e| RpcErr::invalid(e.to_string()))?;
                let data_val: Value = serde_json::from_str(&data).map_err(|e| RpcErr::invalid(e.to_string()))?;
                s = pact::seal_entry(&self.key, &row.id, head_n + 1, &head_prev, kind_val, call, now_ms(), data_val);
                self.db
                    .lock()
                    .unwrap()
                    .execute("UPDATE pact_outbox SET bytes=?2, sig=?3, hash=?4 WHERE ord=?1", params![ord, s.bytes, s.sig, s.hash])
                    .map_err(sql)?;
            }
            match self.pact_send(&row.peer, "pact_post", json!({"pact": row.id, "entry": s})) {
                Ok(_) => self.confirm_own(&row.id, &s)?,
                Err(e) if e.code == "state_conflict" && !e.data["entries"].is_null() => {
                    // Take the callee's newer entries, then sign again on top on the next loop iteration.
                    self.take_entries(row, &e.data["entries"])?;
                }
                Err(e) if e.exit == 8 => return Err(e),
                Err(e) => {
                    // The callee refused it outright; it will never be stored.
                    self.db.lock().unwrap().execute("DELETE FROM pact_outbox WHERE ord=?1", params![ord]).map_err(sql)?;
                    return Err(e);
                }
            }
        }
        Ok(())
    }

    /// Store entries the callee holds that this core lacks.
    pub(super) fn take_entries(self: &Arc<Self>, row: &Row, entries: &Value) -> Result<(), RpcErr> {
        let list: Vec<Signed> = serde_json::from_value(entries.clone()).map_err(|e| RpcErr::invalid(e.to_string()))?;
        let t = terms_of(&self.db.lock().unwrap(), &row.id)?;
        let other = self.other_key(&t)?;
        let me = self.key.public().id();
        let (head, _) = self.local_head(&row.id)?;
        for s in list {
            let parsed: Value = serde_json::from_str(&s.bytes).unwrap_or_default();
            let n = parsed["n"].as_i64().unwrap_or(0);
            if parsed["by"] == me {
                if n > head {
                    self.confirm_own(&row.id, &s)?;
                }
                continue;
            }
            let _ = pact::open_entry(&s, &other).map_err(|why| RpcErr::new("not_authorized", 4, format!("pact entry refused: {why}")))?;
            self.receive(&row.id, &s, &other)?;
        }
        Ok(())
    }

    /// Bring one caller-side pact up to date with the callee.
    pub(in crate::core) fn sync_pact(self: &Arc<Self>, row: &Row) -> Result<(), RpcErr> {
        let (after, _) = self.local_head(&row.id)?;
        let got = self.pact_send(&row.peer, "pact_sync", json!({"pact": row.id, "after": after}))?;
        self.take_entries(row, &got["entries"])?;
        self.flush(row)
    }

    /// Sync every open caller-side pact. Errors are reported per pact.
    pub(in crate::core) fn pact_poll(self: &Arc<Self>) -> Result<Value, RpcErr> {
        let rows: Vec<Row> = {
            let db = self.db.lock().unwrap();
            let mut q = db.prepare("SELECT id, name, peer, side, state FROM pacts WHERE side='caller' AND state IN ('offered','agreed','superseded')").map_err(sql)?;
            let rows = q
                .query_map([], |r| Ok(Row { id: r.get(0)?, name: r.get(1)?, peer: r.get(2)?, side: r.get(3)?, state: r.get(4)? }))
                .map_err(sql)?;
            rows.collect::<rusqlite::Result<Vec<_>>>().map_err(sql)?
        };
        let mut errors = Vec::new();
        for row in &rows {
            if let Err(e) = self.sync_pact(row) {
                errors.push(json!({"pact": row.name, "error": e.message}));
            }
        }
        Ok(json!({"synced": rows.len() - errors.len(), "errors": errors}))
    }
}

// --------------------------------------------------------------------- calls
