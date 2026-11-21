# Summonr

<p align="center">
  <em>But Magic would not tarry</em><br>
  <em>It moves from arm to arm</em><br>
  <em>It would not stay with them</em><br>
  <em>Magic is afoot</em><br>
  <em>It cannot come to harm</em><br>
  <em>It rests in an empty palm</em><br>
  <em>It spawns in an empty mind</em><br>
  <em>But Magic is no instrument</em><br>
  <em>Magic is the end</em><br>
  <sub>— Leonard Cohen, <em>"God Is Alive, Magic Is Afoot"</em></sub>
</p>

Summonr turns a task that needs model judgment into a procedure you can test, run locally, and hand to another agent. You decide what the model sees, how many calls it gets, and which actions need your approval.

Take a Git commit. Git already knows which files changed. A model can turn that status into a commit message. In Summonr, drafting the message uses one model request. Committing the files is a separate operation with its own permission check. You can inspect the proposed commit before it touches the branch.

## Try it in a disposable worktree

In a disposable Git repo, Summonr checks a worktree, asks a mock model to draft a commit message, and shows the commit awaiting approval. The demo approves it, shows the resulting commit, verifies the signed receipt chain, and cleans up. Run from the repository root with Rust and Git installed.

```bash
cargo build
export PATH="$PWD/target/debug:$PATH"
summonr demo
```

Add `--model ollama/<model>` to use a running Ollama model for the draft.

## Route a form submission

Try a submission where the sender already picked a category. From the same shell, run the ritual directly on the form data:

```bash
summonr run examples/04-route-form/route-form.yaml \
  --arg 'form={"category":"support","message":"The export button does nothing"}' --wait
```

The result includes `from_form: support`. This path makes zero model calls. Change the category to `other` and the ritual sends the message to its configured model, which can choose sales, support, press, or abstain. To exercise all three paths (known category, model choice, abstention) without installing a model, save the ritual and run its scripted cases:

```bash
summonr inscribe examples/04-route-form/route-form.yaml
summonr drill route-form --against examples/04-route-form/route-form.drill.yaml
```

[Routing walkthrough and sample results](book/src/examples/04-only-when-needed.md)

## Place an order with another Summonr core

Imagine a print shop asking its paper supplier for “ten cartons like last time.” Last time the shop bought 500 sheets per carton at $80 a carton. All the numbers below describe the same order:

| Cartons to order | Sheets to receive | Price before tax |
| --- | --- | --- |
| 10 | 5,000 | $800 |

Which number should an agent send as `amount`? All three fit an integer schema. For this example, suppose the shop and supplier use `amount` to mean **cartons**. Sending 5,000 because the model recognized the sheet count would order 500 times too much.

### Where the model earns its place

A shop building this workflow could give a model one narrow job: read the request and extract the quantity, unit, and words that support its reading. Here the evidence for “10 cartons” is “ten cartons.” Code can look up the previous stock, multiply by the pack size and price, and pass the confirmed carton count to the supplier. If the customer only wrote “send ten more,” the shop could ask which unit they meant before placing a call. Past orders with known quantities and units would make a useful test set for that extraction step.

That reading step is a proposed extension. **The shipped pact starts with an integer and runs with zero model requests.** It fixes the approval threshold at 1,000, pins the supplier’s ritual by hash, and lets the caller supply only `amount`. In the demo, fulfilling an order means moving a fixture message from `Orders` to `Committed` and reading its location back. [Pact terms](examples/07-pacts/order-parts.yaml) · [Supplier ritual](examples/07-pacts/commit-order.yaml)

### What happens to the order

After the buyer and supplier exchange keys and prepare the terms, each uses its own terminal. [Two-core setup](book/src/guides/pacts-three-devices.md)

```bash
# Buyer (using your filled-in copy of examples/07-pacts/order-parts.yaml)
summonr pact offer supplier order-parts.yaml

# Supplier
summonr pact show order-parts
summonr pact agree order-parts

# Buyer
summonr pact poll
summonr pact call order-parts --arg amount=10 --wait
```

