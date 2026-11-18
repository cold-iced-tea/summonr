# Lessons from Marain

Marain was a separate experiment, named after the Culture's common language: a protocol for agents to hand work to one another through explicit, versioned state (dependencies, open questions, executable contracts, and a session of propose, challenge, accept, submit, check and complete). It was tested in three pilots against two baselines, a competent prose handoff and a structured checklist, with fresh receiving agents and held-out acceptance checks.

It was not merged into Summonr. It was treated as research, and this page records what it found and how each finding shaped Summonr.

## What the pilots measured

Honestly reported, the headline is a null result:

- **Pilot 1** (one parser, three handoff formats): every arm passed 29/29 held-out checks. Feasible, nothing more.
- **Pilot 2** (nine fresh sessions, three tasks, revisions and stale evidence): every format scored the same, 609/614.
- **Pilot 3** (a three-module workflow engine with a cross-module revision): every format passed 355/355. Marain needed six coordination turns against four, and about 100 KB more rendered state.

No correctness advantage, cost saving, or smaller-model substitution was demonstrated. Strong agents on well-specified tasks did not need the protocol. That is itself a finding.

## The lessons

### 1. Expected answers should be computed, not labelled

The one time Marain caught something nothing else did: its version-3 contracts derived every expected output from a pure expression, not a hand label. Applied afterwards to the pilot 2 outputs, they showed that all three arms, prose and checklist included, raised the wrong exception class for malformed input. No hand-written test had covered it.

**In Summonr:** [touchstones](assays.md#touchstones-computed-expectations). A drill file or assay set can name a saved ritual with zero model requests and only pure operations. It computes each case's expected result. Summonr's own language is the formula language, so there is no second expression dialect. A touchstone failure is never counted against the ritual, the same rule Marain used for its own expected-value errors.

### 2. Approvals must bind to the exact version reviewed

Marain's most dependable mechanism: changing a contract or its source invalidated earlier acceptance and evidence, and stale operations were rejected outright. In pilot 3 it rejected every stale acceptance and completion it was given.

**In Summonr:** a held write's petition records a digest of the resource it would act on (for a mail move, the message's folder and bytes). `petition permit` recomputes it. On any change it does nothing and marks the petition `stale`. See [Evidence, petitions and elegies](evidence.md).

### 3. A changed dependency makes downstream results stale

Marain marked statements stale transitively when anything they depended on changed.

**In Summonr:** an assay baseline's fingerprint now covers every model the ritual can call (step pins, the ritual's own model, and child rituals), not only the top-level one. A change anywhere in the tree is noticed.

### 4. Ceremony has a cost

Pilot 3's extra protocol steps cost turns and context without improving correctness. A protocol earns its place only where it catches what agents actually get wrong.

**In Summonr:** none of the new mechanisms adds a step to the common path. Touchstones replace work (hand labels) rather than adding it, and petition digests are invisible until something changes. Agent-to-agent contracts, still planned, will be designed lean rather than by porting Marain's six-step session.

### 5. Evaluation must be isolated

Marain's checker ran candidate code without a sandbox and said so. An adversarial review of Summonr (2026-11-15) found the same class of flaw: an assay ran with ordinary authority and could perform real writes.

**In Summonr:** assays now go through normal admission and simulate every write unless the user says `live`. See [Assays and drift](assays.md#writes-are-simulated).

### 6. Role labels are not identities

Marain's roles (planner, implementer, reviewer) were self-declared labels. Summonr's MCP server likewise treats every connected agent as the same caller. This is the prerequisite for agent-to-agent contracts and is the next piece of design work; it is not solved here.

## What was not taken

- **The contract expression dialect.** Summonr's `data` operations and touchstone rituals do the same job without a second language.
- **The session protocol** (propose, challenge, accept, submit, check, complete). It worked mechanically but did not pay for its turns.
- **The discourse layer** (attributed assertions, challenges, stances). The pilots never needed it.
- **The M9 wire format.** Nine-bit packing gave no measured benefit. Its 3×3 grid lives on as a visual: Summonr draws a **seal** from the first 27 bits of a hash beside the hex in the workbench, the observer and the petition list. A seal helps you recognise a hash at a glance. It is not a way to compare them.
