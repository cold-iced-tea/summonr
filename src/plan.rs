//! Executable step variants. Source definitions remain unchanged for diagnostics and hashes.
use crate::ritual::{self, Expr, Kind, ModelStep, Step as RawStep, Verify};
use serde_json::Value;
use std::collections::BTreeMap;

#[derive(Clone, Debug)]
pub enum Step {
    Operation(Operation),
    Model(Model),
    Ritual(Ritual),
    Each(Each),
    If(Branch),
}
#[derive(Clone, Debug)]
pub struct Operation {
    pub id: Option<String>,
    pub operation: String,
    pub arguments: BTreeMap<String, Value>,
    pub verify: Option<Verify>,
    pub hold: Option<Value>,
    pub retry: u32,
    pub fallback: Vec<Step>,
}
#[derive(Clone, Debug)]
pub struct Model {
    pub id: Option<String>,
    pub model: ModelStep,
    pub retry: u32,
    pub fallback: Vec<Step>,
}
#[derive(Clone, Debug)]
pub struct Ritual {
    pub id: Option<String>,
    pub ritual: String,
    pub version: Option<u32>,
    pub hash: Option<String>,
    pub arguments: BTreeMap<String, Value>,
}
#[derive(Clone, Debug)]
pub struct Each {
    pub id: Option<String>,
    pub variable: String,
    pub source: Vec<String>,
    pub literal: Option<Vec<Value>>,
    pub steps: Vec<Step>,
    pub keep_going: bool,
    pub parallel: u32,
}
#[derive(Clone, Debug)]
pub struct Branch {
    pub id: Option<String>,
    pub condition: Expr,
    pub condition_source: String,
    pub then: Vec<Step>,
    pub otherwise: Vec<Step>,
}
impl Step {
    pub fn id(&self) -> Option<&String> {
        match self {
            Self::Operation(s) => s.id.as_ref(),
            Self::Model(s) => s.id.as_ref(),
            Self::Ritual(s) => s.id.as_ref(),
            Self::Each(s) => s.id.as_ref(),
            Self::If(s) => s.id.as_ref(),
        }
    }
}
pub fn compile(steps: &[RawStep]) -> Result<Vec<Step>, String> {
    steps.iter().map(Step::try_from).collect()
}
impl TryFrom<&RawStep> for Step {
    type Error = String;
    fn try_from(s: &RawStep) -> Result<Self, String> {
        let id = s.id.clone();
        Ok(match s.kind()? {
            Kind::Operation => Self::Operation(Operation {
                id,
                operation: s.operation.clone().expect("kind checked"),
                arguments: s.arguments.clone().unwrap_or_default(),
                verify: s.verify.clone(),
                hold: s.hold.clone(),
                retry: s.retry.unwrap_or(0),
                fallback: match &s.fallback {
                    Some(fb) => compile(fb)?,
                    None => Vec::new(),
                },
            }),
            Kind::Model => Self::Model(Model {
                id,
                model: s.model.clone().expect("kind checked"),
                retry: s.retry.unwrap_or(0),
                fallback: match &s.fallback {
                    Some(fb) => compile(fb)?,
                    None => Vec::new(),
                },
            }),
            Kind::Ritual => Self::Ritual(Ritual {
                id,
                ritual: s.ritual.clone().expect("kind checked"),
                version: s.version,
                hash: s.hash.clone(),
                arguments: s.arguments.clone().unwrap_or_default(),
            }),
            Kind::Each => {
                let raw = s.each.as_deref().expect("kind checked");
                let Some((lhs, rhs)) = raw.split_once(" in ") else {
                    return Err(format!("bad each `{raw}`"));
                };
                let variable = lhs.trim();
                let source = rhs.trim();
                if variable.is_empty() || variable.contains(char::is_whitespace) {
                    return Err(format!("bad each `{raw}`"));
                }
                let literal = if source.starts_with('[') {
                    let vals: Vec<Value> = serde_json::from_str(source)
                        .map_err(|e| format!("bad each `{raw}`: {e}"))?;
                    Some(vals)
                } else {
                    if source.contains(char::is_whitespace) {
                        return Err(format!("bad each `{raw}`"));
                    }
                    None
                };
                let parallel = s.parallel.unwrap_or(1);
                if parallel == 0 {
                    return Err("parallel must be at least 1".into());
                }
                if s.on_failure
                    .as_deref()
                    .is_some_and(|p| !matches!(p, "stop" | "continue"))
                {
                    return Err("on_failure must be stop or continue".into());
                }
                Self::Each(Each {
                    id,
                    variable: variable.into(),
                    source: if literal.is_some() {
                        Vec::new()
                    } else {
                        source.split('.').map(str::to_string).collect()
                    },
                    literal,
                    steps: compile(s.steps.as_deref().unwrap_or_default())?,
                    keep_going: s.on_failure.as_deref() == Some("continue"),
                    parallel,
                })
            }
            Kind::If => Self::If(Branch {
                id,
                condition_source: s.cond.clone().expect("kind checked"),
                condition: ritual::parse_expr(s.cond.as_deref().expect("kind checked"))?,
                then: compile(s.then.as_deref().unwrap_or_default())?,
                otherwise: compile(s.otherwise.as_deref().unwrap_or_default())?,
            }),
        })
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn compilation_preserves_source_and_rejects_mixed_kinds() {
        let source =
            ritual::parse_file(include_str!("../tests/fixtures/rituals/total-receipt.yaml")).unwrap();
        let before = ritual::canonical_json(&source);
        let hash = ritual::content_hash(&source);
        assert!(matches!(
            &compile(&source.steps).unwrap()[0],
            Step::Operation(_)
        ));
        assert_eq!(before, ritual::canonical_json(&source));
        assert_eq!(hash, ritual::content_hash(&source));
        let bad: RawStep = serde_json::from_value(
            serde_json::json!({"operation":"data.count","model":{"prompt":"x"}}),
        )
        .unwrap();
        assert!(Step::try_from(&bad).unwrap_err().contains("only one"));
    }
}
