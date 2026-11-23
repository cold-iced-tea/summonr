//! The observer: every summon in one place. It only watches and asks; all
//! state lives in the core, and closing the observer never stops work.

use crate::rpc::{Client, RpcErr};
use crate::theme::{self, Ink};
use crate::util::{fmt_ago, fmt_dur, fmt_offset, now_ms, parse_duration, thousands, Paths};
use ratatui::crossterm::event::{self, Event, KeyCode, KeyEventKind};
use ratatui::layout::{Alignment, Constraint, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Clear, Paragraph, Wrap};
use ratatui::Frame;
use serde_json::{json, Value};
use std::io::IsTerminal;
use std::time::{Duration, Instant};

#[derive(Default)]
struct App {
    active: Vec<Value>,
    ended: Vec<Value>,
    sel: usize,
    focus: Option<String>,
    detail: Option<Value>,
    events: Vec<Value>,
    events_for: String,
    paths: Option<Paths>,
    event_sub: Option<std::sync::mpsc::Receiver<Value>>,
    tick: u64,
    notice: String,
    confirm_banish: Option<String>,
    view: usize,
    scroll: u16,
    zoom: bool,
    vigil: bool,
    help: bool,
    transition: Option<Instant>,
    core: Value,
}

fn ink(i: Ink) -> Style {
    Style::default().fg(i.color())
}

fn span(i: Ink, s: impl Into<String>) -> Span<'static> {
    Span::styled(s.into(), ink(i))
}

fn title(s: &str) -> Line<'static> {
    Line::from(Span::styled(
        format!(" {s} "),
        ink(Ink::Gold).add_modifier(Modifier::BOLD),
    ))
}

fn panel(name: &str) -> Block<'static> {
    Block::bordered()
        .title(title(name))
        .border_type(if theme::ascii() {
            BorderType::Plain
        } else {
            BorderType::Rounded
        })
        .border_style(ink(Ink::Rule))
}

fn s<'a>(v: &'a Value, k: &str) -> &'a str {
    v[k].as_str().unwrap_or("")
}

fn elapsed(v: &Value) -> i64 {
    v["ended"].as_i64().unwrap_or_else(now_ms) - v["started"].as_i64().unwrap_or_else(now_ms)
}

impl App {
    fn rows(&self) -> Vec<&Value> {
        self.active.iter().chain(self.ended.iter()).collect()
    }

    fn selected_id(&self) -> Option<String> {
        self.rows().get(self.sel).map(|v| s(v, "id").to_string())
    }

    fn refresh(&mut self, client: &mut Client) -> Result<(), RpcErr> {
        let keep = self.focus.take().or_else(|| self.selected_id());
        self.core = client.call("ping", json!({}))?;
        self.active = client
            .call("list", json!({"scope": "active"}))?
            .as_array()
            .cloned()
            .unwrap_or_default();
        self.ended = client
            .call("list", json!({"scope": "ended", "limit": 60}))?
            .as_array()
            .cloned()
            .unwrap_or_default();
        if let Some(id) = &keep {
            if !self.rows().iter().any(|v| s(v, "id") == id) {
                let record = client.call("get", json!({"summon":id}))?;
                if record["status"] == "ended" {
                    self.ended.push(record);
                } else {
                    self.active.push(record);
                }
            }
        }
        let rows = self.rows();
        // Keep the same summon selected as rows move between the circle and
        // the graveyard.
        self.sel = keep
            .and_then(|id| rows.iter().position(|v| s(v, "id") == id))
            .unwrap_or_else(|| self.sel.min(rows.len().saturating_sub(1)));
        match self.selected_id() {
            Some(id) => {
                if id != self.events_for {
                    self.events.clear();
                    self.scroll = 0;
                    self.events_for = id.clone();
                    self.event_sub = self
                        .paths
                        .as_ref()
                        .and_then(|p| Client::subscribe(p, Some(&id), 0, true).ok());
                }
                self.detail = Some(client.call("get", json!({"summon": id}))?);
                let mut subscribed_ok = false;
                if let Some(rx) = &self.event_sub {
                    loop {
                        match rx.try_recv() {
                            Ok(ev) => {
                                let seq = ev["seq"].as_i64().unwrap_or(0);
                                let prev = self.events.last().and_then(|e| e["seq"].as_i64()).unwrap_or(0);
                                if seq > prev {
                                    self.events.push(ev);
                                }
                            }
                            Err(std::sync::mpsc::TryRecvError::Empty) => {
                                subscribed_ok = true;
                                break;
                            }
                            Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                                self.event_sub = None;
                                break;
                            }
                        }
                    }
                }
                if !subscribed_ok {
                    let after = self
                        .events
                        .last()
                        .and_then(|e| e["seq"].as_i64())
                        .unwrap_or(0);
                    if let Some(new) = client
                        .call(
                            "events",
                            json!({"summon": id, "after": after, "slim": true}),
                        )?
                        .as_array()
                    {
                        self.events.extend(new.iter().cloned());
                    }
                }
            }
            None => {
                self.detail = None;
                self.events.clear();
                self.events_for.clear();
                self.event_sub = None;
            }
        }
        Ok(())
    }

    fn banish(&mut self, client: &mut Client) {
        let Some(id) = self.selected_id() else { return };
        if self.confirm_banish.as_deref() != Some(id.as_str()) {
            self.confirm_banish = Some(id.clone());
            self.notice = format!("Banish {id}? Press b again to confirm; any other key keeps it.");
            return;
        }
        self.confirm_banish = None;
        self.notice = match client.call("cancel", json!({"summon": id})) {
            Ok(_) => format!("Banishing {id}. Commands stop at once; a model already answering may keep generating."),
            Err(e) => e.message,
        };
    }
}

