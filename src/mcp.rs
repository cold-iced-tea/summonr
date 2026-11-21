//! An MCP server, so other agents can discover and invoke rituals without
//! this binary on their path: stdio for agents on this machine, Streamable
//! HTTP for agents in containers or on other hosts.
//!
//! The surface is deliberately narrow. A calling model can discover, preview,
//! run, check and stop. It cannot inscribe rituals, grant authority, choose a
//! different model, or approve held actions; those stay with the user.

use crate::link::{CoreLink, LinkFactory};
use crate::rpc::RpcErr;
use crate::util::Paths;
use serde_json::{json, Value};
use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpListener;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

const VERSIONS: &[&str] = &["2025-06-18", "2025-03-26", "2024-11-05"];

const INSTRUCTIONS: &str = "Summonr runs saved procedures called rituals on local models, with limits and permissions enforced outside the model. \
Typical use: list_rituals (or call projected ritual_<name> tools directly), describe_ritual / preview_ritual to inspect inputs, effects, and missing permissions, then run_ritual (or simulate_ritual to dry-run writes). \
A run returns a compact result; if it is still running, call check_run with its run_id, or use get_record to page through events and large results. \
Use list_held_actions and list_fences to inspect pending approvals or fenced writes, and compile_ritual / drill_ritual for offline validation and testing. \
You cannot grant permissions, reconcile fences, or approve held actions. When a result says the user must do something or an error has retryable=false, tell the user and do not retry.";

fn tool(name: &str, description: &str, properties: Value, required: &[&str]) -> Value {
    json!({
        "name": name,
        "description": description,
        "inputSchema": {"type": "object", "properties": properties, "required": required, "additionalProperties": false},
        "outputSchema": {"type":"object"},
    })
}

fn invoke_schema(extra: &[(&str, Value)]) -> Value {
    let mut props = serde_json::Map::from_iter([
        ("name".into(), json!({"type": "string"})),
        ("inputs".into(), json!({"type": "object", "description": "Input values by name, as described by describe_ritual"})),
        ("wait_seconds".into(), json!({"type": "integer", "minimum": 0, "maximum": 300})),
        ("expected_hash".into(), json!({"type": "string", "description": "Refuse if the saved contract has changed"})),
        ("idempotency_key".into(), json!({"type": "string", "minLength": 1, "maxLength": 200})),
        ("limits".into(), json!({"type": "object", "additionalProperties": false, "properties": {
            "requests": {"type": "integer", "minimum": 0},
            "tokens": {"type": "integer", "minimum": 0},
            "time": {"type": "string"},
            "tool_calls": {"type": "integer", "minimum": 0},
            "items": {"type": "integer", "minimum": 0},
        }})),
    ]);
    for (k, v) in extra {
        props.insert((*k).into(), v.clone());
    }
    Value::Object(props)
}

pub fn tools() -> Value {
    json!([
        tool("list_rituals", "List the saved rituals with a one-line description and the names of their inputs.", json!({}), &[]),
        tool(
            "describe_ritual",
            "Show one ritual's contract: its inputs with types and defaults, what it returns, its limits, every effect it can have, and whether it has the permissions it needs.",
            json!({"name": {"type": "string", "description": "Ritual name from list_rituals"}}),
            &["name"],
        ),
        tool(
            "preview_ritual",
            "Validate inputs and show what a run would do, without running anything.",
            json!({"name": {"type": "string"}, "inputs": {"type": "object", "description": "Input values by name"}}),
            &["name"],
        ),
        tool(
            "run_ritual",
            "Run a ritual. Waits up to wait_seconds (default 60, at most 300) and returns the result, or a run_id to check later if it is still running.",
            invoke_schema(&[(
                "petition_missing",
                json!({"type": "boolean", "description": "File pending authority petitions if standing grants are missing"}),
            )]),
            &["name"],
        ),
        tool(
            "simulate_ritual",
            "Run a saved ritual with all writes simulated (never performed or held).",
            invoke_schema(&[]),
            &["name"],
        ),
        tool(
            "record_ritual",
            "Run a saved ritual and record model and tool calls into a named core-managed cassette.",
            invoke_schema(&[
                ("cassette", json!({"type": "string", "minLength": 1, "maxLength": 128, "description": "Alphanumeric/dash/underscore cassette name"})),
                ("simulate", json!({"type": "boolean", "description": "Also simulate writes while recording"})),
            ]),
            &["name", "cassette"],
        ),
        tool(
            "replay_ritual",
            "Replay a saved ritual offline from a previously recorded named cassette (all writes simulated).",
            invoke_schema(&[(
                "cassette",
                json!({"type": "string", "minLength": 1, "maxLength": 128, "description": "Alphanumeric/dash/underscore cassette name"}),
            )]),
            &["name", "cassette"],
        ),
        tool(
            "check_run",
            "Get the state and result of a run. Optionally wait up to wait_seconds for it to finish.",
            json!({"run_id": {"type": "string"}, "wait_seconds": {"type": "integer", "minimum": 0, "maximum": 300}}),
            &["run_id"],
        ),
        tool(
            "get_record",
            "Get a run's full record and bounded event/evidence trace (for runs you own). Use result_offset/result_limit to page through large results.",
            json!({
                "run_id": {"type": "string"},
                "after": {"type": "integer", "minimum": 0, "description": "Return events with seq > after (default 0)"},
                "limit": {"type": "integer", "minimum": 1, "maximum": 200, "description": "Maximum events to return (default 100, max 200)"},
                "slim": {"type": "boolean", "description": "Omit large event data payloads"},
                "result_offset": {"type": "integer", "minimum": 0, "description": "Byte offset into serialized run.result JSON"},
                "result_limit": {"type": "integer", "minimum": 1, "maximum": 65536, "description": "Maximum bytes of serialized run.result JSON to return (1..65536)"}
            }),
            &["run_id"],
        ),
        tool("stop_run", "Ask a running ritual to stop. Work already finished is kept.", json!({"run_id": {"type": "string"}}), &["run_id"]),
        tool(
            "list_held_actions",
            "List actions that rituals proposed and are holding for the user's approval. Read-only: only the user can approve or deny them.",
            json!({}),
            &[],
        ),
        tool(
            "list_fences",
            "List external-effect fences (applied, dispatched, or failed write guards) for a run or across recent runs. Read-only: only the user can reconcile them.",
            json!({"run_id": {"type": "string", "description": "Optional run_id filter"}}),
            &[],
        ),
        tool(
            "list_pacts",
            "List the pacts (agreed contracts with another organisation's core) you may call under, with their state.",
            json!({}),
            &[],
        ),
        tool(
            "describe_pact",
            "Show one pact: its terms, the pinned ritual's inputs and effects on the other side, its acceptance checks, and the calls you made under it.",
            json!({"pact": {"type": "string", "description": "Pact name from list_pacts"}}),
            &["pact"],
        ),
        tool(
            "call_pact",
            "Ask the other organisation's core to run the pact's pinned ritual with these inputs. It runs under their permissions and the pact's limits. Held actions and acceptance of the result belong to the owner of this core: report them, do not try to decide them.",
            json!({"pact": {"type": "string"}, "inputs": {"type": "object", "description": "Only the inputs the pact lets a call give"},
                   "idempotency_key": {"type": "string", "minLength": 1, "maxLength": 200}, "wait_seconds": {"type": "integer", "minimum": 0, "maximum": 300}}),
            &["pact"],
        ),
        tool(
            "check_pact_call",
            "Get a pact call's state (sent, delivered, awaiting_decision, settled, accepted, rejected, closed) and its entries. Optionally wait up to wait_seconds for it to finish.",
            json!({"pact": {"type": "string"}, "call": {"type": "integer"}, "wait_seconds": {"type": "integer", "minimum": 0, "maximum": 300}}),
            &["pact", "call"],
        ),
        tool(
            "compile_ritual",
            "Validate and type-check an inline ritual definition (JSON object or YAML/JSON string) offline without saving it or reading host files.",
            json!({"definition": {"description": "Ritual definition as a JSON object or YAML/JSON string"}}),
            &["definition"],
        ),
        tool(
            "drill_ritual",
            "Run inline scripted drill cases against a saved ritual without real model calls or live writes.",
            json!({
                "ritual": {"type": "string", "description": "Saved ritual name"},
                "version": {"type": "integer", "minimum": 1},
                "cases": {"description": "Drill specification (drill, ritual, optional touchstone, cases) as a JSON object or YAML/JSON string"}
            }),
            &["ritual", "cases"],
        ),
    ])
}

const FORBIDDEN_TOOL_SUBSTRINGS: &[&str] = &["grant", "permit", "approve", "inscribe", "allow"];

/// Compile a saved ritual entry from `rituals` into a first-class MCP tool
/// (`ritual_<name>`), omitting `user_only` inputs so callers see only inputs
/// they are permitted to supply.
fn projected_ritual_tool(entry: &Value) -> Option<Value> {
    let ritual_name = entry["ritual"].as_str()?;
    let tool_name = format!("ritual_{ritual_name}");
    if FORBIDDEN_TOOL_SUBSTRINGS
        .iter()
        .any(|f| tool_name.contains(f))
    {
        return None;
    }
    let desc = match entry["description"].as_str().filter(|s| !s.trim().is_empty()) {
        Some(d) => format!("Run saved ritual `{ritual_name}`: {d}"),
        None => format!("Run saved ritual `{ritual_name}`."),
    };
    let mut properties = serde_json::Map::new();
    let mut required = Vec::new();
    if let Some(decls) = entry["input_decls"].as_object() {
        for (name, decl) in decls {
            if decl["user_only"].as_bool() == Some(true) {
                continue;
            }
            let json_ty = match decl["type"].as_str().unwrap_or("string") {
                "integer" => "integer",
                "decimal" => "number",
                "boolean" => "boolean",
                "list" | "array" => "array",
                "object" => "object",
                _ => "string",
            };
            let mut prop = json!({"type": json_ty});
            if let Some(min) = decl.get("min").filter(|v| v.is_number()) {
                prop["minimum"] = min.clone();
            }
            if let Some(max) = decl.get("max").filter(|v| v.is_number()) {
                prop["maximum"] = max.clone();
            }
            if let Some(en) = decl.get("enum").filter(|v| v.is_array()) {
                prop["enum"] = en.clone();
            }
            if let Some(def) = decl.get("default").filter(|v| !v.is_null()) {
                prop["default"] = def.clone();
            } else {
                required.push(Value::String(name.clone()));
            }
            properties.insert(name.clone(), prop);
        }
    }
    let output_schema = entry
        .get("output_schema")
        .filter(|s| s.is_object())
        .cloned()
        .unwrap_or_else(|| json!({"type": "object"}));
    Some(json!({
        "name": tool_name,
        "description": desc,
        "inputSchema": {
            "type": "object",
            "properties": Value::Object(properties),
            "required": Value::Array(required),
            "additionalProperties": false,
        },
        "outputSchema": output_schema,
    }))
}

fn tools_for(link: &LinkFactory) -> Value {
    let mut list = tools().as_array().cloned().unwrap_or_default();
    if let Ok(mut client) = link() {
        if let Ok(Value::Array(rituals)) = client.call("rituals", json!({})) {
            for r in &rituals {
                if let Some(projected) = projected_ritual_tool(r) {
                    list.push(projected);
                }
            }
        }
    }
    Value::Array(list)
}

/// Preserve the declared result exactly. Execution metadata is separate from task data.
const MAX_COMPACT_RESULT_BYTES: usize = 32 * 1024;

