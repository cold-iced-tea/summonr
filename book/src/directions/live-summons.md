# Live summons and persistent beings

## A command line on a constrained model

Open a model session, supply a document, set a task, and inspect the answer. Add a second source or change the available operations as the work develops. The session keeps its boundaries explicit throughout the interaction.

The session exposes its model and settings, instructions, supplied material, available capabilities, remaining resources, and execution history. Changes are deliberate and inspectable. Session state makes each change to context, instructions, capabilities, and resources visible.

Three operations must remain distinct:

- **Supply information:** make selected content available in context or through a read capability.
- **Grant authority:** authorize particular operations over particular resources.
- **Retain context:** decide which material and exchanges enter later requests.

Reading a file does not authorize editing it. Withdrawing read access does not remove content already supplied in a conversation. Forgetting material requires rebuilding later context, including summaries that might retain it; it cannot undo actions already taken.

## Live editing

A session proposes edits against a specific document version. The proposal identifies its base content, the requested change, and a patch. Deterministic checks establish whether the patch applies, while the user assesses whether the edit meets the intention.

Applying edits uses a scoped write capability, checks the base version, and records evidence of the resulting state. Refreshing the model's view is explicit, so a document change cannot silently alter the meaning of an earlier answer.

## From exploration to procedure

An interactive success can reveal a reusable method. Authoring support helps extract stable inputs, bindings, judgment points, checks, budgets, and return contracts from the experiment.

A successful transcript alone is insufficient. Incidental instructions may be essential; a favorable answer may hide a brittle method. The resulting ritual still needs offline validation, failure cases, and evaluation.

## What makes a being

A persistent being carries identity, role, memory policy, and perspective across invocations. Its actions execute through bounded rituals. Memory retrieval and updates follow explicit rules and retain evidence; a personality description is not permission.

An archivist might retain accepted findings and unresolved questions. A fictional character might retain witnessed events and attributed rumors. Both need a distinction between established state and the model's interpretation of it.

Application-owned records accessed through declared operations supply continuity. Memory policy defines retention, correction, deletion, and retrieval. Context changes invalidate dependent material where necessary, and the session contract identifies which resource limits span multiple interactions.
