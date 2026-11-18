//! Experiments: parameterized `sweep`, `judge`, and `weigh` over saved rituals.
use super::*;
use crate::io::{Io, Live, Simulate};
use std::time::Instant;

fn subset_matches(actual: &Value, expected: &Value) -> bool {
    match (actual, expected) {
        (Value::Object(a), Value::Object(e)) => {
            e.iter().all(|(k, v)| a.get(k).is_some_and(|av| subset_matches(av, v)))
        }
        _ => actual == expected,
    }
}

impl Core {
    pub(super) fn experiment_sweep(self: &Arc<Self>, p: Value) -> Result<Value, RpcErr> {
        let req: requests::Sweep = decode(p)?;
        let (version, _, _) = self
            .load_ritual(&req.ritual, req.version)?
            .ok_or_else(|| RpcErr::not_found(format!("no ritual `{}` in the grimoire", req.ritual)))?;
        let models: Vec<Option<String>> = if req.models.is_empty() {
            vec![None]
        } else {
            req.models.into_iter().map(Some).collect()
        };
        let settings_list: Vec<BTreeMap<String, String>> = if req.settings.is_empty() {
            vec![BTreeMap::new()]
        } else {
            req.settings
        };
        let cases: Vec<(String, Map<String, Value>, Option<Value>)> = if req.cases.is_empty() {
            vec![("default".into(), Map::new(), None)]
        } else {
            req.cases
                .into_iter()
                .enumerate()
                .map(|(i, c)| {
                    (
                        c.id.unwrap_or_else(|| format!("case_{}", i + 1)),
                        c.inputs,
                        c.expect,
                    )
                })
                .collect()
        };

        let exp_id = new_id("exp");
        let mut trials = Vec::new();
        for (mi, model_opt) in models.iter().enumerate() {
            for (si, st) in settings_list.iter().enumerate() {
                let variant = format!(
                    "{}#{}",
                    model_opt.as_deref().unwrap_or("default"),
                    si
                );
                for (case_id, inputs, expect) in &cases {
                    let spec = Spec {
                        target: req.ritual.clone(),
                        version: Some(version),
                        model: model_opt.clone(),
                        settings: st.clone(),
                        args: inputs.clone(),
                        simulate: !req.live,
                        ..Default::default()
                    };
                    let r = self.resolve(&spec)?;
                    Self::check_authority(&r)?;
                    let io: Option<Arc<dyn Io>> = if !req.live {
                        Some(Arc::new(Simulate::new(Arc::new(Live))))
                    } else {
                        None
                    };
                    let t0 = Instant::now();
                    let (summon_id, run, _) = {
                        let _admission = self.admission.lock().unwrap();
                        self.admit()?;
                        self.make_run(r, model_opt.clone(), Some(&exp_id), io)
                    };
                    let fin = self.complete(run, false);
                    let duration_ms = t0.elapsed().as_millis() as u64;
                    let rec = self.summon(&summon_id)?;
                    trials.push(json!({
                        "trial_index": trials.len() + 1,
                        "variant": variant,
                        "variant_index": mi * settings_list.len() + si,
                        "model": rec["model"],
                        "settings": st,
                        "case_id": case_id,
                        "inputs": inputs,
                        "expect": expect,
                        "summon": summon_id,
                        "outcome": fin.outcome,
                        "result": fin.result,
                        "error": fin.error,
                        "requests": rec["requests"],
                        "tokens": rec["tokens"],
                        "duration_ms": duration_ms,
                    }));
                }
            }
        }
        let trials_val = Value::Array(trials.clone());
        let spec_val = json!({"ritual": req.ritual, "version": version, "live": req.live});
        self.db
            .lock()
            .unwrap()
            .execute(
                "INSERT INTO experiments (id, ritual, spec, trials, created) VALUES (?1, ?2, ?3, ?4, ?5)",
                params![exp_id, req.ritual, spec_val.to_string(), trials_val.to_string(), now_ms()],
            )
            .map_err(storage_error)?;
        Ok(json!({
            "experiment": exp_id,
            "ritual": req.ritual,
            "version": version,
            "trial_count": trials.len(),
            "trials": trials,
        }))
    }

    fn load_experiment(&self, id: &str) -> Result<(String, Value, Vec<Value>, Option<Value>), RpcErr> {
        let db = self.db.lock().unwrap();
        let row = db
            .query_row(
                "SELECT ritual, spec, trials, judged FROM experiments WHERE id=?1",
                params![id],
                |r| {
                    Ok((
                        r.get::<_, String>(0)?,
                        r.get::<_, String>(1)?,
                        r.get::<_, String>(2)?,
                        r.get::<_, Option<String>>(3)?,
                    ))
                },
            )
            .optional()
            .map_err(storage_error)?
            .ok_or_else(|| RpcErr::not_found(format!("no experiment `{id}`")))?;
        let spec: Value = serde_json::from_str(&row.1)
            .map_err(|e| RpcErr::new("storage_failed", 11, e.to_string()))?;
        let trials: Vec<Value> = serde_json::from_str(&row.2)
            .map_err(|e| RpcErr::new("storage_failed", 11, e.to_string()))?;
        let judged: Option<Value> = row
            .3
            .as_deref()
            .map(serde_json::from_str)
            .transpose()
            .map_err(|e| RpcErr::new("storage_failed", 11, e.to_string()))?;
        Ok((row.0, spec, trials, judged))
    }

