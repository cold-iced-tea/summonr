# Tutorial: Research and model evaluation

Summonr includes a built-in research loop for **verifying qualitative/extraction work** and **measuring whether a smaller or cheaper model meets your quality bar**:

1. Run your workflow with a strong model (or collect real runs),
2. Sample past runs into unlabeled test cases (`glean`),
3. Label them or attach a deterministic `touchstone` ritual,
4. Sweep candidate models and settings (`sweep`), score them (`judge`), rank winners (`weigh`), and lock in drift checks (`assay`).

## 1. Collect real runs into a test pack (`glean`)

Once a ritual (for example `triage-ticket`) has real ended runs in the core's ledger, sample cases that aren't already in your evaluation file:

```bash
summonr glean triage-ticket 10 --against examples/02-triage-ticket/tickets.yaml
```

`glean` prints each case with the ritual's answer under `observed` and leaves `expect` empty. Summonr refuses to score unlabeled cases, so the model under test never grades its own homework—you review and fill in `expect` (or use a pure `touchstone` ritual).

## 2. Record a baseline and check drift (`assay`)

Run an assay set against your current reference model and save it as the baseline:

```bash
summonr inscribe examples/02-triage-ticket/triage-ticket.yaml
summonr assay triage-ticket --against examples/02-triage-ticket/tickets.yaml --baseline
```

Later, test a smaller/local model—or re-check after editing instructions—against the same cases:

```bash
summonr assay triage-ticket --against examples/02-triage-ticket/tickets.yaml --model ollama/qwen3:4b
```

Exit codes let scripts and CI act on the verdict:
- `0`: within tolerance
- `12`: drifted beyond your configured tolerance
- `13`: inconclusive (for example too few cases, mode mismatch, or touchstone errors)

## 3. Compare models and settings (`sweep`, `judge`, `weigh`)

When you want to compare multiple models, temperatures, or prompt variants at once, write a sweep spec (for example `sweep.yaml`):

```yaml
ritual: route-form
models:
  - mock/keyword
  - ollama/qwen3:4b
settings:
  - {temperature: "0"}
  - {temperature: "0.2"}
cases:
  - id: support_direct
    inputs:
      form: {category: support, message: "Export button broken"}
    expect: {from_form: support}
  - id: billing_ambiguous
    inputs:
      form: {category: other, message: "Refund my last invoice"}
    expect: {from_model: billing}
```

Run the parameterized sweep, score every trial, and rank the variants:

```bash
# 1. Run all (model x settings x case) combinations (writes are simulated unless --live)
summonr sweep route-form --against sweep.yaml --json

# 2. Score the experiment (optionally against a pure touchstone ritual with --against TOUCHSTONE)
summonr judge exp-01... --json

# 3. Rank variants by pass rate, then lowest mean tokens, then lowest mean duration
summonr weigh exp-01... --json

# Or compare two experiments head-to-head:
summonr weigh exp-01... --against exp-02... --json
```

## 4. Keep human sign-off on promotion

An external orchestrator or scheduled job can run `glean`, `sweep`, `judge`, `weigh`, and `assay` automatically, while **saving a new ritual version (`inscribe`) and updating a `baseline`** remain explicit owner decisions. That lets you test cheaper models continuously without silently changing production behavior.
