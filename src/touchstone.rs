//! Touchstones: saved rituals that compute a case's expected result with no
//! model and no effects, so expectations are derived rather than labelled.
use crate::ritual::{Effect, Ritual};
use serde_json::Value;

/// What a touchstone said a case should produce.
#[derive(Debug, Clone, PartialEq)]
pub enum Expected {
    Value(Value),
    /// The touchstone itself did not return. Never evidence against the
    /// ritual under test.
    TouchstoneError(String),
}

/// A touchstone must be fully deterministic and accept exactly the inputs of
/// the ritual it judges. `effects` are the touchstone's validated effects,
/// children included.
pub fn eligible(touchstone: &Ritual, effects: &[Effect], candidate: &Ritual) -> Result<(), String> {
    let name = &touchstone.ritual;
    if touchstone.essence.requests != Some(0) {
        return Err(format!(
            "touchstone `{name}` must declare `essence: {{requests: 0}}` so it can never call a model"
        ));
    }
    if let Some(e) = effects.iter().find(|e| e.kind == "model") {
        return Err(format!(
            "touchstone `{name}` has a model step ({}); a touchstone computes, it does not judge",
            e.step
        ));
    }
    if let Some(e) = effects.iter().find(|e| e.effect != "pure") {
        return Err(format!(
            "touchstone `{name}` uses `{}` ({}); only pure `data` operations are allowed",
            e.what, e.effect
        ));
    }
    for (input, decl) in &candidate.inputs {
        match touchstone.inputs.get(input) {
            None => return Err(format!("touchstone `{name}` does not take input `{input}` of `{}`", candidate.ritual)),
            Some(t) if t.ty != decl.ty => {
                return Err(format!(
                    "touchstone `{name}` input `{input}` is {}, but `{}` declares it {}",
                    t.ty, candidate.ritual, decl.ty
                ))
            }
            Some(_) => {}
        }
    }
    if let Some(extra) = touchstone.inputs.keys().find(|k| !candidate.inputs.contains_key(*k)) {
        return Err(format!(
            "touchstone `{name}` takes input `{extra}`, which `{}` does not",
            candidate.ritual
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn def(v: Value) -> Ritual {
        serde_json::from_value(v).unwrap()
    }
    fn pure_effect() -> Effect {
        Effect { step: "s".into(), kind: "operation".into(), what: "data.parse_json".into(), effect: "pure".into(), determinism: "deterministic".into(), notes: vec![] }
    }
    fn candidate() -> Ritual {
        def(json!({"ritual":"c","model":"mock/a","inputs":{"form":{"type":"string"}},"steps":[{"id":"m","model":{"prompt":"p"}}]}))
    }

    #[test]
    fn touchstone_must_be_pure() {
        let t = def(json!({"ritual":"t","model":"mock/a","inputs":{"form":{"type":"string"}},"essence":{"requests":0},"steps":[{"id":"m","model":{"prompt":"p"}}]}));
        let model = Effect { kind: "model".into(), effect: "read".into(), determinism: "probabilistic".into(), ..pure_effect() };
        let err = eligible(&t, &[model], &candidate()).unwrap_err();
        assert!(err.contains("model"), "{err}");
    }

    #[test]
    fn touchstone_must_declare_zero_requests() {
        let t = def(json!({"ritual":"t","inputs":{"form":{"type":"string"}},"steps":[]}));
        let err = eligible(&t, &[pure_effect()], &candidate()).unwrap_err();
        assert!(err.contains("requests: 0"), "{err}");
    }

    #[test]
    fn touchstone_must_not_write() {
        let t = def(json!({"ritual":"t","inputs":{"form":{"type":"string"}},"essence":{"requests":0},"steps":[]}));
        let write = Effect { what: "mail.move".into(), effect: "write".into(), ..pure_effect() };
        let err = eligible(&t, &[write], &candidate()).unwrap_err();
        assert!(err.contains("mail.move"), "{err}");
    }

    #[test]
    fn touchstone_inputs_must_match() {
        let t = def(json!({"ritual":"t","inputs":{"form":{"type":"integer"}},"essence":{"requests":0},"steps":[]}));
        let err = eligible(&t, &[pure_effect()], &candidate()).unwrap_err();
        assert!(err.contains("`form`"), "{err}");
        let t = def(json!({"ritual":"t","inputs":{},"essence":{"requests":0},"steps":[]}));
        assert!(eligible(&t, &[], &candidate()).unwrap_err().contains("`form`"));
    }

    #[test]
    fn eligible_touchstone_passes() {
        let t = def(json!({"ritual":"t","inputs":{"form":{"type":"string"}},"essence":{"requests":0},"steps":[]}));
        assert_eq!(eligible(&t, &[pure_effect()], &candidate()), Ok(()));
    }
}
