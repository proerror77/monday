# Agent Coordination

Each section, issue, and PR has one write owner, branch, and worktree. Never
place multiple writable agents in one worktree or have them exchange work by
pulling a shared branch.

Before the first write, the owner records:

```yaml
contract: Issue #{number}: {one behavior contract}
owner: {agent or human}
worktree: .worktrees/codex/{issue-slug}
branch: codex/{issue-slug}
base_sha: {exact integration-base SHA}
allowed_files: [{paths or patterns}]
dependency: None | #{blocking-issue}
```

Write this YAML to the worktree-private path returned by
`git rev-parse --git-path agent-worktree.yml`. It is the runtime record; this
policy file is only its template and must not be overwritten.

Read-only research and review may run in parallel. A second writable change is
either a separate issue/worktree or waits for an explicit ownership transfer.
Before every edit, commit, rebase, push, or merge, re-read the branch, `HEAD`,
and worktree status; stop on unexpected movement. Report final worktree status
to the coordinator. Cleanup requires explicit repository-owner authorization.
