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
                let is_schema_obj = m.contains_key("properties")
                    || m.contains_key("additionalProperties")
                    || m.contains_key("required")
                    || m.contains_key("items");
                let is_raw = m.contains_key("type")
                    && !is_schema_obj
                    && m.keys().all(|k| RAW_KEYS.contains(&k.as_str()));
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

#[derive(Debug, Clone, PartialEq)]
pub enum Expr {
    Lit(Value),
    Ref(Vec<String>),
    Not(Box<Expr>),
    And(Box<Expr>, Box<Expr>),
    Or(Box<Expr>, Box<Expr>),
    Eq(Box<Expr>, Box<Expr>),
    Ne(Box<Expr>, Box<Expr>),
    Order(Box<Expr>, Box<Expr>, Compare),
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Compare {
    Lt,
    Le,
    Gt,
    Ge,
}

#[derive(Debug, Clone, PartialEq)]
enum Tok {
    Word(String),
    Str(String),
    Num(Value),
    Order(Compare),
    Eq,
    Ne,
    Open,
    Close,
}

fn lex(src: &str) -> Result<Vec<Tok>, String> {
    let chars: Vec<char> = src.chars().collect();
    let mut i = 0;
    let mut out = Vec::new();
    while i < chars.len() {
        let c = chars[i];
        if c.is_whitespace() {
            i += 1;
        } else if c == '(' {
            out.push(Tok::Open);
            i += 1;
        } else if c == ')' {
            out.push(Tok::Close);
            i += 1;
        } else if c == '=' && chars.get(i + 1) == Some(&'=') {
            out.push(Tok::Eq);
            i += 2;
        } else if c == '!' && chars.get(i + 1) == Some(&'=') {
            out.push(Tok::Ne);
            i += 2;
        } else if c == '<' || c == '>' {
            let equal = chars.get(i + 1) == Some(&'=');
            out.push(Tok::Order(match (c, equal) {
                ('<', false) => Compare::Lt,
                ('<', true) => Compare::Le,
                ('>', false) => Compare::Gt,
                _ => Compare::Ge,
            }));
            i += if equal { 2 } else { 1 };
        } else if c == '"' || c == '\'' {
            let mut s = String::new();
            i += 1;
            loop {
                match chars.get(i) {
                    Some(&ch) if ch == c => break,
                    Some(&ch) => s.push(ch),
                    None => return Err(format!("unterminated string in `{src}`")),
                }
                i += 1;
            }
            i += 1;
            out.push(Tok::Str(s));
        } else if c.is_ascii_digit()
            || (c == '-' && chars.get(i + 1).is_some_and(char::is_ascii_digit))
        {
            let start = i;
            i += 1;
            while i < chars.len()
                && (chars[i].is_ascii_digit() || matches!(chars[i], '.' | 'e' | 'E' | '+' | '-'))
            {
                i += 1;
            }
            let s: String = chars[start..i].iter().collect();
            let n: Value = serde_json::from_str(&s).map_err(|_| format!("bad number `{s}`"))?;
            if !n.is_number() {
                return Err(format!("bad number `{s}`"));
            }
            out.push(Tok::Num(n));
        } else if c.is_alphabetic() || c == '_' {
            let start = i;
            while i < chars.len()
                && (chars[i].is_alphanumeric() || matches!(chars[i], '_' | '-' | '.'))
            {
                i += 1;
            }
            out.push(Tok::Word(chars[start..i].iter().collect()));
        } else {
            return Err(format!("unexpected `{c}` in `{src}`"));
        }
    }
    Ok(out)
}

struct P {
    t: Vec<Tok>,
    i: usize,
}

impl P {
    fn word(&self, w: &str) -> bool {
        matches!(self.t.get(self.i), Some(Tok::Word(x)) if x == w)
    }
    fn or(&mut self) -> Result<Expr, String> {
        let mut l = self.and()?;
        while self.word("or") {
            self.i += 1;
            l = Expr::Or(Box::new(l), Box::new(self.and()?));
        }
        Ok(l)
    }
    fn and(&mut self) -> Result<Expr, String> {
        let mut l = self.not()?;
        while self.word("and") {
            self.i += 1;
            l = Expr::And(Box::new(l), Box::new(self.not()?));
        }
        Ok(l)
    }
    fn not(&mut self) -> Result<Expr, String> {
        if self.word("not") {
            self.i += 1;
            return Ok(Expr::Not(Box::new(self.not()?)));
        }
        self.cmp()
    }
    fn cmp(&mut self) -> Result<Expr, String> {
        let l = self.atom()?;
        match self.t.get(self.i) {
            Some(Tok::Eq) => {
                self.i += 1;
                Ok(Expr::Eq(Box::new(l), Box::new(self.atom()?)))
            }
            Some(Tok::Ne) => {
                self.i += 1;
                Ok(Expr::Ne(Box::new(l), Box::new(self.atom()?)))
            }
            Some(Tok::Order(op)) => {
                let op = *op;
                self.i += 1;
                Ok(Expr::Order(Box::new(l), Box::new(self.atom()?), op))
            }
            _ => Ok(l),
        }
    }
    fn atom(&mut self) -> Result<Expr, String> {
        let t = self.t.get(self.i).cloned().ok_or("expression ends early")?;
        self.i += 1;
        Ok(match t {
            Tok::Str(s) => Expr::Lit(Value::String(s)),
            Tok::Num(n) => Expr::Lit(n),
            Tok::Word(w) => match w.as_str() {
                "true" => Expr::Lit(Value::Bool(true)),
                "false" => Expr::Lit(Value::Bool(false)),
                "null" => Expr::Lit(Value::Null),
                "and" | "or" | "not" => return Err(format!("misplaced `{w}`")),
                _ => Expr::Ref(w.split('.').map(String::from).collect()),
            },
            Tok::Open => {
                let e = self.or()?;
                if self.t.get(self.i) != Some(&Tok::Close) {
                    return Err("missing `)`".into());
                }
                self.i += 1;
                e
            }
            other => return Err(format!("unexpected {other:?}")),
        })
    }
}

pub fn parse_expr(src: &str) -> Result<Expr, String> {
    if src.len() > 4096 {
        return Err("expression exceeds 4096 bytes".into());
    }
    let tokens = lex(src)?;
    if tokens.len() > 256 {
        return Err("expression exceeds 256 tokens".into());
    }
    let mut p = P { t: tokens, i: 0 };
    let e = p.or()?;
    if p.i != p.t.len() {
        return Err(format!("unexpected trailing text in `{src}`"));
    }
    Ok(e)
}

pub fn expr_refs(e: &Expr, out: &mut Vec<Vec<String>>) {
    match e {
        Expr::Lit(_) => {}
        Expr::Ref(p) => out.push(p.clone()),
        Expr::Not(a) => expr_refs(a, out),
        Expr::And(a, b)
        | Expr::Or(a, b)
        | Expr::Eq(a, b)
        | Expr::Ne(a, b)
        | Expr::Order(a, b, _) => {
            expr_refs(a, out);
            expr_refs(b, out);
        }
    }
}

pub trait Env {
    fn lookup(&self, path: &[String]) -> Result<Value, String>;
}

fn as_bool(v: Value, what: &str) -> Result<bool, String> {
    match v {
        Value::Bool(b) => Ok(b),
        other => Err(format!("{what} must be true or false, found {other}")),
    }
}

pub fn eval(e: &Expr, env: &dyn Env) -> Result<Value, String> {
    Ok(match e {
        Expr::Lit(v) => v.clone(),
        Expr::Ref(p) => env.lookup(p)?,
        Expr::Not(a) => Value::Bool(!as_bool(eval(a, env)?, "operand of `not`")?),
        Expr::And(a, b) => Value::Bool(
            as_bool(eval(a, env)?, "operand of `and`")?
                && as_bool(eval(b, env)?, "operand of `and`")?,
        ),
        Expr::Or(a, b) => Value::Bool(
            as_bool(eval(a, env)?, "operand of `or`")?
                || as_bool(eval(b, env)?, "operand of `or`")?,
        ),
        Expr::Eq(a, b) => Value::Bool(eval(a, env)? == eval(b, env)?),
        Expr::Ne(a, b) => Value::Bool(eval(a, env)? != eval(b, env)?),
        Expr::Order(a, b, op) => {
            let (a, b) = (eval(a, env)?, eval(b, env)?);
            let order = compare_values(&a, &b)?;
            Value::Bool(match op {
                Compare::Lt => order.is_lt(),
                Compare::Le => !order.is_gt(),
                Compare::Gt => order.is_gt(),
                Compare::Ge => !order.is_lt(),
            })
        }
    })
}

pub fn compare_values(a: &Value, b: &Value) -> Result<std::cmp::Ordering, String> {
    let integer = |v: &Value| {
        v.as_i64()
            .map(i128::from)
            .or_else(|| v.as_u64().map(i128::from))
    };
    match (a,b) {
        (Value::String(a),Value::String(b)) => Ok(a.cmp(b)),
        _ if integer(a).is_some() && integer(b).is_some() => Ok(integer(a).cmp(&integer(b))),
        (Value::Number(a),Value::Number(b)) if a.is_f64() && b.is_f64() => a.as_f64().unwrap().partial_cmp(&b.as_f64().unwrap()).ok_or("unordered numbers".into()),
        _ => Err("ordering requires two integers, two floating-point numbers, or two strings; no implicit conversion".into()),
    }
}

pub fn eval_condition(src: &str, env: &dyn Env) -> Result<bool, String> {
    as_bool(eval(&parse_expr(src)?, env)?, "a condition")
}

/// Walk a value by path. A null along the way stays null (a skipped step has a
/// null output); a missing key in an object is an error.
pub fn descend(mut v: Value, path: &[String], full: &[String]) -> Result<Value, String> {
    for seg in path {
        v = match v {
            Value::Null => Value::Null,
            Value::Object(mut m) => m
                .remove(seg)
                .ok_or_else(|| format!("`{}` has no field `{seg}`", full.join(".")))?,
            Value::Array(mut a) => {
                let i: usize = seg
                    .parse()
                    .map_err(|_| format!("`{}`: `{seg}` is not a list index", full.join(".")))?;
                if i >= a.len() {
                    return Err(format!("`{}`: index {i} is out of range", full.join(".")));
                }
                a.swap_remove(i)
            }
            _ => {
                return Err(format!(
                    "`{}`: cannot look inside a plain value at `{seg}`",
                    full.join(".")
                ))
            }
        };
    }
    Ok(v)
}

/// Find the `${...}` references in a template string.
pub fn template_refs(s: &str) -> Result<Vec<Vec<String>>, String> {
    let mut out = Vec::new();
    let mut rest = s;
    while let Some(start) = rest.find("${") {
        let after = &rest[start + 2..];
        let end = after
            .find('}')
            .ok_or_else(|| format!("unterminated `${{` in `{s}`"))?;
        let inner = after[..end].trim();
        if inner.is_empty()
            || !inner
                .chars()
                .all(|c| c.is_alphanumeric() || matches!(c, '_' | '-' | '.'))
        {
            return Err(format!(
                "`${{{inner}}}` is not a reference such as `${{inputs.count}}`"
            ));
        }
        out.push(inner.split('.').map(String::from).collect());
        rest = &after[end + 1..];
    }
    Ok(out)
}

/// A string that is exactly one `${ref}` yields the referenced typed value.
/// Any other string has its references substituted as text.
pub fn render_template(s: &str, env: &dyn Env) -> Result<Value, String> {
    let refs = template_refs(s)?;
    if refs.is_empty() {
        return Ok(Value::String(s.to_string()));
    }
    let t = s.trim();
    if refs.len() == 1 && t.starts_with("${") && t.ends_with('}') {
        return env.lookup(&refs[0]);
    }
    let mut out = String::new();
    let mut rest = s;
    for r in refs {
        let start = rest.find("${").unwrap();
        let end = rest[start..].find('}').unwrap() + start;
        out.push_str(&rest[..start]);
        out.push_str(&plain_string(&env.lookup(&r)?));
        rest = &rest[end + 1..];
    }
    out.push_str(rest);
    Ok(Value::String(out))
}

pub fn render_value(v: &Value, env: &dyn Env) -> Result<Value, String> {
    Ok(match v {
        Value::String(s) => render_template(s, env)?,
        Value::Array(a) => Value::Array(
            a.iter()
                .map(|x| render_value(x, env))
                .collect::<Result<_, _>>()?,
        ),
        Value::Object(m) => {
            let mut out = Map::new();
            for (k, x) in m {
                out.insert(k.clone(), render_value(x, env)?);
            }
            Value::Object(out)
        }
        other => other.clone(),
    })
}

// --------------------------------------------------------------------- schema

/// The subset of JSON Schema that output contracts use: type, enum, required,
/// properties, items.
pub fn check_schema(schema: &Value, v: &Value, path: &str) -> Result<(), String> {
    crate::schema::check(schema, v, path)
}

// --------------------------------------------------------------------- inputs

fn coerce(name: &str, decl: &InputDecl, raw: &Value) -> Result<Value, String> {
    let bad = |want: &str| format!("input `{name}` must be {want}, found {raw}");
    let v = match (decl.ty.as_str(), raw) {
        ("integer", Value::Number(n)) if n.is_i64() || n.is_u64() => raw.clone(),
        ("integer", Value::String(s)) => json!(s.parse::<i64>().map_err(|_| bad("an integer"))?),
        ("integer", _) => return Err(bad("an integer")),
        ("decimal", Value::Number(_)) => raw.clone(),
        ("decimal", Value::String(s)) => {
            let n = s.parse::<f64>().map_err(|_| bad("a finite decimal"))?;
            if !n.is_finite() {
                return Err(bad("a finite decimal"));
            }
            json!(n)
        }
        ("decimal", _) => return Err(bad("a decimal")),
        ("boolean", Value::Bool(_)) => raw.clone(),
        ("boolean", Value::String(s)) => match s.as_str() {
            "true" => json!(true),
            "false" => json!(false),
            _ => return Err(bad("true or false")),
        },
        ("boolean", _) => return Err(bad("true or false")),
        ("string", Value::String(_)) => raw.clone(),
        ("string", _) => return Err(bad("a string")),
        ("list" | "array", Value::Array(_)) => raw.clone(),
        ("list" | "array", Value::String(s)) => {
            let v: Value =
                serde_json::from_str(s).map_err(|_| bad("a JSON list such as '[\"a\",\"b\"]'"))?;
            if !v.is_array() {
                return Err(bad("a JSON list"));
            }
            v
        }
        ("list" | "array", _) => return Err(bad("a list")),
        ("object", Value::Object(_)) => raw.clone(),
        ("object", Value::String(s)) => {
            let v: Value = serde_json::from_str(s).map_err(|_| bad("a JSON object such as '{\"a\":1}'"))?;
            if !v.is_object() {
                return Err(bad("a JSON object"));
            }
            v
        }
        ("object", _) => return Err(bad("an object")),
        (other, _) => return Err(format!("input `{name}` has unsupported type `{other}`")),
    };
    if let Some(n) = v.as_f64() {
        if decl.min.is_some_and(|m| n < m) || decl.max.is_some_and(|m| n > m) {
            return Err(format!(
                "input `{name}` = {v} is outside its declared range{}{}",
                decl.min.map(|m| format!(", min {m}")).unwrap_or_default(),
                decl.max.map(|m| format!(", max {m}")).unwrap_or_default()
            ));
        }
    }
    if let Some(schema) = &decl.schema {
        check_schema(schema, &v, &format!("input `{name}`"))?;
    }
    if let Some(options) = &decl.options {
        if !options.contains(&v) {
            return Err(format!(
                "input `{name}` must be one of {}",
                Value::Array(options.clone())
            ));
        }
    }
    Ok(v)
}

/// Inputs marked `user_only` that a caller tried to move off their default.
pub fn user_only_changes(def: &Ritual, supplied: &Map<String, Value>) -> Vec<String> {
    def.inputs
        .iter()
        .filter(|(name, decl)| {
            decl.user_only
                && supplied.get(*name).is_some_and(|raw| {
                    let given = coerce(name, decl, raw).ok();
                    let default = decl
                        .default
                        .as_ref()
                        .and_then(|d| coerce(name, decl, d).ok());
                    given != default
                })
        })
        .map(|(name, _)| name.clone())
        .collect()
}

/// Apply declared types, defaults and constraints. Unknown inputs and missing
/// required inputs are errors; nothing is guessed.
pub fn resolve_inputs(def: &Ritual, supplied: &Map<String, Value>) -> Result<Value, String> {
    resolve_inputs_filtered(def, supplied, None)
}

/// Like `resolve_inputs`, but only requires inputs whose names appear in
/// `requires` (`${inputs.<name>...}`) — used by `petition request` so callers
/// do not have to supply unrelated runtime inputs just to grant authority.
pub fn resolve_authority_inputs(def: &Ritual, supplied: &Map<String, Value>) -> Result<Value, String> {
    let mut needed = BTreeSet::new();
    let collect = |s: &str, out: &mut BTreeSet<String>| {
        let mut rest = s;
        while let Some(start) = rest.find("${inputs.") {
            let after = &rest[start + "${inputs.".len()..];
            if let Some(end) = after.find('}') {
                let path = &after[..end];
                let root = path.split('.').next().unwrap_or("").trim();
                if !root.is_empty() {
                    out.insert(root.to_string());
                }
                rest = &after[end + 1..];
            } else {
                break;
            }
        }
    };
    for req in &def.requires {
        for v in req.on.values() {
            collect(v, &mut needed);
        }
        for v in req.into.values() {
            if let Some(s) = v.as_str() {
                collect(s, &mut needed);
            }
        }
    }
    resolve_inputs_filtered(def, supplied, Some(&needed))
}

fn resolve_inputs_filtered(
    def: &Ritual,
    supplied: &Map<String, Value>,
    only_require: Option<&BTreeSet<String>>,
) -> Result<Value, String> {
    for k in supplied.keys() {
        if !def.inputs.contains_key(k) {
            let known: Vec<&str> = def.inputs.keys().map(String::as_str).collect();
            return Err(format!(
                "ritual `{}` has no input `{k}`; declared inputs: {}",
                def.ritual,
                if known.is_empty() {
                    "none".into()
                } else {
                    known.join(", ")
                }
            ));
        }
    }
    let mut out = Map::new();
    for (name, decl) in &def.inputs {
        if only_require.is_some_and(|req| !req.contains(name) && !supplied.contains_key(name)) {
            continue;
        }
        let v = match (supplied.get(name), &decl.default) {
            (Some(raw), _) => coerce(name, decl, raw)?,
            (None, Some(d)) => coerce(name, decl, d)?,
            (None, None) => {
                return Err(format!(
                    "ritual `{}` requires input `{name}` ({}) and declares no default",
                    def.ritual, decl.ty
                ))
            }
        };
        out.insert(name.clone(), v);
    }
    Ok(Value::Object(out))
}

// ------------------------------------------------------------ load and prepare

const MAX_SCHEMA_DEPTH: usize = 32;

/// Expand compact schema shorthands into standard JSON Schema objects.
pub fn expand_schema(v: Value) -> Result<Value, String> {
    expand_schema_depth(v, 0)
}

fn is_compact_type_token(t: &str) -> bool {
    matches!(
        t,
        "string" | "integer" | "number" | "decimal" | "boolean" | "null" | "object" | "array" | "list"
    ) || ((t.starts_with("list<") || t.starts_with("array<")) && t.ends_with('>'))
}

fn expand_schema_depth(v: Value, depth: usize) -> Result<Value, String> {
    if depth > MAX_SCHEMA_DEPTH {
        return Err(format!("schema nesting exceeds {MAX_SCHEMA_DEPTH} levels"));
    }
    match v {
        Value::String(s) => {
            let t = s.trim();
            match t {
                "string" | "integer" | "number" | "boolean" | "null" | "object" | "array" => {
                    Ok(json!({"type": t}))
                }
                "decimal" => Ok(json!({"type": "number"})),
                "list" => Ok(json!({"type": "array"})),
                _ => {
                    let inner = t
                        .strip_prefix("list<")
                        .or_else(|| t.strip_prefix("array<"))
                        .and_then(|rest| rest.strip_suffix('>'));
                    if let Some(item) = inner {
                        Ok(json!({
                            "type": "array",
                            "items": expand_schema_depth(Value::String(item.trim().to_string()), depth + 1)?
                        }))
                    } else {
                        Err(format!("unknown compact schema type `{s}`"))
                    }
                }
            }
        }
        Value::Array(arr) if !arr.is_empty() => {
            let is_typed_list = arr.len() == 1
                && (arr[0].is_object()
                    || arr[0].is_array()
                    || arr[0]
                        .as_str()
                        .is_some_and(|s| is_compact_type_token(s.trim())));
            if is_typed_list {
                let item = arr.into_iter().next().unwrap();
                Ok(json!({
                    "type": "array",
                    "items": expand_schema_depth(item, depth + 1)?
                }))
            } else if arr.iter().all(|x| x.is_string()) {
                Ok(json!({"type": "string", "enum": arr}))
            } else {
                Ok(json!({"enum": arr}))
            }
        }
        Value::Object(mut m) => {
            const SCHEMA_KEYWORDS: &[&str] = &[
                "type",
                "properties",
                "enum",
                "items",
                "additionalProperties",
                "required",
                "$schema",
                "anyOf",
                "oneOf",
                "allOf",
                "minimum",
                "maximum",
                "minLength",
                "maxLength",
                "minItems",
                "maxItems",
                "title",
                "description",
                "default",
                "pattern",
                "format",
                "$defs",
                "$ref",
            ];
            let has_structural_schema_key = [
                "properties",
                "enum",
                "items",
                "additionalProperties",
                "required",
                "$schema",
                "anyOf",
                "oneOf",
                "allOf",
                "$defs",
                "$ref",
            ]
            .iter()
            .any(|k| m.contains_key(*k));
            let is_json_schema = has_structural_schema_key
                || (m.contains_key("type")
                    && m.keys().all(|k| SCHEMA_KEYWORDS.contains(&k.as_str())));
            if is_json_schema {
                if let Some(Value::Object(defs)) = m.remove("$defs") {
                    let mut out = Map::new();
                    for (k, val) in defs {
                        out.insert(k, expand_schema_depth(val, depth + 1)?);
                    }
                    m.insert("$defs".into(), Value::Object(out));
                }
                if let Some(items) = m.remove("items") {
                    m.insert("items".into(), expand_schema_depth(items, depth + 1)?);
                }
                if let Some(Value::Object(props)) = m.remove("properties") {
                    let mut out = Map::new();
                    for (k, val) in props {
                        out.insert(k, expand_schema_depth(val, depth + 1)?);
                    }
                    m.insert("properties".into(), Value::Object(out));
                }
                for key in ["anyOf", "oneOf", "allOf"] {
                    if let Some(Value::Array(variants)) = m.remove(key) {
                        let mut out = Vec::with_capacity(variants.len());
                        for var in variants {
                            out.push(expand_schema_depth(var, depth + 1)?);
                        }
                        m.insert(key.into(), Value::Array(out));
                    }
                }
                return Ok(Value::Object(m));
            }
            let mut props = Map::new();
            let mut required = Vec::new();
            for (k, val) in m {
                let (name, opt) = match k.strip_suffix('?') {
                    Some(n) => (n.to_string(), true),
                    None => (k, false),
                };
                if !opt {
                    required.push(Value::String(name.clone()));
                }
                props.insert(name, expand_schema_depth(val, depth + 1)?);
            }
            Ok(json!({
                "type": "object",
                "additionalProperties": false,
                "required": required,
                "properties": props,
            }))
        }
        other => Err(format!("invalid schema `{other}`")),
    }
}

fn normalize_ref(path: &str) -> String {
    let segs: Vec<&str> = path.split('.').collect();
    if segs.len() >= 2 && segs[0] == "steps" {
        if segs.len() == 2 {
            return format!("steps.{}.output", segs[1]);
        }
        if !matches!(segs[2], "output" | "status" | "kind" | "counts") {
            return format!("steps.{}.output.{}", segs[1], segs[2..].join("."));
        }
    }
    path.to_string()
}

fn is_bare_ref(s: &str) -> bool {
    let mut chars = s.chars();
    let Some(first) = chars.next() else {
        return false;
    };
    (first.is_ascii_alphabetic() || first == '_')
        && chars.all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '.'))
}

