# Examples

Runnable rituals for the "Learn by example" chapters of the Summonr Book (`book/src/examples/`). Each folder adds one idea to the last.

| Folder | Shows |
| --- | --- |
| `01-check-manifest` | A ritual with no model: parse, validate, summarize |
| `02-triage-ticket` | One model judgment with a closed set of answers and a way to abstain; an assay set |
| `03-receipt-total` | The model reads, code adds, and two readings must agree |
| `04-route-form` | Deterministic first; a model only when the input is ambiguous |
| `05-label-reviews` | A batch over a list, with a budget that can run out |

Setup, from the repository root:

```bash
cargo build
export PATH="$PWD/target/debug:$PATH"
export SUMMONR_HOME="$(mktemp -d)"
mkdir -p "$SUMMONR_HOME/config"
echo 'ollama_url: "http://127.0.0.1:11434"' > "$SUMMONR_HOME/config/config.yaml"
```

Examples 02–05 name `ollama/qwen3:4b`. Use another model with `--model ollama/<name>`, or `--model mock/keyword` to exercise the mechanics without one.
