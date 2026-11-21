use super::*;

/// One call at a time picks its number on the caller side.
static CALLING: Mutex<()> = Mutex::new(());

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct CallArgs {
    pact: String,
    #[serde(default)]
    args: Map<String, Value>,
    #[serde(default)]
    idempotency_key: Option<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct CallRef {
    pact: String,
    call: i64,
    #[serde(default)]
    wait_ms: Option<u64>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Limit {
    pact: String,
    limits: crate::ritual::Essence,
}

fn lower_u(a: Option<u64>, b: Option<u64>) -> Option<u64> {
    match (a, b) {
        (Some(x), Some(y)) => Some(x.min(y)),
        (x, None) => x,
        (None, y) => y,
    }
}

/// The stricter of two sets of limits, field by field.
pub(in crate::core) fn stricter(a: &crate::ritual::Essence, b: &crate::ritual::Essence) -> crate::ritual::Essence {
    let ms = |t: &Option<String>| t.as_deref().and_then(|t| crate::util::parse_duration(t).ok());
    let time = match (ms(&a.time), ms(&b.time)) {
        (Some(x), Some(y)) => if x <= y { a.time.clone() } else { b.time.clone() },
        (Some(_), None) => a.time.clone(),
        _ => b.time.clone(),
    };
    crate::ritual::Essence {
        tool_calls: lower_u(a.tool_calls, b.tool_calls),
        items: lower_u(a.items, b.items),
        requests: lower_u(a.requests, b.requests),
        tokens: lower_u(a.tokens, b.tokens),
        time,
    }
}

/// Usage counted so far under a pact, from recorded calls.
pub(super) fn used(conn: &rusqlite::Connection, pact: &str) -> Result<(u64, u64, u64, u64), RpcErr> {
    let mut q = conn.prepare("SELECT usage FROM pact_calls WHERE pact=?1").map_err(sql)?;
    let rows = q.query_map(params![pact], |r| r.get::<_, Option<String>>(0)).map_err(sql)?.collect::<rusqlite::Result<Vec<_>>>().map_err(sql)?;
    let (mut calls, mut requests, mut tokens, mut tool_calls) = (0u64, 0u64, 0u64, 0u64);
    for u in rows {
        calls = calls.saturating_add(1);
        let u: Value = u.and_then(|u| serde_json::from_str(&u).ok()).unwrap_or_default();
        requests = requests.saturating_add(u["requests"].as_u64().unwrap_or(0));
        tokens = tokens.saturating_add(u["tokens"].as_u64().unwrap_or(0));
        tool_calls = tool_calls.saturating_add(u["tool_calls"].as_u64().unwrap_or(0));
    }
    Ok((calls, requests, tokens, tool_calls))
}

/// Why another call would exceed the pact's totals, if it would.
fn over_totals(t: &Terms, (calls, requests, tokens, tool_calls): (u64, u64, u64, u64)) -> Option<String> {
    let total = &t.ceilings.total;
    if total.calls.is_some_and(|c| calls >= c) {
        return Some(format!("the pact's ceiling of {} calls is reached", total.calls.unwrap()));
    }
    if total.requests.is_some_and(|c| requests >= c) {
        return Some(format!("the pact's ceiling of {} model requests is reached", total.requests.unwrap()));
    }
    if total.tokens.is_some_and(|c| tokens >= c) {
        return Some(format!("the pact's ceiling of {} tokens is reached", total.tokens.unwrap()));
    }
    if total.tool_calls.is_some_and(|c| tool_calls >= c) {
        return Some(format!("the pact's ceiling of {} tool calls is reached", total.tool_calls.unwrap()));
    }
    None
}

fn expired(t: &Terms) -> bool {
    pact::expires_ms(&t.expires).map(|e| now_ms() > e).unwrap_or(true)
}

impl Core {
    /// Make a call under a pact this core is the caller of, as `caller`
    /// (empty for the owner, or `familiar:NAME`).
    pub(in crate::core) fn pact_call_as(self: &Arc<Self>, caller: &str, p: Value) -> Result<Value, RpcErr> {
        let CallArgs { pact, args, idempotency_key } = parse(p)?;
        let row = self.pact_row(&pact)?;
        if row.side != "caller" {
            return Err(RpcErr::invalid(format!("this core is the callee of `{}`; only the caller makes calls", row.name)));
        }
        if row.state != "agreed" {
            return Err(RpcErr::conflict(format!("pact `{}` is {}, not agreed", row.name, row.state)));
        }
        let t = terms_of(&self.db.lock().unwrap(), &row.id)?;
        if expired(&t) {
            return Err(RpcErr::conflict(format!("pact `{}` expired on {}", row.name, t.expires)));
        }
        for k in args.keys() {
            if t.fixed.contains_key(k) {
                return Err(RpcErr::invalid(format!("input `{k}` is fixed by the pact; a call cannot change it")));
            }
            if !t.inputs.contains(k) {
                return Err(RpcErr::invalid(format!("input `{k}` is not one this pact lets a call give (it allows {:?})", t.inputs)));
            }
        }
        let _one = CALLING.lock().unwrap();
        if let Some(key) = &idempotency_key {
            let prior: Option<i64> = self
                .db
                .lock()
                .unwrap()
                .query_row("SELECT call FROM pact_calls WHERE pact=?1 AND asked_by=?2 AND idem=?3", params![row.id, caller, key], |r| r.get(0))
                .optional()
                .map_err(sql)?;
            if let Some(call) = prior {
                return Ok(json!({"pact": row.name, "call": call, "reused": true}));
            }
        }
        let call = {
            let db = self.db.lock().unwrap();
            if let Some(why) = over_totals(&t, used(&db, &row.id)?) {
                return Err(RpcErr::new("exhausted", 10, why));
            }
            let call: i64 = db.query_row("SELECT COALESCE(MAX(call), 0) + 1 FROM pact_calls WHERE pact=?1", params![row.id], |r| r.get(0)).map_err(sql)?;
            let deadline = now_ms().saturating_add(crate::util::parse_duration(&t.ceilings.deadline).unwrap_or(0).min(i64::MAX as u64) as i64);
            db.execute(
                "INSERT INTO pact_calls (pact, call, state, asked_by, deadline, idem) VALUES (?1, ?2, 'sent', ?3, ?4, ?5)",
                params![row.id, call, caller, deadline, idempotency_key],
            )
            .map_err(sql)?;
            call
        };
        let data = json!({"inputs": args, "idempotency_key": idempotency_key, "asked_by": caller});
        match self.caller_append(&row, Kind::Call, Some(call), data) {
            Ok(_) => Ok(json!({"pact": row.name, "call": call, "state": "sent"})),
            Err(e) if e.exit == 8 => Ok(json!({"pact": row.name, "call": call, "state": "queued", "note": e.message})),
            Err(e) => {
                self.db.lock().unwrap().execute("DELETE FROM pact_calls WHERE pact=?1 AND call=?2", params![row.id, call]).map_err(sql)?;
                Err(e)
            }
        }
    }

    /// A call's state and its entries; a familiar sees only its own calls.
    pub(in crate::core) fn pact_call_status(self: &Arc<Self>, caller: &str, p: Value) -> Result<Value, RpcErr> {
        let CallRef { pact, call, wait_ms } = parse(p)?;
        let row = self.pact_row(&pact)?;
        let deadline = Instant::now() + Duration::from_millis(wait_ms.unwrap_or(0).min(300_000));
        loop {
            if row.side == "caller" && wait_ms.is_some() {
                let _ = self.sync_pact(&row);
            }
            let status: Option<(String, Option<String>, Option<String>)> = self
                .db
                .lock()
                .unwrap()
                .query_row("SELECT state, asked_by, run FROM pact_calls WHERE pact=?1 AND call=?2", params![row.id, call], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))
                .optional()
                .map_err(sql)?;
            let Some((state, asked_by, run)) = status.filter(|(_, by, _)| caller.is_empty() || by.as_deref() == Some(caller)) else {
                return Err(RpcErr::not_found(format!("no call {call} under `{}`", row.name)));
            };
            let final_state = matches!(state.as_str(), "accepted" | "rejected" | "closed");
            if final_state || Instant::now() >= deadline {
                let entries: Vec<Value> = thread_of(&self.db.lock().unwrap(), &row.id)?
                    .into_iter()
                    .filter(|(e, _)| e.call == Some(call))
                    .map(|(e, _)| json!({"n": e.n, "kind": e.kind, "by": e.by, "at": e.at, "data": e.data}))
                    .collect();
                return Ok(json!({"pact": row.name, "call": call, "state": state, "asked_by": asked_by, "run": run, "entries": entries}));
            }
            std::thread::sleep(Duration::from_millis(250));
        }
    }

    /// Tighten this side's per-call limits for a pact it is the callee of.
    pub(in crate::core) fn pact_limit(&self, p: Value) -> Result<Value, RpcErr> {
        let Limit { pact, limits } = parse(p)?;
        let row = self.pact_row(&pact)?;
        if row.side != "callee" {
            return Err(RpcErr::invalid("only the callee sets local limits; the caller's limits are the pact's"));
        }
        let t = terms_of(&self.db.lock().unwrap(), &row.id)?;
        let per = &t.ceilings.per_call;
        let ms = |t: &Option<String>| t.as_deref().and_then(|t| crate::util::parse_duration(t).ok());
        let looser = |mine: Option<u64>, theirs: Option<u64>| matches!((mine, theirs), (Some(m), Some(p)) if m > p);
        if looser(limits.tool_calls, per.tool_calls)
            || looser(limits.items, per.items)
            || looser(limits.requests, per.requests)
            || looser(limits.tokens, per.tokens)
            || looser(ms(&limits.time), ms(&per.time))
        {
            return Err(RpcErr::invalid("local limits may only tighten the pact's per-call ceilings; loosening needs a revised pact"));
        }
        self.db.lock().unwrap().execute("UPDATE pacts SET local_limits=?2 WHERE id=?1", params![row.id, serde_json::to_string(&limits).unwrap()]).map_err(sql)?;
        Ok(json!({"pact": row.name, "limits": limits}))
    }

    /// Sign a delivery for `call` and record the call's new state.
    pub(in crate::core) fn deliver(self: &Arc<Self>, pact: &str, call: i64, data: Value) -> Result<(), RpcErr> {
        let closed = data["outcome"] != "returned" && data["held"].as_array().is_none_or(|h| h.is_empty());
        let s = {
            let mut db = self.db.lock().unwrap();
            let tx = db.transaction().map_err(sql)?;
            let s = append_own(&tx, &self.key, pact, Kind::Deliver, Some(call), data)?;
            tx.execute("UPDATE pact_calls SET state=?3 WHERE pact=?1 AND call=?2", params![pact, call, if closed { "closed" } else { "delivered" }]).map_err(sql)?;
            tx.commit().map_err(sql)?;
            s
        };
        self.push_to_peers_async(pact, s);
        Ok(())
    }

    /// The callee starts the pinned ritual for a call it has just stored.
    pub(super) fn start_call(self: &Arc<Self>, pact: &str, e: &Entry) -> Result<(), RpcErr> {
        let call = e.call.ok_or_else(|| RpcErr::invalid("a call entry needs a call number"))?;
        let row = self.pact_row(pact)?;
        let t = terms_of(&self.db.lock().unwrap(), pact)?;
        let deadline = now_ms().saturating_add(crate::util::parse_duration(&t.ceilings.deadline).unwrap_or(0).min(i64::MAX as u64) as i64);
        let refused = {
            let db = self.db.lock().unwrap();
            let so_far = used(&db, pact)?;
            db.execute(
                "INSERT OR IGNORE INTO pact_calls (pact, call, state, asked_by, deadline) VALUES (?1, ?2, 'running', ?3, ?4)",
                params![pact, call, e.data["asked_by"].as_str(), deadline],
            )
            .map_err(sql)?;
            let inputs = e.data["inputs"].as_object().cloned().unwrap_or_default();
            if row.state != "agreed" {
                Some(format!("the pact is {}, not agreed", row.state))
            } else if expired(&t) {
                Some(format!("the pact expired on {}", t.expires))
            } else if let Some(why) = over_totals(&t, so_far) {
                Some(why)
            } else {
                inputs.keys().find(|k| !t.inputs.contains(k)).map(|k| format!("input `{k}` is not one this pact lets a call give"))
            }
        };
        if let Some(why) = refused {
            return self.deliver(pact, call, json!({"outcome": "refused", "error": why}));
        }
        let local: crate::ritual::Essence = self
            .db
            .lock()
            .unwrap()
            .query_row("SELECT local_limits FROM pacts WHERE id=?1", params![pact], |r| r.get::<_, Option<String>>(0))
            .map_err(sql)?
            .and_then(|l| serde_json::from_str(&l).ok())
            .unwrap_or_default();
        let version: Option<u32> = self
            .db
            .lock()
            .unwrap()
            .query_row("SELECT version FROM rituals WHERE name=?1 AND hash=?2", params![t.ritual.name, t.ritual.hash], |r| r.get(0))
            .optional()
            .map_err(sql)?;
        let Some(version) = version else {
            return self.deliver(pact, call, json!({"outcome": "refused", "error": "the pinned ritual version is no longer saved here"}));
        };
        let mut args = t.fixed.clone();
        args.extend(e.data["inputs"].as_object().cloned().unwrap_or_default());
        let spec = json!({
            "target": t.ritual.name, "version": version, "args": args, "caller": format!("pact:{}", row.name),
            "expected_hash": t.ritual.hash, "limits": stricter(&t.ceilings.per_call, &local), "cwd": "/",
            // Ties the run to this call before it starts, so a hold inside it
            // can find its pact; also makes a repeated call entry start nothing new.
            "idempotency_key": format!("pact:{pact}:{call}"),
        });
        match self.invoke(spec) {
            Ok(out) => {
                self.db
                    .lock()
                    .unwrap()
                    .execute("UPDATE pact_calls SET run=?3 WHERE pact=?1 AND call=?2", params![pact, call, out["summon"].as_str()])
                    .map_err(sql)?;
                Ok(())
            }
            Err(err) => self.deliver(pact, call, json!({"outcome": "refused", "error": err.message})),
        }
    }
}

// --------------------------------------------------------------------- holds

/// What a petition filed under a pact call carries.
#[derive(Clone, Debug)]
pub(crate) struct PactTag {
    pub pact: String,
    pub call: i64,
    pub expires: i64,
}

/// Who must decide a pact petition.
#[derive(Clone, Debug)]
pub(crate) struct PactPart {
    pub pact: String,
    pub call: i64,
    pub needs_callee: bool,
    pub needs_caller: bool,
}

impl Core {
    /// For a step of a run started under a pact call: the call's tag, and
    /// (for the pinned ritual's own steps) the caller hold's condition.
    pub(crate) fn pact_hold_for(&self, run: &str, depth: u32, step: &str) -> Option<(PactTag, Option<Value>)> {
        let db = self.db.lock().unwrap();
        let mut root = run.to_string();
        for _ in 0..64 {
            let parent: Option<String> = db.query_row("SELECT parent FROM summons WHERE id=?1", params![root], |r| r.get(0)).optional().ok().flatten()?;
            match parent {
                Some(p) => root = p,
                None => break,
            }
        }
        let key: String = db
            .query_row("SELECT key FROM invocations WHERE summon=?1 AND caller LIKE 'pact:%' AND key LIKE 'pact:%'", params![root], |r| r.get(0))
            .optional()
            .ok()
            .flatten()?;
        let mut parts = key.splitn(3, ':').skip(1);
        let (pact, call) = (parts.next()?.to_string(), parts.next()?.parse::<i64>().ok()?);
        let expires: i64 = db.query_row("SELECT deadline FROM pact_calls WHERE pact=?1 AND call=?2", params![pact, call], |r| r.get(0)).ok()?;
        let terms = terms_of(&db, &pact).ok()?;
        let when = if depth == 0 { terms.held_by_caller.iter().find(|h| h.step == step).map(|h| h.when.clone()) } else { None };
        Some((PactTag { pact, call, expires }, when))
    }

    pub(crate) fn pact_part(&self, petition: &str) -> Result<Option<PactPart>, RpcErr> {
        #[allow(clippy::type_complexity)]
        let row: Option<(Option<String>, Option<i64>, bool, bool)> = self
            .db
            .lock()
            .unwrap()
            .query_row(
                "SELECT pact, pact_call, needs_callee, needs_caller FROM petitions WHERE id=?1",
                params![petition],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
            )
            .optional()
            .map_err(sql)?;
        Ok(match row {
            Some((Some(pact), Some(call), needs_callee, needs_caller)) => {
                Some(PactPart { pact, call, needs_callee, needs_caller })
            }
            _ => None,
        })
    }

    /// Callee: the caller's signed decision on a held action.
    pub(super) fn apply_caller_decision(self: &Arc<Self>, pact: &str, e: &Entry) -> Result<(), RpcErr> {
        self.expire_due(now_ms());
        let id = e.data["petition"].as_str().unwrap_or("").to_string();
        let permit = e.data["permit"].as_bool().unwrap_or(false);
        let part = self.pact_part(&id)?;
        let ours = part.as_ref().is_some_and(|p| p.pact == pact && Some(p.call) == e.call && p.needs_caller);
        let status: Option<String> = self.db.lock().unwrap().query_row("SELECT status FROM petitions WHERE id=?1", params![id], |r| r.get(0)).optional().map_err(sql)?;
        if !ours || status.as_deref() != Some("pending") {
            // Too late (expired or already decided) or not this pact's: the
            // decision stays on the thread, and nothing is done.
            return Ok(());
        }
        let decide = requests::Decide { permit, all: false, ids: vec![id], ritual: None };
        self.petition_decide_as(decide, true).map(|_| ())
    }

    /// Caller: sign the owner's decision on a callee's held action and send it.
    pub(in crate::core) fn relay_decision(self: &Arc<Self>, pet: &Petition, permit: bool) -> Result<Value, RpcErr> {
        let pact = pet.arguments["pact"].as_str().unwrap_or("").to_string();
        let call = pet.arguments["call"].as_i64();
        let row = self.pact_row(&pact)?;
        let n = self
            .db
            .lock()
            .unwrap()
            .execute("UPDATE petitions SET status='executing' WHERE id=?1 AND status='pending'", params![pet.id])
            .map_err(sql)?;
        if n != 1 {
            return Err(RpcErr::conflict("petition already claimed; nothing was decided"));
        }
        let data = json!({"petition": pet.arguments["petition"], "permit": permit, "decided_by": "owner"});
        let sent = match self.caller_append(&row, Kind::Decide, call, data) {
            Ok(s) => json!({"sent": s.hash}),
            Err(e) if e.exit == 8 => json!({"queued": e.message}),
            Err(e) => {
                self.db.lock().unwrap().execute("UPDATE petitions SET status='pending' WHERE id=?1", params![pet.id]).map_err(sql)?;
                return Err(e);
            }
        };
        let status = if permit { "applied" } else { "denied" };
        self.db
            .lock()
            .unwrap()
            .execute("UPDATE petitions SET status=?2, decided=?3, result=?4 WHERE id=?1", params![pet.id, status, now_ms(), sent.to_string()])
            .map_err(sql)?;
        Ok(json!({"id": pet.id, "status": status, "operation": pet.operation, "arguments": pet.arguments, "result": sent}))
    }

    /// Callee: once a pact petition is final, sign a settle entry for it.
    pub(in crate::core) fn settle_if_pact(&self, petition: &str) -> Result<(), RpcErr> {
        let Some(part) = self.pact_part(petition)? else { return Ok(()) };
        let (status, result): (String, Option<String>) =
            self.db.lock().unwrap().query_row("SELECT status, result FROM petitions WHERE id=?1", params![petition], |r| Ok((r.get(0)?, r.get(1)?))).map_err(sql)?;
        if matches!(status.as_str(), "pending" | "executing") {
            return Ok(());
        }
        let mut db = self.db.lock().unwrap();
        let tx = db.transaction().map_err(sql)?;
        let delivered: i64 = tx
            .query_row(
                "SELECT COUNT(*) FROM pact_entries WHERE pact=?1 AND json_extract(bytes, '$.kind')='deliver' AND json_extract(bytes, '$.call')=?2",
                params![part.pact, part.call],
                |r| r.get(0),
            )
            .map_err(sql)?;
        if delivered == 0 {
            return Ok(());
        }
        let already: i64 = tx
            .query_row(
                "SELECT COUNT(*) FROM pact_entries WHERE pact=?1 AND json_extract(bytes, '$.kind')='settle' AND json_extract(bytes, '$.call')=?2 AND json_extract(bytes, '$.data.petition')=?3",
                params![part.pact, part.call, petition],
                |r| r.get(0),
            )
            .map_err(sql)?;
        if already > 0 {
            return Ok(());
        }
        let data = json!({"petition": petition, "state": status, "result_fp": sha256_hex(result.unwrap_or_default().as_bytes())});
        append_own(&tx, &self.key, &part.pact, Kind::Settle, Some(part.call), data)?;
        let open: i64 = tx
            .query_row("SELECT COUNT(*) FROM petitions WHERE pact=?1 AND pact_call=?2 AND status IN ('pending','executing')", params![part.pact, part.call], |r| r.get(0))
            .map_err(sql)?;
        if open == 0 {
            let unapplied: i64 = tx
                .query_row("SELECT COUNT(*) FROM petitions WHERE pact=?1 AND pact_call=?2 AND status != 'applied'", params![part.pact, part.call], |r| r.get(0))
                .map_err(sql)?;
            let returned: i64 = tx
                .query_row(
                    "SELECT COUNT(*) FROM pact_entries WHERE pact=?1 AND json_extract(bytes, '$.kind')='deliver' AND json_extract(bytes, '$.call')=?2 AND json_extract(bytes, '$.data.outcome')='returned'",
                    params![part.pact, part.call],
                    |r| r.get(0),
                )
                .map_err(sql)?;
            let next_state = if unapplied == 0 && returned > 0 { "settled" } else { "closed" };
            tx.execute("UPDATE pact_calls SET state=?3 WHERE pact=?1 AND call=?2 AND state='delivered'", params![part.pact, part.call, next_state]).map_err(sql)?;
        }
        tx.commit().map_err(sql)
    }

    /// Callee: any pending pact petition past its call deadline expires; nothing is performed.
    pub(crate) fn expire_due(&self, now: i64) {
        let due: Vec<String> = {
            let db = self.db.lock().unwrap();
            let Ok(mut q) = db.prepare(
                "SELECT id FROM petitions WHERE pact IS NOT NULL AND status='pending' AND expires IS NOT NULL AND expires < ?1",
            ) else {
                return;
            };
            let Ok(rows) = q.query_map(params![now], |r| r.get(0)) else { return };
            rows.filter_map(Result::ok).collect()
        };
        for id in due {
            let n = self.db.lock().unwrap().execute(
                "UPDATE petitions SET status='expired', decided=?2, result=?3 WHERE id=?1 AND status='pending'",
                params![id, now, json!({"error": "the pact's deadline passed before the caller decided; nothing was done"}).to_string()],
            );
            if n == Ok(1) {
                if let Err(e) = self.settle_if_pact(&id) {
                    eprintln!("pact: cannot settle expired petition {id}: {}", e.message);
                }
            }
        }
    }

    /// Callee: a run started for a call has ended; deliver it.
    pub(crate) fn on_run_end(self: &Arc<Self>, run: &str) {
        let call: Option<(String, i64)> =
            self.db.lock().unwrap().query_row("SELECT pact, call FROM pact_calls WHERE run=?1", params![run], |r| Ok((r.get(0)?, r.get(1)?))).optional().ok().flatten();
        // A run can end before its call row learns the run id; the
        // idempotency key ties them in that case.
        let call = call.or_else(|| {
            let key: String = self.db.lock().unwrap().query_row("SELECT key FROM invocations WHERE summon=?1 AND key LIKE 'pact:%'", params![run], |r| r.get(0)).ok()?;
            let mut parts = key.splitn(3, ':').skip(1);
            Some((parts.next()?.to_string(), parts.next()?.parse().ok()?))
        });
        let Some((pact, call)) = call else { return };
        if let Err(e) = self.deliver_run(&pact, call, run) {
            eprintln!("pact: cannot deliver call {call} of {pact}: {}", e.message);
        }
    }

    pub(super) fn deliver_run(self: &Arc<Self>, pact: &str, call: i64, run: &str) -> Result<(), RpcErr> {
        let delivered: i64 = self
            .db
            .lock()
            .unwrap()
            .query_row(
                "SELECT COUNT(*) FROM pact_entries WHERE pact=?1 AND by=?2 AND json_extract(bytes, '$.kind')='deliver' AND json_extract(bytes, '$.call')=?3",
                params![pact, self.key.public().id(), call],
                |r| r.get(0),
            )
            .map_err(sql)?;
        if delivered > 0 {
            return Ok(());
        }
        let (outcome, result, error, requests, tokens, tool_calls): (Option<String>, Option<String>, Option<String>, i64, i64, i64) = self
            .db
            .lock()
            .unwrap()
            .query_row(
                "SELECT outcome, result, error, requests, tokens, (SELECT COUNT(*) FROM events WHERE summon=?1 AND kind LIKE 'tool.%') FROM summons WHERE id=?1",
                params![run],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?, r.get(5)?)),
            )
            .map_err(sql)?;
        let epitaph: Option<(String, String, String)> =
            self.db.lock().unwrap().query_row("SELECT link, body, sig FROM epitaphs WHERE run=?1", params![run], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?))).optional().map_err(sql)?;
        let held: Vec<Value> = {
            let db = self.db.lock().unwrap();
            let mut q = db
                .prepare("SELECT id, step, operation, arguments, needs_callee, needs_caller FROM petitions WHERE pact=?1 AND pact_call=?2 ORDER BY created")
                .map_err(sql)?;
            let rows = q
                .query_map(params![pact, call], |r| {
                    Ok(json!({"petition": r.get::<_, String>(0)?, "step": r.get::<_, String>(1)?, "operation": r.get::<_, String>(2)?,
                        "arguments": serde_json::from_str::<Value>(&r.get::<_, String>(3)?).unwrap_or_default(),
                        "needs_callee": r.get::<_, bool>(4)?, "needs_caller": r.get::<_, bool>(5)?}))
                })
                .map_err(sql)?;
            rows.collect::<rusqlite::Result<Vec<_>>>().map_err(sql)?
        };
        let outcome = match outcome.as_deref() {
            Some("returned" | "abstained") => "returned",
            Some("exhausted") => "exhausted",
            _ => "failed",
        };
        let usage = json!({"requests": requests, "tokens": tokens, "tool_calls": tool_calls});
        self.db.lock().unwrap().execute("UPDATE pact_calls SET usage=?3 WHERE pact=?1 AND call=?2", params![pact, call, usage.to_string()]).map_err(sql)?;
        let parse = |t: Option<String>| t.and_then(|t| serde_json::from_str::<Value>(&t).ok()).unwrap_or(Value::Null);
        self.deliver(pact, call, json!({
            "outcome": outcome, "result": parse(result), "error": parse(error), "run": run, "usage": usage, "held": held,
            "epitaph": epitaph.map(|(link, body, sig)| json!({"link": link, "body": body, "sig": sig})),
        }))?;
        // Holds decided or expired while the run was still going settle now.
        let ids: Vec<String> = held.iter().filter_map(|h| h["petition"].as_str().map(str::to_string)).collect();
        for id in ids {
            self.settle_if_pact(&id)?;
        }
        Ok(())
    }

    /// Caller: a delivery arrived. Record usage, and raise a local petition
    /// for every held action this side must decide.
    pub(super) fn on_delivered(self: &Arc<Self>, pact: &str, e: &Entry) -> Result<(), RpcErr> {
        let call = e.call.unwrap_or(0);
        let row = self.pact_row(pact)?;
        if row.side != "caller" {
            return Ok(());
        }
        let held = e.data["held"].as_array().cloned().unwrap_or_default();
        let closed = e.data["outcome"] != "returned" && held.is_empty();
        let state = if closed {
            "closed"
        } else if held.iter().any(|h| h["needs_caller"] == true) {
            "awaiting_decision"
        } else if held.is_empty() {
            "settled"
        } else {
            "delivered"
        };
        let db = self.db.lock().unwrap();
        db.execute("UPDATE pact_calls SET state=?3, usage=?4 WHERE pact=?1 AND call=?2", params![pact, call, state, e.data["usage"].to_string()]).map_err(sql)?;
        for h in held.iter().filter(|h| h["needs_caller"] == true) {
            let args = json!({"pact": pact, "call": call, "petition": h["petition"], "step": h["step"], "operation": h["operation"], "arguments": h["arguments"]});
            db.execute(
                "INSERT INTO petitions (id, summon, ritual, step, operation, effect, arguments, selection, because, status, created)
                 VALUES (?1, ?2, ?3, ?4, 'pact.decide', 'write', ?5, '[]', ?6, 'pending', ?7)",
                params![new_id("p"), format!("pact:{pact}:{call}"), row.name, h["step"].as_str().unwrap_or(""), args.to_string(),
                    json!({"pact": row.name, "call": call, "result": e.data["result"]}).to_string(), now_ms()],
            )
            .map_err(sql)?;
        }
        drop(db);
        if state == "settled" {
            self.maybe_accept(pact, call)?;
        }
        Ok(())
    }

    /// Caller: a held action settled on the callee.
    pub(super) fn on_settled(self: &Arc<Self>, pact: &str, e: &Entry) -> Result<(), RpcErr> {
        let row = self.pact_row(pact)?;
        if row.side != "caller" {
            return Ok(());
        }
        let theirs = e.data["petition"].as_str().unwrap_or("");
        let state = e.data["state"].as_str().unwrap_or("");
        let db = self.db.lock().unwrap();
        if !state.is_empty() {
            db.execute(
                "UPDATE petitions SET status=?3, decided=?4 WHERE operation='pact.decide' AND status='pending' AND json_extract(arguments, '$.petition')=?1 AND json_extract(arguments, '$.pact')=?2",
                params![theirs, pact, state, now_ms()],
            )
            .map_err(sql)?;
        }
        // Settled once every held action in the delivery has a settle entry.
        let call = e.call.unwrap_or(0);
        let thread = thread_of(&db, pact)?;
        let held = thread.iter().find(|(x, _)| x.kind == Kind::Deliver && x.call == Some(call)).map(|(x, _)| x.data["held"].as_array().map(|h| h.len()).unwrap_or(0)).unwrap_or(0);
        let settled = thread.iter().filter(|(x, _)| x.kind == Kind::Settle && x.call == Some(call)).count();
        if settled >= held {
            let returned = thread.iter().any(|(x, _)| x.kind == Kind::Deliver && x.call == Some(call) && x.data["outcome"] == "returned");
            let all_applied = thread
                .iter()
                .filter(|(x, _)| x.kind == Kind::Settle && x.call == Some(call))
                .all(|(x, _)| x.data["state"] == "applied");
            let next = if returned && all_applied { "settled" } else { "closed" };
            db.execute("UPDATE pact_calls SET state=?3 WHERE pact=?1 AND call=?2 AND state IN ('delivered','awaiting_decision')", params![pact, call, next]).map_err(sql)?;
            drop(db);
            if returned && all_applied {
                self.maybe_accept(pact, call)?;
            }
        }
        Ok(())
    }
}

