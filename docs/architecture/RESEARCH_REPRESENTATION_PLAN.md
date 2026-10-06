# Read-only research representation planning

The planner proposes a comparison from data capabilities and a frozen research goal. It does not execute research.

`alpha-domain::representation` owns declarations, clocks, view bindings, resource requirements and proposal identities.
`alpha-engine::representation_plan` matches those declarations to six existing tools. It imports no acquisition, fitting or LLM implementation.
`alpha-harness::representation_plan` accepts the existing opaque `VerifiedBinanceMarketTapeSeries` handle. It derives a bounded summary from verified source identities and replay events.

The engine accepts declarations for inventory planning. Every feasible output requires raw verification and native admission.
The app returns an opaque proposal handle after raw verification. This handle has no JSON constructor.
Its JSON output is a report. Restoring the report does not restore verification or permission.
Current view permissions, signed allowlists, source/image identity and remaining budgets require the existing Campaign admission.

## Existing tool limits

| Tool | Required input | Boundary |
| --- | --- | --- |
| Captured book replay | Snapshot seed and sequence-checked diffs | Captured L2 depth only; quantities replace levels |
| Static Top5 | Observed Top5 seed and suitable time coverage | Snapshot-only supports observed state, not complete event history |
| Lagged continuous OFI | Continuous book history and 60-second warmup | No interpolation across gaps or recovery seeds |
| Aggregate trade flow | Verified aggregate-trade aggressor direction | No inferred direction from undirected trades |
| SOL sequence | Binance USD-M SOLUSDT, Top5 and directed trades | Existing 24 channels, 60 × 1-second context, fixed 30-second Study primary target |
| SOL market encoder | Same SOL input requirements | Same fixed 30-second Study primary target; no generic market or horizon claim |

The small registry binds the existing implementation source hashes. It does not create a plugin framework.
The market encoder identity covers its Study, the PIT materializer and the encoder feature/target module.
An older identity that binds only the Study cannot validate against this registry.
The raw adapter preserves each recovery seed as a separate series. It never joins history across that boundary.
The original verifier retains dataset and shard scope in the opaque handle. The planner requires one market/dataset/shard scope, unique capture sessions and ordered, nonoverlapping receive intervals across supplied series.
Trade direction requires the requested symbol's verified trade modality and causal trade evidence in every supplied series. A trade in one session cannot qualify a LOB-only session.
Shared coverage ends at the last replayed snapshot or diff. Legacy H1/H2 and sequence materializers ignore checkpoints, so checkpoints cannot extend their quiet tail.
Market encoder checkpoint flushing is not advertised through this conservative shared coverage summary.
Observed seed depth does not prove every replay row has enough levels. Materialization must verify that condition independently.

## Automatic comparison

Suitable continuous inputs produce two registered field families without feature hints.
H1 uses the renderer's nine snapshot fields. H2 replaces aggregate trade imbalance with `cont_ofi_lag60s`.
The comparison preserves the goal's target, horizon, model, scaling, cost and partition identities.
It reports the different information histories and proposes two trials within declared finite resource limits.
The current H1/H2 renderer supports USD-M BTCUSDT/SOLUSDT/BNBUSDT and Spot BTCUSDT only.
It uses one-second observations and registered 5/10/30-second horizons for the exact `forward_mid_return` target. Other target names emit no comparison arms or hypothesis.
Other instruments or horizons remain implementation gaps.
SOL sequence and encoder Studies fix their primary target at 30 seconds. Their 5/10-second sequence labels are diagnostics, not Study goals.
The proposal does not require GP. Later native admission must select a currently supported model/input path.

A plan cannot widen an existing signed feature or policy revision allowlist.
The current falsification text is a proposal. Native admission must bind executable thresholds before running it.
Plan hashes bind data sources, view and permission references, tools, columns, clocks, target and resource requirements.
The engine validates an imported plan by recomputing the full deterministic result.

## Rejection and evidence

Snapshot-only, gaps and unseeded diffs cannot support continuous history.
Missing direction in any supplied series blocks directed trade and SOL tools. Future availability blocks planning.
Opaque results from separate verifier calls cannot be concatenated to bypass dataset, shard, session or receive-order boundaries.
Each field decision clock must lie within the frozen goal window. Its availability cannot exceed that window end.
A lookback must fit within one continuous series. A resource requirement exceeding the declared limit emits no comparison arms.
Independent validation, strategy sealed, meta certification and exposed terminal views cannot feed this family search.
A caller's view label is a declaration. Raw verification does not establish exposure-ledger or permission truth.
The next execution slice must verify those bindings through the current governed Campaign contract.

This slice provides a library API. CLI registration and Campaign execution belong to their own coordinated slices.
Unit cases verify software behavior. They do not prove real data coverage, training, profitability, RSI improvement or deployment.
