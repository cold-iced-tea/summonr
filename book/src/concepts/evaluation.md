# Evidence for efficiency and delegation

The question is whether reusable capabilities reduce the inference needed by a whole workflow at acceptable quality. Keep four claims separate:

| Claim | Current evidence |
| --- | --- |
| Same payload through Summonr versus direct | Identical token usage and predictions in the recorded pilots; no runtime token saving |
| Compact formulation | 18,674 versus 26,005 reported tokens over 48 validation cases, equally available to direct inference |
| Smaller-model substitution | 21/48 correct versus compact large 39/48; small model missed 12/16 OOS cases |
| Frontier-parent delegation | Structurally prepared handoff pilot and offline checks; no completed task comparison in the reviewed artifacts |

The compact formulation's descriptive paired accuracy interval included a loss. All comparison candidates failed the declared mean-latency gate. Inconsistent model unloading/loading prevents attributing the observed latency difference to orchestration alone. Do not promote these results into proven quality equivalence, autonomous-use safety, or monetary savings.

The earlier runtime/formulation campaign totals 264 calls and 113,851 reported tokens, including rejected development candidates. Billing and energy remain unknown. Model tokenizers and computation are not interchangeable units of money.

## Prepared handoff benchmark

`benchmarks/delegation-20261114/PROTOCOL.md` specifies six paired parent sessions per arm, each with four CLINC150 validation items. Arm A completes the task without a child tool; arm B may delegate through a bounded Summonr child. The benchmark shim accepts only an item ID and allows at most four child requests per B session.

The protocol retains parent usage snapshots, cache components, child attempts, denied requests, timing, failures, and preparation cost. Missing usage is unknown, not zero. Its small pilot gate requires quality/failure preservation, at least 10% lower reported parent tokens, bounded latency, complete child accounting, no protocol violations, and observed delegation. Six case pairs cannot prove general noninferiority.

The shim is benchmark-specific. Its passing mock checks do not establish that a real parent chooses useful delegation, that a child is accurate, or that total compute falls. The production MCP integration has a separate mock regression in `scripts/check_offline.py`.

## Freeze boundaries after runtime changes

The original handoff manifest pins `target/debug/summonr`. Rebuilding the runtime changes that hash. The old runner should refuse it: that is a correct reproducibility guard, not a reason to edit the old manifest in place.

Before a real comparison, create a separately versioned preparation with the new binary and code hashes, retain the previous manifest, and declare the new runtime/protocol boundary before collecting results. Existing cases, gold data, plans and historical reports were not retuned by the robustness work.

No real inference is needed for `cargo test --offline` or `python3 -B scripts/check_offline.py`. The latter redirects the benchmark check's report away from the frozen preparation, preserving its original hashes.
