//! Runtime-owned records. `Value` is reserved for task inputs, outputs and observations.
use crate::outcome::Outcome;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use std::collections::BTreeMap;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StepStatus {
    Returned,
    Abstained,
    Skipped,
    Partial,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StepKind {
    Operation,
    Pure,
    Model,
    Ritual,
    Each,
    Skipped,
}
impl StepKind {
    pub fn produces_result(self) -> bool {
        matches!(self, Self::Pure | Self::Model | Self::Ritual)
    }
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StepRecord {
    pub status: StepStatus,
    pub kind: StepKind,
    pub output: Value,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub counts: Option<BatchCounts>,
    /// The output came straight from the step's own operation, never from a
    /// fallback; only such an output may be re-adopted on revive.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub direct: bool,
}
impl StepRecord {
    pub fn new(status: StepStatus, kind: StepKind, output: Value) -> Self {
        Self {
            status,
            kind,
            output,
            counts: None,
            direct: false,
        }
    }
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "class")]
pub enum Evidence {
    #[serde(rename = "computed by the runtime")]
    Computed { observed: Value },
    #[serde(rename = "reported by the tool")]
    Reported { observed: Value },
    #[serde(rename = "observed by the runtime")]
    Observed {
        by: String,
        observed: Value,
        expected: Value,
    },
    /// The write was authorized but not performed.
    #[serde(rename = "simulated by the runtime")]
    Simulated { would: Value },
}
impl Evidence {
    pub fn observation(&self) -> Option<(&str, &Value)> {
        match self {
            Self::Observed { by, observed, .. } => Some((by, observed)),
            _ => None,
        }
    }
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum EffectRecord {
    Held {
        step: String,
        operation: String,
        petition: String,
    },
    Applied {
        step: String,
        operation: String,
        evidence: Evidence,
    },
    Simulated {
        step: String,
        operation: String,
        evidence: Evidence,
        /// The step asked for a hold; a simulation files no petition.
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        would_hold: bool,
    },
    Unverified {
        step: String,
        operation: String,
        error: String,
    },
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BatchItem {
    pub item: String,
    pub status: Outcome,
    pub error: Option<String>,
    pub outputs: Map<String, Value>,
    pub steps: BTreeMap<String, StepStatus>,
    pub effects: Vec<EffectRecord>,
    pub interrupted: bool,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BatchCounts {
    pub applied: usize,
    pub simulated: usize,
    pub observed: usize,
    pub held: usize,
    pub unverified: usize,
    pub selected: usize,
    pub processed: usize,
    pub remaining: usize,
    pub interrupted: usize,
    pub returned: usize,
    pub abstained: usize,
    pub invalid: usize,
    pub failed: usize,
}
impl BatchCounts {
    pub fn from_items(selected: usize, items: &[BatchItem]) -> Self {
        let outcome = |o| items.iter().filter(|i| i.status == o).count();
        let processed = items.iter().filter(|i| !i.interrupted).count();
        let all: Vec<_> = items.iter().flat_map(|i| &i.effects).collect();
        Self {
            applied: all
                .iter()
                .filter(|e| matches!(e, EffectRecord::Applied { .. }))
                .count(),
            simulated: all
                .iter()
                .filter(|e| matches!(e, EffectRecord::Simulated { .. }))
                .count(),
            observed: all
                .iter()
                .filter(|e| {
                    matches!(
                        e,
                        EffectRecord::Applied {
                            evidence: Evidence::Observed { .. },
                            ..
                        }
                    )
                })
                .count(),
            held: all
                .iter()
                .filter(|e| matches!(e, EffectRecord::Held { .. }))
                .count(),
            unverified: outcome(Outcome::Unverified),
            selected,
            processed,
            remaining: selected - processed,
            interrupted: items.len() - processed,
            returned: outcome(Outcome::Returned),
            abstained: outcome(Outcome::Abstained),
            invalid: outcome(Outcome::Invalid),
            failed: outcome(Outcome::Failed),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PetitionState {
    Pending,
    Executing,
    Unknown,
    Applied,
    Denied,
    Unverified,
    Failed,
    /// The resource changed after the petition was filed; nothing was done.
    Stale,
    /// A pact's deadline passed before the caller decided; nothing was done.
    Expired,
}
impl PetitionState {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Executing => "executing",
            Self::Unknown => "unknown",
            Self::Applied => "applied",
            Self::Denied => "denied",
            Self::Unverified => "unverified",
            Self::Failed => "failed",
            Self::Stale => "stale",
            Self::Expired => "expired",
        }
    }
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Petition {
    pub id: String,
    pub summon: String,
    pub ritual: String,
    pub step: String,
    pub operation: String,
    pub effect: String,
    pub arguments: Value,
    pub verify: Option<crate::reagent::Check>,
    pub because: Value,
    pub status: PetitionState,
    pub created: i64,
    pub decided: Option<i64>,
    pub result: Value,
    pub selection: std::collections::HashSet<String>,
    pub authority: Option<Vec<crate::ritual::Grant>>,
    /// What `reagent::target_digest` said when the write was held.
    #[serde(default)]
    pub digest: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn batch_counts_separate_simulated_from_applied() {
        let item = |effects| BatchItem {
            item: "i".into(),
            status: Outcome::Returned,
            error: None,
            outputs: Map::new(),
            steps: BTreeMap::new(),
            effects,
            interrupted: false,
        };
        let applied = EffectRecord::Applied {
            step: "m".into(),
            operation: "mail.move".into(),
            evidence: Evidence::Reported {
                observed: json!(null),
            },
        };
        let simulated = EffectRecord::Simulated {
            step: "m".into(),
            operation: "mail.move".into(),
            evidence: Evidence::Simulated { would: json!({}) },
            would_hold: false,
        };
        let c = BatchCounts::from_items(2, &[item(vec![applied]), item(vec![simulated])]);
        assert_eq!((c.simulated, c.applied), (1, 1));
    }
}