fn normalize_template_str(s: &mut String) {
    let trimmed = s.trim();
    if let Some(rest) = trimmed.strip_prefix('$') {
        if !rest.starts_with('{') && is_bare_ref(rest) {
            *s = format!("${{{}}}", normalize_ref(rest));
            return;
        }
    }
    if !s.contains("${") {
        return;
    }
    let mut out = String::with_capacity(s.len());
    let mut rest = s.as_str();
    while let Some(start) = rest.find("${") {
        out.push_str(&rest[..start + 2]);
        let after = &rest[start + 2..];
        let Some(end) = after.find('}') else {
            out.push_str(after);
            rest = "";
            break;
        };
        let inner = after[..end].trim();
        if !inner.is_empty()
            && inner
                .chars()
                .all(|c| c.is_alphanumeric() || matches!(c, '_' | '-' | '.'))
        {
            out.push_str(&normalize_ref(inner));
        } else {
            out.push_str(&after[..end]);
        }
        out.push('}');
        rest = &after[end + 1..];
    }
    out.push_str(rest);
    *s = out;
}

fn normalize_template_value(v: &mut Value) {
    match v {
        Value::String(s) => normalize_template_str(s),
        Value::Array(a) => a.iter_mut().for_each(normalize_template_value),
        Value::Object(m) => m.values_mut().for_each(normalize_template_value),
        _ => {}
    }
}

fn normalize_expr_refs(src: &str) -> String {
    let chars: Vec<char> = src.chars().collect();
    let mut out = String::with_capacity(src.len());
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        if c == '"' || c == '\'' {
            let q = c;
            out.push(c);
            i += 1;
            while i < chars.len() {
                let ch = chars[i];
                out.push(ch);
                i += 1;
                if ch == '\\' && i < chars.len() {
                    out.push(chars[i]);
                    i += 1;
                } else if ch == q {
                    break;
                }
            }
        } else if c == '$' && chars.get(i + 1) == Some(&'{') {
            let start = i + 2;
            let mut end = start;
            while end < chars.len() && chars[end] != '}' && chars[end] != '{' {
                end += 1;
            }
            if end < chars.len() && chars[end] == '}' {
                let inner: String = chars[start..end].iter().collect();
                let trimmed = inner.trim();
                if !trimmed.is_empty() {
                    out.push_str(&normalize_ref(trimmed));
                    i = end + 1;
                    continue;
                }
            }
            out.push(c);
            i += 1;
        } else if c.is_alphabetic() || c == '_' {
            let start = i;
            while i < chars.len()
                && (chars[i].is_alphanumeric() || matches!(chars[i], '_' | '-' | '.'))
            {
                i += 1;
            }
            let word: String = chars[start..i].iter().collect();
            if word.starts_with("steps.") {
                out.push_str(&normalize_ref(&word));
            } else {
                out.push_str(&word);
            }
        } else {
            out.push(c);
            i += 1;
        }
    }
    out
}

fn data_assert_step(id: String, msg: String) -> Step {
    Step {
        id: Some(id),
        operation: Some("data.assert".into()),
        arguments: Some(BTreeMap::from([
            ("condition".into(), json!(false)),
            ("message".into(), Value::String(msg)),
        ])),
        ..Default::default()
    }
}

fn schema_uses_refs(v: &Value) -> bool {
    match v {
        Value::Object(m) => {
            m.contains_key("$defs")
                || m.contains_key("$ref")
                || m.values().any(schema_uses_refs)
        }
        Value::Array(a) => a.iter().any(schema_uses_refs),
        _ => false,
    }
}

fn resolve_ritual_schema(sc: Value, defs: &BTreeMap<String, Value>) -> Result<Value, String> {
    let mut expanded = expand_schema(sc)?;
    if defs.is_empty() && !schema_uses_refs(&expanded) {
        return Ok(expanded);
    }
    if !defs.is_empty() {
        let obj = expanded
            .as_object_mut()
            .ok_or_else(|| "schema must be an object".to_string())?;
        let mut merged = Map::new();
        for (k, v) in defs {
            merged.insert(k.clone(), v.clone());
        }
        if let Some(local) = obj.remove("$defs") {
            let local_obj = local
                .as_object()
                .ok_or_else(|| "`$defs` must be an object".to_string())?;
            for (k, v) in local_obj {
                if merged.contains_key(k) {
                    return Err(format!("duplicate `$defs` entry `{k}`"));
                }
                merged.insert(k.clone(), v.clone());
            }
        }
        obj.insert("$defs".into(), Value::Object(merged));
    }
    crate::schema::inline_refs(&expanded)
}

