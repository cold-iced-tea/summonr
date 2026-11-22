# Safe building: cassettes, simulation, drills

A ritual that calls a model is slow to iterate on, and one that writes is risky to test. Summonr gives you three ways to work on a ritual without either cost:

- **Record and replay** save a run's model calls and tool reads to a *cassette* and re-run from it, offline. A replay never performs a write.
- **Simulate** runs a ritual for real, except that writes are authorized and then not performed.
- **Drills** run a ritual's test cases against scripted model answers. No model is contacted and no write happens.

All of these are command-line features. The MCP server offers none of them. Output below comes from real runs with `mock/keyword`, with colour off; IDs and hashes vary.

## Setup

The examples use [route-form](../examples/04-only-when-needed.md) and the demo's `inbox-triage` (see [Your first ritual](../getting-started/first-ritual.md) for preparing the demo mailbox). route-form uses only `data` operations and a model, so it needs no grant. The cassette directory must exist before you record.

```console
$ summonr inscribe route-form.yaml
$ mkdir cassettes
```

## 1. Record

Recording captures what a run actually did, so you can replay it without a model.

```console
$ summonr invoke route-form given 'form={"category":"other","message":"I write for a magazine and would like an interview"}' through mock/keyword recording cassettes/rf.json then return
{
  "from_form": "other",
  "from_model": "abstain"
}
* ELEGY KEPT SILENCE (abstained) · 00:00 · 1 requests · 82 tokens
```

The flag spelling is `--record cassettes/rf.json`. The cassette is a JSON file, written with mode 0600, atomically after every entry:

```console
$ ls -l cassettes
-rw------- 1 you you 1226 Nov 14 21:23 rf.json
```

```json
{
  "cassette_version": 1,
  "entries": [
    { "kind": "model", "step": "judged", "model": "mock/keyword", "request_hash": "25fecc…",
      "payload": { "...": "..." }, "content": "{\"category\":\"abstain\"}",
      "prompt_tokens": 76, "output_tokens": 6 }
  ]
}
```

Every successful model call and every read or write tool call is an entry. Pure `data.*` operations are not recorded: they depend only on their arguments, so a replay computes them again. Each entry carries a hash of its request: model id, prompts, schema and settings for a model call; the operation and arguments for a tool call. Timeouts and token caps are left out of the hash, so changing a limit does not invalidate a cassette. Failed model calls are not recorded. If a save fails after the run has started, that step fails with "cassette could not be saved: …"; a run is never silently left with an incomplete cassette. Recording to the path of an existing cassette replaces it. An existing file that is not a cassette is never replaced: the invoke fails with exit 2, as it does for an unwritable path, before anything runs. A cassette may also be written by hand as YAML; Summonr loads `.yaml` and `.yml` files but always saves JSON.

## 2. Replay

Replay lets you change the parts of a ritual that are not the model or a tool read, and run it again, with no backend at all. Editing a `data` step is fine: its new value is computed.

```console
$ summonr invoke route-form given 'form={"category":"other","message":"I write for a magazine and would like an interview"}' through mock/keyword replaying cassettes/rf.json then return
  warning drift not checked during replay
{
  "from_form": "other",
  "from_model": "abstain"
}
* ELEGY KEPT SILENCE (abstained) · 00:00 · 1 requests · 82 tokens
  release    mock/keyword: not applicable (replay)
```

Replay makes no backend request of any kind: no model, no unload, no drift fingerprint, hence the warning. Tool calls that need a grant are authorized before the cassette is consulted, so a recorded call the ritual is no longer allowed to make is still refused. Pure `data.*` operations are computed live, exactly as in a live run: they are charged against `items`, `tool_calls` and the deadline the same way, so tightening `essence.items` and replaying is exhausted where a live run would be. Any entry an older cassette holds for a pure operation is ignored. Replayed model responses are marked as not decoder-constrained, since no decoder ran.

**Replay always simulates writes.** A write is authorized as usual and then not performed, and the cassette is never consulted for it. It is recorded as `simulated` with evidence "simulated by the runtime", and a held write files no petition. You do not need `simulating` for this; a cassette recorded with or without `simulating` replays the same way.

A request with no matching entry fails the step. It never falls back to a live call:

```console
$ summonr invoke route-form given 'form={"category":"other","message":"different"}' through mock/keyword replaying cassettes/rf.json then return
  warning drift not checked during replay
{
  "partial": {}
}
* ELEGY FELL (failed) · 00:00 · 1 requests · 0 tokens
  step judged: no recording for model step `judged` (request 773af9888830); re-record the cassette
  release    mock/keyword: not applicable (replay)
$ echo $?
11
```

Recorded tool errors replay as the same error kind; an error kind Summonr does not know makes the cassette invalid (exit 2). A cassette path that does not exist is exit 3; a file that does not parse as a cassette is exit 2. Mail replay still needs the fixture mailbox present, because mail authorization looks up folders.

Combining `recording` with `replaying` is refused with exit 2, and so is any of `recording`, `replaying` or `simulating` with `rehearse` or `--dry-run`:

```console
$ summonr invoke route-form given 'form={"category":"support","message":"hi"}' recording a.json replaying cassettes/rf.json
summonr: replaying cannot be combined with recording
$ echo $?
2
```

## 3. Simulate

Simulation answers "what would this change?" against real data. It reads and runs pure operations normally; a write is authorized, then *not performed*.

```console
$ summonr invoke inbox-triage given count=6 apply=true hold=false through mock/keyword simulating then return
```

The keyword mock files one of the six messages. Its move is recorded like this:

