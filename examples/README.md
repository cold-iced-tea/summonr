# Examples

Runnable rituals for the "Learn by example" chapters of the Summonr Book (`book/src/examples/`). Each folder adds one idea to the last.

| Folder | Shows |
| --- | --- |
| `01-check-manifest` | A ritual with no model: parse, validate, summarize |
| `02-triage-ticket` | One model judgment with a closed set of answers and a way to abstain; an assay set |
| `03-receipt-total` | The model reads, code adds, and two readings must agree |
| `04-route-form` | Deterministic first; a model only when the input is ambiguous |
| `05-label-reviews` | A batch over a list, with a budget that can run out |
| `06-standard-library` | Pure collection pipelines (`data` standard library) with zero model calls |
| `07-pacts` | Two-core buyer/supplier contract with pinned ritual, approval threshold, and acceptance check |
| `08-sight` | An image read in code, then shown to a vision model; the drill scripts the reply so no vision model is needed |
| `09-redact-document` | Active boundary enforcement: scoped file roots, `user_only` petition hold, post-write `file.stat` verify, and `audit verify` |
| [11-test-lab](11-test-lab/README.md) | Flake Court and Report to Test: adaptive reruns, regression patches, and per-execution JSONL receipts |

Setup, from the repository root:

```bash
cargo build
export PATH="$PWD/target/debug:$PATH"
export SUMMONR_HOME="$(mktemp -d)"
mkdir -p "$SUMMONR_HOME/config"
echo 'ollama_url: "http://127.0.0.1:11434"' > "$SUMMONR_HOME/config/config.yaml"
```

Examples 02–05 name `ollama/qwen3:4b`. Use another model with `--model ollama/<name>`, or `--model mock/keyword` to exercise enum-based mechanics without a model (note: `03-receipt-total` asks the model to extract structured numeric line items, which requires a real model or a scripted `drill`).

`08-sight` and `09-redact-document` take their directories as inputs, so they are saved as they are, and you grant the directory those inputs may lie under with `--under`. The run then holds only the root it was given. Drill `08-sight` with no vision model:

```bash
summonr inscribe examples/08-sight/describe-image.yaml
summonr allow describe-image --from sight --select read_image --under "$PWD/examples/08-sight"
summonr run describe-image --arg root="$PWD/examples/08-sight/fixtures" --arg path=dot.png --wait   # needs ollama/qwen2.5vl:7b
summonr drill describe-image against examples/08-sight/describe-image.drill.yaml --arg root="$PWD/examples/08-sight/fixtures"
summonr core stop
```

`09-redact-document/README.md` walks through the held, reviewed write.