fn slice_utf8(s: &str, offset: usize, limit: usize) -> (usize, usize, Option<usize>, &str) {
    let total = s.len();
    let mut start = offset.min(total);
    while start < total && !s.is_char_boundary(start) {
        start += 1;
    }
    let mut end = start.saturating_add(limit).min(total);
    while end > start && !s.is_char_boundary(end) {
        end -= 1;
    }
    let next = (end < total).then_some(end);
    (start, total, next, &s[start..end])
}

pub fn compact(d: &Value) -> Value {
    let running = d["status"] != "ended";
    let mut out = json!({
        "run_id": d["id"], "ritual": d["ritual"], "version": d["version"], "hash": d["hash"],
        "state": if running { json!("running") } else { d["outcome"].clone() },
        "progress": {"done":d["done"],"total":d["total"]},
        "usage": {"requests":d["requests"],"reported_tokens":d["tokens"]},
        "evidence": {"run_id":d["id"],"access":"get_record(run_id) or summonr record RUN_ID --json"},
    });
    let mut result_truncated = false;
    if !running {
        let serialized = serde_json::to_string(&d["result"]).unwrap_or_default();
        if serialized.len() > MAX_COMPACT_RESULT_BYTES {
            result_truncated = true;
            let (_, total, _, preview) = slice_utf8(&serialized, 0, 1024);
            out["result_truncated"] = json!(true);
            out["result_bytes"] = json!(total);
            out["result_preview"] = json!(preview);
        } else {
            out["result"] = d["result"].clone();
        }
    }
    if !d["error"].is_null() {
        out["error"] = d["error"].clone();
    }
    if !d["warnings"].is_null() {
        out["warnings"] = d["warnings"].clone();
    }
    if d["reused"].as_bool() == Some(true) {
        out["reused"] = json!(true);
    }
    if d["effects"].is_array() {
        out["effects"] = d["effects"].clone();
    }
    if d["fences"].as_array().is_some_and(|a| !a.is_empty()) {
        out["fences"] = d["fences"].clone();
    }
    // Task data and historical events cannot establish current approval state.
    out["petitions"] = d["petitions"]
        .as_array()
        .map(|a| json!(a))
        .unwrap_or(json!([]));
    let pending = d["petitions"]
        .as_array()
        .is_some_and(|a| a.iter().any(|p| p["status"] == "pending"));
    let uncertain = d["petitions"]
        .as_array()
        .is_some_and(|a| a.iter().any(|p| p["status"] == "unknown"));
    let fenced_write = d["fences"].as_array().is_some_and(|a| {
        a.iter()
            .any(|f| f["effect"] == "write" && matches!(f["state"].as_str(), Some("dispatched" | "failed")))
    });
    if running {
        out["next"] = json!("Still running. Call check_run with this run_id.");
    } else if pending {
        out["next"] = json!(
            "Actions are held for approval. Tell the user to review them with: summonr petition"
        );
    } else if fenced_write {
        out["next"] = json!(
            "A non-idempotent write is fenced. Ask the user to inspect or reconcile it with: summonr fence list / apply / clear"
        );
    } else if uncertain
        || d["outcome"] == "exhausted"
        || d["outcome"] == "interrupted"
        || d["outcome"] == "unverified"
    {
        out["next"] =
            json!("Inspect partial work and effects with the user; do not rerun automatically.");
    } else if d["outcome"] == "rejected" {
        out["next"] =
            json!("Output did not satisfy contract or verification; inspect error and get_record(run_id).");
    } else if d["outcome"] == "cancelled" {
        out["next"] =
            json!("Run was cancelled; inspect progress and partial effects before retrying.");
    } else if d["outcome"] == "failed" {
        out["next"] =
            json!("Run failed; inspect error and get_record(run_id) before retrying.");
    } else if result_truncated {
        out["next"] = json!(
            "Result exceeds 32 KiB; call get_record(run_id, result_offset=0, result_limit=16384) to page through it."
        );
    }
    out
}

fn is_retryable_code(code: &str) -> bool {
    matches!(
        code,
        "timeout" | "unavailable" | "core_unavailable" | "backend_unavailable"
    )
}

fn failure(e: RpcErr) -> Value {
    let mut out = json!({
        "error": e.code,
        "message": e.message,
        "retryable": is_retryable_code(&e.code),
    });
    if let Some(cmds) = e.data["grant_commands"].as_array() {
        out["user_must_run"] = Value::Array(cmds.clone());
        out["note"] = json!("Only the user can grant these permissions. Show them these commands; do not retry until they confirm.");
    }
    if let Some(pids) = e.data["petitions"].as_array() {
        out["petitions"] = Value::Array(pids.clone());
    }
    if let Some(diags) = e.data.get("diagnostics").filter(|v| !v.is_null()) {
        out["diagnostics"] = diags.clone();
    }
    if let Some(hint) = e.data.get("hint").filter(|v| !v.is_null()) {
        out["hint"] = hint.clone();
    }
    out
}

fn wait(client: &mut dyn CoreLink, id: &str, seconds: u64) -> Result<Value, RpcErr> {
    if seconds == 0 {
        return client.call("get", json!({"summon": id}));
    }
    match client.call(
        "wait",
        json!({"summon": id, "timeout_ms": seconds.min(300) * 1000}),
    ) {
        Err(e) if e.code == "timeout" => client.call("get", json!({"summon": id})),
        other => other,
    }
}

fn valid_ritual_name(target: &str) -> bool {
    !target.is_empty()
        && target
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
}

fn invoke_and_wait(client: &mut dyn CoreLink, req: Value, wait_seconds: u64) -> Result<Value, RpcErr> {
    let accepted = client.call("invoke", req)?;
    let id = accepted["summon"].as_str().unwrap_or("").to_string();
    let mut d = wait(client, &id, wait_seconds)?;
    d["warnings"] = accepted["warnings"].clone();
    if accepted["reused"].as_bool() == Some(true) {
        d["reused"] = json!(true);
    }
    Ok(compact(&d))
}

fn call_tool(link: &LinkFactory, name: &str, args: &Value) -> Result<Value, RpcErr> {
    if let Some(target) = name.strip_prefix("ritual_") {
        if !valid_ritual_name(target)
            || FORBIDDEN_TOOL_SUBSTRINGS
                .iter()
                .any(|f| name.contains(f))
        {
            return Err(RpcErr::invalid(format!("unknown tool `{name}`")));
        }
        let inputs = args
            .as_object()
            .cloned()
            .ok_or_else(|| RpcErr::invalid("arguments must be an object"))?;
        let mut client = link()?;
        if let Ok(Value::Array(rituals)) = client.call("rituals", json!({})) {
            if let Some(projected) = rituals
                .iter()
                .find(|r| r["ritual"] == target)
                .and_then(projected_ritual_tool)
            {
                crate::schema::check(&projected["inputSchema"], args, "arguments")
                    .map_err(RpcErr::invalid)?;
            }
        }
        return invoke_and_wait(
            &mut *client,
            json!({
                "target": target,
                "args": inputs,
                "caller": "agent",
                "limits": {},
            }),
            60,
        );
    }
    let catalog = tools();
    let declaration = catalog
        .as_array()
        .unwrap()
        .iter()
        .find(|t| t["name"] == name)
        .ok_or_else(|| RpcErr::invalid(format!("unknown tool `{name}`")))?;
    crate::schema::check(&declaration["inputSchema"], args, "arguments")
        .map_err(RpcErr::invalid)?;
    if let Some(target) = args["name"].as_str() {
        if !valid_ritual_name(target) {
            return Err(RpcErr::invalid("use a saved ritual name, not a file path"));
        }
    }
    let mut client = link()?;
    let text = |k: &str| args[k].as_str().unwrap_or("").to_string();
    let inputs = args["inputs"].as_object().cloned().unwrap_or_default();
    // Tools take saved names only, so the agent's directory is never needed
    // and is not sent (it would cross the network with `--via`).
    match name {
        "list_rituals" => {
            let list = client.call("rituals", json!({}))?;
            Ok(Value::Array(
                list.as_array()
                    .into_iter()
                    .flatten()
                    .map(|r| json!({"name": r["ritual"], "description": r["description"], "inputs": r["inputs"]}))
                    .collect(),
            ))
        }
        "describe_ritual" => client.call("contract", json!({"name": text("name")})),
        "preview_ritual" => {
            let p = client.call(
                "rehearse",
                json!({"target": text("name"), "args": inputs, "caller": "agent"}),
            )?;
            Ok(json!({
                "ritual": p["ritual"], "inputs": p["inputs"], "effects": p["effects"],
                "missing_permissions": p["authority"]["missing"], "warnings": p["warnings"], "note": p["note"],
            }))
        }
        "run_ritual" | "simulate_ritual" | "record_ritual" | "replay_ritual" => {
            // `valid_ritual_name` above already rejects paths; only saved names reach here.
            let mut req = json!({
                "target": text("name"),
                "args": inputs,
                "caller": "agent",
                "expected_hash": args["expected_hash"],
                "idempotency_key": args["idempotency_key"],
                "limits": args.get("limits").cloned().unwrap_or(json!({})),
            });
            match name {
                "run_ritual" => {
                    if args["petition_missing"].as_bool() == Some(true) {
                        req["petition_missing"] = json!(true);
                    }
                }
                "simulate_ritual" => {
                    req["simulate"] = json!(true);
                }
                "record_ritual" => {
                    req["cassette_record"] = json!(text("cassette"));
                    if args["simulate"].as_bool() == Some(true) {
                        req["simulate"] = json!(true);
                    }
                }
                "replay_ritual" => {
                    req["cassette_replay"] = json!(text("cassette"));
                }
                _ => {}
            }
            invoke_and_wait(
                &mut *client,
                req,
                args["wait_seconds"].as_u64().unwrap_or(60),
            )
        }
        "check_run" => Ok(compact(&wait(
                &mut *client,
            &text("run_id"),
            args["wait_seconds"].as_u64().unwrap_or(0),
        )?)),
        "get_record" => {
            let run_id = text("run_id");
            let after = args["after"].as_u64().unwrap_or(0);
            let limit = args["limit"].as_u64().unwrap_or(100).clamp(1, 200);
            let slim = args["slim"].as_bool().unwrap_or(false);
            let mut run = client.call("get", json!({"summon": run_id}))?;
            let events = client.call(
                "events",
                json!({"summon": run_id, "after": after, "limit": limit, "slim": slim}),
            )?;
            let next_after = events
                .as_array()
                .filter(|arr| arr.len() as u64 == limit)
                .and_then(|arr| arr.last())
                .and_then(|e| e["seq"].as_u64());
            let mut out = json!({
                "events": events,
                "next_after": next_after,
            });
            if args.get("result_offset").is_some() || args.get("result_limit").is_some() {
                let offset = args["result_offset"].as_u64().unwrap_or(0) as usize;
                let rlimit = args["result_limit"].as_u64().unwrap_or(16384).clamp(1, 65536) as usize;
                let serialized = serde_json::to_string(&run["result"]).unwrap_or_default();
                let (start, total, next_offset, chunk) = slice_utf8(&serialized, offset, rlimit);
                if let Some(obj) = run.as_object_mut() {
                    obj.remove("result");
                }
                out["result_page"] = json!({
                    "offset": start,
                    "limit": rlimit,
                    "total_bytes": total,
                    "next_offset": next_offset,
                    "chunk": chunk,
                });
            }
            out["run"] = run;
            Ok(out)
        }
        "stop_run" => client
            .call("cancel", json!({"summon": text("run_id")}))
            .map(|_| json!({"run_id": text("run_id"), "stopping": true})),
        "list_held_actions" => {
            let list = client.call("petitions", json!({"status": "pending"}))?;
            Ok(json!({
                "held": list.as_array().into_iter().flatten().map(|p| json!({
                    "id": p["id"], "ritual": p["ritual"], "action": p["operation"], "arguments": p["arguments"], "because": p["because"],
                })).collect::<Vec<_>>(),
                "note": "Only the user can approve or deny these: summonr petition permit ID, or summonr petition deny ID.",
            }))
        }
        "list_fences" => {
            let list = client.call("fence_list", json!({"summon": args.get("run_id").and_then(Value::as_str)}))?;
            Ok(json!({
                "fences": list,
                "note": "Only the user can reconcile fenced writes: summonr fence apply KEY, or summonr fence clear KEY.",
            }))
        }
        // `caller: agent` marks an anonymous agent, which pacts refuse; a
        // familiar's signed request is answered as that familiar instead.
        "list_pacts" => client.call("pact_list", json!({"caller": "agent"})),
        "describe_pact" => client.call("pact_show", json!({"pact": text("pact"), "caller": "agent"})),
        "call_pact" => {
            let started = client.call("pact_call", json!({"pact": text("pact"), "args": inputs, "idempotency_key": args["idempotency_key"], "caller": "agent"}))?;
            let wait_ms = args["wait_seconds"].as_u64().unwrap_or(60).min(300) * 1000;
            let mut status = client.call("pact_call_status", json!({"pact": text("pact"), "call": started["call"], "wait_ms": wait_ms, "caller": "agent"}))?;
            status["note"] = json!("Held actions and acceptance are decided by the owner of this core (summonr petition permit/deny, summonr pact accept/reject).");
            Ok(status)
        }
        "check_pact_call" => client.call(
            "pact_call_status",
            json!({"pact": text("pact"), "call": args["call"], "wait_ms": args["wait_seconds"].as_u64().unwrap_or(0).min(300) * 1000, "caller": "agent"}),
        ),
        "compile_ritual" => {
            let compiled = client.call("compile", json!({"definition": args["definition"]}))?;
            Ok(json!({
                "valid": compiled["valid"],
                "language_version": compiled["language_version"],
                "hash": compiled["hash"],
                "effects": compiled["effects"],
                "return_types": compiled["return_types"],
                "output_schema": compiled["output_schema"],
            }))
        }
        "drill_ritual" => {
            let target = text("ritual");
            if !valid_ritual_name(&target) {
                return Err(RpcErr::invalid("use a saved ritual name, not a file path"));
            }
            client.call(
                "drill_inline",
                json!({"ritual": target, "version": args.get("version").cloned().unwrap_or(Value::Null), "cases": args["cases"]}),
            )
        }
        other => Err(RpcErr::invalid(format!("unknown tool `{other}`"))),
    }
}

