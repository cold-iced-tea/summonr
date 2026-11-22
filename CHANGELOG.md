# Changelog

All notable changes to Summonr are documented in this file.

## [Unreleased]

### Language gaps found building `examples/10-bughunt`
- New pure operations: `data.coalesce` (first non-null), `data.flatten` (one level), `data.range` (bounded integer sequence), `data.index_of` (first equal or containing element, else null), and `data.lines` with optional `number_from` for numbered lines. Each has a static output type.
- `each` takes `keep: [step ids]` so items keep only the outputs later steps need; loop results are data and data is capped at 1 MiB.
- CLI clause `reading k=PATH` / `--arg-file k=PATH`: an input's value is read from a regular file (at most 1 MiB, one trailing newline dropped), so large lists and objects no longer overflow the command line. `reading` is now a reserved word, like `on` or `file`: a positional spelled `reading` must be quoted differently or renamed.
- `git.worktree_add` explains a remote that lacks the trunk branch instead of passing through git's `rev-list` usage text.
- Docs: `ritual-format.md` no longer says `parallel` above 1 is rejected; `each` has supported `parallel: 1..32`.

### Agent experience (from bughunt's AGENT-NOTES.md)
- `brief` / `--brief` on `run --wait` and `record`: outcome, result, usage and child run ids only. Large runs' full records could exceed the 64 MiB RPC frame, making `run --wait --json`, `record` and `logs` fail with "frame too large".
- A failed validation's `error.message` names the first problem instead of only "definition does not satisfy its contract".
- A model that repeats itself until the backend stops it (Ollama "token repeat limit") is `invalid` output, not a failed backend, so `on_failure` and abstention treat it as an answer.
- A `config.yaml` written beside `config/` in a `SUMMONR_HOME` is reported in stderr and every envelope's `warnings` instead of being silently ignored.
- Drills take `model_default: {step: answer}` for every call after a step's listed answers.

## [0.2.0] — 2026-11-22

### Enterprise Governance & Compliance
- **Tamper-evident Ed25519 `audit_log` (`src/core/audit.rs`, `src/core.rs`):**
  - Append-only SQLite `audit_log` hash-chained via SHA-256 and signed with the core's Ed25519 key over `summonr-audit-v1\0`.
  - SQLite `BEFORE UPDATE` and `BEFORE DELETE` immutability triggers (`audit_log_no_update`, `audit_log_no_delete`).
  - Fail-closed startup verification (`verify_integrity_on_startup`) over both `audit_log` and `epitaphs`.
  - `config.yaml` SHA-256 boot anchor (`config.loaded`), Linux `SO_PEERCRED` `(uid, pid)` attribution, and atomic `summon.finished` cross-anchoring to `epitaph_hash` inside `Core::finish_summon`.
- **Role-Based Access Control (RBAC) & Separation of Duties (`src/core/operators.rs`, `src/core/rbac.rs`, `src/exec.rs`):**
  - Ed25519-authenticated operator identities (`summonr operator new/add/list/revoke`) with enterprise roles: `admin`, `policy_author`, `agent_runner`, `reviewer`, `auditor`, `security_admin`, and legacy `author`.
  - Normalized SQLite RBAC schema (`rbac_roles`, `rbac_role_permissions`, `rbac_actor_roles`) with `summonr rbac list | role-create | grant | revoke | bind | unbind`.
  - Pre-execution reagent guard (`Run::guard_reagent_rbac` in `src/exec.rs`) that checks live SQLite bindings before any side effect, petition, or fence, logging `rbac.reagent_denied` to `audit_log`.
  - `rbac.enforce` (refuse unsigned local socket requests) and `rbac.require_independent_reviewer` (block self-approval of held petitions).
- **SIEM Export, Signed Compliance Bundles & Retention Pruning (`src/core/audit.rs`, `src/cli/trust.rs`):**
  - `summonr audit verify [--json]` (exit code `15` on tamper).
  - `summonr audit export [FILE] [--format json|ndjson] [given from=MS to=MS]` for signed SOC 2 evidence bundles (`summonr-evidence-bundle-v1`) and line-delimited SIEM ingestion (`summonr-siem-manifest-v1`).
  - `summonr audit verify-bundle FILE [key K]` for offline third-party verification.
  - `summonr audit prune given days=N [--dry-run]` for chain-preserving plaintext elegy retention pruning.
- **Capability-Sandboxed WASI `wasm_reagents` (`src/reagents/wasm_reagent.rs`):**
  - Wasm v1 magic and SHA-256 digest pinning verified on every call.
  - Unconditional `env_clear()` (zero host env inheritance), explicit `preopen_dirs`, `setsid` process-group timeout kills, bounded stdout (`max_output_bytes`), and fail-closed default effect `"write"`.
- **Documentation & Examples:**
  - Added **Part IV — Enterprise Governance & Compliance** to the mdBook (`book/src/governance/`) mapping controls to SOC 2 and ISO/IEC 27001:2022.
  - Added runnable `examples/09-redact-document/` and end-to-end `scripts/check_governance.py`.

## [0.1.0] — 2026-11-21

- Core runtime, ritual compiler, capability-scoped reagents (`data`, `file`, `http`, `sight`, `mail`, `git`, `gh`, `trial`, `cmd`, `mcp_reagents`), monetary budgets (`essence.cost` + `pricing:`), Reliquary signed ritual packs, Rites staged DAG workflows, bilateral Pacts, and Ed25519-signed `epitaphs` with witness countersignatures and RFC 3161 timestamps.
