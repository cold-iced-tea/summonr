//! Per-command help: `summonr CMD --help`, `summonr help CMD`, and the same
//! with `--json` as a machine-readable descriptor an agent can read once
//! instead of probing the grammar.
use crate::grammar::{self, Style};
use serde_json::{json, Value};

/// One command's contract beyond its summary in `grammar::VERBS`.
struct Spec {
    verb: &'static str,
    /// Usage in flag spelling, after `summonr `.
    usage: &'static str,
    /// Flag names (as in `grammar::CLAUSES`, without the value) it accepts.
    flags: &'static [&'static str],
    examples: &'static [&'static str],
    /// The fields of `data` in its JSON envelope.
    data: &'static [(&'static str, &'static str)],
    /// Exit codes particular to this command, beyond the common ones.
    exits: &'static [(i32, &'static str)],
}

const RUN_DATA: &[(&str, &str)] = &[
    ("id", "the summon id (s-…)"),
    ("ritual", "the ritual's name"),
    ("status", "`running` or `ended`"),
    ("outcome", "with --wait: the run's verdict (returned, abstained, exhausted, cancelled, invalid, failed, unverified, interrupted); also at the envelope's top level"),
    ("result", "the return value, or {partial: …} when the run did not return"),
    ("error", "why the run did not return, or null"),
    ("usage", "requests, tokens and time used"),
];
const RUN_EXITS: &[(i32, &str)] = &[
    (0, "returned or abstained (or accepted, without --wait)"),
    (4, "missing grants; `data.grant_commands` lists the `allow` commands"),
    (7, "--timeout passed; the summon is still running"),
    (10, "exhausted"),
    (11, "failed, invalid, cancelled, interrupted or unverified"),
];

