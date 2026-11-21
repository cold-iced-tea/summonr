//! Summonr: an explicit language and runtime for invoking models as bounded
//! capabilities. One binary: `summonr core` is the persistent core, and
//! every other command is a thin client of its socket.

mod backend;
mod cassette;
mod cli;
mod core;
mod drill;
mod envelope;
mod epitaph;
mod exec;
#[cfg(test)]
mod git_fixture;
mod grammar;
mod io;
mod jobs;
mod keys;
mod link;
mod listen;
mod mcp;
mod outcome;
mod pact;
mod plan;
mod pure;
mod reagent;
mod reagents;
mod records;
mod reliquary;
mod requests;
mod rite;
mod ritual;
mod rpc;
mod schema;
mod stdlib;
mod store;
mod theme;
mod tls;
mod touchstone;
mod tsa;
mod tui;
mod util;
mod workbench;

pub(crate) use reagents::{
    cmd_reagent, file_reagent, gh_reagent, git_reagent, http_reagent, mcp_reagent, trial_reagent,
};

use grammar::{Op, Style};
use rpc::{Client, RpcErr};
use serde_json::{json, Value};
use theme::{bold, paint, Ink};
use util::{fmt_ago, fmt_dur, fmt_offset, now_ms, thousands, Config, Paths};

fn main() {
    // Let a closed pipe end the process quietly, as other CLI tools do.
    unsafe {
        libc::signal(libc::SIGPIPE, libc::SIG_DFL);
    }
    let args: Vec<String> = std::env::args().skip(1).collect();
    let paths = Paths::resolve();
    if args.is_empty() {
        if theme::tty() {
            std::process::exit(finish(
                workbench::run(&paths).map(|_| Value::Null),
                false,
                |_| {},
            ));
        }
        help(Style::Keyword);
        return;
    }
    let typed_flags = args.iter().any(|a| a.starts_with("--"));
    let wants_json = args.iter().any(|a| a == "--json")
        || args.windows(2).any(|w| w[0] == "returning" && w[1] == "json")
        || args
            .windows(3)
            .any(|w| w[0] == "then" && w[1] == "return" && w[2] == "json");
    let op = match grammar::parse(&args) {
        Ok(op) => op,
        Err(e) => {
            std::process::exit(finish(Err(RpcErr::invalid(e.to_string())), wants_json, |_| {}));
        }
    };
    let style = if typed_flags {
        Style::Flag
    } else {
        match Config::load(&paths).ok().and_then(|c| c.style).as_deref() {
            Some("flag") => Style::Flag,
            _ => Style::Keyword,
        }
    };
    std::process::exit(run(op, &paths, style));
}

/// Print one result. JSON output is exactly one versioned envelope on stdout;
/// diagnostics go to stderr.
pub(crate) fn finish(res: Result<Value, RpcErr>, as_json: bool, human: impl FnOnce(&Value)) -> i32 {
    match res {
        Ok(data) => {
            if as_json {
                let warnings = data.get("warnings").cloned().unwrap_or(json!([]));
                println!(
                    "{}",
                    json!({"schema_version": 1, "ok": true, "data": data, "error": null, "warnings": warnings})
                );
            } else {
                human(&data);
            }
            0
        }
        Err(e) => {
            if as_json {
                println!(
                    "{}",
                    json!({"schema_version": 1, "ok": false, "data": e.data, "error": {"code": e.code, "message": e.message}, "warnings": []})
                );
            } else {
                eprintln!("{} {}", paint(Ink::Rubric, "summonr:"), e.message);
                if let Some(cmds) = e.data["grant_commands"].as_array() {
                    eprintln!("{}", paint(Ink::Dim, "  to grant it:"));
                    for c in cmds {
                        eprintln!("    {}", c.as_str().unwrap_or(""));
                    }
                }
            }
            e.exit
        }
    }
}

pub(crate) fn rubric(s: &str) {
    println!(
        "{}",
        bold(Ink::Rubric, &format!("{} {s}", theme::ornament()))
    );
}

