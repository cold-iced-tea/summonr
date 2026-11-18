# 1. Check a manifest, no model

**Idea:** a ritual is a checked, recorded procedure. The first useful one needs no model at all.

A deployment tool receives a service manifest as JSON. Before anything happens, you want to know it is well formed: the required fields are present, `replicas` is sane, and nothing unexpected has crept in. Then you want a short summary.

You could ask a model "is this manifest valid?" That would be slow, cost tokens, and give you an opinion. A schema gives you an answer.

## The ritual

`examples/01-check-manifest/check-manifest.yaml`:

```yaml
language_version: 1
ritual: check-manifest
description: Validate a service manifest and summarize it, with no model.
inputs:
  text: {type: string}
reagents:
  data: {select: [parse_json, project, count]}
essence: {requests: 0, tool_calls: 3, time: 5s}
steps:
  - id: manifest
    operation: data.parse_json
    arguments:
      text: "${inputs.text}"
      schema:
        type: object
        required: [name, version, replicas, dependencies]
        additionalProperties: false
        properties:
          name: {type: string, minLength: 1, maxLength: 63}
          version: {type: string, minLength: 1}
          replicas: {type: integer, minimum: 1, maximum: 50}
          dependencies: {type: array, maxItems: 100, items: {type: string}}
  - id: identity
    operation: data.project
    arguments: {value: "${steps.manifest.output}", fields: [name, version]}
  - id: deps
    operation: data.count
    arguments: {value: "${steps.manifest.output.dependencies}"}
output_schema:
  type: object
  required: [service, dependency_count]
  additionalProperties: false
  properties:
    service: {type: object}
    dependency_count: {type: integer, minimum: 0}
return:
  service: "${steps.identity.output}"
  dependency_count: "${steps.deps.output}"
```

Read it top to bottom:

- **`inputs`** declares exactly what the ritual takes. Anything else is refused.
- **`reagents.data`** selects three pure operations. They cannot touch files, the network, the clock, or a model.
- **`essence: {requests: 0}`** makes it *impossible* for this ritual to call a model, even by mistake.
- **Three steps** parse and validate, pick two fields, and count the dependencies. Each step refers to earlier ones with `${steps.ID.output}`.
- **`output_schema`** is a contract on what the ritual returns.

## Check it before running it

```bash
summonr validate examples/01-check-manifest/check-manifest.yaml
```

This reads and checks the whole definition offline, without starting anything. It prints the compiled contract, including every effect: here, three `pure`, `deterministic` operations and nothing else.

## Run it

```console
$ summonr run examples/01-check-manifest/check-manifest.yaml \
    --arg text="$(cat examples/01-check-manifest/good.json)" --wait
{
  "dependency_count": 3,
  "service": {
    "name": "checkout",
    "version": "2.4.1"
  }
}
* ELEGY AT REST (returned) · 00:00 · 0 requests · 0 tokens
```

Now the bad manifest, which has `replicas: 0` and an extra `debug` field:

```console
$ summonr run examples/01-check-manifest/check-manifest.yaml \
    --arg text="$(cat examples/01-check-manifest/bad.json)" --wait
{
  "partial": {}
}
* ELEGY SPOKE AMISS (invalid) · 00:00 · 0 requests · 0 tokens
  invalid output: parsed: unexpected field debug
```

Exit code 11. Nothing downstream ran. A script can branch on the exit code without parsing any text.

## Why bother with a ritual for this?

You could write this check in any language. What you get here, for free:

- **A record.** `summonr graveyard` lists the run, and `summonr record ID` shows its inputs and result.
- **A contract other programs and agents can read.** `describe_ritual` (chapter 6) tells an agent exactly what this takes and returns, and that it costs no inference.
- **A building block.** In chapter 3 the same kind of deterministic step sits right after a model, checking its work.

## Try it

- Delete `"dependencies"` from `good.json` and run again.
- Change `maximum: 50` to `maximum: 2` and watch `replicas: 3` fail.
- Run `summonr run ... --model mock/keyword`. Nothing changes: there is no model step to override.
