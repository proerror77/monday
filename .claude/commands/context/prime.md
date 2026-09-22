---
allowed-tools: Bash, Read, LS
---

# Prime Context

Read the existing sources needed for `$ARGUMENTS` or the active task. This command
is read-only; a fresh checkout does not need context generation first.

1. Read root `AGENTS.md` and the nearest applicable nested instructions. Resolve
   the requested outcome and reuse the task's recorded progress and identities.
2. Read `.claude/context/README.md`, then select the relevant row in
   `docs/agents/scenarios.md`. Follow its owning contract and implementation;
   load architecture/ADRs only under their stated conditions.
3. For local code context, read `git branch --show-current`, `git rev-parse HEAD`
   and `git status --short`. Distinguish the working tree from a named PR head or
   deployed source. Read external issue/PR/runtime state only as needed for the
   requested claim and through existing authorized access.
4. If an expected source is absent, inspect the current tracked paths and its
   caller to find the replacement. Report an unresolved gap precisely; continue
   independent work. Do not request nine generated context files or infer
   current behavior from a historical plan.

Return a concise summary of the task, exact source identity, relevant contracts,
known progress and next unresolved step. Do not print a context-file census or
claim runtime completion from source documentation.
