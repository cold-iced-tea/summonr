use super::*;
use super::template::{scan_template, single_template_ref, TemplatePart};
use super::inputs::coerce;
use super::types::{closed_obj, nullable, operation_output_type};

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
                            Some(prop @ ("status" | "kind")) => {
                                if let Ty::Obj { fields, .. } = &out_ty {
                                    if fields.contains_key(prop) {
                                        self.errors.push(format!(
                                            "{at}: `{shown}` is ambiguous because step `{n}` outputs a field named `{prop}`; use `steps.{n}.output.{prop}` for the output field"
                                        ));
                                    }
                                }
                                self.descend_type(Ty::Str, &path[3..], path, at)
                            }
                            Some("counts") => {
                                if !matches!(out_ty, Ty::List(_) | Ty::Any) {
                                    self.errors.push(format!(
                                        "{at}: `{shown}`: step `{n}` is not an `each` step and has no `counts`"
                                    ));
                                    return Ty::Any;
                                }
                                let counts_ty = closed_obj([
                                    ("returned", Ty::Int),
                                    ("abstained", Ty::Int),
                                    ("failed", Ty::Int),
                                    ("invalid", Ty::Int),
                                    ("denied", Ty::Int),
                                    ("exhausted", Ty::Int),
                                    ("cancelled", Ty::Int),
                                    ("unverified", Ty::Int),
                                    ("interrupted", Ty::Int),
                                    ("remaining", Ty::Int),
                                    ("held", Ty::Int),
                                    ("applied", Ty::Int),
                                    ("simulated", Ty::Int),
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
            Value::String(s) => match scan_template(s) {
                Ok(parts) => {
                    if let Some(r) = single_template_ref(&parts) {
                        self.resolve_ref(r, at, None)
                    } else {
                        for p in &parts {
                            if let TemplatePart::Ref(r) = p {
                                self.resolve_ref(r, at, None);
                            }
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
                self.vars.push(("index".into(), Ty::Int));
                self.check_cond(pred, &format!("step {at} predicate"), None);
                self.vars.pop();
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
                    self.vars.push(("index".into(), Ty::Int));
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
        let body_all = self.ids.pop().unwrap_or_default();
        let steps_status = body_all
            .keys()
            .map(|k| (k.clone(), Ty::Str))
            .collect();
        // Runtime `BatchItem.outputs` includes only steps whose kind produces a
        // result (not nested `each` steps).
        let nested_each_ids: std::collections::HashSet<&str> = body
            .iter()
            .filter(|s| s.each.is_some())
            .filter_map(|s| s.id.as_deref())
            .collect();
        let body_outputs = body_all
            .into_iter()
            .filter(|(k, _)| !nested_each_ids.contains(k.as_str()))
            .collect();
        Ty::List(Box::new(closed_obj([
            ("item", Ty::Str),
            ("status", Ty::Str),
            ("error", nullable(Ty::Str)),
            ("interrupted", Ty::Bool),
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
        if name.is_empty()
            || !name
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
        {
            v.errors.push(format!(
                "inputs.{name}: input name `{name}` must use letters, digits, `-` or `_`"
            ));
        }
        if !matches!(
            d.ty.as_str(),
            "integer" | "decimal" | "boolean" | "string" | "list" | "array" | "object"
        ) {
            v.errors.push(format!(
                "inputs.{name}.type: input `{name}`: unsupported type `{}`",
                d.ty
            ));
        }
        if (d.min.is_some() || d.max.is_some()) && !matches!(d.ty.as_str(), "integer" | "decimal") {
            v.errors.push(format!(
                "inputs.{name}: `min`/`max` only apply to integer or decimal inputs, not `{}`",
                d.ty
            ));
        }
        if let (Some(lo), Some(hi)) = (d.min, d.max) {
            if lo > hi {
                v.errors.push(format!(
                    "inputs.{name}: `min` ({lo}) exceeds `max` ({hi})"
                ));
            }
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
        if let Some(def_val) = &d.default {
            if let Err(e) = coerce(name, d, def_val) {
                v.errors.push(format!("inputs.{name}.default: {e}"));
            }
        }
    }
    if let Some(t) = &def.essence.time {
        if let Err(e) = crate::util::parse_duration(t) {
            v.errors.push(format!("essence time: {e}"));
        }
    }
    if let Some(c) = &def.essence.cost {
        if let Err(e) = crate::util::parse_cost_microusd(c) {
            v.errors.push(format!("essence cost: {e}"));
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
            for body in [&s.steps, &s.then, &s.otherwise, &s.fallback].into_iter().flatten() {
                walk(body, def, resolve, invocation, depth, out);
            }
        }
    }
    let mut out = BTreeSet::new();
    walk(&def.steps, def, resolve, invocation, 0, &mut out);
    out
}
