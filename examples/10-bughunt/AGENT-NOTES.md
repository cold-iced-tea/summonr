# Using Summonr as an agent: pain points

Kept by the agent (Claude Code) while building bughunt, as a first-hand
account from a primary customer. LIMITS.md is about what the language cannot
say; this file is about what made Summonr slow, confusing or costly to
*use* from a shell and MCP. Each entry: what happened, what it cost, and a
suggestion. Most recent last.

1. **`--wait --json` returns the whole record.** A bench run's envelope was
   30 MB (every effect of every nested step). An agent reading that
   overflows its context; I had to pipe everything through Python to find
   `outcome`, `error` and `result`. *Suggestion:* a compact default for
   `run --wait --json` (outcome, error, result, usage, run id) with the full
   record behind `record RUN_ID`, as the MCP tools already do.
2. **`ok: true` with a failed run.** The envelope is `ok: true` and
   `data.outcome: failed` with exit code 11. Easy to misread as success if
   you check `ok`. *Suggestion:* document loudly that `ok` means "the
   request was served", or put `outcome` at the top level.
3. **Config in the wrong place is silently ignored.** I wrote
   `$SUMMONR_HOME/config.yaml`; Summonr reads `$SUMMONR_HOME/config/config.yaml`.
   Nothing warned; the first sign was "`bughunt-target` is not a repository".
   Ollama still worked only because the core inherited `OLLAMA_HOST` from my
   shell. *Suggestion:* warn when `$SUMMONR_HOME/config.yaml` exists, and
   show the config path and Ollama URL the core actually loaded in
   `core status`.
4. **The core caches config and environment from when it started.** Editing
   config needs `core restart`; which environment the core saw depends on
   which command happened to start it. *Suggestion:* say so in the
   "repository not configured" error ("config loaded at <time> from <path>").
5. **Unhelpful grant syntax error.** `allow X --select list read_text` →
   "argument 7 (`read_text`): expected a clause here". *Suggestion:* when an
   unexpected bare word follows `--select`, suggest `--select a,b`.
6. **Granting is per ritual and repeats what `requires` already says.** Every
   child's authority must be granted to the child *and* declared by the
   parent, so `grant.sh` restates each `requires` block by hand.
   *Suggestion:* `allow RITUAL --as-required` (grant exactly what it
   declares, shown first), or a grimoire-wide check that lists missing grants.
7. **Children must be inscribed before parents.** Inscribing a parent whose
   child is missing fails, so scripts need a hand-maintained order.
   *Suggestion:* `inscribe DIR` that orders by dependency.
8. **`compile` hides the reason in a generic message.** `error.message` is
   "definition does not satisfy its contract"; the actual cause is in
   `data.diagnostics`. *Suggestion:* put the first diagnostic in
   `error.message`.
9. **Killing the CLI does not stop the run.** After killing a `run --wait`
   client the summon kept running in the core and holding the GPU; I had to
   `list` and `stop` each id per home. *Suggestion:* `stop --all`, and an
   option for `--wait` to cancel the summon when the client goes away.
10. **`list --json` shape had to be guessed** (array vs object with
    `summons`). *Suggestion:* document the envelope `data` shape per command.
11. **No progress for nested work.** A 15-minute bench shows `done 0
    total 0` because the work is in child summons. *Suggestion:* roll child
    progress up, or a `follow` that includes children.
12. **Long inputs cannot be passed from a file** (`Argument list too long`
    for a 100 KB list). Being fixed on `feat/language-gaps`
    (`--arg-file`).
13. **A model that loops fails the step with a backend 500**, and at
    temperature 0 a `retry` repeats the same loop. *Suggestion:* treat
    "token repeat limit" as `invalid` output (so `on_failure` and
    `abstain` logic apply) and allow a retry with a different seed.
14. **Drill `expect.result` semantics were a guess.** It turned out to be a
    subset match on listed keys, which is what I wanted, but I found out by
    trying. *Suggestion:* one sentence in the drill docs.
15. **Two homes, two interfaces.** The MCP server is bound to the real home,
    so all lab work went through the CLI with `SUMMONR_HOME` set per command;
    the compact MCP results (entry 1) were not available there.
    *Suggestion:* let an MCP call name a home, or `summonr mcp` per lab.
16. **`on_failure: continue` makes failures invisible by default.** I used
    it so one bad file or chunk would not sink a hunt, and it hid that half
    the files were failing (the data-cap bug). The counts exist
    (`steps.X.counts.failed`) but nothing surfaces them unless the ritual
    author adds steps to. *Suggestion:* a run-level warning in the envelope
    whenever an `each` finished with failed items, with the first error.
17. **The docs told me a feature did not exist.** `ritual-format.md` says
    `parallel` > 1 is rejected; it works. I designed around a limit that
    was not there. *Suggestion:* a doc test that compiles each documented
    "rejected" example and expects the rejection.

26. **A big run's record becomes unreadable.** A 30-minute bench ended
    and `run --wait --json`, `record ID --json` and `logs ID --json` all
    failed with "lost the core: frame too large": the whole record
    (mostly per-step events of child summons) is over the 64 MiB RPC
    frame. The run's results were fine; I recovered them by reading
    `state/summonr.db` with `sqlite3`, which no user should have to do.
    *Suggestion:* never send a whole record in one frame; page `record`
    and `logs`, and give `run --wait --json` / `record` a result-only form
    (outcome, error, result, usage, child ids).
