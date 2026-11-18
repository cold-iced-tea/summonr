# Authoring with Python, executing a declarative plan

Summonr accepts JSON and YAML definitions through the same compiler. The optional
Python SDK provides a code-writing surface that emits JSON. **Summonr never runs
Python builders during compilation, rehearsal, inscription, or execution.**

From the repository root:

```bash
PYTHONPATH=sdk/python python3 sdk/python/examples/receipt.py > /tmp/receipt.json
summonr compile /tmp/receipt.json --json
```

Running Python is an explicit trust decision: a builder has the same access as
any other Python program you launch. For an untrusted agent author, request JSON
instead. Review the emitted artifact and compiler result; then save the JSON.
The daemon and MCP server still execute only declarative plans.

```python
from summonr import Ritual, Str

r = Ritual("text-length", essence={"requests": 0, "tool_calls": 2, "time": "5s"})
text = r.input("text", Str)
length = r.data.count(text, id="length")
r.fail_if(length > 1000, "text exceeds limit", id="limit")
r.returns({"length": length})
r.emit()
```

The value `length` represents future runtime output. Comparisons build expression
nodes; Python boolean conversion, iteration, string interpolation, and numeric
conversion raise errors. Use `r.when` / `r.fail_if` for runtime conditions and
`r.each` for runtime loops. Python loops over actual Python collections still
unroll at authoring time. `map`/`filter`/`fold` callbacks build templates once;
they are never executed by the runtime.

The prototype includes explicit operation, model, child, loop and conditional
builders, schema helpers, and editor stubs. It does not provide full static typing,
a sandbox, source maps, a dedicated else builder, or general Python translation.
External requirements and owner grants remain explicit. Models do not run while
the builder is emitting or the compiler is validating a definition.

The five ports under `sdk/python/examples` cover a manifest, receipt, single review,
review batch, and collection pipeline. Their prepared definitions, hashes and
effect plans are checked against the existing YAML examples. The tests execute
only the pure examples, not the model-backed ones.

See `sdk/python/README.md` in the repository for the full API and test instructions.
The SDK can be installed into a chosen virtual environment with
`python -m pip install ./sdk/python`; it has no runtime dependencies. It is a local
prototype, not a published package-registry release.

## Structural schema and approval

Download the [version 1 definition schema](../reference/ritual-v1.schema.json).
It uses JSON Schema Draft 2020-12 for editor/agent guidance. Do not confuse it with
Summonr's restricted model-output schema dialect. Do not put `$schema` inside the
definition; associate the schema through your editor configuration.

A schema-valid document can still name a nonexistent step, request an unsupported
operation, or lack authority. The Rust compiler remains authoritative. It embeds
bindings, pins children, checks semantics and produces a canonical content hash.
Approval applies to that prepared artifact. Original Python/YAML/JSON source bytes
are not the hashed execution contract; descriptions, prompts and ordered lists
remain part of the definition and can change its hash.

JSON files are parsed strictly as JSON, including bare filenames such as
`receipt.json`. YAML stays supported. Existing language-version-1 canonicalization
and hashes are preserved; omitted version still means version 1. The schema is
identified by `urn:summonr:ritual:1`, not a promised hosted service URL.
