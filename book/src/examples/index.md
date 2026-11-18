# Learning by Example

Eight small workings, each one no longer than a page, and each adding a single discipline to the last. Read them in order; by the eighth you will have crossed from pure arithmetic to parallel batches, checkpointed recovery, MCP agent handoff, and drift measurement.

| # | Working | The discipline it adds |
| --- | --- | --- |
| 1 | [Check a manifest, no model](01-no-model.md) | A ritual is a checked procedure first; not every task deserves a model. |
| 2 | [Route a support ticket](02-first-judgment.md) | One bounded judgment, a closed schema, and an explicit way to keep silence. |
| 3 | [Total a receipt](03-model-reads-code-adds.md) | The model reads; deterministic code adds; two readings must agree. |
| 4 | [Route a contact form](04-only-when-needed.md) | Known rules run first; inference is spent only where the rules fall silent. |
| 5 | [Label reviews on a budget](05-batches-and-budgets.md) | Batches, hard essence ceilings, partial results, and reading the record. |
| 6 | [Hand a ritual to your agent](06-agents.md) | Another agent receives a pinned capability, never an open hand. |
| 7 | [Know when it gets worse](07-assays.md) | Measure against a baseline and catch drift before it compounds. |
| 8 | [What to build next](08-ideas.md) | Patterns to carry into larger systems. |

Every transcript in these chapters was captured from the files in `examples/` against `ollama/qwen3:4b` (or with no model at all). Another model—or the same model on another day—may answer differently; that variance is why chapter 7 exists.

## Setup

From the repository root:

```bash
cargo build
export PATH="$PWD/target/debug:$PATH"
export SUMMONR_HOME="$(mktemp -d)"          # an isolated home for these workings
mkdir -p "$SUMMONR_HOME/config"
echo 'ollama_url: "http://127.0.0.1:11434"' > "$SUMMONR_HOME/config/config.yaml"
ollama pull qwen3:4b                         # needed for workings 2–7
```

These chapters use the flag spelling (`run`, `--arg`, `--wait`). Every command also has a keyword spelling; see [Commands and clauses](../reference/cli.md).

Without Ollama, pass `--model mock/keyword` to exercise the machinery—grants, holds, limits, and records—without real inference.

When you finish:

```bash
summonr core stop
```
