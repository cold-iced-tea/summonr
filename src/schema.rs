//! A deliberately specified JSON Schema subset, checked before execution.
//! Unsupported assertions are errors, never silently ignored.
use serde_json::{Map, Value};

const MAX_REF_DEPTH: usize = 32;

pub fn validate(s: &Value) -> Result<(), String> {
    let root_defs = match s.get("$defs") {
        Some(v) => Some(
            v.as_object()
                .ok_or_else(|| "$defs must be an object".to_string())?,
        ),
        None => None,
    };
    let mut visiting = Vec::new();
    validate_with_defs(s, root_defs, true, &mut visiting)
}

fn resolve_ref<'a>(r: &'a str, root_defs: Option<&'a Map<String, Value>>) -> Result<(&'a str, &'a Value), String> {
    let name = r
        .strip_prefix("#/$defs/")
        .filter(|n| !n.is_empty() && !n.contains('/'))
        .ok_or_else(|| format!("unsupported $ref `{r}`; only local `#/$defs/<name>` is supported"))?;
    let target = root_defs
        .and_then(|defs| defs.get(name))
        .ok_or_else(|| format!("unresolved $ref `{r}`"))?;
    Ok((name, target))
}

fn validate_with_defs(
    s: &Value,
    root_defs: Option<&Map<String, Value>>,
    is_root: bool,
    visiting: &mut Vec<String>,
) -> Result<(), String> {
    let o = s.as_object().ok_or("schema must be an object")?;
    for (k, v) in o {
        match k.as_str() {
            "$defs" => {
                if !is_root {
                    return Err("`$defs` is only supported at the schema root".into());
                }
                let defs = v.as_object().ok_or("`$defs` must be an object")?;
                for (name, def) in defs {
                    if name.trim().is_empty() || name.contains('/') {
                        return Err(format!("invalid `$defs` entry `{name}`"));
                    }
                    visiting.push(name.clone());
                    validate_with_defs(def, root_defs, false, visiting)?;
                    visiting.pop();
                }
            }
            "$ref" => {
                let r = v.as_str().ok_or("`$ref` must be a string")?;
                let (name, target) = resolve_ref(r, root_defs)?;
                if visiting.iter().any(|n| n == name) {
                    return Err(format!("cyclic $ref `{r}`"));
                }
                visiting.push(name.to_string());
                validate_with_defs(target, root_defs, false, visiting)?;
                visiting.pop();
            }
            "type" => {
                if !matches!(
                    v.as_str(),
                    Some("object" | "array" | "string" | "boolean" | "integer" | "number" | "null")
                ) {
                    return Err("schema type must be a supported single type".into());
                }
            }
            "enum" if v.as_array().is_some_and(|a| !a.is_empty()) => (),
            "properties" => {
                for child in v
                    .as_object()
                    .ok_or("properties must be an object")?
                    .values()
                {
                    validate_with_defs(child, root_defs, false, visiting)?;
                }
            }
            "items" => validate_with_defs(v, root_defs, false, visiting)?,
            "required" => {
                let a = v.as_array().ok_or("required must be an array")?;
                let mut seen = std::collections::HashSet::new();
                for x in a {
                    let name = x.as_str().ok_or("required entries must be strings")?;
                    if !seen.insert(name) {
                        return Err("duplicate required field".into());
                    }
                }
            }
            "additionalProperties" if v.is_boolean() => (),
            "minimum" | "maximum" if v.is_number() => (),
            "minLength" | "maxLength" | "minItems" | "maxItems" if v.as_u64().is_some() => (),
            "title" | "description" if v.is_string() => (),
            "default" => (),
            _ => return Err(format!("unsupported or malformed schema keyword `{k}`")),
        }
    }
    for (lo, hi) in [
        ("minimum", "maximum"),
        ("minLength", "maxLength"),
        ("minItems", "maxItems"),
    ] {
        if let (Some(a), Some(b)) = (s[lo].as_f64(), s[hi].as_f64()) {
            if a > b {
                return Err(format!("{lo} exceeds {hi}"));
            }
        }
    }
    Ok(())
}

