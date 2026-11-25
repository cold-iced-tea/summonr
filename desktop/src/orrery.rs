use super::{
    APRICOT, DIM, INK, RULE, RunWindow, VIOLET,
    client::{self, OrbitRun},
    outcome_ink, page, type_line,
};
use chrono::{Days, Local, NaiveDate, TimeZone, Timelike};
use gpui::{prelude::*, *};
use std::{
    cell::Cell,
    f32::consts::{PI, TAU},
    path::PathBuf,
    rc::Rc,
    time::Duration,
};

fn night_now() -> NaiveDate {
    let now = Local::now();
    if now.hour() < 12 {
        now.date_naive().pred_opt().unwrap()
    } else {
        now.date_naive()
    }
}
fn noon(date: NaiveDate) -> i64 {
    Local
        .from_local_datetime(&date.and_hms_opt(12, 0, 0).unwrap())
        .earliest()
        .unwrap()
        .timestamp_millis()
}
fn clock(t: i64) -> String {
    Local
        .timestamp_millis_opt(t)
        .single()
        .map(|d| d.format("%H:%M:%S").to_string())
        .unwrap_or_default()
}
fn overlap(start: i64, end: Option<i64>, at: i64) -> bool {
    start <= at && end.is_none_or(|e| at < e)
}
fn angle(f: f32) -> f32 {
    PI / 2. + TAU * f
}
fn point_at(center: Point<Pixels>, r: f32, f: f32) -> Point<Pixels> {
    let a = angle(f);
    center + point(px(r * a.cos()), px(r * a.sin()))
}
fn line(win: &mut Window, points: impl IntoIterator<Item = Point<Pixels>>, width: f32, color: u32) {
    let mut points = points.into_iter();
    let Some(start) = points.next() else { return };
    let mut pen = PathBuilder::stroke(px(width));
    pen.move_to(start);
    for p in points {
        pen.line_to(p);
    }
    if let Ok(path) = pen.build() {
        win.paint_path(path, rgb(color))
    }
}
fn arc(win: &mut Window, c: Point<Pixels>, r: f32, from: f32, to: f32, width: f32, color: u32) {
    let segments = ((to - from) * (TAU * r / 4.).clamp(12., 250.))
        .ceil()
        .max(1.) as usize;
    line(
        win,
        (0..=segments).map(|i| point_at(c, r, from + (to - from) * i as f32 / segments as f32)),
        width,
        color,
    )
}
#[derive(Clone)]
struct Star {
    id: String,
    track: usize,
    from: f32,
    to: f32,
    ink: u32,
    held: bool,
}
#[derive(Clone)]
struct Dial {
    hours: Vec<(f32, String)>,
    stars: Vec<Star>,
    tracks: usize,
    selected: Option<String>,
    hand: f32,
}
impl Dial {
    fn radius(&self, track: usize, r: f32) -> f32 {
        r * (0.23 + 0.67 * (track + 1) as f32 / self.tracks.max(1) as f32)
    }
    fn draw(&self, b: Bounds<Pixels>, win: &mut Window, cx: &mut App) {
        let c = b.center();
        let r = f32::from(b.size.width.min(b.size.height)) * 0.42;
        arc(win, c, r, 0., 1., 1., 0x69715b);
        for i in 0..96 {
            let f = i as f32 / 96.;
            let major = i % 4 == 0;
            line(
                win,
                [
                    point_at(c, r + 1., f),
                    point_at(c, r + if major { 7. } else { 3. }, f),
                ],
                1.,
                if major { DIM } else { RULE },
            );
        }
        for (f, label) in &self.hours {
            let p = point_at(c, r + 17., *f);
            let text = win.text_system().shape_line(
                label.clone().into(),
                px(10.),
                &[TextRun {
                    len: label.len(),
                    font: font(super::board_font(cx)),
                    color: rgb(DIM).into(),
                    background_color: None,
                    underline: None,
                    strikethrough: None,
                }],
                None,
            );
            let _ = text.paint(
                p - point(text.width * 0.5, px(6.)),
                px(12.),
                TextAlign::Left,
                None,
                win,
                cx,
            );
        }
        for i in 0..self.tracks {
            arc(win, c, self.radius(i, r), 0., 1., 1., RULE);
        }
        for s in &self.stars {
            let radius = self.radius(s.track, r);
            arc(win, c, radius, s.from, s.to, 2., s.ink);
            let p = point_at(c, radius, s.from);
            let size = if Some(&s.id) == self.selected.as_ref() {
                5.
            } else {
                3.
            };
            arc(
                win,
                p,
                size,
                0.,
                1.,
                1.4,
                if s.held { APRICOT } else { s.ink },
            );
            if !s.held {
                win.paint_quad(fill(
                    Bounds::new(p - point(px(1.), px(1.)), gpui::size(px(2.), px(2.))),
                    rgb(s.ink),
                ));
            }
        }
        line(
            win,
            [point_at(c, 5., self.hand), point_at(c, r + 8., self.hand)],
            1.2,
            0xc69e68,
        );
        arc(win, c, 5., 0., 1., 1.3, APRICOT);
    }
}
pub struct Orrery {
    host: String,
    path: Option<PathBuf>,
    runs: Vec<OrbitRun>,
    error: String,
    night: Option<NaiveDate>,
    cursor: Option<i64>,
    filter: Option<String>,
    selected: Option<String>,
    record: Option<Entity<RunWindow>>,
    bounds: Rc<Cell<Option<Bounds<Pixels>>>>,
}
impl Orrery {
    pub fn new(host: String, path: Option<PathBuf>, cx: &mut Context<Self>) -> Self {
        let mut source = path.clone();
        cx.spawn(async move |this, cx| {
            loop {
                if this.upgrade().is_none() {
                    break;
                }
                let (found, result) = cx
                    .background_executor()
                    .spawn(async move {
                        let found = source.map(Ok).unwrap_or_else(client::socket);
                        let result = found
                            .as_ref()
                            .map_err(Clone::clone)
                            .and_then(|p| client::orbit_history(p));
                        (found.ok(), result)
                    })
                    .await;
                source = found.clone();
                if this
                    .update(cx, |v, cx| {
                        v.path = found;
                        match result {
                            Ok(r) => {
                                v.runs = r;
                                v.error.clear()
                            }
                            Err(e) => v.error = e,
                        }
                        cx.notify()
                    })
                    .is_err()
                {
                    break;
                }
                cx.background_executor().timer(Duration::from_secs(5)).await;
            }
        })
        .detach();
        Self {
            host,
            path,
            runs: Vec::new(),
            error: "Reading the board…".into(),
            night: None,
            cursor: None,
            filter: None,
            selected: None,
            record: None,
            bounds: Default::default(),
        }
    }
    fn select(&mut self, id: String, cx: &mut Context<Self>) {
        if self.selected.as_ref() == Some(&id) {
            return;
        }
        if let Some(path) = self.path.clone() {
            self.record = Some(cx.new(|cx| RunWindow::new(path, id.clone(), true, cx)));
        }
        self.selected = Some(id);
        cx.notify();
    }
    fn change_night(&mut self, date: Option<NaiveDate>, cx: &mut Context<Self>) {
        self.night = date;
        self.cursor = None;
        self.selected = None;
        self.record = None;
        self.filter = None;
        cx.notify();
    }
    fn touch(
        &mut self,
        p: Point<Pixels>,
        dial: &Dial,
        start: i64,
        end: i64,
        pick: bool,
        cx: &mut Context<Self>,
    ) {
        let Some(b) = self.bounds.get() else { return };
        let c = b.center();
        let r = f32::from(b.size.width.min(b.size.height)) * 0.42;
        if pick
            && let Some((_, star)) = dial
                .stars
                .iter()
                .map(|s| {
                    let pt = point_at(c, dial.radius(s.track, r), s.from);
                    ((f32::from(pt.x - p.x)).hypot(f32::from(pt.y - p.y)), s)
                })
                .filter(|(dist, _)| *dist <= 8.)
                .min_by(|a, b| a.0.total_cmp(&b.0))
        {
            self.select(star.id.clone(), cx);
            return;
        }
        let f = (f32::from(p.y - c.y).atan2(f32::from(p.x - c.x)) - PI / 2.).rem_euclid(TAU) / TAU;
        self.cursor = Some(
            (start + ((end - start) as f64 * f as f64) as i64).min(Local::now().timestamp_millis()),
        );
        cx.notify();
    }
}
impl Render for Orrery {
    fn render(&mut self, w: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        w.set_window_title("Summonr · Orrery");
        let date = self.night.unwrap_or_else(night_now);
        let start = noon(date);
        let end = noon(date.succ_opt().unwrap());
        let now = Local::now().timestamp_millis();
        let at = self.cursor.unwrap_or(now.min(end - 1));
        let fraction =
            |t: i64| ((t.clamp(start, end) - start) as f64 / (end - start) as f64) as f32;
        let visible: Vec<_> = self
            .runs
            .iter()
            .filter(|r| {
                r.start < end
                    && (r.end.unwrap_or(now) >= start
                        || r.petitions
                            .iter()
                            .any(|(s, e, _)| *s < end && e.unwrap_or(now) >= start))
            })
            .collect();
        let ended = visible
            .iter()
            .filter(|run| {
                run.end
                    .is_some_and(|finished| (start..end).contains(&finished))
            })
            .count();
        let active = visible
            .iter()
            .filter(|run| overlap(run.start, run.end, now.min(end - 1)))
            .count();
        let selection = visible
            .iter()
            .find(|run| Some(&run.id) == self.selected.as_ref())
            .map(|run| {
                let duration =
                    super::elapsed(run.end.unwrap_or(now).saturating_sub(run.start).max(0));
                format!("{}  ·  {}  ·  {}", run.ritual, duration, run.state)
            });
        let mut names: Vec<_> = visible.iter().map(|r| r.ritual.as_str()).collect();
        names.sort_unstable();
        names.dedup();
        let muted = |r: &OrbitRun| self.filter.as_ref().is_some_and(|s| s != &r.ritual);
        let held = |r: &OrbitRun| r.petitions.iter().any(|(s, e, _)| overlap(*s, *e, at));
        let hours = (start..end)
            .step_by(3600000)
            .filter_map(|ms| {
                let t = Local.timestamp_millis_opt(ms).single()?;
                (t.hour() % 3 == 0).then(|| (fraction(ms), t.format("%H").to_string()))
            })
            .collect();
        let dial = Rc::new(Dial {
            hours,
            tracks: names.len(),
            hand: fraction(at),
            selected: self.selected.clone(),
            stars: visible
                .iter()
                .map(|r| Star {
                    id: r.id.clone(),
                    track: names.binary_search(&r.ritual.as_str()).unwrap(),
                    from: fraction(r.start),
                    to: fraction(r.end.unwrap_or(now)),
                    ink: if muted(r) || r.start > at {
                        0x50534d
                    } else if overlap(r.start, r.end, at) {
                        APRICOT
                    } else {
                        outcome_ink(&r.state)
                    },
                    held: held(r),
                })
                .collect(),
        });
        let bounds = self.bounds.clone();
        let painter = dial.clone();
        let picker = dial.clone();
        let chart = div()
            .flex_1()
            .min_h(px(180.))
            .min_w_0()
            .relative()
            .id("orrery-dial")
            .cursor_crosshair()
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(move |v, e: &MouseDownEvent, _, cx| {
                    v.touch(e.position, &picker, start, end, true, cx)
                }),
            )
            .on_mouse_move(cx.listener(move |v, e: &MouseMoveEvent, _, cx| {
                if e.pressed_button == Some(MouseButton::Left) {
                    v.touch(e.position, &dial, start, end, false, cx)
                }
            }))
            .child(
                canvas(
                    |_, _, _| (),
                    move |b, _, w, cx| {
                        bounds.set(Some(b));
                        painter.draw(b, w, cx)
                    },
                )
                .absolute()
                .size_full(),
            );
        let mut list = div().flex().flex_col().gap_1();
        for r in visible.iter().rev().filter(|r| !muted(r)) {
            let id = r.id.clone();
            let status = if held(r) {
                "◇ waiting"
            } else if overlap(r.start, r.end, at) {
                "underway"
            } else if r.start > at {
                "later"
            } else {
                &r.state
            };
            list = list.child(
                div()
                    .id(SharedString::from(id.clone()))
                    .cursor_pointer()
                    .hover(|e| e.bg(rgb(RULE)))
                    .child(type_line(
                        format!("{}  {}  ·  {}", clock(r.start), r.ritual, status),
                        12.,
                        if self.selected.as_ref() == Some(&id) {
                            APRICOT
                        } else {
                            INK
                        },
                    ))
                    .on_click(cx.listener(move |v, _, _, cx| v.select(id.clone(), cx))),
            );
            for (s, e, op) in &r.petitions {
                if overlap(*s, *e, at) {
                    list = list.child(type_line(format!("  ◇ {op}"), 12., APRICOT));
                }
            }
        }
        let mut tracks = div().flex().flex_wrap().gap_2();
        for (i, name) in names.iter().enumerate() {
            let key = name.to_string();
            tracks = tracks.child(
                div()
                    .id(("orbit-track", i))
                    .cursor_pointer()
                    .child(type_line(
                        format!("{} {}", i + 1, name),
                        12.,
                        if self.filter.as_ref() == Some(&key) {
                            APRICOT
                        } else {
                            VIOLET
                        },
                    ))
                    .on_click(cx.listener(move |v, _, _, cx| {
                        v.filter = if v.filter.as_ref() == Some(&key) {
                            None
                        } else {
                            Some(key.clone())
                        };
                        cx.notify();
                    })),
            );
        }
        let mut nav = div().flex().gap_3();
        for (label, target) in [
            ("‹ Earlier", date.checked_sub_days(Days::new(1))),
            ("Later ›", date.succ_opt()),
            ("Now", None),
        ] {
            if target.is_some_and(|d| d > night_now()) {
                continue;
            }
            nav = nav.child(
                div()
                    .id(label)
                    .cursor_pointer()
                    .child(type_line(label, 13., APRICOT))
                    .on_click(cx.listener(move |v, _, _, cx| v.change_night(target, cx))),
            );
        }
        let compact = w.viewport_size().width < px(670.);
        page(cx)
            .child(
                div()
                    .flex()
                    .justify_between()
                    .child(type_line(format!("{}  /  Orrery", self.host), 20., INK))
                    .child(nav),
            )
            .child(type_line(
                format!(
                    "{}   ·   noon to noon   ·   {active} active at cutoff   ·   {ended} ended",
                    date.format("%a %d %b %Y")
                ),
                13.,
                DIM,
            ))
            .when_some(selection, |el, selected| {
                el.child(type_line(selected, 13., APRICOT))
            })
            .when(!self.error.is_empty(), |e| {
                e.child(type_line(self.error.clone(), 13., APRICOT))
            })
            .child(
                div()
                    .flex()
                    .when(compact, |e| e.flex_col())
                    .flex_1()
                    .min_h_0()
                    .gap_3()
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .flex_1()
                            .min_w_0()
                            .child(chart)
                            .child(type_line(
                                format!("{}   ·   drag dial to rewind", clock(at)),
                                14.,
                                APRICOT,
                            ))
                            .child(tracks),
                    )
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .gap_2()
                            .min_h_0()
                            .when(compact, |e| e.h(relative(0.42)))
                            .when(!compact, |e| e.w(relative(0.38)))
                            .child(type_line(
                                format!(
                                    "{} top-level runs",
                                    visible.iter().filter(|r| !muted(r)).count()
                                ),
                                13.,
                                DIM,
                            ))
                            .child(
                                div()
                                    .id("orrery-records")
                                    .min_h_0()
                                    .flex_1()
                                    .overflow_y_scroll()
                                    .child(list),
                            )
                            .when_some(self.selected.clone(), |e, id| {
                                e.child(
                                    div()
                                        .id("lab-baseline")
                                        .cursor_pointer()
                                        .child(type_line("Use as baseline ↗", 13., APRICOT))
                                        .on_click(cx.listener(move |v, _, _, cx| {
                                            if let Err(e) = super::laboratory::open(
                                                v.path.clone(),
                                                Some(id.clone()),
                                                cx,
                                            ) {
                                                v.error = e;
                                                cx.notify();
                                            }
                                        })),
                                )
                            })
                            .when_some(self.record.clone(), |e, r| {
                                e.child(type_line("Selected run · latest record", 12., VIOLET))
                                    .child(div().flex_1().min_h_0().child(r))
                            }),
                    ),
            )
    }
}

#[cfg(test)]
mod tests {
    use super::overlap;

    #[test]
    fn a_decided_petition_is_still_held_when_rewound() {
        assert!(!overlap(100, Some(150), 99));
        assert!(overlap(100, Some(150), 100));
        assert!(overlap(100, Some(150), 149));
        assert!(!overlap(100, Some(150), 150));
        assert!(overlap(100, None, 151));
    }
}
