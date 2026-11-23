//! The terminal grammar. Every clause has a keyword spelling and a flag
//! spelling; both parse to the same `Op`. Nothing downstream of this module
//! knows which spelling was typed.

use serde::Serialize;
use std::collections::BTreeMap;

#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct ReagentScope {
    pub reagent: String,
    pub version: Option<u32>,
    pub select: Vec<String>,
    pub all: bool,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct Op {
    pub verb: String,
    pub positionals: Vec<String>,
    pub model: Option<String>,
    pub settings: BTreeMap<String, String>,
    pub args: BTreeMap<String, String>,
    /// Inputs whose values are read from files (`reading k=PATH`), merged
    /// into `args` by the CLI before the request is sent.
    pub arg_files: BTreeMap<String, String>,
    pub name: Option<String>,
    pub task: Option<String>,
    pub wait: bool,
    pub json: bool,
    pub timeout: Option<String>,
    pub version: Option<u32>,
    pub replace: bool,
    pub dry_run: bool,
    pub against: Option<String>,
    /// A public key (`ed25519:…`) for `familiar add` or `peer add`.
    pub key: Option<String>,
    /// A peer core's URL for `peer add`.
    pub url: Option<String>,
    /// A CA certificate file for checking RFC 3161 timestamps with `verify`.
    pub tsa_ca: Option<String>,
    /// The pinned peer core `mcp` talks to, as the familiar named by `as`.
    pub via: Option<String>,
    /// With `graveyard` / `history`: list this run's child summons instead.
    pub children: Option<String>,
    /// A pact a familiar may call under, for `familiar allow`.
    pub pact: Option<String>,
    /// Why, for `pact decline`, `reject` or `end`.
    pub reason: Option<String>,
    pub record: Option<String>,
    pub replay: Option<String>,
    pub simulate: bool,
    pub baseline: bool,
    /// An assay that performs its writes for real.
    pub live: bool,
    /// Only the outcome, result, usage and child runs (`brief` / `--brief`).
    pub brief: bool,
    /// `all` outside a reagent scope: every pending petition.
    pub all: bool,
    pub scopes: Vec<ReagentScope>,
    pub on: BTreeMap<String, String>,
    pub into: BTreeMap<String, String>,
    pub file: Option<String>,
    pub schema: Option<String>,
    pub session: Option<String>,
    pub format: Option<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ParseError {
    /// Index into the argument list (0 is the verb).
    pub pos: usize,
    pub token: String,
    pub msg: String,
    pub hint: String,
}

impl std::fmt::Display for ParseError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "argument {} (`{}`): {}",
            self.pos + 1,
            self.token,
            self.msg
        )?;
        if !self.hint.is_empty() {
            write!(f, "\n  valid form: {}", self.hint)?;
        }
        Ok(())
    }
}

#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Style {
    Keyword,
    Flag,
}

