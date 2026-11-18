# What is not built yet

Summonr implements one complete path through its design. These parts of the spec remain open or intentionally bounded, and the runtime rejects them rather than half-supporting them.

| Feature | State |
| --- | --- |
| Exact total-token ceilings on backends | Not possible: prompt tokens are known only after inference. Generation is capped and overruns stop dependent work, but one request can exceed the budget. Unknown usage fails closed. |

One deliberate deviation from the design: the core uses threads and blocking I/O rather than an async runtime. The socket protocol does not depend on it.

The [robustness roadmap](roadmap.md) lists the remaining boundaries: full reference typing, per-agent isolation, resource-version preconditions, maintained HTTP transport, broader adapters, and statistically stronger evaluation. None is implied by a passing mock test.
