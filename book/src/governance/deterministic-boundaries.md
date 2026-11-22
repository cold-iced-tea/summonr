# Deterministic Boundaries & Redaction

**Mapped controls:** SOC 2 **CC6.1, CC6.6, CC6.7, PI1.4** · ISO 27001 **A.5.12 (Classification of information), A.8.11 (Data masking), A.8.12 (Data leakage prevention)**

In an unconstrained agent loop, the LLM directly selects tools, constructs arbitrary arguments, and sees raw secrets in its context window. Safety depends on the model obeying system prompts—a probabilistic control that fails under prompt injection or model drift.

Summonr enforces a strict architectural separation between **model judgment** (probabilistic, bounded, side-effect-free) and **system execution** (deterministic, schema-validated, capability-gated).

## 1. Zero Ambient I/O Authority

A ritual declares its exact capability requirements upfront in `requires:`. At runtime, the executor (`src/exec.rs`) and reagent router (`src/reagent.rs`) enforce that:

- **Models never call reagents directly.** A `model:` step receives only the explicitly templated prompt (and optional verified image handles via `sight.read_image`) and must return JSON matching a compiled JSON Schema (`schema:`).
- **Every reagent call is checked against standing grants before I/O.** `reagent::authorize()` (`src/reagent.rs`) validates the exact operation and arguments (root directory, path traversal rejection, URL host/method allowlist, mailbox selection set) before any file descriptor, socket, or subprocess is opened.
- **Child rituals can only narrow authority.** When a ritual invokes a sub-ritual, `ritual::narrow_grants()` (`src/exec.rs`) intersects the parent's grants with the child's `requires:`—a child can never escalate privileges.
- **Abstained judgments block downstream writes.** If any `model:` step in the current scope abstains (`abstained: true`), `src/exec.rs` refuses all non-`read`/non-`pure` reagent calls in that scope (`Fail::Denied`).

## 2. Active Boundary Enforcement: Automated Redaction

To prevent sensitive data (PII, credentials, account numbers) from leaking to external destinations or unapproved storage, Summonr supports deterministic redaction pipelines where **code** enforces the boundary and **models** only classify or extract spans under a strict schema:

1. **Scoped read (`file.read`):** The ritual reads only from an owner-granted directory root (`on: { root: ... }`). Path traversal (`..`, symlink escapes outside the root) is rejected in `src/reagent.rs` before the file is opened.
2. **Bounded judgment (`model:`):** The model identifies sensitive spans and returns structured JSON (e.g., exact substrings or categories) validated against a strict JSON Schema. The model has no write capability.
3. **Deterministic transform (`data.*` or WASI `wasm_reagents`):** Deterministic code applies the redaction masks (e.g., `[REDACTED]`) and verifies that no prohibited patterns remain.
4. **Human-in-the-loop gate (`hold: true`):** High-risk writes can be held as a **petition** (`src/core/petitions.rs`) bound to the SHA-256 `target_digest` of the destination state. Nothing is written until an authorized reviewer approves the exact digest-bound payload.
5. **Scoped write (`file.write`):** The redacted artifact is written only to the explicitly granted output root, protected by an idempotency fence (`src/core.rs`, `fence_acquire`) so retries never duplicate side effects.

See the concrete `redact-document.yaml` walkthrough in `README.md` for a full end-to-end ritual example.

## 3. Preventing Secret Leakage in Inputs & Remote Callers

- **`user_only` inputs:** Ritual inputs marked `user_only: true` can only be supplied by the local owner or an authorized operator—never by remote familiars or MCP callers (`src/core/signed.rs`).
- **Out-of-band image handles (`sight.read_image`):** Binary images are resolved, SHA-256-verified, MIME-sniffed, size-capped, and re-authorized against grants in `src/exec.rs` (`load_images`) immediately before inference, preventing stale-file swaps or arbitrary host file reads.
- **Host-level sandboxing for custom reagents (`wasm_reagents`):** Custom external tools configured under `wasm_reagents:` in `config.yaml` (`src/reagents/wasm_reagent.rs`) are subject to:
  - **Wasm v1 magic (`\0asm\x01\0\0\0`) and SHA-256 digest pinning** verified on every single invocation (fail-closed if the `.wasm` module changed on disk).
  - **Fail-closed effect classification:** Any Wasm tool not explicitly declared `"read"` or `"pure"` defaults to `"write"`, requiring write grants, RBAC write permissions, and idempotency fencing.
  - **Process isolation:** `env_clear()` unconditionally scrubs the entire host environment (zero host env inheritance); only explicitly configured `preopen_dirs` are passed to the WASI runtime (`wasmtime`); `setsid` process-group timeout kills terminate runaway modules; stdout is hard-capped by `max_output_bytes`.

## 4. Verification Commands

```bash
# Validate a ritual's schema, expressions, and declared requirements offline
summonr compile ./redact-document.yaml --json

# Dry-run a ritual without executing any live writes or filing petitions
summonr rehearse redact-document given path=incoming/report.txt

# Inspect active standing grants for a ritual
summonr contract redact-document --json
```
