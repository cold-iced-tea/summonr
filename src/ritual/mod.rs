//! Ritual definitions: the structured file format, references and
//! conditions, typed inputs, whole-definition validation, and the effect plan.

use crate::util::sha256_hex;
use serde::{Deserialize, Serialize};
use serde_json::{json, Map, Value};
use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

pub type ToolLookup = dyn Fn(&str, &str) -> Option<&'static crate::reagent::ToolSpec>;

fn language_v1() -> u32 {
    1
}
fn is_language_v1(v: &u32) -> bool {
    *v == 1
}

#[derive(Deserialize, Serialize, Clone, Debug)]
#[serde(deny_unknown_fields)]
pub struct Ritual {
    #[serde(default = "language_v1", skip_serializing_if = "is_language_v1")]
    pub language_version: u32,
    #[serde(rename = "$defs", alias = "defs", default, skip_serializing_if = "BTreeMap::is_empty")]
    pub defs: BTreeMap<String, Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output_schema: Option<Value>,
    pub ritual: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub description: String,
    #[serde(default)]
    pub inputs: BTreeMap<String, InputDecl>,
    #[serde(default)]
    pub bindings: BTreeMap<String, BindingDecl>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    #[serde(default)]
    pub settings: BTreeMap<String, Value>,
    #[serde(default)]
    pub reagents: BTreeMap<String, ReagentDecl>,
    #[serde(default)]
    pub requires: Vec<Requirement>,
    #[serde(default)]
    pub essence: Essence,
    #[serde(default)]
    pub drift: DriftPolicy,
    pub steps: Vec<Step>,
    #[serde(rename = "return", default)]
    pub ret: BTreeMap<String, Value>,
}

#[derive(Serialize, Clone, Debug)]
pub struct InputDecl {
    #[serde(rename = "type")]
    pub ty: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub min: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max: Option<f64>,
    #[serde(rename = "enum", default, skip_serializing_if = "Option::is_none")]
    pub options: Option<Vec<Value>>,
    /// Only the user may change this input from its default. A calling agent
    /// that tries is refused, so it cannot switch off a safeguard.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub user_only: bool,
    /// For `object` inputs: a schema (the strict subset) the value must match.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub schema: Option<Value>,
}

