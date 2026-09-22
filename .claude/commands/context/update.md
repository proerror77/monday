---
allowed-tools: Bash, Read, Write, LS
---

# Update Context Navigation

Update the shared documentation affected by `$ARGUMENTS` or an evidenced change
in the active task. Follow root `AGENTS.md` and `.claude/context/README.md`.
There is no mandatory session-end snapshot or timestamp refresh.

1. Identify the changed behavior and exact source/diff. Use the task's existing
   evidence; an arbitrary last-five-commits window may include other work.
2. Follow `docs/agents/scenarios.md` to the owning contract and implementation.
   Correct outdated commands, prerequisites, boundaries or evidence claims
   there. Update navigation links if an entrypoint moved.
3. Preserve dated reports and applied history. Record active progress on the
   existing task or issue according to `docs/agents/issue-tracker.md`; do not
   fabricate a project-wide completion percentage or infer deployed state from
   recent commits. Keep host memory and credentials outside tracked docs.
4. Verify changed links, walk the affected task paths, review the actual diff,
   and run `git diff --check` plus any relevant existing contract check. Reuse
   still-valid evidence and preserve unrelated writers' changes.

Report what changed and why, the checks performed and remaining limits. If no
documentation needs correction, say so and leave files unchanged. This command
does not authorize persistent host-memory writes.