// ------------------------------------------------------------------ polling

/// The next wait between polls: back to 15 s after a change, doubling to at
/// most 5 min while nothing changes.
pub(crate) fn next_poll_ms(current: u64, changed: bool) -> u64 {
    if changed {
        15_000
    } else {
        (current * 2).clamp(15_000, 300_000)
    }
}

impl Core {
    /// While serving: sync caller-side pacts with open calls or waiting
    /// entries, and expire caller holds that are past their deadline.
    pub(in crate::core) fn pact_poller(self: Arc<Self>) {
        let mut wait = 15_000u64;
        while !self.stop.load(Ordering::SeqCst) {
            let until = Instant::now() + Duration::from_millis(wait);
            while Instant::now() < until && !self.stop.load(Ordering::SeqCst) {
                std::thread::sleep(Duration::from_millis(200));
            }
            if self.stop.load(Ordering::SeqCst) {
                break;
            }
            self.expire_due(now_ms());
            let count = |db: &rusqlite::Connection| db.query_row("SELECT COUNT(*) FROM pact_entries", [], |r| r.get::<_, i64>(0)).unwrap_or(0);
            let busy: i64 = self
                .db
                .lock()
                .unwrap()
                .query_row(
                    "SELECT (SELECT COUNT(*) FROM pact_outbox) + (SELECT COUNT(*) FROM pacts p WHERE p.side='caller' AND (p.state='offered'
                       OR EXISTS (SELECT 1 FROM pact_calls c WHERE c.pact=p.id AND c.state NOT IN ('accepted','rejected','closed'))))",
                    [],
                    |r| r.get(0),
                )
                .unwrap_or(0);
            if busy == 0 {
                wait = next_poll_ms(wait, false);
                continue;
            }
            let before = count(&self.db.lock().unwrap());
            let _ = self.pact_poll();
            let after = count(&self.db.lock().unwrap());
            wait = next_poll_ms(wait, after != before);
        }
    }
}