impl<'de> serde::Deserialize<'de> for InputDecl {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct Raw {
            #[serde(rename = "type")]
            ty: String,
            #[serde(default)]
            default: Option<Value>,
            #[serde(default)]
            min: Option<f64>,
            #[serde(default)]
            max: Option<f64>,
            #[serde(rename = "enum", default)]
            options: Option<Vec<Value>>,
            #[serde(default)]
            user_only: bool,
            #[serde(default)]
            schema: Option<Value>,
        }
        struct V;
        impl<'de> serde::de::Visitor<'de> for V {
            type Value = InputDecl;
            fn expecting(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
                f.write_str("an input type string or input declaration map")
            }
            fn visit_str<E: serde::de::Error>(self, s: &str) -> Result<InputDecl, E> {
                let t = s.trim();
                if (t.starts_with("list<") || t.starts_with("array<")) && t.ends_with('>') {
                    return Ok(InputDecl {
                        ty: "array".to_string(),
                        default: None,
                        min: None,
                        max: None,
                        options: None,
                        user_only: false,
                        schema: Some(Value::String(t.to_string())),
                    });
                }
                Ok(InputDecl {
                    ty: s.to_string(),
                    default: None,
                    min: None,
                    max: None,
                    options: None,
                    user_only: false,
                    schema: None,
                })
            }
            fn visit_seq<A: serde::de::SeqAccess<'de>>(
                self,
                seq: A,
            ) -> Result<InputDecl, A::Error> {
                let mut arr = Vec::<Value>::deserialize(
                    serde::de::value::SeqAccessDeserializer::new(seq),
                )?;
                if arr.len() != 1 {
                    return Err(serde::de::Error::custom(
                        "compact array input type must have exactly one element schema, e.g. `[string]`",
                    ));
                }
                let item = arr.remove(0);
                Ok(InputDecl {
                    ty: "array".to_string(),
                    default: None,
                    min: None,
                    max: None,
                    options: None,
                    user_only: false,
                    schema: Some(json!({"type": "array", "items": item})),
                })
            }
            fn visit_map<A: serde::de::MapAccess<'de>>(
                self,
                map: A,
            ) -> Result<InputDecl, A::Error> {
                let mut m = Map::<String, Value>::deserialize(
                    serde::de::value::MapAccessDeserializer::new(map),
                )?;
                const RAW_KEYS: &[&str] =
                    &["type", "default", "min", "max", "enum", "user_only", "schema"];
                const RAW_ONLY_KEYS: &[&str] =
                    &["default", "min", "max", "enum", "user_only", "schema"];
                let is_schema_obj = m.contains_key("properties")
                    || m.contains_key("additionalProperties")
                    || m.contains_key("required")
                    || m.contains_key("items");
                let has_raw_only_key = m.keys().any(|k| RAW_ONLY_KEYS.contains(&k.as_str()));
                let looks_like_compact_object = !has_raw_only_key
                    && !m.is_empty()
                    && m.values().all(|v| match v {
                        Value::String(s) => matches!(
                            s.as_str(),
                            "string" | "integer" | "decimal" | "number" | "boolean" | "object" | "array" | "list" | "null"
                        ),
                        Value::Array(_) | Value::Object(_) => true,
                        _ => false,
                    });
                let is_raw = m.contains_key("type")
                    && !is_schema_obj
                    && (m.keys().all(|k| RAW_KEYS.contains(&k.as_str())) || !looks_like_compact_object);
                if is_raw {
                    let r = Raw::deserialize(Value::Object(m))
                        .map_err(serde::de::Error::custom)?;
                    return Ok(InputDecl {
                        ty: r.ty,
                        default: r.default,
                        min: r.min,
                        max: r.max,
                        options: r.options,
                        user_only: r.user_only,
                        schema: r.schema,
                    });
                }
                let user_only = if is_schema_obj {
                    match m.remove("user_only") {
                        Some(Value::Bool(b)) => b,
                        Some(_) => {
                            return Err(serde::de::Error::custom(
                                "input `user_only` must be a boolean",
                            ))
                        }
                        None => false,
                    }
                } else {
                    false
                };
                let default = if is_schema_obj {
                    m.get("default").cloned()
                } else {
                    None
                };
                let ty = if is_schema_obj && m.get("type").and_then(Value::as_str) == Some("array") {
                    "array"
                } else {
                    "object"
                };
                Ok(InputDecl {
                    ty: ty.to_string(),
                    default,
                    min: None,
                    max: None,
                    options: None,
                    user_only,
                    schema: Some(Value::Object(m)),
                })
            }
        }
        d.deserialize_any(V)
    }
}

#[derive(Deserialize, Serialize, Clone, Debug, Default)]
#[serde(deny_unknown_fields)]
pub struct BindingDecl {
    /// Source path, relative to the ritual file. Kept as provenance only once
    /// the content is embedded.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub document: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub content: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hash: Option<String>,
}

#[derive(Deserialize, Serialize, Clone, Debug, Default)]
#[serde(deny_unknown_fields)]
pub struct ReagentDecl {
    #[serde(default)]
    pub select: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub version: Option<u32>,
}

#[derive(Deserialize, Serialize, Clone, Debug)]
#[serde(deny_unknown_fields)]
pub struct Requirement {
    pub reagent: String,
    pub action: String,
    #[serde(default)]
    pub on: BTreeMap<String, String>,
    /// Each key maps to one value or a list of allowed values.
    #[serde(default)]
    pub into: BTreeMap<String, Value>,
}

/// One indivisible permission: a tool, a resource scope, a destination scope.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Grant {
    pub reagent: String,
    pub action: String,
    pub on: BTreeMap<String, String>,
    pub into: BTreeMap<String, String>,
}

