# Research Core Validation

Do not run the full Rust workspace for every research-core change.

Use targeted checks:

```bash
cargo test -p hft-research-manifest --locked
cargo test -p hft-factor-dsl --locked
cargo test -p hft-search-protocol --locked
cargo test -p hft-factor-bank --locked
cargo test -p hft-promotion-gate --locked
```

Use a broader check only when changing workspace dependencies, shared feature flags, or runtime integration:

```bash
cargo check --workspace --locked
```
