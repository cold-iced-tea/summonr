# What bughunt needed that the language could not say

Bughunt was built with only shipped Summonr (language v1, 0.2.0 at
`5fb6bbe`) and no Rust changes. Each entry is: what was needed, what the
language offered, the workaround used, and a one-line proposal. Ordered by
how much they cost the build.

## Shape of data and loops

1. **Loop bodies cannot see outer values.** `data.map`/`filter`/`fold`
   bodies see only `item`, `index`, `acc`; they cannot attach the current
   file name or chunk start to a model's findings, and they cannot nest.
   *Workaround:* an `each` per level (dir → file → chunk → finding) with a
   `data.merge` step inside, then one `data.fold`+`data.concat` per level to
   flatten. Most of `hunt.yaml` and `scan-file.yaml` is this plumbing.
   *Proposal:* `with:` constants on collection operators, and a
   `data.flatten`.
2. **A loop item exposes only its steps' outputs, not the element.**
   `gather` needs an extra `data.get path: ""` step just to carry the
   finding through. *Proposal:* include the element as `item.value` in each
   loop output.
3. **No coalesce.** A step in the branch not taken is `null`, so picking
   "whichever branch ran" takes `data.select` chains keyed on the branch
   condition (`test.yaml` `model_file`, `test_file`, `verdict`,
   `final_file`, `final_tail`). *Proposal:* `data.coalesce` (first non-null).
4. **Predicates cannot take parameters.** No `${…}` inside a predicate
   (correctly, to stop injection), but nothing else either, so the
   confidence floor in `gather` is a literal `0.3`. *Proposal:* a
   `params:` map bound as values (never text) into the predicate.
5. **No range operation.** Chunk starts are a literal list
   (`hunt.yaml` `starts`), and changing `chunk` means changing `starts` too.
   *Proposal:* `data.range {start, end, step}`.
6. **No line numbers, no `index_of`.** A model cannot be asked for a line
   it can see numbered, and a quoted line cannot be located. Findings carry
   a verbatim `snippet` instead, which also turned into the best
   deterministic filter (see Results). *Proposal:* `data.lines` with an
   `numbered: true` option, or `data.enumerate`.
7. **Loop results keep every step's output, and data arguments are capped
   at 1 MiB.** A hunt over `src/` carried each file's text (`read_text`,
   `lines`) inside its loop results, so the fold that collects findings
   failed after 213 model calls ("data value exceeds 1 MiB encoded JSON").
   *Workaround:* per-file work moved into a child ritual,
   `bughunt-scan-file`, whose only output is its findings. The trap came
   back once: a per-chunk "has the test module started?" check re-joined
   the file prefix in every chunk, which put ~60 copies of the file into
   the loop results; half of `src/` then failed and, because files are
   skipped on failure, looked like "no findings" (caught in review; the
   check now runs once per file).
   *Proposal:* an `each` option to keep only named outputs (`keep: [x]`)
   (now on `feat/language-gaps`). Better still, the compiler already knows
   which outputs are referenced later, so `each` could drop unreferenced
   outputs by default. And the cap itself (1 MiB per data argument or
   result, 10,000 list items, hardcoded in `src/stdlib.rs`) is the one
   bound a ritual cannot declare; it could be an essence field (`bytes:`)
   with today's value as the default, and checked at compile time where
   the size is knowable.
8. **Not a limit, a documentation bug:** `ritual-format.md` says
   "`parallel` greater than 1 is rejected", but `each` accepts
   `parallel: 1..32` and the executor runs items concurrently (with atomic
   essence reservations, see `concepts/essence.md`). Bughunt was written
   sequentially because of the doc; parallel chunks would only help if
   Ollama serves requests in parallel (`OLLAMA_NUM_PARALLEL`).

## Pointing a ritual at a project

