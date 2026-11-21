# Working on Summonr

Summonr is a Rust CLI and daemon for invoking models as bounded capabilities. Read `README.md` first: it lists what is built and what is not.

## Build and test

```bash
cargo build
cargo test
```

## Drive it without touching the user's real state

```bash
cargo build
./target/debug/summonr demo     # zero-setup ephemeral Git playground with automatic cleanup
```

Or drive it manually in an isolated temp home:

```bash
export PATH="$PWD/target/debug:$PATH"
export SUMMONR_HOME="$(mktemp -d)"
summonr inscribe tests/fixtures/rituals/classify-message.yaml
summonr inscribe tests/fixtures/rituals/inbox-triage.yaml
summonr allow inbox-triage --from mail --all --on folder=Inbox
summonr run classify-message --arg text="your receipt" --model mock/keyword --wait --json
summonr core stop               # when finished
```

- `mock/keyword` is a deterministic backend that needs no model. Use it for anything that does not test real classification.
- A real model needs Ollama (or an OpenAI-compatible/Anthropic provider configured in `config.yaml`); set `ollama_url` / `OLLAMA_HOST` and use `--model ollama/<name>`.
- Use the flag spelling and `--json`. Output is one envelope: `{schema_version, ok, data, error, warnings}`. Exit codes: 2 invalid, 3 not found, 4 not authorized, 5 conflict, 7 wait timeout, 8 core unavailable, 10 exhausted, 11 failed, 12 drifted, 13 inconclusive, 14 drill cases failed, 15 verification failed.
- `summonr help` lists every command and clause in both spellings.
- The workbench (`summonr` with no arguments) and observer (`summonr scry`) needs a terminal; capture it with a detached `tmux` session if you must see it.

## Using it as a tool

`summonr mcp` is an MCP server over stdio. Its tools are `list_rituals`, `describe_ritual`, `preview_ritual`, `run_ritual`, `simulate_ritual`, `record_ritual`, `replay_ritual`, `compile_ritual`, `drill_ritual`, `check_run`, `get_record`, `stop_run`, `list_held_actions`, and per-ritual projected `ritual_<name>` tools; a familiar (`mcp as NAME`) also has `list_pacts`, `describe_pact`, `call_pact` and `check_pact_call` for pacts its owner allowed. Inputs marked `user_only`, grants, and approval of held actions belong to the user; report them instead of working around them.

## Deterministic work and inference

Prefer deterministic methods whenever the task has explicit rules: parsing, calculation, validation, authority, and execution belong in code. Reserve bounded model calls for reasoning and judgment that those methods cannot supply, then return to deterministic checks. Do not equate deterministic output with semantic correctness, or model confidence with authority.

- `data` operations are pure: no filesystem, network, clock, randomness, or model calls. Preserve that boundary.
- A deterministic-only ritual can enforce `essence.requests: 0`, including through child rituals.
- Expose the deterministic/probabilistic boundary in contracts and evidence; do not silently substitute a model for an algorithm or vice versa.
- `summonr compile FILE --json` validates offline. `python3 -B scripts/check_offline.py` uses mock inference and isolated state.

## Rules of the design

- Nothing is inferred. Unknown inputs, missing grants, duplicate clauses and ambiguous references are errors, never guesses.
- Authority is enforced in `src/reagent.rs`, outside the model. Do not move a check into a prompt.
- Every clause needs both a keyword and a flag spelling, with a paired test in `src/grammar.rs`.
- Report gaps against `README.md`'s "Not built yet" table instead of working around them.
