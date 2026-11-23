# Pure data operations

The `data` reagent contains deterministic functions over explicit arguments. It has no ambient access to files, network, time, randomness, or models. Declare selected operations under `reagents.data.select`; pure operations need no external-resource grant. They consume `essence.tool_calls`, not model-request or token essence. Collection operators also consume `essence.items`. All data arguments must fit within 1 MiB of encoded JSON. New standard-library results have the same byte cap; arrays are bounded to 10,000 elements.

| Operation | Arguments | Output and failure behavior |
| --- | --- | --- |
| `data.parse_json` | `text`, optional `schema` | Parsed JSON; malformed JSON or schema mismatch fails. No markdown stripping, guessing, or repair. Text limit: 1,048,576 Unicode scalar values. |
| `data.project` | object `value`, string array `fields` | Object containing exactly those fields. Missing or duplicate fields fail. At most 10,000 fields. |
| `data.count` | `value` | Array length, object key count, or string Unicode scalar count. Other types fail. |
| `data.sum_integers` | integer array `values` | Signed 64-bit sum. Overflow, out-of-range integers, and non-integers fail. At most 10,000 entries. |
| `data.assert` | boolean `condition`, optional `message` | `{passed: true}`; a false condition fails. Message limit: 1,024 Unicode scalar values. |

| `data.arithmetic` | signed integers `left`, `right`; `operator`: `add`, `subtract`, `multiply`, `divide`, `remainder` | Checked signed 64-bit result. Division truncates toward zero; remainder has the dividend's sign. Overflow and division by zero fail. |
| `data.compare` | `left`, `right`; `operator`: `eq`, `ne`, `lt`, `le`, `gt`, `ge` | Boolean. Ordering accepts two integers, two floating-point values, or two strings; mixed categories fail. Integer comparisons are exact. |
| `data.get` | `value`, JSON-pointer `path`, optional `default` | Selected value. Missing path fails unless a default is supplied; present null remains null. Empty pointer selects the whole value; `/a~1b` selects key `a/b`. |
| `data.keys` | object `value` | Sorted key array. |
| `data.concat` | arrays `left`, `right` | Combined array, at most 10,000 elements. |
| `data.slice` | array `values`, nonnegative `start`, exclusive `end` | Slice; reversed or out-of-range bounds fail rather than clamp. |
| `data.sort` | array `values` | Stable ascending order; all elements must share an ordered category. String order is lexical, not locale-aware. |
| `data.unique` | array `values` | First occurrence of each distinct JSON value, preserving input order. |
| `data.trim` | `text` | Unicode whitespace removed from both ends. |
| `data.split` | `text`, nonempty literal `separator` | String array, retaining empty segments. No regular expressions. |
| `data.join` | string array `values`, `separator` | Joined string; no implicit conversion. |
| `data.replace` | `text`, nonempty literal `from`, `to` | All non-overlapping occurrences replaced. Expansion is bounded before allocation. |
| `data.map` | array `values`, literal `operation`, object `arguments` | Array of pure operation results. |
| `data.filter` | array `values`, literal `predicate`, optional `params` object | Elements whose predicate returns true; non-booleans fail. |
| `data.fold` | array `values`, explicit `initial`, literal `operation`, object `arguments` | Final accumulator; empty array returns the initial value. |
| `data.all` | boolean array `values` | `true` only when every element is `true` (empty array returns `true`). |
| `data.any` | boolean array `values` | `true` when at least one element is `true` (empty array returns `false`). |
| `data.logic` | `operator` (`and`, `or`, `xor`, `not`), boolean `left`, optional boolean `right` | Boolean result; `not` refuses `right`. |
| `data.select` | boolean `condition`, `then`, `else` | Returns `then` when `condition` is `true`, otherwise `else`. |
| `data.min` / `data.max` | non-empty homogeneous array `values` | Minimum or maximum element using typed ordering; empty or mixed-type arrays fail. |
| `data.clamp` | signed integers `value`, `min`, `max` | Clamps `value` into `[min, max]`; `min > max` fails. |
| `data.abs` | signed integer `value` | Checked absolute value; `i64::MIN` overflow fails. |
| `data.merge` | objects `left`, `right`, optional `conflict` (`right`, `left`, `error`) | Merged object; `conflict: error` fails when a key exists in both. |
| `data.set` | object `value`, non-empty `field`, `item` | Copy of `value` with `field` set to `item`. |
| `data.omit` | object `value`, string array `fields` | Copy of `value` with the named fields removed. |
| `data.entries` | object `value` | Sorted array of `{key, value}` objects. |
| `data.from_entries` | array `entries` of `{key, value}` objects | Object constructed from entries; duplicate keys fail. |
| `data.sha256` | `text` | 64-character lowercase hex SHA-256 of the UTF-8 bytes. |
| `data.canonical_json` | `value` | Canonical sorted-key JSON string without whitespace. |
| `data.contains` | string or array `value`, `needle` | Substring check on strings (with string `needle`), or element equality check on arrays. |
| `data.starts_with` / `data.ends_with` | `text`, `prefix` / `suffix` | Boolean prefix or suffix check. |
| `data.case` | `text`, `mode` (`lower`, `upper`) | Unicode lowercase or uppercase string. |
| `data.lines` | `text`, optional integer `number_from` | Array of lines (`\n` or `\r\n`), up to 10,000 lines. With `number_from`, each line is prefixed `N: ` counting from that number, so a model can cite lines. |
| `data.coalesce` | array `values` | First element that is not `null`, or `null` (so its type is nullable). Picks the output of whichever `if` branch ran. |
| `data.unique_by` | array `values` of objects, string array `fields` (1–100) | The first object for each distinct combination of those fields' values, in input order. A non-object or a missing field fails. Equality as in `data.unique`. |
| `data.flatten` | array `values` of arrays | The inner arrays concatenated, one level deep; any non-array element fails. At most 10,000 elements. |
| `data.range` | integers `start`, `end`, optional `step` (≥ 1, default 1) | Integers from `start` up to but not including `end`; empty when `start ≥ end`. More than 10,000 elements fails. |
| `data.index_of` | array `values`, `needle`, optional `match` (`equal` default, or `contains`) | Index of the first element equal to `needle` (or, with `contains`, the first string containing the string `needle`), else `null`. Equality is the same as `data.contains` and `data.unique` (JSON values, so `1.0` is not `1`), not `==` in conditions. |

