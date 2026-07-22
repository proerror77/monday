---
allowed-tools: Bash, Read, Write, LS, Task
---

# Epic Start

For each ready issue in the epic:

1. Read its change contract and dependency state.
2. Create one dedicated `codex/{issue-slug}` branch and
   `.worktrees/codex/{issue-slug}` worktree from its declared base SHA.
3. Record the full ownership tuple in the worktree-private path returned by
   `git rev-parse --git-path agent-worktree.yml` before assigning a writer.
4. Launch at most one writable agent for that issue. Parallel agents are
   read-only reviewers or researchers.
5. Report each issue's branch, worktree, owner, and final status.

Do not create an epic-wide worktree, share a writable worktree, prune, remove,
or delete a branch. Cleanup needs explicit repository-owner authorization.
