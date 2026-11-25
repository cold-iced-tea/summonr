use crate::{ipc_frame, window_design::WindowDesign};
use serde_json::{Value, json};
use std::{
    collections::{BTreeMap, HashSet, VecDeque},
    ffi::OsString,
    os::unix::net::UnixStream,
    path::{Path, PathBuf},
    process::Command,
    time::Duration,
};

pub fn binary() -> OsString {
    std::env::var_os("SUMMONR_BIN").unwrap_or_else(|| "summonr".into())
}

pub fn command(args: &[String]) -> Result<Value, String> {
    let output = Command::new(binary())
        .args(args)
        .arg("--json")
        .output()
        .map_err(|e| format!("Cannot launch summonr: {e}"))?;
    let v: Value = serde_json::from_slice(&output.stdout).map_err(|_| {
        format!(
            "Summonr command failed: {}",
            String::from_utf8_lossy(&output.stderr)
        )
    })?;
    if !output.status.success() || v["ok"] != true {
        return Err(v["error"]["message"]
            .as_str()
            .unwrap_or("Command failed")
            .into());
    }
    Ok(v["data"].clone())
}

pub fn socket() -> Result<PathBuf, String> {
    let status = command(&["core".into(), "status".into()])?;
    status["socket"]
        .as_str()
        .map(PathBuf::from)
        .ok_or_else(|| "Missing core socket path".into())
}

struct Reader(UnixStream);
impl Reader {
    fn connect(path: &Path) -> Result<Self, String> {
        let s = UnixStream::connect(path).map_err(|e| format!("Core offline · {e}"))?;
        let timeout = Some(Duration::from_secs(3));
        s.set_read_timeout(timeout)
            .and_then(|_| s.set_write_timeout(timeout))
            .map_err(|e| e.to_string())?;
        Ok(Self(s))
    }
    fn call(&mut self, method: &str, params: Value) -> Result<Value, String> {
        ipc_frame::write_frame(
            &mut self.0,
            &json!({"jsonrpc":"2.0","id":1,"method":method,"params":params}),
        )
        .map_err(|e| e.to_string())?;
        let mut v = ipc_frame::read_frame(&mut self.0)
            .map_err(|e| e.to_string())?
            .ok_or("Core disconnected")?;
        if let Some(err) = v.get("error") {
            return Err(err["message"]
                .as_str()
                .unwrap_or("Core request failed")
                .into());
        }
        Ok(v["result"].take())
    }
}

#[derive(Clone, Default)]
pub struct Item {
    pub held: bool,
    pub started: Option<i64>,
    pub duration_ms: Option<i64>,
    pub requests: Option<u64>,
    pub id: String,
    pub name: String,
    pub status: String,
}
#[derive(Clone, Default)]
pub struct Board {
    pub active: u64,
    pub held: u64,
    pub items: Vec<Item>,
}

impl Board {
    pub fn filtered(&self, query: &str) -> Vec<&Item> {
        let query = query.trim().to_lowercase();
        if query.is_empty() {
            return self.items.iter().collect();
        }
        self.items
            .iter()
            .filter(|item| {
                let name = item.name.to_lowercase();
                let id = item.id.to_lowercase();
                query.split_whitespace().all(|term| {
                    name.contains(term)
                        || id.starts_with(term)
                        || item.status.eq_ignore_ascii_case(term)
                        || (term == "held" && item.held)
                })
            })
            .collect()
    }
}