/// (ritual name, plain name, takes a target, one-line meaning)
pub const VERBS: &[(&str, &str, bool, &str)] = &[
    (
        "validate",
        "compile",
        true,
        "Check a ritual file offline and return its compiled contract",
    ),
    (
        "invoke",
        "run",
        true,
        "Execute a saved ritual or a ritual file",
    ),
    (
        "rehearse",
        "",
        true,
        "Validate and preview an invocation without executing it",
    ),
    (
        "inscribe",
        "save",
        true,
        "Save a ritual file (or live session) as a new versioned ritual",
    ),
    (
        "summon",
        "session",
        true,
        "Open an interactive summon session (or `list` / `close ID`)",
    ),
    (
        "soul",
        "system",
        true,
        "Set or inspect the system prompt of a live summon session",
    ),
    (
        "bind",
        "attach",
        true,
        "Attach a file or text binding to a live summon session",
    ),
    (
        "imbue",
        "configure",
        true,
        "Set model settings or essence limits on a live summon session",
    ),
    (
        "bid",
        "ask",
        true,
        "Send a bounded prompt turn to a live summon session",
    ),
    (
        "lethe",
        "forget",
        true,
        "Trim or clear conversation turns from a live summon session",
    ),
    (
        "sweep",
        "experiment",
        true,
        "Run a parameter sweep over models, settings, or cases",
    ),
    (
        "judge",
        "score",
        true,
        "Score an experiment's trials against expected outputs or a touchstone",
    ),
    (
        "weigh",
        "rank",
        true,
        "Rank variants in an experiment or compare two experiments",
    ),
    (
        "fence",
        "reconcile",
        true,
        "List or reconcile fenced external effects: list [SUMMON], apply KEY, clear KEY",
    ),
    (
        "grimoire",
        "rituals",
        false,
        "List saved rituals; `read NAME` shows one",
    ),
    (
        "grant",
        "allow",
        true,
        "Authorize a ritual to use reagent tools within a scope",
    ),
    ("revoke", "disallow", true, "Withdraw matching authority"),
    ("circle", "list", false, "List active summons"),
    (
        "console",
        "shell",
        false,
        "Open the persistent command workbench with live status",
    ),
    (
        "job",
        "queue",
        false,
        "Shared result work: create FILE, work NAME, status NAME, item NAME ITEM, retry NAME ITEM",
    ),
    (
        "reliquary",
        "registry",
        false,
        "Signed ritual packs: publish, retract, add/remove PEER, list, show, inscribe, export, import",
    ),
    (
        "rite",
        "workflow",
        false,
        "Multi-stage rites with gates and assumptions: rehearse, start, advance, list, show, events, falsify, assert",
    ),
    (
        "audit",
        "",
        false,
        "Inspect or verify the tamper-evident administrative audit log",
    ),
    (
        "operator",
        "",
        false,
        "Manage RBAC operator keys and roles: new, add, list, revoke",
    ),
    (
        "rbac",
        "",
        false,
        "Manage RBAC roles, reagent permissions, and actor bindings",
    ),
    (
        "daemon",
        "panel",
        false,
        "Open the daemon status panel; closing it leaves work running",
    ),
    (
        "scry",
        "watch",
        false,
        "Open the observer; closing it never stops work",
    ),
    ("await", "wait", true, "Block until a summon ends"),
    (
        "banish",
        "stop",
        true,
        "Request graceful termination of a summon",
    ),
    (
        "revive",
        "resume",
        true,
        "Resume an exhausted, cancelled, or interrupted summon from its last checkpoint",
    ),
    (
        "recast",
        "",
        true,
        "Branch from an earlier checkpoint of an ended summon: RUN STEP",
    ),
    (
        "follow",
        "tail",
        true,
        "Follow a summon's live events until it ends",
    ),
    (
        "key",
        "",
        false,
        "Manage offline root keys and delegated familiar grants",
    ),
    (
        "demo",
        "",
        false,
        "Run an ephemeral Git-repo playground with a held petition and automatic cleanup",
    ),
    ("graveyard", "history", false, "List ended summons"),
    (
        "epitaph",
        "receipt",
        true,
        "A run's signed epitaph: RUN, RUN reveal PART, or export FROM TO FILE",
    ),
    (
        "chain",
        "",
        false,
        "This core's epitaph chain: show the head, or `chain mark` to print a shareable mark",
    ),
    (
        "witness",
        "",
        true,
        "Witnessing: PEER to submit this chain, accept NAME key K, list, drop NAME, tsa URL",
    ),
    (
        "verify",
        "",
        true,
        "Check this chain, a chain mark, or an exported or revealed file",
    ),
    (
        "listen",
        "serve-signed",
        false,
        "Accept signed requests from other devices: listen ADDRESS:PORT",
    ),
    (
        "identity",
        "whoami",
        false,
        "Show this core's public key and fingerprint",
    ),
    ("elegy", "record", true, "Read one summon's complete record"),
    (
        "ledger",
        "logs",
        true,
        "Read one summon's chronological events",
    ),
    (
        "assay",
        "test",
        true,
        "Run an assay set; record a baseline or check drift",
    ),
    (
        "drill",
        "cases",
        true,
        "Run a ritual's drill cases against scripted model answers; writes are simulated",
    ),
    (
        "satchel",
        "tools",
        false,
        "List available reagents and their tools",
    ),
    (
        "familiar",
        "agent",
        false,
        "Agent identities: new NAME, add NAME key K, list, allow/disallow NAME RITUAL..., revoke NAME",
    ),
    (
        "peer",
        "peers",
        false,
        "Pinned remote cores: add NAME url URL key K, list, remove NAME",
    ),
    (
        "pact",
        "contract",
        false,
        "Agent-to-agent contracts: offer PEER FILE, fetch PEER, list, show P, agree/decline P, call P given k=v, accept/reject P CALL, status P CALL, history P, export P FILE, admit NAME key K, limit P k=v, end P",
    ),
    (
        "petition",
        "approval",
        false,
        "List held operations; `permit` or `deny` them by ID or `all`",
    ),
    (
        "glean",
        "sample",
        true,
        "Sample real runs of a ritual as unlabeled assay cases",
    ),
    (
        "mcp",
        "",
        false,
        "Serve rituals to other agents over MCP: `mcp` (stdio) or `mcp http ADDR`",
    ),
    (
        "core",
        "",
        false,
        "Run the core; `daemon` observes it; `core stop` ends it; `core restart` reloads it",
    ),
    ("help", "", false, "Show this help"),
    ("version", "", false, "Print the version"),
];

/// (keyword, flag, meaning)
pub const CLAUSES: &[(&str, &str, &str)] = &[
    ("through M", "--model M", "Model override"),
    ("at k=v ...", "--set k=v ...", "Runtime settings"),
    ("given k=v ...", "--arg k=v ...", "Typed ritual inputs"),
    (
        "reading k=PATH ...",
        "--arg-file k=PATH ...",
        "Ritual inputs whose values are a file's text (at most 1 MiB)",
    ),
    ("as NAME", "--name NAME", "Assign a name"),
    (
        "returning json",
        "--json",
        "Respond at acceptance as a JSON envelope",
    ),
    (
        "then return [json]",
        "--wait [--json]",
        "Wait for completion",
    ),
    ("within 5m", "--timeout 5m", "Bound the wait, not the work"),
    ("from R", "--from R", "Open a reagent scope"),
    (
        "selecting a b",
        "--select a,b",
        "Select tools in the open scope",
    ),
    ("all", "--all", "Select every tool in the open scope"),
    ("on k=v", "--on k=v", "Resource scope of a permission"),
    (
        "into k=v",
        "--into k=v",
        "Destination scope of a permission",
    ),
    (
        "version N",
        "--version N",
        "Pin the preceding target or reagent",
    ),
    (
        "replacement",
        "--replace",
        "Explicitly replace the named target",
    ),
    (
        "against SET",
        "--against SET",
        "Assay set or drill file to run",
    ),
    (
        "recording PATH",
        "--record PATH",
        "Record model and tool traffic to a cassette",
    ),
    (
        "simulating",
        "--simulate",
        "Answer writes with what they would do, changing nothing",
    ),
    (
        "brief",
        "--brief",
        "Show only outcome, result, usage and child runs (always fits one reply)",
    ),
    (
        "key K",
        "--key K",
        "A public key (ed25519:…) for familiar add or peer add",
    ),
    ("url U", "--url U", "A peer core's URL for peer add"),
    ("pact P", "--pact P", "With familiar allow: a pact the familiar may call under"),
    ("because TEXT", "--reason TEXT", "With pact decline, reject or end: the reason"),
    (
        "tsa-ca F",
        "--tsa-ca F",
        "With verify: check RFC 3161 timestamps against this CA certificate",
    ),
    (
        "via PEER",
        "--via PEER",
        "With mcp: reach this pinned peer core instead of this device's",
    ),
    (
        "children RUN",
        "--children RUN",
        "With history: list a run's child summons, ended or not",
    ),
    (
        "baseline",
        "--baseline",
        "Record the account as the baseline",
    ),
    (
        "live",
        "--live",
        "Let an assay perform its writes instead of simulating them",
    ),
    (
        "replaying PATH",
        "--replay PATH",
        "Replay a cassette offline instead of calling models and tools",
    ),
    ("rehearse R", "run R --dry-run", "Preview without executing"),
    ("file PATH", "--file PATH", "Input file for bind, soul, or inscribe"),
    ("schema JSON", "--schema JSON", "Output JSON schema for bid"),
    ("session ID", "--session ID", "Live summon session ID for inscribe"),
    ("format FMT", "--format FMT", "Output format (json or ndjson) for audit export"),
    ("to TASK", "--task TASK", "Prompt or task text for an interactive bid"),
];

