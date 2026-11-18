# Writing a ritual

This guide builds a ritual in stages, from one model step to a guarded batch, explaining each decision. The exact syntax of every field is in the [Ritual file format](../reference/ritual-format.md).

## Stage 1: one model step

Start with the smallest useful thing: a classifier that takes text and returns a label.

```yaml
ritual: classify-message
description: Classify one email into a folder.
inputs:
  text: {type: string}
model: ollama/qwen3:4b
settings: {temperature: 0}
essence: {requests: 1, tokens: 20000, time: 5m}
steps:
  - id: classify
    model:
      instructions: |
        You sort email into folders. Answer with JSON only.
      prompt: "${inputs.text}"
      schema:
        type: object
        properties:
          folder: {type: string, enum: [Receipts, Newsletters, Action, abstain]}
          reason: {type: string}
        required: [folder, reason]
      abstain_if: output.folder == "abstain"
return:
  folder: "${steps.classify.output.folder}"
  reason: "${steps.classify.output.reason}"
```

Decisions worth making on purpose:

- **Give it a schema.** Without one, the output is a raw string and the effect plan says *"no structure declared"*. With an `enum`, the model can only answer from your list.
- **Give it a way to decline.** Include an `abstain` option and an `abstain_if`. A model that is allowed to say "I don't know" makes fewer confident mistakes, and abstentions are counted separately in assays.
- **Set essence.** One request is the honest budget for one classification.
- **Pin determinism where you can.** `temperature: 0` (and `seed`, if your model supports it) makes runs easier to compare.

Try it without saving:

```bash
summonr run ./classify-message.yaml --arg text="Your order has shipped" --model mock/keyword --wait
```

## Stage 2: bind the rules

Long instructions belong in a document, not in YAML. A **binding** is embedded into the ritual when it is inscribed:

```yaml
bindings:
  rules: {document: rules.md}
steps:
  - id: classify
    model:
      instructions: |
        You sort email into folders. Follow the rules below exactly.
      bind: [rules]
      prompt: "${inputs.text}"
```

The system message becomes the instructions, then `## rules`, then the document's content. Stable content comes first, so a backend can reuse its work on the shared prefix across calls.

Because the content is embedded and hashed, editing `rules.md` later has no effect until you inscribe a new version. The rules a run used are always recoverable from its record.

Inscribe it:

```bash
summonr inscribe classify-message.yaml
```

## Stage 3: a batch with tools

Now a ritual that finds messages and classifies each one. It needs the `mail` reagent.

```yaml
ritual: inbox-triage
description: Classify the newest inbox messages.
inputs:
  count: {type: integer, default: 6, min: 1, max: 500}
reagents:
  mail: {select: [search, read]}
requires:
  - {reagent: mail, action: search, on: {folder: Inbox}}
  - {reagent: mail, action: read, on: {folder: Inbox}}
essence: {requests: 60, tokens: 400000, time: 30m}
steps:
  - id: select
    operation: mail.search
    arguments: {folder: Inbox, newest: "${inputs.count}"}
  - id: triage
    each: message in steps.select.output
    on_failure: continue
    steps:
      - id: body
        operation: mail.read
        arguments: {message: "${message.id}"}
      - id: classify
        ritual: classify-message
        arguments: {text: "${steps.body.output.text}"}
return:
  triage: "${steps.triage.output}"
```

Things to notice:

