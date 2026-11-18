# Assays and drift

A ritual that worked last month can quietly stop working: the model was updated, a quantization changed, the server was upgraded. **Assays** measure a ritual against labeled cases, record the result as a **baseline**, and later tell you whether it has **drifted**.

## Assay sets

An assay set is a YAML file:

```yaml
assay_set: inbox-set
min_cases: 12
tolerances: {accuracy_drop: 0.10, abstention_rise: 0.15}
cases:
- id: "2026-11-01"
  inputs: {text: "From: orders@pagebound.example\nSubject: Your order #48213 is confirmed\n..."}
  expect: {folder: Receipts}
- id: "2026-11-10"
  inputs: {text: "From: unknown@relay.example\nSubject: hey\n\nsee attached\n"}
  expect: {folder: abstain}
```

| Field | Default | Meaning |
| --- | --- | --- |
| `assay_set` | required | The set's name |
| `min_cases` | 20 | Fewer cases than this makes a drift check inconclusive |
| `tolerances.accuracy_drop` | 0.05 | How far accuracy may fall before it counts as drift |
| `tolerances.abstention_rise` | 0.10 | How far the abstention rate may rise |
| `cases[].inputs` | required | The ritual's inputs |
| `touchstone` | none | A saved ritual that computes each case's expected result (see below) |
| `cases[].expect` | required without a touchstone | Expected values; keys are dotted paths into the result |

A case is correct when the run returned or abstained and every `expect` path matches. An expected abstention, such as `{folder: abstain}` above, is a correct answer; abstentions are still counted separately so their rate can be tracked.

Cases with an empty `expect` are refused. An unlabeled case cannot be scored.

## Writes are simulated

An assay is a measurement, not a job. Every write a case would make is authorized and then **simulated**, exactly as in [`simulating`](../guides/safe-building.md#3-simulate): nothing moves, and a held write files no petition. Each case is admitted like any other run, so the active-run limit applies.

To let an assay perform its writes, say so: `summonr assay R against SET live` (`--live`). The account records which mode it ran in. Comparing against a baseline recorded in the other mode is **inconclusive**, because a simulated and a live run are not the same measurement.

## Touchstones: computed expectations

Hand labels are slow to write and easy to get subtly wrong. Where part of a ritual's answer follows from a rule, let the rule supply it. A **touchstone** is a saved ritual that computes the expected result for a case:

```yaml
assay_set: route-form-set
touchstone: route-form-touchstone
cases:
- id: a
  inputs: {form: '{"category":"support","message":"The export button does nothing"}'}
- id: b
  inputs: {form: '{"category":"sales","message":"Can I get a demo?"}'}
```

The name comes from the assayer's touchstone, the stone that gold was tested against.

- A touchstone must declare `essence: {requests: 0}`, use only pure `data` operations (children included), and take exactly the inputs of the ritual it judges, with the same types. Anything else is refused before a case runs.
- It is pinned by content hash when the set is loaded. The account shows its name, version and hash, and a baseline stores them. If the touchstone has changed since the baseline, the comparison is **inconclusive**: different expectations are a different measurement.
- Its returned object is the case's `expect`. A set with a touchstone may not also carry hand-written `expect` values.
- If the touchstone cannot answer a case (its own validation fails, or it does not return), that case is counted under `touchstone_errors`, not as wrong, and a drift check is **inconclusive**. A baseline cannot be recorded while any case has a touchstone error. A touchstone's failure is never evidence against the ritual under test.

A touchstone encodes your understanding of the rule. If that understanding is wrong, the touchstone is wrong in the same way, but it is wrong in one place you can read and test, not scattered through hand labels. It can only cover what a rule can compute: routing, totals, structure and pass-throughs, not the judgment a model step exists for.

## Recording a baseline

```console
$ summonr assay classify-message against inbox-set.yaml baseline
* ACCOUNT · classify-message version 1 against inbox-set
  model      mock/keyword
  cases      16 · 2 correct · 14 wrong · 0 invalid · 0 failed · 14 abstentions
  accuracy   12.5%  (95% interval 3–36%)
  abstained  87.5%
  cost       0 ms mean per case · 4,911 tokens
  ✕ 2026-11-01     abstained expected {"folder":"Receipts"} got {"folder":"abstain","reason":"mock"}
  ...
  Baseline recorded for this version.
```

(That is the keyword mock, which is why it scores 12.5%. Use a real model for real numbers.)

Each case runs as its own summon, all in one **series**, with the model kept loaded throughout. The account includes a Wilson 95% interval for accuracy, so a small set does not look more certain than it is.

The baseline stores the account, the set itself and its hash, the mode (simulated or live), and a **fingerprint** of every model the ritual can call: each step's pinned model, the ritual's own, and those of its child rituals. For Ollama each entry holds the model's digest, quantization, parameter size, and server version. A change to any of them is noticed. A baseline recorded before fingerprints covered every model compares as inconclusive until you re-record it.

Baselines belong to one ritual **version**. Inscribing a new version starts without one.

## Checking for drift

```console
$ summonr assay classify-message
...
* DRIFT
  verdict    within tolerance
  change     accuracy -0.0 points · abstention +0.0 points
  backend    fingerprint unchanged
  Nothing was repaired, switched, or re-baselined. Recording a new baseline is an explicit act.
```

Without `against`, the assay re-runs the set stored with the baseline. The verdict is one of:

| Verdict | When | Exit code |
| --- | --- | --- |
| within tolerance | Accuracy and abstention are within the set's tolerances | 0 |
| drifted | Accuracy fell, or abstention rose, past a tolerance | 12 |
| inconclusive | The set, mode or touchstone differs from the baseline's, the set has fewer than `min_cases`, the touchstone could not answer a case, or the baseline predates full fingerprints | 13 |

The exit codes make drift checks easy to schedule.

## Drift warnings at invocation

When a ritual version has a baseline, each invocation compares the backend's current fingerprint with the recorded one. If they differ, the ritual's `drift.on_mismatch` policy decides what happens:

| Policy | Effect |
| --- | --- |
| `warn` (default) | The run proceeds with a warning telling you to run `summonr assay` |
| `note` | The run proceeds silently |
| `refuse` | The run is refused (exit 5) until you check |

A refuse policy does not block the assay that clears it.

## Gleaning real cases

Hand-written cases go stale. **Gleaning** samples real past runs as new cases:

```bash
summonr glean classify-message 10 against inbox-set.yaml
```

It picks, at random, up to 10 ended runs that returned or abstained, skipping assay runs and any run whose inputs are already in the set. Each case comes out with the ritual's answer as `observed` and an empty `expect`:

```yaml
- {"id": "s-…", "inputs": {...}, "expect": {}, "observed": {"folder": "Receipts", ...}, "source": {...}}
```

You fill in `expect` and append the cases to your set, or append them to a set with a touchstone, which labels them by computation. Because assays refuse unlabeled cases, **the model under test never labels its own cases.**
