# CEX image build scope

The production CEX image packages eight executables through five release recipes.
The offline diagnostic has a separate local build documented in [OFFLINE_BASELINE.md](OFFLINE_BASELINE.md).
Removing that executable removes its operator-plus-offline recipe from every CEX image build.
It does not remove the scientific worker's offline-diagnostic library feature.

## Entry points and dependency boundaries

| Entry | Build owner and features | Packaging decision |
| --- | --- | --- |
| `hft-backtest` | research-core; default | Retain the existing Docker entry point. |
| `alpha-harness` | research-core; default operator | Retain Campaign freeze, finalize and dispatch commands. Controller shares these exact bytes. |
| `monday-cex-worker` | research-core; scientific, without alpha-harness default features | Retain the admitted Campaign execution entry point. |
| `monday-offline-baseline` | Explicit local alpha-harness offline-diagnostic build, without defaults | Exclude from production images. No production entry point invokes it. |
| `lob-pit-materializer` | data-pipelines; collector | Retain materialization script dependency. |
| `binance-market-tape-slicer` | data-pipelines; collector | Retain materialization script dependency. |
| `binance-replay-parquet-materializer` | data-pipelines; collector | Retain materialization script dependency. |
| `clickhouse-analytics-materializer` | data-pipelines; collector | Retain data preparation tooling. |
| `monday-cex-prepare` | data-pipelines; preparation | Retain preparation and its source-version smoke check. |

The catalog and both Docker build paths must agree. Product tests check the exact
prebuilt copies, producer archive, binary startup and compiler failure handling.
The existing dependency graph still propagates shared source changes conservatively.
No source path is excluded from CI by this change.

## Reuse and identity

Parameter-only experiments reuse an admitted immutable Build and verified prepared inputs.
Data semantics, including label windows, still invalidate prepared-input reuse.
See [CAMPAIGN_PREPARATION_REUSE.md](CAMPAIGN_PREPARATION_REUSE.md).

The release profile, target, compiler, native environment and workspace inputs remain
bound to compilation provenance. Cargo targets stay isolated by owning workspace.
The changed recipe list remains part of exact build inputs and artifact validation.
The dependency restore identity hashes compiler scripts separately from the product
catalog. Packaging-only selection changes can reuse compatible dependency bytes.
Changing compiler scripts still isolates the cache; this migration starts a new key.
Dependency cache reuse does not authenticate executable bytes: builds still produce,
hash and smoke-test the complete selected product at the admitted source revision.
No research-profile executable is presented as a release artifact.

## Measurement and remaining scope

The prior reference is Monorepo 38429350564 at 14m19s and Prediction 38429350500
at 20m51s. Research job 115408859232 took about 15m57s, including 13m52s build.
The reported merge-to-ACR interval was 24m43s. These are historical observations,
not a controlled benchmark for this change.

This change reduces nine executables and six recipes to eight and five.
It does not promise a proportional wall-time reduction: owners run concurrently,
and the scientific worker still compiles its fitting and offline library closure.
Use retained recipe timings, exact cache-hit state and runner identity to compare runs.
Report queue, compilation, packaging and publication separately; overlapping phases
must not be added together. A draft PR provides no merge-to-ACR measurement.

Further separation of preparation, operator and scientific images requires a complete
consumer and signed-product protocol change. This patch preserves that boundary.
The 10–15 minute small-change target remains unproven until comparable CI measurements exist.
