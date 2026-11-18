# Repositories: git and GitHub chores

Small repository chores (starting a branch, committing work, landing a branch, cleaning up merged worktrees, checking status, opening a pull request) are mostly mechanical. Summonr does them as rituals with **zero model calls**. An agent that would otherwise run a dozen commands, re-reading its whole conversation at each one, makes **one** call. You can run the same rituals yourself.

When anything is off-script, a ritual **refuses** with the exact reason, and only then does a person or an agent need to think.

## The one rule

These rituals **never touch uncommitted work**. They don't stash, reset, clean, force, rebase, check out or switch over changes. They don't remove a worktree that has anything uncommitted, untracked files included. If a step would need any of that, the ritual stops and says what's in the way.

**Ignored files are protected too.**

- `land-branch` refuses if merging would overwrite an ignored file in the main checkout, such as a local `.env` at a path the branch adds.
- `clean-merged` keeps any worktree holding ignored files, unless you've listed them as disposable for that repository. Build output is the usual case:

  ```yaml
  repos:
    summonr:
      path: /home/you/Work/summonr
      test: cargo test --offline
      disposable: [target/]
  ```

**Locked or already-deleted (prunable) worktrees** are reported and kept.

`land-branch` merges **exactly the commit it tested**. If the branch moves while the tests run, it refuses.

When a run ends, anything the test command left running is stopped, as is everything at the time limit.

## Setting up

List each repository in `config.yaml`. Rituals can reach only these, and only by name:

```yaml
repos:
  summonr:
    path: /home/you/Work/summonr   # the main checkout
    trunk: master                  # default
    remote: origin                 # default
    test: cargo test --offline     # what land-branch runs
    disposable: [target/]          # ignored paths a removed worktree may lose
```

The test command is split into words and run directly, never through a shell, so `$HOME` or `;` in it are passed literally. It's yours to set; a ritual or an agent can't choose or change it.

Then inscribe and grant the rituals (the commands are also in `rituals/repo/README.md`):

```bash
for r in repo-status start-branch commit-work land-branch clean-merged open-pr; do summonr inscribe rituals/repo/$r.yaml; done
summonr allow repo-status  --from git --all
summonr allow land-branch  --from git --all
summonr allow land-branch  --from trial --all
summonr allow clean-merged --from git --all
summonr allow start-branch --from git --all
summonr allow commit-work  --from git --all
summonr allow open-pr      --from git --all
summonr allow open-pr      --from gh --all
```

Grants give a ritual the git operations it needs; your `repos:` list decides which repositories exist for it at all.

## The rituals

A piece of work goes through them in order: `start-branch`, then the work itself, then `commit-work`, `land-branch` and `clean-merged`.

### `start-branch REPO BRANCH`

```bash
summonr invoke start-branch given repo=summonr branch=pacts
```

Creates the branch at the trunk, in a new worktree beside the main checkout: `~/Work/summonr` + `pacts` gives `~/Work/summonr-pacts`, and a `/` in the name becomes `-`. It refuses if:
- the branch or the folder already exists;
- the main checkout isn't on the trunk;
- the trunk is behind its remote (it fetches first).

It returns the new worktree's path and commit.

### `commit-work REPO BRANCH MESSAGE`

```bash
summonr invoke commit-work given repo=summonr branch=pacts message="Add pact schema"
```

Commits everything in the branch's own worktree (changes and new files) with your message, exactly as given; add any trailer you want yourself. It refuses:
- the trunk, and a branch with no worktree;
- an unfinished merge, rebase, cherry-pick or revert, and unresolved conflicts;
- an empty commit, an empty message, or one with control characters;
- files that look like secrets: `.env`, `.env.*`, `.envrc`, `.netrc`, `.npmrc`, `.pypirc`, `.pgpass`, `*.pem`, `*.key`, `*.p12`, `*.pfx`, `*.keystore`, `*.jks`, `*.gpg`, `id_rsa*`, `id_ed25519*`, `id_ecdsa*`, `id_dsa*`, `secrets.*`, `service-account*`, and anything with `credentials` in its name, in any folder;
- files over 5 MiB, and any changed file it can't read.

Those checks happen before anything is staged, so a refusal leaves your files exactly as they were. Your git hooks run as usual. If one fails, the changes are left staged and the refusal says so; nothing is committed. It returns the commit and the files it included, and verifies the worktree is clean afterwards.

### `land-branch REPO BRANCH`

```bash
summonr invoke land-branch given repo=summonr branch=epitaphs
```

In order, it refuses if:
- the branch *is* the trunk;
- the branch has no worktree ("check out BRANCH in a worktree so it can be tested");
- the main checkout isn't on the trunk, or has uncommitted changes;
- the branch's worktree has uncommitted changes;
- the trunk is behind its remote ("bring it up to date first"); it fetches first, and never pulls;
- the branch doesn't fast-forward the trunk, or adds nothing;
- the tests fail ("tests failed: …last lines…").

Then it fast-forwards the trunk, pushes it, and **verifies** that the remote now points at the branch's commit. It returns the commit and the last lines of the test output.

### `clean-merged REPO`

Removes every worktree whose branch is fully merged into the trunk and that has nothing uncommitted, then deletes branches that are merged and no longer checked out anywhere. It never deletes the trunk or the checked-out branch. Everything it keeps is listed with a reason: "uncommitted changes: …", "not merged into master", "checked out in …".

### `repo-status`

Read-only. For every configured repository: the branch, whether it's clean and what's changed, how far the trunk is ahead of or behind its remote, and every worktree.

### `open-pr REPO BRANCH [title=…] [body=…]`

Refuses a dirty branch worktree or the trunk. Pushes the branch (verified), then returns the open pull request for it if there is one, or opens one into the trunk (the title defaults to the branch name). It uses your own `gh` login; Summonr stores no GitHub token.

## Letting an agent do it

```bash
summonr familiar new claude
summonr familiar allow claude repo-status start-branch commit-work land-branch clean-merged open-pr
claude mcp add --scope user summonr -- summonr mcp --name claude
```

Now "land the epitaphs branch" costs the agent one `run_ritual` call. These rituals run their writes straight away, pushes included, because you chose that for them. To make a write wait for your approval instead, give the step `hold: true` in your copy of the ritual.

Every run leaves an [elegy and an epitaph](../concepts/epitaphs.md), so each landing is a signed record of exactly what was merged and pushed.

## Later: rites

A **rite** will coordinate rituals across stages that need to pause and resume: for example, open a pull request, wait for review, then land it. That needs durable stages and gates that rituals don't have yet, so it's reserved for now and will be designed alongside pacts.
