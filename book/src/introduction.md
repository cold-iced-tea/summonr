# The Summonr Book

A receipt says the subtotal is $12.50. Did the items add up? Reading a messy receipt takes judgment. Adding integer cents is arithmetic. Summonr lets you put both in one saved procedure and inspect what each step contributed to the answer.

The runnable receipt example gives a model one request to extract the line amounts and the printed subtotal. The runtime checks the reply against a schema, sums the amounts in code, and fails the check if the figures disagree. A matching sum establishes internal consistency. A model that misreads two figures can still pass. To learn how often extraction succeeds on your receipts, run labeled cases against the model. [Walk through the receipt ritual](examples/03-model-reads-code-adds.md) · [Measure judgment with assays](examples/07-assays.md)

## Follow a ritual through the runtime

1. **Define the work.** A ritual declares inputs, operations, model steps, and a return value. Compile it offline to check the definition. Inscribing it saves a version with its bound documents and child ritual references pinned. [Language and compiler](concepts/rituals.md)
2. **Decide what can run.** Rehearsal exposes declared effects and missing authority. At execution, the core narrows the owner’s grants to the ritual’s requirements. A child inherits at most its parent’s authority and draws from its resource budget, including during parallel iteration. [Authority](concepts/authority.md) · [Budget accounting](concepts/essence.md)
3. **Decide what can change.** A write marked `hold` creates a petition. The owner can inspect the proposed arguments and permit or deny the action. Where the adapter records a resource digest, a change since the proposal can make the petition stale. A ritual can also specify a read-back check for the resulting state. [First ritual with a fixture mailbox](getting-started/first-ritual.md)
4. **Inspect what happened.** The detailed local record includes the model payload and usage. A signed receipt called an *epitaph* commits to fingerprints, so another party can verify the chain without receiving the prompts. A later petition decision gets its own seal. [Evidence and its limits](concepts/evidence.md) · [Verifying receipts](concepts/epitaphs.md)

## Build on a working procedure

A saved ritual can become a step in a larger one. The inbox tutorial uses a classifier inside a batch and keeps the moves subject to mailbox permissions. The same saved procedure can become a tool for an MCP client. A caller can request a tighter budget and the owner controls which rituals a named agent may invoke. [Batches and budgets](examples/05-batches-and-budgets.md) · [Calling rituals from an agent](guides/agents.md)

When another party owns the resources, two cores can sign a *pact* that pins the procedure and sets limits on calls. In the purchase-order walkthrough the supplier owns execution, the buyer can reserve approval for larger orders, and delivery and acceptance are recorded separately. This gives each party an answer to a different question. What did we agree to run? What did the supplier deliver? Did the buyer accept it? [Run the purchase-order pact](guides/pacts-three-devices.md)

Start with the fixture mailbox to see permissions and approvals without a model server. To write your own procedure, start with the manifest checker and add a model in the next example. [Your first ritual](getting-started/first-ritual.md) · [Learning by example](examples/index.md)
