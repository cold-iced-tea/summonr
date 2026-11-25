mod board_art;
mod client;
#[path = "../../src/ipc_frame.rs"]
mod ipc_frame;
mod laboratory;
mod orrery;
mod seal;
#[path = "../../src/window_design.rs"]
mod window_design;
mod wordmark;
use client::{Board, Monitor};
use gpui::{prelude::*, *};
use gpui_component::{
    Root,
    input::{Input, InputEvent, InputState},
};
use std::{collections::HashMap, path::PathBuf, time::Duration};

const INK: u32 = 0xdae3cc;
const DIM: u32 = 0x99ac8d;
const VIOLET: u32 = 0xb39cc5;
const APRICOT: u32 = 0xffb885;
const RULE: u32 = 0x344335;
const ROW: u32 = 0x233021;
const PAPER: u32 = 0x0c100e;

struct BoardFont(SharedString);
impl Global for BoardFont {}
fn board_font(cx: &App) -> SharedString {
    cx.try_global::<BoardFont>()
        .map(|f| f.0.clone())
        .unwrap_or_else(|| "monospace".into())
}
fn page(cx: &App) -> Div {
    div()
        .size_full()
        .flex()
        .flex_col()
        .gap_3()
        .p_5()
        .bg(rgb(PAPER))
        .text_color(rgb(INK))
        .font_family(board_font(cx))
}
fn type_line(text: impl Into<SharedString>, size: f32, color: u32) -> Div {
    div()
        .text_size(px(size))
        .text_color(rgb(color))
        .child(text.into())
}

fn artwork(text: &str, size: f32, color: u32) -> Div {
    div()
        .flex_shrink_0()
        .whitespace_nowrap()
        .text_size(px(size))
        .line_height(px(size * 1.06))
        .text_color(rgb(color))
        .child(text.to_owned())
}

fn column(text: impl Into<SharedString>, width: f32, color: u32) -> Div {
    type_line(text, 12., color)
        .w(px(width))
        .flex_shrink_0()
        .overflow_hidden()
        .whitespace_nowrap()
}
fn elapsed(ms: i64) -> String {
    if ms < 1000 {
        format!("{ms}ms")
    } else if ms < 60_000 {
        format!("{:.1}s", ms as f64 / 1000.)
    } else {
        format!("{}:{:02}", ms / 60_000, ms / 1000 % 60)
    }
}
fn outcome_ink(status: &str) -> u32 {
    match status {
        "failed" | "invalid" | "interrupted" | "exhausted" => 0xf49686,
        "running" | "pending_owner" | "held" => APRICOT,
        "returned" | "accepted" => 0xb9d799,
        _ => DIM,
    }
}
fn options(w: f32, h: f32, cx: &App) -> WindowOptions {
    WindowOptions {
        window_bounds: Some(WindowBounds::Windowed(Bounds::centered(
            None,
            size(px(w), px(h)),
            cx,
        ))),
        app_id: Some("summonr-desktop".into()),
        ..Default::default()
    }
}
struct RunWindow {
    reading: Option<usize>,
    inline: bool,
    id: String,
    data: Monitor,
    error: String,
}
impl RunWindow {
    fn new(path: PathBuf, id: String, inline: bool, cx: &mut Context<Self>) -> Self {
        let key = id.clone();
        cx.spawn(async move |this, cx| {
            if inline {
                cx.background_executor()
                    .timer(Duration::from_millis(45))
                    .await;
                if this.upgrade().is_none() {
                    return;
                }
            }
            let mut data = Monitor::default();
            loop {
                let socket = path.clone();
                let key = key.clone();
                let (fresh, err) = cx
                    .background_executor()
                    .spawn(async move {
                        let err = data.refresh(&socket, &key).err().unwrap_or_default();
                        (data, err)
                    })
                    .await;
                data = fresh;
                let delay_ms = if err.is_empty() && data.more_events {
                    0
                } else if err.is_empty() && data.state == "running" {
                    160
                } else {
                    1000
                };
                if this
                    .update(cx, |v, cx| {
                        v.error = err;
                        v.data = data.clone();
                        cx.notify();
                    })
                    .is_err()
                {
                    break;
                }
                cx.background_executor()
                    .timer(Duration::from_millis(delay_ms))
                    .await;
                if this.upgrade().is_none() {
                    break;
                }
            }
        })
        .detach();
        Self {
            id,
            inline,
            reading: None,
            data: Default::default(),
            error: String::new(),
        }
    }
}
impl RunWindow {
    fn seal_view(&self, cx: &mut Context<Self>) -> Div {
        let d = &self.data;
        let seal = &d.seal;
        let selected = self.reading.or(seal.last);
        let strokes = seal.strokes.clone();
        let mut drawing = div()
            .relative()
            .flex_shrink_0()
            .w(px(91. * seal.sites.len().div_ceil(9) as f32))
            .h(px(62.))
            .child(
                canvas(
                    |_, _, _| (),
                    move |bounds, _, win, _| {
                        let mut pen = PathBuilder::stroke(px(1.));
                        for &(a, b) in &strokes {
                            let (x, y) = seal::position(a);
                            pen.move_to(bounds.origin + point(px(x), px(y)));
                            let (x, y) = seal::position(b);
                            pen.line_to(bounds.origin + point(px(x), px(y)));
                        }
                        if let Ok(path) = pen.build() {
                            win.paint_path(path, rgb(VIOLET));
                        }
                    },
                )
                .absolute()
                .size_full(),
            );
        for (i, site) in seal.sites.iter().enumerate() {
            let held = d.questions.iter().any(|(step, _)| step == &site.name);
            if site.evidence.is_none() && !held {
                continue;
            }
            let (x, y) = seal::position(i);
            let color = if held || selected == Some(i) {
                APRICOT
            } else {
                VIOLET
            };
            drawing = drawing.child(
                div()
                    .id(("seal-site", i))
                    .absolute()
                    .left(px(x - 8.))
                    .top(px(y - 8.))
                    .size(px(16.))
                    .flex()
                    .items_center()
                    .justify_center()
                    .cursor_pointer()
                    .child(
                        div()
                            .size(px(if held { 8. } else { 5. }))
                            .rounded_full()
                            .border_1()
                            .border_color(rgb(color))
                            .bg(rgb(if held { PAPER } else { color })),
                    )
                    .on_hover(cx.listener(move |v, over, _, cx| {
                        if *over && v.reading != Some(i) {
                            v.reading = Some(i);
                            cx.notify();
                        }
                    }))
                    .on_click(cx.listener(move |v, _, _, cx| {
                        v.reading = Some(i);
                        cx.notify();
                    })),
            );
        }
        div()
            .flex()
            .flex_col()
            .child(div().id("seal-scroll").overflow_x_scroll().child(drawing))
            .when_some(selected.and_then(|i| seal.sites.get(i)), |el, site| {
                el.child(type_line(
                    format!("{}   {}", site.name, site.evidence.as_deref().unwrap_or("")),
                    12.,
                    INK,
                ))
            })
    }
}

