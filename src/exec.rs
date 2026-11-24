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
/// Cleanup time when a ritual does not set `essence.cleanup_time`.
pub const DEFAULT_CLEANUP_MS: u64 = 30_000;
/// The most `essence.cleanup_time` may ask for.
pub const MAX_CLEANUP_MS: u64 = 600_000;

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
    /// Steps that were not pure (reads, writes, model calls) started under
    /// this meter. Only the root meter, which every child and worker
    /// shares, is counted; see `Run::impure_count`.
    pub impure: AtomicU64,
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
            impure: AtomicU64::new(0),
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
    /// Authenticated actor context for pre-execution RBAC reagent checks.
    pub actor: Option<crate::core::rbac::ActorContext>,
    pub done: u64,
    pub total: u64,
    /// What the ritual's `finally` steps did, once they have run.
    pub finally_report: Option<Value>,
}

/// At most `max` characters of `s`, marked when cut.
fn clip(s: &str, max: usize) -> String {
    if s.chars().count() > max {
        format!("{}…", s.chars().take(max - 1).collect::<String>())
    } else {
        s.to_string()
    }
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

    /// Count a step that is not pure. Work done since a count was taken is
    /// recomputable (and its bookkeeping need not be synced on its own)
    /// only while the count is unchanged.
    fn mark_impure(&self) {
        if let Some(root) = self.meters.last() {
            root.impure.fetch_add(1, Ordering::SeqCst);
        }
    }

    fn impure_count(&self) -> u64 {
        self.meters.last().map_or(0, |m| m.impure.load(Ordering::SeqCst))
    }

    /// A record of recomputable work; see `Core::emit_with`.
    fn emit_light(&self, kind: &str, text: impl Into<String>, data: Value) {
        self.core.emit_with(&self.id, kind, &text.into(), data, false);
    }

    /// Pre-execution RBAC guard: verifies that the invoking actor's active roles
    /// permit the requested reagent operation before any side effects, probes,
    /// petitions, or fences occur. Denials are recorded to the tamper-evident
    /// `audit_log` and emitted as `rbac.denied` events.
    fn guard_reagent_rbac(&self, sid: &str, op: &str, effect: &str) -> Result<(), Fail> {
        use crate::core::rbac::{PermissionAction, RbacDecision};
        let action = match effect {
            "pure" | "read" => PermissionAction::Read,
            _ => PermissionAction::Write,
        };
        match self
            .core
            .authorize_reagent(self.actor.as_ref(), op, action)
        {
            RbacDecision::Allow { .. } => Ok(()),
            RbacDecision::Deny { reason } => {
                let actor = self
                    .actor
                    .as_ref()
                    .map(|a| a.actor_id.as_str())
                    .unwrap_or("anonymous");
                self.core.record_audit_as(
                    actor,
                    "rbac.reagent_denied",
                    op,
                    "denied",
                    json!({
                        "summon": self.id,
                        "ritual": self.root_ritual,
                        "step": sid,
                        "effect": effect,
                        "reason": reason,
                    }),
                );
                self.emit(
                    "rbac.denied",
                    format!("{sid}: {op} denied by RBAC ({reason})"),
                    json!({"step": sid, "operation": op, "actor": actor, "reason": reason}),
                );
                Err(Fail::Denied(format!(
                    "actor `{actor}` is not permitted to invoke `{op}`: {reason}"
                )))
            }
        }
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
            actor: self.actor.clone(),
            done: 0,
            total: 0,
            finally_report: None,
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

    /// Record what a top-level step added to the frame: one row per step
    /// id (`top#ID`), so a long ritual's checkpoints grow linearly.
    /// `durable: false` after a step that did only pure work, which a
    /// revive recomputes if this checkpoint is lost. Only the last row needs
    /// the sync: it syncs the earlier ones with it.
    fn checkpoint_top(&self, frame: &Frame, step: &Step, durable: bool) {
        let mut ids = Vec::new();
        frame_ids(step, &mut ids);
        let rows: Vec<(&String, &StepRecord)> =
            ids.iter().filter_map(|id| frame.steps.get_key_value(id)).collect();
        for (n, (id, record)) in rows.iter().enumerate() {
            self.core.save_checkpoint_with(
                &self.id,
                &format!("top#{id}"),
                &json!(record),
                durable && n + 1 == rows.len(),
            );
        }
    }

    pub fn execute(&mut self) -> Finished {
        // Blocking work inside steps (commands, servers, model requests)
        // watches this run's cancel signal.
        let _cancel = crate::cancel::enter(self.cancel.clone());
        let mut scope = Scope {
            inputs: self.inputs.clone(),
            frames: vec![Frame::default()],
        };
        if self.depth == 0 {
            match self.core.load_checkpoints(&self.id) {
                Ok(map) => {
                    let (steps, abstained) = checkpointed_top(&map);
                    scope.frames[0].steps = steps;
                    scope.frames[0].abstained = abstained;
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
        let fin = self.finish_main(ran, &scope);
        if self.def.finally.is_empty() {
            return fin;
        }
        self.run_finally(fin, &mut scope)
    }

    /// Per-item accounting from any iteration, so a partial batch can never
    /// present itself as a complete one. Completed child rituals and model
    /// calls are kept too: they are the costly work, and a cheap later step
    /// running out must not throw them away.
    fn partial(scope: &Scope) -> Map<String, Value> {
        scope.frames[0]
            .steps
            .iter()
            .filter_map(|(k, v)| match v.kind {
                StepKind::Each => Some((k.clone(), json!({"items": v.output, "counts": v.counts}))),
                StepKind::Ritual | StepKind::Model => {
                    Some((k.clone(), json!({"status": v.status, "output": v.output})))
                }
                _ => None,
            })
            .collect()
    }

    fn finish_main(&self, ran: Result<(), Fail>, scope: &Scope) -> Finished {
        let partial = Self::partial(scope);
        match ran {
            Ok(()) => {
                let ret = Value::Object(
                    self.def
                        .ret
                        .iter()
                        .map(|(k, v)| (k.clone(), v.clone()))
                        .collect(),
                );
                let rendered = ritual::render_value(&ret, scope).and_then(|result| {
                    if let Some(schema) = &self.def.output_schema {
                        ritual::check_schema(schema, &result, "return")?;
                    }
                    Ok(result)
                });
                match rendered {
                    Ok(result) => Finished {
                        outcome: if scope.frames[0].abstained {
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

    /// Run the ritual's `finally` steps once the main steps have ended,
    /// however they ended. Cleanup runs under its own limits: a cancel flag
    /// of its own (a stopped run still cleans up), `essence.cleanup_time`
    /// counted from now instead of what was left of `time`, a fresh
    /// allowance of tool calls and items, and only the model budget the main
    /// steps left. What it uses is added to the run's totals afterwards.
    /// A cleanup failure fails a run that returned; any other outcome is
    /// kept, and the cleanup's own outcome is in `finally_report`.
    fn run_finally(&mut self, fin: Finished, scope: &mut Scope) -> Finished {
        let started = Instant::now();
        let window = self
            .def
            .essence
            .cleanup_time
            .as_deref()
            .and_then(|t| parse_duration(t).ok())
            .unwrap_or(DEFAULT_CLEANUP_MS)
            .min(MAX_CLEANUP_MS);
        let root = self.meters[0].clone();
        let remaining = |limit: fn(&Meter) -> Option<u64>, used: fn(&Meter) -> u64| {
            self.meters
                .iter()
                .filter_map(|m| limit(m).map(|l| l.saturating_sub(used(m))))
                .min()
        };
        let load = |a: &AtomicU64| a.load(Ordering::SeqCst);
        let start = (load(&root.requests), load(&root.tokens), load(&root.cost_microusd), load(&root.impure));
        let cleanup = Arc::new(Meter {
            max_requests: remaining(|m| m.max_requests, |m| m.requests.load(Ordering::SeqCst)).map(|r| start.0 + r),
            max_tokens: remaining(|m| m.max_tokens, |m| m.tokens.load(Ordering::SeqCst)).map(|r| start.1 + r),
            max_cost_microusd: remaining(|m| m.max_cost_microusd, |m| m.cost_microusd.load(Ordering::SeqCst)).map(|r| start.2 + r),
            max_tool_calls: self.def.essence.tool_calls,
            max_items: self.def.essence.items,
            tool_calls: AtomicU64::new(0),
            items: AtomicU64::new(0),
            deadline: Some(started + Duration::from_millis(window)),
            requests: AtomicU64::new(start.0),
            tokens: AtomicU64::new(start.1),
            cost_microusd: AtomicU64::new(start.2),
            cost_known: AtomicBool::new(root.cost_known.load(Ordering::SeqCst)),
            impure: AtomicU64::new(start.3),
        });
        let meters = std::mem::replace(&mut self.meters, vec![cleanup.clone()]);
        let cancel = std::mem::replace(&mut self.cancel, Arc::new(AtomicBool::new(false)));
        let ran = {
            let _signal = crate::cancel::enter(self.cancel.clone());
            self.emit(
                "finally",
                format!("cleanup: {} step(s), {}s at most", self.def.finally.len(), window / 1000),
                json!({"main_outcome": fin.outcome, "cleanup_time_ms": window}),
            );
            // Anything that never ran reads as skipped, so a cleanup step can
            // test `steps.X.status` for a step the failure never reached.
            scope.frames.truncate(1);
            if let Ok(main) = plan::compile(&self.def.steps) {
                let mut ids = Vec::new();
                main.iter().for_each(|s| frame_ids(s, &mut ids));
                for id in ids {
                    scope.frames[0].steps.entry(id).or_insert_with(|| {
                        StepRecord::new(StepStatus::Skipped, StepKind::Skipped, Value::Null)
                    });
                }
            }
            plan::compile(&self.def.finally)
                .map_err(Fail::Failed)
                .and_then(|steps| self.run_steps(&steps, scope))
        };
        self.meters = meters;
        self.cancel = cancel;
        // Cleanup's usage belongs to the run and to every ancestor.
        let delta = |a: &AtomicU64, from: u64| a.load(Ordering::SeqCst).saturating_sub(from);
        let (dr, dt, dc) = (delta(&cleanup.requests, start.0), delta(&cleanup.tokens, start.1), delta(&cleanup.cost_microusd, start.2));
        for m in &self.meters {
            m.requests.fetch_add(dr, Ordering::SeqCst);
            m.tokens.fetch_add(dt, Ordering::SeqCst);
            m.cost_microusd.fetch_add(dc, Ordering::SeqCst);
            m.tool_calls.fetch_add(load(&cleanup.tool_calls), Ordering::SeqCst);
            m.items.fetch_add(load(&cleanup.items), Ordering::SeqCst);
            if !cleanup.cost_known.load(Ordering::SeqCst) {
                m.cost_known.store(false, Ordering::SeqCst);
            }
        }
        self.meters[0].impure.fetch_add(delta(&cleanup.impure, start.3), Ordering::SeqCst);
        if dr > 0 {
            let cost = self.meters[0].cost_known.load(Ordering::SeqCst).then(|| load(&self.meters[0].cost_microusd));
            self.core.usage(&self.id, load(&self.meters[0].requests), load(&self.meters[0].tokens), cost);
        }
        let mut finally_ids = Vec::new();
        if let Ok(steps) = plan::compile(&self.def.finally) {
            steps.iter().for_each(|s| frame_ids(s, &mut finally_ids));
        }
        let ran_ids: Vec<&String> = finally_ids
            .iter()
            .filter(|id| scope.frames[0].steps.get(*id).is_some_and(|r| r.status != StepStatus::Skipped))
            .collect();
        let (outcome, error) = match &ran {
            Ok(()) => (Outcome::Returned, None),
            Err(f) => (f.outcome(), Some(f.message())),
        };
        let report = json!({
            "outcome": outcome, "error": error, "ran": ran_ids,
            "ms": started.elapsed().as_millis() as u64,
        });
        self.emit(
            "finally.end",
            match &error {
                None => format!("cleanup returned: {} step(s) ran", ran_ids.len()),
                Some(e) => format!("cleanup {}: {e}", outcome.as_str()),
            },
            report.clone(),
        );
        self.finally_report = Some(report);
        match ran {
            Err(f) if fin.outcome == Outcome::Returned => Finished {
                outcome: f.outcome(),
                result: json!({"partial": Self::partial(scope), "return": fin.result}),
                error: Some(format!("finally: {}", f.message())),
            },
            _ => fin,
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
            let impure_before = self.impure_count();
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
                    mark_skipped(skip, &mut scope.frames.last_mut().unwrap().steps);
                    self.run_steps(run, scope)?;
                }
            }
            if top_level {
                self.checkpoint_top(&scope.frames[0], s, self.impure_count() != impure_before);
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
        if effect != "pure" {
            self.mark_impure();
        }
        reagent::check_job_effect(self.job_claim.is_some(), effect)?;
        self.guard_reagent_rbac(sid, op, effect)?;
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
        let pure = effect == "pure";
        // A pure step has nothing to fence or reconcile, so it needs no
        // intent record ahead of its result.
        if !pure {
            self.emit(
                "op.intent",
                format!("{sid}: dispatch {op}"),
                json!({"step":sid,"operation":op,"arguments":args,"effect":effect,"fence_key":fence_key}),
            );
        }
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
                let is_cmd = op.starts_with("cmd.") && tool != "list";
                let shown = match (&out, tool) {
                    (Value::Array(a), _) => format!("{} items", a.len()),
                    _ if is_cmd => format!(
                        "exit {}, {} chars",
                        out["code"],
                        out["stdout"].as_str().map(|t| t.chars().count()).unwrap_or(0)
                    ),
                    (_, "read") => format!(
                        "{} chars",
                        out["text"].as_str().map(|t| t.chars().count()).unwrap_or(0)
                    ),
                    _ => brief(&out),
                };
                let simulated = matches!(evidence, Evidence::Simulated { .. });
                let state = if simulated { "simulated" } else { "applied" };
                let text = format!("{op} {} → {shown}", brief(&args));
                if pure {
                    // Deterministic and recomputable: large values are kept
                    // by digest and size, and the record skips its fsync.
                    let mut ev = serde_json::to_value(&evidence).expect("evidence serializes");
                    if let Some(o) = ev.get_mut("observed") {
                        *o = digest_large(o);
                    }
                    self.emit_light(
                        "op",
                        text,
                        json!({"step":sid,"operation": op, "arguments": digest_large(&args), "effect": effect,"state":state,"evidence": ev}),
                    );
                    return Ok(out);
                }
                let mut data = json!({"step":sid,"operation": op, "arguments": args, "effect": effect,"state":state,"evidence":evidence});
                if would_hold {
                    data["would_hold"] = json!(true);
                }
                if is_cmd && !simulated {
                    // What a person debugging a wrapped CLI looks for first.
                    data["code"] = out["code"].clone();
                    data["stderr_tail"] = json!(crate::trial_reagent::tail_within(
                        out["stderr"].as_str().unwrap_or(""),
                        15,
                        crate::trial_reagent::TAIL_BYTES,
                    ));
                }
                self.emit("op", text, data);
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
                    // Refusals (validation, authorization) never sent an
                    // external write, so clear the fence instead of blocking
                    // a future revive. A cancel can now stop a write while it
                    // runs (its process group is killed), so, like a time
                    // limit, it leaves the outcome unknown: keep the fence.
                    if matches!(f, Fail::Invalid(_) | Fail::Denied(_)) {
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
            self.guard_reagent_rbac(sid, "sight.read_image", "read")?;
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
        self.mark_impure();
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
                self.check_live().err().unwrap_or_else(|| chat_failure(sid, &e))
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
            return Err(Fail::Invalid(truncation_message(sid, &model, &settings, max_output_tokens, &resp)));
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
        // The child's start records need no sync of their own: anything it
        // does that others can see (a model call, a fenced write) is
        // preceded by a durable record, which syncs these with it, in order.
        // Its end, which carries its epitaph, is always durable.
        let impure_before = self.impure_count();
        self.core.insert_summon_with(
            &child_id,
            Some(&self.id),
            name,
            version,
            &hash,
            model.as_deref(),
            &inputs,
            &def.essence,
            None,
            false,
        );
        self.emit_light(
            "child",
            format!("{sid}: called {name} v{version} as {child_id}"),
            json!({"summon": child_id, "ritual": name, "version": version}),
        );
        let mut meters = vec![Arc::new(Meter::new(&def.essence))];
        meters.extend(self.meters.iter().cloned());
        // The child's own requirements, rendered with its inputs, so a root
        // it takes as an input narrows to the directory it was given.
        let child_requires = crate::core::Core::render_requires(&def.requires, &inputs).map_err(|e| Fail::Invalid(e.message))?;
        let child_grants = ritual::narrow_grants(&self.grants, &child_requires);
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
            actor: self.actor.clone(),
            done: 0,
            total: 0,
            finally_report: None,
        };
        self.core.emit_with(
            &child_id,
            "summon.start",
            &format!("called by {} at depth {}", self.id, run.depth),
            json!({"parent": self.id}),
            false,
        );
        let fin = run.execute();
        let cleanup = match run.finally_report.take() {
            Some(report) => json!({"models": "held by the calling summon", "finally": report}),
            None => json!("held by the calling summon"),
        };
        self.core.finish_summon(&child_id, &fin, cleanup);
        let persisted_cost = if self.meters[0].cost_known.load(Ordering::SeqCst) {
            Some(self.meters[0].cost_microusd.load(Ordering::SeqCst))
        } else {
            None
        };
        // Usage only changes with model calls, which are impure.
        self.core.usage_with(
            &self.id,
            self.meters[0].requests.load(Ordering::SeqCst),
            self.meters[0].tokens.load(Ordering::SeqCst),
            persisted_cost,
            self.impure_count() != impure_before,
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
        impure_before: u64,
        id: &str,
        keep: Option<&[String]>,
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
            .filter(|(k, _)| keep.is_none_or(|ks| ks.iter().any(|x| x == *k)))
            .map(|(k, v)| (k.clone(), v.output.clone()))
            .collect();
        let steps_run: BTreeMap<String, StepStatus> = frame
            .steps
            .iter()
            .map(|(k, v)| (k.clone(), v.status))
            .collect();
        // Only pure work, which a revive can redo exactly: its bookkeeping
        // need not be durable on its own (the next durable write syncs it).
        // The impure count also sees inside child rituals.
        let durable = ran.is_err() || !frame.effects.is_empty() || self.impure_count() != impure_before;
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
            self.core.progress_with(&self.id, self.done, self.total, durable);
            // One row per item, so a long loop's checkpoints grow linearly.
            let n = items.len() - 1;
            self.core.save_checkpoint_with(
                &self.id,
                &format!("each:{id}#{n:08}"),
                &json!(items[n]),
                durable,
            );
        }
        let detail = outputs.values().next().map(brief).unwrap_or_default();
        self.core.emit_with(
            &self.id,
            "item",
            &format!(
                "{label}: {status}{}{}",
                if detail.is_empty() { "" } else { " " },
                error.clone().unwrap_or(detail)
            ),
            json!({"item": label, "status": status}),
            durable,
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
                .map(|m| checkpointed_items(&m, &id))
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
                let impure_before = self.impure_count();
                let ran = self.run_steps(body, scope);
                self.fence_prefix = prev_prefix;
                let frame = scope.frames.pop().unwrap();
                if let Some(f) =
                    self.record_batch_item(impure_before, &id, s.keep.as_deref(), top_level, keep_going, &mut items, label, frame, ran)
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
            // Workers share the count, so one count for the whole batch.
            let impure_before = self.impure_count();
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
                            let _cancel = crate::cancel::enter(worker.cancel.clone());
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
                        impure_before,
                        &id,
                        s.keep.as_deref(),
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
        let failed = counts.failed + counts.invalid;
        if keep_going && failed > 0 {
            // `on_failure: continue` kept going; make sure someone sees it.
            let first = items.iter().find_map(|i| i.error.as_ref().map(|e| (i.item.clone(), e.clone())));
            let (first_item, first_error) = first.unwrap_or_default();
            self.emit(
                "each.failed",
                format!("{id}: {failed} of {} items failed; first ({first_item}): {}", items.len(), clip(&first_error, 300)),
                json!({"step": id, "failed": failed, "of": items.len(), "first_item": first_item, "first_error": first_error}),
            );
        }
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

/// A model call that failed. A model that degenerated into repeating
/// itself (Ollama aborts with "token repeat limit") gave a bad answer, like
/// malformed JSON: `invalid`, so `on_failure`, `abstain` and retries treat it
/// as output, not as a backend that is down.
fn chat_failure(sid: &str, e: &str) -> Fail {
    if e.contains("token repeat limit") {
        Fail::Invalid(format!("step {sid}: the model repeated itself until it was stopped ({e})"))
    } else {
        Fail::Failed(format!("step {sid}: {e}"))
    }
}

/// The ids a step can add to its frame: its own, and those of steps in its
/// `if` branches and fallbacks, which share the frame.
fn frame_ids(step: &Step, out: &mut Vec<String>) {
    if let Some(id) = step.id() {
        out.push(id.clone());
    }
    match step {
        Step::If(b) => b.then.iter().chain(&b.otherwise).for_each(|s| frame_ids(s, out)),
        Step::Operation(o) => o.fallback.iter().for_each(|s| frame_ids(s, out)),
        Step::Model(m) => m.fallback.iter().for_each(|s| frame_ids(s, out)),
        Step::Ritual(_) | Step::Each(_) => {}
    }
}

/// The completed top-level steps of a run: the whole frame under `top` as
/// older cores wrote it, then one row per step (`top#ID`). The frame is
/// abstained when any of them is.
pub(crate) fn checkpointed_top(map: &BTreeMap<String, Value>) -> (BTreeMap<String, StepRecord>, bool) {
    let mut abstained = false;
    let mut steps: BTreeMap<String, StepRecord> = match map.get("top") {
        Some(top) => {
            abstained = top["abstained"].as_bool().unwrap_or(false);
            serde_json::from_value(top["steps"].clone()).unwrap_or_default()
        }
        None => BTreeMap::new(),
    };
    for (path, v) in map.range("top#".to_string()..) {
        let Some(id) = path.strip_prefix("top#") else { break };
        if let Ok(record) = serde_json::from_value::<StepRecord>(v.clone()) {
            steps.insert(id.to_string(), record);
        }
    }
    abstained |= steps.values().any(|r| r.status == StepStatus::Abstained);
    (steps, abstained)
}

/// The items of a top-level `each` already recorded: one checkpoint row per
/// item (`each:ID#00000003`), after the whole list under `each:ID` that older
/// cores wrote (a run they started and this core revived has both). Rows stop
/// at the first gap or unreadable item.
fn checkpointed_items(map: &BTreeMap<String, Value>, id: &str) -> Vec<BatchItem> {
    let mut items: Vec<BatchItem> = map
        .get(&format!("each:{id}"))
        .and_then(|list| serde_json::from_value(list.clone()).ok())
        .unwrap_or_default();
    let prefix = format!("each:{id}#");
    for (path, v) in map.range(prefix.clone()..) {
        let Some(n) = path.strip_prefix(&prefix) else { break };
        if n.parse::<usize>().ok() != Some(items.len()) {
            break;
        }
        match serde_json::from_value::<BatchItem>(v.clone()) {
            Ok(item) => items.push(item),
            Err(_) => break,
        }
    }
    items
}

/// Pure values up to this many bytes of JSON are recorded whole.
const PURE_INLINE_BYTES: usize = 1024;

/// A pure step's argument or output as recorded: whole when small, else its
/// SHA-256 and size (rerunning the step from the same inputs reproduces it).
/// Objects are digested field by field so small fields stay readable.
fn digest_large(v: &Value) -> Value {
    let raw = v.to_string();
    if raw.len() <= PURE_INLINE_BYTES {
        return v.clone();
    }
    match v {
        Value::Object(m) => Value::Object(m.iter().map(|(k, x)| (k.clone(), digest_large(x))).collect()),
        _ => json!({"sha256": crate::util::sha256_hex(raw.as_bytes()), "bytes": raw.len()}),
    }
}

/// Record every step of an untaken branch as skipped with a null output,
/// including the steps of `if`s nested in it, whose ids share the frame (the
/// checker lets later steps reference them). `each` and fallback steps live in
/// their own scope, so only the step's own id is recorded for those.
fn mark_skipped(steps: &[Step], into: &mut BTreeMap<String, StepRecord>) {
    for step in steps {
        if let Some(sid) = step.id() {
            into.insert(
                sid.clone(),
                StepRecord::new(StepStatus::Skipped, StepKind::Skipped, Value::Null),
            );
        }
        if let Step::If(b) = step {
            mark_skipped(&b.then, into);
            mark_skipped(&b.otherwise, into);
        }
    }
}

/// Why a backend stopped early, in terms a ritual author can act on: the
/// generation cap that applied and where it came from, what the backend
/// reported using, and the context and thinking settings that usually
/// explain it.
fn truncation_message(
    sid: &str,
    model: &str,
    settings: &BTreeMap<String, Value>,
    max_output_tokens: Option<u64>,
    resp: &backend::ChatResp,
) -> String {
    let explicit = ["max_tokens", "num_predict"].into_iter().find(|k| settings.contains_key(*k));
    let cap = match backend::resolve_generation_cap(settings, max_output_tokens).ok().flatten() {
        Some(cap) if explicit.is_some() && max_output_tokens != Some(cap) => {
            format!("generation cap of {cap} from `{}`", explicit.unwrap_or_default())
        }
        Some(cap) if explicit.is_some() => {
            format!("generation cap of {cap}, the token essence left (below `{}`)", explicit.unwrap_or_default())
        }
        Some(cap) => format!("generation cap of {cap}, the token essence left"),
        None => "no generation cap set, so the backend's own limit".into(),
    };
    let mut parts = vec![format!("prompt {} tokens", resp.prompt_tokens), cap];
    if let Some(n) = settings.get("num_ctx") {
        parts.push(format!("num_ctx {n}"));
    }
    if model.starts_with("ollama/") {
        let on = settings.get("think").and_then(Value::as_bool).unwrap_or(false);
        parts.push(if on { "thinking on".into() } else { "thinking off".into() });
    }
    format!(
        "step {sid}: backend truncated generation after {} output tokens ({}); dependent work refused. Raise `num_predict` or the token essence, raise `num_ctx`, or set `think: false`",
        resp.output_tokens,
        parts.join("; ")
    )
}

#[cfg(test)]
mod tests {
    #[test]
    fn a_model_that_loops_is_invalid_output_not_a_failed_backend() {
        let looped = "backend returned 500: {\"error\":\"prediction aborted, token repeat limit reached\"}";
        assert!(matches!(super::chat_failure("scan", looped), crate::reagent::Fail::Invalid(_)));
        let down = "backend unreachable at http://127.0.0.1:11434/api/chat: Connection refused";
        assert!(matches!(super::chat_failure("scan", down), crate::reagent::Fail::Failed(_)));
    }
    use super::*;
    use crate::util::Paths;
    use std::path::PathBuf;

    struct Fixture {
        core: Arc<Core>,
        root: PathBuf,
    }

    impl Fixture {
        fn new() -> Self {
            Self::with_config("")
        }

        fn with_config(yaml: &str) -> Self {
            let root = std::env::temp_dir().join(new_id("summonr-exec"));
            let paths = Paths {
                runtime: root.join("run"),
                state: root.join("state"),
                config: root.join("config"),
            };
            if !yaml.is_empty() {
                std::fs::create_dir_all(&paths.config).unwrap();
                std::fs::write(
                    paths.config.join("config.yaml"),
                    yaml.replace("ROOT", &root.to_string_lossy()),
                )
                .unwrap();
                std::fs::create_dir_all(root.join("work")).unwrap();
            }
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
                actor: None,
                done: 0,
                total: 0,
                finally_report: None,
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

    /// A loop of pure steps over sizable text, the shape of bughunt's score
    /// and scan-file. Reports wall time and event bytes; run with
    /// `cargo test --offline pure_loop_cost -- --ignored --nocapture`.
    fn pure_loop(items: usize) -> Ritual {
        let chunk = "fn example() { let x = 1; }\n".repeat(80);
        let list: Vec<Value> = (0..items).map(|i| json!({"n": i, "text": chunk})).collect();
        definition(json!({
            "ritual": "pure-loop-imp",
            "essence": {"requests": 0},
            "steps": [{
                "id": "loop", "each": format!("x in {}", Value::Array(list)),
                "steps": [
                    {"id": "a", "operation": "data.trim", "arguments": {"text": "${x.text}"}},
                    {"id": "b", "operation": "data.replace", "arguments": {"text": "${steps.a.output}", "from": "let", "to": "const"}},
                    {"id": "c", "operation": "data.contains", "arguments": {"value": "${steps.b.output}", "needle": "const x"}},
                    {"id": "d", "operation": "data.count", "arguments": {"value": "${steps.b.output}"}},
                    {"id": "e", "operation": "data.compare", "arguments": {"left": "${steps.d.output}", "right": 8, "operator": "ge"}},
                    {"id": "f", "operation": "data.logic", "arguments": {"operator": "and", "left": "${steps.c.output}", "right": "${steps.e.output}"}},
                ],
            }],
            "return": {"n": "${steps.loop.counts.returned}"},
        }))
    }

    fn event_bytes(f: &Fixture) -> (u64, u64) {
        f.core.db.lock().unwrap().query_row(
            "SELECT COUNT(*), COALESCE(SUM(LENGTH(data)), 0) FROM events", [], |r| Ok((r.get(0)?, r.get(1)?)),
        ).unwrap()
    }

    #[test]
    #[ignore]
    fn pure_loop_cost() {
        let f = Fixture::new();
        let started = std::time::Instant::now();
        let fin = f.run(pure_loop(200), None).execute();
        let took = started.elapsed();
        assert_eq!(fin.outcome, Outcome::Returned, "{:?}", fin.error);
        let (n, bytes) = event_bytes(&f);
        eprintln!("pure loop, 200 items x 6 steps: {took:?}, {n} events, {bytes} event bytes");
    }

    #[test]
    fn pure_steps_record_one_event_with_large_values_by_digest() {
        let f = Fixture::new();
        let big = "x".repeat(4000);
        let def = definition(json!({
            "ritual": "pure-record-imp",
            "steps": [
                {"id": "big", "operation": "data.trim", "arguments": {"text": big}},
                {"id": "small", "operation": "data.trim", "arguments": {"text": " a "}},
            ],
            "return": {"n": "${steps.small.output}"},
        }));
        let fin = f.run(def, None).execute();
        assert_eq!(fin.outcome, Outcome::Returned, "{:?}", fin.error);
        let db = f.core.db.lock().unwrap();
        let kinds: Vec<String> = db
            .prepare("SELECT kind FROM events WHERE kind LIKE 'op%'")
            .unwrap()
            .query_map([], |r| r.get(0))
            .unwrap()
            .map(Result::unwrap)
            .collect();
        assert_eq!(kinds, ["op", "op"], "a pure step needs no intent record");
        let data: Vec<Value> = db
            .prepare("SELECT data FROM events WHERE kind='op' ORDER BY seq")
            .unwrap()
            .query_map([], |r| r.get::<_, String>(0))
            .unwrap()
            .map(|r| serde_json::from_str(&r.unwrap()).unwrap())
            .collect();
        let text = Value::String(big.clone()).to_string();
        assert_eq!(data[0]["arguments"]["text"]["bytes"], text.len());
        assert_eq!(data[0]["arguments"]["text"]["sha256"], crate::util::sha256_hex(text.as_bytes()));
        assert_eq!(data[0]["evidence"]["class"], "computed by the runtime");
        assert_eq!(data[1]["arguments"]["text"], " a ");
    }

    #[test]
    fn each_checkpoints_read_per_item_rows_and_the_older_whole_list() {
        let item = |n: &str| json!({"item": n, "status": "returned", "outputs": {}, "steps": {}, "effects": [], "interrupted": false});
        let rows = BTreeMap::from([
            ("each:loop#00000000".to_string(), item("a")),
            ("each:loop#00000001".to_string(), item("b")),
            ("each:loop#00000003".to_string(), item("after-a-gap")),
            ("each:loopy#00000000".to_string(), item("other-step")),
        ]);
        let got: Vec<String> = checkpointed_items(&rows, "loop").into_iter().map(|i| i.item).collect();
        assert_eq!(got, ["a", "b"]);
        let legacy = BTreeMap::from([("each:loop".to_string(), json!([item("old")]))]);
        assert_eq!(checkpointed_items(&legacy, "loop")[0].item, "old");
        // Revived by this core: the older list, then rows continuing its numbering.
        let mixed = BTreeMap::from([
            ("each:loop".to_string(), json!([item("old")])),
            ("each:loop#00000001".to_string(), item("new")),
        ]);
        let got: Vec<String> = checkpointed_items(&mixed, "loop").into_iter().map(|i| i.item).collect();
        assert_eq!(got, ["old", "new"]);
        assert!(checkpointed_items(&BTreeMap::new(), "loop").is_empty());
    }

    #[test]
    fn a_cmd_step_records_its_exit_code_and_stderr_tail() {
        let f = Fixture::with_config(
            "commands:\n  noisy:\n    program: sh\n    args: ['-c', 'echo warming-up >&2; printf done']\n    cwd: ROOT/work\n",
        );
        let def = definition(json!({
            "ritual": "cmd-record-imp",
            "reagents": {"cmd": {"select": ["read"]}},
            "steps": [{"id": "run", "operation": "cmd.read", "arguments": {"command": "noisy"}}],
            "return": {"out": "${steps.run.output.stdout}"},
        }));
        let mut run = f.run(def, None);
        run.grants = Arc::new(vec![crate::reagent::need("cmd", "read", &[("command", "noisy")], &[])]);
        let fin = run.execute();
        assert_eq!(fin.outcome, Outcome::Returned, "{:?}", fin.error);
        let db = f.core.db.lock().unwrap();
        let (text, data): (String, String) = db
            .query_row("SELECT text, data FROM events WHERE kind='op'", [], |r| Ok((r.get(0)?, r.get(1)?)))
            .unwrap();
        let data: Value = serde_json::from_str(&data).unwrap();
        assert!(text.ends_with("exit 0, 4 chars"), "{text}");
        assert_eq!(data["code"], 0);
        assert_eq!(data["stderr_tail"], "warming-up");
    }

    #[test]
    fn an_exhausted_parent_keeps_its_completed_children_in_partial() {
        let f = Fixture::new();
        let child = definition(json!({
            "ritual": "costly-child-imp",
            "steps": [{"id": "found", "operation": "data.trim", "arguments": {"text": " finding "}}],
            "return": {"found": "${steps.found.output}"},
        }));
        f.save(&child);
        let parent = definition(json!({
            "ritual": "exhausting-parent-imp", "model": "mock/keyword",
            "essence": {"requests": 0},
            "steps": [
                {"id": "hunt", "ritual": "costly-child-imp", "version": 1},
                {"id": "gather", "model": {"prompt": "over budget"}},
            ],
            "return": {"g": "${steps.gather.output}"},
        }));
        let fin = f.run(parent, None).execute();
        assert_eq!(fin.outcome, Outcome::Exhausted, "{:?}", fin.error);
        assert_eq!(fin.result["partial"]["hunt"]["output"]["found"], "finding", "{}", fin.result);
        assert_eq!(fin.result["partial"]["hunt"]["status"], "returned");
        assert!(fin.result["partial"].get("gather").is_none());
    }

    #[test]
    fn bookkeeping_is_synced_when_a_child_ritual_did_impure_work() {
        let relaxed = || crate::core::RELAXED_WRITES.with(|n| n.get());
        let f = Fixture::new();
        f.save(&definition(json!({
            "ritual": "pure-leaf-imp",
            "steps": [{"id": "t", "operation": "data.trim", "arguments": {"text": " a "}}],
            "return": {"t": "${steps.t.output}"},
        })));
        f.save(&definition(json!({
            "ritual": "model-leaf-imp", "model": "mock/keyword",
            "steps": [{"id": "m", "model": {"prompt": "hi"}}],
            "return": {"m": "${steps.m.output}"},
        })));
        let parent = |leaf: &str| definition(json!({
            "ritual": "fan-imp", "essence": {"requests": 10},
            "steps": [{"id": "loop", "each": "x in [1, 2]", "steps": [
                {"id": "leaf", "ritual": leaf, "version": 1}]}],
            "return": {"n": "${steps.loop.counts.returned}"},
        }));
        let before = relaxed();
        assert_eq!(f.run(parent("pure-leaf-imp"), None).execute().outcome, Outcome::Returned);
        // Per item: the child's three start records, its pure op, the
        // unchanged usage, and the item's progress, checkpoint and event;
        // then the top checkpoint after the loop.
        assert_eq!(relaxed() - before, 2 * 8 + 1);
        let before = relaxed();
        let fin = f.run(parent("model-leaf-imp"), None).execute();
        assert_eq!(fin.outcome, Outcome::Returned, "{:?}", fin.error);
        // Only the child's start records: a model call makes the usage and
        // the item's bookkeeping durable (and the top checkpoint after it).
        assert_eq!(relaxed() - before, 2 * 3);
    }

    #[test]
    fn top_checkpoints_are_synced_after_impure_steps_only() {
        let relaxed = || crate::core::RELAXED_WRITES.with(|n| n.get());
        let f = Fixture::new();
        let def = definition(json!({
            "ritual": "top-imp", "model": "mock/keyword", "essence": {"requests": 1},
            "steps": [
                {"id": "a", "operation": "data.trim", "arguments": {"text": " a "}},
                {"id": "m", "model": {"prompt": "hi"}},
                {"id": "b", "operation": "data.trim", "arguments": {"text": " b "}},
            ],
            "return": {"b": "${steps.b.output}"},
        }));
        let before = relaxed();
        assert_eq!(f.run(def, None).execute().outcome, Outcome::Returned);
        // a and b: their op event and the checkpoint after them; m: none.
        assert_eq!(relaxed() - before, 4);
    }

    #[test]
    fn top_checkpoints_read_per_step_rows_after_the_older_whole_frame() {
        let rec = |status: &str, out: Value| json!({"status": status, "kind": "pure", "output": out});
        let legacy = json!({"steps": {"a": rec("returned", json!(1))}, "abstained": false});
        let map = BTreeMap::from([
            ("top".to_string(), legacy),
            ("top#b".to_string(), rec("abstained", json!(2))),
            ("top#c".to_string(), rec("returned", json!(3))),
            ("each:loop#00000000".to_string(), json!({})),
        ]);
        let (steps, abstained) = checkpointed_top(&map);
        assert_eq!(steps.keys().collect::<Vec<_>>(), ["a", "b", "c"]);
        assert_eq!(steps["c"].output, 3);
        assert!(abstained, "a restored abstained step marks the frame");
        assert!(checkpointed_top(&BTreeMap::new()).0.is_empty());
    }

    #[test]
    fn a_top_step_checkpoints_the_ids_its_branches_added() {
        let f = Fixture::new();
        let def = definition(json!({
            "ritual": "branch-rows-imp",
            "steps": [{"id": "gate", "if": "true",
                "then": [{"id": "inner", "operation": "data.trim", "arguments": {"text": " x "}}],
                "else": [{"id": "never", "operation": "data.trim", "arguments": {"text": " y "}}]}],
            "return": {"i": "${steps.inner.output}"},
        }));
        let mut run = f.run(def, None);
        assert_eq!(run.execute().outcome, Outcome::Returned);
        let map = f.core.load_checkpoints(&run.id).unwrap();
        let rows: Vec<&String> = map.keys().collect();
        assert_eq!(rows, ["top#inner", "top#never"], "{map:?}");
        let (steps, _) = checkpointed_top(&map);
        assert_eq!(steps["inner"].output, "x");
        assert_eq!(steps["never"].status, StepStatus::Skipped);
    }

    #[test]
    fn a_filter_reads_inputs_through_params_as_values() {
        let f = Fixture::new();
        let def: Ritual = crate::ritual::parse_file(r#"
ritual: floor-imp
inputs: {floor: integer, team: string}
steps:
  - id: kept
    operation: data.filter
    arguments:
      values: [{team: a, score: 9}, {team: a, score: 2}, {team: "a' or 'x' == 'x", score: 10}]
      predicate: "item.team == params.team and item.score >= params.floor"
      params: {floor: "${inputs.floor}", team: "${inputs.team}"}
return: {kept: "${steps.kept.output}"}
"#).unwrap();
        let mut run = f.run(def, None);
        run.inputs = json!({"floor": 5, "team": "a"});
        let fin = run.execute();
        assert_eq!(fin.outcome, Outcome::Returned, "{:?}", fin.error);
        assert_eq!(fin.result["kept"], json!([{"team": "a", "score": 9}]));
    }

    #[test]
    fn cancelling_a_run_stops_a_command_that_is_still_running() {
        let f = Fixture::with_config(
            "commands:\n  snooze:\n    program: sleep\n    args: ['30']\n    cwd: ROOT/work\n",
        );
        let def = definition(json!({
            "ritual": "snooze-imp",
            "reagents": {"cmd": {"select": ["read"]}},
            "steps": [{"id": "z", "operation": "cmd.read", "arguments": {"command": "snooze"}}],
            "return": {"code": "${steps.z.output.code}"},
        }));
        let mut run = f.run(def, None);
        run.grants = Arc::new(vec![crate::reagent::need("cmd", "read", &[("command", "snooze")], &[])]);
        let cancel = run.cancel.clone();
        std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(300));
            cancel.store(true, Ordering::SeqCst);
        });
        let started = Instant::now();
        let fin = run.execute();
        let took = started.elapsed();
        assert_eq!(fin.outcome, Outcome::Cancelled, "{:?}", fin.error);
        assert!(took < Duration::from_millis(1500), "the command kept the run for {took:?}");
    }

    #[test]
    fn cancelling_a_model_request_closes_its_connection_so_the_backend_can_stop() {
        // A backend that reads the request, never answers, and reports when
        // the client hangs up: what Ollama watches to stop generating.
        use std::io::Read;
        let server = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = server.local_addr().unwrap();
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let (mut s, _) = server.accept().unwrap();
            let mut buf = [0u8; 4096];
            loop {
                match s.read(&mut buf) {
                    Ok(0) | Err(_) => break,
                    Ok(_) => continue,
                }
            }
            let _ = tx.send(Instant::now());
        });
        let mut f = Fixture::new();
        Arc::get_mut(&mut f.core).unwrap().cfg.ollama_url = Some(format!("http://{addr}"));
        let def = definition(json!({
            "ritual": "slow-model-imp", "model": "ollama/never",
            "steps": [{"id": "m", "model": {"prompt": "hi"}}],
            "return": {"m": "${steps.m.output}"},
        }));
        let mut run = f.run(def, None);
        let cancel = run.cancel.clone();
        let cancelled_at = Arc::new(Mutex::new(None));
        let at = cancelled_at.clone();
        std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(300));
            *at.lock().unwrap() = Some(Instant::now());
            cancel.store(true, Ordering::SeqCst);
        });
        let fin = run.execute();
        assert_eq!(fin.outcome, Outcome::Cancelled, "{:?}", fin.error);
        let hung_up = rx.recv_timeout(Duration::from_secs(3)).expect("the connection was left open after the cancel");
        let at = cancelled_at.lock().unwrap().unwrap();
        assert!(hung_up.saturating_duration_since(at) < Duration::from_millis(500), "the backend saw the hang-up {:?} after the cancel", hung_up.saturating_duration_since(at));
    }

    #[test]
    fn cancelling_a_run_stops_waiting_for_a_model_that_has_not_answered() {
        // A backend that accepts the request and never answers.
        let server = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = server.local_addr().unwrap();
        std::thread::spawn(move || {
            let mut held = Vec::new();
            for s in server.incoming().flatten() {
                held.push(s);
            }
        });
        let mut f = Fixture::new();
        Arc::get_mut(&mut f.core).unwrap().cfg.ollama_url = Some(format!("http://{addr}"));
        let def = definition(json!({
            "ritual": "slow-model-imp", "model": "ollama/never",
            "steps": [{"id": "m", "model": {"prompt": "hi"}}],
            "return": {"m": "${steps.m.output}"},
        }));
        let mut run = f.run(def, None);
        let cancel = run.cancel.clone();
        std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(300));
            cancel.store(true, Ordering::SeqCst);
        });
        let started = Instant::now();
        let fin = run.execute();
        assert_eq!(fin.outcome, Outcome::Cancelled, "{:?}", fin.error);
        assert!(started.elapsed() < Duration::from_millis(1500), "{:?}", started.elapsed());
    }


    /// A local stand-in for Ollama that answers every request with `reply`
    /// and sends each request body it receives to the returned channel.
    pub(crate) fn canned_ollama(reply: Value) -> (String, std::sync::mpsc::Receiver<Value>) {
        use std::io::{BufRead, BufReader, Read, Write};
        let server = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = server.local_addr().unwrap();
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            for s in server.incoming().flatten() {
                let mut r = BufReader::new(s.try_clone().unwrap());
                let mut len = 0usize;
                loop {
                    let mut line = String::new();
                    if r.read_line(&mut line).unwrap_or(0) == 0 || line == "\r\n" {
                        break;
                    }
                    if let Some(v) = line.to_ascii_lowercase().strip_prefix("content-length:") {
                        len = v.trim().parse().unwrap_or(0);
                    }
                }
                let mut body = vec![0; len];
                let _ = r.read_exact(&mut body);
                let _ = tx.send(serde_json::from_slice(&body).unwrap_or(Value::Null));
                let text = reply.to_string();
                let mut s = s;
                let _ = write!(s, "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{text}", text.len());
            }
        });
        (format!("http://{addr}"), rx)
    }

    fn cleanup_ritual(main: Value, finally: Value) -> Ritual {
        definition(json!({
            "ritual": "cleanup-imp",
            "reagents": {"cmd": {"select": ["read"]}},
            "essence": {"cleanup_time": "1s"},
            "steps": main, "finally": finally,
            "return": {"made": "${steps.made.output}"},
        }))
    }

    fn trim(id: &str, text: &str) -> Value {
        json!({"id": id, "operation": "data.trim", "arguments": {"text": text}})
    }

    #[test]
    fn finally_runs_after_a_failure_sees_what_ran_and_keeps_the_failure() {
        let f = Fixture::new();
        let def = cleanup_ritual(
            json!([trim("made", " thing "), trim("boom", "${inputs.nope}"), trim("later", "x")]),
            json!([
                trim("tidy", "${steps.made.output}"),
                {"if": "steps.later.status == \"returned\"", "then": [trim("never", "x")]},
            ]),
        );
        let mut run = f.run(def, None);
        let fin = run.execute();
        assert_eq!(fin.outcome, Outcome::Failed);
        assert!(fin.error.as_deref().unwrap().contains("nope"), "{:?}", fin.error);
        let report = run.finally_report.clone().expect("finally ran");
        assert_eq!(report["outcome"], "returned", "{report}");
        assert_eq!(report["ran"], json!(["tidy"]), "{report}");
    }

    #[test]
    fn a_failing_finally_fails_a_returned_run_but_never_replaces_another_outcome() {
        let f = Fixture::new();
        let mut ok = f.run(cleanup_ritual(json!([trim("made", "a")]), json!([trim("tidy", "${inputs.nope}")])), None);
        let fin = ok.execute();
        assert_eq!(fin.outcome, Outcome::Failed);
        assert!(fin.error.as_deref().unwrap().starts_with("finally: "), "{:?}", fin.error);
        assert_eq!(fin.result["return"], json!({"made": "a"}), "the computed return is kept: {}", fin.result);
        let mut bad = f.run(
            cleanup_ritual(json!([trim("made", "a"), {"id": "x", "operation": "data.missing"}]), json!([trim("tidy", "${inputs.nope}")])),
            None,
        );
        let fin = bad.execute();
        assert_eq!(fin.outcome, Outcome::Invalid, "{:?}", fin.error);
        assert!(!fin.error.as_deref().unwrap().starts_with("finally"), "the main failure is reported: {:?}", fin.error);
        assert_eq!(bad.finally_report.clone().unwrap()["outcome"], "failed");
    }

    #[test]
    fn finally_runs_after_a_stop_and_after_time_runs_out_within_its_own_window() {
        let f = Fixture::with_config(
            "commands:\n  snooze:\n    program: sleep\n    args: ['30']\n    cwd: ROOT/work\n",
        );
        let snooze = json!({"id": "z", "operation": "cmd.read", "arguments": {"command": "snooze"}});
        let grants = Arc::new(vec![crate::reagent::need("cmd", "read", &[("command", "snooze")], &[])]);
        // Stopped in the middle of a command: cleanup still runs, the run stays cancelled.
        let mut run = f.run(cleanup_ritual(json!([trim("made", "a"), snooze.clone()]), json!([trim("tidy", "${steps.made.output}")])), None);
        run.grants = grants.clone();
        let cancel = run.cancel.clone();
        std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(200));
            cancel.store(true, Ordering::SeqCst);
        });
        let fin = run.execute();
        assert_eq!(fin.outcome, Outcome::Cancelled, "{:?}", fin.error);
        assert_eq!(run.finally_report.clone().unwrap()["outcome"], "returned");
        // Out of time: cleanup gets `cleanup_time` (1s here), so a cleanup
        // that hangs is cut off, and the run keeps its own outcome.
        let mut def = cleanup_ritual(json!([trim("made", "a"), snooze.clone()]), json!([trim("tidy", "a"), {"id": "hang", "operation": "cmd.read", "arguments": {"command": "snooze"}}]));
        def.essence.time = Some("300ms".into());
        let mut run = f.run(def, None);
        run.grants = grants;
        let started = Instant::now();
        let fin = run.execute();
        let took = started.elapsed();
        assert_eq!(fin.outcome, Outcome::Exhausted, "{:?}", fin.error);
        let report = run.finally_report.clone().unwrap();
        assert_eq!(report["outcome"], "exhausted", "{report}");
        assert_eq!(report["ran"], json!(["tidy"]), "{report}");
        assert!(took < Duration::from_millis(2500), "cleanup was not bounded: {took:?}");
        assert!(took > Duration::from_millis(1000), "cleanup got its window: {took:?}");
    }

    #[test]
    fn an_invocation_think_setting_overrides_the_rituals_and_reaches_ollama() {
        let (url, bodies) = canned_ollama(json!({
            "message": {"role": "assistant", "content": "ok"},
            "done_reason": "stop", "prompt_eval_count": 3, "eval_count": 1,
        }));
        let mut f = Fixture::new();
        Arc::get_mut(&mut f.core).unwrap().cfg.ollama_url = Some(url);
        let def = definition(json!({
            "ritual": "thinker-imp", "model": "ollama/thinker", "settings": {"think": true},
            "steps": [{"id": "m", "model": {"prompt": "hi"}}],
            "return": {"m": "${steps.m.output}"},
        }));
        let mut run = f.run(def, None);
        // What `--set think=false` stores.
        run.settings_override.insert("think".into(), crate::core::setting_value("false"));
        let fin = run.execute();
        assert_eq!(fin.outcome, Outcome::Returned, "{:?}", fin.error);
        let sent = bodies.recv_timeout(Duration::from_secs(5)).unwrap();
        assert_eq!(sent["think"], json!(false));
    }

    #[test]
    fn a_truncated_generation_names_the_cap_and_the_tokens_it_used() {
        let (url, bodies) = canned_ollama(json!({
            "message": {"role": "assistant", "content": "{\"verdict\": \"conf", "thinking": "Let me think about this at length..."},
            "done_reason": "length", "prompt_eval_count": 900, "eval_count": 512,
        }));
        let mut f = Fixture::new();
        Arc::get_mut(&mut f.core).unwrap().cfg.ollama_url = Some(url);
        let def = definition(json!({
            "ritual": "thinker-imp", "model": "ollama/thinker",
            "settings": {"num_predict": 512, "num_ctx": 16384, "think": true},
            "steps": [{"id": "m", "model": {"prompt": "hi", "schema": {"type": "object", "properties": {"verdict": {"type": "string"}}, "required": ["verdict"]}}}],
            "return": {"m": "${steps.m.output}"},
        }));
        let mut run = f.run(def, None);
        let fin = run.execute();
        assert_eq!(fin.outcome, Outcome::Invalid, "{:?}", fin.error);
        let err = fin.error.unwrap();
        for want in ["backend truncated generation", "step m", "512 output tokens", "cap of 512", "num_ctx 16384", "thinking on"] {
            assert!(err.contains(want), "`{want}` missing from: {err}");
        }
        let sent = bodies.recv_timeout(Duration::from_secs(5)).unwrap();
        assert_eq!(sent["think"], json!(true), "think is forwarded to Ollama");
    }
    #[test]
    fn steps_in_an_if_nested_in_an_untaken_branch_are_skipped() {
        let f = Fixture::new();
        let def = definition(json!({
            "ritual": "nested-if-imp",
            "steps": [{
                "if": "false",
                "then": [{
                    "if": "true",
                    "then": [{"id": "deep", "operation": "data.trim", "arguments": {"text": " a "}}],
                    "else": [{
                        "each": "x in [1]",
                        "id": "loop",
                        "steps": [{"id": "in_loop", "operation": "data.trim", "arguments": {"text": "b"}}],
                    }],
                }],
            }],
            "return": {"deep": "${steps.deep.output}", "loop": "${steps.loop.output}"},
        }));
        let mut run = f.run(def, None);
        let fin = run.execute();
        assert_eq!(fin.outcome, Outcome::Returned, "{:?}", fin.error);
        assert_eq!(fin.result["deep"], Value::Null);
        assert_eq!(fin.result["loop"], Value::Null);
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
    fn each_keep_limits_item_outputs_to_named_steps() {
        let f = Fixture::new();
        let def = definition(json!({
            "ritual": "keep-some",
            "reagents": {"data": {"select": ["arithmetic", "trim"]}},
            "essence": {"requests": 0, "tool_calls": 20, "time": "10s"},
            "steps": [{
                "id": "batch",
                "each": "x in [1, 2]",
                "keep": ["mul"],
                "steps": [
                    {"id": "big", "operation": "data.trim", "arguments": {"text": "  lots of text  "}},
                    {"id": "mul", "operation": "data.arithmetic",
                     "arguments": {"left": "${x}", "right": 10, "operator": "multiply"}}
                ]
            }],
            "return": {"items": "${steps.batch.output}"}
        }));
        let fin = f.run(def, None).execute();
        assert_eq!(fin.outcome, Outcome::Returned, "{:?}", fin.error);
        for (it, want) in fin.result["items"].as_array().unwrap().iter().zip([10, 20]) {
            assert_eq!(it["outputs"], json!({"mul": want}));
            // Step statuses are still recorded for every step.
            assert!(it["steps"].get("big").is_some());
        }
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
