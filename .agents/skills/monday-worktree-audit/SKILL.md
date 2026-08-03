---
name: monday-worktree-audit
description: Classify Monday Git worktrees as active, dirty, or Git-prunable without deleting or cleaning them. Use for worktree inventory, branch cleanup planning, disk-usage review, stale-worktree questions, ownership conflicts, or before any request to remove a worktree or branch.
---

# Monday Worktree Audit

Produce a read-only inventory. Classification is not deletion authorization.

## Workflow

1. From the repository, run `.github/scripts/agent-worktree-preflight.sh report`.
2. Read `git worktree list --porcelain` and preserve each exact path, branch or detached `HEAD`, and Git's `prunable` marker.
3. For every dirty entry, report changed and untracked paths without modifying them.
4. For any cleanup candidate, additionally read its ownership record, exact `HEAD`, upstream/push state, open or closed PR state, merge state, and active-session use.
5. Classify:
   - `active`: registered, clean, and not Git-prunable;
   - `dirty`: tracked or untracked changes exist;
   - `prunable`: Git itself marks the administrative worktree record prunable.
   Record ownership or session use only in `Owner/use`; it never changes `State`.
6. Keep `cleanup-safe` separate from those three states. It requires explicit user authorization plus clean state, no unpushed work, resolved PR disposition, no active owner/session, and a recorded recovery identity.

## Stop conditions

- Never run worktree removal, branch deletion, reset, clean, prune, or discard commands.
- If ownership, push state, PR disposition, or active use is unknown, keep the entry and mark cleanup safety `unknown`.
- A merged or newer PR never makes a nearby dirty worktree safe to remove.

## Output

Return totals for `active`, `dirty`, and `prunable`, followed by:
`Path | Branch/HEAD | State | Dirty/unpushed | PR | Owner/use | Cleanup safety | Reason`.
List only evidence-backed cleanup candidates in a separate final section; do not recommend deletion without an exact authorized path list.
