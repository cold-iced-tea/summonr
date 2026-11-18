# The core and its clients

Summonr is one binary playing two roles.

- **The core** (`summonr core`) is a long-lived background process. It owns every summon, the SQLite database, and the connection to model backends.
- **Execution clients** — `invoke`, `petition`, the observer, the MCP server — is a thin client that talks to the core over a private Unix socket.

```text
  summonr invoke ─┐
  summonr petition┼──►  core.sock  ──►  core ──► SQLite (rituals, grants, summons, events, petitions)
  summonr (observer)                     │
  summonr mcp ────┘                      └──► Ollama / mock
```

## Why a separate core

**Closing a client never stops work.** If you start a long batch and close the terminal, the run continues. Reconnect with `summonr` (the observer), `summonr circle`, or `summonr await ID`.

**There is one source of truth.** Grants, petitions, and records live in one place, so the CLI, the observer, and an agent over MCP all see the same state.

**Stored before observed.** Events are written to the database before any client can see them. If something appears in the observer, it is already in the record.

`validate`/`compile` is an offline exception: it reads dependencies without starting the core.

## Lifecycle

- The core **starts automatically** the first time a command needs it.
- `summonr core` runs it in the foreground (useful for debugging).
- `summonr core stop` asks it to stop.
- If the core dies while a summon is running, that summon is recorded as `interrupted` on the next start, never as done. Completion cannot be established, so Summonr does not claim it.

## Protocol

The socket speaks JSON-RPC 2.0 with length-prefixed frames. The core uses ordinary threads and blocking I/O; the protocol does not depend on that choice.

The socket lives in the user's runtime directory (or under `SUMMONR_HOME`), never in a shared `/tmp`. See [Configuration and files](../reference/config.md).

## Model lifetime

Within a batch, the model stays loaded. When the last active summon ends, the core asks the backend to unload every model it used. If other summons are still active, models are left loaded, and the record says so (`left loaded: 1 other summon(s) active`). During an assay series, models stay loaded until the whole series is done.

## Storage and admission

An exclusive state lock is acquired before recovery. Terminal status and its event commit together; storage errors block further dispatch. The default active invocation ceiling is 16. See [Durability and recovery](reliability.md) for unknown effects, interrupted petitions, and the limits of exactly-once claims.
