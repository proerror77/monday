# Branch Operations

A branch belongs to one independently mergeable section, issue, and PR. Its
dedicated worktree has exactly one write owner.

Before edits, commits, rebases, pushes, or merges, re-read the branch, `HEAD`,
and status. Do not pull or push another issue's branch as a synchronization
mechanism; depend on the merged predecessor or an explicitly declared stack.

Preserve branches and worktrees after merge, failure, or abandonment until the
repository owner explicitly authorizes cleanup of exact targets.
