# CI Language Policy

Ploy is a Rust-first trading platform workspace. Required pull-request
validation should prove the Rust control plane, runner, strategy, research,
contract, frontend, and deployment surfaces still build and test.

## Required CI

The required PR workflow is `.github/workflows/test.yml`.

It should stay focused on:

- Rust workspace build, check, test, audit, and integration lanes.
- Workflow lint.
- Frontend and sidecar contract generation/build checks.

Do not add Python tests or Python package setup back to this workflow. If a
new operational helper is important enough to gate required PR CI, implement it
as Rust code or migrate it into an existing Rust crate first.

## Legacy Python Surface

As of 2026-05-14, the repo still contains a legacy Python compatibility
surface:

- 46 Python helper scripts under `scripts/`.
- 23 Python unit test files under `tests/`.
- Several research/operator workflows that call `python3` for JSON glue,
  artifact analysis, or compatibility scripts.

These files are not the product runtime contract. They are compatibility and
operator helpers that should either be migrated into Rust or retired when their
Rust replacement exists.

## Legacy Python Checks

The isolated workflow is `.github/workflows/legacy-python-tools.yml`.

It runs only when Python helper surfaces change, or when manually dispatched.
This keeps compatibility coverage for existing helper scripts without making
Python a required language for the main project CI.

## Migration Rule

When touching a Python helper, choose one of these outcomes:

- Keep it as legacy compatibility and update the isolated legacy workflow if
  coverage is still useful.
- Replace it with Rust under `apps/` or `crates/` and remove the Python entry.
- Retire it if no workflow, runbook, deployment, or operator path still calls
  it.

Do not introduce new Python production or required-CI surfaces without an
explicit architectural decision.