/// Narrow available grants to the declared requirements. Conflicting scopes never overlap.
pub fn narrow_grants(have: &[Grant], requirements: &[Requirement]) -> Vec<Grant> {
    let needs: Vec<Grant> = requirements.iter().flat_map(Requirement::atoms).collect();
    intersect_grants(have, &needs)
}

pub fn intersect_grants(left: &[Grant], right: &[Grant]) -> Vec<Grant> {
    let mut out = Vec::new();
    for a in left {
        for b in right {
            if a.reagent != b.reagent || a.action != b.action {
                continue;
            }
            let merge = |a: &BTreeMap<String, String>,
                         b: &BTreeMap<String, String>|
             -> Option<BTreeMap<String, String>> {
                if a.iter().any(|(k, v)| b.get(k).is_some_and(|w| w != v)) {
                    return None;
                }
                let mut result = a.clone();
                result.extend(b.clone());
                Some(result)
            };
            if let (Some(on), Some(into)) = (merge(&a.on, &b.on), merge(&a.into, &b.into)) {
                let g = Grant {
                    reagent: a.reagent.clone(),
                    action: a.action.clone(),
                    on,
                    into,
                };
                if !out.contains(&g) {
                    out.push(g);
                }
            }
        }
    }
    out
}

impl Grant {
    /// An empty scope on the grant means "any"; otherwise every key the grant
    /// names must match what is needed.
    pub fn covers(&self, need: &Grant) -> bool {
        let scope_ok = |have: &BTreeMap<String, String>, want: &BTreeMap<String, String>| {
            have.iter().all(|(k, v)| want.get(k) == Some(v))
        };
        self.reagent == need.reagent
            && self.action == need.action
            && scope_ok(&self.on, &need.on)
            && scope_ok(&self.into, &need.into)
    }

    pub fn describe(&self) -> String {
        let mut s = format!("{}.{}", self.reagent, self.action);
        for (k, v) in &self.on {
            s.push_str(&format!(" on {k}={v}"));
        }
        for (k, v) in &self.into {
            s.push_str(&format!(" into {k}={v}"));
        }
        s
    }
}

impl Requirement {
    pub fn atoms(&self) -> Vec<Grant> {
        let mut out = vec![Grant {
            reagent: self.reagent.clone(),
            action: self.action.clone(),
            on: self.on.clone(),
            into: BTreeMap::new(),
        }];
        for (k, v) in &self.into {
            let values: Vec<String> = match v {
                Value::Array(a) => a.iter().map(plain_string).collect(),
                other => vec![plain_string(other)],
            };
            let mut next = Vec::new();
            for g in &out {
                for val in &values {
                    let mut g2 = g.clone();
                    g2.into.insert(k.clone(), val.clone());
                    next.push(g2);
                }
            }
            out = next;
        }
        out
    }
}

#[derive(Deserialize, Serialize, Clone, Debug, Default, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct Essence {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_calls: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub items: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub requests: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tokens: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub time: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cost: Option<String>,
}

#[derive(Deserialize, Serialize, Clone, Debug, Default)]
#[serde(deny_unknown_fields)]
pub struct DriftPolicy {
    /// `note`, `warn` (default) or `refuse` when the backend fingerprint no
    /// longer matches the recorded baseline.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub on_mismatch: Option<String>,
}

#[derive(Deserialize, Serialize, Clone, Debug, Default)]
#[serde(deny_unknown_fields)]
pub struct Step {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    // native operation
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub operation: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub arguments: Option<BTreeMap<String, Value>>,
    /// After the operation, observe the resulting state and compare it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub verify: Option<Verify>,
    /// True (or a reference to a boolean) records the operation as a petition
    /// awaiting approval instead of performing it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hold: Option<Value>,
    // model step
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<ModelStep>,
    // ritual step
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ritual: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub version: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hash: Option<String>,
    // bounded iteration
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub each: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub steps: Option<Vec<Step>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub on_failure: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parallel: Option<u32>,
    // condition
    #[serde(rename = "if", default, skip_serializing_if = "Option::is_none")]
    pub cond: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub then: Option<Vec<Step>>,
    #[serde(rename = "else", default, skip_serializing_if = "Option::is_none")]
    pub otherwise: Option<Vec<Step>>,
    // ergonomic shorthands (desugared by normalize)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fail: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub assert: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
    // recovery policy (Operation and Model steps)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub retry: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fallback: Option<Vec<Step>>,
}

