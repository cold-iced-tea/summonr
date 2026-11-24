//! Rite (plain: workflow): declarative, resumable multi-stage work built on
//! pinned rituals, human/automated gates, and explicit assumption dependencies.

use crate::ritual::{self, InputDecl, Resolver};
use crate::util::{canonical_json, Config, sha256_hex};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use std::collections::{BTreeMap, BTreeSet, VecDeque};

#[derive(Deserialize, Serialize, Clone, Debug)]
#[serde(deny_unknown_fields)]
pub struct AssumptionDecl {
    pub id: String,
    pub statement: String,
}

#[derive(Deserialize, Serialize, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum GateKind {
    Owner,
    Touchstone,
    Assay,
}

#[derive(Deserialize, Serialize, Clone, Debug)]
#[serde(deny_unknown_fields)]
pub struct GateDecl {
    #[serde(rename = "type")]
    pub kind: GateKind,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ritual: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub version: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hash: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub against: Option<String>,
}

#[derive(Deserialize, Serialize, Clone, Debug)]
#[serde(deny_unknown_fields)]
pub struct StageDecl {
    pub id: String,
    pub ritual: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub version: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hash: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub after: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub assumes: Vec<String>,
    #[serde(default, skip_serializing_if = "Map::is_empty")]
    pub inputs: Map<String, Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub gate: Option<GateDecl>,
}

#[derive(Deserialize, Serialize, Clone, Debug)]
#[serde(deny_unknown_fields)]
pub struct RiteDef {
    pub rite: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub window: Option<crate::window_design::WindowDesign>,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub description: String,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub inputs: BTreeMap<String, InputDecl>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub assumptions: Vec<AssumptionDecl>,
    pub stages: Vec<StageDecl>,
}

/// Static validation output for a compiled rite.
#[derive(Clone, Debug)]
pub struct RitePlan {
    pub order: Vec<String>,
    pub stage_effects: BTreeMap<String, Vec<ritual::Effect>>,
}

pub fn parse_rite_file(text: &str) -> Result<RiteDef, String> {
    let val: Value = serde_yaml::from_str(text).map_err(|e| format!("invalid rite YAML/JSON: {e}"))?;
    serde_json::from_value(val).map_err(|e| format!("invalid rite definition: {e}"))
}

pub fn rite_hash(def: &RiteDef) -> String {
    let v = serde_json::to_value(def).expect("RiteDef is serializable");
    sha256_hex(canonical_json(&v).as_bytes())
}

/// Topological sort of stage IDs. Returns Err on cycle or unknown dependency.
pub fn topo_order(stages: &[StageDecl]) -> Result<Vec<String>, String> {
    let ids: BTreeSet<&str> = stages.iter().map(|s| s.id.as_str()).collect();
    let mut indeg: BTreeMap<&str, usize> = BTreeMap::new();
    let mut adj: BTreeMap<&str, Vec<&str>> = BTreeMap::new();
    for s in stages {
        indeg.entry(s.id.as_str()).or_insert(0);
        for dep in &s.after {
            if !ids.contains(dep.as_str()) {
                return Err(format!("stage `{}` depends on unknown stage `{dep}`", s.id));
            }
            if dep == &s.id {
                return Err(format!("stage `{}` cannot depend on itself", s.id));
            }
            *indeg.entry(s.id.as_str()).or_insert(0) += 1;
            adj.entry(dep.as_str()).or_default().push(s.id.as_str());
        }
    }
    let mut q: VecDeque<&str> = indeg
        .iter()
        .filter(|(_, &d)| d == 0)
        .map(|(&id, _)| id)
        .collect();
    let mut out = Vec::with_capacity(stages.len());
    while let Some(u) = q.pop_front() {
        out.push(u.to_string());
        if let Some(nexts) = adj.get(u) {
            for &v in nexts {
                let d = indeg.get_mut(v).unwrap();
                *d -= 1;
                if *d == 0 {
                    q.push_back(v);
                }
            }
        }
    }
    if out.len() != stages.len() {
        return Err("rite stage dependencies contain a cycle".into());
    }
    Ok(out)
}

/// Compute all stages that directly assume `assumption_id` plus every transitive
/// downstream stage in the DAG.
pub fn affected_stages(def: &RiteDef, assumption_id: &str) -> BTreeSet<String> {
    let mut adj: BTreeMap<&str, Vec<&str>> = BTreeMap::new();
    let mut q: VecDeque<&str> = VecDeque::new();
    let mut affected: BTreeSet<String> = BTreeSet::new();
    for s in &def.stages {
        for dep in &s.after {
            adj.entry(dep.as_str()).or_default().push(s.id.as_str());
        }
        if s.assumes.iter().any(|a| a == assumption_id) && affected.insert(s.id.clone()) {
            q.push_back(s.id.as_str());
        }
    }
    while let Some(u) = q.pop_front() {
        if let Some(nexts) = adj.get(u) {
            for &v in nexts {
                if affected.insert(v.to_string()) {
                    q.push_back(v);
                }
            }
        }
    }
    affected
}

