# Research Core Validation

Do not run the full Rust workspace for every research-core change.

Use targeted checks:

```bash
cargo test -p alpha-domain --locked
cargo test -p alpha-store --locked
cargo test -p alpha-engine --locked
cargo test -p alpha-harness --locked
cargo test -p hft-research-manifest --locked
cargo test -p hft-factor-dsl --locked
cargo test -p hft-search-protocol --locked
cargo test -p hft-factor-bank --locked
cargo test -p hft-live --no-default-features --test deployment_envelope --locked
cargo check -p hft-collector --locked
```

After changing workspace membership, shared feature flags, or runtime integration, validate the graph without compiling every package:

```bash
cargo metadata --locked --no-deps
```

Use broader package checks only for the ownership boundaries changed by the patch. Full workspace and all-feature checks are release-lane work.
