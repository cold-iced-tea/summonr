# Changelog

All notable changes to Summonr are documented in this file.

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
  - `env_clear()`, explicit `preopen_dirs` / `allow_env` pass-through, `setsid` process-group timeout kills, bounded stdout (`max_output_bytes`), and fail-closed default effect `"write"`.
- **Documentation & Examples:**
  - Added **Part IV — Enterprise Governance & Compliance** to the mdBook (`book/src/governance/`) mapping controls to SOC 2 and ISO/IEC 27001:2022.
  - Added runnable `examples/09-redact-document/` and end-to-end `scripts/check_governance.py`.

## [0.1.0] — 2026-11-21

- Core runtime, ritual compiler, capability-scoped reagents (`data`, `file`, `http`, `sight`, `mail`, `git`, `gh`, `trial`, `cmd`, `mcp_reagents`), monetary budgets (`essence.cost` + `pricing:`), Reliquary signed ritual packs, Rites staged DAG workflows, bilateral Pacts, and Ed25519-signed `epitaphs` with witness countersignatures and RFC 3161 timestamps.
