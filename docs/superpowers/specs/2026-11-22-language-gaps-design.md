# Small language gaps from bughunt

Date: 2026-11-22. Status: approved in conversation (the "small, additive"
batch from `examples/10-bughunt/LIMITS.md` on `feat/bughunt`).

## Intent

Building bughunt in plain Summonr exposed gaps. This batch closes the ones
that are additive and need no design decision: new pure `data` operations,
one loop option, one CLI clause, and one bug. The larger items (roots as
inputs, `file.walk`, parallel `each`, model choice by expression, predicate
parameters, Codex backend, variable rite stages) are out of scope; each gets
its own conversation.

Non-goals: no change to existing semantics; no authority changes; nothing
that makes a deterministic step depend on ambient state.

## Approaches

Each gap could be closed as a new pure operation (chosen: additive, typed,
documented in one table, follows the stdlib pattern) or as new expression
syntax (rejected: conditions are deliberately small, and new syntax widens
the parser and injection surface).

## Design (calls are the implementer's, marked *call*)

| Gap (LIMITS #) | Change |
| --- | --- |
| 3 coalesce | `data.coalesce {values}` → first non-null element, or `null` if none. |
| 1 flatten | `data.flatten {values}` → one level; every element must be an array; result ≤ 10,000. |
| 5 range | `data.range {start, end, step?=1}` → integers `start ≤ n < end`; `step ≥ 1`; at most 10,000 elements; `start ≥ end` → `[]`. |
| 6 locate | `data.index_of {values, needle, match?: equal\|contains}` → index of the first element equal to (or, for `contains`, a string containing) the needle, else `null`. *call:* `null` not `-1`, matching "missing stays null". |
| 6 numbering | `data.lines` gains optional `number_from: integer`: each line becomes `"N: text"`. *call:* `": "` separator. |
| 7 loop size | `each` gains optional `keep: [step ids]`: each item's `outputs` holds only those steps. Unknown ids are a validation error; references to dropped outputs are type errors. |
| 11 CLI files | New clause pair `reading k=PATH` / `--arg-file k=PATH`: the file's text becomes input `k`'s value, converted like any `given` value. Same key in `given` and `reading` is an error. Files over 1 MiB are refused. Read by the CLI before the request is sent. |
| 13 bug | `git.worktree_add` explains a missing `<remote>/<trunk>` instead of passing git's `rev-list` usage text through. |

Every new operation: schema in `stdlib.rs`, `ToolSpec` in `reagent.rs`,
static output type in `ritual/types.rs`, unit tests, a row in
`book/src/reference/data.md`. The clause gets a paired test in
`src/grammar.rs`, `cli.md` row. `keep` goes in `ritual-format.md` and the
JSON schema. CHANGELOG entry.

## Follow-up

Rewrite bughunt on these features, compare its size, rerun the benchmark.
