use super::*;


#[cfg(test)]
mod core_tests {
    use super::*;
    use super::super::inputs::coerce;

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
    fn string_helpers_are_pure_strict_and_typed() {
        let env = E(json!({"item": {"claim": "  TODO: Maybe a bug  ", "line": "// note", "n": 3, "none": null}}));
        let t = |src: &str| eval(&parse_expr(src).unwrap(), &env).unwrap();
        assert_eq!(t("lower(item.claim)"), json!("  todo: maybe a bug  "));
        assert_eq!(t("upper('ab')"), json!("AB"));
        assert_eq!(t("trim(item.claim)"), json!("TODO: Maybe a bug"));
        assert_eq!(t("contains(lower(item.claim), 'maybe')"), json!(true));
        assert_eq!(t("starts_with(trim(item.line), '//')"), json!(true));
        assert_eq!(t("ends_with(item.line, 'note') and not contains(item.line, 'x')"), json!(true));
        assert_eq!(t("contains('', '')"), json!(true));
        // No implicit coercion: a number, a null or a list is an error.
        for bad in ["lower(item.n)", "trim(item.none)", "contains(item.line, 3)", "starts_with(item.n, '3')"] {
            let err = eval(&parse_expr(bad).unwrap(), &env).unwrap_err();
            assert!(err.contains("must be a string"), "{bad}: {err}");
        }
        // Unknown functions, wrong arity and stray commas fail to parse.
        assert!(parse_expr("shout(item.line)").unwrap_err().contains("unknown function `shout`"));
        assert!(parse_expr("lower(item.line, 'x')").unwrap_err().contains("takes 1 argument"));
        assert!(parse_expr("contains(item.line)").unwrap_err().contains("takes 2 arguments"));
        assert!(parse_expr("lower(item.line").is_err());
        assert!(parse_expr("item.a, item.b").is_err());
        // Calls count toward the token limit like everything else.
        assert!(parse_expr(&format!("{}x{}", "lower(".repeat(100), ")".repeat(100))).unwrap_err().contains("256 tokens"));
        let mut refs = Vec::new();
        expr_refs(&parse_expr("contains(lower(item.claim), params.word)").unwrap(), &mut refs);
        assert_eq!(refs, vec![vec!["item".to_string(), "claim".into()], vec!["params".to_string(), "word".into()]]);
    }

    #[test]
    fn string_helpers_are_checked_statically() {
        let none = |_: &str, _: Option<u32>| None;
        let def = parse_file(
            r#"
ritual: r
inputs:
  count: {type: integer}
  name: {type: string}
reagents:
  data: {select: [assert]}
steps:
  - if: lower(inputs.count) == "x"
    then:
      - {id: a, operation: data.assert, arguments: {condition: true}}
  - if: contains(inputs.name, inputs.count)
    then:
      - {id: b, operation: data.assert, arguments: {condition: true}}
  - if: lower(inputs.name)
    then:
      - {id: c, operation: data.assert, arguments: {condition: true}}
  - if: starts_with(trim(inputs.name), "x") and upper(inputs.name) != "Y"
    then:
      - {id: d, operation: data.assert, arguments: {condition: true}}
"#,
        )
        .unwrap();
        let errs = validate(&def, &none, &crate::reagent::tool, 3).unwrap_err();
        assert_eq!(errs.iter().filter(|e| e.contains("argument 1 of `lower` has type integer, expected string")).count(), 1, "{errs:?}");
        assert!(errs.iter().any(|e| e.contains("argument 2 of `contains` has type integer, expected string")), "{errs:?}");
        assert!(errs.iter().any(|e| e.contains("condition has type string, expected boolean")), "{errs:?}");
        assert_eq!(errs.len(), 3, "{errs:?}");
    }

