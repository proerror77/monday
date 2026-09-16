---
name: monday-worktree-audit
description: Audit Monday worktree inventory, cleanup candidates, or ownership conflicts without mutation. Ordinary use of a known worktree is outside this audit. After a Cursor-Codex handoff, use this to confirm two writers do not share a worktree or PR.
---

# Monday Worktree Audit

Produce a read-only inventory. Classification is not deletion authorization.

## Workflow

1. Scope the audit to the requested paths. For a repository-wide inventory, run
   `.github/scripts/agent-worktree-preflight.sh report` once. For named paths,
   read `git worktree list --porcelain` and each path's status directly. Preserve
   the path, branch or detached `HEAD`, and Git's `prunable` marker; do not repeat
   an equivalent inventory or expand a named-path request to the whole repository.
2. Enumerate unattached local branches only for repository-wide inventory or
   branch-cleanup requests. For dirty entries, report changed and untracked paths.
3. Classify from the selected inventory source:
   - `registered-clean`: registered, clean, and not Git-prunable;
   - `dirty`: tracked or untracked changes exist;
   - `prunable`: Git itself marks the administrative worktree record prunable.
   Record ownership or session use separately in `Owner/use`; clean state does
   not prove an active owner or make a worktree safe to remove.
4. For cleanup candidates or ownership conflicts, read the ownership record,
   exact `HEAD`, upstream/push state, PR disposition, merge state, and active use.
   A lock is active only with `flock` or holder evidence, not mere file existence.
5. Mark `cleanup-safe` only when existing task authorization covers the exact
   paths, they are clean with no unpushed work, PR disposition is resolved, no
   owner/session is active, and a recovery identity is recorded.

## Stop conditions

- Never run worktree removal, branch deletion, reset, clean, prune, or discard commands.
- If ownership, push state, PR disposition, or active use is unknown, keep the entry and mark cleanup safety `unknown`.
- A merged or newer PR never makes a nearby dirty worktree safe to remove.

## Output

For a repository-wide audit, return totals for `registered-clean`, `dirty`, and `prunable`. Then report the in-scope entries as:
`Path | Branch/HEAD | State | Dirty/unpushed | PR | Owner/use | Cleanup safety | Reason`.
When branch inventory is in scope, list unattached branches as
`Branch | HEAD | Upstream/unpushed | PR | Owner/use | Cleanup safety | Reason`.
List only evidence-backed cleanup candidates in a separate final section; do not recommend deletion without an exact authorized path list.
