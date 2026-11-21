//! assays service.
use super::*;
use crate::touchstone::Expected;

impl Core {
    pub(super) fn assay(self: &Arc<Self>, p: Value) -> Result<Value, RpcErr> {
        let p: requests::Assay = decode(p)?;
        let name = p.ritual.clone();
        let (version, _, def) = self.load_ritual(&name, p.version)?.ok_or_else(|| {
            RpcErr::not_found(format!(
                "no ritual `{name}` in the grimoire; assays run against saved rituals"
            ))
        })?;
        let record = p.baseline;
        let prior = self.baseline(&name, version)?;
        let model_override = p.model.clone();

        let set_json = match p.against.as_deref() {
            Some(path) => {
                let full = PathBuf::from(&p.cwd).join(path);
                let text = std::fs::read_to_string(&full).map_err(|e| RpcErr::not_found(format!("cannot read {}: {e}", full.display())))?;
                let v: Value = serde_yaml::from_str(&text).map_err(|e| RpcErr::invalid(format!("{}: {e}", full.display())))?;
                v.to_string()
            }
            None => match &prior {
                Some(b) => b["set_json"].as_str().unwrap_or("").to_string(),
                None => {
                    return Err(RpcErr::conflict(format!(
                        "ritual `{name}` version {version} has no baseline; record one with `summonr assay {name} against SET baseline`"
                    )))
                }
            },
        };
        let set: AssaySet = serde_json::from_str(&set_json)
            .map_err(|e| RpcErr::invalid(format!("assay set: {e}")))?;
        let set_hash = sha256_hex(set_json.as_bytes());
        if set.cases.is_empty() {
            return Err(RpcErr::invalid("the assay set has no cases"));
        }
        let touchstone = match &set.touchstone {
            Some(t) => {
                if let Some(c) = set.cases.iter().find(|c| !c.expect.is_empty()) {
                    return Err(RpcErr::invalid(format!(
                        "case `{}` has a hand-written expect, but the set names touchstone `{t}`; use one or the other",
                        c.id
                    )));
                }
                Some(self.pin_touchstone(t, &name, version)?)
            }
            None => None,
        };
        // A gleaned case counts only once a person, or a touchstone, has said
        // what is correct.
        let unlabeled: Vec<&str> = set
            .cases
            .iter()
            .filter(|c| touchstone.is_none() && c.expect.is_empty())
            .map(|c| c.id.as_str())
            .collect();
        if !unlabeled.is_empty() {
            return Err(RpcErr::invalid(format!(
                "{} case(s) have an empty `expect` and cannot be scored: {}. Fill in the expected values first.",
                unlabeled.len(),
                unlabeled.join(", ")
            )));
        }

        struct UnloadGuard<'a> {
            cfg: &'a crate::util::Config,
            models: BTreeSet<String>,
        }
        impl Drop for UnloadGuard<'_> {
            fn drop(&mut self) {
                for m in &self.models {
                    backend::unload(&self.cfg.backend_url_for(m), m);
                }
            }
        }
        let series = new_id("series");
        let mut trials = Vec::new();
        let mut unload_guard = UnloadGuard {
            cfg: &self.cfg,
            models: BTreeSet::new(),
        };
        let (mut correct, mut abstained, mut invalid, mut failed, mut tokens, mut ms) =
            (0usize, 0usize, 0usize, 0usize, 0i64, 0i64);
        let mut touchstone_errors = 0usize;
        for case in &set.cases {
            let spec = Spec {
                target: name.clone(),
                version: Some(version),
                model: model_override.clone(),
                args: case.inputs.clone(),
                ..Default::default()
            };
            let mut r = self.resolve(&spec)?;
            // A refuse-on-mismatch policy must not block the assay that clears it.
            r.refuse = None;
            r.warnings.clear();
            Self::check_authority(&r)?;
            // An assay is a measurement: its writes are simulated unless the
            // user asked for a live assay. Admission is the same as `invoke`.
            let io: Arc<dyn crate::io::Io> = if p.live {
                Arc::new(crate::io::Live)
            } else {
                Arc::new(crate::io::Simulate::new(Arc::new(crate::io::Live)))
            };
            let (id, run, _) = {
                let _admission = self.admission.lock().unwrap();
                self.admit()?;
                self.make_run(r, model_override.clone(), Some(&series), Some(io))
            };
            let used = run.models_used.clone();
            let fin = self.complete(run, true);
            unload_guard.models.extend(used.lock().unwrap().iter().cloned());
            let s = self.summon(&id)?;
            let took = s["ended"].as_i64().unwrap_or(0) - s["started"].as_i64().unwrap_or(0);
            let trial_tokens = s["tokens"].as_i64().unwrap_or(0);
            tokens += trial_tokens;
            ms += took;
            let expect = match touchstone.as_ref().map(|t| self.run_touchstone(t, &case.inputs, &series)) {
                None => case.expect.clone(),
                Some(Expected::Value(Value::Object(m))) => m,
                Some(other) => {
                    // No expected answer: this case says nothing about the ritual.
                    let err = match other {
                        Expected::TouchstoneError(e) => e,
                        Expected::Value(v) => format!("touchstone returned {v}, not an object"),
                    };
                    touchstone_errors += 1;
                    trials.push(json!({"case": case.id, "summon": id, "outcome": fin.outcome, "touchstone_error": err, "got": fin.result, "ms": took}));
                    continue;
                }
            };
            // An abstention the case expects is a correct answer; abstentions
            // are still counted separately so their rate can be tracked.
            let decided = matches!(fin.outcome, Outcome::Returned | Outcome::Abstained);
            let matches = decided
                && expect.iter().all(|(k, want)| {
                    let path: Vec<String> = k.split('.').map(String::from).collect();
                    ritual::descend(fin.result.clone(), &path, &path)
                        .map(|got| &got == want)
                        .unwrap_or(false)
                });
            if matches {
                correct += 1;
            }
            match fin.outcome {
                Outcome::Returned => {}
                Outcome::Abstained => abstained += 1,
                Outcome::Invalid => invalid += 1,
                _ => failed += 1,
            }
            trials.push(json!({"case": case.id, "summon": id, "outcome": fin.outcome, "correct": matches, "expected": expect, "got": fin.result, "error": fin.error, "ms": took, "tokens": trial_tokens}));
        }
        drop(unload_guard);

        let n = set.cases.len();
        let (lo, hi) = wilson(correct, n);
        let account = json!({
            "cases": n, "correct": correct, "wrong": n - correct - invalid - failed - touchstone_errors,
            "touchstone_errors": touchstone_errors,
            "abstained": abstained, "invalid": invalid, "failed": failed,
            "accuracy": correct as f64 / n as f64, "accuracy_interval_95": [lo, hi],
            "abstention_rate": abstained as f64 / n as f64,
            "mean_ms": ms / n as i64, "tokens": tokens,
        });
        let effective_model = model_override.clone().or_else(|| def.model.clone());
        let sites = ritual::model_sites(&def, &self.resolver(), model_override.as_deref());
        let fingerprint = self.full_fingerprint(&sites);
        let mode = if p.live { "live" } else { "simulated" };
        let pinned = touchstone
            .as_ref()
            .map(|t| json!({"name": t.name, "version": t.version, "hash": t.hash}));
        let mut out = json!({
            "series": series, "ritual": name, "version": version, "model": effective_model, "mode": mode,
            "touchstone": pinned,
            "set": {"name": set.assay_set, "hash": set_hash, "cases": n},
            "account": account, "fingerprint": fingerprint, "trials": trials,
        });
        let _ = self.db.lock().unwrap().execute(
            "INSERT OR REPLACE INTO assay_trials (series, ritual, trials, created) VALUES (?1, ?2, ?3, ?4)",
            params![series, name, out["trials"].to_string(), now_ms()],
        );

        if record && touchstone_errors > 0 {
            // A baseline over unscored cases would distort every later verdict.
            return Err(RpcErr::invalid(format!(
                "the touchstone could not answer {touchstone_errors} case(s); fix the touchstone or those cases before recording a baseline. Nothing was recorded."
            )));
        }
        if record {
            self.db
                .lock()
                .unwrap()
                .execute(
                    "INSERT OR REPLACE INTO baselines (ritual, version, model, set_name, set_hash, set_json, fingerprint, account, created, mode, touchstone) VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11)",
                    params![name, version, effective_model, set.assay_set, set_hash, set_json, fingerprint.to_string(), account.to_string(), now_ms(), mode, pinned.as_ref().map(Value::to_string)],
                )
                .map_err(storage_error)?;
            out["baseline"] = json!("recorded");
        } else if let Some(base) = prior {
            let tol_acc = set.tolerances.accuracy_drop.unwrap_or(0.05);
            let tol_abs = set.tolerances.abstention_rise.unwrap_or(0.10);
            let min_cases = set.min_cases.unwrap_or(20);
            let acc_drop = base["account"]["accuracy"].as_f64().unwrap_or(0.0)
                - account["accuracy"].as_f64().unwrap_or(0.0);
            let abs_rise = account["abstention_rate"].as_f64().unwrap_or(0.0)
                - base["account"]["abstention_rate"].as_f64().unwrap_or(0.0);
            let mut reasons = Vec::new();
            let verdict = if base["mode"] != json!(mode) {
                reasons.push(format!(
                    "the baseline was recorded in {} mode; this assay ran in {mode} mode",
                    base["mode"].as_str().unwrap_or("live")
                ));
                "inconclusive"
            } else if base["touchstone"] != pinned.clone().unwrap_or(Value::Null) {
                reasons.push(
                    "the touchstone changed since the baseline; its expectations are not the same measurement. Re-record the baseline"
                        .to_string(),
                );
                "inconclusive"
            } else if touchstone_errors > 0 {
                reasons.push(format!(
                    "the touchstone could not answer {touchstone_errors} case(s); their correctness is unknown"
                ));
                "inconclusive"
            } else if failed > 0 {
                reasons.push(format!(
                    "{failed} of {n} case(s) failed to run; check backend availability"
                ));
                "inconclusive"
            } else if base["fingerprint"].get("sites").is_none() {
                reasons.push(
                    "the baseline predates fingerprints that cover every model site; re-record it"
                        .to_string(),
                );
                "inconclusive"
            } else if base["set_hash"] != json!(set_hash) {
                reasons.push(
                    "the assay set differs from the one the baseline was recorded with".to_string(),
                );
                "inconclusive"
            } else if n < min_cases {
                reasons.push(format!("{n} cases is fewer than the {min_cases} this set requires to detect its declared tolerance"));
                "inconclusive"
            } else if acc_drop > tol_acc || abs_rise > tol_abs {
                if acc_drop > tol_acc {
                    reasons.push(format!(
                        "accuracy fell by {:.1} points; tolerance is {:.1}",
                        acc_drop * 100.0,
                        tol_acc * 100.0
                    ));
                }
                if abs_rise > tol_abs {
                    reasons.push(format!(
                        "abstention rose by {:.1} points; tolerance is {:.1}",
                        abs_rise * 100.0,
                        tol_abs * 100.0
                    ));
                }
                "drifted"
            } else {
                "within tolerance"
            };
            out["drift"] = json!({
                "verdict": verdict, "reasons": reasons,
                "accuracy_change": -acc_drop, "abstention_change": abs_rise,
                "tolerances": {"accuracy_drop": tol_acc, "abstention_rise": tol_abs},
                "fingerprint_changed": base["fingerprint"] != fingerprint,
                "baseline": {"account": base["account"], "fingerprint": base["fingerprint"], "recorded": base["created"]},
                "note": "Nothing was repaired, switched, or re-baselined. Recording a new baseline is an explicit act.",
            });
        }
        Ok(out)
    }

    pub(super) fn glean(&self, p: Value) -> Result<Value, RpcErr> {
        let p: requests::Glean = decode(p)?;
        let name = p.ritual.as_str();
        if self.load_ritual(name, None)?.is_none() {
            return Err(RpcErr::not_found(format!(
                "no ritual `{name}` in the grimoire"
            )));
        }
        let count = p.count as usize;
        let mut known: HashSet<String> = HashSet::new();
        if let Some(path) = p.against.as_deref() {
            let full = PathBuf::from(&p.cwd).join(path);
            let text = std::fs::read_to_string(&full)
                .map_err(|e| RpcErr::not_found(format!("cannot read {}: {e}", full.display())))?;
            let set: AssaySet = serde_yaml::from_str(&text)
                .map_err(|e| RpcErr::invalid(format!("{}: {e}", full.display())))?;
            known.extend(
                set.cases
                    .iter()
                    .map(|c| Value::Object(c.inputs.clone()).to_string()),
            );
        }
        let rows: Vec<(String, i64, String, String, String, i64)> = {
            let db = self.db.lock().unwrap();
            let mut st = db
                .prepare(
                    "SELECT id, version, inputs, result, outcome, ended FROM summons
                     WHERE ritual=?1 AND status='ended' AND outcome IN ('returned','abstained') AND series IS NULL
                     ORDER BY RANDOM()",
                )
                .map_err(storage_error)?;
            let rows = st
                .query_map(params![name], |r| {
                    Ok((
                        r.get(0)?,
                        r.get(1)?,
                        r.get(2)?,
                        r.get::<_, Option<String>>(3)?.unwrap_or_default(),
                        r.get(4)?,
                        r.get(5)?,
                    ))
                })
                .map_err(storage_error)?;
            rows.collect::<Result<Vec<_>, _>>().map_err(storage_error)?
        };
        let eligible = rows.len();
        let mut already = 0usize;
        let mut cases = Vec::new();
        for (id, version, inputs, result, outcome, ended) in rows {
            // Canonical form, so key order never hides a duplicate.
            let key = parse_json(Some(inputs.clone()))
                .map_err(storage_error)?
                .to_string();
            if !known.insert(key) {
                already += 1;
                continue;
            }
            if cases.len() < count {
                cases.push(json!({
                    "id": id, "inputs": parse_json(Some(inputs)).map_err(storage_error)?, "expect": {},
                    "observed": parse_json(Some(result)).map_err(storage_error)?,
                    "source": {"summon": id, "version": version, "outcome": outcome, "ended": ended},
                }));
            }
        }
        Ok(
            json!({"ritual": name, "cases": cases, "eligible_runs": eligible, "skipped_duplicates": already}),
        )
    }

    /// Paired comparison between two assay runs (`a` and `b`), given either as
    /// saved `series` IDs in `summons` or inline trial arrays / assay outputs.
    pub(crate) fn assay_compare(&self, params: Value) -> Result<Value, RpcErr> {
        let a_spec = params
            .get("a")
            .filter(|v| !v.is_null())
            .ok_or_else(|| RpcErr::invalid("assay_compare requires `a`"))?;
        let b_spec = params
            .get("b")
            .filter(|v| !v.is_null())
            .ok_or_else(|| RpcErr::invalid("assay_compare requires `b`"))?;

        let a_trials = self.load_compare_trials(a_spec, "a")?;
        let b_trials = self.load_compare_trials(b_spec, "b")?;

        let index_trials = |trials: Vec<CompareTrial>, label: &str| -> Result<BTreeMap<String, CompareTrial>, RpcErr> {
            let mut map = BTreeMap::new();
            for t in trials {
                if map.contains_key(&t.case) {
                    return Err(RpcErr::invalid(format!(
                        "`{label}` contains duplicate case `{}`",
                        t.case
                    )));
                }
                map.insert(t.case.clone(), t);
            }
            Ok(map)
        };
        let map_a = index_trials(a_trials, "a")?;
        let map_b = index_trials(b_trials, "b")?;

        let mut both_correct = 0usize;
        let mut only_a_correct = 0usize;
        let mut only_b_correct = 0usize;
        let mut both_wrong = 0usize;
        let mut unmatched_a = 0usize;
        let mut unmatched_b = 0usize;

        let mut ms_a_sum = 0.0f64;
        let mut ms_b_sum = 0.0f64;
        let mut ms_pairs = 0usize;

        let mut tok_a_sum = 0i64;
        let mut tok_b_sum = 0i64;
        let mut tok_pairs = 0usize;

        for (case, ta) in &map_a {
            match map_b.get(case) {
                Some(tb) => {
                    match (ta.correct, tb.correct) {
                        (true, true) => both_correct += 1,
                        (true, false) => only_a_correct += 1,
                        (false, true) => only_b_correct += 1,
                        (false, false) => both_wrong += 1,
                    }
                    if let (Some(ma), Some(mb)) = (ta.ms, tb.ms) {
                        ms_a_sum += ma;
                        ms_b_sum += mb;
                        ms_pairs += 1;
                    }
                    if let (Some(xa), Some(xb)) = (ta.tokens, tb.tokens) {
                        tok_a_sum += xa;
                        tok_b_sum += xb;
                        tok_pairs += 1;
                    }
                }
                None => unmatched_a += 1,
            }
        }
        for case in map_b.keys() {
            if !map_a.contains_key(case) {
                unmatched_b += 1;
            }
        }

        let matched = both_correct + only_a_correct + only_b_correct + both_wrong;
        let correct_a = both_correct + only_a_correct;
        let correct_b = both_correct + only_b_correct;
        let accuracy_a = if matched == 0 {
            0.0
        } else {
            correct_a as f64 / matched as f64
        };
        let accuracy_b = if matched == 0 {
            0.0
        } else {
            correct_b as f64 / matched as f64
        };
        let accuracy_delta = accuracy_b - accuracy_a;
        let (lo_a, hi_a) = wilson(correct_a, matched);
        let (lo_b, hi_b) = wilson(correct_b, matched);

        let discordant = only_a_correct + only_b_correct;
        let (chi2, significant_at_05) = if discordant == 0 {
            (0.0, false)
        } else {
            let b = only_a_correct as f64;
            let c = only_b_correct as f64;
            let chi2 = ((b - c).abs() - 1.0).max(0.0).powi(2) / (b + c);
            (chi2, chi2 >= 3.841)
        };

        let (mean_ms_a, mean_ms_b, mean_ms_delta) = if ms_pairs > 0 {
            let ma = ms_a_sum / ms_pairs as f64;
            let mb = ms_b_sum / ms_pairs as f64;
            (json!(ma), json!(mb), json!(mb - ma))
        } else {
            (Value::Null, Value::Null, Value::Null)
        };

        let (tokens_a, tokens_b, token_delta) = if tok_pairs > 0 {
            (json!(tok_a_sum), json!(tok_b_sum), json!(tok_b_sum - tok_a_sum))
        } else {
            (Value::Null, Value::Null, Value::Null)
        };

        Ok(json!({
            "matched": matched,
            "unmatched_a": unmatched_a,
            "unmatched_b": unmatched_b,
            "both_correct": both_correct,
            "only_a_correct": only_a_correct,
            "only_b_correct": only_b_correct,
            "both_wrong": both_wrong,
            "accuracy_a": accuracy_a,
            "accuracy_b": accuracy_b,
            "accuracy_delta": accuracy_delta,
            "accuracy_a_interval_95": [lo_a, hi_a],
            "accuracy_b_interval_95": [lo_b, hi_b],
            "mcnemar": {
                "chi2": chi2,
                "significant_at_05": significant_at_05,
                "discordant": discordant,
            },
            "mean_ms_a": mean_ms_a,
            "mean_ms_b": mean_ms_b,
            "mean_ms_delta": mean_ms_delta,
            "tokens_a": tokens_a,
            "tokens_b": tokens_b,
            "token_delta": token_delta,
        }))
    }

    fn load_compare_trials(&self, spec: &Value, label: &str) -> Result<Vec<CompareTrial>, RpcErr> {
        match spec {
            Value::Array(arr) => parse_inline_trials(arr, label),
            Value::Object(obj) => match obj.get("trials").and_then(Value::as_array) {
                Some(arr) => parse_inline_trials(arr, label),
                None => Err(RpcErr::invalid(format!(
                    "`{label}` object must contain a `trials` array"
                ))),
            },
            Value::String(series_id) => {
                let db = self.db.lock().unwrap();
                let stored: Option<String> = db
                    .query_row(
                        "SELECT trials FROM assay_trials WHERE series = ?1",
                        params![series_id],
                        |r| r.get(0),
                    )
                    .optional()
                    .map_err(storage_error)?;
                let raw = stored.ok_or_else(|| {
                    RpcErr::not_found(format!("no assay trials found for series `{series_id}`"))
                })?;
                let arr: Value = serde_json::from_str(&raw).map_err(|e| {
                    RpcErr::new("storage_failed", 11, format!("corrupt assay_trials for `{series_id}`: {e}"))
                })?;
                let list = arr.as_array().ok_or_else(|| {
                    RpcErr::new("storage_failed", 11, format!("corrupt assay_trials for `{series_id}`"))
                })?;
                parse_inline_trials(list, label)
            }
            _ => Err(RpcErr::invalid(format!(
                "`{label}` must be a series id string or a trial array"
            ))),
        }
    }
}

