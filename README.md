# Summonr

**The secure governance layer for autonomous agents.**

Unconstrained agent loops conflate **model judgment** with **system safety**: the same probabilistic loop that reads untrusted input also selects tools, constructs arguments, retries failed writes, and decides when to stop. Prompt instructions are advisory; a single prompt injection, hallucinated argument, or retry storm can escalate authority or corrupt external state.

Summonr separates judgment from execution at the runtime boundary. Every agent capability is compiled into a declarative, content-hashed **ritual** (`language_version: 1`) owned by a persistent local daemon (`summonr core`). Models supply bounded, schema-checked decisions inside isolated contexts; deterministic Rust code in `src/core.rs` and `src/exec.rs` enforces grants, resource budgets, approval holds, write fences, and cryptographic audit trails outside the model.

| Dimension | Unconstrained Agent Loop | Summonr Governance Layer |
| --- | --- | --- |
| **Authority** | Ambient credentials shared across the loop | Explicit per-ritual grants narrowed to exact scopes (`on`, `into`), checked outside the model |
| **Control flow** | Model chooses next tool and retries | Static DAG of typed steps, conditionals, and bounded loops; models only fill declared slots |
| **Budgets** | Soft prompt hints or post-hoc billing shocks | Shared ancestor `Meter` (`requests`, `tokens`, `cost`, `time`, `tool_calls`, `items`) with fail-closed enforcement |
| **Side effects** | Immediate execution on tool call | Explicit `hold` petitions with TOCTOU resource-digest checks, `effect_fence` journal, and `verify` read-back |
| **Auditability** | Free-form logs | Full `elegy` traces + Ed25519 hash-chained `epitaph` receipts, Merkle proofs, and peer witnesses |

---

## Quick start

Run from the repository root with Rust (1.87+) and Git installed:

```bash
cargo build
export PATH="$PWD/target/debug:$PATH"
summonr demo
```

`summonr demo` spins up an isolated `/tmp` Git worktree and `SUMMONR_HOME`, runs a status check, drafts a commit message with `mock/keyword` (or `--model ollama/<model>`), holds `git.commit` as a petition, permits it, verifies the signed epitaph chain, and cleans up.

---

## Active boundary enforcement: automated redaction walkthrough

Consider an automated document-redaction workflow: an agent must read an incoming document from a staging directory, identify sensitive passages, and write a redacted copy to an outbound directory—without being able to read arbitrary host paths, overwrite the original, spend unbounded API budget, or bypass human review.

### 1. Define the ritual (`redact-document.yaml`)

```yaml
language_version: 1
ritual: redact-document
description: Read a staged document, flag PII with a bounded model call, and hold the redacted write for review.
inputs:
  path: {type: string}
  hold: {type: boolean, default: true, user_only: true}
model: openai/gpt-4.1-mini
settings: {temperature: 0}
reagents:
  file: {select: [read_text, write_text, stat]}
requires:
  - reagent: file
    action: read_text
    on: {root: /srv/intake}
  - reagent: file
    action: write_text
    on: {root: /srv/redacted}
  - reagent: file
    action: stat
    on: {root: /srv/redacted}
essence: {requests: 1, tokens: 12000, cost: "$0.05", tool_calls: 3, time: 2m}
steps:
  - id: source
    operation: file.read_text
    arguments: {root: /srv/intake, path: "${inputs.path}", max_bytes: 65536}

  - id: scan
    model:
      instructions: |
        Identify personally identifiable information (names, SSNs, account numbers,
        addresses) in the document and return the redacted text with each span
        replaced by [REDACTED]. If the document is unreadable or ambiguous, abstain.
      prompt: "${steps.source.output.text}"
      schema:
        type: object
        additionalProperties: false
        required: [decision, redacted_text, findings]
        properties:
          decision: {type: string, enum: [redact, clean, abstain]}
          redacted_text: {type: string}
          findings:
            type: array
            items: {type: string}
      abstain_if: output.decision == "abstain"

  - if: steps.scan.output.decision != "abstain"
    then:
      - id: write_out
        operation: file.write_text
        arguments:
          root: /srv/redacted
          path: "${inputs.path}"
          text: "${steps.scan.output.redacted_text}"
        hold: "${inputs.hold}"
        verify:
          observe: file.stat
          arguments: {root: /srv/redacted, path: "${inputs.path}"}
          expect: {exists: true}
return:
  source_sha256: "${steps.source.output.sha256}"
  decision: "${steps.scan.output.decision}"
  findings: "${steps.scan.output.findings}"
```