/// Compute transitive upstream ancestors of `stage_id`.
pub(crate) fn ancestors_of(stages: &[StageDecl], stage_id: &str) -> BTreeSet<String> {
    let by_id: BTreeMap<&str, &StageDecl> = stages.iter().map(|s| (s.id.as_str(), s)).collect();
    let mut out = BTreeSet::new();
    let mut q = VecDeque::new();
    if let Some(s) = by_id.get(stage_id) {
        for d in &s.after {
            if out.insert(d.clone()) {
                q.push_back(d.as_str());
            }
        }
    }
    while let Some(u) = q.pop_front() {
        if let Some(s) = by_id.get(u) {
            for d in &s.after {
                if out.insert(d.clone()) {
                    q.push_back(d.as_str());
                }
            }
        }
    }
    out
}

/// Pin every stage ritual and touchstone gate ritual, and validate the entire rite DAG.
pub fn prepare_and_validate_rite(
    def: &mut RiteDef,
    resolve: Resolver,
    cfg: &Config,
) -> Result<RitePlan, Vec<String>> {
    let mut errors = Vec::new();
    if def.rite.trim().is_empty() {
        errors.push("rite name must not be empty".into());
    }
    if def.stages.is_empty() {
        errors.push("rite must declare at least one stage".into());
    }

    let mut assumption_ids = BTreeSet::new();
    for a in &def.assumptions {
        if a.id.trim().is_empty() {
            errors.push("assumption id must not be empty".into());
        } else if !assumption_ids.insert(a.id.clone()) {
            errors.push(format!("duplicate assumption id `{}`", a.id));
        }
    }

    let mut stage_ids = BTreeSet::new();
    for s in &def.stages {
        if s.id.trim().is_empty() {
            errors.push("stage id must not be empty".into());
        } else if !stage_ids.insert(s.id.clone()) {
            errors.push(format!("duplicate stage id `{}`", s.id));
        }
        for a in &s.assumes {
            if !assumption_ids.contains(a) {
                errors.push(format!("stage `{}` references unknown assumption `{a}`", s.id));
            }
        }
    }

    let order = match topo_order(&def.stages) {
        Ok(o) => o,
        Err(e) => {
            errors.push(e);
            Vec::new()
        }
    };

    // Check input template references against declared inputs and upstream stages.
    let stages_snapshot = def.stages.clone();
    for s in &def.stages {
        let upstream = ancestors_of(&stages_snapshot, &s.id);
        for (k, v) in &s.inputs {
            collect_value_refs(v, &mut |path| match path.first().map(String::as_str) {
                Some("inputs") => {
                    if let Some(name) = path.get(1) {
                        if !def.inputs.contains_key(name) {
                            errors.push(format!(
                                "stage `{}` input `{k}` references unknown rite input `{name}`",
                                s.id
                            ));
                        }
                    }
                }
                Some("stages") => {
                    if let Some(ref_stage) = path.get(1) {
                        if !upstream.contains(ref_stage) {
                            errors.push(format!(
                                "stage `{}` input `{k}` references stage `{ref_stage}` which is not an upstream dependency in `after:`",
                                s.id
                            ));
                        }
                    }
                }
                Some(other) => {
                    errors.push(format!(
                        "stage `{}` input `{k}` references unknown root `{other}` (use `inputs.*` or `stages.<id>.output`)",
                        s.id
                    ));
                }
                None => {}
            });
        }
    }

    // Pin stage rituals and gate rituals, and validate each stage ritual.
    let mut stage_effects = BTreeMap::new();
    for s in &mut def.stages {
        match resolve(&s.ritual, s.version) {
            Some((v, h, rdef)) => {
                if let Some(ref expected) = s.hash {
                    if expected != &h {
                        errors.push(format!(
                            "stage `{}` pins ritual `{}` hash {expected}, but grimoire has {h}",
                            s.id, s.ritual
                        ));
                    }
                }
                s.version = Some(v);
                s.hash = Some(h);
                match ritual::validate_with_cfg(&rdef, resolve, cfg, cfg.max_depth()) {
                    Ok(eff) => {
                        stage_effects.insert(s.id.clone(), eff);
                    }
                    Err(es) => {
                        for e in es {
                            errors.push(format!("stage `{}` ritual `{}`: {e}", s.id, s.ritual));
                        }
                    }
                }
            }
            None => {
                errors.push(format!(
                    "stage `{}` references ritual `{}` which is not in the grimoire",
                    s.id, s.ritual
                ));
            }
        }

        if let Some(ref mut gate) = s.gate {
            match gate.kind {
                GateKind::Owner => {}
                GateKind::Touchstone => {
                    let Some(ref tr) = gate.ritual else {
                        errors.push(format!(
                            "stage `{}` touchstone gate must name a `ritual:`",
                            s.id
                        ));
                        continue;
                    };
                    match resolve(tr, gate.version) {
                        Some((tv, th, tdef)) => {
                            if tdef.essence.requests != Some(0) {
                                errors.push(format!(
                                    "stage `{}` touchstone gate ritual `{tr}` must be pure (`essence.requests: 0`)",
                                    s.id
                                ));
                            }
                            gate.version = Some(tv);
                            gate.hash = Some(th);
                        }
                        None => {
                            errors.push(format!(
                                "stage `{}` touchstone gate ritual `{tr}` is not in the grimoire",
                                s.id
                            ));
                        }
                    }
                }
                GateKind::Assay => {}
            }
        }
    }

    if !errors.is_empty() {
        return Err(errors);
    }
    Ok(RitePlan {
        order,
        stage_effects,
    })
}

