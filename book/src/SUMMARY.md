# Summary

[Introduction](introduction.md)

# Part I — Foundations (Easy to Learn)

- [Installation & core lifecycle](getting-started/installation.md)
- [Your first ritual](getting-started/first-ritual.md)
- [Finding your way](getting-started/orientation.md)
- [Glossary](getting-started/glossary.md)

# Part II — Learning by Example

- [The tutorial arc](examples/index.md)
  - [1. Pure code and validation (no model)](examples/01-no-model.md)
  - [2. Adding one bounded judgment](examples/02-first-judgment.md)
  - [3. Model reads, code computes](examples/03-model-reads-code-adds.md)
  - [4. Call a model only for ambiguous forms](examples/04-only-when-needed.md)
  - [5. Batches, parallelism, and recovery](examples/05-batches-and-budgets.md)
  - [6. Handing a ritual to your agent](examples/06-agents.md)
  - [7. Measuring quality with assays](examples/07-assays.md)
  - [8. What to build next](examples/08-ideas.md)

# Part III — Mastering the Architecture

- [Deterministic work, deliberate inference](concepts/determinism.md)
- [Language & compiler](concepts/rituals.md)
  - [Tutorial: Composing a ritual](guides/composing-rituals.md)
  - [Tutorial: Live summons & bindings](guides/live-sessions.md)
  - [Tutorial: Research & model evaluation](guides/research-workflows.md)
  - [Writing clean rituals](guides/writing-rituals.md)
  - [Validating before inference](guides/validation.md)
  - [Python authoring and strict JSON](guides/python-authoring.md)
- [Runtime, authority & budgets](concepts/core.md)
  - [Authority and capability narrowing](concepts/authority.md)
  - [Essence: limits and atomic reservations](concepts/essence.md)
  - [Durability, checkpoints, and `revive`](concepts/reliability.md)
  - [Safe building: cassettes, simulation, drills](guides/safe-building.md)
- [Operations, UI & evaluation](guides/workbench.md)
  - [Watching runs (`scry`)](guides/observer.md)
  - [Developing and testing](guides/development.md)
  - [Assays, touchstones, and drift](concepts/assays.md)
  - [Efficiency and delegation evidence](concepts/evaluation.md)
  - [Calling rituals from another agent (MCP)](guides/agents.md)
  - [Shared jobs and workers](guides/shared-jobs.md)
  - [Repositories: `git`, `gh`, and `trial`](guides/repositories.md)
- [Cryptographic evidence & cross-party pacts](concepts/evidence.md)
  - [Identity: keys, familiars, and peers](concepts/identity.md)
  - [Epitaphs, petition seals, and witnesses](concepts/epitaphs.md)
  - [Pacts: agent-to-agent contracts](concepts/pacts.md)
  - [Running pacts across three devices](guides/pacts-three-devices.md)
  - [Lessons from Marain](concepts/lessons-from-marain.md)

# Part IV — Reference & Horizon

- [Runtime capabilities](reference/capabilities.md)
- [Ritual file format](reference/ritual-format.md)
- [References and conditions](reference/expressions.md)
- [Commands and clauses](reference/cli.md)
- [The standard library](reference/standard-library.md)
- [Pure `data` operations](reference/data.md)
- [Scoped file operations](reference/file.md)
- [Scoped HTTP operations](reference/http.md)
- [The `mail` reagent](reference/mail.md)
- [The `git`, `gh`, and `trial` reagents](reference/git.md)
- [MCP-backed stdio reagents](reference/mcp-reagents.md)
- [Configuration and files](reference/config.md)
- [Outcomes and exit codes](reference/outcomes.md)

---

- [Working with bounded intelligence (directions)](directions/index.md)
  - [Live summons and persistent beings](directions/live-summons.md)
  - [Directors and contracted work](directions/directors.md)
  - [Research, harvesting, and laboratories](directions/laboratories.md)
  - [Worlds and perspectives](directions/worlds.md)
- [Proofs and cross-party contracts](planned-features.md)
- [Implementation status (`not-built-yet`)](not-built-yet.md)