    pub(super) fn experiment_judge(self: &Arc<Self>, p: Value) -> Result<Value, RpcErr> {
        let req: requests::Judge = decode(p)?;
        let (ritual_name, spec, trials, _) = self.load_experiment(&req.experiment)?;
        let version = spec["version"].as_u64().unwrap_or(1) as u32;
        let touchstone = match &req.touchstone {
            Some(t) => Some(self.pin_touchstone(t, &ritual_name, version)?),
            None => None,
        };
        let mut scored = Vec::with_capacity(trials.len());
        let mut by_variant: BTreeMap<String, Vec<(bool, u64, u64, u64)>> = BTreeMap::new();
        for t in &trials {
            let outcome = t["outcome"].as_str().unwrap_or("");
            let variant = t["variant"].as_str().unwrap_or("default").to_string();
            let (passed, reason) = if outcome != "returned" {
                (false, format!("outcome was `{outcome}`"))
            } else if let Some(ref ts) = touchstone {
                let inputs: Map<String, Value> =
                    serde_json::from_value(t["inputs"].clone()).unwrap_or_default();
                match self.run_touchstone(ts, &inputs, &req.experiment) {
                    crate::touchstone::Expected::Value(expected) => {
                        if subset_matches(&t["result"], &expected) {
                            (true, "matched touchstone".into())
                        } else {
                            (false, format!("result {} != touchstone {}", t["result"], expected))
                        }
                    }
                    crate::touchstone::Expected::TouchstoneError(e) => (false, format!("touchstone error: {e}")),
                }
            } else if !t["expect"].is_null() {
                if subset_matches(&t["result"], &t["expect"]) {
                    (true, "matched expect".into())
                } else {
                    (false, format!("result {} != expected {}", t["result"], t["expect"]))
                }
            } else {
                (true, "returned".into())
            };
            let reqs = t["requests"].as_u64().unwrap_or(0);
            let toks = t["tokens"].as_u64().unwrap_or(0);
            let dur = t["duration_ms"].as_u64().unwrap_or(0);
            by_variant
                .entry(variant.clone())
                .or_default()
                .push((passed, reqs, toks, dur));
            let mut row = t.clone();
            row["passed"] = json!(passed);
            row["reason"] = json!(reason);
            scored.push(row);
        }
        let mut variants = Vec::new();
        for (variant, items) in &by_variant {
            let total = items.len() as u64;
            let passed = items.iter().filter(|x| x.0).count() as u64;
            let total_reqs: u64 = items.iter().map(|x| x.1).sum();
            let total_toks: u64 = items.iter().map(|x| x.2).sum();
            let total_dur: u64 = items.iter().map(|x| x.3).sum();
            variants.push(json!({
                "variant": variant,
                "passed": passed,
                "total": total,
                "pass_rate": (passed as f64) / (total.max(1) as f64),
                "mean_requests": (total_reqs as f64) / (total.max(1) as f64),
                "mean_tokens": (total_toks as f64) / (total.max(1) as f64),
                "mean_duration_ms": (total_dur as f64) / (total.max(1) as f64),
            }));
        }
        let total_passed = scored.iter().filter(|t| t["passed"] == true).count();
        let judged = json!({
            "experiment": req.experiment,
            "ritual": ritual_name,
            "touchstone": req.touchstone,
            "passed": total_passed,
            "total": scored.len(),
            "variants": variants,
            "trials": scored,
        });
        self.db
            .lock()
            .unwrap()
            .execute(
                "UPDATE experiments SET judged=?2 WHERE id=?1",
                params![req.experiment, judged.to_string()],
            )
            .map_err(storage_error)?;
        Ok(judged)
    }