// --------------------------------------------------------------- acceptance

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct DecideDelivery {
    pact: String,
    call: i64,
    #[serde(default)]
    reason: Option<String>,
}

fn short(text: &str) -> String {
    if text.chars().count() > 500 { format!("{}…", text.chars().take(500).collect::<String>()) } else { text.to_string() }
}

impl Core {
    /// The pact's checks on a delivered result: the ritual's return schema,
    /// then the acceptance ritual. Never fails; a problem is a "no".
    pub(crate) fn acceptance_checks(self: &Arc<Self>, t: &Terms, schema: Option<&Value>, result: &Value) -> Value {
        let mut out = json!({"accept": true, "reason": "no checks are set", "schema": null, "ritual": null});
        if t.checks.schema {
            let checked = match schema {
                Some(s) => crate::ritual::check_schema(s, result, "result"),
                None => Err("the ritual declares no return schema to check against".into()),
            };
            match checked {
                Ok(()) => {
                    out["schema"] = json!({"ok": true});
                    out["reason"] = json!("the result matches the ritual's return schema");
                }
                Err(e) => {
                    out["schema"] = json!({"ok": false, "error": short(&e)});
                    out["accept"] = json!(false);
                    out["reason"] = json!(format!("the result does not match the ritual's return schema: {}", short(&e)));
                    return out;
                }
            }
        }
        if t.checks.ritual.is_some() {
            let (accept, reason) = match self.run_acceptance(t, result) {
                Ok(v) => v,
                Err(e) => (false, format!("the acceptance ritual could not judge this delivery: {}", short(&e))),
            };
            out["ritual"] = json!({"accept": accept, "reason": reason});
            out["accept"] = json!(accept);
            out["reason"] = json!(reason);
        }
        out
    }