### 2. Compile, rehearse, and grant least-privilege scopes

```bash
# Validate syntax, types, references, and schemas offline; save a content-hashed version
summonr inscribe redact-document.yaml

# Preview inputs, static effects, configured pricing, and missing grants (zero execution)
summonr rehearse redact-document --arg path=case-4817.txt

# Owner explicitly grants separated read and write roots (no write grant on /srv/intake)
summonr allow redact-document --from file --select read_text --on root=/srv/intake
summonr allow redact-document --from file --select write_text stat --on root=/srv/redacted
```

Every boundary is enforced in the core before and during execution:

- **Filesystem isolation:** `file.read_text` and `file.write_text` walk directory descriptors with `openat` and `O_NOFOLLOW`—no `..`, no symlinks, no writes back to `/srv/intake`.
- **Owner-locked controls:** `hold` is marked `user_only: true`. An MCP caller (`familiar`) cannot pass `hold=false` to skip review.
- **Fail-closed cost & token caps:** `essence.cost: "$0.05"` resolves the model against owner-configured `pricing:` in `config.yaml` before dispatch. If `openai/gpt-4.1-mini` is unpriced, the run refuses with zero requests sent; if a reply overruns the cap, execution halts as `exhausted`.
- **Stale-target protection (TOCTOU):** When `write_out` is held as a petition, the core records the target's digest. If the destination file changes before approval, permitting the petition marks it `stale` and aborts the write.
- **Exactly-once fencing & read-back:** Non-idempotent writes are guarded by the `effect_fence` journal (`applied` / `dispatched` / `failed`), and `verify` runs `file.stat` after the write to confirm observed state.

### 3. Execute, review the held write, and verify the receipt

```bash
# Run the ritual; the model scans the file and write_out pauses as a petition
summonr run redact-document --arg path=case-4817.txt --wait

# Inspect pending writes with proposed arguments and model findings
summonr petition
summonr petition permit p-<id>

# Verify the signed, hash-chained epitaph receipt offline
summonr epitaph s-<id> --export receipt.json
summonr verify receipt.json
```

### 4. Expose safely to an autonomous agent over MCP

```bash
summonr familiar new claude
summonr familiar allow claude redact-document
claude mcp add --scope user summonr -- ~/.cargo/bin/summonr mcp --name claude
```

The agent receives a signed `familiar` identity and sees only its allowlisted rituals (including projected `ritual_<name>` tools). It can tighten `limits` (requests, tokens, cost, time), never loosen them, and cannot inscribe definitions, grant authority, pick a different model, or approve petitions.

---

## Core governance architecture

| Layer | Implementation | What it enforces |
| --- | --- | --- |
| **Daemon & state** | [src/core.rs](src/core.rs), [src/store.rs](src/store.rs) | Single persistent core over a private Unix socket (`core.sock`) with exclusive `core.lock` ownership, transactional SQLite state, and crash recovery (`interrupted` marking). |
| **Step executor & meters** | [src/exec.rs](src/exec.rs) | Fresh model contexts per step, shared ancestor `Meter` ceilings (`requests`, `tokens`, `cost`, `time`, `tool_calls`, `items`), `effect_fence` deduplication, checkpoints (`revive` / `recast`), and bounded `retry` / `fallback`. |
| **Capability reagents** | [src/reagent.rs](src/reagent.rs), [src/reagents/](src/reagents/) | Scoped `data` (pure), `file`, `http` (DNS-pinned egress allow/deny + private-IP blocking), `sight` (issued image handles + digest-only records + base64 error scrubbing), `mail`, `git`, `gh`, `trial`, `cmd`, and owner-configured stdio `mcp_reagents`. |
| **Cryptographic receipts** | [src/epitaph.rs](src/epitaph.rs), [src/core/witness.rs](src/core/witness.rs) | Ed25519 hash-chained `epitaphs` (salted fingerprints, selective disclosure), Merkle inclusion proofs, peer witness countersignatures with fork detection, and optional RFC 3161 timestamps. |
| **Cross-core & multi-stage** | [src/core/pacts/](src/core/pacts/), [src/core/reliquary.rs](src/core/reliquary.rs), [src/core/rites.rs](src/core/rites.rs) | Bilateral `pacts` (pinned contracts with dual-party holds and separate completion vs. acceptance), `reliquary` signed ritual packs (zero-grant transfer), and `rites` (DAG workflows with `owner`/`touchstone`/`assay` gates and assumption reopening). |
| **Evaluation & drift** | [src/core/assays.rs](src/core/assays.rs), [src/core/experiments.rs](src/core/experiments.rs) | Offline `drill` scripts, cassettes (`record` / `replay`), `glean` sampling, `assay` drift checks (exit `0` / `12` / `13`), and `sweep` / `judge` / `weigh` model comparisons. |
| **MCP & transport** | [src/mcp.rs](src/mcp.rs), [src/listen.rs](src/listen.rs), [src/tls.rs](src/tls.rs) | Stdio MCP (8 concurrent workers), bounded Streamable HTTP with secret URL / Host / Origin checks, optional TLS (`rustls`), and OAuth 2.1 Bearer JWT verification (`mcp_auth` + RFC 9728 metadata). |

