# Read-only research representation planning

The planner proposes a comparison from data capabilities and a frozen research goal. It does not execute research.

`alpha-domain::representation` owns declarations, clocks, view bindings, resource requirements and proposal identities.
`alpha-engine::representation_plan` matches those declarations to six existing tools. It imports no acquisition, fitting or LLM implementation.
`alpha-harness::representation_plan` accepts the existing opaque `VerifiedBinanceMarketTapeSeries` handle. It derives a bounded summary from verified source identities and replay events.
The tape-only API produces inventory and a missing instrument-rule limitation. It emits no materialization candidates.
`plan_from_verified_tape_with_rules` also borrows `VerifiedInstrumentRuleReferences::Spot` or `Usdm`.
These contain the source-bound handles returned by `verify_bound_spot_reference_artifact` and `verify_bound_reference_artifact_read_only_current_batch` in the original collector artifact owners.
The handles have private fields, no JSON constructor and no `Clone` or `Deserialize`. Publicly constructible reference batches cannot replace them.

The engine accepts declarations for inventory planning. Its serialized rule coverage is a declaration, not an artifact credential.
The app returns an opaque proposal handle after raw verification. This handle has no JSON constructor.
Its JSON output is a report. Restoring the report does not restore verification or permission.
Current view permissions, signed allowlists, source/image identity and remaining budgets require the existing Campaign admission.

## Existing tool limits

| Tool | Required input | Boundary |
| --- | --- | --- |
| Captured book replay | Any positive-depth snapshot seed and sequence-checked diffs covering its replay window | Captured L2 depth only; quantities replace levels; no Top5, rule or scientific-label prerequisite |
| Static Top5 | Observed Top5 seed and suitable time coverage | Snapshot-only supports observed state, not complete event history |
| Lagged continuous OFI | Continuous book history and 60-second warmup | No interpolation across gaps or recovery seeds |
| Aggregate trade flow | Verified aggregate-trade aggressor direction | No inferred direction from undirected trades |
| SOL sequence | Binance USD-M SOLUSDT, Top5 and directed trades | Existing 24 channels, 60 × 1-second context, fixed 30-second Study primary target |
| SOL market encoder | Same SOL input requirements | Same fixed 30-second Study primary target; no generic market or horizon claim |

The small registry binds the existing implementation source hashes. It does not create a plugin framework.
All six tool identities bind the shared materializer, feature calculations, replay and reference verification modules, manifest contracts and owning lockfiles.
Their shared source closure also binds the Campaign renderer, actual materialization loader, feature-matrix metadata, and the domain's exact horizon, protocol, calendar, rule and label predicates.
Reference admission also binds the adapter's official-origin constants, their collector reexport and the original verifier's shared canonical-directory helper. Local path dependencies are source inputs; their edits are not covered by lockfiles alone.
Renderer conditions and planner conditions cannot change while keeping the same registry identity.
The two SOL Study identities also bind their own Study, readers and fitting implementation sources. Including source bytes does not import or run those implementations.
The digest frames both paths and source bodies. Changing a materializer changes all affected tool identities; a Study-only identity cannot validate against this registry.
The raw adapter preserves each recovery seed as a separate series. It never joins history across that boundary.
The original verifier retains dataset and shard scope in the opaque handle. The planner requires one market/dataset/shard scope, unique capture sessions and ordered, nonoverlapping receive intervals across supplied series.
Trade direction requires the requested symbol's verified trade modality and causal trade evidence in every supplied series. A trade in one session cannot qualify a LOB-only session.
Feature coverage and clocks stop at the frozen decision-window end. `label_available_through_ns` separately records the same recovery series' label-only book coverage after that decision.
Post-decision diffs can extend this label endpoint. They never update feature clocks, depth, snapshot/diff counts or feature continuity.
A post-decision recovery snapshot cuts the old series' label interval; a later series cannot mature its targets.
Legacy H1/H2 and sequence materializers ignore checkpoints, so checkpoints extend neither feature nor label coverage.
Market encoder checkpoint flushing is not advertised through these conservative summaries.
Top5, flow and Study materials require at least five observed seed levels. Shallow books remain usable raw replay inventory.
Observed seed depth does not prove every replay row has enough levels. Materialization must verify that condition independently.

## Automatic comparison

Suitable continuous inputs plus matching verified rule artifacts propose two registered materialization field families without feature hints.
H1 uses the renderer's nine snapshot fields. H2 replaces aggregate trade imbalance with `cont_ofi_lag60s`.
The comparison preserves the goal's target, horizon, model, scaling, cost and partition identities.
It reports the different information histories in `materializations`. These candidates are not trials.
Scientific `arms` remain empty. `requested_resources` is `None`, and candidate plans have status `NoExecutableComparison` because no executable comparison and accounting contract is bound.
A hypothesis is an unfunded comparison proposal. It is not proof that the frozen resource limit admits that comparison.
The calendar renderer's 138-trial comparison family controls statistical correction. Its own source distinguishes this from individually reserved Campaign trials. Neither two materials nor the statistical family can substitute for actual trial accounting.
The next native execution slice must resolve the real template, counter and signed budget before accepting a scientific comparison.
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
Materialization tools also require instrument-rule coverage for their own history and label availability window.
H1/H2 material candidates require a single continuous book series covering the complete decision window and `window_end + observation_frequency × horizon`. Immature tape emits no candidates, even when rule artifacts cover the label end.
The goal computes this endpoint with checked arithmetic before tape scanning. Post-decision label data is not a feature input or a permission grant.
The raw replay tool can still describe book replay when rule inputs are missing; inventory never upgrades that fact into materialization readiness.
Reference artifacts must match the same market and symbol, preserve stable rules, bracket the sixty-second lookback and forward-label end, and satisfy the original 90-second maximum observation gap. Spot references also preserve the materializer's enabled fill-bound checks.
Verified rule data and manifest hashes participate in the capability and proposal identities.
Opaque results from separate verifier calls cannot be concatenated to bypass dataset, shard, session or receive-order boundaries.
Each field decision clock must lie within the frozen goal window. Its availability cannot exceed that window end.
A lookback, decision coverage and mature label coverage must fit within one continuous series. The frozen resource limit remains a future admission constraint; this planner neither estimates nor reserves scientific compute or trials.
The label coverage field is a serialized declaration. Restoring it from JSON never restores raw verification or current DataView permission.
Independent validation, strategy sealed, meta certification and exposed terminal views cannot feed this family search.
A caller's view label is a declaration. Raw verification does not establish exposure-ledger or permission truth.
The next execution slice must verify those bindings through the current governed Campaign contract.

This slice provides a library API. CLI registration and Campaign execution belong to their own coordinated slices.
Unit cases verify software behavior. They do not prove real data coverage, training, profitability, RSI improvement or deployment.