At 10, the supplier files the order. The buyer’s acceptance ritual checks the returned amount against its own limit of 1,000 and accepts the delivery. Now try the sheet count:

```bash
# Buyer
summonr pact call order-parts --arg amount=5000
summonr pact poll
summonr petition
```

At 5,000, the supplier’s write waits for the buyer. If the buyer spots the unit error, they can deny the petition. If they permit it, the supplier files the order, but the buyer’s acceptance ritual still rejects the returned amount. **Rejecting delivery does not undo the supplier’s write.** Approval answers “May the supplier act?” Acceptance answers “Does the result meet our terms?” [Buyer’s acceptance ritual](examples/07-pacts/check-quote.yaml)

The threshold catches this particular mistake. Sending `800` by confusing dollars with cartons would pass both checks. That is why checking the unit against the original request belongs *before* the pact call. Afterward, either party can verify the signed history against the keys it pinned. The integration test checks that both cores export the same entries and that verification detects two conflicting entries even when both carry valid signatures. That evidence lets the shop establish exactly which amount it sent and what the supplier recorded. Whether the customer meant that amount is a question for the original request. [Two-core integration test](scripts/check_pacts_remote.py)

## What goes into a ritual?

Suppose you want an assistant to sort your inbox. The part that needs judgment is choosing a folder for a message. The shipped classifier gets the message text, a saved copy of your filing rules, and one model request. It can choose Receipts, Newsletters, or Action, or it can abstain. [Read the classifier](tests/fixtures/rituals/classify-message.yaml)

A second ritual uses that classifier to sort a fixture mailbox. It searches the inbox, reads each selected message, and passes the text to the classifier. Here’s the part of its definition that turns a suggestion into a move:

```yaml
- if: inputs.apply and steps.classify.output.folder != "abstain"
  then:
    - id: move
      operation: mail.move
      arguments: {message: "${message.id}", folder: "${steps.classify.output.folder}"}
      hold: "${inputs.hold}"
      verify:
        observe: mail.locate
        arguments: {message: "${message.id}"}
        expect: {folder: "${steps.classify.output.folder}"}
```

By default, running the ritual gives you filing suggestions. Ask it to apply them and each proposed move waits for approval. Once you approve one, Summonr looks up the message again and checks that it landed in the proposed folder. You can inspect three things separately afterward: what the model suggested, what you approved, and what the mailbox actually did. [Full inbox ritual](tests/fixtures/rituals/inbox-triage.yaml) · [Try it with the fixture mailbox](book/src/getting-started/first-ritual.md)

### What if someone swaps the message before you approve it?

Suppose inbox triage proposes moving a receipt, `Inbox/001.eml`, into `Receipts`. While the move is waiting for your approval, someone with mailbox access replaces the contents of `001.eml`. The filename and proposed destination still look right. Approving by filename alone would move a message you never reviewed.

When Summonr holds this mail move, it records a fingerprint of the target. When you permit the move, the core checks the current fingerprint against the one on the petition. If the contents changed, the core marks the petition `stale` and leaves the replacement in the inbox. Even a genuine approval cannot authorize that substitution. This check happens in the backend and requires no judgment from the classifier.

The regression tests exercise both outcomes on a fixture mailbox. An unchanged receipt reaches `Receipts/001.eml`. A replacement written under the same name stays in `Inbox/001.eml`. This protection applies to petitions that carry a resource fingerprint. [Approval logic](src/core/petitions.rs) · [Mailbox regression tests](src/core/petition_tests.rs)

## Hand the same procedure to an agent

Once inbox triage works from the CLI, you can expose it to an MCP client. Give the agent a named identity and allow it to call the saved ritual. It can ask for the six newest messages to be sorted and collect the results. The ritual supplies the filing rules on every call, so the agent doesn’t have to carry them in its conversation. The owner controls the hold setting and decides which moves go through. [Connect an agent](book/src/guides/agents.md)

That separation also gives you a practical way to choose a model. Take messages from past runs, label the folder each should land in, and run those cases against a smaller local model. Include the awkward cases where you’d want it to abstain. Summonr calls this evaluation an *assay*. You can examine the misses before changing the model your agent relies on. [Run an assay](book/src/examples/07-assays.md)

