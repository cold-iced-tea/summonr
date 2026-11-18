# Deterministic work, deliberate inference

> *But Magic would not tarry*  
> *It moves from arm to arm*  
> *It would not stay with them*  
> *Magic is afoot*  
> *It cannot come to harm*  
> *It rests in an empty palm*  
> *It spawns in an empty mind*  
> *But Magic is no instrument*  
> *Magic is the end*  
> — Leonard Cohen, *"God Is Alive, Magic Is Afoot"*

Touch deterministic law wherever the problem admits it; save inference for the questions that still require a judge.

A real task almost never consists of one kind of labour. Parsing a wire format, summing integer cents, checking a permission, and reading an ambiguous letter are distinct acts. Hiding all four inside a single prompt erases their differences and pays token and uncertainty costs for work that ordinary code already settles cleanly.

Summonr's purpose is to keep that border sharp and make every crossing deliberate.

## A recurring pattern

```text
known inputs
    ↓
deterministic parsing, filtering, lookup, calculation
    ↓
ambiguity that requires judgment?
    ├── no ──────────────────────────────┐
    └── yes → bounded inference          │
                    ↓                    │
          proposed answer or plan        │
                    └────────────────────┘
                              ↓
           deterministic validation and policy
                              ↓
             explicit execution and observation
```

This pattern can repeat within one task. A model might reason about how to solve a new problem, produce a constrained plan, then let deterministic operations carry out the repetitive work. A later observation may reveal new ambiguity and justify another bounded inference call. The boundaries belong in the procedure, not in an instruction asking the model to remember them.

## Allocate work by method

| Work | Preferred method | Why |
| --- | --- | --- |
| Parse valid structured input | Parser plus schema | The format defines the rules; reject malformed input explicitly |
| Add monetary amounts in minor units | Checked integer arithmetic | No inference or floating-point approximation is needed |
| Select fields or count items | Pure transformation | Inputs determine the result completely |
| Enforce permissions and budgets | Runtime checks | A probabilistic answer cannot grant authority |
| Interpret ambiguous natural language | Bounded model call where appropriate | The mapping may require contextual judgment |
| Propose an unfamiliar solution | Model reasoning with explicit inputs and output contract | Reasoning can supply a candidate procedure or decision |
| Validate a proposed result | Deterministic predicates where available | Separate an acceptable representation from a justified action |
| Apply an external change | Explicit adapter operation | Execute only within declared authority and policy |
| Establish whether a change happened | Read-back or independent evidence | A generated claim is not an observation |

A deterministic method may be an algorithm, database query, parser, type checker, established solver, or ordinary application API. It need not be a fragile set of ad hoc keywords. Conversely, a “rule” that guesses intent from a word is still a fallible classifier, even if its output is deterministic.

## Three distinctions that must remain separate

**Deterministic versus probabilistic** describes how an answer is produced. A temperature setting or schema-constrained decoder does not turn model reasoning into a proved algorithm.

**Correct versus incorrect** describes whether the method solves the intended problem. A deterministic implementation can contain bugs, encode the wrong business rule, or use stale input. Testing and specification still matter.

**Authorized versus unauthorized** describes which effects may occur. Neither a correct classification nor high confidence grants permission. Summonr checks authority outside the model.

There is also a difference between a pure calculation and deterministic I/O. A pure operation returns the same output for the same explicit input. A mailbox read depends on external state, which another actor can change. A repeatable algorithm does not make its environment immutable.

## What the runtime provides

Pure `data` operations parse JSON, select fields, count, sum integers, and assert conditions. They do not access the network, filesystem, clock, or models. Declare them explicitly and give deterministic-only rituals `essence.requests: 0` to make unintended inference impossible.

Contracts expose an `execution_class`: `pure_deterministic`, `deterministic_with_io`, or `model_dependent`. The effect plan labels each operation's determinism, and `model_sites` lists declared model steps, including child steps. This is a structural inventory; branches and loops mean it is not an exact prediction of how many calls will run.

A model response is parsed and checked before use. Abstention blocks writes in its execution scope. Writes remain subject to runtime authority, may be held for the owner, and can carry a deterministic read-back check. None of these checks proves that a semantically ambiguous judgment was correct.

## Reuse the solved part

When a task has been solved once, identify what can become a stable procedure. Keep explicit inputs, exact transformations, invariants, and a narrowly defined judgment point. Reuse that procedure without making a frontier model reconstruct every mechanical step.

The potential benefit is fewer repeated planning decisions, less parent context, fewer opportunities to improvise an unsafe action, and a smaller amount of inference devoted to genuinely unresolved work. These are hypotheses to measure across the entire workflow, including authoring, repair, retries, and review. More code or a smaller model does not automatically produce savings.

## An authoring checklist

1. Write down the actual question that needs judgment.
2. Move known transformations and constraints into deterministic operations.
3. Pass the smallest sufficient evidence to each model call.
4. Treat the model's output as a proposal with an explicit failure or abstention path.
5. Validate what can be validated without another model.
6. Execute effects under runtime policy and observe their result.
7. Measure the complete workflow; retain failures and uncertainty.

The [receipt example](../reference/data.md) demonstrates a workflow with no judgment point and therefore no inference at all. The inbox example has a real judgment point—classification—surrounded by deterministic selection, permissions, held actions, and verification.