    /// Run the caller's acceptance ritual from the source in the terms.
    pub(super) fn run_acceptance(self: &Arc<Self>, t: &Terms, result: &Value) -> Result<(bool, String), String> {
        let r = t.checks.ritual.as_ref().ok_or("no acceptance ritual")?;
        let def = crate::ritual::parse_file(&r.source)?;
        if crate::ritual::content_hash(&def) != r.hash {
            return Err("its source does not match the pinned hash".into());
        }
        if !result.is_object() {
            return Err("the delivered result is not an object".into());
        }
        let dir = self.paths.state.join("pacts").join(pact::pact_id(t));
        std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
        let file = dir.join("accept.yaml");
        std::fs::write(&file, &r.source).map_err(|e| e.to_string())?;
        let started = self
            .invoke(json!({"target": file.to_str().unwrap_or(""), "args": {"delivery": result}, "caller": format!("pact:{}", t.pact), "cwd": dir.to_str().unwrap_or("/"), "expected_hash": r.hash}))
            .map_err(|e| e.message)?;
        let done = self.dispatch("wait", json!({"summon": started["summon"], "timeout_ms": 120_000})).map_err(|e| e.message)?;
        if done["outcome"] != "returned" {
            return Err(format!("it ended {}: {}", done["outcome"], done["error"]));
        }
        let accept = done["result"]["accept"].as_bool().ok_or("it did not return a boolean `accept`")?;
        let reason = done["result"]["reason"].as_str().unwrap_or("").to_string();
        Ok((accept, short(&reason)))
    }

