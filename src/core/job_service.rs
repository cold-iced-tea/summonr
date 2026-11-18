//! job service service.
use super::*;

impl Core {
    pub(super) fn job_create(&self, p: Value) -> Result<Value, RpcErr> {
        if p.to_string().len() > 8 * 1024 * 1024 {
            return Err(RpcErr::invalid("job manifest exceeds 8 MiB"));
        }
        let manifest: jobs::Manifest = decode(p)?;
        let (version, hash, def) = self
            .load_ritual(&manifest.ritual, manifest.version)?
            .ok_or_else(|| RpcErr::not_found("job ritual must be saved first"))?;
        let effects =
            ritual::validate(&def, &self.resolver(), &reagent::tool, self.cfg.max_depth())
                .map_err(|e| RpcErr::invalid(e.join("; ")))?;
        job_effects(&effects)?;
        if def.essence.requests.is_none()
            || def.essence.time.is_none()
            || (def.essence.requests != Some(0) && def.essence.tokens.is_none())
        {
            return Err(RpcErr::invalid(
                "job ritual needs explicit requests and time limits, plus tokens when requests > 0",
            ));
        }
        for item in &manifest.items {
            ritual::resolve_inputs(&def, &item.args).map_err(RpcErr::invalid)?;
        }
        let spec = json!({"target":manifest.ritual,"version":version,"expected_hash":hash});
        jobs::create(&mut self.db.lock().unwrap(), &manifest, &spec, now_ms())
    }

    pub(super) fn job_work(self: &Arc<Self>, p: Value) -> Result<Value, RpcErr> {
        let p: requests::JobWork = decode(p)?;
        let job = p.job.as_str();
        let count = p.count;
        let lease = p.lease_ms;
        let owner = new_id("worker");
        let (spec, claims) = {
            let _admission = self.admission.lock().unwrap();
            if self.active.lock().unwrap().len() >= self.cfg.max_active_runs.unwrap_or(16) {
                return Err(RpcErr::conflict("active run limit reached"));
            }
            let mut conn = self.db.lock().unwrap();
            let spec = jobs::spec(&conn, job)?;
            let claims = jobs::claim(
                &mut conn,
                job,
                &owner,
                count as usize,
                lease as i64,
                now_ms(),
            )?;
            (spec, claims)
        };
        let mut records = Vec::new();
        for claim in claims {
            let execution = (|| -> Result<Value, RpcErr> {
                let run = {
                    let _admission = self.admission.lock().unwrap();
                    self.storage_health()?;
                    jobs::check(&self.db.lock().unwrap(), &claim, now_ms())?;
                    if self.active.lock().unwrap().len() >= self.cfg.max_active_runs.unwrap_or(16) {
                        return Err(RpcErr::conflict("active run limit reached"));
                    }
                    let mut spec: Spec = serde_json::from_value(spec.clone())
                        .map_err(|e| RpcErr::invalid(e.to_string()))?;
                    spec.args = claim.args.clone();
                    spec.limits.time = Some(format!(
                        "{}ms",
                        claim.expires.saturating_sub(now_ms()).max(0)
                    ));
                    let resolved = self.resolve(&spec)?;
                    job_effects(&resolved.effects)?;
                    Self::check_authority(&resolved)?;
                    jobs::check(&self.db.lock().unwrap(), &claim, now_ms())?;
                    let (id, mut run, _) = self.make_run(resolved, None, None, None);
                    run.job_claim = Some(claim.clone());
                    self.emit(&id,"job.claim","claimed shared job item",json!({"job":claim.job,"item":claim.item,"version":claim.version,"owner":claim.owner,"generation":claim.generation,"expires":claim.expires}));
                    let attached = jobs::attach(&self.db.lock().unwrap(), &claim, &id, now_ms());
                    if let Err(e) = attached {
                        self.active.lock().unwrap().remove(&id);
                        // The run has not dispatched any work. Preserve that in its terminal record.
                        let fin = Finished {
                            outcome: Outcome::Failed,
                            result: Value::Null,
                            error: Some(e.message.clone()),
                        };
                        self.finish_summon(&id, &fin, json!([]));
                        return Err(e);
                    }
                    self.storage_health()?;
                    run
                };
                let id = run.id.clone();
                let fin = self.complete(run, false);
                self.storage_health()?;
                Ok(json!({"summon":id,"outcome":fin.outcome,"result":fin.result,"error":fin.error}))
            })();
            let record = match execution {
                Ok(v) => v,
                Err(e) => json!({"outcome":"failed","error":e.message,"code":e.code}),
            };
            let committed = jobs::finish(
                &mut self.db.lock().unwrap(),
                &claim,
                record["outcome"].as_str().unwrap_or("failed"),
                &record,
                now_ms(),
            )?;
            records.push(json!({"item":claim.item,"generation":claim.generation,"committed":committed,"record":record}));
        }
        Ok(json!({"job":job,"worker":owner,"attempted":records.len(),"items":records}))
    }
}
