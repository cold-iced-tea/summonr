//! Summonr: an explicit language and runtime for invoking models as bounded
//! capabilities. One binary: `summonr core` is the persistent core, and
//! every other command is a thin client of its socket.

mod window_design;
mod ipc_frame;
mod backend;
mod cancel;
mod cancellable_http;
mod command_backend;
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
    cmd_reagent, file_reagent, gh_reagent, git_reagent, http_reagent, mcp_reagent, sight_reagent,
    trial_reagent, wasm_reagent,
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
    let mut args: Vec<String> = std::env::args().skip(1).collect();
    match take_home(&mut args, &cwd()) {
        // Set before anything reads it, so a core this CLI starts inherits it.
        Ok(Some(home)) => std::env::set_var("SUMMONR_HOME", home),
        Ok(None) => {}
        Err(e) => std::process::exit(finish(Err(RpcErr::invalid(e)), args.iter().any(|a| a == "--json"), |_| {})),
    }
    let paths = Paths::resolve();
    if let Some(w) = misplaced_config(&paths) {
        eprintln!("summonr: warning: {w}");
        let _ = ENV_WARNINGS.set(vec![w]);
    }
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
    let mut op = match grammar::parse(&args) {
        Ok(op) => op,
        Err(e) => {
            std::process::exit(finish(Err(RpcErr::invalid(e.to_string())), wants_json, |_| {}));
        }
    };
    let _ = VERB.set(op.verb.clone());
    if let Err(e) = read_arg_files(&mut op) {
        std::process::exit(finish(Err(e), wants_json, |_| {}));
    }
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

/// Remove `--home PATH` (or `--home=PATH`) from the command line, wherever it
/// is, and return the home as an absolute path. Same as `SUMMONR_HOME=PATH`,
/// which it overrides.
fn take_home(args: &mut Vec<String>, cwd: &str) -> Result<Option<String>, String> {
    let mut found = None;
    let mut i = 0;
    while i < args.len() {
        let value = if args[i] == "--home" {
            let v = args.get(i + 1).filter(|v| !v.starts_with("--")).cloned()
                .ok_or("`--home` needs a directory: --home ./lab")?;
            args.drain(i..i + 2);
            v
        } else if let Some(v) = args[i].strip_prefix("--home=") {
            let v = v.to_string();
            args.remove(i);
            v
        } else {
            i += 1;
            continue;
        };
        if value.is_empty() {
            return Err("`--home` needs a directory: --home ./lab".into());
        }
        if found.replace(value).is_some() {
            return Err("give `--home` once".into());
        }
    }
    Ok(found.map(|h| {
        let p = std::path::Path::new(&h);
        if p.is_absolute() { h } else { std::path::Path::new(cwd).join(p).to_string_lossy().into_owned() }
    }))
}

/// `reading k=PATH` / `--arg-file k=PATH`: the file's text becomes input
/// `k`, exactly as if it had been typed after `given k=`. Read here, by the
/// CLI, so the request carries values, never paths the core would open.
fn read_arg_files(op: &mut grammar::Op) -> Result<(), RpcErr> {
    use std::io::Read;
    const MAX: u64 = 1_048_576;
    for (k, path) in std::mem::take(&mut op.arg_files) {
        let fail = |why: String| RpcErr::invalid(format!("input `{k}`: {path}: {why}"));
        // Check the opened handle, not the path, and bound the read itself:
        // /dev/zero, FIFOs and /proc files report size 0.
        let file = std::fs::File::open(&path).map_err(|e| fail(format!("cannot read: {e}")))?;
        let meta = file.metadata().map_err(|e| fail(format!("cannot read: {e}")))?;
        if !meta.is_file() {
            return Err(fail("not a regular file".into()));
        }
        let mut bytes = Vec::new();
        file.take(MAX + 1)
            .read_to_end(&mut bytes)
            .map_err(|e| fail(format!("cannot read: {e}")))?;
        if bytes.len() as u64 > MAX {
            return Err(fail("is over 1 MiB".into()));
        }
        let mut text = String::from_utf8(bytes).map_err(|_| fail("is not UTF-8 text".into()))?;
        // Drop one trailing newline, as $(cat FILE) would, so "5\n" is 5.
        if text.ends_with("\r\n") {
            text.truncate(text.len() - 2);
        } else if text.ends_with('\n') {
            text.truncate(text.len() - 1);
        }
        op.args.insert(k, text);
    }
    Ok(())
}

