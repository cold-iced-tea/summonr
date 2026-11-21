use super::*;


/// Tokenize a template string into literal text and `${ref}` segments.
/// `$${` escapes a literal `${`.
pub(super) enum TemplatePart {
    Lit(String),
    Ref(Vec<String>),
}

pub(super) fn scan_template(s: &str) -> Result<Vec<TemplatePart>, String> {
    let mut parts = Vec::new();
    let mut lit = String::new();
    let bytes = s.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'$' && bytes.get(i + 1) == Some(&b'$') && bytes.get(i + 2) == Some(&b'{') {
            lit.push_str("${");
            i += 3;
            continue;
        }
        if bytes[i] == b'$' && bytes.get(i + 1) == Some(&b'{') {
            if !lit.is_empty() {
                parts.push(TemplatePart::Lit(std::mem::take(&mut lit)));
            }
            let rest = &s[i + 2..];
            let end = rest
                .find('}')
                .ok_or_else(|| format!("unterminated `${{` in `{s}`"))?;
            let inner = rest[..end].trim();
            if inner.is_empty()
                || !inner
                    .chars()
                    .all(|c| c.is_alphanumeric() || matches!(c, '_' | '-' | '.'))
            {
                return Err(format!(
                    "`${{{inner}}}` is not a reference such as `${{inputs.count}}`"
                ));
            }
            parts.push(TemplatePart::Ref(
                inner.split('.').map(String::from).collect(),
            ));
            i += 2 + end + 1;
            continue;
        }
        let ch = s[i..].chars().next().unwrap();
        lit.push(ch);
        i += ch.len_utf8();
    }
    if !lit.is_empty() {
        parts.push(TemplatePart::Lit(lit));
    }
    Ok(parts)
}

pub(super) fn single_template_ref(parts: &[TemplatePart]) -> Option<&[String]> {
    match parts {
        [TemplatePart::Ref(r)] => Some(r.as_slice()),
        [TemplatePart::Lit(a), TemplatePart::Ref(r)] if a.trim().is_empty() => Some(r.as_slice()),
        [TemplatePart::Ref(r), TemplatePart::Lit(b)] if b.trim().is_empty() => Some(r.as_slice()),
        [TemplatePart::Lit(a), TemplatePart::Ref(r), TemplatePart::Lit(b)]
            if a.trim().is_empty() && b.trim().is_empty() =>
        {
            Some(r.as_slice())
        }
        _ => None,
    }
}

/// Find the `${...}` references in a template string.
pub fn template_refs(s: &str) -> Result<Vec<Vec<String>>, String> {
    Ok(scan_template(s)?
        .into_iter()
        .filter_map(|p| match p {
            TemplatePart::Ref(r) => Some(r),
            TemplatePart::Lit(_) => None,
        })
        .collect())
}

/// A string that is exactly one `${ref}` yields the referenced typed value.
/// Any other string has its references substituted as text.
pub fn render_template(s: &str, env: &dyn Env) -> Result<Value, String> {
    let parts = scan_template(s)?;
    if let Some(r) = single_template_ref(&parts) {
        return env.lookup(r);
    }
    let mut out = String::new();
    for p in parts {
        match p {
            TemplatePart::Lit(t) => out.push_str(&t),
            TemplatePart::Ref(r) => out.push_str(&plain_string(&env.lookup(&r)?)),
        }
    }
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

