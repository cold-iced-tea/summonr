mod store;
use super::{
    APRICOT, DIM, INK, PAPER, RULE, RunWindow, VIOLET, client, options, page, seal, type_line,
};
use gpui::{prelude::*, *};
use gpui_component::{
    Root,
    input::{Input, InputState},
};
use serde_json::{Value, json};
use std::{collections::HashMap, path::PathBuf};
use store::Shelf;

struct OpenLab(WindowHandle<Root>, WeakEntity<Laboratory>);
impl Global for OpenLab {}

pub fn open(path: Option<PathBuf>, baseline: Option<String>, cx: &mut App) -> Result<(), String> {
    if let Some(open) = cx.try_global::<OpenLab>() {
        let (handle, lab) = (open.0, open.1.clone());
        if handle
            .update(cx, |_, w, cx| {
                w.activate_window();
                if let Some(id) = baseline.clone() {
                    let _ = lab.update(cx, |v, cx| v.import(id, cx));
                }
            })
            .is_ok()
        {
            return Ok(());
        }
    }
    cx.open_window(options(1050., 700., cx), |w, cx| {
        let lab = cx.new(|cx| Laboratory::new(path, baseline, w, cx));
        let weak = lab.downgrade();
        let root = cx.new(|cx| Root::new(lab, w, cx));
        cx.set_global(OpenLab(w.window_handle().downcast::<Root>().unwrap(), weak));
        root
    })
    .map_err(|e| format!("Cannot open Laboratory: {e}"))?;
    Ok(())
}
struct Side {
    run: Entity<RunWindow>,
    _watch: Subscription,
}
#[derive(Clone, Default)]
struct Pair {
    id: String,
    report: Option<Value>,
}
fn pair_cases(baseline: &[Value], trial: &[Value]) -> Result<HashMap<String, Pair>, String> {
    fn index(rows: &[Value]) -> Result<HashMap<&str, &Value>, String> {
        let mut cases = HashMap::new();
        for row in rows {
            let name = row["case"].as_str().ok_or("Missing case name")?;
            if cases.insert(name, row).is_some() {
                return Err(format!("Cannot pair duplicate case {name}"));
            }
        }
        Ok(cases)
    }
    let trials = index(trial)?;
    let mut pairs = HashMap::new();
    for (name, base) in index(baseline)? {
        if let Some(row) = trials.get(name) {
            let id = base["summon"].as_str().ok_or("Missing baseline ID")?;
            let trial = row["summon"].as_str().ok_or("Missing trial ID")?;
            pairs.insert(
                id.into(),
                Pair {
                    id: trial.into(),
                    report: Some((*row).clone()),
                },
            );
        }
    }
    Ok(pairs)
}
struct Laboratory {
    path: Option<PathBuf>,
    shelf: Option<Shelf>,
    series: Option<String>,
    cases: Vec<Value>,
    index: usize,
    baseline: Option<Side>,
    trial: Option<Side>,
    pairs: HashMap<String, Pair>,
    model: Entity<InputState>,
    version: Entity<InputState>,
    inputs: Entity<InputState>,
    criterion: Entity<InputState>,
    assay_name: Entity<InputState>,
    assay_set: Entity<InputState>,
    running_assay: bool,
    pending_form: bool,
    overlay: bool,
    focus: FocusHandle,
    loading: bool,
    launch: bool,
    generation: u64,
    notice: String,
}
impl Laboratory {
    fn new(
        path: Option<PathBuf>,
        id: Option<String>,
        w: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let focus = cx.focus_handle();
        focus.focus(w, cx);
        let mut lab = Self {
            path,
            shelf: None,
            series: None,
            cases: vec![],
            index: 0,
            baseline: None,
            trial: None,
            pairs: HashMap::new(),
            model: cx.new(|cx| InputState::new(w, cx).placeholder("Ritual default")),
            version: cx.new(|cx| InputState::new(w, cx).placeholder("Version")),
            inputs: cx.new(|cx| InputState::new(w, cx).placeholder("Input object")),
            criterion: cx
                .new(|cx| InputState::new(w, cx).placeholder("Expected fields (optional)")),
            assay_name: cx.new(|cx| InputState::new(w, cx).placeholder("Saved ritual name")),
            assay_set: cx
                .new(|cx| InputState::new(w, cx).placeholder("Set file or recorded baseline")),
            running_assay: false,
            pending_form: false,
            overlay: false,
            focus,
            loading: false,
            launch: false,
            generation: 0,
            notice: String::new(),
        };
        for input in [
            &lab.version,
            &lab.model,
            &lab.inputs,
            &lab.criterion,
            &lab.assay_name,
            &lab.assay_set,
        ] {
            let handle = input.read(cx).focus_handle(cx);
            cx.on_focus(&handle, w, |_, _, cx| cx.notify()).detach();
            cx.on_blur(&handle, w, |_, _, cx| cx.notify()).detach();
        }
        lab.refresh(id, cx);
        lab
    }
    fn refresh(&mut self, id: Option<String>, cx: &mut Context<Self>) {
        let path = self.path.clone();
        let generation = self.generation;
        self.notice = "Reading the shelf…".into();
        cx.spawn(async move |this, cx| {
            let (path, shelf) = cx
                .background_executor()
                .spawn(async move {
                    let path = path.map(Ok).unwrap_or_else(client::socket);
                    let shelf = path
                        .as_ref()
                        .map_err(Clone::clone)
                        .and_then(|p| store::shelf(p));
                    (path.ok(), shelf)
                })
                .await;
            let _ = this.update(cx, |v, cx| {
                v.path = path;
                let current = v.generation == generation;
                match shelf {
                    Ok(s) => {
                        v.shelf = Some(s);
                        if current {
                            v.notice.clear();
                        }
                    }
                    Err(e) => {
                        if current {
                            v.notice = e;
                        }
                    }
                }
                if current {
                    if v.pending_form && v.baseline.is_none() && !v.cases.is_empty() {
                        v.pick(v.index, cx);
                    }
                    if let Some(id) = id {
                        v.import(id, cx)
                    } else if v.baseline.is_none() && !v.loading {
                        if let Some(series) = v
                            .shelf
                            .as_ref()
                            .and_then(|s| s.series.first())
                            .map(|s| s.id.clone())
                        {
                            v.load_series(series, false, cx);
                        } else if let Some(id) = v
                            .shelf
                            .as_ref()
                            .and_then(|s| s.recent.first())
                            .map(|r| r.0.clone())
                        {
                            v.import(id, cx);
                        }
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }
    fn import(&mut self, id: String, cx: &mut Context<Self>) {
        self.generation += 1;
        self.loading = false;
        self.series = None;
        self.cases = vec![json!({"case":"Recorded run","summon":id})];
        self.pick(0, cx);
    }
    fn side(&self, id: &str, cx: &mut Context<Self>) -> Option<Side> {
        let path = self.path.clone()?;
        let run = cx.new(|cx| RunWindow::new(path, id.into(), true, cx));
        let watch = cx.observe(&run, |_, _, cx| cx.notify());
        Some(Side { run, _watch: watch })
    }
    fn pick(&mut self, index: usize, cx: &mut Context<Self>) {
        let Some(id) = self
            .cases
            .get(index)
            .and_then(|r| r["summon"].as_str())
            .map(str::to_owned)
        else {
            return;
        };
        self.index = index;
        self.baseline = self.side(&id, cx);
        self.trial = self
            .pairs
            .get(&id)
            .map(|p| p.id.clone())
            .and_then(|id| self.side(&id, cx));
        self.overlay = false;
        self.pending_form = true;
        cx.notify();
    }
    fn load_series(&mut self, series: String, as_trial: bool, cx: &mut Context<Self>) {
        let Some(db) = self.shelf.as_ref().map(|s| s.db.clone()) else {
            return;
        };
        self.generation += 1;
        let generation = self.generation;
        self.loading = true;
        cx.spawn(async move |this, cx| {
            let key = series.clone();
            let cases = cx
                .background_executor()
                .spawn(async move { store::cases(&db, &key) })
                .await;
            let _ = this.update(cx, |v, cx| {
                if v.generation != generation {
                    return;
                }
                v.loading = false;
                match cases {
                    Ok(cases) if as_trial => {
                        match pair_cases(&v.cases, &cases) {
                            Ok(pairs) => {
                                let count = pairs.len();
                                for case in &v.cases {
                                    if let Some(id) = case["summon"].as_str() {
                                        v.pairs.remove(id);
                                    }
                                }
                                v.pairs.extend(pairs);
                                v.notice = format!(
                                    "{count} of {} baseline cases paired by name · {} trial cases",
                                    v.cases.len(),
                                    cases.len()
                                );
                            }
                            Err(e) => {
                                v.notice = e;
                                cx.notify();
                                return;
                            }
                        }
                        v.pick(v.index, cx);
                    }
                    Ok(cases) => {
                        v.series = Some(series);
                        v.cases = cases;
                        v.notice.clear();
                        v.pick(0, cx);
                    }
                    Err(e) => v.notice = e,
                }
                cx.notify();
            });
        })
        .detach();
    }
    fn run_assay(&mut self, cx: &mut Context<Self>) {
        if self.running_assay {
            return;
        }
        let Some(path) = self.path.clone() else {
            self.notice = "Waiting for the board connection".into();
            cx.notify();
            return;
        };
        let ritual = self.assay_name.read(cx).value().trim().to_owned();
        if ritual.is_empty() {
            self.notice = "Enter the saved ritual name for the assay".into();
            cx.notify();
            return;
        }
        let mut params =
            json!({"ritual":ritual,"live":false,"cwd":std::env::current_dir().unwrap_or_default()});
        let file = self.assay_set.read(cx).value();
        if !file.trim().is_empty() {
            params["against"] = file.trim().into();
        }
        let generation = self.generation;
        let pair = self.series.is_some()
            && self
                .baseline
                .as_ref()
                .is_some_and(|s| s.run.read(cx).data.name == ritual);
        self.running_assay = true;
        self.notice = format!("Measuring {ritual}…");
        cx.notify();
        cx.spawn(async move |this, cx| {
            let (report, shelf) = cx
                .background_executor()
                .spawn(async move {
                    let report = client::run_assay(&path, params);
                    let shelf = store::shelf(&path);
                    (report, shelf)
                })
                .await;
            let _ = this.update(cx, |v, cx| {
                v.running_assay = false;
                if let Ok(s) = shelf {
                    v.shelf = Some(s);
                }
                match report {
                    Ok(r) => {
                        v.notice = format!(
                            "{} of {} cases met their criterion",
                            r["account"]["correct"], r["account"]["cases"]
                        );
                        if generation == v.generation
                            && v.shelf.is_some()
                            && let Some(id) = r["series"].as_str()
                        {
                            v.load_series(id.into(), pair, cx);
                        }
                    }
                    Err(e) => v.notice = e,
                }
                cx.notify();
            });
        })
        .detach();
    }
    fn run_trial(&mut self, cx: &mut Context<Self>) {
        if self.launch || self.loading || self.pending_form {
            return;
        }
        let (Some(path), Some(base)) = (self.path.clone(), self.baseline.as_ref()) else {
            return;
        };
        let base = base.run.read(cx);
        let parsed = (|| -> Result<_, String> {
            let args: serde_json::Map<String, Value> =
                serde_json::from_str(&self.inputs.read(cx).value())
                    .map_err(|e| format!("Inputs must be a JSON object · {e}"))?;
            let version = self
                .version
                .read(cx)
                .value()
                .trim()
                .parse::<u32>()
                .ok()
                .filter(|v| *v > 0)
                .ok_or("Enter a positive ritual version")?;
            let model = self.model.read(cx).value().trim().to_owned();
            let mut p =
                json!({"target":base.data.name,"version":version,"args":args,"simulate":true});
            if !model.is_empty() {
                p["model"] = model.into();
            }
            let criterion = self.criterion.read(cx).value();
            let expected = if criterion.trim().is_empty() {
                Value::Null
            } else {
                Value::Object(
                    serde_json::from_str(&criterion)
                        .map_err(|e| format!("Expected fields must be a JSON object · {e}"))?,
                )
            };
            Ok((p, expected))
        })();
        let (params, expected) = match parsed {
            Ok(p) => p,
            Err(e) => {
                self.notice = e;
                cx.notify();
                return;
            }
        };
        let id = base.id.clone();
        self.launch = true;
        self.notice = "Submitting simulated trial…".into();
        cx.notify();
        cx.spawn(async move |this, cx| {
            let result = cx
                .background_executor()
                .spawn(async move { client::request(&path, "invoke", params) })
                .await;
            let _ = this.update(cx, |v, cx| {
                v.launch = false;
                match result.and_then(|r| {
                    r["summon"]
                        .as_str()
                        .map(str::to_owned)
                        .ok_or("Missing trial ID".into())
                }) {
                    Ok(key) => {
                        v.pairs.insert(
                            id.clone(),
                            Pair {
                                id: key.clone(),
                                report: Some(json!({"expected":expected})),
                            },
                        );
                        v.notice = format!("Trial {key}");
                        if v.cases.get(v.index).is_some_and(|r| r["summon"] == id) {
                            v.trial = v.side(&key, cx);
                        }
                    }
                    Err(e) => v.notice = e,
                }
                cx.notify();
            });
        })
        .detach();
    }
}

fn button(label: impl Into<SharedString>, color: u32) -> Stateful<Div> {
    let label = label.into();
    div()
        .id(label.clone())
        .cursor_pointer()
        .px_2()
        .py_1()
        .border_1()
        .border_color(rgb(RULE))
        .hover(|d| d.bg(rgb(RULE)))
        .child(type_line(label, 12., color))
}
fn evidence(v: &Value) -> String {
    if let Some(s) = v.as_str() {
        s.to_owned()
    } else {
        v.to_string()
    }
}
fn verdict(expected: &Value, record: &Value) -> &'static str {
    if record["ended"].is_null() {
        return "In progress";
    }
    let Some(fields) = expected.as_object().filter(|f| !f.is_empty()) else {
        return "Unscored";
    };
    if !matches!(record["outcome"].as_str(), Some("returned" | "abstained")) {
        return "Missed";
    }
    if fields.iter().all(|(path, want)| {
        let mut got = &record["result"];
        for part in path.split('.') {
            got = match got {
                Value::Null => got,
                Value::Array(a) => match part.parse::<usize>().ok().and_then(|i| a.get(i)) {
                    Some(v) => v,
                    None => return false,
                },
                Value::Object(o) => match o.get(part) {
                    Some(v) => v,
                    None => return false,
                },
                _ => return false,
            };
        }
        got == want
    }) {
        "Met"
    } else {
        "Missed"
    }
}

fn plot(a: Option<&seal::Seal>, b: Option<&seal::Seal>, color: u32) -> impl IntoElement {
    let mut labels = Vec::<String>::new();
    let mut edges = std::collections::BTreeMap::new();
    let mut points = std::collections::BTreeMap::new();
    for (seal, bit) in [(a, 1u8), (b, 2u8)] {
        if let Some(s) = seal {
            let mut loc = Vec::new();
            for site in &s.sites {
                let index = labels
                    .iter()
                    .position(|n| n == &site.name)
                    .unwrap_or_else(|| {
                        labels.push(site.name.clone());
                        labels.len() - 1
                    });
                loc.push(index);
                if site.evidence.is_some() {
                    *points.entry(index).or_insert(0u8) |= bit;
                }
            }
            for &(from, to) in &s.strokes {
                let (x, y) = (loc[from], loc[to]);
                *edges.entry((x.min(y), x.max(y))).or_insert(0u8) |= bit;
            }
        }
    }
    let overlay = b.is_some();
    let ink = move |mask: u8| {
        if !overlay {
            color
        } else {
            match mask {
                1 => VIOLET,
                2 => APRICOT,
                _ => INK,
            }
        }
    };
    let width = (91 * labels.len().div_ceil(9)).max(91) as f32;
    canvas(
        |_, _, _| (),
        move |bounds, _, w, _| {
            let scale = (f32::from(bounds.size.width) / width).min(1.35);
            let pos = |i| {
                let (x, y) = seal::position(i);
                bounds.origin + point(px(scale * x), px(3. + scale * y))
            };
            for (&(x, y), &mask) in &edges {
                let mut pen = PathBuilder::stroke(px(1.2));
                pen.move_to(pos(x));
                pen.line_to(pos(y));
                if let Ok(line) = pen.build() {
                    w.paint_path(line, rgb(ink(mask)));
                }
            }
            for (&i, &mask) in &points {
                w.paint_quad(fill(
                    Bounds::new(pos(i) - point(px(2.), px(2.)), size(px(4.), px(4.))),
                    rgb(ink(mask)),
                ));
            }
        },
    )
    .w_full()
    .h(px(80.))
}
fn side_panel(label: &str, data: Option<&RunWindow>, report: Option<&Value>, color: u32) -> Div {
    let mut panel = div()
        .flex_1()
        .min_w_0()
        .p_2()
        .flex()
        .flex_col()
        .gap_1()
        .border_t_1()
        .border_color(rgb(color))
        .child(type_line(label.to_owned(), 13., color));
    let Some(run) = data else {
        return panel;
    };
    let d = &run.data;
    let r = &d.record;
    panel = panel
        .child(type_line(
            format!(
                "{}  {}",
                d.name,
                run.id.chars().take(10).collect::<String>()
            ),
            14.,
            INK,
        ))
        .child(type_line(
            format!(
                "v{}  ·  {}",
                r["version"],
                r["model"].as_str().unwrap_or("ritual default")
            ),
            12.,
            DIM,
        ))
        .child(plot(Some(&d.seal), None, color));
    if !run.error.is_empty() {
        return panel.child(type_line(run.error.clone(), 12., APRICOT));
    }
    let expected = report.map(|c| &c["expected"]).unwrap_or(&Value::Null);
    let criterion = match report.and_then(|v| v["correct"].as_bool()) {
        Some(true) => "Met",
        Some(false) => "Missed",
        None => verdict(expected, r),
    };
    let duration = r["started"]
        .as_i64()
        .zip(r["ended"].as_i64())
        .map(|(a, b)| super::elapsed((b - a).max(0)))
        .unwrap_or_else(|| "…".into());
    panel
        .child(type_line(format!("{}   ·   {duration}", d.state), 13., INK))
        .child(type_line(
            format!("Criterion   {criterion}"),
            13.,
            if criterion == "Missed" {
                APRICOT
            } else {
                color
            },
        ))
        .when(!expected.is_null(), |e| {
            e.child(type_line(
                format!("Expected   {}", evidence(expected)),
                12.,
                DIM,
            ))
        })
        .when_some(
            report.and_then(|r| r["touchstone_error"].as_str()),
            |e, err| e.child(type_line(err.to_owned(), 12., APRICOT)),
        )
        .when(!d.result.is_empty(), |e| {
            e.child(
                div()
                    .id(SharedString::from(format!("result-{}", run.id)))
                    .max_h(px(110.))
                    .overflow_y_scroll()
                    .child(type_line(d.result.clone(), 12., INK)),
            )
        })
}
fn first_difference(a: &client::Monitor, b: &client::Monitor) -> String {
    for (i, (left, right)) in a.opening.iter().zip(&b.opening).enumerate() {
        if left != right {
            return format!("Event {}   ·   {}  →  {}", i + 1, left, right);
        }
    }
    if !a.record["ended"].is_null()
        && !b.record["ended"].is_null()
        && !a.more_events
        && !b.more_events
    {
        let n = a.opening.len().min(b.opening.len());
        if a.opening.len() != b.opening.len() {
            return format!(
                "Event {}   ·   {}  →  {}",
                n + 1,
                a.opening.get(n).map(String::as_str).unwrap_or("end"),
                b.opening.get(n).map(String::as_str).unwrap_or("end")
            );
        }
        if let (Some(x), Some(y)) = (
            a.record["result"].as_object(),
            b.record["result"].as_object(),
        ) {
            for k in x.keys().chain(y.keys()) {
                if x.get(k) != y.get(k) {
                    return format!(
                        "Returned field {k}   ·   {}  →  {}",
                        x.get(k).map(evidence).unwrap_or("missing".into()),
                        y.get(k).map(evidence).unwrap_or("missing".into())
                    );
                }
            }
        }
        if a.record["result"] != b.record["result"] {
            return "Returned values differ".into();
        }
        return format!(
            "First {n} recorded event kinds and step names match. Returned values match."
        );
    }
    format!(
        "{} matching events observed so far",
        a.opening.len().min(b.opening.len())
    )
}
impl Render for Laboratory {
    fn render(&mut self, w: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        w.set_window_title("Summonr · Laboratory");
        let base = self.baseline.as_ref().map(|s| s.run.read(cx));
        let case = self.cases.get(self.index);
        let pair = case
            .and_then(|r| r["summon"].as_str())
            .and_then(|id| self.pairs.get(id));
        if self.pending_form
            && let Some(base) = base
            && !base.data.record.is_null()
        {
            let r = &base.data.record;
            let expected = case.map(|c| &c["expected"]).unwrap_or(&Value::Null);
            let fields = [
                (&self.assay_name, base.data.name.clone()),
                (&self.model, r["model"].as_str().unwrap_or("").to_owned()),
                (&self.version, r["version"].to_string()),
                (&self.inputs, r["inputs"].to_string()),
                (
                    &self.criterion,
                    if expected.is_null() {
                        String::new()
                    } else {
                        expected.to_string()
                    },
                ),
            ];
            for (input, text) in fields {
                input.update(cx, |i, cx| i.set_value(text, w, cx));
            }
            self.pending_form = false;
        }
        let base = self.baseline.as_ref().map(|s| s.run.read(cx));
        let trial = self.trial.as_ref().map(|s| s.run.read(cx));
        let aligned = base
            .zip(trial)
            .is_some_and(|(a, b)| !a.data.hash.is_empty() && a.data.hash == b.data.hash);
        let mut shelf = div().id("lab-shelf").overflow_y_scroll().flex_1().min_h_0();
        if let Some(s) = &self.shelf {
            for entry in &s.series {
                let key = entry.id.clone();
                let other = key.clone();
                let compatible = self.series.as_ref().is_some_and(|id| {
                    s.series
                        .iter()
                        .any(|e| &e.id == id && e.ritual == entry.ritual)
                }) && self.series.as_ref() != Some(&key);
                let date = chrono::DateTime::from_timestamp_millis(entry.date)
                    .map(|t| {
                        t.with_timezone(&chrono::Local)
                            .format("%m/%d %H:%M")
                            .to_string()
                    })
                    .unwrap_or_default();
                shelf =
                    shelf.child(
                        div()
                            .id(SharedString::from(key.clone()))
                            .py_1()
                            .child(
                                button(
                                    entry.ritual.clone(),
                                    if self.series.as_ref() == Some(&key) {
                                        APRICOT
                                    } else {
                                        INK
                                    },
                                )
                                .on_click(cx.listener(
                                    move |v, _, _, cx| v.load_series(key.clone(), false, cx),
                                )),
                            )
                            .child(type_line(
                                format!("{date}  ·  {}/{} met", entry.passed, entry.count),
                                12.,
                                DIM,
                            ))
                            .when(compatible, |e| {
                                e.child(button("Pair as trial", APRICOT).on_click(cx.listener(
                                    move |v, _, _, cx| v.load_series(other.clone(), true, cx),
                                )))
                            }),
                    );
            }
            shelf = shelf
                .when(s.series.is_empty(), |e| {
                    e.child(type_line("No recorded assays", 12., DIM))
                })
                .child(type_line("Recent runs", 13., VIOLET).mt_3());
            for (id, name) in &s.recent {
                let id = id.clone();
                shelf = shelf.child(
                    div()
                        .id(SharedString::from(id.clone()))
                        .py_1()
                        .cursor_pointer()
                        .child(type_line(
                            format!("{}  {name}", id.chars().take(8).collect::<String>()),
                            12.,
                            INK,
                        ))
                        .on_click(cx.listener(move |v, _, _, cx| v.import(id.clone(), cx))),
                );
            }
        }
        let mut cases = div().id("lab-cases").overflow_y_scroll().max_h(px(120.));
        for (i, row) in self.cases.iter().enumerate() {
            let id = row["summon"].as_str().unwrap_or("");
            let paired = self.pairs.get(id);
            let score = |r: &Value| match r["correct"].as_bool() {
                Some(true) => "met",
                Some(false) => "missed",
                None => "unscored",
            };
            cases = cases.child(
                div()
                    .id(("lab-case", i))
                    .cursor_pointer()
                    .px_2()
                    .py_1()
                    .when(i == self.index, |e| e.bg(rgb(RULE)))
                    .child(type_line(
                        format!(
                            "{}    {}    {}",
                            row["case"].as_str().unwrap_or("case"),
                            score(row),
                            paired
                                .map(|p| p
                                    .report
                                    .as_ref()
                                    .filter(|r| r.get("correct").is_some())
                                    .map(score)
                                    .unwrap_or("trial"))
                                .unwrap_or("—")
                        ),
                        12.,
                        INK,
                    ))
                    .on_click(cx.listener(move |v, _, _, cx| v.pick(i, cx))),
            );
        }
        let field = |label: &str, input: &Entity<InputState>| {
            div()
                .flex_1()
                .min_w_0()
                .child(type_line(label.to_owned(), 12., DIM))
                .child(
                    Input::new(input)
                        .appearance(false)
                        .bordered(false)
                        .focus_bordered(false)
                        .bg(rgb(PAPER))
                        .text_color(rgb(INK))
                        .border_1()
                        .border_color(rgb(if input.read(cx).focus_handle(cx).is_focused(w) {
                            APRICOT
                        } else {
                            RULE
                        })),
                )
        };
        let editable = self.baseline.is_some() && !self.pending_form;
        let controls = div()
            .flex()
            .flex_col()
            .gap_1()
            .child(
                div()
                    .flex()
                    .gap_2()
                    .child(field("Trial version", &self.version))
                    .child(field("Trial model", &self.model)),
            )
            .child(field("Trial inputs", &self.inputs))
            .child(field("Expected fields", &self.criterion))
            .child(
                button(
                    if self.launch {
                        "Submitting…"
                    } else {
                        "Run simulated trial"
                    },
                    APRICOT,
                )
                .on_click(cx.listener(|v, _, _, cx| v.run_trial(cx))),
            );
        let assay_controls = div()
            .mt_3()
            .border_t_1()
            .border_color(rgb(RULE))
            .pt_2()
            .child(
                div()
                    .flex()
                    .gap_2()
                    .child(field("Assay ritual", &self.assay_name))
                    .child(field("Assay set", &self.assay_set)),
            )
            .child(
                button(
                    if self.running_assay {
                        "Measuring…"
                    } else {
                        "Run simulated assay"
                    },
                    VIOLET,
                )
                .on_click(cx.listener(|v, _, _, cx| v.run_assay(cx))),
            );
        let mut bench = div()
            .id("lab-bench")
            .overflow_y_scroll()
            .min_h_0()
            .min_w_0()
            .flex_1()
            .child(cases);
        if self.cases.is_empty() {
            bench = bench.child(type_line(
                "Choose a recorded run or start an assay below.",
                14.,
                DIM,
            ));
        }
        if base.is_some() {
            let trial_panel = side_panel(
                "Trial",
                trial,
                pair.and_then(|p| p.report.as_ref()),
                APRICOT,
            )
            .when(editable, |el| el.child(controls.mt_2()));
            let panels = div()
                .flex()
                .gap_2()
                .mt_2()
                .when(w.viewport_size().width < px(690.), |el| el.flex_col())
                .child(side_panel("Baseline", base, case, VIOLET))
                .child(trial_panel);
            bench = bench.child(panels);
        }
        if aligned {
            bench = bench.child(button("Overlay · Space", VIOLET).on_click(cx.listener(
                |v, _, w, cx| {
                    v.focus.focus(w, cx);
                    v.overlay = !v.overlay;
                    cx.notify();
                },
            )));
            if self.overlay {
                bench = bench.child(plot(
                    base.map(|b| &b.data.seal),
                    trial.map(|b| &b.data.seal),
                    INK,
                ));
            }
        }
        if let Some((a, b)) = base
            .zip(trial)
            .filter(|(a, b)| !a.data.record.is_null() && !b.data.record.is_null())
        {
            for (key, label) in [
                ("version", "Version"),
                ("model", "Recorded model"),
                ("inputs", "Inputs"),
            ] {
                let (left, right) = (&a.data.record[key], &b.data.record[key]);
                if left != right {
                    bench = bench.child(
                        div()
                            .id(SharedString::from(format!("changed-{key}")))
                            .max_h(px(70.))
                            .overflow_y_scroll()
                            .child(type_line(
                                format!("{label}   {}  →  {}", evidence(left), evidence(right)),
                                12.,
                                DIM,
                            )),
                    );
                }
            }
            bench = bench
                .child(type_line("Recorded difference", 12., DIM).mt_2())
                .child(type_line(first_difference(&a.data, &b.data), 13., INK));
        }
        let bench = bench.child(assay_controls);
        let compact = w.viewport_size().width < px(690.);
        page(cx)
            .track_focus(&self.focus)
            .on_key_down(cx.listener(move |v, e: &KeyDownEvent, w, cx| {
                if aligned && e.keystroke.key == "space" && v.focus.is_focused(w) {
                    v.overlay = true;
                    cx.stop_propagation();
                    cx.notify();
                }
            }))
            .on_key_up(cx.listener(|v, e: &KeyUpEvent, _, cx| {
                if e.keystroke.key == "space" && v.overlay {
                    v.overlay = false;
                    cx.notify();
                }
            }))
            .child(
                div()
                    .flex()
                    .justify_between()
                    .items_center()
                    .child(type_line("Laboratory", 22., INK))
                    .child(
                        button("Refresh shelf", DIM)
                            .on_click(cx.listener(|v, _, _, cx| v.refresh(None, cx))),
                    ),
            )
            .child(
                div()
                    .flex()
                    .flex_1()
                    .min_h_0()
                    .gap_3()
                    .when(compact, |e| e.flex_col())
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .gap_1()
                            .when(compact, |e| e.h(px(125.)))
                            .when(!compact, |e| e.w(px(220.)))
                            .child(type_line("Assay shelf", 13., VIOLET))
                            .child(shelf),
                    )
                    .child(bench),
            )
            .when(!self.notice.is_empty(), |e| {
                e.child(type_line(self.notice.clone(), 12., DIM))
            })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[::core::prelude::v1::test]
    fn aligns_reordered_cases_and_rejects_ambiguous_names() {
        let a = json!({"case":"a","summon":"s-a"});
        let b = json!({"case":"b","summon":"s-b"});
        let trial = json!({"case":"b","summon":"s-trial-b"});
        let pairs = pair_cases(&[a, b], std::slice::from_ref(&trial)).unwrap();
        assert_eq!(pairs.len(), 1);
        assert_eq!(pairs["s-b"].id, "s-trial-b");
        assert!(pair_cases(&[], &[trial.clone(), trial]).is_err());
    }
    #[::core::prelude::v1::test]
    fn returned_case_is_scored_against_expected_fields() {
        let record = json!({"ended":1,"outcome":"returned","result":{"checks":[{"pass":false}]}});
        assert_eq!(verdict(&json!({"checks.0.pass":true}), &record), "Missed");
        assert_eq!(verdict(&json!({"checks.0.pass":false}), &record), "Met");
        assert_eq!(verdict(&Value::Null, &record), "Unscored");
    }
    #[::core::prelude::v1::test]
    fn laboratory_reuses_its_window_and_accepts_a_new_baseline() {
        gpui_platform::headless().run(|cx| {
            gpui_component::init(cx);
            let path = Some(PathBuf::from("/nonexistent/lab-test.sock"));
            open(path.clone(), None, cx).unwrap();
            open(path.clone(), Some("s-receipt".into()), cx).unwrap();
            let OpenLab(handle, lab) = cx.global::<OpenLab>();
            let handle = *handle;
            assert_eq!(cx.windows().len(), 1);
            let lab = lab.upgrade().unwrap();
            assert_eq!(lab.read(cx).cases[0]["summon"], "s-receipt");
            handle.update(cx, |_, w, _| w.remove_window()).unwrap();
            open(path, None, cx).unwrap();
            assert_ne!(handle, cx.global::<OpenLab>().0);
            cx.spawn(async |cx| {
                cx.background_executor()
                    .timer(std::time::Duration::from_millis(15))
                    .await;
                cx.update(|cx| cx.quit());
            })
            .detach();
        });
    }
}
