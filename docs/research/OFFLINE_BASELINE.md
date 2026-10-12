# Offline baseline technical diagnostic

The Rust `monday-offline-baseline` driver fits fixed Ridge folds on prepared native rows and evaluates their full rolling out-of-sample ledger. This diagnostic cannot complete a production Campaign or grant compute authority.

## Reproduce a local synthetic run

From the repository root:

```sh
MONDAY_SOURCE_REVISION="$(git rev-parse HEAD)" \
  cargo build --manifest-path rust_hft/research-core/Cargo.toml --locked \
  -p alpha-harness --no-default-features --features offline-diagnostic \
  --bin monday-offline-baseline
BIN=rust_hft/research-core/target/debug/monday-offline-baseline
"$BIN" synthetic-fixture --output synthetic-fixture
SHA=$(shasum -a 256 synthetic-fixture/synthetic-rows.json | cut -d ' ' -f 1)
EXE_SHA=$(shasum -a 256 "$BIN" | cut -d ' ' -f 1)
"$BIN" run --input synthetic-fixture/synthetic-rows.json --input-sha256 "$SHA" \
  --spec synthetic-fixture/synthetic-spec.json --input-kind synthetic \
  --executable-sha256 "$EXE_SHA" \
  --output synthetic-result-1
"$BIN" run --input synthetic-fixture/synthetic-rows.json --input-sha256 "$SHA" \
  --spec synthetic-fixture/synthetic-spec.json --input-kind synthetic \
  --executable-sha256 "$EXE_SHA" \
  --output synthetic-result-2
cmp synthetic-result-1/report.json synthetic-result-2/report.json
```

Respect `CARGO_TARGET_DIR` when locating the executable. The fixture contains 300 rows at 1 Hz dated 2020-01-01, a generated imbalance feature and five-second mid-price labels. The fixed protocol has initial train 120 rows, validation 30 rows per fold, three expanding folds, purge/embargo 5 rows each and a closed 20-row tail. Costs are 2 bps fees, 0.5 bps latency, 1 bps additional slippage and the generated 1 bps spread. Zero fixture funding proves no historical funding condition.

## Input, fitting and accounting

Input is a current `NativeResearchRowV1`/`ResearchRow` JSON array, at most 32 MiB, with an independently supplied SHA256. A spec, at most 256 KiB, contains existing `EvaluationProtocolV1`, one to eight ordered feature names, positive Ridge regularization and `FormulaEvaluatorConfig`. No implicit search or clock-age cutoff is performed.

Training means, scales, intercepts and coefficients use training rows only. Every fold saves a model and separate training/OOS MSE and MAE. The existing dataset kernel checks chronological availability, label maturity, purge/embargo and closed holdout. The driver requires one continuous series with native canonical series_id=1 and rejects gaps. It never scores the closed tail.

One complete all-fold OOS evaluation uses the existing horizon policy and native CEX baseline evaluator version. The shared kernel applies transaction costs, observed spread, turnover and funding, and closes each position inside its predeclared range. Training metrics describe regression errors; a synthetic result cannot establish strategy validity.

Upstream feature/materialization evidence must establish causal provenance. Names and timestamps alone cannot authenticate that a feature excluded future information. Actual historical byte hashes, native replay, scoped coverage, receive-time availability and funding applicability must be independently verified in ACK before preparing historical rows.

`report.json` contains models, partitions, regression metrics and the full OOS cost/equity ledger. `manifest.json` records verified input/spec/report hashes and the externally declared executable hash, embedded `MONDAY_SOURCE_REVISION`, input kind and external authority reference. Without that build variable, the source is explicitly `unbound-local-code`; the caller's declared executable digest is explicitly unauthenticated by this driver. The external Build admission must bind that digest to the exact executable before real launch; the driver does not trust current_exe() or its path for identity. New result directories and create-new files reject overwrite. Diagnostic status is explicit and Campaign, deployment and statistical strategy acceptance remain false.

## Placement and authority

Local runs use declared synthetic inputs. Historical inputs, fitted models and full results remain in ACK/OSS. Historical mode requires declared ACK placement and an external admitted lease reference. **These declarations do not authenticate a grant.** Owning admission must verify an effective new lease before launch. This entry cannot reset a ledger or enforce cloud monetary spend. Production completion retains `campaign-freeze -> campaign-finalize -> dispatch submit -> campaign-execute`.

The engine module is enabled by scientific fitting or the explicit diagnostic feature. The operator default does not enable it. The scientific harness includes the diagnostic feature so its existing test matrix covers the CLI contract. The standalone local feature avoids Burn fitting dependencies.

## Focused validation

```sh
cargo test --manifest-path rust_hft/research-core/Cargo.toml --locked \
  -p alpha-engine --no-default-features --features offline-diagnostic \
  --lib diagnostic_baseline
cargo test --manifest-path rust_hft/research-core/Cargo.toml --locked \
  -p alpha-harness --no-default-features --features offline-diagnostic \
  --test offline_baseline
```

Engine tests cover old timestamps, train-only fitting under validation-label mutation, gaps/immature labels, closed-tail independence and effective transaction/slippage costs. CLI validation covers deterministic reports and manifest hashes, flat fold endings, tamper rejection, overwrite refusal and rejection of local historical inputs.

## Real-data acceptance remains open

July objects now return 404 and are excluded. The technical candidate is BTCUSDT in two adjacent USD-M LOB/trade segments, 2026-10-10 05:00–05:10 UTC. Metadata declares checkpoint/trade coverage/stream continuity. Actual byte hashes and native replay remain unverified. Ten minutes can exercise the chain; it cannot establish statistical validity.

The release catalog and CEX Dockerfile now prepare a separate offline-diagnostic recipe and binary. The shared alpha-harness/Controller recipe remains unchanged; the existing native release recipe uses Cargo default features plus offline-diagnostic, unlike the local no-default build. PR release transport/ABI/smoke evidence does not issue a signed Build: current-main CI, ACR/OSS publication and independent ACK import still apply. The public repository policy currently lacks oss_by_product; that configuration remains with its existing owner.

The designated existing ECS worker is stopped in StopCharging mode; stale ACK inventory is not readiness proof. A new scoped effective lease, verified Build, readiness and any separately approved bounded worker start/stop remain prerequisites. RMB10 is a requested ceiling, not a proven billing enforcer. The old H1 184/184 ledger and sealed views remain untouched. Source preservation has separate approval.

## Executed local evidence

On 2026-10-12, the fixed synthetic fixture ran twice through three fitted models and 90 OOS observations. Report bytes matched exactly, SHA256 `246118352e34c1854e354ac5623efc77e87c991f25fe3e759cd45365bc0dfede`. Under the declared costs it opened no positions: zero turnover and net return, with strategy gates rejected for insufficient trades/positive edge. This is synthetic technical execution, not a real market outcome or evidence of profitability. Four focused engine tests passed. Local CLI/no-default builds also expose pre-existing unused/dead-code warnings in unrelated operator modules; they were not suppressed or mass-edited.
