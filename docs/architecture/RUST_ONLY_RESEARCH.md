# Rust-only research and model boundary

## Outcome

Monday has two research architectures and one language boundary. Research,
market-data diagnostics, feature computation, training, evaluation, artifact
serialization, and runtime integration are Rust. TypeScript remains only in
operator frontends. Python source, PyTorch/libtorch bindings, and silent model
fallbacks are not accepted.

The 2026-07-15 cutover removed 145 tracked Python files (49,330 lines), the
`ml_workspace` and `ml_trainer` trees, their CI and container entrypoints, the
historical nested PLOY workflows, and the mock-capable `tch` strategy path.

## Separate research architectures

| Contract lane | Label and split | Native model | Governing evidence |
| --- | --- | --- | --- |
| Continuous contracts | Forward return on point-in-time rows; purged and embargoed walk-forward | `hft-research-ml` Burn regressor plus the Formula/search engines in `alpha-harness` | IC, RankIC, ICIR, predictive loss, post-cost return, turnover, drawdown, sealed holdout |
| Prediction markets | Official binary settlement; event-disjoint train/validation/test | `ploy-research` Burn probability model plus typed probability-blend LoopRun | Brier score, log loss, calibration, settlement PnL, full-depth fillability, event-level capacity |

These lanes share provenance, budgets, immutable evidence, and Monday's
execution boundary. They do not share labels, splitters, thresholds, model
manifests, promotion records, or Cargo workspaces.

## Native ML stack

- Burn `0.20.1` supplies CPU tensor operations, autodiff, optimizers, and model
  modules. Models are serialized as Burnpack with typed JSON manifests.
- `burn-ndarray` is the deterministic CPU backend used for research jobs.
- Tract remains a read-only ONNX compatibility loader for already governed
  artifacts. It is not a training framework or a fallback when native training
  fails.
- No model trainer may import an execution adapter, edit runtime strategy
  configuration, or mark its own artifact promotion-ready.

Every native bundle binds its feature order, point-in-time dataset identity,
split, seed, training configuration, metrics, model checksum, framework version,
and research-only scope. Loaders verify the manifest and checksum before reading
weights. Existing bundles are never overwritten.

Prediction training additionally accepts only an immutable
`VerifiedBinarySnapshot` handle. The loader verifies the snapshot's evaluator
artifacts against a caller-supplied trusted `snapshot_contract_hash`; no public
constructor or mutable accessor can bypass that check. Snapshot v2 atomically
binds the exact UP/DOWN token pair, its complementary official outcome, and the
locally recorded availability time of both persisted token versions. The loader
hashes and parses the same captured artifact bytes, including the Parquet label
clock at microsecond precision. Both Burn training and the shared walk-forward
path used by the actual LoopRun cut off training labels by that availability
clock rather than by scheduled expiry alone. Snapshot v1 artifacts must be
rebuilt before use in this lane.

## Factor proposal loop

The LLM does not invent executable strategy code. For continuous contracts it
proposes a bounded hypothesis over the registered factor grammar; the Rust DSL,
point-in-time validator, and walk-forward evaluator decide whether the hypothesis
is falsified. For prediction markets it proposes only typed probability-blend
candidates using registered Chainlink, Binance, and Polymarket surfaces; the
event-disjoint evaluator returns deterministic candidate-specific feedback.

Rejected hypotheses, data gaps, metric failures, and budget exhaustion remain
ledger evidence. They may influence a later bounded research iteration but can
never mutate risk, OMS, credentials, deployment, or live execution.

## Operational replacements

| Removed surface | Rust authority |
| --- | --- |
| Generic model trainer and experimental ML workspace | `rust_hft/research-core/ml` plus `rust_hft/alpha-harness` |
| PLOY binary TCN training helper | `products/ploy/crates/ploy-research` native Burn probability model |
| PLOY market-data gap script | `ploy-market-data --example market_data_gap_audit` typed audit report |
| Binance LOB archiver service | `hft-collector --bin binance-lob-archiver` |
| Bitget latency summary script | `hft-data-adapter-bitget --bin latency-report` |
| `tch`/TorchScript strategy with mock fallback | removed; native Burn research bundles and fail-closed Tract compatibility remain separate |

Repository CI rejects tracked `*.py`, `tch`, and `torch-sys`. A missing Rust
replacement is an unavailable capability, not permission to restore a second
language path.
