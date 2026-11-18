//! Pure, deterministic transformations. No filesystem, network, clock, model, or ambient authority.
use serde_json::{json, Value};

pub fn schema(name: &str) -> Option<Value> {
    let (properties, required) = match name {
        "parse_json" => (
            json!({"text":{"type":"string","maxLength":1048576},"schema":{"type":"object"}}),
            vec!["text"],
        ),
        "project" => (
            json!({"value":{"type":"object"},"fields":{"type":"array","maxItems":10000,"items":{"type":"string"}}}),
            vec!["value", "fields"],
        ),
        "count" => (json!({"value":{}}), vec!["value"]),
        "sum_integers" => (
            json!({"values":{"type":"array","maxItems":10000,"items":{"type":"integer"}}}),
            vec!["values"],
        ),
        "assert" => (
            json!({"condition":{"type":"boolean"},"message":{"type":"string","maxLength":1024}}),
            vec!["condition"],
        ),
        _ => return crate::stdlib::schema(name),
    };
    Some(
        json!({"type":"object","properties":properties,"required":required,"additionalProperties":false}),
    )
}

pub fn call(name: &str, args: &Value) -> Result<Value, String> {
    let schema = schema(name).ok_or_else(|| format!("unknown data operation {name}"))?;
    crate::schema::check(&schema, args, "arguments")?;
    crate::stdlib::bounded(args.clone())?;
    match name {
        "parse_json" => {
            let result: Value = serde_json::from_str(args["text"].as_str().unwrap())
                .map_err(|e| format!("invalid JSON: {e}"))?;
            if let Some(schema) = args.get("schema") {
                crate::schema::check(schema, &result, "parsed")?;
            }
            Ok(result)
        }
        "project" => {
            let mut out = serde_json::Map::new();
            for field in args["fields"].as_array().unwrap() {
                let key = field.as_str().unwrap();
                if out.contains_key(key) {
                    return Err(format!("duplicate projected field {key}"));
                }
                let value = args["value"]
                    .get(key)
                    .ok_or_else(|| format!("missing projected field {key}"))?;
                out.insert(key.to_owned(), value.clone());
            }
            Ok(Value::Object(out))
        }
        "count" => {
            let n = match &args["value"] {
                Value::Array(a) => a.len(),
                Value::Object(o) => o.len(),
                Value::String(s) => s.chars().count(),
                _ => return Err("count needs an array, object, or string".into()),
            };
            Ok(json!(n))
        }
        "sum_integers" => {
            let mut total = 0i64;
            for value in args["values"].as_array().unwrap() {
                let n = value
                    .as_i64()
                    .ok_or("sum_integers accepts signed 64-bit integers")?;
                total = total.checked_add(n).ok_or("integer sum overflow")?;
            }
            Ok(json!(total))
        }
        "assert" if args["condition"] == true => Ok(json!({"passed":true})),
        "assert" => Err(args["message"]
            .as_str()
            .unwrap_or("deterministic assertion failed")
            .to_owned()),
        _ => crate::stdlib::call(name, args),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn computes_without_inference_and_rejects_ambiguous_values() {
        assert_eq!(
            call("sum_integers", &json!({"values":[100,25,-5]})).unwrap(),
            120
        );
        assert!(call("sum_integers", &json!({"values":[i64::MAX,1]})).is_err());
        assert!(call("sum_integers", &json!({"values":[1.5]})).is_err());
        assert_eq!(call("count", &json!({"value":"é🙂"})).unwrap(), 2);
        assert_eq!(
            call("project", &json!({"value":{"a":1,"b":2},"fields":["b"]})).unwrap(),
            json!({"b":2})
        );
        assert!(call("project", &json!({"value":{},"fields":["missing"]})).is_err());
        assert!(call("assert", &json!({"condition":false})).is_err());
    }
    #[test]
    fn parsing_is_not_guessing_or_repair() {
        assert_eq!(
            call(
                "parse_json",
                &json!({"text":"[1,2]","schema":{"type":"array","items":{"type":"integer"}}})
            )
            .unwrap(),
            json!([1, 2])
        );
        for text in ["not json", "```json\\n{}\\n```", "{'x':1}"] {
            assert!(call("parse_json", &json!({"text":text})).is_err());
        }
        assert!(call("parse_json", &json!({"text":"{}","schema":{"oneOf":[]}})).is_err());
    }
}
