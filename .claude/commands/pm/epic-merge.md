---
allowed-tools: Bash, Read, Write, LS
---

# Epic Merge

Merge each issue PR in its declared dependency order. Before each merge,
re-read the PR head, branch, `HEAD`, CI state, and rollout/rollback impact.
After a merge, report the corresponding worktree and branch as preserved,
merged, or blocked. Never remove worktrees, prune metadata, or delete branches
unless the repository owner explicitly authorizes the exact targets.
