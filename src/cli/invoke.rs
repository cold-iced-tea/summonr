use crate::grammar::Op;
use crate::rpc::{Client, RpcErr};
use crate::theme::{self, bold, paint, Ink};
use crate::util::{self, fmt_ago, fmt_dur, fmt_offset};
use crate::{cwd, effects, elegy_summary, finish, outcome_exit, print_result, row, rubric, s, target};
use serde_json::{json, Value};


pub(crate) fn wait_for(client: &mut Client, id: &str, op: &Op) -> Result<Value, RpcErr> {
    let timeout = match &op.timeout {
        Some(t) => Some(util::parse_duration(t).map_err(RpcErr::invalid)?),
        None => None,
    };
    if op.cancel_on_drop {
        client.tether(id)?;
    }
    use std::io::{IsTerminal, Write};
    // Progress goes to stderr, and only to a terminal, so stdout (and
    // `--json`) carries nothing but the result.
    if !std::io::stderr().is_terminal() {
        return client.call("wait", json!({"summon": id, "timeout_ms": timeout, "brief": op.brief}));
    }
    let clear = || {
        eprint!("\r\x1b[2K");
        std::io::stderr().flush().ok();
    };
    let started = std::time::Instant::now();
    let mut tick = 0u64;
    let mut tree: Option<Vec<Value>> = None;
    let mut tree_read: Option<std::time::Instant> = None;
    loop {
        let d = client.call("get", json!({"summon": id}))?;
        if d["status"] == "ended" {
            clear();
            if op.json {
                return client.call("wait", json!({"summon": id, "timeout_ms": timeout, "brief": op.brief}));
            }
            if op.brief {
                return client.call("get", json!({"summon": id, "brief": true}));
            }
            return Ok(d);
        }
        if timeout.is_some_and(|t| started.elapsed().as_millis() as u64 >= t) {
            clear();
            return Err(RpcErr::new(
                "timeout",
                7,
                format!("stopped waiting for `{id}`; the summon is still running"),
            ));
        }
        // The tree changes slowly next to the spinner; read it once a second.
        if tree_read.is_none_or(|t| t.elapsed() >= std::time::Duration::from_secs(1)) {
            tree = client
                .call("list", json!({"scope": "children", "summon": id, "recursive": true, "limit": 10000}))
                .ok()
                .and_then(|v| v.as_array().cloned());
            tree_read = Some(std::time::Instant::now());
        }
        let elapsed = d["started"]
            .as_i64()
            .map(|s| util::now_ms() - s)
            .unwrap_or(started.elapsed().as_millis() as i64);
        let text = clip_to_width(&format!("{id} labours · {}", progress_line(&d, tree.as_deref(), elapsed)), terminal_width().saturating_sub(6));
        eprint!(
            "\r\x1b[2K  {} {} {}",
            paint(Ink::Ember, theme::phase(tick)),
            paint(Ink::Gold, &theme::sigil(id)),
            paint(Ink::Dim, &text)
        );
        std::io::stderr().flush().ok();
        tick += 1;
        std::thread::sleep(std::time::Duration::from_millis(150));
    }
}

/// Columns of the terminal on stderr, or 80 when it cannot be read.
fn terminal_width() -> usize {
    // SAFETY: TIOCGWINSZ only writes a `winsize` into the struct passed.
    let mut ws: libc::winsize = unsafe { std::mem::zeroed() };
    let ok = unsafe { libc::ioctl(libc::STDERR_FILENO, libc::TIOCGWINSZ, &mut ws) } == 0;
    if ok && ws.ws_col > 0 { ws.ws_col as usize } else { 80 }
}

/// `s` cut to `width` characters, so an in-place line never wraps.
fn clip_to_width(s: &str, width: usize) -> String {
    if s.chars().count() <= width {
        return s.to_string();
    }
    let mut out: String = s.chars().take(width.saturating_sub(1)).collect();
    out.push('…');
    out
}