const SPECS: &[Spec] = &[
    Spec {
        verb: "invoke",
        usage: "run RITUAL|FILE [--arg k=v ...] [--arg-file k=PATH ...] [--model M] [--set k=v ...] [--wait] [--timeout 5m] [--json]",
        flags: &["--arg", "--arg-file", "--model", "--set", "--version", "--wait", "--timeout", "--json", "--dry-run", "--record", "--replay", "--simulate", "--brief", "--cancel-on-drop"],
        examples: &[
            "summonr run inbox-triage --arg count=10 --wait --json",
            "summonr run route-form --arg-file form=./form.txt --simulate --wait",
            "summonr run r --dry-run --json",
        ],
        data: RUN_DATA,
        exits: RUN_EXITS,
    },
    Spec {
        verb: "rehearse",
        usage: "run RITUAL --dry-run [--arg k=v ...] [--json]",
        flags: &["--arg", "--arg-file", "--model", "--set", "--version", "--json"],
        examples: &["summonr run inbox-triage --dry-run --arg count=10 --json"],
        data: &[("effects", "what each step would do"), ("missing", "grants not yet given"), ("warnings", "lint and drift warnings")],
        exits: &[(4, "missing grants")],
    },
    Spec {
        verb: "await",
        usage: "wait SUMMON [--timeout 30s] [--brief] [--json]",
        flags: &["--timeout", "--brief", "--json", "--wait"],
        examples: &["summonr wait s-1234 --timeout 5m --json"],
        data: RUN_DATA,
        exits: RUN_EXITS,
    },
    Spec {
        verb: "elegy",
        usage: "record SUMMON [--brief] [--json]",
        flags: &["--brief", "--json"],
        examples: &["summonr record s-1234 --brief --json"],
        data: &[
            ("outcome", "the run's verdict, or null while running; also at the envelope's top level"),
            ("result", "the return value or {partial: …}"),
            ("steps", "per-step status and output"),
            ("effects", "every write, with its state and evidence"),
            ("usage", "requests, tokens and time"),
        ],
        exits: &[(3, "no such summon")],
    },
    Spec {
        verb: "ledger",
        usage: "logs SUMMON [--json]",
        flags: &["--json"],
        examples: &["summonr logs s-1234 --json"],
        data: &[("[]", "events: {seq, ts, kind, text, data}")],
        exits: &[(3, "no such summon")],
    },
    Spec {
        verb: "graveyard",
        usage: "history [RITUAL] [--children RUN [--recursive]] [--json]",
        flags: &["--children", "--recursive", "--json"],
        examples: &["summonr history --json", "summonr history --children s-1234 --recursive"],
        data: &[("[]", "summons: {id, ritual, status, outcome, started, ended, …}")],
        exits: &[],
    },
    Spec {
        verb: "banish",
        usage: "stop SUMMON|--all [--json]",
        flags: &["--all", "--json"],
        examples: &["summonr stop s-1234"],
        data: &[("summon", "the summon asked to stop")],
        exits: &[(3, "no such summon"), (5, "already ended")],
    },
    Spec {
        verb: "drill",
        usage: "cases RITUAL --against FILE [--arg k=v ...] [--json]",
        flags: &["--against", "--arg", "--version", "--json"],
        examples: &[
            "summonr cases route-form --against route-form.drill.yaml --json",
            "summonr drill describe-image against describe-image.drill.yaml given root=/home/me/images",
        ],
        data: &[
            ("passed", "how many cases passed"),
            ("failed", "how many cases failed"),
            ("touchstone_errors", "cases whose touchstone could not compute an expectation"),
            ("cases", "each case's id, passed, reasons"),
        ],
        exits: &[(3, "no drill file"), (14, "one or more cases failed")],
    },
    Spec {
        verb: "assay",
        usage: "test RITUAL --against SET [--baseline] [--live] [--json]",
        flags: &["--against", "--baseline", "--live", "--model", "--version", "--json"],
        examples: &["summonr test inbox-triage --against ./set.yaml --baseline"],
        data: &[("verdict", "within tolerance, drifted or inconclusive")],
        exits: &[(5, "no baseline"), (12, "drifted"), (13, "inconclusive")],
    },
    Spec {
        verb: "validate",
        usage: "compile FILE [--json]",
        flags: &["--json"],
        examples: &["summonr compile ./r.yaml --json"],
        data: &[("contract", "the compiled definition and its hash"), ("diagnostics", "errors and warnings")],
        exits: &[(2, "the ritual is invalid")],
    },
    Spec {
        verb: "inscribe",
        usage: "save FILE|DIR [--name NAME] [--replace] [--json]",
        flags: &["--name", "--replace", "--file", "--session", "--json"],
        examples: &["summonr save ./r.yaml", "summonr save ./rituals/"],
        data: &[("ritual", "name"), ("version", "the saved version"), ("hash", "content hash")],
        exits: &[(5, "a ritual with this name has different content; add --replace")],
    },
    Spec {
        verb: "grimoire",
        usage: "rituals [read RITUAL [--version N]] [--json]",
        flags: &["--version", "--json"],
        examples: &["summonr rituals --json", "summonr rituals read inbox-triage"],
        data: &[("[]", "rituals: {ritual, version, hash, …}")],
        exits: &[],
    },
    Spec {
        verb: "grant",
        usage: "allow RITUAL --from REAGENT (--select a,b | --all) [--on k=v] [--under DIR] [--into k=v] [--json]  |  allow check [RITUAL]",
        flags: &["--from", "--select", "--all", "--on", "--under", "--into", "--version", "--json"],
        examples: &[
            "summonr allow inbox-triage --from mail --select search,read --on folder=Inbox",
            "summonr allow scan --from file --select read_text --under /home/me/Work",
            "summonr allow check",
        ],
        data: &[("granted", "the grants added"), ("missing", "with `check`: what each ritual still needs")],
        exits: &[(3, "no such ritual or reagent")],
    },
    Spec {
        verb: "revoke",
        usage: "disallow RITUAL --from REAGENT (--select a,b | --all) [--on k=v] [--under DIR] [--into k=v] [--json]",
        flags: &["--from", "--select", "--all", "--on", "--under", "--into", "--json"],
        examples: &["summonr disallow inbox-triage --from mail --select move"],
        data: &[("revoked", "the grants removed")],
        exits: &[],
    },
    Spec {
        verb: "petition",
        usage: "approval [permit|deny ID|--all] [--reason TEXT] [--json]",
        flags: &["--all", "--reason", "--json"],
        examples: &["summonr approval --json", "summonr approval permit p-12"],
        data: &[("[]", "petitions: {id, operation, arguments, state, …}")],
        exits: &[(3, "no such petition")],
    },
    Spec {
        verb: "fence",
        usage: "reconcile [list | apply ID | clear ID] [--json]",
        flags: &["--json"],
        examples: &["summonr reconcile list --json"],
        data: &[("[]", "fences: {summon, step, operation, state}")],
        exits: &[],
    },
    Spec {
        verb: "satchel",
        usage: "tools [--json]",
        flags: &["--json"],
        examples: &["summonr tools --json"],
        data: &[("[]", "reagents and their tools, with effect and description")],
        exits: &[],
    },
    Spec {
        verb: "core",
        usage: "core [status | stop | restart] [--json]",
        flags: &["--json"],
        examples: &["summonr core status --json", "summonr --home ./lab core status --json"],
        data: &[
            ("running", "whether a core answers"),
            ("home", "the home directory in use, or null for the XDG layout"),
            ("config_path", "the config.yaml the core reads"),
            ("config_loaded", "whether that file exists (false: defaults)"),
            ("ollama_url", "the effective Ollama URL"),
            ("pid", "the core's process id, when running"),
        ],
        exits: &[(8, "the core is unavailable")],
    },
    Spec {
        verb: "mcp",
        usage: "mcp [stdio | http ADDR:PORT] [--name FAMILIAR] [--via PEER]",
        flags: &["--name", "--via"],
        examples: &["summonr mcp --name codex", "summonr mcp --home ./lab"],
        data: &[],
        exits: &[(4, "the familiar cannot sign")],
    },
    Spec {
        verb: "help",
        usage: "help [COMMAND] [--json]",
        flags: &["--json"],
        examples: &["summonr help run --json", "summonr run --help"],
        data: &[("command", "with COMMAND: this descriptor"), ("commands", "without: every command")],
        exits: &[],
    },
];