9. **File roots are fixed at inscription.** `requires.on.root` is a
   literal, so a ritual cannot be "pointed at" a project. *Workaround:*
   `setup.sh`/`drill.sh` rewrite `/absolute/path/to/...` placeholders and
   re-inscribe (the same trick example 08 uses). *Proposal:* a root input
   whose value must fall inside a granted root, checked at run time.
   *Now available:* `on: {root: "${inputs.target}"}` with a grant
   `--under DIR` (see the book's "Roots chosen at run time"). These
   examples still use the placeholders until they are migrated.
10. **`file.list` is not recursive.** Directories to hunt are an input
   list. *Proposal:* `file.walk` with depth and entry bounds.
11. **The CLI cannot read an input from a file.** A 98-finding list or a
    gold set overflows the command line ("Argument list too long").
    *Workaround:* `bughunt-bench` reads the gold set with `file.read_json`
    and runs hunt and score in one summon; the rite passes findings between
    stages internally. *Proposal:* `--arg name=@file.json`.

## Running tests

12. **`trial.run` runs the repository's single `test` command.** No test
    selection and no per-test timeout. *Workaround:* a dedicated repo
    entry, `bughunt-target`, whose `test` is
    `timeout 300 env CARGO_TARGET_DIR=… cargo test --offline -q --bins bughunt_repro`
    (the command runs without a shell, so `timeout` and `env` are programs).
13. **`git.worktree_add` starts only from an up-to-date trunk and needs
    `<remote>/<trunk>` to exist.** *Workaround:* `setup.sh` checks the
    target commit out as a `bughunt-base` branch and pushes it to a local
    bare remote in the lab.
    **Bug found:** when the remote has no trunk branch, `worktree_add`
    fails with git's raw `rev-list` usage text
    (`git rev-list: 'git <command> [<revision>...] -- [<file>...]'`), not
    a message saying the remote branch is missing
    (`src/reagents/git_reagent.rs`, `worktree_add`).
14. **Summonr is a binary crate**, so a repro in `tests/` could reach no
    internals. *Workaround:* the base branch commits an empty
    `src/bughunt_repro.rs` declared as `#[cfg(test)] mod bughunt_repro;`.
    Each harness is language-specific; nothing in the language describes "how
    to run one test in this project".
    For a crate in a Cargo workspace (`setup.sh ... CRATE_PATH`), the file
    stays at the repository's `src/bughunt_repro.rs` (the path the rituals
    write) and the crate declares it through `#[path]`; a `#![no_std]`
    crate also gets `#[cfg(test)] extern crate std;`, and the trial passes
    `RUSTFLAGS=--cap-lints=warn` so an old commit's `deny(warnings)` does not
    fail on lints a newer compiler added.
    A Python target (`setup.sh ... --lang python`) needs no declaration: the
    base branch commits an empty `test_bughunt_repro.py` at the repository
    root, and the `test` command is
    `timeout 300 env PYTHONPATH=IMPORT_ROOT PYTHONDONTWRITEBYTECODE=1 python3 -m unittest test_bughunt_repro`.
    `PYTHONPATH` is relative so it names the import root of whichever
    worktree trial runs in. Only standard-library-only targets work: nothing
    installs a project's dependencies, and a lab has no virtualenv.

## Models

15. **Model names are fixed per step.** Routing by severity means one
    model step per model in `if` branches, with the prompt duplicated.
    *Proposal:* a step-level `model: {one_of: [...], choose: EXPR}` that
    keeps the set of model sites static.
16. **Codex is not a model backend.** It can only be a `cmd` command
    (`bughunt-codex`, read-only sandbox), so its answer is unvalidated
    stdout: no schema, no fingerprint, no token essence.
    *Proposal:* a command-backed model backend, or an output schema on
    `cmd.read`.

## Rites

17. **A rite has a fixed number of stages**, so "one hunt per angle" is
    three hard-coded stages; angles are inputs, the count is not.
18. **A touchstone gate sees only the stage's output**, so `gather` has to
    return `min_batch` and `pass` for `batch-ready` to read.
19. **Assumptions are not checkable in the language.** `target-commit` is
    a statement; nothing reads the lab's HEAD (the hunt has no `git`
    authority, and adding it only for this would widen the grant).

## Other languages

20. **A step's `bind:` list and `schema` are fixed**, unlike its model
    (`one_of`/`choose`, 15). A Rust and a Python repro need different
    guides and answer fields, so `bughunt-test` has two writer steps
    (`write_repro` and `write_repro_py`, likewise the rewrite) in `if`
    branches with the prompt duplicated, and `data.coalesce` picks
    whichever ran. The triage guides stay Rust-worded; a Python run puts a
    short translation note in front of the prompt (empty for Rust, so
    Rust prompts are unchanged).
    *Proposal:* `bind:` entries chosen by a reference, as `choose` does for
    models, from a fixed set of bindings.