/// Inline local `#/$defs/<name>` references so downstream consumers (such as
/// inference backends) receive self-contained schemas.
pub fn inline_refs(s: &Value) -> Result<Value, String> {
    validate(s)?;
    let root_defs = s.get("$defs").and_then(|v| v.as_object());
    inline_refs_with_defs(s, root_defs, true, 0)
}

fn inline_refs_with_defs(
    s: &Value,
    root_defs: Option<&Map<String, Value>>,
    is_root: bool,
    depth: usize,
) -> Result<Value, String> {
    if depth > MAX_REF_DEPTH {
        return Err(format!("$ref nesting exceeds {MAX_REF_DEPTH} levels"));
    }
    let o = s.as_object().ok_or("schema must be an object")?;
    if let Some(r) = o.get("$ref").and_then(|v| v.as_str()) {
        let (_, target) = resolve_ref(r, root_defs)?;
        let mut resolved = inline_refs_with_defs(target, root_defs, false, depth + 1)?;
        if let Some(resolved_obj) = resolved.as_object_mut() {
            for (k, v) in o {
                if k == "$ref" || (is_root && k == "$defs") {
                    continue;
                }
                let val = match k.as_str() {
                    "properties" => {
                        let mut out = Map::new();
                        for (pk, pv) in v.as_object().ok_or("properties must be an object")? {
                            out.insert(pk.clone(), inline_refs_with_defs(pv, root_defs, false, depth + 1)?);
                        }
                        Value::Object(out)
                    }
                    "items" => inline_refs_with_defs(v, root_defs, false, depth + 1)?,
                    _ => v.clone(),
                };
                resolved_obj.insert(k.clone(), val);
            }
        }
        return Ok(resolved);
    }
    let mut out = Map::new();
    for (k, v) in o {
        if is_root && k == "$defs" {
            continue;
        }
        let val = match k.as_str() {
            "properties" => {
                let mut props = Map::new();
                for (pk, pv) in v.as_object().ok_or("properties must be an object")? {
                    props.insert(pk.clone(), inline_refs_with_defs(pv, root_defs, false, depth + 1)?);
                }
                Value::Object(props)
            }
            "items" => inline_refs_with_defs(v, root_defs, false, depth + 1)?,
            _ => v.clone(),
        };
        out.insert(k.clone(), val);
    }
    Ok(Value::Object(out))
}

pub fn check(s: &Value, v: &Value, path: &str) -> Result<(), String> {
    validate(s)?;
    let root_defs = s.get("$defs").and_then(|d| d.as_object());
    check_with_defs(s, root_defs, v, path, 0)
}

fn check_with_defs(
    s: &Value,
    root_defs: Option<&Map<String, Value>>,
    v: &Value,
    path: &str,
    depth: usize,
) -> Result<(), String> {
    if depth > MAX_REF_DEPTH {
        return Err(format!("$ref nesting exceeds {MAX_REF_DEPTH} levels"));
    }
    let at = if path.is_empty() {
        "output".to_owned()
    } else {
        path.to_owned()
    };
    if let Some(r) = s.get("$ref").and_then(|x| x.as_str()) {
        let (_, target) = resolve_ref(r, root_defs)?;
        check_with_defs(target, root_defs, v, path, depth + 1)?;
    }
    if let Some(t) = s["type"].as_str() {
        let ok = match t {
            "object" => v.is_object(),
            "array" => v.is_array(),
            "string" => v.is_string(),
            "boolean" => v.is_boolean(),
            "integer" => v.is_i64() || v.is_u64(),
            "number" => v.is_number(),
            "null" => v.is_null(),
            _ => false,
        };
        if !ok {
            return Err(format!("{at}: expected {t}"));
        }
    }
    if let Some(a) = s["enum"].as_array() {
        if !a.contains(v) {
            return Err(format!("{at}: value outside enum"));
        }
    }
    if let Some(o) = v.as_object() {
        if let Some(a) = s["required"].as_array() {
            for name in a {
                if !o.contains_key(name.as_str().unwrap()) {
                    return Err(format!("{at}: missing {name}"));
                }
            }
        }
        for (name, value) in o {
            if let Some(sub) = s["properties"].get(name) {
                check_with_defs(sub, root_defs, value, &format!("{at}.{name}"), depth)?;
            } else if s["additionalProperties"] == false {
                return Err(format!("{at}: unexpected field {name}"));
            }
        }
    }
    if let Some(a) = v.as_array() {
        bounds(s, a.len() as f64, "minItems", "maxItems", &at)?;
        if let Some(sub) = s.get("items") {
            for (i, value) in a.iter().enumerate() {
                check_with_defs(sub, root_defs, value, &format!("{at}[{i}]"), depth)?;
            }
        }
    }
    if let Some(t) = v.as_str() {
        bounds(s, t.chars().count() as f64, "minLength", "maxLength", &at)?;
    }
    if let Some(n) = v.as_f64() {
        bounds(s, n, "minimum", "maximum", &at)?;
    }
    Ok(())
}

