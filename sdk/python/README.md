# Summonr Python authoring prototype

A dependency-free builder for the existing declarative definition. Python builds
the plan; it is not the execution language. The runtime remains Rust and executes
only the validated, prepared definition.

From the repository root, explicitly run a **trusted** builder:

```bash
PYTHONPATH=sdk/python python3 sdk/python/examples/receipt.py > /tmp/receipt.json
summonr compile /tmp/receipt.json --json
summonr rehearse /tmp/receipt.json --arg text='your receipt' --json
# After reviewing the definition, effects, requirements and model calls:
summonr save /tmp/receipt.json
```

The builder runs with your Python process's authority. It is arbitrary Python and
is **not sandboxed**. Do not run an untrusted agent-authored builder merely to
preview it. Have the agent supply JSON, inspect it, and compile it without running
Python. Compile/preview/save/run never import or execute `.py` inputs.

You may install the package into your own Python virtual environment with
`python -m pip install ./sdk/python`. That is optional; `PYTHONPATH` suffices for
the examples. The distribution is named `summonr-authoring`; import `summonr`.
The prototype has not been published to a package registry.

## Runtime values are references

```python
from summonr import Ritual, Str

r = Ritual("length-check", essence={"requests": 0, "tool_calls": 2, "time": "5s"})
text = r.input("text", Str)
length = r.data.count(text, id="length")
r.fail_if(length > 1000, "text too long", id="limit")
r.returns({"length": length})
r.emit()
```

`length` is not a Python integer. `length > 1000` creates an expression; it does
not compare at authoring time. `if length > 1000`, `bool(length)`, `str(length)`,
f-strings, numeric conversion, and Python iteration raise `TypeError`. Use
`r.when(expression)` / `r.fail_if(...)` and `r.each(...)`. Combine conditions with
parenthesized `&`, `|`, and `~`; Python `and`, `or`, and `not` are refused.

`r.each` is a context manager that authors one runtime loop body, not a Python
iterator. Ordinary Python loops over actual lists still unroll at authoring time.
The compiler validates reference visibility and bounds after emission. Fields on
a reference use attribute/index notation; use `r.data.get` for keys that cannot
be expressed as dotted segments. Expressions are not general argument values;
use `data.compare` when an operation needs a computed boolean.

`r.model(..., returns=Object(...))` describes a structured model step.
`abstain_if=lambda output: output.label == "abstain"` builds its predicate.
`r.call` describes a child ritual, which the Rust preparation step pins to a saved
version/hash. `r.map`, `r.filter`, and `r.fold` describe the bounded pure collection
operations; their callbacks run once to build templates, never once per runtime
item in Python. No callback is serialized or invoked by the daemon.

Primitive use populates `reagents.select`; it **does not infer requirements or
grant authority**. External reads/writes must still declare explicit `requires`
and receive owner grants. Supply explicit reagent ordering, IDs, metadata, and
prompts when reproducing an existing definition's exact hash. Explicit reagent
lists are preserved, and missing used primitives are appended.

Types (`Str`, `Int`, `Number`, `Bool`, `List`, `Object`) construct output schemas.
Input declarations use the runtime's input types. A typed list input such as
`List[Int]` is declared with its item schema (`{"type": "list", "schema": ...}`),
which the runtime checks when the run is invoked. Other constraints the runtime
does not enforce on inputs are refused.

Recent language features have flat builders: every pure `r.data.*` operation
(`coalesce`, `range`, `index_of`, `unique_by`, `flatten`, ...; `data.assert` is
`r.data.assert_`), `r.each(..., keep=[...])`, `r.filter(..., params=...)` with
`r.param(NAME)`, the expression string helpers `lower`, `upper`, `trim`,
`contains`, `starts_with` and `ends_with`, `with r.cleanup():` for `finally:`,
and `r.require(reagent, action, on={"root": target})` for a root taken from an
input. `tests/test_parity.py` compiles each with the real binary when one is built. Editor stubs
provide API completion; they do not establish full static typing of referenced
fields or eliminate the mandatory Rust compilation step.

## Prototype boundaries

This prototype supports operation/model/child steps, when/fail_if, runtime each,
collections, budgets, bindings, requirements, verification, holds, and return
schemas. There is no decorator magic, automatic source rewriting, Python-to-IR
compiler, sandbox, source map, general expression arithmetic, or dedicated else
builder yet. YAML/JSON remain available for the full definition surface.

Examples preserve the existing manifests, prompts, limits, declaration order and
IDs. Model examples are **compiled and compared only** by the checks; they are
not quality-tested or executed against a real model. Review the emitted JSON and
bind approval to its prepared hash, not to the Python filename. Rebuilding a
builder may produce a different artifact because Python can inspect its environment.

## Tests

```bash
PYTHONPATH=sdk/python python3 -B -m unittest discover -s sdk/python/tests -v
python3 -m venv /tmp/summonr-authoring-tests
/tmp/summonr-authoring-tests/bin/pip install -r sdk/python/requirements-dev.txt
/tmp/summonr-authoring-tests/bin/python -B scripts/check_authoring.py
```

The latter uses an independent Draft 2020-12 validator and a temporary Summonr
home. It needs local socket access, performs only pure calculations, and checks
manifest, receipt, single-review, review-batch and collection equivalence.
