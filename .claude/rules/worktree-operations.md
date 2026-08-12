# Worktree Operations

A worktree isolates concurrent, published, or multi-session writes. It is not
required for read-only work or an isolated local change with known ownership.

Each writable worktree has one owner. Before its first write, record the
contract, owner, path, branch, base SHA, allowed files, and dependencies in the
private `agent-worktree.yml` path returned by Git.

Stop on overlap or unexpected movement. Preserve worktrees after merge or
abandonment until the repository owner explicitly authorizes exact cleanup;
never use removal, prune, reset, or branch deletion as recovery.