/// Handle one JSON-RPC message. Notifications produce no reply.
pub fn handle(link: &LinkFactory, req: &Value) -> Option<Value> {
    let id = req.get("id").cloned()?;
    let reply = |result: Value| Some(json!({"jsonrpc": "2.0", "id": id, "result": result}));
    match req["method"].as_str().unwrap_or("") {
        "initialize" => {
            let asked = req["params"]["protocolVersion"].as_str().unwrap_or("");
            let version = if VERSIONS.contains(&asked) {
                asked
            } else {
                VERSIONS[0]
            };
            reply(json!({
                "protocolVersion": version,
                "capabilities": {
                    "tools": {},
                    "experimental": {
                        "summonr": {
                            "protocol": "mcp",
                            "version": env!("CARGO_PKG_VERSION"),
                            "controls": ["expected_hash", "idempotency_key", "limits", "wait_seconds"],
                            "features": [
                                "projected_ritual_tools",
                                "get_record_pagination",
                                "compile_ritual",
                                "drill_ritual",
                                "simulate_ritual",
                                "record_ritual",
                                "replay_ritual",
                                "schema_defs",
                                "list_fences",
                                "petition_missing",
                                "retryable_errors"
                            ]
                        }
                    }
                },
                "serverInfo": {"name": "summonr", "version": env!("CARGO_PKG_VERSION")},
                "instructions": INSTRUCTIONS,
            }))
        }
        "ping" => reply(json!({})),
        "tools/list" => reply(json!({"tools": tools_for(link)})),
        "tools/call" => {
            let name = req["params"]["name"].as_str().unwrap_or("");
            let (body, is_error) = match call_tool(link, name, &req["params"]["arguments"]) {
                Ok(v) => (v, false),
                Err(e) => (failure(e), true),
            };
            let structured = if body.is_object() {
                body.clone()
            } else {
                json!({"items":body})
            };
            reply(
                json!({"content": [{"type": "text", "text": structured.to_string()}], "structuredContent":structured, "isError": is_error}),
            )
        }
        other => Some(
            json!({"jsonrpc": "2.0", "id": id, "error": {"code": -32601, "message": format!("method `{other}` is not supported")}}),
        ),
    }
}

/// Which clauses each MCP transport accepts. The secret-URL HTTP server is
/// always the anonymous agent, so it refuses a name rather than ignore it.
pub fn transport_clauses(transport: Option<&str>, name: Option<&str>, via: Option<&str>) -> Result<(), String> {
    if transport == Some("http") && (name.is_some() || via.is_some()) {
        return Err("`mcp http` serves the anonymous agent and takes no name or via; for familiars use `summonr listen` on the core and `summonr mcp --name NAME --via PEER` on the agent's device".into());
    }
    if via.is_some() && name.is_none() {
        return Err("via needs a familiar: give `as NAME` (--name NAME) too".into());
    }
    Ok(())
}

/// At most this many tool calls run at once on one stdio connection.
pub const MAX_CONCURRENT_CALLS: usize = 8;

/// The output stream, shared by workers; each reply is written whole under the lock.
type Shared = Arc<Mutex<Box<dyn Write + Send>>>;

fn emit(out: &Shared, reply: &Value) -> bool {
    let mut o = out.lock().unwrap();
    writeln!(o, "{reply}").and_then(|_| o.flush()).is_ok()
}

/// Run one tool call on a worker: its slot is released and its caller answered
/// even if the handler panics.
fn answer_call(out: &Shared, in_flight: &AtomicUsize, id: Value, call: impl FnOnce() -> Option<Value>) {
    struct Release<'a>(&'a AtomicUsize);
    impl Drop for Release<'_> {
        fn drop(&mut self) {
            self.0.fetch_sub(1, Ordering::SeqCst);
        }
    }
    let _slot = Release(in_flight);
    let reply = std::panic::catch_unwind(std::panic::AssertUnwindSafe(call)).unwrap_or_else(|_| {
        Some(json!({"jsonrpc": "2.0", "id": id, "error": {"code": -32603, "message": "internal error while handling this tool call"}}))
    });
    if let Some(r) = reply {
        emit(out, &r);
    }
}

/// Newline-delimited JSON-RPC on stdin and stdout.
pub fn stdio(link: &LinkFactory) -> i32 {
    let out: Shared = Arc::new(Mutex::new(Box::new(std::io::stdout())));
    serve_lines(link, std::io::stdin().lock(), out)
}

/// Serve requests line by line. Tool calls run on worker threads, so a call
/// that waits (`run_ritual`) never blocks `stop_run` or `check_run`; replies
/// are written in completion order and correlated by id.
pub fn serve_lines<R: BufRead>(link: &LinkFactory, input: R, out: Shared) -> i32 {
    let in_flight = Arc::new(AtomicUsize::new(0));
    let mut workers: Vec<std::thread::JoinHandle<()>> = Vec::new();
    for line in input.lines() {
        let Ok(line) = line else { break };
        if line.trim().is_empty() {
            continue;
        }
        let req = match serde_json::from_str::<Value>(&line) {
            Ok(req) => req,
            Err(e) => {
                let reply = json!({"jsonrpc": "2.0", "id": null, "error": {"code": -32700, "message": e.to_string()}});
                if !emit(&out, &reply) {
                    break;
                }
                continue;
            }
        };
        workers.retain(|w| !w.is_finished());
        if req["method"] == "tools/call" && req.get("id").is_some() {
            if in_flight.load(Ordering::SeqCst) >= MAX_CONCURRENT_CALLS {
                let reply = json!({"jsonrpc": "2.0", "id": req["id"], "error": {"code": -32000,
                    "message": format!("server busy: {MAX_CONCURRENT_CALLS} tool calls in flight; retry when one finishes")}});
                if !emit(&out, &reply) {
                    break;
                }
                continue;
            }
            in_flight.fetch_add(1, Ordering::SeqCst);
            let (link, out, count) = (link.clone(), out.clone(), in_flight.clone());
            workers.push(std::thread::spawn(move || {
                let id = req["id"].clone();
                answer_call(&out, &count, id, || handle(&link, &req));
            }));
            continue;
        }
        if let Some(r) = handle(link, &req) {
            if !emit(&out, &r) {
                break;
            }
        }
    }
    // Let in-flight calls finish and answer before exiting.
    for w in workers {
        let _ = w.join();
    }
    0
}

/// The secret path segment that stands in for a credential. Clients that can
/// only be given a URL (no custom headers) can still be kept out.
pub fn token(paths: &Paths) -> Result<String, String> {
    let file = paths.config.join("mcp_token");
    if let Ok(t) = std::fs::read_to_string(&file) {
        if t.trim().len() >= 32 {
            return Ok(t.trim().to_string());
        }
    }
    let mut bytes = [0u8; 16];
    std::fs::File::open("/dev/urandom")
        .and_then(|mut f| f.read_exact(&mut bytes))
        .map_err(|e| format!("cannot read random bytes: {e}"))?;
    let t: String = bytes.iter().map(|b| format!("{b:02x}")).collect();
    std::fs::create_dir_all(&paths.config).map_err(|e| e.to_string())?;
    use std::os::unix::fs::OpenOptionsExt;
    match std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&file)
    {
        Ok(mut f) => {
            f.write_all(t.as_bytes()).map_err(|e| e.to_string())?;
            f.sync_all().map_err(|e| e.to_string())?;
        }
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
            return std::fs::read_to_string(&file)
                .map_err(|e| e.to_string())
                .and_then(|s| {
                    if s.trim().len() >= 32 {
                        Ok(s.trim().to_owned())
                    } else {
                        Err("invalid or incomplete MCP credential; repair it explicitly".into())
                    }
                })
        }
        Err(e) => return Err(e.to_string()),
    }
    Ok(t)
}

fn respond(stream: &mut impl Write, status: &str, body: &[u8], extra: &str) -> std::io::Result<()> {
    write!(
        stream,
        "HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\n{extra}\r\n",
        body.len()
    )?;
    stream.write_all(body)?;
    stream.flush()
}

fn hmac_sha256(key: &[u8], msg: &[u8]) -> [u8; 32] {
    let k = ring::hmac::Key::new(ring::hmac::HMAC_SHA256, key);
    let tag = ring::hmac::sign(&k, msg);
    tag.as_ref().try_into().expect("HMAC-SHA256 is 32 bytes")
}

fn ct_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    let mut diff = 0u8;
    for (x, y) in a.iter().zip(b.iter()) {
        diff |= x ^ y;
    }
    diff == 0
}

fn b64url_decode(s: &str) -> Option<Vec<u8>> {
    use base64::Engine;
    base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(s.trim_end_matches('='))
        .ok()
}