    /// Caller: a delivery is settled; judge it, or ask the owner to.
    pub(super) fn maybe_accept(self: &Arc<Self>, pact: &str, call: i64) -> Result<(), RpcErr> {
        let row = self.pact_row(pact)?;
        let (t, thread) = {
            let db = self.db.lock().unwrap();
            (terms_of(&db, pact)?, thread_of(&db, pact)?)
        };
        if thread.iter().any(|(e, _)| e.call == Some(call) && matches!(e.kind, Kind::Accept | Kind::Reject)) {
            return Ok(());
        }
        let asked: i64 = self
            .db
            .lock()
            .unwrap()
            .query_row("SELECT COUNT(*) FROM petitions WHERE operation='pact.accept' AND summon=?1", params![format!("pact:{pact}:{call}")], |r| r.get(0))
            .map_err(sql)?;
        if asked > 0 {
            return Ok(());
        }
        let Some((delivery, _)) = thread.iter().find(|(e, _)| e.kind == Kind::Deliver && e.call == Some(call)) else { return Ok(()) };
        let schema = thread.iter().find_map(|(e, _)| e.data["rehearsal"]["output_schema"].as_object().map(|o| Value::Object(o.clone())));
        let result = delivery.data["result"].clone();
        let checks = self.acceptance_checks(&t, schema.as_ref(), &result);
        match t.accept {
            pact::AcceptMode::Checks => {
                let accept = checks["accept"] == true;
                let data = json!({"decided_by": "checks", "reason": checks["reason"], "checks": checks});
                let kind = if accept { Kind::Accept } else { Kind::Reject };
                let state = if accept { "accepted" } else { "rejected" };
                match self.caller_append(&row, kind, Some(call), data) {
                    Ok(_) => {
                        self.db.lock().unwrap().execute("UPDATE pact_calls SET state=?3 WHERE pact=?1 AND call=?2", params![pact, call, state]).map_err(sql)?;
                        Ok(())
                    }
                    Err(e) if e.exit == 8 => Ok(()),
                    Err(e) => Err(e),
                }
            }
            pact::AcceptMode::Owner => {
                self.db
                    .lock()
                    .unwrap()
                    .execute(
                        "INSERT INTO petitions (id, summon, ritual, step, operation, effect, arguments, selection, because, status, created)
                         VALUES (?1, ?2, ?3, 'accept', 'pact.accept', 'write', ?4, '[]', ?5, 'pending', ?6)",
                        params![new_id("p"), format!("pact:{pact}:{call}"), row.name, json!({"pact": pact, "call": call}).to_string(),
                            json!({"pact": row.name, "call": call, "result": result, "checks": checks}).to_string(), now_ms()],
                    )
                    .map_err(sql)?;
                Ok(())
            }
        }
    }

