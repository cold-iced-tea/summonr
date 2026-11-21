//! The executor: runs one summon's steps in order, with fresh model contexts,
//! essence accounting shared with every ancestor, and authority checked by
//! the core for every tool call.

use crate::backend::{self, ChatReq};
use crate::core::Core;
use crate::io::Io;
use crate::outcome::Outcome;
use crate::plan::{self, Step};
use crate::reagent::{self, Authority, Fail};
use crate::records::{
    BatchCounts, BatchItem, EffectRecord, Evidence, StepKind, StepRecord, StepStatus,
};
use crate::ritual::{self, Env, Grant, Ritual};
use crate::util::{new_id, parse_duration};
use serde_json::{json, Map, Value};
use std::collections::{BTreeMap, BTreeSet, HashSet};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// Resource ceilings for one summon. A child summon charges its own meter and
/// every ancestor's, so a descendant can never exceed a parent's ceiling.
pub struct Meter {
    pub max_requests: Option<u64>,
    pub max_tool_calls: Option<u64>,
    pub max_items: Option<u64>,
    pub tool_calls: AtomicU64,
    pub items: AtomicU64,
    pub max_tokens: Option<u64>,
    pub max_cost_microusd: Option<u64>,
    pub deadline: Option<Instant>,
    pub requests: AtomicU64,
    pub tokens: AtomicU64,
    pub cost_microusd: AtomicU64,
    pub cost_known: AtomicBool,
}

impl Meter {
    pub fn new(e: &ritual::Essence) -> Meter {
        let time = e.time.as_deref().and_then(|t| parse_duration(t).ok());
        let max_cost_microusd = e
            .cost
            .as_deref()
            .and_then(|c| crate::util::parse_cost_microusd(c).ok());
        Meter {
            max_requests: e.requests,
            max_tool_calls: e.tool_calls,
            max_items: e.items,
            tool_calls: AtomicU64::new(0),
            items: AtomicU64::new(0),
            max_tokens: e.tokens,
            max_cost_microusd,
            deadline: time.map(|ms| {
                Instant::now()
                    .checked_add(Duration::from_millis(ms))
                    .unwrap_or_else(Instant::now)
            }),
            requests: AtomicU64::new(0),
            tokens: AtomicU64::new(0),
            cost_microusd: AtomicU64::new(0),
            cost_known: AtomicBool::new(true),
        }
    }

    fn check(&self) -> Result<(), Fail> {
        if self
            .max_requests
            .is_some_and(|m| self.requests.load(Ordering::SeqCst) >= m)
        {
            return Err(Fail::Exhausted(format!(
                "request limit of {} reached",
                self.max_requests.unwrap()
            )));
        }
        if self
            .max_tokens
            .is_some_and(|m| self.tokens.load(Ordering::SeqCst) >= m)
        {
            return Err(Fail::Exhausted(format!(
                "token limit of {} reached",
                self.max_tokens.unwrap()
            )));
        }
        if self
            .max_cost_microusd
            .is_some_and(|m| self.cost_microusd.load(Ordering::SeqCst) >= m)
        {
            return Err(Fail::Exhausted(format!(
                "cost limit of {} reached",
                crate::util::fmt_cost_microusd(self.max_cost_microusd.unwrap())
            )));
        }
        if self.deadline.is_some_and(|d| Instant::now() >= d) {
            return Err(Fail::Exhausted("time limit reached".into()));
        }
        Ok(())
    }

    fn check_time(&self) -> Result<(), Fail> {
        if self.deadline.is_some_and(|d| Instant::now() >= d) {
            return Err(Fail::Exhausted("time limit reached".into()));
        }
        Ok(())
    }

    /// Unlike request admission, equality is valid for a completed response.
    /// Reject an overrun even on the final request, before its output is used.
    fn check_usage(&self) -> Result<(), Fail> {
        if self
            .max_tokens
            .is_some_and(|m| self.tokens.load(Ordering::SeqCst) > m)
        {
            return Err(Fail::Exhausted(format!(
                "reported token usage exceeded limit of {}",
                self.max_tokens.unwrap()
            )));
        }
        if self
            .max_cost_microusd
            .is_some_and(|m| self.cost_microusd.load(Ordering::SeqCst) > m)
        {
            return Err(Fail::Exhausted(format!(
                "reported cost exceeded limit of {}",
                crate::util::fmt_cost_microusd(self.max_cost_microusd.unwrap())
            )));
        }
        self.check_time()
    }
}

#[derive(Default, Clone)]
pub struct Frame {
    /// step id -> {"status", "kind", "output"}
    pub steps: BTreeMap<String, StepRecord>,
    pub vars: BTreeMap<String, Value>,
    pub abstained: bool,
    /// What happened to each write in this frame: applied (with the evidence
    /// class), held as a petition, or unverified.
    pub effects: Vec<EffectRecord>,
}

#[derive(Clone)]
pub struct Scope {
    pub inputs: Value,
    pub frames: Vec<Frame>,
}

impl Env for Scope {
    fn lookup(&self, path: &[String]) -> Result<Value, String> {
        let shown = || path.join(".");
        match path[0].as_str() {
            "inputs" => ritual::descend(self.inputs.clone(), &path[1..], path),
            "steps" => {
                let id = path
                    .get(1)
                    .ok_or_else(|| format!("`{}` must name a step", shown()))?;
                let found = self.frames.iter().rev().find_map(|f| f.steps.get(id));
                match found {
                    Some(v) => ritual::descend(
                        serde_json::to_value(v).expect("step record serializes"),
                        &path[2..],
                        path,
                    ),
                    None => Err(format!(
                        "`{}`: step `{id}` has not produced a value",
                        shown()
                    )),
                }
            }
            var => match self.frames.iter().rev().find_map(|f| f.vars.get(var)) {
                Some(v) => ritual::descend(v.clone(), &path[1..], path),
                None => Err(format!("`{}` is not a known reference", shown())),
            },
        }
    }
}

pub struct Finished {
    pub outcome: Outcome,
    pub result: Value,
    pub error: Option<String>,
}

pub struct Run {
    pub job_claim: Option<crate::jobs::Claim>,
    pub core: Arc<Core>,
    pub id: String,
    /// Root summon id of the execution tree; fences are keyed under this so
    /// child writes survive revive and recast.
    pub root_id: String,
    /// Stable hierarchical prefix for fence keys (e.g. `call>`, `batch[2]/`).
    pub fence_prefix: String,
    pub def: Ritual,
    pub inputs: Value,
    pub model_override: Option<String>,
    pub settings_override: BTreeMap<String, Value>,
    pub meters: Vec<Arc<Meter>>,
    pub selected: Arc<Mutex<HashSet<String>>>,
    pub cancel: Arc<AtomicBool>,
    pub grants: Arc<Vec<Grant>>,
    pub depth: u32,
    pub io: Arc<dyn Io>,
    pub models_used: Arc<Mutex<BTreeSet<String>>>,
    /// The directly invoked ritual, whose standing grants this summon uses.
    pub root_ritual: String,
    pub done: u64,
    pub total: u64,
}

fn brief(v: &Value) -> String {
    let s = v.to_string();
    if s.chars().count() > 96 {
        format!("{}…", s.chars().take(95).collect::<String>())
    } else {
        s
    }
}

impl Run {
    fn emit(&self, kind: &str, text: impl Into<String>, data: Value) {
        self.core.emit(&self.id, kind, &text.into(), data);
    }

    fn check_live(&self) -> Result<(), Fail> {
        self.core
            .storage_health()
            .map_err(|e| Fail::Failed(e.message))?;
        if let Some(claim) = &self.job_claim {
            crate::jobs::check(&self.core.db.lock().unwrap(), claim, crate::util::now_ms())
                .map_err(|e| Fail::Failed(e.message))?;
        }
        if self.cancel.load(Ordering::SeqCst) {
            return Err(Fail::Cancelled);
        }
        for meter in &self.meters {
            meter.check_usage()?;
        }
        Ok(())
    }

    fn admit_tools(&self, n: u64) -> Result<(), Fail> {
        static ADMIT_TOOLS_LOCK: Mutex<()> = Mutex::new(());
        let _guard = ADMIT_TOOLS_LOCK.lock().unwrap();
        self.check_live()?;
        for m in &self.meters {
            if m.max_tool_calls
                .is_some_and(|limit| m.tool_calls.load(Ordering::SeqCst).saturating_add(n) > limit)
            {
                return Err(Fail::Exhausted(
                    "tool call limit reached (including verification)".into(),
                ));
            }
        }
        for m in &self.meters {
            m.tool_calls.fetch_add(n, Ordering::SeqCst);
        }
        Ok(())
    }

    fn admit_item(&self) -> Result<(), Fail> {
        static ADMIT_ITEMS_LOCK: Mutex<()> = Mutex::new(());
        let _guard = ADMIT_ITEMS_LOCK.lock().unwrap();
        for m in &self.meters {
            if m.max_items
                .is_some_and(|limit| m.items.load(Ordering::SeqCst) >= limit)
            {
                return Err(Fail::Exhausted("item limit reached".into()));
            }
        }
        for m in &self.meters {
            m.items.fetch_add(1, Ordering::SeqCst);
        }
        Ok(())
    }

    fn admit_request(&self) -> Result<(), Fail> {
        static ADMIT_REQUESTS_LOCK: Mutex<()> = Mutex::new(());
        let _guard = ADMIT_REQUESTS_LOCK.lock().unwrap();
        for meter in &self.meters {
            meter.check()?;
        }
        self.check_live()?;
        // Admission consumes a request even when the backend fails or times
        // out; a failed attempt must not become a free implicit retry.
        for meter in &self.meters {
            meter.requests.fetch_add(1, Ordering::SeqCst);
        }
        Ok(())
    }

    fn fork_worker(&self) -> Self {
        Self {
            job_claim: self.job_claim.clone(),
            core: self.core.clone(),
            id: self.id.clone(),
            root_id: self.root_id.clone(),
            fence_prefix: self.fence_prefix.clone(),
            def: self.def.clone(),
            inputs: self.inputs.clone(),
            model_override: self.model_override.clone(),
            settings_override: self.settings_override.clone(),
            meters: self.meters.clone(),
            selected: self.selected.clone(),
            cancel: self.cancel.clone(),
            grants: self.grants.clone(),
            depth: self.depth,
            io: self.io.clone(),
            models_used: self.models_used.clone(),
            root_ritual: self.root_ritual.clone(),
            done: 0,
            total: 0,
        }
    }

    fn request_limits(&self) -> (Option<Duration>, Option<u64>) {
        let now = Instant::now();
        let timeout = self
            .meters
            .iter()
            .filter_map(|m| m.deadline)
            .map(|d| d.saturating_duration_since(now))
            .min();
        let tokens = self
            .meters
            .iter()
            .filter_map(|m| {
                m.max_tokens
                    .map(|limit| limit.saturating_sub(m.tokens.load(Ordering::SeqCst)))
            })
            .min();
        (timeout, tokens)
    }

    fn checkpoint_top(&self, frame: &Frame) {
        self.core.save_checkpoint(
            &self.id,
            "top",
            &json!({
                "steps": frame.steps,
                "abstained": frame.abstained,
            }),
        );
    }