fn row_lines(v: &Value, selected: bool, tick: u64, width: usize) -> Vec<Line<'static>> {
    let (word, word_ink) = theme::state_word(s(v, "status"), v["outcome"].as_str());
    let running = v["status"] != "ended";
    let mark = if selected {
        if theme::ascii() {
            ">"
        } else {
            "▌"
        }
    } else {
        " "
    };
    let child = if v["parent"].is_null() {
        ""
    } else if theme::ascii() {
        "\\ "
    } else {
        "↳ "
    };
    let name = format!("{child}{} v{}", s(v, "ritual"), v["version"]);
    let tail = if running {
        format!("{word} {}", theme::phase(tick))
    } else {
        word.to_string()
    };
    let used = 5 + name.chars().count() + tail.chars().count();
    let pad = " ".repeat(width.saturating_sub(used).max(1));
    let name_style = if selected {
        ink(Ink::Bone)
            .bg(theme::surface())
            .add_modifier(Modifier::BOLD)
    } else {
        ink(Ink::Bone)
    };
    let progress = match (
        v["done"].as_i64().unwrap_or(0),
        v["total"].as_i64().unwrap_or(0),
    ) {
        (_, 0) => String::new(),
        (d, t) => format!(" · {d}/{t}"),
    };
    let when = if running {
        fmt_dur(elapsed(v))
    } else {
        fmt_ago(v["ended"].as_i64().unwrap_or(0))
    };
    vec![
        Line::from(vec![
            span(Ink::Rubric, mark),
            span(Ink::Gold, format!("{} ", theme::sigil(s(v, "id")))),
            Span::styled(name, name_style),
            Span::raw(pad),
            span(word_ink, tail),
        ]),
        Line::from(span(
            Ink::Dim,
            format!("     {}{progress} · {when}", s(v, "id")),
        )),
    ]
}

/// The rows that fit, scrolled so the selection stays visible.
fn window(
    lines: Vec<Line<'static>>,
    selected_row: Option<usize>,
    height: usize,
) -> Vec<Line<'static>> {
    let per = 2;
    let fit = (height / per).max(1);
    let first = match selected_row {
        Some(r) if r >= fit => r + 1 - fit,
        _ => 0,
    };
    lines
        .into_iter()
        .skip(first * per)
        .take(fit * per)
        .collect()
}

fn draw(f: &mut Frame, app: &mut App) {
    let area = f.area();
    f.render_widget(
        Block::default().style(ink(Ink::Bone).bg(theme::background())),
        area,
    );
    if area.width < 36 || area.height < 12 {
        f.render_widget(
            Paragraph::new(
                "SUMMONR
Terminal too small.
Resize to at least 36 x 12.
q leaves; work continues.",
            ),
            area,
        );
        return;
    }
    let area = if area.width >= 80 && area.height >= 24 {
        Rect::new(area.x + 2, area.y + 1, area.width - 4, area.height - 2)
    } else {
        area
    };
    let rows = Layout::vertical([
        Constraint::Length(3),
        Constraint::Min(6),
        Constraint::Length(1),
    ])
    .split(area);
    header(f, app, rows[0]);
    if app.rows().is_empty() {
        welcome(f, app, rows[1]);
    } else if app.zoom || area.width < 96 {
        detail(f, app, rows[1]);
    } else {
        let cols = Layout::horizontal([
            Constraint::Length(40),
            Constraint::Length(2),
            Constraint::Min(30),
        ])
        .split(rows[1]);
        lists(f, app, cols[0]);
        detail(f, app, cols[2]);
    }
    let keys = if app.notice.is_empty() {
        if app.rows().is_empty() {
            "d  daemon     ?  key guide     q  leave".to_string()
        } else if area.width < 80 {
            "j/k select   Tab view   d daemon   ? keys   q leave".to_string()
        } else {
            "j/k select   1-4 views   PgUp/Dn scroll   z expand   d daemon   ? keys   q leave"
                .to_string()
        }
    } else {
        app.notice.clone()
    };
    f.render_widget(
        Paragraph::new(Line::from(span(Ink::Dim, format!(" {keys}"))))
            .style(Style::default().bg(theme::surface())),
        rows[2],
    );
    if app.vigil {
        vigil(f, app);
    }
    if app.help {
        help(f);
    }
}

