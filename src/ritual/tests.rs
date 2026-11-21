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
