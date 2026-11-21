# Glossary

Summonr borrows the names of an older craft, yet each word designates one exact mechanism. Nothing here is ornament.

| Term | Exact meaning |
| --- | --- |
| **deterministic method** | A computation whose output follows from its inputs; its truth still depends on the rule's own soundness. |
| **pure operation** | A side-effect-free function over explicit data (`data.*`), blind to files, clocks, sockets, and models. |
| **probabilistic method** | A model's judgment or proposal: testimony to be parsed and weighed, never trusted on its own authority. |
| **validate** (`compile`) | Check a definition offline—syntax, types, scopes, and depth—and return its compiled contract. |
| **ritual** | A versioned procedure: inputs, steps, essence, requirements, and return shape, written in YAML or strict JSON. |
| **inscribe** (`save`) | Embed bound documents, pin child versions, validate, and seal a ritual into the grimoire under a content hash. |
| **grimoire** (`rituals`) | The store of inscribed rituals. |
| **summon** | One bounded run (or child call), named by an id such as `s-05ce3b2c`. |
| **invoke** (`run`) | Begin a summon. |
| **rehearse** (`run --dry-run`) | Resolve inputs and authority and print the effect plan without spending a request or touching state. |
| **reagent** | A named capability provider (`mail`, `file`, `git`, `data`, or an owner-configured stdio MCP server). |
| **sight** (`vision`) | Image input for model steps: `sight.read_image` returns a digest-only handle, and a model step's `images` list sends the checked bytes to a vision-capable model. |
| **satchel** (`tools`) | The inventory of reagents and their tools. |
| **grant** (`allow`) | Standing permission, given by the owner to one ritual, for one tool within an explicit scope. |
| **revoke** (`disallow`) | Withdraw a grant. |
| **essence** | Hard resource ceilings for a summon: `requests`, `tokens`, `time`, `tool_calls`, and `items`. |
| **binding** | A document embedded and hashed at inscription, supplied verbatim to the model steps that name it. |
| **abstain** | A model step declining to decide—a first-class outcome that blocks downstream writes in its scope. |
| **petition** (`approval`) | A proposed write held for the owner's hand rather than executed at once. |
| **permit** / **deny** | Decide a pending petition; the decision itself is sealed onto the epitaph chain. |
| **verify** | A deterministic read-back after a write, proving the world now matches what the tool claimed. |
| **elegy** (`record`) | The complete record of a summon: inputs, payloads, usage, timing, effects, and result. |
| **ledger** (`logs`) | A summon's chronological event stream. |
| **circle** (`list`) | Summons still labouring. |
| **graveyard** (`history`) | Summons that have ended. |
| **scry** (`watch`) | Open the observer UI. |
| **await** (`wait`) | Block until a summon reaches its end. |
| **banish** (`stop`) | Signal a running summon to stop; finished work and checkpoints remain. |
| **revive** (`resume`) | Resume an `exhausted`, `cancelled`, or `interrupted` root run from its last durable checkpoint. |
| **assay** (`test`) | Score a ritual against a labeled set of cases; record a baseline or test for drift. |
| **assay set** | A YAML file of inputs and expected outputs (or touchstone checks). |
| **baseline** | A recorded assay score paired with the backend model's fingerprint. |
| **drift** | Measured loss of accuracy or rise in abstention against a recorded baseline. |
| **glean** (`sample`) | Draw real past runs as unlabeled candidate cases for an assay set. |
| **cassette** | A recorded JSON transcript of one run's model and tool exchanges, used for offline replay. |
| **simulate** (`--simulate`) | Run live reads and models while turning writes into recorded proposals. |
| **drill** (`cases`) | Exercise a ritual against scripted model answers; contacts no model and performs no write. |
| **core** | The background daemon that holds the SQLite store, owns the keys, and executes every summon. |
| **observer** | The terminal window that watches active and ended summons. |

## Outcome words

Beside each ended summon, the CLI and observer set a single archaic rubric. Scripts should match the lowercase outcome key in JSON; the rubric is for the eye.

| Outcome | Rubric | Meaning |
| --- | --- | --- |
| *(running)* | LABOURS | Still in execution. |
| `returned` | AT REST | Completed and returned its value. |
| `abstained` | KEPT SILENCE | Completed, but a model step declined to judge. |
| `exhausted` | SPENT | Reached an essence ceiling. |
| `cancelled` | BANISHED | Stopped by `banish` / `stop`. |
| `invalid` | SPOKE AMISS | A model's reply failed to parse or violated its schema. |
| `failed` | FELL | A step, assertion, or authorization check failed. |
| `interrupted` | LOST | The core stopped mid-run; completion could not be established. |

Within a batch, an individual item may also be `unverified`: the write ran, but the read-back did not find the expected state.

Every summon also bears a three-glyph **sigil** hashed from its id—a mark for recognition, carrying no authority of its own.