/// Exit codes every command can return.
const COMMON_EXITS: &[(i32, &str)] = &[
    (0, "success"),
    (1, "unexpected error"),
    (2, "invalid: bad syntax, ritual or input"),
    (3, "not found"),
    (4, "not authorized"),
    (5, "conflict"),
    (8, "the core is unavailable"),
];

/// Flags any command accepts.
const GLOBAL_FLAGS: &[(&str, &str)] = &[
    ("--help", "Show this command's help (with --json, as a descriptor)"),
    ("--home PATH", "Use PATH as the Summonr home, as SUMMONR_HOME=PATH would (and over it); anywhere on the line"),
];

fn spec(verb: &str) -> Option<&'static Spec> {
    SPECS.iter().find(|s| s.verb == verb)
}

fn verb_row(verb: &str) -> Option<&'static (&'static str, &'static str, bool, &'static str)> {
    grammar::VERBS.iter().find(|(r, _, _, _)| *r == verb)
}

/// The clause rows for a command's flags, in its order.
fn clause_rows(flags: &[&str]) -> Vec<&'static (&'static str, &'static str, &'static str)> {
    // `--dry-run` is listed in CLAUSES as `run R --dry-run`.
    const EXTRA: &[(&str, &str, &str)] = &[("rehearse R", "--dry-run", "Preview without executing")];
    flags
        .iter()
        .filter_map(|f| {
            grammar::CLAUSES
                .iter()
                .chain(EXTRA)
                .find(|(_, flag, _)| flag.split_whitespace().next() == Some(f))
        })
        .collect()
}

fn flags_of(verb: &str) -> &'static [&'static str] {
    spec(verb).map(|s| s.flags).unwrap_or(&["--json"])
}

/// The machine-readable descriptor of one command (canonical verb).
pub(crate) fn descriptor(verb: &str) -> Option<Value> {
    let (keyword, plain, takes_target, summary) = verb_row(verb)?;
    let sp = spec(verb);
    let rows = clause_rows(flags_of(verb));
    let mut exits: std::collections::BTreeMap<i32, &str> = COMMON_EXITS.iter().copied().collect();
    for (code, why) in sp.map(|s| s.exits).unwrap_or_default() {
        exits.insert(*code, why);
    }
    let shown = if plain.is_empty() { *keyword } else { *plain };
    let aliases: Vec<&str> = [*keyword, *plain].into_iter().filter(|a| !a.is_empty() && *a != shown).collect();
    Some(json!({
        "command": shown,
        "aliases": aliases,
        "summary": summary,
        "usage": format!("summonr {}", sp.map(|s| s.usage.to_string()).unwrap_or_else(|| format!("{shown}{} [--json]", if *takes_target { " TARGET" } else { "" }))),
        "flags": rows.iter().map(|(kw, flag, meaning)| json!({"flag": flag, "keyword": kw, "meaning": meaning}))
            .chain(GLOBAL_FLAGS.iter().map(|(flag, meaning)| json!({"flag": flag, "keyword": null, "meaning": meaning, "global": true})))
            .collect::<Vec<_>>(),
        "clauses": rows.iter().map(|(kw, _, _)| kw).collect::<Vec<_>>(),
        "examples": sp.map(|s| s.examples).unwrap_or_default(),
        "exit_codes": exits.iter().map(|(c, why)| (c.to_string(), json!(why))).collect::<serde_json::Map<_, _>>(),
        "data_shape": sp.filter(|s| !s.data.is_empty()).map(|s| {
            s.data.iter().map(|(k, v)| (k.to_string(), json!(v))).collect::<serde_json::Map<_, _>>()
        }),
        "envelope": {
            "ok": "transport status: the request was understood and answered. A run that failed still has ok: true",
            "outcome": "the run's verdict: data.outcome on run --wait, wait and record; passed or failed on drill; null on every other command and on a run not waited on",
            "data": "the reply, shaped as data_shape",
            "error": "on ok: false, {code, message}",
        },
    }))
}