fn collect_value_refs(v: &Value, f: &mut impl FnMut(&[String])) {
    match v {
        Value::String(s) => {
            if let Ok(refs) = ritual::template_refs(s) {
                for r in refs {
                    f(&r);
                }
            }
        }
        Value::Array(arr) => {
            for item in arr {
                collect_value_refs(item, f);
            }
        }
        Value::Object(map) => {
            for val in map.values() {
                collect_value_refs(val, f);
            }
        }
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ritual;

    fn pure_ritual(name: &str) -> ritual::Ritual {
        ritual::parse_file(&format!(
            "ritual: {name}\ninputs:\n  x: {{type: string, default: \"ok\"}}\nessence: {{requests: 0, time: 1m}}\nsteps:\n  - id: s1\n    operation: data.trim\n    arguments: {{text: \"${{inputs.x}}\"}}\nreturn:\n  out: \"${{steps.s1.output}}\"\n"
        ))
        .unwrap()
    }

    fn impure_ritual(name: &str) -> ritual::Ritual {
        ritual::parse_file(&format!(
            "ritual: {name}\ninputs:\n  x: {{type: string}}\nmodel: mock/keyword\nessence: {{requests: 1, time: 1m}}\nsteps:\n  - id: m\n    model:\n      instructions: \"pick\"\n      prompt: \"${{inputs.x}}\"\n      schema: {{type: object, properties: {{a: {{type: string, enum: [yes, no]}}}}, required: [a]}}\nreturn:\n  a: \"${{steps.m.output.a}}\"\n"
        ))
        .unwrap()
    }

    #[test]
    fn cycle_and_unknown_assumption_are_rejected() {
        let yaml = r#"
rite: bad-cycle
assumptions:
  - id: a1
    statement: "valid"
stages:
  - id: s1
    ritual: r1
    after: [s2]
    assumes: [missing_a]
  - id: s2
    ritual: r1
    after: [s1]
"#;
        let mut def = parse_rite_file(yaml).unwrap();
        let r1 = pure_ritual("r1");
        let h1 = ritual::content_hash(&r1);
        let res = |name: &str, _: Option<u32>| {
            if name == "r1" {
                Some((1, h1.clone(), r1.clone()))
            } else {
                None
            }
        };
        let errs = prepare_and_validate_rite(&mut def, &res, &Config::default()).unwrap_err();
        assert!(errs.iter().any(|e| e.contains("cycle")), "{errs:?}");
        assert!(errs.iter().any(|e| e.contains("unknown assumption")), "{errs:?}");
    }

    #[test]
    fn impure_touchstone_gate_is_rejected() {
        let yaml = r#"
rite: bad-gate
stages:
  - id: s1
    ritual: r1
    gate:
      type: touchstone
      ritual: m1
"#;
        let mut def = parse_rite_file(yaml).unwrap();
        let r1 = pure_ritual("r1");
        let h1 = ritual::content_hash(&r1);
        let m1 = impure_ritual("m1");
        let hm1 = ritual::content_hash(&m1);
        let res = |name: &str, _: Option<u32>| match name {
            "r1" => Some((1, h1.clone(), r1.clone())),
            "m1" => Some((1, hm1.clone(), m1.clone())),
            _ => None,
        };
        let errs = prepare_and_validate_rite(&mut def, &res, &Config::default()).unwrap_err();
        assert!(errs.iter().any(|e| e.contains("must be pure")), "{errs:?}");
    }

    #[test]
    fn affected_stages_includes_transitive_dependents() {
        let yaml = r#"
rite: chain
assumptions:
  - id: a1
    statement: "base"
stages:
  - id: s1
    ritual: r1
    assumes: [a1]
  - id: s2
    ritual: r1
    after: [s1]
  - id: s3
    ritual: r1
    after: [s2]
  - id: independent
    ritual: r1
"#;
        let def = parse_rite_file(yaml).unwrap();
        let aff = affected_stages(&def, "a1");
        assert!(aff.contains("s1"));
        assert!(aff.contains("s2"));
        assert!(aff.contains("s3"));
        assert!(!aff.contains("independent"));
    }
}
