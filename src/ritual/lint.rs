//! Warnings about a definition that is valid but likely to fail late: the
//! checks here never reject a ritual, they point at a cheaper way to write it.
use super::{expr_refs, parse_expr, template_refs, Ritual, Step};
use serde_json::Value;
use std::collections::BTreeSet;

/// Steps whose output can be large (a file, a page, a command's output, a
/// model's answer), named for the warning.
fn large_output(step: &Step) -> Option<String> {
    if step.model.is_some() {
        return Some("a model step".into());
    }
    let op = step.operation.as_deref()?;
    let large = matches!(op, "file.read_text" | "file.read_json" | "data.lines" | "cmd.read")
        || op.starts_with("http.get");
    large.then(|| op.to_string())
}

/// Every step, depth first, including loop bodies, branches and fallbacks.
pub(super) fn walk(steps: &[Step]) -> impl Iterator<Item = &Step> {
    let mut pending: Vec<_> = steps.iter().rev().collect();
    std::iter::from_fn(move || {
        let step = pending.pop()?;
        for children in [&step.fallback, &step.otherwise, &step.then, &step.steps]
            .into_iter()
            .flatten()
        {
            pending.extend(children.iter().rev());
        }
        Some(step)
    })
}

/// The template references in a value's strings.
fn refs_in(v: &Value, out: &mut Vec<Vec<String>>) {
    match v {
        Value::String(s) => out.extend(template_refs(s).unwrap_or_default()),
        Value::Array(a) => a.iter().for_each(|x| refs_in(x, out)),
        Value::Object(m) => m.values().for_each(|x| refs_in(x, out)),
        _ => {}
    }
}

/// Whether `path` is `steps.ID.output…` for one of `ids`.
fn is_loop_output(path: &[String], ids: &BTreeSet<String>) -> bool {
    path.len() >= 3 && path[0] == "steps" && ids.contains(&path[1]) && path[2] == "output"
}

/// Data operations whose output still holds the items they were given.
const PASSES_ITEMS: &[&str] = &[
    "data.filter", "data.concat", "data.unique_by", "data.select", "data.flatten", "data.coalesce",
];

fn step_refs(s: &Step) -> Vec<Vec<String>> {
    let mut refs = Vec::new();
    for v in s.arguments.iter().flat_map(|a| a.values()) {
        refs_in(v, &mut refs);
    }
    refs
}

/// The loop `id` and every step whose output still holds its items (a
/// filter of its results, a concat with them, …), transitively.
fn carriers(all: &[&Step], id: &str) -> BTreeSet<String> {
    let mut ids = BTreeSet::from([id.to_string()]);
    loop {
        let before = ids.len();
        for s in all {
            let passes = s.operation.as_deref().is_some_and(|o| PASSES_ITEMS.contains(&o));
            if let (true, Some(sid)) = (passes, s.id.as_ref()) {
                if step_refs(s).iter().any(|p| is_loop_output(p, &ids)) {
                    ids.insert(sid.clone());
                }
            }
        }
        if ids.len() == before {
            return ids;
        }
    }
}

/// The item step named by `…outputs.NAME` in a path, if any.
fn output_name(path: &[String]) -> Option<String> {
    path.iter()
        .position(|p| p == "outputs")
        .and_then(|i| path.get(i + 1))
        .cloned()
}