/// The human help for one command, as lines.
pub(crate) fn text(verb: &str, style: Style) -> Option<Vec<String>> {
    let d = descriptor(verb)?;
    let mut out = vec![format!("summonr {} — {}", d["command"].as_str().unwrap_or(verb), d["summary"].as_str().unwrap_or(""))];
    if let Some(a) = d["aliases"].as_array().filter(|a| !a.is_empty()) {
        out.push(format!("  also: {}", a.iter().filter_map(Value::as_str).collect::<Vec<_>>().join(", ")));
    }
    out.push(String::new());
    out.push(format!("usage: {}", d["usage"].as_str().unwrap_or("")));
    out.push(String::new());
    out.push("flags:".into());
    for f in d["flags"].as_array().into_iter().flatten() {
        let (flag, kw) = (f["flag"].as_str().unwrap_or(""), f["keyword"].as_str());
        let (lead, other) = match (style, kw) {
            (Style::Keyword, Some(k)) => (k, flag),
            (_, Some(k)) => (flag, k),
            (_, None) => (flag, ""),
        };
        out.push(format!("  {lead:<22} {other:<20} {}", f["meaning"].as_str().unwrap_or("")));
    }
    if let Some(ex) = d["examples"].as_array().filter(|e| !e.is_empty()) {
        out.push(String::new());
        out.push("examples:".into());
        out.extend(ex.iter().filter_map(Value::as_str).map(|e| format!("  {e}")));
    }
    out.push(String::new());
    out.push("exit codes:".into());
    for (code, why) in d["exit_codes"].as_object().into_iter().flatten() {
        out.push(format!("  {code:>3}  {}", why.as_str().unwrap_or("")));
    }
    out.push(String::new());
    out.push("With --json, `ok` is transport status (a failed run is ok: true); the run's verdict is the envelope's top-level `outcome`.".into());
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(s: &str) -> Vec<String> {
        s.split(' ').map(String::from).collect()
    }

    #[test]
    fn every_command_and_alias_takes_help() {
        for (keyword, plain, _, _) in grammar::VERBS {
            for name in [keyword, plain].into_iter().filter(|n| !n.is_empty()) {
                for flag in ["--help", "-h"] {
                    let op = grammar::parse(&args(&format!("{name} {flag}"))).unwrap_or_else(|e| panic!("{name} {flag}: {e}"));
                    assert!(op.help, "{name} {flag}");
                }
                // After a target and other clauses too.
                let op = grammar::parse(&args(&format!("{name} x --json --help"))).unwrap();
                assert!(op.help && op.json, "{name}");
                let d = descriptor(&op.verb).unwrap_or_else(|| panic!("no descriptor for {}", op.verb));
                for key in ["command", "aliases", "summary", "flags", "clauses", "exit_codes", "data_shape", "usage"] {
                    assert!(d.get(key).is_some(), "{name}: {key}");
                }
            }
        }
        // A flag that is otherwise invalid does not hide the help.
        assert!(grammar::parse(&args("run r --bogus --help")).unwrap().help);
        // `help CMD` names a command by either spelling.
        let op = grammar::parse(&args("help run --json")).unwrap();
        assert_eq!((op.verb.as_str(), op.positionals.as_slice()), ("help", &["run".to_string()][..]));
    }

    #[test]
    fn descriptors_name_real_flags_and_exit_codes() {
        for s in SPECS {
            assert!(verb_row(s.verb).is_some(), "{} is not a command", s.verb);
            for f in s.flags {
                assert!(
                    clause_rows(&[f]).len() == 1,
                    "{}: `{f}` is not in grammar::CLAUSES",
                    s.verb
                );
            }
        }
        let run = descriptor("invoke").unwrap();
        assert_eq!(run["command"], "run");
        assert_eq!(run["aliases"], json!(["invoke"]));
        assert!(run["flags"].as_array().unwrap().iter().any(|f| f["flag"] == "--model M" && f["keyword"] == "through M"));
        assert!(run["clauses"].as_array().unwrap().contains(&json!("given k=v ...")));
        assert_eq!(run["exit_codes"]["7"], "--timeout passed; the summon is still running");
        assert!(run["data_shape"]["outcome"].as_str().unwrap().contains("verdict"));
        let drill = descriptor("drill").unwrap();
        assert!(drill["flags"].as_array().unwrap().iter().any(|f| f["flag"] == "--arg k=v ..." && f["keyword"] == "given k=v ..."));
        assert!(text("drill", Style::Flag).unwrap().iter().any(|l| l.contains("--arg k=v")));
        assert_eq!(descriptor("drill").unwrap()["exit_codes"]["14"], "one or more cases failed");
        // A command without its own spec still has a usable descriptor.
        let d = descriptor("identity").unwrap();
        assert_eq!(d["command"], "whoami");
        assert!(d["data_shape"].is_null());
        assert!(text("grant", Style::Flag).unwrap().iter().any(|l| l.contains("--under DIR")));
        assert!(descriptor("nope").is_none());
    }
}
