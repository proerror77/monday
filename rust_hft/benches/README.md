Consolidated benches

Declared `[[bench]]` targets (see root `Cargo.toml`):

- `strategy_dispatch.rs`: Strategy dispatch benchmarks
- `ultra_components.rs`: Ultra-path component benchmarks

The enforced P99/P999 quote-to-worker latency benchmark belongs to `hft-engine`:

```sh
cargo test -p hft-engine --bench hotpath_latency_p99 --release --locked -- --nocapture
```

It measures local processing with 20,000 samples after warmup and rejects P99 above
500 microseconds or P999 above 1 millisecond. Network and exchange latency are excluded.