/// Validate an OAuth 2.1 `Authorization: Bearer <jwt>` header against `cfg`.
/// Returns `Ok(())` or `Err((http_status, oauth_error_code, description))`.
fn verify_bearer_jwt(
    auth_header: Option<&str>,
    cfg: &crate::util::McpAuthConfig,
    now_secs: u64,
) -> Result<(), (&'static str, &'static str, &'static str)> {
    let raw = auth_header
        .and_then(|h| {
            let mut parts = h.splitn(2, ' ');
            let scheme = parts.next()?;
            let tok = parts.next()?.trim();
            if scheme.eq_ignore_ascii_case("bearer") && !tok.is_empty() {
                Some(tok)
            } else {
                None
            }
        })
        .ok_or(("401 Unauthorized", "invalid_token", "missing Bearer token"))?;

    let mut segs = raw.split('.');
    let (Some(h_b64), Some(p_b64), Some(s_b64), None) =
        (segs.next(), segs.next(), segs.next(), segs.next())
    else {
        return Err(("401 Unauthorized", "invalid_token", "malformed JWT"));
    };
    let signing_input = format!("{h_b64}.{p_b64}");

    let hdr_bytes = b64url_decode(h_b64)
        .ok_or(("401 Unauthorized", "invalid_token", "invalid JWT header"))?;
    let hdr: Value = serde_json::from_slice(&hdr_bytes)
        .map_err(|_| ("401 Unauthorized", "invalid_token", "invalid JWT header JSON"))?;
    let alg = hdr["alg"]
        .as_str()
        .ok_or(("401 Unauthorized", "invalid_token", "missing JWT alg"))?;
    let sig = b64url_decode(s_b64)
        .ok_or(("401 Unauthorized", "invalid_token", "invalid JWT signature"))?;

    let sig_ok = match alg {
        "EdDSA" => cfg
            .public_key
            .as_deref()
            .and_then(|pk| crate::keys::PublicKey::parse(pk).ok())
            .is_some_and(|pk| pk.verify(b"", signing_input.as_bytes(), &sig)),
        "HS256" => cfg
            .hs256_secret_env
            .as_deref()
            .and_then(|env_name| std::env::var(env_name).ok())
            .filter(|secret| !secret.is_empty())
            .is_some_and(|secret| {
                let expected = hmac_sha256(secret.as_bytes(), signing_input.as_bytes());
                ct_eq(&expected, &sig)
            }),
        _ => false,
    };
    if !sig_ok {
        return Err(("401 Unauthorized", "invalid_token", "JWT signature invalid"));
    }

    let payload_bytes = b64url_decode(p_b64)
        .ok_or(("401 Unauthorized", "invalid_token", "invalid JWT payload"))?;
    let claims: Value = serde_json::from_slice(&payload_bytes)
        .map_err(|_| ("401 Unauthorized", "invalid_token", "invalid JWT payload JSON"))?;

    if claims["iss"].as_str() != Some(cfg.issuer.as_str()) {
        return Err(("401 Unauthorized", "invalid_token", "issuer mismatch"));
    }
    let aud_ok = match &claims["aud"] {
        Value::String(s) => s == &cfg.audience,
        Value::Array(arr) => arr.iter().any(|v| v.as_str() == Some(cfg.audience.as_str())),
        _ => false,
    };
    if !aud_ok {
        return Err(("401 Unauthorized", "invalid_token", "audience mismatch"));
    }
    let exp = claims["exp"]
        .as_u64()
        .ok_or(("401 Unauthorized", "invalid_token", "missing exp claim"))?;
    if now_secs > exp.saturating_add(cfg.clock_skew_secs) {
        return Err(("401 Unauthorized", "invalid_token", "token expired"));
    }
    if let Some(nbf) = claims["nbf"].as_u64() {
        if now_secs.saturating_add(cfg.clock_skew_secs) < nbf {
            return Err(("401 Unauthorized", "invalid_token", "token not yet valid"));
        }
    }

    if !cfg.required_scopes.is_empty() {
        let mut granted: Vec<&str> = Vec::new();
        if let Some(s) = claims["scope"].as_str() {
            granted.extend(s.split_whitespace());
        }
        if let Some(arr) = claims["scp"].as_array() {
            granted.extend(arr.iter().filter_map(Value::as_str));
        }
        if !cfg
            .required_scopes
            .iter()
            .all(|req| granted.contains(&req.as_str()))
        {
            return Err(("403 Forbidden", "insufficient_scope", "missing required scope"));
        }
    }
    Ok(())
}

fn serve_connection(
    stream: crate::tls::NetStream,
    local: Option<std::net::SocketAddr>,
    link: LinkFactory,
    path: String,
    auth: Option<std::sync::Arc<crate::util::McpAuthConfig>>,
) {
    serve_connection_with_deadline(
        stream,
        local,
        link,
        path,
        auth,
        crate::listen::REQUEST_DEADLINE,
    );
}

fn serve_connection_with_deadline(
    stream: crate::tls::NetStream,
    local: Option<std::net::SocketAddr>,
    link: LinkFactory,
    path: String,
    auth: Option<std::sync::Arc<crate::util::McpAuthConfig>>,
    request_deadline: Duration,
) {
    let _ = stream.set_write_timeout(Some(Duration::from_secs(30)));
    let mut reader = BufReader::new(crate::listen::Deadline {
        stream,
        until: std::time::Instant::now() + request_deadline,
    });
    loop {
        reader.get_mut().until = std::time::Instant::now() + request_deadline;
        let mut request_line = String::new();
        if reader
            .by_ref()
            .take(8193)
            .read_line(&mut request_line)
            .unwrap_or(0)
            == 0
            || request_line.len() > 8192
        {
            return;
        }
        let mut parts = request_line.split_whitespace();
        let (method, target) = (
            parts.next().unwrap_or("").to_string(),
            parts.next().unwrap_or("").to_string(),
        );
        let mut length = 0usize;
        let mut browser = false;
        let mut header_bytes = 0;
        let mut length_seen = false;
        let mut invalid = false;
        let mut host_ok = false;
        let mut authorization: Option<String> = None;
        loop {
            let mut h = String::new();
            let n = reader.by_ref().take(8193).read_line(&mut h).unwrap_or(0);
            header_bytes += n;
            if n == 0 || h.len() > 8192 || header_bytes > 16384 {
                return;
            }
            if h.trim().is_empty() {
                break;
            }
            if let Some((k, v)) = h.split_once(':') {
                if k.chars().any(char::is_whitespace) {
                    invalid = true;
                }
                match k.trim().to_ascii_lowercase().as_str() {
                    "content-length" => {
                        if length_seen {
                            invalid = true;
                        }
                        length_seen = true;
                        match v.trim().parse() {
                            Ok(n) => length = n,
                            Err(_) => invalid = true,
                        }
                    }
                    "transfer-encoding" => invalid = true,
                    "host" => {
                        let host = v.trim();
                        host_ok = local.is_some_and(|a| {
                            host == a.to_string()
                                || (a.ip().is_loopback()
                                    && (host == format!("localhost:{}", a.port())
                                        || host == format!("127.0.0.1:{}", a.port())))
                        });
                    }
                    // Refuse browser-origin requests. Absence of Origin is not authentication.
                    "origin" => browser = true,
                    "authorization" => {
                        if authorization.is_some() {
                            invalid = true;
                        }
                        authorization = Some(v.trim().to_string());
                    }
                    _ => {}
                }
            }
        }
        if invalid || !host_ok {
            let _ = respond(
                &mut reader.get_mut().stream,
                "400 Bad Request",
                b"{}",
                "Connection: close\r\n",
            );
            return;
        }
        let req_path = target.split('?').next().unwrap_or("");
        let is_metadata = auth.is_some() && req_path == "/.well-known/oauth-protected-resource";
        let is_mcp_path =
            ct_eq(req_path.as_bytes(), path.as_bytes()) || (auth.is_some() && req_path == "/mcp");
        // Check browser origin, route, and auth BEFORE allocating or reading any body.
        if browser {
            let _ = respond(
                &mut reader.get_mut().stream,
                "403 Forbidden",
                b"{\"error\":\"browser requests are refused\"}",
                "Connection: close\r\n",
            );
            return;
        }
        if is_metadata {
            let out_stream = &mut reader.get_mut().stream;
            let sent = match method.as_str() {
                "GET" => {
                    let cfg = auth.as_ref().unwrap();
                    let meta = json!({
                        "resource": cfg.audience,
                        "authorization_servers": cfg.authorization_servers,
                        "scopes_supported": cfg.required_scopes,
                        "bearer_methods_supported": ["header"],
                    });
                    respond(out_stream, "200 OK", meta.to_string().as_bytes(), "")
                }
                _ => respond(out_stream, "405 Method Not Allowed", b"", "Allow: GET\r\n"),
            };
            if sent.is_err() {
                return;
            }
            continue;
        }
        if !is_mcp_path {
            let _ = respond(
                &mut reader.get_mut().stream,
                "404 Not Found",
                b"{\"error\":\"not found\"}",
                "Connection: close\r\n",
            );
            return;
        }
        let auth_err = auth.as_ref().and_then(|cfg| {
            let now_secs = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_secs())
                .unwrap_or(0);
            verify_bearer_jwt(authorization.as_deref(), cfg, now_secs).err()
        });
        if let Some((status, code, desc)) = auth_err {
            let hdr = format!(
                "Connection: close\r\nWWW-Authenticate: Bearer resource_metadata=\"/.well-known/oauth-protected-resource\", error=\"{code}\", error_description=\"{desc}\"\r\n"
            );
            let body = json!({"error": code, "error_description": desc}).to_string();
            let _ = respond(&mut reader.get_mut().stream, status, body.as_bytes(), &hdr);
            return;
        }
        if method != "POST" {
            let out_stream = &mut reader.get_mut().stream;
            let sent = match method.as_str() {
                // No server-initiated stream is offered.
                "GET" => respond(
                    out_stream,
                    "405 Method Not Allowed",
                    b"",
                    "Allow: POST, DELETE\r\n",
                ),
                "DELETE" => respond(out_stream, "200 OK", b"{}", ""),
                _ => respond(
                    out_stream,
                    "405 Method Not Allowed",
                    b"",
                    "Allow: POST, DELETE\r\n",
                ),
            };
            if sent.is_err() {
                return;
            }
            continue;
        }
        if length > 8 * 1024 * 1024 {
            let _ = respond(
                &mut reader.get_mut().stream,
                "413 Payload Too Large",
                b"{}",
                "Connection: close\r\n",
            );
            return;
        }
        let mut body = vec![0u8; length];
        let mut filled = 0;
        while filled < length {
            match reader.read(&mut body[filled..]) {
                Ok(0) | Err(_) => return,
                Ok(n) => filled += n,
            }
        }
        let out_stream = &mut reader.get_mut().stream;
        let sent = match serde_json::from_slice::<Value>(&body) {
            Ok(Value::Array(batch)) => {
                let replies: Vec<Value> = batch.iter().filter_map(|r| handle(&link, r)).collect();
                if replies.is_empty() {
                    respond(out_stream, "202 Accepted", b"", "")
                } else {
                    respond(
                        out_stream,
                        "200 OK",
                        Value::Array(replies).to_string().as_bytes(),
                        "",
                    )
                }
            }
            Ok(req) => match handle(&link, &req) {
                Some(r) => respond(out_stream, "200 OK", r.to_string().as_bytes(), ""),
                None => respond(out_stream, "202 Accepted", b"", ""),
            },
            Err(e) => {
                let err = json!({"jsonrpc": "2.0", "id": null, "error": {"code": -32700, "message": e.to_string()}});
                respond(
                    out_stream,
                    "400 Bad Request",
                    err.to_string().as_bytes(),
                    "",
                )
            }
        };
        if sent.is_err() {
            return;
        }
    }
}

