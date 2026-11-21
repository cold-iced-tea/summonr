//! petitions service.
use super::*;

/// How long an approved write (and its digest re-check) may take.
const APPROVED_WRITE_TIME: std::time::Duration = std::time::Duration::from_secs(600);

impl Core {
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn propose(
        &self,
        summon: &str,
        ritual: &str,
        step: &str,
        operation: &str,
        effect: &str,
        args: &Value,
        check: Option<&reagent::Check>,
        selection: &[String],
        because: &Value,
        authority: &[Grant],
        digest: Option<&str>,
        pact: Option<(&super::pacts::PactTag, bool, bool)>,
    ) -> String {
        let id = new_id("p");
        let (tag, needs_callee, needs_caller) = match pact {
            Some((t, callee, caller)) => (Some(t), callee, caller),
            None => (None, true, false),
        };
        let now = now_ms();
        let default_expires = self
            .cfg
            .petition_ttl
            .as_deref()
            .and_then(|t| crate::util::parse_duration(t).ok())
            .map(|ms| now + ms as i64);
        let expires = tag.map(|t| t.expires).or(default_expires);
        self.persisted(self.db.lock().unwrap().execute(
            "INSERT INTO petitions (id, summon, ritual, step, operation, effect, arguments, verify, selection, because, status, created, authority, digest,
               pact, pact_call, needs_callee, needs_caller, expires)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, 'pending', ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?18)",
            params![id, summon, ritual, step, operation, effect, args.to_string(), check.map(|c| serde_json::to_string(c).unwrap_or_default()),
                serde_json::to_string(selection).unwrap_or_default(), because.to_string(), now, serde_json::to_string(authority).unwrap(), digest,
                tag.map(|t| t.pact.clone()), tag.map(|t| t.call), needs_callee, needs_caller, expires],
        ));
        id
    }

    pub(super) fn petitions(&self, p: &requests::Petitions) -> Result<Vec<Petition>, RpcErr> {
        let now = now_ms();
        self.expire_due(now);
        self.persisted(self.db.lock().unwrap().execute(
            "UPDATE petitions SET status='expired', decided=?1, result=?2 WHERE pact IS NULL AND status='pending' AND expires IS NOT NULL AND expires <= ?1",
            params![now, json!({"error": "petition TTL expired before owner decided; nothing was done"}).to_string()],
        ));
        let status = p.status.as_str();
        let summon = p.summon.as_deref().unwrap_or("");
        let db = self.db.lock().unwrap();
        let mut st = db
            .prepare(
                "SELECT id, summon, ritual, step, operation, effect, arguments, verify, because, status, created, decided, result, selection, authority, digest
                 FROM petitions WHERE (?1 = 'all' OR status = ?1) AND (?2 = '' OR summon = ?2) ORDER BY created",
            )
            .map_err(storage_error)?;
        let rows = st
            .query_map(params![status, summon], |r| {
                serde_json::from_value(json!({
                    "id": r.get::<_, String>(0)?, "summon": r.get::<_, String>(1)?, "ritual": r.get::<_, String>(2)?,
                    "step": r.get::<_, String>(3)?, "operation": r.get::<_, String>(4)?, "effect": r.get::<_, String>(5)?,
                    "arguments": parse_json(Some(r.get::<_, String>(6)?))?, "verify": parse_json(r.get::<_, Option<String>>(7)?)?,
                    "because": parse_json(Some(r.get::<_, String>(8)?))?, "status": r.get::<_, String>(9)?,
                    "created": r.get::<_, i64>(10)?, "decided": r.get::<_, Option<i64>>(11)?,
                    "result": parse_json(r.get::<_, Option<String>>(12)?)?, "selection": parse_json(Some(r.get::<_, String>(13)?))?,
                    "authority": parse_json(r.get::<_, Option<String>>(14)?)?,
                    "digest": r.get::<_, Option<String>>(15)?,
                })).map_err(|e|rusqlite::Error::FromSqlConversionFailure(0,rusqlite::types::Type::Text,Box::new(e)))
            })
            .map_err(storage_error)?;
        rows.collect::<Result<Vec<_>, _>>().map_err(storage_error)
    }

    pub(super) fn petition_decide(self: &Arc<Self>, p: Value) -> Result<Value, RpcErr> {
        let p: requests::Decide = decode(p)?;
        self.petition_decide_as(p, false)
    }

    /// Decide petitions as this core's owner, or (`by_caller`) as the caller
    /// of the pact a petition was filed under. A pact petition is performed
    /// only once every side it needs has permitted; a deny from either is final.
    pub(super) fn petition_decide_as(self: &Arc<Self>, p: requests::Decide, by_caller: bool) -> Result<Value, RpcErr> {
        self.expire_due(now_ms());
        let permit = p.permit;
        let pending = self.petitions(&requests::Petitions::default())?;
        let chosen: Vec<Petition> = if p.all {
            let only = p.ritual.as_deref();
            pending
                .into_iter()
                .filter(|x| only.is_none_or(|r| x.ritual == r))
                .collect()
        } else {
            let ids = p.ids;
            if ids.is_empty() {
                return Err(RpcErr::invalid(
                    "name the petitions to decide, or say `all`",
                ));
            }
            // Every named petition must be pending before any is acted on.
            let mut out = Vec::new();
            for id in &ids {
                match pending.iter().find(|x| x.id == *id) {
                    Some(x) => out.push(x.clone()),
                    None => {
                        return Err(RpcErr::not_found(format!(
                            "`{id}` is not a pending petition; nothing was decided"
                        )))
                    }
                }
            }
            out
        };
        let mut seen = HashSet::new();
        if chosen.iter().any(|pet| !seen.insert(pet.id.as_str())) {
            return Err(RpcErr::invalid(
                "duplicate petition IDs; nothing was decided",
            ));
        }
        // Pact parts: who must decide, and whether this decision completes it.
        let parts: Vec<Option<super::pacts::PactPart>> = chosen.iter().map(|pet| self.pact_part(&pet.id)).collect::<Result<_, _>>()?;
        for (pet, part) in chosen.iter().zip(&parts) {
            match part {
                Some(part) if !by_caller && !part.needs_callee => {
                    return Err(RpcErr::new("not_authorized", 4, format!("`{}` is held for the pact's caller to decide, not this core's owner; nothing was decided", pet.id)))
                }
                None if by_caller => return Err(RpcErr::new("not_authorized", 4, "the caller decides only petitions filed under its pact")),
                _ => {}
            }
        }
        let mut waiting = Vec::new();
        let mut ready = Vec::new();
        let mut relayed = Vec::new();
        // Atomically record part decisions, re-read the other party's decision
        // inside the same transaction (eliminating the concurrent double-permit
        // race), and claim ready local petitions as `executing`.
        {
            let mut db = self.db.lock().unwrap();
            let tx = db
                .transaction()
                .map_err(|e| RpcErr::new("storage_failed", 11, e.to_string()))?;
            for (pet, part) in chosen.into_iter().zip(parts) {
                let is_relayed = pet.operation == "pact.decide" || pet.operation == "pact.accept";
                if let Some(_part) = part {
                    let column = if by_caller { "caller_decision" } else { "callee_decision" };
                    let n = tx
                        .execute(
                            &format!("UPDATE petitions SET {column}=?2 WHERE id=?1 AND status='pending'"),
                            params![pet.id.as_str(), if permit { "permit" } else { "deny" }],
                        )
                        .map_err(|e| RpcErr::new("storage_failed", 11, e.to_string()))?;
                    if n != 1 {
                        return Err(RpcErr::conflict(
                            "petition already claimed; nothing was decided",
                        ));
                    }
                    let (needs_callee, needs_caller, callee_dec, caller_dec): (i64, i64, Option<String>, Option<String>) = tx
                        .query_row(
                            "SELECT needs_callee, needs_caller, callee_decision, caller_decision FROM petitions WHERE id=?1",
                            params![pet.id.as_str()],
                            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
                        )
                        .map_err(|e| RpcErr::new("storage_failed", 11, e.to_string()))?;
                    let other_permitted = if by_caller {
                        needs_callee == 0 || callee_dec.as_deref() == Some("permit")
                    } else {
                        needs_caller == 0 || caller_dec.as_deref() == Some("permit")
                    };
                    if permit && !other_permitted {
                        waiting.push(pet.id.clone());
                        continue;
                    }
                }
                if is_relayed {
                    relayed.push(pet);
                } else {
                    let n = tx
                        .execute(
                            "UPDATE petitions SET status='executing' WHERE id=?1 AND status='pending'",
                            params![pet.id.as_str()],
                        )
                        .map_err(|e| RpcErr::new("storage_failed", 11, e.to_string()))?;
                    if n != 1 {
                        return Err(RpcErr::conflict(
                            "petition already claimed; nothing was decided",
                        ));
                    }
                    ready.push(pet);
                }
            }
            tx.commit()
                .map_err(|e| RpcErr::new("storage_failed", 11, e.to_string()))?;
        }
        let chosen = ready;
        let mut decided = Vec::new();
        for pet in relayed {
            decided.push(if pet.operation == "pact.accept" {
                self.decide_delivery(&pet, permit, None)?
            } else {
                self.relay_decision(&pet, permit)?
            });
        }
        for pet in chosen {
            let id = pet.id.as_str().to_string();
            let mut warning = None;
            // An approved write is bounded like any run: a hung program
            // fails it instead of holding this call (and the petition) forever.
            let deadline = std::time::Instant::now() + APPROVED_WRITE_TIME;
            let now = || {
                reagent::target_digest_with_config(
                    self.cfg.mail_root.as_deref(),
                    Some(&self.cfg),
                    &pet.operation,
                    &pet.arguments,
                    Some(deadline),
                )
                .ok()
                .flatten()
            };
            let (status, result) = if !permit {
                (PetitionState::Denied, json!(null))
            } else if pet.operation == "authority.grant" {
                let reagent = pet.arguments["reagent"].as_str().unwrap_or("");
                let action = pet.arguments["action"].as_str().unwrap_or("");
                let on = pet.arguments.get("on").cloned().unwrap_or_else(|| json!({}));
                let into = pet.arguments.get("into").cloned().unwrap_or_else(|| json!({}));
                match self.grant(json!({
                    "ritual": pet.ritual,
                    "scopes": [{"reagent": reagent, "select": [action]}],
                    "on": on,
                    "into": into,
                })) {
                    Ok(granted) => (PetitionState::Applied, json!({"granted": granted})),
                    Err(e) => (PetitionState::Failed, json!({"error": e.message})),
                }
            } else if let Some(recorded) = pet.digest.as_deref().filter(|d| now().as_deref() != Some(*d)) {
                // What would be acted on is not what was reviewed.
                (
                    PetitionState::Stale,
                    json!({"recorded": recorded, "now": now(), "error": "the resource changed after this petition was filed; nothing was done"}),
                )
            } else {
                if pet.digest.is_none() {
                    warning = Some("petition is not bound to a resource version; it was filed before digests or by a reagent without one");
                }
                match (self.grants_for(&pet.ritual), pet.authority.as_ref()) {
                    (Err(e), _) => (PetitionState::Failed, json!({"error": e.message})),
                    // Legacy petitions have no snapshot: refuse effects rather than broaden authority.
                    (Ok(_), None) => (
                        PetitionState::Failed,
                        json!({"error": "petition has no authority snapshot; reconcile or deny it"}),
                    ),
                    (Ok(current), Some(snapshot)) => {
                        let grants = ritual::intersect_grants(&current, snapshot);
                        let selected = pet.selection;
                        let selected = Mutex::new(selected);
                        let auth = reagent::Authority {
                            deadline: Some(deadline),
                            on_item: None,
                            grants: &grants,
                            selected: &selected,
                            // Checked again against the exact bytes being moved.
                            expected_digest: pet.digest.as_deref(),
                            config: Some(&self.cfg),
                        };
                        let check = pet.verify;
                        match reagent::perform(
                            self.cfg.mail_root.as_deref(),
                            &pet.operation,
                            &pet.arguments,
                            check.as_ref(),
                            &auth,
                            &crate::io::Live,
                        ) {
                            Ok((output, evidence)) => (
                                PetitionState::Applied,
                                json!({"output": output, "evidence": evidence}),
                            ),
                            Err(f @ reagent::Fail::Unverified(_)) => {
                                (PetitionState::Unverified, json!({"error": f.message()}))
                            }
                            // The resource changed between the first check and the write.
                            Err(f) if pet.digest.is_some() && now() != pet.digest => (
                                PetitionState::Stale,
                                json!({"recorded": pet.digest, "now": now(), "error": f.message()}),
                            ),
                            Err(f) => (PetitionState::Failed, json!({"error": f.message()})),
                        }
                    }
                }
            };
            let decided_ms = now_ms();
            let write_res: rusqlite::Result<usize> = (|| {
                let mut db = self.db.lock().unwrap();
                let tx = db.transaction()?;
                let n = tx.execute(
                    "UPDATE petitions SET status=?2, decided=?3, result=?4 WHERE id=?1 AND status='executing'",
                    params![id, status.as_str(), decided_ms, result.to_string()],
                )?;
                super::epitaphs::write_petition_epitaph(
                    &tx,
                    &self.key,
                    &id,
                    &pet.summon,
                    status.as_str(),
                    decided_ms,
                    &result,
                )?;
                tx.commit()?;
                Ok(n)
            })();
            self.persisted(write_res);
            decided.push(json!({"id": id, "status": status, "operation": pet.operation, "arguments": pet.arguments, "result": result, "warning": warning}));
        }
        self.storage_health()?;
        for d in &decided {
            if let Some(id) = d["id"].as_str() {
                self.settle_if_pact(id)?;
            }
        }
        Ok(json!({"decided": decided, "waiting": waiting}))
    }

    fn propose_missing_grants(&self, ritual: &str, missing: &[Grant]) -> Vec<String> {
        let mut pids = Vec::new();
        for g in missing {
            let args = json!({
                "reagent": g.reagent,
                "action": g.action,
                "on": g.on,
                "into": g.into,
            });
            let existing: Option<String> = self
                .db
                .lock()
                .unwrap()
                .query_row(
                    "SELECT id FROM petitions WHERE ritual=?1 AND operation='authority.grant' AND status='pending' AND arguments=?2 LIMIT 1",
                    params![ritual, args.to_string()],
                    |row| row.get(0),
                )
                .optional()
                .ok()
                .flatten();
            let pid = match existing {
                Some(p) => p,
                None => self.propose(
                    "",
                    ritual,
                    "authority",
                    "authority.grant",
                    "grant",
                    &args,
                    None,
                    &[],
                    &json!({"reason": format!("missing authority for {}.{}", g.reagent, g.action)}),
                    &[],
                    None,
                    None,
                ),
            };
            pids.push(pid);
        }
        pids
    }

    pub(crate) fn propose_missing_authority(&self, r: &Resolved) -> Vec<String> {
        self.propose_missing_grants(&r.def.ritual, &r.missing)
    }

    pub(super) fn petition_request(&self, p: Value) -> Result<Value, RpcErr> {
        let req: requests::PetitionRequest = decode(p)?;
        let (_, _, def) = self
            .load_ritual(&req.ritual, None)?
            .ok_or_else(|| RpcErr::not_found(format!("no ritual `{}` in the grimoire", req.ritual)))?;
        let inputs = ritual::resolve_authority_inputs(&def, &req.args).map_err(RpcErr::invalid)?;
        let rendered = Self::render_requires(&def.requires, &inputs)?;
        let grants = self.grants_for(&def.ritual)?;
        let missing: Vec<Grant> = rendered
            .iter()
            .flat_map(Requirement::atoms)
            .filter(|need| !grants.iter().any(|g| g.covers(need)))
            .collect();
        if missing.is_empty() {
            return Ok(json!({"ritual": def.ritual, "petitions": [], "missing": []}));
        }
        let pids = self.propose_missing_grants(&def.ritual, &missing);
        let desc: Vec<String> = missing.iter().map(Grant::describe).collect();
        Ok(json!({
            "ritual": def.ritual,
            "petitions": pids,
            "missing": desc,
        }))
    }
}
