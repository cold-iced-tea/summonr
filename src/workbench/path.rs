//! Read-only execution path for one pinned run. Opening and refreshing use the
//! recorded version and slim ledger pages, independently of the live board.
use super::*;
use ratatui::widgets::{Row as TableRow, Table};
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc,
};

struct Row {
    label: String,
    key: Option<(bool, String)>,
    state: String,
    evidence: VecDeque<String>,
    seq: i64,
}

impl Row {
    fn new(label: String, key: Option<(bool, String)>) -> Self {
        Self {
            label,
            key,
            state: String::new(),
            evidence: VecDeque::new(),
            seq: 0,
        }
    }

    fn note(&mut self, text: String) {
        if self.evidence.len() == 8 {
            self.evidence.pop_front();
        }
        self.evidence.push_back(clean(&text));
    }
}

fn plan_rows(steps: &[plan::Step], indent: &str, rows: &mut Vec<Row>) {
    for step in steps {
        use plan::Step::*;
        let (kind, default) = match step {
            Operation(s) => (s.operation.clone(), s.operation.as_str()),
            Model(_) => ("model".into(), "model"),
            Ritual(s) => (format!("call {}", s.ritual), s.ritual.as_str()),
            Each(s) => (format!("each {}", s.variable), "each"),
            If(s) => (
                format!("if {}", s.condition_source),
                s.condition_source.as_str(),
            ),
        };
        let id = step.id().map(String::as_str).unwrap_or(default);
        let label = if step.id().is_some() {
            format!("{indent}{id}  {kind}")
        } else {
            format!("{indent}{kind}")
        };
        rows.push(Row::new(label, Some((matches!(step, If(_)), id.into()))));
        let mut group = |name: &str, children: &[plan::Step]| {
            if children.is_empty() {
                return;
            }
            rows.push(Row::new(format!("{indent}  {name}"), None));
            plan_rows(children, &format!("{indent}    "), rows);
        };
        match step {
            If(s) => {
                group("then", &s.then);
                group("else", &s.otherwise);
            }
            Each(s) => plan_rows(&s.steps, &format!("{indent}  "), rows),
            Operation(s) => group("fallback", &s.fallback),
            Model(s) => group("fallback", &s.fallback),
            Ritual(_) => {}
        }
    }
}

struct Report {
    title: String,
    rows: Vec<Row>,
    selected: usize,
}

fn read(paths: &Paths, id: &str, closed: &AtomicBool) -> Result<Report, String> {
    let mut client = Client::connect_existing(paths).map_err(|e| e.message)?;
    let mut call = |m, p| client.call(m, p).map_err(|e| e.message);
    let record = call("get", json!({"summon":id}))?;
    let name = record["ritual"].as_str().ok_or("Missing ritual name")?;
    let version = record["version"]
        .as_u64()
        .ok_or("Missing recorded version")?;
    let saved = call("ritual_read", json!({"name":name,"version":version}))?;
    if saved["hash"].as_str().is_none() || saved["hash"] != record["hash"] {
        return Err("Saved definition differs from the recorded hash.".into());
    }
    let def: ritual::Ritual = decode(saved["definition"].clone())?;
    let mut rows = Vec::new();
    plan_rows(&plan::compile(&def.steps)?, "", &mut rows);
    if !def.finally.is_empty() {
        rows.push(Row::new("finally".into(), None));
        plan_rows(&plan::compile(&def.finally)?, "  ", &mut rows);
    }
    // A local step ID can occur in several scopes. Leave ambiguous evidence
    // on the run ledger rather than attributing it to a particular site.
    let mut sites = HashMap::new();
    for (index, row) in rows.iter().enumerate() {
        if let Some(key) = &row.key {
            sites
                .entry(key.clone())
                .and_modify(|v| *v = None)
                .or_insert(Some(index));
        }
    }
    let other = rows.len();
    rows.push(Row::new("Run ledger".into(), None));
    let mut after = 0;
    loop {
        if closed.load(Ordering::Relaxed) {
            return Err("Closed".into());
        }
        let page: Vec<Ledger> = decode(call(
            "events",
            json!({"summon":id,"after":after,"limit":256,"slim":true}),
        )?)?;
        for event in &page {
            after = event.seq;
            if matches!(event.kind.as_str(), "summon.start" | "summon.end") {
                continue;
            }
            let index = event
                .step
                .as_ref()
                .and_then(|s| {
                    sites.get(&(
                        matches!(event.kind.as_str(), "branch.then" | "branch.else"),
                        s.clone(),
                    ))
                })
                .copied()
                .flatten()
                .unwrap_or(other);
            let row = &mut rows[index];
            row.seq = event.seq;
            row.state = match event.kind.as_str() {
                "branch.then" => "then taken / else skipped",
                "branch.else" => "else taken / then skipped",
                "step.error" => "failed attempt",
                "op" => "operation recorded",
                "model" => "model responded",
                "held" => "hold recorded",
                kind => kind,
            }
            .into();
            row.note(format!("#{}  {}  {}", event.seq, event.kind, event.text));
        }
        if page.len() < 256 {
            break;
        }
    }
    let petitions: Vec<Value> = decode(call("petitions", json!({"summon":id,"status":"all"}))?)?;
    let mut pending = None;
    for p in petitions {
        let step = p["step"].as_str().unwrap_or("");
        let i = sites
            .get(&(false, step.into()))
            .copied()
            .flatten()
            .unwrap_or(other);
        let status = p["status"].as_str().unwrap_or("unknown");
        if status == "pending" {
            pending = Some(i);
            rows[i].state = "held for review".into();
        } else if rows[i].state != "held for review" {
            rows[i].state = format!("petition {status}");
        }
        rows[i].note(format!(
            "{}  {status}  {}  {}",
            p["id"].as_str().unwrap_or(""),
            p["operation"].as_str().unwrap_or(""),
            p["arguments"]
        ));
    }
    let last = rows
        .iter()
        .enumerate()
        .filter(|(_, r)| r.seq > 0)
        .max_by_key(|(_, r)| r.seq)
        .map(|(i, _)| i);
    if rows[other].evidence.is_empty() {
        rows.pop();
    }
    let mut end = Row::new("Run outcome".into(), None);
    end.state = record["outcome"]
        .as_str()
        .or(record["status"].as_str())
        .unwrap_or("unknown")
        .into();
    if !record["result"].is_null() {
        end.note(format!("result  {}", record["result"]));
    }
    if !record["error"].is_null() {
        end.note(format!("error  {}", record["error"]));
    }
    let selected = pending
        .or_else(|| if end.state == "returned" { None } else { last })
        .unwrap_or(rows.len());
    rows.push(end);
    Ok(Report {
        title: format!("{name} v{version}  /  {id}"),
        rows,
        selected,
    })
}

