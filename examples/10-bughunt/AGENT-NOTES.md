# Using Summonr as an agent: pain points

Kept by the agent (Claude Code) while building bughunt, as a first-hand
account from a primary customer. LIMITS.md is about what the language cannot
say; this file is about what made Summonr slow, confusing or costly to
*use* from a shell and MCP. Each entry: what happened, what it cost, and a
suggestion. Most recent last.

**Status (2026-11-22):** addressed on branch `feat/language-gaps` (not yet
merged): 1 and 26 (`brief` / `--brief` on `run --wait` and `record`),
3 (misplaced `config.yaml` warning), 8 (first diagnostic in
`error.message`), 12 (`reading` / `--arg-file`), 13 (a looping model is
`invalid`), 17 (the `parallel` doc), 19 (`model_default` in drills), and
the dedupe half of 29 (`data.unique_by`). 14 was my oversight: the drill
guide already says `result` is a subset match. The rest are open.

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
    trying. *Correction:* `guides/safe-building.md` already says so; I had
    not read it. The real lesson is discoverability: `summonr drill --help`
    could point at that page.
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

29. **Pure work is slow because every step is a durable, fully recorded
    event.** Rescoring 1,135 findings against 2 gold hunks took 156 s in
    `score` (~137 ms per finding, ~28 events each); the lab database
    reached 338 MB, of which 291 MB is 83,571 events (`scan-file` alone:
    222 MB, ~5 KB per event, since events carry whole arguments and
    outputs such as chunk text). Checkpoints were small (3.4 MB, replaced
    per item, though rewritten whole each time). *Suggestions:* batch pure
    steps' events into one transaction per loop item (they are
    deterministic and recomputable); record pure steps by hash and size
    rather than full payload; append to `each` checkpoints instead of
    rewriting them. The same change fixes entry 26 at its source.
    Language side: `data.unique_by {values, fields}` and a lookup/join
    operation would turn `gather`'s dedupe loop and `score`'s nested loops
    into one call each.

30. **`cmd` cannot carry a multi-line prompt.** Parameters may not contain
    newlines (a sound injection rule), so calling a CLI model such as
    Codex through `cmd` needs a one-line prompt; the first Codex run failed
    on that, and the failure only showed as `failed: 1` in the item counts
    (entry 16 again). *Suggestion:* let a command declare one parameter as
    stdin (`stdin: prompt`), which is where multi-line text belongs, or a
    command-backed model backend (LIMITS 16).

31. **A `cmd` command's failure is invisible, and it gets no environment.**
    Commands start with an empty environment (sound), and there is no
    config field to pass a variable through, so `codex` (a wrapper that
    needs `HOME`) exited at once. The step recorded "0 chars"; neither the
    exit code nor stderr appeared in the events, so finding the cause took
    running the command by hand with `env -i`. *Correction (2026-11-23):*
    `cmd` actually inherited the core's environment minus secrets rather
    than clearing it; `codex` lacked `HOME` because of the environment
    that lab core started with. Explicit `env: [NAME, ...]`, `stdin:`,
    and exit-code/stderr diagnostics are now implemented on `master`
    (`5b464b5`). *Suggestion:* an `env:` allowlist per command (`env:
    [HOME, PATH]`), and always record the exit code and a stderr tail in
    the step's event, as `trial` does.

32. **Steps inside an `if` nested in an untaken branch are never
    recorded.** The docs say steps in a branch not taken are `skipped` with a
    `null` output; that holds for the branch's own steps, but an `if` inside
    it is not visited, so its steps have no record at all and a later
    reference fails at run time ("step `write_small` has not produced a
    value"). The static checker accepted the reference. Found only when
    the Codex branch first ran. *Suggestion:* record every step under an
    untaken branch as skipped, recursively, or make the checker reject the
    reference. Workaround: sibling `if`s with combined conditions.

33. **Codex declines exploit-shaped findings, even as a regression test.**
    For a "critical" CRLF header-injection finding Codex answered "I can't
    provide a targeted reproduction of the header-injection bypass", both
    with the original prompt and when reworded (with the user's approval)
    as a regression test asserting the safe behaviour; the finding text
    itself describes an attack. The verdict was correctly `untestable`, and
    the refusal was visible only because the test source is kept in the
    result. Codex also assumed it should write the file and noted its
    sandbox is read-only, although the prompt asks it to print the file.
    Not a Summonr issue; for the rite it means cloud escalation helps with
    correctness bugs more than with security findings, which stay with the
    local models. The Codex path itself now works (12 s per finding).

34. **A `read` command can write, if it wraps an agent.** The Gemini path
    runs the Antigravity CLI (`agy -p`, `--sandbox`) as a `cmd` command
    declared `effect: read`. Neon answered well, and also used its own
    file tools to write `src/bughunt_repro.rs` into the lab's main
    checkout; `--sandbox` restricts its terminal, not its edits. Summonr
    recorded a read. Nothing in Summonr can see or stop what a wrapped
    agent does with its own tools; the declared effect is the operator's
    claim. *Workaround:* agent CLIs run in a read-only snapshot of the
    source (`setup.sh` makes `LAB/readonly-src`). *Suggestion:* document
    that `cmd` effects are declarations, and offer a filesystem sandbox for
    commands (read-only binds, a private writable temp), or a
    command-backed model backend that runs the CLI in such a sandbox.

35. **Neon versus Codex on the same finding.** Codex declined the CRLF
    finding twice. Gemini 4 Neon (via `agy`) wrote a real test against
    Summonr's own `reagent::authorize(…, "http.post", …)`; its first
    version did not compile, the retry went back to Neon (the retry now
    uses the model that wrote the first attempt), and the second version
    failed with the marker: authorization accepted a CR/LF header value
    (`Ok(())`). The verdict was still `untestable`, because the honesty
    guard wants the suspect module's name (`http_reagent::`) and this test
    reached it through the generic dispatcher. The guard was written to
    stop copied-logic tests and here rejected an honest one; loosening it
    (e.g. also accepting the tool's dispatch name) is a trust decision left
    to the owner. Also: confirming that authorize does not check header
    values is not the same as an exploit; the HTTP client may still refuse
    CR/LF when sending. 87 s for the finding, including two builds.

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
