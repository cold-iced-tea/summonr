# Bughunt: a bug-hunting rite written in plain Summonr

Date: 2026-11-22. Status: design, awaiting approval.

## Intent

Build `bughunt`, a rite you point at a project to find bugs and ways to break
or circumvent it, written only with what ships in Summonr: rituals, a rite,
and the standard reagents. Anyone who downloads Summonr could have written it.

Two reasons, in order:

1. **Measure the language.** Use the smallest local model that can do the job
   and record every place where the ritual language could not say what was
   wanted. That log is the main deliverable.
2. **A useful bughunter.** Fast, cheap detection for trying new bug ideas and
   attack angles, then a slower stage that actually tries to break the code.

## Goals

- Stage 1 (hunt): fast local model, one angle per run ("authority bypass",
  "panics on bad input", ...), structured findings. Runnable alone with
  `summonr run` for quick experiments.
- Stage 2 (test): for each finding, write a test that should fail if the bug
  is real, run it on a throwaway checkout, mark the finding `confirmed`,
  `refuted` or `untestable`.
- A rite that wires several hunts → gather gate → test.
- Scored against bugs proven by real fix commits.
- `LIMITS.md`: every language gap hit while building, with the workaround used.

## Non-goals

- No new built-in CLI command and no Rust changes in v1. A missing feature is
  logged in `LIMITS.md` and worked around (or the step is dropped), not added
  mid-build. Fixing limits is a later project.
- Not fixing any of the bugs found.
- No run-to-run memory store (user decision 2026-11-21).

## Anti-goals

- A hunter whose output is mostly noise that a human has to sort.
- Hidden state: everything a run keeps is in its record or a file it writes
  explicitly.
- Anything a downloader could not reproduce (private reagents, hand-edited
  stores, special config beyond documented `config.yaml` entries).

## Constraints

- master stays at `5fb6bbe`: it is the target for the secondary benchmark.
  Work happens on `feat/bughunt`.
- Model names are static per step (model sites are known at compile time), so
  severity routing is `if`/`else` over fixed model names.
- `git` and `trial` act only on repos listed in `config.yaml` `repos:`; the
  target project must be registered there (a documented step, fine for a
  downloader).
- Local models available: `ollama/qwen3:4b` (default),
  `qwen36-lite`, `Qwen3.6-35B-A3B` Q4 (step up). Ollama must be running.
- Codex is not a model backend. Cloud escalation can only go through the `cmd`
  reagent running `codex exec`; it is optional and off by default.

## Approaches considered

| Approach | Trade-off | Verdict |
| --- | --- | --- |
| Built-in `summonr bughunt` CLI command | Easiest to use, but proves nothing about the language and isn't something a downloader writes | Rejected |
| Two rituals plus a queue file (`file.append_jsonl`) that the test ritual drains once N findings exist | Findings accumulate loosely across days; batch boundaries fuzzy; trigger is manual or cron | Rejected for v1 |
| **Rite: hunt stages → gather gate → test stage** | Batch is fixed when the rite starts (angles chosen up front), but it is resumable, gated, rerunnable and scoreable; assumption on target commit reopens the test if code moves | **Chosen (user)** |

Stage 2 options: **re-read and judge** (cheap, still an opinion) vs **try to
break it** with `trial` on a throwaway worktree (slower, "confirmed" means it
actually broke). **Chosen: break it** (user took the recommendation).

## Design

Lives in `examples/10-bughunt/`, like the other examples:

```
examples/10-bughunt/
  README.md            how to register a target repo and run it
  hunt.yaml            stage 1 ritual
  gather.yaml          touchstone (requests: 0): merge, dedupe, count, gate
  test.yaml            stage 2 ritual
  bughunt.rite.yaml    the rite
  score.yaml           touchstone: compare findings with a gold set
  benchmark/           gold sets (see Scoring)
  LIMITS.md            the language-limits log
```

### hunt (stage 1)

