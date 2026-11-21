use crate::grammar::Op;
use crate::rpc::{Client, RpcErr};
use crate::theme::{paint, Ink};
use crate::util::thousands;
use crate::{cwd, finish, row, rubric, s, target};
use serde_json::{json, Value};


pub(crate) fn experiment_sweep(op: &Op, client: &mut Client) -> i32 {
    let res = target(op, "a ritual name").and_then(|ritual| {
        let mut body = if let Some(path) = &op.against {
            let text = std::fs::read_to_string(path)
                .map_err(|e| RpcErr::invalid(format!("cannot read {path}: {e}")))?;
            let mut v: Value = serde_yaml::from_str(&text)
                .map_err(|e| RpcErr::invalid(format!("{path}: {e}")))?;
            if v.get("ritual").is_none() {
                v["ritual"] = json!(ritual);
            }
            v
        } else {
            let mut models = Vec::new();
            if let Some(m) = &op.model {
                models.push(m.clone());
            }
            let settings = if op.settings.is_empty() {
                vec![]
            } else {
                vec![op.settings.clone()]
            };
            let cases = if op.args.is_empty() {
                vec![]
            } else {
                vec![json!({"id": "case_1", "inputs": op.args})]
            };
            json!({"ritual": ritual, "models": models, "settings": settings, "cases": cases})
        };
        if op.live {
            body["live"] = json!(true);
        }
        if let Some(v) = op.version {
            body["version"] = json!(v);
        }
        client.call("experiment_sweep", body)
    });
    finish(res, op.json, |d| {
        println!(
            "experiment {} ({} trials for {})",
            s(d, "experiment"),
            d["trial_count"],
            s(d, "ritual")
        );
    })
}

pub(crate) fn experiment_judge(op: &Op, client: &mut Client) -> i32 {
    let res = target(op, "an experiment ID").and_then(|exp| {
        client.call(
            "experiment_judge",
            json!({"experiment": exp, "touchstone": op.against}),
        )
    });
    finish(res, op.json, |d| {
        println!(
            "experiment {}: {}/{} passed",
            s(d, "experiment"),
            d["passed"],
            d["total"]
        );
    })
}

pub(crate) fn experiment_weigh(op: &Op, client: &mut Client) -> i32 {
    let res = target(op, "an experiment ID").and_then(|exp| {
        client.call(
            "experiment_weigh",
            json!({"experiment": exp, "against": op.against}),
        )
    });
    finish(res, op.json, |d| {
        println!("winner: {}", d["winner"].as_str().unwrap_or("-"));
    })
}