pub fn board(path: &Path) -> Result<Board, String> {
    let mut rpc = Reader::connect(path)?;
    let status = rpc.call("ping", json!({}))?;
    let pending = if status["petitions"].as_u64().unwrap_or(0) > 0 {
        rpc.call("petitions", json!({"status":"pending"}))?
    } else {
        Value::Null
    };
    let mut items = Vec::new();
    let mut seen = HashSet::new();
    for (method, params) in [
        ("list", json!({"scope":"active","limit":1000})),
        ("rite_list", json!({})),
        ("list", json!({"scope":"ended","limit":1000})),
    ] {
        let list = rpc.call(method, params)?;
        for r in list.as_array().ok_or("Invalid run list")? {
            let id = r["id"].as_str().ok_or("Missing run ID")?;
            if seen.insert(id.to_owned()) {
                let started = r["started"].as_i64();
                items.push(Item {
                    held: pending.as_array().into_iter().flatten().any(|p| {
                        p["summon"] == id
                            || (p["operation"] == "rite.gate" && p["arguments"]["rite"] == id)
                    }) || r["status"] == "pending_owner",
                    started,
                    duration_ms: started
                        .zip(r["ended"].as_i64())
                        .map(|(start, end)| end.saturating_sub(start).max(0)),
                    requests: r["requests"].as_u64(),
                    id: id.into(),
                    name: r["ritual"]
                        .as_str()
                        .or(r["name"].as_str())
                        .unwrap_or(id)
                        .into(),
                    status: r["outcome"]
                        .as_str()
                        .or(r["status"].as_str())
                        .unwrap_or("")
                        .into(),
                });
            }
        }
    }
    Ok(Board {
        active: status["active"].as_u64().unwrap_or(0),
        held: status["petitions"].as_u64().unwrap_or(0),
        items,
    })
}

#[derive(Clone)]
pub struct TraceRow {
    pub position: String,
    pub clock: String,
    pub text: String,
    pub current: bool,
}

#[derive(Clone, Default)]
pub struct Monitor {
    pub seal: crate::seal::Seal,
    pub questions: Vec<(String, String)>,
    pub design: WindowDesign,
    pub name: String,
    pub state: String,
    pub lines: VecDeque<TraceRow>,
    pub waiting: bool,
    pub result: String,
    pub run_error: Option<String>,
    pub last_step: Option<String>,
    pub more_events: bool,
    pub hash: String,
    pub record: Value,
    pub opening: Vec<String>,
    started: Option<i64>,
    metadata_at: Option<std::time::Instant>,
    after: i64,
}
impl Monitor {
    pub fn refresh(&mut self, path: &Path, id: &str) -> Result<(), String> {
        let mut rpc = Reader::connect(path)?;
        if !id.starts_with("rite-")
            && self
                .metadata_at
                .is_some_and(|t| t.elapsed() < Duration::from_secs(1))
        {
            return self.read_events(id, |method, params| rpc.call(method, params));
        }
        self.read(id, |method, params| rpc.call(method, params))?;
        self.metadata_at = Some(std::time::Instant::now());
        Ok(())
    }

