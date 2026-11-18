# Finding your way

Summonr has several entry points. Choose a concrete task first, then learn the mechanisms it needs.

| You want to… | Start here | What you will learn |
| --- | --- | --- |
| Understand the boundaries without installing a model | [Your first ritual](first-ritual.md) | Preview, grants, held actions, and records using a fixture |
| Make a local model do useful structured work | [Route a support ticket](../examples/02-first-judgment.md) | Explicit evidence, constrained answers, and abstention |
| Combine interpretation with exact calculation | [Total a receipt](../examples/03-model-reads-code-adds.md) | Model extraction followed by deterministic arithmetic |
| Give an agent a limited capability | [Calling rituals from another agent](../guides/agents.md) | Contracts, identity, and caller limits |
| Study whether a procedure remains useful | [Assays and drift](../concepts/assays.md) | Labeled cases, baselines, and inconclusive outcomes |
| Explore the broader architecture | [Working with bounded intelligence](../directions/index.md) | Live sessions, beings, directors, laboratories, and worlds |

## A working mental model

A **ritual** describes a procedure. A **summon** is a bounded model session or an execution of a ritual. A **reagent** supplies operations that the procedure can explicitly call. A **grant** authorizes a scoped external capability. **Essence** limits resources. An **elegy** records the execution.

A binding supplies material to model steps. It does not grant permission to modify that material. Declaring a tool does not grant access to its resources. A successful return does not prove the answer is semantically correct.

Definitions can be YAML or JSON. The Python SDK authors equivalent JSON; trusted Python builders are run explicitly, never executed by Summonr during validation or invocation.

## What the workbench gives you

After [installation](installation.md), `summonr` opens a command workbench with a live status sidebar. Commands use the same execution paths as the CLI. Inspect saved plans, invoke procedures, watch usage, and open records. See [The command workbench](../guides/workbench.md).

For exploratory work, use a live summon to shape context, instructions, and capabilities interactively. For repeatable work, invoke a ritual. Persistent beings retain selected memories under an explicit policy; ritual model steps receive the context their definitions supply. See [Live summons and persistent beings](../directions/live-summons.md).

## Start without real effects

The first tutorial uses the repository's demo setup and a fixture mailbox. Mock inference exercises execution mechanics without requiring Ollama. Its keyword answers are not evidence about a real model's judgment or resistance to misleading text.

The separate example sequence uses a temporary home. Keep experiments isolated from your actual grimoire and resources until you understand the permissions you are granting. Setup scripts can rebuild fixture state; follow each tutorial's setup instructions rather than combining them casually.

## Grow a capability deliberately

Begin with explicit input and one useful result. Validate the definition offline. Run a small case, inspect its record, then add failure and abstention cases. Give it external reads only when needed. Add effects with explicit scopes and appropriate verification. Build an assay before relying on a model change.

A researcher can stop at a measured experiment. A hobbyist can keep a useful local utility. A software engineer can invoke a ritual from an existing application. None requires adopting a complete agent platform.