fn normalize_steps(
    steps: &mut [Step],
    defs: &BTreeMap<String, Value>,
    data_tools: &mut std::collections::BTreeSet<String>,
    seq: &mut usize,
) -> Result<(), String> {
    for (idx, s) in steps.iter_mut().enumerate() {
        let step_label = s.id.clone().unwrap_or_else(|| format!("#{}", idx + 1));
        if let Some(msg) = s.fail.take() {
            if s.operation.is_some()
                || s.model.is_some()
                || s.ritual.is_some()
                || s.each.is_some()
                || s.cond.is_some()
                || s.assert.is_some()
                || s.message.is_some()
                || s.arguments.is_some()
                || s.verify.is_some()
                || s.hold.is_some()
                || s.steps.is_some()
                || s.then.is_some()
                || s.otherwise.is_some()
            {
                return Err("`fail` step cannot be combined with other step kinds or modifiers".into());
            }
            *seq += 1;
            let id = s.id.take().unwrap_or_else(|| format!("_fail_{seq}"));
            let failed = data_assert_step(id, msg);
            s.id = failed.id;
            s.operation = failed.operation;
            s.arguments = failed.arguments;
        } else if let Some(cond) = s.assert.take() {
            if s.operation.is_some()
                || s.model.is_some()
                || s.ritual.is_some()
                || s.each.is_some()
                || s.cond.is_some()
                || s.arguments.is_some()
                || s.verify.is_some()
                || s.hold.is_some()
                || s.steps.is_some()
                || s.then.is_some()
                || s.otherwise.is_some()
            {
                return Err("`assert` step cannot be combined with other step kinds or modifiers".into());
            }
            *seq += 1;
            let inner_id = format!("_assert_{seq}");
            let msg = s
                .message
                .take()
                .unwrap_or_else(|| format!("assertion failed: {cond}"));
            let norm_cond = normalize_expr_refs(&cond);
            s.cond = Some(format!("not ({norm_cond})"));
            s.then = Some(vec![data_assert_step(inner_id, msg)]);
        } else if s.message.is_some() {
            return Err("step modifier `message` only applies to `assert`".into());
        }

        if let Some(op) = s.operation.clone() {
            if let Some(tool) = op.strip_prefix("data.") {
                data_tools.insert(tool.to_string());
            }
            if op == "data.parse_json" {
                if let Some(args) = &mut s.arguments {
                    if let Some(sc) = args.get_mut("schema") {
                        *sc = resolve_ritual_schema(sc.clone(), defs)
                            .map_err(|e| format!("steps.{step_label}.arguments.schema: {e}"))?;
                    }
                }
            }
            if matches!(op.as_str(), "data.map" | "data.fold") {
                if let Some(args) = &mut s.arguments {
                    if let Some(Value::String(inner_op)) = args.get("operation") {
                        if let Some(t) = inner_op.strip_prefix("data.") {
                            data_tools.insert(t.to_string());
                        }
                    }
                    if let Some(inner_args) = args.get_mut("arguments") {
                        normalize_template_value(inner_args);
                    }
                }
            }
            if op == "data.filter" {
                if let Some(args) = &mut s.arguments {
                    if let Some(Value::String(pred)) = args.get_mut("predicate") {
                        *pred = normalize_expr_refs(pred);
                    }
                }
            }
        }
        if let Some(args) = &mut s.arguments {
            for v in args.values_mut() {
                normalize_template_value(v);
            }
        }
        if let Some(v) = &mut s.verify {
            if let Some(tool) = v.observe.strip_prefix("data.") {
                data_tools.insert(tool.to_string());
            }
            for val in v.arguments.values_mut() {
                normalize_template_value(val);
            }
            for val in v.expect.values_mut() {
                normalize_template_value(val);
            }
        }
        if let Some(hold) = &mut s.hold {
            normalize_template_value(hold);
        }
        if let Some(m) = &mut s.model {
            normalize_template_str(&mut m.prompt);
            if let Some(sc) = &mut m.schema {
                *sc = resolve_ritual_schema(sc.clone(), defs)
                    .map_err(|e| format!("steps.{step_label}.model.schema: {e}"))?;
            }
            if let Some(ab) = &mut m.abstain_if {
                *ab = normalize_expr_refs(ab);
            }
        }
        if let Some(cond) = &mut s.cond {
            *cond = normalize_expr_refs(cond);
        }
        if let Some(each) = &mut s.each {
            if let Some((var, rhs)) = each.split_once(" in ") {
                *each = format!("{} in {}", var.trim(), normalize_ref(rhs.trim()));
            }
        }
        for inner in [
            &mut s.steps,
            &mut s.then,
            &mut s.otherwise,
            &mut s.fallback,
        ]
        .into_iter()
        .flatten()
        {
            normalize_steps(inner, defs, data_tools, seq)?;
        }
    }
    Ok(())
}

/// Desugar ergonomic authoring shorthands into canonical ritual form.
pub fn normalize(def: &mut Ritual) -> Result<(), String> {
    if !def.defs.is_empty() {
        let mut expanded_defs = BTreeMap::new();
        for (name, sc) in &def.defs {
            let exp = expand_schema(sc.clone())
                .map_err(|e| format!("$defs.{name}: {e}"))?;
            expanded_defs.insert(name.clone(), exp);
        }
        let defs_root = Value::Object(
            [(
                "$defs".to_string(),
                Value::Object(expanded_defs.clone().into_iter().collect()),
            )]
            .into_iter()
            .collect(),
        );
        crate::schema::validate(&defs_root).map_err(|e| format!("$defs: {e}"))?;
        def.defs = expanded_defs;
    }
    for (name, decl) in &mut def.inputs {
        if let Some(sc) = &mut decl.schema {
            *sc = resolve_ritual_schema(sc.clone(), &def.defs)
                .map_err(|e| format!("inputs.{name}.schema: {e}"))?;
        }
    }
    if let Some(sc) = &mut def.output_schema {
        *sc = resolve_ritual_schema(sc.clone(), &def.defs)
            .map_err(|e| format!("output_schema: {e}"))?;
    }
    for v in def.ret.values_mut() {
        normalize_template_value(v);
    }
    let mut data_tools = std::collections::BTreeSet::new();
    let mut seq = 0;
    normalize_steps(&mut def.steps, &def.defs, &mut data_tools, &mut seq)?;

    let mut from_requires: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for req in &def.requires {
        let entry = from_requires.entry(req.reagent.clone()).or_default();
        if !entry.contains(&req.action) {
            entry.push(req.action.clone());
        }
    }
    for (reagent, select) in from_requires {
        def.reagents
            .entry(reagent)
            .or_insert(ReagentDecl { select, version: None });
    }
    if !data_tools.is_empty() && !def.reagents.contains_key("data") {
        def.reagents.insert(
            "data".into(),
            ReagentDecl {
                select: data_tools.into_iter().collect(),
                version: None,
            },
        );
    }
    Ok(())
}

/// Format a `serde_yaml::Error` with line and column context when available.
pub fn format_yaml_error(e: &serde_yaml::Error) -> String {
    match e.location() {
        Some(loc) => format!("line {}, column {}: {e}", loc.line(), loc.column()),
        None => e.to_string(),
    }
}

pub fn parse_file(text: &str) -> Result<Ritual, String> {
    let mut def: Ritual = serde_yaml::from_str(text).map_err(|e| format_yaml_error(&e))?;
    normalize(&mut def)?;
    Ok(def)
}

/// JSON is parsed strictly as JSON; builders are never executed here.
pub fn parse_definition(path: &Path, text: &str) -> Result<Ritual, String> {
    let mut def: Ritual = match path.extension().and_then(|s| s.to_str()) {
        Some("json") => serde_json::from_str(text).map_err(|e| {
            format!(
                "{}: line {}, column {}: invalid JSON definition: {e}",
                path.display(),
                e.line(),
                e.column()
            )
        })?,
        Some("py") => {
            return Err("Python builders are not executable ritual inputs. Run a trusted builder explicitly with Python, then compile its emitted .json definition.".into());
        }
        _ => return parse_file(text).map_err(|e| format!("{}: {e}", path.display())),
    };
    normalize(&mut def)?;
    Ok(def)
}

pub fn canonical_json(def: &Ritual) -> String {
    // serde_json maps are ordered by key, so this form is stable.
    serde_json::to_string(&serde_json::to_value(def).unwrap_or(Value::Null)).unwrap_or_default()
}

pub fn content_hash(def: &Ritual) -> String {
    sha256_hex(canonical_json(def).as_bytes())
}

/// What a resolver returns for a saved ritual: version, content hash, definition.
pub type Resolved = (u32, String, Ritual);
pub type Resolver<'a> = &'a dyn Fn(&str, Option<u32>) -> Option<Resolved>;

fn pin_steps(steps: &mut [Step], resolve: Resolver) -> Result<(), String> {
    for s in steps {
        if let Some(name) = s.ritual.clone() {
            let (ver, hash, _) = resolve(&name, s.version).ok_or_else(|| match s.version {
                Some(v) => format!("ritual step: `{name}` version {v} is not in the grimoire"),
                None => format!("ritual step: `{name}` is not in the grimoire; inscribe it first"),
            })?;
            s.version = Some(ver);
            s.hash = Some(hash);
        }
        for inner in [&mut s.steps, &mut s.then, &mut s.otherwise]
            .into_iter()
            .flatten()
        {
            pin_steps(inner, resolve)?;
        }
    }
    Ok(())
}

/// Embed bound documents and pin child rituals, so the saved definition no
/// longer depends on files or on later versions of anything it calls.
pub fn prepare(def: &mut Ritual, base: &Path, resolve: Resolver) -> Result<(), String> {
    for (name, b) in def.bindings.iter_mut() {
        if b.content.is_none() {
            let rel = b
                .document
                .clone()
                .ok_or_else(|| format!("binding `{name}` needs `document` or `content`"))?;
            let path = base.join(&rel);
            let text = std::fs::read_to_string(&path)
                .map_err(|e| format!("binding `{name}`: cannot read {}: {e}", path.display()))?;
            b.content = Some(text);
        }
        b.hash = Some(sha256_hex(b.content.as_deref().unwrap_or("").as_bytes()));
    }
    pin_steps(&mut def.steps, resolve)
}

// ----------------------------------------------------------------- validation

#[derive(Serialize, Clone, Debug)]
pub struct Effect {
    pub step: String,
    pub kind: String,
    pub what: String,
    pub effect: String,
    pub determinism: String,
    /// How the effect is verified and whether it may be held for approval.
    pub notes: Vec<String>,
}

/// Static type inferred for inputs, step outputs, loop variables, and expressions.
#[derive(Clone, Debug, PartialEq)]
pub enum Ty {
    Any,
    Null,
    Bool,
    Int,
    Num,
    Str,
    List(Box<Ty>),
    Obj {
        fields: BTreeMap<String, Ty>,
        closed: bool,
    },
    Nullable(Box<Ty>),
}

fn closed_obj<const N: usize>(entries: [(&str, Ty); N]) -> Ty {
    Ty::Obj {
        fields: entries
            .into_iter()
            .map(|(k, v)| (k.to_string(), v))
            .collect(),
        closed: true,
    }
}

fn nullable(ty: Ty) -> Ty {
    match ty {
        Ty::Any | Ty::Null | Ty::Nullable(_) => ty,
        other => Ty::Nullable(Box::new(other)),
    }
}

impl Ty {
    pub fn label(&self) -> String {
        match self {
            Ty::Any => "any".into(),
            Ty::Null => "null".into(),
            Ty::Bool => "boolean".into(),
            Ty::Int => "integer".into(),
            Ty::Num => "decimal".into(),
            Ty::Str => "string".into(),
            Ty::List(elem) => {
                if **elem == Ty::Any {
                    "list".into()
                } else {
                    format!("list of {}", elem.label())
                }
            }
            Ty::Obj { .. } => "object".into(),
            Ty::Nullable(inner) => format!("nullable {}", inner.label()),
        }
    }

    /// The static type of a declared input; an `object` or `list`/`array` with
    /// a schema is typed from that schema.
    pub fn from_decl(decl: &InputDecl) -> Ty {
        match (decl.ty.as_str(), &decl.schema) {
            ("object" | "list" | "array", Some(schema)) => Ty::from_schema(schema),
            (ty, _) => Ty::from_input(ty),
        }
    }

    pub fn from_input(ty: &str) -> Ty {
        match ty {
            "integer" => Ty::Int,
            "decimal" => Ty::Num,
            "boolean" => Ty::Bool,
            "string" => Ty::Str,
            "list" | "array" => Ty::List(Box::new(Ty::Any)),
            _ => Ty::Any,
        }
    }

    pub fn from_schema(s: &Value) -> Ty {
        match s.get("type").and_then(Value::as_str) {
            Some("integer") => Ty::Int,
            Some("number") => Ty::Num,
            Some("boolean") => Ty::Bool,
            Some("string") => Ty::Str,
            Some("null") => Ty::Null,
            Some("array") => {
                let elem = s.get("items").map(Ty::from_schema).unwrap_or(Ty::Any);
                Ty::List(Box::new(elem))
            }
            Some("object") => match s.get("properties").and_then(Value::as_object) {
                Some(props) => {
                    let fields = props
                        .iter()
                        .map(|(k, v)| (k.clone(), Ty::from_schema(v)))
                        .collect();
                    let closed = if props.is_empty() {
                        s.get("additionalProperties") == Some(&Value::Bool(false))
                    } else {
                        s.get("additionalProperties") != Some(&Value::Bool(true))
                    };
                    Ty::Obj { fields, closed }
                }
                None => Ty::Obj {
                    fields: BTreeMap::new(),
                    closed: s.get("additionalProperties") == Some(&Value::Bool(false)),
                },
            },
            _ => {
                if let Some(arr) = s.get("enum").and_then(Value::as_array) {
                    if !arr.is_empty() {
                        if arr.iter().all(Value::is_string) {
                            return Ty::Str;
                        }
                        if arr.iter().all(|v| v.is_i64() || v.is_u64()) {
                            return Ty::Int;
                        }
                        if arr.iter().all(Value::is_boolean) {
                            return Ty::Bool;
                        }
                    }
                }
                Ty::Any
            }
        }
    }

    pub fn compatible_with(&self, expected: &Ty) -> bool {
        match (self, expected) {
            (Ty::Any, _) | (_, Ty::Any) => true,
            (Ty::Nullable(a), b) => a.compatible_with(b) || matches!(b, Ty::Null | Ty::Nullable(_)),
            (a, Ty::Nullable(b)) => *a == Ty::Null || a.compatible_with(b),
            (Ty::Int, Ty::Int | Ty::Num) => true,
            (Ty::Num, Ty::Num) => true,
            (Ty::Bool, Ty::Bool) => true,
            (Ty::Str, Ty::Str) => true,
            (Ty::Null, Ty::Null) => true,
            (Ty::List(a), Ty::List(b)) => a.compatible_with(b),
            (
                Ty::Obj {
                    fields: a,
                    closed: a_closed,
                },
                Ty::Obj { fields: b, .. },
            ) => b.iter().all(|(k, b_ty)| match a.get(k) {
                Some(a_ty) => a_ty.compatible_with(b_ty),
                None => !*a_closed || matches!(b_ty, Ty::Nullable(_)),
            }),
            _ => false,
        }
    }
}

