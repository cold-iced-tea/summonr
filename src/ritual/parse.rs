use super::*;


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

fn normalize_expr_refs(src: &str) -> Result<String, String> {
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
                    if !is_bare_ref(trimmed) {
                        return Err(format!(
                            "`${{{trimmed}}}` in a condition must be a single reference; use parentheses for grouping"
                        ));
                    }
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
    Ok(out)
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
    desugared_assert: &mut bool,
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
                || s.retry.is_some()
                || s.fallback.is_some()
                || s.parallel.is_some()
                || s.on_failure.is_some()
                || s.keep.is_some()
                || s.version.is_some()
                || s.hash.is_some()
            {
                return Err("`fail` step cannot be combined with other step kinds or modifiers".into());
            }
            *seq += 1;
            *desugared_assert = true;
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
            *desugared_assert = true;
            let inner_id = format!("_assert_{seq}");
            let msg = s
                .message
                .take()
                .unwrap_or_else(|| format!("assertion failed: {cond}"));
            let norm_cond = normalize_expr_refs(&cond)?;
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
                        *pred = normalize_expr_refs(pred)?;
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
            match &mut m.images {
                Some(Images::One(r)) => normalize_template_str(r),
                Some(Images::Many(rs)) => rs.iter_mut().for_each(normalize_template_str),
                None => {}
            }
            if let Some(ab) = &mut m.abstain_if {
                *ab = normalize_expr_refs(ab)?;
            }
        }
        if let Some(cond) = &mut s.cond {
            *cond = normalize_expr_refs(cond)?;
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
            normalize_steps(inner, defs, data_tools, desugared_assert, seq)?;
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
    let mut desugared_assert = false;
    let mut seq = 0;
    normalize_steps(&mut def.steps, &def.defs, &mut data_tools, &mut desugared_assert, &mut seq)?;

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
    } else if desugared_assert {
        let entry = def.reagents.entry("data".into()).or_insert(ReagentDecl {
            select: Vec::new(),
            version: None,
        });
        if !entry.select.iter().any(|t| t == "assert") {
            entry.select.push("assert".into());
        }
    }
    Ok(())
}

/// Reject duplicate map keys anywhere in a YAML or JSON document before
/// deserializing into `BTreeMap`/`Map` fields.
struct NoDupKeys;

impl<'de> serde::de::DeserializeSeed<'de> for NoDupKeys {
    type Value = ();
    fn deserialize<D: serde::Deserializer<'de>>(self, d: D) -> Result<(), D::Error> {
        d.deserialize_any(NoDupKeys)
    }
}

impl<'de> serde::de::Visitor<'de> for NoDupKeys {
    type Value = ();
    fn expecting(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("any YAML or JSON value")
    }
    fn visit_bool<E>(self, _: bool) -> Result<(), E> { Ok(()) }
    fn visit_i64<E>(self, _: i64) -> Result<(), E> { Ok(()) }
    fn visit_u64<E>(self, _: u64) -> Result<(), E> { Ok(()) }
    fn visit_f64<E>(self, _: f64) -> Result<(), E> { Ok(()) }
    fn visit_str<E>(self, _: &str) -> Result<(), E> { Ok(()) }
    fn visit_bytes<E>(self, _: &[u8]) -> Result<(), E> { Ok(()) }
    fn visit_none<E>(self) -> Result<(), E> { Ok(()) }
    fn visit_unit<E>(self) -> Result<(), E> { Ok(()) }
    fn visit_seq<A: serde::de::SeqAccess<'de>>(self, mut seq: A) -> Result<(), A::Error> {
        while seq.next_element_seed(NoDupKeys)?.is_some() {}
        Ok(())
    }
    fn visit_map<A: serde::de::MapAccess<'de>>(self, mut map: A) -> Result<(), A::Error> {
        let mut seen = std::collections::HashSet::<String>::new();
        while let Some(k) = map.next_key::<String>()? {
            if !seen.insert(k.clone()) {
                return Err(serde::de::Error::custom(format!("duplicate key `{k}`")));
            }
            map.next_value_seed(NoDupKeys)?;
        }
        Ok(())
    }
}

/// Format a `serde_yaml::Error` with line and column context when available.
pub fn format_yaml_error(e: &serde_yaml::Error) -> String {
    match e.location() {
        Some(loc) => format!("line {}, column {}: {e}", loc.line(), loc.column()),
        None => e.to_string(),
    }
}

pub fn parse_file(text: &str) -> Result<Ritual, String> {
    use serde::de::DeserializeSeed;
    let de = serde_yaml::Deserializer::from_str(text);
    NoDupKeys.deserialize(de).map_err(|e| format_yaml_error(&e))?;
    let mut def: Ritual = serde_yaml::from_str(text).map_err(|e| format_yaml_error(&e))?;
    normalize(&mut def)?;
    Ok(def)
}

/// JSON is parsed strictly as JSON; builders are never executed here.
pub fn parse_definition(path: &Path, text: &str) -> Result<Ritual, String> {
    let mut def: Ritual = match path.extension().and_then(|s| s.to_str()) {
        Some("json") => {
            use serde::de::DeserializeSeed;
            let mut de = serde_json::Deserializer::from_str(text);
            NoDupKeys.deserialize(&mut de).map_err(|e| {
                format!(
                    "{}: line {}, column {}: invalid JSON definition: {e}",
                    path.display(),
                    e.line(),
                    e.column()
                )
            })?;
            serde_json::from_str(text).map_err(|e| {
                format!(
                    "{}: line {}, column {}: invalid JSON definition: {e}",
                    path.display(),
                    e.line(),
                    e.column()
                )
            })?
        }
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
        for inner in [&mut s.steps, &mut s.then, &mut s.otherwise, &mut s.fallback]
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
