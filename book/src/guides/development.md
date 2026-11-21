# Developing and testing Summonr

Build and check locally without a model:

```bash
cargo build --offline
cargo test --offline
python3 -B scripts/check_offline.py
python3 -B scripts/check_jobs.py
python3 -B scripts/check_stdlib.py
python3 -B scripts/check_workbench.py
mdbook build book
```

`--offline` requires dependencies already cached. The Python integration scripts use temporary homes and mock inference; they do not touch your live state.

## Regression boundaries

Tests cover exclusive core ownership before recovery, injected SQLite failures, atomic terminal records, concurrent petition claims, duplicate approvals, unknown-effect recovery, invocation deduplication across restart, authority intersection, schema rejection, result fidelity, and partial effects after exhaustion. Budget tests include ancestor request/token limits and tool/item admission. Compiler grammar tests check both spellings.

These tests establish specific mechanics, not a complete security audit or general model quality. Add a failing regression for each execution defect before extending its semantics.

## Faults worth testing when adding an adapter

1. Refusal before dispatch: no external effect and no free retry.
2. Timeout during dispatch: outcome may be unknown; retain intent and missing usage.
3. Success followed by failed verification: the write happened; label it unverified.
4. Crash before recording the result: reconcile before replay.
5. Revoked permission between proposal and decision: no broadened authority.
6. Duplicate submission or decision: only one admitted execution identity.

Do not implement automatic retries for non-idempotent effects without adapter-specific deduplication and reconciliation. A local transaction cannot guarantee exactly-once behavior at a remote service.

## Internal boundaries

Socket requests deserialize into method-specific structs in `requests.rs`, with
unknown fields rejected and explicit validation of required values and ranges.
`outcome.rs` owns terminal outcomes and exit-code mappings. `records.rs` gives
steps, effects, evidence, petitions, and batch counts typed representations;
arbitrary task inputs and outputs remain JSON.

The YAML step representation preserves validation diagnostics and canonical
hashes. `plan.rs` converts validated source steps into executable enum variants,
so execution does not repeatedly guess which optional field determines a step.
`reagent.rs` registers named tool specifications behind a reagent interface;
authority and shared-job effect checks remain outside models.

`store.rs` owns the SQLite connection and checked read methods. Explicit write
transactions remain in their service modules. Assays, petitions, and job services
live under `src/core/`; this is not a claim that every persistence operation has
been moved into a repository abstraction. Corrupt rows and SQL errors must fail,
not become empty results.

The workbench is a client of the same CLI and RPC paths. Its integration check
uses a temporary tmux server and a pure ritual to verify quoting, plan display,
live status, disconnect/reconnect, and daemon survival after exit. It requires
`tmux` and permission to bind local sockets, but no model.

## Definition and SDK conformance

The optional authoring checks are documented in `sdk/python/README.md`.
`scripts/check_authoring.py` uses an independent JSON Schema validator and checks
source/compiled schema fixtures, exact SDK/YAML definitions, hashes and effect
plans, child pinning, first-class JSON files, and refusal to execute Python inputs.
It executes only pure examples in a temporary home. The Python SDK itself is
standard-library-only; the conformance checker has explicit development dependencies.

## Compatibility

New contracts use `language_version: 1`. Omitted version and omitted optional fields retain the existing canonical representation where possible. Unknown schema assertions and irrelevant step modifiers now fail rather than being ignored. Integrations relying on the old MCP batch summary must consume the faithful `result` and separate effect events instead.

Read the [roadmap](../roadmap.md) for unfinished features. Do not document a proposed capability as already available.
