# Directors and contracted work

## Commissioning work

A director identifies useful work, divides it, selects capabilities, and decides whether the results satisfy the goal. Summonr provides the execution substrate: defined inputs, pinned procedures, scoped authority, resource limits, and records.

Business contracting is a useful analogy. Discover capabilities before choosing a supplier. Request an approach when the problem is underspecified; request a resource estimate when the deliverable is already defined. Record the agreement before execution and assess the delivery afterward.

| Record | Question it answers |
| --- | --- |
| Proposal | What can the worker deliver, by what approach, under which assumptions? |
| Agreement | What scope, artifacts, authority, and limits are authorized? |
| Execution record | What ran and what happened? |
| Acceptance decision | Does the delivery meet the agreed conditions? |
| Change request | What newly discovered work requires a revised agreement? |

Completion and acceptance are separate. A ritual may return a valid artifact that fails the project's acceptance criteria. A failed execution may still produce useful evidence.

## Domain capabilities

A commitment reagent tracks deliverables and dependencies. A revision-aware Git reader anchors work to exact source. Named check runners supply test evidence. An artifact store retains immutable deliverables. A decision ledger links choices to assumptions and supporting records.

Each capability declares its authorization scopes, concurrency rules, and verification semantics. Narrow domain operations give the runtime concrete effects to authorize and record.

## Execution and ownership

Child rituals are pinned and draw on ancestor budgets and authority. MCP exposes allowed saved capabilities without letting a calling agent inscribe, grant, select a model, or approve held actions.

[Shared jobs](../guides/shared-jobs.md) let concurrent workers claim disjoint batches from an immutable collection. Generation and lease checks protect acceptance of results. They do not guarantee exactly-once inference, permit external writes, choose workers dynamically, or enforce a shared job-wide inference budget.

Those mechanisms support portions of coordination. They do not supply the director's judgment about priorities, task decomposition, or when to stop investigating.

## Assumptions as dependencies

The director records assumptions as explicit dependencies. If a worker assumes identifiers are globally unique, dependent artifacts can refer to that assumption. Discovering that identifiers are only locally unique reopens affected commitments and decisions.

Required fields can encourage this practice. They cannot establish that a worker identified every important assumption. Semantic review remains necessary.

Portable signed receipts and cross-party contracts connect work across parties; see [Proofs and cross-party contracts](../planned-features.md). A receipt attests to recorded execution, not establish semantic correctness or acceptance.
