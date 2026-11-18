# Outcomes and exit codes

## Summon outcomes

| Outcome | Meaning | Exit code when waited on |
| --- | --- | --- |
| `returned` | Finished with its return value | 0 |
| `abstained` | Finished; a top-level model or ritual step abstained | 0 |
| `exhausted` | An essence limit was reached | 10 |
| `cancelled` | Stopped by banish | 11 |
| `invalid` | A model's output was not JSON or did not match its schema | 11 |
| `failed` | Anything else, including a refused tool call or a failed condition | 11 |
| `unverified` | A completed operation could not be verified | 11 |
| `interrupted` | The core stopped while it was running | 11 |

When a summon does not return, its result is `{"partial": ...}`: the per-item record and counts of every batch that ran, including interrupted items and their recorded prior effects. Unknown external outcomes still require reconciliation.

An abstention inside an `each` makes that *item* `abstained`; it does not make the whole summon abstained.

## Item states in a batch

`returned`, `abstained`, `invalid`, `failed`, and `unverified` (a write whose read-back did not match).

## Write states

Each write in a batch is recorded as one of:

| State | Meaning |
| --- | --- |
| `applied` | Performed. Its evidence class is `observed by the runtime` (verified) or `reported by the tool` (no `verify`). |
| `held` | Recorded as a petition, not performed |
| `unverified` | Performed, but the read-back did not show the expected state |
| `simulated` | Authorized, then not performed (`simulating`, every replay, and every drill). Its evidence class is `simulated by the runtime`. A simulated `hold` write files no petition and carries `would_hold: true`. |

A petition is atomically claimed as `executing` before a decision executes. It then becomes `applied`, `unverified`, `failed`, or `denied`. Recovery changes an interrupted claim to `unknown`; this means the external outcome must be reconciled, not retried blindly.

Batch counts gain a `simulated` entry. A simulated write is never verified; the ledger says `<step> check skipped: simulated`. See [Safe building](../guides/safe-building.md).

## Exit codes

| Code | Meaning |
| --- | --- |
| 0 | Success (including `returned` and `abstained`; drift within tolerance) |
| 1 | Unexpected error |
| 2 | Invalid: bad syntax, invalid ritual, bad input, a cassette or drill file that does not parse, `recording` over a file that is not a cassette |
| 3 | Not found: no such ritual, summon, petition, reagent, or file (including a missing cassette or drill file) |
| 4 | Not authorized: missing grants, an agent changing a `user_only` input, or an agent asking to record, replay or simulate |
| 5 | Conflict: ritual exists with different content, summon already ended, drift policy refuses, no baseline |
| 7 | The wait timed out; the summon is still running |
| 8 | The core is unavailable |
| 10 | Exhausted |
| 11 | Failed, invalid, cancelled, interrupted, unverified, or durable storage failure |
| 12 | Assay: drifted |
| 13 | Assay: inconclusive |
| 14 | Drill: one or more cases failed |
| 15 | Verify: an epitaph, mark, export or reveal did not verify |

## Structured contract errors

`storage_failed` blocks further dispatch after a persistence failure. `busy` means the configured active-run ceiling was reached; it applies to `invoke` and to each drill case. `state_conflict` covers stale contract hashes, changed arguments or run mode (`record`, `replay`, `simulate`) under an idempotency key, and already-claimed petitions. These are explicit failures; no automatic retry or model substitution occurs.