struct CompareTrial {
    case: String,
    correct: bool,
    ms: Option<f64>,
    tokens: Option<i64>,
}

fn parse_inline_trials(arr: &[Value], label: &str) -> Result<Vec<CompareTrial>, RpcErr> {
    let mut out = Vec::with_capacity(arr.len());
    for (i, item) in arr.iter().enumerate() {
        let obj = item.as_object().ok_or_else(|| {
            RpcErr::invalid(format!("`{label}[{i}]` must be a trial object"))
        })?;
        let case = obj
            .get("case")
            .or_else(|| obj.get("id"))
            .and_then(Value::as_str)
            .ok_or_else(|| {
                RpcErr::invalid(format!("`{label}[{i}]` is missing a string `case`"))
            })?
            .to_string();
        let correct = obj.get("correct").and_then(Value::as_bool).ok_or_else(|| {
            RpcErr::invalid(format!(
                "`{label}[{i}]` is missing a boolean `correct` (unscored or touchstone_error trials cannot be paired)"
            ))
        })?;
        let ms = obj.get("ms").and_then(Value::as_f64);
        let tokens = obj.get("tokens").and_then(Value::as_i64);
        out.push(CompareTrial {
            case,
            correct,
            ms,
            tokens,
        });
    }
    Ok(out)
}