pub(crate) fn row(key: &str, value: &str) {
    println!("  {} {value}", paint(Ink::Dim, &format!("{key:<10}")));
}

pub(crate) fn s<'a>(v: &'a Value, k: &str) -> &'a str {
    v[k].as_str().unwrap_or("")
}

pub(crate) fn target(op: &Op, what: &str) -> Result<String, RpcErr> {
    op.positionals
        .first()
        .cloned()
        .ok_or_else(|| RpcErr::invalid(format!("this command needs {what}")))
}

pub(crate) fn cwd() -> String {
    std::env::current_dir()
        .map(|p| p.to_string_lossy().into_owned())
        .unwrap_or_else(|_| ".".into())
}

pub(crate) fn outcome_exit(outcome: &str) -> i32 {
    outcome
        .parse::<outcome::Outcome>()
        .map(|o| o.exit_code())
        .unwrap_or(11)
}

pub(crate) fn elegy_summary(d: &Value) {
    let outcome = d["outcome"].as_str().unwrap_or("?");
    let (word, ink) = theme::state_word(s(d, "status"), d["outcome"].as_str());
    let cost_str = d["cost_microusd"]
        .as_u64()
        .filter(|&c| c > 0)
        .map(|c| format!(" · {}", util::fmt_cost_microusd(c)))
        .unwrap_or_default();
    println!(
        "{} {} {}",
        bold(Ink::Rubric, &format!("{} ELEGY", theme::ornament())),
        paint(ink, &format!("{word} ({outcome})")),
        paint(
            Ink::Dim,
            &format!(
                "· {} · {} requests · {} tokens{cost_str}",
                fmt_dur(d["ended"].as_i64().unwrap_or(0) - d["started"].as_i64().unwrap_or(0)),
                d["requests"],
                thousands(d["tokens"].as_u64().unwrap_or(0))
            )
        )
    );
    if let Some(e) = d["error"].as_str() {
        println!("  {}", paint(Ink::Rubric, e));
    }
    for c in d["cleanup"].as_array().into_iter().flatten() {
        row("release", &format!("{}: {}", s(c, "model"), s(c, "result")));
    }
}

pub(crate) fn print_result(d: &Value) {
    if !d["result"].is_null() {
        println!("{}", format_result(&d["result"]));
    }
}

pub(crate) fn format_result(result: &Value) -> String {
    // User-owned JSON never identifies an internal batch, regardless of its keys.
    serde_json::to_string_pretty(result).expect("JSON values always serialize")
}

pub(crate) fn list(rows: &Value, empty: &str) {
    let rows = rows.as_array().cloned().unwrap_or_default();
    if rows.is_empty() {
        println!("{}", paint(Ink::Dim, empty));
        return;
    }
    for v in rows {
        let (word, ink) = theme::state_word(s(&v, "status"), v["outcome"].as_str());
        let literal = v["outcome"].as_str().unwrap_or(s(&v, "status"));
        let progress = match v["total"].as_i64().unwrap_or(0) {
            0 => String::new(),
            t => format!(" {}/{t}", v["done"]),
        };
        let when = match v["ended"].as_i64() {
            Some(e) => fmt_ago(e),
            None => fmt_dur(now_ms() - v["started"].as_i64().unwrap_or(0)),
        };
        println!(
            "{} {:<10} {:<26} {} {}",
            paint(Ink::Gold, &theme::sigil(s(&v, "id"))),
            s(&v, "id"),
            format!(
                "{}{} v{}",
                if v["parent"].is_null() { "" } else { "↳ " },
                s(&v, "ritual"),
                v["version"]
            ),
            paint(ink, &format!("{word:<13}")),
            paint(Ink::Dim, &format!("({literal}){progress} · {when}"))
        );
    }
}

