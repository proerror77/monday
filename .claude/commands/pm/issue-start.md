---
allowed-tools: Bash, Read, Write, LS, Task
---

# Issue Start

Verify the issue has one change contract, acceptance evidence, out-of-scope
boundary, and declared dependency. Then create one dedicated worktree and
record its contract, owner, path, branch, base SHA, allowed files, and
dependency in the worktree-private `agent-worktree.yml` path returned by
`git rev-parse --git-path agent-worktree.yml`.

Launch one writable agent only. Any concurrent analysis or review is read-only.
Stop if ownership, the base SHA, or file scope is ambiguous. Do not clean up a
worktree or branch without explicit repository-owner authorization.
