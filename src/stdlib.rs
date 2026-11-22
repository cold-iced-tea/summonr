//! Bounded, deterministic standard data operations. No ambient authority.
use crate::{
    reagent::Fail,
    ritual::{self, Env},
};
use serde_json::{json, Value};

const MAX_BYTES: usize = 1_048_576;
pub fn schema(name: &str) -> Option<Value> {
    let text = json!({"type":"string","maxLength":1048576});
    let array = json!({"type":"array","maxItems":10000});
    let int = json!({"type":"integer"});
    let (p, r) = match name {
        "arithmetic" => (
            json!({"left":int,"right":int,"operator":{"enum":["add","subtract","multiply","divide","remainder"]}}),
            vec!["left", "right", "operator"],
        ),
        "compare" => (
            json!({"left":{},"right":{},"operator":{"enum":["eq","ne","lt","le","gt","ge"]}}),
            vec!["left", "right", "operator"],
        ),
        "get" => (
            json!({"value":{},"path":text,"default":{}}),
            vec!["value", "path"],
        ),
        "keys" => (json!({"value":{"type":"object"}}), vec!["value"]),
        "concat" => (json!({"left":array,"right":array}), vec!["left", "right"]),
        "slice" => (
            json!({"values":array,"start":{"type":"integer","minimum":0},"end":{"type":"integer","minimum":0}}),
            vec!["values", "start", "end"],
        ),
        "sort" | "unique" => (json!({"values":array}), vec!["values"]),
        "trim" => (json!({"text":text}), vec!["text"]),
        "split" => (
            json!({"text":text,"separator":{"type":"string","minLength":1,"maxLength":1048576}}),
            vec!["text", "separator"],
        ),
        "join" => (
            json!({"values":{"type":"array","maxItems":10000,"items":{"type":"string"}},"separator":text}),
            vec!["values", "separator"],
        ),
        "replace" => (
            json!({"text":text,"from":{"type":"string","minLength":1,"maxLength":1048576},"to":text}),
            vec!["text", "from", "to"],
        ),
        "map" | "fold" => {
            let mut p =
                json!({"values":array,"operation":{"type":"string"},"arguments":{"type":"object"}});
            let mut r = vec!["values", "operation", "arguments"];
            if name == "fold" {
                p["initial"] = json!({});
                r.push("initial");
            }
            (p, r)
        }
        "filter" => (
            json!({"values":array,"predicate":{"type":"string","maxLength":4096}}),
            vec!["values", "predicate"],
        ),
        "all" | "any" => (
            json!({"values":{"type":"array","maxItems":10000,"items":{"type":"boolean"}}}),
            vec!["values"],
        ),
        "logic" => (
            json!({"operator":{"enum":["and","or","xor","not"]},"left":{"type":"boolean"},"right":{"type":"boolean"}}),
            vec!["operator", "left"],
        ),
        "select" => (
            json!({"condition":{"type":"boolean"},"then":{},"else":{}}),
            vec!["condition", "then", "else"],
        ),
        "min" | "max" => (
            json!({"values":{"type":"array","minItems":1,"maxItems":10000}}),
            vec!["values"],
        ),
        "clamp" => (
            json!({"value":int,"min":int,"max":int}),
            vec!["value", "min", "max"],
        ),
        "abs" => (json!({"value":int}), vec!["value"]),
        "merge" => (
            json!({"left":{"type":"object"},"right":{"type":"object"},"conflict":{"enum":["right","left","error"]}}),
            vec!["left", "right"],
        ),
        "set" => (
            json!({"value":{"type":"object"},"field":{"type":"string","minLength":1,"maxLength":1024},"item":{}}),
            vec!["value", "field", "item"],
        ),
        "omit" => (
            json!({"value":{"type":"object"},"fields":{"type":"array","maxItems":10000,"items":{"type":"string"}}}),
            vec!["value", "fields"],
        ),
        "entries" => (json!({"value":{"type":"object"}}), vec!["value"]),
        "from_entries" => (json!({"entries":array}), vec!["entries"]),
        "sha256" => (json!({"text":text}), vec!["text"]),
        "canonical_json" => (json!({"value":{}}), vec!["value"]),
        "contains" => (json!({"value":{},"needle":{}}), vec!["value", "needle"]),
        "starts_with" => (json!({"text":text,"prefix":text}), vec!["text", "prefix"]),
        "ends_with" => (json!({"text":text,"suffix":text}), vec!["text", "suffix"]),
        "case" => (
            json!({"text":text,"mode":{"enum":["lower","upper"]}}),
            vec!["text", "mode"],
        ),
        "lines" => (
            json!({"text":text,"number_from":{"type":"integer"}}),
            vec!["text"],
        ),
        "coalesce" | "flatten" => (json!({"values":array}), vec!["values"]),
        "range" => (
            json!({"start":int,"end":int,"step":{"type":"integer","minimum":1}}),
            vec!["start", "end"],
        ),
        "index_of" => (
            json!({"values":array,"needle":{},"match":{"enum":["equal","contains"]}}),
            vec!["values", "needle"],
        ),
        _ => return None,
    };
    Some(json!({"type":"object","properties":p,"required":r,"additionalProperties":false}))
}
pub fn bounded(value: Value) -> Result<Value, String> {
    if serde_json::to_vec(&value).map_err(|e| e.to_string())?.len() > MAX_BYTES {
        return Err("data value exceeds 1 MiB encoded JSON".into());
    }
    if value.as_array().is_some_and(|a| a.len() > 10000) {
        return Err("array exceeds 10000 items".into());
    }
    Ok(value)
}
// serde_json equality treats floating negative zero as equal to positive zero.
fn equality_key(value: &Value) -> String {
    fn normalize(v: &mut Value) {
        match v {
            Value::Number(n) if n.is_f64() && n.as_f64() == Some(0.0) => *v = json!(0.0),
            Value::Array(a) => a.iter_mut().for_each(normalize),
            Value::Object(m) => m.values_mut().for_each(normalize),
            _ => {}
        }
    }
    let mut v = value.clone();
    normalize(&mut v);
    v.to_string()
}