fn header(f: &mut Frame, app: &App, area: Rect) {
    let regions = Layout::horizontal([
        Constraint::Min(20),
        Constraint::Length(if area.width >= 80 { 43 } else { 0 }),
    ])
    .split(area);
    f.render_widget(
        Paragraph::new(Line::from(vec![
            span(Ink::Gold, format!(" {}  S U M M O N R", theme::sun())),
            span(Ink::Dim, "   /   SCRY"),
        ])),
        regions[0],
    );
    f.render_widget(
        Paragraph::new(Line::from(vec![
            span(Ink::Verdigris, "CONNECTED"),
            span(
                Ink::Dim,
                format!(
                    "   {} active   {} pending  ",
                    app.core["active"], app.core["petitions"]
                ),
            ),
        ]))
        .alignment(Alignment::Right),
        regions[1],
    );
    let rule = if theme::ascii() { "-" } else { "─" }.repeat(area.width as usize);
    f.render_widget(
        Paragraph::new(Line::from(span(Ink::Rule, rule))),
        Rect::new(area.x, area.y + 1, area.width, 1),
    );
}

fn centered_line(f: &mut Frame, area: Rect, y: u16, text: &str, color: Ink) {
    if y < area.height {
        f.render_widget(
            Paragraph::new(Line::from(span(color, text.to_string()))).alignment(Alignment::Center),
            Rect::new(area.x, area.y + y, area.width, 1),
        );
    }
}

/// An idle instrument should have a composed resting state, not empty tables.
fn welcome(f: &mut Frame, app: &App, area: Rect) {
    let height = 25.min(area.height);
    let page = Rect::new(
        area.x,
        area.y + area.height.saturating_sub(height) / 2,
        area.width,
        height,
    );
    let mut y = 0;
    if area.height >= 23 && !theme::plain() {
        let seal = if theme::ascii() {
            [
                "           |           ",
                "      .----+----.      ",
                "    .'     |     '.    ",
                "---(-------o-------)---",
                "    '.     |     .'    ",
                "      '----+----'      ",
                "           |           ",
            ]
        } else {
            [
                "           │           ",
                "      ╭────┼────╮      ",
                "    ╭─╯    │    ╰─╮    ",
                "────┼──────☉──────┼────",
                "    ╰─╮    │    ╭─╯    ",
                "      ╰────┼────╯      ",
                "           │           ",
            ]
        };
        for line in seal {
            centered_line(f, page, y, line, Ink::Gold);
            y += 1;
        }
        y += 1;
    }
    centered_line(
        f,
        page,
        y,
        if page.width < 44 {
            "THE QUIET INSTRUMENT"
        } else {
            "T H E   Q U I E T   I N S T R U M E N T"
        },
        Ink::Bone,
    );
    y += 2;
    centered_line(
        f,
        page,
        y,
        if page.width < 44 {
            "Deliberate inference."
        } else {
            "Deterministic work. Deliberate inference."
        },
        Ink::Verdigris,
    );
    y += 2;
    centered_line(
        f,
        page,
        y,
        if page.width < 44 {
            "No summon is selected."
        } else {
            "No summon is selected. The core is ready."
        },
        Ink::Dim,
    );
    y += 1;
    centered_line(f, page, y, "Runs will appear here as they begin.", Ink::Dim);
    y += 3;
    centered_line(f, page, y, "IN ANOTHER TERMINAL", Ink::Gold);
    y += 2;
    centered_line(f, page, y, "summonr rituals", Ink::Bone);
    y += 1;
    centered_line(f, page, y, "Browse your saved capabilities", Ink::Dim);
    if page.width >= 60 {
        y += 2;
        centered_line(
            f,
            page,
            y,
            "summonr run <ritual> --arg name=value --wait",
            Ink::Bone,
        );
    }
    if area.height >= 32 {
        centered_line(
            f,
            area,
            area.height - 3,
            &format!(
                "CORE {}    /    {} ENDED    /    {} PENDING",
                app.core["pid"], app.core["ended"], app.core["petitions"]
            ),
            Ink::Dim,
        );
    }
}

