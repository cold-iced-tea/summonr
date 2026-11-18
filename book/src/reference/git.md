# The `git`, `gh` and `trial` reagents

All three act only on repositories listed under `repos:` in `config.yaml`, by name. Commands run without a shell, with prompts disabled and no input, and are stopped at the run's time limit. Branch and ref names must be plain (letters, digits, `.`, `_`, `/`, `-`; no `..`, `@{`, or trailing `/`, `.`, `.lock`).

## `git`

| Operation | Effect | Arguments | Output |
| --- | --- | --- | --- |
| `repos` | read | | configured repository names |
| `info` | read | `repo` | `{trunk, remote}` |
| `status` | read | `repo`, `worktree`? | `{branch\|null, clean, changed}` (untracked files count) |
| `ahead_behind` | read | `repo`, `from`, `to` | `{ahead, behind}`: commits only in `from`, only in `to` |
| `merged` | read | `repo`, `branch` | `{merged}`: contained in the trunk |
| `head` | read | `repo`, `ref` | `{commit}` |
| `remote_head` | read | `repo`, `branch` | `{commit\|null}` on the remote |
| `worktrees` | read | `repo` | `[{path, branch, head, clean, main}]` |
| `worktree_for` | read | `repo`, `branch` | `{path\|null, clean, changed}` |
| `branches` | read | `repo` | `[{name, merged, current}]` |
| `branch_exists` | read | `repo`, `branch` | `{exists}` |
| `cleanup_plan` | read | `repo` | worktrees and branches with `action` (`remove`/`keep`) and `reason` |
| `fetch` | read | `repo` | updates remote-tracking refs only |
| `merge_ff` | write | `repo`, `branch` | fast-forwards the trunk; refuses a dirty or off-trunk main checkout and any non-fast-forward |
| `push` | write | `repo`, `branch` | pushes one branch, never forced |
| `delete_branch` | write | `repo`, `branch` | merged branches only; never the trunk or a checked-out branch |
| `remove_worktree` | write | `repo`, `path` | clean, listed, non-main worktrees only |
| `worktree_add` | write | `repo`, `branch` | new branch at an up-to-date trunk, in `<main>-<branch>` beside the main checkout |
| `commit` | write | `repo`, `branch`, `message` | that branch's own worktree only; refuses secrets, files over 5 MiB, empty commits |

## `gh`

| Operation | Effect | Arguments | Output |
| --- | --- | --- | --- |
| `pr_for` | read | `repo`, `branch` | `{url\|null}` of the open pull request |
| `pr_open` | write | `repo`, `branch`, `title`?, `body`? | the open pull request, or a new one into the trunk: `{url, existing}` |
| `pr_create` | write | `repo`, `branch`, `title`, `body` | `{url}` |

Uses the `gh` program (set `gh:` in `config.yaml`) and its login.

## `trial`

| Operation | Effect | Arguments | Output |
| --- | --- | --- | --- |
| `run` | read | `repo`, `worktree`? | `{passed, code, tail}`: the repository's `test` command, its exit code and last 30 lines |

A test command that outlives the run's time limit is stopped and reported as not passed.