pub(crate) fn ledger(events: &Value, started: i64) {
    for ev in events.as_array().into_iter().flatten() {
        let (mark, ink) = theme::event_mark(s(ev, "kind"));
        println!(
            "  {} {} {}",
            paint(
                Ink::Dim,
                &fmt_offset(ev["ts"].as_i64().unwrap_or(0) - started)
            ),
            paint(ink, mark),
            s(ev, "text")
        );
    }
}


#[cfg(test)]
use cli::core::stop_running_core;
use cli::core::{core_command, help, validate_file};
use cli::eval::{assay, drill, experiment_judge, experiment_sweep, experiment_weigh};
use cli::invoke::{fence_command, follow_command, invoke, petition, wait_for};
use cli::session::{session_bid, session_bind, session_imbue, session_lethe, session_soul, session_summon};
use cli::trust::{chain_command, epitaph_command, epitaph_verify_command, familiar_command, key_command, peer_command, verify_command, witness_command};
use cli::work::{demo_command, job_command, pact_command, reliquary_command, rite_command};

fn run(op: Op, paths: &Paths, style: Style) -> i32 {
    let json_out = op.json;
    if op.record.is_some() && op.verb != "invoke" {
        return finish(
            Err(RpcErr::invalid("recording is valid only with invoke")),
            json_out,
            |_| {},
        );
    }
    if op.replay.is_some() && op.verb != "invoke" {
        return finish(
            Err(RpcErr::invalid("replaying is valid only with invoke")),
            json_out,
            |_| {},
        );
    }
    if op.simulate && op.verb != "invoke" {
        return finish(
            Err(RpcErr::invalid("simulating is valid only with invoke")),
            json_out,
            |_| {},
        );
    }
    if op.tsa_ca.is_some() && op.verb != "verify" {
        return finish(Err(RpcErr::invalid("tsa-ca is valid only with verify")), json_out, |_| {});
    }
    if op.key.is_some() && !matches!(op.verb.as_str(), "familiar" | "peer" | "verify" | "witness" | "pact") {
        return finish(
            Err(RpcErr::invalid("key is valid only with familiar add, peer add, verify, witness accept or pact admit")),
            json_out,
            |_| {},
        );
    }
    if op.via.is_some() && op.verb != "mcp" {
        return finish(
            Err(RpcErr::invalid("via is valid only with mcp")),
            json_out,
            |_| {},
        );
    }
    if op.url.is_some() && op.verb != "peer" {
        return finish(
            Err(RpcErr::invalid("url is valid only with peer add")),
            json_out,
            |_| {},
        );
    }
    if op.live && !matches!(op.verb.as_str(), "assay" | "witness") {
        return finish(
            Err(RpcErr::invalid("live is valid only with assay or witness compare")),
            json_out,
            |_| {},
        );
    }
    if op.replay.is_some() && op.record.is_some() {
        return finish(
            Err(RpcErr::invalid(
                "replaying cannot be combined with recording",
            )),
            json_out,
            |_| {},
        );
    }
    match op.verb.as_str() {
        "help" => {
            if json_out {
                let verbs: Vec<Value> = grammar::VERBS
                    .iter()
                    .map(|(r, p, _, m)| json!({"ritual": r, "plain": p, "meaning": m}))
                    .collect();
                let clauses: Vec<Value> = grammar::CLAUSES
                    .iter()
                    .map(|(k, f, m)| json!({"keyword": k, "flag": f, "meaning": m}))
                    .collect();
                return finish(
                    Ok(json!({"version": env!("CARGO_PKG_VERSION"), "commands": verbs, "clauses": clauses})),
                    true,
                    |_| {},
                );
            }
            help(style);
            return 0;
        }
        "version" => {
            return finish(
                Ok(json!({"name": "summonr", "version": env!("CARGO_PKG_VERSION")})),
                json_out,
                |_| println!("summonr {}", env!("CARGO_PKG_VERSION")),
            );
        }
        "validate" => return finish(validate_file(&op, paths), json_out, |v| println!("{}", v)),
        "core" => return core_command(&op, paths),
        "console" => return finish(workbench::run(paths).map(|_| Value::Null), false, |_| {}),
        "listen" => {
            return match op.positionals.first() {
                Some(addr) if op.positionals.len() == 1 => listen::listen(paths, addr),
                _ => finish(
                    Err(RpcErr::invalid(
                        "give one address to listen on, such as `listen 0.0.0.0:7744`",
                    )),
                    json_out,
                    |_| {},
                ),
            };
        }
        "mcp" => {
            if let Err(e) = mcp::transport_clauses(op.positionals.first().map(String::as_str), op.name.as_deref(), op.via.as_deref()) {
                return finish(Err(RpcErr::invalid(e)), json_out, |_| {});
            }
            return match op.positionals.first().map(String::as_str) {
                None | Some("stdio") => {
                    let link = match link::factory(paths, op.name.as_deref(), op.via.as_deref()) {
                        Ok(l) => l,
                        Err(e) => {
                            return finish(Err(RpcErr::new("not_authenticated", 4, e)), json_out, |_| {});
                        }
                    };
                    match (&op.name, &op.via) {
                        (None, _) => eprintln!("summonr mcp: running as the anonymous agent; create a familiar: summonr familiar new NAME"),
                        (Some(n), None) => eprintln!("summonr mcp: signing as familiar `{n}` to this device's core"),
                        (Some(n), Some(v)) => eprintln!("summonr mcp: signing as familiar `{n}` to pinned peer `{v}`"),
                    }
                    mcp::stdio(&link)
                }
                Some("http") => mcp::http(
                    paths,
                    op.positionals
                        .get(1)
                        .map(String::as_str)
                        .unwrap_or("127.0.0.1:7733"),
                ),
                Some(other) => finish(
                    Err(RpcErr::invalid(format!(
                        "unknown mcp transport `{other}`; use `mcp` for stdio or `mcp http ADDRESS:PORT`"
                    ))),
                    json_out,
                    |_| {},
                ),
            }
        }
        "daemon" => return finish(tui::daemon_panel(paths).map(|_| Value::Null), false, |_| {}),
        "scry" => {
            return finish(
                tui::run(paths, op.positionals.first().cloned()).map(|_| Value::Null),
                false,
                |_| {},
            )
        }
        "key" => return key_command(&op),
        "demo" => return demo_command(&op),
        "epitaph" if op.positionals.first().map(String::as_str) == Some("verify") => {
            return epitaph_verify_command(&op);
        }
        _ => {}
    }
    let mut client = match Client::connect(paths) {
        Ok(c) => c,
        Err(e) => return finish(Err(e), json_out, |_| {}),
    };
    match op.verb.as_str() {
        "invoke" => invoke(&op, &mut client),
        "job" => job_command(&op, &mut client),
        "inscribe" => {
            let res = if let Some(sess) = &op.session {
                target(&op, "a ritual name").and_then(|ritual| {
                    client.call(
                        "session_inscribe",
                        json!({"session": sess, "ritual": ritual, "replace": op.replace}),
                    )
                })
            } else {
                target(&op, "a ritual file").and_then(|path| {
                    client.call(
                        "inscribe",
                        json!({"path": path, "cwd": cwd(), "name": op.name, "replace": op.replace}),
                    )
                })
            };
            finish(res, json_out, |d| {
                if d["unchanged"] == true {
                    println!(
                        "{} is already inscribed as version {} with these exact contents.",
                        s(d, "ritual"),
                        d["version"]
                    );
                } else {
                    println!(
                        "Inscribed {} as version {}.",
                        bold(Ink::Bone, s(d, "ritual")),
                        d["version"]
                    );
                }
                row("hash", s(d, "hash"));
                effects(&d["effects"]);
            })
        }
        "grimoire" => match op.positionals.first().map(String::as_str) {
            Some("read") => {
                let res = op
                    .positionals
                    .get(1)
                    .cloned()
                    .ok_or_else(|| RpcErr::invalid("name the ritual: grimoire read NAME"))
                    .and_then(|name| {
                        client.call("ritual_read", json!({"name": name, "version": op.version}))
                    });
                finish(res, json_out, |d| {
                    rubric(&format!("{} · version {}", s(d, "ritual"), d["version"]));
                    row("hash", s(d, "hash"));
                    let grants = d["grants"].as_array().cloned().unwrap_or_default();
                    if grants.is_empty() {
                        row("grants", "none");
                    }
                    for g in grants {
                        let mut line = format!("{}.{}", s(&g, "reagent"), s(&g, "action"));
                        for (k, v) in g["on"].as_object().into_iter().flatten() {
                            line.push_str(&format!(" on {k}={}", v.as_str().unwrap_or("")));
                        }
                        for (k, v) in g["into"].as_object().into_iter().flatten() {
                            line.push_str(&format!(" into {k}={}", v.as_str().unwrap_or("")));
                        }
                        row("grant", &line);
                    }
                    match d["baseline"].as_object() {
                        Some(b) => row(
                            "baseline",
                            &format!(
                                "{} · accuracy {:.0}% over {} cases · recorded {}",
                                b["set"].as_str().unwrap_or(""),
                                b["account"]["accuracy"].as_f64().unwrap_or(0.0) * 100.0,
                                b["account"]["cases"],
                                fmt_ago(b["created"].as_i64().unwrap_or(0))
                            ),
                        ),
                        None => row("baseline", "none recorded"),
                    }
                    println!(
                        "{}",
                        serde_yaml::to_string(&d["definition"]).unwrap_or_default()
                    );
                })
            }
            Some(other) => finish(
                Err(RpcErr::invalid(format!(
                    "unknown grimoire operation `{other}`; try `grimoire` or `grimoire read NAME`"
                ))),
                json_out,
                |_| {},
            ),
            None => finish(client.call("rituals", json!({})), json_out, |d| {
                let rows = d.as_array().cloned().unwrap_or_default();
                if rows.is_empty() {
                    println!("{}", paint(Ink::Dim, "The grimoire is empty. Inscribe a ritual file: summonr inscribe ./ritual.yaml"));
                }
                for r in rows {
                    println!(
                        "{} {} {}",
                        bold(Ink::Bone, &format!("{:<22}", s(&r, "ritual"))),
                        paint(Ink::Gold, &format!("v{}", r["version"])),
                        paint(
                            Ink::Dim,
                            &format!(
                                "{}{} · saved {}",
                                r["model"].as_str().unwrap_or("no model of its own"),
                                if r["baseline"] == true {
                                    " · baseline recorded"
                                } else {
                                    ""
                                },
                                fmt_ago(r["saved"].as_i64().unwrap_or(0))
                            )
                        )
                    );
                    if !s(&r, "description").is_empty() {
                        println!("  {}", paint(Ink::Dim, s(&r, "description")));
                    }
                }
            }),
        },
        "grant" | "revoke" => {
            let res = target(&op, "a ritual name").and_then(|name| {
                client.call(
                    &op.verb,
                    json!({"ritual": name, "scopes": op.scopes, "on": op.on, "into": op.into}),
                )
            });
            finish(res, json_out, |d| {
                let (key, word) = if op.verb == "grant" {
                    ("granted", "Granted")
                } else {
                    ("revoked", "Revoked")
                };
                let items = d[key].as_array().cloned().unwrap_or_default();
                if items.is_empty() {
                    println!("Nothing matched.");
                }
                for g in items {
                    println!(
                        "{word} {} {}.{} {}",
                        s(d, "ritual"),
                        s(&g, "reagent"),
                        s(&g, "action"),
                        paint(Ink::Dim, &format!("on {} into {}", g["on"], g["into"]))
                    );
                }
            })
        }
        "circle" => finish(
            client.call("list", json!({"scope": "active"})),
            json_out,
            |d| list(d, "The circle is empty. Nothing labours."),
        ),
        "identity" => finish(client.call("identity", json!({})), json_out, |d| {
            rubric("IDENTITY · this core");
            row("key", s(d, "key"));
            row("fingerprint", &keys::grouped(s(d, "id")));
            if theme::tty() {
                for line in theme::seal(s(d, "id")).into_iter().flatten() {
                    println!("              {}", paint(Ink::Gold, &line));
                }
            }
            println!(
                "{}",
                paint(
                    Ink::Dim,
                    "Give the key to another device to pin this core; compare the full fingerprint, not the seal."
                )
            );
        }),
        "graveyard" => finish(
            client.call("list", json!({"scope": "ended", "limit": 40})),
            json_out,
            |d| list(d, "No summon has ended yet."),
        ),
        "banish" => {
            let res = target(&op, "a summon ID")
                .and_then(|id| client.call("cancel", json!({"summon": id})));
            finish(res, json_out, |d| {
                println!(
                    "Banishing {}. A request already underway may finish first.",
                    s(d, "summon")
                )
            })
        }
        "revive" => {
            let res = target(&op, "a summon ID")
                .and_then(|id| client.call("revive", json!({"summon": id})))
                .and_then(|accepted| {
                    let id = s(&accepted, "summon").to_string();
                    wait_for(&mut client, &id, &op)
                });
            let code = res
                .as_ref()
                .map(|d| outcome_exit(s(d, "outcome")))
                .unwrap_or(0);
            let shown = finish(res, json_out, |d| {
                print_result(d);
                elegy_summary(d);
            });
            if shown != 0 {
                shown
            } else {
                code
            }
        }
        "recast" => {
            let res = (|| -> Result<Value, RpcErr> {
                let id = op
                    .positionals
                    .first()
                    .ok_or_else(|| RpcErr::invalid("use: recast RUN STEP [through MODEL]"))?;
                let step = op
                    .positionals
                    .get(1)
                    .ok_or_else(|| RpcErr::invalid("use: recast RUN STEP [through MODEL]"))?;
                let accepted = client.call(
                    "recast",
                    json!({"summon": id, "from_step": step, "model": op.model}),
                )?;
                let new_id = s(&accepted, "summon").to_string();
                wait_for(&mut client, &new_id, &op)
            })();
            let code = res
                .as_ref()
                .map(|d| outcome_exit(s(d, "outcome")))
                .unwrap_or(0);
            let shown = finish(res, json_out, |d| {
                print_result(d);
                elegy_summary(d);
            });
            if shown != 0 {
                shown
            } else {
                code
            }
        }
        "follow" => follow_command(&op, &mut client),
        "await" => {
            let res = target(&op, "a summon ID").and_then(|id| wait_for(&mut client, &id, &op));
            let code = res
                .as_ref()
                .map(|d| outcome_exit(s(d, "outcome")))
                .unwrap_or(0);
            let shown = finish(res, json_out, |d| {
                print_result(d);
                elegy_summary(d);
            });
            if shown != 0 {
                shown
            } else {
                code
            }
        }
        "elegy" => {
            let res = target(&op, "a summon ID").and_then(|id| {
                let mut d = client.call("get", json!({"summon": id}))?;
                d["events"] = client.call(
                    "events",
                    json!({"summon": id, "slim": !json_out, "limit": 5000}),
                )?;
                d["children"] = client.call("list", json!({"scope": "children", "summon": id}))?;
                d["petitions"] =
                    client.call("petitions", json!({"status": "all", "summon": id}))?;
                Ok(d)
            });
            finish(res, json_out, |d| {
                rubric(&format!(
                    "INCIPIT · {} {}",
                    theme::sigil(s(d, "id")),
                    s(d, "id")
                ));
                row(
                    "ritual",
                    &format!(
                        "{} · version {} · {}",
                        s(d, "ritual"),
                        d["version"],
                        s(d, "hash")
                    ),
                );
                row("model", d["model"].as_str().unwrap_or("none of its own"));
                row("inputs", &d["inputs"].to_string());
                row("essence", &d["essence"].to_string());
                if let Some(p) = d["parent"].as_str() {
                    row("lineage", &format!("called by {p}"));
                }
                for c in d["children"].as_array().into_iter().flatten() {
                    row(
                        "child",
                        &format!(
                            "{} {} ({})",
                            s(c, "id"),
                            s(c, "ritual"),
                            c["outcome"].as_str().unwrap_or("running")
                        ),
                    );
                }
                for p in d["petitions"].as_array().into_iter().flatten() {
                    row(
                        "petition",
                        &format!(
                            "{} {} {} ({})",
                            s(p, "id"),
                            s(p, "operation"),
                            p["arguments"],
                            s(p, "status")
                        ),
                    );
                }
                rubric("LEDGER");
                ledger(&d["events"], d["started"].as_i64().unwrap_or(0));
                if d["status"] == "ended" {
                    rubric("RESULT");
                    print_result(d);
                    elegy_summary(d);
                }
                println!(
                    "{}",
                    paint(
                        Ink::Dim,
                        "  Exact outgoing payloads: add `returning json` (`--json`)."
                    )
                );
            })
        }
        "ledger" => {
            let res = target(&op, "a summon ID").and_then(|id| {
                let d = client.call("get", json!({"summon": id}))?;
                let ev = client.call(
                    "events",
                    json!({"summon": id, "slim": !json_out, "limit": 5000}),
                )?;
                Ok(json!({"summon": id, "started": d["started"], "events": ev}))
            });
            finish(res, json_out, |d| {
                ledger(&d["events"], d["started"].as_i64().unwrap_or(0))
            })
        }
        "assay" => assay(&op, &mut client),
        "drill" => drill(&op, &mut client),
        "summon" => session_summon(&op, &mut client),
        "soul" => session_soul(&op, &mut client),
        "bind" => session_bind(&op, &mut client),
        "imbue" => session_imbue(&op, &mut client),
        "bid" => session_bid(&op, &mut client),
        "lethe" => session_lethe(&op, &mut client),
        "sweep" => experiment_sweep(&op, &mut client),
        "judge" => experiment_judge(&op, &mut client),
        "weigh" => experiment_weigh(&op, &mut client),
        "fence" => fence_command(&op, &mut client),
        "petition" => petition(&op, &mut client),
        "familiar" => familiar_command(&op, &mut client),
        "epitaph" => epitaph_command(&op, &mut client),
        "chain" => chain_command(&op, &mut client),
        "verify" => verify_command(&op, &mut client),
        "witness" => witness_command(&op, &mut client),
        "peer" => peer_command(&op, &mut client),
        "pact" => pact_command(&op, &mut client),
        "reliquary" => reliquary_command(&op, &mut client),
        "rite" => rite_command(&op, &mut client),
        "glean" => {
            let res = target(&op, "a ritual name").and_then(|name| {
                let count = match op.positionals.get(1) {
                    Some(n) => Some(
                        n.parse::<u64>()
                            .map_err(|_| RpcErr::invalid(format!("`{n}` is not a count")))?,
                    ),
                    None => None,
                };
                client.call(
                    "glean",
                    json!({"ritual": name, "count": count, "against": op.against, "cwd": cwd()}),
                )
            });
            finish(res, json_out, |d| {
                let cases = d["cases"].as_array().cloned().unwrap_or_default();
                println!("# {} unlabeled case(s) gleaned from real runs of {} ({} eligible, {} skipped as duplicates).", cases.len(), s(d, "ritual"), d["eligible_runs"], d["skipped_duplicates"]);
                println!("# `observed` is what the ritual answered. Fill in `expect` with what is correct,");
                println!("# then append these lines under `cases:` in your assay set. Unlabeled cases cannot be scored.");
                for c in cases {
                    println!("- {c}");
                }
            })
        }
        "satchel" => finish(client.call("tools", json!({})), json_out, |d| {
            for t in d.as_array().into_iter().flatten() {
                println!(
                    "{} {} {}",
                    bold(Ink::Bone, &format!("{:<18}", format!("{}.{}", s(t, "reagent"), s(t, "tool")))),
                    paint(Ink::Ember, &format!("{:<17}", s(t, "effect"))),
                    paint(
                        Ink::Dim,
                        &format!("{} · {}", s(t, "determinism"), s(t, "description"))
                    )
                );
            }
        }),
        other => finish(
            Err(RpcErr::invalid(format!(
                "`{other}` is not available in this version"
            ))),
            json_out,
            |_| {},
        ),
    }
}


