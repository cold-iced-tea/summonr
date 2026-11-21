# The standard library

The standard library is built into every Summonr binary. It provides operations,
not preinstalled task workflows. Nothing downloads a model, calls a model, or
grants access during installation. Use `summonr tools` to inspect the catalogue;
select the operations your ritual needs under `reagents`.

- [`data`](data.md): pure computation over explicit values, including bounded
  map, filter, and fold.
- [`file`](file.md): scoped, read-only text files and directory listings.
- [`sight`](sight.md): scoped image reads that return handles for model steps (vision).
- [`http`](http.md): scoped HTTP(S) GET returning bounded UTF-8 text.
- [`mail`](mail.md): the existing fixture mailbox, including its held writes.

`data` needs no external grant. `file`, `sight`, `http`, and `mail` do. Reads produce
observations of mutable external state; they are not pure functions, and repeated
reads need not agree. Read results are untrusted input if later sent to a model.
An origin or directory grant does not establish that its content is trustworthy.

Use ordinary computation wherever explicit rules suffice. Reach for a bounded
model step only for the portion requiring interpretation or judgment. A schema
checks representation; it does not prove extraction or reasoning correct.

## Composition without inference

`examples/06-standard-library/collections.yaml` multiplies integers, keeps positive
results, and sums them with an explicit accumulator:

```bash
summonr run examples/06-standard-library/collections.yaml --wait --json
```

No model or grants are needed. Its model-request ceiling is zero. The standard
library is available immediately; examples are ordinary files you can inspect,
modify, and save. They are not automatically inscribed over your own definitions.

The collection operators are deterministic data operations, not general tool
loops. Use `each` and child rituals when a body needs external I/O or inference;
those operations retain their own explicit authority and budgets.
