# PLOY Integration Boundary

## Outcome

PLOY is maintained inside Monday as the independent Rust and TypeScript product workspace at `products/ploy`. The import preserves PLOY's product, prediction-market, research, frontend, sidecar, control-plane, and compatibility code without merging its Cargo graph into `rust_hft`.

Monday remains the repository and production authority. In particular:

- `rust_hft` owns real market connectivity, risk, OMS, reconciliation, cancellation, and execution.
- PLOY may produce research evidence, product/operator interactions, typed intents, and sidecar recommendations.
- PLOY must not bypass Monday by submitting orders from its sidecar or legacy standalone deployment paths.
- Live trading remains disabled. This migration does not deploy, resume, approve, or mutate any trading host.

## Source and provenance

- Source repository: `https://github.com/proerror77/ploy`
- Source branch: `main`
- Source SHA: `8ce4e0f150173a44030294101f4b1371cbdf80bc`
- Source commit date: `2026-07-13T21:34:10+08:00`
- Source commit: `fix: harden Polymarket tick-level live hot path (#755)`
- Import mode: tracked source snapshot, not full Git history
- Declared license: MIT in the upstream Cargo metadata and README
- License caveat: the upstream root had no project-level `LICENSE` file at the source SHA

The source checkout was shallow, so the snapshot was produced with `git archive` at the exact SHA. Ignored local data, targets, node modules, virtual environments, runtime logs, and `.env` files were not copied.

## Deliberate exclusions and replacements

- `.env.production` was renamed to `.env.production.example`; all credential fields were empty at import time.
- Standalone agent/session state (`.claude`, `.superpowers`, `.full-review`, and the old `CLAUDE.md`) was excluded.
- The 1.48 MB standalone `tasks/todo.md` session log was replaced with a concise Monday migration tracker.
- The unused vendored Polymarket SDK was excluded; the workspace resolves the maintained crates.io dependency and the vendor directory was not a workspace member.
- The product-local `AGENTS.md` was rewritten for the Monday authority boundary.

The original `.github`, `deployment`, and `infra` trees remain under `products/ploy` for source compatibility and historical evidence. GitHub does not execute nested workflows. They are not Monday deployment entrypoints.

## Historical local-only documents

Seven documents from local PLOY commit `5de411bbe8889284b47fe9932821af077d2962fc` are preserved under `products/ploy/docs/archive/local-readiness-2026-07-11`. They are explicitly stale: they target the standalone PLOY/Tango topology and must not be treated as current Monday plans.

## CI and maintenance

- Monday's existing Rust workspace stays rooted at `rust_hft` and keeps its own toolchain and CI.
- PLOY stays rooted at `products/ploy`, uses Rust `1.91` and Node `22`, and has a dedicated root workflow at `.github/workflows/ploy-ci.yml`.
- PLOY-only changes do not run Monday's Rust or Docker build matrices; repository-wide security checks still scan the full diff.
- The active PLOY runtime entrypoints are `new-ployd`, `new-ploy-runner`, `ployctl`, and `ploytui`. The root `ploy` crate is a compatibility shim.

## Operations and archive boundary

The former PLOY repository had 20 repository secrets, 4 variables, 8 environments, protected live approvals, scheduled workflows, 28 open issues, and standalone deployment paths. Secret values cannot be read back and were intentionally not copied because the legacy workflows are not activated in Monday.

Before any future PLOY-derived deployment is enabled, create a separate reviewed change that rebuilds the required secret sources, environments, branch protection, immutable artifacts, runner identity, and host-path mapping inside Monday. That work must keep Monday's execution authority and fail-closed live gates intact.

The archived issue index remains available at `https://github.com/proerror77/ploy/issues?q=is%3Aissue%20is%3Aopen`; issues `#751` and `#361` are the most recent infrastructure follow-ups and are historical blockers, not active deployment approval.

## Acceptance criteria

- The exact PLOY source snapshot and local-only design documents are preserved with provenance.
- PLOY builds and tests from `products/ploy` without joining the `rust_hft` Cargo workspace.
- Frontend and sidecar contract checks run from the Monday repository.
- Repository secret scanning passes without expanding the allowlist.
- No legacy PLOY workflow is activated at the Monday workflow root.
- The former PLOY repository is redirected to Monday and archived only after the Monday migration PR is merged and green.
