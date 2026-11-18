# 8. What to build next

The seven examples use only a handful of moves. Almost every useful ritual is a combination of them.

## The moves

| Move | Where you saw it | Use it when |
| --- | --- | --- |
| **Validate, don't ask** | 1 | The rules are known: formats, ranges, required fields |
| **Closed judgment** | 2 | You need a decision a rule can't make; give an `enum` and an `abstain` |
| **Read, then compute** | 3 | Text holds numbers or facts; the model copies them, code works with them |
| **Two readings must agree** | 3 | You can get the same fact two independent ways; `if` + `data.assert` |
| **Deterministic first** | 4 | Most inputs are already clear; branch so only the unclear ones reach a model |
| **Bounded batch** | 5 | Many items; `each` + a child ritual + essence on the parent |
| **Read the record** | 5 | An answer surprises you; the exact payload and response are kept |
| **Capability, not prompt** | 6 | An agent needs this job done; give it a saved ritual over MCP |
| **Measure and watch** | 7 | The ritual matters; an assay set, a baseline, a scheduled check |
| **Hold the write** | [first ritual](../getting-started/first-ritual.md) | A change to the world needs a person's yes; `hold` + `verify` |

## Small capabilities to build

Each of these combines `data` operations, model steps, child rituals, `each`, and `if`.

**Expense categorizer.** Input: a JSON list of transactions. Batch over them; each child ritual classifies the merchant into your categories with an `enum` + `abstain`. *Moves: closed judgment, bounded batch.* Pair it with an assay set built from last month's hand-categorized expenses.

**Invoice cross-check.** Like the receipt, but extract line items, tax, and the printed total, and require `sum(lines) + tax == total` with two `data.sum_integers` steps and an `if`. *Moves: read then compute, two readings agree.*

**Changelog entry from a commit message.** A model reads a commit message and returns `{type: [feat, fix, docs, chore, abstain], summary}` with a `maxLength` on the summary. *Moves: closed judgment.* Abstentions become "needs a human-written note".

**Config gate for CI.** Validate a JSON config against a strict schema with `parse_json`, count entries with `data.count`, and exit non-zero on failure. No model, `requests: 0`. *Moves: validate, don't ask.* The exit code plugs straight into a CI step.

**Meeting notes to action items.** A model extracts `[{owner, task}]` with closed objects and length limits. A second, cheaper step classifies each task as `this-week` or `later`. *Moves: read then compute, bounded batch.* Treat the owners as claims; nothing is sent anywhere.

**Is the small model good enough?** Take any ritual above, build an assay set from 30 real cases (see below), and run `summonr test NAME --model ollama/<smaller-model>`. *Moves: measure and watch.* Your own cases answer the question, not a leaderboard.

## Outside software: one judgment, not the whole job

The same pattern—say what the model gets to see, what it gets to decide, and what happens when it's unsure—works anywhere you want help with one messy step without handing over the whole operation:

| Use case | What the model helps with | What Summonr can enforce |
| --- | --- | --- |
| **Librarian — Find a half-remembered book** | *"It had a yellow cover, a girl who lived above a bakery, and a scene where the river froze."* Turn those clues into searches and ask one follow-up if several titles fit. | Return candidates found in the catalogue or an approved index. Show which clues match, treat cover color as a hint because editions vary, and say when the evidence is thin. |
| **Print shop — Turn a vague request into a quote** | Interpret *"200 copies like last year, but on heavier paper"* and clarify the missing specification. | Calculate the price from the confirmed job spec and the shop's rates. Hold the finished quote for approval before sending, or ask a supplier via a **pact** sharing only specs and quantity. |
| **Museum archivist — Make a collection searchable** | Transcribe old labels and suggest which people, places, or objects they refer to. | Keep the source beside each suggestion, check proposed catalogue links, hold edits for review, and use corrected first-box suggestions to assay whether a local model handles the next ten. |
| **Interview researcher — Trace themes back to the source** | Suggest themes across interviews and identify passages that support them. | Check each quotation against the transcript and use the researcher's reviewed examples to evaluate a smaller model. |
| **Tool library — Find the right tool for the job** | Turn *"I need to hang shelves on a brick wall"* into relevant catalogue searches. | Check availability and lending requirements, then let the borrower confirm the reservation. |

### Everyday & personal rituals

- **Keep a committee's decisions straight:** After a meeting, the model drafts a decision log with quotes and timestamps. The committee confirms what passed, who volunteered, and what's still open—messy discussion stays separate from the confirmed record.
- **Catch mistakes before sharing paperwork:** Before handing a packet to a landlord, school, or club, code catches obvious patterns like account numbers and a local model spots passages with personal details. You review proposed redactions against the original and export the approved copy.
- **Run a personal meal-planning ritual:** Give it the actual contents of your fridge, your recipe collection, and how much time you have. The model proposes substitutions and leftover ideas while deterministic checks enforce quantities and exclusions; any grocery order waits for approval.
- **Make sense of a greenhouse log:** Code flags when temperatures crossed a threshold or watering fell behind; the model reads the grower's notes and suggests what to inspect next, showing readings beside suggestions while the grower decides what to change.
- **Turn a voice note into an accessible reply:** Speak in fragments and get a draft in your preferred tone, with missing details or commitments flagged for confirmation, read back, and held for approval before sending.

## Growing a real assay set

Hand-written test cases go stale. Once a ritual has run on real inputs for a while:

```bash
summonr sample triage-ticket 20 --against examples/02-triage-ticket/tickets.yaml
```

This picks up to 20 real past runs whose inputs are not already in the set, and prints each as a case with the ritual's answer under `observed` and an **empty** `expect`. You fill in the correct answers and append them to the set. Assays refuse cases with an empty `expect`, so the model being tested never labels its own homework.

## Let the capability grow

A document reader becomes part of a research laboratory when you preserve its specimens and evaluate its extractions. A classifier becomes useful to a director when its contract and assay results explain which work it is suitable for. A note-taking session becomes a persistent collaborator when you define what it remembers and how those memories are corrected.

Explore [Working with bounded intelligence](../directions/index.md) to connect these small procedures to live sessions, beings, directors, laboratories, and worlds. Keep the same explicit inputs, authority, resources, and evidence as the application grows.