    pub fn execute(&mut self) -> Finished {
        let mut scope = Scope {
            inputs: self.inputs.clone(),
            frames: vec![Frame::default()],
        };
        if self.depth == 0 {
            match self.core.load_checkpoints(&self.id) {
                Ok(map) => {
                    if let Some(top) = map.get("top") {
                        if let Ok(steps) = serde_json::from_value::<BTreeMap<String, StepRecord>>(
                            top["steps"].clone(),
                        ) {
                            scope.frames[0].steps = steps;
                        }
                        scope.frames[0].abstained = top["abstained"].as_bool().unwrap_or(false);
                    }
                }
                Err(e) => {
                    return Finished {
                        outcome: Outcome::Failed,
                        result: json!({}),
                        error: Some(e.message),
                    };
                }
            }
        }
        let ran = plan::compile(&self.def.steps)
            .map_err(Fail::Failed)
            .and_then(|steps| self.run_steps(&steps, &mut scope));
        let ran = ran.and_then(|_| self.check_live());
        let top = &scope.frames[0];
        // Per-item accounting from any iteration, so a partial batch can
        // never present itself as a complete one.
        let partial: Map<String, Value> = top
            .steps
            .iter()
            .filter(|(_, v)| v.kind == StepKind::Each)
            .map(|(k, v)| (k.clone(), json!({"items": v.output, "counts": v.counts})))
            .collect();
        match ran {
            Ok(()) => {
                let ret = Value::Object(
                    self.def
                        .ret
                        .iter()
                        .map(|(k, v)| (k.clone(), v.clone()))
                        .collect(),
                );
                let rendered = ritual::render_value(&ret, &scope).and_then(|result| {
                    if let Some(schema) = &self.def.output_schema {
                        ritual::check_schema(schema, &result, "return")?;
                    }
                    Ok(result)
                });
                match rendered {
                    Ok(result) => Finished {
                        outcome: if top.abstained {
                            Outcome::Abstained
                        } else {
                            Outcome::Returned
                        },
                        result,
                        error: None,
                    },
                    Err(e) => Finished {
                        outcome: Outcome::Failed,
                        result: json!({"partial": partial}),
                        error: Some(format!("return value: {e}")),
                    },
                }
            }
            Err(f) => Finished {
                outcome: f.outcome(),
                result: json!({"partial": partial}),
                error: Some(f.message()),
            },
        }
    }

    fn run_with_recovery<F>(
        &mut self,
        sid: &str,
        retry: u32,
        fallback: &[Step],
        scope: &mut Scope,
        mut attempt: F,
    ) -> Result<(Value, bool), Fail>
    where
        F: FnMut(&mut Self, &mut Scope) -> Result<(Value, bool), Fail>,
    {
        for try_idx in 0..=retry {
            self.check_live()?;
            match attempt(self, scope) {
                Ok(res) => return Ok(res),
                Err(f @ (Fail::Exhausted(_) | Fail::Cancelled | Fail::Denied(_))) => {
                    return Err(f);
                }
                Err(f @ (Fail::Failed(_) | Fail::Unverified(_) | Fail::Invalid(_))) => {
                    if try_idx < retry {
                        self.emit(
                            "step.retry",
                            format!("{sid}: attempt {} failed ({}); retrying", try_idx + 1, f.message()),
                            json!({"step": sid, "attempt": try_idx + 1}),
                        );
                        continue;
                    }
                    if !fallback.is_empty() {
                        self.emit(
                            "step.fallback",
                            format!("{sid}: running fallback after {}", f.message()),
                            json!({"step": sid}),
                        );
                        scope.frames.push(Frame::default());
                        let ran = self.run_steps(fallback, scope);
                        let frame = scope.frames.pop().unwrap();
                        ran?;
                        let last_out = fallback
                            .last()
                            .and_then(|last| last.id())
                            .and_then(|id| frame.steps.get(id))
                            .map(|rec| rec.output.clone())
                            .unwrap_or(Value::Null);
                        return Ok((last_out, frame.abstained));
                    }
                    return Err(f);
                }
            }
        }
        unreachable!()
    }

    fn run_steps(&mut self, steps: &[Step], scope: &mut Scope) -> Result<(), Fail> {
        for (step_idx, s) in steps.iter().enumerate() {
            self.check_live()?;
            let id = s.id().cloned().unwrap_or_default();
            let top_level = self.depth == 0 && scope.frames.len() == 1;
            if top_level && !id.is_empty() && scope.frames[0].steps.contains_key(&id) {
                // A restored read happened in an earlier run, so this run's
                // issue key for its handle is re-issued; only a step that
                // is statically a sight read and whose output came from that live
                // read (never a fallback, which may be a model's) earns one.
                if let Step::Operation(op) = s {
                    if op.operation == "sight.read_image" && scope.frames[0].steps[&id].direct {
                        let auth = Authority {
                            grants: &self.grants,
                            deadline: None,
                            on_item: None,
                            selected: &self.selected,
                            expected_digest: None,
                            config: Some(&self.core.cfg),
                        };
                        reagent::adopt(&op.operation, &scope.frames[0].steps[&id].output, &auth);
                    }
                }
                continue;
            }
            match s {
                Step::Operation(s) => {
                    let mut direct = false;
                    let (out, abstained) = self.run_with_recovery(
                        &id,
                        s.retry,
                        &s.fallback,
                        scope,
                        |this, sc| {
                            let v = this.operation(s, step_idx, sc)?;
                            direct = true;
                            Ok((v, false))
                        },
                    )?;
                    let kind = if s.operation.starts_with("data.") {
                        StepKind::Pure
                    } else {
                        StepKind::Operation
                    };
                    let frame = scope.frames.last_mut().unwrap();
                    frame.abstained |= abstained;
                    let mut rec = StepRecord::new(StepStatus::Returned, kind, out);
                    rec.direct = direct;
                    frame.steps.insert(id, rec);
                }
                Step::Model(s) => {
                    let (out, abstained) = self.run_with_recovery(
                        &id,
                        s.retry,
                        &s.fallback,
                        scope,
                        |this, sc| this.model(s, sc),
                    )?;
                    let frame = scope.frames.last_mut().unwrap();
                    frame.abstained |= abstained;
                    frame.steps.insert(
                        id,
                        StepRecord::new(
                            if abstained {
                                StepStatus::Abstained
                            } else {
                                StepStatus::Returned
                            },
                            StepKind::Model,
                            out,
                        ),
                    );
                }
                Step::Ritual(s) => {
                    let (out, abstained) = self.child(s, step_idx, scope)?;
                    let frame = scope.frames.last_mut().unwrap();
                    frame.abstained |= abstained;
                    frame.steps.insert(
                        id,
                        StepRecord::new(
                            if abstained {
                                StepStatus::Abstained
                            } else {
                                StepStatus::Returned
                            },
                            StepKind::Ritual,
                            out,
                        ),
                    );
                }
                Step::Each(s) => self.each(s, step_idx, scope)?,
                Step::If(s) => {
                    let taken = ritual::eval(&s.condition, scope)
                        .map_err(Fail::Failed)?
                        .as_bool()
                        .ok_or_else(|| Fail::Failed("a condition must be boolean".into()))?;
                    let (run, skip) = if taken {
                        (&s.then, &s.otherwise)
                    } else {
                        (&s.otherwise, &s.then)
                    };
                    for skipped in skip {
                        if let Some(sid) = skipped.id() {
                            scope.frames.last_mut().unwrap().steps.insert(
                                sid.clone(),
                                StepRecord::new(
                                    StepStatus::Skipped,
                                    StepKind::Skipped,
                                    Value::Null,
                                ),
                            );
                        }
                    }
                    self.run_steps(run, scope)?;
                }
            }
            if top_level {
                self.checkpoint_top(&scope.frames[0]);
            }
        }
        Ok(())
    }

