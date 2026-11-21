//! Summonr: an explicit language and runtime for invoking models as bounded
//! capabilities. One binary: `summonr core` is the persistent core, and
//! every other command is a thin client of its socket.

mod backend;
mod cassette;
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
mod requests;
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
fn finish(res: Result<Value, RpcErr>, as_json: bool, human: impl FnOnce(&Value)) -> i32 {
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

fn rubric(s: &str) {
    println!(
        "{}",
        bold(Ink::Rubric, &format!("{} {s}", theme::ornament()))
    );
}

fn row(key: &str, value: &str) {
    println!("  {} {value}", paint(Ink::Dim, &format!("{key:<10}")));
}

fn s<'a>(v: &'a Value, k: &str) -> &'a str {
    v[k].as_str().unwrap_or("")
}

fn target(op: &Op, what: &str) -> Result<String, RpcErr> {
    op.positionals
        .first()
        .cloned()
        .ok_or_else(|| RpcErr::invalid(format!("this command needs {what}")))
}

fn cwd() -> String {
    std::env::current_dir()
        .map(|p| p.to_string_lossy().into_owned())
        .unwrap_or_else(|_| ".".into())
}

fn outcome_exit(outcome: &str) -> i32 {
    outcome
        .parse::<outcome::Outcome>()
        .map(|o| o.exit_code())
        .unwrap_or(11)
}

