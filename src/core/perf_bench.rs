//! Performance benchmarks on realistic shapes, driven through the RPC
//! dispatcher like a real client. Ignored by default; run with
//!
//! ```text
//! cargo test --offline perf_bench -- --ignored --nocapture --test-threads=1
//! TMPDIR=$PWD/target/tmp-bench cargo test ...   # on the real disk
//! ```
//!
//! Each prints one `bench` line: wall time and what it stored.
use super::test_home::Home;
use super::*;
use std::time::{Duration, Instant};

fn run_timed(h: &Home, target: &str, args: Value) -> (Value, Duration) {
    let t = Instant::now();
    let started = h
        .call("invoke", json!({"target": target, "args": args}))
        .unwrap();
    let rec = h
        .call("wait", json!({"summon": started["summon"], "timeout_ms": 600_000}))
        .unwrap();
    (rec, t.elapsed())
}

fn scalar(h: &Home, sql: &str) -> i64 {
    h.core
        .db
        .lock()
        .unwrap()
        .query_row(sql, [], |r| r.get(0))
        .unwrap()
}

fn sizes(h: &Home) -> String {
    format!(
        "events {} ({} KB), checkpoints {} KB, summons {}",
        scalar(h, "SELECT COUNT(*) FROM events"),
        scalar(h, "SELECT COALESCE(SUM(LENGTH(data)),0) FROM events") / 1024,
        scalar(h, "SELECT COALESCE(SUM(LENGTH(data)),0) FROM checkpoints") / 1024,
        scalar(h, "SELECT COUNT(*) FROM summons"),
    )
}

/// One finding as bughunt's hunt stage makes them.
fn finding(i: usize) -> Value {
    json!({
        "id": format!("f-{i}"), "file": format!("src/file{}.rs", i % 40), "line": i * 7 % 2000,
        "kind": if i.is_multiple_of(3) { "bug" } else { "bypass" },
        "severity": (["low", "medium", "high", "critical"])[i % 4],
        "confidence": 0.5 + (i % 5) as f64 / 10.0,
        "claim": format!("The function at line {i} may not validate its input before use; {}", "x".repeat(120)),
        "snippet": "let key = match reagent {\n        \"file\" => \"root\",\n        _ => return Ok(()),\n    };",
    })
}

#[test]
#[ignore]
fn perf_bench_long_flat_ritual() {
    let h = Home::new("");
    let text = "fn example() { let x = 1; }\n".repeat(80);
    let steps: Vec<Value> = (0..200)
        .map(|i| json!({"id": format!("s{i}"), "operation": "data.trim", "arguments": {"text": text}}))
        .collect();
    h.inscribe_def(
        json!({"ritual": "flat", "steps": steps, "return": {"n": "${steps.s199.output}"}}),
        false,
    );
    let (rec, took) = run_timed(&h, "flat", json!({}));
    assert_eq!(rec["outcome"], "returned", "{rec}");
    eprintln!("bench long-flat 200 steps: {took:?}; {}", sizes(&h));
}

#[test]
#[ignore]
fn perf_bench_data_pipeline() {
    let h = Home::new("");
    perf_inscribe_pipeline(&h);
    let findings: Vec<Value> = (0..1500).map(finding).collect();
    let (rec, took) = run_timed(&h, "pipeline", json!({"findings": findings}));
    assert_eq!(rec["outcome"], "returned", "{rec}");
    eprintln!("bench data-pipeline 1500 findings: {took:?}; {}", sizes(&h));
}

fn perf_inscribe_pipeline(h: &Home) {
    h.inscribe_def(
        json!({
            "ritual": "pipeline", "inputs": {"findings": {"type": "list"}},
            "steps": [
                {"id": "serious", "operation": "data.filter", "arguments": {
                    "values": "${inputs.findings}", "predicate": "item.severity == 'high' or item.severity == 'critical'"}},
                {"id": "tagged", "operation": "data.map", "arguments": {
                    "values": "${inputs.findings}", "operation": "data.set",
                    "arguments": {"value": "${item}", "field": "seen", "item": true}}},
                {"id": "ids", "operation": "data.fold", "arguments": {
                    "values": "${steps.serious.output}", "initial": [], "operation": "data.concat",
                    "arguments": {"left": "${acc}", "right": ["${item.id}"]}}},
                {"id": "both", "operation": "data.concat", "arguments": {
                    "left": "${steps.tagged.output}", "right": "${steps.serious.output}"}},
                {"id": "unique", "operation": "data.unique_by", "arguments": {
                    "values": "${steps.both.output}", "fields": ["file", "line"]}},
                {"id": "n", "operation": "data.count", "arguments": {"value": "${steps.unique.output}"}},
            ],
            "return": {"n": "${steps.n.output}", "ids": "${steps.ids.output}"},
        }),
        false,
    );
}