21. **A lab cannot set a ritual's input defaults.** A Python lab wants
    `ext=.py`, `stop_at=""` and `lang=python` everywhere (and the rite's
    stages do not pass them). *Workaround:* `setup.sh --lang python`
    rewrites those exact input lines while staging the rituals, so a
    changed line silently stops being flipped. *Proposal:* per-home input
    defaults in `config.yaml`, recorded in each run.
22. **The verdict's honesty checks are substring tests.** For Python, an
    `import`/`from` line must name the suspect module (or its package:
    `src/packaging/_parser.py` imported as `packaging.markers`); the marker
    counts only on `FAILED (failures=1)`, so an uncaught exception that
    happens to quote it is `untestable`; `expectedFailure`, `unittest.skip`
    and `skipTest` make a test dishonest. A module whose name is a substring
    of another import (`auth` in `oauthlib`) passes the import check.

## Rough edges met on the way

- `config.yaml` belongs in `$SUMMONR_HOME/config/`; one written to
  `$SUMMONR_HOME/` is silently ignored, and the core reads config only at
  start (`setup.sh` restarts it).
- Child rituals must be inscribed before parents; re-inscribing changed
  content needs `--replace`.
- `allow --select` takes one comma-separated list, not repeated words.
- A drill's sandbox refuses a repository that contains symlinks
  (`sandbox: symlink at …`), so `bughunt-test` cannot be drilled against
  rust-url, whose crates link their licence files. Live runs use real git
  worktrees and are not affected.

## Results

See the README for the commands. Model `ollama/qwen3:4b` unless noted;
RX 7900-class GPU.

### Part 1: Hunt benchmark (`ollama/qwen3:4b`)

Three angles per gold set (`bypass`, `panics/overflows`, `injection`).
**Located** counts gold hunks matched by `bughunt-score` (with per-line
whitespace-normalized hits in parentheses; `make_gold.py` strips leading
indentation per line while `score.yaml` only trims the outer snippet, so
multi-line indented snippets miss the shipped scorer). **Diagnosed** is
manual judgement against `git show COMMIT` (or the code for
`gemini-reagents`, which is an unverified audit list).

| Gold set | Requests | Tokens | Wall time | Raw → Gathered | Gold hunks | Located (norm) | Diagnosed |
| --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| `fix-c06fd86` | 516 | 869,551 | 1800s | 1,135 → 547 | 2 | 1 (1) | 0 |
| `fix-4979f5c` | 573 | 987,895 | 2453s | 1,273 → 624 | 2 | 0 (1) | 0 (1 partial) |
| `fix-35f37f0` | 516 | 870,999 | 2548s | 1,147 → 558 | 9 | 4 (4) | 0 |
| `gemini-reagents` | 75 | 164,734 | 364s | 192 → 89 | 19 | 11 (12) | n/a (unverified) |

*Notes:* `fix-c06fd86` and `fix-4979f5c` were scored via `bughunt-rescore`
after `gather` exhausted (c06fd86) and a host reboot interrupted `score`
(4979f5c; 2059s hunts + 394s rescore).

### Part 2: Test stage (local `Qwen3.6-35B-A3B` / `qwen3:4b`, 25 findings per set)

Up to 25 gathered findings per gold set (gold hits first, then highest
severity). Every 25-finding run completed all 25 items and then ended
`invalid: data value exceeds 1 MiB encoded JSON` at `finished` (limit 7
above, hitting `bughunt-test` because `per_finding` retained `file.read_text`
and `data.lines` across 25 items); all 100 items were recovered from
`result.partial`.

| Gold set | Requests | Tokens | Wall time | Confirmed (true / raw) | Refuted | Untestable | Truncated (`invalid`) |
| --- | ---: | ---: | ---: | :---: | ---: | ---: | ---: |
| `fix-c06fd86` | 32 | 223,555 | 1777s | 0 / 1 | 6 | 9 | 9 |
| `fix-4979f5c` | 34 | 248,037 | 1997s | 0 / 1 | 3 | 11 | 10 |
| `fix-35f37f0` | 32 | 264,203 | 2269s | 0 / 1 | 3 | 9 | 12 |
| `gemini-reagents` | 36 | 257,109 | 1995s | 0 / 0 | 2 | 14 | 9 |