fn elegy_summary(d: &Value) {
    let outcome = d["outcome"].as_str().unwrap_or("?");
    let (word, ink) = theme::state_word(s(d, "status"), d["outcome"].as_str());
    println!(
        "{} {} {}",
        bold(Ink::Rubric, &format!("{} ELEGY", theme::ornament())),
        paint(ink, &format!("{word} ({outcome})")),
        paint(
            Ink::Dim,
            &format!(
                "· {} · {} requests · {} tokens",
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

fn print_result(d: &Value) {
    if !d["result"].is_null() {
        println!("{}", format_result(&d["result"]));
    }
}

fn format_result(result: &Value) -> String {
    // User-owned JSON never identifies an internal batch, regardless of its keys.
    serde_json::to_string_pretty(result).expect("JSON values always serialize")
}

fn list(rows: &Value, empty: &str) {
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

fn ledger(events: &Value, started: i64) {
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

/// Offline compilation: never starts a daemon or contacts a backend.
fn validate_file(op: &Op, paths: &Paths) -> Result<Value, RpcErr> {
    let file = target(op, "ritual file")?;
    let path = std::path::Path::new(&file);
    let text = std::fs::read_to_string(path).map_err(|e| {
        if e.kind() == std::io::ErrorKind::NotFound {
            RpcErr::not_found(format!("{file}: {e}"))
        } else {
            RpcErr::invalid(format!("{file}: {e}"))
        }
    })?;
    let mut def = ritual::parse_definition(path, &text).map_err(RpcErr::invalid)?;
    let db = if paths.db().exists() {
        Some(
            rusqlite::Connection::open_with_flags(
                paths.db(),
                rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
            )
            .map_err(|e| RpcErr::invalid(format!("cannot read saved dependencies: {e}")))?,
        )
    } else {
        None
    };
    let resolve = |name: &str, version: Option<u32>| -> Option<ritual::Resolved> {
        let db = db.as_ref()?;
        let row: (u32,String,String) = db.query_row("SELECT version,hash,def FROM rituals WHERE name=?1 AND (?2 IS NULL OR version=?2) ORDER BY version DESC LIMIT 1",rusqlite::params![name,version], |r|Ok((r.get(0)?,r.get(1)?,r.get(2)?))).ok()?;
        Some((row.0, row.1, serde_json::from_str(&row.2).ok()?))
    };
    ritual::prepare(
        &mut def,
        path.parent().unwrap_or(std::path::Path::new(".")),
        &resolve,
    )
    .map_err(RpcErr::invalid)?;
    let cfg = Config::load(paths).map_err(RpcErr::invalid)?;
    let effects = ritual::validate_with_cfg(&def, &resolve, &cfg, cfg.max_depth())
        .map_err(|errors| RpcErr::invalid("definition does not satisfy its contract").with(json!({"diagnostics": errors.into_iter().map(|message|json!({"code":"invalid_definition","file":file,"message":message})).collect::<Vec<_>>()})))?;
    Ok(
        json!({"valid":true,"language_version":def.language_version,"hash":ritual::content_hash(&def),"effects":effects,"definition":def}),
    )
}

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

fn stop_running_core(paths: &Paths) -> Result<bool, RpcErr> {
    if std::os::unix::net::UnixStream::connect(paths.socket()).is_err() {
        return Ok(false);
    }
    Client::connect_existing(paths)?.call("shutdown", json!({}))?;
    let wait_until = std::time::Instant::now() + std::time::Duration::from_secs(5);
    while std::time::Instant::now() < wait_until
        && std::os::unix::net::UnixStream::connect(paths.socket()).is_ok()
    {
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
    if std::os::unix::net::UnixStream::connect(paths.socket()).is_ok() {
        return Err(RpcErr::new(
            "core_unavailable",
            8,
            "core did not stop in time",
        ));
    }
    Ok(true)
}

fn restart_core(paths: &Paths) -> Result<Value, RpcErr> {
    let was_running = stop_running_core(paths)?;
    let mut ping = Client::connect(paths)?.call("ping", json!({}))?;
    if let Some(obj) = ping.as_object_mut() {
        obj.insert("restarted".into(), json!(true));
        obj.insert("was_running".into(), json!(was_running));
    }
    Ok(ping)
}

fn core_command(op: &Op, paths: &Paths) -> i32 {
    match op.positionals.first().map(String::as_str) {
        None => match core::Core::open(paths.clone()).and_then(|c| c.serve()) {
            Ok(()) => 0,
            Err(e) => {
                eprintln!("summonr core: {e}");
                8
            }
        },
        Some("stop") => match stop_running_core(paths) {
            Ok(false) => finish(Ok(json!({"running":false})), op.json, |_| {
                println!("No core is running.")
            }),
            Ok(true) => finish(Ok(json!({"stopping":true})), op.json, |_| {
                println!("The core is stopping.")
            }),
            Err(e) => finish(Err(e), op.json, |_| {}),
        },
        Some("restart") => finish(restart_core(paths), op.json, |d| {
            if d["was_running"].as_bool().unwrap_or(false) {
                println!(
                    "The core restarted: pid {}, socket {}.",
                    d["pid"],
                    s(d, "socket")
                )
            } else {
                println!(
                    "The core started: pid {}, socket {}.",
                    d["pid"],
                    s(d, "socket")
                )
            }
        }),
        Some("status") => {
            if std::os::unix::net::UnixStream::connect(paths.socket()).is_err() {
                return finish(Ok(json!({"running":false})), op.json, |_| {
                    println!("No core is running. Any command starts one.")
                });
            }
            let res = Client::connect(paths).and_then(|mut c| c.call("ping", json!({})));
            finish(res, op.json, |d| {
                println!(
                    "The core keeps vigil: pid {}, {} active, socket {}.",
                    d["pid"],
                    d["active"],
                    s(d, "socket")
                )
            })
        }
        Some(other) => finish(
            Err(RpcErr::invalid(format!(
                "unknown core operation `{other}`; use `core`, `core status`, `core stop`, or `core restart`"
            ))),
            op.json,
            |_| {},
        ),
    }
}

fn effects(list: &Value) {
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

fn wait_for(client: &mut Client, id: &str, op: &Op) -> Result<Value, RpcErr> {
    let timeout = match &op.timeout {
        Some(t) => Some(util::parse_duration(t).map_err(RpcErr::invalid)?),
        None => None,
    };
    if op.json || !theme::tty() {
        return client.call("wait", json!({"summon": id, "timeout_ms": timeout}));
    }
    // In a terminal, show the summon at work while waiting.
    let started = std::time::Instant::now();
    let mut tick = 0u64;
    loop {
        let d = client.call("get", json!({"summon": id}))?;
        if d["status"] == "ended" {
            print!("\r\x1b[2K");
            return Ok(d);
        }
        if timeout.is_some_and(|t| started.elapsed().as_millis() as u64 >= t) {
            print!("\r\x1b[2K");
            return Err(RpcErr::new(
                "timeout",
                7,
                format!("stopped waiting for `{id}`; the summon is still running"),
            ));
        }
        let progress = match d["total"].as_i64().unwrap_or(0) {
            0 => String::new(),
            t => format!(" · {}/{t}", d["done"]),
        };
        print!(
            "\r\x1b[2K  {} {} {}",
            paint(Ink::Ember, theme::phase(tick)),
            paint(Ink::Gold, &theme::sigil(id)),
            paint(
                Ink::Dim,
                &format!(
                    "{id} labours{progress} · {} · {} tokens",
                    fmt_dur(started.elapsed().as_millis() as i64),
                    d["tokens"]
                )
            )
        );
        use std::io::Write;
        std::io::stdout().flush().ok();
        tick += 1;
        std::thread::sleep(std::time::Duration::from_millis(150));
    }
}

fn invoke(op: &Op, client: &mut Client) -> i32 {
    let name = match target(op, "a ritual name or file") {
        Ok(n) => n,
        Err(e) => return finish(Err(e), op.json, |_| {}),
    };
    let args: serde_json::Map<String, Value> = op
        .args
        .iter()
        .map(|(k, v)| (k.clone(), Value::String(v.clone())))
        .collect();
    let absolute = |r: &String| {
        let p = std::path::Path::new(r);
        if p.is_absolute() {
            r.clone()
        } else {
            std::path::Path::new(&cwd())
                .join(p)
                .to_string_lossy()
                .into_owned()
        }
    };
    let record = op.record.as_ref().map(absolute);
    let replay = op.replay.as_ref().map(absolute);
    let params = json!({"target": name, "version": op.version, "model": op.model, "settings": op.settings, "args": args, "cwd": cwd(), "record": record, "replay": replay, "simulate": op.simulate});
    if op.dry_run {
        if op.record.is_some() || op.replay.is_some() || op.simulate {
            return finish(
                Err(RpcErr::invalid(
                    "recording, replaying and simulating cannot be combined with rehearse or --dry-run",
                )),
                op.json,
                |_| {},
            );
        }
        return finish(client.call("rehearse", params), op.json, |d| {
            rubric(&format!(
                "REHEARSAL · {} version {}",
                s(d, "ritual"),
                d["version"]
            ));
            row("hash", s(d, "hash"));
            row("inputs", &d["inputs"].to_string());
            row("model", d["model"].as_str().unwrap_or("none of its own"));
            row("essence", &d["essence"].to_string());
            for b in d["bindings"].as_array().into_iter().flatten() {
                row(
                    "binding",
                    &format!(
                        "{} · {} chars · {}",
                        s(b, "name"),
                        b["chars"],
                        &s(b, "hash")[..12.min(s(b, "hash").len())]
                    ),
                );
            }
            rubric("EFFECTS");
            effects(&d["effects"]);
            rubric("AUTHORITY");
            let missing = d["authority"]["missing"]
                .as_array()
                .cloned()
                .unwrap_or_default();
            if missing.is_empty() {
                println!(
                    "  {}",
                    paint(Ink::Verdigris, "every required permission is granted")
                );
            }
            for m in missing {
                println!(
                    "  {} {}.{} on {} into {}",
                    paint(Ink::Rubric, "missing"),
                    s(&m, "reagent"),
                    s(&m, "action"),
                    m["on"],
                    m["into"]
                );
            }
            for w in d["warnings"].as_array().into_iter().flatten() {
                println!(
                    "  {} {}",
                    paint(Ink::Ember, "warning"),
                    w.as_str().unwrap_or("")
                );
            }
            if let Some(r) = d["refused"].as_str() {
                println!("  {} {r}", paint(Ink::Rubric, "would refuse"));
            }
            println!("{}", paint(Ink::Dim, &format!("  {}", s(d, "note"))));
        });
    }
    let accepted = match client.call("invoke", params) {
        Ok(a) => a,
        Err(e) => return finish(Err(e), op.json, |_| {}),
    };
    let id = s(&accepted, "summon").to_string();
    if !op.wait {
        return finish(Ok(accepted), op.json, |d| {
            println!(
                "Summoned {} for {} version {}. It labours.",
                bold(Ink::Bone, &id),
                s(d, "ritual"),
                d["version"]
            );
            for w in d["warnings"].as_array().into_iter().flatten() {
                println!(
                    "  {} {}",
                    paint(Ink::Ember, "warning"),
                    w.as_str().unwrap_or("")
                );
            }
            println!(
                "{}",
                paint(
                    Ink::Dim,
                    &format!("  watch: summonr scry    wait: summonr await {id} then return")
                )
            );
        });
    }
    if !op.json {
        for w in accepted["warnings"].as_array().into_iter().flatten() {
            eprintln!(
                "  {} {}",
                paint(Ink::Ember, "warning"),
                w.as_str().unwrap_or("")
            );
        }
    }
    let res = wait_for(client, &id, op).map(|mut d| {
        d["warnings"] = accepted["warnings"].clone();
        d
    });
    let code = res
        .as_ref()
        .map(|d| outcome_exit(s(d, "outcome")))
        .unwrap_or(0);
    let shown = finish(res, op.json, |d| {
        print_result(d);
        elegy_summary(d);
    });
    if shown != 0 {
        shown
    } else {
        code
    }
}

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

fn session_summon(op: &Op, client: &mut Client) -> i32 {
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

fn session_soul(op: &Op, client: &mut Client) -> i32 {
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

fn session_bind(op: &Op, client: &mut Client) -> i32 {
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

fn session_imbue(op: &Op, client: &mut Client) -> i32 {
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

fn session_bid(op: &Op, client: &mut Client) -> i32 {
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

fn session_lethe(op: &Op, client: &mut Client) -> i32 {
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

fn experiment_sweep(op: &Op, client: &mut Client) -> i32 {
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

fn experiment_judge(op: &Op, client: &mut Client) -> i32 {
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

fn experiment_weigh(op: &Op, client: &mut Client) -> i32 {
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

fn fence_command(op: &Op, client: &mut Client) -> i32 {
    let sub = op.positionals.first().map(String::as_str);
    let res = match sub {
        None | Some("list") => client.call(
            "fence_list",
            json!({"summon": op.positionals.get(1)}),
        ),
        Some("apply") => match op.positionals.get(1) {
            Some(k) => client.call("fence_resolve", json!({"fence_key": k, "action": "applied"})),
            None => Err(RpcErr::invalid("`fence apply` needs a fence key")),
        },
        Some("clear") => match op.positionals.get(1) {
            Some(k) => client.call("fence_resolve", json!({"fence_key": k, "action": "clear"})),
            None => Err(RpcErr::invalid("`fence clear` needs a fence key")),
        },
        Some(other) => Err(RpcErr::invalid(format!(
            "unknown fence action `{other}`; expected list, apply, or clear"
        ))),
    };
    finish(res, op.json, |d| {
        if let Some(arr) = d.as_array() {
            if arr.is_empty() {
                println!("no effect fences recorded.");
            } else {
                for row in arr {
                    println!(
                        "{}  {}  {}  {}  {}",
                        s(row, "fence_key"),
                        s(row, "step"),
                        s(row, "operation"),
                        s(row, "state"),
                        row["error"].as_str().unwrap_or("")
                    );
                }
            }
        } else {
            println!(
                "fence {} -> {}",
                s(d, "fence_key"),
                s(d, "state")
            );
        }
    })
}

/// Exit 14 when any drill case failed or its touchstone could not answer;
/// 0 when all passed.
fn drill(op: &Op, client: &mut Client) -> i32 {
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

fn assay(op: &Op, client: &mut Client) -> i32 {
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

fn help(style: Style) {
    println!(
        "{}",
        bold(Ink::Rubric, &format!("{} SUMMONR", theme::sun()))
    );
    println!("An explicit language and runtime for invoking models as bounded capabilities.\n");
    rubric("COMMANDS");
    for (ritual, plain, _, meaning) in grammar::VERBS {
        let (lead, other) = if style == Style::Flag && !plain.is_empty() {
            (*plain, *ritual)
        } else {
            (*ritual, *plain)
        };
        println!(
            "  {} {} {meaning}",
            bold(Ink::Bone, &format!("{lead:<12}")),
            paint(Ink::Dim, &format!("{other:<12}"))
        );
    }
    println!();
    rubric("CLAUSES · each has two spellings that mean the same thing");
    for (kw, flag, meaning) in grammar::CLAUSES {
        let (lead, other) = if style == Style::Flag {
            (*flag, *kw)
        } else {
            (*kw, *flag)
        };
        println!(
            "  {:<20} {} {meaning}",
            lead,
            paint(Ink::Dim, &format!("{other:<18}"))
        );
    }
    println!();
    rubric("EXAMPLE");
    let example: Vec<String> = "invoke inbox-triage given count=10 then return json within 5m"
        .split(' ')
        .map(String::from)
        .collect();
    if let Ok(op) = grammar::parse(&example) {
        println!(
            "  summonr {}",
            grammar::render(&op, Style::Keyword).join(" ")
        );
        println!("  summonr {}", grammar::render(&op, Style::Flag).join(" "));
    }
    println!(
        "\n{}",
        paint(
            Ink::Dim,
            "Run `summonr` for the command workbench; `summonr scry` opens the observer. Closing either leaves work running."
        )
    );
}

fn short(h: &str) -> String {
    keys::grouped(&h.chars().take(16).collect::<String>())
}

fn epitaph_command(op: &Op, client: &mut Client) -> i32 {
    let p = &op.positionals;
    match (p.first().map(String::as_str), p.len()) {
        (Some("prove"), 2) => finish(
            client.call("epitaph_prove", json!({"run": p[1]})),
            op.json,
            |d| {
                rubric(&format!("MERKLE PROOF · {}", s(d, "run")));
                row("seq", &format!("#{} of {}", d["seq"], d["count"]));
                row("leaf", s(d, "leaf_hash"));
                row("merkle_root", s(d, "merkle_root"));
                row("steps", &format!("{}", d["proof"].as_array().map(Vec::len).unwrap_or(0)));
            },
        ),
        (Some("export"), 4) => {
            let (from, to) = match (p[1].parse::<i64>(), p[2].parse::<i64>()) {
                (Ok(f), Ok(t)) => (f, t),
                _ => return finish(Err(RpcErr::invalid("use: epitaph export FROM TO FILE")), op.json, |_| {}),
            };
            let res = client.call("epitaph_export", json!({"from": from, "to": to})).and_then(|d| {
                std::fs::write(&p[3], serde_json::to_string_pretty(&d).unwrap())
                    .map_err(|e| RpcErr::new("storage_failed", 11, format!("cannot write {}: {e}", p[3])))?;
                Ok(json!({"file": p[3], "count": d["epitaphs"].as_array().map(Vec::len)}))
            });
            finish(res, op.json, |d| println!("wrote {} epitaph(s) to {}", d["count"], s(d, "file")))
        }
        (Some(run), 3) if p[1] == "reveal" => finish(
            client.call("epitaph_reveal", json!({"run": run, "part": p[2]})),
            op.json,
            |d| println!("{}", serde_json::to_string_pretty(d).unwrap()),
        ),
        (Some(run), 1) => finish(client.call("epitaph_get", json!({"run": run})), op.json, |d| {
            let body: Value = serde_json::from_str(s(d, "body")).unwrap_or_default();
            let link: Value = serde_json::from_str(s(d, "link")).unwrap_or_default();
            rubric(&format!(
                "EPITAPH · {} v{} · {}",
                s(&body["ritual"], "name"),
                body["ritual"]["version"],
                s(&body, "outcome")
            ));
            let by = match s(&body, "caller") {
                "" => "the owner".to_string(),
                other => other.to_string(),
            };
            row("run", &format!("{}   by {by}", s(&body, "run")));
            row("chain", &format!("#{}   after {}", d["seq"], short(s(&link, "prev"))));
            let events = body["fingerprints"]["events"]["count"].as_u64().unwrap_or(0);
            let children = body["children"].as_array().map(Vec::len).unwrap_or(0);
            row("contents", &format!("inputs, result and {events} event(s); {children} child run(s) — fingerprints only"));
            let key = keys::PublicKey::parse(s(d, "core_key")).map(|k| k.id()).unwrap_or_default();
            let mark = if d["verifies"] == true { paint(Ink::Verdigris, "✓ verifies") } else { paint(Ink::Rubric, "✕ does not verify") };
            row("signed by", &format!("{}   {mark}", short(&key)));
            if theme::tty() {
                for line in theme::seal(s(d, "hash")).into_iter().flatten() {
                    println!("              {}", paint(Ink::Gold, &line));
                }
            }
        }),
        _ => finish(
            Err(RpcErr::invalid("use: epitaph RUN | epitaph RUN reveal PART | epitaph prove RUN | epitaph verify FILE | epitaph export FROM TO FILE")),
            op.json,
            |_| {},
        ),
    }
}

fn chain_command(op: &Op, client: &mut Client) -> i32 {
    let marking = match op.positionals.as_slice() {
        [] => false,
        [m] if m == "mark" => true,
        _ => return finish(Err(RpcErr::invalid("use: chain | chain mark")), op.json, |_| {}),
    };
    finish(client.call("chain_head", json!({})), op.json, |d| {
        if d["seq"] == 0 {
            println!("{}", paint(Ink::Dim, "The chain is empty: no run has ended since epitaphs began."));
            return;
        }
        if marking {
            println!("{}  {}", s(d, "mark"), paint(Ink::Dim, &crate::util::now_iso()));
            println!("{}", paint(Ink::Dim, "  Paste this anywhere other people can see it: an email, a chat, a notebook."));
            return;
        }
        rubric("CHAIN · this core");
        row("head", &format!("#{}", d["seq"]));
        row("hash", &keys::grouped(s(d, "hash")));
        if theme::tty() {
            for line in theme::seal(s(d, "hash")).into_iter().flatten() {
                println!("              {}", paint(Ink::Gold, &line));
            }
        }
    })
}

fn witness_command(op: &Op, client: &mut Client) -> i32 {
    let p = &op.positionals;
    match (p.first().map(String::as_str), p.len()) {
        (Some("accept"), 2) => match &op.key {
            Some(k) => finish(client.call("witness_accept", json!({"name": p[1], "key": k})), op.json, |d| {
                rubric(&format!("WARD · {}", s(d, "name")));
                show_fingerprint(s(d, "id"));
                println!("{}", paint(Ink::Gold, COMPARE));
            }),
            None => finish(Err::<Value, _>(RpcErr::invalid("give the ward core's key: witness accept NAME key ed25519:…")), op.json, |_| {}),
        },
        (Some("list"), 1) => finish(client.call("witness_list", json!({})), op.json, |d| {
            let list = d.as_array().cloned().unwrap_or_default();
            if list.is_empty() {
                println!("{}", paint(Ink::Dim, "This core witnesses no one. Admit a core: summonr witness accept NAME key K"));
            }
            for w in list {
                let held = if w["seq"] == 0 { "not yet witnessed".to_string() } else { format!("witnessed through #{} {}", w["seq"], fmt_ago(w["time"].as_i64().unwrap_or(0))) };
                let forks = match w["forks"].as_i64().unwrap_or(0) {
                    0 => String::new(),
                    n => paint(Ink::Rubric, &format!("  {n} fork(s): this ward offered a rewritten history")),
                };
                println!("  {} {}{forks}", bold(Ink::Bone, s(&w, "name")), paint(Ink::Dim, &held));
            }
        }),
        (Some("drop"), 2) => finish(client.call("witness_drop", json!({"name": p[1]})), op.json, |d| println!("ward {} dropped", s(d, "name"))),
        (Some("tsa"), 2) => finish(client.call("witness_tsa", json!({"url": p[1]})), op.json, |d| {
            println!("{} {} timestamped head #{}", paint(Ink::Verdigris, "✓"), s(d, "url"), d["seq"])
        }),
        (Some("compare"), n) if n >= 1 => {
            let peers: Vec<&str> = p[1..].iter().map(String::as_str).collect();
            let args = if peers.is_empty() {
                json!({"live": op.live})
            } else {
                json!({"peers": peers, "live": op.live})
            };
            let res = client.call("witness_compare", args);
            let ok = res.as_ref().map(|d| d["ok"] == true).unwrap_or(false);
            let no_witnesses = res
                .as_ref()
                .map(|d| d["witnesses"].as_u64() == Some(0))
                .unwrap_or(false);
            let code = finish(res, op.json, |d| {
                if d["witnesses"].as_u64() == Some(0) {
                    println!("{} no witnesses to compare", paint(Ink::Gold, "?"));
                } else if d["ok"] == true {
                    println!("{} witnesses agree (no split views detected)", paint(Ink::Verdigris, "✓"));
                } else {
                    println!("{} witness comparison failed (split_view={}, invalid={})", paint(Ink::Rubric, "✕"), d["split_view"], d["invalid"]);
                    for c in d["conflicts"].as_array().into_iter().flatten() {
                        println!("  #{} {} ({}) vs {} ({})", c["seq"], c["a"], c["hash_a"], c["b"], c["hash_b"]);
                    }
                }
            });
            if code != 0 {
                code
            } else if no_witnesses {
                13
            } else if ok {
                0
            } else {
                15
            }
        }
        (Some(peer), 1) => finish(client.call("witness_to", json!({"peer": peer})), op.json, |d| {
            println!("{} {} witnessed head #{}", paint(Ink::Verdigris, "✓"), s(d, "peer"), d["seq"])
        }),
        _ => finish(
            Err::<Value, _>(RpcErr::invalid("use: witness PEER | witness accept NAME key K | witness list | witness drop NAME | witness tsa URL | witness compare [PEER...] [--live]")),
            op.json,
            |_| {},
        ),
    }
}

/// The key a file must verify against: `key K`, or this core's own key or a
/// pinned peer's, never one the file merely claims.
fn trusted_key(op: &Op, client: &mut Client, claimed: &str) -> Result<keys::PublicKey, RpcErr> {
    if let Some(k) = &op.key {
        return keys::PublicKey::parse(k).map_err(RpcErr::invalid);
    }
    let me = client.call("identity", json!({}))?;
    let peers = client.call("peer_list", json!({}))?;
    let pinned = std::iter::once(me["key"].clone()).chain(peers.as_array().into_iter().flatten().map(|p| p["key"].clone()));
    if pinned.into_iter().any(|k| k.as_str() == Some(claimed)) {
        return keys::PublicKey::parse(claimed).map_err(RpcErr::invalid);
    }
    Err(RpcErr::invalid("this file is from a core you haven't pinned; give its key: verify FILE key ed25519:…"))
}

fn verify_command(op: &Op, client: &mut Client) -> i32 {
    let p = &op.positionals;
    let failed = |why: String, json: bool| -> i32 {
        let shown = finish(Err::<Value, _>(RpcErr::new("verification_failed", 15, why)), json, |_| {});
        if shown == 0 { 15 } else { shown }
    };
    // This core's own chain, optionally checking a mark.
    if p.is_empty() || (p.len() == 1 && p[0].starts_with("summonr-mark:")) {
        let mut args = json!({});
        if let Some(m) = p.first() {
            args["mark"] = json!(m);
        }
        if let Some(ca) = &op.tsa_ca {
            args["tsa_ca"] = json!(ca);
        }
        let res = client.call("chain_verify", args);
        let ok = res.as_ref().map(|d| d["ok"] == true).unwrap_or(false);
        let code = finish(res, op.json, |d| {
            match &d["problem"] {
                Value::Null => println!("{} {} epitaph(s) verify", paint(Ink::Verdigris, "✓"), d["checked"]),
                pr => println!("{} epitaph {}: {}", paint(Ink::Rubric, "✕"), pr["seq"], s(pr, "reason")),
            }
            for run in d["not_checkable"].as_array().into_iter().flatten() {
                println!("  {} {}: its elegy is gone, so its contents can't be re-checked", paint(Ink::Gold, "?"), run.as_str().unwrap_or(""));
            }
            match (&d["mark"]["added_since"], d["mark"]["error"].as_str()) {
                (Value::Number(n), _) => println!("{} the chain still contains this mark; {n} epitaph(s) added since, none altered", paint(Ink::Verdigris, "✓")),
                (_, Some(e)) => println!("{} {e}", paint(Ink::Rubric, "✕")),
                _ => {}
            }
            for w in d["witnesses"].as_array().into_iter().flatten() {
                println!("  witness {}: last countersigned #{} {}", s(w, "peer"), w["seq"], fmt_ago(w["time"].as_i64().unwrap_or(0)));
                if w["valid"] == false {
                    println!("    {} countersignature does not verify under the pinned witness key", paint(Ink::Rubric, "✕"));
                }
                if w["matches"] == false {
                    println!("    {} it countersigned a different epitaph #{}: this history was rewritten after it was witnessed", paint(Ink::Rubric, "✕"), w["seq"]);
                }
            }
            for t in d["timestamps"].as_array().into_iter().flatten() {
                println!("  timestamp #{} from {}: {}", t["seq"], s(t, "url"), s(t, "verdict"));
                if t["matches"] == false {
                    println!("    {} it timestamped a different epitaph #{}: this history was rewritten after it was timestamped", paint(Ink::Rubric, "✕"), t["seq"]);
                }
            }
        });
        return if code != 0 { code } else if ok { 0 } else { 15 };
    }
    // A file: an export (optionally with a mark) or a reveal.
    let file: Value = match std::fs::read_to_string(&p[0]).map_err(|e| e.to_string()).and_then(|t| serde_json::from_str(&t).map_err(|e| e.to_string())) {
        Ok(v) => v,
        Err(e) => return finish(Err::<Value, _>(RpcErr::invalid(format!("{}: {e}", p[0]))), op.json, |_| {}),
    };
    if file["kind"] == "pact-thread" {
        let path = std::fs::canonicalize(&p[0]).map(|x| x.display().to_string()).unwrap_or_else(|_| p[0].clone());
        let res = client.call("pact_verify", json!({"file": path, "key": op.key}));
        let ok = res.as_ref().map(|d| d["ok"] == true).unwrap_or(false);
        let code = finish(res, op.json, |d| {
            if d["ok"] == true {
                println!("{} the pact thread verifies: {} entries, {} call(s), {} delivery epitaph(s) checked", paint(Ink::Verdigris, "✓"), d["entries"], d["calls"], d["epitaphs_checked"]);
            } else if let Some(f) = d["forks"].as_array().filter(|f| !f.is_empty()) {
                println!("{} {} fork(s): a core signed two different entries on the same previous entry — signed evidence that it equivocated", paint(Ink::Rubric, "✕"), f.len());
            } else {
                println!("{} entry {}: {}", paint(Ink::Rubric, "✕"), d["problem"]["entry"], s(&d["problem"], "reason"));
            }
        });
        return if code != 0 { code } else if ok { 0 } else { 15 };
    }
    if file.get("reveal").is_some() {
        let e = &file["epitaph"];
        let key = match trusted_key(op, client, s(e, "core_key")) {
            Ok(k) => k,
            Err(err) => return finish(Err::<Value, _>(err), op.json, |_| {}),
        };
        let checked = epitaph::check_link(s(e, "link"), s(e, "sig"), &key)
            .and_then(|l| epitaph::check_body(&l, s(e, "body")))
            .and_then(|b| epitaph::verify_reveal(&b, &file["reveal"]));
        return match checked {
            Ok(()) => finish(Ok(json!({"ok": true})), op.json, |_| {
                println!("{} {} of {} matches its epitaph", paint(Ink::Verdigris, "✓"), s(&file["reveal"], "part"), s(&file["reveal"], "run"))
            }),
            Err(why) => failed(why, op.json),
        };
    }
    let key = match trusted_key(op, client, s(&file, "core_key")) {
        Ok(k) => k,
        Err(err) => return finish(Err::<Value, _>(err), op.json, |_| {}),
    };
    match epitaph::verify_export(&file, &key) {
        Err((seq, why)) => failed(format!("epitaph {seq}: {why}"), op.json),
        Ok(last) => match p.get(1) {
            None => finish(Ok(json!({"ok": true, "last": last})), op.json, |_| {
                println!("{} the exported chain verifies through #{last}", paint(Ink::Verdigris, "✓"))
            }),
            Some(m) => match epitaph::parse_mark(m).ok_or("not a chain mark".to_string()).and_then(|mk| epitaph::verify_mark(&file, &mk)) {
                Ok(n) => finish(Ok(json!({"ok": true, "added_since": n})), op.json, |_| {
                    println!("{} the chain still contains this mark; {n} epitaph(s) added since, none altered", paint(Ink::Verdigris, "✓"))
                }),
                Err(why) => failed(why, op.json),
            },
        },
    }
}

/// The full fingerprint for comparing by eye, with its seal on a terminal.
fn show_fingerprint(id: &str) {
    row("fingerprint", &keys::grouped(id));
    if theme::tty() {
        for line in theme::seal(id).into_iter().flatten() {
            println!("              {}", paint(Ink::Gold, &line));
        }
    }
}

const COMPARE: &str = "Compare this fingerprint with the other device before relying on it.";

fn familiar_command(op: &Op, client: &mut Client) -> i32 {
    let p = &op.positionals;
    let name = p.get(1).cloned().unwrap_or_default();
    let res = match (p.first().map(String::as_str), p.len()) {
        (None | Some("list"), 0 | 1) => {
            return finish(client.call("familiar_list", json!({})), op.json, |d| {
                let list = d.as_array().cloned().unwrap_or_default();
                if list.is_empty() {
                    println!("{}", paint(Ink::Dim, "No familiars. Create one: summonr familiar new NAME"));
                }
                for x in list {
                    let state = if x["revoked"].is_null() { "" } else { " (revoked)" };
                    println!("  {} {}{}", bold(Ink::Bone, s(&x, "name")), paint(Ink::Dim, s(&x, "kind")), paint(Ink::Rubric, state));
                    println!("      {}", paint(Ink::Dim, &keys::grouped(s(&x, "id"))));
                    if theme::tty() {
                        for line in theme::seal(s(&x, "id")).into_iter().flatten() {
                            println!("      {}", paint(Ink::Gold, &line));
                        }
                    }
                    println!("      allowed: {}", x["allowed"].as_array().map(|a| a.iter().filter_map(Value::as_str).collect::<Vec<_>>().join(", ")).filter(|s| !s.is_empty()).unwrap_or_else(|| "nothing".into()));
                }
            });
        }
        (Some("new"), 2) => client.call("familiar_new", json!({"name": name})),
        (Some("add"), 2) => match &op.key {
            Some(k) => client.call("familiar_add", json!({"name": name, "key": k})),
            None => Err(RpcErr::invalid("give the public key: familiar add NAME key ed25519:…")),
        },
        (Some("register-grant"), 2) => {
            match std::fs::read_to_string(&p[1])
                .map_err(|e| RpcErr::invalid(format!("cannot read {}: {e}", p[1])))
                .and_then(|t| {
                    serde_json::from_str::<Value>(&t)
                        .map_err(|e| RpcErr::invalid(format!("invalid grant JSON: {e}")))
                }) {
                Ok(parsed) => {
                    let grant = parsed.get("data").cloned().unwrap_or(parsed);
                    let mut args = json!({"grant": grant});
                    if let Some(k) = &op.key {
                        args["root_key"] = json!(k);
                    }
                    client.call("familiar_register_grant", args)
                }
                Err(e) => Err(e),
            }
        }
        (Some(verb @ ("allow" | "disallow")), n) if n >= 3 || (n == 2 && op.pact.is_some()) => client.call(
            if verb == "allow" { "familiar_allow" } else { "familiar_disallow" },
            json!({"name": name, "rituals": &p[2..], "pacts": op.pact.iter().collect::<Vec<_>>()}),
        ),
        (Some("revoke"), 2) => client.call("familiar_revoke", json!({"name": name})),
        _ => Err(RpcErr::invalid(
            "use: familiar [list] | new NAME | add NAME key K | allow NAME RITUAL... | disallow NAME RITUAL... | revoke NAME",
        )),
    };
    let verb = p.first().cloned().unwrap_or_default();
    finish(res, op.json, |d| match verb.as_str() {
        "new" => {
            rubric(&format!("FAMILIAR · {}", s(d, "name")));
            row("key", s(d, "key"));
            show_fingerprint(s(d, "id"));
            row("key file", s(d, "key_file"));
            println!("{}", paint(Ink::Dim, "It may call nothing yet: summonr familiar allow NAME RITUAL..."));
        }
        "add" => {
            rubric(&format!("FAMILIAR · {} (remote)", s(d, "name")));
            show_fingerprint(s(d, "id"));
            println!("{}", paint(Ink::Gold, COMPARE));
        }
        "register-grant" => {
            rubric(&format!("FAMILIAR · {} (delegated)", s(d, "name")));
            row("root", s(d, "root_key_id"));
            row("allowed", &d["allowed"].to_string());
        }
        "allow" | "disallow" => println!("{} may call: {}", s(d, "name"), d["allowed"]),
        _ => println!("{} revoked; its next request will be refused.", s(d, "name")),
    })
}

fn peer_command(op: &Op, client: &mut Client) -> i32 {
    let p = &op.positionals;
    let name = p.get(1).cloned().unwrap_or_default();
    let res = match (p.first().map(String::as_str), p.len()) {
        (None | Some("list"), 0 | 1) => {
            return finish(client.call("peer_list", json!({})), op.json, |d| {
                let list = d.as_array().cloned().unwrap_or_default();
                if list.is_empty() {
                    println!("{}", paint(Ink::Dim, "No peers pinned."));
                }
                for x in list {
                    println!("  {} {}", bold(Ink::Bone, s(&x, "name")), s(&x, "url"));
                    println!("      {}", paint(Ink::Dim, &keys::grouped(s(&x, "id"))));
                }
            });
        }
        (Some("add"), 2) => match (&op.url, &op.key) {
            (Some(u), Some(k)) => client.call("peer_add", json!({"name": name, "url": u, "key": k})),
            _ => Err(RpcErr::invalid("give both: peer add NAME url URL key ed25519:…")),
        },
        (Some("remove"), 2) => client.call("peer_remove", json!({"name": name})),
        _ => Err(RpcErr::invalid("use: peer [list] | add NAME url URL key K | remove NAME")),
    };
    let adding = p.first().map(String::as_str) == Some("add");
    finish(res, op.json, |d| {
        if adding {
            rubric(&format!("PEER · {} at {}", s(d, "name"), s(d, "url")));
            show_fingerprint(s(d, "id"));
            println!("{}", paint(Ink::Gold, COMPARE));
        } else {
            println!("peer {} removed", s(d, "name"));
        }
    })
}

fn petition(op: &Op, client: &mut Client) -> i32 {
    let action = op.positionals.first().map(String::as_str);
    match action {
        Some("request") => {
            let res = op
                .positionals
                .get(1)
                .ok_or_else(|| RpcErr::invalid("`petition request` needs a ritual name"))
                .and_then(|ritual| {
                    client.call(
                        "petition_request",
                        json!({"ritual": ritual, "args": op.args, "cwd": cwd()}),
                    )
                });
            finish(res, op.json, |d| {
                let pids = d["petitions"].as_array().cloned().unwrap_or_default();
                if pids.is_empty() {
                    println!("{} already has all required authority.", s(d, "ritual"));
                } else {
                    println!(
                        "filed {} authority petition(s) for {}: {}",
                        pids.len(),
                        s(d, "ritual"),
                        pids.iter().filter_map(Value::as_str).collect::<Vec<_>>().join(", ")
                    );
                }
            })
        }
        None | Some("list") => finish(client.call("petitions", json!({"status": "pending"})), op.json, |d| {
            let list = d.as_array().cloned().unwrap_or_default();
            if list.is_empty() {
                println!("{}", paint(Ink::Dim, "No petitions await. Nothing is held."));
                return;
            }
            // The digest first: what is being asked, in bulk.
            let mut groups: std::collections::BTreeMap<String, usize> = Default::default();
            for p in &list {
                let dest = p["arguments"].as_object().map(|a| a.iter().filter(|(k, _)| *k != "message").map(|(k, v)| format!("{k}={}", v.as_str().unwrap_or(&v.to_string()))).collect::<Vec<_>>().join(" ")).unwrap_or_default();
                *groups.entry(format!("{} · {} {dest}", s(p, "ritual"), s(p, "operation"))).or_default() += 1;
            }
            rubric(&format!("{} PETITIONS AWAIT", list.len()));
            for (g, n) in &groups {
                println!("  {} {g}", paint(Ink::Gold, &format!("{n:>3} ×")));
            }
            println!();
            for p in &list {
                let why = p["because"].as_object().and_then(|b| b.values().next()).map(|v| v["reason"].as_str().map(String::from).unwrap_or_else(|| v.to_string())).unwrap_or_default();
                println!(
                    "  {} {} {} {}",
                    bold(Ink::Bone, s(p, "id")),
                    paint(Ink::Ember, s(p, "operation")),
                    p["arguments"],
                    paint(Ink::Dim, &format!("· {} · {}", s(p, "summon"), fmt_ago(p["created"].as_i64().unwrap_or(0))))
                );
                match p["digest"].as_str() {
                    Some(d) => {
                        println!("             {}", paint(Ink::Dim, &format!("bound to {}", &d[..d.len().min(12)])));
                        // Seals are for a person at a terminal, never for parsing.
                        if theme::tty() {
                            for row in theme::seal(d).into_iter().flatten() {
                                println!("             {}", paint(Ink::Gold, &row));
                            }
                        }
                    }
                    None => println!("             {}", paint(Ink::Dim, "not bound to a resource version")),
                }
                if !why.is_empty() {
                    println!("             {}", paint(Ink::Dim, &why));
                }
            }
            println!("{}", paint(Ink::Dim, "\n  permit: summonr petition permit ID...   or   summonr petition permit all\n  deny:   summonr petition deny ID...     or   summonr petition deny all"));
        }),
        Some(verb @ ("permit" | "deny")) => {
            let ids: Vec<&String> = op.positionals.iter().skip(1).collect();
            let res = client.call("petition_decide", json!({"permit": verb == "permit", "ids": ids, "all": op.all}));
            // A permitted action that failed or could not be verified is not a success.
            let count = |states: &[&str]| res.as_ref().ok().map(|d| d["decided"].as_array().into_iter().flatten().filter(|x| states.contains(&s(x, "status"))).count()).unwrap_or(0);
            let bad = count(&["failed", "unverified"]);
            // A stale petition was refused because its resource changed: a conflict.
            let stale = count(&["stale"]);
            let shown = finish(res, op.json, |d| {
                let list = d["decided"].as_array().cloned().unwrap_or_default();
                if list.is_empty() {
                    println!("Nothing was pending.");
                }
                for x in list {
                    let (mark, ink) = match s(&x, "status") {
                        "applied" => ("✓", Ink::Verdigris),
                        "denied" => ("⊘", Ink::Dim),
                        "stale" => ("≠", Ink::Gold),
                        _ => ("✕", Ink::Rubric),
                    };
                    let detail = match x["result"]["evidence"]["class"].as_str() {
                        Some("observed by the runtime") => format!("observed {} by {}", x["result"]["evidence"]["observed"], s(&x["result"]["evidence"], "by")),
                        Some(other) => other.to_string(),
                        None => x["result"]["error"].as_str().unwrap_or("").to_string(),
                    };
                    println!("  {} {} {} {} {}", paint(ink, mark), s(&x, "id"), paint(ink, s(&x, "status")), s(&x, "operation"), paint(Ink::Dim, &format!("{} {detail}", x["arguments"])));
                    if let Some(w) = x["warning"].as_str() {
                        println!("      {}", paint(Ink::Dim, w));
                    }
                }
            });
            if shown != 0 { shown } else if bad > 0 { 11 } else if stale > 0 { 5 } else { 0 }
        }
        Some(other) => finish(Err(RpcErr::invalid(format!("unknown petition operation `{other}`; use `petition`, `petition permit`, or `petition deny`"))), op.json, |_| {}),
    }
}

fn job_command(op: &Op, client: &mut Client) -> i32 {
    let result = (|| -> Result<Value, RpcErr> {
        // Job options reuse the paired given/--arg grammar, but are not ritual inputs.
        let verb = op.positionals.first().map(String::as_str).unwrap_or("");
        let allowed: &[&str] = match verb {
            "work" => &["count", "lease"],
            "status" => &["offset"],
            _ => &[],
        };
        if op.args.keys().any(|k| !allowed.contains(&k.as_str())) {
            return Err(RpcErr::invalid("unknown option for this job operation"));
        }
        if op.model.is_some()
            || !op.settings.is_empty()
            || op.wait
            || op.version.is_some()
            || op.dry_run
            || op.replace
            || op.name.is_some()
            || op.against.is_some()
            || op.key.is_some()
            || op.url.is_some()
            || op.via.is_some()
            || op.baseline
            || op.live
            || op.simulate
            || op.all
            || !op.scopes.is_empty()
            || !op.on.is_empty()
            || !op.into.is_empty()
        {
            return Err(RpcErr::invalid(
                "job accepts positionals, --json, and its documented --arg options only",
            ));
        }
        let expected = if matches!(verb, "item" | "retry") {
            3
        } else {
            2
        };
        if op.positionals.len() != expected {
            return Err(RpcErr::invalid(
                "use job create FILE | work NAME | status NAME | item NAME ITEM | retry NAME ITEM",
            ));
        }
        let target = &op.positionals[1];
        let mut params = json!({"job":target});
        let method = match verb {
            "create" => {
                let file =
                    std::fs::File::open(target).map_err(|e| RpcErr::invalid(e.to_string()))?;
                use std::io::Read;
                let mut bytes = Vec::new();
                file.take(8 * 1024 * 1024 + 1)
                    .read_to_end(&mut bytes)
                    .map_err(|e| RpcErr::invalid(e.to_string()))?;
                if bytes.len() > 8 * 1024 * 1024 {
                    return Err(RpcErr::invalid("job manifest exceeds 8 MiB"));
                }
                params = serde_json::from_slice(&bytes)
                    .map_err(|e| RpcErr::invalid(format!("job manifest must be JSON: {e}")))?;
                "job_create"
            }
            "work" => {
                let count = op
                    .args
                    .get("count")
                    .map(|v| v.parse::<u64>())
                    .transpose()
                    .map_err(|_| RpcErr::invalid("count must be an integer"))?
                    .unwrap_or(1);
                let lease =
                    util::parse_duration(op.args.get("lease").map(String::as_str).unwrap_or("5m"))
                        .map_err(RpcErr::invalid)?;
                params["count"] = json!(count);
                params["lease_ms"] = json!(lease);
                "job_work"
            }
            "status" => {
                let offset = op
                    .args
                    .get("offset")
                    .map(|v| v.parse::<u64>())
                    .transpose()
                    .map_err(|_| RpcErr::invalid("offset must be an integer"))?
                    .unwrap_or(0);
                params["offset"] = json!(offset);
                "job_status"
            }
            "item" | "retry" => {
                params["item"] = json!(op.positionals[2]);
                if verb == "item" {
                    "job_item"
                } else {
                    "job_retry"
                }
            }
            _ => return Err(RpcErr::invalid("unknown job operation")),
        };
        client.call(method, params)
    })();
    finish(result, op.json, |v| {
        println!("{}", serde_json::to_string_pretty(v).unwrap())
    })
}

fn pact_command(op: &Op, client: &mut Client) -> i32 {
    let p = &op.positionals;
    let arg = |i: usize| p.get(i).cloned().unwrap_or_default();
    let call_no = |i: usize| p.get(i).and_then(|c| c.parse::<i64>().ok());
    let abs = |f: &str| std::fs::canonicalize(f).map(|x| x.display().to_string()).unwrap_or_else(|_| f.to_string());
    let usage = "use: pact list | offer PEER FILE [as NAME] | fetch PEER | show P | agree P | decline P [because T] | call P given k=v... [wait] | status P CALL | accept P CALL | reject P CALL [because T] | history P | export P FILE | admit NAME key K | unadmit NAME | limit P k=v... | end P [because T] | poll";
    let res = match (p.first().map(String::as_str), p.len()) {
        (None | Some("list"), 0 | 1) => client.call("pact_list", json!({})),
        (Some("offer"), 3) => client.call("pact_offer", json!({"peer": arg(1), "file": abs(&arg(2)), "name": op.name})),
        (Some("fetch"), 2) => client.call("pact_fetch", json!({"peer": arg(1)})),
        (Some("show"), 2) => client.call("pact_show", json!({"pact": arg(1)})),
        (Some("agree"), 2) => client.call("pact_agree", json!({"pact": arg(1)})),
        (Some("decline"), 2) => client.call("pact_decline", json!({"pact": arg(1), "reason": op.reason})),
        (Some("call"), 2) | (Some("call"), 3) if p.get(2).is_none_or(|w| w == "wait") => {
            let wait = op.wait || p.get(2).is_some_and(|w| w == "wait");
            client.call("pact_call", json!({"pact": arg(1), "args": op.args})).and_then(|started| {
                if wait {
                    client.call("pact_call_status", json!({"pact": arg(1), "call": started["call"], "wait_ms": 300_000}))
                } else {
                    Ok(started)
                }
            })
        }
        (Some("status"), 3) | (Some("status"), 4) if p.get(3).is_none_or(|w| w == "wait") => {
            let wait = op.wait || p.get(3).is_some_and(|w| w == "wait");
            client.call("pact_call_status", json!({"pact": arg(1), "call": call_no(2), "wait_ms": if wait { Some(300_000) } else { None }}))
        }
        (Some("accept"), 3) => client.call("pact_accept", json!({"pact": arg(1), "call": call_no(2)})),
        (Some("reject"), 3) => client.call("pact_reject", json!({"pact": arg(1), "call": call_no(2), "reason": op.reason})),
        (Some("history"), 2) => client.call("pact_history", json!({"pact": arg(1)})),
        (Some("export"), 3) => client.call("pact_export", json!({"pact": arg(1), "file": abs(&arg(2))})),
        (Some("admit"), 2) => match &op.key {
            Some(k) => client.call("pact_admit", json!({"name": arg(1), "key": k})),
            None => Err(RpcErr::invalid("give the counterparty core's key: pact admit NAME key ed25519:…")),
        },
        (Some("unadmit"), 2) => client.call("pact_unadmit", json!({"name": arg(1)})),
        (Some("limit"), n) if n >= 3 => {
            let mut limits = serde_json::Map::new();
            let mut bad = None;
            for kv in &p[2..] {
                match kv.split_once('=') {
                    Some(("time", v)) => {
                        limits.insert("time".into(), json!(v));
                    }
                    Some((k, v)) => match v.parse::<u64>() {
                        Ok(n) => {
                            limits.insert(k.into(), json!(n));
                        }
                        Err(_) => bad = Some(format!("`{kv}`: limits are whole numbers, or a duration for time")),
                    },
                    None => bad = Some(format!("`{kv}`: write limits as name=value, such as tool_calls=3")),
                }
            }
            match bad {
                Some(b) => Err(RpcErr::invalid(b)),
                None => client.call("pact_limit", json!({"pact": arg(1), "limits": limits})),
            }
        }
        (Some("end"), 2) => client.call("pact_end", json!({"pact": arg(1), "reason": op.reason})),
        (Some("poll"), 1) => client.call("pact_poll", json!({})),
        _ => Err(RpcErr::invalid(usage)),
    };
    let verb = p.first().cloned().unwrap_or_else(|| "list".into());
    finish(res, op.json, |d| match verb.as_str() {
        "list" => {
            let list = d.as_array().cloned().unwrap_or_default();
            if list.is_empty() {
                println!("{}", paint(Ink::Dim, "No pacts. Offer one: summonr pact offer PEER terms.yaml"));
            }
            for x in list {
                println!("  {} {} with {} — {}{}", bold(Ink::Bone, s(&x, "name")), paint(Ink::Dim, s(&x, "side")), s(&x, "peer"), s(&x, "state"),
                    if x["calls_open"].as_i64().unwrap_or(0) > 0 { format!(", {} call(s) open", x["calls_open"]) } else { String::new() });
            }
        }
        "show" => {
            rubric(&format!("PACT · {} · {}", s(d, "name"), s(d, "state")));
            row("with", &format!("{} (this core is the {})", s(d, "peer"), s(d, "side")));
            row("id", s(d, "id"));
            row("ritual", &format!("{} {}", s(&d["terms"]["ritual"], "name"), s(&d["terms"]["ritual"], "hash")));
            row("accept", s(&d["terms"], "accept"));
            row("entries", &d["entries"].to_string());
            for e in d["ritual"]["effects"].as_array().into_iter().flatten() {
                println!("    {} {} ({})", s(e, "step"), s(e, "what"), s(e, "effect"));
            }
            for c in d["calls"].as_array().into_iter().flatten() {
                println!("  call {} — {}", c["call"], s(c, "state"));
            }
            for w in d["warnings"].as_array().into_iter().flatten() {
                println!("  {} {}", paint(Ink::Gold, "!"), w.as_str().unwrap_or(""));
            }
        }
        "history" => {
            for e in d["entries"].as_array().into_iter().flatten() {
                let mark = if e["verified"] == true { paint(Ink::Verdigris, "✓") } else { paint(Ink::Rubric, "✕") };
                let call = e["call"].as_i64().map(|c| format!(" call {c}")).unwrap_or_default();
                println!("  {mark} #{} {}{} by {}", e["n"], s(e, "kind"), call, s(e, "by"));
            }
            for f in d["forks"].as_array().into_iter().flatten() {
                println!("  {} fork seen {}", paint(Ink::Rubric, "✕"), fmt_ago(f["seen"].as_i64().unwrap_or(0)));
            }
        }
        _ => println!("{}", serde_json::to_string_pretty(d).unwrap_or_default()),
    })
}

fn epitaph_verify_command(op: &Op) -> i32 {
    let res = (|| -> Result<Value, RpcErr> {
        let path = op
            .positionals
            .get(1)
            .ok_or_else(|| RpcErr::invalid("use: epitaph verify FILE (or `-` for stdin)"))?;
        let text = if path == "-" {
            use std::io::Read;
            let mut buf = String::new();
            std::io::stdin()
                .read_to_string(&mut buf)
                .map_err(|e| RpcErr::invalid(format!("stdin: {e}")))?;
            buf
        } else {
            std::fs::read_to_string(path)
                .map_err(|e| RpcErr::invalid(format!("cannot read {path}: {e}")))?
        };
        let parsed: Value = serde_json::from_str(&text)
            .map_err(|e| RpcErr::invalid(format!("invalid proof JSON: {e}")))?;
        let data = parsed.get("data").unwrap_or(&parsed);
        let leaf_hash = data["leaf_hash"]
            .as_str()
            .ok_or_else(|| RpcErr::invalid("proof JSON missing `leaf_hash`"))?;
        let root = data["merkle_root"]
            .as_str()
            .ok_or_else(|| RpcErr::invalid("proof JSON missing `merkle_root`"))?;
        let proof: Vec<epitaph::MerkleStep> = serde_json::from_value(data["proof"].clone())
            .map_err(|e| RpcErr::invalid(format!("invalid `proof` array: {e}")))?;
        let ok = epitaph::verify_merkle_proof(leaf_hash, root, &proof);
        if !ok {
            return Err(RpcErr::new(
                "verification_failed",
                15,
                "Merkle inclusion proof does not match merkle_root",
            )
            .with(json!({"ok": false, "leaf_hash": leaf_hash, "merkle_root": root})));
        }
        Ok(json!({
            "ok": true,
            "run": data["run"],
            "seq": data["seq"],
            "leaf_hash": leaf_hash,
            "merkle_root": root,
            "steps": proof.len(),
        }))
    })();
    finish(res, op.json, |d| {
        println!(
            "{} Merkle proof verified for {} (#{}, {} step(s))",
            paint(Ink::Verdigris, "✓"),
            s(d, "run"),
            d["seq"],
            d["steps"]
        );
    })
}

fn key_command(op: &Op) -> i32 {
    let p = &op.positionals;
    let res = match (p.first().map(String::as_str), p.len()) {
        (Some("init-root"), 2) => (|| -> Result<Value, RpcErr> {
            let path = std::path::Path::new(&p[1]);
            let kp = keys::generate();
            keys::save_new(path, &kp)
                .map_err(|e| RpcErr::new("storage_failed", 11, e))?;
            let pub_path = path.with_extension("pub");
            let pub_text = kp.public().text();
            let _ = std::fs::write(&pub_path, format!("{pub_text}\n"));
            Ok(json!({
                "key_file": p[1],
                "pub_file": pub_path.display().to_string(),
                "key": pub_text,
                "id": kp.public().id(),
            }))
        })(),
        (Some("sign-grant"), 4) => (|| -> Result<Value, RpcErr> {
            let root_kp = keys::load(std::path::Path::new(&p[1]))
                .map_err(|e| RpcErr::invalid(format!("cannot load root key {}: {e}", p[1])))?;
            let familiar_name = p[2].clone();
            let familiar_pub = keys::PublicKey::parse(&p[3]).map_err(RpcErr::invalid)?;
            let split_list = |key: &str| -> Vec<String> {
                op.args
                    .get(key)
                    .map(|v| {
                        v.split(',')
                            .map(str::trim)
                            .filter(|s| !s.is_empty())
                            .map(String::from)
                            .collect()
                    })
                    .unwrap_or_default()
            };
            let rituals = split_list("rituals");
            let pacts = split_list("pacts");
            let now = now_ms();
            let expires_at_ms = match op.args.get("expires_days") {
                Some(raw) => {
                    let days: i64 = raw
                        .parse()
                        .map_err(|_| RpcErr::invalid("expires_days must be an integer"))?;
                    if days <= 0 {
                        return Err(RpcErr::invalid("expires_days must be positive"));
                    }
                    Some(now + days * 86_400_000)
                }
                None => None,
            };
            let grant = keys::FamiliarGrant::sign(
                &root_kp,
                familiar_name,
                &familiar_pub,
                rituals,
                pacts,
                now,
                expires_at_ms,
            );
            let val = serde_json::to_value(&grant).map_err(|e| RpcErr::invalid(e.to_string()))?;
            if let Some(out) = op.args.get("out") {
                std::fs::write(out, serde_json::to_string_pretty(&val).unwrap())
                    .map_err(|e| RpcErr::new("storage_failed", 11, format!("cannot write {out}: {e}")))?;
            }
            Ok(val)
        })(),
        _ => Err(RpcErr::invalid(
            "use: key init-root FILE | key sign-grant ROOT_KEY FAMILIAR PUBKEY [given rituals=a,b pacts=p expires_days=30 out=grant.json]",
        )),
    };
    finish(res, op.json, |d| {
        if d.get("key_file").is_some() {
            rubric("ROOT KEY");
            row("key file", s(d, "key_file"));
            row("pub file", s(d, "pub_file"));
            row("public key", s(d, "key"));
            row("id", s(d, "id"));
        } else {
            println!("{}", serde_json::to_string_pretty(d).unwrap_or_default());
        }
    })
}

fn follow_command(op: &Op, client: &mut Client) -> i32 {
    let id = match target(op, "a summon ID") {
        Ok(id) => id,
        Err(e) => return finish(Err(e), op.json, |_| {}),
    };
    let mut after: i64 = 0;
    let mut started: Option<i64> = None;
    const BATCH: usize = 200;
    loop {
        let info = match client.call("get", json!({"summon": id})) {
            Ok(d) => d,
            Err(e) => return finish(Err(e), op.json, |_| {}),
        };
        if started.is_none() {
            started = info["started"].as_i64();
        }
        loop {
            let ev = match client.call("events", json!({"summon": id, "after": after, "limit": BATCH})) {
                Ok(e) => e,
                Err(e) => return finish(Err(e), op.json, |_| {}),
            };
            let count = ev.as_array().map(Vec::len).unwrap_or(0);
            if let Some(arr) = ev.as_array() {
                for item in arr {
                    if let Some(seq) = item["seq"].as_i64() {
                        after = after.max(seq);
                    }
                    if op.json {
                        println!("{}", item);
                    } else {
                        let (mark, ink) = theme::event_mark(s(item, "kind"));
                        println!(
                            "  {} {} {}",
                            paint(
                                Ink::Dim,
                                &fmt_offset(item["ts"].as_i64().unwrap_or(0) - started.unwrap_or(0))
                            ),
                            paint(ink, mark),
                            s(item, "text")
                        );
                    }
                }
            }
            if count < BATCH {
                break;
            }
        }
        if info["status"].as_str() == Some("ended") {
            return outcome_exit(s(&info, "outcome"));
        }
        std::thread::sleep(std::time::Duration::from_millis(250));
    }
}

fn demo_command(op: &Op) -> i32 {
    struct Cleanup(std::path::PathBuf);
    impl Drop for Cleanup {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    let res = (|| -> Result<Value, RpcErr> {
        let root = std::env::temp_dir().join(format!(
            "summonr-demo-{}-{}",
            std::process::id(),
            now_ms()
        ));
        let _cleanup = Cleanup(root.clone());
        let home = root.join("home");
        let repo = root.join("repo");
        let origin = root.join("origin.git");
        let worktree = root.join("feat-demo");
        let paths = Paths {
            runtime: home.join("run"),
            state: home.join("state"),
            config: home.join("config"),
        };
        std::fs::create_dir_all(&paths.config)
            .map_err(|e| RpcErr::new("storage_failed", 11, e.to_string()))?;
        std::fs::create_dir_all(&paths.state)
            .map_err(|e| RpcErr::new("storage_failed", 11, e.to_string()))?;
        std::fs::create_dir_all(&paths.runtime)
            .map_err(|e| RpcErr::new("storage_failed", 11, e.to_string()))?;

        let git = |dir: &std::path::Path, args: &[&str]| -> Result<String, RpcErr> {
            let out = std::process::Command::new("git")
                .args(["-c", "user.name=Summonr Demo", "-c", "user.email=demo@summonr.local"])
                .args(args)
                .current_dir(dir)
                .output()
                .map_err(|e| RpcErr::invalid(format!("git failed: {e}")))?;
            if !out.status.success() {
                return Err(RpcErr::invalid(format!(
                    "git {} failed: {}",
                    args.join(" "),
                    String::from_utf8_lossy(&out.stderr)
                )));
            }
            Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
        };

        std::fs::create_dir_all(&origin)
            .map_err(|e| RpcErr::new("storage_failed", 11, e.to_string()))?;
        git(&origin, &["init", "--bare", "-b", "main"])?;
        std::fs::create_dir_all(&repo)
            .map_err(|e| RpcErr::new("storage_failed", 11, e.to_string()))?;
        git(&repo, &["init", "-b", "main"])?;
        git(&repo, &["remote", "add", "origin", &origin.to_string_lossy()])?;
        std::fs::write(repo.join("README.md"), "# Playground Repo\n")
            .map_err(|e| RpcErr::new("storage_failed", 11, e.to_string()))?;
        git(&repo, &["add", "README.md"])?;
        git(&repo, &["commit", "-m", "initial commit"])?;
        git(&repo, &["push", "-u", "origin", "main"])?;
        git(
            &repo,
            &["worktree", "add", "-b", "feat/demo", &worktree.to_string_lossy()],
        )?;
        std::fs::write(worktree.join("hello.txt"), "hello from summonr demo\n")
            .map_err(|e| RpcErr::new("storage_failed", 11, e.to_string()))?;

        let cfg = serde_yaml::to_string(&json!({
            "repos": {
                "playground": {
                    "path": repo.to_string_lossy(),
                    "trunk": "main",
                }
            }
        }))
        .unwrap();
        std::fs::write(paths.config.join("config.yaml"), cfg)
            .map_err(|e| RpcErr::new("storage_failed", 11, e.to_string()))?;

        let core = core::Core::open(paths)
            .map_err(|e| RpcErr::new("storage_failed", 11, e))?;

        let model = op.model.clone().unwrap_or_else(|| "mock/keyword".to_string());
        let ritual_path = root.join("demo-commit-work.yaml");
        let ritual_yaml = format!(
            r#"ritual: demo-commit-work
description: Inspect a feature worktree, draft a commit message, and hold git.commit for human approval.
model: {model}
inputs:
  repo: string
  branch: string
reagents:
  git: {{select: [worktree_for, status, commit]}}
requires:
  - {{reagent: git, action: worktree_for}}
  - {{reagent: git, action: status}}
  - {{reagent: git, action: commit}}
essence: {{requests: 1, tokens: 400, tool_calls: 10, time: 30s}}
steps:
  - id: tree
    operation: git.worktree_for
    arguments: {{repo: "${{inputs.repo}}", branch: "${{inputs.branch}}"}}
  - id: state
    operation: git.status
    arguments: {{repo: "${{inputs.repo}}", worktree: "${{steps.tree.output.path}}"}}
  - id: draft
    model:
      instructions: Draft a concise one-line commit message for these repository changes.
      prompt: "${{steps.state.output}}"
      schema:
        message: string
  - id: commit
    operation: git.commit
    hold: true
    arguments: {{repo: "${{inputs.repo}}", branch: "${{inputs.branch}}", message: "${{steps.draft.output.message}}"}}
return:
  commit: "${{steps.commit.output}}"
  message: "${{steps.draft.output.message}}"
"#
        );
        std::fs::write(&ritual_path, ritual_yaml)
            .map_err(|e| RpcErr::new("storage_failed", 11, e.to_string()))?;
        core.dispatch("inscribe", json!({"path": ritual_path.to_string_lossy()}))?;
        core.dispatch(
            "grant",
            json!({
                "ritual": "demo-commit-work",
                "scopes": [{"reagent": "git", "select": ["worktree_for", "status", "commit"]}],
            }),
        )?;

        let inv = core.dispatch(
            "invoke",
            json!({
                "target": "demo-commit-work",
                "model": model,
                "args": {"repo": "playground", "branch": "feat/demo"},
            }),
        )?;
        let summon_id = s(&inv, "summon").to_string();
        let ended = core.dispatch("wait", json!({"summon": summon_id, "timeout_ms": 10000}))?;
        let pending = core.dispatch("petitions", json!({"status": "pending", "summon": summon_id}))?;
        let petition = pending
            .as_array()
            .and_then(|a| a.first())
            .cloned()
            .ok_or_else(|| RpcErr::invalid("demo did not produce a held git.commit petition"))?;
        let pid = s(&petition, "id").to_string();

        let decided = core.dispatch(
            "petition_decide",
            json!({"ids": [pid], "permit": true}),
        )?;
        let log = git(&worktree, &["log", "-1", "--oneline"])?;
        let verified = core.dispatch("chain_verify", json!({}))?;
        drop(core);

        Ok(json!({
            "ephemeral_root": root.to_string_lossy(),
            "cleaned_up": true,
            "model": model,
            "summon": summon_id,
            "outcome": ended["outcome"],
            "held_petition": {
                "id": pid,
                "operation": petition["operation"],
                "digest": petition["digest"],
                "arguments": petition["arguments"],
            },
            "after_permit": {
                "status": decided["status"],
                "git_head": log,
            },
            "chain_verified": verified["ok"],
        }))
    })();

    finish(res, op.json, |d| {
        rubric("EPHEMERAL GIT PLAYGROUND");
        row("model", s(d, "model"));
        row("summon", &format!("{} ({})", s(d, "summon"), s(d, "outcome")));
        row("held petition", &format!("{} · {}", s(&d["held_petition"], "id"), s(&d["held_petition"], "operation")));
        row("after permit", s(&d["after_permit"], "git_head"));
        row("epitaph chain", &format!("verified = {}", d["chain_verified"]));
        row("cleanup", "disposable /tmp repo + SUMMONR_HOME removed");
    })
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
