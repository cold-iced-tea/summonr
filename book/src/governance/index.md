# Enterprise Governance & Compliance

Summonr is designed for organizations deploying autonomous or semi-autonomous LLM workflows in regulated environments. Rather than relying on prompt instructions to keep a model inside policy boundaries, Summonr enforces deterministic system controls in Rust at the runtime, storage, and I/O layers.

This section maps Summonr's implemented technical controls directly to **SOC 2 Trust Services Criteria (Security & Processing Integrity)** and **ISO/IEC 27001:2022 Annex A** controls, referencing exact source files and CLI commands so security engineers and external auditors can verify every claim against the codebase.

## Control Mapping Matrix

| Control Theme | SOC 2 Criteria | ISO 27001:2022 | Summonr Technical Control | Verification Path |
|---|---|---|---|---|
| **Data Leakage Prevention & Active Redaction** | CC6.1, CC6.6, CC6.7, PI1.4 | A.5.12, A.8.11, A.8.12 | Scoped reagents (`file`, `http`, `mail`, `sight`), `user_only` inputs, abstention write-blocking, SHA-256-pinned WASI `wasm_reagents` | `src/reagent.rs`, `src/exec.rs`, `src/reagents/wasm_reagent.rs`, `README.md` |
| **Role-Based Access Control (RBAC)** | CC6.1, CC6.2, CC6.3 | A.5.15, A.5.18, A.8.2 | Ed25519-authenticated operators, normalized SQLite RBAC tables (`rbac_roles`, `rbac_role_permissions`, `rbac_actor_roles`), pre-execution reagent guard | `src/core/operators.rs`, `src/core/rbac.rs`, `src/exec.rs` (`guard_reagent_rbac`) |
| **Separation of Duties** | CC5.1, CC6.1, CC8.1 | A.5.3 | Distinct `policy_author`, `agent_runner`, `reviewer`, `auditor` roles; `rbac.require_independent_reviewer` blocks self-approval of held petitions | `src/core/petitions.rs`, `src/core/operators.rs` |
| **Tamper-Evident Audit Logging** | CC7.2, CC7.3, PI1.5 | A.8.15, A.8.17 | Append-only SQLite `audit_log` with `BEFORE UPDATE`/`BEFORE DELETE` triggers, SHA-256 hash chain, Ed25519 signatures (`summonr-audit-v1\0`), `SO_PEERCRED` attribution | `src/core/audit.rs`, `src/core.rs`, `summonr audit verify` |
| **Execution Integrity & Receipt Chain** | PI1.1, PI1.4, PI1.5 | A.8.15, A.8.24 | Atomic `finish_summon` writes signed `epitaphs` + Merkle root and cross-anchors `summon.finished` into `audit_log`; fail-closed startup integrity verification | `src/core/epitaphs.rs`, `src/core.rs`, `summonr chain verify` |
| **Change & Configuration Integrity** | CC8.1 | A.8.9, A.8.32 | `config.yaml` SHA-256 boot anchor (`config.loaded`), versioned immutable `rituals` table, signed Reliquary packs | `src/core/audit.rs` (`anchor_config_on_boot`), `src/core/reliquary.rs` |
| **SIEM Ingestion & Evidence Retention** | CC7.2, C1.2 | A.5.33, A.8.10, A.8.15 | Signed SOC 2 evidence bundles (`summonr audit export --format json`), SIEM NDJSON stream (`--format ndjson`), chain-preserving retention pruning (`summonr audit prune`) | `src/core/audit.rs`, `src/cli/trust.rs` |

## Chapters in This Section

1. [Deterministic Boundaries & Redaction](deterministic-boundaries.md) — How Summonr isolates model inference from I/O, prevents data exfiltration, and enforces active boundary controls.
2. [Access Control & Least Privilege](access-control.md) — How Summonr's dual-layer RBAC model governs reagent management, ritual execution, and separation of duties.
3. [Tamper-Evident Audit Trails](audit-trails.md) — How the cryptographic receipt chain works, how tampering is detected fail-closed, and how auditors verify and export evidence.
