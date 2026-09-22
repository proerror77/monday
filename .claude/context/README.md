# Project Context

Monday keeps shared context beside the contracts and code it describes. This
directory is a navigation entrypoint, not a second project-state database.

- Read [AGENTS.md](../../AGENTS.md) for authority, ownership, delivery and evidence.
- Use the [task and symptom map](../../docs/agents/scenarios.md) to select the
  relevant command, runbook, prerequisites and completion evidence.
- Use [domain docs](../../docs/agents/domain.md) for architecture and ADR pointers.
- Use the [issue tracker](../../docs/agents/issue-tracker.md) and the task's
  issue/PR/artifact identities for work that spans sessions. Refresh the named
  source or runtime identity before reporting its current state.

`/context:prime` reads the relevant existing sources. `/context:create` adds
missing navigation for a concrete task. `/context:update` corrects affected
links or owning documentation when behavior changes. None requires generated
`project-overview.md`, `progress.md`, or other parallel context snapshots.

Repository skills under [.agents/skills](../../.agents/skills/) are shared with
the checkout. Host-installed skills, conversation memory and credentials are
separate resources; their presence on one host does not establish availability
on another. Context commands do not authorize persistent host-memory writes.
