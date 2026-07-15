# Rust HFT test boundary

Monday does not use a repository-local all-in-one test wrapper. The former
`run_test_suite.sh` family mixed stale filters, optional infrastructure, and
deployment claims, so it was retired during the Rust-only consolidation.

## Source of truth

- Root CI: `.github/workflows/ci.yml`
- Package manifests and feature flags: `rust_hft/Cargo.toml` plus each crate's
  `Cargo.toml`
- Test targets: crate-local `src` test modules and `rust_hft/tests/`

Run commands from the Monday repository root with an explicit manifest and
locked dependencies.

## Focused validation

Start with the package and target affected by the change:

```bash
cargo test --manifest-path rust_hft/Cargo.toml -p hft-engine --locked
cargo test --manifest-path rust_hft/Cargo.toml -p hft-live \
  --no-default-features --test deployment_artifacts --locked
cargo clippy --manifest-path rust_hft/Cargo.toml -p hft-engine \
  --all-targets --locked -- -D warnings
cargo fmt --manifest-path rust_hft/Cargo.toml --package hft-engine -- --check
```

The native continuous-contract ML lane is separate:

```bash
cargo test --manifest-path rust_hft/Cargo.toml -p hft-research-ml --locked
```

## Feature and integration matrices

Use the exact feature matrix encoded in root CI. Important examples include:

```bash
cargo test --manifest-path rust_hft/Cargo.toml \
  -p hft-data-adapter-binance --no-default-features --locked
cargo test --manifest-path rust_hft/Cargo.toml \
  -p hft-data-adapter-binance --no-default-features --features json-simd --locked
cargo check --manifest-path rust_hft/Cargo.toml \
  -p hft-live --features clickhouse,redis,grpc --locked
```

Database-, cloud-, venue-, and live-runtime checks require their documented
external prerequisites. A missing service or credential is an environment
boundary, not permission to substitute a mock or report an empty dataset.

## Reporting

For every validation run, record:

- the exact command and working directory;
- pass, fail, and ignored counts;
- the first causal error and any warnings;
- which external or remote state was not verified.

A local Cargo pass does not prove that a collector is deployed, a cloud service
is healthy, or live trading is enabled.
