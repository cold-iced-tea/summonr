# Flake Court and Report to Test

Two local, zero-model rites for trusted Python unittest projects. They run nominated tests in fresh, detached Git clones. Tests execute as your user and need their dependencies available to the configured Python interpreter. Each Summonr home pins one experiment to two commit SHAs. You can repeat either rite in that home.

## Run the clock demo

From the Summonr checkout, with Git and Python 3 installed:

```bash
cargo build --locked
lab="$PWD/examples/11-test-lab"
bin="$PWD/target/debug/summonr"
export SUMMONR_HOME="$(mktemp -d)/clock-lab"
python3 -B "$lab/prepare.py" "$SUMMONR_HOME" --bin "$bin"
"$bin" rite start "$lab/flake-court.rite.yaml" --json > "$SUMMONR_HOME/court.json"
"$bin" rite start "$lab/report-to-test.rite.yaml" --json > "$SUMMONR_HOME/report.json"
python3 "$lab/receipt.py" < "$SUMMONR_HOME/court.json"
python3 "$lab/receipt.py" --patch < "$SUMMONR_HOME/report.json"
```

Open the desktop with this same `SUMMONR_HOME` to see the named stages. The final stage contains the verdict and evidence. Use `summonr rite show ID --json` to retrieve it again. The fixture is a tiny two-commit repo with an order-sensitive test and a minute-formatting bug. Its expected verdicts are `order_sensitivity_on_both_revisions` and `verified_fix`.

## Run an experiment on your project

Pass `--spec path/to/case.json` to prepare.py with a new home. Include either the test pair, the report, or both. Use an absolute local repo path. Preparation resolves both revision names once. Test names must each identify exactly one unittest method. Imports should work from the checkout root in a fresh process.

```json
{
  "repo": "/absolute/path/to/clock-project",
  "parent": "HEAD^",
  "head": "HEAD",
  "timeout": 10,
  "target": "test_clockface.ClockTests.test_default",
  "predecessor": "test_clockface.ClockTests.test_switch",
  "report": {
    "title": "Minute display drops the leading zero",
    "module": "clockface",
    "function": "display",
    "args": [65],
    "expected": "1:05"
  }
}
```

**Flake Court** probes the target twice in isolation. Two passes trigger a test of the nominated predecessor immediately followed by the target in the same Python process. If both sequences fail at the target, the hearing runs both controls on the parent. Two isolated failures instead trigger a parent comparison; mixed isolated results trigger another isolated pair. The budget is eight suite invocations, each in its own checkout. Each observation includes the order, statuses, diagnostics, SHA, and wall time including checkout. A skip, failed predecessor, import error or timeout cannot establish an order effect. Two repetitions establish an observation at that sample size. They do not estimate a long-run flake rate.

**Report to Test** accepts a worked example for an importable function with positional JSON arguments and an expected JSON value. It drafts a standalone unittest and records the patch and SHA-256 of its source. Verification uses identical source against both revisions. `verified_fix` requires the generated equality check to fail on the parent and the same test to pass on the fix. Other receipts distinguish an example that already passed, a mismatch that remains, and an inconclusive experiment. To save the patch for review, redirect `receipt.py --patch` to a file and inspect it with `git apply --stat FILE`.

**Data collection.** Redirect the default receipt export to a JSONL file, or append different receipts into one dataset. Each row identifies its rite, stage summon, revision, test sequence, wall time and observed outcome. Use the stage summon and ordinal to deduplicate repeated exports. The core retains the original ledger. Source patches and test diagnostics can contain project content.

Setup creates a new home and grants the packaged stage rituals access to its configured local commands. The draft command reads the manifest. Test-running commands have write effects because they execute project code. The manifest digest is checked before every stage. Preparation never edits the target checkout.