    fn operation(&mut self, s: &plan::Operation, step_idx: usize, scope: &mut Scope) -> Result<Value, Fail> {
        let op = s.operation.as_str();
        let sid = s.id.as_deref().unwrap_or(op);
        let site = match s.id.as_deref() {
            Some(name) => name.to_string(),
            None => format!("#{step_idx}:{op}"),
        };
        let fence_step = if self.fence_prefix.is_empty() {
            site
        } else {
            format!("{}{}", self.fence_prefix, site)
        };
        let (reagent, tool) = op
            .split_once('.')
            .ok_or_else(|| Fail::Failed(format!("bad operation `{op}`")))?;
        let object = |m: &BTreeMap<String, Value>| Value::Object(m.clone().into_iter().collect());
        let render = |v: Value, scope: &Scope| {
            ritual::render_value(&v, scope).map_err(|e| Fail::Failed(format!("{op}: {e}")))
        };
        let mut raw_args = object(&s.arguments);
        // A collection's body (map/fold arguments, filter predicate) is
        // evaluated per item, never rendered as text from the outer scope.
        let body_key = match op {
            "data.map" | "data.fold" => Some("arguments"),
            "data.filter" => Some("predicate"),
            _ => None,
        };
        let body = body_key.and_then(|k| raw_args.as_object_mut().unwrap().remove(k));
        let mut args = render(raw_args, scope)?;
        if let (Some(k), Some(body)) = (body_key, body) {
            args[k] = body;
        }
        let effect = reagent::tool_meta(Some(&self.core.cfg), reagent, tool)
            .map(|t| t.effect)
            .unwrap_or("unknown");
        reagent::check_job_effect(self.job_claim.is_some(), effect)?;
        if !matches!(effect, "read" | "pure") && scope.frames.iter().any(|f| f.abstained) {
            return Err(Fail::Denied(
                "an abstained result cannot authorize a write in this scope".into(),
            ));
        }
        let check = match &s.verify {
            Some(v) => Some(reagent::Check {
                observe: v.observe.clone(),
                arguments: render(object(&v.arguments), scope)?,
                expect: render(object(&v.expect), scope)?,
            }),
            None => None,
        };
        let hold = match &s.hold {
            None => false,
            Some(h) => match render(h.clone(), scope)? {
                Value::Bool(b) => b,
                other => {
                    return Err(Fail::Failed(format!(
                        "step {sid}: `hold` must be true or false, found {other}"
                    )))
                }
            },
        };

        // Under a pact, the caller may also have to approve this step, when
        // its condition (computed by this ritual) is true. It adds to the
        // step's own hold, never replaces it.
        let pact = self.core.pact_hold_for(&self.id, self.depth, sid);
        let caller_hold = match pact.as_ref().and_then(|(_, when)| when.clone()) {
            None => false,
            Some(w) => match render(w, scope)? {
                Value::Bool(b) => b,
                other => {
                    return Err(Fail::Failed(format!(
                        "step {sid}: the pact's caller hold must be true or false, found {other}"
                    )))
                }
            },
        };
        let tag = pact.filter(|_| hold || caller_hold).map(|(tag, _)| (tag, hold, caller_hold));
        let hold = hold || caller_hold;
        // A simulated run never files a petition: the held write is
        // authorized and recorded as simulated, with `would_hold`.
        let would_hold = hold && self.io.simulates(op, Some(&self.core.cfg));
        if hold && !would_hold {
            // Buffer before acting: record what would be done and why, and
            // leave the decision to a person. A write no grant covers is
            // refused here, before its target is probed or a petition filed.
            let deadline = self.meters.iter().filter_map(|m| m.deadline).min();
            reagent::authorize(
                self.core.cfg.mail_root.as_deref(),
                op,
                &args,
                &Authority {
                    grants: &self.grants,
                    deadline,
                    on_item: None,
                    selected: &self.selected,
                    expected_digest: None,
                    config: Some(&self.core.cfg),
                },
            )?;
            self.check_live()?;
            // A petition is bound to the version of its target that was
            // reviewed. If that version cannot be read, nothing is held: an
            // unbound approval would act on whatever is there by then.
            let digest = reagent::target_digest_with_config(
                self.core.cfg.mail_root.as_deref(),
                Some(&self.core.cfg),
                op,
                &args,
                deadline,
            )
            .map_err(|e| match e {
                Fail::Exhausted(_) | Fail::Cancelled => e,
                other => Fail::Failed(format!(
                    "{op} was not held: cannot record the current state of its target ({})",
                    other.message()
                )),
            })?;
            self.check_live()?;
            let frame = scope.frames.last().unwrap();
            let because: Map<String, Value> = frame
                .steps
                .iter()
                .filter(|(_, v)| v.kind.produces_result())
                .map(|(k, v)| (k.clone(), v.output.clone()))
                .collect();
            let selection: Vec<String> = self.selected.lock().unwrap().iter().cloned().collect();
            let pid = self.core.propose(
                &self.id,
                &self.root_ritual,
                sid,
                op,
                effect,
                &args,
                check.as_ref(),
                &selection,
                &Value::Object(because),
                &self.grants,
                digest.as_deref(),
                tag.as_ref().map(|(t, callee, caller)| (t, *callee, *caller)),
            );
            self.core
                .storage_health()
                .map_err(|e| Fail::Failed(e.message))?;
            self.emit(
                "held",
                format!("{op} {} held as petition {pid}", brief(&args)),
                json!({"petition": pid, "operation": op, "arguments": args,"state":"held"}),
            );
            scope
                .frames
                .last_mut()
                .unwrap()
                .effects
                .push(EffectRecord::Held {
                    step: sid.into(),
                    operation: op.into(),
                    petition: pid.clone(),
                });
            return Ok(json!({"held": true, "petition": pid}));
        }

        self.admit_tools(1 + u64::from(check.is_some()))?;
        let on_item = || {
            self.check_live()?;
            self.admit_item()?;
            if matches!(op, "data.map" | "data.fold") {
                self.admit_tools(1)?;
            }
            Ok(())
        };
        let auth = Authority {
            grants: &self.grants,
            deadline: self.meters.iter().filter_map(|m| m.deadline).min(),
            on_item: Some(&on_item),
            selected: &self.selected,
            expected_digest: None,
            config: Some(&self.core.cfg),
        };
        self.check_live()?;
        let is_write = !matches!(effect, "read" | "pure");
        let fence_key = if is_write && !self.io.simulates(op, Some(&self.core.cfg)) {
            reagent::authorize(self.core.cfg.mail_root.as_deref(), op, &args, &auth)?;
            self.check_live()?;
            match self
                .core
                .fence_acquire(&self.root_id, &fence_step, op, effect, &args)
                .map_err(|e| Fail::Failed(e.message))?
            {
                crate::core::FenceAcquire::Cached { output, evidence } => {
                    self.emit(
                        "op.fenced",
                        format!("{op} {sid} → cached (already applied)"),
                        json!({"step": sid, "operation": op, "state": "cached", "evidence": evidence}),
                    );
                    scope
                        .frames
                        .last_mut()
                        .unwrap()
                        .effects
                        .push(EffectRecord::Applied {
                            step: sid.into(),
                            operation: op.into(),
                            evidence,
                        });
                    return Ok(output);
                }
                crate::core::FenceAcquire::Blocked { reason } => {
                    self.emit(
                        "op.fenced",
                        format!("{op} {sid} blocked: {reason}"),
                        json!({"step": sid, "operation": op, "state": "blocked", "reason": reason}),
                    );
                    return Err(Fail::Denied(reason));
                }
                crate::core::FenceAcquire::Proceed { fence_key } => Some(fence_key),
            }
        } else {
            None
        };
        self.emit(
            "op.intent",
            format!("{sid}: dispatch {op}"),
            json!({"step":sid,"operation":op,"arguments":args,"effect":effect,"fence_key":fence_key}),
        );
        match reagent::perform(
            self.core.cfg.mail_root.as_deref(),
            op,
            &args,
            check.as_ref(),
            &auth,
            self.io.as_ref(),
        ) {
            Ok((out, evidence)) => {
                if let Some(ref fk) = fence_key {
                    if !matches!(evidence, Evidence::Simulated { .. }) {
                        self.core.fence_complete(fk, &out, &evidence);
                    }
                }
                // Message bodies stay out of the ledger line; the model
                // payload record holds what was actually sent.
                let shown = match (&out, tool) {
                    (Value::Array(a), _) => format!("{} items", a.len()),
                    (_, "read") => format!(
                        "{} chars",
                        out["text"].as_str().map(|t| t.chars().count()).unwrap_or(0)
                    ),
                    _ => brief(&out),
                };
                let simulated = matches!(evidence, Evidence::Simulated { .. });
                let state = if simulated { "simulated" } else { "applied" };
                let mut data = json!({"step":sid,"operation": op, "arguments": args, "effect": effect,"state":state,"evidence":evidence});
                if would_hold {
                    data["would_hold"] = json!(true);
                }
                self.emit("op", format!("{op} {} → {shown}", brief(&args)), data);
                if !matches!(effect, "read" | "pure") {
                    if simulated {
                        if check.is_some() {
                            self.emit(
                                "check",
                                format!("{sid} check skipped: simulated"),
                                json!({"step":sid,"state":"simulated"}),
                            );
                        }
                    } else if check.is_some() {
                        self.emit(
                            "check",
                            format!(
                                "{sid} verified: {} observed {}",
                                evidence.observation().map(|(by, _)| by).unwrap_or(""),
                                evidence
                                    .observation()
                                    .map(|(_, v)| brief(v))
                                    .unwrap_or_default()
                            ),
                            serde_json::to_value(&evidence).expect("evidence serializes"),
                        );
                    }
                    let record = if simulated {
                        EffectRecord::Simulated {
                            step: sid.into(),
                            operation: op.into(),
                            evidence,
                            would_hold,
                        }
                    } else {
                        EffectRecord::Applied {
                            step: sid.into(),
                            operation: op.into(),
                            evidence,
                        }
                    };
                    scope.frames.last_mut().unwrap().effects.push(record);
                }
                Ok(out)
            }
            Err(f) => {
                if let Some(ref fk) = fence_key {
                    // Pre-dispatch refusals (validation, authorization, cancel)
                    // never sent an external write, so clear the fence instead
                    // of blocking a future revive.
                    if matches!(f, Fail::Invalid(_) | Fail::Denied(_) | Fail::Cancelled) {
                        self.core.fence_clear(fk);
                    } else {
                        self.core.fence_fail(fk, &f.message());
                    }
                }
                let kind = if matches!(f, Fail::Unverified(_)) {
                    "check.failed"
                } else {
                    "op.refused"
                };
                self.emit(
                    kind,
                    format!("{op} {}: {}", brief(&args), f.message()),
                    json!({"operation": op, "arguments": args, "effect": effect}),
                );
                if matches!(f, Fail::Unverified(_)) {
                    scope
                        .frames
                        .last_mut()
                        .unwrap()
                        .effects
                        .push(EffectRecord::Unverified {
                            step: sid.into(),
                            operation: op.into(),
                            error: f.message(),
                        });
                }
                Err(f)
            }
        }
    }

    /// Resolve a model step's `images` to verified bytes, adding an image
    /// part to `manifest` for each. A handle counts only if `sight` issued it
    /// in this run, the run's grants still cover its file, and the file still
    /// holds the bytes it was issued for.
    fn step_images(
        &self,
        sid: &str,
        images: Option<&ritual::Images>,
        scope: &Scope,
        manifest: &mut Vec<Value>,
    ) -> Result<Vec<backend::ImagePart>, Fail> {
        use crate::reagents::sight_reagent::{issue_key, load_verified, sniff};
        let Some(images) = images else {
            return Ok(Vec::new());
        };
        let render = |r: &String| {
            ritual::render_value(&Value::String(r.clone()), scope)
                .map_err(|e| Fail::Failed(format!("step {sid} images: {e}")))
        };
        let handles: Vec<Value> = match images {
            ritual::Images::One(r) => match render(r)? {
                Value::Array(list) => list,
                one => vec![one],
            },
            ritual::Images::Many(rs) => rs.iter().map(render).collect::<Result<_, _>>()?,
        };
        if handles.iter().any(|h| h.get("kind").and_then(Value::as_str) != Some("image")) {
            return Err(Fail::Invalid(format!(
                "step {sid}: images entry is not an image handle"
            )));
        }
        let (max_bytes, max_per_step) = self.core.cfg.sight_limits();
        if handles.len() > max_per_step {
            return Err(Fail::Invalid(format!(
                "step {sid}: {} images; at most {max_per_step} are allowed per step",
                handles.len()
            )));
        }
        {
            let selected = self.selected.lock().unwrap();
            if !handles
                .iter()
                .all(|h| issue_key(h).is_some_and(|k| selected.contains(&k)))
            {
                return Err(Fail::Denied(format!(
                    "step {sid}: image was not read by sight in this run"
                )));
            }
        }
        let auth = Authority {
            grants: &self.grants,
            deadline: self.meters.iter().filter_map(|m| m.deadline).min(),
            on_item: None,
            selected: &self.selected,
            expected_digest: None,
            config: Some(&self.core.cfg),
        };
        let mut parts = Vec::with_capacity(handles.len());
        for h in &handles {
            // `issue_key` succeeded, so these are strings.
            let root = h["root"].as_str().unwrap_or_default();
            let path = h["path"].as_str().unwrap_or_default();
            // A handle recorded over the limit is refused without reading it:
            // the limit is a property of the step, not of a changed file.
            let recorded = h.get("bytes").and_then(Value::as_u64).ok_or_else(|| {
                Fail::Invalid(format!("step {sid}: images entry is not an image handle"))
            })?;
            if recorded > max_bytes {
                return Err(Fail::Invalid(format!(
                    "step {sid}: image `{path}` is {recorded} bytes, over the {max_bytes}-byte limit for model steps"
                )));
            }
            reagent::authorize(
                self.core.cfg.mail_root.as_deref(),
                "sight.read_image",
                &json!({"root": root, "path": path}),
                &auth,
            )
            .map_err(|e| {
                let why = match e {
                    Fail::Denied(m) | Fail::Invalid(m) | Fail::Failed(m) => m,
                    other => other.message(),
                };
                Fail::Denied(format!(
                    "step {sid}: image `{path}` is not readable under this run's grants: {why}"
                ))
            })?;
            let in_step = |m: String| format!("step {sid}: {m}");
            // Never `Exhausted`: a changed or grown file is a stale image.
            let bytes = load_verified(h, max_bytes).map_err(|e| match e {
                Fail::Invalid(m) => Fail::Invalid(in_step(m)),
                Fail::Denied(m) => Fail::Denied(in_step(m)),
                other => Fail::Failed(in_step(other.message())),
            })?;
            let (mime, width, height) = sniff(&bytes).map_err(|e| Fail::Invalid(in_step(e.message())))?;
            let sha256 = h["sha256"].as_str().unwrap_or_default().to_string();
            manifest.push(json!({
                "part": "image", "sha256": sha256, "path": path, "mime": mime,
                "width": width, "height": height, "bytes": bytes.len(),
            }));
            parts.push(backend::ImagePart { sha256, mime: mime.into(), bytes });
        }
        Ok(parts)
    }

