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
essence: {requests: N, tokens: N, time: DURATION, tool_calls: N, items: N, cleanup_time: DURATION}
drift: {on_mismatch: note | warn | refuse}
steps: [STEP, ...]           # required, at least one
finally: [STEP, ...]         # optional cleanup, runs however `steps` end
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
| `finally` | Cleanup steps that run after `steps`, whatever the outcome. See [Finally](#finally). |
| `return` | Values rendered after the last step. Strings may contain references; they cannot name a `finally` step. |

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
| `type` | `integer`, `decimal` (or `number`: the same type, a finite decimal; `NaN` and infinities are refused), `boolean`, `string`, `list`, or `object` |
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

`reagents.R.select` lists the tools steps may use. `requires` lists the authority the ritual needs and narrows its execution grants. Pure `data` operations need no external-resource grant. If `reagents` omits a reagent already listed in `requires` (or omits `data`), the compiler derives its `select` list automatically from `requires` and `data.*` steps. Each `into` key maps to one value or a list; a list expands into one requirement per value. Scope values may reference inputs (`on: {root: "${inputs.target}"}`); they are rendered with the run's inputs before grants are checked, and the run is narrowed to the rendered value. A `file` or `sight` root taken from an input is usually granted with `--under DIR` ([roots chosen at run time](../concepts/authority.md#roots-chosen-at-run-time)). See [Authority](../concepts/authority.md).

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

**Routing among a fixed set.** Instead of `name`, a step can list the models it may run on and choose one per run, without repeating its prompt in several `if` branches:

```yaml
- id: pick                             # a long file goes to the larger model
  operation: data.select
  arguments: {condition: "${steps.long.output}", then: ollama/qwen3:32b, else: ollama/qwen3:4b}
- id: review
  model:
    one_of: [ollama/qwen3:4b, ollama/qwen3:32b, claude/sonnet]
    choose: "${steps.pick.output}"     # a model id, one of the above
    prompt: "${inputs.code}"
    schema: {type: object, ...}
```

- `one_of` lists 1 to 16 distinct `backend/model` ids; `choose` is a single reference (an input or an earlier step's output) whose value is a string. Validation refuses `choose` without `one_of`, both `one_of` and `name`, and a `choose` that is not one reference or not a string.
- Every member is known before the run: each is a model site for drift fingerprints, rehearsal, assays and pricing, whichever a run picks.
- At run time the value must be exactly one of the members. Anything else, including `null`, makes the step `invalid` before a request is admitted, so nothing is sent and no request is spent.
- `through` / `--model` does not change a routed step; the set is fixed, as a pinned `name` is.
- Records, cassettes and drills see the model that ran, as for any step.

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

Runs the body once per element of a list, binding the element to `VAR`. The loop variable may not be named `inputs`, `steps` or `output`. Ids defined in the body are visible only inside it. Items run one after another unless `parallel: N` (1 to 32) is given, in which case up to N items run at once; results, checkpoints and the output list stay in item order, and essence is reserved atomically across workers (see [Essence](../concepts/essence.md)).

Output: a list of items, each `{item, status, error, outputs, steps, effects, interrupted}`, where `item` is the element's `id` field if it has one, or its index. The step also carries `counts`:

| Count | Meaning |
| --- | --- |
| `selected` | Elements in the list |
| `processed` | Items that finished, in any state |
| `remaining` | Items not completed because the batch stopped; an interrupted item may already have effects |
| `interrupted` | Included partial items that stopped on exhaustion/cancellation |
| `returned`, `abstained`, `invalid`, `failed`, `unverified` | Items by status |
| `applied`, `observed`, `held` | Writes applied, writes observed by the runtime, writes held as petitions |

`keep` lists body steps whose outputs each item keeps; the others are left out of `outputs` (their status still appears under `steps`). Use it when a body reads large data, such as a file's text, that later steps do not need: loop results are data, and data arguments are capped at 1 MiB. A name that is not a step whose result an item records (a `data` operation, model step or ritual step, including inside `if` branches; not an `if` itself, another operation, or a nested loop) is a validation error, and a reference to a dropped output is a type error. `compile`, `inscribe` and `rehearse` (and a run's admission warnings) warn about a loop without `keep` whose body has a step that can return a lot (`file.read_text`, `file.read_json`, `http.get_*`, `data.lines`, `cmd.read` or a model step) when its results are later passed to a `data` operation, and suggest `keep:` with the item outputs that later steps actually read.

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

## Finally

`finally:` lists steps that run once `steps` have ended, however they ended: `returned`, `abstained`, `failed`, `invalid`, `exhausted` or `cancelled`. Use it to undo what the ritual made outside itself, such as a worktree:

```yaml
steps:
  - {id: wt, operation: git.worktree_add, arguments: {repo: target, branch: "${inputs.branch}"}}
  - {id: tested, operation: trial.run, arguments: {repo: target, worktree: "${steps.wt.output.path}"}}
finally:
  - if: steps.wt.status == "returned"
    then:
      - {id: remove, operation: git.remove_worktree, arguments: {repo: target, path: "${steps.wt.output.path}"}}
return: {passed: "${steps.tested.output.passed}"}
```

- **What cleanup can see.** Every main step. A step the run never reached reads as `skipped` with a `null` output, so test `steps.ID.status == "returned"` before using what a step made. Neither `return` nor a main step can name a `finally` step, and ids are unique across both lists.
- **Checked like main steps.** Cleanup steps are validated and typed like any other step. Their effects need `requires` and grants, and they appear in `describe` and `preview`.
- **Its own limits.** A stopped run still cleans up: cleanup has a cancel flag of its own, so a second `stop` does not interrupt it. It gets `essence.cleanup_time` (default `30s`, at most `10m`), counted from when it starts, in place of whatever was left of `time`. It gets a fresh allowance of `tool_calls` and `items`, but only the requests, tokens and cost the main steps left over. What it uses still counts toward the run's totals and its ancestors'.
- **Outcomes.** A cleanup failure turns a `returned` run into that failure, with an error starting `finally:`, and keeps the computed return under `result.return`. Any other outcome is kept as it was: cleanup never hides why a run stopped.
- **Where to see it.** The outcome is recorded in the run's `cleanup` (`{"finally": {"outcome", "error", "ran", "ms"}}`) and in `finally` / `finally.end` events. `summonr graveyard ID` prints it as a `finally` row.

A child ritual's `finally` runs when that child ends, before the parent continues.

## Settings

Settings come from three places, later ones winning: the ritual's `settings`, the invocation's `at k=v` / `--set k=v`, then the step's `settings`.

The Ollama backend accepts:

| Setting | Passed to Ollama as |
| --- | --- |
| `temperature`, `seed`, `top_p`, `top_k`, `num_ctx`, `num_predict` | `options` |
| `think` | top-level `think` (default `false`) |

Unknown settings, invalid types and out-of-range values are rejected during definition validation and for invocation overrides. `num_predict` is further capped by the remaining token essence.

### Thinking models

`think` (a boolean) turns a thinking model's reasoning on or off. Summonr sends `"think": false` to Ollama unless a setting says otherwise, so a thinking model does not spend its generation cap reasoning before it writes the answer. To let it think, give it room as well:

```yaml
settings: {think: true, num_predict: 8192, num_ctx: 16384}
```

`summonr run NAME --set think=false` overrides the ritual for one run, and a step's own `settings` override both. The OpenAI-compatible and Anthropic backends have no thinking switch: they accept `think: false`, which is what they already do, and send nothing for it, and they refuse `think: true` when the step runs rather than drop it silently.

### When generation is truncated

A model step whose backend stops because it ran out of room (Ollama's `done_reason: length`) ends `invalid`, and nothing that depends on it runs. The error says what to change:

```text
step test: backend truncated generation after 512 output tokens (prompt 900 tokens; generation cap of 512 from `num_predict`; num_ctx 16384; thinking on); dependent work refused. ...
```

The output count includes thinking tokens. A cap described as "the token essence left" means the run's `essence.tokens` is what ran out, not the setting. Output well below the cap with a large prompt usually means `num_ctx` filled up.

The `mock` backend does not use sampling settings, but compiled definitions still validate them so a spelling error cannot hide behind a mock run.

## Durations

A whole number followed by `ms`, `s`, `m` or `h`: `500ms`, `30s`, `5m`, `2h`.
