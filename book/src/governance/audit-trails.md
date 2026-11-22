# Tamper-Evident Audit Trails

**Mapped controls:** SOC 2 **CC7.2, CC7.3, CC8.1, PI1.5, C1.2** · ISO 27001 **A.5.33 (Protection of records), A.8.9 (Configuration management), A.8.15 (Logging), A.8.24 (Use of cryptography)**

Summonr maintains two cryptographically linked, append-only chains inside its SQLite state database (`state/summonr.db`), both signed by the core's persistent Ed25519 identity key (`src/core.rs`, `src/core/audit.rs`, `src/core/epitaphs.rs`).

## 1. Dual Cryptographic Chain Architecture

### A. Administrative & Execution Audit Chain (`audit_log`)

Implemented in `src/core/audit.rs`, every entry in `audit_log` records `(seq, ts, actor, action, target, outcome, detail, prev_hash, hash, sig)`:

- **Hash chaining:** Each entry serializes a canonical JSON payload (`v: 1, seq, prev, ts, actor, action, target, outcome, detail`), computes `hash = SHA-256(payload)`, and sets `prev_hash` to the previous row's `hash` (or 64 hex zeros for `seq = 1`).
- **Ed25519 signatures:** Each entry is signed over the canonical payload using the domain separator `summonr-audit-v1\0` (`AUDIT_DOMAIN`).
- **Database-level immutability triggers:** SQLite `BEFORE UPDATE` and `BEFORE DELETE` triggers (`audit_log_no_update`, `audit_log_no_delete` in `src/core.rs`) abort any in-band mutation of `audit_log`.
- **Actor attribution:** Local Unix socket connections automatically extract `(uid, pid)` via `SO_PEERCRED` (`src/core/audit.rs`, `peer_cred_actor`); signed operator requests attribute to `operator:<name>(<role>)`.
- **Unified coverage:** `audit_log` captures:
  - Configuration boot anchors (`config.loaded` with SHA-256 of `config.yaml`, configured MCP/Wasm reagents, commands, and pricing rules).
  - Every completed summon (`summon.finished` committed atomically inside `Core::finish_summon` in `src/core.rs`, cross-anchored to `epitaph_seq` and `epitaph_hash`).
  - Every RBAC and policy denial (`rbac.denied`, `rbac.reagent_denied`, `rbac.unsigned_refused`, `rbac.self_approval_refused`).
  - Every governance mutation (`ritual.inscribed`, `grant.added`, `grant.revoked`, `operator.new/add/revoke`, `rbac.role_create`, `rbac.permission_grant/revoke`, `rbac.actor_bind/unbind`, `audit.export`, `elegy.pruned`).

### B. Execution Receipt Chain (`epitaphs`)

Implemented in `src/core/epitaphs.rs`, every ended summon commits an Ed25519-signed `epitaph` in the same SQLite transaction as its `summon.end` event. Epitaph hashes form an append-only chain and a Merkle tree (`epitaph_merkle_root`) that can be independently anchored to peer cores (`summonr witness`) or RFC 3161 Time-Stamping Authorities (`summonr witness tsa`).

## 2. Fail-Closed Startup Verification

On every `Core::open` (`src/core.rs` → `verify_integrity_on_startup` in `src/core/audit.rs`), Summonr automatically verifies **both** `audit_log` and `epitaphs` before serving traffic:

- If an offline attacker drops the SQLite triggers and mutates or deletes any row, startup verification detects the broken hash link or invalid Ed25519 signature.
- The core immediately enters a **fail-closed `storage_error` state**, refusing all operational RPCs (`storage_failed`) while keeping `audit_verify` readable so incident responders can inspect the exact compromised `seq`.

## 3. Auditor Verification & SIEM Export Runbook

### Verify Chain Integrity

Run `summonr audit verify` to walk the entire `audit_log` from `seq = 1` to the head, recompute every canonical SHA-256 hash, check every `prev_hash` link, and verify every Ed25519 signature:

```bash
# Human-readable pass/fail integrity report (exits 0 on pass, 15 on tamper)
summonr audit verify

# Machine-readable JSON integrity report for CI/monitoring
summonr audit verify --json

# Also verify the epitaph execution receipt chain
summonr chain verify --json
```

Example failure output if a SQLite row was modified:

```text
✕ audit_log FAILED at seq 4: stored hash does not match entry content
```

### Export for SIEM Ingestion or SOC 2 Evidence Collection

```bash
# Stream structured JSON evidence bundle directly to stdout
summonr audit export --format json

# Stream line-delimited JSON (NDJSON) for SIEM ingestion (one audit entry per line
# plus a trailing signed `summonr-siem-manifest-v1` record)
summonr audit export --format ndjson

# Write a self-contained, Ed25519-signed SOC 2 evidence bundle to disk
summonr audit export ./soc2-evidence.bundle.json --format json

# Filter export to an audit window (epoch milliseconds)
summonr audit export ./q4-evidence.bundle.json given from=1767225600000 to=1775001600000
```

### Offline Verification by an External Auditor

An external auditor does not need access to the running core or its SQLite database. Given the exported bundle and the core's Ed25519 public key (`summonr identity`), they can verify the bundle signature (`summonr-bundle-v1\0`), every `audit_log` entry hash & signature, every `epitaph` link, and the `epitaph_merkle_root` completely offline:

```bash
summonr audit verify-bundle ./soc2-evidence.bundle.json key ed25519:<CORE_PUBLIC_KEY>
```

### Chain-Preserving Data Retention Pruning

To satisfy data-minimization and retention policies (SOC 2 C1.2 / ISO 27001 A.8.10) without breaking cryptographic verifiability, `summonr audit prune` removes plaintext `summons`, `events`, and `epitaph_salts` older than `N` days (skipping any summon with a pending petition) while keeping `epitaphs`, the Merkle root, and `audit_log` intact:

```bash
# Preview how many ended summons would be pruned
summonr audit prune given days=90 --dry-run

# Execute retention pruning (logged as `elegy.pruned` in audit_log)
summonr audit prune given days=90
```