/// A loop without `keep:` records every step's output for every item. When
/// a step in it can produce a lot of text and the loop's results are later
/// passed to a data operation (capped at 1 MiB), the run can fail after all
/// of the loop's work is done. Suggest `keep:` with the item outputs that
/// are actually read later.
pub fn unbounded_loop_warnings(def: &Ritual) -> Vec<String> {
    let all: Vec<_> = walk(&def.steps).chain(walk(&def.finally)).collect();
    let mut warnings = Vec::new();
    for lp in all.iter().filter(|s| s.each.is_some() && s.keep.is_none()) {
        let Some(id) = lp.id.as_deref() else { continue };
        let mut body = walk(lp.steps.as_deref().unwrap_or_default());
        let Some((big_id, big_what)) = body.find_map(|s| {
            large_output(s).map(|w| (s.id.clone().unwrap_or_else(|| "(unnamed)".into()), w))
        }) else {
            continue;
        };
        let mut consumer = None;
        let mut wanted = BTreeSet::new();
        let ids = carriers(&all, id);
        for s in &all {
            let refs = step_refs(s);
            let reads_loop = refs.iter().any(|p| is_loop_output(p, &ids));
            if !reads_loop {
                // A later loop over this one's results reads `VAR.outputs.X`.
                if let Some((var, src)) = s.each.as_deref().and_then(|e| e.split_once(" in ")) {
                    let src: Vec<String> = src.trim().split('.').map(String::from).collect();
                    if is_loop_output(&src, &ids) {
                        for t in walk(s.steps.as_deref().unwrap_or_default()) {
                            let r = step_refs(t);
                            wanted.extend(r.iter().filter(|p| p.first().map(String::as_str) == Some(var.trim())).filter_map(|p| output_name(p)));
                        }
                    }
                }
                continue;
            }
            wanted.extend(refs.iter().filter(|p| is_loop_output(p, &ids)).filter_map(|p| output_name(&p[3..])));
            // A collection's body and a filter's predicate read `item.outputs.X`.
            wanted.extend(refs.iter().filter(|p| p.first().map(String::as_str) == Some("item")).filter_map(|p| output_name(p)));
            if let Some(Value::String(pred)) = s.arguments.as_ref().and_then(|a| a.get("predicate")) {
                if let Ok(e) = parse_expr(pred) {
                    let mut paths = Vec::new();
                    expr_refs(&e, &mut paths);
                    wanted.extend(paths.iter().filter_map(|p| output_name(p)));
                }
            }
            if let Some(op) = s.operation.as_deref().filter(|o| o.starts_with("data.")) {
                consumer.get_or_insert((s.id.clone().unwrap_or_else(|| op.to_string()), op.to_string()));
            }
        }
        let mut ret_refs = Vec::new();
        for v in def.ret.values() {
            refs_in(v, &mut ret_refs);
        }
        wanted.extend(ret_refs.iter().filter(|p| is_loop_output(p, &ids)).filter_map(|p| output_name(&p[3..])));
        let Some((at, op)) = consumer else { continue };
        let keep = if wanted.is_empty() {
            "`keep: []` (nothing reads the items' outputs), or `keep:` with the outputs you need".to_string()
        } else {
            format!("`keep: [{}]`", wanted.into_iter().collect::<Vec<_>>().join(", "))
        };
        warnings.push(format!(
            "each `{id}` has no `keep:`, so its results hold every step's output for every item, including `{big_id}` ({big_what}); \
             step `{at}` passes them to {op}, which refuses values over 1 MiB, so the run can fail after the loop's work is done. Add {keep} to `{id}`."
        ));
    }
    warnings
}

/// Every lint for a definition.
pub fn lint(def: &Ritual) -> Vec<String> {
    unbounded_loop_warnings(def)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ritual::{model_sites, parse_file, validate};

    #[test]
    fn deep_step_tree_is_safe_on_a_two_mib_stack() {
        std::thread::Builder::new()
            .stack_size(2 * 1024 * 1024)
            .spawn(|| {
                let mut def = parse_file(
                    "ritual: deep\nmodel: mock/keyword\nsteps:\n  - id: leaf\n    model: {prompt: hi}\n",
                )
                .unwrap();
                for i in 0..1024 {
                    let children = Some(std::mem::take(&mut def.steps));
                    let step = match i % 3 {
                        0 => Step { cond: Some("true".into()), then: children, ..Step::default() },
                        1 => Step { id: Some(format!("loop_{i}")), each: Some("item in []".into()), steps: children, ..Step::default() },
                        _ => Step {
                            id: Some(format!("recover_{i}")),
                            model: Some(serde_yaml::from_str("{prompt: hi}").unwrap()),
                            fallback: children,
                            ..Step::default()
                        },
                    };
                    def.steps.push(step);
                }
                assert!(lint(&def).is_empty());
                assert_eq!(model_sites(&def, &|_, _| None, None), BTreeSet::from(["mock/keyword".into()]));
                let result = validate(&def, &|_, _| None, &|_, _| None, 3);
                // Dispose of the deliberately deep, unvalidated AST iteratively.
                let mut pending = std::mem::take(&mut def.steps);
                while let Some(mut step) = pending.pop() {
                    pending.extend([step.steps.take(), step.then.take(), step.fallback.take()]
                        .into_iter().flatten().flatten());
                }
                assert!(result.unwrap_err().iter().any(|e| e.contains("step nesting exceeds")));
            })
            .unwrap()
            .join()
            .unwrap();
    }
}
