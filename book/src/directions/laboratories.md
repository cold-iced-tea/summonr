# Research, harvesting, and laboratories

## The experimental cycle

A laboratory starts with a question and a procedure for answering it:

```text
question → collection → specimens → test construction
                                      ↓
findings ← analysis ← recorded experiment
```

A research question might ask whether a small model can route a particular class of messages, whether a changed prompt increases unsupported claims, or where a workflow stops behaving acceptably as input size grows.

A useful experiment defines its population, inputs, expected outcomes or measurement rules, procedure versions, resource limits, and interpretation limits. Running many examples is not enough to establish what the results mean.

## Begin with a small experiment

Start with the contact-form procedure from the examples. It routes explicit categories deterministically and asks a model only about ambiguous forms. Its bundled drill supplies model replies, so you can inspect the procedure without installing a model or collecting new data.

From the repository root, after building Summonr:

```bash
export PATH="$PWD/target/debug:$PATH"
export SUMMONR_HOME="$(mktemp -d)"
summonr inscribe examples/04-route-form/route-form.yaml
summonr drill route-form against examples/04-route-form/route-form.drill.yaml
summonr inscribe examples/04-route-form/route-form-touchstone.yaml
summonr drill route-form against examples/04-route-form/route-form.touchstone-drill.yaml
summonr core stop
```

Each drill should report three passed cases. The first checks the deterministic branch, a scripted model decision, and a scripted abstention. The second uses a pure touchstone to compute expected results and checks that explicit categories require no model call.

This establishes behavior under the supplied cases and replies. To investigate a real model's judgment, continue with [Know when it gets worse](../examples/07-assays.md), using independently labeled examples. For captured exchanges and tool behavior, use [Safe building](../guides/safe-building.md). Specimen lineage and test packages connect these tools into a broader experimental workflow.

## Specimens and harvesting

A specimen preserves source content or an appropriate reference, collection method, acquisition time where available, content digest, and permitted uses. Derived specimens link to their transformations and parents.

Collection reagents return content digests and source metadata. The specimen registry records what was captured and how it was acquired. A byte digest identifies captured content; snapshot consistency depends on the source and collection method.

Harvesting user interactions should distinguish ordinary application records from voluntary research contributions. Define what is collected, what is retained, and how sensitive content is handled. A fictional game, for example, could generate useful continuity failures without requiring that every player's private transcript become a shared dataset.

## Manufacturing tests

Test construction can combine deterministic generation and model proposals:

| Technique | Example | What establishes the expectation? |
| --- | --- | --- |
| Boundary construction | Empty, oversized, malformed, and threshold inputs | The declared contract and explicit predicates |
| Metamorphic transformation | Reorder receipt lines | A justified invariant: the sum should remain equal |
| Failure distillation | Shrink a failing document | Rechecking that the relevant failure remains |
| Adversarial variation | Add misleading instructions to source text | Defined authority and task expectations |
| Coverage-directed generation | Propose cases for missing categories | Independent labeling and coverage review |
| Production promotion | Select a real failed run | Reviewed expected results before admission to an assay |

A generated case is not automatically a labeled case. Model agreement is not a deterministic oracle. Metamorphic relations themselves need justification; some transformations change meaning in subtle ways.

Existing **touchstones** compute expectations using saved zero-request, pure-operation rituals. They are valuable where the answer is computable. **Gleaning** samples ended runs but leaves expectations empty. Assays refuse unlabeled cases, preventing the evaluated model's observed answer from silently becoming its own label.

## Different tests answer different questions

A drill exercises procedure behavior under scripted model replies. It does not establish real-model quality. Replay checks behavior against captured exchanges; it does not show that the current live service still behaves that way. An assay measures the selected cases under its comparison rules, not universal competence.

Keep held-out evaluation separate from material used to tune a procedure. Record retries, abstentions, failures, unknown usage, and incomplete work. Measure the full workflow when making efficiency claims, including preparation and review. Efficiency claims need measurements appropriate to the complete task.

## Packaging an experiment

A test package contains:

- A question, scope, and interpretation limits.
- Specimen manifests and appropriately shareable inputs.
- Pinned ritual definitions and relevant backend fingerprints.
- Label provenance, touchstones, and comparison rules.
- Drills, cassettes where appropriate, and execution instructions.
- Results, failures, resource accounting, and analysis.

Separate immutable test definitions from results of particular runs. Package validation should identify missing material and incompatible versions rather than silently substitute live data. Reproduction may still depend on hardware, backend behavior, and access restrictions.

Use these packages to qualify models for particular tasks, test reagent conformance, characterize workflow operating ranges, or support replication of research findings. A director commissions these experiments and use their qualified conclusions when selecting capabilities.
