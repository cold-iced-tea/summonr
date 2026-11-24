# Commands and clauses

Summonr has two spellings for everything. The **keyword** spelling reads like a sentence; the **flag** spelling is conventional. Both parse to the same operation, and every clause has a paired test proving it.

```bash
summonr invoke inbox-triage given count=10 then return json within 5m
summonr run inbox-triage --arg count=10 --wait --json --timeout 5m
```

Scripts and agents should use the flag spelling with `--json`. `summonr help` lists everything in both spellings. Set `style: flag` in the config to have help and suggestions use flags.

## Commands

| Keyword | Flag | Does |
| --- | --- | --- |
| `invoke` | `run` | Execute a saved ritual, or a ritual file |
| `rehearse` | `run --dry-run` | Validate and preview an invocation, executing nothing |
| `inscribe` | `save` | Save a ritual file as a new version; given a directory, save every ritual in it (`*.yaml`, `*.yml`, not drills or rites) with children before the parents that call them, refusing a cycle before saving anything |
| `grimoire` | `rituals` | List saved rituals; `grimoire read NAME` shows one |
| `grant` | `allow` | Authorize a ritual to use reagent tools within a scope; several tools go comma-separated (`--select list,read_text`). `allow check [RITUAL]` lists what a ritual and the children it calls declare under `requires` but have not been granted, with the `allow` command for each (no name: every ritual). A child runs under its caller's grants, so it needs its own only to be invoked directly |
| `revoke` | `disallow` | Withdraw matching authority |
| `circle` | `list` | List active summons |
| `console` | `shell` | Open the persistent command workbench |
| `scry` | `watch` | Open the observer |
| `follow RUN` | `tail RUN` | Stream live events for a summon until it ends |
| `job` | `queue` | Shared result work: `create FILE`, `work NAME`, `status NAME`, `item NAME ITEM`, `retry NAME ITEM` |
| `daemon` | `panel` | Open the daemon status panel |
| `await` | `wait` | Block until a summon ends |
| `banish` | `stop` | Ask a summon to stop; `stop --all` (`banish all`) asks every running summon in this home to stop |
| `revive` | `resume` | Resume an exhausted, cancelled, or interrupted summon from its last checkpoint |
| `recast RUN STEP` | | Re-run a summon from `STEP`, reusing earlier step outputs |
| `graveyard` | `history` | List ended summons; `children RUN` (`--children RUN`) lists one run's child summons, ended or not; add `recursive` (`--recursive`) for the whole tree of descendants, indented, with outcome, requests and tokens |
| `elegy` | `record` | Read one summon's complete record |
| `ledger` | `logs` | Read one summon's events |
| `epitaph prove RUN` | | Generate a Merkle inclusion proof for `RUN` |
| `epitaph verify FILE` | | Verify a saved Merkle inclusion proof `FILE` |
| `witness compare [PEERS...] [--live]` | | Compare epitaph chain views across peer witnesses (stored witnessings or `--live` endpoints) against split views |
| `key init-root FILE` | | Generate an offline Ed25519 root key at `FILE` |
| `demo` | | Run an ephemeral `/tmp` Git playground with a held `git.commit` petition and automatic cleanup |
| `key sign-grant ROOT_KEY FAMILIAR PUBKEY` | | Sign an offline familiar grant for `FAMILIAR` and `PUBKEY` using `ROOT_KEY` |
| `familiar register-grant FILE` | | Register an offline-signed familiar grant `FILE` (verified against `config/root.pub`) |
| `assay` | `test` | Run an assay set; record a baseline or check drift |
| `assay compare A B` | | Compare two assay accounts `A` and `B` side by side |
| `satchel` | `tools` | List reagents and their tools |
| `petition` | `approval` | List held operations; `permit` / `deny` by id or `all` |
| `glean` | `sample` | Sample real runs as unlabeled assay cases |
| `drill R against FILE` | `cases R --against FILE` | Run a ritual against scripted model answers and expected outcomes; never contacts a model, never performs a write. A missing drill file is exit 3. Exit 14 if any case fails |
| `mcp` | | Serve rituals over MCP: `mcp` (stdio) or `mcp http ADDR:PORT` |
| `core` | | Run the core in the foreground; `core status` checks it; `core stop` ends it; `core restart` reloads it |
| `help` | | Show help |
| `version` | | Print the version |

`summonr` with no arguments opens the command workbench. `summonr scry` opens the dedicated observer.

## Clauses