```json
{
  "evidence": {
    "class": "simulated by the runtime",
    "would": { "operation": "mail.move", "simulated": true,
               "arguments": { "folder": "Action", "message": "2026-11-11" } }
  },
  "operation": "mail.move",
  "state": "simulated",
  "step": "move"
}
```

The ledger records that the read-back was skipped, because there is nothing to read back:

```console
$ summonr ledger s-f35dbebc3cad
  +00:00.0 - move: dispatch mail.move
  +00:00.0 - mail.move {"folder":"Action","message":"2026-11-11"} → {"arguments":…,"simulated":tru…
  +00:00.0 - move check skipped: simulated
```

The mailbox still holds all sixteen messages in `Inbox`. Batch counts gain a `simulated` entry next to `applied` and `held`.

With the default `hold=true`, a simulated write files **no petition**. It is recorded as simulated with `would_hold: true`, and `summonr petition` stays empty:

```console
$ summonr petition
No petitions await. Nothing is held.
```

That is deliberate: approving a petition is never simulated, so simulation cannot be used to produce one. `simulating` combines with `recording` (but not with `rehearse`); a cassette recorded while simulating holds no write entries, and it replays fine without `simulating`, because a replay simulates writes anyway. `simulating` with `replaying` is accepted and changes nothing.

## 4. Drill

A drill is a test file for a ritual: each case gives inputs, scripted model answers, and what you expect. It is the fastest loop, and the only one that needs no model at all.

```yaml
drill: route-form-cases
ritual: route-form
cases:
  - id: other-goes-to-model
    inputs: {form: '{"category":"other","message":"I write for a magazine and would like an interview"}'}
    model: {judged: [{category: press}]}
    expect:
      outcome: returned
      result: {from_form: other, from_model: press}
      model_calls: 1
```

`model` maps a step id to its answers in call order; `model_default` maps a step id to one answer used for every call after the listed ones run out, so a drill over many real files (which drills read for real) needs no long list of identical answers. A string is used verbatim, any other value is serialized as JSON, and `{error: "text"}` simulates a backend failure. `expect.outcome` defaults to `returned`. `result` is a dotted-path subset match, `counts` matches effect counts across the whole run tree (`applied`, `simulated`, `held`, `observed`, `unverified`), and `model_calls` counts every model request in the tree. The complete file is `examples/04-route-form/route-form.drill.yaml`.

```console
$ summonr drill route-form against route-form.drill.yaml
✓ form-category-needs-no-model
✓ other-goes-to-model
✓ other-model-abstains
3 passed, 0 failed
$ echo $?
0
```

Change one expectation and a case fails with its reason, and the exit code is 14:

```console
$ summonr cases route-form --against bad.drill.yaml
✓ form-category-needs-no-model
✕ other-goes-to-model: result.from_model: expected "press", got "sales"
✓ other-model-abstains
2 passed, 1 failed
$ echo $?
14
```

A case also fails if a model step has no scripted answer, or if scripted answers go unused. Things to know:

- Writes are always simulated in a drill, so `counts.simulated` is how you assert that a ritual would write. A would-be `hold` counts as both `held` and `simulated`. `observed` and `unverified` are always 0, because nothing is performed.
- Drills never contact a model backend, but they do need the ritual's real grants. A missing grant is exit 4 before any case runs.
- Answers are keyed by step id only. A parent and a child that share a step id share answers.
- A case may name a `cassette`, a path relative to the drill file. Its reads come from the cassette and its `data` operations are computed. Without a cassette, a case's reads (mail, file, http) are live.
- A drill file or case cassette that does not exist is exit 3; one that does not parse is exit 2.
- The file may be JSON or YAML. Unknown keys are rejected, and `ritual` must equal the ritual named on the command line.

### Drills with a touchstone

A drill file may name a `touchstone`: a saved ritual with `essence: {requests: 0}` and only pure `data` operations, taking the same inputs as the ritual under test. For every case expected to return, it computes the expected result, so the case needs no hand-written `result`. `outcome`, `counts` and `model_calls` still apply. The complete example is `examples/04-route-form/route-form.touchstone-drill.yaml`, with its touchstone `route-form-touchstone.yaml` beside it:

```yaml
drill: route-form-touchstone-cases
ritual: route-form
touchstone: route-form-touchstone
cases:
  - id: support-from-form
    inputs: {form: '{"category":"support","message":"The export button does nothing"}'}
    expect: {model_calls: 0}
```

```console
$ summonr inscribe route-form-touchstone.yaml
$ summonr drill route-form against route-form.touchstone-drill.yaml
✓ support-from-form
✓ sales-from-form
✓ other-keeps-form-category
3 passed, 0 failed
```

The touchstone is pinned by hash before any case runs; the output's `touchstone` field names the version used. A file that names a touchstone and also gives a case `expect.result` is refused (exit 2), as is a touchstone that could call a model, perform an effect, or take different inputs. If the touchstone cannot answer a case, the case is reported as a touchstone error (`?`), counted apart from failures, and the drill exits 14: the case was not checked. See [Assays](../concepts/assays.md#touchstones-computed-expectations) for what a touchstone can and cannot vouch for.

## Which one to use

| You want to | Use |
| --- | --- |
| Check a ritual's logic and every branch, with no model | A drill |
| Re-run a real session with no model, to debug a non-model step | Record once, then replay |
| Preview what a real run on real data would write | Simulate |

See [Commands and clauses](../reference/cli.md) for the clause spellings, and [Outcomes and exit codes](../reference/outcomes.md) for exit 14 and the `simulated` state.
