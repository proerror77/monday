# Development truth-source recovery audit

Date: 2026-08-26

## Why this exists

The development loop has been treating patch throughput as progress while the
truth source kept drifting:

- the root checkout is ahead and behind `origin/main`, so it is not a safe
  writer for new work;
- the active worktree set contains branch-backed and detached checkouts mixed
  together, which makes "what is still live" ambiguous;
- production collector recovery and development process fixes have been mixed
  into the same conversational loop.

This report defines the bounded recovery order for the development side. It
does not authorize branch deletion, worktree removal, or production mutation.

## Current audit snapshot

Observed on 2026-08-26:

- Root checkout: `main` at `a5af7dd06a5913d25d9065ea5fc140a9b7422811`
- Root relation to `origin/main`: ahead 11, behind 75
- Root dirty state: untracked `DEV_STATE.md`, untracked `rust_hft/.cargo_home/git/`
- Clean process worktree on `origin/main`: `/private/tmp/monday-process-truth-source`
- Open PRs: only `#992 docs: record external market data research sources`
- Production public-data collectors: Spot and USD-M runtime units active; Spot
  recovery unit is `masked-runtime`; USD-M recovery unit is installed but idle

These facts imply one immediate rule: do not keep developing from the root
checkout until its 11 local-only commits are reconciled into explicit,
reviewable slices.

The inventory label `registered-clean` means only that Git still registers the
worktree and its checkout is clean. It does not prove an active owner or session.

## Recovery order

1. Freeze the root checkout as read-only evidence.
2. Use one clean worktree on exact `origin/main` as the only writer for process
   fixes.
3. Split the root-only work into separate reviewable slices instead of one bulk
   merge:
   - process and documentation,
   - control-plane/runtime evidence changes,
   - Monday V2 architecture and module-boundary changes.
4. Keep detached worktrees for readback only. Any new write must happen on a
   named branch.
5. Do not close or delete branches/worktrees until each candidate has exact PR
   or supersession evidence.

## Root local-only commit buckets

The root-only history currently mixes three different scopes:

- process and workflow guidance:
  - `4a62efe4 fix(tests): align test execution guidance with minimal validation`
  - `8eb09031 fix(tests): stop db and collector scripts from forcing clean rebuilds`
- deployment/runtime evidence:
  - `37ccb648 fix(deploy): add governed ECS runtime readback`
  - `40d5038c fix(collector): stop reconnects from restarting capture sessions`
  - `a5af7dd0 Remove legacy drain and hardlink ctime race`
- Monday V2 architecture migration:
  - `7fa5e7c7 docs: add Monday V2 architecture boundary and migration notes`
  - `107e2827`, `bd9087bf`, `794bccdf` governance-contract and mission-routing changes

They should not be landed together.

## Immediate execution plan

1. Land the process-only slice first:
   - worktree inventory semantics,
   - truth-source recovery runbook,
   - minimal validation guidance if still missing from the active branch.
2. Re-read the remaining root-only deployment/runtime changes against current
   `origin/main` and split them into one release/control-plane slice.
3. Only after the process and runtime slices are stable, start the larger
   Monday V2 module-boundary migration.

## What this report intentionally does not do

- It does not declare any root-only commit safe to drop.
- It does not claim production readback is complete.
- It does not replace the Monday V2 ADR; it only defines the development-loop
  recovery order needed before the larger migration continues.