    #[test]
    fn coalesce_with_a_non_null_element_is_not_nullable() {
        let none = |_: &str, _: Option<u32>| None;
        let def = parse_file(
            r#"
ritual: r
inputs:
  text: {type: string}
  flag: {type: boolean}
reagents:
  data: {select: [index_of, coalesce, range, trim]}
steps:
  - id: at
    operation: data.index_of
    arguments: {values: ["a", "b"], needle: "${inputs.text}"}
  - if: inputs.flag
    then:
      - id: t
        operation: data.trim
        arguments: {text: "${inputs.text}"}
  - id: pos
    operation: data.coalesce
    arguments: {values: ["${steps.at.output}", 0]}
  - id: name
    operation: data.coalesce
    arguments: {values: ["${steps.t.output}", "${inputs.text}", ""]}
  - id: first
    operation: data.coalesce
    arguments: {values: [null, 3, "${steps.t.output}"]}
  - id: mixed
    operation: data.coalesce
    arguments: {values: ["${steps.t.output}", 0]}
  - id: maybe
    operation: data.coalesce
    arguments: {values: ["${steps.at.output}", null]}
  - id: dynamic
    operation: data.coalesce
    arguments: {values: ["${steps.at.output}"]}
  - id: nums
    operation: data.range
    arguments: {start: 0, end: "${steps.pos.output}"}
return:
  pos: "${steps.pos.output}"
  name: "${steps.name.output}"
  first: "${steps.first.output}"
  mixed: "${steps.mixed.output}"
  maybe: "${steps.maybe.output}"
  dynamic: "${steps.dynamic.output}"
"#,
        )
        .unwrap();
        let (_, types) = validate_with_return_types_cfg(&def, &none, &crate::util::Config::default(), 3).unwrap();
        assert_eq!(types["pos"], "integer", "{types:?}");
        assert_eq!(types["name"], "string", "{types:?}");
        // Elements after the first certain one can never be chosen.
        assert_eq!(types["first"], "integer", "{types:?}");
        // Non-null, but the candidates disagree.
        assert_eq!(types["mixed"], "any", "{types:?}");
        assert_eq!(types["maybe"], "nullable integer", "{types:?}");
        assert_eq!(types["dynamic"], "nullable integer", "{types:?}");
    }

    #[test]
    fn a_grant_under_a_directory_covers_roots_beneath_it_and_narrows_to_them() {
        let g = |reagent: &str, on: &[(&str, &str)]| Grant {
            reagent: reagent.into(),
            action: "read_text".into(),
            on: on.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect(),
            into: BTreeMap::new(),
        };
        let under = g("file", &[("under", "/home/u/Work")]);
        for root in ["/home/u/Work", "/home/u/Work/a", "/home/u/Work/a/b/", "/home/u//Work/a"] {
            assert!(under.covers(&g("file", &[("root", root)])), "{root}");
        }
        for root in ["/home/u/Work-other", "/home/u", "/home/u/Work/../x", "/home/u/Work/./a", "Work/a", "/"] {
            assert!(!under.covers(&g("file", &[("root", root)])), "{root}");
        }
        // Only directory-rooted reagents read `under` as a directory.
        assert!(!g("http", &[("under", "/home/u/Work")]).covers(&g("http", &[("root", "/home/u/Work/a")])));
        assert!(g("sight", &[("under", "/p")]).covers(&g("sight", &[("root", "/p/q")])));
        // Narrowing keeps the exact root, so run-time checks compare roots only.
        let need = [g("file", &[("root", "/home/u/Work/a")])];
        assert_eq!(intersect_grants(std::slice::from_ref(&under), &need), need.to_vec());
        assert!(intersect_grants(std::slice::from_ref(&under), &[g("file", &[("root", "/etc")])]).is_empty());
        // Two directories meet at the deeper one, or not at all.
        let deeper = g("file", &[("under", "/home/u/Work/a")]);
        assert_eq!(intersect_grants(std::slice::from_ref(&under), std::slice::from_ref(&deeper)), vec![deeper.clone()]);
        assert!(intersect_grants(&[deeper], &[g("file", &[("under", "/home/u/Work/b")])]).is_empty());
        assert!(root_is_under("/a/b", "/a") && !root_is_under("/ab", "/a") && !root_is_under("/a", "/a/b"));
    }

