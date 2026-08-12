# Agent Coordination

One active contract has one write owner. Read-only research and review may run
in parallel.

Use the current checkout for an isolated local change when its ownership and
dirty state are known. Use a dedicated branch and worktree when work is
concurrent, published, or likely to span sessions. A managed worktree records
its contract, owner, path, branch, base SHA, allowed files, and dependencies in
the private path returned by `git rev-parse --git-path agent-worktree.yml`.

Stop on overlapping ownership or unexpected branch movement. Re-read branch,
`HEAD`, status, and PR head before publishing or merging. Ownership handoff does
not create a new issue; update the existing contract record.

Never remove a worktree or branch without explicit repository-owner
authorization for the exact target.