/// Exit 14 when any drill case failed or its touchstone could not answer;
/// 0 when all passed.
pub(crate) fn drill(op: &Op, client: &mut Client) -> i32 {
    let res = target(op, "a ritual name").and_then(|name| {
        let against = op.against.clone().ok_or_else(|| {
            RpcErr::invalid(
                "name the drill file: `drill RITUAL against FILE` (`cases RITUAL --against FILE`)",
            )
        })?;
        client.call(
            "drill",
            json!({"ritual": name, "version": op.version, "against": against, "cwd": cwd()}),
        )
    });
    let code = match res.as_ref().ok().map(|d| {
        d["failed"].as_u64().unwrap_or(0) + d["touchstone_errors"].as_u64().unwrap_or(0)
    }) {
        Some(n) if n > 0 => 14,
        _ => 0,
    };
    let shown = finish(res, op.json, |d| {
        for c in d["cases"].as_array().into_iter().flatten() {
            if c["passed"] == true {
                println!("{} {}", paint(Ink::Verdigris, "✓"), s(c, "id"));
            } else if let (Some(err), true) = (
                c["touchstone_error"].as_str(),
                c["reasons"].as_array().is_some_and(Vec::is_empty),
            ) {
                println!(
                    "{} {}: touchstone could not answer: {err}",
                    paint(Ink::Gold, "?"),
                    s(c, "id")
                );
            } else {
                let reasons: Vec<&str> = c["reasons"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .filter_map(Value::as_str)
                    .collect();
                println!(
                    "{} {}: {}",
                    paint(Ink::Rubric, "✕"),
                    s(c, "id"),
                    reasons.join("; ")
                );
            }
        }
        match d["touchstone_errors"].as_u64() {
            Some(n) if n > 0 => println!(
                "{} passed, {} failed, {n} touchstone error(s)",
                d["passed"], d["failed"]
            ),
            _ => println!("{} passed, {} failed", d["passed"], d["failed"]),
        }
    });
    if shown != 0 {
        shown
    } else {
        code
    }
}

pub(crate) fn assay(op: &Op, client: &mut Client) -> i32 {
    if op.positionals.first().map(String::as_str) == Some("compare") {
        let p = &op.positionals;
        if p.len() != 3 {
            return finish(
                Err(RpcErr::invalid("use: assay compare A B (where A and B are series IDs or JSON files)")),
                op.json,
                |_| {},
            );
        }
        let load_side = |spec: &str| -> Result<Value, RpcErr> {
            let path = std::path::Path::new(spec);
            if path.exists() {
                let text = std::fs::read_to_string(path)
                    .map_err(|e| RpcErr::invalid(format!("cannot read {spec}: {e}")))?;
                let parsed: Value = serde_json::from_str(&text)
                    .map_err(|e| RpcErr::invalid(format!("invalid JSON in {spec}: {e}")))?;
                // Unwrap CLI envelope {"schema_version": 1, "ok": true, "data": ...} if present
                Ok(parsed.get("data").cloned().unwrap_or(parsed))
            } else {
                Ok(Value::String(spec.to_string()))
            }
        };
        let res = (|| -> Result<Value, RpcErr> {
            let a = load_side(&p[1])?;
            let b = load_side(&p[2])?;
            client.call("assay_compare", json!({"a": a, "b": b}))
        })();
        return finish(res, op.json, |d| {
            rubric("ASSAY COMPARISON");
            row("matched", &format!("{} (unmatched a: {}, b: {})", d["matched"], d["unmatched_a"], d["unmatched_b"]));
            row("contingency", &format!("both: {}, only a: {}, only b: {}, neither: {}", d["both_correct"], d["only_a_correct"], d["only_b_correct"], d["both_wrong"]));
            row("accuracy a", &format!("{:.1}%", d["accuracy_a"].as_f64().unwrap_or(0.0) * 100.0));
            row("accuracy b", &format!("{:.1}%", d["accuracy_b"].as_f64().unwrap_or(0.0) * 100.0));
            row("delta (b-a)", &format!("{:+.1}%", d["accuracy_delta"].as_f64().unwrap_or(0.0) * 100.0));
            row("McNemar chi2", &format!("{:.3} (significant_at_05: {})", d["mcnemar_chi2"].as_f64().unwrap_or(0.0), d["significant_at_05"]));
        });
    }
    let res = target(op, "a ritual name").and_then(|name| {
        client.call("assay", json!({"ritual": name, "version": op.version, "against": op.against, "baseline": op.baseline, "live": op.live, "model": op.model, "cwd": cwd()}))
    });
    // Exit codes let a scheduler act on the verdict: 12 drifted, 13 inconclusive.
    let code = match res
        .as_ref()
        .ok()
        .and_then(|d| d["drift"]["verdict"].as_str())
    {
        Some("drifted") => 12,
        Some("inconclusive") => 13,
        _ => 0,
    };
    let shown = finish(res, op.json, |d| {
        let a = &d["account"];
        rubric(&format!(
            "ACCOUNT · {} version {} against {}",
            s(d, "ritual"),
            d["version"],
            s(&d["set"], "name")
        ));
        row("model", d["model"].as_str().unwrap_or("none"));
        row(
            "cases",
            &format!(
                "{} · {} correct · {} wrong · {} invalid · {} failed · {} abstentions",
                a["cases"], a["correct"], a["wrong"], a["invalid"], a["failed"], a["abstained"]
            ),
        );
        let iv = &a["accuracy_interval_95"];
        row(
            "accuracy",
            &format!(
                "{}  (95% interval {:.0}–{:.0}%)",
                paint(
                    Ink::Phosphor,
                    &format!("{:.1}%", a["accuracy"].as_f64().unwrap_or(0.0) * 100.0)
                ),
                iv[0].as_f64().unwrap_or(0.0) * 100.0,
                iv[1].as_f64().unwrap_or(0.0) * 100.0
            ),
        );
        row(
            "abstained",
            &format!(
                "{:.1}%",
                a["abstention_rate"].as_f64().unwrap_or(0.0) * 100.0
            ),
        );
        row(
            "cost",
            &format!(
                "{} ms mean per case · {} tokens",
                a["mean_ms"],
                thousands(a["tokens"].as_u64().unwrap_or(0))
            ),
        );
        for t in d["trials"].as_array().into_iter().flatten() {
            if t["correct"] != true {
                println!(
                    "  {} {:<14} {} expected {} got {}",
                    paint(Ink::Rubric, "✕"),
                    s(t, "case"),
                    paint(Ink::Dim, s(t, "outcome")),
                    t["expected"],
                    t["error"]
                        .as_str()
                        .map(String::from)
                        .unwrap_or_else(|| t["got"].to_string())
                );
            }
        }
        if d["baseline"] == "recorded" {
            println!(
                "{}",
                paint(Ink::Verdigris, "  Baseline recorded for this version.")
            );
        }
        if let Some(dr) = d["drift"].as_object() {
            let verdict = dr["verdict"].as_str().unwrap_or("");
            let ink = match verdict {
                "within tolerance" => Ink::Verdigris,
                "drifted" => Ink::Rubric,
                _ => Ink::Ember,
            };
            rubric("DRIFT");
            row("verdict", &paint(ink, verdict));
            row(
                "change",
                &format!(
                    "accuracy {:+.1} points · abstention {:+.1} points",
                    dr["accuracy_change"].as_f64().unwrap_or(0.0) * 100.0,
                    dr["abstention_change"].as_f64().unwrap_or(0.0) * 100.0
                ),
            );
            row(
                "backend",
                if dr["fingerprint_changed"] == true {
                    "fingerprint changed since the baseline"
                } else {
                    "fingerprint unchanged"
                },
            );
            for r in dr["reasons"].as_array().into_iter().flatten() {
                row("because", r.as_str().unwrap_or(""));
            }
            println!(
                "{}",
                paint(
                    Ink::Dim,
                    &format!("  {}", dr["note"].as_str().unwrap_or(""))
                )
            );
        }
    });
    if shown != 0 {
        shown
    } else {
        code
    }
}
