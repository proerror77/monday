---
allowed-tools: Bash, Read, Write, LS
---

# Create Context Navigation

Make existing project knowledge discoverable for `$ARGUMENTS` or the active
task. Read root `AGENTS.md`, `.claude/context/README.md` and the relevant row in
`docs/agents/scenarios.md` first.

1. Identify the concrete task or symptom, its prerequisites, owning entrypoint,
   and observable completion or stop condition from current code and contracts.
2. Reuse an existing runbook, domain doc or repository skill. Add a missing link
   or scenario row only when it helps a new agent reach that source. If the
   underlying contract lacks necessary documentation, write it beside the
   owning code or under the applicable `docs/` directory and link to it.
3. Preserve unrelated content and historical evidence. Do not bulk-overwrite
   `.claude/context`, generate parallel project/progress snapshots, invent
   product goals, or copy host memory and credentials into the repository.
4. Verify changed links and walk the affected task from its entrypoint to the
   named command and evidence. Run `git diff --check` and any relevant existing
   documentation/contract check; do not add tests that merely assert wording.

If the existing navigation already covers the task, report the relevant path
without creating files. Otherwise report the concrete navigation change,
source evidence, validation and any unresolved gap. Persistent host-memory
writes remain outside this command.
