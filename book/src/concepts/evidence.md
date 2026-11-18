# Evidence, petitions and elegies

Summonr is careful about the difference between three kinds of statement:

1. **A model's claim.** "This email is a receipt." Useful, probabilistic, unverified.
2. **A tool's report.** "Moved message 42 to Receipts." Probably true, but it is the tool vouching for itself.
3. **An observation by the runtime.** Summonr read the state back afterwards and saw message 42 in `Receipts`.

Pure `data` operations add a fourth derivation label: **computed by the runtime**. A computed answer can still be based on wrong inputs or an inappropriate rule. These labels describe evidence, not semantic certainty.

## Model output is a claim

The effect plan marks every model step: *"output is a model's claim; its structure is checked"* when it has a `schema`, or *"no structure declared"* when it does not.

With a schema, Ollama is asked to constrain decoding to it, and the parsed output is checked against it again afterwards. Output that is not JSON, or does not match, makes the step `invalid`. The record says whether structure was *"enforced by decoder, then checked"* or *"checked afterward only"* (the mock backend cannot constrain decoding).

A model can also **abstain**. `abstain_if: output.folder == "abstain"` marks the result `abstained`. Writes in that execution scope are refused; reads and pure computations may still consume the value.

## Verified writes

An operation can carry a `verify` block:

```yaml
- id: move
  operation: mail.move
  arguments: {message: "${message.id}", folder: "${steps.classify.output.folder}"}
  verify:
    observe: mail.locate
    arguments: {message: "${message.id}"}
    expect: {folder: "${steps.classify.output.folder}"}
```

After the move, Summonr calls `mail.locate` (which must be a read tool, selected and required by the ritual) and compares each `expect` field with what it sees.

- Match: the write is recorded as **observed by the runtime**, with what was observed.
- Mismatch or failed read-back after a completed operation: the item ends `unverified`, never as verified success. The write may already have happened.
- No `verify`: the write is recorded as **reported by the tool**, and the effect plan warns: *"not observed afterward; the tool's own report is the only evidence."*
- Simulated (under `simulating`, a replay, or a drill): the write is authorized and then not performed. It is recorded with state `simulated` and evidence **simulated by the runtime**, carrying what it would have done; the `verify` read-back is skipped, and a held write files no petition. A simulated write is never "observed by the runtime". See [Safe building](../guides/safe-building.md).

Batch counts keep these apart: `applied`, `simulated`, `observed`, `held` and `unverified`.

## Petitions: held writes

An operation with `hold: true`, or `hold: "${inputs.hold}"`, is not performed. Instead, Summonr records a **petition** containing:

- the operation and its fully resolved arguments;
- the verification to run with it;
- the summon's selection (which messages it may touch);
- **because**: the outputs of the model and ritual steps in that frame, so you can see the reasoning;
- a **digest** of the resource it would act on. For a mail move this is SHA-256 of the message's folder and bytes. The list shows its prefix (and, at a terminal, its [seal](lessons-from-marain.md#what-was-not-taken)).

`hold` is only allowed on writes; holding a read is a validation error.

```bash
summonr petition                      # pending petitions, grouped
summonr petition permit p-bd5c4a92    # perform it (several ids allowed)
summonr petition permit all           # every pending petition
summonr petition deny p-bd5c4a92      # discard it
```

Permitting first recomputes the digest. If the message changed, moved, or disappeared since the petition was filed, nothing is done and the petition becomes `stale` (exit 5): approval applies to what you reviewed, not to whatever is there now. Petitions filed before digests existed are still permitted, with a warning that they are unbound.

Otherwise permitting runs the operation under the ritual's **current** grants and runs its verification. Each petition is atomically claimed as `executing` before action, then becomes `applied`, `unverified`, `failed`, `stale`, or `denied`. An interrupted claim becomes `unknown` on recovery and is never automatically retried. When you name several ids, all of them must be pending before any is acted on.

Agents can list petitions (`list_held_actions`) but cannot decide them. When a run over MCP leaves held actions, its result tells the agent to send you to `summonr petition`.

## Elegies: the complete record

Every summon leaves an **elegy**:

```bash
summonr elegy s-68df2d99          # readable summary
summonr elegy s-68df2d99 --json   # everything
summonr ledger s-68df2d99         # just the event log
```

It contains:

- ritual name, version and content hash; the effective model;
- the resolved inputs and essence;
- for each model request: the **exact payload** sent to the backend; a context manifest (instructions, each binding with its hash and size, the task); the response; reported usage and whether it is exact or estimated; load, prompt, eval and total timing; and how structure was enforced;
- every operation, refusal, check, hold, child summon, abstention, and release of a model;
- the outcome, result or partial result, and error.

Message bodies are kept out of ledger lines (a read shows `→ 351 chars`). The model payload record holds what was actually sent.

Every model step runs in a **fresh context**. Nothing carries over between steps or items except what you pass explicitly by reference, and the manifest shows exactly what went in.

## Failure evidence and privacy

`model.attempt` and `op.intent` events precede dispatch. A failed call retains its intent even when no response or token usage arrives. Do not treat unknown token usage as zero. MCP returns task data faithfully and keeps effect events separate; the evidence reference points to the full record.

Raw prompts, bindings, responses and inputs can contain private data. Keep the state directory private and limit access to the owner. Automatic redaction, encrypted storage, retention policies and familiar-specific evidence access are not implemented. Do not supply secrets to a model merely because its payload will be audited.

See [Durability and recovery](reliability.md) for storage failures, partial effects and the difference between submission idempotency and effect replay.