/// Streamable HTTP on `addr`, at a path that includes the secret token
/// (or `/mcp` when `mcp_auth` OAuth 2.1 Bearer JWT verification is configured).
pub fn http(paths: &Paths, addr: &str) -> i32 {
    let cfg = match crate::util::Config::load(paths) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("summonr mcp: {e}");
            return 2;
        }
    };
    let auth = cfg.mcp_auth.map(std::sync::Arc::new);
    let tls = match cfg.tls {
        Some(ref t) => match crate::tls::load_server_config(&t.cert, &t.key) {
            Ok(sc) => Some(sc),
            Err(e) => {
                eprintln!("summonr mcp: {e}");
                return 2;
            }
        },
        None => None,
    };
    let token = match token(paths) {
        Ok(t) => t,
        Err(e) => {
            eprintln!("summonr mcp: {e}");
            return 1;
        }
    };
    let listener = match TcpListener::bind(addr) {
        Ok(l) => l,
        Err(e) => {
            eprintln!("summonr mcp: cannot listen on {addr}: {e}");
            return 8;
        }
    };
    let scheme = if tls.is_some() { "https" } else { "http" };
    let path = format!("/mcp/{token}");
    println!("Summonr MCP server (Streamable HTTP)");
    if auth.is_some() {
        println!("  url   {scheme}://{addr}/mcp (and {scheme}://{addr}{path})");
        println!("  auth  OAuth 2.1 Bearer JWT required; metadata at {scheme}://{addr}/.well-known/oauth-protected-resource");
    } else {
        println!("  url   {scheme}://{addr}{path}");
        println!("  The URL contains a secret. Anyone who can reach this address and knows it can run your rituals.");
    }
    println!("  Every caller here is the same anonymous agent. To give agents their own identities, use `summonr listen` with familiars (see the Identity page of the book).");
    println!("  Callers can discover, preview, run, check and stop. They cannot grant authority or approve held actions.");
    let limiter = crate::listen::ConnectionLimiter::new(
        crate::listen::MAX_CONNECTIONS,
        crate::listen::MAX_CONNECTIONS_PER_IP,
    );
    for stream in listener.incoming().flatten() {
        let Ok(peer) = stream.peer_addr() else {
            continue;
        };
        let Some(permit) = limiter.try_acquire(peer.ip()) else {
            continue;
        };
        let (link, path, auth, tls) = (
            crate::link::factory(paths, None, None).expect("an unsigned link always builds"),
            path.clone(),
            auth.clone(),
            tls.clone(),
        );
        std::thread::spawn(move || {
            let _permit = permit;
            let _ = stream.set_read_timeout(Some(crate::listen::REQUEST_DEADLINE));
            let _ = stream.set_write_timeout(Some(crate::listen::REQUEST_DEADLINE));
            let local = stream.local_addr().ok();
            if let Ok(net) = crate::tls::NetStream::wrap(stream, tls.as_ref()) {
                serve_connection(net, local, link, path, auth);
            }
        });
    }
    0
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::TcpStream;

    #[test]
    fn pact_tools_listed() {
        let names: Vec<String> = tools().as_array().unwrap().iter().map(|t| t["name"].as_str().unwrap().to_string()).collect();
        for n in ["list_pacts", "describe_pact", "call_pact", "check_pact_call"] {
            assert!(names.contains(&n.to_string()), "{n}");
        }
        let call = tools().as_array().unwrap().iter().find(|t| t["name"] == "call_pact").unwrap().clone();
        assert!(call["description"].as_str().unwrap().contains("owner"), "{call}");
    }

    #[test]
    fn projected_tool_maps_object() {
        let entry = json!({"ritual": "check-quote", "description": "", "input_decls": {"delivery": {"type": "object"}}});
        let tool = projected_ritual_tool(&entry).unwrap();
        assert_eq!(tool["inputSchema"]["properties"]["delivery"]["type"], "object");
    }

    fn paths() -> Paths {
        let root = std::env::temp_dir().join(crate::util::new_id("summonr-mcp"));
        Paths {
            runtime: root.join("run"),
            state: root.join("state"),
            config: root.join("config"),
        }
    }

    #[test]
    fn handshake_and_tool_listing_need_no_core() {
        let p = paths();
        let init = handle(&crate::link::factory(&p, None, None).unwrap(), &json!({"jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {"protocolVersion": "2025-03-26"}})).unwrap();
        assert_eq!(
            init["result"]["protocolVersion"], "2025-03-26",
            "a supported version is echoed"
        );
        let init = handle(&crate::link::factory(&p, None, None).unwrap(), &json!({"jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {"protocolVersion": "2099-01-01"}})).unwrap();
        assert_eq!(
            init["result"]["protocolVersion"], VERSIONS[0],
            "an unknown version falls back"
        );
        assert!(handle(
            &crate::link::factory(&p, None, None).unwrap(),
            &json!({"jsonrpc": "2.0", "method": "notifications/initialized"})
        )
        .is_none());
        let list = handle(
            &crate::link::factory(&p, None, None).unwrap(),
            &json!({"jsonrpc": "2.0", "id": 2, "method": "tools/list"}),
        )
        .unwrap();
        let names: Vec<&str> = list["result"]["tools"]
            .as_array()
            .unwrap()
            .iter()
            .map(|t| t["name"].as_str().unwrap())
            .collect();
        assert!(names.contains(&"run_ritual"));
        // Authority stays with the user: nothing here can grant, approve, or inscribe.
        for forbidden in ["grant", "permit", "approve", "inscribe", "allow"] {
            assert!(
                !names.iter().any(|n| n.contains(forbidden)),
                "{forbidden} must not be exposed"
            );
        }
        let unknown = handle(
            &crate::link::factory(&p, None, None).unwrap(),
            &json!({"jsonrpc": "2.0", "id": 3, "method": "resources/list"}),
        )
        .unwrap();
        assert_eq!(unknown["error"]["code"], -32601);
    }

    #[test]
    fn compact_results_surface_held_actions_and_remaining_work() {
        let d = json!({
            "petitions":[{"id":"p-1","status":"pending"}],
            "id": "s-1", "ritual": "r", "status": "ended", "outcome": "exhausted", "done": 1, "total": 3, "error": "essence exhausted",
            "result": {"partial": {"triage": {"counts": {"remaining": 2}, "items": [
                {"item": "m1", "status": "returned", "outputs": {"classify": {"folder": "Receipts"}},
                 "effects": [{"operation": "mail.move", "state": "held", "petition": "p-1"}]},
            ]}}},
        });
        let c = compact(&d);
        assert_eq!(c["state"], "exhausted");
        assert_eq!(c["result"], d["result"]);
        assert_eq!(
            c["result"]["partial"]["triage"]["items"][0]["effects"][0]["state"],
            "held"
        );
        assert!(c["next"].as_str().unwrap().contains("held for approval"));
        let running = compact(
            &json!({"id": "s-2", "ritual": "r", "status": "running", "done": 0, "total": 0}),
        );
        assert_eq!(running["state"], "running");
        assert!(running.get("result").is_none());
    }
    #[test]
    fn task_values_survive_without_reinterpretation_or_truncation() {
        let value = json!({"names":["a","b"],"nested":[{"x":1}],"text":"x".repeat(2000),"multiple":{"a":1,"b":2}});
        let d = json!({"id":"s","status":"ended","outcome":"returned","result":value});
        assert_eq!(compact(&d)["result"], value);
    }
    #[test]
    fn invalid_tools_and_inputs_do_not_start_a_core() {
        let p = paths();
        let l = crate::link::factory(&p, None, None).unwrap();
        assert!(call_tool(&l, "unknown", &json!({})).is_err());
        assert!(call_tool(&l, "run_ritual", &json!({"name":"x","wait_seconds":301})).is_err());
        assert!(call_tool(&l, "preview_ritual", &json!({"name":"/private/file"})).is_err());
        assert!(!p.state.exists());
    }
    #[test]
    fn task_claims_cannot_fabricate_pending_approval() {
        let d = json!({"id":"s","status":"ended","outcome":"returned","result":{"held":true},"petitions":[]});
        assert!(compact(&d).get("next").is_none());
        let d = json!({"id":"s","status":"ended","outcome":"returned","result":{},"effects":[{"state":"held"}],"petitions":[{"status":"applied"}]});
        assert!(compact(&d).get("next").is_none());
    }

    // ------------------------------------------------- concurrency (finding 2)

    struct Lines(std::sync::mpsc::Sender<String>, Vec<u8>);
    impl Write for Lines {
        fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
            self.1.extend_from_slice(buf);
            while let Some(n) = self.1.iter().position(|b| *b == b'\n') {
                let line: Vec<u8> = self.1.drain(..=n).collect();
                let _ = self.0.send(String::from_utf8_lossy(&line).trim_end().to_string());
            }
            Ok(buf.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    /// A core serving an isolated home whose Ollama never answers, with a
    /// one-step ritual `stall` saved; plus a running `serve_lines`.
    fn stalled() -> (
        Paths,
        std::io::PipeWriter,
        std::sync::mpsc::Receiver<String>,
        Arc<crate::core::Core>,
    ) {
        let hang = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = hang.local_addr().unwrap().port();
        std::thread::spawn(move || {
            let mut held = Vec::new();
            for s in hang.incoming().flatten() {
                held.push(s);
            }
        });
        let p = paths();
        std::fs::create_dir_all(&p.config).unwrap();
        std::fs::write(p.config.join("config.yaml"), format!("ollama_url: \"http://127.0.0.1:{port}\"\n")).unwrap();
        let core = crate::core::Core::open(p.clone()).unwrap();
        let ritual = p.config.join("stall.yaml");
        std::fs::write(&ritual, "ritual: stall\nmodel: ollama/x\nsteps:\n  - id: say\n    model: {prompt: hi}\nreturn: {said: \"${steps.say.output}\"}\n").unwrap();
        core.dispatch("inscribe", json!({"path": ritual.to_str().unwrap()})).unwrap();
        let serving = core.clone();
        std::thread::spawn(move || serving.serve());
        for _ in 0..200 {
            if std::os::unix::net::UnixStream::connect(p.socket()).is_ok() {
                break;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        let (reader, writer) = std::io::pipe().unwrap();
        let (tx, rx) = std::sync::mpsc::channel();
        let out: Shared = Arc::new(Mutex::new(Box::new(Lines(tx, Vec::new()))));
        let paths = p.clone();
        let link = crate::link::factory(&paths, None, None).unwrap();
        std::thread::spawn(move || serve_lines(&link, BufReader::new(reader), out));
        (p, writer, rx, core)
    }

    fn send(w: &mut std::io::PipeWriter, v: Value) {
        writeln!(w, "{v}").unwrap();
    }

    fn reply_for(rx: &std::sync::mpsc::Receiver<String>, id: i64, within: Duration) -> Option<Value> {
        let end = std::time::Instant::now() + within;
        while let Some(left) = end.checked_duration_since(std::time::Instant::now()) {
            let line = rx.recv_timeout(left).ok()?;
            let v: Value = serde_json::from_str(&line).expect("every stdout line is whole JSON");
            if v["id"] == id {
                return Some(v);
            }
        }
        None
    }

    #[test]
    fn stop_run_answers_while_run_ritual_waits() {
        let (p, mut w, rx, core) = stalled();
        send(&mut w, json!({"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":"run_ritual","arguments":{"name":"stall","inputs":{},"wait_seconds":30}}}));
        std::thread::sleep(Duration::from_millis(300));
        send(&mut w, json!({"jsonrpc":"2.0","id":3,"method":"tools/call","params":{"name":"list_rituals","arguments":{}}}));
        assert!(reply_for(&rx, 3, Duration::from_secs(2)).is_some(), "list_rituals must answer while run_ritual waits");
        let running = core.dispatch("list", json!({})).unwrap();
        let id = running.as_array().unwrap().iter().find(|s| s["ritual"] == "stall").unwrap()["id"].as_str().unwrap().to_string();
        send(&mut w, json!({"jsonrpc":"2.0","id":4,"method":"tools/call","params":{"name":"stop_run","arguments":{"run_id":id}}}));
        let stopped = reply_for(&rx, 4, Duration::from_secs(2)).expect("stop_run must answer while run_ritual waits");
        assert_eq!(stopped["result"]["structuredContent"]["stopping"], true, "{stopped}");
        core.stop.store(true, std::sync::atomic::Ordering::SeqCst);
        let _ = std::fs::remove_dir_all(p.state.parent().unwrap());
    }

    #[test]
    fn parse_error_reply_is_a_whole_line() {
        let (p, mut w, rx, core) = stalled();
        send(&mut w, json!({"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":"run_ritual","arguments":{"name":"stall","inputs":{},"wait_seconds":30}}}));
        std::thread::sleep(Duration::from_millis(200));
        writeln!(w, "{{bad json").unwrap();
        let line = rx.recv_timeout(Duration::from_secs(2)).expect("a parse error is answered at once");
        let v: Value = serde_json::from_str(&line).unwrap();
        assert_eq!(v["error"]["code"], -32700, "{v}");
        core.stop.store(true, std::sync::atomic::Ordering::SeqCst);
        let _ = std::fs::remove_dir_all(p.state.parent().unwrap());
    }

    #[test]
    fn calls_beyond_the_limit_are_refused_as_busy() {
        let (p, mut w, rx, core) = stalled();
        for id in 0..MAX_CONCURRENT_CALLS as i64 {
            send(&mut w, json!({"jsonrpc":"2.0","id":100 + id,"method":"tools/call","params":{"name":"run_ritual","arguments":{"name":"stall","inputs":{},"wait_seconds":30}}}));
        }
        std::thread::sleep(Duration::from_millis(300));
        send(&mut w, json!({"jsonrpc":"2.0","id":9,"method":"tools/call","params":{"name":"list_rituals","arguments":{}}}));
        let busy = reply_for(&rx, 9, Duration::from_secs(2)).expect("an over-limit call is answered at once");
        assert!(busy["error"]["message"].as_str().unwrap().contains("busy"), "{busy}");
        core.stop.store(true, std::sync::atomic::Ordering::SeqCst);
        let _ = std::fs::remove_dir_all(p.state.parent().unwrap());
    }


    #[test]
    fn id_less_tool_call_is_ignored_and_never_blocks() {
        let (p, mut w, rx, core) = stalled();
        send(&mut w, json!({"jsonrpc":"2.0","method":"tools/call","params":{"name":"run_ritual","arguments":{"name":"stall","inputs":{},"wait_seconds":30}}}));
        send(&mut w, json!({"jsonrpc":"2.0","id":3,"method":"tools/call","params":{"name":"list_rituals","arguments":{}}}));
        assert!(reply_for(&rx, 3, Duration::from_secs(2)).is_some(), "the reader must not be blocked");
        let runs = core.dispatch("list", json!({})).unwrap();
        assert!(runs.as_array().unwrap().is_empty(), "an unanswerable tool call must not start work: {runs}");
        core.stop.store(true, std::sync::atomic::Ordering::SeqCst);
        let _ = std::fs::remove_dir_all(p.state.parent().unwrap());
    }

    #[test]
    fn worker_panic_frees_its_slot_and_answers() {
        let (tx, rx) = std::sync::mpsc::channel();
        let out: Shared = Arc::new(Mutex::new(Box::new(Lines(tx, Vec::new()))));
        let in_flight = Arc::new(AtomicUsize::new(1));
        answer_call(&out, &in_flight, json!(7), || panic!("handler bug"));
        assert_eq!(in_flight.load(Ordering::SeqCst), 0);
        let v: Value = serde_json::from_str(&rx.recv_timeout(Duration::from_secs(1)).unwrap()).unwrap();
        assert_eq!(v["id"], 7);
        assert_eq!(v["error"]["code"], -32603, "{v}");
    }


    #[test]
    fn mcp_as_familiar_lists_only_allowed_rituals() {
        let (p, mut w, _rx, core) = stalled();
        drop(w.flush());
        // A second ritual the familiar is not allowed to see.
        let other = p.config.join("other.yaml");
        std::fs::write(&other, "ritual: other\nmodel: mock/keyword\nsteps:\n  - id: say\n    model: {prompt: hi}\n").unwrap();
        core.dispatch("inscribe", json!({"path": other.to_str().unwrap()})).unwrap();
        core.dispatch("familiar_new", json!({"name": "alpha"})).unwrap();
        core.dispatch("familiar_allow", json!({"name": "alpha", "rituals": ["stall"]})).unwrap();
        let link = crate::link::factory(&p, Some("alpha"), None).unwrap();
        let reply = handle(&link, &json!({"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"list_rituals","arguments":{}}})).unwrap();
        let names: Vec<String> = reply["result"]["structuredContent"]["items"]
            .as_array()
            .unwrap()
            .iter()
            .map(|r| r["name"].as_str().unwrap().to_string())
            .collect();
        assert_eq!(names, ["stall"], "{reply}");
        // And the owner-only path is closed: a familiar cannot see others' runs.
        let r = handle(&link, &json!({"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":"check_run","arguments":{"run_id":"s-nope"}}})).unwrap();
        assert_eq!(r["result"]["isError"], true, "{r}");
        // Projected per-ritual tools on `tools/list` also respect the familiar's allowlist.
        let listed_tools = handle(&link, &json!({"jsonrpc":"2.0","id":3,"method":"tools/list"})).unwrap();
        let tool_names: Vec<&str> = listed_tools["result"]["tools"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|t| t["name"].as_str())
            .collect();
        assert!(tool_names.contains(&"ritual_stall"), "{tool_names:?}");
        assert!(!tool_names.contains(&"ritual_other"), "{tool_names:?}");
        core.stop.store(true, std::sync::atomic::Ordering::SeqCst);
        let _ = std::fs::remove_dir_all(p.state.parent().unwrap());
    }

    #[test]
    fn per_ritual_tools_project_schema_omit_user_only_and_execute_directly() {
        let (p, mut w, _rx, core) = stalled();
        drop(w.flush());
        let sum = p.config.join("sum-test.yaml");
        std::fs::write(
            &sum,
            "ritual: sum-test\ndescription: Add two integers\ninputs:\n  a: {type: integer, min: 0}\n  b: {type: integer, default: 5}\n  hold: {type: boolean, default: true, user_only: true}\nreagents:\n  data: {select: [arithmetic]}\nessence: {requests: 0, tool_calls: 2}\nsteps:\n  - id: add\n    operation: data.arithmetic\n    arguments: {left: \"${inputs.a}\", right: \"${inputs.b}\", operator: add}\nreturn: {sum: \"${steps.add.output}\"}\n",
        )
        .unwrap();
        core.dispatch("inscribe", json!({"path": sum.to_str().unwrap()})).unwrap();
        let link = crate::link::factory(&p, None, None).unwrap();
        let listed = handle(&link, &json!({"jsonrpc":"2.0","id":1,"method":"tools/list"})).unwrap();
        let tool = listed["result"]["tools"]
            .as_array()
            .unwrap()
            .iter()
            .find(|t| t["name"] == "ritual_sum-test")
            .expect("ritual_sum-test must be projected");
        assert_eq!(tool["inputSchema"]["properties"]["a"]["type"], "integer");
        assert_eq!(tool["inputSchema"]["properties"]["a"]["minimum"], 0.0);
        assert_eq!(tool["inputSchema"]["properties"]["b"]["default"], 5);
        assert!(
            tool["inputSchema"]["properties"].get("hold").is_none(),
            "user_only inputs must be omitted from projected MCP schema"
        );
        assert_eq!(tool["inputSchema"]["required"], json!(["a"]));

        let ran = handle(
            &link,
            &json!({"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":"ritual_sum-test","arguments":{"a":10}}}),
        )
        .unwrap();
        assert_eq!(ran["result"]["isError"], false, "{ran}");
        assert_eq!(ran["result"]["structuredContent"]["result"]["sum"], 15);
        core.stop.store(true, std::sync::atomic::Ordering::SeqCst);
        let _ = std::fs::remove_dir_all(p.state.parent().unwrap());
    }


    #[test]
    fn mcp_http_refuses_name_and_via() {
        assert!(transport_clauses(Some("http"), Some("codex"), None).is_err());
        assert!(transport_clauses(Some("http"), None, Some("lab")).is_err());
        assert!(transport_clauses(None, None, Some("lab")).is_err(), "via needs a name");
        assert!(transport_clauses(None, Some("codex"), Some("lab")).is_ok());
        assert!(transport_clauses(Some("stdio"), Some("codex"), None).is_ok());
        assert!(transport_clauses(Some("http"), None, None).is_ok());
    }


    /// Records every core call a tool makes.
    struct Spy(Arc<Mutex<Vec<(String, Value)>>>);
    impl CoreLink for Spy {
        fn call(&mut self, method: &str, params: Value) -> Result<Value, RpcErr> {
            self.0.lock().unwrap().push((method.to_string(), params));
            Ok(json!({"summon": "s-1", "status": "ended", "warnings": [], "effects": [], "ritual": "r", "inputs": {}, "authority": {"missing": []}}))
        }
    }

    #[test]
    fn tools_do_not_send_the_agents_directory() {
        let seen = Arc::new(Mutex::new(Vec::new()));
        let s2 = seen.clone();
        let link: LinkFactory = Arc::new(move || Ok(Box::new(Spy(s2.clone())) as Box<dyn CoreLink>));
        for (tool, args) in [("run_ritual", json!({"name": "r"})), ("preview_ritual", json!({"name": "r"}))] {
            let _ = call_tool(&link, tool, &args);
        }
        for (method, params) in seen.lock().unwrap().iter() {
            assert!(params.get("cwd").is_none(), "{method} sent cwd: {params}");
        }
        assert!(seen.lock().unwrap().iter().any(|(m, _)| m == "invoke"));
    }

    #[test]
    fn mcp_enforces_expected_hash_limits_and_exposes_return_types_and_record() {
        let (p, mut w, _rx, core) = stalled();
        drop(w.flush());
        let r_file = p.config.join("classify-conformance.yaml");
        std::fs::write(
            &r_file,
            "ritual: classify-conformance\ndescription: Conformance ritual\nmodel: mock/keyword\ninputs:\n  text: {type: string}\nessence: {requests: 1, tokens: 500, time: 10s}\nsteps:\n  - id: c\n    model:\n      instructions: Classify\n      prompt: \"${inputs.text}\"\n      schema:\n        type: object\n        required: [folder]\n        properties:\n          folder: {type: string, enum: [Receipts, Action]}\nreturn:\n  folder: \"${steps.c.output.folder}\"\n  note: ok\n",
        )
        .unwrap();
        let ins = core.dispatch("inscribe", json!({"path": r_file.to_str().unwrap()})).unwrap();
        let actual_hash = ins["hash"].as_str().unwrap().to_string();
        let link = crate::link::factory(&p, None, None).unwrap();

        // 1. describe_ritual exposes inferred return_types even without explicit output_schema
        let desc = handle(
            &link,
            &json!({"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"describe_ritual","arguments":{"name":"classify-conformance"}}}),
        )
        .unwrap();
        assert_eq!(desc["result"]["isError"], false, "{desc}");
        assert_eq!(
            desc["result"]["structuredContent"]["return_types"],
            json!({"folder": "string", "note": "string"})
        );

        // 2. Wrong expected_hash is refused before execution
        let bad_hash = handle(
            &link,
            &json!({"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":"run_ritual","arguments":{"name":"classify-conformance","inputs":{"text":"receipt"},"expected_hash":"0".repeat(64)}}}),
        )
        .unwrap();
        assert_eq!(bad_hash["result"]["isError"], true, "{bad_hash}");

        // 3. Correct expected_hash with limits.requests: 0 refuses model execution
        let zero_req = handle(
            &link,
            &json!({"jsonrpc":"2.0","id":3,"method":"tools/call","params":{"name":"run_ritual","arguments":{"name":"classify-conformance","inputs":{"text":"receipt"},"expected_hash":actual_hash,"limits":{"requests":0,"tokens":500,"time":"10s"}}}}),
        )
        .unwrap();
        assert_eq!(zero_req["result"]["isError"], false, "{zero_req}");
        assert_eq!(zero_req["result"]["structuredContent"]["state"], "exhausted");
        assert_eq!(zero_req["result"]["structuredContent"]["usage"]["requests"], 0);

        // 4. get_record retrieves the full run + events over MCP without shell access
        let run_id = zero_req["result"]["structuredContent"]["run_id"].as_str().unwrap();
        let rec = handle(
            &link,
            &json!({"jsonrpc":"2.0","id":4,"method":"tools/call","params":{"name":"get_record","arguments":{"run_id":run_id}}}),
        )
        .unwrap();
        assert_eq!(rec["result"]["isError"], false, "{rec}");
        assert_eq!(rec["result"]["structuredContent"]["run"]["id"], run_id);
        assert!(!rec["result"]["structuredContent"]["events"].as_array().unwrap().is_empty());

        // 5. get_record supports result_offset/result_limit pagination
        let paged = handle(
            &link,
            &json!({"jsonrpc":"2.0","id":5,"method":"tools/call","params":{"name":"get_record","arguments":{"run_id":run_id,"result_offset":0,"result_limit":10}}}),
        )
        .unwrap();
        assert_eq!(paged["result"]["isError"], false, "{paged}");
        assert!(paged["result"]["structuredContent"]["result_page"]["chunk"].is_string());
        assert!(paged["result"]["structuredContent"]["run"]["result"].is_null());

        core.stop.store(true, std::sync::atomic::Ordering::SeqCst);
        let _ = std::fs::remove_dir_all(p.state.parent().unwrap());
    }

    #[test]
    fn compact_truncates_large_results_and_points_to_get_record() {
        let huge = "x".repeat(40_000);
        let c = compact(&json!({
            "id": "s-big",
            "ritual": "big",
            "version": 1,
            "hash": "h",
            "status": "ended",
            "outcome": "returned",
            "done": 1,
            "total": 1,
            "requests": 0,
            "tokens": 0,
            "result": {"blob": huge},
            "effects": [],
            "petitions": []
        }));
        assert_eq!(c["result_truncated"], true);
        assert!(c["result"].is_null());
        assert!(c["next"].as_str().unwrap().contains("get_record"));
    }

    #[test]
    fn oauth2_bearer_jwt_verifies_eddsa_hs256_claims_and_scopes() {
        use base64::Engine;
        let b64 = |bytes: &[u8]| base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(bytes);
        let kp = crate::keys::generate();
        let now = 1_800_000_000u64;
        let cfg = crate::util::McpAuthConfig {
            issuer: "https://idp.example.com".into(),
            audience: "https://summonr.internal/mcp".into(),
            authorization_servers: vec!["https://idp.example.com".into()],
            required_scopes: vec!["summonr:run".into()],
            public_key: Some(kp.public().text()),
            hs256_secret_env: None,
            clock_skew_secs: 60,
        };

        let make_eddsa = |hdr: Value, claims: Value| -> String {
            let h = b64(hdr.to_string().as_bytes());
            let p = b64(claims.to_string().as_bytes());
            let input = format!("{h}.{p}");
            let sig = kp.sign(b"", input.as_bytes());
            format!("{input}.{}", b64(&sig))
        };

        let good_claims = json!({
            "iss": "https://idp.example.com",
            "aud": "https://summonr.internal/mcp",
            "exp": now + 300,
            "scope": "openid summonr:run"
        });
        let tok = make_eddsa(json!({"alg": "EdDSA", "typ": "JWT"}), good_claims);
        assert!(verify_bearer_jwt(Some(&format!("Bearer {tok}")), &cfg, now).is_ok());

        // Missing header -> 401
        assert_eq!(verify_bearer_jwt(None, &cfg, now).unwrap_err().0, "401 Unauthorized");

        // alg: none -> 401
        let none_tok = format!(
            "{}.{}.",
            b64(b"{\"alg\":\"none\"}"),
            b64(json!({"iss":"https://idp.example.com","aud":"https://summonr.internal/mcp","exp":now+300,"scope":"summonr:run"}).to_string().as_bytes())
        );
        assert_eq!(
            verify_bearer_jwt(Some(&format!("Bearer {none_tok}")), &cfg, now).unwrap_err().0,
            "401 Unauthorized"
        );

        // Wrong audience -> 401
        let bad_aud = make_eddsa(
            json!({"alg": "EdDSA"}),
            json!({"iss": "https://idp.example.com", "aud": "https://other", "exp": now + 300, "scope": "summonr:run"}),
        );
        assert_eq!(
            verify_bearer_jwt(Some(&format!("Bearer {bad_aud}")), &cfg, now).unwrap_err().0,
            "401 Unauthorized"
        );

        // Expired -> 401
        let expired = make_eddsa(
            json!({"alg": "EdDSA"}),
            json!({"iss": "https://idp.example.com", "aud": "https://summonr.internal/mcp", "exp": now - 120, "scope": "summonr:run"}),
        );
        assert_eq!(
            verify_bearer_jwt(Some(&format!("Bearer {expired}")), &cfg, now).unwrap_err().0,
            "401 Unauthorized"
        );

        // Missing required scope -> 403 insufficient_scope
        let no_scope = make_eddsa(
            json!({"alg": "EdDSA"}),
            json!({"iss": "https://idp.example.com", "aud": "https://summonr.internal/mcp", "exp": now + 300, "scope": "openid"}),
        );
        let err = verify_bearer_jwt(Some(&format!("Bearer {no_scope}")), &cfg, now).unwrap_err();
        assert_eq!(err.0, "403 Forbidden");
        assert_eq!(err.1, "insufficient_scope");

        // HS256 verification via env var
        let secret = "test-hs256-secret-value";
        unsafe { std::env::set_var("SUMMONR_TEST_MCP_HS256", secret) };
        let hs_cfg = crate::util::McpAuthConfig {
            public_key: None,
            hs256_secret_env: Some("SUMMONR_TEST_MCP_HS256".into()),
            ..cfg
        };
        let h = b64(b"{\"alg\":\"HS256\",\"typ\":\"JWT\"}");
        let p = b64(
            json!({"iss": "https://idp.example.com", "aud": ["https://summonr.internal/mcp"], "exp": now + 300, "scp": ["summonr:run"]})
                .to_string()
                .as_bytes(),
        );
        let input = format!("{h}.{p}");
        let sig = hmac_sha256(secret.as_bytes(), input.as_bytes());
        let hs_tok = format!("{input}.{}", b64(&sig));
        assert!(verify_bearer_jwt(Some(&format!("Bearer {hs_tok}")), &hs_cfg, now).is_ok());
    }

    #[test]
    fn http_mcp_enforces_oauth_and_serves_protected_resource_metadata() {
        use base64::Engine;
        use std::io::{Read, Write};
        let b64 = |bytes: &[u8]| base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(bytes);
        struct Direct(std::sync::Arc<crate::core::Core>);
        impl CoreLink for Direct {
            fn call(&mut self, method: &str, params: Value) -> Result<Value, RpcErr> {
                self.0.dispatch(method, params)
            }
        }
        let p = paths();
        let core = crate::core::Core::open(p.clone()).unwrap();
        let core_clone = core.clone();
        let link: LinkFactory =
            std::sync::Arc::new(move || Ok(Box::new(Direct(core_clone.clone())) as Box<dyn CoreLink>));
        let kp = crate::keys::generate();
        let auth = std::sync::Arc::new(crate::util::McpAuthConfig {
            issuer: "https://idp.example.com".into(),
            audience: "https://summonr.internal/mcp".into(),
            authorization_servers: vec!["https://idp.example.com".into()],
            required_scopes: vec!["summonr:run".into()],
            public_key: Some(kp.public().text()),
            hs256_secret_env: None,
            clock_skew_secs: 60,
        });

        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let auth_clone = auth.clone();
        let link_clone = link.clone();
        std::thread::spawn(move || {
            for stream in listener.incoming().flatten().take(3) {
                let local = stream.local_addr().ok();
                serve_connection(
                    crate::tls::NetStream::Plain(stream),
                    local,
                    link_clone.clone(),
                    "/mcp/secret".into(),
                    Some(auth_clone.clone()),
                );
            }
        });

        let send = |raw: &str| -> String {
            let mut s = TcpStream::connect(addr).unwrap();
            s.write_all(raw.as_bytes()).unwrap();
            s.shutdown(std::net::Shutdown::Write).unwrap();
            let mut out = String::new();
            s.read_to_string(&mut out).unwrap();
            out
        };

        // 1. RFC 9728 metadata endpoint
        let meta = send(&format!("GET /.well-known/oauth-protected-resource HTTP/1.1\r\nHost: {addr}\r\n\r\n"));
        assert!(meta.starts_with("HTTP/1.1 200 OK"), "{meta}");
        assert!(meta.contains("https://idp.example.com"), "{meta}");

        // 2. Unauthenticated POST /mcp -> 401 + WWW-Authenticate
        let body = r#"{"jsonrpc":"2.0","id":1,"method":"tools/list"}"#;
        let unauth = send(&format!(
            "POST /mcp HTTP/1.1\r\nHost: {addr}\r\nContent-Length: {}\r\n\r\n{body}",
            body.len()
        ));
        assert!(unauth.starts_with("HTTP/1.1 401 Unauthorized"), "{unauth}");
        assert!(unauth.contains("WWW-Authenticate: Bearer"), "{unauth}");

        // 2b. Unauthenticated request with a huge Content-Length is refused
        // before any body bytes are read.
        let listener2 = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr2 = listener2.local_addr().unwrap();
        let auth2 = auth.clone();
        let link2 = link.clone();
        std::thread::spawn(move || {
            for stream in listener2.incoming().flatten().take(2) {
                let local = stream.local_addr().ok();
                serve_connection_with_deadline(
                    crate::tls::NetStream::Plain(stream),
                    local,
                    link2.clone(),
                    "/mcp/secret".into(),
                    Some(auth2.clone()),
                    Duration::from_millis(300),
                );
            }
        });
        {
            let mut s = TcpStream::connect(addr2).unwrap();
            s.set_read_timeout(Some(Duration::from_secs(2))).unwrap();
            // Send headers only, never send the 1 MiB body.
            write!(
                s,
                "POST /mcp HTTP/1.1\r\nHost: {addr2}\r\nContent-Length: 1048576\r\n\r\n"
            )
            .unwrap();
            let mut out = String::new();
            let _ = s.read_to_string(&mut out);
            assert!(out.starts_with("HTTP/1.1 401 Unauthorized"), "{out}");
        }
        {
            // Slowloris headers are cut off at the whole-request deadline.
            let mut s = TcpStream::connect(addr2).unwrap();
            s.write_all(b"POST /mcp HTTP/1.1\r\n").unwrap();
            let start = std::time::Instant::now();
            let mut closed = false;
            for b in b"X-Slow: aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa" {
                std::thread::sleep(Duration::from_millis(50));
                if s.write_all(&[*b]).is_err() {
                    closed = true;
                    break;
                }
                let mut buf = [0u8; 64];
                s.set_read_timeout(Some(Duration::from_millis(1))).unwrap();
                if let Ok(0) = s.read(&mut buf) {
                    closed = true;
                    break;
                }
            }
            assert!(closed, "slowloris headers must be cut off at the deadline");
            assert!(start.elapsed() < Duration::from_secs(2));
        }

        // 3. Authenticated POST /mcp -> 200 OK
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs();
        let h = b64(b"{\"alg\":\"EdDSA\",\"typ\":\"JWT\"}");
        let payload = b64(
            json!({"iss":"https://idp.example.com","aud":"https://summonr.internal/mcp","exp":now+300,"scope":"summonr:run"})
                .to_string()
                .as_bytes(),
        );
        let input = format!("{h}.{payload}");
        let sig = kp.sign(b"", input.as_bytes());
        let tok = format!("{input}.{}", b64(&sig));
        let ok = send(&format!(
            "POST /mcp HTTP/1.1\r\nHost: {addr}\r\nAuthorization: Bearer {tok}\r\nContent-Length: {}\r\n\r\n{body}",
            body.len()
        ));
        assert!(ok.starts_with("HTTP/1.1 200 OK"), "{ok}");
        assert!(ok.contains("run_ritual"), "{ok}");

        core.stop.store(true, std::sync::atomic::Ordering::SeqCst);
        let _ = std::fs::remove_dir_all(p.state.parent().unwrap());
    }

    #[test]
    fn mcp_compile_and_drill_inline_and_capability_handshake() {
        let (p, mut w, _rx, core) = stalled();
        drop(w.flush());
        let link = crate::link::factory(&p, None, None).unwrap();

        let init = handle(
            &link,
            &json!({"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-06-18"}}),
        )
        .unwrap();
        let caps = &init["result"]["capabilities"]["experimental"]["summonr"];
        assert_eq!(caps["protocol"], "mcp");
        assert!(caps["controls"].as_array().unwrap().contains(&json!("expected_hash")));
        assert!(caps["features"].as_array().unwrap().contains(&json!("compile_ritual")));
        assert!(caps["features"].as_array().unwrap().contains(&json!("drill_ritual")));
        assert!(caps["features"].as_array().unwrap().contains(&json!("schema_defs")));

        let valid_yaml = r#"
ritual: echo-num
inputs: {n: integer}
reagents:
  data: {select: [arithmetic]}
essence: {requests: 0, tool_calls: 1}
steps:
  - id: inc
    operation: data.arithmetic
    arguments: {left: "${inputs.n}", right: 1, operator: add}
return: {out: "${steps.inc.output}"}
"#;
        let compiled = handle(
            &link,
            &json!({"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":"compile_ritual","arguments":{"definition": valid_yaml}}}),
        )
        .unwrap();
        assert_eq!(compiled["result"]["isError"], false, "{compiled}");
        let sc = &compiled["result"]["structuredContent"];
        assert_eq!(sc["valid"], true);
        assert!(sc["hash"].as_str().unwrap().len() == 64);

        let bad_yaml = "ritual: bad\nsteps:\n  - id: s\n    operation: data.arithmetic\n    arguments: {left: \"${inputs.missing}\", right: 1, operator: add}\n";
        let bad = handle(
            &link,
            &json!({"jsonrpc":"2.0","id":3,"method":"tools/call","params":{"name":"compile_ritual","arguments":{"definition": bad_yaml}}}),
        )
        .unwrap();
        assert_eq!(bad["result"]["isError"], true);
        assert!(!bad["result"]["structuredContent"]["diagnostics"].as_array().unwrap().is_empty());

        let with_bindings = "ritual: bind\nbindings: {doc: {document: /etc/passwd}}\nsteps:\n  - id: s\n    operation: data.trim\n    arguments: {text: hi}\n";
        let bind_err = handle(
            &link,
            &json!({"jsonrpc":"2.0","id":4,"method":"tools/call","params":{"name":"compile_ritual","arguments":{"definition": with_bindings}}}),
        )
        .unwrap();
        assert_eq!(bind_err["result"]["isError"], true);

        let r_path = p.config.join("echo-num.yaml");
        std::fs::write(&r_path, valid_yaml).unwrap();
        core.dispatch("inscribe", json!({"path": r_path.to_str().unwrap()})).unwrap();

        let drill_ok = handle(
            &link,
            &json!({"jsonrpc":"2.0","id":5,"method":"tools/call","params":{"name":"drill_ritual","arguments":{
                "ritual": "echo-num",
                "cases": {
                    "drill": "inline-echo",
                    "ritual": "echo-num",
                    "cases": [
                        {"id": "c1", "inputs": {"n": 4}, "expect": {"outcome": "returned", "result": {"out": 5}}}
                    ]
                }
            }}}),
        )
        .unwrap();
        assert_eq!(drill_ok["result"]["isError"], false, "{drill_ok}");
        assert_eq!(drill_ok["result"]["structuredContent"]["passed"], 1);
        assert_eq!(drill_ok["result"]["structuredContent"]["failed"], 0);

        let drill_cassette = handle(
            &link,
            &json!({"jsonrpc":"2.0","id":6,"method":"tools/call","params":{"name":"drill_ritual","arguments":{
                "ritual": "echo-num",
                "cases": {
                    "drill": "inline-cassette",
                    "ritual": "echo-num",
                    "cases": [
                        {"id": "c1", "inputs": {"n": 4}, "cassette": "/tmp/nope.yaml", "expect": {"outcome": "returned"}}
                    ]
                }
            }}}),
        )
        .unwrap();
        assert_eq!(drill_cassette["result"]["isError"], true);

        core.stop.store(true, std::sync::atomic::Ordering::SeqCst);
        let _ = std::fs::remove_dir_all(p.state.parent().unwrap());
    }

    #[test]
    fn mcp_fails_closed_on_unknown_controls_and_surfaces_idempotency_warnings() {
        let (p, mut w, _rx, core) = stalled();
        drop(w.flush());
        let r_path = p.config.join("idemp.yaml");
        std::fs::write(
            &r_path,
            "ritual: idemp\ninputs: {x: integer}\nreagents:\n  data: {select: [arithmetic]}\nessence: {requests: 0, tool_calls: 1}\nsteps:\n  - id: inc\n    operation: data.arithmetic\n    arguments: {left: \"${inputs.x}\", right: 1, operator: add}\nreturn: {out: \"${steps.inc.output}\"}\n",
        )
        .unwrap();
        core.dispatch("inscribe", json!({"path": r_path.to_str().unwrap()})).unwrap();
        let link = crate::link::factory(&p, None, None).unwrap();

        // Unknown top-level control field on run_ritual fails closed
        let bad_ctrl = handle(
            &link,
            &json!({"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"run_ritual","arguments":{"name":"idemp","inputs":{"x":1},"bogus_control":true}}}),
        )
        .unwrap();
        assert_eq!(bad_ctrl["result"]["isError"], true);

        // Unknown limits field on run_ritual fails closed
        let bad_lim = handle(
            &link,
            &json!({"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":"run_ritual","arguments":{"name":"idemp","inputs":{"x":1},"limits":{"bogus":1}}}}),
        )
        .unwrap();
        assert_eq!(bad_lim["result"]["isError"], true);

        // Unknown field on projected ritual_idemp fails closed
        let bad_proj = handle(
            &link,
            &json!({"jsonrpc":"2.0","id":3,"method":"tools/call","params":{"name":"ritual_idemp","arguments":{"x":1,"extra":2}}}),
        )
        .unwrap();
        assert_eq!(bad_proj["result"]["isError"], true);

        // Idempotent repeat surfaces reused: true
        let first = handle(
            &link,
            &json!({"jsonrpc":"2.0","id":4,"method":"tools/call","params":{"name":"run_ritual","arguments":{"name":"idemp","inputs":{"x":2},"idempotency_key":"k-1"}}}),
        )
        .unwrap();
        assert_eq!(first["result"]["isError"], false, "{first}");
        assert!(first["result"]["structuredContent"].get("reused").is_none());

        let second = handle(
            &link,
            &json!({"jsonrpc":"2.0","id":5,"method":"tools/call","params":{"name":"run_ritual","arguments":{"name":"idemp","inputs":{"x":2},"idempotency_key":"k-1"}}}),
        )
        .unwrap();
        assert_eq!(second["result"]["isError"], false, "{second}");
        assert_eq!(second["result"]["structuredContent"]["reused"], true);
        assert_eq!(
            second["result"]["structuredContent"]["run_id"],
            first["result"]["structuredContent"]["run_id"]
        );

        // simulate_ritual, record_ritual, and replay_ritual over MCP
        let sim = handle(
            &link,
            &json!({"jsonrpc":"2.0","id":6,"method":"tools/call","params":{"name":"simulate_ritual","arguments":{"name":"idemp","inputs":{"x":5}}}}),
        )
        .unwrap();
        assert_eq!(sim["result"]["isError"], false, "{sim}");
        assert_eq!(sim["result"]["structuredContent"]["result"]["out"], 6);

        let rec = handle(
            &link,
            &json!({"jsonrpc":"2.0","id":7,"method":"tools/call","params":{"name":"record_ritual","arguments":{"name":"idemp","cassette":"mcp-c1","inputs":{"x":7}}}}),
        )
        .unwrap();
        assert_eq!(rec["result"]["isError"], false, "{rec}");
        assert_eq!(rec["result"]["structuredContent"]["result"]["out"], 8);

        let rep = handle(
            &link,
            &json!({"jsonrpc":"2.0","id":8,"method":"tools/call","params":{"name":"replay_ritual","arguments":{"name":"idemp","cassette":"mcp-c1","inputs":{"x":7}}}}),
        )
        .unwrap();
        assert_eq!(rep["result"]["isError"], false, "{rep}");
        assert_eq!(rep["result"]["structuredContent"]["result"]["out"], 8);

        core.stop.store(true, std::sync::atomic::Ordering::SeqCst);
        let _ = std::fs::remove_dir_all(p.state.parent().unwrap());
    }

    #[test]
    fn compact_and_failure_give_actionable_agent_guidance() {
        for (outcome, expected_substr) in [
            ("failed", "Run failed"),
            ("cancelled", "cancelled"),
            ("rejected", "contract or verification"),
        ] {
            let c = compact(&json!({
                "id": "sum-1",
                "ritual": "r",
                "version": 1,
                "hash": "h",
                "status": "ended",
                "outcome": outcome,
                "done": 1,
                "total": 1,
                "requests": 1,
                "tokens": 10,
                "result": null,
            }));
            assert!(
                c["next"].as_str().unwrap_or("").contains(expected_substr),
                "outcome={outcome}: {c}"
            );
        }

        let fenced = compact(&json!({
            "id": "sum-2",
            "ritual": "r",
            "version": 1,
            "hash": "h",
            "status": "ended",
            "outcome": "failed",
            "done": 1,
            "total": 1,
            "requests": 1,
            "tokens": 10,
            "result": null,
            "fences": [{"fence_key": "k1", "effect": "write", "state": "dispatched"}],
        }));
        assert!(
            fenced["next"].as_str().unwrap_or("").contains("summonr fence"),
            "{fenced}"
        );

        let retry = failure(RpcErr::new("timeout", 9, "timed out").with(json!({"hint": "increase wait_seconds"})));
        assert_eq!(retry["retryable"], true);
        assert_eq!(retry["hint"], "increase wait_seconds");

        let perm = failure(RpcErr::invalid("bad input"));
        assert_eq!(perm["retryable"], false);
    }
}