const KEYWORDS: &[&str] = &[
    "through",
    "at",
    "given",
    "as",
    "to",
    "returning",
    "then",
    "within",
    "from",
    "selecting",
    "all",
    "on",
    "into",
    "reading",
    "version",
    "replacement",
    "against",
    "recording",
    "replaying",
    "simulating",
    "baseline",
    "live",
    "brief",
    "key",
    "url",
    "via",
    "children",
    "tsa-ca",
    "pact",
    "because",
    "file",
    "schema",
    "session",
    "format",
];

pub fn canonical_verb(word: &str) -> Option<&'static str> {
    VERBS
        .iter()
        .find(|(r, p, _, _)| *r == word || (!p.is_empty() && *p == word))
        .map(|(r, _, _, _)| *r)
}

fn plain_verb(canonical: &str) -> &str {
    VERBS
        .iter()
        .find(|(r, p, _, _)| *r == canonical && !p.is_empty())
        .map(|(_, p, _, _)| *p)
        .unwrap_or(canonical)
}

fn takes_target(canonical: &str) -> bool {
    VERBS.iter().any(|(r, _, t, _)| *r == canonical && *t)
}

fn is_clause_start(tok: &str) -> bool {
    tok.starts_with("--") || KEYWORDS.contains(&tok)
}

struct Cur<'a> {
    a: &'a [String],
    i: usize,
}

impl<'a> Cur<'a> {
    fn peek(&self) -> Option<&'a str> {
        self.a.get(self.i).map(|s| s.as_str())
    }
    fn next(&mut self) -> Option<&'a str> {
        let t = self.peek();
        if t.is_some() {
            self.i += 1;
        }
        t
    }
    fn err(&self, pos: usize, msg: impl Into<String>, hint: impl Into<String>) -> ParseError {
        ParseError {
            pos,
            token: self.a.get(pos).cloned().unwrap_or_default(),
            msg: msg.into(),
            hint: hint.into(),
        }
    }
}

fn set_once<T>(
    slot: &mut Option<T>,
    v: T,
    cur: &Cur,
    pos: usize,
    what: &str,
) -> Result<(), ParseError> {
    if slot.is_some() {
        return Err(cur.err(
            pos,
            format!("{what} is given more than once; the last value is never silently chosen"),
            "",
        ));
    }
    *slot = Some(v);
    Ok(())
}

fn split_assignment(tok: &str, cur: &Cur, pos: usize) -> Result<(String, String), ParseError> {
    match tok.split_once('=') {
        Some((k, v)) if !k.is_empty() => Ok((k.to_string(), v.to_string())),
        _ => Err(cur.err(pos, "expected an assignment", "name=value")),
    }
}

/// One or more `k=v` tokens. A flag may carry a single attached assignment.
fn assignments(
    cur: &mut Cur,
    clause_pos: usize,
    attached: Option<&str>,
    into: &mut BTreeMap<String, String>,
    hint: &str,
) -> Result<(), ParseError> {
    let mut found = Vec::new();
    if let Some(a) = attached {
        found.push((clause_pos, split_assignment(a, cur, clause_pos)?));
    } else {
        while let Some(t) = cur.peek() {
            if t.starts_with("--") || !t.contains('=') {
                break;
            }
            let pos = cur.i;
            cur.next();
            found.push((pos, split_assignment(t, cur, pos)?));
        }
    }
    if found.is_empty() {
        return Err(cur.err(clause_pos, "expects at least one assignment", hint));
    }
    for (pos, (k, v)) in found {
        if into.insert(k.clone(), v).is_some() {
            return Err(cur.err(pos, format!("`{k}` is assigned more than once"), ""));
        }
    }
    Ok(())
}

/// A single value. In a value position the token is consumed even when it is
/// spelled like a keyword; a flag value beginning with `-` needs `--flag=value`.
fn value(
    cur: &mut Cur,
    clause_pos: usize,
    attached: Option<&str>,
    is_flag: bool,
    hint: &str,
) -> Result<String, ParseError> {
    if let Some(a) = attached {
        return Ok(a.to_string());
    }
    match cur.peek() {
        Some(t) if is_flag && t.starts_with('-') && t.len() > 1 => Err(cur.err(
            cur.i,
            "a value beginning with `-` must be attached",
            format!("{}=VALUE", cur.a[clause_pos]),
        )),
        Some(t) => {
            cur.next();
            Ok(t.to_string())
        }
        None => Err(cur.err(clause_pos, "expects a value", hint)),
    }
}

fn number(s: &str, cur: &Cur, pos: usize) -> Result<u32, ParseError> {
    s.parse()
        .map_err(|_| cur.err(pos, format!("`{s}` is not a version number"), "version 3"))
}

