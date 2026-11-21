# Rituals and versions

A ritual is a YAML file that says, completely and explicitly, what a procedure takes, does, may touch, may spend, and returns. Here is the demo's classifier:

```yaml
ritual: classify-message
description: Classify one email into a folder under the bound rules.
inputs:
  text: {type: string}
bindings:
  rules: {document: rules.md}
model: ollama/qwen3:4b
settings: {temperature: 0}
essence: {requests: 1, tokens: 20000, time: 5m}
steps:
  - id: classify
    model:
      instructions: |
        You sort email into folders. Follow the rules below exactly.
        Answer with JSON only: the folder and a reason of one short sentence.
      bind: [rules]
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

The full format is in [Ritual file format](../reference/ritual-format.md). This chapter covers what happens to a ritual between the file and a run.

## Nothing is inferred

Summonr's first rule is that it never guesses:

- Unknown fields in the file are errors (every structure rejects unknown keys).
- Unknown inputs on the command line are errors.
- A reference such as `${input.text}` (missing the `s`) is reported as ambiguous, not resolved to the closest match.
- Granting tools with no tools named is an error: "omission never means all".
- Missing grants, duplicate clauses, and ambiguous references are all errors.

## Validation happens before anything runs

When you inscribe, rehearse, or invoke, the **whole** definition is validated first, including every child ritual it calls. One bad reference at the end of a ritual stops it before step one:

```console
$ summonr inscribe bad.yaml
summonr: bad.yaml is not valid:
  step a prompt: `input.text` is ambiguous; references start with `inputs.`, `steps.`, or a loop item, not `input`
  return `x`: `steps.b.output` names step `b`, which is not defined earlier in scope
```

All errors are reported together, and the exit code is 2.

Validation checks, among other things, that:

- every step is exactly one kind (operation, model, ritual, each, or if);
- every step that produces output has a unique `id`;
- references point at declared inputs, at steps defined *earlier* in scope, or at a loop item (with `"did you mean ...?"` suggestions on typos);
- every tool used is selected under `reagents` *and* declared under `requires`;
- `verify` uses a read tool; `hold` is only on writes;
- a child ritual exists, receives only inputs it declares, gets every input it requires, and needs no authority its parent does not declare;
- there are no cycles, and nesting is within the depth ceiling (default 3);
- durations, input types, and drift policies are well formed.

## Normalization before hashing

Before validation and content hashing, `parse_file` and `parse_definition` normalize authoring shorthands into canonical form:

- **Compact inputs & schemas:** `inputs: {text: string}` expands to `{type: string}`. Compact schemas in `output_schema`, `model.schema`, `inputs.*.schema`, and `data.parse_json` `schema` expand scalar names (`string`, `integer`, `list<T>`), enum arrays, and closed object maps (`field?: type`) into standard JSON Schema.
- **Auto-derived `reagents`:** When `reagents` omits a reagent already listed in `requires` (or omits `data`), `reagents.<name>.select` is populated automatically. Explicit `reagents` entries remain authoritative for tool narrowing.
- **Concise references & guards:** `$inputs.x` and `$steps.foo.bar` expand to `"${inputs.x}"` and `"${steps.foo.output.bar}"`; `assert:` and `fail:` desugar into guarded `data.assert` steps. Because canonical definitions remain untouched, existing verbose rituals keep identical `content_hash` values.

## Inscription freezes a ritual

Inscribing does two things beyond validation.

**Bound documents are embedded.** `bindings.rules: {document: rules.md}` reads `rules.md` (relative to the ritual file) and stores its content and SHA-256 hash inside the saved definition. Editing `rules.md` later does not change version 1. The file path is kept only as provenance.

**Child rituals are pinned.** A step `ritual: classify-message` is saved with the exact version and content hash it resolved to. If that version's content ever differs at run time, the step fails rather than running something else.

The saved definition then has its own content hash.

## Versions

| Situation | What `inscribe` does |
| --- | --- |
| New name | Saves version 1 |
| Same name, identical content | Reports it unchanged; no new version |
| Same name, different content | Refuses (exit 5), unless you add `replacement` / `--replace` |
| With `replacement` | Saves the next version |

Use `as NAME` / `--name NAME` to inscribe a file under a different name:

```bash
summonr inscribe ./r.yaml as inbox-rite replacement
```

Invocations run the latest version unless pinned with `version N`. Baselines are recorded per version, so a new version starts without one.

## Running a file directly

`invoke` also accepts a path to a ritual file: any target containing `/` or ending in `.yaml` or `.yml`. The file is validated and run as version 0 without being saved:

```bash
summonr run tests/fixtures/rituals/classify-message.yaml --arg text="Your receipt for order 12" --model mock/keyword --wait --json
```

Over MCP, an agent can only run saved rituals by name, never a file. See [Calling rituals from another agent](../guides/agents.md).

## Reading the grimoire

```bash
summonr grimoire                    # every saved ritual, latest version
summonr grimoire read inbox-triage  # one definition, its grants and baseline
```