fn each_ritual(keep: bool) -> Value {
    let mut each = json!({"id": "loop", "each": "f in inputs.findings", "steps": [
        {"id": "big", "operation": "data.replace", "arguments": {"text": "${f.claim}${f.claim}${f.claim}${f.claim}${f.claim}${f.claim}${f.claim}${f.claim}${f.claim}${f.claim}${f.claim}${f.claim}${f.claim}${f.claim}${f.claim}${f.claim}", "from": "x", "to": "y"}},
        {"id": "len", "operation": "data.count", "arguments": {"value": "${steps.big.output}"}},
        {"id": "ok", "operation": "data.compare", "arguments": {"left": "${steps.len.output}", "right": 100, "operator": "ge"}},
    ]});
    if keep {
        each["keep"] = json!(["ok"]);
    }
    json!({"ritual": if keep { "each-keep" } else { "each-all" }, "inputs": {"findings": {"type": "list"}},
           "steps": [each], "return": {"n": "${steps.loop.counts.returned}"}})
}

#[test]
#[ignore]
fn perf_bench_each_retention() {
    for keep in [false, true] {
        let h = Home::new("");
        h.inscribe_def(each_ritual(keep), false);
        let findings: Vec<Value> = (0..500).map(finding).collect();
        let name = if keep { "each-keep" } else { "each-all" };
        let (rec, took) = run_timed(&h, name, json!({"findings": findings}));
        assert_eq!(rec["outcome"], "returned", "{rec}");
        let frame = rec.to_string().len() / 1024;
        eprintln!("bench each-retention 500 items keep={keep}: {took:?}; record {frame} KB; {}", sizes(&h));
    }
}

/// A parent whose each calls a child ritual per item: hundreds of summons
/// with parent links and thousands of events, then the reads a person or an
/// agent makes afterwards.
#[test]
#[ignore]
fn perf_bench_large_state_db() {
    let h = Home::new("");
    h.inscribe_def(
        json!({"ritual": "leaf", "inputs": {"f": {"type": "object"}}, "steps": [
            {"id": "a", "operation": "data.trim", "arguments": {"text": "${inputs.f.claim}"}},
            {"id": "b", "operation": "data.case", "arguments": {"text": "${steps.a.output}", "mode": "upper"}},
            {"id": "c", "operation": "data.count", "arguments": {"value": "${steps.b.output}"}},
        ], "return": {"c": "${steps.c.output}"}}),
        false,
    );
    h.inscribe_def(
        json!({"ritual": "fan", "inputs": {"findings": {"type": "list"}}, "steps": [
            {"id": "loop", "each": "f in inputs.findings", "keep": ["leaf"], "steps": [
                {"id": "leaf", "ritual": "leaf", "arguments": {"f": "${f}"}}]}],
            "return": {"n": "${steps.loop.counts.returned}"}}),
        false,
    );
    let mut last = Value::Null;
    let t = Instant::now();
    for _ in 0..3 {
        let findings: Vec<Value> = (0..150).map(finding).collect();
        last = run_timed(&h, "fan", json!({"findings": findings})).0;
        assert_eq!(last["outcome"], "returned", "{last}");
    }
    eprintln!("bench large-db build (3 x 150 children): {:?}; {}", t.elapsed(), sizes(&h));
    let id = last["id"].as_str().unwrap().to_string();
    let time = |what: &str, method: &str, p: Value| {
        let t = Instant::now();
        let mut out = Value::Null;
        for _ in 0..10 {
            out = h.call(method, p.clone()).unwrap();
        }
        eprintln!(
            "bench large-db {what}: {:?} per call, {} KB",
            t.elapsed() / 10,
            out.to_string().len() / 1024
        );
    };
    time("history", "list", json!({"scope": "ended", "limit": 40}));
    time("history --children", "list", json!({"scope": "children", "summon": id, "limit": 1000}));
    time("record", "get", json!({"summon": id}));
    time("record --brief", "get", json!({"summon": id, "brief": true}));
    time("logs page", "events", json!({"summon": id, "limit": 5000}));
    let db = h.core.db.lock().unwrap();
    for sql in [
        "SELECT * FROM summons WHERE parent = 'x' ORDER BY started",
        "SELECT * FROM summons WHERE status = 'ended' AND parent IS NULL ORDER BY ended DESC LIMIT 40",
        "WITH RECURSIVE tree(id) AS (SELECT id FROM summons WHERE id='x' UNION ALL SELECT s.id FROM summons s JOIN tree t ON s.parent=t.id) SELECT e.kind FROM tree t CROSS JOIN events e ON e.summon=t.id WHERE e.kind IN ('op','held') ORDER BY e.ts,e.seq",
        "SELECT p.id FROM petitions p WHERE p.summon='x'",
        "SELECT * FROM effect_fence WHERE summon='x'",
    ] {
        let mut q = db.prepare(&format!("EXPLAIN QUERY PLAN {sql}")).unwrap();
        let plan: Vec<String> = q
            .query_map([], |r| r.get::<_, String>(3))
            .unwrap()
            .map(Result::unwrap)
            .collect();
        eprintln!("plan {}: {}", &sql[..sql.len().min(60)], plan.join(" | "));
    }
}