    fn read(
        &mut self,
        id: &str,
        mut call: impl FnMut(&str, Value) -> Result<Value, String>,
    ) -> Result<(), String> {
        if id.starts_with("rite-") {
            let r = call("rite_show", json!({"rite":id}))?;
            self.design = design(&r)?;
            self.name = r["name"].as_str().unwrap_or(id).into();
            self.state = r["status"].as_str().unwrap_or("").into();
            self.questions = r["stages"]
                .as_array()
                .into_iter()
                .flatten()
                .filter(|s| s["gate_status"] == "pending_owner")
                .filter_map(|s| s["stage"].as_str())
                .map(|stage| (stage.into(), format!("Accept stage {stage}?")))
                .collect();
            self.waiting = !self.questions.is_empty();
            // Rites expose their receipts as stage outputs.
            self.record["result"] = r["stages"]
                .as_array()
                .into_iter()
                .flatten()
                .rev()
                .find_map(|stage| stage.get("output").filter(|v| !v.is_null()))
                .cloned()
                .unwrap_or(Value::Null);
            self.result = if self.record["result"].is_null() {
                String::new()
            } else {
                self.record["result"].to_string()
            };
            self.lines = r["stages"]
                .as_array()
                .ok_or("Missing rite stages")?
                .iter()
                .enumerate()
                .map(|(i, s)| TraceRow {
                    position: format!("{:02X}", i),
                    clock: s["gate_status"].as_str().unwrap_or("").into(),
                    text: format!(
                        "{}   {}",
                        s["stage"].as_str().unwrap_or(""),
                        s["status"].as_str().unwrap_or("")
                    ),
                    current: matches!(s["status"].as_str(), Some("running"))
                        || s["gate_status"] == "pending_owner",
                })
                .collect();
        } else {
            let r = call("get", json!({"summon":id,"brief":true}))?;
            let hash = r["hash"].as_str().ok_or("Missing ritual hash")?;
            self.name = r["ritual"].as_str().ok_or("Missing ritual name")?.into();
            if self.hash != hash {
                let saved = call(
                    "ritual_read",
                    json!({"name":self.name,"version":r["version"]}),
                )?;
                if saved["hash"] != hash {
                    return Err("Saved ritual hash differs from run record".into());
                }
                self.design = design(&saved["definition"])?;
                self.seal.layout(&saved["definition"]["steps"]);
                self.hash = hash.into();
            }
            self.waiting = r["petitions"]
                .as_array()
                .is_some_and(|ps| ps.iter().any(|p| p["status"] == "pending"));
            self.questions.clear();
            if self.waiting {
                let pending = call("petitions", json!({"status":"pending"}))?;
                for p in pending.as_array().into_iter().flatten().filter(|p| {
                    r["petitions"]
                        .as_array()
                        .into_iter()
                        .flatten()
                        .any(|entry| entry["id"] == p["id"])
                }) {
                    let step = if p["summon"] == id {
                        p["step"].as_str().unwrap_or("")
                    } else {
                        ""
                    };
                    self.questions.push((
                        step.into(),
                        format!(
                            "Allow {} {}?",
                            p["operation"].as_str().unwrap_or("operation"),
                            p["arguments"]
                        ),
                    ));
                }
            }
            self.state = r["outcome"]
                .as_str()
                .or(r["status"].as_str())
                .unwrap_or("")
                .into();
            self.run_error = r["error"].as_str().map(str::to_owned);
            self.result = if let Some(error) = &self.run_error {
                error.clone()
            } else if !r["result"].is_null() {
                r["result"].to_string()
            } else {
                String::new()
            };
            self.started = r["started"].as_i64();
            self.record = r;
            self.read_events(id, call)?;
        }
        Ok(())
    }
    fn read_events(
        &mut self,
        id: &str,
        mut call: impl FnMut(&str, Value) -> Result<Value, String>,
    ) -> Result<(), String> {
        let page = call(
            "events",
            json!({"summon":id,"after":self.after,"limit":256,"slim":true}),
        )?;
        let events = page.as_array().ok_or("Invalid run ledger")?;
        self.more_events = events.len() == 256;
        for e in events {
            if self.opening.len() < 256 {
                self.opening.push(
                    format!(
                        "{} {}",
                        e["kind"].as_str().unwrap_or("event"),
                        e["step"].as_str().unwrap_or("")
                    )
                    .trim_end()
                    .to_owned(),
                );
            }
            if let Some(step) = e["step"].as_str() {
                self.seal.observe(step, e["text"].as_str().unwrap_or(""));
                self.last_step = Some(format!("{}   {}", step, e["text"].as_str().unwrap_or("")));
            }
            self.after = e["seq"].as_i64().ok_or("Missing event sequence")?;
            if let Some(prev) = self.lines.front_mut() {
                prev.current = false;
            }
            let elapsed = e["ts"]
                .as_i64()
                .zip(self.started)
                .map(|(ts, start)| {
                    let seconds = ts.saturating_sub(start).max(0) / 1000;
                    format!("{:02}:{:02}", seconds / 60, seconds % 60)
                })
                .unwrap_or_default();
            self.lines.push_front(TraceRow {
                position: format!("{:03X}", self.after),
                clock: elapsed,
                text: e["text"].as_str().unwrap_or("").into(),
                current: true,
            });
            if self.lines.len() > 64 {
                self.lines.pop_back();
            }
        }
        Ok(())
    }
}
pub fn result_summary(result: &Value) -> Vec<String> {
    let mut summary = Vec::new();
    if let Some(verdict) = result["verdict"].as_str().filter(|s| !s.trim().is_empty()) {
        let verdict: String = verdict.chars().take(80).collect();
        summary.push(format!("Verdict: {}", verdict.replace('_', " ")));
    }
    for (key, label) in [("raw", "Raw gold"), ("after_gather", "Gathered gold")] {
        if let Some(hit) = result[key]["gold_hit"].as_u64() {
            let score = result[key]["gold_total"]
                .as_u64()
                .map_or_else(|| hit.to_string(), |total| format!("{hit}/{total}"));
            summary.push(format!("{label}: {score}"));
        }
    }
    for key in ["findings", "confirmed", "dismissed"] {
        if let Some(count) = result[key]
            .as_u64()
            .or_else(|| result[key].as_array().map(|items| items.len() as u64))
        {
            summary.push(format!("{count} {key}"));
        }
    }
    summary
}

