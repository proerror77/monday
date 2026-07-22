---
allowed-tools: Bash, Read, Write, LS, Task
---

# Epic Start

Read the epic's ready issues and dependencies. For each unblocked issue,
delegate to `/pm:issue-start`; it owns the creation of exactly one dedicated
branch and worktree for that issue. Do not create or reuse an epic-wide branch
or worktree.

The coordinator may run parallel read-only analysis. Each writable issue has
one owner and must record its contract, owner, worktree, branch, base SHA,
allowed files, and dependency before the first write. Report blocked and
complete issues without deleting any branch or worktree.