fn overlay(f: &mut Frame, name: &str, lines: Vec<Line<'static>>, width: u16, height: u16) {
    let screen = f.area();
    let width = width.min(screen.width.saturating_sub(2));
    let height = height.min(screen.height.saturating_sub(2));
    let area = Rect::new(
        screen.x + (screen.width - width) / 2,
        screen.y + (screen.height - height) / 2,
        width,
        height,
    );
    if !theme::plain() {
        for cell in &mut f.buffer_mut().content {
            cell.set_fg(Ink::Rule.color());
        }
    }
    f.render_widget(Clear, area);
    f.render_widget(
        Paragraph::new(lines)
            .wrap(Wrap { trim: false })
            .style(ink(Ink::Bone).bg(theme::surface()))
            .block(panel(name).border_style(ink(Ink::Gold))),
        area,
    );
}

fn vigil(f: &mut Frame, app: &App) {
    if f.area().width < 60 || f.area().height < 20 {
        overlay(
            f,
            "DAEMON",
            vec![
                Line::from(span(Ink::Verdigris, " Connected to the core")),
                kv(
                    "process",
                    vec![span(Ink::Bone, app.core["pid"].to_string())],
                ),
                kv(
                    "active",
                    vec![span(Ink::Bone, app.core["active"].to_string())],
                ),
                kv(
                    "ended",
                    vec![span(Ink::Bone, app.core["ended"].to_string())],
                ),
                kv(
                    "pending",
                    vec![span(Ink::Gold, app.core["petitions"].to_string())],
                ),
                Line::from(""),
                Line::from(span(Ink::Dim, " d / Esc back    q leave")),
            ],
            40,
            10,
        );
        return;
    }
    let center = |text: String, color| Line::from(span(color, text)).alignment(Alignment::Center);
    overlay(
        f,
        "THE VIGIL / DAEMON",
        vec![
            Line::from(""),
            center(
                format!("{}   THE CORE KEEPS VIGIL   {}", theme::sun(), theme::sun()),
                Ink::Gold,
            ),
            center("Connected to the local runtime".into(), Ink::Verdigris),
            Line::from(""),
            center(
                format!(
                    "{:^16}{:^16}{:^16}",
                    app.core["active"].to_string(),
                    app.core["ended"].to_string(),
                    app.core["petitions"].to_string()
                ),
                Ink::Bone,
            ),
            center(
                format!("{:^16}{:^16}{:^16}", "ACTIVE", "ENDED", "PENDING"),
                Ink::Dim,
            ),
            Line::from(""),
            center(
                if theme::ascii() {
                    "-".repeat(48)
                } else {
                    "─".repeat(48)
                },
                Ink::Rule,
            ),
            Line::from(""),
            kv(
                "process",
                vec![span(
                    Ink::Bone,
                    format!(
                        "{}   /   version {}",
                        app.core["pid"],
                        s(&app.core, "version")
                    ),
                )],
            ),
            kv(
                "socket",
                vec![span(Ink::Dim, s(&app.core, "socket").to_string())],
            ),
            Line::from(""),
            center("Closing this window leaves work running.".into(), Ink::Dim),
            Line::from(""),
            center("d / Esc  return to scry       q  leave".into(), Ink::Gold),
        ],
        66,
        19,
    );
}

fn help(f: &mut Frame) {
    overlay(
        f,
        "READING THE INSTRUMENT",
        vec![
            Line::from(""),
            Line::from("  j/k or arrows     select a summon"),
            Line::from("  g/G or Home/End   first / last summon"),
            Line::from("  1 / 2 / 3 / 4     ledger / result / authority / record"),
            Line::from("  Tab / r           next evidence view"),
            Line::from("  PgUp / PgDn       scroll evidence"),
            Line::from("  f                 reset scroll / follow ledger"),
            Line::from("  z                 expand the selected summon"),
            Line::from("  d                 open the daemon panel"),
            Line::from("  b, then b         request cancellation"),
            Line::from("  ? / Esc           close this guide"),
            Line::from("  q                 leave; work continues"),
            Line::from(""),
            Line::from(span(
                Ink::Gold,
                "  Deterministic execution. Deliberate inference.",
            )),
        ],
        66,
        18,
    );
}