fn operation_output_type(
    op: &str,
    args: Option<&BTreeMap<String, Value>>,
    arg_types: &BTreeMap<String, Ty>,
) -> Ty {
    match op {
        "data.arithmetic" | "data.count" | "data.sum_integers" | "data.clamp" | "data.abs" => Ty::Int,
        "data.compare"
        | "data.all"
        | "data.any"
        | "data.logic"
        | "data.contains"
        | "data.starts_with"
        | "data.ends_with" => Ty::Bool,
        "data.assert" => closed_obj([("passed", Ty::Bool)]),
        "data.trim"
        | "data.join"
        | "data.replace"
        | "data.sha256"
        | "data.canonical_json"
        | "data.case" => Ty::Str,
        "data.keys" | "data.split" | "data.lines" => Ty::List(Box::new(Ty::Str)),
        "data.min" | "data.max" => match arg_types.get("values") {
            Some(Ty::List(elem)) => (**elem).clone(),
            _ => Ty::Any,
        },
        "data.select" => match (arg_types.get("then"), arg_types.get("else")) {
            (Some(a), Some(b)) if a == b => a.clone(),
            (Some(Ty::Null), Some(b)) => nullable(b.clone()),
            (Some(a), Some(Ty::Null)) => nullable(a.clone()),
            _ => Ty::Any,
        },
        "data.slice" | "data.sort" | "data.unique" | "data.filter" => match arg_types.get("values")
        {
            Some(Ty::List(elem)) => Ty::List(elem.clone()),
            _ => Ty::List(Box::new(Ty::Any)),
        },
        "data.concat" => match (arg_types.get("left"), arg_types.get("right")) {
            (Some(Ty::List(a)), Some(Ty::List(b))) if a == b => Ty::List(a.clone()),
            _ => Ty::List(Box::new(Ty::Any)),
        },
        "data.map" => {
            let elem = args
                .and_then(|m| m.get("operation"))
                .and_then(Value::as_str)
                .map(|inner| operation_output_type(inner, None, &BTreeMap::new()))
                .unwrap_or(Ty::Any);
            Ty::List(Box::new(elem))
        }
        "data.fold" => args
            .and_then(|m| m.get("operation"))
            .and_then(Value::as_str)
            .map(|inner| operation_output_type(inner, None, &BTreeMap::new()))
            .unwrap_or(Ty::Any),
        "data.parse_json" => args
            .and_then(|m| m.get("schema"))
            .map(Ty::from_schema)
            .unwrap_or(Ty::Any),
        "data.project" => match args
            .and_then(|m| m.get("fields"))
            .and_then(Value::as_array)
        {
            Some(arr) if arr.iter().all(Value::is_string) => {
                let src = arg_types.get("value");
                let mut fields = BTreeMap::new();
                for f in arr {
                    let name = f.as_str().unwrap().to_string();
                    let f_ty = match src {
                        Some(Ty::Obj { fields: sf, .. }) => sf.get(&name).cloned().unwrap_or(Ty::Any),
                        _ => Ty::Any,
                    };
                    fields.insert(name, f_ty);
                }
                Ty::Obj {
                    fields,
                    closed: true,
                }
            }
            _ => Ty::Obj {
                fields: BTreeMap::new(),
                closed: false,
            },
        },
        "data.merge" => match (arg_types.get("left"), arg_types.get("right")) {
            (
                Some(Ty::Obj {
                    fields: lf,
                    closed: lc,
                }),
                Some(Ty::Obj {
                    fields: rf,
                    closed: rc,
                }),
            ) => {
                let mut fields = lf.clone();
                let policy = args
                    .and_then(|m| m.get("conflict"))
                    .and_then(Value::as_str)
                    .unwrap_or("right");
                for (k, v) in rf {
                    if !fields.contains_key(k) || policy != "left" {
                        fields.insert(k.clone(), v.clone());
                    }
                }
                Ty::Obj {
                    fields,
                    closed: *lc && *rc,
                }
            }
            _ => Ty::Obj {
                fields: BTreeMap::new(),
                closed: false,
            },
        },
        "data.set" => match (
            arg_types.get("value"),
            args.and_then(|m| m.get("field")).and_then(Value::as_str),
        ) {
            (
                Some(Ty::Obj {
                    fields: vf,
                    closed,
                }),
                Some(field),
            ) if !field.contains("${") => {
                let mut fields = vf.clone();
                let item_ty = arg_types.get("item").cloned().unwrap_or(Ty::Any);
                fields.insert(field.to_string(), item_ty);
                Ty::Obj {
                    fields,
                    closed: *closed,
                }
            }
            _ => Ty::Obj {
                fields: BTreeMap::new(),
                closed: false,
            },
        },
        "data.omit" => match (
            arg_types.get("value"),
            args.and_then(|m| m.get("fields")).and_then(Value::as_array),
        ) {
            (
                Some(Ty::Obj {
                    fields: vf,
                    closed,
                }),
                Some(arr),
            ) if arr.iter().all(|v| v.as_str().is_some_and(|s| !s.contains("${"))) => {
                let mut fields = vf.clone();
                for f in arr {
                    if let Some(name) = f.as_str() {
                        fields.remove(name);
                    }
                }
                Ty::Obj {
                    fields,
                    closed: *closed,
                }
            }
            _ => Ty::Obj {
                fields: BTreeMap::new(),
                closed: false,
            },
        },
        "data.entries" => Ty::List(Box::new(closed_obj([("key", Ty::Str), ("value", Ty::Any)]))),
        "data.from_entries" => Ty::Obj {
            fields: BTreeMap::new(),
            closed: false,
        },
        "data.get" => Ty::Any,
        "mail.search" => Ty::List(Box::new(closed_obj([
            ("id", Ty::Str),
            ("from", Ty::Str),
            ("subject", Ty::Str),
            ("date", Ty::Str),
        ]))),
        "mail.read" => closed_obj([
            ("id", Ty::Str),
            ("folder", Ty::Str),
            ("from", Ty::Str),
            ("subject", Ty::Str),
            ("text", Ty::Str),
            ("truncated", Ty::Bool),
        ]),
        "mail.locate" => closed_obj([
            ("id", Ty::Str),
            ("folder", nullable(Ty::Str)),
            ("exists", Ty::Bool),
        ]),
        "mail.move" => closed_obj([
            ("id", Ty::Str),
            ("from", Ty::Str),
            ("to", Ty::Str),
            ("moved", Ty::Bool),
            ("already_there", nullable(Ty::Bool)),
            ("held", nullable(Ty::Bool)),
            ("petition", nullable(Ty::Str)),
        ]),
        "file.read_text" => closed_obj([
            ("root", Ty::Str),
            ("path", Ty::Str),
            ("text", Ty::Str),
            ("bytes", Ty::Int),
            ("sha256", Ty::Str),
        ]),
        "file.read_json" => {
            let val_ty = args
                .and_then(|m| m.get("schema"))
                .map(Ty::from_schema)
                .unwrap_or(Ty::Any);
            closed_obj([
                ("root", Ty::Str),
                ("path", Ty::Str),
                ("value", val_ty),
                ("bytes", Ty::Int),
                ("sha256", Ty::Str),
            ])
        }
        "file.stat" => closed_obj([
            ("root", Ty::Str),
            ("path", Ty::Str),
            ("exists", Ty::Bool),
            ("kind", nullable(Ty::Str)),
            ("bytes", nullable(Ty::Int)),
            ("sha256", nullable(Ty::Str)),
        ]),
        "file.list" => closed_obj([
            ("root", Ty::Str),
            ("path", Ty::Str),
            (
                "entries",
                Ty::List(Box::new(closed_obj([("name", Ty::Str), ("kind", Ty::Str)]))),
            ),
        ]),
        "file.write_text" => closed_obj([
            ("root", Ty::Str),
            ("path", Ty::Str),
            ("bytes", Ty::Int),
            ("sha256", Ty::Str),
            ("held", nullable(Ty::Bool)),
            ("petition", nullable(Ty::Str)),
        ]),
        "file.append_jsonl" => closed_obj([
            ("root", Ty::Str),
            ("path", Ty::Str),
            ("bytes_appended", Ty::Int),
            ("record_sha256", Ty::Str),
            ("sha256", Ty::Str),
            ("held", nullable(Ty::Bool)),
            ("petition", nullable(Ty::Str)),
        ]),
        "http.get_text" => closed_obj([
            ("url", Ty::Str),
            ("status", Ty::Int),
            ("content_type", Ty::Str),
            ("text", Ty::Str),
            ("bytes", Ty::Int),
            ("sha256", Ty::Str),
        ]),
        "http.get_json" => {
            let val_ty = args
                .and_then(|m| m.get("schema"))
                .map(Ty::from_schema)
                .unwrap_or(Ty::Any);
            closed_obj([
                ("url", Ty::Str),
                ("status", Ty::Int),
                ("content_type", Ty::Str),
                ("value", val_ty),
                ("bytes", Ty::Int),
                ("sha256", Ty::Str),
            ])
        }
        "trial.run" => closed_obj([
            ("passed", Ty::Bool),
            ("code", nullable(Ty::Int)),
            ("tail", Ty::Str),
        ]),
        "cmd.list" => Ty::List(Box::new(closed_obj([
            ("command", Ty::Str),
            ("effect", Ty::Str),
            ("params", Ty::List(Box::new(Ty::Str))),
        ]))),
        "cmd.read" | "cmd.write" => closed_obj([
            ("command", Ty::Str),
            ("passed", Ty::Bool),
            ("code", Ty::Int),
            ("stdout", Ty::Str),
            ("stderr", Ty::Str),
            ("tail", Ty::Str),
            ("json", Ty::Any),
            ("held", nullable(Ty::Bool)),
            ("petition", nullable(Ty::Str)),
        ]),
        "git.repos" => Ty::List(Box::new(Ty::Str)),
        "git.status" => closed_obj([
            ("branch", nullable(Ty::Str)),
            ("clean", Ty::Bool),
            ("changed", Ty::List(Box::new(Ty::Str))),
        ]),
        "git.ahead_behind" => closed_obj([("ahead", Ty::Int), ("behind", Ty::Int)]),
        "git.merged" => closed_obj([("merged", Ty::Bool)]),
        "git.head" => closed_obj([("commit", Ty::Str)]),
        "git.remote_head" => closed_obj([("commit", nullable(Ty::Str))]),
        "git.worktrees" => Ty::List(Box::new(closed_obj([
            ("path", Ty::Str),
            ("branch", nullable(Ty::Str)),
            ("head", Ty::Str),
            ("clean", nullable(Ty::Bool)),
            ("main", Ty::Bool),
            ("prunable", Ty::Bool),
            ("locked", Ty::Bool),
        ]))),
        "git.branches" => Ty::List(Box::new(closed_obj([
            ("name", Ty::Str),
            ("merged", Ty::Bool),
            ("current", Ty::Bool),
        ]))),
        "git.info" => closed_obj([("trunk", Ty::Str), ("remote", Ty::Str)]),
        "git.worktree_for" => closed_obj([
            ("path", nullable(Ty::Str)),
            ("clean", nullable(Ty::Bool)),
            ("changed", Ty::List(Box::new(Ty::Str))),
        ]),
        "git.branch_exists" => closed_obj([("exists", Ty::Bool)]),
        "git.cleanup_plan" => closed_obj([
            (
                "worktrees",
                Ty::List(Box::new(closed_obj([
                    ("path", Ty::Str),
                    ("branch", nullable(Ty::Str)),
                    ("action", Ty::Str),
                    ("reason", Ty::Str),
                ]))),
            ),
            (
                "branches",
                Ty::List(Box::new(closed_obj([
                    ("name", Ty::Str),
                    ("action", Ty::Str),
                    ("reason", Ty::Str),
                ]))),
            ),
        ]),
        "git.fetch" => closed_obj([("fetched", Ty::Bool)]),
        "git.merge_ff" => closed_obj([("trunk", Ty::Str), ("commit", Ty::Str)]),
        "git.push" => closed_obj([("branch", Ty::Str), ("commit", Ty::Str)]),
        "git.delete_branch" => closed_obj([("deleted", Ty::Str)]),
        "git.remove_worktree" => closed_obj([("removed", Ty::Str)]),
        "git.worktree_add" => closed_obj([
            ("path", Ty::Str),
            ("branch", Ty::Str),
            ("commit", Ty::Str),
        ]),
        "git.commit" => closed_obj([
            ("commit", Ty::Str),
            ("files", Ty::List(Box::new(Ty::Str))),
        ]),
        "gh.pr_for" | "gh.pr_create" => closed_obj([("url", nullable(Ty::Str))]),
        "gh.pr_open" => closed_obj([("url", Ty::Str), ("existing", Ty::Bool)]),
        _ => Ty::Any,
    }
}

type ToolMetaLookup<'a> = &'a dyn Fn(&str, &str) -> Option<crate::reagent::ToolMeta>;

struct V<'a> {
    def: &'a Ritual,
    resolve: Resolver<'a>,
    tools: ToolMetaLookup<'a>,
    errors: Vec<String>,
    effects: Vec<Effect>,
    ids: Vec<BTreeMap<String, Ty>>,
    vars: Vec<(String, Ty)>,
    max_depth: u32,
    depth: u32,
    stack: Vec<String>,
}