impl Render for RunWindow {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let seal = self.seal_view(cx);
        let d = &self.data;
        let has_seal = d.seal.last.is_some()
            || d.seal
                .sites
                .iter()
                .any(|s| d.questions.iter().any(|(step, _)| step == &s.name));
        let questions = || {
            d.questions
                .iter()
                .map(|(_, q)| type_line(q.clone(), 13., APRICOT))
        };
        if self.inline {
            let rite = self.id.starts_with("rite-");
            return div()
                .id("preview-evidence")
                .size_full()
                .overflow_y_scroll()
                .p_3()
                .child(type_line("RUN PREVIEW", 12., DIM))
                .child(
                    type_line(
                        if d.name.is_empty() {
                            "Reading record…".to_owned()
                        } else {
                            d.name.clone()
                        },
                        18.,
                        INK,
                    )
                    .mt_2(),
                )
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap_2()
                        .mt_1()
                        .child(seal::id_sigil(&self.id, outcome_ink(&d.state)))
                        .child(type_line(self.id.clone(), 12., DIM)),
                )
                .child(type_line(d.state.clone(), 13., outcome_ink(&d.state)).mt_1())
                .when(!self.error.is_empty(), |el| {
                    el.child(type_line(self.error.clone(), 13., APRICOT))
                })
                .when(has_seal, |el| el.child(seal))
                .children(questions())
                .when(d.waiting, |el| {
                    el.child(type_line("? Waiting for you", 13., APRICOT))
                })
                .when_some(d.run_error.clone(), |el, error| {
                    el.child(type_line("Recorded error", 12., DIM).mt_3())
                        .child(type_line(error, 14., 0xf49686))
                })
                .when_some(d.last_step.clone().filter(|_| !has_seal), |el, step| {
                    el.child(
                        type_line(
                            if d.more_events {
                                "Step observed · reading ledger…"
                            } else {
                                "Last observed step"
                            },
                            12.,
                            DIM,
                        )
                        .mt_3(),
                    )
                    .child(type_line(step, 14., INK))
                })
                .when_some(
                    client::child_progress(&d.record["children"]),
                    |el, progress| el.child(type_line(progress, 12., DIM).mt_2()),
                )
                .when(d.run_error.is_none() && !d.result.is_empty(), |el| {
                    let summary = client::result_summary(&d.record["result"]);
                    el.child(type_line("Result", 12., DIM).mt_3())
                        .when(!summary.is_empty(), |el| {
                            el.child(
                                type_line(summary.join("  ·  "), 13., VIOLET)
                                    .bg(rgb(ROW))
                                    .p_2(),
                            )
                        })
                        .child(type_line(d.result.clone(), 13., INK))
                })
                .child(type_line(if rite { "Stages" } else { "Recent evidence" }, 12., DIM).mt_3())
                .children(
                    d.lines
                        .iter()
                        .take(if rite { d.lines.len() } else { 4 })
                        .map(|l| type_line(format!("{}  {}", l.position, l.text), 12., INK).mt_1()),
                )
                .into_any_element();
        }
        let design = &d.design;
        let accent =
            u32::from_str_radix(design.accent.trim_start_matches('#'), 16).unwrap_or(VIOLET);
        let title = if design.title.is_empty() {
            &d.name
        } else {
            &design.title
        };
        window.set_window_title(&format!(
            "{} · {}",
            if title.is_empty() { "Summonr" } else { title },
            self.id
        ));
        let side = design.layout == Some(window_design::WindowLayout::Sidebar)
            && window.viewport_size().width >= px(540.);
        let header = div()
            .flex()
            .flex_col()
            .gap_2()
            .when(side, |el| {
                el.w(px(188.))
                    .flex_shrink_0()
                    .pr_3()
                    .border_r_1()
                    .border_color(rgb(0x46414b))
            })
            .overflow_hidden()
            .when(!design.masthead.is_empty(), |el| {
                el.child(artwork(&design.masthead.join("\n"), 13., accent))
            })
            .child(div().text_xl().text_color(rgb(accent)).child(title.clone()))
            .when(!design.by.is_empty(), |el| {
                el.child(
                    div()
                        .text_sm()
                        .text_color(rgb(DIM))
                        .child(format!("by {}", design.by)),
                )
            });
        let body = div()
            .flex_1()
            .min_w_0()
            .min_h_0()
            .flex()
            .flex_col()
            .gap_3()
            .child(
                div()
                    .text_sm()
                    .child(format!("{}   {}   {}", d.name, self.id, d.state)),
            )
            .when(has_seal, |el| el.child(seal))
            .children(questions())
            .when(!self.error.is_empty(), |el| {
                el.child(format!("Last observation · {}", self.error))
            })
            .child(
                div()
                    .flex()
                    .justify_between()
                    .border_b_1()
                    .pb_1()
                    .border_color(rgb(RULE))
                    .child(type_line(
                        if self.id.starts_with("rite-") {
                            "Stage sequence"
                        } else {
                            "Run ledger"
                        },
                        12.,
                        DIM,
                    ))
                    .child(type_line(
                        if d.waiting {
                            "? Waiting for you"
                        } else if self.id.starts_with("rite-") {
                            "gate"
                        } else {
                            "elapsed"
                        },
                        12.,
                        if d.waiting { APRICOT } else { DIM },
                    )),
            )
            .child(
                div()
                    .id("trace")
                    .flex_1()
                    .overflow_y_scroll()
                    .children(d.lines.iter().map(|l| {
                        div()
                            .flex()
                            .gap_2()
                            .items_start()
                            .px_1()
                            .py_1()
                            .when(l.current, |el| el.bg(rgb(ROW)))
                            .child(type_line(if l.current { "›" } else { " " }, 14., APRICOT))
                            .child(type_line(l.position.clone(), 12., accent))
                            .child(
                                div()
                                    .flex_1()
                                    .min_w_0()
                                    .text_size(px(14.))
                                    .child(l.text.clone()),
                            )
                            .child(type_line(l.clock.clone(), 12., DIM))
                    })),
            )
            .when(!d.result.is_empty(), |el| {
                el.child(
                    div()
                        .id("result")
                        .max_h(px(100.))
                        .overflow_y_scroll()
                        .text_color(rgb(accent))
                        .child(d.result.clone()),
                )
            });
        page(cx)
            .when(side, |el| el.flex_row())
            .child(header)
            .child(body)
            .into_any_element()
    }
}

