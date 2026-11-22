# 10 · Bughunt

A bug-hunting rite written only in shipped Summonr: small local models
look for bugs and ways around a project's limits, a gate waits for a big
enough batch, and a test stage tries to prove each finding by writing a
test that fails only if the bug is real.

It doubles as a measurement of the language: everything it needed and could
not say is in [LIMITS.md](LIMITS.md).

| Ritual | Does | Model |
| --- | --- | --- |
| `bughunt-hunt` | Lists source files in `dirs` and runs `bughunt-scan-file` on each; reports `files_failed` and `chunks_failed`, and fails if every file failed (model or backend down) | |
| `bughunt-scan-file` | Reads one file in chunks up to its `mod tests {`, asks for findings from one `angle`; marks each finding `quoted` when its snippet (8+ characters) really appears in the code; a chunk the model chokes on is skipped and counted | `qwen3:4b`, one call per chunk |
| `bughunt-gather` | Merges hunts, drops unquoted and low-confidence findings, collapses duplicates, `pass` when `count ≥ min_batch` | none |
| `bughunt-batch-ready` | Touchstone gate on gather's `pass` | none |
| `bughunt-test` | Per finding (first `max_batch`, default 25): model writes `src/bughunt_repro.rs`, `trial` runs it in a throwaway worktree with a 300 s timeout, one retry when it does not build; cleanup never fails the run | `qwen3:4b` for low/medium, `Qwen3.6-35B-A3B` for high/critical, optional Codex for critical |
| `bughunt-verdict` | `confirmed` (failed with `BUGHUNT-CONFIRMED`), `refuted` (one test ran and passed), else `untestable`; a test that never names the suspect module, or uses `#[should_panic]`, can be neither | none |
| `bughunt-score` | Hits and noise against a gold set | none |
| `bughunt-bench` | One hunt per angle, gather, and score against a gold file; reports failed hunts | as hunt |
| `bughunt.rite.yaml` | Three hunts → gather (gated) → test | |

## Set up a lab

The target must be a git repository; the repro harness assumes Rust with
`src/main.rs` or `src/lib.rs`. Paths must not contain spaces. You need the
models and the target's crates locally:

```bash
ollama pull qwen3:4b
ollama pull hf.co/unsloth/Qwen3.6-35B-A3B-GGUF:UD-Q4_K_M   # high/critical findings
(cd /path/to/project && cargo fetch)
```

```bash
cargo build --release
examples/10-bughunt/setup.sh ~/bughunt-lab /path/to/project COMMIT "$PWD/target/release/summonr"
export SUMMONR_HOME=~/bughunt-lab/home
```

`setup.sh` clones the project into `~/bughunt-lab/target` at `COMMIT` on a
`bughunt-base` branch with the repro harness committed, gives it a local
bare remote (see LIMITS.md, 13), writes the lab's `config/config.yaml`
(set `OLLAMA_HOST` first if Ollama is not on `127.0.0.1:11434`), rewrites
the rituals' roots to the lab, inscribes and grants them, and copies the
gold sets into `~/bughunt-lab/benchmark/`. Warm the build once so the first
trial does not spend its time compiling:

```bash
(cd ~/bughunt-lab/target && CARGO_TARGET_DIR=~/bughunt-lab/cargo-target cargo test --offline --bins bughunt_repro)
# use --lib instead of --bins if the project has src/lib.rs
```

## Run

```bash
# One quick hunt, any angle you like:
summonr run bughunt-hunt --arg 'dirs=["src/reagents"]' --arg 'angle=panics on hostile input' --wait --json

# The whole rite:
summonr rite start examples/10-bughunt/bughunt.rite.yaml --arg commit=COMMIT --arg 'dirs=["src/reagents"]'
summonr rite advance RITE_ID      # after the gate passes, runs the test stage

# Score a hunt against a gold set in ~/bughunt-lab/benchmark/:
summonr run bughunt-bench --arg 'dirs=["src"]' --arg 'angles=["panics on hostile input"]' --arg gold=fix-c06fd86.json --wait --json
```

Optional cloud escalation for critical findings, on the test stage (or the
rite): `--arg escalate=codex` runs `codex exec` in its read-only sandbox;
`--arg escalate=gemini` runs Gemini 4 Neon through the Antigravity CLI
(`agy -p`) in a read-only snapshot of the source (`LAB/readonly-src`), since
agy's own file tools are not limited by `--sandbox`. A build failure is
retried once by the same model.

## Gold sets

`benchmark/make_gold.py fix REPO COMMIT` turns a real fix commit into a gold
set at its parent: each `src/` hunk the fix changed (old side plus context)
is one item. These are bugs someone fixed, so they are real. Run a bench at
the parent commit (its own lab).

`benchmark/make_gold.py gemini REPO AUDIT.md 5fb6bbe [DIR...]` turns the
Gemini audit (125 findings, 139 linked code spans) into a gold set, kept to
files directly in the given directories so recall is measured against what
the hunt scanned (`gemini-reagents.json` is `src/reagents`). Those are
unverified model claims: use them for recall only, and treat `confirmed`
test results on them as spot checks of the list.

`benchmark/run.sh REPO OUT_DIR SUMMONR_BIN` runs every shipped gold set, one
lab each, with the rite's three angles.

## Drills

```bash
examples/10-bughunt/drill.sh "$PWD/target/release/summonr"
```

Runs every `*.drill.yaml` offline with scripted model answers against
`fixtures/`. The git/trial path of `bughunt-test` cannot be drilled (writes
are simulated, so no worktree exists); it is checked live.

## If a test run is interrupted

`bughunt-test` adds a `repro` worktree beside the target and removes it at
the end. If a run dies half way, the next one refuses to start (`branch
repro already exists`). The run's results are still in its record under `result.partial` (or in
`result.results` if only cleanup failed; `result.cleanup` says so). Clean up
by hand:

```bash
git -C ~/bughunt-lab/target worktree remove --force ~/bughunt-lab/target-repro
git -C ~/bughunt-lab/target branch -D repro
```
