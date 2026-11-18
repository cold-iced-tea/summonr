//! A persistent command client with a live, non-authoritative instrument pane.
//! Commands are argv, never shell programs. No inference occurs without a command.
use crate::{
    grammar,
    outcome::Outcome,
    plan, ritual,
    rpc::{Client, RpcErr},
    theme::{self, Ink},
    util::{self, Paths},
};
use ratatui::crossterm::{
    event::{
        self, DisableBracketedPaste, EnableBracketedPaste, Event, KeyCode, KeyEventKind,
        KeyModifiers,
    },
    execute,
};
use ratatui::{
    layout::{Constraint, Layout},
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::{Block, BorderType, Paragraph, Wrap},
    Frame,
};
use serde::Deserialize;
use serde_json::{json, Value};
use std::{
    collections::VecDeque,
    io::{IsTerminal, Read},
    process::{Command, Stdio},
    sync::mpsc::{self, Receiver},
    time::{Duration, Instant},
};

#[derive(Default, Deserialize)]
struct CoreStatus {
    pid: u32,
    version: String,
    active: usize,
    ended: usize,
    petitions: usize,
}
#[derive(Clone, Deserialize)]
struct RunInfo {
    id: String,
    ritual: String,
    status: String,
    outcome: Option<Outcome>,
    model: Option<String>,
    hash: String,
    requests: u64,
    tokens: u64,
    started: i64,
    ended: Option<i64>,
    done: u64,
    total: u64,
    essence: ritual::Essence,
    #[serde(default)]
    cancellation_requested: bool,
}
#[derive(Deserialize)]
struct Ledger {
    seq: i64,
    text: String,
}
struct Snapshot {
    selection_error: Option<String>,
    core: CoreStatus,
    runs: Vec<RunInfo>,
    selected: Option<RunInfo>,
    events: Vec<Ledger>,
}
struct Diagram {
    title: String,
    lines: Vec<Line<'static>>,
}
enum Reply {
    Command { text: String, code: i32 },
    Diagram(Diagram),
}
struct Pending {
    control: bool,
    id: u64,
    rx: Receiver<Result<Reply, String>>,
}
struct App {
    input: String,
    cursor: usize,
    history: VecDeque<String>,
    history_index: Option<usize>,
    draft: String,
    transcript: VecDeque<Line<'static>>,
    scroll: u16,
    pending: Vec<Pending>,
    sequence: u64,
    core: CoreStatus,
    runs: Vec<RunInfo>,
    selected: Option<RunInfo>,
    focus: Option<String>,
    events: VecDeque<Ledger>,
    connected: bool,
    error: String,
    diagram: Option<Diagram>,
    show_diagram: bool,
    side_scroll: u16,
    side_visible: bool,
    poll: Option<Receiver<Result<Snapshot, String>>>,
    poll_focus: Option<String>,
    last_poll: Instant,
}
fn ink(color: Ink) -> Style {
    Style::default().fg(color.color())
}
/// The hash's M9 seal, indented under its hex line; nothing for a non-hex hash.
fn seal_lines(hash: &str) -> Vec<Line<'static>> {
    theme::seal(hash)
        .map(|rows| rows.into_iter().map(|r| line(Ink::Gold, format!("     {r}"))).collect())
        .unwrap_or_default()
}

fn line(color: Ink, text: impl Into<String>) -> Line<'static> {
    Line::from(Span::styled(text.into(), ink(color)))
}
fn block(title: impl Into<String>) -> Block<'static> {
    Block::bordered()
        .border_type(if theme::ascii() {
            BorderType::Plain
        } else {
            BorderType::Rounded
        })
        .border_style(ink(Ink::Rule))
        .title(Span::styled(format!(" {} ", title.into()), ink(Ink::Gold)))
}
fn decode<T: serde::de::DeserializeOwned>(v: Value) -> Result<T, String> {
    serde_json::from_value(v).map_err(|e| format!("invalid core response: {e}"))
}
fn clean(s: &str) -> String {
    s.chars()
        .map(|c| {
            if c == '\n' || !c.is_control() {
                c
            } else if c == '\t' {
                ' '
            } else {
                '?'
            }
        })
        .collect()
}