`data.assert.condition` is a boolean value or a reference to one; it does not evaluate an expression string. Use `if` for expression-based control flow. All operations reject unknown arguments. Input schema checks apply before execution; the optional schema passed to `parse_json` uses the [supported subset](../guides/validation.md).

Pure results carry the evidence class `computed by the runtime`. This describes their derivation; it does not establish that the input is true or the business rule appropriate. `verify` observations still use read tools: a pure assertion is not an external-state observation.

## A receipt total without inference

`tests/fixtures/rituals/total-receipt.yaml` parses an object containing `amounts_minor`, requires nonnegative integers, and computes the total. It declares:

```yaml
reagents:
  data: {select: [parse_json, sum_integers]}
essence: {requests: 0, tool_calls: 2, time: 5s}
```

Run it in an isolated home:

```bash
export SUMMONR_HOME=/tmp/my-summonr-demo
summonr compile tests/fixtures/rituals/total-receipt.yaml --json
summonr run tests/fixtures/rituals/total-receipt.yaml \
  --arg 'text={"amounts_minor":[100,25]}' --wait --json
summonr core stop --json
```

The return value is `{"total_minor":125}` and the recorded model-request count is zero. Missing fields, negative amounts, malformed JSON, and overflow fail explicitly. Use a fresh temporary home; do not reuse another person's state directory.

Unstructured receipt interpretation would be a separate judgment step. If a model extracts candidate amounts from text, pass the resulting structured values through the same deterministic validation and arithmetic. The sum is computed exactly; the extraction remains a model claim until supported by additional evidence.

## Bounded collection composition

```yaml
reagents:
  data: {select: [map, filter, fold, arithmetic]}
essence: {requests: 0, items: 100, tool_calls: 100, time: 5s}
steps:
  - id: doubled
    operation: data.map
    arguments:
      values: [1, 2, 3]
      operation: data.arithmetic
      arguments: {left: "${item}", right: 2, operator: multiply}
  - id: kept
    operation: data.filter
    arguments: {values: "${steps.doubled.output}", predicate: "item > 2"}
  - id: total
    operation: data.fold
    arguments:
      values: "${steps.kept.output}"
      initial: 0
      operation: data.arithmetic
      arguments: {left: "${acc}", right: "${item}", operator: add}
```

Body templates have their own scope: `item`, zero-based `index`, and, for fold,
`acc`. They do not capture outer `inputs` or `steps`; supply literal constants or
include necessary data in each item. Filter predicates can use `item` and `index`
with the [condition language](expressions.md), and cannot contain `${...}` at
all, even inside a quoted string: a value pasted into a predicate's text could
change what it means. To compare items with outside values, pass them in
`params`; each entry is rendered like any argument and bound as a value, read in
the predicate as `params.NAME`, so a string that looks like predicate source is
only ever compared as a string:

```yaml
  - id: confident
    operation: data.filter
    arguments:
      values: "${steps.findings.output}"
      predicate: "item.confidence >= params.floor and item.file == params.file"
      params: {floor: "${inputs.floor}", file: "${inputs.file}"}
```

Param names are plain identifiers, and a predicate may read only the params its
step declares. Body operations must be literal
`data.*` primitive names and must also be selected under `reagents.data.select`.
Nested collection operators, I/O, model calls, and child rituals are refused.

Each visited element consumes one item. The collection consumes one tool call;
map/fold additionally consume one tool call per visited primitive invocation.
Deadline, cancellation, and ancestor budgets are checked between elements. Errors
stop the collection; no partial array or accumulator is returned as a successful
result. Use `each` for workflows needing per-item records and partial results.