fn bounds(s: &Value, n: f64, lo: &str, hi: &str, at: &str) -> Result<(), String> {
    if s[lo].as_f64().is_some_and(|x| n < x) || s[hi].as_f64().is_some_and(|x| n > x) {
        Err(format!("{at}: outside {lo}/{hi} bounds"))
    } else {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    #[test]
    fn unsupported_assertions_never_pass_silently() {
        for s in [
            json!({"oneOf":[]}),
            json!({"type":["null","string"]}),
            json!({"required":[3]}),
            json!({"enum":[]}),
            json!({"properties":{"nested":{"pattern":"x"}}}),
        ] {
            assert!(validate(&s).is_err(), "{s}");
        }
    }
    #[test]
    fn checks_nested_closed_objects_and_bounds() {
        let s = json!({"type":"object","additionalProperties":false,"required":["n"],"properties":{"n":{"type":"integer","minimum":1,"maximum":3}}});
        assert!(check(&s, &json!({"n":2}), "").is_ok());
        for v in [
            json!({"n":0}),
            json!({"n":4}),
            json!({"n":2,"extra":true}),
            json!({}),
        ] {
            assert!(check(&s, &v, "").is_err());
        }
        assert!(check(&json!({"type":"string","maxLength":1}), &json!("é"), "").is_ok());
    }

    #[test]
    fn supports_local_defs_and_refs_and_rejects_cycles() {
        let s = json!({
            "$defs": {
                "verdict": {"type": "string", "enum": ["supported", "contradicted", "insufficient"]},
                "claim": {
                    "type": "object",
                    "additionalProperties": false,
                    "required": ["verdict", "quote"],
                    "properties": {
                        "verdict": {"$ref": "#/$defs/verdict"},
                        "quote": {"type": "string"}
                    }
                }
            },
            "type": "object",
            "required": ["review"],
            "properties": {
                "review": {"$ref": "#/$defs/claim"}
            }
        });
        assert!(check(&s, &json!({"review":{"verdict":"supported","quote":"ok"}}), "").is_ok());
        assert!(check(&s, &json!({"review":{"verdict":"bogus","quote":"ok"}}), "").is_err());

        let inlined = inline_refs(&s).unwrap();
        assert!(inlined.get("$defs").is_none());
        assert_eq!(inlined["properties"]["review"]["properties"]["verdict"]["type"], "string");
        assert!(check(&inlined, &json!({"review":{"verdict":"supported","quote":"ok"}}), "").is_ok());

        let cyclic = json!({
            "$defs": {
                "a": {"$ref": "#/$defs/b"},
                "b": {"$ref": "#/$defs/a"}
            },
            "$ref": "#/$defs/a"
        });
        assert!(validate(&cyclic).is_err());
        let missing = json!({"$defs": {}, "$ref": "#/$defs/nope"});
        assert!(validate(&missing).is_err());
    }
}
