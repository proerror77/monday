# CI Language Policy

PLOY research, data, control-plane, runner, strategy, contract, and operational
helper code is Rust. TypeScript is limited to the operator frontend.

## Required CI

The active workflow is the Monday-root `.github/workflows/ploy-ci.yml`. It checks:

- Rust build, test, formatting, audit, and integration lanes;
- frontend contract generation, lint, and build;
- absence of tracked `*.py` source;
- absence of `tch` and `torch-sys` dependencies.

## Change rule

New research or operational capability must be implemented in an existing Rust
crate or a narrowly owned Rust crate. A missing migration does not permit a
fallback implementation: the capability remains fail-closed until its Rust
contract, tests, and evidence format exist.

Native model training must bind its point-in-time dataset, feature order, split,
seed, metrics, and artifact hash. Training produces lab evidence only and cannot
self-promote or reach an execution adapter.
