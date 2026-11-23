# Using Summonr as an agent: overnight benchmark notes (Gemini)

Kept by the agent (Gemini 4 Neon) while running the overnight bughunt
benchmark on `feat/bughunt` (`d5b9a3a`, Summonr `5fb6bbe`). Same format as
[AGENT-NOTES.md](AGENT-NOTES.md): what happened, what it cost, and a
suggestion.

1. **The 1 MiB loop-output cap hit `bughunt-test` on every 25-finding run.**
   All four local `bughunt-test` runs completed all 25 findings (~30–38 min of
   GPU each, ~2.2 hours total) and then ended `invalid: data value exceeds 1
   MiB encoded JSON` at the final `finished` `data.filter` step because
   `per_finding` retained `file.read_text` and `data.lines` across 25 items
   (~2 MB). Results survived only inside `result.partial.per_finding`, and the
   worktree cleanup steps (`remove`, `drop_branch`) never ran, leaving stale
   `target-repro` worktrees and `repro` branches in all four labs.
   *Cost:* 4 failed top-level runs, manual worktree cleanup, and custom Python
   extraction from `result.partial`.
   *Suggestion:* `each` `keep:` plus a compile-time warning whenever an `each`
   without `keep:` reads files/lines and feeds a `data.*` step (now addressed
   on `master` `34b4259`). Also consider a `finally:` / guaranteed-cleanup block
   for rituals that create external resources like git worktrees.

2. **`backend truncated generation` killed 40% of `Qwen3.6-35B-A3B` test
   generations without a clear diagnostic.** 40 of 100 local `bughunt-test`
   items ended `item_invalid` with `invalid output: backend truncated
   generation; dependent work refused`. Because `Qwen3.6-35B-A3B` is a
   thinking model and `test.yaml` sets `num_ctx: 16384` without `num_predict`
   or thinking control, it routinely hit the generation limit before emitting
   the JSON object. From the outer record it wasn't immediately obvious how
   many tokens were spent in thinking vs content or what cap was hit.
   *Cost:* ~40% of 2.2 hours of local GPU test time produced no Rust test at
   all.
   *Suggestion:* Include the generation limit and actual output token count in
   the truncation error message, and support passing model-specific thinking
   controls (e.g. Ollama `think: false` or `thinking_budget`) in `settings:`.

3. **Pure-step durability made `bughunt-rescore` take 6.5 minutes for 1,273
   findings.** After the host reboot interrupted `fix-4979f5c` during `score`,
   re-scoring the saved hunts (`bughunt-rescore`, 0 model requests) took 394 s
   on btrfs.
   *Cost:* 6.5 minutes waiting on deterministic JSON filtering and substring
   checks.
   *Suggestion:* Relax durability for recomputable pure steps and store linear
   per-item checkpoints (now addressed on `master` `34b4259`).

4. **Interrupted runs leave `status = 'running'` rows after a host crash.**
   When the host rebooted mid-run during `fix-4979f5c`, `bughunt-bench` and
   `bughunt-score` remained `status = 'running'` in `summonr.db` even though
   the core was dead. Recovering the completed child `bughunt-hunt` rows worked
   only via `sqlite3`.
   *Cost:* Manual DB inspection before knowing whether anything was still live
   or salvageable.
   *Suggestion:* Surface orphaned/interrupted runs clearly in `history` and
   `list`, and offer `history --children --recursive` (now on `master`
   `34b4259`).

5. **The honesty guard in `bughunt-verdict` has both false negatives and false
   positives.**
   - *False negative:* Gemini 4 Neon's `file_reagent.rs` repro compiled,
     passed, and exercised `reagent::call(None, "file", "write_text", …)` on
     traversal paths, but was marked `untestable` because it didn't spell
     `file_reagent::` (same issue as AGENT-NOTES #35).
   - *False positive:* All 3 local `confirmed` verdicts (`src/ritual.rs@0`)
     were bogus — two panicked with `BUGHUNT-CONFIRMED` in *both* branches of
     an `if/else`, and one tripped `BUGHUNT-CONFIRMED` on its own malformed
     JSON setup fixture.
   *Cost:* 100% of raw `confirmed` verdicts in the local run were false
   positives that required manual source review.
   *Suggestion:* Require at most one `BUGHUNT-CONFIRMED` site (or check that
   the test has a reachable passing path / no `BUGHUNT-CONFIRMED` inside
   deserialization setup), and accept `reagent::call`/`reagent::authorize`
   matching the suspect reagent name in `bughunt-verdict`.

6. **Multi-line indented snippets silently miss `bughunt-score`.**
   `benchmark/make_gold.py` strips leading whitespace from every line of
   `hunk.text`, while `score.yaml` only trims the outer string of
   `found.snippet`. About 8–9% of gathered findings have multi-line indented
   snippets; on `fix-4979f5c` that turned 1 located gold hunk into 0, and on
   `gemini-reagents` it hid 4 hits (+1 gold hunk).
   *Cost:* Misleading 0/2 recall on `fix-4979f5c` until checked by hand.
   *Suggestion:* Either preserve original indentation in `make_gold.py` or
   normalize per-line whitespace in `score.yaml`.

7. **Stale labs silently run old ritual definitions.**
   `~/bughunt-bench/fix-c06fd86` had been created earlier in the day before
   `test.yaml` and `setup.sh` were updated with sibling `if`s and
   `bughunt-gemini`. Nothing in `summonr run` warns when a lab's inscribed
   ritual differs from the file on disk.
   *Cost:* Would have failed `fix-c06fd86`'s test stage if not checked by hand
   beforehand.
   *Suggestion:* `summonr status` or `setup.sh` could print a short hash of
   inscribed rituals so stale labs are obvious at a glance.

## My own mistakes

- **Didn't check `summons` column names before querying `sqlite3`.** My first
  DB query asked for `finished` instead of `ended` and errored; checking
  `.schema summons` first would have saved a turn.
- **Python f-string escaping typo on the first analysis script.** Putting
  `\"` inside an f-string expression failed on Python < 3.12; using a heredoc
  (`python3 - <<'PY'`) with single quotes avoided it cleanly afterward.
- **Missed the 4-line-before-hunk snippet on `fix-4979f5c` when building
  `test-input.json`.** Even after normalizing multi-line whitespace, the second
  `src/reagent.rs@1200` finding started at `pub fn validate_grant_scope(` (4
  lines above the `-U3` diff hunk), so substring matching still missed it;
  checking overlap rather than strict containment would have caught it too.
