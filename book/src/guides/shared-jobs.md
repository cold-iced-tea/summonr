# Shared jobs: many workers, one collection

A shared job is a durable collection of uniquely identified items. Several ritual
instances can work on the same job: the core assigns disjoint batches atomically,
then accepts results only from the current, unexpired owner of each item.
Coordination uses deterministic database transactions. There is no bidding model,
currency, market, or inference involved in assigning work.

This version supports **computing results**, such as classifications, extracted
fields, or receipt totals. Workers may read and compute, and may invoke bounded
models. They cannot perform external writes or create held-write petitions,
including through child rituals. Applying classifications to mail or another
external system remains a separate authorized workflow. A database lease alone
cannot fence a filesystem or an external service.

## Try two workers without a model

From the repository root, use the same temporary home in both terminals:

```bash
export SUMMONR_HOME=/tmp/summonr-job-demo
summonr save tests/fixtures/rituals/total-receipt.yaml --json
summonr job create tests/fixtures/jobs/receipts.json --json
```

Start one command in each terminal (both must use that `SUMMONR_HOME`):

```bash
summonr job work receipt-totals --arg count=2 --arg lease=1m --json
```

Each command claims at most two items and runs them sequentially. Different
worker commands execute concurrently. The command waits for its finite batch;
closing its client does not cancel work already accepted by the daemon. There is
no background loop that keeps claiming more batches. With four items, running
the command twice processes all four, whether the workers overlap or not.

```bash
summonr job status receipt-totals --json
summonr job item receipt-totals receipt-001 --json
summonr core stop
```

The totals are 125, 350, 799, and 200; model requests are zero. Each item is also a
normal summon, visible in scry. `job item` returns up to the latest 100 attempt
records, with summon IDs for full usage, evidence, errors, and child records.
Older attempts remain in the database. `queue` is an alias for `job`;
`given count=2 lease=1m` is equivalent to the two `--arg` clauses.

## Manifest and pinned contracts

A manifest is JSON, limited to 8 MiB and 10,000 items:

```json
{
  "job": "receipt-totals",
  "ritual": "total-receipt",
  "version": 1,
  "items": [
    {
      "id": "receipt-001",
      "version": "snapshot-1",
      "args": {"text": "{\"amounts_minor\":[100,25]}"}
    }
  ]
}
```

The ritual must already be saved. Omitting its version selects the latest version
**once, at creation**. The job stores that version and contract hash. Later
inscriptions do not change the job. Item IDs must be unique within the job;
item versions and inputs are explicit and immutable. Creating an existing job
name fails; use a different name for a different workload.

Item versions identify the supplied input snapshot. They are not automatically
checked against live files or services. Supply the data in `args` when consistent
snapshots matter. Separate jobs do not deduplicate each other's resources, and
ordinary runs outside the queue do not participate in its ownership checks.

The ritual must declare request and time ceilings, plus a token ceiling when
requests can be greater than zero. Existing grants, schema validation, child
pins, and runtime budget enforcement still apply. Each item has its own ritual
budget; this is not a new shared job-wide inference budget. For a work request
claiming N items, its configured request ceiling is at most N times the ritual's
request ceiling. Explicit retries and later work requests can spend more.

## Claims, deadlines, and recovery

`count` is 1–100 (default 1). `lease` is 1 second–1 hour (default 5 minutes).
The lease covers the **whole claimed batch**, including waiting behind earlier
items. Choose a batch size that can finish within it. There is no automatic
renewal. The lease also tightens each item's execution deadline.

Every claim records a worker ID, generation number, input version, and expiry.
The runtime checks ownership during execution and again in the result-commit
transaction. A late result from an expired or superseded generation cannot
replace the current result. Attempt history retains rejected late results.
Clients cannot supply ownership tokens or arbitrary completed results over RPC.

A later work request makes expired claims available again. Status retains the
stored `claimed` state until reclamation; inspect its expiry timestamp. On daemon
restart, claimed attempts become `interrupted` and their items become ready.
Completed items stay completed. Recovery starts no workers or inference itself.

An in-flight model call may finish after its claim expires. Computation can also
repeat if the daemon stops after recording the summon but before committing its
job result. **This is exclusive acceptance of results, not exactly-once inference.**
Normal summon records retain attempted calls and reported usage. Job status sums
root-summon requests and reported tokens across attempts, including failed and
superseded attempts; missing backend usage remains unknown, not free.

Only `returned` outcomes become `done`. Abstention, failure, invalid output,
cancellation, and exhaustion become `failed`, preserving the outcome and partial
result. They require an explicit retry:

```bash
summonr job retry receipt-totals receipt-001 --json
summonr job work receipt-totals --arg count=1 --json
```

The retry makes the named failed item ready; it does not guarantee that the next
worker selects it before other ready items. Workers select in manifest order.
A successful work RPC means the batch was processed; inspect each item's
`committed` and `record.outcome` to determine success.

`job status` returns counts plus up to 100 items. Follow `next_offset` with
`--arg offset=N`. `job item JOB ITEM` exposes recent attempts. Job commands are
available through CLI and local RPC; no new MCP tools are exposed yet.

## What is deliberately absent

Capability bidding, dynamic worker selection, per-familiar queue permissions,
external-effect reconciliation, live-resource version fencing, automatic retries,
and parallel iteration inside one ritual are not implemented. The core's normal
active-run limit still applies. If admission fails after a batch is claimed,
that item's attempt is recorded as failed and requires an explicit retry.
The useful adaptation from Colony is a bounded piece of work with a recorded
owner and outcome; its economic machinery is not part of Summonr.