/// A read-back that turns "the tool said it worked" into "the runtime
/// observed the expected state".
#[derive(Deserialize, Serialize, Clone, Debug, Default)]
#[serde(deny_unknown_fields)]
pub struct Verify {
    pub observe: String,
    #[serde(default)]
    pub arguments: BTreeMap<String, Value>,
    pub expect: BTreeMap<String, Value>,
}

#[derive(Deserialize, Serialize, Clone, Debug, Default)]
#[serde(deny_unknown_fields)]
pub struct ModelStep {
    /// A model pinned for this step. It is not affected by a `through` override.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(default)]
    pub settings: BTreeMap<String, Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub instructions: Option<String>,
    #[serde(default)]
    pub bind: Vec<String>,
    pub prompt: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub schema: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub abstain_if: Option<String>,
    /// Image handles shown to the model: references only, never literals.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub images: Option<Images>,
}

/// One reference (possibly to a list of handles) or a list of references.
#[derive(Deserialize, Serialize, Clone, Debug)]
#[serde(untagged)]
pub enum Images {
    One(String),
    Many(Vec<String>),
}

#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Kind {
    Operation,
    Model,
    Ritual,
    Each,
    If,
}

impl Step {
    pub fn kind(&self) -> Result<Kind, String> {
        let mut kinds = Vec::new();
        if self.operation.is_some() {
            kinds.push(Kind::Operation);
        }
        if self.model.is_some() {
            kinds.push(Kind::Model);
        }
        if self.ritual.is_some() {
            kinds.push(Kind::Ritual);
        }
        if self.each.is_some() {
            kinds.push(Kind::Each);
        }
        if self.cond.is_some() {
            kinds.push(Kind::If);
        }
        if kinds.len() == 1 {
            let k = kinds[0];
            let forbidden = (k != Kind::Operation
                && (self.verify.is_some() || self.hold.is_some()))
                || (!matches!(k, Kind::Operation | Kind::Ritual) && self.arguments.is_some())
                || (k != Kind::Ritual && (self.version.is_some() || self.hash.is_some()))
                || (k != Kind::Each
                    && (self.each.is_some()
                        || self.steps.is_some()
                        || self.parallel.is_some()
                        || self.on_failure.is_some()))
                || (!matches!(k, Kind::Operation | Kind::Model)
                    && (self.retry.is_some() || self.fallback.is_some()))
                || (k != Kind::If && (self.then.is_some() || self.otherwise.is_some()));
            if forbidden {
                return Err("step contains modifiers that do not apply to its kind".into());
            }
        }
        match kinds.as_slice() {
            [k] => Ok(*k),
            [] => Err("a step must be one of: operation, model, ritual, each, if".into()),
            _ => Err("a step may be only one of: operation, model, ritual, each, if".into()),
        }
    }
}

pub fn plain_string(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        other => other.to_string(),
    }
}

// ---------------------------------------------------------------- expressions


mod expr;
mod inputs;
mod parse;
mod template;
mod types;
mod validate;
#[cfg(test)]
mod tests;

pub use expr::{
    compare_values, descend, eval, eval_condition, expr_refs, parse_expr, values_equal,
    Env, Expr,
};
pub use inputs::{
    check_schema, expand_schema, resolve_authority_inputs, resolve_inputs, user_only_changes,
};
pub use parse::{
    canonical_json, content_hash, parse_definition, parse_file,
    prepare, Resolved, Resolver,
};
pub use template::{render_template, render_value, template_refs};
pub use types::{Effect, Ty};
pub use validate::{
    model_sites, validate, validate_with_cfg, validate_with_return_types_cfg,
};
