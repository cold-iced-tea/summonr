# Durability and recovery

Summonr's reliability boundary is the local core, SQLite state, and its declared adapters. A durable record can show that an action was admitted or observed; it cannot make an external service participate in a local database transaction.

## One owner before recovery

The core acquires an exclusive lock on `state/core.lock` before opening and recovering execution state. The lock lasts for the process lifetime. A second process cannot mark a live core's runs interrupted. Never delete the lock file to bypass an active owner; the file can remain after exit and the OS releases the lock automatically.

SQLite uses WAL and `synchronous=FULL`. Run completion and its terminal event commit together. Other execution writes are checked; the first persistence failure is latched and blocks subsequent dispatch. Repair storage and restart rather than continuing with an incomplete audit trail. Shutdown remains available after a storage fault.

An operation intent is stored before dispatch. Inference attempts likewise record their payload before the backend call, so a timeout or unreadable response is not a free or invisible request. Response usage remains unknown until reported. Generation ending with `done_reason: length` is rejected before dependent work.

## Petition state machine

```text
pending → executing → applied
                    → unverified
                    → failed
pending → executing → denied
executing → unknown  (on core recovery)
```

All selected petitions are atomically claimed before any is acted on. Duplicate IDs and already-claimed actions are refused. Multiple approvals cannot claim the same pending petition. Claiming a batch is atomic; applying its external effects is not.

A petition stores the narrowed authority available when proposed. Permission at execution is the intersection of that snapshot and current owner grants. Later grants cannot widen an old proposal. Legacy petitions without an authority snapshot cannot be applied; deny and propose them anew after review.

A crash after claim may leave the action completed, partially completed, or unstarted. Recovery labels it `unknown`; it does not return it to the pending queue. Inspect the actual resource and recorded intent before taking another action. There is no automatic retry of unknown effects.

## Partial execution

An interrupted batch item remains in `items` with `interrupted: true`, its completed steps, and its effects. It is excluded from `processed` and included in `remaining`. Therefore **remaining does not mean untouched**. A message may have moved before the next model request hit a limit.

Verification failure after a completed write produces `unverified`, including when the read-back itself fails. It never establishes that the write did not happen. Read-back is an observation at a moment in time, not proof that no other actor can change the resource afterward.

## What idempotency guarantees

MCP/RPC submission keys deduplicate the whole invocation. Same key and execution arguments return the same ID; changed arguments conflict. Records survive process restart. This does not provide exactly-once delivery to arbitrary external tools, transactional rollback, automatic checkpoints, or resumable instruction execution.

The fixture mailbox refuses ambiguous duplicate IDs and symbolic entries, and refuses a destination that already exists. It still uses ordinary filesystem operations and is not a sandbox against a hostile process concurrently replacing directory entries. Use a controlled fixture root.

## Operating limits

The default admission ceiling is 16 active top-level invocations (`max_active_runs`). Model requests, tool calls, and loop items are sequential within a ritual. HTTP connections have a separate ceiling of 32. These limits reduce resource exhaustion; they are not a complete operating-system resource sandbox.

Do not infer a hard remote execution deadline from a local timeout. Cancellation cannot prove that remote inference stopped, and cleanup may delay return.

`stop` (and `run --wait --cancel-on-drop` when its client goes away) takes effect within about a tenth of a second, even in the middle of a step: a running command, test run (`trial`), git call, wasm module or MCP server has its whole process group killed, and the step ends `cancelled`. A model request is abandoned the same way, but its HTTP connection stays open until the backend answers or the request's timeout passes (the HTTP client cannot close a request it is reading), so a backend such as Ollama may go on generating until then. A write stopped while running leaves its fence in place, as after a time limit: its outcome is unknown until reconciled.

## Per-step checkpoints and `revive`

As a top-level run executes, completed top-level steps (`top`) and non-interrupted `each` batch items (one row per item, `each:<id>#<n>`) are persisted in the `checkpoints` table.

If a root run ends as `exhausted`, `cancelled`, or `interrupted`, `summonr revive <run_id>` (`resume`):

1. Verifies the saved ritual version and content hash still match and re-checks standing grants.
2. Creates a **new** summon seeded with the prior checkpoints (keeping the ended summon's signed epitaph immutable).
3. Skips already-completed steps and batch items so completed effects are not re-executed, and resumes from the interrupted boundary.
