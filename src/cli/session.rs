use crate::grammar::Op;
use crate::rpc::{Client, RpcErr};
use crate::{finish, format_result, s, target};
use serde_json::{json, Value};


fn parse_session_essence(op: &Op) -> Result<Value, RpcErr> {
    let mut ess = serde_json::Map::new();
    for (k, v) in &op.args {
        match k.as_str() {
            "requests" | "tokens" | "tool_calls" | "items" => {
                let n: u64 = v
                    .parse()
                    .map_err(|_| RpcErr::invalid(format!("essence `{k}` must be a non-negative integer")))?;
                ess.insert(k.clone(), json!(n));
            }
            "time" => {
                ess.insert("time".into(), json!(v));
            }
            _ => {
                return Err(RpcErr::invalid(format!(
                    "unknown essence key `{k}`; expected requests, tokens, time, tool_calls, or items"
                )));
            }
        }
    }
    if let Some(t) = &op.timeout {
        ess.insert("time".into(), json!(t));
    }
    Ok(Value::Object(ess))
}

pub(crate) fn session_summon(op: &Op, client: &mut Client) -> i32 {
    let sub = op.positionals.first().map(String::as_str);
    let res = match sub {
        Some("list") => client.call("session_list", json!({})),
        Some("close") => match op.positionals.get(1) {
            Some(id) => client.call("session_close", json!({"session": id})),
            None => Err(RpcErr::invalid("`summon close` needs a session ID")),
        },
        Some("show") => match op.positionals.get(1) {
            Some(id) => client.call("session_get", json!({"session": id})),
            None => Err(RpcErr::invalid("`summon show` needs a session ID")),
        },
        _ => parse_session_essence(op).and_then(|ess| {
            let name = op.name.clone().or_else(|| op.positionals.first().cloned());
            client.call(
                "session_open",
                json!({
                    "name": name,
                    "model": op.model,
                    "settings": op.settings,
                    "essence": ess,
                }),
            )
        }),
    };
    finish(res, op.json, |d| {
        if let Some(arr) = d.as_array() {
            if arr.is_empty() {
                println!("no summon sessions.");
            } else {
                for row in arr {
                    println!(
                        "{}  {}  {}  {} reqs / {} tok",
                        s(row, "session"),
                        row["name"].as_str().unwrap_or("-"),
                        s(row, "status"),
                        row["used_requests"],
                        row["used_tokens"]
                    );
                }
            }
        } else if d.get("status").and_then(Value::as_str) == Some("closed") {
            println!("closed session {}", s(d, "session"));
        } else if d.get("turns").is_some() {
            println!(
                "session {} ({})  model: {}  turns: {}",
                s(d, "session"),
                s(d, "status"),
                d["model"].as_str().unwrap_or("-"),
                d["turn_count"]
            );
        } else {
            println!(
                "session {} open  model: {}",
                s(d, "session"),
                d["model"].as_str().unwrap_or("(default)")
            );
        }
    })
}

pub(crate) fn session_soul(op: &Op, client: &mut Client) -> i32 {
    let res = target(op, "a session ID").and_then(|id| {
        let text = if let Some(path) = &op.file {
            Some(
                std::fs::read_to_string(path)
                    .map_err(|e| RpcErr::invalid(format!("cannot read {path}: {e}")))?,
            )
        } else if let Some(t) = &op.task {
            Some(t.clone())
        } else {
            op.positionals.get(1).cloned()
        };
        match text {
            Some(t) => client.call("session_soul", json!({"session": id, "text": t})),
            None => client.call("session_get", json!({"session": id})),
        }
    });
    finish(res, op.json, |d| {
        println!("{}", s(d, "system_prompt"));
    })
}

pub(crate) fn session_bind(op: &Op, client: &mut Client) -> i32 {
    let res = target(op, "a session ID").and_then(|id| {
        let name = op
            .name
            .clone()
            .ok_or_else(|| RpcErr::invalid("name the binding: `bind SESSION as NAME file PATH`"))?;
        let (doc, text) = if let Some(path) = &op.file {
            let content = std::fs::read_to_string(path)
                .map_err(|e| RpcErr::invalid(format!("cannot read {path}: {e}")))?;
            (Some(path.clone()), content)
        } else if let Some(t) = &op.task {
            (None, t.clone())
        } else if let Some(t) = op.positionals.get(1) {
            (None, t.clone())
        } else {
            return Err(RpcErr::invalid("provide `file PATH` or `to TEXT` to bind"));
        };
        client.call(
            "session_bind",
            json!({"session": id, "name": name, "document": doc, "text": text}),
        )
    });
    finish(res, op.json, |d| {
        println!(
            "bound `{}` ({}) to session {}",
            s(d, "binding"),
            &s(d, "hash")[..12.min(s(d, "hash").len())],
            s(d, "session")
        );
    })
}

pub(crate) fn session_imbue(op: &Op, client: &mut Client) -> i32 {
    let res = target(op, "a session ID").and_then(|id| {
        if let Some(extra) = op.positionals.get(1) {
            return Err(RpcErr::invalid(format!(
                "unexpected argument `{extra}`; use `at k=v` (`--set`) for settings or `given requests=N` (`--arg`) for essence"
            )));
        }
        let ess = if op.args.is_empty() && op.timeout.is_none() {
            Value::Null
        } else {
            parse_session_essence(op)?
        };
        client.call(
            "session_imbue",
            json!({"session": id, "model": op.model, "settings": op.settings, "essence": ess}),
        )
    });
    finish(res, op.json, |d| {
        println!("imbued session {}", s(d, "session"));
    })
}

pub(crate) fn session_bid(op: &Op, client: &mut Client) -> i32 {
    let res = target(op, "a session ID").and_then(|id| {
        let prompt = op
            .task
            .clone()
            .or_else(|| op.positionals.get(1).cloned())
            .ok_or_else(|| RpcErr::invalid("give a prompt: `bid SESSION to \"...\"` (`ask SESSION --task \"...\"`)"))?;
        let schema: Option<Value> = match &op.schema {
            Some(raw) => {
                let text = if std::path::Path::new(raw).exists() {
                    std::fs::read_to_string(raw)
                        .map_err(|e| RpcErr::invalid(format!("cannot read schema {raw}: {e}")))?
                } else {
                    raw.clone()
                };
                let v: Value = serde_yaml::from_str(&text)
                    .map_err(|e| RpcErr::invalid(format!("invalid schema: {e}")))?;
                Some(v)
            }
            None => None,
        };
        client.call(
            "session_bid",
            json!({"session": id, "prompt": prompt, "schema": schema}),
        )
    });
    finish(res, op.json, |d| {
        if let Some(text) = d["output"].as_str() {
            println!("{text}");
        } else {
            println!("{}", format_result(&d["output"]));
        }
    })
}

pub(crate) fn session_lethe(op: &Op, client: &mut Client) -> i32 {
    let res = target(op, "a session ID").and_then(|id| {
        let keep = if let Some(v) = op.version {
            Some(v as usize)
        } else if let Some(arg) = op.positionals.get(1) {
            if arg == "all" {
                None
            } else {
                Some(
                    arg.parse::<usize>()
                        .map_err(|_| RpcErr::invalid(format!("`{arg}` is not a turn count or `all`")))?,
                )
            }
        } else {
            None
        };
        client.call("session_lethe", json!({"session": id, "keep": keep}))
    });
    finish(res, op.json, |d| {
        println!(
            "forgot {} turn(s) from {} ({} remaining)",
            d["forgotten"],
            s(d, "session"),
            d["remaining_turns"]
        );
    })
}