pub(super) struct View {
    pub id: String,
    closed: Arc<AtomicBool>,
    rx: Receiver<Result<Report, String>>,
    report: Option<Report>,
    message: String,
    scroll: u16,
}

impl Drop for View {
    fn drop(&mut self) {
        self.closed.store(true, Ordering::Relaxed);
    }
}

impl View {
    pub fn open(paths: &Paths, id: String) -> Self {
        let (tx, rx) = mpsc::channel();
        let paths = paths.clone();
        let run = id.clone();
        let closed = Arc::new(AtomicBool::new(false));
        let cancel = closed.clone();
        std::thread::spawn(move || {
            let _ = tx.send(read(&paths, &run, &cancel));
        });
        Self {
            id,
            closed,
            rx,
            report: None,
            message: "Reading execution record…".into(),
            scroll: 0,
        }
    }

    pub fn key(&mut self, key: KeyCode) {
        let Some(r) = &mut self.report else { return };
        match key {
            KeyCode::Up => r.selected = r.selected.saturating_sub(1),
            KeyCode::Down => r.selected = (r.selected + 1).min(r.rows.len() - 1),
            KeyCode::Home => r.selected = 0,
            KeyCode::End => r.selected = r.rows.len() - 1,
            KeyCode::PageUp => {
                self.scroll = self.scroll.saturating_sub(4);
                return;
            }
            KeyCode::PageDown => {
                self.scroll = self.scroll.saturating_add(4);
                return;
            }
            _ => return,
        }
        self.scroll = 0;
    }

    pub fn draw(&mut self, f: &mut Frame, area: Rect) {
        if let Ok(result) = self.rx.try_recv() {
            match result {
                Ok(report) => self.report = Some(report),
                Err(e) => self.message = e,
            }
        }
        let parts = Layout::vertical([
            Constraint::Length(1),
            Constraint::Percentage(53),
            Constraint::Min(3),
            Constraint::Length(1),
        ])
        .split(area);
        f.render_widget(
            Paragraph::new(line(Ink::Gold, " SUMMONR  /  Execution path")),
            parts[0],
        );
        f.render_widget(
            Paragraph::new(" Up/Down step   PgUp/Dn evidence   r refresh   Esc back"),
            parts[3],
        );
        let Some(r) = &self.report else {
            f.render_widget(
                Paragraph::new(self.message.as_str()).wrap(Wrap { trim: false }),
                parts[1],
            );
            return;
        };
        let panel = block(&r.title);
        let inner = panel.inner(parts[1]);
        let first = (r.selected + 1).saturating_sub(inner.height as usize);
        let rows = r
            .rows
            .iter()
            .enumerate()
            .skip(first)
            .take(inner.height as usize)
            .map(|(i, row)| {
                let selected = i == r.selected;
                TableRow::new([if selected { ">" } else { " " }, &row.label, &row.state])
                    .style(ink(if selected { Ink::Ember } else { Ink::Bone }))
            });
        let table = Table::new(
            rows,
            [
                Constraint::Length(1),
                Constraint::Percentage(58),
                Constraint::Min(12),
            ],
        )
        .block(panel);
        f.render_widget(table, parts[1]);
        let row = &r.rows[r.selected];
        let panel = block(format!("Evidence / {}", row.label.trim()));
        let inner = panel.inner(parts[2]);
        let evidence = if row.evidence.is_empty() {
            "No recorded evidence for this row.".into()
        } else {
            row.evidence
                .iter()
                .rev()
                .cloned()
                .collect::<Vec<_>>()
                .join("\n")
        };
        let text = Paragraph::new(evidence).wrap(Wrap { trim: false });
        self.scroll = self.scroll.min(
            text.line_count(inner.width)
                .saturating_sub(inner.height as usize)
                .min(u16::MAX as usize) as u16,
        );
        f.render_widget(text.scroll((self.scroll, 0)).block(panel), parts[2]);
    }
}
