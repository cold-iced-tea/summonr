# 6. Hand a ritual to your agent

**Idea:** a coding agent can call your rituals as tools. It gets a *capability* with a fixed contract and budget, not a model it can steer however it likes.

Say you use Claude Code or Codex, and it often needs receipt totals or ticket routing. Without Summonr, it would prompt a model itself, or try the arithmetic in its own head. With Summonr, it calls the ritual you tested, and gets back an exact, recorded answer.

## Save the rituals and serve them

Agents can only run **saved** rituals, by name:

```bash
summonr inscribe examples/02-triage-ticket/triage-ticket.yaml
summonr inscribe examples/03-receipt-total/receipt-total.yaml

claude mcp add --scope user summonr -e SUMMONR_HOME="$SUMMONR_HOME" -- "$(command -v summonr)" mcp
```

## What the agent sees

The agent first asks what `receipt-total` is (`describe_ritual`). Abridged from the real response:

```json
{
  "description": "Read a pasted receipt, extract the line amounts with a model, add them up in code, ...",
  "execution_class": "model_dependent",
  "model_sites": [{"model": "ollama/qwen3:4b", "step": "extract"}],
  "effects": [
    {"step": "extract", "what": "ollama/qwen3:4b", "effect": "read", "determinism": "probabilistic",
     "notes": ["output is a model's claim; its structure is checked"]},
    {"step": "lines", "what": "data.count", "effect": "pure", "determinism": "deterministic"},
    {"step": "total", "what": "data.sum_integers", "effect": "pure", "determinism": "deterministic"}
  ],
  "essence": {"requests": 1, "tokens": 8000, "tool_calls": 2, "time": "2m"},
  "authority": "granted",
  "inputs": {"text": {"type": "string"}}
}
```

Before running anything, the agent knows: one model call, which model, what is a claim and what is computed, the budget, and that it touches nothing outside the run.

Then it runs it (`run_ritual` with `{"name": "receipt-total", "inputs": {"text": "..."}}`):

```json
{
  "state": "returned",
  "result": {"line_items_cents": [900, 375, 1150, 295], "lines": 4, "subtotal_cents": 2720},
  "usage": {"requests": 1, "reported_tokens": 210},
  "effects": [
    {"kind": "op", "data": {"operation": "data.sum_integers", "evidence": {"class": "computed by the runtime"}, "...": "..."}}
  ],
  "evidence": {"run_id": "s-a101…", "access": "summonr record RUN_ID --json"}
}
```

The agent gets the answer, the cost, how each value was produced, and a run id you can audit later.

## The agent can tighten limits, never loosen them

`run_ritual` accepts `limits`. An agent in a hurry can pass `{"limits": {"time": "30s"}}`. Limits only ever narrow:

| Agent passed | What happened |
| --- | --- |
| `"limits": {"requests": 5}` | Ran under the declared `requests: 1`. Asking for more has no effect. |
| `"limits": {"requests": 0}` | `exhausted` before any model call: *"request limit of 0 reached"* |

## What the agent cannot do

| It cannot | Because |
| --- | --- |
| Run a ritual file, or write a new ritual | `run_ritual` takes a saved name only; there is no inscribe tool |
| Pick a different model | There is no model parameter |
| Grant itself permissions | There is no grant tool; missing grants come back as commands for *you* to run |
| Approve a held action | Petitions are listed read-only |
| Switch off a `user_only` safeguard | Changing it is refused with exit 4 |

Compare that with handing an agent an API key and a prompt.

## Try it yourself without an agent

The MCP server speaks newline-delimited JSON-RPC on stdio, so you can drive it by hand:

```bash
printf '%s\n' \
 '{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-06-18","capabilities":{},"clientInfo":{"name":"me","version":"0"}}}' \
 '{"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":"describe_ritual","arguments":{"name":"triage-ticket"}}}' \
 '{"jsonrpc":"2.0","id":3,"method":"tools/call","params":{"name":"run_ritual","arguments":{"name":"triage-ticket","inputs":{"text":"My invoice shows the wrong VAT number."}}}}' \
 | summonr mcp
```

See [Calling rituals from another agent](../guides/agents.md) for the HTTP transport and the full tool list.