pub(crate) fn effects(list: &Value) {
    for e in list.as_array().into_iter().flatten() {
        let ink = match s(e, "effect") {
            "read" => Ink::Verdigris,
            "idempotent write" => Ink::Ember,
            _ => Ink::Rubric,
        };
        println!(
            "  {:<38} {} {}",
            s(e, "step"),
            paint(ink, &format!("{:<17}", s(e, "effect"))),
            paint(
                Ink::Dim,
                &format!("{} · {}", s(e, "what"), s(e, "determinism"))
            )
        );
        for n in e["notes"].as_array().into_iter().flatten() {
            println!(
                "  {:<38} {}",
                "",
                paint(Ink::Dim, &format!("↳ {}", n.as_str().unwrap_or("")))
            );
        }
    }
}


pub(crate) fn short(h: &str) -> String {
    keys::grouped(&h.chars().take(16).collect::<String>())
}


#[cfg(test)]
mod result_tests {
    use super::*;
    #[test]
    fn user_arrays_and_runtime_shaped_keys_are_printed_faithfully() {
        for value in [
            json!({"amounts":[100,25],"total":125}),
            json!({"items":[1,2],"partial":{"status":"failed"}}),
            json!([{"status":"returned","outputs":{"a":1}}]),
        ] {
            assert_eq!(
                serde_json::from_str::<Value>(&format_result(&value)).unwrap(),
                value
            );
        }
    }

