# Rites: multi-stage workflows with gates and assumptions

A **rite** (plain spelling: `workflow`) coordinates multiple pinned rituals across a directed acyclic graph of stages, with explicit **gates** and **assumption dependencies**.

## Completion vs. acceptance

Every stage tracks execution (`status`: `pending`, `running`, `completed`, `failed`, `reopened`) separately from acceptance (`gate_status`: `waiting`, `pending_owner`, `accepted`, `rejected`, `invalidated`). A downstream stage only becomes ready once every upstream stage in `after:` is both `completed` and `accepted`.

- **`type: owner` gate:** Files an ordinary `petition` (`operation: rite.gate`) so the owner approves or denies the stage through `summonr petition permit/deny`.
- **`type: touchstone` gate:** Runs a pinned pure ritual (`essence.requests: 0`) against the stage output.
- **`type: assay` gate:** Runs `summonr assay` against the stage's ritual and accepts only when within tolerance.

## Assumptions as dependencies

Assumptions are declared on the rite (`assumptions:`) and referenced by stages (`assumes:`). When an assumption turns out to be false:

```bash
summonr rite falsify RITE_ID ASSUMPTION_ID because "reason"
```

Summonr marks the assumption `falsified`, cancels any pending owner gate petitions on affected stages, and transitions every stage referencing that assumption—plus all transitive downstream stages—to `reopened` with `accepted: false`. Reopening **never** automatically re-runs writes; once the assumption is re-established (`summonr rite assert RITE_ID ASSUMPTION_ID`), run `summonr rite advance RITE_ID` to execute reopened stages under normal authority and fences.

## Commands

| Keyword form | Plain spelling | Effect |
| --- | --- | --- |
| `rite rehearse FILE [given k=v…]` | `workflow rehearse …` | Compile, pin stages, and preview effects without running |
| `rite start FILE [given k=v…]` | `workflow start …` | Create a durable rite and advance to the first gate |
| `rite advance ID` | `workflow advance ID` | Run ready or reopened stages up to the next gate |
| `rite list` / `rite show ID` / `rite events ID` | `workflow list/show/events` | Inspect active, reopened, and accepted rites |
| `rite falsify ID ASSUMPTION [because T]` | `workflow falsify …` | Falsify an assumption and reopen dependent stages |
| `rite assert ID ASSUMPTION [because T]` | `workflow assert …` | Reinstate an assumption so reopened stages can advance |