    #[test]
    fn a_model_step_routes_within_a_static_set() {
        let none = |_: &str, _: Option<u32>| None;
        let base = |model: Value| {
            parse_file(&serde_json::to_string(&json!({
                "ritual": "route",
                "inputs": {"size": {"type": "string"}, "n": {"type": "integer"}},
                "essence": {"requests": 1},
                "steps": [{"id": "c", "model": model}],
                "return": {"out": "${steps.c.output}"}
            })).unwrap())
            .unwrap()
        };
        let good = base(json!({"one_of": ["mock/fast", "ollama/big", "mycli/default"], "choose": "${inputs.size}", "prompt": "hi"}));
        let effects = validate(&good, &none, &crate::reagent::tool, 3).unwrap();
        assert_eq!(effects[0].what, "one of mock/fast, ollama/big, mycli/default");
        // Every member is a model site, so fingerprints, rehearsal and assays see them all.
        let sites = model_sites(&good, &none, Some("mock/override"));
        assert_eq!(sites, ["mock/fast", "ollama/big", "mycli/default"].iter().map(|s| s.to_string()).collect());
        // The new fields are omitted when unused, so existing hashes are unchanged.
        let plain = base(json!({"name": "mock/fast", "prompt": "hi"}));
        let text = canonical_json(&plain).to_string();
        assert!(!text.contains("one_of") && !text.contains("choose"), "{text}");
        for (model, why) in [
            (json!({"one_of": ["mock/a"], "prompt": "hi"}), "needs `choose`"),
            (json!({"choose": "${inputs.size}", "prompt": "hi"}), "`choose` needs `one_of`"),
            (json!({"name": "mock/a", "one_of": ["mock/a"], "choose": "${inputs.size}", "prompt": "hi"}), "either `name` or `one_of`"),
            (json!({"one_of": [], "choose": "${inputs.size}", "prompt": "hi"}), "at least one model"),
            (json!({"one_of": ["mock/a", "mock/a"], "choose": "${inputs.size}", "prompt": "hi"}), "twice"),
            (json!({"one_of": ["nobackend"], "choose": "${inputs.size}", "prompt": "hi"}), "backend/model"),
            (json!({"one_of": ["mock/a"], "choose": "mock/${inputs.size}", "prompt": "hi"}), "a single reference"),
            (json!({"one_of": ["mock/a"], "choose": "mock/a", "prompt": "hi"}), "a single reference"),
            (json!({"one_of": ["mock/a"], "choose": "${inputs.n}", "prompt": "hi"}), "has type integer, expected string"),
            (json!({"one_of": ["mock/a"], "choose": "${inputs.nope}", "prompt": "hi"}), "undeclared input"),
        ] {
            let errs = validate(&base(model.clone()), &none, &crate::reagent::tool, 3).unwrap_err();
            assert!(errs.iter().any(|e| e.contains(why)), "{model}: {errs:?}");
        }
        let many: Vec<String> = (0..17).map(|i| format!("mock/m{i}")).collect();
        let errs = validate(&base(json!({"one_of": many, "choose": "${inputs.size}", "prompt": "hi"})), &none, &crate::reagent::tool, 3).unwrap_err();
        assert!(errs.iter().any(|e| e.contains("at most 16")), "{errs:?}");
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

    #[test]
    fn filter_predicate_refuses_text_substitution() {
        // A `${}` inside a quoted literal would be substituted as text and
        // then parsed, letting an input rewrite the predicate.
        let yaml = |pred: &str| {
            format!(
                "ritual: r\ninputs: {{team: string}}\nrequires: [{{reagent: data, action: filter}}]\nsteps:\n  - id: visible\n    operation: data.filter\n    arguments:\n      values: [{{team: red}}]\n      predicate: {pred:?}\nreturn: {{v: \"${{steps.visible.output}}\"}}\n"
            )
        };
        let check = |pred: &str| {
            let mut def = parse_file(&yaml(pred)).unwrap();
            prepare(&mut def, std::path::Path::new("."), &|_, _| None)?;
            validate(&def, &|_, _| None, &crate::reagent::tool, 3).map_err(|e| e.join("; "))
        };
        let e = check("item.team == '${inputs.team}'").unwrap_err();
        assert!(e.contains("predicate"), "{e}");
        assert!(check("item.team == 'red'").is_ok());
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
    fn coalesce_may_be_null_and_keep_names_only_recorded_steps() {
        let none = |_: &str, _: Option<u32>| None;
        // coalesce of an empty or all-null list is null, so its type is nullable.
        let args = std::collections::BTreeMap::from([(
            "values".to_string(),
            crate::ritual::types::Ty::List(Box::new(crate::ritual::types::Ty::Str)),
        )]);
        let ty = crate::ritual::types::operation_output_type("data.coalesce", None, &args);
        assert_eq!(ty.label(), "nullable string");
        let keep_if = parse_file(
            r#"
ritual: r
steps:
  - id: loop
    each: x in [1]
    keep: [gate]
    steps:
      - id: gate
        if: "true"
        then:
          - id: inner
            operation: data.trim
            arguments: {text: " a "}
"#,
        )
        .unwrap();
        let errs = validate(&keep_if, &none, &crate::reagent::tool, 3).unwrap_err();
        assert!(errs.iter().any(|e| e.contains("`keep` names `gate`")), "{errs:?}");
        let keep_branch = parse_file(
            r#"
ritual: r
steps:
  - id: loop
    each: x in [1]
    keep: [inner]
    steps:
      - if: "true"
        then:
          - id: inner
            operation: data.trim
            arguments: {text: " a "}
"#,
        )
        .unwrap();
        assert!(validate(&keep_branch, &none, &crate::reagent::tool, 3).is_ok());
    }

    #[test]
    fn each_keep_is_checked() {
        let none = |_: &str, _: Option<u32>| None;
        let src = |keep: &str, ret: &str| {
            format!(
                r#"
ritual: r
steps:
  - id: loop
    each: x in [1]
    keep: {keep}
    steps:
      - id: a
        operation: data.trim
        arguments: {{text: " a "}}
      - id: b
        operation: data.trim
        arguments: {{text: " b "}}
return:
  out: "${{steps.loop.output.0.outputs.{ret}}}"
"#
            )
        };
        assert!(validate(&parse_file(&src("[a]", "a")).unwrap(), &none, &crate::reagent::tool, 3).is_ok());
        let errs = validate(&parse_file(&src("[nope]", "b")).unwrap(), &none, &crate::reagent::tool, 3).unwrap_err();
        assert!(errs.iter().any(|e| e.contains("`keep` names `nope`")), "{errs:?}");
        let errs = validate(&parse_file(&src("[a]", "b")).unwrap(), &none, &crate::reagent::tool, 3).unwrap_err();
        assert!(errs.iter().any(|e| e.contains("has no field `b`")), "{errs:?}");
        let not_each = parse_file(
            r#"
ritual: r
steps:
  - id: a
    operation: data.trim
    arguments: {text: " a "}
    keep: [a]
"#,
        )
        .unwrap();
        assert!(
            validate(&not_each, &none, &crate::reagent::tool, 3).is_err(),
            "keep outside each must be rejected"
        );
    }

    #[test]
    fn static_types_for_gap_operations() {
        let none = |_: &str, _: Option<u32>| None;
        // range yields integers: looking inside a loop element is an error.
        let range = parse_file(
            r#"
ritual: r
steps:
  - id: starts
    operation: data.range
    arguments: {start: 0, end: 10, step: 5}
  - id: loop
    each: n in steps.starts.output
    steps:
      - id: one
        operation: data.trim
        arguments: {text: "${n.field}"}
"#,
        )
        .unwrap();
        let errs = validate(&range, &none, &crate::reagent::tool, 3).unwrap_err();
        assert!(errs.iter().any(|e| e.contains("cannot look inside")), "{errs:?}");
        // flatten keeps the inner element type; lines stays a list of strings.
        let flat = parse_file(
            r#"
ritual: r
steps:
  - id: ls
    operation: data.lines
    arguments: {text: "a", number_from: 1}
  - id: flat
    operation: data.flatten
    arguments: {values: ["${steps.ls.output}", ["b"]]}
  - id: loop
    each: line in steps.flat.output
    steps:
      - id: one
        operation: data.trim
        arguments: {text: "${line.field}"}
"#,
        )
        .unwrap();
        let errs = validate(&flat, &none, &crate::reagent::tool, 3).unwrap_err();
        assert!(errs.iter().any(|e| e.contains("cannot look inside")), "{errs:?}");
        // index_of is a nullable integer.
        let idx = parse_file(
            r#"
ritual: r
steps:
  - id: at
    operation: data.index_of
    arguments: {values: [a, b], needle: b}
return:
  bad: "${steps.at.output.field}"
"#,
        )
        .unwrap();
        let errs = validate(&idx, &none, &crate::reagent::tool, 3).unwrap_err();
        assert!(errs.iter().any(|e| e.contains("cannot look inside")), "{errs:?}");
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

    const IMAGES_HASH_RITUAL: &str = r#"
ritual: canonical
inputs:
  text: {type: string}
model: mock/m
steps:
  - id: s
    model:
      prompt: "hi ${inputs.text}"
return:
  out: "${steps.s.output}"
"#;

    #[test]
    fn ritual_without_images_hash_unchanged() {
        let parsed = parse_file(IMAGES_HASH_RITUAL).unwrap();
        assert_eq!(content_hash(&parsed), "37b41b37668d5e7cd6ea75c7306647c43f8e38882342954a9ce4b3280f0c897c");
    }

    fn images_ritual(images: &str) -> String {
        format!(
            r#"
ritual: img
model: mock/m
inputs:
  p: {{type: string}}
  hs: {{type: list}}
reagents:
  sight: {{select: [read_image]}}
  data: {{select: [trim]}}
requires:
  - {{reagent: sight, action: read_image, on: {{root: /scans}}}}
steps:
  - id: page
    operation: sight.read_image
    arguments: {{root: /scans, path: "${{inputs.p}}"}}
  - id: txt
    operation: data.trim
    arguments: {{text: "${{inputs.p}}"}}
  - id: read
    model:
      prompt: "go"
      images: {images}
return:
  out: "${{steps.read.output}}"
"#
        )
    }

    fn images_errors(images: &str) -> Result<(), Vec<String>> {
        let def = parse_file(&images_ritual(images)).unwrap();
        validate(&def, &|_, _| None, &crate::reagent::tool, 3).map(|_| ())
    }

    #[test]
    fn model_images_shorthand_expands() {
        let def = parse_file(&images_ritual("[$steps.page.output]")).unwrap();
        let m = def.steps.iter().find(|s| s.id.as_deref() == Some("read")).unwrap();
        match m.model.as_ref().unwrap().images.as_ref().unwrap() {
            Images::Many(v) => assert_eq!(v, &vec!["${steps.page.output}".to_string()]),
            other => panic!("{other:?}"),
        }
        images_errors("[$steps.page.output]").unwrap();
    }

    #[test]
    fn model_images_single_reference_to_list() {
        let def = parse_file(&images_ritual("\"${inputs.hs}\"")).unwrap();
        let m = def.steps.iter().find(|s| s.id.as_deref() == Some("read")).unwrap();
        assert!(matches!(
            m.model.as_ref().unwrap().images,
            Some(Images::One(ref r)) if r == "${inputs.hs}"
        ));
        images_errors("\"${inputs.hs}\"").unwrap();
    }

    #[test]
    fn model_images_literal_rejected() {
        let errs = images_errors("[\"hello\"]").unwrap_err();
        assert!(errs.iter().any(|e| e.contains("images must be references")), "{errs:?}");
    }

    #[test]
    fn model_images_more_than_eight_rejected() {
        let nine = format!("[{}]", ["$steps.page.output"; 9].join(", "));
        let errs = images_errors(&nine).unwrap_err();
        assert!(errs.iter().any(|e| e.contains("at most 8")), "{errs:?}");
        let eight = format!("[{}]", ["$steps.page.output"; 8].join(", "));
        images_errors(&eight).unwrap();
    }

    #[test]
    fn model_images_ref_to_non_image_rejected() {
        let errs = images_errors("[$steps.txt.output]").unwrap_err();
        assert!(errs.iter().any(|e| e.contains("not an image handle")), "{errs:?}");
        let errs = images_errors("[$inputs.p]").unwrap_err();
        assert!(errs.iter().any(|e| e.contains("not an image handle")), "{errs:?}");
    }

    #[test]
    fn model_images_unknown_type_and_loop_var_pass() {
        // input of object type is untyped beyond "object": must pass
        let raw = r#"
ritual: img2
model: mock/m
inputs:
  h: {type: object}
  hs: {type: list}
reagents:
  sight: {select: [read_image]}
requires:
  - {reagent: sight, action: read_image, on: {root: /scans}}
steps:
  - id: page
    operation: sight.read_image
    arguments: {root: /scans, path: "x"}
  - id: loop
    each: img in ["${steps.page.output}"]
    steps:
      - id: read
        model:
          prompt: "go"
          images: [$img]
  - id: read2
    model:
      prompt: "go"
      images: [$inputs.h]
return:
  out: "${steps.read2.output}"
"#;
        let def = parse_file(raw).unwrap();
        validate(&def, &|_, _| None, &crate::reagent::tool, 3).unwrap();
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

    #[test]
    fn batch_d_language_fixes_hold() {
        // D1: `${...}` with compound expression in condition is rejected; exact
        // single-ref does not drop trailing text; `$${` escapes literal `${`.
        assert!(parse_file(
            "ritual: r\ninputs: {a: boolean, b: boolean}\nsteps:\n  - if: \"not ${inputs.a or inputs.b}\"\n    then: [{id: x, operation: data.trim, arguments: {text: ok}}]\n"
        ).is_err());
        struct MapEnv(Value);
        impl Env for MapEnv {
            fn lookup(&self, path: &[String]) -> Result<Value, String> {
                descend(self.0.clone(), path, path)
            }
        }
        let env = MapEnv(json!({"inputs": {"n": 7}}));
        assert_eq!(render_template("${inputs.n} items}", &env).unwrap(), json!("7 items}"));
        assert_eq!(render_template("literal $${HOME}", &env).unwrap(), json!("literal ${HOME}"));

        // D2: duplicate keys rejected in both YAML and JSON.
        assert!(parse_file("ritual: r\ninputs: {a: string, a: integer}\nsteps: [{id: x, model: {prompt: hi}}]\n").is_err());
        let dup_json = r#"{"ritual":"r","inputs":{"a":{"type":"string"},"a":{"type":"integer"}},"steps":[{"id":"x","model":{"prompt":"hi"}}]}"#;
        assert!(parse_definition(Path::new("dup.json"), dup_json).is_err());

        // D3: unknown field `description` on a scalar input errors instead of morphing to object.
        assert!(parse_file("ritual: r\ninputs:\n  name: {type: string, default: Bob, description: Who}\nsteps: [{id: x, model: {prompt: hi}}]\n").is_err());

        // D4: decimal input coerces integer default/arg to f64 consistently.
        let dec = parse_file("ritual: r\ninputs: {price: {type: decimal, default: 3}}\nsteps: [{id: x, model: {prompt: hi}}]\n").unwrap();
        let resolved = resolve_inputs(&dec, &Map::new()).unwrap();
        assert_eq!(resolved["price"], json!(3.0));
        assert!(eval_condition("inputs.price == 3", &MapEnv(json!({"inputs": resolved}))).unwrap());

        // D5: collection `index` is typed as Int and accepted; `counts` rejected on non-each.
        let idx_def = parse_file(
            "ritual: r\nreagents: {data: {select: [filter]}}\nsteps:\n  - id: f\n    operation: data.filter\n    arguments: {values: [1, 2, 3], predicate: \"index > 0\"}\nreturn: {v: \"${steps.f.output}\"}\n"
        ).unwrap();
        let none = |_: &str, _: Option<u32>| None;
        assert!(validate(&idx_def, &none, &crate::reagent::tool, 3).is_ok());
        let bad_counts = parse_file(
            "ritual: r\nreagents: {data: {select: [trim]}}\nsteps:\n  - id: t\n    operation: data.trim\n    arguments: {text: hi}\nreturn: {c: \"${steps.t.counts.returned}\"}\n"
        ).unwrap();
        assert!(validate(&bad_counts, &none, &crate::reagent::tool, 3).is_err());

        // D7: ambiguous `steps.<id>.status` when output also has field `status` is rejected.
        let ambig = parse_file(
            "ritual: r\nsteps:\n  - id: ticket\n    model:\n      prompt: hi\n      schema: {status: [open, closed]}\n  - if: steps.ticket.status == 'open'\n    then: [{id: a, operation: data.trim, arguments: {text: ok}}]\n"
        ).unwrap();
        assert!(validate(&ambig, &none, &crate::reagent::tool, 3).is_err());

        // D8: exponential `$ref` expansion hits node budget quickly.
        let mut defs = Map::new();
        for i in 0..6 {
            let mut props = Map::new();
            for j in 0..10 {
                props.insert(format!("p{j}"), json!({"$ref": format!("#/$defs/d{}", i + 1)}));
            }
            defs.insert(format!("d{i}"), json!({"type": "object", "properties": props}));
        }
        defs.insert("d6".into(), json!({"type": "string"}));
        let bomb = json!({"$defs": defs, "$ref": "#/$defs/d0"});
        assert!(crate::schema::inline_refs(&bomb).unwrap_err().contains("exceeds"));

        // D10: `fail:` rejects `retry`/`fallback`; `assert:` works with explicit `reagents.data`;
        // bad input defaults fail validation.
        assert!(parse_file("ritual: r\nsteps:\n  - fail: stop\n    retry: 2\n").is_err());
        let asrt = parse_file(
            "ritual: r\nreagents: {data: {select: [trim]}}\nsteps:\n  - id: t\n    operation: data.trim\n    arguments: {text: hi}\n  - assert: \"steps.t.output == 'hi'\"\nreturn: {v: \"${steps.t.output}\"}\n"
        ).unwrap();
        assert!(validate(&asrt, &none, &crate::reagent::tool, 3).is_ok());
        let bad_def = parse_file(
            "ritual: r\ninputs: {n: {type: integer, default: abc}}\nsteps: [{id: x, model: {prompt: hi}}]\n"
        ).unwrap();
        assert!(validate(&bad_def, &none, &crate::reagent::tool, 3).is_err());
    }
}

#[cfg(test)]
mod contract_message_tests {
    #[test]
    fn contract_error_names_the_first_problem() {
        let one = vec!["step a: `x` has no field `y`".to_string()];
        assert_eq!(
            crate::ritual::contract_message(&one),
            "definition does not satisfy its contract: step a: `x` has no field `y`"
        );
        let three = vec!["first".to_string(), "second".to_string(), "third".to_string()];
        assert_eq!(
            crate::ritual::contract_message(&three),
            "definition does not satisfy its contract: first (and 2 more in data.diagnostics)"
        );
    }

    const LOOP_THEN_FILTER: &str = r#"
ritual: r
inputs: {files: list}
steps:
  - id: tested
    each: f in inputs.files
    steps:
      - id: source
        operation: file.read_text
        arguments: {root: /tmp, path: "${f}"}
      - id: numbered
        operation: data.lines
        arguments: {text: "${steps.source.output.text}"}
      - id: verdict
        model: {prompt: "judge ${steps.numbered.output}"}
  - id: confirmed
    operation: data.filter
    arguments: {values: "${steps.tested.output}", predicate: "item.outputs.verdict == 'yes'"}
return: {n: "${steps.confirmed.output}"}
"#;

    #[test]
    fn a_large_loop_without_keep_feeding_a_data_op_warns_with_the_ids_to_keep() {
        let def = crate::ritual::parse_file(LOOP_THEN_FILTER).unwrap();
        let w = crate::ritual::lint(&def);
        assert_eq!(w.len(), 1, "{w:?}");
        assert!(w[0].contains("each `tested` has no `keep:`"), "{}", w[0]);
        assert!(w[0].contains("`source` (file.read_text)"), "{}", w[0]);
        assert!(w[0].contains("step `confirmed` passes them to data.filter"), "{}", w[0]);
        assert!(w[0].contains("Add `keep: [verdict]` to `tested`"), "{}", w[0]);
    }

    #[test]
    fn loops_with_keep_small_steps_or_no_data_consumer_do_not_warn() {
        let kept = LOOP_THEN_FILTER.replace("    each: f in inputs.files\n", "    each: f in inputs.files\n    keep: [verdict]\n");
        assert!(crate::ritual::lint(&crate::ritual::parse_file(&kept).unwrap()).is_empty());
        let small = r#"
ritual: r
inputs: {xs: list}
steps:
  - id: loop
    each: x in inputs.xs
    steps:
      - {id: t, operation: data.trim, arguments: {text: "${x}"}}
  - {id: all, operation: data.count, arguments: {value: "${steps.loop.output}"}}
"#;
        assert!(crate::ritual::lint(&crate::ritual::parse_file(small).unwrap()).is_empty());
        let unread = LOOP_THEN_FILTER.replace("values: \"${steps.tested.output}\"", "values: []");
        assert!(crate::ritual::lint(&crate::ritual::parse_file(&unread).unwrap()).is_empty());
    }

    #[test]
    fn keep_suggestions_follow_fold_bodies_and_later_loops() {
        let fold = r#"
ritual: r
inputs: {xs: list}
steps:
  - id: loop
    each: x in inputs.xs
    steps:
      - {id: page, operation: file.read_text, arguments: {root: /tmp, path: "${x}"}}
      - {id: found, operation: data.count, arguments: {value: "${steps.page.output.text}"}}
  - id: total
    operation: data.fold
    arguments: {values: "${steps.loop.output}", initial: 0, operation: data.arithmetic,
                arguments: {left: "${acc}", right: "${item.outputs.found}", operator: add}}
  - id: again
    each: r in steps.loop.output
    steps:
      - {id: s, operation: data.trim, arguments: {text: "${r.outputs.page.text}"}}
"#;
        let w = crate::ritual::lint(&crate::ritual::parse_file(fold).unwrap());
        assert_eq!(w.len(), 1, "{w:?}");
        assert!(w[0].contains("Add `keep: [found, page]` to `loop`"), "{}", w[0]);
    }

    #[test]
    fn keep_suggestions_follow_items_through_a_filter() {
        // bughunt-test's shape: filter the loop's results, then fold the kept items.
        let text = LOOP_THEN_FILTER.replace(
            "return: {n: \"${steps.confirmed.output}\"}",
            "  - id: results\n    operation: data.fold\n    arguments: {values: \"${steps.confirmed.output}\", initial: [], operation: data.concat, arguments: {left: \"${acc}\", right: [\"${item.outputs.numbered}\"]}}\nreturn: {n: \"${steps.results.output}\"}",
        );
        let w = crate::ritual::lint(&crate::ritual::parse_file(&text).unwrap());
        assert!(w[0].contains("Add `keep: [numbered, verdict]` to `tested`"), "{w:?}");
    }

    #[test]
    fn filter_params_are_checked_statically() {
        let none = |_: &str, _: Option<u32>| None;
        let ok = crate::ritual::parse_file(r#"
ritual: r
inputs: {floor: integer}
steps:
  - id: f
    operation: data.filter
    arguments: {values: [1, 2], predicate: "item >= params.floor", params: {floor: "${inputs.floor}"}}
"#).unwrap();
        crate::ritual::validate(&ok, &none, &crate::reagent::tool, 3).unwrap();
        let undeclared = crate::ritual::parse_file(r#"
ritual: r
steps:
  - id: f
    operation: data.filter
    arguments: {values: [1, 2], predicate: "item >= params.floor"}
"#).unwrap();
        let errs = crate::ritual::validate(&undeclared, &none, &crate::reagent::tool, 3).unwrap_err();
        assert!(errs.iter().any(|e| e.contains("params.floor")), "{errs:?}");
    }

    #[test]
    fn finally_steps_are_checked_see_the_main_steps_and_declare_their_effects() {
        let none = |_: &str, _: Option<u32>| None;
        let compile = |text: &str| {
            let mut def = crate::ritual::parse_file(text).map_err(|e| vec![e])?;
            crate::ritual::prepare(&mut def, std::path::Path::new("."), &none).map_err(|e| vec![e])?;
            crate::ritual::validate(&def, &none, &crate::reagent::tool, 3)
        };
        let good = r#"
ritual: r
inputs: {branch: string}
requires:
  - {reagent: git, action: worktree_add}
  - {reagent: git, action: remove_worktree}
steps:
  - {id: wt, operation: git.worktree_add, arguments: {repo: demo, branch: "${inputs.branch}"}}
  - {id: n, operation: data.trim, arguments: {text: " x "}}
finally:
  - if: steps.wt.status == "returned"
    then:
      - {id: remove, operation: git.remove_worktree, arguments: {repo: demo, path: "${steps.wt.output.path}"}}
return: {n: "${steps.n.output}"}
"#;
        let effects = compile(good).unwrap();
        assert!(effects.iter().any(|e| e.step == "finally/#1/remove" && e.what == "git.remove_worktree"), "{effects:?}");
        let undeclared = good.replace("  - {reagent: git, action: remove_worktree}\n", "");
        assert!(compile(&undeclared).unwrap_err().iter().any(|e| e.contains("remove_worktree")), "a finally effect needs a requirement too");
        let bad = |from: &str, to: &str, want: &str| {
            let text = good.replace(from, to);
            let errs = compile(&text).unwrap_err();
            assert!(errs.iter().any(|e| e.contains(want)), "`{want}` not in {errs:?}");
        };
        bad("return: {n: \"${steps.n.output}\"}", "return: {n: \"${steps.remove.output}\"}", "remove");
        bad("{id: remove,", "{id: n,", "already used");
        bad("{text: \" x \"}", "{text: \"${steps.remove.output}\"}", "remove");
        let mut nested = crate::ritual::parse_file(good).unwrap();
        crate::ritual::prepare(&mut nested, std::path::Path::new("."), &none).unwrap();
        assert_eq!(nested.finally.len(), 1);
        let plain = crate::ritual::parse_file("ritual: r\nsteps: [{id: a, operation: data.trim, arguments: {text: x}}]\n").unwrap();
        assert!(!serde_json::to_string(&plain).unwrap().contains("finally"), "rituals without finally keep their hash");
    }

    #[test]
    fn number_inputs_are_finite_bounded_decimals() {
        let none = |_: &str, _: Option<u32>| None;
        let mut def = crate::ritual::parse_file(r#"
ritual: r
inputs:
  floor: {type: number, min: 0, max: 1, default: 0.3}
steps:
  - id: kept
    operation: data.filter
    arguments: {values: [0.1, 0.5], predicate: "item >= params.floor", params: {floor: "${inputs.floor}"}}
return: {kept: "${steps.kept.output}"}
"#).unwrap();
        crate::ritual::prepare(&mut def, std::path::Path::new("."), &none).unwrap();
        crate::ritual::validate(&def, &none, &crate::reagent::tool, 3).unwrap();
        assert_eq!(def.inputs["floor"].ty, "decimal", "number is the same type as decimal");
        let mut short = crate::ritual::parse_file("ritual: r\ninputs: {floor: number}\nsteps: []\n").unwrap();
        crate::ritual::prepare(&mut short, std::path::Path::new("."), &none).unwrap();
        assert_eq!(short.inputs["floor"].ty, "decimal");
        let given = |v: serde_json::Value| crate::ritual::resolve_inputs(&def, serde_json::json!({"floor": v}).as_object().unwrap());
        assert_eq!(crate::ritual::resolve_inputs(&def, &serde_json::Map::new()).unwrap()["floor"], 0.3);
        assert_eq!(given(serde_json::json!("0.25")).unwrap()["floor"], 0.25, "a --arg string");
        for bad in [serde_json::json!("nan"), serde_json::json!("inf"), serde_json::json!("-Infinity"), serde_json::json!(1.5), serde_json::json!(-0.1), serde_json::json!("x")] {
            assert!(given(bad.clone()).is_err(), "{bad} should be refused");
        }
        for default in [".nan", ".inf", "2"] {
            let text = format!("ritual: r\ninputs:\n  floor: {{type: number, min: 0, max: 1, default: {default}}}\nsteps: []\n");
            let refused = crate::ritual::parse_file(&text).and_then(|mut d| {
                crate::ritual::prepare(&mut d, std::path::Path::new("."), &none)?;
                crate::ritual::validate(&d, &none, &crate::reagent::tool, 3).map_err(|e| e.join("; "))?;
                Ok(())
            });
            assert!(refused.is_err(), "default {default} should be refused");
        }
    }
}