impl V<'_> {
    fn did_you_mean<'b>(needle: &str, candidates: impl Iterator<Item = &'b str>) -> Option<&'b str> {
        let a: Vec<char> = needle.chars().collect();
        let max_dist = if a.len() <= 3 { 1 } else { 2 };
        let mut best: Option<(&'b str, usize)> = None;
        for cand in candidates {
            let b: Vec<char> = cand.chars().collect();
            if a.len().abs_diff(b.len()) > max_dist {
                continue;
            }
            let mut prev: Vec<usize> = (0..=b.len()).collect();
            let mut curr = vec![0; b.len() + 1];
            for (i, ca) in a.iter().enumerate() {
                curr[0] = i + 1;
                for (j, cb) in b.iter().enumerate() {
                    let cost = usize::from(ca != cb);
                    curr[j + 1] = (prev[j + 1] + 1)
                        .min(curr[j] + 1)
                        .min(prev[j] + cost);
                }
                std::mem::swap(&mut prev, &mut curr);
            }
            let d = prev[b.len()];
            if d > 0 && d <= max_dist && best.is_none_or(|(_, bd)| d < bd) {
                best = Some((cand, d));
            }
        }
        best.map(|(s, _)| s)
    }

    fn descend_type(&mut self, mut ty: Ty, tail: &[String], full: &[String], at: &str) -> Ty {
        let shown = full.join(".");
        let mut is_null = false;
        for seg in tail {
            if let Ty::Nullable(inner) = ty {
                is_null = true;
                ty = *inner;
            }
            ty = match ty {
                Ty::Any => Ty::Any,
                Ty::Obj { ref fields, closed } => match fields.get(seg) {
                    Some(f) => f.clone(),
                    None if closed => {
                        let known: Vec<&str> = fields.keys().map(String::as_str).collect();
                        let hint = Self::did_you_mean(seg, known.iter().copied())
                            .map(|s| format!("; did you mean `{s}`?"))
                            .unwrap_or_default();
                        self.errors.push(format!(
                            "{at}: `{shown}` has no field `{seg}` (known fields: {}){hint}",
                            if known.is_empty() {
                                "none".into()
                            } else {
                                known.join(", ")
                            }
                        ));
                        return Ty::Any;
                    }
                    None => Ty::Any,
                },
                Ty::List(elem) => {
                    if seg.parse::<usize>().is_err() {
                        self.errors.push(format!(
                            "{at}: `{shown}`: `{seg}` is not a list index on a list value"
                        ));
                        return Ty::Any;
                    }
                    *elem
                }
                other => {
                    self.errors.push(format!(
                        "{at}: `{shown}`: cannot look inside {} value at `{seg}`",
                        other.label()
                    ));
                    return Ty::Any;
                }
            };
        }
        if is_null {
            nullable(ty)
        } else {
            ty
        }
    }

    fn resolve_ref(&mut self, path: &[String], at: &str, allow_output: Option<&Ty>) -> Ty {
        let shown = path.join(".");
        match path[0].as_str() {
            "inputs" => match path.get(1) {
                Some(n) => match self.def.inputs.get(n) {
                    Some(decl) => {
                        let base = Ty::from_decl(decl);
                        self.descend_type(base, &path[2..], path, at)
                    }
                    None => {
                        let hint = Self::did_you_mean(n, self.def.inputs.keys().map(String::as_str))
                            .map(|s| format!("; did you mean `{s}`?"))
                            .unwrap_or_default();
                        self.errors.push(format!(
                            "{at}: `{shown}` names undeclared input `{n}`{hint}"
                        ));
                        Ty::Any
                    }
                },
                None => {
                    self.errors
                        .push(format!("{at}: `{shown}` must name an input"));
                    Ty::Any
                }
            },
            "steps" => match path.get(1) {
                Some(n) => {
                    let step_ty = self.ids.iter().rev().find_map(|f| f.get(n)).cloned();
                    match step_ty {
                        Some(out_ty) => match path.get(2).map(String::as_str) {
                            Some("output") => self.descend_type(out_ty, &path[3..], path, at),
                            Some("status" | "kind") => {
                                self.descend_type(Ty::Str, &path[3..], path, at)
                            }
                            Some("counts") => {
                                let counts_ty = closed_obj([
                                    ("returned", Ty::Int),
                                    ("abstained", Ty::Int),
                                    ("failed", Ty::Int),
                                    ("remaining", Ty::Int),
                                ]);
                                self.descend_type(counts_ty, &path[3..], path, at)
                            }
                            Some(other) => {
                                self.errors.push(format!(
                                    "{at}: `{shown}`: step `{n}` has no property `{other}`; use `steps.{n}.output` or `steps.{n}.status`"
                                ));
                                Ty::Any
                            }
                            None => {
                                self.errors.push(format!(
                                    "{at}: `{shown}` must read `steps.{n}.output` or `steps.{n}.status`"
                                ));
                                Ty::Any
                            }
                        },
                        None => {
                            let hint = Self::did_you_mean(
                                n,
                                self.ids.iter().flat_map(|f| f.keys().map(String::as_str)),
                            )
                            .map(|s| format!("; did you mean `{s}`?"))
                            .unwrap_or_default();
                            self.errors.push(format!(
                                "{at}: `{shown}` names step `{n}`, which is not defined earlier in scope{hint}"
                            ));
                            Ty::Any
                        }
                    }
                }
                None => {
                    self.errors
                        .push(format!("{at}: `{shown}` must name a step"));
                    Ty::Any
                }
            },
            "output" if allow_output.is_some() => {
                let base = allow_output.unwrap().clone();
                self.descend_type(base, &path[1..], path, at)
            }
            v => match self.vars.iter().rev().find(|(x, _)| x == v).cloned() {
                Some((_, var_ty)) => self.descend_type(var_ty, &path[1..], path, at),
                None => {
                    self.errors.push(format!(
                        "{at}: `{shown}` is ambiguous; references start with `inputs.`, `steps.`, or a loop item, not `{v}`"
                    ));
                    Ty::Any
                }
            },
        }
    }

    fn value_type(&mut self, v: &Value, at: &str) -> Ty {
        match v {
            Value::Null => Ty::Null,
            Value::Bool(_) => Ty::Bool,
            Value::Number(n) => {
                if n.is_i64() || n.is_u64() {
                    Ty::Int
                } else {
                    Ty::Num
                }
            }
            Value::String(s) => match template_refs(s) {
                Ok(refs) if refs.is_empty() => Ty::Str,
                Ok(refs) => {
                    let t = s.trim();
                    if refs.len() == 1 && t.starts_with("${") && t.ends_with('}') {
                        self.resolve_ref(&refs[0], at, None)
                    } else {
                        for r in &refs {
                            self.resolve_ref(r, at, None);
                        }
                        Ty::Str
                    }
                }
                Err(e) => {
                    self.errors.push(format!("{at}: {e}"));
                    Ty::Any
                }
            },
            Value::Array(a) => {
                let mut elem: Option<Ty> = None;
                for x in a {
                    let xt = self.value_type(x, at);
                    elem = Some(match elem {
                        None => xt,
                        Some(prev) if prev == xt => prev,
                        Some(_) => Ty::Any,
                    });
                }
                Ty::List(Box::new(elem.unwrap_or(Ty::Any)))
            }
            Value::Object(m) => {
                let mut fields = BTreeMap::new();
                for (k, x) in m {
                    fields.insert(k.clone(), self.value_type(x, at));
                }
                Ty::Obj {
                    fields,
                    closed: true,
                }
            }
        }
    }

    fn check_values(&mut self, v: &Value, at: &str) -> Ty {
        self.value_type(v, at)
    }

    fn expr_type(&mut self, e: &Expr, at: &str, allow_output: Option<&Ty>) -> Ty {
        match e {
            Expr::Lit(Value::Null) => Ty::Null,
            Expr::Lit(Value::Bool(_)) => Ty::Bool,
            Expr::Lit(Value::Number(n)) => {
                if n.is_i64() || n.is_u64() {
                    Ty::Int
                } else {
                    Ty::Num
                }
            }
            Expr::Lit(Value::String(_)) => Ty::Str,
            Expr::Lit(_) => Ty::Any,
            Expr::Ref(p) => self.resolve_ref(p, at, allow_output),
            Expr::Not(a) => {
                let t = self.expr_type(a, at, allow_output);
                if !t.compatible_with(&Ty::Bool) {
                    self.errors.push(format!(
                        "{at}: operand of `not` has type {}, expected boolean",
                        t.label()
                    ));
                }
                Ty::Bool
            }
            Expr::And(a, b) | Expr::Or(a, b) => {
                let op_name = if matches!(e, Expr::And(_, _)) {
                    "and"
                } else {
                    "or"
                };
                for sub in [a, b] {
                    let t = self.expr_type(sub, at, allow_output);
                    if !t.compatible_with(&Ty::Bool) {
                        self.errors.push(format!(
                            "{at}: operand of `{op_name}` has type {}, expected boolean",
                            t.label()
                        ));
                    }
                }
                Ty::Bool
            }
            Expr::Eq(a, b) | Expr::Ne(a, b) => {
                let lt = self.expr_type(a, at, allow_output);
                let rt = self.expr_type(b, at, allow_output);
                if !lt.compatible_with(&rt) && !rt.compatible_with(&lt) {
                    self.errors.push(format!(
                        "{at}: cannot compare incompatible types {} and {}",
                        lt.label(),
                        rt.label()
                    ));
                }
                Ty::Bool
            }
            Expr::Order(a, b, _) => {
                let lt = self.expr_type(a, at, allow_output);
                let rt = self.expr_type(b, at, allow_output);
                fn unwrap_null(t: &Ty) -> &Ty {
                    match t {
                        Ty::Nullable(inner) => inner.as_ref(),
                        other => other,
                    }
                }
                let ok = matches!(
                    (unwrap_null(&lt), unwrap_null(&rt)),
                    (Ty::Any, _)
                        | (_, Ty::Any)
                        | (Ty::Int, Ty::Int)
                        | (Ty::Num, Ty::Num)
                        | (Ty::Str, Ty::Str)
                );
                if !ok {
                    self.errors.push(format!(
                        "{at}: ordering comparison requires two integers, two decimals, or two strings (found {} and {})",
                        lt.label(),
                        rt.label()
                    ));
                }
                Ty::Bool
            }
        }
    }

    fn check_cond(&mut self, src: &str, at: &str, allow_output: Option<&Ty>) {
        match parse_expr(src) {
            Ok(e) => {
                let t = self.expr_type(&e, at, allow_output);
                if !t.compatible_with(&Ty::Bool) {
                    self.errors.push(format!(
                        "{at}: condition has type {}, expected boolean",
                        t.label()
                    ));
                }
            }
            Err(e) => self.errors.push(format!("{at}: {e}")),
        }
    }

    fn steps(&mut self, steps: &[Step], prefix: &str) {
        for (i, s) in steps.iter().enumerate() {
            let label = s.id.clone().unwrap_or_else(|| format!("#{}", i + 1));
            let at = if prefix.is_empty() {
                label.clone()
            } else {
                format!("{prefix}/{label}")
            };
            let kind = match s.kind() {
                Ok(k) => k,
                Err(e) => {
                    self.errors.push(format!("step {at}: {e}"));
                    continue;
                }
            };
            if let Some(id) = &s.id {
                if self.ids.iter().any(|f| f.contains_key(id)) {
                    self.errors
                        .push(format!("step {at}: id `{id}` is already used in scope"));
                }
            } else if kind != Kind::If {
                self.errors.push(format!(
                    "step {at}: needs an `id` so its output can be referenced"
                ));
            }
            if let Some(r) = s.retry {
                if r == 0 || r > 5 {
                    self.errors.push(format!(
                        "step {at}: `retry` must be between 1 and 5"
                    ));
                }
            }
            let mut out_ty = match kind {
                Kind::Operation => self.operation(s, &at),
                Kind::Model => self.model(s, &at),
                Kind::Ritual => self.ritual(s, &at),
                Kind::Each => self.each(s, &at),
                Kind::If => {
                    self.check_cond(
                        s.cond.as_deref().unwrap_or(""),
                        &format!("step {at}"),
                        None,
                    );
                    if s.then.is_none() {
                        self.errors.push(format!("step {at}: `if` needs `then`"));
                    }
                    let existing: BTreeSet<String> =
                        self.ids.last().unwrap().keys().cloned().collect();
                    for branch in [&s.then, &s.otherwise].into_iter().flatten() {
                        // A branch's ids are visible afterwards; a branch not
                        // taken leaves them skipped with a null output.
                        self.steps(branch, &at);
                    }
                    let frame = self.ids.last_mut().unwrap();
                    for (k, v) in frame.iter_mut() {
                        if !existing.contains(k) {
                            *v = nullable(v.clone());
                        }
                    }
                    Ty::Null
                }
            };
            if let Some(fb) = &s.fallback {
                if fb.is_empty() {
                    self.errors.push(format!(
                        "step {at}: `fallback` must contain at least one step"
                    ));
                } else {
                    self.ids.push(BTreeMap::new());
                    self.steps(fb, &format!("{at}/fallback"));
                    let fb_ids = self.ids.pop().unwrap();
                    let fb_ty = fb
                        .last()
                        .and_then(|last| last.id.as_ref())
                        .and_then(|id| fb_ids.get(id))
                        .cloned()
                        .unwrap_or(Ty::Any);
                    if fb_ty != out_ty {
                        out_ty = Ty::Any;
                    }
                }
            }
            if let Some(id) = &s.id {
                self.ids.last_mut().unwrap().insert(id.clone(), out_ty);
            }
        }
    }

    fn operation(&mut self, s: &Step, at: &str) -> Ty {
        let op = s.operation.as_deref().unwrap_or("");
        let Some((reagent, tool)) = op.split_once('.') else {
            self.errors.push(format!(
                "step {at}: operation `{op}` must be written reagent.tool"
            ));
            return Ty::Any;
        };
        let Some(decl) = self.def.reagents.get(reagent) else {
            self.errors.push(format!(
                "step {at}: reagent `{reagent}` is not bound under `reagents`"
            ));
            return Ty::Any;
        };
        if !decl.select.iter().any(|t| t == tool) {
            self.errors.push(format!(
                "step {at}: tool `{tool}` is not selected from reagent `{reagent}`"
            ));
        }
        let Some(spec) = (self.tools)(reagent, tool) else {
            self.errors.push(format!(
                "step {at}: reagent `{reagent}` has no tool `{tool}`"
            ));
            return Ty::Any;
        };
        let effect = spec.effect;
        let determinism = spec.determinism;
        if effect != "pure"
            && !self
                .def
                .requires
                .iter()
                .any(|r| r.reagent == reagent && r.action == tool)
        {
            self.errors.push(format!(
                "step {at}: `{op}` is used but no entry under `requires` declares {reagent}.{tool}"
            ));
        }
        let mut arg_types = BTreeMap::new();
        let schema_opt = crate::reagent::input_schema(op);
        if let Some(args) = &s.arguments {
            for (key, v) in args {
                if key == "arguments" && matches!(op, "data.map" | "data.fold") {
                    continue;
                }
                let actual = self.check_values(v, &format!("step {at}"));
                if let Some(sub) = schema_opt
                    .as_ref()
                    .and_then(|sc| sc["properties"].get(key))
                {
                    let expected = Ty::from_schema(sub);
                    if !actual.compatible_with(&expected) {
                        self.errors.push(format!(
                            "step {at}: argument `{key}` for `{op}` has type {}, expected {}",
                            actual.label(),
                            expected.label()
                        ));
                    }
                }
                if op == "data.count" && key == "value" {
                    let unwrap = match &actual {
                        Ty::Nullable(inner) => inner.as_ref(),
                        other => other,
                    };
                    if matches!(unwrap, Ty::Int | Ty::Num | Ty::Bool) {
                        self.errors.push(format!(
                            "step {at}: argument `value` for `data.count` has type {}, expected list, object, or string",
                            actual.label()
                        ));
                    }
                }
                arg_types.insert(key.clone(), actual);
            }
        }
        let args = serde_json::to_value(s.arguments.clone().unwrap_or_default()).unwrap();
        if schema_opt.is_some() {
            if let Err(e) = crate::reagent::validate_arguments(op, &args, false) {
                self.errors.push(format!("step {at}: {e}"));
            }
        }
        let elem_ty = match arg_types.get("values") {
            Some(Ty::List(e)) => (**e).clone(),
            _ => Ty::Any,
        };
        if op == "data.filter" {
            if let Some(pred) = args.get("predicate").and_then(Value::as_str) {
                self.vars.push(("item".into(), elem_ty.clone()));
                self.check_cond(pred, &format!("step {at} predicate"), None);
                self.vars.pop();
            }
        }
        let mut notes = Vec::new();
        if matches!(op, "data.map" | "data.fold") {
            if let Some(inner) = args
                .get("operation")
                .and_then(Value::as_str)
                .and_then(|s| s.strip_prefix("data."))
            {
                if !decl.select.iter().any(|t| t == inner) {
                    self.errors.push(format!(
                        "step {at}: collection primitive data.{inner} must also be selected"
                    ));
                }
                if let Some(inner_args) = args.get("arguments").and_then(Value::as_object) {
                    let inner_op = format!("data.{inner}");
                    let inner_schema = crate::reagent::input_schema(&inner_op);
                    self.vars.push(("item".into(), elem_ty));
                    if op == "data.fold" {
                        let init_ty = arg_types.get("initial").cloned().unwrap_or(Ty::Any);
                        self.vars.push(("acc".into(), init_ty));
                    }
                    for (ik, iv) in inner_args {
                        let actual = self.check_values(iv, &format!("step {at} {inner_op}"));
                        if let Some(sub) = inner_schema
                            .as_ref()
                            .and_then(|sc| sc["properties"].get(ik))
                        {
                            let expected = Ty::from_schema(sub);
                            if !actual.compatible_with(&expected) {
                                self.errors.push(format!(
                                    "step {at}: argument `{ik}` for `{inner_op}` has type {}, expected {}",
                                    actual.label(),
                                    expected.label()
                                ));
                            }
                        }
                    }
                    if op == "data.fold" {
                        self.vars.pop();
                    }
                    self.vars.pop();
                }
                notes.push(format!("pure collection body: data.{inner}; each item consumes item and tool-call essence"));
            }
        }
        if let Some(v) = &s.verify {
            if crate::reagent::input_schema(&v.observe).is_some() {
                if let Err(e) = crate::reagent::validate_arguments(
                    &v.observe,
                    &serde_json::to_value(&v.arguments).unwrap(),
                    false,
                ) {
                    self.errors.push(format!("step {at} verify: {e}"));
                }
            }
            let obs_schema = crate::reagent::input_schema(&v.observe);
            let mut obs_arg_types = BTreeMap::new();
            for (k, val) in &v.arguments {
                let actual = self.check_values(val, &format!("step {at} verify"));
                if let Some(sub) = obs_schema
                    .as_ref()
                    .and_then(|sc| sc["properties"].get(k))
                {
                    let expected = Ty::from_schema(sub);
                    if !actual.compatible_with(&expected) {
                        self.errors.push(format!(
                            "step {at} verify: argument `{k}` for `{}` has type {}, expected {}",
                            v.observe,
                            actual.label(),
                            expected.label()
                        ));
                    }
                }
                obs_arg_types.insert(k.clone(), actual);
            }
            let obs_out = operation_output_type(&v.observe, Some(&v.arguments), &obs_arg_types);
            for (ek, eval) in &v.expect {
                let actual = self.check_values(eval, &format!("step {at} verify"));
                if let Ty::Obj { ref fields, closed } = obs_out {
                    match fields.get(ek) {
                        Some(field_ty) if !actual.compatible_with(field_ty) => {
                            self.errors.push(format!(
                                "step {at} verify: expected field `{ek}` has type {}, but `{}` produces {}",
                                actual.label(),
                                v.observe,
                                field_ty.label()
                            ));
                        }
                        None if closed => {
                            self.errors.push(format!(
                                "step {at} verify: `{}` output has no field `{ek}`",
                                v.observe
                            ));
                        }
                        _ => {}
                    }
                }
            }
            match v.observe.split_once('.') {
                Some((vr, vt)) => {
                    let selected = self
                        .def
                        .reagents
                        .get(vr)
                        .is_some_and(|d| d.select.iter().any(|t| t == vt));
                    match (self.tools)(vr, vt) {
                        Some(spec) if spec.effect != "read" => self.errors.push(format!(
                            "step {at}: verify must observe with a read tool; `{}` is {}", v.observe, spec.effect
                        )),
                        Some(_) if !selected => self.errors.push(format!(
                            "step {at}: verify tool `{}` is not selected under `reagents`", v.observe
                        )),
                        Some(_) if !self.def.requires.iter().any(|r| r.reagent == vr && r.action == vt) => self.errors.push(format!(
                            "step {at}: verify uses `{}` but no entry under `requires` declares it", v.observe
                        )),
                        Some(_) => notes.push(format!("observed afterward by {}", v.observe)),
                        None => self.errors.push(format!("step {at}: verify tool `{}` does not exist", v.observe)),
                    }
                }
                None => self.errors.push(format!(
                    "step {at}: verify `observe` must be written reagent.tool"
                )),
            }
            if v.expect.is_empty() {
                self.errors.push(format!(
                    "step {at}: verify needs at least one expected value"
                ));
            }
        } else if !matches!(effect, "read" | "pure") {
            notes.push("not observed afterward; the tool's own report is the only evidence".into());
        }
        if let Some(h) = &s.hold {
            if matches!(effect, "read" | "pure") {
                self.errors.push(format!(
                    "step {at}: `hold` applies to writes; `{op}` only reads"
                ));
            }
            match h {
                Value::Bool(true) => notes.push("held as a petition until approved".into()),
                Value::Bool(false) => {}
                Value::String(_) => {
                    let ht = self.check_values(h, &format!("step {at} hold"));
                    if !ht.compatible_with(&Ty::Bool) {
                        self.errors.push(format!(
                            "step {at} hold: reference has type {}, expected boolean",
                            ht.label()
                        ));
                    }
                    notes.push("held as a petition when its `hold` reference is true".into());
                }
                _ => self.errors.push(format!(
                    "step {at}: `hold` must be true, false, or a reference to a boolean"
                )),
            }
        }
        self.effects.push(Effect {
            step: at.into(),
            kind: "operation".into(),
            what: op.into(),
            effect: effect.into(),
            determinism: determinism.into(),
            notes,
        });
        operation_output_type(op, s.arguments.as_ref(), &arg_types)
    }

    fn model(&mut self, s: &Step, at: &str) -> Ty {
        let m = s.model.as_ref().unwrap();
        let out_ty = match &m.schema {
            Some(schema) => {
                if let Err(e) = crate::schema::validate(schema) {
                    self.errors.push(format!("step {at} schema: {e}"));
                    Ty::Any
                } else {
                    Ty::from_schema(schema)
                }
            }
            None => Ty::Str,
        };
        let mut settings = self.def.settings.clone();
        settings.extend(m.settings.clone());
        if let Err(e) = crate::backend::validate_settings(&settings) {
            self.errors.push(format!("step {at} settings: {e}"));
        }
        let name = m.name.clone().or_else(|| self.def.model.clone());
        if name.is_none() {
            self.errors.push(format!(
                "step {at}: no model; set `model` on the ritual or `name` on the step"
            ));
        }
        for b in &m.bind {
            if !self.def.bindings.contains_key(b) {
                self.errors.push(format!(
                    "step {at}: binding `{b}` is not declared under `bindings`"
                ));
            }
        }
        self.check_values(
            &Value::String(m.prompt.clone()),
            &format!("step {at} prompt"),
        );
        if let Some(a) = &m.abstain_if {
            self.check_cond(a, &format!("step {at} abstain_if"), Some(&out_ty));
        }
        self.effects.push(Effect {
            step: at.into(),
            kind: "model".into(),
            what: name.unwrap_or_else(|| "?".into()),
            effect: "read".into(),
            determinism: "probabilistic".into(),
            notes: vec![if m.schema.is_some() {
                "output is a model's claim; its structure is checked".into()
            } else {
                "output is a model's claim; no structure declared".into()
            }],
        });
        out_ty
    }

    fn ritual(&mut self, s: &Step, at: &str) -> Ty {
        let name = s.ritual.as_deref().unwrap_or("");
        if self.depth + 1 > self.max_depth {
            self.errors.push(format!(
                "step {at}: calling `{name}` exceeds the nesting ceiling of {}",
                self.max_depth
            ));
            return Ty::Any;
        }
        if self.stack.iter().any(|n| n == name) {
            self.errors.push(format!(
                "step {at}: cycle {} -> {name}",
                self.stack.join(" -> ")
            ));
            return Ty::Any;
        }
        let Some((ver, _, child)) = (self.resolve)(name, s.version) else {
            self.errors
                .push(format!("step {at}: ritual `{name}` is not in the grimoire"));
            return Ty::Any;
        };
        // Explicit inputs: every argument must be declared by the child, and
        // every required child input must be supplied.
        let args = s.arguments.clone().unwrap_or_default();
        for (k, v) in &args {
            let actual = self.check_values(v, &format!("step {at}"));
            match child.inputs.get(k) {
                Some(decl) => {
                    let expected = Ty::from_decl(decl);
                    // String literals can be coerced by resolve_inputs (e.g. "20" -> integer),
                    // whereas typed step/input references must be type-compatible.
                    let is_coercible_literal = v.as_str().is_some_and(|st| !st.contains("${"));
                    if !is_coercible_literal && !actual.compatible_with(&expected) {
                        self.errors.push(format!(
                            "step {at}: argument `{k}` for `{name}` has type {}, expected {}",
                            actual.label(),
                            expected.label()
                        ));
                    }
                }
                None => self
                    .errors
                    .push(format!("step {at}: `{name}` has no input `{k}`")),
            }
        }
        for (k, d) in &child.inputs {
            if d.default.is_none() && !args.contains_key(k) {
                self.errors
                    .push(format!("step {at}: `{name}` requires input `{k}`"));
            }
        }
        // Narrowed authority: the child may not need more than the parent declares.
        let have: Vec<Grant> = self
            .def
            .requires
            .iter()
            .flat_map(Requirement::atoms)
            .collect();
        for need in child.requires.iter().flat_map(Requirement::atoms) {
            if !have.iter().any(|h| h.covers(&need)) {
                self.errors.push(format!(
                    "step {at}: `{name}` requires {}, which this ritual does not declare",
                    need.describe()
                ));
            }
        }
        // Transitive contracts.
        let mut stack = self.stack.clone();
        stack.push(name.to_string());
        match validate_inner(
            &child,
            self.resolve,
            self.tools,
            self.max_depth,
            self.depth + 1,
            stack,
        ) {
            Ok((effects, ret_ty)) => {
                for e in effects {
                    self.effects.push(Effect {
                        step: format!("{at} > {name} v{ver} / {}", e.step),
                        ..e
                    });
                }
                ret_ty
            }
            Err(errs) => {
                for e in errs {
                    self.errors.push(format!("step {at}: in `{name}`: {e}"));
                }
                Ty::Any
            }
        }
    }

    fn each(&mut self, s: &Step, at: &str) -> Ty {
        let spec = s.each.as_deref().unwrap_or("");
        let Some((lhs, rhs)) = spec.split_once(" in ") else {
            self.errors.push(format!(
                "step {at}: `each` must read `item in steps.name.output` or `item in [...]`"
            ));
            return Ty::Any;
        };
        let var = lhs.trim().to_string();
        let source = rhs.trim().to_string();
        if var.is_empty() || var.contains(char::is_whitespace) {
            self.errors.push(format!(
                "step {at}: `each` variable `{var}` must be a single identifier"
            ));
            return Ty::Any;
        }
        if matches!(var.as_str(), "inputs" | "steps" | "output") {
            self.errors
                .push(format!("step {at}: loop item cannot be named `{var}`"));
        }
        let src_ty = if source.starts_with('[') {
            match serde_json::from_str::<Value>(&source) {
                Ok(v @ Value::Array(_)) => self.value_type(&v, &format!("step {at}")),
                Ok(_) => {
                    self.errors
                        .push(format!("step {at}: `each` literal must be a JSON array"));
                    Ty::Any
                }
                Err(e) => {
                    self.errors
                        .push(format!("step {at}: invalid `each` array literal: {e}"));
                    Ty::Any
                }
            }
        } else {
            if source.contains(char::is_whitespace) {
                self.errors.push(format!(
                    "step {at}: `each` must read `item in steps.name.output` or `item in [...]`"
                ));
                return Ty::Any;
            }
            let path: Vec<String> = source.split('.').map(String::from).collect();
            self.resolve_ref(&path, &format!("step {at}"), None)
        };
        let item_ty = match src_ty {
            Ty::List(elem) => *elem,
            Ty::Nullable(inner) => match *inner {
                Ty::List(elem) => *elem,
                Ty::Any => Ty::Any,
                other => {
                    self.errors.push(format!(
                        "step {at}: `each` source `{source}` has type {}, expected a list",
                        other.label()
                    ));
                    Ty::Any
                }
            },
            Ty::Any => Ty::Any,
            other => {
                self.errors.push(format!(
                    "step {at}: `each` source `{source}` has type {}, expected a list",
                    other.label()
                ));
                Ty::Any
            }
        };
        if s.parallel == Some(0) {
            self.errors.push(format!(
                "step {at}: `parallel` must be at least 1"
            ));
        }
        if let Some(f) = &s.on_failure {
            if f != "stop" && f != "continue" {
                self.errors.push(format!(
                    "step {at}: on_failure must be `stop` or `continue`"
                ));
            }
        }
        let Some(body) = &s.steps else {
            self.errors.push(format!("step {at}: `each` needs `steps`"));
            return Ty::Any;
        };
        self.ids.push(BTreeMap::new());
        self.vars.push((var, item_ty));
        self.steps(body, at);
        self.vars.pop();
        let body_outputs = self.ids.pop().unwrap_or_default();
        let steps_status = body_outputs
            .keys()
            .map(|k| (k.clone(), Ty::Str))
            .collect();
        Ty::List(Box::new(closed_obj([
            ("item", Ty::Str),
            ("status", Ty::Str),
            ("error", nullable(Ty::Str)),
            (
                "outputs",
                Ty::Obj {
                    fields: body_outputs,
                    closed: true,
                },
            ),
            (
                "steps",
                Ty::Obj {
                    fields: steps_status,
                    closed: true,
                },
            ),
            ("effects", Ty::List(Box::new(Ty::Any))),
        ])))
    }
}