fn lists(f: &mut Frame, app: &App, area: Rect) {
    let want = (app.active.len() as u16 * 2 + 2).max(4);
    let parts = Layout::vertical([
        Constraint::Length(want.min(area.height / 2)),
        Constraint::Min(4),
    ])
    .split(area);
    let width = area.width.saturating_sub(2) as usize;

    let mut lines = Vec::new();
    for (i, v) in app.active.iter().enumerate() {
        lines.extend(row_lines(v, i == app.sel, app.tick, width));
    }
    if lines.is_empty() {
        lines.push(Line::from(span(
            Ink::Dim,
            " The circle is empty. Nothing labours.",
        )));
    }
    let sel = (app.sel < app.active.len()).then_some(app.sel);
    let block = panel(&format!("CIRCLE / {} ACTIVE", app.active.len()));
    let inner = block.inner(parts[0]).height as usize;
    f.render_widget(
        Paragraph::new(window(lines, sel, inner)).block(block),
        parts[0],
    );

    let mut lines = Vec::new();
    for (i, v) in app.ended.iter().enumerate() {
        lines.extend(row_lines(
            v,
            i + app.active.len() == app.sel,
            app.tick,
            width,
        ));
    }
    if lines.is_empty() {
        lines.push(Line::from(span(Ink::Dim, " No summon has ended yet.")));
    }
    let sel = app.sel.checked_sub(app.active.len());
    let block = panel(&format!("GRAVEYARD / {} RECENT", app.ended.len()));
    let inner = block.inner(parts[1]).height as usize;
    f.render_widget(
        Paragraph::new(window(lines, sel, inner)).block(block),
        parts[1],
    );
}

fn kv(key: &str, value: Vec<Span<'static>>) -> Line<'static> {
    let mut l = vec![span(Ink::Dim, format!(" {key:<9}"))];
    l.extend(value);
    Line::from(l)
}

fn essence_line(
    label: &str,
    used: u64,
    max: Option<u64>,
    show: impl Fn(u64) -> String,
) -> Line<'static> {
    match max {
        Some(m) => {
            let (on, off) = theme::gauge(used, m, 16);
            let hot = used as u128 * 10 >= m as u128 * 9;
            kv(
                label,
                vec![
                    span(if hot { Ink::Rubric } else { Ink::Ember }, on),
                    span(Ink::Dim, off),
                    span(Ink::Bone, format!("  {} / {}", show(used), show(m))),
                ],
            )
        }
        None => kv(
            label,
            vec![
                span(Ink::Bone, show(used)),
                span(Ink::Dim, "  no ceiling declared"),
            ],
        ),
    }
}

