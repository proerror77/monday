# Cursor ↔ Codex handoff

Monk routes work between Cursor and Codex with a written packet. There is no
agent-to-agent chat bus, queue, or Hermes/Codex/Cursor mutual-chat shell.

Authority, trading gates, and delivery rules stay in `AGENTS.md`. This file only
says who writes, on which branch, and what the packet must contain.

## Roles

| Actor | Owns | Does not own |
| --- | --- | --- |
| Monk | Human-facing orchestration: route the packet, name one writer, stop overlapping writers | Code edits, research runs, trading, or extra authority |
| Codex | Deep research, campaign diagnosis, and plans. Use `gpt-5.6-sol` high when researching Monday | Repository implementation PRs unless Monk names Codex the single writer |
| Cursor Cloud Agent | Repository code changes, implementation PRs, and their CI | Inventing a second writer on a Codex research branch |

GitHub source of truth is `https://github.com/proerror77/monday`. A local Mac
checkout is not a shared workspace.

## When to use which

Use Codex when the next action is diagnosis, campaign evidence, a research plan,
or a bounded investigation that should not mutate `main` through an
implementation PR.

Use Cursor when the next action is a repository code change, docs/skill change,
implementation PR, or CI/merge for that PR.

Keep Codex on the research side when a draft research PR already exists, such as
[#1141](https://github.com/proerror77/monday/pull/1141) on
`codex/poly-data-research-integration`. Do not treat that draft as a Cursor
implementation branch.

Monk copies the packet. Agents do not message each other.

## Packet

Paste this block on the issue, PR, or Monk prompt. Every field is required.
Write `none` rather than omitting a field.

```text
From: Codex | Cursor
To: Cursor | Codex
Routed by: Monk
Goal: <one observable outcome>
Evidence paths: <issue/PR URLs, SHAs, artifact paths, campaign/run IDs>
Constraints: <in/out of scope, analysis-only, no collector/runtime mutation>
Done criteria: <exact identities to read back>
CONFIRM / trading gates: <none | human CONFIRM still required; named fail-closed gates that stay closed>
Branch: <codex/… | cursor/…>
Writer: <one agent, one worktree, one PR>
```

`CONFIRM / trading gates` is `none` only when the work cannot submit orders,
change risk limits, resume a paused runtime, open a sealed holdout, or bypass a
signed grant. If any of those could apply, name the still-closed gate and that
human CONFIRM is required. A research or docs packet does not open Live.

Done criteria are identities, not vibes: PR URL, head SHA, required checks, or
the research run/artifact digest to read back.

## Branch naming

- `codex/<slug>`: research, diagnosis, or a Codex-owned draft/research PR.
- `cursor/<slug>`: Cursor-owned implementation against `main`.

Do not reuse a `codex/…` branch for a Cursor implementation unless Monk
transfers exclusive write ownership of that exact PR and worktree. The default
is a new `cursor/…` PR that `Refs` the research issue or PR.

## PR ownership

Cursor opens and implements the code PR. Codex may leave a draft or research PR
on `codex/…`; that PR stays Codex-owned until Monk reassigns the writer.

Monk routes. Neither agent merges the other's in-flight PR, force-pushes the
other's branch, or converts a research draft into an implementation PR by
pushing onto it.

Follow `docs/agents/issue-tracker.md`: one visible relationship in the PR body
(`Closes #N`, `Refs #N`, or `None`). Runtime and tracking issues still close
from their own evidence.

When Cursor finishes, it returns the same packet fields to Monk with the
implementation PR URL, head SHA, and check results. Monk may send that packet to
Codex for diagnosis. Codex then remains read-only on that PR unless it becomes
the single writer.

## Mac vs cloud

- Codex App on the operator Mac is for visible research sessions.
- Cursor Cloud Agent is for repository writes and PRs on GitHub.
- Neither side requires a remote IDE.
- `~/Documents/monday` on the Mac may lag `origin/main`. Operators sync that
  checkout themselves. Agents treat GitHub `origin/main` and the named PR head
  as identity, not the Mac tree.

## Conflicts

One active contract has one writer. One worktree and one PR have one writer.

A second agent on the same files is read-only unless Monk reassigns exclusive
ownership. Record the ownership tuple in the worktree-private
`git rev-parse --git-path agent-worktree.yml` path used by managed preflight.
The operator CLI `.github/scripts/agent-worktree-preflight.sh`
(`list` / `apply` / `release` / `spawn` / `sweep`) is the machine interface
for that tuple. `apply --packet-file` occupies a mutually exclusive lease,
`list` reports cleanup safety, and `release` removes only cleanup-safe
worktrees. A squash-merged PR (`(#N)` on the integration tip, or `gh` merged
head) counts as recovered unique commits. Unleased leftover worktrees can be
released by path when cleanup-safe. `spawn` is dry-run unless `--execute`;
dry-run does not start a process or change the execution count. It requires an
active unexpired lease, verifies the admitted packet and current worktree/
branch/base, and refuses `grok`/`human` seats or trading gates other than `none`
or `fail-closed`. It does not call Cursor Cloud or Codex HTTP APIs, and it does
not schedule ACK Jobs.

`task-declare`, `task-invoke`, `task-suspend`, `task-status`, and `task-egress`
are the current local task prototype, not a deployed AX controller. Invoke
materializes workspace files and executes a command. Its checkpoint restores
files before re-executing the command; it does not restore process memory or an
LLM session. At the AX research baseline, `task-suspend` copies files and changes
phase without stopping the worker, and task admission does not atomically
exclude other tasks. Do not use that phase as proof of quiescence or safe
ownership release. Egress wrappers and RSS checks are not a portable sandbox.
Model secrets belong outside task text, commands, logs and checkpoints. Checking
for a leak after execution is not proof of secret containment.

Use the [AX workflow](ax-workflow.md) and
[adoption ADR](../architecture/ADR-0003-ax-agent-execution.md) for the replacement
contract and evidence required to retire this prototype. Keep engineering task
execution separate from Campaign admission and trading authority.

### Machine packet and execution evidence

`apply --packet-file` accepts the snake-case packet below. Lease metadata is
single-line; additional notes and multiline evidence/constraints stay in the
complete packet forwarded to the worker. Use references to evidence rather
than credentials or bulk inputs.

```yaml
from: human
to: codex
routed_by: Monk
goal: Fix the named behavior and open a PR
evidence_paths: issue URL, source SHA, reproduction receipt path
constraints: No deployment; preserve unrelated changes and trading gates
done_criteria: PR URL, exact head and relevant check results
trading_gates: fail-closed
branch: codex/named-fix
writer: codex
allowed_files:
  - path/to/owning/module
deadline: 2099-01-01T00:00:00Z
pr: none
```

Replace the example deadline and scope with the task's actual bounds. `apply`
reads, parses and hashes one private packet snapshot. Later edits to the
original file cannot change an admitted task. Spawn rejects a missing or
altered snapshot, or metadata that differs from it; historical leases without
a snapshot cannot be silently reconstructed into executable leases.

The local Cursor invocation remains `cursor-agent --print --mode plan`;
the Codex invocation remains `codex exec`. Both receive the complete verified
packet, including evidence, constraints and done criteria. The lease coordinates
ownership; `allowed_files` is not an operating-system write sandbox.

An executed spawn writes private, per-execution start and terminal receipts
under the repository's Git common directory, alongside stdout/stderr logs.
Receipts bind the lease, packet hash, source heads, branch, controller/worker
PIDs, timestamps and actual process exit code. The terminal receipt binds the
start receipt and log hashes. Even exit code zero reports `task_status:
unverified`; PR identity, checks and the requested outcome need their own
readback. Dry-run prints metadata references, not the packet text.

While execution is unresolved, a marker blocks another spawn, overlapping
leases and release by either lease ID or worktree path. `sweep` marks overdue
leases expired but retains that occupation and reports the worktree as `keep`.
A confirmed process exit writes the terminal receipt before clearing the
marker. Interruption of the controller retains an unresolved record: a missing
controller PID, expired deadline or clean checkout does not prove the worker
stopped. Inspect the recorded worker and retained evidence before manually
recovering an interrupted execution; this helper does not kill processes or
automatically resume them.

If two open PRs or worktrees overlap, stop the later writer and ask Monk to
choose. Use `.agents/skills/monday-worktree-audit` to inventory conflicts; do
not delete the other writer's branch or worktree.

## Not in scope

- No chat bus, MCP bridge, or shared session between Cursor, Codex, and Hermes.
- No automatic promotion from a Codex research PR into Live, Paper, or Shadow.
- No extra delivery authority beyond `AGENTS.md`.
