# Agent Workflow Reference

> **Historical standalone PLOY agent policy.** This file is retained as migration
> evidence and is not the current instruction source. Follow the Monday root
> `CLAUDE.md`, `products/ploy/AGENTS.md`, and `docs/architecture/PLOY_INTEGRATION.md`.

Use this file for detailed agent workflow policy. Keep `AGENTS.md` and
`CLAUDE.md` short and repo-specific, then link here for the long-form process
rules.

## CLI Output Compression

- Prefer `rtk read <file>` instead of raw `cat`, `head`, or `tail`.
- Use `rtk git status`, `rtk git diff`, `rtk git log`, and `rtk git push`.
- Use `rtk cargo ...`, `rtk test npm test`, and other supported
  `rtk` wrappers when they apply.
- If no `rtk` wrapper exists for the command you need, use the plain command.

## Tool Mapping

- Read: use `rtk read`, `sed`, or `rg`.
- Write, Edit, MultiEdit: use `apply_patch`.
- Bash: use `functions.exec_command`.
- Grep: use `rg` with `grep` as fallback.
- Glob: use `rg --files` or `find`.
- LS: use `ls` via shell.
- Web fetch for known URLs: use `curl` or official docs tools.
- Web search: use Codex browsing tools.
- If `curl` hits JS-rendered pages, Cloudflare, or login walls, switch to the
  `agent-browser` skill workflow.
- Parallel shell reads and searches: use `multi_tool_use.parallel`.

## Planning And Task Tracking

- For non-trivial work, write a short plan in `tasks/todo.md` with checkable
  items.
- Update the plan as work advances and add a brief review section for
  substantial tracked work.
- Re-plan when discovery changes the shape of the task.
- Check in with the user before implementation only when the next step is
  destructive, irreversible, production-impacting, or still materially
  ambiguous after reasonable local discovery.

## Execution Default

- Once scope and plan are clear, keep moving: plan -> implement -> verify ->
  update progress.
- Do not pause for step-by-step approval when the next action is already
  implied by the task.
- Keep context small and prefer the simplest change that fully solves the
  problem.

## Subagents And Worktrees

- Use subagents for bounded research, exploration, review, or parallel
  analysis.
- Use separate branches and worktrees when multiple agents or live sessions may
  touch the same files.
- Assign file ownership before parallel work and reserve high-conflict files to
  one integrator.
- Integrate parallel work intentionally, for example with `cherry-pick`, and
  remove stale temporary worktrees afterward.
- Do not let completed session worktrees accumulate. After merge or PR close,
  verify the worktree is clean or archive the branch with a local tag/bundle,
  then remove the worktree and delete the local branch.
- Keep only the main checkout plus currently active session worktrees. Clean
  stale completed worktrees before starting new non-trivial work.
- Do not convert cleanup archives into permanent local folders. Temporary
  bundles, patches, screenshots, and archived worktree directories should be
  removed after the merge/PR result is verified unless they are explicitly
  retained as recovery evidence.
- The final handoff for worktree-based sessions should state the cleanup proof:
  `git worktree list`, `git worktree prune`, deleted local branches, and any
  intentionally retained archive path.

## Git And Commits

- Keep committed changes atomic: one logical change per commit.
- Before editing or staging, run preflight checks: `rtk git status --short`,
  `git branch --show-current`, and `rtk git diff --name-only`.
- Stage explicit paths and review staged changes with `rtk git diff --cached`.
- Use commit messages in the form `<scope>: <intent>`.
- Do not push partial WIP commits to shared branches.

## Skills

- Use a relevant skill when the user names it or the task clearly matches it.
- Open the referenced `SKILL.md` and prefer provided scripts or templates over
  retyping.
- Keep skill loading targeted; do not bulk-read unrelated references.
- If a skill is missing or unreadable, say so briefly and continue with the
  best fallback.

## Verification And Review

- Never mark work complete without proving it.
- Run the smallest relevant build, test, lint, or smoke checks for the files
  you changed.
- If behavior changes, add or update tests when practical.
- Explain any skipped validation explicitly.
- Docs-only changes still require diff review and cross-file consistency checks.
- Instruction-file changes should keep `AGENTS.md` and `CLAUDE.md` aligned.
- For review requests, lead with concrete findings before summary.
- Diff behavior against `main` when relevant.

## Issue Tracking And Lessons

- If a problem cannot be resolved in the current change, create or update an
  issue with context, impact, reproduction, and next action.
- After a substantive user correction that reveals a reusable repo-specific
  failure pattern, update `tasks/lessons.md`.
- Review relevant lessons at session start when they apply.

## Runtime And Deployment Constraints

- Default to dry-run for local validation.
- The workspace-default local platform path is `new-ployd` + `ployctl`.
- Treat remaining `ploy ...` runtime commands as compatibility surfaces; do not
  introduce new deployment guidance that depends on them.
- Do not introduce or rely on direct live order paths unless explicitly
  required.
- On trading hosts, do not build Rust source on-host. Build in CI and deploy
  release artifacts only.
- Historical nested deployment workflows were retired. Do not recreate a PLOY-local
  workflow authority; the Monday-root workflow is canonical.
- Live services should keep these systemd guardrails:
  - `Restart=always`
  - `RestartSec=5`
  - `MemoryHigh=1280M`
  - `MemoryMax=1536M`
  - `OOMPolicy=kill`
- After deploy, verify `systemctl show <service> -p MemoryMax -p Restart -p
  OOMPolicy` and confirm no active `cargo` or `rustc` process remains.