fn validate_inner(
    def: &Ritual,
    resolve: Resolver,
    tools: ToolMetaLookup<'_>,
    max_depth: u32,
    depth: u32,
    stack: Vec<String>,
) -> Result<(Vec<Effect>, Ty), Vec<String>> {
    let mut v = V {
        def,
        resolve,
        tools,
        errors: Vec::new(),
        effects: Vec::new(),
        ids: vec![BTreeMap::new()],
        vars: Vec::new(),
        max_depth,
        depth,
        stack,
    };
    if def.language_version != 1 {
        v.errors.push(format!(
            "unsupported language_version {}",
            def.language_version
        ));
    }
    let expected_out = match &def.output_schema {
        Some(schema) => match crate::schema::validate(schema) {
            Ok(()) => Some(Ty::from_schema(schema)),
            Err(e) => {
                v.errors.push(format!("output_schema: {e}"));
                None
            }
        },
        None => None,
    };
    if def.ritual.is_empty()
        || !def
            .ritual
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
    {
        v.errors.push(format!(
            "ritual name `{}` must use letters, digits, `-` or `_`",
            def.ritual
        ));
    }
    for (name, d) in &def.inputs {
        if !matches!(
            d.ty.as_str(),
            "integer" | "decimal" | "boolean" | "string" | "list" | "array" | "object"
        ) {
            v.errors.push(format!(
                "inputs.{name}.type: input `{name}`: unsupported type `{}`",
                d.ty
            ));
        }
        if d.user_only && d.default.is_none() {
            v.errors.push(format!(
                "inputs.{name}.user_only: input `{name}`: `user_only` needs a default for agents to run with"
            ));
        }
        if let Some(schema) = &d.schema {
            if let Err(e) = crate::schema::validate(schema) {
                v.errors.push(format!("inputs.{name}.schema: {e}"));
            }
        }
    }
    if let Some(t) = &def.essence.time {
        if let Err(e) = crate::util::parse_duration(t) {
            v.errors.push(format!("essence time: {e}"));
        }
    }
    if let Some(p) = &def.drift.on_mismatch {
        if !matches!(p.as_str(), "note" | "warn" | "refuse") {
            v.errors.push(format!(
                "drift on_mismatch must be note, warn or refuse, not `{p}`"
            ));
        }
    }
    if def.steps.is_empty() {
        v.errors.push("a ritual needs at least one step".into());
    }
    v.steps(&def.steps, "");
    let mut ret_fields = BTreeMap::new();
    for (k, val) in &def.ret {
        let t = v.check_values(val, &format!("return `{k}`"));
        ret_fields.insert(k.clone(), t);
    }
    let inferred_ret = Ty::Obj {
        fields: ret_fields.clone(),
        closed: true,
    };
    if let (Some(schema), Some(Ty::Obj { fields: exp_fields, closed: exp_closed })) =
        (&def.output_schema, &expected_out)
    {
        if let Some(req) = schema.get("required").and_then(Value::as_array) {
            for r in req.iter().filter_map(Value::as_str) {
                if !ret_fields.contains_key(r) {
                    v.errors.push(format!(
                        "return: missing required output_schema field `{r}`"
                    ));
                }
            }
        }
        for (k, actual_ty) in &ret_fields {
            match exp_fields.get(k) {
                Some(exp_ty) if !actual_ty.compatible_with(exp_ty) => {
                    v.errors.push(format!(
                        "return `{k}`: has type {}, expected {} from output_schema",
                        actual_ty.label(),
                        exp_ty.label()
                    ));
                }
                None if *exp_closed && !exp_fields.is_empty() => {
                    v.errors.push(format!(
                        "return `{k}`: field is not allowed by output_schema"
                    ));
                }
                _ => {}
            }
        }
    }
    let final_ret = expected_out.unwrap_or(inferred_ret);
    if v.errors.is_empty() {
        Ok((v.effects, final_ret))
    } else {
        Err(v.errors)
    }
}

/// Validate the complete definition before anything runs, and return every
/// declared effect, including those of rituals it calls.
pub fn validate(
    def: &Ritual,
    resolve: Resolver,
    tools: &ToolLookup,
    max_depth: u32,
) -> Result<Vec<Effect>, Vec<String>> {
    validate_inner(
        def,
        resolve,
        &|r, t| tools(r, t).map(crate::reagent::ToolMeta::from),
        max_depth,
        0,
        vec![def.ritual.clone()],
    )
    .map(|(e, _)| e)
}

pub fn validate_with_cfg(
    def: &Ritual,
    resolve: Resolver,
    cfg: &crate::util::Config,
    max_depth: u32,
) -> Result<Vec<Effect>, Vec<String>> {
    validate_inner(
        def,
        resolve,
        &|r, t| crate::reagent::tool_meta(Some(cfg), r, t),
        max_depth,
        0,
        vec![def.ritual.clone()],
    )
    .map(|(e, _)| e)
}

pub type ValidatedWithTypes = (Vec<Effect>, BTreeMap<String, String>);

/// Validate the complete definition and also return the statically inferred
/// type labels for each field under `return:`.
pub fn validate_with_return_types_cfg(
    def: &Ritual,
    resolve: Resolver,
    cfg: &crate::util::Config,
    max_depth: u32,
) -> Result<ValidatedWithTypes, Vec<String>> {
    validate_with_return_types_meta(
        def,
        resolve,
        &|r, t| crate::reagent::tool_meta(Some(cfg), r, t),
        max_depth,
    )
}