/// Warnings about the environment, added to every envelope (and stderr).
static ENV_WARNINGS: std::sync::OnceLock<Vec<String>> = std::sync::OnceLock::new();

fn env_warnings() -> Value {
    json!(ENV_WARNINGS.get().cloned().unwrap_or_default())
}

fn merged_warnings(own: Option<&Value>) -> Value {
    let mut all = own.and_then(Value::as_array).cloned().unwrap_or_default();
    all.extend(env_warnings().as_array().cloned().unwrap_or_default());
    Value::Array(all)
}

/// A `config.yaml` written beside the config directory instead of inside it
/// is silently ignored; say so.
fn misplaced_config(paths: &Paths) -> Option<String> {
    let parent = paths.config.parent()?;
    // Only in a SUMMONR_HOME layout; ~/.config/config.yaml may be another tool's.
    if paths.state.parent() != Some(parent) {
        return None;
    }
    let stray = parent.join("config.yaml");
    stray.is_file().then(|| {
        format!(
            "{} is not read; Summonr reads {}",
            stray.display(),
            paths.config.join("config.yaml").display()
        )
    })
}

/// The command being run, so an envelope can carry its run's verdict.
static VERB: std::sync::OnceLock<String> = std::sync::OnceLock::new();

/// The run verdict an envelope carries at its top level: `ok` only says the
/// request was answered, so a failed run is `ok: true`. Run, wait and record
/// replies mirror `data.outcome`; a drill is `passed` or `failed`; every
/// other command, and a run not waited on, has `null`.
fn envelope_outcome(verb: &str, data: &Value) -> Value {
    match verb {
        "invoke" | "await" | "elegy" => data.get("outcome").filter(|o| o.is_string()).cloned().unwrap_or(Value::Null),
        "drill" => match (data["failed"].as_u64(), data["touchstone_errors"].as_u64()) {
            (Some(0), Some(0) | None) => json!("passed"),
            (Some(_), _) => json!("failed"),
            _ => Value::Null,
        },
        _ => Value::Null,
    }
}

fn ok_envelope(verb: &str, data: Value, warnings: Value) -> Value {
    let outcome = envelope_outcome(verb, &data);
    json!({"schema_version": 1, "ok": true, "outcome": outcome, "data": data, "error": null, "warnings": warnings})
}