pub fn call(name: &str, a: &Value) -> Result<Value, String> {
    let s = |k: &str| a[k].as_str().unwrap();
    let values = || a["values"].as_array().unwrap();
    let out = match name {
        "arithmetic" => {
            let l = a["left"]
                .as_i64()
                .ok_or("left must fit signed 64-bit integer")?;
            let r = a["right"]
                .as_i64()
                .ok_or("right must fit signed 64-bit integer")?;
            let result = match s("operator") {
                "add" => l.checked_add(r),
                "subtract" => l.checked_sub(r),
                "multiply" => l.checked_mul(r),
                "divide" => l.checked_div(r),
                "remainder" => l.checked_rem(r),
                _ => unreachable!(),
            };
            json!(result.ok_or("integer overflow or division by zero")?)
        }
        "compare" => {
            let (l, r) = (&a["left"], &a["right"]);
            json!(match s("operator") {
                "eq" => ritual::values_equal(l, r),
                "ne" => !ritual::values_equal(l, r),
                op => {
                    let c = ritual::compare_values(l, r)?;
                    match op {
                        "lt" => c.is_lt(),
                        "le" => !c.is_gt(),
                        "gt" => c.is_gt(),
                        "ge" => !c.is_lt(),
                        _ => unreachable!(),
                    }
                }
            })
        }
        "get" => {
            let path = s("path");
            if !path.is_empty() && !path.starts_with('/') {
                return Err("path must be an empty or slash-prefixed JSON pointer".into());
            }
            for part in path.split('/') {
                let mut chars = part.chars();
                while let Some(c) = chars.next() {
                    if c == '~' && !matches!(chars.next(), Some('0' | '1')) {
                        return Err("invalid JSON pointer escape".into());
                    }
                }
            }
            a["value"]
                .pointer(path)
                .or_else(|| a.get("default"))
                .cloned()
                .ok_or("JSON pointer is missing; supply an explicit default if intended")?
        }
        "keys" => json!(a["value"].as_object().unwrap().keys().collect::<Vec<_>>()),
        "concat" => {
            let mut v = a["left"].as_array().unwrap().clone();
            v.extend_from_slice(a["right"].as_array().unwrap());
            json!(v)
        }
        "slice" => {
            let v = values();
            let start = a["start"].as_u64().ok_or("invalid start")?;
            let end = a["end"].as_u64().ok_or("invalid end")?;
            if start > end || end > v.len() as u64 {
                return Err("slice bounds out of range".into());
            }
            json!(&v[start as usize..end as usize])
        }
        "sort" => {
            let mut v = values().clone();
            if let Some(first) = v.first() {
                for item in &v {
                    ritual::compare_values(first, item)?;
                }
            }
            v.sort_by(|a, b| ritual::compare_values(a, b).expect("validated homogeneous order"));
            json!(v)
        }
        "unique" => {
            let mut seen = std::collections::HashSet::new();
            let mut v = Vec::new();
            for x in values() {
                if seen.insert(equality_key(x)) {
                    v.push(x.clone());
                }
            }
            json!(v)
        }
        "trim" => json!(s("text").trim()),
        "split" => {
            let v: Vec<_> = s("text").split(s("separator")).take(10001).collect();
            json!(v)
        }
        "join" => {
            let v = values();
            let bytes = v
                .iter()
                .map(|x| x.as_str().unwrap().len())
                .sum::<usize>()
                .saturating_add(
                    s("separator")
                        .len()
                        .saturating_mul(v.len().saturating_sub(1)),
                );
            if bytes > MAX_BYTES {
                return Err("joined text exceeds 1 MiB".into());
            }
            json!(v
                .iter()
                .map(|x| x.as_str().unwrap())
                .collect::<Vec<_>>()
                .join(s("separator")))
        }
        "replace" => {
            let text = s("text");
            let count = text.matches(s("from")).count();
            let bytes = text
                .len()
                .saturating_sub(count * s("from").len())
                .saturating_add(count.saturating_mul(s("to").len()));
            if bytes > MAX_BYTES {
                return Err("replacement text exceeds 1 MiB".into());
            }
            json!(text.replace(s("from"), s("to")))
        }
        "all" => json!(values().iter().all(|v| v.as_bool() == Some(true))),
        "any" => json!(values().iter().any(|v| v.as_bool() == Some(true))),
        "logic" => {
            let l = a["left"].as_bool().ok_or("left must be boolean")?;
            let op = s("operator");
            if op == "not" {
                if a.get("right").is_some() {
                    return Err("`not` takes only `left`, not `right`".into());
                }
                json!(!l)
            } else {
                let r = a
                    .get("right")
                    .and_then(Value::as_bool)
                    .ok_or("binary logic operator requires boolean `right`")?;
                json!(match op {
                    "and" => l && r,
                    "or" => l || r,
                    "xor" => l ^ r,
                    _ => unreachable!(),
                })
            }
        }
        "select" => {
            if a["condition"].as_bool() == Some(true) {
                a["then"].clone()
            } else {
                a["else"].clone()
            }
        }
        "min" | "max" => {
            let v = values();
            let first = v.first().ok_or("values must not be empty")?;
            let mut best = first;
            for item in &v[1..] {
                let cmp = ritual::compare_values(item, best)?;
                if (name == "min" && cmp.is_lt()) || (name == "max" && cmp.is_gt()) {
                    best = item;
                }
            }
            best.clone()
        }
        "clamp" => {
            let val = a["value"].as_i64().ok_or("value must fit signed 64-bit integer")?;
            let min = a["min"].as_i64().ok_or("min must fit signed 64-bit integer")?;
            let max = a["max"].as_i64().ok_or("max must fit signed 64-bit integer")?;
            if min > max {
                return Err(format!("clamp min ({min}) exceeds max ({max})"));
            }
            json!(val.clamp(min, max))
        }
        "abs" => {
            let val = a["value"].as_i64().ok_or("value must fit signed 64-bit integer")?;
            json!(val.checked_abs().ok_or("integer overflow in abs")?)
        }
        "merge" => {
            let mut out = a["left"].as_object().unwrap().clone();
            let right = a["right"].as_object().unwrap();
            let policy = a.get("conflict").and_then(Value::as_str).unwrap_or("right");
            for (k, v) in right {
                match (out.contains_key(k), policy) {
                    (true, "error") => return Err(format!("merge conflict on key `{k}`")),
                    (true, "left") => {}
                    _ => {
                        out.insert(k.clone(), v.clone());
                    }
                }
            }
            Value::Object(out)
        }
        "set" => {
            let mut out = a["value"].as_object().unwrap().clone();
            out.insert(s("field").to_owned(), a["item"].clone());
            Value::Object(out)
        }
        "omit" => {
            let mut out = a["value"].as_object().unwrap().clone();
            for f in a["fields"].as_array().unwrap() {
                out.remove(f.as_str().unwrap());
            }
            Value::Object(out)
        }
        "entries" => {
            let obj = a["value"].as_object().unwrap();
            let mut keys: Vec<&String> = obj.keys().collect();
            keys.sort();
            json!(keys.into_iter().map(|k| json!({"key": k, "value": obj[k]})).collect::<Vec<_>>())
        }
        "from_entries" => {
            let mut out = serde_json::Map::new();
            for item in a["entries"].as_array().unwrap() {
                let obj = item.as_object().ok_or("each entry must be an object with `key` and `value`")?;
                let k = obj
                    .get("key")
                    .and_then(Value::as_str)
                    .ok_or("entry `key` must be a string")?;
                let v = obj.get("value").ok_or("entry is missing `value`")?;
                if out.contains_key(k) {
                    return Err(format!("duplicate entry key `{k}`"));
                }
                out.insert(k.to_owned(), v.clone());
            }
            Value::Object(out)
        }
        "sha256" => json!(crate::util::sha256_hex(s("text").as_bytes())),
        "canonical_json" => json!(crate::util::canonical_json(&a["value"])),
        "contains" => match (&a["value"], &a["needle"]) {
            (Value::String(hay), Value::String(needle)) => json!(hay.contains(needle.as_str())),
            (Value::Array(arr), needle) => {
                let target = equality_key(needle);
                json!(arr.iter().any(|x| equality_key(x) == target))
            }
            _ => return Err("contains requires a string (with string needle) or an array".into()),
        },
        "starts_with" => json!(s("text").starts_with(s("prefix"))),
        "ends_with" => json!(s("text").ends_with(s("suffix"))),
        "case" => match s("mode") {
            "lower" => json!(s("text").to_lowercase()),
            "upper" => json!(s("text").to_uppercase()),
            _ => unreachable!(),
        },
        "lines" => {
            let v: Vec<&str> = s("text").lines().take(10001).collect();
            let first = match a.get("number_from") {
                None => None,
                Some(n) => Some(n.as_i64().ok_or("number_from must fit signed 64-bit integer")?),
            };
            match first {
                None => json!(v),
                Some(first) => {
                    let mut out = Vec::with_capacity(v.len());
                    for (i, line) in v.iter().enumerate() {
                        let n = first
                            .checked_add(i as i64)
                            .ok_or("line number overflows signed 64-bit integer")?;
                        out.push(format!("{n}: {line}"));
                    }
                    json!(out)
                }
            }
        }
        "coalesce" => values().iter().find(|v| !v.is_null()).cloned().unwrap_or(Value::Null),
        "flatten" => {
            let mut out = Vec::new();
            for v in values() {
                let inner = v.as_array().ok_or("flatten needs every element to be an array")?;
                if out.len() + inner.len() > 10000 {
                    return Err("array exceeds 10000 items".into());
                }
                out.extend(inner.iter().cloned());
            }
            json!(out)
        }
        "range" => {
            let start = a["start"].as_i64().ok_or("start must fit signed 64-bit integer")?;
            let end = a["end"].as_i64().ok_or("end must fit signed 64-bit integer")?;
            let step = match a.get("step") {
                None => 1,
                Some(v) => v.as_i64().ok_or("step must fit signed 64-bit integer")?,
            };
            if step < 1 {
                return Err("step must be at least 1".into());
            }
            let mut out = Vec::new();
            let mut n = start;
            while n < end {
                if out.len() == 10000 {
                    return Err("range exceeds 10000 items".into());
                }
                out.push(n);
                n = match n.checked_add(step) {
                    Some(next) => next,
                    None => break,
                };
            }
            json!(out)
        }
        "index_of" => {
            let needle = &a["needle"];
            let contains = a.get("match").and_then(Value::as_str) == Some("contains");
            let mut found = Value::Null;
            for (i, v) in values().iter().enumerate() {
                let hit = if contains {
                    let (Some(hay), Some(pin)) = (v.as_str(), needle.as_str()) else {
                        return Err("index_of with match contains needs string values and a string needle".into());
                    };
                    hay.contains(pin)
                } else {
                    equality_key(v) == equality_key(needle)
                };
                if hit {
                    found = json!(i);
                    break;
                }
            }
            found
        }
        _ => return Err(format!("unknown primitive data operation {name}")),
    };
    bounded(out)
}
struct ItemEnv(Value);
impl Env for ItemEnv {
    fn lookup(&self, path: &[String]) -> Result<Value, String> {
        ritual::descend(self.0.clone(), path, path)
    }
}