fn validate_with_return_types_meta(
    def: &Ritual,
    resolve: Resolver,
    tools: ToolMetaLookup<'_>,
    max_depth: u32,
) -> Result<ValidatedWithTypes, Vec<String>> {
    let (effects, ret_ty) =
        validate_inner(def, resolve, tools, max_depth, 0, vec![def.ritual.clone()])?;
    let mut types = BTreeMap::new();
    if let Ty::Obj { fields, .. } = ret_ty {
        for (k, t) in fields {
            types.insert(k, t.label());
        }
    }
    Ok((effects, types))
}

/// Every model a run of `def` could call: each model step's pinned name, or
/// else the invocation override, or else its defining ritual's model, through
/// `each` and `if` bodies and pinned child rituals. Drift fingerprints cover
/// all of them, so a change anywhere in the tree is noticed.
pub fn model_sites(def: &Ritual, resolve: Resolver, invocation: Option<&str>) -> BTreeSet<String> {
    fn walk(
        steps: &[Step],
        def: &Ritual,
        resolve: Resolver,
        invocation: Option<&str>,
        depth: u32,
        out: &mut BTreeSet<String>,
    ) {
        for s in steps {
            if let Some(m) = &s.model {
                let name = m.name.clone().or_else(|| invocation.map(String::from)).or_else(|| def.model.clone());
                out.extend(name);
            }
            if let Some(name) = &s.ritual {
                // Validation bounds depth; this guard only keeps a malformed tree finite.
                if depth < 64 {
                    if let Some((_, _, child)) = resolve(name, s.version) {
                        walk(&child.steps, &child, resolve, invocation, depth + 1, out);
                    }
                }
            }
            for body in [&s.steps, &s.then, &s.otherwise].into_iter().flatten() {
                walk(body, def, resolve, invocation, depth, out);
            }
        }
    }
    let mut out = BTreeSet::new();
    walk(&def.steps, def, resolve, invocation, 0, &mut out);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    struct E(Value);
    impl Env for E {
        fn lookup(&self, path: &[String]) -> Result<Value, String> {
            descend(self.0.clone(), path, path)
        }
    }

    fn obj_decl(schema: Option<Value>) -> InputDecl {
        let mut d = json!({"type": "object"});
        if let Some(s) = schema {
            d["schema"] = s;
        }
        serde_json::from_value(d).unwrap()
    }

    #[test]
    fn object_input_accepts_object() {
        let v = coerce("delivery", &obj_decl(None), &json!({"accept": true})).unwrap();
        assert_eq!(v, json!({"accept": true}));
    }

    #[test]
    fn object_input_parses_cli_string() {
        let v = coerce("delivery", &obj_decl(None), &json!("{\"n\": 2}")).unwrap();
        assert_eq!(v, json!({"n": 2}));
    }

    #[test]
    fn object_input_refuses_list_and_scalar() {
        for raw in [json!([1]), json!(3), json!("[1]"), json!(true)] {
            assert!(coerce("delivery", &obj_decl(None), &raw).is_err(), "{raw}");
        }
    }

    #[test]
    fn object_input_checks_schema() {
        let d = obj_decl(Some(json!({"type": "object", "required": ["accept"]})));
        let e = coerce("delivery", &d, &json!({})).unwrap_err();
        assert!(e.contains("delivery"), "{e}");
        assert!(coerce("delivery", &d, &json!({"accept": false})).is_ok());
    }

    #[test]
    fn object_input_validates_in_a_ritual() {
        let def = parse_file("ritual: r\ninputs:\n  delivery: {type: object}\nsteps: []\n").unwrap();
        assert!(def.inputs.contains_key("delivery"));
        assert_eq!(Ty::from_decl(&def.inputs["delivery"]), Ty::Any);
    }

    fn env() -> E {
        E(
            json!({"inputs": {"apply": true, "count": 3}, "steps": {"c": {"output": {"folder": "Receipts"}}, "skipped": {"output": null}}, "message": {"id": "m-1"}}),
        )
    }

    #[test]
    fn json_is_a_strict_frontend_and_python_is_never_a_definition() {
        let yaml = "ritual: sample\nsteps: []\n";
        let a = parse_definition(Path::new("sample.yaml"), yaml).unwrap();
        let b = parse_definition(
            Path::new("sample.json"),
            r#"{"language_version":1,"ritual":"sample","steps":[]}"#,
        )
        .unwrap();
        assert_eq!(canonical_json(&a), canonical_json(&b));
        assert_eq!(content_hash(&a), content_hash(&b));
        assert!(parse_definition(Path::new("sample.json"), yaml).is_err());
        assert!(parse_definition(
            Path::new("sample.json"),
            r#"{"ritual":"a","ritual":"b","steps":[]}"#
        )
        .is_err());
        assert!(
            parse_definition(Path::new("sample.py"), "print('must not run')")
                .unwrap_err()
                .contains("Python builders")
        );
    }

    #[test]
    fn conditions() {
        let e = env();
        assert!(
            eval_condition("inputs.apply and steps.c.output.folder != \"abstain\"", &e).unwrap()
        );
        assert!(
            !eval_condition("not inputs.apply or steps.c.output.folder == 'Action'", &e).unwrap()
        );
        assert!(eval_condition("inputs.count == 3", &e).unwrap());
        assert!(eval_condition("steps.skipped.output.folder == null", &e).unwrap());
        assert!(
            eval_condition("inputs.count", &e).is_err(),
            "a number is not a condition"
        );
        assert!(eval_condition("inputs.missing == 1", &e).is_err());
    }

    #[test]
    fn templates_keep_types() {
        let e = env();
        assert_eq!(render_template("${inputs.count}", &e).unwrap(), json!(3));
        assert_eq!(
            render_template("id ${message.id} x${inputs.count}", &e).unwrap(),
            json!("id m-1 x3")
        );
        assert_eq!(render_template("plain", &e).unwrap(), json!("plain"));
        assert!(render_template("${inputs.count + 1}", &e).is_err());
    }

    #[test]
    fn schema_subset() {
        let s = json!({"type": "object", "required": ["folder"], "properties": {"folder": {"type": "string", "enum": ["A", "B"]}}});
        assert!(check_schema(&s, &json!({"folder": "A"}), "").is_ok());
        assert!(check_schema(&s, &json!({"folder": "C"}), "").is_err());
        assert!(check_schema(&s, &json!({}), "").is_err());
        assert!(check_schema(&s, &json!("A"), "").is_err());
    }

    #[test]
    fn grants_cover_narrower_needs_only() {
        let g = |on: &[(&str, &str)], into: &[(&str, &str)]| Grant {
            reagent: "mail".into(),
            action: "move".into(),
            on: on
                .iter()
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .collect(),
            into: into
                .iter()
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .collect(),
        };
        let need = g(&[("folder", "Inbox")], &[("folder", "Receipts")]);
        assert!(g(&[("folder", "Inbox")], &[("folder", "Receipts")]).covers(&need));
        assert!(g(&[], &[]).covers(&need), "an unscoped grant is broad");
        assert!(!g(&[("folder", "Inbox")], &[("folder", "Action")]).covers(&need));
        assert!(!g(&[("folder", "Archive")], &[]).covers(&need));
    }

    #[test]
    fn ritual_defs_reuse_across_model_and_output_schema() {
        let yaml = r##"
ritual: review
defs:
  verdict:
    type: string
    enum: [supported, contradicted, insufficient]
  assessment:
    type: object
    additionalProperties: false
    required: [verdict, quote]
    properties:
      verdict: {"$ref": "#/$defs/verdict"}
      quote: string
inputs:
  claim: string
model: mock/keyword
steps:
  - id: judge
    model:
      prompt: "${inputs.claim}"
      schema: {"$ref": "#/$defs/assessment"}
output_schema:
  type: object
  required: [review]
  properties:
    review: {"$ref": "#/$defs/assessment"}
return:
  review: "${steps.judge.output}"
"##;
        let mut def = parse_file(yaml).unwrap();
        prepare(&mut def, std::path::Path::new("."), &|_, _| None).unwrap();
        assert!(validate(&def, &|_, _| None, &no_tools, 3).is_ok());
        let model_schema = def.steps[0].model.as_ref().unwrap().schema.as_ref().unwrap();
        assert_eq!(model_schema["properties"]["verdict"]["enum"], json!(["supported", "contradicted", "insufficient"]));
        let out_schema = def.output_schema.as_ref().unwrap();
        assert_eq!(out_schema["properties"]["review"]["properties"]["verdict"]["type"], "string");
    }

    const CHILD: &str = r#"
ritual: child
inputs: {text: {type: string}}
model: mock/keyword
steps:
  - id: c
    model: {prompt: "${inputs.text}"}
return: {out: "${steps.c.output}"}
"#;

    fn no_tools(_: &str, _: &str) -> Option<&'static crate::reagent::ToolSpec> {
        None
    }

    #[test]
    fn validation_catches_bad_references_and_cycles() {
        let child = parse_file(CHILD).unwrap();
        let resolve =
            |n: &str, _: Option<u32>| (n == "child").then(|| (1, "h".to_string(), child.clone()));
        assert!(validate(&child, &resolve, &no_tools, 3).is_ok());

        let bad = parse_file(&CHILD.replace("inputs.text", "inputs.nope")).unwrap();
        let errs = validate(&bad, &resolve, &no_tools, 3).unwrap_err();
        assert!(errs[0].contains("undeclared input"), "{errs:?}");

        let parent = parse_file(
            "ritual: parent\nsteps:\n  - id: a\n    ritual: child\n    arguments: {text: hi}\n",
        )
        .unwrap();
        let effects = validate(&parent, &resolve, &no_tools, 3).unwrap();
        assert_eq!(effects.len(), 1);
        assert_eq!(
            effects[0].determinism, "probabilistic",
            "contracts are transitive"
        );

        let missing = parse_file("ritual: parent\nsteps:\n  - id: a\n    ritual: child\n").unwrap();
        assert!(
            validate(&missing, &resolve, &no_tools, 3).unwrap_err()[0].contains("requires input")
        );

        let looped = parse_file(
            "ritual: child\nsteps:\n  - id: a\n    ritual: child\n    arguments: {text: x}\n",
        )
        .unwrap();
        let resolve_loop = |_: &str, _: Option<u32>| Some((1, "h".to_string(), looped.clone()));
        assert!(validate(&looped, &resolve_loop, &no_tools, 3).unwrap_err()[0].contains("cycle"));
    }

    #[test]
    fn inputs_are_typed_and_never_guessed() {
        let def = parse_file(
            "ritual: r\ninputs:\n  count: {type: integer, default: 10, max: 50}\n  apply: {type: boolean, default: false}\n  text: {type: string}\nsteps: []\n",
        )
        .unwrap();
        let m = |v: Value| v.as_object().unwrap().clone();
        let ok = resolve_inputs(&def, &m(json!({"count": "20", "text": "hi"}))).unwrap();
        assert_eq!(ok, json!({"apply": false, "count": 20, "text": "hi"}));
        assert!(
            resolve_inputs(&def, &m(json!({"count": "20"}))).is_err(),
            "required input missing"
        );
        assert!(
            resolve_inputs(&def, &m(json!({"count": "99", "text": "x"}))).is_err(),
            "above max"
        );
        assert!(
            resolve_inputs(&def, &m(json!({"text": "x", "extra": "1"}))).is_err(),
            "unknown input"
        );
        assert!(
            resolve_inputs(&def, &m(json!({"text": "x", "apply": "yes"}))).is_err(),
            "not a boolean"
        );

        let guarded = parse_file("ritual: r\ninputs:\n  hold: {type: boolean, default: true, user_only: true}\n  n: {type: integer, default: 1}\nsteps: []\n").unwrap();
        assert!(user_only_changes(&guarded, &m(json!({"n": 5}))).is_empty());
        assert!(
            user_only_changes(&guarded, &m(json!({"hold": true}))).is_empty(),
            "restating the default is harmless"
        );
        assert_eq!(
            user_only_changes(&guarded, &m(json!({"hold": "false"}))),
            vec!["hold"]
        );
    }
    #[test]
    fn modifiers_and_versions_fail_before_execution() {
        let s: Step =
            serde_json::from_value(json!({"id":"m","model":{"prompt":"x"},"hold":true})).unwrap();
        assert!(s.kind().is_err());
        let def=parse_file("language_version: 2\nritual: r\nmodel: mock/keyword\nsteps:\n  - id: x\n    model: {prompt: x}\n").unwrap();
        assert!(validate(&def, &|_, _| None, &no_tools, 3).is_err());
    }
    #[test]
    fn authority_intersection_only_narrows() {
        let broad: Grant =
            serde_json::from_value(json!({"reagent":"mail","action":"move","on":{},"into":{}}))
                .unwrap();
        let needs:Vec<Requirement>=serde_json::from_value(json!([{"reagent":"mail","action":"move","on":{"folder":"Inbox"},"into":{"folder":"Receipts"}}])).unwrap();
        let result = narrow_grants(std::slice::from_ref(&broad), &needs);
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].into["folder"], "Receipts");
        let other:Grant=serde_json::from_value(json!({"reagent":"mail","action":"move","on":{"folder":"Inbox"},"into":{"folder":"Trash"}})).unwrap();
        assert!(intersect_grants(&result, &[other]).is_empty());
        assert!(!result[0].covers(&broad));
    }
}

#[cfg(test)]
mod model_site_tests {
    use super::*;

    fn def(v: Value) -> Ritual {
        serde_json::from_value(v).unwrap()
    }

    #[test]
    fn model_sites_cover_steps_children_and_override() {
        let child = def(json!({"ritual":"child","model":"mock/c","steps":[{"id":"x","model":{"prompt":"p"}}]}));
        let parent = def(json!({"ritual":"parent","model":"mock/a","steps":[
            {"id":"one","model":{"prompt":"p"}},
            {"id":"two","model":{"name":"mock/p","prompt":"p"}},
            {"id":"kid","ritual":"child"}
        ]}));
        let resolve = |n: &str, _v: Option<u32>| (n == "child").then(|| (1, "h".to_string(), child.clone()));
        let sites = model_sites(&parent, &resolve, None);
        assert_eq!(sites.into_iter().collect::<Vec<_>>(), ["mock/a", "mock/c", "mock/p"]);
        let sites = model_sites(&parent, &resolve, Some("mock/o"));
        assert_eq!(sites.into_iter().collect::<Vec<_>>(), ["mock/o", "mock/p"]);
    }

    #[test]
    fn model_sites_look_inside_each_and_if() {
        let r = def(json!({"ritual":"r","model":"mock/a","inputs":{"n":{"type":"integer"}},"steps":[
            {"id":"loop","each":"${inputs.n}","steps":[{"id":"in","model":{"name":"mock/e","prompt":"p"}}]},
            {"if":"true","then":[{"id":"t","model":{"name":"mock/t","prompt":"p"}}],"else":[{"id":"f","model":{"name":"mock/f","prompt":"p"}}]}
        ]}));
        let none = |_: &str, _: Option<u32>| None;
        let sites = model_sites(&r, &none, None);
        assert_eq!(sites.into_iter().collect::<Vec<_>>(), ["mock/e", "mock/f", "mock/t"]);
    }
}

#[cfg(test)]
mod type_checking_tests {
    use super::*;