| Keyword | Flag | Meaning |
| --- | --- | --- |
| `through M` | `--model M` | Model override for this run and its children |
| `at k=v ...` | `--set k=v ...` | Runtime settings |
| `given k=v ...` | `--arg k=v ...` | Ritual inputs |
| `reading k=PATH ...` | `--arg-file k=PATH ...` | Ritual inputs whose values are a regular file's UTF-8 text, read by the CLI (at most 1 MiB each), with one trailing newline dropped (as `$(cat FILE)` would), then converted like `given` values; an input may not come from both |
| `as NAME` | `--name NAME` | Assign a name (for example, when inscribing) |
| `returning json` | `--json` | Answer as a JSON envelope |
| `then return [json]` | `--wait [--json]` | Wait for completion |
| `within 5m` | `--timeout 5m` | Bound the wait, not the work |
| `from R` | `--from R` | Open a reagent scope (grants) |
| `selecting a b` | `--select a,b` | Select tools in the open scope |
| `all` | `--all` | Select every tool in the open scope |
| `on k=v` | `--on k=v` | Resource scope of a permission |
| `into k=v` | `--into k=v` | Destination scope of a permission |
| `version N` | `--version N` | Pin the preceding target or reagent |
| `replacement` | `--replace` | Explicitly replace the named target |
| `against SET` | `--against SET` | Assay set to run |
| `baseline` | `--baseline` | Record the account as the baseline |
| `tethered` | `--cancel-on-drop` | With `run --wait`: if this client goes away before the summon ends (killed, interrupted, or stopped by `--timeout`), the core cancels the summon instead of leaving it running, as `stop` would (see [Reliability](../concepts/reliability.md)) |
| `recording PATH` | `--record PATH` | `invoke` only: save every model call and every read or write tool call to a cassette (pure `data` operations are not recorded). Replaces an existing cassette; refuses (exit 2) a file that is not one |
| `replaying PATH` | `--replay PATH` | `invoke` only: answer model calls and reads from a cassette, fully offline; compute `data` operations; always simulate writes. A missing cassette is exit 3, an unparseable one exit 2 |
| `simulating` | `--simulate` | `invoke` only: authorize writes but do not perform them |

`recording`, `replaying` and `simulating` are each refused with `rehearse` or `--dry-run`, and `recording` together with `replaying` is refused; each is exit 2. `simulating` combines with `recording`; with `replaying` it is accepted but redundant, since a replay always simulates writes. The MCP server offers none of the three, nor `drill`, and the core refuses all three from an agent caller (exit 4). See [Safe building](../guides/safe-building.md).

While `--wait` (`then return`) waits and stderr is a terminal, one line on stderr is redrawn in place: items done, the child ritual at work (down to the deepest running descendant), how many child runs there are and how many failed, and the whole tree's requests, tokens and elapsed time:

```text
  ◑ △☉♂ s-36b7… labours · 3/25 · bughunt-test › bughunt-verdict · 26 children, 2 failed · 41 requests · 223,555 tokens · 12:03
```

Nothing is drawn when stderr is not a terminal, and stdout only ever carries the result, so `--json` output stays clean even in a terminal. The line is cleared before the result is printed.

Duplicate clauses are errors. `at` values are parsed as JSON when they can be (`at temperature=0.2` is a number) and kept as strings otherwise.

## Common invocations

```bash
# run and wait
summonr invoke inbox-triage given count=10 apply=true then return
summonr run inbox-triage --arg count=10 --arg apply=true --wait

# start and return immediately (prints the summon id)
summonr run inbox-triage --arg count=10

# preview
summonr rehearse inbox-triage given count=10

# pin a version and override settings
summonr run classify-message --version 1 --arg text="..." --set temperature=0 --wait

# grants
summonr grant inbox-triage from mail selecting search read on folder=Inbox
summonr allow inbox-triage --from mail --all --on folder=Inbox
summonr revoke inbox-triage from mail selecting move

# petitions
summonr petition
summonr petition request inbox-triage
summonr petition permit p-bd5c4a92 p-0c1e7f22
summonr petition deny all

# manual summon sessions
summonr summon scratch through ollama/qwen3:8b given requests=10
summonr soul m-1234 to "Be concise."
summonr bind m-1234 as notes file ./notes.txt
summonr imbue m-1234 at temperature=0.2
summonr bid m-1234 to "Summarize the notes."
summonr lethe m-1234 1
summonr inscribe my-summary session m-1234

# assays & experiments
summonr assay classify-message against inbox-set.yaml baseline
summonr assay classify-message
summonr glean classify-message 10 against inbox-set.yaml
summonr sweep classify-message against ./sweep.yaml
summonr judge exp-1234
summonr weigh exp-1234

# safe building
summonr invoke route-form given form="..." recording cassettes/a.json then return
summonr invoke route-form given form="..." replaying cassettes/a.json then return
summonr invoke inbox-triage given apply=true simulating then return
summonr drill route-form against route-form.drill.yaml
```

## JSON output

With `--json`, every command prints one envelope:

```json
{"schema_version": 1, "ok": true, "data": { ... }, "error": null, "warnings": []}
```

On failure, `ok` is `false` and `error` carries a stable code and message. The process [exit code](outcomes.md) tells you the class of failure without parsing.

## Offline compiler

`validate FILE --json` and `compile FILE --json` are equivalent. They read the file and saved child definitions, return the canonical compiled contract and diagnostics, and make no backend calls or state changes. Unlike rehearsal, compilation does not inspect a live backend fingerprint.

Invocation idempotency keys, expected contract hashes and caller limit overrides are currently exposed through MCP and the local RPC `invoke` method. They do not yet have CLI clause spellings.
