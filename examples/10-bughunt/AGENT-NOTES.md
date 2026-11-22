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