    #[test]
    fn static_type_checker_catches_field_typos_and_scalar_indexing() {
        let none = |_: &str, _: Option<u32>| None;
        // 1. Typo on a model step's declared schema field
        let typo_model = parse_file(
            r#"
ritual: r
model: mock/keyword
steps:
  - id: classify
    model:
      prompt: "hi"
      schema:
        type: object
        required: [folder]
        properties:
          folder: {type: string}
return:
  out: "${steps.classify.output.foldr}"
"#,
        )
        .unwrap();
        let errs = validate(&typo_model, &none, &crate::reagent::tool, 3).unwrap_err();
        assert!(
            errs.iter().any(|e| e.contains("has no field `foldr`")),
            "{errs:?}"
        );

        // 2. Indexing into an unstructured model output (which is a string)
        let scalar_model = parse_file(
            r#"
ritual: r
model: mock/keyword
steps:
  - id: say
    model:
      prompt: "hi"
return:
  out: "${steps.say.output.field}"
"#,
        )
        .unwrap();
        let errs = validate(&scalar_model, &none, &crate::reagent::tool, 3).unwrap_err();
        assert!(
            errs.iter().any(|e| e.contains("cannot look inside string value")),
            "{errs:?}"
        );

        // 3. Typo on a loop item from mail.search
        let bad_loop_item = parse_file(
            r#"
ritual: r
reagents:
  mail: {select: [search, read]}
requires:
  - {reagent: mail, action: search}
  - {reagent: mail, action: read}
steps:
  - id: select
    operation: mail.search
    arguments: {folder: Inbox}
  - id: loop
    each: msg in steps.select.output
    steps:
      - id: read
        operation: mail.read
        arguments: {message: "${msg.nonexistent}"}
"#,
        )
        .unwrap();
        let errs = validate(&bad_loop_item, &none, &crate::reagent::tool, 3).unwrap_err();
        assert!(
            errs.iter().any(|e| e.contains("has no field `nonexistent`")),
            "{errs:?}"
        );
    }

    #[test]
    fn static_type_checker_catches_argument_condition_and_return_type_mismatches() {
        let none = |_: &str, _: Option<u32>| None;
        // 1. Passing a string input to an integer tool argument, and an integer to `each` and `hold`
        let bad_args = parse_file(
            r#"
ritual: r
inputs:
  label: {type: string}
  num: {type: integer}
reagents:
  mail: {select: [search, move]}
requires:
  - {reagent: mail, action: search}
  - {reagent: mail, action: move}
steps:
  - id: s
    operation: mail.search
    arguments: {folder: Inbox, newest: "${inputs.label}"}
  - id: mv
    operation: mail.move
    arguments: {message: "m1", folder: "Receipts"}
    hold: "${inputs.num}"
  - id: iter
    each: x in inputs.num
    steps:
      - id: s2
        operation: mail.search
        arguments: {folder: Inbox}
"#,
        )
        .unwrap();
        let errs = validate(&bad_args, &none, &crate::reagent::tool, 3).unwrap_err();
        assert!(
            errs.iter()
                .any(|e| e.contains("argument `newest` for `mail.search` has type string, expected integer")),
            "{errs:?}"
        );
        assert!(
            errs.iter()
                .any(|e| e.contains("hold: reference has type integer, expected boolean")),
            "{errs:?}"
        );
        assert!(
            errs.iter()
                .any(|e| e.contains("`each` source `inputs.num` has type integer, expected a list")),
            "{errs:?}"
        );

        // 2. Non-boolean condition and incompatible ordering comparison
        let bad_cond = parse_file(
            r#"
ritual: r
inputs:
  count: {type: integer}
  name: {type: string}
reagents:
  data: {select: [assert]}
steps:
  - if: inputs.count
    then:
      - id: a
        operation: data.assert
        arguments: {condition: true}
  - if: inputs.name > inputs.count
    then:
      - id: b
        operation: data.assert
        arguments: {condition: true}
"#,
        )
        .unwrap();
        let errs = validate(&bad_cond, &none, &crate::reagent::tool, 3).unwrap_err();
        assert!(
            errs.iter()
                .any(|e| e.contains("condition has type integer, expected boolean")),
            "{errs:?}"
        );
        assert!(
            errs.iter()
                .any(|e| e.contains("ordering comparison requires two integers")),
            "{errs:?}"
        );

        // 3. Return value violating output_schema
        let bad_return = parse_file(
            r#"
ritual: r
reagents:
  data: {select: [count]}
steps:
  - id: n
    operation: data.count
    arguments: {value: [1, 2]}
output_schema:
  type: object
  required: [total]
  additionalProperties: false
  properties:
    total: {type: string}
return:
  total: "${steps.n.output}"
"#,
        )
        .unwrap();
        let errs = validate(&bad_return, &none, &crate::reagent::tool, 3).unwrap_err();
        assert!(
            errs.iter()
                .any(|e| e.contains("return `total`: has type integer, expected string")),
            "{errs:?}"
        );
    }

    #[test]
    fn static_type_checker_propagates_child_ritual_return_types() {
        let child = parse_file(
            r#"
ritual: child-typed
inputs:
  text: {type: string}
model: mock/keyword
steps:
  - id: c
    model:
      prompt: "${inputs.text}"
      schema:
        type: object
        required: [folder]
        properties:
          folder: {type: string}
return:
  folder: "${steps.c.output.folder}"
"#,
        )
        .unwrap();
        let resolve = |n: &str, _: Option<u32>| {
            (n == "child-typed").then(|| (1, "h".to_string(), child.clone()))
        };
        let parent_bad = parse_file(
            r#"
ritual: parent
inputs:
  count: {type: integer}
steps:
  - id: call_child
    ritual: child-typed
    arguments: {text: "${inputs.count}"}
return:
  result: "${steps.call_child.output.wrong_field}"
"#,
        )
        .unwrap();
        let errs = validate(&parent_bad, &resolve, &crate::reagent::tool, 3).unwrap_err();
        assert!(
            errs.iter().any(|e| e.contains("argument `text` for `child-typed` has type integer, expected string")),
            "{errs:?}"
        );
        assert!(
            errs.iter().any(|e| e.contains("has no field `wrong_field`")),
            "{errs:?}"
        );
    }

    #[test]
    fn array_input_and_inline_each_literal_validate_and_typecheck() {
        let def = parse_file(
            r#"
ritual: ergonomic-lists
inputs:
  nums:
    type: array
    schema: {type: array, items: {type: integer}}
reagents:
  data: {select: [arithmetic, sum_integers]}
steps:
  - id: summed
    operation: data.sum_integers
    arguments: {values: "${inputs.nums}"}
  - id: inline_loop
    each: x in [10, 20, 30]
    steps:
      - id: inc
        operation: data.arithmetic
        arguments: {left: "${x}", right: 1, operator: add}
return:
  total: "${steps.summed.output}"
"#,
        )
        .unwrap();
        let none = |_: &str, _: Option<u32>| None;
        validate(&def, &none, &crate::reagent::tool, 3).unwrap();
        let bound = resolve_inputs(
            &def,
            &serde_json::from_str(r#"{"nums":"[1,2,3]"}"#).unwrap(),
        )
        .unwrap();
        assert_eq!(bound["nums"], serde_json::json!([1, 2, 3]));
    }
}

#[cfg(test)]
mod ergonomics_tests {
    use super::*;

    #[test]
    fn compact_inputs_schemas_refs_assert_fail_and_auto_reagents_normalize_and_validate() {
        let def = parse_file(
            r#"
ritual: ergonomic-demo
inputs:
  text: string
  count: integer
  tags: {type: array, schema: list<string>}
requires:
  - {reagent: mail, action: search}
model: mock/keyword
steps:
  - assert: inputs.count > 0
    message: "count must be positive"
  - id: found
    operation: mail.search
    arguments: {folder: Inbox, newest: $inputs.count}
  - id: first
    each: msg in steps.found
    steps:
      - id: classify
        model:
          prompt: $inputs.text
          schema:
            folder: [Receipts, Newsletters, Action, abstain]
            note?: string
          abstain_if: output.folder == "abstain"
  - if: steps.found.status == "returned"
    then:
      - id: parsed
        operation: data.parse_json
        arguments:
          text: '{"items": [1, 2]}'
          schema:
            items: list<integer>
      - id: total
        operation: data.sum_integers
        arguments: {values: $steps.parsed.items}
    else:
      - fail: "search did not return"
output_schema:
  count: integer
  price_tag: string
return:
  count: $inputs.count
  price_tag: "$100"
"#,
        )
        .unwrap();

        assert_eq!(def.inputs["text"].ty, "string");
        assert_eq!(
            def.inputs["tags"].schema,
            Some(json!({"type": "array", "items": {"type": "string"}}))
        );
        assert_eq!(
            def.reagents["mail"].select,
            vec!["search".to_string()]
        );
        assert!(def.reagents["data"].select.contains(&"assert".to_string()));
        assert!(def.reagents["data"].select.contains(&"parse_json".to_string()));
        assert!(def.reagents["data"].select.contains(&"sum_integers".to_string()));
        assert_eq!(def.ret["count"], json!("${inputs.count}"));
        assert_eq!(def.ret["price_tag"], json!("$100"));
        assert_eq!(
            def.output_schema,
            Some(json!({
                "type": "object",
                "additionalProperties": false,
                "required": ["count", "price_tag"],
                "properties": {
                    "count": {"type": "integer"},
                    "price_tag": {"type": "string"}
                }
            }))
        );

        let none = |_: &str, _: Option<u32>| None;
        validate(&def, &none, &crate::reagent::tool, 3).unwrap();
    }

    #[test]
    fn explicit_reagents_still_enforces_narrowing() {
        let def = parse_file(
            r#"
ritual: explicit-narrow
reagents:
  data: {select: [sum_integers]}
steps:
  - id: p
    operation: data.parse_json
    arguments: {text: "{}"}
"#,
        )
        .unwrap();
        let none = |_: &str, _: Option<u32>| None;
        let errs = validate(&def, &none, &crate::reagent::tool, 3).unwrap_err();
        assert!(
            errs.iter().any(|e| e.contains("not selected from reagent `data`")),
            "{errs:?}"
        );
    }

    #[test]
    fn canonical_ritual_hashes_remain_identical_after_normalize() {
        let raw = r#"
ritual: canonical
inputs:
  text: {type: string}
reagents:
  data: {select: [sum_integers]}
steps:
  - id: s
    operation: data.sum_integers
    arguments: {values: [1, 2]}
output_schema:
  type: object
  required: [total]
  additionalProperties: false
  properties:
    total: {type: integer}
return:
  total: "${steps.s.output}"
"#;
        let parsed = parse_file(raw).unwrap();
        let direct: Ritual = serde_yaml::from_str(raw).unwrap();
        assert_eq!(content_hash(&parsed), content_hash(&direct));
    }

    #[test]
    fn did_you_mean_hints_on_input_step_and_field_typos() {
        let def = parse_file(
            r#"
ritual: hints
inputs:
  message_text: string
  payload:
    type: object
    schema:
      folder: string
steps:
  - id: parsed
    operation: data.parse_json
    arguments:
      text: $inputs.mesage_text
      schema:
        folder: string
return:
  step_typo: $steps.parsd.folder
  field_typo: $inputs.payload.foldr
"#,
        )
        .unwrap();
        let none = |_: &str, _: Option<u32>| None;
        let errs = validate(&def, &none, &crate::reagent::tool, 3).unwrap_err();
        assert!(
            errs.iter().any(|e| e.contains("did you mean `message_text`?")),
            "{errs:?}"
        );
        assert!(
            errs.iter().any(|e| e.contains("did you mean `parsed`?")),
            "{errs:?}"
        );
        assert!(
            errs.iter().any(|e| e.contains("did you mean `folder`?")),
            "{errs:?}"
        );
    }

    #[test]
    fn deeply_nested_schema_is_rejected() {
        let mut s = json!("string");
        for _ in 0..40 {
            s = json!({"a": s});
        }
        let err = expand_schema(s).unwrap_err();
        assert!(err.contains("exceeds"), "{err}");
    }

    #[test]
    fn nested_typed_input_schemas_parse_expand_and_validate() {
        let def = parse_file(
            r#"
ritual: nested-inputs
model: mock/keyword
inputs:
  user:
    name: string
    tags: [string]
  event:
    type: string
    payload: object
  secret_obj:
    user_only: true
    default:
      token: anon
    properties:
      token:
        type: string
  counts: array<integer>
  matrix: [[string]]
steps:
  - id: greet
    model:
      prompt: $inputs.user.name
return:
  name: $inputs.user.name
  tags: $inputs.user.tags
"#,
        )
        .unwrap();

        assert_eq!(def.inputs["user"].ty, "object");
        assert_eq!(
            def.inputs["user"].schema,
            Some(json!({
                "type": "object",
                "additionalProperties": false,
                "required": ["name", "tags"],
                "properties": {
                    "name": {"type": "string"},
                    "tags": {"type": "array", "items": {"type": "string"}}
                }
            }))
        );
        assert_eq!(def.inputs["counts"].ty, "array");
        assert_eq!(
            def.inputs["counts"].schema,
            Some(json!({"type": "array", "items": {"type": "integer"}}))
        );
        assert_eq!(def.inputs["matrix"].ty, "array");
        assert_eq!(
            def.inputs["matrix"].schema,
            Some(json!({"type": "array", "items": {"type": "array", "items": {"type": "string"}}}))
        );
        assert_eq!(def.inputs["event"].ty, "object");
        assert_eq!(
            def.inputs["event"].schema,
            Some(json!({
                "type": "object",
                "additionalProperties": false,
                "required": ["payload", "type"],
                "properties": {
                    "type": {"type": "string"},
                    "payload": {"type": "object"}
                }
            }))
        );
        assert_eq!(def.inputs["secret_obj"].ty, "object");
        assert!(def.inputs["secret_obj"].user_only);

        let none = |_: &str, _: Option<u32>| None;
        validate(&def, &none, &crate::reagent::tool, 3).unwrap();

        let bad = serde_json::from_value::<Map<String, Value>>(json!({
            "user": {"name": "ada", "tags": [123]},
            "counts": [1, 2],
            "matrix": [["a"]]
        }))
        .unwrap();
        assert!(resolve_inputs(&def, &bad).is_err());

        let good = serde_json::from_value::<Map<String, Value>>(json!({
            "user": {"name": "ada", "tags": ["rust"]},
            "event": {"type": "ping", "payload": {}},
            "counts": [1, 2],
            "matrix": [["a"]]
        }))
        .unwrap();
        assert!(resolve_inputs(&def, &good).is_ok());
    }

    #[test]
    fn yaml_and_validation_diagnostics_include_location_and_field_paths() {
        let yaml_err = parse_file("ritual: bad\nsteps:\n  - [unclosed\n").unwrap_err();
        assert!(
            yaml_err.contains("line ") && yaml_err.contains("column "),
            "{yaml_err}"
        );

        let schema_err = parse_file(
            "ritual: bad-schema\ninputs:\n  user:\n    name: unknown_type\nsteps:\n  - id: a\n    model: {prompt: hi}\n",
        )
        .unwrap_err();
        assert!(schema_err.contains("inputs.user.schema:"), "{schema_err}");

        let def = parse_file(
            "ritual: bad-input\ninputs:\n  flag: {type: boolean, user_only: true}\nsteps:\n  - id: a\n    model: {prompt: hi}\n",
        )
        .unwrap();
        let none = |_: &str, _: Option<u32>| None;
        let errs = validate(&def, &none, &crate::reagent::tool, 3).unwrap_err();
        assert!(
            errs.iter().any(|e| e.contains("inputs.flag.user_only:")),
            "{errs:?}"
        );
    }
}