pub fn parse(args: &[String]) -> Result<Op, ParseError> {
    let mut cur = Cur { a: args, i: 0 };
    let mut op = Op::default();
    let first = match cur.next() {
        Some(f) => f,
        None => return Err(cur.err(0, "no command given", "summonr help")),
    };
    let verb = match first {
        "--help" | "-h" => "help",
        "--version" | "-V" => "version",
        other => canonical_verb(other)
            .ok_or_else(|| cur.err(0, "unknown command", "summonr help"))?,
    };
    op.verb = verb.to_string();
    if verb == "rehearse" {
        op.verb = "invoke".into();
        op.dry_run = true;
    }

    // The first token after a targeted verb is its target by position, even
    // when it is spelled like a keyword.
    if takes_target(verb) {
        if let Some(t) = cur.peek() {
            if !t.starts_with("--") {
                cur.next();
                op.positionals.push(t.to_string());
            }
        }
    }
    while let Some(t) = cur.peek() {
        if is_clause_start(t) {
            break;
        }
        cur.next();
        op.positionals.push(t.to_string());
    }

    let mut returning_seen = false;
    let mut scope_open = false;

    while let Some(tok) = cur.next() {
        let pos = cur.i - 1;
        let (name, attached, is_flag) = if let Some(rest) = tok.strip_prefix("--") {
            match rest.split_once('=') {
                Some((n, v)) => (n, Some(v), true),
                None => (rest, None, true),
            }
        } else {
            (tok, None, false)
        };
        // Normalize flag names onto the keyword they spell.
        let clause = if is_flag {
            match name {
                "json" | "wait" | "dry-run" | "replace" | "simulate" | "baseline" | "live" | "brief" | "all"
                    if attached.is_some() =>
                {
                    return Err(cur.err(
                        pos,
                        format!("flag `--{name}` does not take a value"),
                        format!("--{name}"),
                    ));
                }
                "model" => "through",
                "set" => "at",
                "arg" => "given",
                "arg-file" => "reading",
                "name" => "as",
                "task" => "to",
                "json" => "json",
                "wait" => "wait",
                "timeout" => "within",
                "from" => "from",
                "select" => "selecting",
                "all" => "all",
                "on" => "on",
                "into" => "into",
                "version" => "version",
                "replace" => "replacement",
                "dry-run" => "dry-run",
                "against" => "against",
                "record" => "recording",
                "replay" => "replaying",
                "simulate" => "simulating",
                "baseline" => "baseline",
                "live" => "live",
                "brief" => "brief",
                "key" => "key",
                "url" => "url",
                "via" => "via",
                "children" => "children",
                "tsa-ca" => "tsa-ca",
                "pact" => "pact",
                "reason" => "because",
                "file" => "file",
                "schema" => "schema",
                "session" => "session",
                "format" => "format",
                _ => {
                    return Err(cur.err(pos, "unknown flag", "summonr help"));
                }
            }
        } else if KEYWORDS.contains(&name) {
            name
        } else {
            return Err(cur.err(pos, "expected a clause here", "summonr help"));
        };

        match clause {
            "through" => {
                let v = value(&mut cur, pos, attached, is_flag, "through ollama/model")?;
                set_once(&mut op.model, v, &cur, pos, "the model")?;
            }
            "at" => assignments(
                &mut cur,
                pos,
                attached,
                &mut op.settings,
                "at temperature=0.2",
            )?,
            "given" => assignments(&mut cur, pos, attached, &mut op.args, "given count=100")?,
            "reading" => assignments(&mut cur, pos, attached, &mut op.arg_files, "reading gold=./gold.json")?,
            "on" => assignments(&mut cur, pos, attached, &mut op.on, "on folder=Inbox")?,
            "into" => assignments(
                &mut cur,
                pos,
                attached,
                &mut op.into,
                "into folder=Receipts",
            )?,
            "as" => {
                let v = value(&mut cur, pos, attached, is_flag, "as inbox-rite")?;
                set_once(&mut op.name, v, &cur, pos, "the name")?;
            }
            "to" => {
                let v = value(&mut cur, pos, attached, is_flag, "to \"task text\"")?;
                set_once(&mut op.task, v, &cur, pos, "the task")?;
            }
            "key" => {
                let v = value(&mut cur, pos, attached, is_flag, "key ed25519:…")?;
                set_once(&mut op.key, v, &cur, pos, "the key")?;
            }
            "tsa-ca" => {
                let v = value(&mut cur, pos, attached, is_flag, "tsa-ca ./ca.pem")?;
                set_once(&mut op.tsa_ca, v, &cur, pos, "the timestamp CA file")?;
            }
            "via" => {
                let v = value(&mut cur, pos, attached, is_flag, "via lab")?;
                set_once(&mut op.via, v, &cur, pos, "the peer")?;
            }
            "children" => {
                let v = value(&mut cur, pos, attached, is_flag, "children s-1234")?;
                set_once(&mut op.children, v, &cur, pos, "the parent run")?;
            }
            "url" => {
                let v = value(&mut cur, pos, attached, is_flag, "url http://host:port")?;
                set_once(&mut op.url, v, &cur, pos, "the URL")?;
            }
            "pact" => {
                let v = value(&mut cur, pos, attached, is_flag, "pact order-parts")?;
                set_once(&mut op.pact, v, &cur, pos, "the pact")?;
            }
            "because" => {
                let v = value(&mut cur, pos, attached, is_flag, "because \"the reason\"")?;
                set_once(&mut op.reason, v, &cur, pos, "the reason")?;
            }
            "file" => {
                let v = value(&mut cur, pos, attached, is_flag, "file ./notes.txt")?;
                set_once(&mut op.file, v, &cur, pos, "the file path")?;
            }
            "schema" => {
                let v = value(&mut cur, pos, attached, is_flag, "schema '{\"type\":\"object\"}'")?;
                set_once(&mut op.schema, v, &cur, pos, "the schema")?;
            }
            "session" => {
                let v = value(&mut cur, pos, attached, is_flag, "session m-1234")?;
                set_once(&mut op.session, v, &cur, pos, "the session ID")?;
            }
            "format" => {
                let v = value(&mut cur, pos, attached, is_flag, "--format json")?;
                set_once(&mut op.format, v, &cur, pos, "the output format")?;
            }
            "against" => {
                let v = value(&mut cur, pos, attached, is_flag, "against ./set.yaml")?;
                set_once(&mut op.against, v, &cur, pos, "the assay set")?;
            }
            "replaying" => {
                let v = value(&mut cur, pos, attached, is_flag, "replaying ./c.json")?;
                set_once(&mut op.replay, v, &cur, pos, "the cassette path")?;
            }
            "recording" => {
                let v = value(&mut cur, pos, attached, is_flag, "recording ./c.json")?;
                set_once(&mut op.record, v, &cur, pos, "the cassette path")?;
            }
            "within" => {
                let v = value(&mut cur, pos, attached, is_flag, "within 5m")?;
                set_once(&mut op.timeout, v, &cur, pos, "the wait timeout")?;
            }
            "returning" => {
                if returning_seen {
                    return Err(cur.err(pos, "response timing is given more than once", ""));
                }
                returning_seen = true;
                if cur.peek() == Some("json") {
                    cur.next();
                    op.json = true;
                }
            }
            "then" => {
                if cur.peek() != Some("return") {
                    return Err(cur.err(
                        pos,
                        "`then` must be followed by `return`",
                        "then return json",
                    ));
                }
                cur.next();
                if op.wait {
                    return Err(cur.err(pos, "response timing is given more than once", ""));
                }
                op.wait = true;
                if cur.peek() == Some("json") {
                    cur.next();
                    op.json = true;
                }
            }
            "wait" => {
                if op.wait {
                    return Err(cur.err(pos, "response timing is given more than once", ""));
                }
                op.wait = true;
            }
            "json" => {
                if op.json {
                    return Err(cur.err(pos, "JSON output is given more than once", ""));
                }
                op.json = true;
            }
            "dry-run" => {
                if op.dry_run {
                    return Err(cur.err(pos, "dry-run is given more than once", ""));
                }
                op.dry_run = true;
            }
            "replacement" => {
                if op.replace {
                    return Err(cur.err(pos, "replacement is given more than once", ""));
                }
                op.replace = true;
            }
            "simulating" => {
                if op.simulate {
                    return Err(cur.err(pos, "simulation is given more than once", ""));
                }
                op.simulate = true;
            }
            "baseline" => {
                if op.baseline {
                    return Err(cur.err(pos, "baseline is given more than once", ""));
                }
                op.baseline = true;
            }
            "live" => {
                if op.live {
                    return Err(cur.err(pos, "live mode is given more than once", ""));
                }
                op.live = true;
            }
            "brief" => {
                if op.brief {
                    return Err(cur.err(pos, "brief is given more than once", ""));
                }
                op.brief = true;
            }
            "from" => {
                let v = value(&mut cur, pos, attached, is_flag, "from mail selecting read")?;
                if op.scopes.iter().any(|s| s.reagent == v) {
                    return Err(cur.err(
                        pos,
                        format!("reagent `{v}` is scoped more than once"),
                        "",
                    ));
                }
                op.scopes.push(ReagentScope {
                    reagent: v,
                    ..Default::default()
                });
                scope_open = true;
            }
            "selecting" => {
                if !scope_open {
                    return Err(cur.err(
                        pos,
                        "no reagent scope is open",
                        "from mail selecting read",
                    ));
                }
                let mut names: Vec<String> = Vec::new();
                if is_flag {
                    let v = value(&mut cur, pos, attached, true, "--select read,search")?;
                    names.extend(v.split(',').filter(|s| !s.is_empty()).map(String::from));
                } else {
                    while let Some(t) = cur.peek() {
                        if is_clause_start(t) {
                            break;
                        }
                        cur.next();
                        names.push(t.to_string());
                    }
                }
                if names.is_empty() {
                    return Err(cur.err(
                        pos,
                        "expects at least one tool name",
                        "selecting read search",
                    ));
                }
                let scope = op.scopes.last_mut().unwrap();
                for n in names {
                    if scope.select.contains(&n) {
                        return Err(cur.err(
                            pos,
                            format!("tool `{n}` is selected more than once"),
                            "",
                        ));
                    }
                    scope.select.push(n);
                }
            }
            "all" => {
                if scope_open {
                    op.scopes.last_mut().unwrap().all = true;
                } else if op.verb == "petition" {
                    op.all = true;
                } else {
                    return Err(cur.err(pos, "no reagent scope is open", "from mail all"));
                }
            }
            "version" => {
                let v = value(&mut cur, pos, attached, is_flag, "version 3")?;
                let n = number(&v, &cur, pos)?;
                if scope_open {
                    let scope = op.scopes.last_mut().unwrap();
                    set_once(&mut scope.version, n, &cur, pos, "the reagent version")?;
                } else {
                    set_once(&mut op.version, n, &cur, pos, "the version")?;
                }
            }
            _ => unreachable!("clause table and match are out of step"),
        }
    }

    let last = args.len().saturating_sub(1);
    if returning_seen && op.wait {
        return Err(cur.err(
            last,
            "`returning` and `then return` cannot both appear",
            "then return json",
        ));
    }
    if op.timeout.is_some() && !op.wait && op.verb != "await" {
        return Err(cur.err(
            last,
            "a wait timeout needs a wait",
            "then return json within 5m",
        ));
    }
    for s in &op.scopes {
        if s.all && !s.select.is_empty() {
            return Err(cur.err(
                last,
                format!("reagent `{}` has both `all` and a selection", s.reagent),
                "",
            ));
        }
    }
    if op.pact.is_some() && op.verb != "familiar" {
        return Err(cur.err(last, "`pact` names a pact only in `familiar allow`", "familiar allow codex pact order-parts"));
    }
    if op.reason.is_some() && !matches!(op.verb.as_str(), "pact" | "reliquary" | "rite") {
        return Err(cur.err(last, "`because` gives a reason only to pact, reliquary, or rite commands", "pact reject order-parts 3 because late"));
    }
    if op.children.is_some() && op.verb != "graveyard" {
        return Err(cur.err(last, "`children` lists a run's child summons only with history", "history children s-1234"));
    }
    if let Some(k) = op.arg_files.keys().find(|k| op.args.contains_key(*k)) {
        return Err(cur.err(
            last,
            format!("`{k}` is given both on the command line and from a file"),
            "",
        ));
    }
    Ok(op)
}