struct Console {
    filter: Entity<InputState>,
    search_open: bool,
    orrery: Option<WindowHandle<orrery::Orrery>>,
    marain: bool,
    translation: Option<Task<()>>,
    selected: Option<String>,
    preview: Option<Entity<RunWindow>>,
    directory_scroll: ScrollHandle,
    host: String,
    orbit: u32,
    motion: Option<Task<()>>,
    still: bool,
    socket: Option<PathBuf>,
    board: Board,
    connection: String,
    message: String,
    busy: bool,
    input: Entity<InputState>,
    windows: HashMap<String, WindowHandle<RunWindow>>,
}
impl Console {
    fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let input = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder("run NAME …   /   rite start FILE   /   open ID")
        });
        let filter =
            cx.new(|cx| InputState::new(window, cx).placeholder("Ritual, run ID or status"));
        cx.subscribe_in(&filter, window, |this, _, event: &InputEvent, _, cx| {
            if matches!(event, InputEvent::Change) {
                this.directory_scroll.set_offset(point(px(0.), px(0.)));
                cx.notify();
            }
        })
        .detach();
        input.read(cx).focus_handle(cx).focus(window, cx);
        cx.subscribe_in(&input, window, |this, _, ev: &InputEvent, win, cx| {
            if matches!(ev, InputEvent::PressEnter { .. }) {
                this.submit(win, cx)
            }
        })
        .detach();
        cx.spawn(async move |this, cx| {
            let mut path = None;
            loop {
                let (socket, board) = cx
                    .background_executor()
                    .spawn(async move {
                        let found = path.map(Ok).unwrap_or_else(client::socket);
                        let data = found
                            .as_ref()
                            .map_err(Clone::clone)
                            .and_then(|p| client::board(p));
                        (found.ok(), data)
                    })
                    .await;
                path = socket.clone();
                let delay_ms = if board.as_ref().is_ok_and(|b| b.active > 0) {
                    300
                } else {
                    1000
                };
                if this
                    .update(cx, |v, cx| {
                        v.socket = socket;
                        match board {
                            Ok(b) => {
                                v.board = b;
                                v.connection.clear()
                            }
                            Err(e) => v.connection = e,
                        }
                        let animate = v.board.active > 0 && v.connection.is_empty() && !v.still;
                        if animate && v.motion.is_none() {
                            v.motion = Some(cx.spawn(async |this, cx| {
                                loop {
                                    cx.background_executor()
                                        .timer(Duration::from_millis(125))
                                        .await;
                                    if this
                                        .update(cx, |v, cx| {
                                            v.orbit = v.orbit.wrapping_add(1);
                                            cx.notify()
                                        })
                                        .is_err()
                                    {
                                        break;
                                    }
                                }
                            }));
                        } else if !animate {
                            v.motion = None;
                            v.orbit = 0;
                        }
                        v.windows.retain(|_, h| h.read(cx).is_ok());
                        cx.notify()
                    })
                    .is_err()
                {
                    break;
                }
                cx.background_executor()
                    .timer(Duration::from_millis(delay_ms))
                    .await;
                if this.upgrade().is_none() {
                    break;
                }
            }
        })
        .detach();
        let still = std::env::var_os("SUMMONR_REDUCED_MOTION").is_some();
        let translation = (!still).then(|| {
            cx.spawn(async |this, cx| {
                cx.background_executor().timer(Duration::from_secs(4)).await;
                let _ = this.update(cx, |v, cx| {
                    v.marain = false;
                    cx.notify();
                });
            })
        });
        Self {
            filter,
            search_open: false,
            marain: !still,
            translation,
            orrery: None,
            selected: None,
            preview: None,
            directory_scroll: ScrollHandle::new(),
            host: std::fs::read_to_string("/proc/sys/kernel/hostname")
                .unwrap_or_else(|_| "localhost".into())
                .trim()
                .to_owned(),
            orbit: 0,
            motion: None,
            still,
            socket: None,
            board: Default::default(),
            connection: "Connecting…".into(),
            message: String::new(),
            busy: false,
            input,
            windows: HashMap::new(),
        }
    }
    fn open_orrery(&mut self, cx: &mut Context<Self>) {
        if self
            .orrery
            .is_some_and(|h| h.update(cx, |_, w, _| w.activate_window()).is_ok())
        {
            return;
        }
        let host = self.host.clone();
        let path = self.socket.clone();
        let options = options(920., 640., cx);
        match cx.open_window(options, move |_, cx| {
            cx.new(|cx| orrery::Orrery::new(host, path, cx))
        }) {
            Ok(handle) => self.orrery = Some(handle),
            Err(e) => self.message = format!("Cannot open Orrery: {e}"),
        }
        cx.notify();
    }
    fn open(&mut self, id: String, focus: bool, cx: &mut Context<Self>) {
        if let Some(handle) = self.windows.get(&id)
            && handle
                .update(cx, |_, w, _| {
                    if focus {
                        w.activate_window();
                    }
                })
                .is_ok()
        {
            return;
        }
        let Some(path) = self.socket.clone() else {
            self.message = "Waiting for core socket".into();
            cx.notify();
            return;
        };
        let key = id.clone();
        let mut options = options(570., 480., cx);
        options.focus = focus;
        match cx.open_window(options, move |_, cx| {
            cx.new(|cx| RunWindow::new(path, key, false, cx))
        }) {
            Ok(handle) => {
                self.windows.insert(id, handle);
            }
            Err(e) => self.message = format!("Cannot open window: {e}"),
        }
        cx.notify();
    }
    fn submit(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let raw = self.input.read(cx).value().to_string();
        let Some(args) = shlex::split(&raw) else {
            self.message = "Unclosed quote".into();
            cx.notify();
            return;
        };
        if args.is_empty() {
            return;
        }
        if args.as_slice() == ["laboratory"] {
            self.input.update(cx, |i, cx| i.set_value("", window, cx));
            if let Err(e) = laboratory::open(self.socket.clone(), None, cx) {
                self.message = e;
                cx.notify();
            }
            return;
        }
        if args.as_slice() == ["orrery"] {
            self.input.update(cx, |i, cx| i.set_value("", window, cx));
            self.open_orrery(cx);
            return;
        }
        if args[0] == "open" && args.len() == 2 {
            self.open(args[1].clone(), true, cx);
            return;
        }
        if self.busy {
            return;
        }
        if !(args[0] == "run" || (args[0] == "rite" && args.get(1).is_some_and(|s| s == "start"))) {
            self.message = "Use run NAME, rite start FILE, or open ID.".into();
            cx.notify();
            return;
        }
        self.input.update(cx, |i, cx| i.set_value("", window, cx));
        self.busy = true;
        self.message = format!("Submitted · {raw}");
        cx.spawn(async move |this, cx| {
            let result = cx
                .background_executor()
                .spawn(async move { client::command(&args) })
                .await;
            let _ = this.update(cx, |v, cx| {
                v.busy = false;
                match result {
                    Ok(r) => {
                        if let Some(id) = r["summon"].as_str().or(r["id"].as_str()) {
                            v.message = format!("Opened {id}");
                            v.open(id.into(), false, cx);
                        } else {
                            v.message = r.to_string()
                        }
                    }
                    Err(e) => v.message = e,
                }
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }
}
impl Render for Console {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        window.set_window_title("Summonr · Console");
        let (mark, state) = if self.connection == "Connecting…" {
            ('·', "CONNECTING")
        } else if !self.connection.is_empty() {
            ('×', "OFFLINE")
        } else if self.board.held > 0 {
            ('?', "WAITING FOR YOU")
        } else if self.board.active > 0 {
            ('◆', "WORKING")
        } else {
            ('◇', "IDLE")
        };
        let width = f32::from(window.viewport_size().width);
        let wide = width >= 620.;
        let split = width >= 1000.;
        let items = self.board.filtered(&self.filter.read(cx).value());
        if !items
            .iter()
            .any(|item| Some(&item.id) == self.selected.as_ref())
        {
            self.selected = items.first().map(|item| item.id.clone());
        }
        if split {
            if let (Some(id), Some(path)) = (&self.selected, &self.socket) {
                if !self.preview.as_ref().is_some_and(|p| p.read(cx).id == *id) {
                    self.preview =
                        Some(cx.new(|cx| RunWindow::new(path.clone(), id.clone(), true, cx)));
                }
            } else {
                self.preview = None
            }
        } else {
            self.preview = None
        }

        let sweep = (self.motion.is_some())
            .then_some((self.orbit % 40) as f32 / 39. * (wordmark::W + 60) as f32 - 30.);

        let directory = div()
            .flex()
            .flex_col()
            .flex_1()
            .min_w_0()
            .min_h_0()
            .child(
                div()
                    .flex()
                    .px_2()
                    .gap_2()
                    .bg(rgb(0x182119))
                    .items_center()
                    .h(px(21.))
                    .child(column("KEY", 34., DIM))
                    .when(wide, |el| el.child(column("ADDRESS", 92., DIM)))
                    .child(type_line("RITUAL / RITE", 11., DIM).flex_1())
                    .when(wide, |el| {
                        el.child(column("TIME", 64., DIM))
                            .child(column("CALLS", 48., DIM))
                    })
                    .child(column("STATE", 86., DIM)),
            )
            .child(
                div()
                    .id("runs")
                    .track_scroll(&self.directory_scroll)
                    .overflow_y_scroll()
                    .flex_1()
                    .min_h_0()
                    .children(items.iter().enumerate().map(|(index, item)| {
                        let id = item.id.clone();
                        let open = self.windows.contains_key(&id);
                        let hover_id = id.clone();
                        let selected =
                            (split || self.search_open) && self.selected.as_ref() == Some(&id);
                        div()
                            .id(SharedString::from(id.clone()))
                            .px_2()
                            .h(px(25.))
                            .flex()
                            .gap_2()
                            .items_center()
                            .when(selected, |el| el.bg(rgb(ROW)))
                            .when(!selected && index % 2 == 1, |el| el.bg(rgb(0x111712)))
                            .border_l_2()
                            .border_color(rgb(if selected {
                                APRICOT
                            } else if open {
                                VIOLET
                            } else {
                                RULE
                            }))
                            .cursor_pointer()
                            .hover(|el| el.bg(rgb(0x263525)).border_color(rgb(0xc8ef87)))
                            .child(column(
                                if index < 9 {
                                    format!("[{}]", index + 1)
                                } else {
                                    String::new()
                                },
                                32.,
                                APRICOT,
                            ))
                            .when(wide, |el| {
                                el.child(column(id.chars().take(10).collect::<String>(), 92., DIM))
                            })
                            .child(
                                type_line(
                                    if item.held {
                                        format!("◇  {}", item.name)
                                    } else {
                                        item.name.clone()
                                    },
                                    14.,
                                    if item.held { APRICOT } else { INK },
                                )
                                .flex_1()
                                .min_w_0()
                                .overflow_hidden()
                                .whitespace_nowrap(),
                            )
                            .when(wide, |el| {
                                el.child(column(
                                    item.duration_ms.map(elapsed).unwrap_or_else(|| "—".into()),
                                    64.,
                                    DIM,
                                ))
                                .child(column(
                                    item.requests
                                        .map(|v| v.to_string())
                                        .unwrap_or_else(|| "—".into()),
                                    48.,
                                    DIM,
                                ))
                            })
                            .child(column(item.status.clone(), 86., outcome_ink(&item.status)))
                            .on_hover(cx.listener(move |v, over: &bool, _, cx| {
                                if split && *over && v.selected.as_ref() != Some(&hover_id) {
                                    v.selected = Some(hover_id.clone());
                                    cx.notify();
                                }
                            }))
                            .on_click(cx.listener(move |v, _, _, cx| v.open(id.clone(), true, cx)))
                    })),
            );
        page(cx)
            .capture_key_down(cx.listener(move |v, event: &KeyDownEvent, w, cx| {
                let key = &event.keystroke;
                let plain = key.modifiers == Modifiers::default();
                let command_empty = v.input.read(cx).value().is_empty();
                if plain && key.key == "/" && !v.search_open && command_empty {
                    v.search_open = true;
                    v.filter.read(cx).focus_handle(cx).focus(w, cx);
                    cx.stop_propagation();
                    cx.notify();
                    return;
                }
                if plain && key.key == "escape" && v.search_open {
                    v.search_open = false;
                    v.filter.update(cx, |input, cx| input.set_value("", w, cx));
                    v.input.read(cx).focus_handle(cx).focus(w, cx);
                    v.directory_scroll.set_offset(point(px(0.), px(0.)));
                    cx.stop_propagation();
                    cx.notify();
                    return;
                }
                let items = v.board.filtered(&v.filter.read(cx).value());
                if plain
                    && !items.is_empty()
                    && (v.search_open && v.filter.read(cx).focus_handle(cx).is_focused(w)
                        || split && command_empty)
                {
                    let index = items
                        .iter()
                        .position(|item| Some(&item.id) == v.selected.as_ref())
                        .unwrap_or(0);
                    let next = match key.key.as_str() {
                        "up" => Some(index.saturating_sub(1)),
                        "down" => Some((index + 1).min(items.len() - 1)),
                        "enter" => {
                            v.open(items[index].id.clone(), true, cx);
                            cx.stop_propagation();
                            return;
                        }
                        _ => None,
                    };
                    if let Some(i) = next {
                        v.selected = Some(items[i].id.clone());
                        v.directory_scroll.scroll_to_item(i);
                        cx.stop_propagation();
                        cx.notify();
                        return;
                    }
                }
                if key.modifiers.alt
                    && !key.modifiers.control
                    && !key.modifiers.platform
                    && !key.modifiers.shift
                    && let Some(item) = key
                        .key
                        .parse::<usize>()
                        .ok()
                        .filter(|n| (1..=9).contains(n))
                        .and_then(|n| items.get(n - 1))
                {
                    let id = item.id.clone();
                    cx.stop_propagation();
                    v.open(id, true, cx);
                }
            }))
            .child(
                div()
                    .flex()
                    .items_center()
                    .justify_between()
                    .border_b_1()
                    .pb_2()
                    .border_color(rgb(RULE))
                    .child(type_line(
                        format!("●  {}  /  LOCAL BOARD", self.host),
                        12.,
                        DIM,
                    ))
                    .when(wide, |el| {
                        let glyph_ink = if mark == '?' { APRICOT } else { VIOLET };
                        el.child(
                            div()
                                .id("state-marain")
                                .cursor_pointer()
                                .h(px(24.))
                                .flex()
                                .items_center()
                                .justify_center()
                                .child(if self.marain {
                                    seal::state_glyphs(state, glyph_ink).into_any_element()
                                } else {
                                    type_line(seal::state_english(state), 12., glyph_ink)
                                        .into_any_element()
                                })
                                .on_click(cx.listener(|v, _, _, cx| {
                                    v.translation = None;
                                    v.marain = !v.marain;
                                    cx.notify();
                                })),
                        )
                    })
                    .child(
                        div()
                            .id("open-laboratory")
                            .cursor_pointer()
                            .child(type_line("⚗ Laboratory", 13., VIOLET))
                            .on_click(cx.listener(|v, _, _, cx| {
                                if let Err(e) = laboratory::open(v.socket.clone(), None, cx) {
                                    v.message = e;
                                    cx.notify();
                                }
                            })),
                    )
                    .child(
                        div()
                            .id("open-orrery")
                            .cursor_pointer()
                            .child(type_line("◎  Orrery", 13., VIOLET))
                            .on_click(cx.listener(|v, _, _, cx| v.open_orrery(cx))),
                    )
                    .child(type_line(
                        state,
                        12.,
                        if mark == '?' { APRICOT } else { VIOLET },
                    )),
            )
            .child(
                div()
                    .flex()
                    .items_center()
                    .justify_between()
                    .relative()
                    .h(px(if wide { 112. } else { 86. }))
                    .bg(rgb(0x101610))
                    .overflow_hidden()
                    .child(
                        canvas(
                            |_, _, _| (),
                            move |bounds, _, win, _| {
                                let w = f32::from(bounds.size.width);
                                let h = f32::from(bounds.size.height);
                                let mut paint =
                                    |x: f32, y: f32, width: f32, height: f32, color: u32| {
                                        win.paint_quad(fill(
                                            Bounds::new(
                                                bounds.origin + point(px(x), px(y)),
                                                size(px(width), px(height)),
                                            ),
                                            rgb(color),
                                        ));
                                    };
                                // A sparse registration grid grows denser beneath the ring.
                                for y in (8..h as usize).step_by(8) {
                                    for x in (8..w as usize).step_by(8) {
                                        if x as f32 > w * 0.65 || (x + y) % 24 == 0 {
                                            paint(x as f32, y as f32, 1., 1., 0x364332);
                                        }
                                    }
                                }
                                let available = w - if wide { 180. } else { 20. };
                                let scale = (available / wordmark::W as f32)
                                    .min((h - 22.) / wordmark::H as f32)
                                    .max(0.1);
                                let top = (h - scale * wordmark::H as f32) / 2.;
                                for strip in wordmark::strips() {
                                    paint(
                                        10. + strip.x as f32 * scale,
                                        top + strip.y as f32 * scale,
                                        strip.width as f32 * scale,
                                        scale + 0.05,
                                        strip.ink,
                                    );
                                }
                                // A moving reflection only lights the letter faces during active work.
                                if let Some(sweep) = sweep {
                                    for strip in
                                        wordmark::strips().iter().filter(|s| s.ink >= 0x8bae63)
                                    {
                                        let beam = sweep - strip.y as f32 * 0.5;
                                        let left = beam.max(strip.x as f32);
                                        let right = (beam + 3.).min((strip.x + strip.width) as f32);
                                        if right > left {
                                            paint(
                                                10. + left * scale,
                                                top + strip.y as f32 * scale,
                                                (right - left) * scale,
                                                scale,
                                                0xedffd6,
                                            );
                                        }
                                    }
                                }
                                for (x, y) in
                                    [(0., 0.), (w - 12., 0.), (0., h - 1.), (w - 12., h - 1.)]
                                {
                                    paint(x, y, 12., 1., 0x85976b);
                                }
                            },
                        )
                        .absolute()
                        .size_full(),
                    )
                    .when(wide, |el| {
                        el.child(div().absolute().right_2().top_2().child(artwork(
                            &board_art::core(self.orbit, mark),
                            8.5,
                            VIOLET,
                        )))
                    }),
            )
            .child(
                div()
                    .flex()
                    .items_center()
                    .justify_between()
                    .border_b_1()
                    .pb_2()
                    .border_color(rgb(RULE))
                    .child(type_line(
                        if !self.board.items.is_empty() && wide {
                            if split {
                                "RUN DIRECTORY   / search   ↑↓ preview   Enter open"
                            } else {
                                "RUN DIRECTORY   / search   [alt + 1–9]"
                            }
                        } else {
                            "RUN DIRECTORY   / search"
                        },
                        13.,
                        INK,
                    ))
                    .child(type_line(
                        format!(
                            "{} / {}   ·   {} active   {} held",
                            items.len(),
                            self.board.items.len(),
                            self.board.active,
                            self.board.held
                        ),
                        12.,
                        DIM,
                    )),
            )
            .when(self.search_open, |el| {
                el.child(
                    div().border_b_1().border_color(rgb(APRICOT)).child(
                        Input::new(&self.filter)
                            .appearance(false)
                            .bordered(false)
                            .focus_bordered(false),
                    ),
                )
            })
            .when(!self.board.items.is_empty() && items.is_empty(), |el| {
                el.child(type_line("No matching runs.", 13., DIM))
            })
            .when(!self.connection.is_empty(), |el| {
                el.child(type_line(self.connection.clone(), 12., DIM))
            })
            .when(
                self.connection.is_empty() && self.board.items.is_empty(),
                |el| el.child(type_line("No runs yet.", 13., DIM)),
            )
            .child(
                div()
                    .flex()
                    .flex_1()
                    .min_h_0()
                    .gap_3()
                    .child(directory)
                    .when(split, |el| {
                        el.child(
                            div()
                                .w(px((width * 0.36).min(550.)))
                                .flex_shrink_0()
                                .border_l_1()
                                .border_color(rgb(RULE))
                                .when_some(self.preview.clone(), |el, p| el.child(p))
                                .when(self.preview.is_none(), |el| {
                                    el.child(type_line("No run selected", 13., DIM).p_3())
                                }),
                        )
                    }),
            )
            .when(
                wide && items.iter().any(|i| i.duration_ms.is_some()),
                |el| {
                    let mut ended: Vec<_> = items
                        .iter()
                        .copied()
                        .filter(|i| i.duration_ms.is_some())
                        .collect();
                    ended.sort_by_key(|i| i.started);
                    let ended = &ended[ended.len().saturating_sub(35)..];
                    let peak = ended
                        .iter()
                        .filter_map(|i| i.duration_ms)
                        .max()
                        .unwrap_or(0);
                    el.child(
                        div()
                            .flex()
                            .gap_3()
                            .items_center()
                            .border_t_1()
                            .pt_1()
                            .border_color(rgb(RULE))
                            .child(type_line("RUN TIMES", 11., DIM))
                            .child(
                                div()
                                    .flex_1()
                                    .flex()
                                    .gap(px(2.))
                                    .items_end()
                                    .h(px(27.))
                                    .children(ended.iter().map(|item| {
                                        let ms = item.duration_ms.unwrap_or(0);
                                        let height = 2. + 24. * ms as f32 / peak.max(1) as f32;
                                        let id = item.id.clone();
                                        div()
                                            .id(SharedString::from(format!("timing-{id}")))
                                            .flex_1()
                                            .h_full()
                                            .flex()
                                            .flex_col()
                                            .justify_end()
                                            .cursor_pointer()
                                            .hover(|s| s.bg(rgb(ROW)))
                                            .child(div().h(px(height)).w_full().bg(rgb(
                                                if split && self.selected.as_ref() == Some(&id) {
                                                    APRICOT
                                                } else {
                                                    outcome_ink(&item.status)
                                                },
                                            )))
                                            .on_hover(cx.listener({
                                                let id = id.clone();
                                                move |v, over: &bool, _, cx| {
                                                    if split
                                                        && *over
                                                        && v.selected.as_ref() != Some(&id)
                                                    {
                                                        v.selected = Some(id.clone());
                                                        if let Some(i) = v
                                                            .board
                                                            .filtered(&v.filter.read(cx).value())
                                                            .iter()
                                                            .position(|r| r.id == id)
                                                        {
                                                            v.directory_scroll.scroll_to_item(i);
                                                        }
                                                        cx.notify();
                                                    }
                                                }
                                            }))
                                            .on_click(cx.listener(move |v, _, _, cx| {
                                                v.open(id.clone(), true, cx)
                                            }))
                                    })),
                            )
                            .child(type_line(format!("peak {}", elapsed(peak)), 11., DIM)),
                    )
                },
            )
            .when(!self.message.is_empty(), |el| {
                el.child(
                    div()
                        .text_sm()
                        .text_color(rgb(DIM))
                        .child(self.message.clone()),
                )
            })
            .child(
                div()
                    .border_t_1()
                    .border_color(rgb(RULE))
                    .pt_2()
                    .flex()
                    .items_center()
                    .gap_2()
                    .child(type_line("❯", 19., APRICOT))
                    .child(
                        div().flex_1().min_w_0().child(
                            Input::new(&self.input)
                                .appearance(false)
                                .bordered(false)
                                .focus_bordered(false),
                        ),
                    ),
            )
    }
}
fn main() {
    gpui_platform::application().run(|cx| {
        gpui_component::init(cx);
        let installed = cx.text_system().all_font_names();
        let mono = [
            "JetBrainsMono Nerd Font",
            "JetBrains Mono",
            "IBM Plex Mono",
            "DejaVu Sans Mono",
            "Liberation Mono",
        ]
        .into_iter()
        .find(|name| installed.iter().any(|f| f == name))
        .unwrap_or("monospace");
        cx.set_global(BoardFont(mono.into()));
        cx.on_window_closed(|cx, _| {
            if cx.windows().is_empty() {
                cx.quit()
            }
        })
        .detach();
        cx.open_window(options(790., 520., cx), |w, cx| {
            let console = cx.new(|cx| Console::new(w, cx));
            cx.new(|cx| Root::new(console, w, cx))
        })
        .expect("open Summonr console");
        cx.activate(true);
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[::core::prelude::v1::test]
    fn reopening_a_run_reuses_then_recreates_its_window() {
        gpui_platform::headless().run(|cx| {
            gpui_component::init(cx);
            let console = cx
                .open_window(options(620., 460., cx), |w, cx| {
                    cx.new(|cx| Console::new(w, cx))
                })
                .unwrap();
            let first = console
                .update(cx, |v, window, cx| {
                    v.socket = Some(PathBuf::from("/nonexistent/summonr-test.sock"));
                    v.input.update(cx, |input, cx| {
                        input.set_value("unfinished command", window, cx)
                    });
                    v.open("s-example".into(), true, cx);
                    v.open("s-example".into(), true, cx);
                    v.windows["s-example"]
                })
                .unwrap();
            assert_eq!(cx.windows().len(), 2);
            first
                .update(cx, |_, window, _| window.remove_window())
                .unwrap();
            assert_eq!(cx.windows().len(), 1);
            console
                .update(cx, |v, _, cx| {
                    assert_eq!(v.input.read(cx).value().as_ref(), "unfinished command");
                    v.open("s-example".into(), true, cx);
                    assert_ne!(first, v.windows["s-example"]);
                })
                .unwrap();
            assert_eq!(cx.windows().len(), 2);
            let orrery = console
                .update(cx, |v, _, cx| {
                    v.open_orrery(cx);
                    v.open_orrery(cx);
                    v.orrery.unwrap()
                })
                .unwrap();
            assert_eq!(cx.windows().len(), 3);
            orrery.update(cx, |_, w, _| w.remove_window()).unwrap();
            console
                .update(cx, |v, _, cx| {
                    assert_eq!(v.input.read(cx).value().as_ref(), "unfinished command");
                    v.open_orrery(cx);
                    assert_ne!(v.orrery.unwrap(), orrery);
                })
                .unwrap();
            cx.spawn(async |cx| {
                cx.background_executor()
                    .timer(Duration::from_millis(10))
                    .await;
                cx.update(|cx| cx.quit())
            })
            .detach();
        });
    }
}