27. **A cheap child's budget can throw away an expensive sibling's work.**
    The bench ran three hunts (516 model calls, 30 minutes), then `gather`
    hit its own 1-minute time essence. The bench ended `exhausted` and its
    result held none of the hunts' findings; they survived only as child
    summon rows. Pure work on ~1,100 findings took minutes, which I had
    not expected. *Suggestion:* include completed child results in an
    exhausted parent's `result.partial`; and say in the docs roughly what
    pure steps cost (each one is recorded), so time essence for pure
    rituals can be set sensibly.
28. **`history` lists only top-level summons.** Finding the child hunts
    of a failed bench needed the record, which was unreadable (26), so
    again the database. *Suggestion:* `history --children ID`.

## Reflections after the build

What it was like to build a real multi-ritual workflow, beyond single
annoyances.

18. **Structural failures show up late and live.** The 1 MiB data cap hit
    twice, each time after 10–15 minutes of GPU work, on runs that had
    compiled and drilled cleanly. Static analysis already knows the step
    kinds: a `file.read_text` (or `data.lines` of it) inside an `each` whose
    results are later passed to a data operation is an obvious size risk.
    *Suggestion:* a compile-time warning for "large read inside a loop
    without `keep`", and an estimate of loop result size in `rehearse`.
19. **Drills gave false confidence on size.** Every drill passed with
    tiny fixtures while half the real files failed. What finally caught it
    was a reviewer drilling `scan-file` against the *real* source tree
    with scripted answers, which drills already allow; I never thought to.
    *Suggestion:* document "drill against real data" as a pattern, and add
    a default scripted answer (`model: {scan: {default: {findings: []}}}`)
    so a drill over 40 files does not need 400 listed answers.
20. **The write path cannot be drilled.** `bughunt-test` (worktree, write,
    trial, cleanup) has no drill: writes are simulated, so the worktree
    never exists and `trial` fails. The most fragile ritual is the
    untested one; every bug in it was found live. *Suggestion:* a drill
    mode that runs writes for real inside a disposable copy (a temp
    clone, a temp file root), since the runtime already knows which roots
    and repos a ritual touches.
21. **Factoring into child rituals is expensive, so I put it off.** A child
    ritual is the language's only abstraction, but each one costs its own
    file, its own `requires`, a parent that repeats those `requires`,
    grants for both, and a place in the inscribe order. I kept logic
    inline in 5-level-deep YAML until the data cap forced the split, and
    the deep nesting cost me YAML indentation errors and string-surgery
    edits. The splits that did happen (`scan-file`, `verdict`) made things
    clearly better, and `verdict` became drillable because it was pure.
    *Suggestion:* make a child ritual cheap: inherit `requires` from the
    parent's declaration when the child asks for no more, and inscribe
    a directory in dependency order (entry 7).
22. **Pure rituals are where Summonr shines, and it rewards pushing logic
    into them.** gather, verdict, score and batch-ready were quick to write,
    were drilled exhaustively, and never failed live. The static checker
    caught real mistakes before any run (a loop item has no field for its
    loop variable; a misspelt output). Worth saying in the docs as the
    recommended shape: model steps small, everything else pure and drilled.
23. **Run health is hard to see while it runs.** For a 15-minute bench
    I could not tell from the CLI whether it was progressing, stuck, or
    quietly failing files; I inferred it from request counts in `list`.
    I never discovered whether `follow` shows child summons. *Suggestion:*
    one-line live progress for `run --wait` on a TTY (files, chunks,
    failures), and a mention of `follow` in `run --help`.
24. **Every lab home starts its own core, and cores outlive the work.**
    Four bench labs mean four cores; stopping a run meant stopping each
    summon, then each core, per home. *Suggestion:* cores for temp homes
    that exit when idle, and `core stop --all-homes`.
25. **Model honesty needs deterministic guards, and Summonr is good at
    them.** The 35B model twice "confirmed" a bug by testing its own copy
    of the logic; the 4B model invented ~40% of its quoted snippets and
    gave uniform 0.6–0.95 confidence. Every fix was a pure check (snippet
    really in the chunk, test names the real module, no
    `#[should_panic]`), not a better prompt. This is the product's core
    idea working as intended and worth a docs page of patterns.

## My own mistakes (lessons for an agent building with Summonr)

Not Summonr's fault, but a Summonr user that is an agent will make them too.

- **Fanning out before one worst-case smoke.** I launched multi-file,
  multi-angle benchmarks three times before running one hunt on the
  single largest real file; two of those runs were wasted on the data cap.
  Lesson: before any fan-out, run the smallest unit on the biggest real
  input.
- **Trusting the docs over the code.** I designed around "`parallel` > 1
  is rejected" without checking `compile`; one offline compile would have
  shown it works.
- **Writing config by guess.** I wrote `config.yaml` where I assumed it
  lived instead of reading `config.md` first.
- **A fix that reintroduced the bug it was near.** The test-module skip
  re-joined the file prefix per chunk, recreating the 1 MiB problem I had
  just fixed, and the drills could not see it. I tested the behaviour
  (chunks skipped) but not the cost (data carried).
- **A test before the code, mostly.** Most rituals got their drill first
  and failed first; `bench.yaml` did not, and `test.yaml` could not.
- **A process kill that matched my own shell, twice.** `pkill -f` (and a
  `pgrep` loop) on a pattern that appeared in the killing command line
  ended that shell, and I repeated it after writing this lesson down.
  Writing a lesson down is not the same as changing the habit; use exact
  pids from `ps`, or have Summonr own the stop (entry 9).
