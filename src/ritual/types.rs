use super::*;


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

pub(super) fn closed_obj<const N: usize>(entries: [(&str, Ty); N]) -> Ty {
    Ty::Obj {
        fields: entries
            .into_iter()
            .map(|(k, v)| (k.to_string(), v))
            .collect(),
        closed: true,
    }
}

/// The handle `sight.read_image` returns.
pub(super) fn image_handle_type() -> Ty {
    closed_obj([
        ("kind", Ty::Str),
        ("root", Ty::Str),
        ("path", Ty::Str),
        ("sha256", Ty::Str),
        ("mime", Ty::Str),
        ("bytes", Ty::Int),
        ("width", Ty::Int),
        ("height", Ty::Int),
    ])
}

pub(super) fn nullable(ty: Ty) -> Ty {
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

pub(super) fn operation_output_type(
    op: &str,
    args: Option<&BTreeMap<String, Value>>,
    arg_types: &BTreeMap<String, Ty>,
) -> Ty {
    match op {
        "sight.read_image" => image_handle_type(),
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
        "data.range" => Ty::List(Box::new(Ty::Int)),
        "data.index_of" => nullable(Ty::Int),
        "data.coalesce" => match arg_types.get("values") {
            Some(Ty::List(elem)) => (**elem).clone(),
            _ => Ty::Any,
        },
        "data.flatten" => match arg_types.get("values") {
            Some(Ty::List(elem)) => match &**elem {
                Ty::List(inner) => Ty::List(inner.clone()),
                _ => Ty::List(Box::new(Ty::Any)),
            },
            _ => Ty::List(Box::new(Ty::Any)),
        },
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
            ("summary", Ty::Str),
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
            ("summary", Ty::Str),
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