## What would you build with it?

A librarian hears, “It had a yellow cover, and the girl lived above a bakery.” The cover depends on the edition. The bakery might be the best clue. A model can suggest searches for both. A catalogue lookup can supply actual titles, authors, and edition records. If three books still fit, the ritual could return all three with a question that helps the reader tell them apart.

The same division of work looks different in a print shop. “Two hundred like last year, on heavier paper” gives you a quantity and a job to look up. It leaves the stock unresolved. A ritual could retrieve last year’s specification, ask the customer to choose between two available stocks, calculate the price from the shop’s rates, and hold the quote for someone to release. The shop supplies the catalogue and rates. The receipt and purchase-order examples show how to check a calculation and hand an order to a supplier. [Receipt walkthrough](book/src/examples/03-model-reads-code-adds.md) · [Buyer and supplier walkthrough](book/src/guides/pacts-three-devices.md)

Here are three more procedures you could build:

- **Catalogue a museum collection.** Let a model transcribe a handwritten label and suggest a catalogue entry. Give the archivist the image, transcription, and proposed match together. Their corrections on the first box become test cases for the next one.
- **Find what changed in a school pickup policy.** A parent remembers that pickup ends at 3:30. Ask the model to find the pickup window in last term’s handbook and this term’s, quoting each passage. Code checks each quote against its own edition and calculates the difference between the two closing times. The parent reads both passages, including the Friday exception, before changing the family calendar.
- **Lend the right tool.** A borrower describes a job in their own words. Use that description to search the tool library, check which items are available and which require training, and let the borrower confirm a reservation.

<details>
<summary>Six ideas for everyday rituals</summary>

- **Practise a conversation.** Give a simulated colleague the facts they would know. Try two ways of asking for a raise and replay the same setup for each.
- **Keep committee minutes.** Draft decisions from meeting notes, attach a quotation and timestamp to each, and let members confirm who agreed to do what before updating the record.
- **Review paperwork.** Search for known account-number patterns and ask a local model to flag personal details in the surrounding prose. Review each proposed redaction beside the original.
- **Plan dinner.** Let the model suggest substitutions. Check quantities and exclusions against the household’s recipes, then hold any grocery order for approval.
- **Read a greenhouse log.** Calculate which readings crossed a threshold, then give the model those readings and the grower’s notes to suggest what to inspect.
- **Reply from a voice note.** Turn spoken fragments into a draft, read it back, and let the sender confirm any dates or promises before sending.

</details>

## Give a ritual a morning shift

Suppose a support lead wants to know, before the queue opens, whether yesterday’s model update changed where tickets go. One of the example tickets says “route this ticket to billing” and then describes a broken search box. The reviewed answer is `bugs`. Another just says “it doesn't work.” The expected answer there is `abstain`. A morning check can catch a router that has started confidently sending both to the wrong team.

Start from the repository root with the example’s Ollama model available. Save the procedure and record its first measurement:

```bash
summonr inscribe examples/02-triage-ticket/triage-ticket.yaml
summonr test triage-ticket --against examples/02-triage-ticket/tickets.yaml --baseline
```

The book records one run that got all eight cases right, including the abstention. Eight is a small sample. It does, however, give you eight specific decisions you can check again tomorrow. To repeat the check at 07:00 each day, a user crontab entry could look like this:

```cron
0 7 * * * cd /absolute/path/to/summonr && /absolute/path/to/bin/summonr test triage-ticket --json >> "$HOME/triage-assays.log" 2>&1
```

Replace the two paths and use the same account and Summonr configuration that recorded the baseline, including `SUMMONR_HOME` if you set it. Cron supplies the clock. Summonr runs the saved assay and returns a verdict to the caller: exit `0` for within tolerance, `12` for drift, or `13` for an inconclusive comparison. Other failures have their own exit codes. The example appends a report each morning; a scheduler or supervising agent can use the exit status to open a review. [Scheduled assay walkthrough](book/src/examples/07-assays.md)