    pub(super) fn experiment_weigh(self: &Arc<Self>, p: Value) -> Result<Value, RpcErr> {
        let req: requests::Weigh = decode(p)?;
        let ensure_judged = |id: &str| -> Result<Value, RpcErr> {
            let (_, _, _, judged) = self.load_experiment(id)?;
            match judged {
                Some(j) => Ok(j),
                None => self.experiment_judge(json!({"experiment": id})),
            }
        };
        let a = ensure_judged(&req.experiment)?;
        if let Some(other_id) = &req.against {
            let b = ensure_judged(other_id)?;
            let rate = |j: &Value| {
                let t = j["total"].as_u64().unwrap_or(1).max(1) as f64;
                j["passed"].as_u64().unwrap_or(0) as f64 / t
            };
            let mean_trial = |j: &Value, field: &str| {
                let arr = j["trials"].as_array().map(Vec::as_slice).unwrap_or(&[]);
                let n = arr.len().max(1) as f64;
                arr.iter().filter_map(|x| x[field].as_u64()).sum::<u64>() as f64 / n
            };
            let ra = rate(&a);
            let rb = rate(&b);
            let ta = mean_trial(&a, "tokens");
            let tb = mean_trial(&b, "tokens");
            let da = mean_trial(&a, "duration_ms");
            let db = mean_trial(&b, "duration_ms");
            let winner = if (ra - rb).abs() > 1e-9 {
                if ra > rb { &req.experiment } else { other_id }
            } else if (ta - tb).abs() > 1e-9 {
                if ta < tb { &req.experiment } else { other_id }
            } else if da <= db {
                &req.experiment
            } else {
                other_id
            };
            return Ok(json!({
                "a": {"experiment": req.experiment, "pass_rate": ra, "mean_tokens": ta, "mean_duration_ms": da},
                "b": {"experiment": other_id, "pass_rate": rb, "mean_tokens": tb, "mean_duration_ms": db},
                "pass_rate_delta": ra - rb,
                "mean_tokens_delta": ta - tb,
                "mean_duration_ms_delta": da - db,
                "winner": winner,
            }));
        }
        let mut variants = a["variants"].as_array().cloned().unwrap_or_default();
        variants.sort_by(|x, y| {
            let rx = x["pass_rate"].as_f64().unwrap_or(0.0);
            let ry = y["pass_rate"].as_f64().unwrap_or(0.0);
            ry.partial_cmp(&rx)
                .unwrap_or(std::cmp::Ordering::Equal)
                .then_with(|| {
                    let tx = x["mean_tokens"].as_f64().unwrap_or(0.0);
                    let ty = y["mean_tokens"].as_f64().unwrap_or(0.0);
                    tx.partial_cmp(&ty).unwrap_or(std::cmp::Ordering::Equal)
                })
                .then_with(|| {
                    let dx = x["mean_duration_ms"].as_f64().unwrap_or(0.0);
                    let dy = y["mean_duration_ms"].as_f64().unwrap_or(0.0);
                    dx.partial_cmp(&dy).unwrap_or(std::cmp::Ordering::Equal)
                })
        });
        let winner = variants.first().and_then(|v| v["variant"].as_str()).map(str::to_string);
        Ok(json!({
            "experiment": req.experiment,
            "winner": winner,
            "ranked": variants,
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::test_home::Home;

    #[test]
    fn sweep_judge_and_weigh_rank_variants_and_compare_experiments() {
        let h = Home::new("");
        let r_path = h.root.join("add.yaml");
        std::fs::write(
            &r_path,
            r#"
ritual: add-exp
inputs: {a: integer, b: integer}
reagents:
  data: {select: [arithmetic]}
essence: {requests: 0, tool_calls: 2}
steps:
  - id: sum
    operation: data.arithmetic
    arguments: {left: "${inputs.a}", right: "${inputs.b}", operator: add}
return: {out: "${steps.sum.output}"}
"#,
        )
        .unwrap();
        h.core
            .dispatch("inscribe", json!({"path": r_path.to_str().unwrap()}))
            .unwrap();

        let exp1 = h
            .core
            .dispatch(
                "experiment_sweep",
                json!({
                    "ritual": "add-exp",
                    "models": ["mock/keyword"],
                    "settings": [{"temperature": "0.1"}, {"temperature": "0.5"}],
                    "cases": [
                        {"id": "c1", "inputs": {"a": 2, "b": 3}, "expect": {"out": 5}},
                        {"id": "c2", "inputs": {"a": 10, "b": 4}, "expect": {"out": 14}},
                    ]
                }),
            )
            .unwrap();
        let eid1 = exp1["experiment"].as_str().unwrap().to_string();
        assert_eq!(exp1["trial_count"], 4);

        let judged = h
            .core
            .dispatch("experiment_judge", json!({"experiment": eid1}))
            .unwrap();
        assert_eq!(judged["passed"], 4);
        assert_eq!(judged["total"], 4);

        let weighed = h
            .core
            .dispatch("experiment_weigh", json!({"experiment": eid1}))
            .unwrap();
        assert!(weighed["winner"].is_string());
        assert_eq!(weighed["ranked"].as_array().unwrap().len(), 2);

        // Compare against a second experiment with a failing case
        let exp2 = h
            .core
            .dispatch(
                "experiment_sweep",
                json!({
                    "ritual": "add-exp",
                    "cases": [{"id": "c1", "inputs": {"a": 2, "b": 3}, "expect": {"out": 999}}]
                }),
            )
            .unwrap();
        let eid2 = exp2["experiment"].as_str().unwrap().to_string();
        let cmp = h
            .core
            .dispatch("experiment_weigh", json!({"experiment": eid1, "against": eid2}))
            .unwrap();
        assert_eq!(cmp["winner"], eid1);
        assert_eq!(cmp["pass_rate_delta"], 1.0);
    }
}