Every CLI clause has two spellings—keyword (`invoke ... given ... then return json`) and scriptable flag (`run ... --arg ... --wait --json`)—parsed by one grammar in [src/grammar.rs](src/grammar.rs). Authoring supports YAML, first-class JSON, the [v1 JSON Schema](schemas/ritual-v1.schema.json), and the [Python authoring SDK](sdk/python/README.md).

---

## v0.2: verifiable enterprise governance

v0.2 ships production enterprise governance controls mapped directly to SOC 2 (Security & Processing Integrity) and ISO/IEC 27001:2022 (see [Enterprise Governance & Compliance](book/src/governance/index.md)):

- **SOC 2 / ISO 27001 evidence & retention:** Signed compliance evidence bundles (`summonr audit export --format json`), line-delimited SIEM streaming (`--format ndjson`), offline third-party bundle verification (`summonr audit verify-bundle`), and chain-preserving retention pruning (`summonr audit prune`).
- **Dual-layer RBAC & separation of duties:** Ed25519 operator roles (`admin`, `policy_author`, `agent_runner`, `reviewer`, `auditor`, `security_admin`), normalized SQLite RBAC tables (`summonr rbac ...`), pre-execution reagent guards in `src/exec.rs`, `rbac.enforce`, and `rbac.require_independent_reviewer` to block self-approval of held petitions.
- **Tamper-evident SQLite audit logs:** Append-only `audit_log` with SQLite `BEFORE UPDATE`/`BEFORE DELETE` triggers, SHA-256 hash chaining, Ed25519 signatures (`summonr-audit-v1\0`), `SO_PEERCRED` actor attribution, `config.yaml` boot anchoring, atomic `summon.finished` cross-anchoring, fail-closed startup verification, and `summonr audit verify`.
- **Capability-sandboxed WASI `wasm_reagents`:** SHA-256-pinned WebAssembly reagents with unconditional `env_clear()`, explicit directory preopens (`preopen_dirs`), process-group timeout kills, bounded stdout, and fail-closed write classification (`src/reagents/wasm_reagent.rs`).

**Next (v0.3 horizon):** `M-of-N` quorum petition approvals, automatic PII redaction for stored elegy payloads, and embedded in-process Wasmtime fuel/memory-limited Component Model execution.

---

## Verification & documentation

```bash
cargo clippy --offline --all-targets -- -D warnings
cargo test --offline
cargo build --offline
python3 -B scripts/check_offline.py
python3 -B scripts/check_governance.py
python3 -B scripts/check_jobs.py
python3 -B scripts/check_stdlib.py
python3 -B scripts/check_reliquary.py
python3 -B scripts/check_rites.py
mdbook build book
```

- **Book:** [Introduction](book/src/introduction.md) · [Enterprise Governance & Compliance](book/src/governance/index.md) · [First ritual](book/src/getting-started/first-ritual.md) · [Determinism boundary](book/src/concepts/determinism.md) · [Authority](book/src/concepts/authority.md) · [Essence](book/src/concepts/essence.md) · [Sight](book/src/reference/sight.md) · [Configuration](book/src/reference/config.md) · [Robustness roadmap](book/src/roadmap.md)
- **Worked examples:** [examples/README.md](examples/README.md) (including [09-redact-document](examples/09-redact-document/README.md) for active boundary enforcement)

## License

Summonr is licensed under the [Apache License, Version 2.0](LICENSE).
