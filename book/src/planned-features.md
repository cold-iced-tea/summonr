# Proofs and cross-party contracts

Execution evidence follows work across applications and between parties. A receipt identifies the procedure, authority, inputs, and recorded outcome; a contract establishes what each party agrees to perform.

## Ritual proofs: built

Every core has a signing identity. Ritual proofs use that identity to sign execution receipts. See [Identity](concepts/identity.md).

Ritual proofs are built as [epitaphs](concepts/epitaphs.md): a signed, chained, fingerprint-only account of every run and post-run petition decision, checkable offline, revealable piece by piece, with Merkle inclusion proofs (`epitaph_prove`) and witnessing by other cores or RFC 3161 timestamp authorities.

Every summon leaves an elegy: the exact outgoing payloads, a context manifest, usage, timing, the authority it held, and whether each write was observed by the runtime or only reported by a tool. A ritual proof makes that record portable and checkable by someone who does not trust the machine that produced it.

The contract:

- **A signed receipt per summon.** The core signs a canonical digest of the ritual hash, inputs, model and backend fingerprint, payloads, outputs, evidence classes, grants and outcome. A child summon's receipt is linked into its parent's.
- **Tamper evidence.** Receipts chain together, so removing or altering a past run is detectable.
- **Offline verification.** `summonr verify RECEIPT` (or a small standalone verifier) checks the signature, the chain and the ritual hash without access to the original core.
- **Selective disclosure.** A receipt can prove that a step happened under a given contract while keeping sensitive payloads private, by publishing digests instead of content.

What a proof establishes is narrow on purpose: that this core ran this ritual version, under this authority, and recorded these results. It does not prove that a model's answer was correct, only that it was labeled as a model's claim.

Receipt verification accounts for signing identity, key rotation, backend fingerprint provenance, and the material disclosed or redacted. A valid signature alone does not attest to an independently trustworthy backend.

## Agent-to-agent contracts

Parties have explicit identities: familiars are Ed25519 keys, a remote core is pinned as a peer, and requests and replies between devices are signed. See [Identity](concepts/identity.md).

An agent can call a saved ritual over MCP, but it cannot inscribe, grant, pick a model, or approve a held action. An agent-to-agent contract extends that boundary between parties: one agent hands work to another party's agent through a ritual that both sides can inspect before anything runs.

The contract:

- **A shared, pinned contract.** Both sides agree on a ritual by content hash. Its inputs, return schema, model sites, effects and essence are visible to both through `rehearse`, before work starts.
- **Authority on each side stays local.** The caller's grants never transfer. The callee runs the work under its own grants, narrowed to what the contract requires, as child rituals are narrowed.
- **Budgets enforced in between.** Requests, tokens, time, tool calls and items are capped by whichever side is stricter, and exhaustion ends the work with a recorded account rather than a partial claim of success.
- **Evidence returned with the result.** The callee returns a ritual proof alongside the answer, so the caller can check what was done instead of trusting the reply.
- **Held actions cross the boundary explicitly.** A write that needs approval becomes a petition visible to the party that must approve it, never something one agent can approve for the other.

## Agreement and delivery

Contract negotiation fixes the version, deliverable, acceptance conditions, and resource ceilings before execution. Changes require an explicit revised agreement. The receiving party assesses the delivered evidence against its acceptance conditions; a completed invocation and an accepted deliverable remain separate records.

## Pacts: built

Agent-to-agent contracts are built as [pacts](concepts/pacts.md) (plain spelling `contract`), including optional TLS (`tls` in `config.yaml`) and asynchronous callee-to-caller/witness push with polling fallback. Still open: pacts between more than two parties and caller holds on steps inside child rituals.