### Part 2: Cloud comparison (5 `gemini-reagents` gold hits set to `critical`)

One high-severity gold hit from each of `cmd_reagent.rs`, `file_reagent.rs`,
`git_reagent.rs`, `http_reagent.rs`, and `wasm_reagent.rs` (3 panic/overflow
claims, 2 path-traversal/command-injection claims), run on the same list with
both cloud backends.

| Backend | Wall time | Outcome | Refusals | Confirmed | Refuted | Untestable | Manual judgement |
| --- | ---: | --- | ---: | ---: | ---: | ---: | --- |
| `escalate=gemini` (Gemini 4 Neon) | 452s | `returned` | 0 | 0 | 4 | 1 | 5/5 honest, compiling tests refuting the claims; 1 (`file_reagent.rs`) marked `untestable` only because it called `reagent::call("file", "write_text", …)` instead of naming `file_reagent::`. `git status` clean. |
| `escalate=codex` (Codex CLI) | 138s | `returned` | 0 | 0 | 5 | 0 | 5/5 honest, compiling tests refuting the claims (including the 2 security-framed claims under the regression-test prompt wording). `git status` clean. |

### What the numbers say

- **`qwen3:4b` is a noisy locator, not a diagnostician.** It located 6 of 13
  fix-commit gold hunks (with whitespace normalization) and 12 of 19
  `gemini-reagents` hunks, but strictly diagnosed 0 of the 3 real fix-commit
  bugs (1 partial on `validate_grant_scope`). Roughly 15% of gathered `src/`
  findings are literal copies of the angle string or "the code does not panic"
  at `high` severity and `0.90–0.95` confidence.
- **Wrong claims poison the test stage, and local 35B needs a thinking/output
  budget.** Across 100 local trials, 40 failed with `backend truncated
  generation` (thinking tokens exhausted the default generation cap before
  emitting JSON), and all 3 `confirmed` verdicts were false confirmations on
  `src/ritual.rs` (panicking with `BUGHUNT-CONFIRMED` in both branches of an
  `if/else`, or on a broken JSON fixture).
- **Cloud test writers are dramatically more reliable.** On the same 5
  reagent findings, both Gemini 4 Neon and Codex produced 5/5 compiling,
  honest Rust regression tests with 0 refusals and 0 truncations (Codex 3.3×
  faster at 138s vs 452s). Both proved all 5 claims false, demonstrating that
  the test-stage harness works when paired with a capable test writer — and
  that the remaining bottleneck is upstream (hunt diagnosis, gather filtering,
  and honesty-guard refinement).

### Follow-up: Bughunt v2 + `34b4259` runtime + `Qwen3.6-35B-A3B` hunter

After landing the runtime/language improvements (`34b4259`) and updating
bughunt to v2 (`keep:`, `data.range`, `data.index_of`, `data.unique_by`,
`data.filter` `params:`, normalized `score.yaml`, hardened `verdict.yaml`,
`triage` stage, `num_predict: 6144`, `env:`/`stdin:`):

- **Pure-step speedup (`bughunt-rescore` on `hunts-fix-4979f5c.json`, btrfs):**
  **394s → 4.24s (93× faster)**, lab DB 420 MB → 20 MB, `--brief` returned
  cleanly with identical counts.
- **v2 `bughunt-test` across all 4 labs (100 findings):** **4/4 `returned`**
  (0 1 MiB failures), **3.7× faster** (`8,038s → 2,175s`), **-27.5% tokens**,
  **3.3× fewer truncations** (`40/100 → 12/100`), **0 false confirmations**
  (down from 3/3), and **61/100** noisy leads dismissed by triage.
- **A/B hunt (`Qwen3.6-35B-A3B` vs `qwen3:4b`, same 3 angles):** 35B-A3B
  (3B-active MoE) was **15–17% faster** than `qwen3:4b` (`gemini-reagents`:
  310s vs 364s; `fix-4979f5c`: 2039s vs 2453s), jumped `gemini-reagents`
  recall to **14/19** (vs 9/19 v2 on 4B), and on `fix-4979f5c` located **2/2**
  gold hunks and **accurately diagnosed** `validate_grant_scope`'s
  `_ => return Ok(())` bypass. `scan-file.yaml` now defaults to
  `Qwen3.6-35B-A3B` (`num_predict: 4096`).