impl App {
    fn new() -> Self {
        let mut app = Self {
            input: String::new(),
            cursor: 0,
            history: VecDeque::new(),
            history_index: None,
            draft: String::new(),
            transcript: VecDeque::new(),
            scroll: 0,
            pending: Vec::new(),
            sequence: 0,
            core: CoreStatus::default(),
            runs: Vec::new(),
            selected: None,
            focus: None,
            events: VecDeque::new(),
            connected: false,
            error: String::new(),
            diagram: None,
            show_diagram: false,
            side_scroll: 0,
            side_visible: true,
            poll: None,
            poll_focus: None,
            last_poll: Instant::now() - Duration::from_secs(1),
        };
        app.append(Ink::Gold, "S U M M O N R   /   THE WORKBENCH");
        app.append(Ink::Bone, "Deterministic work. Deliberate inference.");
        app.append(
            Ink::Dim,
            "Type commands directly. There is no need to prefix them with summonr.",
        );
        app.append(
            Ink::Dim,
            "rituals   |   run NAME --arg key=value   |   inspect NAME",
        );
        app.append(
            Ink::Dim,
            "F2 status/plan  F3 next run  F4 sidebar  PgUp/Dn scroll  exit leaves work running",
        );
        app.append(
            Ink::Dim,
            "This is a Summonr console, not a system shell. Use quotes for values with spaces.",
        );
        app.append(Ink::Dim, "");
        app
    }
    fn append(&mut self, color: Ink, text: &str) {
        for text in clean(text).lines() {
            self.transcript.push_back(line(color, text.to_string()));
        }
        while self.transcript.len() > 2000 {
            self.transcript.pop_front();
        }
    }
    fn start_poll(&mut self, paths: &Paths) {
        if self.poll.is_some() || self.last_poll.elapsed() < Duration::from_millis(500) {
            return;
        }
        let paths = paths.clone();
        let focus = self.focus.clone();
        let previous = self.selected.as_ref().map(|r| r.id.clone());
        let cursor = self.events.back().map(|e| e.seq).unwrap_or(0);
        let (tx, rx) = mpsc::channel();
        self.poll_focus = self.focus.clone();
        self.poll = Some(rx);
        self.last_poll = Instant::now();
        std::thread::spawn(move || {
            let result =
                (|| -> Result<Snapshot, String> {
                    let mut client = Client::connect_existing(&paths).map_err(|e| e.message)?;
                    let mut call = |method, p| client.call(method, p).map_err(|e| e.message);
                    let core = decode(call("ping", json!({}))?)?;
                    let mut runs: Vec<RunInfo> =
                        decode(call("list", json!({"scope":"active","limit":100}))?)?;
                    runs.sort_by_key(|r| std::cmp::Reverse(r.started));
                    runs.extend(decode::<Vec<RunInfo>>(call(
                        "list",
                        json!({"scope":"ended","limit":50}),
                    )?)?);
                    let mut selection_error = None;
                    let selected = match focus.or_else(|| runs.first().map(|r| r.id.clone())) {
                        Some(id) => match client.call("get", json!({"summon":id})) {
                            Ok(value) => Some(decode::<RunInfo>(value)?),
                            Err(e) if e.exit == 3 => {
                                selection_error = Some(e.message);
                                None
                            }
                            Err(e) => return Err(e.message),
                        },
                        None => None,
                    };
                    let after = if selected.as_ref().map(|r| &r.id) == previous.as_ref() {
                        cursor
                    } else {
                        0
                    };
                    let events =
                        if let Some(run) = &selected {
                            decode(client.call(
                        "events",
                        json!({"summon":run.id,"after":after,"limit":1000,"slim":true}),
                    ).map_err(|e|e.message)?)?
                        } else {
                            Vec::new()
                        };
                    Ok(Snapshot {
                        selection_error,
                        core,
                        runs,
                        selected,
                        events,
                    })
                })();
            let _ = tx.send(result);
        });
    }
    fn collect(&mut self) {
        if let Some(result) = self.poll.as_ref().and_then(|rx| rx.try_recv().ok()) {
            self.poll = None;
            match result {
                Ok(s) => {
                    self.core = s.core;
                    self.runs = s.runs;
                    if self.focus == self.poll_focus {
                        if self.selected.as_ref().map(|r| &r.id)
                            != s.selected.as_ref().map(|r| &r.id)
                        {
                            self.events.clear();
                        }
                        self.selected = s.selected;
                        if let Some(error) = s.selection_error {
                            self.append(Ink::Rubric, &error);
                            self.focus = None;
                        }
                        self.events.extend(s.events);
                    }
                    while self.events.len() > 200 {
                        self.events.pop_front();
                    }
                    self.connected = true;
                    self.error.clear();
                }
                Err(e) => {
                    self.connected = false;
                    self.error = e;
                    if self.focus == self.poll_focus {
                        self.focus = None;
                    }
                }
            }
        }
        let mut completed = Vec::new();
        for p in &self.pending {
            if let Ok(result) = p.rx.try_recv() {
                completed.push((p.id, result));
            }
        }
        for (id, result) in completed {
            self.pending.retain(|p| p.id != id);
            match result {
                Ok(Reply::Command { text, code }) => {
                    self.append(Ink::Bone, &text);
                    self.append(
                        if code == 0 {
                            Ink::Verdigris
                        } else {
                            Ink::Rubric
                        },
                        &format!("[{id}] exit {code}"),
                    );
                }
                Ok(Reply::Diagram(d)) => {
                    self.append(Ink::Verdigris, &format!("[{id}] loaded {}", d.title));
                    self.diagram = Some(d);
                    self.show_diagram = true;
                    self.side_visible = true;
                    self.side_scroll = 0;
                }
                Err(e) => self.append(Ink::Rubric, &format!("[{id}] {e}")),
            }
            self.append(Ink::Dim, "");
            self.last_poll = Instant::now() - Duration::from_secs(1);
        }
    }
    fn slot_available(&self, control: bool) -> bool {
        if control {
            !self.pending.iter().any(|p| p.control)
        } else {
            self.pending.iter().filter(|p| !p.control).count() < 8
        }
    }
    fn submit(&mut self, paths: &Paths) -> bool {
        if self.input.trim().is_empty() {
            return false;
        }
        let words = match words(&self.input) {
            Ok(w) => w,
            Err(e) => {
                let rejected = std::mem::take(&mut self.input);
                self.cursor = 0;
                self.history.push_back(rejected.clone());
                while self.history.len() > 100 {
                    self.history.pop_front();
                }
                self.history_index = None;
                self.append(Ink::Gold, &format!("> {rejected}"));
                self.append(
                    Ink::Rubric,
                    &format!("{e} Use Up to edit the rejected command."),
                );
                return false;
            }
        };
        if words.is_empty() {
            return false;
        }
        let command_args = if words[0] == "summonr" {
            &words[1..]
        } else {
            &words[..]
        };
        let control = grammar::parse(command_args).is_ok_and(|op| op.verb == "banish");
        let local = command_args.first().is_some_and(|s| {
            matches!(
                s.as_str(),
                "exit" | "quit" | "clear" | "daemon" | "panel" | "scry" | "watch" | "follow"
            )
        });
        let full = !self.slot_available(control);
        if full && !local {
            self.append(Ink::Rubric, if control { "A stop request is already pending; its reserved slot will reopen shortly." } else { "Eight commands are already pending; stop has a separate reserved slot. Your command remains in the editor." });
            return false;
        }
        let text = std::mem::take(&mut self.input);
        self.cursor = 0;
        self.history_index = None;
        self.scroll = 0;
        self.history.push_back(text.clone());
        while self.history.len() > 100 {
            self.history.pop_front();
        }
        self.append(Ink::Gold, &format!("> {text}"));
        let args = if words[0] == "summonr" {
            words[1..].to_vec()
        } else {
            words
        };
        if args.is_empty() {
            self.append(Ink::Dim, "You are already in the workbench.");
            return false;
        }
        match args[0].as_str() {
            "exit" | "quit" if args.len() == 1 => return true,
            "clear" if args.len() == 1 => {
                self.transcript.clear();
                return false;
            }
            "daemon" | "panel" if args.len() == 1 => {
                self.show_diagram = false;
                self.side_visible = true;
                return false;
            }
            "follow" if args.len() == 1 => {
                self.focus = None;
                self.show_diagram = false;
                self.side_visible = true;
                return false;
            }
            "scry" | "watch" if args.len() == 2 => {
                self.focus = Some(args[1].clone());
                self.show_diagram = false;
                self.side_visible = true;
                return false;
            }
            _ => {}
        }
        if args[0] != "inspect" {
            let op = match grammar::parse(&args) {
                Ok(o) => o,
                Err(e) => {
                    self.append(Ink::Rubric, &e.to_string());
                    return false;
                }
            };
            if matches!(op.verb.as_str(), "scry" | "daemon" | "console" | "mcp")
                || (op.verb == "core" && op.positionals.is_empty())
            {
                self.append(Ink::Dim,"Use F2/F3 for views here. Run foreground servers and standalone observers in another terminal.");
                return false;
            }
        } else if args.len() != 2 {
            self.append(Ink::Rubric, "Use inspect NAME for a saved ritual.");
            return false;
        }
        self.sequence += 1;
        let id = self.sequence;
        let (tx, rx) = mpsc::channel();
        self.pending.push(Pending { id, rx, control });
        let paths = paths.clone();
        std::thread::spawn(move || {
            let result = if args[0] == "inspect" {
                inspect(&paths, &args[1]).map(Reply::Diagram)
            } else {
                run_command(&args)
            };
            let _ = tx.send(result);
        });
        false
    }
    fn history(&mut self, up: bool) {
        if self.history.is_empty() {
            return;
        }
        if up {
            let index = match self.history_index {
                None => {
                    self.draft = self.input.clone();
                    self.history.len() - 1
                }
                Some(i) => i.saturating_sub(1),
            };
            self.history_index = Some(index);
            self.input = self.history[index].clone();
        } else if let Some(i) = self.history_index {
            if i + 1 == self.history.len() {
                self.history_index = None;
                self.input = self.draft.clone();
            } else {
                self.history_index = Some(i + 1);
                self.input = self.history[i + 1].clone();
            }
        }
        self.cursor = self.input.len();
    }
    fn key(&mut self, key: KeyCode, mods: KeyModifiers, paths: &Paths) -> bool {
        if mods.contains(KeyModifiers::CONTROL) {
            match key {
                KeyCode::Char('d') if self.input.is_empty() => return true,
                KeyCode::Char('c') => {
                    self.input.clear();
                    self.cursor = 0;
                    self.history_index = None;
                    return false;
                }
                KeyCode::Char('l') => {
                    self.transcript.clear();
                    self.scroll = 0;
                    return false;
                }
                KeyCode::Char('a') => {
                    self.cursor = 0;
                    return false;
                }
                KeyCode::Char('e') => {
                    self.cursor = self.input.len();
                    return false;
                }
                _ => return false,
            }
        }
        match key {
            KeyCode::Enter=>return self.submit(paths),
            KeyCode::Char(c)=>{self.input.insert(self.cursor,c);self.cursor+=c.len_utf8();},
            KeyCode::Left=>{self.cursor=self.input[..self.cursor].char_indices().last().map(|(i,_)|i).unwrap_or(0);},
            KeyCode::Right=>{if let Some(c)=self.input[self.cursor..].chars().next(){self.cursor+=c.len_utf8();}},
            KeyCode::Home=>self.cursor=0,KeyCode::End=>self.cursor=self.input.len(),
            KeyCode::Backspace=>{let prev=self.input[..self.cursor].char_indices().last().map(|(i,_)|i).unwrap_or(0);self.input.drain(prev..self.cursor);self.cursor=prev;},
            KeyCode::Delete=>{if let Some(c)=self.input[self.cursor..].chars().next(){self.input.drain(self.cursor..self.cursor+c.len_utf8());}},
            KeyCode::Up=>self.history(true),KeyCode::Down=>self.history(false),
            KeyCode::PageUp if mods.contains(KeyModifiers::ALT)=>self.side_scroll=self.side_scroll.saturating_sub(8),
            KeyCode::PageDown if mods.contains(KeyModifiers::ALT)=>self.side_scroll=self.side_scroll.saturating_add(8),
            KeyCode::PageUp=>self.scroll=self.scroll.saturating_add(8),KeyCode::PageDown=>self.scroll=self.scroll.saturating_sub(8),
            KeyCode::F(2)=>{self.show_diagram = !self.show_diagram;self.side_scroll=0;self.side_visible=true;},
            KeyCode::F(3)=>{if !self.runs.is_empty(){let i=self.selected.as_ref().and_then(|s|self.runs.iter().position(|r|r.id==s.id)).map(|i|(i+1)%self.runs.len()).unwrap_or(0);self.focus=Some(self.runs[i].id.clone());}},
            KeyCode::F(4)=>self.side_visible = !self.side_visible,
            KeyCode::F(1)=>self.append(Ink::Dim,"Commands: rituals, run NAME, record ID, job status NAME, inspect NAME. F2 status/plan; F3 pins next run; follow resumes automatic selection; F4 sidebar; Alt-PgUp/Dn sidebar scroll; PgUp/Dn transcript; arrows edit/history; Ctrl-L clear; Ctrl-C clear input; exit or Ctrl-D leave. Accepted daemon work continues."),
            _=>{},
        }
        false
    }
}