fn push_assignments(out: &mut Vec<String>, lead: &str, map: &BTreeMap<String, String>) {
    if !map.is_empty() {
        out.push(lead.to_string());
        out.extend(map.iter().map(|(k, v)| format!("{k}={v}")));
    }
}

/// Render an operation in one spelling. `parse(render(op, style)) == op`.
pub fn render(op: &Op, style: Style) -> Vec<String> {
    let kw = style == Style::Keyword;
    let mut out = Vec::new();
    let rehearse = kw && op.verb == "invoke" && op.dry_run;
    out.push(if rehearse {
        "rehearse".to_string()
    } else if kw {
        op.verb.clone()
    } else {
        plain_verb(&op.verb).to_string()
    });
    out.extend(op.positionals.iter().cloned());
    let pair = |out: &mut Vec<String>, k: &str, f: &str, v: &Option<String>| {
        if let Some(v) = v {
            if kw {
                out.push(k.into());
                out.push(v.clone());
            } else {
                out.push(format!("--{f}={v}"));
            }
        }
    };
    if let Some(v) = op.version {
        out.push(if kw {
            "version".into()
        } else {
            "--version".into()
        });
        out.push(v.to_string());
    }
    if op.all {
        out.push(if kw { "all".into() } else { "--all".into() });
    }
    pair(&mut out, "through", "model", &op.model);
    push_assignments(&mut out, if kw { "at" } else { "--set" }, &op.settings);
    push_assignments(&mut out, if kw { "reading" } else { "--arg-file" }, &op.arg_files);
    push_assignments(&mut out, if kw { "given" } else { "--arg" }, &op.args);
    pair(&mut out, "as", "name", &op.name);
    pair(&mut out, "to", "task", &op.task);
    for s in &op.scopes {
        if kw {
            out.push("from".into());
            out.push(s.reagent.clone());
        } else {
            out.push(format!("--from={}", s.reagent));
        }
        if let Some(v) = s.version {
            out.push(if kw {
                "version".into()
            } else {
                "--version".into()
            });
            out.push(v.to_string());
        }
        if s.all {
            out.push(if kw { "all".into() } else { "--all".into() });
        } else if !s.select.is_empty() {
            if kw {
                out.push("selecting".into());
                out.extend(s.select.iter().cloned());
            } else {
                out.push(format!("--select={}", s.select.join(",")));
            }
        }
    }
    push_assignments(&mut out, if kw { "on" } else { "--on" }, &op.on);
    push_assignments(&mut out, if kw { "into" } else { "--into" }, &op.into);
    pair(&mut out, "via", "via", &op.via);
    pair(&mut out, "children", "children", &op.children);
    pair(&mut out, "tsa-ca", "tsa-ca", &op.tsa_ca);
    pair(&mut out, "url", "url", &op.url);
    pair(&mut out, "pact", "pact", &op.pact);
    pair(&mut out, "because", "reason", &op.reason);
    pair(&mut out, "key", "key", &op.key);
    pair(&mut out, "against", "against", &op.against);
    pair(&mut out, "recording", "record", &op.record);
    pair(&mut out, "replaying", "replay", &op.replay);
    if op.simulate {
        out.push(if kw {
            "simulating".into()
        } else {
            "--simulate".into()
        });
    }
    if op.baseline {
        out.push(if kw {
            "baseline".into()
        } else {
            "--baseline".into()
        });
    }
    if op.live {
        out.push(if kw { "live".into() } else { "--live".into() });
    }
    if op.brief {
        out.push(if kw { "brief".into() } else { "--brief".into() });
    }
    if op.replace {
        out.push(if kw {
            "replacement".into()
        } else {
            "--replace".into()
        });
    }
    if op.dry_run && !rehearse {
        out.push("--dry-run".into());
    }
    if op.wait {
        if kw {
            out.push("then".into());
            out.push("return".into());
            if op.json {
                out.push("json".into());
            }
        } else {
            out.push("--wait".into());
            if op.json {
                out.push("--json".into());
            }
        }
    } else if op.json {
        if kw {
            out.push("returning".into());
            out.push("json".into());
        } else {
            out.push("--json".into());
        }
    }
    pair(&mut out, "within", "timeout", &op.timeout);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn p(s: &str) -> Result<Op, ParseError> {
        let args: Vec<String> = s.split_whitespace().map(String::from).collect();
        parse(&args)
    }

    /// Each pair is the same operation on the ritual surface and the plain
    /// surface. The parser must produce identical operations.
    const PAIRS: &[(&str, &str)] = &[
        ("console", "shell"),
        ("job work sorting given count=8 lease=5m returning json", "queue work sorting --arg count=8 --arg lease=5m --json"),
        ("job create items.json returning json", "queue create items.json --json"),
        ("job status sorting given offset=100 returning json", "queue status sorting --arg offset=100 --json"),
        ("job item sorting item-1 returning json", "queue item sorting item-1 --json"),
        ("job retry sorting item-1 returning json", "queue retry sorting item-1 --json"),
        ("daemon", "panel"),
        ("scry s-1", "watch s-1"),
        (
            "invoke inbox-rite version 3 through ollama/example at temperature=0.2 given count=100 then return json within 5m",
            "run inbox-rite --version 3 --model ollama/example --set temperature=0.2 --arg count=100 --wait --json --timeout 5m",
        ),
        ("invoke inbox-rite given count=100 apply=false", "run inbox-rite --arg count=100 --arg apply=false"),
        ("invoke inbox-rite returning json", "run inbox-rite --json"),
        ("rehearse inbox-rite given count=100", "run inbox-rite --arg count=100 --dry-run"),
        ("invoke r recording ./c.json", "run r --record ./c.json"),
        ("invoke r replaying ./c.json", "run r --replay ./c.json"),
        ("invoke r simulating", "run r --simulate"),
        (
            "grant inbox-rite from mail selecting search read on folder=Inbox",
            "allow inbox-rite --from mail --select search,read --on folder=Inbox",
        ),
        (
            "grant inbox-rite from mail selecting move on folder=Inbox into folder=Receipts",
            "allow inbox-rite --from mail --select move --on folder=Inbox --into folder=Receipts",
        ),
        ("grant inbox-rite from mail version 2 all", "allow inbox-rite --from mail --version 2 --all"),
        ("revoke inbox-rite from mail selecting move", "disallow inbox-rite --from=mail --select=move"),
        ("inscribe ./r.yaml as inbox-rite replacement", "save ./r.yaml --name inbox-rite --replace"),
        ("assay inbox-rite against ./set.yaml baseline", "test inbox-rite --against ./set.yaml --baseline"),
        ("assay inbox-rite against ./set.yaml live", "test inbox-rite --against ./set.yaml --live"),
        ("await s-1 within 30s then return", "wait s-1 --wait --timeout 30s"),
        ("await s-1 within 20s", "wait s-1 --timeout 20s"),
        ("grimoire read inbox-rite version 2", "rituals read inbox-rite --version 2"),
        ("elegy s-1 returning json", "record s-1 --json"),
        ("validate ./r.yaml returning json", "compile ./r.yaml --json"),
        ("petition permit all", "approval permit --all"),
        ("identity", "whoami"),
        ("epitaph s-1", "receipt s-1"),
        ("witness accept b key ed25519:x", "witness accept b --key ed25519:x"),
        ("verify ./c.json tsa-ca ./ca.pem", "verify ./c.json --tsa-ca ./ca.pem"),
        ("listen 0.0.0.0:7744", "serve-signed 0.0.0.0:7744"),
        ("mcp as codex via lab", "mcp --name codex --via lab"),
        ("familiar add codex key ed25519:x", "agent add codex --key ed25519:x"),
        ("familiar allow codex total-receipt route-form", "agent allow codex total-receipt route-form"),
        ("familiar allow codex pact order-parts", "agent allow codex --pact order-parts"),
        ("pact offer b ./terms.yaml", "contract offer b ./terms.yaml"),
        ("pact offer b ./terms.yaml as orders", "contract offer b ./terms.yaml --name orders"),
        ("pact fetch b", "contract fetch b"),
        ("pact list", "contract list"),
        ("pact show order-parts", "contract show order-parts"),
        ("pact agree order-parts", "contract agree order-parts"),
        ("pact decline order-parts because too-dear", "contract decline order-parts --reason too-dear"),
        ("pact call order-parts given amount=5", "contract call order-parts --arg amount=5"),
        ("pact accept order-parts 3", "contract accept order-parts 3"),
        ("pact reject order-parts 3 because late", "contract reject order-parts 3 --reason late"),
        ("pact history order-parts", "contract history order-parts"),
        ("pact export order-parts ./t.json", "contract export order-parts ./t.json"),
        ("pact admit m key ed25519:x", "contract admit m --key ed25519:x"),
        ("pact limit order-parts tool_calls=3", "contract limit order-parts tool_calls=3"),
        ("pact end order-parts because done", "contract end order-parts --reason done"),
        ("pact status order-parts 3", "contract status order-parts 3"),
        ("peer add b url http://h:1 key ed25519:x", "peers add b --url http://h:1 --key ed25519:x"),
        ("petition deny p-1 p-2", "approval deny p-1 p-2"),
        ("glean classify-message 10 against ./set.yaml", "sample classify-message 10 --against ./set.yaml"),
        ("drill route-form against ./d.yaml", "cases route-form --against ./d.yaml"),
        ("invoke bench reading gold=./g.json", "run bench --arg-file gold=./g.json"),
        ("invoke bench then return json brief", "run bench --wait --json --brief"),
        ("elegy s-1 brief returning json", "record s-1 --brief --json"),
        ("rite start ./r.yaml reading hunts=./h.json given n=2", "workflow start ./r.yaml --arg-file hunts=./h.json --arg n=2"),
        ("graveyard children s-1 returning json", "history --children s-1 --json"),
    ];

    #[test]
    fn children_belongs_to_history() {
        assert_eq!(p("history --children s-9").unwrap().children.as_deref(), Some("s-9"));
        assert!(p("invoke r children s-9").is_err());
    }

    #[test]
    fn pact_clauses_belong_to_their_verbs() {
        assert!(p("invoke r pact x").is_err(), "`pact` is a clause of familiar allow only");
        assert!(p("invoke r because x").is_err(), "`because` is a clause of pact only");
        let op = p("familiar allow codex pact order-parts").unwrap();
        assert_eq!(op.pact.as_deref(), Some("order-parts"));
        let op = p("pact reject order-parts 3 because late").unwrap();
        assert_eq!(op.reason.as_deref(), Some("late"));
        assert_eq!(op.positionals, vec!["reject", "order-parts", "3"]);
    }

    #[test]
    fn spellings_agree() {
        for (kw, fl) in PAIRS {
            let a = p(kw).unwrap_or_else(|e| panic!("{kw}: {e}"));
            let b = p(fl).unwrap_or_else(|e| panic!("{fl}: {e}"));
            assert_eq!(a, b, "\n  keyword: {kw}\n  flag:    {fl}");
        }
    }

    #[test]
    fn render_round_trips() {
        for (kw, _) in PAIRS {
            let op = p(kw).unwrap();
            for style in [Style::Keyword, Style::Flag] {
                let again =
                    parse(&render(&op, style)).unwrap_or_else(|e| panic!("{kw} as {style:?}: {e}"));
                assert_eq!(op, again, "{kw} as {style:?}");
            }
        }
    }

    #[test]
    fn spellings_mix() {
        let a = p("invoke inbox-rite given count=5 --wait --json").unwrap();
        let b = p("invoke inbox-rite given count=5 then return json").unwrap();
        assert_eq!(a, b);
    }

    #[test]
    fn duplicates_across_spellings_are_errors() {
        assert!(p("invoke r through a --model b").is_err());
        assert!(p("invoke r given count=1 --arg count=2").is_err());
        // An input comes from the command line or from a file, never both.
        assert!(p("invoke r given gold=1 reading gold=./g.json").is_err());
        assert!(p("invoke r --arg-file gold=./a.json --arg-file gold=./b.json").is_err());
        assert!(p("invoke r returning json then return json").is_err());
        assert!(p("invoke r --wait then return").is_err());
    }

    #[test]
    fn scoping_rules() {
        assert!(p("invoke r within 5m").is_err(), "timeout needs a wait");
        assert!(
            p("grant r selecting read").is_err(),
            "selection needs a scope"
        );
        assert!(p("grant r from mail all selecting read").is_err());
        let op = p("grant r version 2 from mail version 3 selecting read").unwrap();
        assert_eq!(op.version, Some(2));
        assert_eq!(op.scopes[0].version, Some(3));
    }

    #[test]
    fn flags_escape_keyword_shaped_names() {
        // A tool literally named `all` or `from` cannot be selected by keyword.
        let op = p("grant r --from mail --select from,all").unwrap();
        assert_eq!(op.scopes[0].select, vec!["from", "all"]);
        assert!(!op.scopes[0].all);
        // The target is positional even when spelled like a keyword.
        let op = p("invoke through --arg x=1").unwrap();
        assert_eq!(op.positionals, vec!["through"]);
        // Keyword-shaped values are data in a value position.
        let op = p("invoke r given text=through").unwrap();
        assert_eq!(op.args["text"], "through");
        let op = p("inscribe ./r.yaml --name=through").unwrap();
        assert_eq!(op.name.as_deref(), Some("through"));
    }

    #[test]
    fn no_shortcut_flags() {
        assert!(p("invoke r --temperature 0.2").is_err());
        assert!(p("invoke r --count 100").is_err());
    }

    #[test]
    fn errors_point_at_the_token() {
        let e = p("invoke r given count=1 nonsense").unwrap_err();
        assert_eq!(e.pos, 4);
        assert_eq!(e.token, "nonsense");
    }

    #[test]
    fn manual_summon_paired_spellings() {
        assert_eq!(
            p("summon scratch through mock/keyword given time=5m returning json").unwrap(),
            p("session scratch --model mock/keyword --arg time=5m --json").unwrap(),
        );
        assert_eq!(
            p("soul m-1 to concise").unwrap(),
            p("system m-1 --task concise").unwrap(),
        );
        assert_eq!(
            p("bind m-1 as notes file ./notes.txt").unwrap(),
            p("attach m-1 --name notes --file ./notes.txt").unwrap(),
        );
        assert_eq!(
            p("imbue m-1 at temperature=0.2 given time=2m").unwrap(),
            p("configure m-1 --set temperature=0.2 --arg time=2m").unwrap(),
        );
        assert_eq!(
            p("bid m-1 to summarize schema '{\"type\":\"object\"}' returning json").unwrap(),
            p("ask m-1 --task summarize --schema '{\"type\":\"object\"}' --json").unwrap(),
        );
        assert_eq!(
            p("lethe m-1 version 2 returning json").unwrap(),
            p("forget m-1 --version 2 --json").unwrap(),
        );
        assert_eq!(
            p("inscribe my-rite session m-1 replacement returning json").unwrap(),
            p("save my-rite --session m-1 --replace --json").unwrap(),
        );
        assert_eq!(
            p("sweep my-rite against ./sweep.yaml live returning json").unwrap(),
            p("experiment my-rite --against ./sweep.yaml --live --json").unwrap(),
        );
        assert_eq!(
            p("judge x-1 against my-touchstone returning json").unwrap(),
            p("score x-1 --against my-touchstone --json").unwrap(),
        );
        assert_eq!(
            p("weigh x-1 against x-2 returning json").unwrap(),
            p("rank x-1 --against x-2 --json").unwrap(),
        );
        assert_eq!(
            p("fence apply fk-1 returning json").unwrap(),
            p("reconcile apply fk-1 --json").unwrap(),
        );
        assert_eq!(
            p("reliquary publish my-ritual version 2 returning json").unwrap(),
            p("registry publish my-ritual --version 2 --json").unwrap(),
        );
        assert_eq!(
            p("reliquary import pack.json key ed25519:abc replacement returning json").unwrap(),
            p("registry import pack.json --key ed25519:abc --replace --json").unwrap(),
        );
        assert_eq!(
            p("rite start release.yaml given repo=summonr returning json").unwrap(),
            p("workflow start release.yaml --arg repo=summonr --json").unwrap(),
        );
        assert_eq!(
            p("rite falsify rite-1 base_ok because broken returning json").unwrap(),
            p("workflow falsify rite-1 base_ok --reason broken --json").unwrap(),
        );
    }
}