    fn model(&mut self, s: &plan::Model, scope: &Scope) -> Result<(Value, bool), Fail> {
        let m = &s.model;
        let sid = s.id.as_deref().unwrap_or("model");
        let model = m
            .name
            .clone()
            .or_else(|| self.model_override.clone())
            .or_else(|| self.def.model.clone())
            .ok_or_else(|| Fail::Failed(format!("step {sid}: no model")))?;

        // Stable content first, so a backend can reuse its evaluation of the
        // shared prefix across fresh contexts.
        let mut system = m
            .instructions
            .clone()
            .unwrap_or_default()
            .trim()
            .to_string();
        let mut manifest = Vec::new();
        if !system.is_empty() {
            manifest.push(json!({"part": "instructions", "chars": system.chars().count()}));
        }
        for b in &m.bind {
            let binding = self
                .def
                .bindings
                .get(b)
                .ok_or_else(|| Fail::Failed(format!("step {sid}: unknown binding `{b}`")))?;
            let content = binding.content.as_deref().unwrap_or("");
            system.push_str(&format!("\n\n## {b}\n\n{}", content.trim()));
            manifest.push(json!({"part": "binding", "name": b, "hash": binding.hash, "chars": content.chars().count()}));
        }
        let user = ritual::plain_string(
            &ritual::render_template(&m.prompt, scope)
                .map_err(|e| Fail::Failed(format!("step {sid} prompt: {e}")))?,
        );
        manifest.push(json!({"part": "task", "chars": user.chars().count()}));
        // Every image is resolved, authorized and re-verified here, before a
        // request is admitted: a refused image spends nothing.
        let images = self.step_images(sid, m.images.as_ref(), scope, &mut manifest)?;
        let base_url = self.core.cfg.backend_url_for(&model);
        if !images.is_empty() && !self.io.offline() && !self.io.scripted() {
            backend::supports_images(&base_url, &model, self.request_limits().0)
                .map_err(|e| Fail::Failed(format!("step {sid}: {e}")))?;
        }

        let mut settings = self.def.settings.clone();
        settings.extend(self.settings_override.clone());
        settings.extend(m.settings.clone());

        let (timeout, max_output_tokens) = self.request_limits();
        if let Some(remaining_tokens) = max_output_tokens {
            // Conservative pre-flight lower bound (~8 bytes/token) so obviously oversized
            // prompts fail closed before contacting an external backend without false-positiving
            // on tight-budget tests.
            let estimated_prompt_tokens = ((system.len() + user.len()) as u64) / 8;
            if estimated_prompt_tokens > remaining_tokens {
                let msg = format!(
                    "step {sid}: estimated prompt size (~{estimated_prompt_tokens} tokens) exceeds remaining token essence ({remaining_tokens})"
                );
                self.emit(
                    "model.refused",
                    msg.clone(),
                    json!({
                        "step": sid,
                        "model": model,
                        "estimated_prompt_tokens": estimated_prompt_tokens,
                        "remaining_tokens": remaining_tokens,
                    }),
                );
                return Err(Fail::Exhausted(msg));
            }
        }
        let (_, model_name) = backend::split_model(&model).map_err(Fail::Failed)?;
        let cost_capped = self.meters.iter().any(|m| m.max_cost_microusd.is_some());
        let pricing = self.core.cfg.price_for(&model);
        if cost_capped && pricing.is_none() {
            let msg = format!(
                "step {sid}: model `{model}` has no entry in config.yaml `pricing:` and this run has a cost limit"
            );
            self.emit(
                "model.refused",
                msg.clone(),
                json!({"step": sid, "model": model, "reason": "unpriced_model"}),
            );
            return Err(Fail::Failed(msg));
        }
        self.admit_request()?;
        self.models_used.lock().unwrap().insert(model.clone());
        let req = ChatReq {
            model: model_name,
            system: &system,
            user: &user,
            schema: m.schema.as_ref(),
            settings: &settings,
            timeout,
            max_output_tokens,
            images: &images,
        };
        let current_cost = if self.meters[0].cost_known.load(Ordering::SeqCst) {
            Some(self.meters[0].cost_microusd.load(Ordering::SeqCst))
        } else {
            None
        };
        self.core.usage(
            &self.id,
            self.meters[0].requests.load(Ordering::SeqCst),
            self.meters[0].tokens.load(Ordering::SeqCst),
            current_cost,
        );
        let payload = backend::payload(&model, &req).map_err(Fail::Failed)?;
        self.emit(
            "model.attempt",
            format!("{sid}: inference admitted"),
            json!({"step":sid,"model":model,"payload":payload,"usage":"unknown until response"}),
        );
        self.check_live()?;
        let resp = self
            .io
            .chat(&base_url, &model, sid, req)
            .map_err(|e| {
                self.emit(
                    "model.error",
                    format!("{sid}: {e}"),
                    json!({"model": model}),
                );
                self.check_live()
                    .err()
                    .unwrap_or_else(|| Fail::Failed(format!("step {sid}: {e}")))
            })?;
        let tokens = resp.prompt_tokens + resp.output_tokens;
        let (turn_cost, cost_source) = match &pricing {
            Some(rate) => {
                let c = rate
                    .cost_microusd(resp.prompt_tokens, resp.output_tokens)
                    .map_err(Fail::Failed)?;
                (Some(c), "rate_card")
            }
            None => match resp.provider_cost_microusd {
                Some(c) => (Some(c), "provider"),
                None => (None, "unpriced"),
            },
        };
        for meter in &self.meters {
            meter.tokens.fetch_add(tokens, Ordering::SeqCst);
            match turn_cost {
                Some(c) => {
                    meter.cost_microusd.fetch_add(c, Ordering::SeqCst);
                }
                None => {
                    meter.cost_known.store(false, Ordering::SeqCst);
                }
            }
        }
        let persisted_cost = if self.meters[0].cost_known.load(Ordering::SeqCst) {
            Some(self.meters[0].cost_microusd.load(Ordering::SeqCst))
        } else {
            None
        };
        self.core.usage(
            &self.id,
            self.meters[0].requests.load(Ordering::SeqCst),
            self.meters[0].tokens.load(Ordering::SeqCst),
            persisted_cost,
        );
        self.emit(
            "model",
            format!("{sid} through {model}: {} tokens in, {} out, {:.1}s", resp.prompt_tokens, resp.output_tokens, resp.total_ms as f64 / 1000.0),
            json!({
                "step": sid, "model": model, "context": "fresh", "manifest": manifest,
                "payload": resp.payload, "response": resp.content,
                "done_reason": resp.done_reason,
                "usage": {
                    "prompt_tokens": resp.prompt_tokens,
                    "output_tokens": resp.output_tokens,
                    "confidence": resp.token_confidence,
                    "cost_microusd": turn_cost,
                    "cost_source": cost_source,
                    "provider_cost_microusd": resp.provider_cost_microusd,
                },
                "timing_ms": {"load": resp.load_ms, "prompt": resp.prompt_ms, "eval": resp.eval_ms, "total": resp.total_ms},
                "structure": if m.schema.is_none() { "none declared" } else if resp.constrained { "enforced by decoder, then checked" } else { "checked afterward only" },
                "essence_enforcement": {
                    "generation_cap": max_output_tokens,
                    "request_timeout_ms": timeout.map(|t| t.as_millis() as u64),
                    "total_tokens": "reported after inference; an overrun rejects the result before dependent work",
                },
            }),
        );
        if resp.done_reason.as_deref() == Some("length") {
            return Err(Fail::Invalid(
                "backend truncated generation; dependent work refused".into(),
            ));
        }
        if (max_output_tokens.is_some() || cost_capped) && resp.token_confidence == "unknown" {
            return Err(Fail::Failed("backend did not report token usage; cannot check token/cost essence or continue dependent work".into()));
        }
        self.check_live()?;

        let output = match &m.schema {
            Some(schema) => {
                let parsed: Value = serde_json::from_str(resp.content.trim()).map_err(|e| {
                    let f = Fail::Invalid(format!("step {sid}: output is not JSON: {e}"));
                    self.emit("invalid", f.message(), json!({"response": resp.content}));
                    f
                })?;
                ritual::check_schema(schema, &parsed, "").map_err(|e| {
                    let f = Fail::Invalid(format!("step {sid}: {e}"));
                    self.emit("invalid", f.message(), json!({"response": resp.content}));
                    f
                })?;
                parsed
            }
            None => Value::String(resp.content),
        };
        let mut abstained = false;
        if let Some(cond) = &m.abstain_if {
            // `abstain_if` reads the step's own output as `output.<field>`.
            let env = AbstainEnv {
                inner: scope,
                output: &output,
            };
            abstained = ritual::eval_condition(cond, &env)
                .map_err(|e| Fail::Failed(format!("step {sid} abstain_if: {e}")))?;
            if abstained {
                self.emit(
                    "abstain",
                    format!("{sid} abstained: {}", brief(&output)),
                    json!({"output": output}),
                );
            }
        }
        Ok((output, abstained))
    }