/// One line of progress for a waiting run: items done, the child ritual at
/// work (down to the deepest running descendant), how many child runs
/// failed, and the whole tree's requests, tokens and elapsed time. `tree`
/// is every descendant, depth first, or `None` when it could not be read.
pub(crate) fn progress_line(root: &Value, tree: Option<&[Value]>, elapsed_ms: i64) -> String {
    let mut parts = Vec::new();
    if let t @ 1.. = root["total"].as_i64().unwrap_or(0) {
        parts.push(format!("{}/{t}", root["done"].as_i64().unwrap_or(0)));
    }
    if let Some(tree) = tree {
        // The deepest running descendant, and the running chain above it.
        let running = |r: &&Value| r["status"] != "ended";
        if let Some(leaf) = tree.iter().filter(running).max_by_key(|r| r["depth"].as_u64().unwrap_or(0)) {
            let mut chain = vec![leaf];
            while let Some(parent) = chain.last().and_then(|c| c["parent"].as_str()) {
                match tree.iter().find(|r| r["id"].as_str() == Some(parent)) {
                    Some(p) => chain.push(p),
                    None => break,
                }
            }
            let names: Vec<&str> = chain.iter().rev().filter_map(|r| r["ritual"].as_str()).collect();
            let shown = if names.len() > 3 { &names[names.len() - 3..] } else { &names[..] };
            parts.push(shown.join(" › "));
        }
        if !tree.is_empty() {
            let failed = tree
                .iter()
                .filter(|r| r["status"] == "ended" && !matches!(r["outcome"].as_str(), Some("returned" | "abstained")))
                .count();
            let noun = if tree.len() == 1 { "child" } else { "children" };
            parts.push(match failed {
                0 => format!("{} {noun}", tree.len()),
                f => format!("{} {noun}, {f} failed", tree.len()),
            });
        }
    }
    parts.push(format!("{} requests", util::thousands(root["requests"].as_u64().unwrap_or(0))));
    parts.push(format!("{} tokens", util::thousands(root["tokens"].as_u64().unwrap_or(0))));
    parts.push(fmt_dur(elapsed_ms));
    parts.join(" · ")
}

pub(crate) fn invoke(op: &Op, client: &mut Client) -> i32 {
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


pub(crate) fn fence_command(op: &Op, client: &mut Client) -> i32 {
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



pub(crate) fn petition(op: &Op, client: &mut Client) -> i32 {
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


pub(crate) fn follow_command(op: &Op, client: &mut Client) -> i32 {
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_progress_line_rolls_up_the_whole_tree() {
        let root = json!({"id": "s-1", "ritual": "bughunt-test", "status": "running", "done": 3, "total": 25, "requests": 41, "tokens": 223555});
        let tree = vec![
            json!({"id": "s-2", "parent": "s-1", "ritual": "bughunt-gather", "status": "ended", "outcome": "returned", "depth": 1}),
            json!({"id": "s-3", "parent": "s-1", "ritual": "bughunt-triage", "status": "ended", "outcome": "invalid", "depth": 1}),
            json!({"id": "s-4", "parent": "s-1", "ritual": "bughunt-verdict", "status": "running", "done": 1, "total": 4, "depth": 1}),
            json!({"id": "s-5", "parent": "s-4", "ritual": "bughunt-compile", "status": "running", "depth": 2}),
            json!({"id": "s-6", "parent": "s-1", "ritual": "bughunt-triage", "status": "ended", "outcome": "cancelled", "depth": 1}),
        ];
        let line = progress_line(&root, Some(&tree), 723_000);
        assert_eq!(
            line,
            "3/25 · bughunt-verdict › bughunt-compile · 5 children, 2 failed · 41 requests · 223,555 tokens · 12:03"
        );
        let quiet = progress_line(&json!({"requests": 0, "tokens": 0, "done": 0, "total": 0}), Some(&[]), 1500);
        assert_eq!(quiet, "0 requests · 0 tokens · 00:01");
        let unknown = progress_line(&root, None, 1000);
        assert!(!unknown.contains("children"), "no tree, no rollup: {unknown}");
    }
}
