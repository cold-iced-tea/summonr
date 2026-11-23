use crate::grammar::{self, Op, Style};
use crate::rpc::{Client, RpcErr};
use crate::theme::{self, bold, paint, Ink};
use crate::util::{Config, Paths};
use crate::{core, finish, ritual, rubric, s, target};
use serde_json::{json, Value};

/// Offline compilation: never starts a daemon or contacts a backend.
pub(crate) fn validate_file(op: &Op, paths: &Paths) -> Result<Value, RpcErr> {
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
        .map_err(|errors| RpcErr::invalid(ritual::contract_message(&errors)).with(json!({"diagnostics": errors.into_iter().map(|message|json!({"code":"invalid_definition","file":file,"message":message})).collect::<Vec<_>>()})))?;
    Ok(
        json!({"valid":true,"language_version":def.language_version,"hash":ritual::content_hash(&def),"effects":effects,"warnings":ritual::lint(&def),"definition":def}),
    )
}



pub(crate) fn stop_running_core(paths: &Paths) -> Result<bool, RpcErr> {
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

pub(crate) fn restart_core(paths: &Paths) -> Result<Value, RpcErr> {
    let was_running = stop_running_core(paths)?;
    let mut ping = Client::connect(paths)?.call("ping", json!({}))?;
    if let Some(obj) = ping.as_object_mut() {
        obj.insert("restarted".into(), json!(true));
        obj.insert("was_running".into(), json!(was_running));
    }
    Ok(ping)
}

pub(crate) fn core_command(op: &Op, paths: &Paths) -> i32 {
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


pub(crate) fn help(style: Style) {
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