pub fn child_progress(children: &Value) -> Option<String> {
    let children = children.as_array().filter(|rows| !rows.is_empty())?;
    let mut counts = BTreeMap::new();
    for child in children {
        let status = child["outcome"]
            .as_str()
            .or_else(|| child["status"].as_str())
            .or_else(|| {
                child
                    .get("outcome")
                    .filter(|v| v.is_null())
                    .map(|_| "running")
            })
            .unwrap_or("unknown");
        *counts.entry(status).or_insert(0usize) += 1;
    }
    Some(format!(
        "{} children · {}",
        children.len(),
        counts
            .into_iter()
            .map(|(status, count)| format!("{count} {status}"))
            .collect::<Vec<_>>()
            .join(" · ")
    ))
}

fn design(r: &Value) -> Result<WindowDesign, String> {
    match r.get("window") {
        Some(v) => serde_json::from_value(v.clone()).map_err(|e| e.to_string()),
        None => Ok(WindowDesign::default()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn directory_matches_terms_in_order_without_renumbering_gaps() {
        let board = Board {
            items: vec![
                Item {
                    id: "s-ab123".into(),
                    name: "Bughunt-bench".into(),
                    status: "returned".into(),
                    ..Item::default()
                },
                Item {
                    id: "s-cd456".into(),
                    name: "bughunt-hunt".into(),
                    status: "running".into(),
                    held: true,
                    ..Item::default()
                },
                Item {
                    id: "s-ef789".into(),
                    name: "commit-work".into(),
                    status: "invalid".into(),
                    ..Item::default()
                },
            ],
            ..Board::default()
        };
        let ids = |query| {
            board
                .filtered(query)
                .into_iter()
                .map(|item| item.id.as_str())
                .collect::<Vec<_>>()
        };
        assert_eq!(ids("   "), ["s-ab123", "s-cd456", "s-ef789"]);
        assert_eq!(ids("BUGHUNT returned"), ["s-ab123"]);
        assert_eq!(ids("s-CD running held"), ["s-cd456"]);
        assert_eq!(ids("invalid"), ["s-ef789"]);
        assert!(ids("456").is_empty());
    }

    #[test]
    fn structured_results_accept_counts_and_lists_and_ignore_unrelated_fields() {
        assert_eq!(
            result_summary(&json!({
                "raw":{"gold_hit":2,"gold_total":3},
                "after_gather":{"gold_hit":1},
                "findings":[{},{}],
                "confirmed":0,"dismissed":1,
                "verdict":"verified_fix"
            })),
            [
                "Verdict: verified fix",
                "Raw gold: 2/3",
                "Gathered gold: 1",
                "2 findings",
                "0 confirmed",
                "1 dismissed"
            ]
        );
        assert!(
            result_summary(&json!({"findings":true,"verdict":3,"raw":{"gold_hit":-1}})).is_empty()
        );
    }

    #[test]
    fn child_counts_support_brief_and_older_records() {
        assert_eq!(child_progress(&Value::Null), None);
        assert_eq!(
            child_progress(&json!([
                {"outcome":"returned"},{"outcome":null},{"status":"running"},{"id":"unknown"}
            ]))
            .as_deref(),
            Some("4 children · 1 returned · 2 running · 1 unknown")
        );
    }

    #[test]
    fn monitor_reads_the_recorded_version_and_tracks_its_event_cursor() {
        let mut replies = [
                (
                    "get",
                    json!({"summon":"s-one","brief":true}),
                    json!({"ritual":"batch","version":2,"hash":"pinned","outcome":"returned","result":{"pass":false}}),
                ),
                (
                    "ritual_read",
                    json!({"name":"batch","version":2}),
                    json!({"hash":"pinned","definition":{"window":{"title":"Batch gate"}}}),
                ),
                (
                    "events",
                    json!({"summon":"s-one","after":0,"limit":256,"slim":true}),
                    json!([{"seq":1,"step":"gate","text":"checked"},{"seq":2,"text":"returned"}]),
                ),
            ].into_iter();
        let mut m = Monitor::default();
        m.read("s-one", |method, params| {
            let (expected_method, expected_params, result) = replies.next().unwrap();
            assert_eq!(method, expected_method);
            assert_eq!(params, expected_params);
            Ok(result)
        })
        .unwrap();
        assert_eq!(replies.count(), 0);
        assert_eq!(
            (m.after, m.design.title.as_str(), m.result.as_str()),
            (2, "Batch gate", r#"{"pass":false}"#)
        );
        assert_eq!(m.last_step.as_deref(), Some("gate   checked"));
        assert!(!m.more_events);
        assert_eq!(m.lines.front().unwrap().position, "002");
        assert!(m.lines.front().unwrap().current);
    }
}

#[derive(Clone)]
pub struct OrbitRun {
    pub id: String,
    pub ritual: String,
    pub start: i64,
    pub end: Option<i64>,
    pub state: String,
    pub petitions: Vec<(i64, Option<i64>, String)>,
}

pub fn orbit_history(path: &Path) -> Result<Vec<OrbitRun>, String> {
    let mut rpc = Reader::connect(path)?;
    let mut runs = std::collections::BTreeMap::new();
    for scope in ["active", "ended"] {
        let rows = rpc.call("list", json!({"scope": scope, "limit":1000}))?;
        for row in rows.as_array().ok_or("Invalid run history")? {
            if !row["parent"].is_null() {
                continue;
            }
            let id = row["id"].as_str().ok_or("Missing run ID")?;
            runs.insert(
                id.to_owned(),
                OrbitRun {
                    id: id.into(),
                    ritual: row["ritual"].as_str().ok_or("Missing ritual")?.into(),
                    start: row["started"].as_i64().ok_or("Missing run start")?,
                    end: row["ended"].as_i64(),
                    state: row["outcome"].as_str().unwrap_or("running").into(),
                    petitions: Vec::new(),
                },
            );
        }
    }
    let petitions = rpc.call("petitions", json!({"status":"all"}))?;
    for p in petitions.as_array().ok_or("Invalid petition history")? {
        if let Some(run) = p["summon"].as_str().and_then(|id| runs.get_mut(id))
            && let Some(created) = p["created"].as_i64()
        {
            run.petitions.push((
                created,
                p["decided"].as_i64(),
                p["operation"].as_str().unwrap_or("petition").into(),
            ));
        }
    }
    let mut runs: Vec<_> = runs.into_values().collect();
    runs.sort_by_key(|r| r.start);
    Ok(runs)
}

pub fn request(socket: &Path, method: &str, params: Value) -> Result<Value, String> {
    Reader::connect(socket)?.call(method, params)
}

/// Assays reply once all cases finish. The core enforces the runs' own time limits.
pub fn run_assay(socket: &Path, params: Value) -> Result<Value, String> {
    let mut rpc = Reader::connect(socket)?;
    rpc.0.set_read_timeout(None).map_err(|e| e.to_string())?;
    rpc.call("assay", params)
}
