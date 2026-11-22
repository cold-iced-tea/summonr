# Ritual file format

A ritual is a YAML document. Every structure rejects unknown fields.

## Top level

```yaml
language_version: 1         # optional; only version 1 is supported
ritual: NAME                 # required
description: TEXT
inputs: {NAME: INPUT, ...}
bindings: {NAME: BINDING, ...}
model: BACKEND/MODEL
settings: {KEY: VALUE, ...}
reagents: {REAGENT: {select: [TOOL, ...], version: N}, ...}
requires: [REQUIREMENT, ...]
essence: {requests: N, tokens: N, time: DURATION, tool_calls: N, items: N}
drift: {on_mismatch: note | warn | refuse}
steps: [STEP, ...]           # required, at least one
output_schema: {type: object, ...}   # optional final return contract
return: {NAME: VALUE, ...}
```

| Field | Notes |
| --- | --- |
| `language_version` | Language semantics version, default 1; separate from inscription version. |
| `output_schema` | Optional schema checked against the final return value. |
| `ritual` | Letters, digits, `-` and `_` only. |
| `description` | Shown by `grimoire` and to agents. |
| `model` | Default model for model steps. `ollama/<name>` or `mock/keyword`. |
| `settings` | Default model settings. See [Settings](#settings). |
| `essence` | All fields optional; an omitted field is unlimited. See [Essence](../concepts/essence.md). |
| `drift.on_mismatch` | What an invocation does when the backend fingerprint differs from the baseline. Default `warn`. |
| `return` | Values rendered after the last step. Strings may contain references. |

## Inputs

```yaml
inputs:
  count: {type: integer, default: 6, min: 1, max: 500}
  mode:  {type: string, enum: [fast, careful], default: careful}
  hold:  {type: boolean, default: true, user_only: true}
  text:  string   # shorthand for {type: string}
  tags:  [string] # or array<string> / list<string>
  user:
    name: string
    tags: [string]
```

| Field | Meaning |
| --- | --- |
| `type` | `integer`, `decimal`, `boolean`, `string`, `list`, or `object` |
| `default` | Used when the input is not supplied. An input without a default is required. |
| `min`, `max` | Numeric range, inclusive |
| `enum` | Allowed values |
| `user_only` | Only the user, not a calling agent, may change it from its default. Requires a default. |

Scalar type names, `array<T>` / `list<T>` (or `[T]`), and nested object maps (`{name: string, tags: [string]}`) may be used directly as input shorthands. Values given on the command line are strings and are converted to the declared type: `"6"` → `6`, `"true"` → `true`. A `list` or `object` is given as JSON: `--arg tags='["a","b"]'`. Anything that does not convert, any unknown input, and any missing required input is an error.

## Bindings

```yaml
bindings:
  rules: {document: rules.md}       # path relative to the ritual file
  tone:  {content: "Be brief."}     # or inline content
```

At inscription, each document is read, embedded as `content`, and hashed. A saved ritual never depends on the file again.

## Reagents and requirements

```yaml
reagents:
  mail: {select: [search, read, move, locate]}
requires:
  - {reagent: mail, action: search, on: {folder: Inbox}}
  - {reagent: mail, action: move, on: {folder: Inbox}, into: {folder: [Receipts, Action]}}
  - {reagent: mail, action: locate}
```

`reagents.R.select` lists the tools steps may use. `requires` lists the authority the ritual needs and narrows its execution grants. Pure `data` operations need no external-resource grant. If `reagents` omits a reagent already listed in `requires` (or omits `data`), the compiler derives its `select` list automatically from `requires` and `data.*` steps. Each `into` key maps to one value or a list; a list expands into one requirement per value. See [Authority](../concepts/authority.md).

## Ergonomic shorthands

At parse time, definitions normalize into canonical form before hashing and validation:

- **References:** `$inputs.count`, `$steps.classify.folder`, and `$message.id` expand to `"${inputs.count}"`, `"${steps.classify.output.folder}"`, and `"${message.id}"`. Inside expressions and templates, `steps.<id>` and `steps.<id>.<field>` (for fields other than `output`, `status`, `kind`, `counts`) expand to `steps.<id>.output` / `steps.<id>.output.<field>`.
- **Assertions and failures:** `- assert: <expr>` (with optional `message:`) and `- fail: <message>` desugar to guarded `data.assert` steps.
- **Compact schemas:** In `output_schema`, `model.schema`, `inputs.*.schema`, and `data.parse_json` `schema`, you can write scalar names (`string`, `integer`, `number`, `boolean`, `array<T>`, `list<T>`), enum arrays (`[Receipts, Action]`), or closed object maps (`{folder: [Receipts, Action], note?: string}` where `?` marks optional fields).
- **Schema reuse (`defs` / `$defs`):** Define shared subschemas at the top level under `defs:` (or `$defs:`) or inside a schema's `$defs`, and reference them via `{"$ref": "#/$defs/<name>"}`. References are validated for cycles and inlined during preparation.

## Steps

A step is exactly one of five kinds, chosen by which key it has. Modifiers for a different step kind are rejected. Every step except `if` needs an `id` that is unique in its scope.

### Operation

```yaml
- id: move
  operation: mail.move                 # reagent.tool
  arguments: {message: "${message.id}", folder: "${steps.classify.output.folder}"}
  hold: "${inputs.hold}"               # true, false, or a reference to a boolean
  verify:
    observe: mail.locate               # a read tool, selected and required
    arguments: {message: "${message.id}"}
    expect: {folder: "${steps.classify.output.folder}"}   # at least one field
```

`hold` is only allowed on writes. Output: the tool's result, or `{held: true, petition: ID}` when held.

### Model

```yaml
- id: classify
  model:
    name: ollama/qwen3:4b              # optional: pins this step's model
    settings: {temperature: 0}         # optional: step settings
    instructions: "You sort email..."  # system message
    bind: [rules]                      # bindings appended to the system message
    prompt: "${inputs.text}"           # required: the user message
    images: ["${steps.page.output}"]   # optional: handles from sight.read_image
    schema: {type: object, ...}        # optional: output contract
    abstain_if: output.folder == "abstain"
```

**Which model runs:** the step's `name` if set; otherwise the invocation's `through` / `--model`; otherwise the ritual's `model`. A pinned step model wins over an invocation override.

**Schema** supports the strict [documented subset](../guides/validation.md), including closed objects and numeric/length bounds. Unsupported keywords fail validation instead of being ignored. With a schema, output must be JSON that matches it, or the step is `invalid`. Without one, output is the raw string.

**`abstain_if`** is a [condition](expressions.md) that may refer to the step's own output as `output.<field>`. When true, the step is marked abstained and writes in that execution scope are refused. Reads and pure computations may still run; abstention is not an implicit retry or fallback.

**`images`** is a list of references (or one reference to a list), at most 8 per step, each resolving to a handle issued by [`sight.read_image`](sight.md) in the same run tree. Handle-shaped objects from inputs, MCP arguments or model output are denied. Before any request is sent, the grant is re-checked and the file re-read; a changed or missing file fails the step as a stale image. Images go before the prompt text in the user message. The mock backend accepts images; an Ollama model must report `vision` (checked once, skipped in drills and replays). Recorded payloads replace image data with a digest placeholder.

Each model step is a fresh context: system message (instructions + bindings) and one user message (the prompt, preceded by any images).

### Ritual

```yaml
- id: classify
  ritual: classify-message
  version: 1                           # optional; pinned at inscription either way
  arguments: {text: "${steps.body.output.text}"}
```

Runs a saved ritual as a linked child summon. Arguments must be inputs the child declares; every required child input must be given. Output: the child's return value. The child's abstention marks this step abstained. The child must not require authority the parent does not declare. Nesting depth is limited (default 3) and cycles are rejected.

### Each

```yaml
- id: triage
  each: message in steps.select.output   # VAR in REFERENCE
  on_failure: continue                    # or stop (default)
  keep: [classify]                        # optional: item outputs to keep
  steps: [...]
```

Runs the body once per element of a list, sequentially, binding the element to `VAR`. The loop variable may not be named `inputs`, `steps` or `output`. Ids defined in the body are visible only inside it. `parallel` greater than 1 is rejected.

Output: a list of items, each `{item, status, error, outputs, steps, effects, interrupted}`, where `item` is the element's `id` field if it has one, or its index. The step also carries `counts`:

| Count | Meaning |
| --- | --- |
| `selected` | Elements in the list |
| `processed` | Items that finished, in any state |
| `remaining` | Items not completed because the batch stopped; an interrupted item may already have effects |
| `interrupted` | Included partial items that stopped on exhaustion/cancellation |
| `returned`, `abstained`, `invalid`, `failed`, `unverified` | Items by status |
| `applied`, `observed`, `held` | Writes applied, writes observed by the runtime, writes held as petitions |

`keep` lists body steps whose outputs each item keeps; the others are left out of `outputs` (their status still appears under `steps`). Use it when a body reads large data, such as a file's text, that later steps do not need: loop results are data, and data arguments are capped at 1 MiB. A name that is not a result-producing step of the body is a validation error, and a reference to a dropped output is a type error.

With `on_failure: stop`, the first failed item ends the batch. With `continue`, it is recorded and the batch goes on. Exhaustion and cancellation always stop.

### If

```yaml
- if: inputs.apply and steps.classify.output.folder != "abstain"
  then: [...]                            # required
  else: [...]                            # optional
```

Ids in either branch are visible after the `if`. Steps in the branch not taken are recorded as `skipped` with a `null` output, so later references to them see `null`.

### Retry and fallback

Any step may carry a bounded `retry` count (`1..=5`) and/or a `fallback` branch:

```yaml
- id: fetch
  operation: http.get
  arguments: {url: "${inputs.url}"}
  retry: 2
  fallback:
    - id: cached
      operation: data.value
      arguments: {value: null}
```

`retry` must be in `1..=5`; values outside that range are rejected at validation. If the step still fails after exhausting retries, `fallback` steps run in order and the last fallback step's output becomes the step's output. Exhaustion, cancellation, and abstention are not retried.

## Settings

Settings come from three places, later ones winning: the ritual's `settings`, the invocation's `at k=v` / `--set k=v`, then the step's `settings`.

The Ollama backend accepts:

| Setting | Passed to Ollama as |
| --- | --- |
| `temperature`, `seed`, `top_p`, `top_k`, `num_ctx`, `num_predict` | `options` |
| `think` | top-level `think` (default `false`) |

Unknown settings, invalid types and out-of-range values are rejected during definition validation and for invocation overrides. `num_predict` is further capped by the remaining token essence.

The `mock` backend does not use sampling settings, but compiled definitions still validate them so a spelling error cannot hide behind a mock run.

## Durations

A whole number followed by `ms`, `s`, `m` or `h`: `500ms`, `30s`, `5m`, `2h`.
