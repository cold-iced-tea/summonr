//! drills service: ritual cases run against scripted model answers.
use super::*;
use crate::drill;
use crate::touchstone::Expected;
use crate::io::{Io, Live, Replay, Scripted, Simulate};

impl Core {
    pub(super) fn drill(self: &Arc<Self>, p: Value) -> Result<Value, RpcErr> {
        let p: requests::Drill = decode(p)?;
        let (version, _, _) = self.load_ritual(&p.ritual, p.version)?.ok_or_else(|| {
            RpcErr::not_found(format!(
                "no ritual `{}` in the grimoire; drills run against saved rituals",
                p.ritual
            ))
        })?;
        let path = PathBuf::from(&p.cwd).join(&p.against);
        let text = std::fs::read_to_string(&path)
            .map_err(|e| RpcErr::not_found(format!("cannot read {}: {e}", path.display())))?;
        let file =
            drill::parse(&text).map_err(|e| RpcErr::invalid(format!("{}: {e}", path.display())))?;
        let dir = path.parent().unwrap_or(Path::new(".")).to_path_buf();
        self.run_drill_file(&p.ritual, version, &file, &path.display().to_string(), Some(&dir), None)
    }

    pub(super) fn drill_inline(self: &Arc<Self>, p: Value) -> Result<Value, RpcErr> {
        self.drill_inline_as(p, None)
    }

    /// An inline drill. A familiar's (`who` = its caller and allowed
    /// rituals) runs as that familiar: user-only inputs stay the owner's, the
    /// version is the current one, and a touchstone must be allowed too.
    pub(super) fn drill_inline_as(
        self: &Arc<Self>,
        p: Value,
        who: Option<(&str, &[String])>,
    ) -> Result<Value, RpcErr> {
        let p: requests::DrillInline = decode(p)?;
        if who.is_some() && p.version.is_some() {
            return Err(RpcErr::new(
                "not_authorized",
                4,
                "the ritual version is chosen by the owner of this core, not by a calling agent",
            ));
        }
        let (version, _, _) = self.load_ritual(&p.ritual, p.version)?.ok_or_else(|| {
            RpcErr::not_found(format!(
                "no ritual `{}` in the grimoire; drills run against saved rituals",
                p.ritual
            ))
        })?;
        let file = match p.cases {
            Value::String(s) => drill::parse(&s).map_err(RpcErr::invalid)?,
            other => drill::parse_value(other).map_err(RpcErr::invalid)?,
        };
        if let Some(c) = file.cases.iter().find(|c| c.cassette.is_some()) {
            return Err(RpcErr::invalid(format!(
                "case `{}` specifies a cassette path; inline drill_ritual cannot read host files",
                c.id
            )));
        }
        self.run_drill_file(&p.ritual, version, &file, "inline drill", None, who)
    }

    fn run_drill_file(
        self: &Arc<Self>,
        name: &str,
        version: u32,
        file: &drill::DrillFile,
        label: &str,
        dir: Option<&Path>,
        who: Option<(&str, &[String])>,
    ) -> Result<Value, RpcErr> {
        if file.ritual != name {
            return Err(RpcErr::invalid(format!(
                "{label} drills ritual `{}`, not `{name}`",
                file.ritual
            )));
        }
        let touchstone = match &file.touchstone {
            Some(t) if who.is_some_and(|(_, allowed)| !allowed.contains(t)) => {
                // The same answer as for a ritual that does not exist.
                return Err(RpcErr::not_found(format!("no touchstone `{t}` in the grimoire")));
            }
            Some(t) => {
                if let Some(c) = file.cases.iter().find(|c| c.expect.result.is_some()) {
                    return Err(RpcErr::invalid(format!(
                        "{label}: case `{}` has a hand-written expect.result, but the file names touchstone `{t}`; use one or the other",
                        c.id
                    )));
                }
                Some(self.pin_touchstone(t, name, version)?)
            }
            None => None,
        };

        // Everything that can fail is decided before any case runs: inputs,
        // the ritual's real grants, and cassettes.
        let mut prepared = Vec::new();
        for case in &file.cases {
            let spec = Spec {
                target: name.to_string(),
                version: Some(version),
                args: case.inputs.clone(),
                scripted: true,
                caller: who.map(|(c, _)| c.to_string()).unwrap_or_default(),
                ..Default::default()
            };
            let mut r = self.resolve(&spec).map_err(|mut e| {
                e.message = format!("case `{}`: {}", case.id, e.message);
                e
            })?;
            // Scripted resolution checks no drift, so nothing refuses or warns.
            r.refuse = None;
            r.warnings.clear();
            Self::check_authority(&r)?;
            let tools: Arc<dyn Io> = match (&case.cassette, dir) {
                (Some(c), Some(d)) => Arc::new(Replay::load(&d.join(c)).map_err(|e| {
                    let mut e = cassette_error(e);
                    e.message = format!("case `{}`: {}", case.id, e.message);
                    e
                })?),
                _ => Arc::new(Live),
            };
            prepared.push((case, r, tools));
        }

        let series = new_id("series");
        let mut cases = Vec::new();
        for (case, r, tools) in prepared {
            let script = Arc::new(
                Scripted::new(case.model.clone(), tools).with_defaults(case.model_default.clone()),
            );
            let io: Arc<dyn Io> = Arc::new(Simulate::new(script.clone()));
            // The same admission as `invoke`: a case is a run like any other.
            let (id, run, _) = {
                let _admission = self.admission.lock().unwrap();
                self.admit()?;
                self.make_run(r, None, Some(&series), Some(io))
            };
            let fin = self.complete(run, false);
            let record = self.summon(&id)?;
            // A touchstone describes the returned result; it is not asked about
            // cases expected to abstain or fail.
            let returns = case.expect.outcome.unwrap_or(Outcome::Returned) == Outcome::Returned;
            let expected = touchstone
                .as_ref()
                .filter(|_| returns)
                .map(|t| self.run_touchstone(t, &case.inputs, &series));
            cases.push(drill::judge(case, &fin, &record, &script, expected.as_ref()));
        }
        let passed = cases.iter().filter(|c| c["passed"] == true).count();
        // A case whose only problem is an unanswered touchstone was not checked;
        // a case with real mismatches failed, whatever the touchstone did.
        let touchstone_errors = cases
            .iter()
            .filter(|c| !c["touchstone_error"].is_null() && c["reasons"].as_array().is_some_and(Vec::is_empty))
            .count();
        Ok(json!({
            "drill": file.drill, "ritual": name, "version": version, "series": series,
            "touchstone": touchstone.as_ref().map(Pinned::describe),
            "passed": passed, "failed": cases.len() - passed - touchstone_errors,
            "touchstone_errors": touchstone_errors, "cases": cases,
        }))
    }
}

