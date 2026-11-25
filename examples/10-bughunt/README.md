# 10 · Bughunt

A bug-hunting rite written only in shipped Summonr: small local models
look for bugs and ways around a project's limits, a gate waits for a big
enough batch, and a test stage tries to prove each finding by writing a
test that fails only if the bug is real.

It doubles as a measurement of the language: everything it needed and could
not say is in [LIMITS.md](LIMITS.md).

| Ritual | Does | Model |
| --- | --- | --- |
| `bughunt-hunt` | Lists source files with extension `ext` (default `.rs`) in `dirs` and runs `bughunt-scan-file` on each; reports `files_failed` and `chunks_failed`, and fails if every file failed (model or backend down) | |
| `bughunt-scan-file` | Reads one file up to `stop_at` (`data.index_of`) in dynamic `data.range` chunks, asks for findings from one `angle`; marks each finding `quoted` when its snippet (8+ characters) really appears in the code; uses `each` `keep:` so file text never leaves the loop | `Qwen3.6-35B-A3B`, one call per chunk (override with `--model`) |
| `bughunt-gather` | Merges hunts (`data.flatten`), drops unquoted, below-`min_confidence` (`data.filter` `params:`), angle-echo, and no-bug findings, collapses duplicates (`data.unique_by`), `pass` when `count ≥ min_batch` | none |
| `bughunt-batch-ready` | Touchstone gate on gather's `pass` | none |
| `bughunt-test` | Per finding (first `max_batch`, default 25, `keep: [result]`): optional `triage` re-reads a wider code window on `Qwen3.6-35B-A3B` and dismisses false leads before building; otherwise writes `src/bughunt_repro.rs` (`lang=python`: `test_bughunt_repro.py`), `trial` runs it in a throwaway worktree (300 s timeout), one retry on build failure; cleanup never fails the run | `qwen3:4b` for low/medium, `Qwen3.6-35B-A3B` for triage and high/critical, optional Codex/Gemini for critical |
| `bughunt-verdict` | `confirmed` (failed with `BUGHUNT-CONFIRMED`), `refuted` (one test ran and passed), else `untestable`; rejects copied-logic tests (accepting both `<module>::` and `reagent::call`/`authorize` for `*_reagent.rs`), `#[should_panic]`, multiple `BUGHUNT-CONFIRMED` branches, and setup-deserialization markers. `lang=python` reads a unittest run instead (see [Python targets](#python-targets)) | none |
| `bughunt-score` | Hits and noise against a gold set (normalizes per-line indentation so multi-line snippets match) | none |
| `bughunt-bench` | One hunt per angle (forwarding `ext` and `stop_at`), gather, and score against a gold file; reports failed hunts | as hunt |
| `bughunt.rite.yaml` | Three hunts → gather (gated) → test | |

## Set up a lab

The target must be a git repository; the repro harness assumes Rust with
`src/main.rs` or `src/lib.rs` unless you pass `--lang python` (see
[Python targets](#python-targets)). Paths must not contain spaces. You need the
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

For a crate inside a Cargo workspace, add its directory as a fifth argument
(`... "$PWD/target/release/summonr" crates/foo`): the trial then runs that
crate's tests only. A project that does not commit `Cargo.lock` gets one
resolved offline from the local Cargo cache, so run `cargo fetch` first.

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

### Python targets

```bash
examples/10-bughunt/setup.sh ~/bughunt-py /path/to/project COMMIT "$PWD/target/release/summonr" src --lang python
```

The fifth argument is the import root put on `PYTHONPATH` (default: `src`
if the repository has one, else the root). The harness is an empty
`test_bughunt_repro.py` at the repository root; a repro is one
`unittest.TestCase` that imports the code as its users do and fails with
`AssertionError("BUGHUNT-CONFIRMED: …")`, and trial runs
`python3 -m unittest test_bughunt_repro` in the throwaway worktree. The
lab's rituals default to Python: hunts read `.py` files with no test-module
marker (`ext=.py`, `stop_at=""`), and `bughunt-test` and `bughunt-verdict`
take `lang=python`. The verdict then wants `Ran 1 test` and `OK` to refute,
and `FAILED (failures=1)` with the marker to confirm (an uncaught exception
is `untestable`); an import or syntax error is the build failure that gets
one rewrite. The target must need only the standard library.

On a Rust lab or your own core, the same rituals take the inputs directly:
`bughunt-bench --arg ext=.py --arg stop_at=`, `bughunt-test --arg lang=python`.

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
is one item. Test code never counts: Rust's `mod tests {` and `*_tests.rs`,
Python's `test_*.py`, `*_test.py` and `conftest.py`, Shell's `test_*.sh`,
`*_test.sh` and `*.bats`, and anything under `tests/` (or `test/`, for
Python and Shell). These are bugs someone fixed, so they are real. Run a bench at
the parent commit (its own lab).

`benchmark/make_gold.py gemini REPO AUDIT.md 5fb6bbe [DIR...]` turns the
Gemini audit (125 findings, 139 linked code spans) into a gold set, kept to
files directly in the given directories so recall is measured against what
the hunt scanned (`gemini-reagents.json` is `src/reagents`). Those are
unverified model claims: use them for recall only, and treat `confirmed`
test results on them as spot checks of the list.

`benchmark/run.sh REPO OUT_DIR SUMMONR_BIN` runs the suite's gold sets (the
smoke tier unless `--tier`, `--category` or case names say otherwise) with the
rite's three angles, in one of two ways:

- **On your own core** (`--shared-core`, or `--core-home PATH` for another
  home). No labs and no `setup.sh`: the bench rituals are saved once (a new
  version only if a file changed), each case's parent commit is unpacked
  read-only into `OUT_DIR/NAME/target`, and `bughunt-bench` runs with
  `root=OUT_DIR/NAME/target` and `benchmark_root=` the gold directory, under
  two `--under` grants:

  ```bash
  examples/10-bughunt/benchmark/run.sh ~/Work/summonr ~/bughunt-out "$PWD/target/release/summonr" --shared-core
  ```

  The run is narrowed to the one root it was given, so cases cannot read
  each other's checkouts. The grants stay afterwards; the script prints the
  `disallow` lines that withdraw them.
- **In isolated labs** (`--isolated`, the default): one `setup.sh` lab per
  case with its own clone, `SUMMONR_HOME` and core, which leaves your core
  untouched and makes each run reproducible on its own.

Either way the results are `OUT_DIR/bench-NAME.json` and
`OUT_DIR/hunts-NAME.json` (the raw hunts, for `bughunt-rescore`), taken from
the run's own `--json` record, followed by a category table. `--model M`
overrides the hunter model.

### External suite

`benchmark/external-suite.json` is an out-of-distribution set: 22 real fix
commits in six third-party crates (`http`, `rust-url`, `httparse`, `semver`,
`tar`, `toml`), pinned by full SHA. They cover path traversal during archive
extraction, archive and HTTP framing, parser recursion, integer overflow,
input validation and logic errors. Summonr ships no third-party source; build
the repos, Cargo cache and gold sets locally, then benchmark them:

```bash
examples/10-bughunt/benchmark/fetch_external.py ~/bughunt-ext   # [--repo http] [--tier smoke]
examples/10-bughunt/benchmark/run.sh ~/bughunt-ext ~/bughunt-ext/out "$PWD/target/release/summonr" \
  --suite examples/10-bughunt/benchmark/external-suite.json --gold-dir ~/bughunt-ext/gold --tier smoke --shared-core
examples/10-bughunt/benchmark/verify_repros.py ~/bughunt-ext \
  --suite examples/10-bughunt/benchmark/external-suite.json --name ext-httparse-c0631f2 TEST_RESULT.json
```

Each case names its `repo_name`, `repo_url`, `crate_path` (workspace
crates), and the `dirs` a hunt scans; `verify_repros.py` replays confirmed
repros at the parent and the fix of that crate.

### Python suite

`benchmark/python-suite.json` holds 6 real fix commits in two
standard-library-only projects, `packaging` and `tomli`: marker recursion
depth, drive-relative licence paths, a line break absorbed into a URL
requirement, an unanchored wheel build-tag regex, quadratic dotted keys, and
duplicate inline-table keys. Every case is `"lang": "python"` with an
`import_root` and a `module`; `fetch_external.py` imports that module at the
parent and the fix instead of fetching crates, `run.sh` passes `ext=.py
stop_at=` (and `--lang python` to an isolated lab), and `verify_repros.py`
replays repros with unittest:

```bash
examples/10-bughunt/benchmark/fetch_external.py ~/bughunt-py --suite examples/10-bughunt/benchmark/python-suite.json
examples/10-bughunt/benchmark/run.sh ~/bughunt-py ~/bughunt-py/out "$PWD/target/release/summonr" \
  --suite examples/10-bughunt/benchmark/python-suite.json --gold-dir ~/bughunt-py/gold --shared-core
examples/10-bughunt/benchmark/verify_repros.py ~/bughunt-py \
  --suite examples/10-bughunt/benchmark/python-suite.json --name py-packaging-2602dd0 TEST_RESULT.json
```

## Drills

```bash
examples/10-bughunt/drill.sh "$PWD/target/release/summonr"
```

Runs every `*.drill.yaml` offline with scripted model answers against
`fixtures/` (Python cases use `fixtures/target/pkg/auth.py` and a fake
unittest trial; `test.drill.yaml` uses `sandbox: {repos:
[bughunt-target]}` to drill the real worktree/write/trial/cleanup path on
disposable copies) and checks that all inscribed rituals produce zero
compile-time lint warnings.

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