#[test]
#[ignore]
fn perf_bench_where_time_goes() {
    // Step-by-step timing of the data pipeline, from event timestamps.
    let h = Home::new("");
    perf_inscribe_pipeline(&h);
    let findings: Vec<Value> = (0..1500).map(finding).collect();
    let (rec, _) = run_timed(&h, "pipeline", json!({"findings": findings}));
    let db = h.core.db.lock().unwrap();
    let mut q = db.prepare("SELECT ts, kind, text FROM events WHERE summon=?1 ORDER BY seq").unwrap();
    let rows: Vec<(i64, String, String)> = q
        .query_map([rec["id"].as_str().unwrap()], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))
        .unwrap()
        .map(Result::unwrap)
        .collect();
    let mut prev = rows[0].0;
    for (ts, kind, text) in rows {
        eprintln!("bench step +{:>5}ms {kind} {}", ts - prev, &text[..text.len().min(50)]);
        prev = ts;
    }
    let effects = rec["effects"].as_array().map(Vec::len).unwrap_or(0);
    eprintln!("bench record effects: {effects}");
}

/// A state database the size of a long-used lab: 200 roots with 100 children
/// each and 200,000 events, written directly; then the reads.
#[test]
#[ignore]
fn perf_bench_query_plans_at_scale() {
    let h = Home::new("");
    {
        let mut db = h.core.db.lock().unwrap();
        let tx = db.transaction().unwrap();
        for r in 0..200 {
            let root = format!("s-root{r}");
            for c in 0..=100 {
                let (id, parent) = if c == 0 { (root.clone(), None) } else { (format!("s-{r}-{c}"), Some(root.clone())) };
                tx.execute(
                    "INSERT INTO summons (id, parent, ritual, version, hash, status, outcome, started, ended, inputs, essence, result) VALUES (?1, ?2, 'r', 1, 'h', 'ended', 'returned', ?3, ?3, '{}', '{}', '{}')",
                    params![id, parent, (r * 1000 + c) as i64],
                ).unwrap();
                for seq in 1..=10 {
                    tx.execute(
                        "INSERT INTO events (summon, seq, ts, kind, text, data) VALUES (?1, ?2, ?3, 'op', 't', '{\"effect\":\"pure\"}')",
                        params![id, seq, seq],
                    ).unwrap();
                }
            }
        }
        tx.commit().unwrap();
    }
    eprintln!("bench scale db: {}", sizes(&h));
    let time = |what: &str, method: &str, p: Value| {
        let t = Instant::now();
        for _ in 0..20 {
            h.call(method, p.clone()).unwrap();
        }
        eprintln!("bench scale {what}: {:?} per call", t.elapsed() / 20);
    };
    time("history", "list", json!({"scope": "ended", "limit": 40}));
    time("history --children", "list", json!({"scope": "children", "summon": "s-root7", "limit": 1000}));
    time("record", "get", json!({"summon": "s-root7"}));
    time("record --brief", "get", json!({"summon": "s-root7", "brief": true}));
    time("logs page", "events", json!({"summon": "s-root7"}));
}
