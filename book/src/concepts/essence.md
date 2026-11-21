# Essence: limits

**Essence** is a summon's resource budget. A ritual declares it:

```yaml
essence: {requests: 60, tokens: 400000, time: 30m}
```

| Field | Counts | Notes |
| --- | --- | --- |
| `requests` | Model requests admitted | A failed or timed-out request still counts. A failure is never a free retry. |
| `tokens` | Prompt + output tokens reported by the backend | See the limitation below. |
| `time` | Wall-clock time from the start of the summon | `500ms`, `30s`, `10m`, `2h` |

Every field is optional. A field you leave out is unlimited.

## Shared with every ancestor

A child ritual has its own essence **and** draws on every ancestor's. Each model request is charged to the child's meter and to each parent's. A descendant can never exceed a parent's ceiling, however generous its own declaration.

In the demo, each `classify-message` call allows 1 request and 20,000 tokens, and all of them together are bounded by `inbox-triage`'s 60 requests and 400,000 tokens.

## How limits are enforced

**Before a request.** A request is admitted only if no meter in the chain is spent. The request count is charged at admission.

**During a request.** The HTTP call to the backend gets a timeout equal to the *earliest* remaining deadline among all ancestors. That bound covers connecting and reading the response body, not just the gap between calls. For Ollama, generation is capped (`num_predict`) at the *smallest* remaining token budget, or at a smaller `num_predict` you set yourself.

**After a request.** Reported usage is added to every meter. If any meter is now over its token limit or past its deadline, the result is **rejected before any dependent step can use it**. A response that arrives late or over budget is recorded, but nothing acts on it.

**Between steps.** Cancellation and limits are checked before every step and before successful completion.

## The token limitation

Ollama reports prompt tokens only after inference. Summonr can cap *generated* tokens before a request, but it cannot know the prompt size in advance. So one request can push combined usage over the budget. When that happens, the overrun is detected and its output is rejected, but the tokens have already been spent.

If a token budget applies and the backend does not report usage, Summonr fails closed: the step fails rather than continuing blind.

Images count too. Backends report image tokens inside prompt tokens, so they count under `tokens` (and cost, if priced). A step with `images` overshoots a token budget more easily than a text step, because the image cost is also unknown until the reply. Keep `tokens` roomy on steps that show images.

Mock token counts are estimates (characters ÷ 4) and are labeled `estimated`.

## Exhaustion in a batch

When essence runs out inside an `each`, the batch stops and reports exactly where it got to:

```json
"counts": {
  "selected": 40, "processed": 23, "remaining": 17,
  "returned": 20, "abstained": 3, "invalid": 0, "failed": 0,
  "applied": 0, "observed": 0, "held": 5, "unverified": 0
}
```

The item that was cut short is not counted as processed; it stays in `remaining`. A partial batch can never present itself as complete. The summon ends `exhausted` (exit 10), and its result carries the partial per-item record.

## Cancellation

`summonr banish ID` (or `stop`, or `stop_run` over MCP) asks the whole summon tree to stop. Banishing a child stops its root. A request already in flight may finish first; nothing after it runs. The outcome is `cancelled`.

Disconnecting from a remote backend cannot prove it stopped computing. Scheduling, DNS and cleanup can delay the final return slightly, so time limits are firm bounds on what Summonr *acts on*, not a guarantee of an exact wall-clock stop.

## Deterministic work has limits too

`tool_calls` bounds admitted operation calls, including pure `data` operations. An operation with verification reserves two calls before starting, so a write cannot consume the last slot and only then discover that verification has no budget. Reserved slots are conservatively charged even when the operation fails before read-back. Held proposals are not executed tool calls; approving them is a separate owner operation outside the ended run's meter.

`items` bounds loop-item admission across a run and its children. Nested loop items also consume the shared ancestor allowance. Exhaustion preserves the partially processed item and prior effects. These are admission limits, not a measurement of CPU time or allocation bytes.

Give deterministic-only rituals `requests: 0`. Pure operations and deterministic child rituals can still execute; any model step is refused before dispatch. MCP callers can supply lower limits, never higher ones. Omitted limits remain unbounded except for the configured top-level active-run admission ceiling.

## Atomic reservations under `parallel` `each`

When an `each` step runs with `parallel: N` (`N > 1`), worker threads share the run's meter stack (`self.meters`, including every ancestor meter). Admission of `requests`, `tool_calls`, and `items` is serialized atomically across all ancestor meters before any worker dispatches a call or starts an item, so concurrent workers cannot race past a parent or child budget ceiling. Results and checkpoints are committed in deterministic item order.
