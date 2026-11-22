# 10 · Bughunt

A bug-hunting rite written only in shipped Summonr: small local models
look for bugs and ways around a project's limits, a gate waits for a big
enough batch, and a test stage tries to prove each finding by writing a
test that fails only if the bug is real.

It doubles as a measurement of the language: everything it needed and could
not say is in [LIMITS.md](LIMITS.md).

| Ritual | Does | Model |
| --- | --- | --- |
| `bughunt-hunt` | Lists source files in `dirs`, reads them in chunks, asks for findings from one `angle`; marks each finding `quoted` when its snippet really appears in the code | `qwen3:4b`, one call per chunk |
| `bughunt-gather` | Merges hunts, drops unquoted and low-confidence findings, collapses duplicates, `pass` when `count ≥ min_batch` | none |
| `bughunt-batch-ready` | Touchstone gate on gather's `pass` | none |
| `bughunt-test` | Per finding: model writes `src/bughunt_repro.rs`, `trial` runs it in a throwaway worktree, one retry with compiler output | `qwen3:4b` for low/medium, `Qwen3.6-35B-A3B` for high/critical, optional Codex for critical |
| `bughunt-verdict` | `confirmed` (failed with `BUGHUNT-CONFIRMED`), `refuted` (one test ran and passed), else `untestable`; a test that never names the suspect module can be neither | none |
| `bughunt-score` | Hits and noise against a gold set | none |
| `bughunt-bench` | One hunt plus score against a gold file | as hunt |
| `bughunt.rite.yaml` | Three hunts → gather (gated) → test | |

## Set up a lab

The target must be a git repository; the repro harness assumes Rust with
`src/main.rs` or `src/lib.rs`.

```bash
cargo build --release
examples/10-bughunt/setup.sh ~/bughunt-lab /path/to/project COMMIT "$PWD/target/release/summonr"
export SUMMONR_HOME=~/bughunt-lab/home
```

`setup.sh` clones the project into `~/bughunt-lab/target` at `COMMIT` on a
`bughunt-base` branch with the repro harness committed, gives it a local
bare remote (see LIMITS.md, 12), writes the lab's `config/config.yaml`
(set `OLLAMA_HOST` first if Ollama is not on `127.0.0.1:11434`), rewrites
the rituals' roots to the lab, inscribes and grants them, and copies the
gold sets into `~/bughunt-lab/benchmark/`. Warm the build once so the first
trial does not spend its time compiling:

```bash
(cd ~/bughunt-lab/target && CARGO_TARGET_DIR=~/bughunt-lab/cargo-target cargo test --offline --bins bughunt_repro)
```

## Run

```bash
# One quick hunt, any angle you like:
summonr run bughunt-hunt --arg 'dirs=["src/reagents"]' --arg 'angle=panics on hostile input' --wait --json

# The whole rite:
summonr rite start examples/10-bughunt/bughunt.rite.yaml --arg commit=COMMIT --arg 'dirs=["src/reagents"]'
summonr rite advance RITE_ID      # after the gate passes, runs the test stage

# Score a hunt against a gold set in ~/bughunt-lab/benchmark/:
summonr run bughunt-bench --arg 'dirs=["src","src/reagents"]' --arg 'angle=...' --arg gold=fix-c06fd86.json --wait --json
```

Optional Codex escalation for critical findings: `--arg escalate=codex` on
the test stage (or the rite). It runs `codex exec` in a read-only sandbox
through the `bughunt-codex` command that `setup.sh` configures.

## Gold sets

`benchmark/make_gold.py fix REPO COMMIT` turns a real fix commit into a gold
set at its parent: each `src/` hunk the fix changed (old side plus context)
is one item. These are bugs someone fixed, so they are real. Run a bench at
the parent commit (its own lab).

`benchmark/make_gold.py gemini REPO AUDIT.md 5fb6bbe` turns the 125-finding
Gemini audit into a gold set. Those are unverified model claims: use them
for recall only, and treat `confirmed` test results on them as spot checks
of the list.

## Drills

```bash
examples/10-bughunt/drill.sh "$PWD/target/release/summonr"
```

Runs every `*.drill.yaml` offline with scripted model answers against
`fixtures/`. The git/trial path of `bughunt-test` cannot be drilled (writes
are simulated, so no worktree exists); it is checked live.