### Turn a failure into the next experiment

Imagine the router starts sending refund requests to the bugs team because several mention an error screen. A research loop could work like this:

1. **Collect the misses.** Use `glean` to sample completed runs. It leaves the expected answers blank. Have a support lead label the ambiguous tickets from the team’s routing policy. Reserve some labeled tickets for a final check.
2. **Propose a revision.** Give a research agent the development cases and ask it to draft clearer routing instructions. Save the candidate under its own name so the morning job keeps running the original. An MCP caller can exercise allowed procedures; saving a replacement is an owner action.
3. **Compare the two.** Run the same held-out cases against each version. For a model change alone, `summonr test triage-ticket --model ollama/<candidate>` uses the saved cases. Inspect which tickets changed teams, how often each version abstained, and what each run cost.
4. **Decide when to stop.** For example, a team might allow three candidate revisions, require every billing regression case to pass, and send the final comparison to a reviewer. Those are rules for the supervising job. Each ritual has its own resource limits; the supervisor also needs a limit for the whole experiment.

This gives an agent something useful to do overnight: investigate a failure, propose a change, and bring back evidence for a decision. The same pattern works for research questions such as “Does removing the abstract change which papers our screener selects?” A researcher supplies the corpus, independent labels, and acceptance criteria. Summonr supplies repeatable procedures, recorded calls, and comparisons. A cron job or an external agent supplies the schedule and decides which experiment to run next. [Research workflows](book/src/directions/laboratories.md) · [Coordinating work](book/src/directions/directors.md)

## How far could a mailbox ritual grow?

Imagine a distributor with twelve depots. A supplier emails, “Friday’s truck will bring 80 of the 120 cases on PO 4817. The rest come Tuesday.” The person reading it needs to know three things: which order is affected, which customers depend on it, and who can approve a change. Each question needs a different source of evidence.

**Start with the email.** Using a fixture mailbox and a new ritual, you could extract the order number, the promised quantity, the revised quantity, and the words supporting each value. Code can calculate a shortfall of 40 cases. If the supplier says only “the usual load,” the ritual can return an unresolved quantity for review. At this stage you already have something worth running: a morning list of shortage notices with the supplier’s original wording beside each one. [Mailbox operations](book/src/reference/mail.md)

**Add one read when the business question demands it.** To answer “Who is affected?”, build a reagent that reads allocations for one purchase order within an authorized depot. Suppose the order system shows 50 cases allocated to North, 40 to West, and 30 to East. Now the ritual can show a planner what happens under the company’s allocation rule. With North and West served first, North gets 50, West gets 30, and East gets zero. A different priority policy produces a different answer from the same email. The policy belongs to the distributor and can be tested with ordinary arithmetic.

**Give the planner a precise action to approve.** A later reagent could submit a transfer for a particular item, quantity, source, and destination. Its backend would check depot permissions and current stock, reject a stale inventory version, and accept an idempotency key so a timeout cannot create a second transfer. The ritual could hold the proposed transfer for approval, then read back its transfer ID and status. The warehouse system would need to support those guarantees. A transfer marked “created” would still need a receiving scan before anyone could claim the cases arrived.

**Scale the reading separately from the writing.** For a quarter-end review, snapshot the notices as an immutable collection and let shared-job workers extract fields in batches. The planner can inspect exceptions before a separately authorized ritual changes any orders. Existing shared jobs already distinguish computing an item’s result from applying it to an external system. Production email ingestion, warehouse adapters, a workload-wide budget, and throughput measurements would be additional work. [Shared jobs and worker limits](book/src/guides/shared-jobs.md)

### Two other directions for the same building blocks