- Inputs: `repo`, `angle` (free text), optional `paths` (globs/prefixes),
  `max_files`.
- `file.list` the source tree → `data.filter` by extension/prefix →
  `data.slice` to `max_files`.
- `data.map` (or `each`) over files: `file.read_text`, split into line-numbered
  chunks if large (`data.lines` + `data.slice`), one model call per chunk with
  the angle in the instructions.
- Model `ollama/qwen3:4b`, temperature 0, schema per finding:
  `{file, line, kind: bug|bypass, claim, trigger, severity: low|medium|high|critical, confidence: 0-1}`,
  empty list allowed (abstaining is fine).
- Returns `{angle, commit, findings: [...]}`. Read-only: no write grants.

### gather (gate)

Pure touchstone ritual: concatenate hunt outputs, dedupe on
`file + line ± window + kind`, drop below a confidence floor, count. The gate
passes when `count >= min_batch` (rite input). An owner gate can be added
instead or after, so the user skims before the expensive stage.

### test (stage 2)

Per finding:

1. `git.worktree_add` a throwaway branch at the hunted commit (repo marked
   `disposable` paths as needed).
2. Model writes a minimal test that should **fail if the claim is true**,
   with the file path to put it in; `file.write_text` into the worktree.
3. `trial.run` with that test selected.
4. Verdict: test fails for the stated reason → `confirmed`; passes →
   `refuted`; doesn't compile / wrong failure → `untestable` (one retry
   with the compiler error fed back).
5. `git.remove_worktree`.

Model routing by severity, as fixed `if` branches:
low/medium → `ollama/qwen3:4b`; high/critical → `Qwen3.6-35B-A3B`;
optional `escalate: codex` input routes critical findings to `cmd` →
`codex exec`. Start with the small model everywhere and only step up where the
scores show it is needed.

Returns each finding plus `verdict`, the test source, and the trial tail.

### The rite

```
hunt(angle A) ─┐
hunt(angle B) ─┼─▶ gather [touchstone gate: count ≥ min_batch] ─▶ test
hunt(angle C) ─┘
assumption: target-commit = <sha>   (falsify → test reopens)
```

Inputs: `repo`, `commit`, `angles` (one stage each; if a rite can't take a
variable number of stages, ship three fixed angle stages and log the limit),
`min_batch`.

### Scoring

- **Primary gold set: fix commits.** For each real fix commit (review fixes
  `c4c5856`…`ee3c47f`, Batches F/G where they fixed behaviour, the
  `fix(sight)` commits, `d48ca09`, `5fb6bbe`), the target is the parent
  commit and the gold is the `src/` hunks the fix changed. A finding hits if
  its file matches and its line falls inside a changed hunk ± 10 lines.
  Extracted once into `benchmark/fixes.json` (a script, or a ritual using
  `git` if the reagent can produce diffs; otherwise logged as a limit).
- **Secondary: the 125-bug Gemini list** at `5fb6bbe`, converted to
  `benchmark/gemini-125.json`. Used for recall only, flagged unverified;
  stage-2 `confirmed` results on it count as spot checks of the list.
- `score.yaml` (touchstone) reports hunt recall, noise (findings not in gold),
  and test-stage precision (confirmed that are in gold).

### LIMITS.md format

One entry per gap: what bughunt needed, what the language offered, the
workaround used (or "dropped"), and a one-line proposal. Measured cost
(requests, tokens, wall time) per stage goes at the end.

## Left to the builder

- Prompts, chunk size, dedupe window, confidence floor, default `min_batch`.
- Whether `file` grants can be scoped to a worktree path created during the
  run; whether `git.worktree_add` can start at an arbitrary commit; whether
  `trial.run` can select one test. Spike these first; each "no" is a
  `LIMITS.md` entry with a workaround.
- Where repro tests go so they can reach internals (integration tests only see
  public API; that restriction is itself likely a limit worth logging).
