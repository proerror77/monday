---
name: monday-worktree-audit
description: Classify requested Monday Git worktrees as registered-clean, dirty, or Git-prunable without deleting or cleaning them. Use for explicit inventory, cleanup planning, stale-worktree, disk-usage, or ownership-conflict requests; do not use merely because ordinary work creates or uses a known worktree.
---

# Monday Worktree Audit

Produce a read-only inventory. Classification is not deletion authorization.

## Workflow

1. Scope the audit to the named paths. Run `.github/scripts/agent-worktree-preflight.sh report` for a repository-wide inventory or cleanup plan.
2. Read `git worktree list --porcelain` once and preserve each in-scope path, branch or detached `HEAD`, and Git's `prunable` marker.
3. Enumerate unattached local branches only for repository-wide inventory or branch-cleanup requests.
4. For every dirty entry, report changed and untracked paths without modifying them.
5. For any cleanup candidate, additionally read its ownership record, exact `HEAD`, upstream/push state, open or closed PR state, merge state, and active-session use.
   Lock-file existence is not lock ownership; record a lock as active only with `flock` or holder evidence.
6. Classify worktrees exactly once, using the preflight report as authoritative:
   - `registered-clean`: registered, clean, and not Git-prunable; this does not prove an active owner or session;
   - `dirty`: tracked or untracked changes exist;
   - `prunable`: Git itself marks the administrative worktree record prunable.
   Record ownership or session use only in `Owner/use`; it never changes `State`.
7. Keep `cleanup-safe` separate from those three states. It requires explicit user authorization plus clean state, no unpushed work, resolved PR disposition, no active owner/session, and a recorded recovery identity.

## Stop conditions

- Never run worktree removal, branch deletion, reset, clean, prune, or discard commands.
- If ownership, push state, PR disposition, or active use is unknown, keep the entry and mark cleanup safety `unknown`.
- A merged or newer PR never makes a nearby dirty worktree safe to remove.

## Output

For a repository-wide audit, return totals for `registered-clean`, `dirty`, and `prunable`. Then report the in-scope entries as:
`Path | Branch/HEAD | State | Dirty/unpushed | PR | Owner/use | Cleanup safety | Reason`.
Then list unattached branches as `Branch | HEAD | Upstream/unpushed | PR | Owner/use | Cleanup safety | Reason`.
List only evidence-backed cleanup candidates in a separate final section; do not recommend deletion without an exact authorized path list.