/// Shell-like quoting only. No expansion, substitution, pipelines or shell evaluation.
fn words(input: &str) -> Result<Vec<String>, String> {
    let mut out = Vec::new();
    let mut word = String::new();
    let mut quote = None;
    let mut escaped = false;
    let mut started = false;
    for c in input.chars() {
        if escaped {
            word.push(c);
            escaped = false;
            started = true;
            continue;
        }
        if c == '\\' && quote != Some('\'') {
            escaped = true;
            started = true;
            continue;
        }
        if let Some(q) = quote {
            if c == q {
                quote = None;
            } else {
                word.push(c);
            }
            started = true;
        } else if c == '\'' || c == '"' {
            quote = Some(c);
            started = true;
        } else if c.is_whitespace() {
            if started {
                out.push(std::mem::take(&mut word));
                started = false;
            }
        } else {
            word.push(c);
            started = true;
        }
    }
    if escaped || quote.is_some() {
        return Err("Unfinished quote or escape; nothing was executed.".into());
    }
    if started {
        out.push(word);
    }
    Ok(out)
}
fn bounded_read(mut stream: impl Read) -> Result<String, String> {
    const LIMIT: usize = 1024 * 1024;
    let mut result = Vec::new();
    let mut buffer = [0; 8192];
    let mut truncated = false;
    loop {
        let n = stream.read(&mut buffer).map_err(|e| e.to_string())?;
        if n == 0 {
            break;
        }
        let keep = n.min(LIMIT.saturating_sub(result.len()));
        result.extend_from_slice(&buffer[..keep]);
        truncated |= keep < n;
    }
    let mut text = String::from_utf8_lossy(&result).into_owned();
    if truncated {
        text.push_str("\n[Display capped at 1 MiB; use the standalone CLI for full output.]\n");
    }
    Ok(text)
}
fn run_command(args: &[String]) -> Result<Reply, String> {
    let argv = args.to_vec();
    let mut child = Command::new(std::env::current_exe().map_err(|e| e.to_string())?)
        .args(argv)
        .env("NO_COLOR", "1")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| e.to_string())?;
    let stdout = child.stdout.take().expect("piped stdout");
    let stderr = child.stderr.take().expect("piped stderr");
    let out = std::thread::spawn(move || bounded_read(stdout));
    let err = std::thread::spawn(move || bounded_read(stderr));
    let code = child.wait().map_err(|e| e.to_string())?.code().unwrap_or(1);
    let text = out.join().map_err(|_| "stdout reader failed")??;
    let error = err.join().map_err(|_| "stderr reader failed")??;
    let text = match serde_json::from_str::<Value>(&text) {
        Ok(value) => serde_json::to_string_pretty(&value).expect("JSON serializes"),
        Err(_) => text,
    };
    Ok(Reply::Command {
        text: format!("{text}{error}"),
        code,
    })
}
fn inspect(paths: &Paths, name: &str) -> Result<Diagram, String> {
    let mut client = Client::connect(paths).map_err(|e| e.message)?;
    let response = client
        .call("ritual_read", json!({"name":name}))
        .map_err(|e| e.message)?;
    #[derive(Deserialize)]
    struct ReadRitual {
        ritual: String,
        version: u32,
        hash: String,
        definition: ritual::Ritual,
    }
    let r: ReadRitual = decode(response)?;
    let steps = plan::compile(&r.definition.steps)?;
    let mut lines = vec![
        line(Ink::Gold, format!("{} / v{}", r.ritual, r.version)),
        line(
            Ink::Dim,
            format!("hash {}", r.hash.chars().take(12).collect::<String>()),
        ),
    ];
    lines.extend(seal_lines(&r.hash));
    lines.extend([
        line(Ink::Dim, "Static plan / not live execution"),
        line(
            Ink::Bone,
            format!(
                "IN  {}",
                r.definition
                    .inputs
                    .keys()
                    .cloned()
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
        ),
    ]);
    lines.push(line(
        Ink::Ember,
        format!(
            "MODEL {}",
            r.definition.model.as_deref().unwrap_or("none declared")
        ),
    ));
    draw_steps(&steps, 0, &mut lines, r.definition.model.as_deref());
    lines.push(line(
        Ink::Bone,
        format!(
            "OUT {}",
            r.definition
                .ret
                .keys()
                .cloned()
                .collect::<Vec<_>>()
                .join(", ")
        ),
    ));
    lines.push(line(Ink::Dim, "Child calls are pinned; not expanded."));
    Ok(Diagram {
        title: format!("{} v{}", r.ritual, r.version),
        lines,
    })
}
fn draw_steps(
    steps: &[plan::Step],
    depth: usize,
    lines: &mut Vec<Line<'static>>,
    model: Option<&str>,
) {
    for step in steps {
        let prefix = format!(
            "{}{} ",
            "  ".repeat(depth),
            if theme::ascii() { "|-" } else { "├─" }
        );
        match step {
            plan::Step::Operation(s) => {
                let spec = s
                    .operation
                    .split_once('.')
                    .and_then(|(r, t)| crate::reagent::tool(r, t));
                let class = spec.map(|t| t.effect).unwrap_or("unknown");
                lines.push(line(
                    if class == "pure" {
                        Ink::Verdigris
                    } else if class == "read" {
                        Ink::Bone
                    } else {
                        Ink::Ember
                    },
                    format!("{prefix}[{class}] {}", s.operation),
                ));
                if s.verify.is_some() {
                    lines.push(line(
                        Ink::Dim,
                        format!("{}  verify read-back", "  ".repeat(depth)),
                    ));
                }
            }
            plan::Step::Model(s) => lines.push(line(
                Ink::Ember,
                format!(
                    "{prefix}[model] {}",
                    s.model
                        .name
                        .as_deref()
                        .or(model)
                        .unwrap_or("caller override required")
                ),
            )),
            plan::Step::Ritual(s) => lines.push(line(
                Ink::Gold,
                format!(
                    "{prefix}[call] {} v{}",
                    s.ritual,
                    s.version
                        .map(|v| v.to_string())
                        .unwrap_or_else(|| "?".into())
                ),
            )),
            plan::Step::Each(s) => {
                lines.push(line(
                    Ink::Gold,
                    format!("{prefix}[each] {} in {}", s.variable, s.source.join(".")),
                ));
                draw_steps(&s.steps, depth + 1, lines, model);
            }
            plan::Step::If(s) => {
                lines.push(line(
                    Ink::Gold,
                    format!("{prefix}[if] {}", s.condition_source),
                ));
                draw_steps(&s.then, depth + 1, lines, model);
                if !s.otherwise.is_empty() {
                    lines.push(line(Ink::Gold, format!("{prefix}[else]")));
                    draw_steps(&s.otherwise, depth + 1, lines, model);
                }
            }
        }
    }
}

fn evidence_summary(text: &str) -> String {
    let clean = clean(text);
    let concise = clean.split(['{', '[']).next().unwrap_or(&clean);
    let mut chars = concise.chars();
    let short: String = chars.by_ref().take(100).collect();
    if chars.next().is_some() || concise.len() != clean.len() {
        format!("{short} … (record for details)")
    } else {
        short
    }
}

fn sidebar(app: &App) -> Vec<Line<'static>> {
    if app.show_diagram {
        return app
            .diagram
            .as_ref()
            .map(|d| d.lines.clone())
            .unwrap_or_else(|| {
                vec![
                    line(Ink::Gold, "THE RITUAL PLAN"),
                    line(Ink::Dim, "Type inspect NAME to draw a saved ritual."),
                    line(
                        Ink::Dim,
                        "Pure computation and model calls have distinct labels.",
                    ),
                    line(Ink::Dim, "F2 returns to live status."),
                ]
            });
    }
    let mut lines = vec![
        line(
            if app.connected {
                Ink::Verdigris
            } else {
                Ink::Rubric
            },
            if app.connected {
                "CONNECTED / live observations"
            } else {
                "DISCONNECTED / last observations"
            },
        ),
        line(
            Ink::Dim,
            format!("pid {}   v{}", app.core.pid, app.core.version),
        ),
        line(
            Ink::Bone,
            format!(
                "{} active    {} pending",
                app.core.active, app.core.petitions
            ),
        ),
        line(Ink::Dim, format!("{} ended root runs", app.core.ended)),
        line(Ink::Dim, ""),
    ];
    if !app.error.is_empty() {
        lines.push(line(Ink::Rubric, app.error.clone()));
        lines.push(line(Ink::Dim, "Run a command to start the core again."));
    }
    if let Some(r) = &app.selected {
        lines.push(line(
            Ink::Gold,
            if app.focus.is_some() {
                "PINNED RUN / follow resumes auto"
            } else {
                "FOLLOWING / F3 pins a run"
            },
        ));
        if r.cancellation_requested {
            lines.push(line(Ink::Ember, "STOPPING / cancellation requested"));
            lines.push(line(Ink::Dim, "Waiting for in-flight work to return."));
        }
        lines.push(line(Ink::Bone, r.ritual.clone()));
        lines.push(line(Ink::Dim, r.id.clone()));
        let (word, color) = theme::state_word(&r.status, r.outcome.map(Outcome::as_str));
        lines.push(line(
            color,
            format!(
                "{word} / {}",
                r.outcome.map(Outcome::as_str).unwrap_or(&r.status)
            ),
        ));
        lines.push(line(
            Ink::Dim,
            format!("hash {}", r.hash.chars().take(12).collect::<String>()),
        ));
        lines.extend(seal_lines(&r.hash));
        lines.push(line(Ink::Dim, ""));
        lines.push(line(Ink::Gold, "MODEL & BUDGET"));
        lines.push(line(
            Ink::Bone,
            r.model
                .clone()
                .unwrap_or_else(|| "No root model; steps may name one".into()),
        ));
        lines.push(line(
            Ink::Bone,
            format!(
                "requests  {} / {}",
                r.requests,
                r.essence
                    .requests
                    .map(|v| v.to_string())
                    .unwrap_or_else(|| "unset".into())
            ),
        ));
        lines.push(line(
            Ink::Bone,
            format!(
                "tokens    {} / {}",
                r.tokens,
                r.essence
                    .tokens
                    .map(|v| v.to_string())
                    .unwrap_or_else(|| "unset".into())
            ),
        ));
        lines.push(line(Ink::Dim, "Token usage is reported, not inferred."));
        lines.push(line(
            Ink::Bone,
            format!(
                "elapsed   {}",
                util::fmt_dur(r.ended.unwrap_or_else(util::now_ms) - r.started)
            ),
        ));
        if let Some(time) = &r.essence.time {
            lines.push(line(Ink::Dim, format!("time cap  {time}")));
        }
        if r.total > 0 {
            lines.push(line(
                Ink::Bone,
                format!("items     {} / {}", r.done, r.total),
            ));
        }
        lines.push(line(Ink::Dim, ""));
        lines.push(line(Ink::Gold, "RECENT EVIDENCE"));
        for e in app
            .events
            .iter()
            .rev()
            .take(6)
            .collect::<Vec<_>>()
            .into_iter()
            .rev()
        {
            lines.push(line(Ink::Dim, evidence_summary(&e.text)));
        }
    } else {
        lines.push(line(Ink::Gold, "THE CORE KEEPS VIGIL"));
        lines.push(line(Ink::Dim, "Your runs appear here as they begin."));
        lines.push(line(Ink::Dim, ""));
        lines.push(line(Ink::Gold, "START WITH"));
        lines.push(line(Ink::Bone, "rituals"));
        lines.push(line(Ink::Bone, "inspect NAME"));
        lines.push(line(Ink::Dim, "No model is called by this panel."));
    }
    lines
}
fn draw(f: &mut Frame, app: &mut App) {
    let area = f.area();
    f.render_widget(
        Block::default().style(ink(Ink::Bone).bg(theme::background())),
        area,
    );
    if area.width < 36 || area.height < 14 {
        f.render_widget(
            Paragraph::new("SUMMONR\nResize to at least 36 x 14.\nCtrl-D leaves an empty prompt."),
            area,
        );
        return;
    }
    let rows = Layout::vertical([
        Constraint::Length(2),
        Constraint::Min(8),
        Constraint::Length(1),
    ])
    .split(area);
    let activity = if app.pending.is_empty() {
        "ready".to_string()
    } else {
        format!(
            "{} command{} pending",
            app.pending.len(),
            if app.pending.len() == 1 { "" } else { "s" }
        )
    };
    f.render_widget(
        Paragraph::new(line(
            Ink::Gold,
            format!(
                " {}  S U M M O N R  /  WORKBENCH     {activity}",
                theme::sun()
            ),
        )),
        rows[0],
    );
    let (side, main) = if app.side_visible && area.width >= 100 {
        let cols = Layout::horizontal([
            Constraint::Length(40),
            Constraint::Length(2),
            Constraint::Min(36),
        ])
        .split(rows[1]);
        (Some(cols[0]), cols[2])
    } else if app.side_visible && area.height >= 22 {
        let parts = Layout::vertical([Constraint::Length(10), Constraint::Min(10)]).split(rows[1]);
        (Some(parts[0]), parts[1])
    } else {
        (None, rows[1])
    };
    if let Some(area) = side {
        let title = if app.show_diagram {
            "RITUAL / STATIC PLAN"
        } else {
            "DAEMON / OBSERVATIONS"
        };
        let block = block(title);
        let inner = block.inner(area);
        let content = Paragraph::new(sidebar(app)).wrap(Wrap { trim: false });
        app.side_scroll = app.side_scroll.min(
            content
                .line_count(inner.width)
                .saturating_sub(inner.height as usize)
                .min(u16::MAX as usize) as u16,
        );
        f.render_widget(content.block(block).scroll((app.side_scroll, 0)), area);
    }
    let parts = Layout::vertical([Constraint::Min(4), Constraint::Length(3)]).split(main);
    let block = block("COMMANDS / SESSION");
    let inner = block.inner(parts[0]);
    let paragraph = Paragraph::new(app.transcript.iter().cloned().collect::<Vec<_>>())
        .wrap(Wrap { trim: false });
    let max = paragraph
        .line_count(inner.width)
        .saturating_sub(inner.height as usize)
        .min(u16::MAX as usize) as u16;
    app.scroll = app.scroll.min(max);
    f.render_widget(
        paragraph
            .block(block)
            .scroll((max.saturating_sub(app.scroll), 0)),
        parts[0],
    );
    let input_block = block_input();
    let input_area = input_block.inner(parts[1]);
    let capacity = input_area.width.saturating_sub(3).max(1) as usize;
    let mut start = 0;
    while Span::raw(&app.input[start..app.cursor]).width() >= capacity {
        let Some(c) = app.input[start..app.cursor].chars().next() else {
            break;
        };
        start += c.len_utf8();
    }
    let display = clean(&app.input[start..]).replace('\n', " ");
    f.render_widget(
        Paragraph::new(Line::from(vec![
            Span::styled(" > ", ink(Ink::Gold)),
            Span::raw(display),
        ]))
        .block(input_block),
        parts[1],
    );
    let col = Span::raw(clean(&app.input[start..app.cursor]).replace('\n', " ")).width();
    f.set_cursor_position((input_area.x + 3 + col as u16, input_area.y));
    f.render_widget(Paragraph::new(line(Ink::Dim," F1 help   F2 status/plan   F3 next run   F4 sidebar   PgUp/Dn scroll   exit leaves work running")).style(Style::default().bg(theme::surface())),rows[2]);
}
fn block_input() -> Block<'static> {
    block("ENTER COMMAND")
        .border_style(ink(Ink::Gold))
        .title_style(ink(Ink::Gold).add_modifier(Modifier::BOLD))
}