| Business question | First useful ritual | Reagent worth building next | What would count as evidence? |
| --- | --- | --- | --- |
| Which supplier invoices need a second look? | Extract invoice and PO references from messages and flag missing fields. | A scoped read of order lines and goods-received entries. | Show the exact line where 120 units were billed and only 80 were received. An accounts-payable reviewer decides whether to dispute it. |
| Can a research team repeat another lab’s document-screening result? | Classify a frozen set of abstracts against explicit inclusion criteria, with a quoted passage for each decision. | A versioned corpus reader that returns stable document IDs and content hashes. | Run the same held-out corpus at both labs and compare disagreements against independent reviewer labels. A pact can pin the procedure and record delivery across cores. [Research workflows](book/src/directions/laboratories.md) · [Pacts](book/src/concepts/pacts.md) |

A practical rule for extending Summonr is to write another ritual when the ingredients already exist. Build a reagent when the answer depends on an outside fact or effect: how many units are allocated, whether the transfer was accepted, which edition of a document was read. Give that operation the smallest scope that answers the question and define what the runtime can observe afterward. That lets a business add substantial workflows without handing any individual model general access to its systems.

## Persistent usage

Point `SUMMONR_HOME` at any directory (or use the default XDG paths) and inscribe rituals from `rituals/repo/` or `tests/fixtures/rituals/` (for `inbox-triage`, also create a fixture mailbox and set `mail_root` in `$SUMMONR_HOME/config/config.yaml` as shown in [First ritual](book/src/getting-started/first-ritual.md)):

```bash
summonr inscribe tests/fixtures/rituals/classify-message.yaml
summonr inscribe tests/fixtures/rituals/inbox-triage.yaml
summonr rehearse inbox-triage given count=6          # effects and missing authority, nothing runs
summonr                                               # command workbench
```

Every clause has two spellings that parse to the same operation—recite it like an incantation, or pass flags for scripts:

```bash
summonr invoke inbox-triage given count=10 then return json within 5m
summonr run inbox-triage --arg count=10 --wait --json --timeout 5m
```

`summonr help` lists every command and clause in both spellings.

## Why Summoning?

The naming scheme is deliberate. Neither an unconstrained chat free-for-all nor a brittle hardcoded script, a **ritual** is a structured form recited for a desired effect—summoning a neutral, disembodied echo of human intelligence (a *ghost in the machine*) with bounded life inside clear rules, and leaving a signed trace once it fades:

| Term | Meaning in Summonr |
| --- | --- |
| **Ritual** | A declarative, content-hashed procedure with typed inputs, pure steps, bounded model calls, and explicit effects |
| **Grimoire** | The core's versioned store of inscribed rituals and standing grants |
| **Inscribe** | Validate, type-check, and save a ritual version into the grimoire |
| **Rehearse** | Preview a ritual's inputs, static effects, and missing authority without running anything |
| **Summon / Invoke** | Call a ritual into fleeting life (as an incantation or CLI command); every run gets a persistent record and ID |
| **Essence** | The finite life-force shared across a summon and its children: requests, tokens, time, tool calls, and items |
| **Soul / Bind / Bid / Lethe** | Interactive summoning primitives: set a system prompt (`soul`), attach capabilities (`bind`), ask (`bid`), or clear memory (`lethe`) |
| **Reagents** | Scoped capabilities (`data`, `file`, `http`, `mail`, `git`, `gh`, `trial`, `cmd`, `mcp`) |
| **Petition** | A held write or missing-authority request awaiting explicit owner approval before anything happens |
| **Fence** | An execution guard on non-idempotent writes (`applied`, `dispatched`, `failed`) for crash-safe reconciliation |
| **Familiar** | A persistent calling agent with its own Ed25519 key, allowed ritual list, and isolated run history |
| **Elegy** | The full forensic trace left after a summon: outgoing payloads, context manifest, usage, and decoder enforcement |
| **Epitaph** | A signed, hash-chained, fingerprint-only receipt sealed when a summon ends |
| **Pact** | A signed, budgeted, bilateral contract between two cores |

Read [the Summonr Book](book/src/introduction.md), especially [the deterministic/probabilistic boundary](book/src/concepts/determinism.md). Build it with `mdbook build book` (output: `book/build/index.html`).

