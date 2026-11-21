# Calling rituals from another agent

The preferred integration is `summonr mcp` over stdio. A frontier model can use local capabilities without being a backend inside Summonr. The caller discovers a contract, delegates bounded work, and receives the declared result and separate execution evidence.

## Connect and discover

Give each agent its own identity: `summonr familiar new codex`, then `summonr familiar allow codex RITUAL...`, and configure its MCP client to launch `summonr mcp --name codex`. Its requests are then signed, it sees only the rituals you allowed and only its own runs, and its runs are recorded under its name. To reach a core on another device, add `--via PEER`. See [Identity](../concepts/identity.md).

Without `--name`, configure your MCP client to launch the Summonr executable with argument `mcp`; every such client is the same anonymous `agent`. Set `SUMMONR_HOME` in that server's environment to serve an isolated grimoire. Stdio carries newline-delimited JSON-RPC; diagnostics must not share stdout.

| Tool | Contract |
| --- | --- |
| `list_rituals` | Saved names, descriptions, and inputs; structured response wraps the list in `items` |
| `describe_ritual` | Language version, version/hash, input declarations, optional output schema, effects, limits, permissions, and baseline summary |
| `preview_ritual` | Resolve a saved name and inputs without classification or tool execution |
| `run_ritual` | Run a saved name with optional hash guard, idempotency key, `petition_missing`, and lower limits |
| `simulate_ritual` | Run a saved name with all writes simulated (never performed or held) |
| `record_ritual` | Run a saved name and record model/tool calls into a named core-managed cassette |
| `replay_ritual` | Replay a saved name offline from a named core-managed cassette (writes simulated) |
| `compile_ritual` | Validate and type-check an inline ritual definition offline (no host file reads) |
| `drill_ritual` | Run inline scripted drill cases against a saved ritual without real model calls or live writes |
| `check_run` | Retrieve state/result, optionally waiting up to 300 seconds |
| `get_record` | Retrieve a run's full record and bounded/paginated event trace |
| `stop_run` | Request cancellation; does not roll back completed effects |
| `list_held_actions` | Pending actions (held writes and authority petitions); only the owner can decide them |

Named cassettes belong to whoever recorded them: the owner, each familiar and each pact have their own, so one agent cannot replay or overwrite another's recordings.

Arguments are validated before connecting to the core. Unknown keys, malformed types, out-of-range waits, and file paths in place of saved names are refused. `preview_ritual` may inspect a backend fingerprint when a baseline exists; use the offline compiler for a check that makes no backend requests.

## Pin what you reviewed

Call `describe_ritual`, then pass its hash to `run_ritual`:

```json
{
  "name": "classify-message",
  "inputs": {"text": "Your receipt is attached"},
  "expected_hash": "HASH_FROM_DESCRIBE",
  "idempotency_key": "workflow-42-message-7",
  "limits": {"requests": 1, "tokens": 2000, "time": "45s"},
  "wait_seconds": 60
}
```

A stale hash refuses new execution. A caller's limits can only lower the ritual's limits. `limits` accepts `requests`, `tokens`, `time`, `tool_calls`, and `items`.

A repeated idempotency key with the same execution arguments returns the original run ID, even after restart. Reusing the key with different arguments is a conflict. Waiting is not part of execution identity. Keys are scoped to the core's caller class; this is not per-user authentication. Keep keys unique across workflows using the same server.

**Submission deduplication is not effect replay.** An interrupted run stays interrupted. Do not assign a new key merely to make an uncertain operation run again; first reconcile its effects with the owner.

## Consume results without guessing

Tool replies include both JSON text and `structuredContent`. The latter is an object; list tools use an `items` member. Run replies contain:

- `run_id`, ritual version/hash, and `state`;
- `progress`, reported request/token usage, and an evidence reference;
- the exact declared `result` once execution ends;
- historical effect events, current petition statuses, errors, warnings, and a suggested next action where applicable.

Task data is not a summary. Arrays remain arrays, long strings remain complete, and multiple return fields survive. Effect evidence is separate from task data. A task returning a field named `held` cannot create an approval requirement; current petition records determine that state. For large outputs, keep the ritual's return value bounded; arbitrary artifact pagination is not implemented.

Over stdio, up to 8 tool calls run at once, so `stop_run` or `check_run` answers while a `run_ritual` is still waiting. Replies arrive in completion order; match them to requests by JSON-RPC `id`. A ninth concurrent call is refused at once with a "server busy" error; retry it when a reply arrives.

`running` means poll the same ID. `exhausted`, `interrupted`, and `unverified` require inspecting partial work. A `returned` batch may still contain failed, abstained, or held items when its loop uses `on_failure: continue`; inspect item states and effects, not just the outer outcome.

## Authority stays outside the caller

The MCP interface cannot inscribe definitions, grant permissions, select a different model, or decide petitions. Inputs marked `user_only` may only be restated at their default. The demo's `hold` is owner-only, so a caller cannot disable it.

This boundary protects calls made through this interface. It does not isolate an agent that already has unrestricted shell access under the owner's OS account. The current MCP server is single-owner: callers sharing its credential share access to exposed rituals and run handles. Do not use it as a multi-tenant service.

## HTTP transport

For a trusted local HTTP client:

```bash
summonr mcp http 127.0.0.1:7341
```

The printed URL contains a secret. It is stored with owner-only permissions in `config/mcp_token`. Requests with an Origin header are refused; Host is checked against the listener address (with localhost aliases for loopback, plus any `mcp_allowed_hosts` or `mcp_auth.audience` host configured in `config.yaml`). The parser limits request/header sizes, body size, timeouts, and concurrent connections. Chunked requests and duplicate Content-Length headers are rejected. Optional TLS (`tls: {cert, key}`) and OAuth 2.1 Bearer JWT verification (`mcp_auth`) can be enabled in `config.yaml`.

This remains a small custom transport, not a claim of complete MCP conformance. Use stdio by preference. Token rotation while serving is not implemented, and an absent Origin is not proof that a request came from a trusted client.

## Integration checks

After building, run `python3 -B scripts/check_offline.py`. It exercises the production MCP server with a mock ritual, hash guards and idempotency, user-only input rejection, and documentation links. It runs no real model.
