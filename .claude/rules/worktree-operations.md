# Worktree Operations

One section, issue, and PR is one writable rollback unit. Create one dedicated
worktree and branch for that unit; do not place two writable agents in it.

Before the first write, record the contract, owner, worktree path, branch, base
SHA, allowed files, and dependency. Re-read the branch, `HEAD`, and status
before edits, commits, rebases, pushes, or merges. A reviewer is read-only.

Use the declared integration base, not a blind checkout of `main`. Preserve
worktrees after merge or abandonment until the repository owner explicitly
authorizes removal. `git worktree list --porcelain` is the read-only inventory
command; do not use `remove`, `prune`, or branch deletion as recovery steps.