fn detail(f: &mut Frame, app: &mut App, area: Rect) {
    let Some(d) = &app.detail else {
        let text = vec![
            Line::from(""),
            Line::from(span(Ink::Dim, "  No summon is selected.")),
            Line::from(span(
                Ink::Dim,
                "  Invoke a ritual from any terminal and it will appear here:",
            )),
            Line::from(""),
            Line::from(span(
                Ink::Bone,
                "    summonr invoke <ritual> given name=value",
            )),
        ];
        f.render_widget(Paragraph::new(text).block(panel("ELEGY")), area);
        return;
    };
    let area = if area.height >= 18 {
        let tabs = ["1 Ledger", "2 Result", "3 Authority", "4 Record"];
        let spans: Vec<Span> = tabs
            .iter()
            .enumerate()
            .flat_map(|(i, label)| {
                [
                    Span::styled(
                        format!(" {label} "),
                        if app.view == i {
                            ink(Ink::Bone)
                                .bg(theme::surface())
                                .add_modifier(Modifier::BOLD)
                        } else {
                            ink(Ink::Dim)
                        },
                    ),
                    Span::raw(" "),
                ]
            })
            .collect();
        f.render_widget(
            Paragraph::new(Line::from(spans)),
            Rect::new(area.x, area.y, area.width, 1),
        );
        Rect::new(area.x, area.y + 2, area.width, area.height - 2)
    } else {
        area
    };
    let parts = Layout::vertical([
        Constraint::Length(if area.height >= 30 {
            10
        } else if area.height >= 22 {
            7
        } else {
            4
        }),
        Constraint::Length(if area.height >= 22 { 5 } else { 0 }),
        Constraint::Min(4),
    ])
    .split(area);
    let (word, word_ink) = theme::state_word(s(d, "status"), d["outcome"].as_str());
    let literal = d["outcome"].as_str().unwrap_or(s(d, "status"));
    let hash = s(d, "hash");
    let incipit = vec![
        kv(
            "ritual",
            vec![
                span(
                    Ink::Bone,
                    format!("{} · version {}", s(d, "ritual"), d["version"]),
                ),
                span(Ink::Dim, format!(" · {}", &hash[..hash.len().min(12)])),
            ],
        ),
        kv(
            "model",
            vec![span(
                Ink::Bone,
                d["model"].as_str().unwrap_or("none of its own").to_string(),
            )],
        ),
        kv(
            "state",
            vec![
                span(word_ink, word),
                span(Ink::Dim, format!("  ({literal})")),
            ],
        ),
        kv(
            "lineage",
            vec![span(
                Ink::Bone,
                match d["parent"].as_str() {
                    Some(p) => format!("called by {p}"),
                    None => match d["series"].as_str() {
                        Some(sr) => format!("trial in {sr}"),
                        None => match d["familiar"].as_str().and_then(|p| p.strip_prefix("familiar:")) {
                            Some(who) => format!("invoked by familiar {who}"),
                            None => "invoked directly".to_string(),
                        },
                    },
                },
            )],
        ),
        kv(
            "inputs",
            vec![span(Ink::Bone, {
                let t = d["inputs"].to_string();
                if t.chars().count() > 70 {
                    format!("{}…", t.chars().take(69).collect::<String>())
                } else {
                    t
                }
            })],
        ),
    ];
    let mut incipit = incipit;
    if area.height >= 30 {
        // The ritual's seal, beside the hash it is drawn from.
        for row in theme::seal(hash).into_iter().flatten() {
            incipit.push(Line::from(vec![span(Ink::Dim, format!("{:>10} ", "")), span(Ink::Gold, row)]));
        }
    }
    f.render_widget(
        Paragraph::new(incipit).block(panel(&format!(
            "INCIPIT · {} {}",
            theme::sigil(s(d, "id")),
            s(d, "id")
        ))),
        parts[0],
    );

    let e = &d["essence"];
    let time_max = e["time"].as_str().and_then(|t| parse_duration(t).ok());
    let essence = vec![
        essence_line(
            "requests",
            d["requests"].as_u64().unwrap_or(0),
            e["requests"].as_u64(),
            thousands,
        ),
        essence_line(
            "tokens",
            d["tokens"].as_u64().unwrap_or(0),
            e["tokens"].as_u64(),
            thousands,
        ),
        essence_line("time", elapsed(d).max(0) as u64, time_max, |ms| {
            fmt_dur(ms as i64)
        }),
    ];
    f.render_widget(Paragraph::new(essence).block(panel("ESSENCE")), parts[1]);

    let started = d["started"].as_i64().unwrap_or(0);
    let labels = ["1 LEDGER", "2 RESULT", "3 AUTHORITY", "4 RECORD"];
    let mut block = panel(&format!(
        "{}  /  PgUp PgDn  /  {}",
        labels[app.view],
        if app.scroll == 0 && app.view == 0 {
            "LIVE".to_string()
        } else {
            format!("offset {}", app.scroll)
        }
    ));
    if !theme::reduced_motion()
        && app
            .transition
            .is_some_and(|t| t.elapsed() < Duration::from_millis(360))
    {
        block = block.border_style(ink(Ink::Bone));
    }
    let inner = block.inner(parts[2]);
    let lines: Vec<Line> = if app.view != 0 {
        let mut out = Vec::new();
        if let Some(err) = d["error"].as_str() {
            out.push(Line::from(span(Ink::Rubric, format!(" {err}"))));
        }
        let value = match app.view {
            1 => d["result"].clone(),
            2 => {
                json!({"petitions":d["petitions"], "effects":d["effects"], "cleanup":d["cleanup"]})
            }
            _ => d.clone(),
        };
        let pretty = serde_json::to_string_pretty(&value).unwrap_or_default();
        out.extend(
            pretty
                .lines()
                .map(|l| Line::from(span(Ink::Bone, format!(" {l}")))),
        );
        out
    } else {
        let mut out: Vec<Line> = app
            .events
            .iter()
            .map(|ev| {
                let (mark, mark_ink) = theme::event_mark(s(ev, "kind"));
                Line::from(vec![
                    span(
                        Ink::Dim,
                        format!(
                            " {}  ",
                            fmt_offset(ev["ts"].as_i64().unwrap_or(0) - started)
                        ),
                    ),
                    span(mark_ink, format!("{mark} ")),
                    span(
                        if s(ev, "kind") == "summon.end" {
                            word_ink
                        } else {
                            Ink::Bone
                        },
                        s(ev, "text").to_string(),
                    ),
                ])
            })
            .collect();
        if d["status"] == "ended" {
            out.push(Line::from(vec![
                span(Ink::Rubric, format!(" {} COLOPHON  ", theme::ornament())),
                span(word_ink, literal.to_string()),
                span(
                    Ink::Dim,
                    format!(
                        " · {} · {} requests · {} tokens",
                        fmt_dur(elapsed(d)),
                        d["requests"],
                        thousands(d["tokens"].as_u64().unwrap_or(0))
                    ),
                ),
            ]));
        }
        out
    };
    let paragraph = Paragraph::new(lines).wrap(Wrap { trim: false });
    let maximum = paragraph
        .line_count(inner.width)
        .saturating_sub(inner.height as usize)
        .min(u16::MAX as usize) as u16;
    app.scroll = app.scroll.min(maximum);
    let offset = if app.view == 0 {
        maximum.saturating_sub(app.scroll)
    } else {
        app.scroll
    };
    f.render_widget(paragraph.block(block).scroll((offset, 0)), parts[2]);
}

