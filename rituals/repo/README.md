# Repository rituals

Six zero-model rituals for small repository chores. They refuse anything off-script and never touch uncommitted work. See the book's *Repositories* guide for details.

| Ritual | Does |
| --- | --- |
| `repo-status` | Each configured repository's branch, uncommitted changes, standing against its remote, and worktrees (read-only) |
| `start-branch REPO BRANCH` | Creates the branch at an up-to-date trunk, in its own worktree beside the main checkout |
| `commit-work REPO BRANCH MESSAGE` | Commits everything in that worktree with your message; refuses secrets, large files and the trunk |
| `land-branch REPO BRANCH` | Checks everything is clean and a fast-forward, runs the tests, fast-forwards the trunk, pushes, verifies the remote |
| `clean-merged REPO` | Removes worktrees and branches fully merged and clean; keeps the rest and says why |
| `open-pr REPO BRANCH` | Pushes a clean branch and opens a pull request into the trunk, or reports the open one |

## Set up

1. List each repository in `config.yaml` (the only repositories these rituals can reach):

   ```yaml
   repos:
     summonr:
       path: /home/you/Work/summonr
       trunk: master
       remote: origin
       test: cargo test --offline
   ```

2. Inscribe the rituals and grant them their reagents:

   ```bash
   for r in repo-status start-branch commit-work land-branch clean-merged open-pr; do summonr inscribe rituals/repo/$r.yaml; done
   summonr allow repo-status --from git --all
   summonr allow land-branch --from git --all
   summonr allow land-branch --from trial --all
   summonr allow clean-merged --from git --all
   summonr allow start-branch --from git --all
   summonr allow commit-work --from git --all
   summonr allow open-pr --from git --all
   summonr allow open-pr --from gh --all
   ```

3. To let an agent run them (one call instead of many):

   ```bash
   summonr familiar new claude
   summonr familiar allow claude repo-status start-branch commit-work land-branch clean-merged open-pr
   claude mcp add --scope user summonr -- summonr mcp --name claude
   ```