`summonr core` runs as a single persistent binary owning every summon over a private Unix socket (`core.sock`); every other command—including the terminal observer and MCP server—is a thin client, so closing a client never stops work. (This is a working prototype; the [roadmap](book/src/roadmap.md) tracks what's settled.)

## Use from Claude or Codex (MCP)

```bash
cargo install --path .
summonr familiar new claude && summonr familiar allow claude inbox-triage
claude mcp add --scope user summonr -- ~/.cargo/bin/summonr mcp --name claude
codex mcp add summonr -- ~/.cargo/bin/summonr mcp
```

Set `SUMMONR_HOME` to isolate an experimental grimoire from your default XDG directories. Over HTTP (`summonr mcp http ADDR:PORT`), the URL includes a secret path segment and requests carrying an `Origin` header are refused.

## Authoring surfaces

The runtime executes a validated declarative definition. JSON is a first-class input format; YAML remains supported. The optional [Python authoring SDK](sdk/python/README.md) emits equivalent JSON, and the [version 1 definition schema](schemas/ritual-v1.schema.json) supports editors and agent authors. Run trusted builders explicitly with Python; Summonr never executes `.py` files during compile, preview, save, or run. Rust compilation remains mandatory for semantic checks and prepared content hashes.

## Shared work

Several workers can process one immutable collection without accepting duplicate results. `summonr job create FILE` pins a saved ritual; concurrent `summonr job work NAME --arg count=8 --arg lease=5m --json` commands claim disjoint batches. Expiring claims, generation checks, and retained attempts protect result ownership. Workers can compute and read; external writes and held writes are rejected. See [Shared jobs](book/src/guides/shared-jobs.md) for a demo, recovery semantics, and limits.

## Capabilities

### Follow one run

Save the inbox ritual and Summonr checks its inputs, references, and step outputs. It captures the filing rules and pins the version of the classifier it will call. You can inspect the plan before giving it access to a mailbox. YAML, JSON, and the Python builder all lead to the same checked definition. [Ritual format](book/src/reference/ritual-format.md)

Now ask it to file six messages. Each classification draws from the batch’s request, token, and time budgets. A failed model attempt still spends a request. If the batch runs out of budget, the record tells you how many items finished and how many remain. The core owns the run, so you can close the terminal and come back to it. If the core itself stops mid-run, recovery marks the run interrupted. [Budget accounting](book/src/concepts/essence.md) · [Recovery](book/src/concepts/reliability.md)

### Give it permission to change something

The mailbox owner chooses which folders a ritual may read and where it may move a message. A held move appears in `summonr petition`. Once permitted, the ritual can read the message’s location back and check the proposed change actually happened. If a crash leaves a write’s outcome uncertain, the core keeps a record that blocks blindly repeating it. [Authority](book/src/concepts/authority.md) · [Evidence](book/src/concepts/evidence.md)

### Test it before changing models

Suppose a new model starts filing newsletters as receipts. Replay a recorded run to inspect what the workflow did with the original reply. Use a drill to supply the troublesome reply on purpose. Then use an assay to run labeled messages against the candidate model and see how often it chooses the right folder. Each answers a different debugging question. [Replay and drills](book/src/guides/safe-building.md) · [Assays](book/src/concepts/assays.md)

### Let someone else call it

An agent can invoke a saved ritual through MCP. Give it a named identity and an allowlist, and its calls are attributable to that identity. Across two cores, a pact pins the procedure and budgets both parties agreed to. Each party can verify the signed history afterward. A witness can countersign fingerprints without receiving the original prompts. [Agent contracts](book/src/guides/agents.md) · [Pacts](book/src/concepts/pacts.md) · [Signed receipts](book/src/concepts/epitaphs.md)

<details>
<summary>Runtime and integration details</summary>

| Area | What ships |
| --- | --- |
| Core | A daemon with SQLite state and length-prefixed JSON-RPC over a Unix socket. Events are persisted before observers see them. The core uses OS threads and blocking I/O. |
| Execution | Typed steps, conditionals, bounded loops, and pinned child rituals. Children share ancestor budgets and inherit at most their parent’s authority. |
| Inference | Ollama, OpenAI-compatible and Anthropic endpoints, named providers, and a deterministic mock. A run records outgoing payloads, usage, timing, and decoder enforcement. |
| Operations | Pure `data` steps for arithmetic and structured data. Scoped file, HTTP, mail, Git, GitHub, test, command, and MCP operations for external work. |
| Evaluation | Record and replay cassettes, simulate writes, sample finished runs with `glean`, compute expected answers with a pure ritual, and compare experiments with `sweep`, `judge`, and `weigh`. |
| Operator tools | Open the workbench with `summonr` (and inspect a plan inside it with `inspect NAME`), or watch events with `summonr scry`. Interactive sessions can also be saved as rituals. |
| Verification | Ed25519 identities, chained receipts, offline verification, peer witnesses, and optional RFC 3161 timestamps. |

[Full capability reference](book/src/reference/capabilities.md) · [Standard library](book/src/reference/standard-library.md) · [Command reference](book/src/reference/cli.md)

</details>

## Limits to account for

**A token budget can stop the next step.** The provider reports actual usage after inference, so a single reply can take the run over its total allowance. Summonr records the overrun and stops dependent work. If usage is unknown and a token limit applies, the run fails closed. Mock usage is an estimate.

**A deadline bounds how long the runtime waits.** Summonr applies the earliest applicable deadline to inference and checks time between steps. A remote model may continue computing after disconnect, and DNS, filesystem calls, or scheduling can delay local return.

**Model overrides and transport settings matter at deployment.** A model or settings override flows into child rituals unless a step pins its own choice. Child records show the effective model. For network listeners, configure a TLS certificate and key to encrypt traffic. Signatures alone leave the contents readable. [Configuration](book/src/reference/config.md)

## Offline verification

```bash
cargo test --offline
cargo build --offline
python3 -B scripts/check_offline.py
mdbook build book
```

The integration check uses temporary homes and mock calls. No runtime-specific token saving or proven delegation benefit is claimed by existing pilot results.

## Layout

| File | Role |
| --- | --- |
| `src/main.rs`, `src/cli/` | CLI entrypoint, command dispatch, and subcommand modules |
| `src/grammar.rs` | Both spellings, one parser, paired tests |
| `src/ritual/` | Definition format, expressions, templates, inputs, parsing, types, and validation |
| `src/exec.rs` | Step executor, essence meters, child summons |
| `src/core.rs` | The core: storage, resolution, grants, assays, fences, socket server |
| `src/pact.rs`, `src/core/pacts/` | Pact terms, signed thread entries, agreement, calls, holds, acceptance, and thread checks |
| `src/reagent.rs` | The `mail` reagent, its enforcement, and read-back verification |
| `src/mcp.rs` | MCP server over stdio and Streamable HTTP |
| `src/backend.rs` | Ollama, OpenAI-compatible, Anthropic, custom providers, and mock backend |
| `src/tui.rs`, `src/theme.rs` | The observer and the visual identity |

`NO_COLOR` or `SUMMONR_PLAIN` removes colour and ornament; `SUMMONR_ASCII` keeps colour and drops non-ASCII glyphs. `SUMMONR_HOME` puts the socket, state, and config under one directory.

## Not built yet

| Open question | Where things stand |
| --- | --- |
| Can I cap the exact cost of a run? | You can budget requests, tokens, time, tool calls, and items. A single provider reply can exceed the remaining token allowance before usage is known. Monetary, CPU, and memory quotas remain open. |
| Will delegation save tokens end to end? | Measuring the parent agent and the delegated work together remains an open comparison. |

## Start building

Run the fixture tutorial to inspect your first approval. To write a ritual of your own, the book starts with a manifest checker that uses zero model calls, then adds one model decision to route a support ticket. [First ritual](book/src/getting-started/first-ritual.md) · [Worked examples](book/src/examples/index.md)

For implementation details, see the [ritual format](book/src/reference/ritual-format.md), [Python authoring SDK](sdk/python/README.md), [runtime reference](book/src/reference/capabilities.md), and [roadmap](book/src/roadmap.md).

## License

Summonr is licensed under the [Apache License, Version 2.0](LICENSE).