pub fn run(paths: &Paths) -> Result<(), RpcErr> {
    if !std::io::stdin().is_terminal() || !std::io::stdout().is_terminal() {
        return Err(RpcErr::invalid(
            "the workbench needs a terminal; use individual commands with --json for scripts",
        ));
    }
    // Only initial opening starts a daemon. Polling must respect an explicit core stop.
    Client::connect(paths)?;
    let mut app = App::new();
    let mut terminal = ratatui::init();
    if let Err(e) = execute!(std::io::stdout(), EnableBracketedPaste) {
        ratatui::restore();
        return Err(RpcErr::new("terminal", 1, e.to_string()));
    }
    let result = (|| -> Result<(), RpcErr> {
        loop {
            app.collect();
            app.start_poll(paths);
            terminal
                .draw(|f| draw(f, &mut app))
                .map_err(|e| RpcErr::new("terminal", 1, e.to_string()))?;
            if event::poll(Duration::from_millis(100))
                .map_err(|e| RpcErr::new("terminal", 1, e.to_string()))?
            {
                match event::read().map_err(|e| RpcErr::new("terminal", 1, e.to_string()))? {
                    Event::Key(k) if k.kind == KeyEventKind::Press => {
                        if app.key(k.code, k.modifiers, paths) {
                            return Ok(());
                        }
                    }
                    Event::Paste(text) => {
                        let text = clean(&text);
                        if app.input.len() + text.len() <= 32768 {
                            app.input.insert_str(app.cursor, &text);
                            app.cursor += text.len();
                        } else {
                            app.append(Ink::Rubric, "Paste exceeds the 32 KiB input limit.");
                        }
                    }
                    _ => {}
                }
            }
        }
    })();
    let _ = execute!(std::io::stdout(), DisableBracketedPaste);
    ratatui::restore();
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn quoted_arguments_are_literal_not_shell_code() {
        assert_eq!(
            words("run r --arg 'text=hello world' --arg 'json={\"a\":1}'").unwrap(),
            vec![
                "run",
                "r",
                "--arg",
                "text=hello world",
                "--arg",
                "json={\"a\":1}"
            ]
        );
        assert_eq!(
            words("run r --arg 'text=$(touch nope)' --arg text=\"\"").unwrap()[3],
            "text=$(touch nope)"
        );
        assert_eq!(words("run r --arg text=\"\"").unwrap()[3], "text=");
        assert!(words("run r --arg 'unfinished").is_err());
    }
    #[test]
    fn editor_handles_unicode_and_history_without_splitting_characters() {
        let paths = Paths::resolve();
        let mut a = App::new();
        a.key(KeyCode::Char('é'), KeyModifiers::NONE, &paths);
        a.key(KeyCode::Char('界'), KeyModifiers::NONE, &paths);
        a.key(KeyCode::Left, KeyModifiers::NONE, &paths);
        a.key(KeyCode::Backspace, KeyModifiers::NONE, &paths);
        assert_eq!(a.input, "界");
        a.key(KeyCode::Delete, KeyModifiers::NONE, &paths);
        assert!(a.input.is_empty());
        a.history.push_back("rituals".into());
        a.input = "draft".into();
        a.history(true);
        assert_eq!(a.input, "rituals");
        a.history(false);
        assert_eq!(a.input, "draft");
    }
    #[test]
    fn diagrams_distinguish_code_models_and_control_flow() {
        let raw:ritual::Ritual=serde_json::from_value(json!({"ritual":"r","steps":[{"id":"code","operation":"data.count","arguments":{"value":[1]}},{"if":"true","then":[{"id":"m","model":{"prompt":"x"}}],"else":[{"id":"child","ritual":"other","version":2}]}]})).unwrap();
        let mut lines = Vec::new();
        draw_steps(
            &plan::compile(&raw.steps).unwrap(),
            0,
            &mut lines,
            Some("test/model"),
        );
        let text = lines
            .iter()
            .map(|l| l.to_string())
            .collect::<Vec<_>>()
            .join("\n");
        for label in [
            "[pure]",
            "[model] test/model",
            "[if] true",
            "[else]",
            "[call]",
        ] {
            assert!(text.contains(label), "{text}");
        }
    }
    #[test]
    fn terminal_sizes_and_long_output_render_without_panics() {
        for (w, h) in [(1, 1), (36, 14), (60, 26), (100, 30), (180, 50)] {
            let mut a = App::new();
            a.input = "界".repeat(100);
            a.cursor = a.input.len();
            a.append(Ink::Bone, &"long output ".repeat(1000));
            a.scroll = u16::MAX;
            a.show_diagram = true;
            let mut t = ratatui::Terminal::new(ratatui::backend::TestBackend::new(w, h)).unwrap();
            t.draw(|f| draw(f, &mut a)).unwrap();
        }
    }
    #[test]
    fn cancellation_has_a_reserved_slot_and_bad_selection_is_visible() {
        let mut app = App::new();
        for id in 0..8 {
            let (_, rx) = mpsc::channel();
            app.pending.push(Pending {
                id,
                rx,
                control: false,
            });
        }
        assert!(!app.slot_available(false));
        assert!(app.slot_available(true));
        let (_, rx) = mpsc::channel();
        app.pending.push(Pending {
            id: 9,
            rx,
            control: true,
        });
        assert!(!app.slot_available(true));
        app.focus = Some("missing".into());
        app.poll_focus = app.focus.clone();
        let (tx, rx) = mpsc::channel();
        app.poll = Some(rx);
        tx.send(Ok(Snapshot {
            core: CoreStatus::default(),
            runs: vec![],
            selected: None,
            events: vec![],
            selection_error: Some("no summon missing".into()),
        }))
        .unwrap();
        app.collect();
        assert!(app.connected);
        assert!(app.focus.is_none());
        assert!(app
            .transcript
            .iter()
            .any(|l| l.to_string().contains("no summon missing")));
    }
}