    #[test]
    fn core_stops_when_its_socket_is_removed() {
        let dir = std::env::temp_dir().join(format!("summonr-core-orphan-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let paths = Paths { runtime: dir.join("run"), state: dir.join("state"), config: dir.join("config") };
        std::fs::create_dir_all(&paths.runtime).unwrap();
        let core = core::Core::open(paths.clone()).unwrap();
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let _ = tx.send(core.serve());
        });
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
        while std::time::Instant::now() < deadline && std::os::unix::net::UnixStream::connect(paths.socket()).is_err() {
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        // The home is deleted under the core (a test's temp dir, a cleared
        // runtime dir): nothing can reach it any more, so it must exit.
        std::fs::remove_dir_all(&dir).unwrap();
        let ended = rx.recv_timeout(std::time::Duration::from_secs(10));
        assert!(matches!(ended, Ok(Ok(()))), "{ended:?}");
    }

    #[test]
    fn stop_running_core_reports_absent_and_waits_for_shutdown() {
        let dir = std::env::temp_dir().join(format!(
            "summonr-core-restart-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        let paths = Paths {
            runtime: dir.join("run"),
            state: dir.join("state"),
            config: dir.join("config"),
        };
        std::fs::create_dir_all(&paths.runtime).unwrap();
        assert!(!stop_running_core(&paths).unwrap());

        let core = core::Core::open(paths.clone()).unwrap();
        let handle = std::thread::spawn(move || core.serve());
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
        while std::time::Instant::now() < deadline
            && std::os::unix::net::UnixStream::connect(paths.socket()).is_err()
        {
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        assert!(std::os::unix::net::UnixStream::connect(paths.socket()).is_ok());
        assert!(stop_running_core(&paths).unwrap());
        assert!(std::os::unix::net::UnixStream::connect(paths.socket()).is_err());
        handle.join().unwrap().unwrap();
        let _ = std::fs::remove_dir_all(&dir);
    }
}
