//! drills service: ritual cases run against scripted model answers.
use super::*;
use crate::drill;
use crate::touchstone::Expected;
use crate::io::{Io, Live, Replay, Sandboxed, Scripted, Simulate};
use super::sandbox::Sandbox;

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
        if file.sandbox.is_some() {
            if who.is_some() {
                return Err(RpcErr::new(
                    "not_authorized",
                    4,
                    "a sandboxed drill copies the owner's repositories and files; only the owner can run one",
                ));
            }
            return self.run_sandboxed(name, version, file, label, dir);
        }
        self.run_drill_cases(name, version, file, label, dir, who, false)
    }

    /// Run a drill's cases in a throwaway core whose only repositories and
    /// roots are disposable copies, with local writes performed for real.
    fn run_sandboxed(
        self: &Arc<Self>,
        name: &str,
        version: u32,
        file: &drill::DrillFile,
        label: &str,
        dir: Option<&Path>,
    ) -> Result<Value, RpcErr> {
        let (_, _, def) = self
            .load_ritual(name, Some(version))?
            .ok_or_else(|| RpcErr::not_found(format!("no ritual `{name}`")))?;
        // The file roots the ritual requires, for every case's inputs.
        let mut roots = Vec::new();
        for case in &file.cases {
            let inputs = ritual::resolve_inputs(&def, &case.inputs).map_err(|e| {
                RpcErr::invalid(format!("case `{}`: {e}", case.id))
            })?;
            for r in Self::render_requires(&def.requires, &inputs)? {
                if let Some(root) = r.on.get("root") {
                    if !roots.contains(root) {
                        roots.push(root.clone());
                    }
                }
            }
        }
        let repos: Vec<String> = match file.sandbox.as_ref().and_then(|s| s.repos()) {
            Some(only) => only.to_vec(),
            None => self.cfg.repos.keys().cloned().collect(),
        };
        let sb = Sandbox::build(&self.cfg, &repos, &roots)?;
        let paths = crate::util::Paths {
            runtime: sb.dir.join("core/run"),
            state: sb.dir.join("core/state"),
            config: sb.dir.join("core/config"),
        };
        let mut temp = Core::open(paths).map_err(|e| RpcErr::invalid(format!("sandbox core: {e}")))?;
        Arc::get_mut(&mut temp)
            .ok_or_else(|| RpcErr::invalid("sandbox core is shared"))?
            .cfg = sb.config(&self.cfg);
        // The ritual, the children it calls and its touchstone, with paths
        // rewritten to the copies; children first, and parents re-pinned to
        // the rewritten children's hashes.
        let mut pinned: HashMap<(String, u32), String> = HashMap::new();
        let mut wanted = vec![(name.to_string(), Some(version))];
        if let Some(t) = &file.touchstone {
            wanted.push((t.clone(), None));
        }
        for (n, v) in wanted {
            self.copy_into_sandbox(&temp, &sb, &n, v, &mut pinned)?;
        }
        for g in self.grants_for(name)? {
            let g: Grant = serde_json::from_value(sb.rewrite(&serde_json::to_value(&g).unwrap_or_default()))
                .map_err(|e| RpcErr::invalid(format!("sandbox grant: {e}")))?;
            temp.db
                .lock()
                .unwrap()
                .execute(
                    "INSERT OR IGNORE INTO grants (ritual, grant_json, created) VALUES (?1, ?2, ?3)",
                    params![name, serde_json::to_string(&g).unwrap_or_default(), now_ms()],
                )
                .map_err(storage_error)?;
        }
        let mut local = file.clone();
        local.sandbox = None;
        for case in &mut local.cases {
            if let Value::Object(m) = sb.rewrite(&Value::Object(case.inputs.clone())) {
                case.inputs = m;
            }
        }
        let mut out = temp.run_drill_cases(name, version, &local, label, dir, None, true)?;
        out["sandbox"] = sb.describe();
        out["note"] = json!("case records lived in the sandbox's core and were deleted with it");
        drop(temp);
        drop(sb);
        Ok(out)
    }

    /// Copy one saved ritual version (and, first, every child it calls)
    /// into the sandbox core, rewritten to the sandbox's paths.
    fn copy_into_sandbox(
        &self,
        temp: &Arc<Core>,
        sb: &Sandbox,
        name: &str,
        version: Option<u32>,
        pinned: &mut HashMap<(String, u32), String>,
    ) -> Result<(u32, String), RpcErr> {
        let (v, _, def) = self
            .load_ritual(name, version)?
            .ok_or_else(|| RpcErr::not_found(format!("no ritual `{name}` in the grimoire")))?;
        if let Some(h) = pinned.get(&(name.to_string(), v)) {
            return Ok((v, h.clone()));
        }
        let mut def: Ritual = serde_json::from_value(sb.rewrite(&serde_json::to_value(&def).unwrap_or_default()))
            .map_err(|e| RpcErr::invalid(format!("sandbox: rewriting `{name}`: {e}")))?;
        fn children(steps: &mut [ritual::Step], each: &mut dyn FnMut(&mut ritual::Step) -> Result<(), RpcErr>) -> Result<(), RpcErr> {
            for s in steps.iter_mut() {
                if s.ritual.is_some() {
                    each(s)?;
                }
                for inner in [&mut s.steps, &mut s.then, &mut s.otherwise, &mut s.fallback].into_iter().flatten() {
                    children(inner, each)?;
                }
            }
            Ok(())
        }
        let mut repin = |s: &mut ritual::Step| {
            let child = s.ritual.clone().unwrap_or_default();
            let (cv, ch) = self.copy_into_sandbox(temp, sb, &child, s.version, pinned)?;
            if s.version.is_some() {
                s.version = Some(cv);
            }
            if s.hash.is_some() {
                s.hash = Some(ch);
            }
            Ok(())
        };
        children(&mut def.steps, &mut repin)?;
        children(&mut def.finally, &mut repin)?;
        let hash = ritual::content_hash(&def);
        temp.db
            .lock()
            .unwrap()
            .execute(
                "INSERT OR IGNORE INTO rituals (name, version, hash, def, created) VALUES (?1, ?2, ?3, ?4, ?5)",
                params![name, v, hash, ritual::canonical_json(&def), now_ms()],
            )
            .map_err(storage_error)?;
        pinned.insert((name.to_string(), v), hash.clone());
        Ok((v, hash))
    }

    #[allow(clippy::too_many_arguments)]
    fn run_drill_cases(
        self: &Arc<Self>,
        name: &str,
        version: u32,
        file: &drill::DrillFile,
        label: &str,
        dir: Option<&Path>,
        who: Option<(&str, &[String])>,
        sandboxed: bool,
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
            // In a sandbox, local writes act on the copies; elsewhere every
            // write is simulated.
            let io: Arc<dyn Io> = if sandboxed {
                Arc::new(Sandboxed::new(script.clone()))
            } else {
                Arc::new(Simulate::new(script.clone()))
            };
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