    /// A ritual step: a pinned child ritual run as its own linked summon in a
    /// fresh context, with explicit inputs, no more authority than this
    /// summon holds, and essence drawn from this summon's.
    fn child(&mut self, s: &plan::Ritual, step_idx: usize, scope: &Scope) -> Result<(Value, bool), Fail> {
        let name = s.ritual.as_str();
        let sid = s.id.as_deref().unwrap_or(name);
        let child_site = match s.id.as_deref() {
            Some(id) => id.to_string(),
            None => format!("#{step_idx}:{name}"),
        };
        let (version, hash, def) = self
            .core
            .load_ritual(name, s.version)
            .map_err(|e| Fail::Failed(e.message))?
            .ok_or_else(|| {
                Fail::Failed(format!(
                    "step {sid}: ritual `{name}` is not in the grimoire"
                ))
            })?;
        if s.hash.as_deref().is_some_and(|h| h != hash) {
            return Err(Fail::Failed(format!(
                "step {sid}: `{name}` version {version} no longer matches its pinned hash"
            )));
        }
        let raw = Value::Object(s.arguments.clone().into_iter().collect());
        let args = ritual::render_value(&raw, scope)
            .map_err(|e| Fail::Failed(format!("step {sid}: {e}")))?;
        let inputs = ritual::resolve_inputs(&def, args.as_object().unwrap())
            .map_err(|e| Fail::Failed(format!("step {sid}: {e}")))?;
        for meter in &self.meters {
            meter.check_usage()?;
        }
        let child_id = new_id("s");
        let model = self.model_override.clone().or_else(|| def.model.clone());
        self.core.insert_summon(
            &child_id,
            Some(&self.id),
            name,
            version,
            &hash,
            model.as_deref(),
            &inputs,
            &def.essence,
            None,
        );
        self.emit(
            "child",
            format!("{sid}: called {name} v{version} as {child_id}"),
            json!({"summon": child_id, "ritual": name, "version": version}),
        );
        let mut meters = vec![Arc::new(Meter::new(&def.essence))];
        meters.extend(self.meters.iter().cloned());
        let child_grants = ritual::narrow_grants(&self.grants, &def.requires);
        let mut run = Run {
            job_claim: self.job_claim.clone(),
            core: self.core.clone(),
            id: child_id.clone(),
            root_id: self.root_id.clone(),
            fence_prefix: format!("{}{child_site}>", self.fence_prefix),
            def,
            inputs,
            model_override: self.model_override.clone(),
            settings_override: self.settings_override.clone(),
            meters,
            selected: self.selected.clone(),
            cancel: self.cancel.clone(),
            grants: Arc::new(child_grants),
            depth: self.depth + 1,
            io: self.io.clone(),
            models_used: self.models_used.clone(),
            root_ritual: self.root_ritual.clone(),
            done: 0,
            total: 0,
        };
        self.core.emit(
            &child_id,
            "summon.start",
            &format!("called by {} at depth {}", self.id, run.depth),
            json!({"parent": self.id}),
        );
        let fin = run.execute();
        self.core
            .finish_summon(&child_id, &fin, json!("held by the calling summon"));
        let persisted_cost = if self.meters[0].cost_known.load(Ordering::SeqCst) {
            Some(self.meters[0].cost_microusd.load(Ordering::SeqCst))
        } else {
            None
        };
        self.core.usage(
            &self.id,
            self.meters[0].requests.load(Ordering::SeqCst),
            self.meters[0].tokens.load(Ordering::SeqCst),
            persisted_cost,
        );
        match fin.outcome {
            Outcome::Returned => Ok((fin.result, false)),
            Outcome::Abstained => Ok((fin.result, true)),
            Outcome::Exhausted => Err(Fail::Exhausted(format!(
                "in {name} ({child_id}): {}",
                fin.error.unwrap_or_default()
            ))),
            Outcome::Cancelled => Err(Fail::Cancelled),
            Outcome::Invalid => Err(Fail::Invalid(format!(
                "{name} ({child_id}): {}",
                fin.error.unwrap_or_default()
            ))),
            other => Err(Fail::Failed(format!(
                "{name} ({child_id}) {other}: {}",
                fin.error.unwrap_or_default()
            ))),
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn record_batch_item(
        &mut self,
        id: &str,
        top_level: bool,
        keep_going: bool,
        items: &mut Vec<BatchItem>,
        label: String,
        frame: Frame,
        ran: Result<(), Fail>,
    ) -> Option<Fail> {
        let outputs: Map<String, Value> = frame
            .steps
            .iter()
            .filter(|(_, v)| v.kind.produces_result())
            .map(|(k, v)| (k.clone(), v.output.clone()))
            .collect();
        let steps_run: BTreeMap<String, StepStatus> = frame
            .steps
            .iter()
            .map(|(k, v)| (k.clone(), v.status))
            .collect();
        let (status, error) = match &ran {
            Ok(()) if frame.abstained => (Outcome::Abstained, None),
            Ok(()) => (Outcome::Returned, None),
            Err(f) => (f.outcome(), Some(f.message())),
        };
        let fatal = match ran {
            Err(f @ (Fail::Exhausted(_) | Fail::Cancelled)) => Some(f),
            Err(f) if !keep_going => Some(f),
            _ => None,
        };
        let interrupted = matches!(fatal, Some(Fail::Exhausted(_)) | Some(Fail::Cancelled));
        items.push(BatchItem {
            item: label.clone(),
            status,
            error: error.clone(),
            outputs: outputs.clone(),
            steps: steps_run,
            effects: frame.effects,
            interrupted,
        });
        if !interrupted && top_level {
            self.done += 1;
            self.core.progress(&self.id, self.done, self.total);
            self.core
                .save_checkpoint(&self.id, &format!("each:{id}"), &json!(items));
        }
        let detail = outputs.values().next().map(brief).unwrap_or_default();
        self.emit(
            "item",
            format!(
                "{label}: {status}{}{}",
                if detail.is_empty() { "" } else { " " },
                error.clone().unwrap_or(detail)
            ),
            json!({"item": label, "status": status}),
        );
        fatal
    }

    fn each(&mut self, s: &plan::Each, step_idx: usize, scope: &mut Scope) -> Result<(), Fail> {
        let id = s.id.clone().unwrap_or_default();
        let each_site = if id.is_empty() {
            format!("#{step_idx}:each")
        } else {
            id.clone()
        };
        let var = &s.variable;
        let path = &s.source;
        let source = path.join(".");
        let list = if let Some(items) = &s.literal {
            items.clone()
        } else {
            match scope.lookup(path).map_err(Fail::Failed)? {
                Value::Array(a) => a,
                other => {
                    return Err(Fail::Failed(format!(
                        "each: `{source}` is not a list (found {})",
                        brief(&other)
                    )))
                }
            }
        };
        let body = &s.steps;
        let keep_going = s.keep_going;
        let n = list.len();
        let top_level = self.depth == 0 && scope.frames.len() == 1;
        let mut items: Vec<BatchItem> = if top_level {
            self.core
                .load_checkpoints(&self.id)
                .ok()
                .and_then(|m| m.get(&format!("each:{id}")).cloned())
                .and_then(|v| serde_json::from_value(v).ok())
                .unwrap_or_default()
        } else {
            Vec::new()
        };
        let resume_from = items.len();
        if top_level {
            self.total += n as u64;
            self.done += resume_from as u64;
            self.core.progress(&self.id, self.done, self.total);
        }
        let mut stop: Option<Fail> = None;
        if s.parallel <= 1 {
            for (i, item) in list.into_iter().enumerate() {
                if i < resume_from {
                    continue;
                }
                if let Err(f) = self.check_live() {
                    stop = Some(f);
                    break;
                }
                if let Err(f) = self.admit_item() {
                    stop = Some(f);
                    break;
                }
                let label = item
                    .get("id")
                    .map(ritual::plain_string)
                    .unwrap_or_else(|| i.to_string());
                scope.frames.push(Frame {
                    vars: BTreeMap::from([(var.clone(), item)]),
                    ..Default::default()
                });
                let next_prefix = format!("{}{each_site}[{i}]/", self.fence_prefix);
                let prev_prefix = std::mem::replace(&mut self.fence_prefix, next_prefix);
                let ran = self.run_steps(body, scope);
                self.fence_prefix = prev_prefix;
                let frame = scope.frames.pop().unwrap();
                if let Some(f) =
                    self.record_batch_item(&id, top_level, keep_going, &mut items, label, frame, ran)
                {
                    stop = Some(f);
                    break;
                }
            }
        } else {
            let remaining: Vec<(usize, Value)> = list
                .into_iter()
                .enumerate()
                .skip(resume_from)
                .collect();
            let rem_len = remaining.len();
            if rem_len > 0 {
                let workers = (s.parallel as usize).min(rem_len).clamp(1, 32);
                let next_idx = std::sync::atomic::AtomicUsize::new(0);
                let aborted = AtomicBool::new(false);
                type ParallelSlot = Option<(String, Frame, Result<(), Fail>)>;
                let slots: Mutex<Vec<ParallelSlot>> =
                    Mutex::new((0..rem_len).map(|_| None).collect());
                let pre_stop: Mutex<Option<(usize, Fail)>> = Mutex::new(None);
                std::thread::scope(|sc| {
                    for _ in 0..workers {
                        sc.spawn(|| {
                            let mut worker = self.fork_worker();
                            loop {
                                if aborted.load(Ordering::SeqCst) {
                                    break;
                                }
                                let pos = next_idx.fetch_add(1, Ordering::SeqCst);
                                if pos >= rem_len {
                                    break;
                                }
                                let (i, item) = &remaining[pos];
                                if let Err(f) = worker.check_live().and_then(|_| worker.admit_item())
                                {
                                    let mut ps = pre_stop.lock().unwrap();
                                    if ps.as_ref().is_none_or(|(pi, _)| pos < *pi) {
                                        *ps = Some((pos, f));
                                    }
                                    aborted.store(true, Ordering::SeqCst);
                                    break;
                                }
                                let label = item
                                    .get("id")
                                    .map(ritual::plain_string)
                                    .unwrap_or_else(|| i.to_string());
                                let mut wscope = scope.clone();
                                wscope.frames.push(Frame {
                                    vars: BTreeMap::from([(var.clone(), item.clone())]),
                                    ..Default::default()
                                });
                                let next_prefix = format!("{}{each_site}[{i}]/", worker.fence_prefix);
                                let prev_prefix = std::mem::replace(&mut worker.fence_prefix, next_prefix);
                                let ran = worker.run_steps(body, &mut wscope);
                                worker.fence_prefix = prev_prefix;
                                let frame = wscope.frames.pop().unwrap();
                                let fatal_here = matches!(
                                    ran,
                                    Err(Fail::Exhausted(_) | Fail::Cancelled)
                                ) || (!keep_going && ran.is_err());
                                if fatal_here {
                                    aborted.store(true, Ordering::SeqCst);
                                }
                                slots.lock().unwrap()[pos] = Some((label, frame, ran));
                            }
                        });
                    }
                });
                let mut pre = pre_stop.into_inner().unwrap();
                let mut results = slots.into_inner().unwrap();
                for (pos, slot) in results.iter_mut().enumerate() {
                    if pre.as_ref().is_some_and(|(pi, _)| pos == *pi) {
                        stop = pre.take().map(|(_, f)| f);
                        break;
                    }
                    let Some((label, frame, ran)) = slot.take() else {
                        continue;
                    };
                    if let Some(f) = self.record_batch_item(
                        &id,
                        top_level,
                        keep_going,
                        &mut items,
                        label,
                        frame,
                        ran,
                    ) {
                        stop = Some(f);
                        break;
                    }
                }
                if stop.is_none() {
                    if let Some((_, f)) = pre {
                        stop = Some(f);
                    }
                }
            }
        }
        let counts = BatchCounts::from_items(n, &items);
        let mut marked = StepRecord::new(
            if stop.is_some() {
                StepStatus::Partial
            } else {
                StepStatus::Returned
            },
            StepKind::Each,
            serde_json::to_value(items).expect("batch records serialize"),
        );
        marked.counts = Some(counts);
        scope.frames.last_mut().unwrap().steps.insert(id, marked);
        match stop {
            Some(f) => Err(f),
            None => Ok(()),
        }
    }
}

/// Lets `abstain_if` read the step's own output as `output.<field>` while
/// every other reference resolves normally.
struct AbstainEnv<'a> {
    inner: &'a Scope,
    output: &'a Value,
}

impl Env for AbstainEnv<'_> {
    fn lookup(&self, path: &[String]) -> Result<Value, String> {
        if path[0] == "output" {
            ritual::descend(self.output.clone(), &path[1..], path)
        } else {
            self.inner.lookup(path)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::util::Paths;
    use std::path::PathBuf;

    struct Fixture {
        core: Arc<Core>,
        root: PathBuf,
    }

    impl Fixture {
        fn new() -> Self {
            let root = std::env::temp_dir().join(new_id("summonr-exec"));
            let paths = Paths {
                runtime: root.join("run"),
                state: root.join("state"),
                config: root.join("config"),
            };
            let core = Core::open(paths).unwrap();
            Self { core, root }
        }

        fn save(&self, def: &Ritual) {
            self.core.db.lock().unwrap().execute(
                "INSERT INTO rituals (name, version, hash, def, created) VALUES (?1, 1, ?2, ?3, 0)",
                rusqlite::params![def.ritual, ritual::content_hash(def), serde_json::to_string(def).unwrap()],
            ).unwrap();
        }

        fn run(&self, def: Ritual, model: Option<&str>) -> Run {
            let id = new_id("s");
            self.core.insert_summon(
                &id,
                None,
                &def.ritual,
                1,
                &ritual::content_hash(&def),
                def.model.as_deref(),
                &json!({}),
                &def.essence,
                None,
            );
            Run {
                job_claim: None,
                core: self.core.clone(),
                root_id: id.clone(),
                fence_prefix: String::new(),
                id,
                inputs: json!({}),
                model_override: model.map(String::from),
                settings_override: BTreeMap::new(),
                meters: vec![Arc::new(Meter::new(&def.essence))],
                def,
                selected: Arc::new(Mutex::new(HashSet::new())),
                cancel: Arc::new(AtomicBool::new(false)),
                grants: Arc::new(Vec::new()),
                depth: 0,
                io: Arc::new(crate::io::Live),
                models_used: Arc::new(Mutex::new(BTreeSet::new())),
                root_ritual: String::new(),
                done: 0,
                total: 0,
            }
        }

        fn model_events(&self) -> Vec<Value> {
            let db = self.core.db.lock().unwrap();
            let mut query = db
                .prepare("SELECT data FROM events WHERE kind='model'")
                .unwrap();
            query
                .query_map([], |r| r.get::<_, String>(0))
                .unwrap()
                .map(|r| serde_json::from_str(&r.unwrap()).unwrap())
                .collect()
        }
    }

    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.root);
        }
    }

    fn definition(value: Value) -> Ritual {
        serde_json::from_value(value).unwrap()
    }

    fn simple_model(tokens: Option<u64>) -> Ritual {
        definition(json!({
            "ritual": "test-imp", "model": "mock/keyword",
            "essence": {"requests": 1, "tokens": tokens},
            "steps": [{"id": "answer", "model": {"prompt": "hello"}}],
            "return": {"answer": "${steps.answer.output}"},
        }))
    }

    #[test]
    fn overrides_reach_nested_children_and_their_records() {
        let f = Fixture::new();
        let leaf = definition(json!({
            "ritual": "leaf-imp", "model": "unsupported/must-not-run",
            "settings": {"temperature": 0.9},
            "steps": [{"id": "answer", "model": {"prompt": "hello"}}],
            "return": {"answer": "${steps.answer.output}"},
        }));
        let middle = definition(json!({
            "ritual": "middle-imp", "steps": [{"id": "leaf", "ritual": "leaf-imp", "version": 1}],
            "return": {"answer": "${steps.leaf.output.answer}"},
        }));
        f.save(&leaf);
        f.save(&middle);
        let parent = definition(json!({
            "ritual": "parent-imp", "steps": [{"id": "middle", "ritual": "middle-imp", "version": 1}],
            "return": {"answer": "${steps.middle.output.answer}"},
        }));
        let mut run = f.run(parent, Some("mock/keyword"));
        run.settings_override
            .insert("temperature".into(), json!(0.2));
        let fin = run.execute();
        assert_eq!(fin.outcome, Outcome::Returned, "{:?}", fin.error);
        assert_eq!(fin.result["answer"], "mock");
        let events = f.model_events();
        assert_eq!(events.len(), 1);
        assert_eq!(events[0]["model"], "mock/keyword");
        assert_eq!(events[0]["payload"]["options"]["temperature"], 0.2);
        let db = f.core.db.lock().unwrap();
        let wrong_children: u64 = db
            .query_row(
                "SELECT COUNT(*) FROM summons WHERE parent IS NOT NULL AND model != 'mock/keyword'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(wrong_children, 0);
    }

    #[test]
    fn a_pinned_step_model_still_wins_over_an_inherited_override() {
        let f = Fixture::new();
        let mut def = simple_model(None);
        def.steps[0].model.as_mut().unwrap().name = Some("mock/pinned".into());
        let mut run = f.run(def, Some("unsupported/must-not-run"));
        assert_eq!(run.execute().outcome, Outcome::Returned);
        assert_eq!(f.model_events()[0]["model"], "mock/pinned");
    }

    #[test]
    fn final_response_overrun_is_exhausted_and_keeps_actual_usage() {
        let f = Fixture::new();
        let mut run = f.run(simple_model(Some(1)), None);
        let fin = run.execute();
        assert_eq!(fin.outcome, Outcome::Exhausted);
        assert!(run.meters[0].tokens.load(Ordering::SeqCst) > 1);
        assert_eq!(run.meters[0].requests.load(Ordering::SeqCst), 1);
        assert_eq!(
            f.model_events().len(),
            1,
            "over-budget payload evidence must survive"
        );
        assert!(fin.result.get("answer").is_none());
    }

    #[test]
    fn ancestor_overrun_stops_dependent_operations() {
        let f = Fixture::new();
        let mut def = simple_model(Some(1000));
        def.steps.push(
            serde_json::from_value(json!({
                "id": "must-not-run", "operation": "mail.search", "arguments": {"folder": "Inbox"},
            }))
            .unwrap(),
        );
        let mut run = f.run(def, None);
        let ancestor = Arc::new(Meter::new(&ritual::Essence {
            tokens: Some(1),
            ..Default::default()
        }));
        run.meters.push(ancestor.clone());
        assert_eq!(run.execute().outcome, Outcome::Exhausted);
        assert!(ancestor.tokens.load(Ordering::SeqCst) > 1);
        let operations: u64 = f
            .core
            .db
            .lock()
            .unwrap()
            .query_row(
                "SELECT COUNT(*) FROM events WHERE kind IN ('op', 'op.refused')",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(operations, 0);
    }

    #[test]
    fn exact_token_boundary_can_return_but_cannot_admit_another_request() {
        let f = Fixture::new();
        let mut first = f.run(simple_model(None), None);
        assert_eq!(first.execute().outcome, Outcome::Returned);
        let used = first.meters[0].tokens.load(Ordering::SeqCst);
        let mut exact = f.run(simple_model(Some(used)), None);
        assert_eq!(exact.execute().outcome, Outcome::Returned);
        assert!(matches!(exact.meters[0].check(), Err(Fail::Exhausted(_))));
    }

    #[test]
    fn failed_attempts_consume_request_essence() {
        let f = Fixture::new();
        let mut run = f.run(simple_model(None), Some("unsupported/no-network"));
        assert_eq!(run.execute().outcome, Outcome::Failed);
        assert_eq!(run.meters[0].requests.load(Ordering::SeqCst), 1);
        run.model_override = Some("mock/keyword".into());
        assert_eq!(run.execute().outcome, Outcome::Exhausted);
        assert_eq!(f.model_events().len(), 0);
    }

    #[test]
    fn earliest_ancestor_deadline_and_smallest_remaining_tokens_win() {
        let f = Fixture::new();
        let mut run = f.run(simple_model(Some(100)), None);
        run.meters[0].tokens.store(20, Ordering::SeqCst);
        let ancestor = Arc::new(Meter {
            max_requests: None,
            max_tokens: Some(50),
            deadline: Some(Instant::now() + Duration::from_millis(50)),
            requests: AtomicU64::new(0),
            tokens: AtomicU64::new(30),
            ..Meter::new(&ritual::Essence::default())
        });
        run.meters.push(ancestor);
        let (timeout, tokens) = run.request_limits();
        assert!(timeout.unwrap() <= Duration::from_millis(50));
        assert_eq!(tokens, Some(20));
        let expired = Arc::new(Meter {
            max_requests: None,
            max_tokens: None,
            deadline: Some(Instant::now() - Duration::from_millis(1)),
            requests: AtomicU64::new(0),
            tokens: AtomicU64::new(0),
            ..Meter::new(&ritual::Essence::default())
        });
        run.meters.push(expired);
        assert!(matches!(run.check_live(), Err(Fail::Exhausted(_))));
        assert_eq!(run.execute().outcome, Outcome::Exhausted);
        assert!(f.model_events().is_empty());
    }
    #[test]
    fn interrupted_item_preserves_prior_effects() {
        let mut f = Fixture::new();
        let mailbox = f.root.join("mail");
        std::fs::create_dir_all(mailbox.join("Inbox")).unwrap();
        std::fs::write(mailbox.join("Inbox/001.eml"), "Subject: receipt\n\nbody").unwrap();
        Arc::get_mut(&mut f.core).unwrap().cfg.mail_root =
            Some(mailbox.to_string_lossy().into_owned());
        let def = definition(
            json!({"ritual":"partial","model":"mock/keyword","essence":{"requests":0},
            "requires":[{"reagent":"mail","action":"search"},{"reagent":"mail","action":"move"}],
            "steps":[{"id":"selected","operation":"mail.search","arguments":{"folder":"Inbox"}},
              {"id":"batch","each":"message in steps.selected.output","steps":[
                {"id":"move","operation":"mail.move","arguments":{"message":"${message.id}","folder":"Receipts"}},
                {"id":"infer","model":{"prompt":"receipt"}}]}]}),
        );
        let mut run = f.run(def.clone(), None);
        run.grants = Arc::new(
            def.requires
                .iter()
                .flat_map(ritual::Requirement::atoms)
                .collect(),
        );
        let fin = run.execute();
        assert_eq!(fin.outcome, Outcome::Exhausted);
        assert!(mailbox.join("Receipts/001.eml").exists());
        assert_eq!(fin.result["partial"]["batch"]["counts"]["remaining"], 1);
        assert_eq!(
            fin.result["partial"]["batch"]["items"][0]["interrupted"],
            true
        );
        assert_eq!(
            fin.result["partial"]["batch"]["items"][0]["effects"][0]["state"],
            "applied"
        );
    }

    #[test]
    fn ancestor_tool_and_item_limits_are_not_free() {
        let f = Fixture::new();
        let mut run = f.run(simple_model(None), None);
        let limit = ritual::Essence {
            tool_calls: Some(1),
            items: Some(1),
            ..Default::default()
        };
        run.meters.push(Arc::new(Meter::new(&limit)));
        assert!(run.admit_tools(2).is_err());
        assert_eq!(run.meters[0].tool_calls.load(Ordering::SeqCst), 0);
        run.admit_tools(1).unwrap();
        assert!(run.admit_tools(1).is_err());
        run.admit_item().unwrap();
        assert!(run.admit_item().is_err());
    }

    #[test]
    fn declared_return_schema_is_enforced() {
        let f = Fixture::new();
        let mut def = simple_model(None);
        def.output_schema = Some(json!({"type":"object","required":["missing"]}));
        assert_eq!(f.run(def, None).execute().outcome, Outcome::Failed);
    }
    #[test]
    fn deterministic_children_work_under_zero_model_budget() {
        let f = Fixture::new();
        let child = definition(
            json!({"ritual":"sum","reagents":{"data":{"select":["sum_integers"]}},"steps":[{"id":"s","operation":"data.sum_integers","arguments":{"values":[2,3]}}],"return":{"total":"${steps.s.output}"}}),
        );
        f.save(&child);
        let parent = definition(
            json!({"ritual":"parent","essence":{"requests":0},"steps":[{"id":"c","ritual":"sum","version":1}],"return":{"total":"${steps.c.output.total}"}}),
        );
        let mut run = f.run(parent, None);
        let done = run.execute();
        assert_eq!(done.outcome, Outcome::Returned);
        assert_eq!(done.result, json!({"total":5}));
        assert_eq!(run.meters[0].requests.load(Ordering::SeqCst), 0);
        assert!(f.model_events().is_empty());
    }
    #[test]
    fn job_guard_reaches_children_and_blocks_held_writes_before_petition_creation() {
        let f = Fixture::new();
        let child = definition(
            json!({"ritual":"writer","steps":[{"id":"write","operation":"mail.move","arguments":{"message":"001","folder":"Inbox"},"hold":true}]}),
        );
        f.save(&child);
        let parent =
            definition(json!({"ritual":"parent","steps":[{"id":"child","ritual":"writer"}]}));
        let manifest:crate::jobs::Manifest=serde_json::from_value(json!({"job":"guard-test","ritual":"parent","items":[{"id":"1","version":"v1","args":{}}]})).unwrap();
        let claim = {
            let mut db = f.core.db.lock().unwrap();
            crate::jobs::create(&mut db, &manifest, &json!({}), crate::util::now_ms()).unwrap();
            crate::jobs::claim(
                &mut db,
                "guard-test",
                "worker",
                1,
                60000,
                crate::util::now_ms(),
            )
            .unwrap()
            .remove(0)
        };
        let mut run = f.run(parent, None);
        run.job_claim = Some(claim);
        let done = run.execute();
        assert_eq!(done.outcome, Outcome::Failed);
        assert!(done.error.unwrap().contains("shared job workers cannot"));
        let n: i64 = f
            .core
            .db
            .lock()
            .unwrap()
            .query_row("SELECT COUNT(*) FROM petitions", [], |r| r.get(0))
            .unwrap();
        assert_eq!(n, 0);
    }

    #[test]
    fn abstention_cannot_flow_into_a_write() {
        let f = Fixture::new();
        let def = definition(json!({"ritual":"abstain","model":"mock/keyword","steps":[
            {"id":"a","model":{"prompt":"x","abstain_if":"true"}},
            {"id":"write","operation":"mail.move","arguments":{"message":"001","folder":"Inbox"},"hold":true}]}));
        let done = f.run(def, None).execute();
        assert_eq!(done.outcome, Outcome::Failed);
        assert!(done.error.unwrap().contains("abstained result"));
        let count: i64 = f
            .core
            .db
            .lock()
            .unwrap()
            .query_row("SELECT COUNT(*) FROM petitions", [], |r| r.get(0))
            .unwrap();
        assert_eq!(count, 0);
    }

    fn simulated_fixture(f: &mut Fixture, requires: Value) -> (Ritual, std::path::PathBuf) {
        let mailbox = f.root.join("mail");
        std::fs::create_dir_all(mailbox.join("Inbox")).unwrap();
        std::fs::write(mailbox.join("Inbox/001.eml"), "Subject: receipt\n\nbody").unwrap();
        Arc::get_mut(&mut f.core).unwrap().cfg.mail_root =
            Some(mailbox.to_string_lossy().into_owned());
        let def = definition(json!({"ritual":"held-sim","requires":requires,
            "steps":[{"id":"selected","operation":"mail.search","arguments":{"folder":"Inbox"}},
              {"id":"batch","each":"message in steps.selected.output","steps":[
                {"id":"move","operation":"mail.move","hold":true,
                 "arguments":{"message":"${message.id}","folder":"Receipts"},
                 "verify":{"observe":"mail.locate","arguments":{"message":"${message.id}"},"expect":{"folder":"Receipts"}}}]}]}));
        (def, mailbox)
    }

    fn simulated_run(f: &Fixture, def: Ritual) -> Run {
        let mut run = f.run(def.clone(), None);
        run.grants = Arc::new(
            def.requires
                .iter()
                .flat_map(ritual::Requirement::atoms)
                .collect(),
        );
        run.io = Arc::new(crate::io::Simulate::new(Arc::new(crate::io::Live)));
        run
    }

    fn petition_count(f: &Fixture) -> i64 {
        f.core
            .db
            .lock()
            .unwrap()
            .query_row("SELECT COUNT(*) FROM petitions", [], |r| r.get(0))
            .unwrap()
    }

    #[test]
    fn simulated_run_never_performs_or_holds_an_mcp_write() {
        let mut f = Fixture::new();
        let marker = f.root.join("server-started");
        Arc::get_mut(&mut f.core).unwrap().cfg.mcp_reagents.insert(
            "ext".into(),
            crate::util::McpReagentConfig {
                command: vec!["touch".into(), marker.to_string_lossy().into_owned()],
                cwd: None,
                env: Default::default(),
                tools: [(
                    "send".to_string(),
                    crate::util::McpToolConfig { effect: "write".into(), description: None },
                )]
                .into_iter()
                .collect(),
            },
        );
        let def = definition(json!({"ritual":"mcp-sim",
            "requires":[{"reagent":"ext","action":"send"}],
            "steps":[{"id":"send","operation":"ext.send","arguments":{"to":"x"}},
                     {"id":"held","operation":"ext.send","hold":true,"arguments":{"to":"y"}}]}));
        let mut run = simulated_run(&f, def);
        let id = run.id.clone();
        let fin = run.execute();
        assert_eq!(fin.outcome, Outcome::Returned, "{:?}", fin.error);
        assert!(!marker.exists(), "the MCP server was started");
        assert_eq!(petition_count(&f), 0);
        let fences: i64 = f
            .core
            .db
            .lock()
            .unwrap()
            .query_row("SELECT COUNT(*) FROM effect_fence WHERE summon=?1", [&id], |r| r.get(0))
            .unwrap();
        assert_eq!(fences, 0);
    }

    #[test]
    fn simulated_hold_files_no_petition_and_records_would_hold() {
        let mut f = Fixture::new();
        let (def, mailbox) = simulated_fixture(
            &mut f,
            json!([{"reagent":"mail","action":"search"},{"reagent":"mail","action":"locate"},
                   {"reagent":"mail","action":"move","on":{"folder":"Inbox"},"into":{"folder":["Receipts"]}}]),
        );
        let mut run = simulated_run(&f, def);
        let id = run.id.clone();
        let fin = run.execute();
        assert_eq!(fin.outcome, Outcome::Returned, "{:?}", fin.error);
        assert_eq!(petition_count(&f), 0);
        assert!(mailbox.join("Inbox/001.eml").exists());
        assert!(!mailbox.join("Receipts/001.eml").exists());
        let events: Vec<(String, String, Value)> = {
            let db = f.core.db.lock().unwrap();
            let mut q = db
                .prepare("SELECT kind,text,data FROM events WHERE summon=?1 ORDER BY seq")
                .unwrap();
            q.query_map([&id], |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, String>(2)?,
                ))
            })
            .unwrap()
            .map(|r| {
                let (k, t, d) = r.unwrap();
                (k, t, serde_json::from_str(&d).unwrap())
            })
            .collect()
        };
        assert!(events.iter().all(|e| e.0 != "held"));
        let op = events
            .iter()
            .find(|e| e.0 == "op" && e.2["operation"] == "mail.move")
            .expect("move op event");
        assert_eq!(op.2["state"], "simulated");
        assert_eq!(op.2["would_hold"], true);
        let check = events.iter().find(|e| e.0 == "check").expect("check event");
        assert_eq!(check.1, "move check skipped: simulated");
        assert!(events.iter().all(|e| !e.1.contains("verified")));
        let reads = events
            .iter()
            .find(|e| e.0 == "op" && e.2["operation"] == "mail.search")
            .unwrap();
        assert_eq!(reads.2["state"], "applied");
    }

    #[test]
    fn simulated_hold_still_refuses_an_ungranted_write() {
        let mut f = Fixture::new();
        let (def, mailbox) = simulated_fixture(
            &mut f,
            json!([{"reagent":"mail","action":"search"},{"reagent":"mail","action":"locate"}]),
        );
        let fin = simulated_run(&f, def).execute();
        assert_ne!(fin.outcome, Outcome::Returned);
        assert_eq!(petition_count(&f), 0);
        assert!(mailbox.join("Inbox/001.eml").exists());
        assert!(!mailbox.join("Receipts/001.eml").exists());
    }

    #[test]
    fn parallel_each_preserves_order_and_enforces_atomic_limits() {
        let f = Fixture::new();
        let def = definition(json!({
            "ritual": "par-mul",
            "reagents": {"data": {"select": ["arithmetic"]}},
            "essence": {"requests": 0, "tool_calls": 10, "time": "10s"},
            "steps": [{
                "id": "batch",
                "each": "x in [1, 2, 3, 4]",
                "parallel": 3,
                "steps": [{
                    "id": "mul",
                    "operation": "data.arithmetic",
                    "arguments": {"left": "${x}", "right": 10, "operator": "multiply"}
                }]
            }],
            "return": {"items": "${steps.batch.output}"}
        }));
        let mut run = f.run(def, None);
        let fin = run.execute();
        assert_eq!(fin.outcome, Outcome::Returned, "{:?}", fin.error);
        let items = fin.result["items"].as_array().unwrap();
        let outs: Vec<i64> = items
            .iter()
            .map(|it| it["outputs"]["mul"].as_i64().unwrap())
            .collect();
        assert_eq!(outs, vec![10, 20, 30, 40]);

        // Atomic limit: 4 items with parallel=4 and tool_calls=2 must never exceed 2 tool calls.
        let capped = definition(json!({
            "ritual": "par-cap",
            "reagents": {"data": {"select": ["arithmetic"]}},
            "essence": {"requests": 0, "tool_calls": 2, "time": "10s"},
            "steps": [{
                "id": "batch",
                "each": "x in [1, 2, 3, 4]",
                "parallel": 4,
                "steps": [{
                    "id": "mul",
                    "operation": "data.arithmetic",
                    "arguments": {"left": "${x}", "right": 10, "operator": "multiply"}
                }]
            }]
        }));
        let mut run2 = f.run(capped, None);
        let fin2 = run2.execute();
        assert_eq!(fin2.outcome, Outcome::Exhausted);
        assert!(run2.meters[0].tool_calls.load(Ordering::SeqCst) <= 2);
    }

    #[test]
    fn retry_consumes_essence_and_fallback_recovers_after_exhausting_retries() {
        let f = Fixture::new();
        let def = definition(json!({
            "ritual": "retry-fb",
            "reagents": {"data": {"select": ["parse_json", "arithmetic"]}},
            "essence": {"requests": 0, "tool_calls": 5, "time": "10s"},
            "steps": [{
                "id": "parsed",
                "operation": "data.parse_json",
                "arguments": {"text": "not json"},
                "retry": 2,
                "fallback": [{
                    "id": "fb",
                    "operation": "data.arithmetic",
                    "arguments": {"left": 40, "right": 2, "operator": "add"}
                }]
            }],
            "return": {"val": "${steps.parsed.output}"}
        }));
        let mut run = f.run(def, None);
        let fin = run.execute();
        assert_eq!(fin.outcome, Outcome::Returned, "{:?}", fin.error);
        assert_eq!(fin.result["val"], json!(42));
        // 3 attempts (1 + retry 2) + 1 fallback step = 4 tool calls.
        assert_eq!(run.meters[0].tool_calls.load(Ordering::SeqCst), 4);
    }

    #[test]
    fn preflight_prompt_sizing_rejects_oversized_prompt_before_dispatch() {
        let f = Fixture::new();
        let long_prompt = "x".repeat(400);
        let def = definition(json!({
            "ritual": "oversized-prompt",
            "model": "mock/keyword",
            "essence": {"requests": 1, "tokens": 20, "time": "10s"},
            "steps": [{
                "id": "ask",
                "model": {
                    "prompt": long_prompt
                }
            }],
            "return": {"ans": "${steps.ask.output}"}
        }));
        let mut run = f.run(def, None);
        let fin = run.execute();
        assert_eq!(fin.outcome, Outcome::Exhausted, "{:?}", fin.error);
        assert!(
            fin.error
                .as_deref()
                .is_some_and(|e| e.contains("estimated prompt size")),
            "{:?}",
            fin.error
        );
        // Refused before admitting a model request.
        assert_eq!(run.meters[0].requests.load(Ordering::SeqCst), 0);
        assert_eq!(run.meters[0].tokens.load(Ordering::SeqCst), 0);
    }

    // ---------------------------------------------------------------- sight

    use crate::reagents::sight_reagent::{issue_key, tests::{JPEG_1X1, PNG_1X1}};

    /// A ritual whose `look` model step sees `images`, after the steps in
    /// `before` (default: read `a.png` beneath `root`).
    fn sight_ritual(root: &str, model: &str, before: Option<Value>, images: Value) -> Ritual {
        let before = before.unwrap_or_else(|| {
            json!([{"id":"page","operation":"sight.read_image","arguments":{"root":root,"path":"a.png"}}])
        });
        let mut steps = before.as_array().unwrap().clone();
        steps.push(json!({"id":"look","model":{"prompt":"what is this","images":images}}));
        definition(json!({
            "ritual":"sight-imp","model":model,
            "inputs":{"h":{"type":"object"},"hs":{"type":"list"}},
            "requires":[
                {"reagent":"sight","action":"read_image","on":{"root":root}},
                {"reagent":"file","action":"write_text","on":{"root":root}}
            ],
            "steps":steps,
            "return":{"seen":"${steps.look.output}"},
        }))
    }

    /// A `scans` directory holding `a.png` and `b.jpg`.
    fn image_dir(f: &Fixture) -> (PathBuf, String) {
        let dir = f.root.join("scans");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("a.png"), PNG_1X1).unwrap();
        std::fs::write(dir.join("b.jpg"), JPEG_1X1).unwrap();
        let root = dir.to_string_lossy().into_owned();
        (dir, root)
    }

    fn granted(f: &Fixture, def: Ritual) -> Run {
        let mut run = f.run(def.clone(), None);
        run.grants = Arc::new(def.requires.iter().flat_map(ritual::Requirement::atoms).collect());
        run
    }

    fn handle(root: &str, path: &str, data: &[u8]) -> Value {
        json!({"kind":"image","root":root,"path":path,"sha256":crate::util::sha256_hex(data),
               "mime":"image/png","bytes":data.len(),"width":1,"height":1})
    }

    fn all_event_text(f: &Fixture) -> String {
        let db = f.core.db.lock().unwrap();
        let mut q = db.prepare("SELECT data FROM events").unwrap();
        q.query_map([], |r| r.get::<_, String>(0))
            .unwrap()
            .map(Result::unwrap)
            .collect::<Vec<_>>()
            .join("\n")
    }

    fn requests(run: &Run) -> u64 {
        run.meters[0].requests.load(Ordering::SeqCst)
    }

    #[test]
    fn image_step_sends_parts_and_records_digests() {
        let f = Fixture::new();
        let (_, root) = image_dir(&f);
        let before = json!([
            {"id":"page","operation":"sight.read_image","arguments":{"root":&root,"path":"a.png"}},
            {"id":"photo","operation":"sight.read_image","arguments":{"root":&root,"path":"b.jpg"}}
        ]);
        let def = sight_ritual(&root, "mock/keyword", Some(before), json!(["${steps.page.output}", "${steps.photo.output}"]));
        let mut run = granted(&f, def);
        let fin = run.execute();
        assert_eq!(fin.outcome, Outcome::Returned, "{:?}", fin.error);
        assert_eq!(requests(&run), 1);
        let (png, jpg) = (crate::util::sha256_hex(PNG_1X1), crate::util::sha256_hex(JPEG_1X1));
        let ev = &f.model_events()[0];
        assert_eq!(ev["payload"]["images"], json!([png, jpg]));
        let parts: Vec<&Value> = ev["manifest"].as_array().unwrap().iter().filter(|p| p["part"] == "image").collect();
        assert_eq!(parts.len(), 2);
        assert_eq!(
            *parts[0],
            json!({"part":"image","sha256":png,"path":"a.png","mime":"image/png","width":1,"height":1,"bytes":PNG_1X1.len()})
        );
        assert_eq!(parts[1]["mime"], "image/jpeg");
        use base64::Engine;
        let text = all_event_text(&f);
        for data in [PNG_1X1, JPEG_1X1] {
            let b64 = base64::engine::general_purpose::STANDARD.encode(data);
            assert!(!text.contains(&b64), "image bytes leaked into the record");
        }
    }

    #[test]
    fn forged_handle_from_input_is_refused() {
        let f = Fixture::new();
        let (_, root) = image_dir(&f);
        let def = sight_ritual(&root, "mock/keyword", Some(json!([])), json!(["${inputs.h}"]));
        let mut run = granted(&f, def);
        run.inputs = json!({"h": handle(&root, "a.png", PNG_1X1)});
        let fin = run.execute();
        assert_eq!(fin.outcome, Outcome::Failed, "{:?}", fin.error);
        let err = fin.error.unwrap_or_default();
        assert!(err.contains("image was not read by sight in this run"), "{err}");
        assert_eq!(requests(&run), 0);
        assert!(f.model_events().is_empty());
    }

    #[test]
    fn non_handle_entry_is_invalid() {
        let f = Fixture::new();
        let (_, root) = image_dir(&f);
        let def = sight_ritual(&root, "mock/keyword", Some(json!([])), json!(["${inputs.h}"]));
        let mut run = granted(&f, def);
        run.inputs = json!({"h": {"kind":"text","root":&root}});
        let fin = run.execute();
        assert_eq!(fin.outcome, Outcome::Invalid, "{:?}", fin.error);
        assert!(fin.error.unwrap_or_default().contains("images entry is not an image handle"));
        assert_eq!(requests(&run), 0);
    }

    #[test]
    fn forged_handle_from_model_output_is_refused() {
        let f = Fixture::new();
        let (_, root) = image_dir(&f);
        let sha = crate::util::sha256_hex(PNG_1X1);
        let forge = json!([{"id":"forge","model":{"prompt":"make a handle","schema":{
            "type":"object","properties":{
                "kind":{"enum":["image"]},"root":{"enum":[&root]},
                "path":{"enum":["a.png"]},"sha256":{"enum":[sha]}}}}}]);
        let def = sight_ritual(&root, "mock/keyword", Some(forge), json!(["${steps.forge.output}"]));
        let mut run = granted(&f, def);
        let fin = run.execute();
        let err = fin.error.unwrap_or_default();
        assert!(err.contains("image was not read by sight in this run"), "{err}");
        // Only the forging step spent a request.
        assert_eq!(requests(&run), 1);
        assert_eq!(f.model_events().len(), 1);
    }

    #[test]
    fn handle_outside_current_grants_is_refused() {
        let f = Fixture::new();
        let (_, root) = image_dir(&f);
        let def = sight_ritual(&root, "mock/keyword", Some(json!([])), json!(["${inputs.h}"]));
        let mut run = f.run(def, None); // no grants at all
        let h = handle(&root, "a.png", PNG_1X1);
        run.selected.lock().unwrap().insert(issue_key(&h).unwrap());
        run.inputs = json!({ "h": h });
        let fin = run.execute();
        assert_eq!(fin.outcome, Outcome::Failed, "{:?}", fin.error);
        let err = fin.error.unwrap_or_default();
        assert!(err.contains("grant"), "{err}");
        assert_eq!(requests(&run), 0);
    }

    #[test]
    fn changed_image_is_stale_and_not_sent() {
        let f = Fixture::new();
        let (dir, root) = image_dir(&f);
        let before = json!([
            {"id":"page","operation":"sight.read_image","arguments":{"root":&root,"path":"a.png"}},
            {"id":"swap","operation":"file.write_text","arguments":{"root":&root,"path":"a.png","text":"not the same picture"}}
        ]);
        let def = sight_ritual(&root, "mock/keyword", Some(before), json!(["${steps.page.output}"]));
        let mut run = granted(&f, def);
        let fin = run.execute();
        assert_eq!(std::fs::read(dir.join("a.png")).unwrap(), b"not the same picture");
        let err = fin.error.unwrap_or_default();
        assert!(err.contains("stale image"), "{err}");
        assert_eq!(requests(&run), 0);
        assert!(f.model_events().is_empty());
    }

    #[test]
    fn symlinked_image_is_refused_at_send() {
        let f = Fixture::new();
        let (dir, root) = image_dir(&f);
        std::os::unix::fs::symlink(dir.join("a.png"), dir.join("link.png")).unwrap();
        let def = sight_ritual(&root, "mock/keyword", Some(json!([])), json!(["${inputs.h}"]));
        let mut run = granted(&f, def);
        // As if issued for a regular file later replaced by a symlink.
        let h = handle(&root, "link.png", PNG_1X1);
        run.selected.lock().unwrap().insert(issue_key(&h).unwrap());
        run.inputs = json!({ "h": h });
        let fin = run.execute();
        assert_eq!(fin.outcome, Outcome::Failed, "{:?}", fin.error);
        let err = fin.error.unwrap_or_default();
        assert!(err.contains("stale image"), "{err}");
        assert_eq!(requests(&run), 0);
    }

    #[test]
    fn too_many_images_refused() {
        let f = Fixture::new();
        let (_, root) = image_dir(&f);
        // A single reference to a list is counted after flattening.
        let def = sight_ritual(&root, "mock/keyword", Some(json!([])), json!("${inputs.hs}"));
        let mut run = granted(&f, def);
        let h = handle(&root, "a.png", PNG_1X1);
        run.inputs = json!({ "hs": vec![h; 9] });
        let fin = run.execute();
        assert_eq!(fin.outcome, Outcome::Invalid, "{:?}", fin.error);
        assert!(fin.error.unwrap_or_default().contains("at most 8"));
        assert_eq!(requests(&run), 0);
    }

    #[test]
    fn config_lowers_limits() {
        let mut f = Fixture::new();
        let (_, root) = image_dir(&f);
        Arc::get_mut(&mut f.core).unwrap().cfg.sight = Some(crate::util::SightConfig {
            max_bytes: None,
            max_per_step: Some(1),
        });
        let two = json!(["${steps.page.output}", "${steps.page.output}"]);
        let mut run = granted(&f, sight_ritual(&root, "mock/keyword", None, two));
        let fin = run.execute();
        assert_eq!(fin.outcome, Outcome::Invalid, "{:?}", fin.error);
        assert!(fin.error.unwrap_or_default().contains("at most 1"));
        assert_eq!(requests(&run), 0);
        drop(run);

        Arc::get_mut(&mut f.core).unwrap().cfg.sight = Some(crate::util::SightConfig {
            max_bytes: Some(10),
            max_per_step: None,
        });
        let mut run = granted(&f, sight_ritual(&root, "mock/keyword", None, json!(["${steps.page.output}"])));
        let fin = run.execute();
        // The handle already records more bytes than the lowered limit.
        assert_eq!(fin.outcome, Outcome::Invalid, "{:?}", fin.error);
        assert!(fin.error.unwrap_or_default().contains("10-byte limit"));
        assert_eq!(requests(&run), 0);
    }

    #[test]
    fn oversize_handle_is_invalid_without_reading() {
        let mut f = Fixture::new();
        let (_, root) = image_dir(&f);
        Arc::get_mut(&mut f.core).unwrap().cfg.sight = Some(crate::util::SightConfig {
            max_bytes: Some(10),
            max_per_step: None,
        });
        let def = sight_ritual(&root, "mock/keyword", Some(json!([])), json!(["${inputs.h}"]));
        let mut run = granted(&f, def);
        // No such file: reading it would report a stale image instead.
        let h = handle(&root, "missing.png", PNG_1X1);
        run.selected.lock().unwrap().insert(issue_key(&h).unwrap());
        run.inputs = json!({ "h": h });
        let fin = run.execute();
        assert_eq!(fin.outcome, Outcome::Invalid, "{:?}", fin.error);
        let err = fin.error.unwrap_or_default();
        assert!(err.contains("10-byte limit") && !err.contains("stale"), "{err}");
        assert_eq!(requests(&run), 0);
    }

    #[test]
    fn grown_image_is_stale_not_exhausted() {
        let mut f = Fixture::new();
        let (_, root) = image_dir(&f);
        Arc::get_mut(&mut f.core).unwrap().cfg.sight = Some(crate::util::SightConfig {
            max_bytes: Some(100),
            max_per_step: None,
        });
        let before = json!([
            {"id":"page","operation":"sight.read_image","arguments":{"root":&root,"path":"a.png"}},
            {"id":"swap","operation":"file.write_text","arguments":{"root":&root,"path":"a.png","text":"x".repeat(200)}}
        ]);
        let def = sight_ritual(&root, "mock/keyword", Some(before), json!(["${steps.page.output}"]));
        let mut run = granted(&f, def);
        let fin = run.execute();
        assert_eq!(fin.outcome, Outcome::Failed, "{:?}", fin.error);
        assert!(fin.error.unwrap_or_default().contains("stale image"));
        assert_eq!(requests(&run), 0);
    }

    #[test]
    fn keep_going_batch_continues_past_a_stale_image() {
        let mut f = Fixture::new();
        let (_, root) = image_dir(&f);
        Arc::get_mut(&mut f.core).unwrap().cfg.sight = Some(crate::util::SightConfig {
            max_bytes: Some(1000),
            max_per_step: None,
        });
        // Each item reads its file, then a.png grows past the limit: the
        // a.png item goes stale (not exhausted), the b.jpg item is still sent.
        let def = definition(json!({
            "ritual":"sight-batch","model":"mock/keyword",
            "inputs":{"files":{"type":"list"}},
            "requires":[
                {"reagent":"sight","action":"read_image","on":{"root":&root}},
                {"reagent":"file","action":"write_text","on":{"root":&root}}
            ],
            "steps":[{"id":"batch","each":"f in inputs.files","on_failure":"continue","steps":[
                {"id":"page","operation":"sight.read_image","arguments":{"root":&root,"path":"${f}"}},
                {"id":"swap","operation":"file.write_text","arguments":{"root":&root,"path":"a.png","text":"x".repeat(2000)}},
                {"id":"look","model":{"prompt":"what is this","images":["${steps.page.output}"]}}
            ]}],
            "return":{"batch":"${steps.batch.output}"},
        }));
        let mut run = granted(&f, def);
        run.inputs = json!({"files":["a.png","b.jpg"]});
        let fin = run.execute();
        assert_eq!(requests(&run), 1, "{:?} {}", fin.error, fin.result);
        assert_eq!(f.model_events().len(), 1);
        let items = &fin.result["batch"];
        assert_eq!(items[0]["status"], "failed", "{}", fin.result);
        assert!(items[0]["error"].as_str().unwrap_or_default().contains("stale image"), "{}", fin.result);
        assert_eq!(items[1]["status"], "returned", "{}", fin.result);
        assert_eq!(f.model_events()[0]["payload"]["images"], json!([crate::util::sha256_hex(JPEG_1X1)]));
    }

    #[test]
    fn oversize_image_in_each_continue_is_invalid_and_batch_goes_on() {
        let mut f = Fixture::new();
        let (dir, root) = image_dir(&f);
        std::fs::write(dir.join("big.png"), vec![0u8; 2000]).unwrap();
        Arc::get_mut(&mut f.core).unwrap().cfg.sight = Some(crate::util::SightConfig {
            max_bytes: Some(1000),
            max_per_step: None,
        });
        let def = definition(json!({
            "ritual":"sight-batch","model":"mock/keyword",
            "inputs":{"files":{"type":"list"}},
            "requires":[{"reagent":"sight","action":"read_image","on":{"root":&root}}],
            "steps":[{"id":"batch","each":"f in inputs.files","on_failure":"continue","steps":[
                {"id":"page","operation":"sight.read_image","arguments":{"root":&root,"path":"${f}"}},
                {"id":"look","model":{"prompt":"what is this","images":["${steps.page.output}"]}}
            ]}],
            "return":{"batch":"${steps.batch.output}"},
        }));
        let mut run = granted(&f, def);
        run.inputs = json!({"files":["big.png","b.jpg"]});
        let fin = run.execute();
        assert_eq!(fin.outcome, Outcome::Returned, "{:?} {}", fin.error, fin.result);
        let items = &fin.result["batch"];
        assert_eq!(items[0]["status"], "invalid", "{}", fin.result);
        assert!(items[0]["error"].as_str().unwrap_or_default().contains("1000-byte limit"), "{}", fin.result);
        assert_eq!(items[1]["status"], "returned", "{}", fin.result);
        assert_eq!(requests(&run), 1);
    }

    #[test]
    fn mock_drill_with_images_needs_no_backend() {
        let mut f = Fixture::new();
        let (_, root) = image_dir(&f);
        // Nothing listens here: any vision probe or chat would fail the run.
        Arc::get_mut(&mut f.core).unwrap().cfg.ollama_url = Some("http://127.0.0.1:9".into());
        let def = sight_ritual(&root, "ollama/llava", None, json!(["${steps.page.output}"]));
        let mut run = granted(&f, def);
        let answers = BTreeMap::from([("look".to_string(), vec![json!("a page")])]);
        run.io = Arc::new(crate::io::Simulate::new(Arc::new(crate::io::Scripted::new(
            answers,
            Arc::new(crate::io::Live),
        ))));
        let fin = run.execute();
        assert_eq!(fin.outcome, Outcome::Returned, "{:?}", fin.error);
        assert_eq!(fin.result["seen"], "a page");
    }

    #[test]
    fn step_without_images_unchanged() {
        let f = Fixture::new();
        let mut run = f.run(simple_model(None), None);
        assert_eq!(run.execute().outcome, Outcome::Returned);
        let ev = &f.model_events()[0];
        assert_eq!(
            ev["payload"],
            json!({"backend":"mock","model":"keyword","system":"","user":"hello","format":null,"options":{}})
        );
        assert_eq!(ev["manifest"], json!([{"part":"task","chars":5}]));
    }
}
