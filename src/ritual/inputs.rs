use super::*;

// --------------------------------------------------------------------- schema

/// The subset of JSON Schema that output contracts use: type, enum, required,
/// properties, items.
pub fn check_schema(schema: &Value, v: &Value, path: &str) -> Result<(), String> {
    crate::schema::check(schema, v, path)
}

// --------------------------------------------------------------------- inputs

pub(super) fn coerce(name: &str, decl: &InputDecl, raw: &Value) -> Result<Value, String> {
    let bad = |want: &str| format!("input `{name}` must be {want}, found {raw}");
    let v = match (decl.ty.as_str(), raw) {
        ("integer", Value::Number(n)) if n.is_i64() || n.is_u64() => raw.clone(),
        ("integer", Value::String(s)) => json!(s.parse::<i64>().map_err(|_| bad("an integer"))?),
        ("integer", _) => return Err(bad("an integer")),
        ("decimal", Value::Number(n)) => {
            let f = n.as_f64().ok_or_else(|| bad("a finite decimal"))?;
            if !f.is_finite() {
                return Err(bad("a finite decimal"));
            }
            json!(f)
        }
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

pub(super) fn is_compact_type_token(t: &str) -> bool {
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