- **`reagents` and `requires` both list each tool.** `reagents.select` says which tools the ritual uses; `requires` says which scopes it needs. Validation fails if a step uses a tool missing from either.
- **`"${inputs.count}"` stays an integer.** A string that is exactly one reference keeps the referenced value's type.
- **`each` is bounded by its source.** It iterates over a list that already exists. Here the list is bounded by `max: 500` on `count`.
- **`on_failure: continue`** records a failed item and moves on. The default, `stop`, ends the batch at the first failure. Running out of essence or being cancelled always stops.
- **The child ritual runs as its own summon,** in a fresh context, with no more authority than this ritual declares, drawing on this ritual's essence.
- **The batch result** is a list of items with per-item status, outputs and effects, plus counts. See [Essence](../concepts/essence.md#exhaustion-in-a-batch).

## Stage 4: guarded writes

Finally, let it move mail, but only when asked, only after approval, and only with proof.

```yaml
inputs:
  count: {type: integer, default: 6, min: 1, max: 500}
  apply: {type: boolean, default: false}
  hold: {type: boolean, default: true, user_only: true}
reagents:
  mail: {select: [search, read, move, locate]}
requires:
  - {reagent: mail, action: search, on: {folder: Inbox}}
  - {reagent: mail, action: read, on: {folder: Inbox}}
  - {reagent: mail, action: move, on: {folder: Inbox}, into: {folder: [Receipts, Newsletters, Action]}}
  - {reagent: mail, action: locate}
steps:
  # ... select, body, classify as before, inside the each ...
      - if: inputs.apply and steps.classify.output.folder != "abstain"
        then:
          - id: move
            operation: mail.move
            arguments: {message: "${message.id}", folder: "${steps.classify.output.folder}"}
            hold: "${inputs.hold}"
            verify:
              observe: mail.locate
              arguments: {message: "${message.id}"}
              expect: {folder: "${steps.classify.output.folder}"}
```

Each guard does one job:

| Guard | What it prevents |
| --- | --- |
| `apply` defaults to `false` | Accidental writes. A plain run is read-only. |
| `if ... != "abstain"` | Acting on a non-decision. |
| `into: {folder: [...]}` | Moves to any folder you did not list and grant. |
| `hold: "${inputs.hold}"` | Writes without a person seeing them first. |
| `user_only: true` on `hold` | An agent switching the hold off. |
| `verify` | Counting a write as done when it is not. |

The `if` step has no `id`; only steps that produce output need one. Steps inside a branch that is not taken are recorded as `skipped` with a `null` output.

## Authoring shorthands

Several shorthands keep definitions terse without changing the compiled contract:

- **Typed inputs:** Write `inputs: {name: string}` or nested objects and lists such as `inputs: {user: {name: string, tags: [string]}}` (or `array<T>` / `list<T>`).
- **Auto-derived `reagents`:** Omit `reagents` when `requires` or `data.*` steps already name the tools; the compiler derives `select` lists automatically.
- **References:** `$inputs.count` and `$steps.classify.folder` expand to `"${inputs.count}"` and `"${steps.classify.output.folder}"`. Inside expressions and templates, `steps.<id>.<field>` (outside `output`, `status`, `kind`, `counts`) reads `steps.<id>.output.<field>`.
- **Assertions and failures:** `- assert: <expr>` (with optional `message:`) and `- fail: <message>` desugar to guarded `data.assert` steps.
- **Retry and fallback:** Add `retry: N` (`1..=5`) and/or `fallback: [...]` to any step for bounded recovery on failure.

## Checklist before inscribing

1. `summonr rehearse NAME given ...` shows the effect plan and any missing authority. Read the effects: is every write verified or held?
2. Run it against `mock/keyword` to exercise the mechanics.
3. Build an assay set with real, labeled cases, including ones that should abstain and at least one hostile one. Record a baseline with a real model.
4. Grant the narrowest scopes that work.

## Common validation errors

| Message | Fix |
| --- | --- |
| `` `input.text` is ambiguous `` | References start with `inputs.`, `steps.`, or a loop variable. |
| `names step `x`, which is not defined earlier in scope` | Steps can only refer to earlier steps, in the same or an enclosing scope. Loop-body ids are not visible outside the loop. |
| `needs an `id` so its output can be referenced` | Every step except `if` needs an `id`. |
| `tool `move` is not selected from reagent `mail`` | Add it to `reagents.mail.select`. |
| `no entry under `requires` declares mail.move` | Add a `requires` entry for it. |
| `` `hold` applies to writes `` | Only writes can be held. |
| `requires mail.move ..., which this ritual does not declare` | A child needs authority its parent does not declare. Add it to the parent's `requires`. |
| `` `user_only` needs a default `` | Give the input a default so agents can run the ritual. |
| `` `parallel` above 1 is not supported `` | Iteration is sequential in this version. |
