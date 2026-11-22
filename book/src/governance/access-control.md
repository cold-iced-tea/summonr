# Access Control & Least Privilege

**Mapped controls:** SOC 2 **CC5.1, CC6.1, CC6.2, CC6.3** · ISO 27001 **A.5.3 (Segregation of duties), A.5.15 (Access control), A.5.18 (Access rights), A.8.2 (Privileged access rights)**

Summonr enforces **dual-layer authorization** on every sensitive action:

1. **Actor-level RBAC** determines *who* is permitted to call an administrative RPC, manage rituals/reagents, or invoke a reagent operation (`read` vs `write`).
2. **Ritual-level standing grants** (`reagent::authorize` in `src/reagent.rs`) determine *what* specific paths, hosts, or actions a ritual is scoped to touch.

Both layers must allow an operation for it to execute. Neither alone is sufficient.

## 1. Enterprise Role Model

Built-in roles are defined in `src/core/operators.rs` (`OperatorRole`) and seeded into SQLite on `Core::open` (`src/core/rbac.rs`, `seed_rbac`):

| Role | Reagent Management | Ritual Management | Execution | Governance & Audit |
|---|---|---|---|---|
| **`admin`** | Full (`*`) | Full (`*`) | Full (`*`) | Full (`*`), operator & RBAC management |
| **`policy_author`** | Manage reagents & grants; read-only reagent calls | `inscribe`, `compile`, `rehearse`, `drill`, `assay` | Dry-run / rehearsal only (cannot `invoke` live runs) | Read-only |
| **`agent_runner`** | Execute granted reagents (subject to role's reagent permissions) | Read-only (`rituals`, `contract`) | `invoke`, `cancel`, `revive`, `rite_start`, `job_*` | Read-only |
| **`reviewer`** | None | Read-only | `petition_decide`, `pact_agree/decline/accept/reject` | Separation-of-duties approval gate |
| **`auditor`** | None | Read-only | None | `audit_list`, `audit_verify`, `audit_export`, `chain_verify`, `epitaph_prove` |
| **`security_admin`** | Manage grants, familiars, peers, RBAC bindings | Read-only | None | `operator_*`, `rbac_*`, `grant`, `revoke`, `audit_prune` |

*(The legacy `author` role is retained for backward compatibility with single-operator setups.)*

## 2. SQLite RBAC Schema

Role definitions, resource permissions, and actor bindings live in four normalized SQLite tables initialized in `src/core.rs` (`SCHEMA`):

- **`operators`** — Ed25519 public key identity (`name`, `role`, `key`, `key_id`, `created`, `revoked`). Operator names are never reused; revocation sets a non-null `revoked` timestamp.
- **`rbac_roles`** — Built-in and custom enterprise roles (`name`, `description`, `builtin`, `created`).
- **`rbac_role_permissions`** — Explicit allow tuples `(role_name, resource_type, resource_pattern, action)` where:
  - `resource_type` ∈ `{ rpc, reagent, ritual }`
  - `resource_pattern` supports exact names (`mail.locate`), prefix wildcards (`mail.*`), or `*`
  - `action` ∈ `{ invoke, read, write, manage, approve, audit, * }`
- **`rbac_actor_roles`** — Many-to-many bindings `(actor_id, role_name, granted_by, granted_at, revoked_at)` indexed on active bindings (`WHERE revoked_at IS NULL`). Creating or revoking an operator via `operator_new` / `operator_add` / `operator_revoke` automatically syncs `rbac_actor_roles`.

Custom least-privilege roles (e.g., an actor permitted to call `mail.locate` but denied `mail.move`) can be created via the audited RPCs `rbac_role_create`, `rbac_permission_grant`, `rbac_permission_revoke`, `rbac_actor_bind`, `rbac_actor_unbind`, and inspected via `rbac_list` (`src/core/rbac.rs`).

## 3. Pre-Execution Reagent Guard (`src/exec.rs`)

When an authenticated operator or service invokes a ritual, `Core::make_run` (`src/core.rs`) binds an `ActorContext` onto `Run.actor` (propagated automatically to child rituals and parallel batch workers in `src/exec.rs`).

Before **any** reagent call executes—and before `target_digest`, petition `propose`, `fence_acquire`, or `reagent::perform`—`Run::guard_reagent_rbac()` (`src/exec.rs`) calls `Core::authorize_reagent()` (`src/core/rbac.rs`):

1. Maps the reagent's declared effect (`"pure"` / `"read"` → `PermissionAction::Read`; all other effects → `PermissionAction::Write`).
2. Queries live active rows from `rbac_actor_roles` joined with `rbac_role_permissions` in SQLite—so **revoking a role takes effect immediately, even mid-run**.
3. On denial:
   - Appends a signed, hash-chained `rbac.reagent_denied` entry to `audit_log` with `actor`, `target` (operation), `summon`, `ritual`, `step`, `effect`, and `reason`.
   - Emits a `rbac.denied` event on the summon ledger.
   - Returns `Err(Fail::Denied(...))`, aborting execution with zero side effects.

The same guard also runs in `Run::load_images()` (`src/exec.rs`) before `sight.read_image` reads any image file from disk.

## 4. Production Hardening & Separation of Duties

Enable strict enterprise enforcement in `~/.config/summonr/config.yaml` (`src/util.rs`, `RbacConfig`):

```yaml
rbac:
  # Require Ed25519-signed operator requests on the local Unix socket;
  # unsigned requests are refused and logged as `rbac.unsigned_refused`.
  enforce: true

  # Prevent an operator from approving petitions filed by their own summons;
  # self-approvals are refused and logged as `rbac.self_approval_refused`.
  require_independent_reviewer: true
```

## 5. Operator & RBAC CLI Commands

```bash
# Provision Ed25519 keypairs for separated enterprise roles
summonr operator new alice policy_author
summonr operator new bot-runner agent_runner
summonr operator new bob reviewer
summonr operator new carol auditor

# Register an operator with an existing Ed25519 public key
summonr operator add dave security_admin key ed25519:...

# List all operators and revocation status
summonr operator list --json

# Immediately revoke an operator (blocks both RPC dispatch and live reagent calls)
summonr operator revoke bot-runner
```