pub fn run(paths: &Paths, focus: Option<String>) -> Result<(), RpcErr> {
    open(paths, focus, false)
}

pub fn daemon_panel(paths: &Paths) -> Result<(), RpcErr> {
    open(paths, None, true)
}

fn open(paths: &Paths, focus: Option<String>, vigil: bool) -> Result<(), RpcErr> {
    if !std::io::stdin().is_terminal() || !std::io::stdout().is_terminal() {
        return Err(RpcErr::invalid("the observer needs a terminal; use `core status --json` or `record ID --json` for scripts"));
    }
    let mut client = Client::connect(paths)?;
    let mut app = App {
        focus,
        vigil,
        paths: Some(paths.clone()),
        ..App::default()
    };
    app.refresh(&mut client)?;
    let mut terminal = ratatui::init();
    let mut last = Instant::now();
    let result = loop {
        if let Err(e) = terminal.draw(|f| draw(f, &mut app)) {
            break Err(RpcErr::new("error", 1, e.to_string()));
        }
        match event::poll(Duration::from_millis(120)) {
            Err(e) => break Err(RpcErr::new("error", 1, e.to_string())),
            Ok(false) => {}
            Ok(true) => match event::read() {
                Err(e) => break Err(RpcErr::new("error", 1, e.to_string())),
                Ok(Event::Key(k)) if k.kind == KeyEventKind::Press => {
                    if handle_key(&mut app, k.code) {
                        break Ok(());
                    }
                    if k.code == KeyCode::Char('b') && !app.vigil && !app.help {
                        app.banish(&mut client);
                    }
                    if let Err(e) = app.refresh(&mut client) {
                        break Err(e);
                    }
                    last = Instant::now();
                }
                _ => {}
            },
        }
        if last.elapsed() >= Duration::from_millis(500) {
            if !theme::reduced_motion() {
                app.tick = app.tick.wrapping_add(1);
            }
            if let Err(e) = app.refresh(&mut client) {
                break Err(e);
            }
            last = Instant::now();
        }
    };
    ratatui::restore();
    result
}