/// Print one result. JSON output is exactly one versioned envelope on stdout;
/// diagnostics go to stderr.
pub(crate) fn finish(res: Result<Value, RpcErr>, as_json: bool, human: impl FnOnce(&Value)) -> i32 {
    match res {
        Ok(data) => {
            if as_json {
                let warnings = merged_warnings(data.get("warnings"));
                println!("{}", ok_envelope(VERB.get().map(String::as_str).unwrap_or(""), data, warnings));
            } else {
                human(&data);
            }
            0
        }
        Err(e) => {
            if as_json {
                println!(
                    "{}",
                    json!({"schema_version": 1, "ok": false, "outcome": null, "data": e.data, "error": {"code": e.code, "message": e.message}, "warnings": env_warnings()})
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
        match c.get("finally") {
            Some(f) => row("finally", &finally_line(f)),
            None => row("release", &format!("{}: {}", s(c, "model"), s(c, "result"))),
        }
    }
}

/// One line for what a ritual's `finally` steps did.
pub(crate) fn finally_line(f: &Value) -> String {
    let ran: Vec<&str> = f["ran"].as_array().into_iter().flatten().filter_map(Value::as_str).collect();
    let mut line = format!(
        "{} · ran {}",
        f["outcome"].as_str().unwrap_or("?"),
        if ran.is_empty() { "nothing".to_string() } else { ran.join(", ") }
    );
    if let Some(e) = f["error"].as_str() {
        line.push_str(&format!(" · {e}"));
    }
    line
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
use cli::trust::{audit_command, chain_command, epitaph_command, epitaph_verify_command, familiar_command, key_command, operator_command, peer_command, rbac_command, verify_command, witness_command};
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
    if op.key.is_some()
        && !matches!(
            op.verb.as_str(),
            "familiar" | "peer" | "verify" | "witness" | "pact" | "audit" | "operator" | "reliquary"
        )
    {
        return finish(
            Err(RpcErr::invalid("key is valid only with familiar add, peer add, operator add, audit verify-bundle, reliquary, verify, witness accept or pact admit")),
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
    // `CMD --help` and `help CMD`: one command's contract.
    let help_for = if op.help {
        Some(op.verb.clone())
    } else if op.verb == "help" {
        op.positionals.first().cloned()
    } else {
        None
    };
    if let Some(name) = help_for {
        let verb = grammar::canonical_verb(&name).unwrap_or("");
        return match cli::help::descriptor(verb) {
            Some(d) if json_out => finish(Ok(json!({"command": d})), true, |_| {}),
            Some(_) => {
                for line in cli::help::text(verb, style).unwrap_or_default() {
                    println!("{line}");
                }
                0
            }
            None => finish(
                Err(RpcErr::not_found(format!("no command `{name}`; `summonr help` lists them"))),
                json_out,
                |_| {},
            ),
        };
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
            finish(res, json_out, |d| match d["inscribed"].as_array() {
                // A directory, children first.
                Some(all) => {
                    for r in all {
                        let state = if r["unchanged"] == true { "unchanged" } else { "inscribed" };
                        println!(
                            "  {} v{} {}",
                            bold(Ink::Bone, s(r, "ritual")),
                            r["version"],
                            paint(Ink::Dim, state)
                        );
                        print_warnings(&r["warnings"]);
                    }
                    println!("{} rituals from {}, in dependency order.", all.len(), s(d, "directory"));
                }
                None => {
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
                    print_warnings(&d["warnings"]);
                }
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
        // `allow check [RITUAL]`; a ritual named `check` is still granted with `--from`.
        "grant" if op.positionals.first().map(String::as_str) == Some("check") && op.scopes.is_empty() => {
            let ritual = op.positionals.get(1).cloned();
            let res = client.call("grant_check", json!({"ritual": ritual}));
            finish(res, json_out, |d| {
                for r in d["rituals"].as_array().into_iter().flatten() {
                    let missing = r["missing"].as_array().cloned().unwrap_or_default();
                    let state = match (missing.is_empty(), r["called_by"].as_str()) {
                        (true, _) => paint(Ink::Verdigris, "granted"),
                        (false, Some(by)) => paint(Ink::Dim, &format!("runs under {by}'s grants; needs these only if invoked directly")),
                        (false, None) => paint(Ink::Rubric, "missing"),
                    };
                    println!("{} v{} {}", bold(Ink::Bone, s(r, "ritual")), r["version"], state);
                    for (m, c) in missing.iter().zip(r["grant_commands"].as_array().into_iter().flatten()) {
                        println!("  {}", m.as_str().unwrap_or(""));
                        println!("    {}", paint(Ink::Dim, c.as_str().unwrap_or("")));
                    }
                    for t in r["scoped_by_inputs"].as_array().into_iter().flatten() {
                        println!("  {} {}", t.as_str().unwrap_or(""), paint(Ink::Dim, "is scoped by an input; checked when the ritual is run"));
                    }
                }
                if d["ready"] == true {
                    println!("Ready: every ritual that can be invoked has the authority it declares.");
                }
            })
        }
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
        "graveyard" => match &op.children {
            Some(parent) if op.recursive => finish(
                client.call("list", json!({"scope": "children", "summon": parent, "limit": 10000, "recursive": true})),
                json_out,
                |d| {
                    let rows = d.as_array().cloned().unwrap_or_default();
                    if rows.is_empty() {
                        println!("{}", paint(Ink::Dim, "That run has no child summons."));
                    }
                    for r in rows {
                        let indent = "  ".repeat(r["depth"].as_u64().unwrap_or(1).saturating_sub(1) as usize);
                        let (word, ink) = theme::state_word(s(&r, "status"), r["outcome"].as_str());
                        println!(
                            "{indent}{} {} v{} {} {}",
                            s(&r, "id"),
                            bold(Ink::Bone, s(&r, "ritual")),
                            r["version"],
                            paint(ink, word),
                            paint(Ink::Dim, &format!("{} requests · {} tokens", r["requests"], r["tokens"]))
                        );
                    }
                },
            ),
            // A big run's children (hunts, chunks) without reading its record.
            Some(parent) => finish(
                client.call("list", json!({"scope": "children", "summon": parent, "limit": 1000})),
                json_out,
                |d| list(d, "That run has no child summons."),
            ),
            None => finish(
                client.call("list", json!({"scope": "ended", "limit": 40})),
                json_out,
                |d| list(d, "No summon has ended yet."),
            ),
        },
        "banish" if op.all => {
            // Every running summon in this home; a root's children stop with it.
            let res = client.call("list", json!({"scope": "active"})).and_then(|active| {
                let mut stopped = Vec::new();
                for r in active.as_array().into_iter().flatten().filter(|r| r["parent"].is_null()) {
                    let id = s(r, "id").to_string();
                    client.call("cancel", json!({"summon": id}))?;
                    stopped.push(json!({"summon": id, "ritual": r["ritual"]}));
                }
                Ok(json!({"stopped": stopped}))
            });
            finish(res, json_out, |d| {
                let all = d["stopped"].as_array().cloned().unwrap_or_default();
                if all.is_empty() {
                    println!("Nothing is running.");
                }
                for r in &all {
                    println!("Banishing {} ({}).", s(r, "summon"), s(r, "ritual"));
                }
                if !all.is_empty() {
                    println!("Commands and plain-HTTP model requests stop at once; an HTTPS backend may keep generating.");
                }
            })
        }
        "banish" => {
            let res = target(&op, "a summon ID")
                .and_then(|id| client.call("cancel", json!({"summon": id})));
            finish(res, json_out, |d| {
                println!(
                    "Banishing {}. Commands and plain-HTTP model requests stop at once; an HTTPS backend may keep generating.",
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
                if op.brief {
                    return client.call("get", json!({"summon": id, "brief": true}));
                }
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
        "audit" => audit_command(&op, &mut client),
        "operator" => operator_command(&op, &mut client),
        "rbac" => rbac_command(&op, &mut client),
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


pub(crate) fn print_warnings(list: &Value) {
    for w in list.as_array().into_iter().flatten() {
        println!("  {} {}", paint(Ink::Ember, "warning"), w.as_str().unwrap_or(""));
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

#[cfg(test)]
mod arg_file_tests {
    use super::read_arg_files;

    #[test]
    fn reading_puts_file_text_into_args() {
        let dir = std::env::temp_dir().join(format!("summonr-argfile-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let f = dir.join("gold.json");
        std::fs::write(&f, "{\"total\": 1}\n").unwrap();
        let mut op = grammar::parse(&["run".into(), "r".into(), "--arg-file".into(), format!("gold={}", f.display())]).unwrap();
        read_arg_files(&mut op).unwrap();
        // One trailing newline is dropped, as $(cat FILE) would.
        assert_eq!(op.args["gold"], "{\"total\": 1}");
        assert!(op.arg_files.is_empty());
        let n = dir.join("n.txt");
        std::fs::write(&n, "5\r\n").unwrap();
        let mut op = grammar::parse(&["run".into(), "r".into(), "--arg-file".into(), format!("n={}", n.display())]).unwrap();
        read_arg_files(&mut op).unwrap();
        assert_eq!(op.args["n"], "5");
        std::fs::write(&n, "a\n\n").unwrap();
        let mut op = grammar::parse(&["run".into(), "r".into(), "--arg-file".into(), format!("n={}", n.display())]).unwrap();
        read_arg_files(&mut op).unwrap();
        assert_eq!(op.args["n"], "a\n", "only one newline is dropped");
        // Not a regular file: refused before any read (no endless /dev/zero).
        let mut op = grammar::parse(&["run".into(), "r".into(), "--arg-file".into(), "z=/dev/zero".into()]).unwrap();
        assert!(read_arg_files(&mut op).unwrap_err().message.contains("not a regular file"));

        let missing = dir.join("nope.json");
        let mut op = grammar::parse(&["run".into(), "r".into(), "--arg-file".into(), format!("gold={}", missing.display())]).unwrap();
        let err = read_arg_files(&mut op).unwrap_err().message;
        assert!(err.contains("gold") && err.contains("nope.json"), "{err}");

        let big = dir.join("big.json");
        std::fs::write(&big, vec![b'x'; 1_048_577]).unwrap();
        let mut op = grammar::parse(&["run".into(), "r".into(), "--arg-file".into(), format!("gold={}", big.display())]).unwrap();
        assert!(read_arg_files(&mut op).unwrap_err().message.contains("1 MiB"));
        std::fs::remove_dir_all(&dir).unwrap();
    }
    use crate::grammar;
}

#[cfg(test)]
mod misplaced_config_tests {
    #[test]
    fn config_yaml_beside_config_dir_is_reported() {
        let home = std::env::temp_dir().join(format!("summonr-misplaced-{}", std::process::id()));
        let paths = crate::util::Paths {
            runtime: home.join("run"),
            state: home.join("state"),
            config: home.join("config"),
        };
        std::fs::create_dir_all(&paths.config).unwrap();
        assert_eq!(super::misplaced_config(&paths), None);
        std::fs::write(home.join("config.yaml"), "repos: {}\n").unwrap();
        let w = super::misplaced_config(&paths).unwrap();
        assert!(w.contains("is not read") && w.contains("config/config.yaml"), "{w}");
        std::fs::write(paths.config.join("config.yaml"), "repos: {}\n").unwrap();
        assert!(super::misplaced_config(&paths).is_some(), "still worth saying while both exist");
        let xdg = crate::util::Paths {
            runtime: home.join("run"),
            state: home.join("share/summonr"),
            config: home.join("config"),
        };
        assert_eq!(super::misplaced_config(&xdg), None, "XDG layout: not ours to judge");
        std::fs::remove_dir_all(&home).unwrap();
    }
}

#[cfg(test)]
mod envelope_tests {
    use super::*;

    #[test]
    fn a_runs_verdict_is_at_the_top_of_its_envelope_and_ok_stays_transport() {
        let failed = json!({"id": "s-1", "status": "ended", "outcome": "failed", "error": "boom"});
        for verb in ["invoke", "await", "elegy"] {
            let e = ok_envelope(verb, failed.clone(), json!([]));
            assert_eq!((e["ok"].clone(), e["outcome"].clone(), e["schema_version"].clone()), (json!(true), json!("failed"), json!(1)), "{verb}");
            assert_eq!(e["data"], failed, "data is unchanged");
        }
        // A run not waited on has no verdict yet.
        assert_eq!(ok_envelope("invoke", json!({"id": "s-1", "status": "running"}), json!([]))["outcome"], Value::Null);
        assert_eq!(ok_envelope("drill", json!({"passed": 3, "failed": 0, "touchstone_errors": 0}), json!([]))["outcome"], "passed");
        assert_eq!(ok_envelope("drill", json!({"passed": 2, "failed": 1, "touchstone_errors": 0}), json!([]))["outcome"], "failed");
        assert_eq!(ok_envelope("drill", json!({"passed": 2, "failed": 0, "touchstone_errors": 1}), json!([]))["outcome"], "failed");
        // Other commands carry the key, as null, even when data has an `outcome`.
        let e = ok_envelope("graveyard", json!([{"outcome": "failed"}]), json!([]));
        assert!(e.as_object().unwrap().contains_key("outcome") && e["outcome"].is_null());
        assert!(ok_envelope("petition", json!({"outcome": "x"}), json!([]))["outcome"].is_null());
    }
}

#[cfg(test)]
mod home_flag_tests {
    use super::take_home;

    fn v(s: &str) -> Vec<String> {
        s.split(' ').map(String::from).collect()
    }

    #[test]
    fn home_is_taken_from_anywhere_and_made_absolute() {
        let mut a = v("--home lab run r --json");
        assert_eq!(take_home(&mut a, "/w").unwrap().as_deref(), Some("/w/lab"));
        assert_eq!(a, v("run r --json"));
        let mut a = v("mcp --home=/abs/lab --name codex");
        assert_eq!(take_home(&mut a, "/w").unwrap().as_deref(), Some("/abs/lab"));
        assert_eq!(a, v("mcp --name codex"));
        let mut a = v("core status");
        assert_eq!(take_home(&mut a, "/w").unwrap(), None);
        assert_eq!(a, v("core status"));
        for bad in ["run r --home", "--home --json run r", "--home= run r", "--home a --home b run r"] {
            assert!(take_home(&mut v(bad), "/w").is_err(), "{bad}");
        }
    }
}