pub fn collection(
    name: &str,
    a: &Value,
    tick: Option<&dyn Fn() -> Result<(), Fail>>,
) -> Result<Value, Fail> {
    let invalid = Fail::Invalid;
    let operation = if name != "filter" {
        let op = a["operation"].as_str().unwrap();
        let short = op
            .strip_prefix("data.")
            .ok_or_else(|| invalid("collection operations must use data.*".into()))?;
        if matches!(short, "map" | "filter" | "fold") || crate::pure::schema(short).is_none() {
            return Err(invalid(
                "nested collections and non-pure operations are not allowed".into(),
            ));
        }
        Some(short)
    } else {
        None
    };
    let predicate = if name == "filter" {
        Some(ritual::parse_expr(a["predicate"].as_str().unwrap()).map_err(invalid)?)
    } else {
        None
    };
    let mut acc = a.get("initial").cloned().unwrap_or(Value::Null);
    let mut result = Vec::new();
    let mut bytes = 2usize;
    for (index, item) in a["values"].as_array().unwrap().iter().enumerate() {
        if let Some(tick) = tick {
            tick()?;
        }
        let env = ItemEnv(json!({"item":item,"index":index,"acc":acc}));
        let output = if let Some(expr) = &predicate {
            ritual::eval(expr, &env).map_err(invalid)?
        } else {
            let args = ritual::render_value(&a["arguments"], &env).map_err(invalid)?;
            crate::pure::call(operation.unwrap(), &args).map_err(invalid)?
        };
        match name {
            "fold" => acc = output,
            "map" => {
                bytes = bytes.saturating_add(output.to_string().len() + 1);
                result.push(output);
            }
            "filter" => match output.as_bool() {
                Some(true) => {
                    bytes = bytes.saturating_add(item.to_string().len() + 1);
                    result.push(item.clone());
                }
                Some(false) => {}
                None => return Err(invalid("filter predicate must return a boolean".into())),
            },
            _ => unreachable!(),
        }
        if bytes > MAX_BYTES {
            return Err(invalid("collection output exceeds 1 MiB".into()));
        }
    }
    bounded(if name == "fold" { acc } else { json!(result) }).map_err(invalid)
}

