//! Rite service: durable multi-stage workflows with human (`petition`) and
//! automated (`touchstone`/`assay`) gates, completion-vs-acceptance separation,
//! and assumption dependencies that reopen downstream stages when falsified.
use super::*;
use crate::rite::{self, GateKind, RiteDef};

fn sql(e: rusqlite::Error) -> RpcErr {
    RpcErr::new("storage_failed", 11, e.to_string())
}

fn parse<T: serde::de::DeserializeOwned>(p: Value) -> Result<T, RpcErr> {
    serde_json::from_value(p).map_err(|e| RpcErr::invalid(e.to_string()))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RiteStartArgs {
    file: String,
    #[serde(default)]
    args: BTreeMap<String, String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RiteIdArgs {
    rite: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct AssumptionChangeArgs {
    rite: String,
    assumption: String,
    #[serde(default)]
    reason: Option<String>,
}

struct RiteEnv {
    inputs: Value,
    stages: Map<String, Value>,
}

impl ritual::Env for RiteEnv {
    fn lookup(&self, path: &[String]) -> Result<Value, String> {
        match path.first().map(String::as_str) {
            Some("inputs") => ritual::descend(self.inputs.clone(), &path[1..], path),
            Some("stages") => {
                ritual::descend(Value::Object(self.stages.clone()), &path[1..], path)
            }
            _ => Err(format!("unknown rite reference `{}`", path.join("."))),
        }
    }
}

impl Core {
    fn resolve_rite_inputs(
        def: &RiteDef,
        raw_args: &BTreeMap<String, String>,
    ) -> Result<Value, RpcErr> {
        // Reuse ritual::resolve_inputs via a thin synthetic Ritual shell carrying only `inputs`.
        let mut supplied = Map::new();
        for (k, v) in raw_args {
            let parsed = serde_json::from_str::<Value>(v).unwrap_or_else(|_| Value::String(v.clone()));
            supplied.insert(k.clone(), parsed);
        }
        let shell: Ritual = serde_json::from_value(json!({
            "ritual": def.rite,
            "inputs": def.inputs,
            "steps": [],
        }))
        .map_err(|e| RpcErr::invalid(e.to_string()))?;
        ritual::resolve_inputs(&shell, &supplied).map_err(RpcErr::invalid)
    }

    fn load_rite_row(&self, id: &str) -> Result<(RiteDef, Value, String), RpcErr> {
        let db = self.db.lock().unwrap();
        let row: Option<(String, String, String)> = db
            .query_row(
                "SELECT def_json, inputs_json, status FROM rites WHERE id=?1 OR name=?1 ORDER BY created DESC LIMIT 1",
                params![id],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .optional()
            .map_err(sql)?;
        let Some((def_s, inp_s, status)) = row else {
            return Err(RpcErr::not_found(format!("no rite `{id}`")));
        };
        let def: RiteDef = serde_json::from_str(&def_s)
            .map_err(|e| RpcErr::new("storage_failed", 11, format!("damaged rite: {e}")))?;
        let inputs: Value = serde_json::from_str(&inp_s)
            .map_err(|e| RpcErr::new("storage_failed", 11, format!("damaged rite inputs: {e}")))?;
        Ok((def, inputs, status))
    }

    fn resolve_rite_id(&self, id_or_name: &str) -> Result<String, RpcErr> {
        let db = self.db.lock().unwrap();
        db.query_row(
            "SELECT id FROM rites WHERE id=?1 OR name=?1 ORDER BY created DESC LIMIT 1",
            params![id_or_name],
            |r| r.get::<_, String>(0),
        )
        .optional()
        .map_err(sql)?
        .ok_or_else(|| RpcErr::not_found(format!("no rite `{id_or_name}`")))
    }

    fn record_rite_event(&self, rite: &str, kind: &str, detail: Value) {
        self.persisted(self.db.lock().unwrap().execute(
            "INSERT INTO rite_events (rite, ts, kind, detail_json) VALUES (?1, ?2, ?3, ?4)",
            params![rite, now_ms(), kind, detail.to_string()],
        ));
    }

    pub(super) fn rite_rehearse(&self, p: Value) -> Result<Value, RpcErr> {
        let p: RiteStartArgs = parse(p)?;
        let text = std::fs::read_to_string(&p.file)
            .map_err(|e| RpcErr::not_found(format!("cannot read {}: {e}", p.file)))?;
        let mut def = rite::parse_rite_file(&text).map_err(RpcErr::invalid)?;
        let plan = rite::prepare_and_validate_rite(&mut def, &self.resolver(), &self.cfg)
            .map_err(|es| RpcErr::invalid(es.join("; ")))?;
        let inputs = Self::resolve_rite_inputs(&def, &p.args)?;
        let hash = rite::rite_hash(&def);
        let stages: Vec<Value> = plan
            .order
            .iter()
            .map(|sid| {
                let st = def.stages.iter().find(|s| &s.id == sid).unwrap();
                let eff = plan.stage_effects.get(sid).cloned().unwrap_or_default();
                json!({
                    "id": st.id,
                    "ritual": st.ritual,
                    "version": st.version,
                    "hash": st.hash,
                    "after": st.after,
                    "assumes": st.assumes,
                    "gate": st.gate,
                    "effects": eff.iter().map(|e| json!({"step": e.step, "what": e.what, "effect": e.effect})).collect::<Vec<_>>(),
                })
            })
            .collect();
        Ok(json!({
            "rite": def.rite,
            "hash": hash,
            "inputs": inputs,
            "assumptions": def.assumptions,
            "stages": stages,
        }))
    }

    pub(super) fn rite_start(self: &Arc<Self>, p: Value) -> Result<Value, RpcErr> {
        let p: RiteStartArgs = parse(p)?;
        let text = std::fs::read_to_string(&p.file)
            .map_err(|e| RpcErr::not_found(format!("cannot read {}: {e}", p.file)))?;
        let mut def = rite::parse_rite_file(&text).map_err(RpcErr::invalid)?;
        let _plan = rite::prepare_and_validate_rite(&mut def, &self.resolver(), &self.cfg)
            .map_err(|es| RpcErr::invalid(es.join("; ")))?;
        let inputs = Self::resolve_rite_inputs(&def, &p.args)?;
        let hash = rite::rite_hash(&def);
        let id = new_id("rite");
        let now = now_ms();

        {
            let mut db = self.db.lock().unwrap();
            let tx = db.transaction().map_err(sql)?;
            tx.execute(
                "INSERT INTO rites (id, name, hash, def_json, inputs_json, status, created, updated)
                 VALUES (?1, ?2, ?3, ?4, ?5, 'active', ?6, ?6)",
                params![
                    id,
                    def.rite,
                    hash,
                    serde_json::to_string(&def).unwrap_or_default(),
                    inputs.to_string(),
                    now
                ],
            )
            .map_err(sql)?;
            for a in &def.assumptions {
                tx.execute(
                    "INSERT INTO rite_assumptions (rite, id, statement, status, falsified_reason, updated)
                     VALUES (?1, ?2, ?3, 'held', NULL, ?4)",
                    params![id, a.id, a.statement, now],
                )
                .map_err(sql)?;
            }
            for s in &def.stages {
                tx.execute(
                    "INSERT INTO rite_stages (rite, stage, ritual, version, hash, status, summon_id, output_json, gate_status, petition_id, accepted, reopened_reason)
                     VALUES (?1, ?2, ?3, ?4, ?5, 'pending', NULL, NULL, 'waiting', NULL, 0, NULL)",
                    params![id, s.id, s.ritual, s.version.unwrap_or(1), s.hash.as_deref().unwrap_or("")],
                )
                .map_err(sql)?;
            }
            tx.execute(
                "INSERT INTO rite_events (rite, ts, kind, detail_json) VALUES (?1, ?2, 'started', ?3)",
                params![id, now, json!({"name": def.rite, "hash": hash}).to_string()],
            )
            .map_err(sql)?;
            tx.commit().map_err(sql)?;
        }

        // Advance immediately up to the first gate or completion.
        self.rite_advance(json!({"rite": id}))
    }

    pub(super) fn rite_advance(self: &Arc<Self>, p: Value) -> Result<Value, RpcErr> {
        let p: RiteIdArgs = parse(p)?;
        let id = self.resolve_rite_id(&p.rite)?;
        let (def, inputs, _) = self.load_rite_row(&id)?;
        let order = rite::topo_order(&def.stages).map_err(RpcErr::invalid)?;

        let mut progressed = true;
        while progressed {
            progressed = false;
            // Snapshot current stage states and assumption states.
            let (stage_map, held_assumptions) = self.rite_snapshot(&id)?;
            for sid in &order {
                let st_decl = def.stages.iter().find(|s| &s.id == sid).unwrap();
                let st = &stage_map[sid];
                if st.status != "pending" && st.status != "reopened" {
                    continue;
                }
                // All upstream stages must be both completed AND accepted.
                let deps_ready = st_decl.after.iter().all(|dep| {
                    stage_map
                        .get(dep)
                        .is_some_and(|d| d.status == "completed" && d.accepted)
                });
                if !deps_ready {
                    continue;
                }
                // All assumed assumptions must be held.
                let assumes_ready = st_decl
                    .assumes
                    .iter()
                    .all(|a| held_assumptions.contains(a));
                if !assumes_ready {
                    continue;
                }

                // Render stage inputs from rite inputs + upstream accepted outputs.
                let mut stages_env = Map::new();
                for dep in rite::ancestors_of(&def.stages, sid) {
                    if let Some(d) = stage_map.get(&dep).filter(|d| d.status == "completed" && d.accepted) {
                        stages_env.insert(
                            dep,
                            json!({"output": d.output.clone().unwrap_or(Value::Null)}),
                        );
                    }
                }
                let env = RiteEnv {
                    inputs: inputs.clone(),
                    stages: stages_env,
                };
                let mut rendered_args = Map::new();
                for (k, v) in &st_decl.inputs {
                    let rv = ritual::render_value(v, &env).map_err(RpcErr::invalid)?;
                    rendered_args.insert(k.clone(), rv);
                }

                self.persisted(self.db.lock().unwrap().execute(
                    "UPDATE rite_stages SET status='running', reopened_reason=NULL WHERE rite=?1 AND stage=?2",
                    params![id, sid],
                ));
                self.record_rite_event(&id, "stage_started", json!({"stage": sid, "ritual": st_decl.ritual}));

                // Run the pinned stage ritual through the normal summon executor.
                let started = match self.invoke(json!({
                    "target": st_decl.ritual,
                    "version": st_decl.version,
                    "expected_hash": st_decl.hash,
                    "args": rendered_args,
                    "cwd": "/",
                })) {
                    Ok(started) => started,
                    Err(error) => {
                        self.persisted(self.db.lock().unwrap().execute(
                            "UPDATE rite_stages SET status='failed', gate_status='rejected', accepted=0 WHERE rite=?1 AND stage=?2",
                            params![id, sid],
                        ));
                        self.record_rite_event(
                            &id,
                            "stage_failed",
                            json!({"stage": sid, "error": error.message}),
                        );
                        self.refresh_rite_status(&id)?;
                        return Err(error);
                    }
                };
                let summon_id = started["summon"].as_str().unwrap_or("").to_string();
                let ended = self.wait(json!({"summon": summon_id, "timeout_ms": 300_000}))?;
                let outcome = ended["outcome"].as_str().unwrap_or("failed");
                if outcome != "returned" {
                    self.persisted(self.db.lock().unwrap().execute(
                        "UPDATE rite_stages SET status='failed', summon_id=?3, gate_status='rejected', accepted=0 WHERE rite=?1 AND stage=?2",
                        params![id, sid, summon_id],
                    ));
                    self.record_rite_event(
                        &id,
                        "stage_failed",
                        json!({"stage": sid, "summon": summon_id, "outcome": outcome}),
                    );
                    continue;
                }

                let output = ended["result"].clone();
                // Record completion separately from acceptance!
                self.persisted(self.db.lock().unwrap().execute(
                    "UPDATE rite_stages SET status='completed', summon_id=?3, output_json=?4, accepted=0 WHERE rite=?1 AND stage=?2",
                    params![id, sid, summon_id, output.to_string()],
                ));
                self.record_rite_event(
                    &id,
                    "stage_completed",
                    json!({"stage": sid, "summon": summon_id}),
                );

                // Evaluate the stage's gate.
                match st_decl.gate.as_ref().map(|g| g.kind) {
                    None => {
                        self.persisted(self.db.lock().unwrap().execute(
                            "UPDATE rite_stages SET gate_status='accepted', accepted=1 WHERE rite=?1 AND stage=?2",
                            params![id, sid],
                        ));
                        self.record_rite_event(&id, "gate_accepted", json!({"stage": sid, "gate": "none"}));
                        progressed = true;
                    }
                    Some(GateKind::Owner) => {
                        let because = json!({
                            "rite": id,
                            "stage": sid,
                            "output": output,
                            "assumptions": st_decl.assumes,
                        });
                        let pid = self.propose(
                            &summon_id,
                            &st_decl.ritual,
                            sid,
                            "rite.gate",
                            "gate",
                            &json!({"rite": id, "stage": sid}),
                            None,
                            &[],
                            &because,
                            &[],
                            None,
                            None,
                        );
                        self.persisted(self.db.lock().unwrap().execute(
                            "UPDATE rite_stages SET gate_status='pending_owner', petition_id=?3, accepted=0 WHERE rite=?1 AND stage=?2",
                            params![id, sid, pid],
                        ));
                        self.record_rite_event(
                            &id,
                            "gate_held",
                            json!({"stage": sid, "petition": pid}),
                        );
                    }
                    Some(GateKind::Touchstone) => {
                        let gate = st_decl.gate.as_ref().unwrap();
                        let tr = gate.ritual.as_deref().unwrap_or("");
                        let ts_args = json!({"output": output});
                        let ts_start = self.invoke(json!({
                            "target": tr,
                            "version": gate.version,
                            "expected_hash": gate.hash,
                            "args": ts_args,
                            "cwd": "/",
                        }))?;
                        let ts_id = ts_start["summon"].as_str().unwrap_or("").to_string();
                        let ts_end = self.wait(json!({"summon": ts_id, "timeout_ms": 60_000}))?;
                        let ts_ok = ts_end["outcome"] == "returned"
                            && ts_end["result"].get("pass") != Some(&Value::Bool(false))
                            && ts_end["result"].get("ok") != Some(&Value::Bool(false));
                        if ts_ok {
                            self.persisted(self.db.lock().unwrap().execute(
                                "UPDATE rite_stages SET gate_status='accepted', accepted=1 WHERE rite=?1 AND stage=?2",
                                params![id, sid],
                            ));
                            self.record_rite_event(
                                &id,
                                "gate_accepted",
                                json!({"stage": sid, "gate": "touchstone", "summon": ts_id}),
                            );
                            progressed = true;
                        } else {
                            self.persisted(self.db.lock().unwrap().execute(
                                "UPDATE rite_stages SET gate_status='rejected', accepted=0 WHERE rite=?1 AND stage=?2",
                                params![id, sid],
                            ));
                            self.record_rite_event(
                                &id,
                                "gate_rejected",
                                json!({"stage": sid, "gate": "touchstone", "summon": ts_id, "result": ts_end["result"]}),
                            );
                        }
                    }
                    Some(GateKind::Assay) => {
                        let gate = st_decl.gate.as_ref().unwrap();
                        let assay_res = self.assay(json!({
                            "ritual": st_decl.ritual,
                            "version": st_decl.version,
                            "against": gate.against,
                            "cwd": "/",
                        }));
                        let accepted = assay_res
                            .as_ref()
                            .is_ok_and(|v| matches!(v["verdict"].as_str(), Some("within_tolerance" | "baseline")));
                        if accepted {
                            self.persisted(self.db.lock().unwrap().execute(
                                "UPDATE rite_stages SET gate_status='accepted', accepted=1 WHERE rite=?1 AND stage=?2",
                                params![id, sid],
                            ));
                            self.record_rite_event(&id, "gate_accepted", json!({"stage": sid, "gate": "assay"}));
                            progressed = true;
                        } else {
                            self.persisted(self.db.lock().unwrap().execute(
                                "UPDATE rite_stages SET gate_status='rejected', accepted=0 WHERE rite=?1 AND stage=?2",
                                params![id, sid],
                            ));
                            self.record_rite_event(&id, "gate_rejected", json!({"stage": sid, "gate": "assay"}));
                        }
                    }
                }
            }
        }

        // Update overall rite status.
        self.refresh_rite_status(&id)?;
        self.rite_show(json!({"rite": id}))
    }

    /// Called by `petition_decide_as` when the owner permits or denies a `rite.gate` petition.
    pub(super) fn decide_rite_gate(&self, pet: &Petition, permit: bool) -> Result<Value, RpcErr> {
        let rite_id = pet.arguments["rite"].as_str().unwrap_or("");
        let stage_id = pet.arguments["stage"].as_str().unwrap_or("");
        let now = now_ms();
        let (gate_status, accepted, pet_status) = if permit {
            ("accepted", 1i64, "applied")
        } else {
            ("rejected", 0i64, "denied")
        };
        let res_json = json!({"rite": rite_id, "stage": stage_id, "gate_status": gate_status});
        {
            let mut db = self.db.lock().unwrap();
            let tx = db.transaction().map_err(sql)?;
            tx.execute(
                "UPDATE petitions SET status=?2, decided=?3, result=?4 WHERE id=?1",
                params![pet.id, pet_status, now, res_json.to_string()],
            )
            .map_err(sql)?;
            tx.execute(
                "UPDATE rite_stages SET gate_status=?3, accepted=?4 WHERE rite=?1 AND stage=?2",
                params![rite_id, stage_id, gate_status, accepted],
            )
            .map_err(sql)?;
            tx.commit().map_err(sql)?;
        }
        self.record_rite_event(
            rite_id,
            if permit { "gate_accepted" } else { "gate_rejected" },
            json!({"stage": stage_id, "gate": "owner", "petition": pet.id}),
        );
        self.refresh_rite_status(rite_id)?;
        Ok(json!({
            "id": pet.id,
            "status": pet_status,
            "operation": pet.operation,
            "arguments": pet.arguments,
            "result": res_json,
        }))
    }

    pub(super) fn rite_falsify(&self, p: Value) -> Result<Value, RpcErr> {
        let p: AssumptionChangeArgs = parse(p)?;
        let id = self.resolve_rite_id(&p.rite)?;
        let (def, _, _) = self.load_rite_row(&id)?;
        if !def.assumptions.iter().any(|a| a.id == p.assumption) {
            return Err(RpcErr::not_found(format!(
                "rite `{id}` has no assumption `{}`",
                p.assumption
            )));
        }
        let reason = p
            .reason
            .unwrap_or_else(|| format!("assumption `{}` falsified", p.assumption));
        let affected = rite::affected_stages(&def, &p.assumption);
        let now = now_ms();
        let mut reopened = Vec::new();
        {
            let mut db = self.db.lock().unwrap();
            let tx = db.transaction().map_err(sql)?;
            tx.execute(
                "UPDATE rite_assumptions SET status='falsified', falsified_reason=?3, updated=?4 WHERE rite=?1 AND id=?2",
                params![id, p.assumption, reason, now],
            )
            .map_err(sql)?;
            for sid in &affected {
                let prev: Option<(String, Option<String>)> = tx
                    .query_row(
                        "SELECT status, petition_id FROM rite_stages WHERE rite=?1 AND stage=?2",
                        params![id, sid],
                        |r| Ok((r.get(0)?, r.get(1)?)),
                    )
                    .optional()
                    .map_err(sql)?;
                if let Some((prev_status, pet_id)) = prev {
                    if prev_status != "pending" {
                        tx.execute(
                            "UPDATE rite_stages SET status='reopened', gate_status='invalidated', accepted=0, reopened_reason=?3 WHERE rite=?1 AND stage=?2",
                            params![id, sid, reason],
                        )
                        .map_err(sql)?;
                        reopened.push(sid.clone());
                    }
                    if let Some(pid) = pet_id {
                        tx.execute(
                            "UPDATE petitions SET status='cancelled', decided=?2, result=?3 WHERE id=?1 AND status='pending'",
                            params![pid, now, json!({"reason": reason}).to_string()],
                        )
                        .map_err(sql)?;
                    }
                }
            }
            tx.execute(
                "UPDATE rites SET status='reopened', updated=?2 WHERE id=?1",
                params![id, now],
            )
            .map_err(sql)?;
            tx.commit().map_err(sql)?;
        }
        self.record_rite_event(
            &id,
            "assumption_falsified",
            json!({"assumption": p.assumption, "reason": reason, "reopened": reopened}),
        );
        Ok(json!({
            "rite": id,
            "assumption": p.assumption,
            "status": "falsified",
            "reason": reason,
            "reopened": reopened,
        }))
    }

    pub(super) fn rite_assert(&self, p: Value) -> Result<Value, RpcErr> {
        let p: AssumptionChangeArgs = parse(p)?;
        let id = self.resolve_rite_id(&p.rite)?;
        let now = now_ms();
        let n = self
            .db
            .lock()
            .unwrap()
            .execute(
                "UPDATE rite_assumptions SET status='held', falsified_reason=NULL, updated=?3 WHERE rite=?1 AND id=?2",
                params![id, p.assumption, now],
            )
            .map_err(sql)?;
        if n == 0 {
            return Err(RpcErr::not_found(format!(
                "rite `{id}` has no assumption `{}`",
                p.assumption
            )));
        }
        self.record_rite_event(
            &id,
            "assumption_asserted",
            json!({"assumption": p.assumption, "reason": p.reason}),
        );
        self.refresh_rite_status(&id)?;
        Ok(json!({
            "rite": id,
            "assumption": p.assumption,
            "status": "held",
        }))
    }

    pub(super) fn rite_list(&self) -> Result<Value, RpcErr> {
        let db = self.db.lock().unwrap();
        let mut st = db
            .prepare("SELECT id, name, hash, status, created, updated FROM rites ORDER BY created DESC")
            .map_err(sql)?;
        let items: Vec<Value> = st
            .query_map([], |r| {
                Ok(json!({
                    "id": r.get::<_, String>(0)?,
                    "name": r.get::<_, String>(1)?,
                    "hash": r.get::<_, String>(2)?,
                    "status": r.get::<_, String>(3)?,
                    "created": r.get::<_, i64>(4)?,
                    "updated": r.get::<_, i64>(5)?,
                }))
            })
            .map_err(sql)?
            .collect::<Result<_, _>>()
            .map_err(sql)?;
        Ok(Value::Array(items))
    }

    pub(super) fn rite_show(&self, p: Value) -> Result<Value, RpcErr> {
        let p: RiteIdArgs = parse(p)?;
        let id = self.resolve_rite_id(&p.rite)?;
        let (def, inputs, status) = self.load_rite_row(&id)?;
        let db = self.db.lock().unwrap();
        let mut st = db
            .prepare(
                "SELECT stage, ritual, version, hash, status, summon_id, output_json, gate_status, petition_id, accepted, reopened_reason
                 FROM rite_stages WHERE rite=?1",
            )
            .map_err(sql)?;
        let mut stages_by_id: BTreeMap<String, Value> = BTreeMap::new();
        let rows = st
            .query_map(params![id], |r| {
                let stage: String = r.get(0)?;
                let out_s: Option<String> = r.get(6)?;
                let output = out_s
                    .and_then(|s| serde_json::from_str::<Value>(&s).ok())
                    .unwrap_or(Value::Null);
                Ok((
                    stage.clone(),
                    json!({
                        "stage": stage,
                        "ritual": r.get::<_, String>(1)?,
                        "version": r.get::<_, u32>(2)?,
                        "hash": r.get::<_, String>(3)?,
                        "status": r.get::<_, String>(4)?,
                        "summon": r.get::<_, Option<String>>(5)?,
                        "output": output,
                        "gate_status": r.get::<_, String>(7)?,
                        "petition": r.get::<_, Option<String>>(8)?,
                        "accepted": r.get::<_, i64>(9)? != 0,
                        "reopened_reason": r.get::<_, Option<String>>(10)?,
                    }),
                ))
            })
            .map_err(sql)?;
        for item in rows {
            let (k, v) = item.map_err(sql)?;
            stages_by_id.insert(k, v);
        }
        let ordered_stages: Vec<Value> = def
            .stages
            .iter()
            .filter_map(|s| stages_by_id.get(&s.id).cloned())
            .collect();

        let mut ast = db
            .prepare(
                "SELECT id, statement, status, falsified_reason, updated FROM rite_assumptions WHERE rite=?1 ORDER BY id",
            )
            .map_err(sql)?;
        let assumptions: Vec<Value> = ast
            .query_map(params![id], |r| {
                Ok(json!({
                    "id": r.get::<_, String>(0)?,
                    "statement": r.get::<_, String>(1)?,
                    "status": r.get::<_, String>(2)?,
                    "reason": r.get::<_, Option<String>>(3)?,
                    "updated": r.get::<_, i64>(4)?,
                }))
            })
            .map_err(sql)?
            .collect::<Result<_, _>>()
            .map_err(sql)?;

        let mut view = json!({
            "id": id,
            "name": def.rite,
            "description": def.description,
            "status": status,
            "inputs": inputs,
            "assumptions": assumptions,
            "stages": ordered_stages,
        });
        if let Some(window) = def.window {
            view["window"] = json!(window);
        }
        Ok(view)
    }

    pub(super) fn rite_events(&self, p: Value) -> Result<Value, RpcErr> {
        let p: RiteIdArgs = parse(p)?;
        let id = self.resolve_rite_id(&p.rite)?;
        let db = self.db.lock().unwrap();
        let mut st = db
            .prepare("SELECT id, ts, kind, detail_json FROM rite_events WHERE rite=?1 ORDER BY id")
            .map_err(sql)?;
        let events: Vec<Value> = st
            .query_map(params![id], |r| {
                let d: String = r.get(3)?;
                Ok(json!({
                    "id": r.get::<_, i64>(0)?,
                    "ts": r.get::<_, i64>(1)?,
                    "kind": r.get::<_, String>(2)?,
                    "detail": serde_json::from_str::<Value>(&d).unwrap_or(Value::Null),
                }))
            })
            .map_err(sql)?
            .collect::<Result<_, _>>()
            .map_err(sql)?;
        Ok(json!({"rite": id, "events": events}))
    }

    fn rite_snapshot(&self, id: &str) -> Result<(BTreeMap<String, StageRow>, BTreeSet<String>), RpcErr> {
        let db = self.db.lock().unwrap();
        let mut st = db
            .prepare("SELECT stage, status, output_json, accepted FROM rite_stages WHERE rite=?1")
            .map_err(sql)?;
        let mut map = BTreeMap::new();
        for row in st
            .query_map(params![id], |r| {
                let out_s: Option<String> = r.get(2)?;
                Ok(StageRow {
                    stage: r.get(0)?,
                    status: r.get(1)?,
                    output: out_s.and_then(|s| serde_json::from_str(&s).ok()),
                    accepted: r.get::<_, i64>(3)? != 0,
                })
            })
            .map_err(sql)?
        {
            let r = row.map_err(sql)?;
            map.insert(r.stage.clone(), r);
        }
        let mut ast = db
            .prepare("SELECT id FROM rite_assumptions WHERE rite=?1 AND status='held'")
            .map_err(sql)?;
        let held: BTreeSet<String> = ast
            .query_map(params![id], |r| r.get::<_, String>(0))
            .map_err(sql)?
            .collect::<Result<_, _>>()
            .map_err(sql)?;
        Ok((map, held))
    }

    fn refresh_rite_status(&self, id: &str) -> Result<(), RpcErr> {
        let (map, _) = self.rite_snapshot(id)?;
        let new_status = if map.values().all(|s| s.status == "completed" && s.accepted) {
            "accepted"
        } else if map.values().any(|s| s.status == "reopened") {
            "reopened"
        } else if map.values().any(|s| s.status == "failed") {
            "failed"
        } else {
            "active"
        };
        self.persisted(self.db.lock().unwrap().execute(
            "UPDATE rites SET status=?2, updated=?3 WHERE id=?1",
            params![id, new_status, now_ms()],
        ));
        Ok(())
    }
}

struct StageRow {
    stage: String,
    status: String,
    output: Option<Value>,
    accepted: bool,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_core(dir: &std::path::Path) -> Arc<Core> {
        let paths = Paths {
            config: dir.join("config"),
            state: dir.join("state"),
            runtime: dir.join("run"),
        };
        std::fs::create_dir_all(&paths.config).unwrap();
        std::fs::create_dir_all(&paths.state).unwrap();
        std::fs::create_dir_all(&paths.runtime).unwrap();
        Core::open(paths).unwrap()
    }

    #[test]
    fn refused_stage_invocation_does_not_leave_rite_running() {
        let home = crate::core::test_home::Home::new("{}");
        home.inscribe_def(json!({
            "ritual":"requires-text", "inputs":{"text":"string"},
            "steps":[{"id":"clean","operation":"data.trim","arguments":{"text":"${inputs.text}"}}]
        }), false);
        let file = home.path("refused.yaml");
        std::fs::write(&file, "rite: refused
stages:
  - id: stage
    ritual: requires-text
").unwrap();
        assert!(home.call("rite_start", json!({"file":file})).is_err());
        let record = home.call("rite_show", json!({"rite":"refused"})).unwrap();
        assert_eq!(record["stages"][0]["status"], "failed");
        assert_eq!(record["status"], "failed");
    }

    #[test]
    fn rite_resolves_transitive_stage_output() {
        let home = crate::core::test_home::Home::new("{}");
        home.inscribe_def(json!({
            "ritual":"echo-stage", "inputs":{"text":"string"},
            "steps":[{"id":"clean","operation":"data.trim","arguments":{"text":"${inputs.text}"}}],
            "return":{"text":"${steps.clean.output}"}
        }), false);
        let file = home.path("transitive.yaml");
        std::fs::write(&file, r#"
rite: transitive
stages:
  - id: a
    ritual: echo-stage
    inputs: {text: hello}
  - id: b
    ritual: echo-stage
    after: [a]
    inputs: {text: intermediate}
  - id: c
    ritual: echo-stage
    after: [b]
    inputs: {text: "${stages.a.output.text}"}
"#).unwrap();
        let result = home.call("rite_start", json!({"file":file})).unwrap();
        assert_eq!(result["stages"][2]["output"]["text"], "hello");
    }

    #[test]
    fn local_test_lab_rites_produce_receipts() {
        use std::process::Command;

        let example = Path::new(env!("CARGO_MANIFEST_DIR")).join("examples/11-test-lab");
        let home = std::env::temp_dir().join(new_id("summonr-test-lab"));
        let setup = Command::new("python3")
            .arg("-B")
            .arg(example.join("prepare.py"))
            .arg(&home)
            .arg("--configure-only")
            .status()
            .unwrap();
        assert!(setup.success());
        let core = test_core(&home);
        for (name, effect) in [
            ("court-probe", "write"),
            ("court-hearing", "write"),
            ("report-draft", "read"),
            ("report-verify", "write"),
        ] {
            core.dispatch("inscribe", json!({"path":example.join("rituals").join(format!("{name}.yaml"))})).unwrap();
            core.dispatch("grant", json!({"ritual":name,"scopes":[{"reagent":"cmd","select":[effect]}]})).unwrap();
        }
        for (rite, expected, count) in [
            ("flake-court", "order_sensitivity_on_both_revisions", 8),
            ("report-to-test", "verified_fix", 2),
        ] {
            let run = core.dispatch("rite_start", json!({"file":example.join(format!("{rite}.rite.yaml"))})).unwrap();
            let stages = run["stages"].as_array().unwrap();
            assert_eq!(stages[1]["output"]["verdict"], expected, "{run}");
            let receipt = home.join("receipt.json");
            std::fs::write(&receipt, json!({"ok":true,"data":run}).to_string()).unwrap();
            let export = Command::new("python3")
                .arg(example.join("receipt.py"))
                .stdin(std::fs::File::open(receipt).unwrap())
                .output()
                .unwrap();
            assert!(export.status.success(), "{}", String::from_utf8_lossy(&export.stderr));
            let lines = String::from_utf8(export.stdout).unwrap();
            assert_eq!(lines.lines().count(), count);
            for line in lines.lines() {
                let row: Value = serde_json::from_str(line).unwrap();
                assert_eq!(row["rite_id"], run["id"]);
                assert_eq!(row["verdict"], expected);
            }
            if rite == "report-to-test" {
                let patch = home.join("draft.patch");
                std::fs::write(&patch, stages[1]["output"]["patch"].as_str().unwrap()).unwrap();
                assert!(Command::new("git").args(["apply", "--check"]).arg(patch)
                    .current_dir(home.join("fixture")).status().unwrap().success());
            }
        }
        drop(core);
        std::fs::remove_dir_all(&home).unwrap();
    }

    #[test]
    fn rite_gates_and_assumption_reopening() {
        let tmp = std::env::temp_dir().join(format!("summonr-rite-test-{}", new_id("t")));
        std::fs::create_dir_all(&tmp).unwrap();
        let core = test_core(&tmp.join("home"));

        // Inscribe a pure stage ritual and a pure touchstone gate ritual.
        let stage_r = tmp.join("echo-stage.yaml");
        std::fs::write(
            &stage_r,
            "ritual: echo-stage\ninputs:\n  text: {type: string}\nessence: {requests: 0, time: 1m}\nsteps:\n  - id: s1\n    operation: data.trim\n    arguments: {text: \"${inputs.text}\"}\nreturn:\n  out: \"${steps.s1.output}\"\n",
        )
        .unwrap();
        core.inscribe(json!({"path": stage_r.to_str().unwrap()})).unwrap();

        let ts_r = tmp.join("check-ok.yaml");
        std::fs::write(
            &ts_r,
            "ritual: check-ok\ninputs:\n  output: {type: object}\nessence: {requests: 0, time: 1m}\nsteps:\n  - id: s1\n    operation: data.trim\n    arguments: {text: \"ok\"}\nreturn:\n  pass: true\n",
        )
        .unwrap();
        core.inscribe(json!({"path": ts_r.to_str().unwrap()})).unwrap();

        let rite_file = tmp.join("release.yaml");
        std::fs::write(
            &rite_file,
            r#"
rite: release-prep
window: {title: Release desk, by: release-team}
inputs:
  msg: {type: string}
assumptions:
  - id: base_ok
    statement: "base assumptions hold"
stages:
  - id: s1
    ritual: echo-stage
    inputs: {text: "${inputs.msg}"}
    assumes: [base_ok]
    gate:
      type: touchstone
      ritual: check-ok
  - id: s2
    ritual: echo-stage
    after: [s1]
    inputs: {text: "${stages.s1.output.out}"}
    gate:
      type: owner
  - id: s3
    ritual: echo-stage
    after: [s2]
    inputs: {text: "${stages.s2.output.out}"}
"#,
        )
        .unwrap();

        let started = core
            .rite_start(json!({"file": rite_file.to_str().unwrap(), "args": {"msg": "  hi  "}}))
            .unwrap();
        assert_eq!(started["window"]["title"], "Release desk");
        let rite_id = started["id"].as_str().unwrap().to_string();
        // s1 completed & accepted by touchstone; s2 completed & waiting on owner petition; s3 still pending.
        let stages = started["stages"].as_array().unwrap();
        assert_eq!(stages[0]["status"], "completed");
        assert_eq!(stages[0]["accepted"], true);
        assert_eq!(stages[1]["status"], "completed");
        assert_eq!(stages[1]["accepted"], false);
        assert_eq!(stages[1]["gate_status"], "pending_owner");
        assert_eq!(stages[2]["status"], "pending");

        // Owner permits the s2 gate petition -> s2 becomes accepted, then advance completes s3.
        let pid = stages[1]["petition"].as_str().unwrap().to_string();
        core.petition_decide(json!({"ids": [pid], "permit": true}))
            .unwrap();
        let after = core.rite_advance(json!({"rite": rite_id})).unwrap();
        assert_eq!(after["status"], "accepted");
        assert_eq!(after["stages"][2]["accepted"], true);

        // Falsifying `base_ok` reopens s1 AND transitive dependents s2, s3 without auto-running.
        let fals = core
            .rite_falsify(json!({"rite": rite_id, "assumption": "base_ok", "reason": "broken"}))
            .unwrap();
        assert_eq!(fals["reopened"].as_array().unwrap().len(), 3);
        let shown = core.rite_show(json!({"rite": rite_id})).unwrap();
        assert_eq!(shown["status"], "reopened");
        for st in shown["stages"].as_array().unwrap() {
            assert_eq!(st["status"], "reopened");
            assert_eq!(st["accepted"], false);
        }

        // Re-assert assumption and advance again -> s1 runs & passes touchstone, s2 holds at owner gate.
        core.rite_assert(json!({"rite": rite_id, "assumption": "base_ok"}))
            .unwrap();
        let resumed = core.rite_advance(json!({"rite": rite_id})).unwrap();
        assert_eq!(resumed["stages"][0]["accepted"], true);
        assert_eq!(resumed["stages"][1]["gate_status"], "pending_owner");
        assert_eq!(resumed["stages"][2]["status"], "reopened");

        let _ = std::fs::remove_dir_all(&tmp);
    }
}