/// A touchstone resolved once, before any case runs.
pub(super) struct Pinned {
    pub name: String,
    pub version: u32,
    pub hash: String,
}

impl Pinned {
    fn describe(&self) -> Value {
        json!({"name": self.name, "version": self.version, "hash": self.hash})
    }
}

impl Core {
    /// Load the newest version of a touchstone, check it may judge `candidate`,
    /// and pin that exact version for the whole series.
    pub(super) fn pin_touchstone(
        &self,
        touchstone: &str,
        candidate: &str,
        candidate_version: u32,
    ) -> Result<Pinned, RpcErr> {
        let (version, hash, def) = self.load_ritual(touchstone, None)?.ok_or_else(|| {
            RpcErr::not_found(format!("no touchstone `{touchstone}` in the grimoire"))
        })?;
        let (_, _, judged) = self
            .load_ritual(candidate, Some(candidate_version))?
            .ok_or_else(|| RpcErr::not_found(format!("no ritual `{candidate}`")))?;
        let effects = ritual::validate(&def, &self.resolver(), &reagent::tool, self.cfg.max_depth())
            .map_err(|errs| RpcErr::invalid(format!("touchstone `{touchstone}` is not valid:\n  {}", errs.join("\n  "))))?;
        crate::touchstone::eligible(&def, &effects, &judged).map_err(RpcErr::invalid)?;
        Ok(Pinned { name: touchstone.to_string(), version, hash })
    }

    /// Compute one case's expected result. Anything but a returned answer is a
    /// touchstone error, never a verdict on the ritual under test.
    pub(super) fn run_touchstone(
        self: &Arc<Self>,
        t: &Pinned,
        inputs: &Map<String, Value>,
        series: &str,
    ) -> Expected {
        let spec = Spec {
            target: t.name.clone(),
            version: Some(t.version),
            args: inputs.clone(),
            scripted: true,
            ..Default::default()
        };
        let mut r = match self.resolve(&spec) {
            Ok(r) => r,
            Err(e) => return Expected::TouchstoneError(e.message),
        };
        if r.hash != t.hash {
            return Expected::TouchstoneError(format!(
                "touchstone `{}` version {} changed during the series",
                t.name, t.version
            ));
        }
        r.refuse = None;
        r.warnings.clear();
        let io: Arc<dyn Io> = Arc::new(Simulate::new(Arc::new(Live)));
        let run = {
            let _admission = self.admission.lock().unwrap();
            if let Err(e) = self.admit() {
                return Expected::TouchstoneError(e.message);
            }
            self.make_run(r, None, Some(series), Some(io)).1
        };
        let fin = self.complete(run, false);
        match fin.outcome {
            Outcome::Returned => Expected::Value(fin.result),
            other => Expected::TouchstoneError(format!(
                "touchstone `{}` {other}: {}",
                t.name,
                fin.error.unwrap_or_default()
            )),
        }
    }
}
