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
   flatten. About 60% of `hunt.yaml` is this plumbing.
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
7. **`each` is sequential** (`parallel > 1` is rejected). A hunt is one
   model call per chunk, one after another.

## Pointing a ritual at a project

8. **File roots are fixed at inscription.** `requires.on.root` is a
   literal, so a ritual cannot be "pointed at" a project. *Workaround:*
   `setup.sh`/`drill.sh` rewrite `/absolute/path/to/...` placeholders and
   re-inscribe (the same trick example 08 uses). *Proposal:* a root input
   whose value must fall inside a granted root, checked at run time.
9. **`file.list` is not recursive.** Directories to hunt are an input
   list. *Proposal:* `file.walk` with depth and entry bounds.
10. **The CLI cannot read an input from a file.** A 98-finding list or a
    gold set overflows the command line ("Argument list too long").
    *Workaround:* `bughunt-bench` reads the gold set with `file.read_json`
    and runs hunt and score in one summon; the rite passes findings between
    stages internally. *Proposal:* `--arg name=@file.json`.

## Running tests

11. **`trial.run` runs the repository's single `test` command.** No test
    selection. *Workaround:* a dedicated repo entry, `bughunt-target`, whose
    `test` is `cargo test --offline --bins bughunt_repro`.
12. **`git.worktree_add` starts only from an up-to-date trunk and needs
    `<remote>/<trunk>` to exist.** *Workaround:* `setup.sh` checks the
    target commit out as a `bughunt-base` branch and pushes it to a local
    bare remote in the lab.
    **Bug found:** when the remote has no trunk branch, `worktree_add`
    fails with git's raw `rev-list` usage text
    (`git rev-list: 'git <command> [<revision>...] -- [<file>...]'`), not
    a message saying the remote branch is missing
    (`src/reagents/git_reagent.rs`, `worktree_add`).
13. **Summonr is a binary crate**, so a repro in `tests/` could reach no
    internals. *Workaround:* the base branch commits an empty
    `src/bughunt_repro.rs` declared as `#[cfg(test)] mod bughunt_repro;`.
    This harness is Rust-specific; nothing in the language describes "how
    to run one test in this project".

## Models

14. **Model names are fixed per step.** Routing by severity means one
    model step per model in `if` branches, with the prompt duplicated.
    *Proposal:* a step-level `model: {one_of: [...], choose: EXPR}` that
    keeps the set of model sites static.
15. **Codex is not a model backend.** It can only be a `cmd` command
    (`bughunt-codex`, read-only sandbox), so its answer is unvalidated
    stdout: no schema, no fingerprint, no token essence.
    *Proposal:* a command-backed model backend, or an output schema on
    `cmd.read`.

## Rites

16. **A rite has a fixed number of stages**, so "one hunt per angle" is
    three hard-coded stages; angles are inputs, the count is not.
17. **A touchstone gate sees only the stage's output**, so `gather` has to
    return `min_batch` and `pass` for `batch-ready` to read.
18. **Assumptions are not checkable in the language.** `target-commit` is
    a statement; nothing reads the lab's HEAD (the hunt has no `git`
    authority, and adding it only for this would widen the grant).

## Rough edges met on the way

- `config.yaml` belongs in `$SUMMONR_HOME/config/`; one written to
  `$SUMMONR_HOME/` is silently ignored, and the core reads config only at
  start (`setup.sh` restarts it).
- Child rituals must be inscribed before parents; re-inscribing changed
  content needs `--replace`.
- `allow --select` takes one comma-separated list, not repeated words.

## Results

See the README for the commands. Model `ollama/qwen3:4b` unless noted;
RX 7900-class GPU.

RESULTS_PLACEHOLDER
