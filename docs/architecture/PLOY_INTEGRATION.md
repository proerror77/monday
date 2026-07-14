# PLOY Integration Boundary

## Outcome

PLOY is maintained inside Monday as an independent Rust product workspace with a TypeScript frontend at `products/ploy`. The import preserves PLOY's product, prediction-market, research, frontend, sidecar, control-plane, and compatibility code without merging its Cargo graph into `rust_hft`.

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
- Import mode: adapted tracked source snapshot, not full Git history
- Declared license: MIT in the upstream Cargo metadata and README
- License caveat: the upstream root had no project-level `LICENSE` file at the source SHA

The source checkout was shallow, so the provenance baseline was produced with
`git archive` at the source SHA. The Monday tree is an adapted snapshot: security
hardening, repository metadata, execution-authority enforcement, conflict cleanup,
and formatting normalization changed selected tracked blobs. Ignored local data,
targets, node modules, virtual environments, and runtime logs were not copied.
The complete path and SHA-256 record is in
`products/ploy/MIGRATION_ADAPTATIONS.md`.

## Deliberate exclusions and replacements

- `.env.production` was renamed to `.env.production.example`; all credential fields were empty at import time.
- Standalone agent/session state (`.claude`, `.superpowers`, `.full-review`, and the old `CLAUDE.md`) was excluded.
- The 1.48 MB standalone `tasks/todo.md` session log was replaced with a concise Monday migration tracker.
- The unused vendored Polymarket SDK was excluded; the workspace resolves the maintained crates.io dependency and the vendor directory was not a workspace member.
- The product-local `AGENTS.md` was rewritten for the Monday authority boundary.
- The standalone README is preserved under
  `products/ploy/docs/archive/standalone-source-2026-07-13`; the active README
  documents Monday-only development and execution boundaries.
- The write-capable standalone `ploy-openclaw` package was relocated byte-for-byte
  into that archive. The only active OpenClaw example is read-only and rejects
  unlisted RPC methods and remote-control mutations before SSH.

The original `.github`, `deployment`, and `infra` trees remain under `products/ploy` for source compatibility and historical evidence. GitHub does not execute nested workflows. They are not Monday deployment entrypoints.

## Language boundary

- Durable Monday market data, account, order, reconciliation, sidecar, report,
  backtest, and monitoring paths are Rust. TypeScript is limited to the operator
  frontend.
- Python remains only where behavior has not yet earned Rust parity: the legacy ML
  workspace, the Python LOB archiver during Rust shadow comparison, and imported
  PLOY research/compatibility utilities. Root CI may compile or fixture-test those
  files, but it does not make the nested PLOY deployment workflows active.
- Shell remains for host bootstrap, CI command composition, and package installation;
  no shell script owns trading decisions, risk, OMS, or exchange mutations.
- The Rust sidecar is built and tested but has no approved deployment package. Its
  missing evidence adapters fail closed and require a separate parity and deployment
  review rather than falling back to Python or user-global tools.

## Historical local-only documents

Seven documents from local PLOY commit `5de411bbe8889284b47fe9932821af077d2962fc` are preserved under `products/ploy/docs/archive/local-readiness-2026-07-11`. They are explicitly stale: they target the standalone PLOY/Tango topology and must not be treated as current Monday plans.

## CI and maintenance

- Monday's existing Rust workspace stays rooted at `rust_hft` and keeps its own toolchain and CI.
- PLOY stays rooted at `products/ploy`, uses Rust `1.91` and Node `22`, and has a dedicated root workflow at `.github/workflows/ploy-ci.yml`.
- PLOY-only changes do not run Monday's Rust or Docker build matrices; repository-wide security checks still scan the full diff.
- Semgrep excludes the nested historical PLOY workflow and infrastructure
  directories because those files are preserved evidence rather than executable
  Monday delivery paths. Root workflows and active source remain scanned, and
  tracked-secret detection still covers the complete repository tree.
- The active PLOY runtime entrypoints are `new-ployd`, `ploy-agent-sidecar`, `new-ploy-runner`, `ployctl`, and `ploytui`. The root `ploy` crate is a compatibility shim.
- `PloyDaemon::boot` installs `DisabledLiveExecutionGateway`; production code
  cannot inject the private Polymarket gateway, and the standard runner `full`
  feature does not enable its legacy control-plane live executor.
- The standalone Node account-operation packages are retired. Polymarket account,
  order, cancellation, and reconciliation operations remain in `rust_hft`; PLOY
  does not retain a second execution path.

## Operations and archive boundary

The former PLOY repository had 20 repository secrets, 4 variables, 8 environments, protected live approvals, scheduled workflows, 28 open issues, and standalone deployment paths. Secret values cannot be read back and were intentionally not copied because the legacy workflows are not activated in Monday.

Before any future PLOY-derived deployment is enabled, create a separate reviewed change that rebuilds the required secret sources, environments, branch protection, immutable artifacts, runner identity, and host-path mapping inside Monday. That work must keep Monday's execution authority and fail-closed live gates intact.

The archived issue index remains available at `https://github.com/proerror77/ploy/issues?q=is%3Aissue%20is%3Aopen`; issues `#751` and `#361` are the most recent infrastructure follow-ups and are historical blockers, not active deployment approval.

## Acceptance criteria

- The adapted PLOY source snapshot and local-only design documents are preserved with explicit provenance.
- PLOY builds and tests from `products/ploy` without joining the `rust_hft` Cargo workspace.
- Frontend contract checks and Rust sidecar tests run from the Monday repository.
- Repository secret scanning passes without expanding the allowlist.
- No legacy PLOY workflow is activated at the Monday workflow root.
- The former PLOY repository is redirected to Monday and archived only after the Monday migration PR is merged and green.
