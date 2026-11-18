# Tutorial: Composing a ritual

A Summonr ritual composes **deterministic steps** (fetching, parsing, math, verification) with **bounded model steps** (classification, extraction, drafting) in one validated file.

This walkthrough builds a small ritual that:
1. Takes structured input (`category` and `message`),
2. Branches deterministically (`if`) so known categories never call a model,
3. Summons a model *only* when the category is ambiguous, with a strict schema and `abstain_if`,
4. Compiles, drills offline, and inscribes into your grimoire.

## 1. Declare inputs, bindings, and essence

Every ritual starts with typed `inputs`, optional pinned `bindings`, a `model` (if any step uses inference), and an `essence` budget:

```yaml
language_version: 1
ritual: triage-note
description: Route a note deterministically when tagged, or summon a model when marked `other`.
inputs:
  category:
    type: string
    enum: [support, billing, other]
  message:
    type: string
model: mock/keyword
settings: {temperature: 0}
essence:
  requests: 1
  tokens: 8000
  time: 30s
```

Notice `requests: 1`—even if the ritual is expanded later, the runtime guarantees at most one model request per run.

## 2. Branch between deterministic code and inference

Use `if / else` so clear cases stay pure and only `other` reaches a `model` step:

```yaml
steps:
  - if: inputs.category != "other"
    then:
      - id: direct
        operation: data.identity
        arguments:
          value:
            route: "${inputs.category}"
            source: "form"
    else:
      - id: classify
        model:
          instructions: |
            Classify the message into support, billing, or abstain.
            Answer with JSON only.
          prompt: "${inputs.message}"
          schema:
            type: object
            properties:
              route: {type: string, enum: [support, billing, abstain]}
              reason: {type: string}
            required: [route, reason]
            additionalProperties: false
          abstain_if: output.route == "abstain"
return:
  route: "${steps.direct.output.route ?? steps.classify.output.route}"
  source: "${steps.direct.output.source ?? 'model'}"
```

Before writing external side effects (such as `file.write_text`, `file.delete`, `mail.move`, or `git.commit`), add:
- `reagents:` declaring which adapters/tools are used,
- `requires:` declaring exact resource scopes,
- `hold: true` (or `hold: "${inputs.hold}"` with a `user_only: true` input) so writes wait as **petitions**, and
- `verify:` to read state back after execution.

See [`tests/fixtures/rituals/inbox-triage.yaml`](../../../tests/fixtures/rituals/inbox-triage.yaml) for a full example using `each`, a child `ritual:` step, `hold`, and `verify`.

## 3. Compile offline first

Type-check references, schemas, and effect plans before running anything:

```bash
summonr compile examples/04-route-form/route-form.yaml --json
```

## 4. Test with a scripted drill

Write deterministic cases in a `.drill.yaml` file (scripted model answers, simulated writes) and run them:

```bash
summonr inscribe examples/04-route-form/route-form.yaml
summonr drill route-form --against examples/04-route-form/route-form.drill.yaml
```

If any case fails or abstains unexpectedly, `drill` exits `14`.

## 5. Run and inspect

Preview effects and authority with `rehearse`, then run and inspect the full elegy:

```bash
summonr rehearse route-form \
  --arg 'form={"category":"other","message":"Invoice total looks wrong"}'
summonr run route-form \
  --arg 'form={"category":"other","message":"Invoice total looks wrong"}' --wait --json
```

From there you can hand the saved ritual to Claude or Codex via `summonr familiar allow NAME route-form` and `summonr mcp --name NAME` (see [Calling rituals from another agent](agents.md)).