/// Returns true only when the observer should close. Overlays intercept keys.
fn handle_key(app: &mut App, code: KeyCode) -> bool {
    if code != KeyCode::Char('b') {
        app.confirm_banish = None;
        app.notice.clear();
    }
    if code == KeyCode::Char('q') {
        return true;
    }
    if app.help {
        if matches!(code, KeyCode::Esc | KeyCode::Char('?')) {
            app.help = false;
        }
        return false;
    }
    if app.vigil {
        if matches!(code, KeyCode::Esc | KeyCode::Char('d')) {
            app.vigil = false;
        }
        return false;
    }
    let rows = app.rows().len();
    let before = (app.view, app.sel, app.zoom);
    match code {
        KeyCode::Esc => return true,
        KeyCode::Down | KeyCode::Char('j') if app.sel + 1 < rows => app.sel += 1,
        KeyCode::Up | KeyCode::Char('k') => app.sel = app.sel.saturating_sub(1),
        KeyCode::Home | KeyCode::Char('g') => app.sel = 0,
        KeyCode::End | KeyCode::Char('G') => app.sel = rows.saturating_sub(1),
        KeyCode::Tab | KeyCode::Char('r') => {
            app.view = (app.view + 1) % 4;
            app.scroll = 0;
        }
        KeyCode::Char(c @ '1'..='4') => {
            app.view = c as usize - '1' as usize;
            app.scroll = 0;
        }
        KeyCode::PageUp if app.view == 0 => app.scroll = app.scroll.saturating_add(8),
        KeyCode::PageDown if app.view == 0 => app.scroll = app.scroll.saturating_sub(8),
        KeyCode::PageUp => app.scroll = app.scroll.saturating_sub(8),
        KeyCode::PageDown => app.scroll = app.scroll.saturating_add(8),
        KeyCode::Char('f') => app.scroll = 0,
        KeyCode::Char('z') => app.zoom = !app.zoom,
        KeyCode::Char('d') => app.vigil = true,
        KeyCode::Char('?') => app.help = true,
        _ => {}
    }
    if before != (app.view, app.sel, app.zoom) {
        app.transition = Some(Instant::now());
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::{backend::TestBackend, Terminal};

    fn fixture() -> App {
        let record = json!({"id":"s-fixture", "ritual":"receipt-total", "version":1,
            "hash":"abc", "status":"ended", "outcome":"returned", "started":100,
            "ended":200, "requests":0, "tokens":0, "essence":{"requests":0},
            "inputs":{"text":"receipt"}, "result":{"total_minor":125},
            "petitions":[], "effects":[], "cleanup":null});
        App {
            core: json!({"pid":42,"version":"0.1.0","active":0,"ended":1,"petitions":0,"socket":"/tmp/fixture/core.sock"}),
            ended: vec![record.clone()],
            detail: Some(record),
            ..App::default()
        }
    }

    fn render(app: &mut App, width: u16, height: u16) -> String {
        let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
        terminal.draw(|f| draw(f, app)).unwrap();
        terminal
            .backend()
            .buffer()
            .content
            .iter()
            .map(|c| c.symbol())
            .collect()
    }

    /// Offline render fixtures for inspecting the actual terminal-cell layout.
    #[test]
    #[ignore = "writes visual fixtures to /tmp/summonr-ui-preview"]
    fn visual_preview() {
        let path = std::path::Path::new("/tmp/summonr-ui-preview");
        std::fs::create_dir_all(path).unwrap();
        for name in ["idle", "record", "daemon", "narrow"] {
            let mut app = fixture();
            if name == "idle" || name == "narrow" {
                app.ended.clear();
                app.detail = None;
            }
            app.vigil = name == "daemon";
            app.view = 1;
            let (width, height) = if name == "narrow" {
                (60, 24)
            } else {
                (140, 42)
            };
            let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
            terminal.draw(|f| draw(f, &mut app)).unwrap();
            let cells: Vec<Value> = terminal.backend().buffer().content.iter().map(|c|
                json!({"text":c.symbol(), "fg":format!("{:?}",c.fg), "bg":format!("{:?}",c.bg)})
            ).collect();
            std::fs::write(
                path.join(format!("{name}.json")),
                serde_json::to_vec(&json!({"width":width,"height":height,"cells":cells})).unwrap(),
            )
            .unwrap();
        }
    }

    #[test]
    fn workspace_survives_small_and_wide_terminals_in_every_view() {
        let mut app = fixture();
        for (w, h) in [(1, 1), (35, 11), (36, 12), (78, 22), (120, 35), (180, 50)] {
            for view in 0..4 {
                app.view = view;
                render(&mut app, w, h);
                app.vigil = true;
                render(&mut app, w, h);
                app.vigil = false;
            }
        }
        assert!(render(&mut app, 120, 35).contains("receipt-total"));
        assert!(render(&mut App::default(), 80, 24).contains("No summon is selected"));
    }

    #[test]
    fn overlays_intercept_navigation_and_escape_does_not_quit() {
        let mut app = fixture();
        handle_key(&mut app, KeyCode::Char('d'));
        handle_key(&mut app, KeyCode::Char('2'));
        assert_eq!(app.view, 0);
        assert!(!handle_key(&mut app, KeyCode::Esc));
        assert!(!app.vigil);
        handle_key(&mut app, KeyCode::Char('?'));
        assert!(!handle_key(&mut app, KeyCode::Esc));
        assert!(!app.help);
        assert!(handle_key(&mut app, KeyCode::Char('q')));
    }

    #[test]
    fn wrapped_results_scroll_to_the_end_and_never_blank_on_overscroll() {
        let mut app = fixture();
        app.detail.as_mut().unwrap()["result"] =
            json!({"long": "meaningful evidence ".repeat(100),"zz_end":"LAST EVIDENCE"});
        app.view = 1;
        assert!(!render(&mut app, 60, 24).contains("LAST EVIDENCE"));
        app.scroll = u16::MAX;
        assert!(render(&mut app, 60, 24).contains("LAST EVIDENCE"));
        handle_key(&mut app, KeyCode::Char('f'));
        assert_eq!(app.scroll, 0);
    }

    #[test]
    fn wrapped_ledger_follows_the_last_visual_line() {
        let mut app = fixture();
        app.events =
            vec![json!({"seq":1,"ts":110,"kind":"op","text":"long operation ".repeat(100)})];
        assert!(render(&mut app, 60, 24).contains("COLOPHON"));
        handle_key(&mut app, KeyCode::PageUp);
        assert!(!render(&mut app, 60, 24).contains("COLOPHON"));
    }
}