pub fn validate_collection(op: &str, args: &Value) -> Result<(), String> {
    fn refs(value: &Value, fold: bool) -> Result<(), String> {
        match value {
            Value::String(s) => {
                for p in ritual::template_refs(s)? {
                    if !(matches!(p.first().map(String::as_str), Some("item" | "index"))
                        || (fold && p.first().map(String::as_str) == Some("acc")))
                    {
                        return Err(
                            "collection body references must start with item or index (acc is available only in fold)".into(),
                        );
                    }
                }
            }
            Value::Array(a) => {
                for x in a {
                    refs(x, fold)?;
                }
            }
            Value::Object(m) => {
                for x in m.values() {
                    refs(x, fold)?;
                }
            }
            _ => {}
        }
        Ok(())
    }
    if op == "data.filter" {
        let p = args
            .get("predicate")
            .and_then(Value::as_str)
            .ok_or("filter needs a literal predicate")?;
        // A `${}` left after normalization sits inside a quoted literal: it
        // would be substituted as text and then parsed, so a value could
        // rewrite the predicate. Predicates see only item and index.
        if p.contains("${") {
            return Err("a filter predicate cannot contain `${...}`: values would be pasted \
                        into it as source text; compare item fields with literals, or prepare \
                        the values in an earlier step"
                .into());
        }
        let expr = ritual::parse_expr(p)?;
        let mut paths = Vec::new();
        ritual::expr_refs(&expr, &mut paths);
        if paths
            .iter()
            .any(|p| !matches!(p.first().map(String::as_str), Some("item" | "index")))
        {
            return Err("filter references must start with item or index".into());
        }
    } else {
        let name = args
            .get("operation")
            .and_then(Value::as_str)
            .ok_or("collection needs a literal data operation")?;
        let short = name
            .strip_prefix("data.")
            .ok_or("collections accept only data operations")?;
        if matches!(short, "map" | "filter" | "fold") || crate::pure::schema(short).is_none() {
            return Err("unknown primitive or nested collection operation".into());
        }
        let body = args.get("arguments").ok_or("collection needs arguments")?;
        refs(body, op == "data.fold")?;
        crate::reagent::validate_arguments(name, body, false)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    fn run(name: &str, args: Value) -> Result<Value, String> {
        crate::pure::call(name, &args)
    }
    #[test]
    fn primitives_preserve_types_and_fail_explicitly() {
        assert_eq!(
            run(
                "arithmetic",
                json!({"left":7,"right":-2,"operator":"divide"})
            )
            .unwrap(),
            -3
        );
        for a in [
            json!({"left":i64::MAX,"right":1,"operator":"add"}),
            json!({"left":i64::MIN,"right":-1,"operator":"divide"}),
            json!({"left":1,"right":0,"operator":"remainder"}),
            json!({"left":1.5,"right":2,"operator":"add"}),
        ] {
            assert!(run("arithmetic", a).is_err());
        }
        assert_eq!(
            run(
                "get",
                json!({"value":{"a/b":null},"path":"/a~1b","default":5})
            )
            .unwrap(),
            Value::Null
        );
        assert_eq!(
            run("get", json!({"value":{},"path":"/absent","default":5})).unwrap(),
            5
        );
        assert!(run("get", json!({"value":{},"path":"/absent"})).is_err());
        assert!(run("get", json!({"value":{},"path":"/a~2"})).is_err());
        assert_eq!(
            run(
                "sort",
                json!({"values":[9007199254740993u64,9007199254740992u64,-1]})
            )
            .unwrap(),
            json!([-1, 9007199254740992u64, 9007199254740993u64])
        );
        assert!(run("sort", json!({"values":[1,1.0]})).is_err());
        assert_eq!(
            run("unique", json!({"values":[-0.0,0.0]}))
                .unwrap()
                .as_array()
                .unwrap()
                .len(),
            1
        );
        assert_eq!(
            run("unique", json!({"values":[null,1,null,1,"1"]})).unwrap(),
            json!([null, 1, "1"])
        );
        assert_eq!(run("trim", json!({"text":"  é🙂 \n"})).unwrap(), "é🙂");
        assert_eq!(
            run("split", json!({"text":"a,,b","separator":","})).unwrap(),
            json!(["a", "", "b"])
        );
        assert_eq!(
            run("join", json!({"values":["é","🙂"],"separator":"/"})).unwrap(),
            "é/🙂"
        );
        assert!(run(
            "replace",
            json!({"text":"x".repeat(2000),"from":"x","to":"y".repeat(2000)})
        )
        .is_err());
        assert!(run("slice", json!({"values":[1],"start":0,"end":2})).is_err());
    }
    #[test]
    fn comparisons_preserve_large_integer_literals() {
        let env = ItemEnv(json!({}));
        for expression in [
            "9007199254740993 > 9007199254740992",
            "-2 < -1",
            "1 <= 1",
            "2 >= 1",
            "'a' < 'b'",
        ] {
            assert!(ritual::eval_condition(expression, &env).unwrap());
        }
        assert!(ritual::eval_condition("1 < '2'", &env).is_err());
        assert!(ritual::eval_condition("1 < 2.0", &env).is_err());
        assert!(ritual::parse_expr(&"not ".repeat(300)).is_err());
    }
    #[test]
    fn collections_are_pure_bounded_and_accounted() {
        let args = json!({"values":[1,2,3],"operation":"data.arithmetic","arguments":{"left":"${acc}","right":"${item}","operator":"add"},"initial":0});
        validate_collection("data.fold", &args).unwrap();
        assert_eq!(collection("fold", &args, None).unwrap(), 6);
        let count = std::cell::Cell::new(0);
        let tick = || {
            count.set(count.get() + 1);
            if count.get() > 2 {
                Err(Fail::Exhausted("items".into()))
            } else {
                Ok(())
            }
        };
        assert!(matches!(
            collection("fold", &args, Some(&tick)),
            Err(Fail::Exhausted(_))
        ));
        assert_eq!(
            collection(
                "filter",
                &json!({"values":[1,2,3],"predicate":"item > 1"}),
                None
            )
            .unwrap(),
            json!([2, 3])
        );
        for op in ["mail.read", "data.map", "data.missing"] {
            assert!(validate_collection(
                "data.map",
                &json!({"values":[],"operation":op,"arguments":{}})
            )
            .is_err());
        }
        assert!(
            validate_collection("data.filter", &json!({"predicate":"inputs.secret == 1"})).is_err()
        );
        assert!(validate_collection(
            "data.filter",
            &json!({"predicate":"item.team == '${inputs.team}'"})
        )
        .is_err());
    }
    #[test]
    fn extended_stdlib_logic_math_objects_and_hashing() {
        assert_eq!(run("all", json!({"values":[true,true]})).unwrap(), true);
        assert_eq!(run("all", json!({"values":[true,false]})).unwrap(), false);
        assert_eq!(run("any", json!({"values":[false,true]})).unwrap(), true);
        assert_eq!(run("any", json!({"values":[false,false]})).unwrap(), false);
        assert_eq!(
            run("logic", json!({"operator":"xor","left":true,"right":false})).unwrap(),
            true
        );
        assert_eq!(
            run("logic", json!({"operator":"not","left":false})).unwrap(),
            true
        );
        assert!(run("logic", json!({"operator":"not","left":false,"right":true})).is_err());
        assert_eq!(
            run("select", json!({"condition":true,"then":"yes","else":"no"})).unwrap(),
            "yes"
        );
        assert_eq!(run("min", json!({"values":[9,-4,3]})).unwrap(), -4);
        assert_eq!(run("max", json!({"values":[9,-4,3]})).unwrap(), 9);
        assert!(run("min", json!({"values":[1,"a"]})).is_err());
        assert_eq!(
            run("clamp", json!({"value":150,"min":0,"max":100})).unwrap(),
            100
        );
        assert!(run("clamp", json!({"value":50,"min":100,"max":0})).is_err());
        assert_eq!(run("abs", json!({"value":-42})).unwrap(), 42);
        assert!(run("abs", json!({"value":i64::MIN})).is_err());
        assert_eq!(
            run(
                "merge",
                json!({"left":{"a":1,"b":2},"right":{"b":9,"c":3},"conflict":"right"})
            )
            .unwrap(),
            json!({"a":1,"b":9,"c":3})
        );
        assert!(run(
            "merge",
            json!({"left":{"a":1},"right":{"a":2},"conflict":"error"})
        )
        .is_err());
        assert_eq!(
            run("set", json!({"value":{"a":1},"field":"b","item":2})).unwrap(),
            json!({"a":1,"b":2})
        );
        assert_eq!(
            run("omit", json!({"value":{"a":1,"secret":"x"},"fields":["secret"]})).unwrap(),
            json!({"a":1})
        );
        let entries = run("entries", json!({"value":{"b":2,"a":1}})).unwrap();
        assert_eq!(entries, json!([{"key":"a","value":1},{"key":"b","value":2}]));
        assert_eq!(
            run("from_entries", json!({"entries":entries})).unwrap(),
            json!({"a":1,"b":2})
        );
        let canon = run("canonical_json", json!({"value":{"b":2,"a":1}})).unwrap();
        assert_eq!(canon, "{\"a\":1,\"b\":2}");
        assert_eq!(
            run("sha256", json!({"text":canon})).unwrap(),
            crate::util::sha256_hex(b"{\"a\":1,\"b\":2}")
        );
        assert_eq!(
            run("contains", json!({"value":"hello world","needle":"world"})).unwrap(),
            true
        );
        assert_eq!(
            run("contains", json!({"value":[1,2,3],"needle":2})).unwrap(),
            true
        );
        assert_eq!(
            run("starts_with", json!({"text":"ORD-100","prefix":"ORD-"})).unwrap(),
            true
        );
        assert_eq!(
            run("ends_with", json!({"text":"report.json","suffix":".json"})).unwrap(),
            true
        );
        assert_eq!(
            run("case", json!({"text":"SummonR","mode":"lower"})).unwrap(),
            "summonr"
        );
        assert_eq!(
            run("lines", json!({"text":"a\nb\r\nc\n"})).unwrap(),
            json!(["a", "b", "c"])
        );
    }

    #[test]
    fn gap_operations_from_bughunt() {
        // coalesce: first non-null, or null.
        assert_eq!(run("coalesce", json!({"values":[null, "a", "b"]})).unwrap(), "a");
        assert_eq!(run("coalesce", json!({"values":[null, null]})).unwrap(), Value::Null);
        assert_eq!(run("coalesce", json!({"values":[]})).unwrap(), Value::Null);
        // flatten: one level, every element an array.
        assert_eq!(run("flatten", json!({"values":[[1], [], [2, [3]]]})).unwrap(), json!([1, 2, [3]]));
        assert!(run("flatten", json!({"values":[[1], 2]})).is_err());
        // range: start <= n < end by step.
        assert_eq!(run("range", json!({"start":0, "end":10, "step":4})).unwrap(), json!([0, 4, 8]));
        assert_eq!(run("range", json!({"start":2, "end":5})).unwrap(), json!([2, 3, 4]));
        assert_eq!(run("range", json!({"start":5, "end":5})).unwrap(), json!([]));
        assert!(run("range", json!({"start":0, "end":5, "step":0})).is_err());
        assert!(run("range", json!({"start":0, "end":20000})).is_err());
        assert!(run("range", json!({"start":0, "end":5, "step":9223372036854775808u64})).is_err());
        assert!(run("lines", json!({"text":"a", "number_from":9223372036854775808u64})).is_err());
        // index_of: equal by default, contains for strings, null when absent.
        assert_eq!(run("index_of", json!({"values":["a", "b", "b"], "needle":"b"})).unwrap(), 1);
        assert_eq!(
            run("index_of", json!({"values":["fn a() {", "    let x = 1;"], "needle":"let x", "match":"contains"})).unwrap(),
            1
        );
        assert_eq!(run("index_of", json!({"values":[1, 2], "needle":3})).unwrap(), Value::Null);
        assert!(run("index_of", json!({"values":[1], "needle":"1", "match":"contains"})).is_err());
        // lines: optional numbering.
        assert_eq!(
            run("lines", json!({"text":"a\nb", "number_from":10})).unwrap(),
            json!(["10: a", "11: b"])
        );
    }
}