    /// Owner: accept or reject a delivered call (`pact accept` / `pact reject`).
    pub(in crate::core) fn pact_decide_delivery(self: &Arc<Self>, p: Value, accept: bool) -> Result<Value, RpcErr> {
        let DecideDelivery { pact, call, reason } = parse(p)?;
        let row = self.pact_row(&pact)?;
        let pet = self
            .petitions(&requests::Petitions::default())?
            .into_iter()
            .find(|x| x.operation == "pact.accept" && x.summon == format!("pact:{}:{call}", row.id))
            .ok_or_else(|| RpcErr::conflict(format!("call {call} under `{}` is not waiting for acceptance", row.name)))?;
        self.decide_delivery(&pet, accept, reason)
    }

    /// Sign the owner's acceptance or rejection of a delivery and send it.
    pub(in crate::core) fn decide_delivery(self: &Arc<Self>, pet: &Petition, accept: bool, reason: Option<String>) -> Result<Value, RpcErr> {
        let pact = pet.arguments["pact"].as_str().unwrap_or("").to_string();
        let call = pet.arguments["call"].as_i64().unwrap_or(0);
        let row = self.pact_row(&pact)?;
        let n = self.db.lock().unwrap().execute("UPDATE petitions SET status='executing' WHERE id=?1 AND status='pending'", params![pet.id]).map_err(sql)?;
        if n != 1 {
            return Err(RpcErr::conflict("this delivery was already decided"));
        }
        let reason = reason.unwrap_or_else(|| if accept { "accepted by the owner".into() } else { "rejected by the owner".into() });
        let data = json!({"decided_by": "owner", "reason": reason, "checks": pet.because["checks"]});
        let sent = match self.caller_append(&row, if accept { Kind::Accept } else { Kind::Reject }, Some(call), data) {
            Ok(s) => json!({"sent": s.hash}),
            Err(e) if e.exit == 8 => json!({"queued": e.message}),
            Err(e) => {
                self.db.lock().unwrap().execute("UPDATE petitions SET status='pending' WHERE id=?1", params![pet.id]).map_err(sql)?;
                return Err(e);
            }
        };
        let (status, state) = if accept { ("applied", "accepted") } else { ("denied", "rejected") };
        let db = self.db.lock().unwrap();
        db.execute("UPDATE petitions SET status=?2, decided=?3, result=?4 WHERE id=?1", params![pet.id, status, now_ms(), sent.to_string()]).map_err(sql)?;
        db.execute("UPDATE pact_calls SET state=?3 WHERE pact=?1 AND call=?2", params![pact, call, state]).map_err(sql)?;
        Ok(json!({"id": pet.id, "pact": row.name, "call": call, "status": status, "state": state, "result": sent}))
    }
}

