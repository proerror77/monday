# Alpha Factor Search CI/CD Contract

This document defines Ploy's CI/CD method for mining alpha factors with a
two-layer search architecture:

```text
LLM semantic prior layer
  -> MCTS/systematic search layer
  -> CI backtest feedback layer
  -> candidate strategy executable replay layer
  -> promotion/handoff gate
```

It complements `docs/PROJECT_SEMANTICS.md`; it does not bypass settlement,
execution, runtime-parity, or dry-run/live promotion gates.

## Paper Basis

The design is grounded in these research patterns:

- `Navigating the Alpha Jungle`: combine LLM symbolic formula generation with
  MCTS exploration, backtest feedback, and frequent-subtree avoidance for
  interpretable formulaic factor mining.
- `RiskMiner`: formulate alpha mining as a reward-dense MDP and solve it with
  risk-seeking MCTS so the search uses the structure of the discrete formula
  space instead of treating candidate generation as a flat neural policy.
- `Alpha-GPT`: use LLMs as an interactive bridge from human trading ideas to
  symbolic alpha expressions and iterative modification suggestions.
- `QuantaAlpha`: preserve semantic consistency between hypothesis, expression,
  and executable code, and reuse high-reward trajectory segments through
  controlled evolution.

For Ploy, these are architecture inputs, not permission to skip repo gates.
The useful pattern is LLM-guided symbolic priors plus machine-checked
exploration and CI evidence.

## Current State

The repo now has the core alpha-search factory, not only the downstream
promotion half:

- `factor-walk-forward-v2-hosted-artifact.yml`: GitHub-hosted path that consumes
  a retained complete sampled research snapshot artifact, emits alpha-search artifacts, and
  can chain bounded follow-up search runs.
- `autofactor-strategy-promotion.yml`: evaluator for existing
  `factor-walk-forward-v2-*` artifacts.
- `scripts/evaluate_autofactor_strategy_promotion.py`: fail-closed promotion
  gate from discovered factor rows to strategy handoff artifacts.
- `scripts/apply_autofactor_handoff_to_config.py`: ready-only config PR bridge.
- `crates/ploy-research/src/autofactor.rs`: Rust `FactorExpr` DSL, safe
  operators, seed candidates, settlement-native generated candidates, and
  candidate/watchlist/reject scoring.
- `crates/ploy-research/src/alpha_search.rs`: alpha-search artifact writer for
  search space, typed priors, candidates, rejected expressions, tree trace,
  node metrics, search feedback, and MCTS expansion plan.

FactorEvolve data-surface gates are defined in
`docs/runbooks/factor-evolve-data-surfaces.md`. Search artifacts and research
issues should classify data dependencies as `required_for_prediction`,
`required_for_execution`, `optional_context`, or `missing_blocks_promotion`.
Current first-class surfaces include Binance aggTrade / spot ticks, Polymarket
quote ticks, Polymarket full CLOB snapshots, official settlement, and runtime
fill evidence. Binance futures OI, funding, liquidation, basis / mark data, and
OKX / Bybit LOB remain external-data roadmap gaps; they block promotion only
when the candidate hypothesis depends on them.

That means the repo can already do:

```text
research snapshot
  -> factor walk-forward
  -> alpha-search artifact bundle
  -> MCTS expansion plan
  -> optional chained hosted search iteration
  -> candidate strategy executable replay artifact
  -> AutoFactor promotion evaluation
  -> blocked/ready handoff artifact
  -> optional dry-run config PR
```

The implemented search controller is intentionally bounded. It is currently a
typed, deterministic formula-search and MCTS-planning layer rather than a
free-form code generator:

```text
semantic hypothesis + formula prior
  -> feature pool + constant pool + operator pool
  -> MCTS candidate expansion
  -> multi-dimensional backtest feedback
  -> search trace artifacts
  -> factor walk-forward rows
```

The main unresolved blocker is no longer "can the system generate/explore alpha
candidates?" It can. The current blocker is promotion quality evidence:
the selected runtime score must first pass a strategy-level historical
`executable_replay` using event-level decisions, full-depth executable prices
and depth, conservative fills, and official settlement. After dry-run starts,
recorded replay/dry-run parity must also be ready for the target dry-run lane.
Search reward, MCTS rank, or a ready-looking candidate row is not enough to
claim a profitable strategy.

## Architecture

### 1. LLM Semantic Prior Layer

The LLM should produce a prior, not final truth. Its job is to translate a
market intuition into a typed, reviewable search seed:

- natural-language hypothesis
- expected edge mechanism
- applicable strategy lane, such as `settlement_probability` or `repricing`
- required data surfaces
- candidate feature families
- initial symbolic `FactorExpr` formulas
- suggested constants and bounded ranges
- suggested local modifications
- invalid assumptions and expected failure modes

The LLM output must be machine-checkable before it enters search. Invalid
features, unsupported operators, unbounded constants, future-looking columns,
or missing data surfaces are rejected before any backtest.

Required artifact:

- `llm-priors.json`
- `llm-priors.md`

### 2. MCTS Search Layer

MCTS owns exploration and exploitation over the formula space. Each node is a
candidate formula state:

```text
node = {
  factor_expr,
  hypothesis_ref,
  parent,
  mutation,
  selected_dimension,
  metrics,
  visits,
  reward,
  blockers
}
```

The search loop:

```text
select node
  -> choose weak dimension
  -> expand with allowed mutation
  -> evaluate candidate
  -> backpropagate multi-dimensional reward
  -> persist structural subtree frequencies
  -> penalize crowded or invalid subtrees
```

MCTS may use the LLM as an expansion policy, but only inside the declared DSL.
The LLM can propose modifications like "gate by quote freshness" or "scale by
capacity", but the Rust layer must compile them into `FactorExpr` using allowed
features, constants, and operators.

Required artifacts:

- `tree-trace.json`
- `node-metrics.json`
- `candidate-expressions.json`
- `rejected-expressions.json`
- `avoided-subtrees.json`

### 3. CI Backtest Feedback Layer

Backtest feedback is the scoring signal for MCTS. It must be produced by the
same CI evidence path used for promotion:

- snapshot provenance
- walk-forward windows
- executable target labels
- data-surface audit
- fillability/capacity assumptions
- settlement or exit accounting
- promotion-gate readiness

The reward should not be a single PnL number. It should combine:

- effectiveness: executable target score, PnL, IC/ICIR, top-bucket label
- stability: positive-window ratio, window decay, symbol holdout behavior
- diversity: novelty versus accepted/watchlisted factor expressions
- execution cost: spread, capacity, fillability, quote age, turnover-like churn
- event uniqueness: top-bucket one-event-one-decision behavior, unique event
  coverage, and repeated-event penalties before promotion
- overfit risk: complexity, parameter count, train/test decay, permutation and
  time-shift sensitivity
- runtime readiness: explicit profile mapping and scorer support

Required artifact:

- `search-feedback.json`
- `search-feedback.md`

### 4. Candidate Strategy Executable Replay Layer

This layer turns the selected factor/runtime score into a historical strategy
replay before any dry-run handoff. It is separate from factor IC/ICIR scoring
and separate from recorded replay parity.

Required artifact:

- `candidate-strategy-replay.json` or
  `autofactor-candidate-strategy-replay.json`

In the hosted factor walk-forward sweep, this artifact is generated per variant
when an external replay artifact is not supplied. The generated artifact is
based on the selected runtime-mappable `factor_walk_forward_v2` top bucket and
declares `basis=factor_walk_forward_top_bucket_aggregate`. It is only candidate
context. It is not sufficient for the pre-dry-run executable strategy gate,
because it does not prove the deployed runtime scorer emits the same decisions
on an ordered MarketUpdate stream.

The dry-run handoff gate requires a true runtime replay artifact with
`basis=runtime_market_update_replay`, produced from `ploy-runner run
--output-json` by `scripts/build_runtime_candidate_strategy_replay.py`. It does
not replace recorded replay/dry-run parity after a dry-run runtime has produced
orders and fills.

Recorded replay/dry-run parity is intentionally after dry-run in the promotion
sequence. It should be reported as pending during pre-dry-run research, not as a
blocker that prevents a historically replayed candidate from entering dry-run.

Minimum contract:

- `evidence_stage=executable_replay`
- exact `strategy_profile` and `runtime_score`
- event-level, one-decision-per-event accounting
- official settlement labels
- full-depth executable entry/depth/fill assumptions
- trade count, unique event count, entry fill rate, PnL or ROI, and drawdown
- `promotion_ready=true` and no blocking risk flags

### 5. Promotion/Handoff Layer

Only this layer can produce a dry-run handoff or config PR. The LLM and MCTS
layers can generate candidates, but they cannot promote them.

The existing fail-closed path remains:

```text
factor-walk-forward-v2/report.txt
  + candidate-strategy-replay.json
  -> scripts/evaluate_autofactor_strategy_promotion.py
  -> autofactor-factor-registry.json
  -> autofactor-strategy-handoff.json
  -> optional ready-only handoff issue or config PR
```

## Search-Space Contract

Every alpha-search run must declare its search space in machine-readable form.

### Feature Pool

Features should be grouped by data meaning, not only by column name:

- external price movement: Binance spot / aggTrade moves, velocity, acceleration
- external microstructure: L2 imbalance, OFI, depth, thinness, pressure
- Polymarket quote state: side ask/bid, spread, quote age, PM lag
- full-depth execution: sweep price, capacity, conservative depth assumptions
- event geometry: time remaining, distance to strike, near-strike score
- probability state: external/model q estimates, market prior, EventVolSurface
  prior; PM quote-implied fair probability is diagnostic unless paired with an
  independent predictive model
- volatility state: realized/IV/DVOL primitives when present and audited
- risk/friction: fees, slippage, fillability, capacity, turnover proxies

A feature absent from the snapshot must fail closed for generated candidates
that require it. Do not silently substitute a different data surface.

### Constant Pool

Constants are part of the hypothesis. They must be declared, bounded, and
reported. Initial safe pools:

- additive epsilons: `0.001`, `0.005`, `0.01`, `0.02`
- spread/capacity thresholds: `0.01`, `0.02`, `0.05`, `0.10`
- time windows or lags: `1`, `3`, `5`, `10`, `30`, `60`, `300`
- clipping bounds: `[-5, 5]`, `[-3, 3]`, `[0, 1]`
- nonlinear scales: `1`, `2`, `3`, `5`, `10`

Constants discovered by search are not automatically runtime constants. A
runtime handoff must show the selected constants, their source run, and why they
survived walk-forward and anti-overfit gates.

### Operator Pool

Operators must be safe, deterministic, and supported by replay/runtime parity.
The current `FactorExpr` foundation already supports:

- inputs and constants
- `Add`, `Sub`, `Mul`
- `SafeDiv`
- `Max`, `Min`
- `Tanh`, `Log1pAbs`, `SqrtAbs`
- `Clip`
- `Delta`
- `RollingMean`, `RollingStd`, `ZScore`

New operators require deterministic semantics, finite-value handling,
complexity accounting, serialization compatibility, test coverage, and a
runtime/replay parity plan if the operator can reach strategy handoff.

## LLM Modification Types

LLM proposals must compile into one of these bounded mutation types:

- `add_feature_gate`: multiply by a bounded regime, capacity, or freshness score
- `replace_denominator`: change a normalization term with safe division
- `add_spread_penalty`: subtract or divide by executable friction
- `add_capacity_gate`: penalize candidates that only work at unavailable depth
- `add_near_strike_interaction`: condition on event geometry
- `change_time_window`: swap among declared lag/window constants
- `clip_or_squash`: add `Clip`, `Tanh`, or `Log1pAbs` for robustness
- `invert_or_contrarian`: test opposite sign only when semantically justified
- `remove_component`: ablate a subtree to reduce overfit risk

Free-form code generation is not allowed in the search loop. LLM output is
advice until it is parsed into typed mutations.

## Frequent-Subtree Avoidance

The search layer tracks repeated expression structures with canonical structural
signatures, not only top-level operator names. The signature normalizes
commutative `Add`/`Mul` operands and abstracts numeric constants, so formulas
that only reorder operands or tune an epsilon are treated as the same shape.
The search records both whole-expression signatures and repeated inner subtrees
with structural depth >= 2.

Crowded signatures are persisted in `mcts-state.json` under
`subtree_frequencies`, then surfaced in `avoided-subtrees.json` when their
count crosses the penalty threshold. Node metrics split the old overloaded
`diversity` meaning into `simplicity`, `structural_novelty`, and
`diversity_penalty`; reward subtracts the diversity penalty so repeated
structures affect MCTS ranking instead of remaining write-only diagnostics.

Examples:

- repeated `safe_div(external_move, spread + eps)` variants
- repeated `edge * near_strike_score` variants
- repeated stale-quote gates with only epsilon changes

Avoidance is not a hard ban forever. It is a diversity control: a crowded
subtree can be revisited only when the candidate improves a declared weak
dimension such as capacity, stability, or overfit risk. The closed-loop agent
also carries crowded `structural_avoid_signatures` into the next
`llm-priors.json` draft, so the next prior-generation step can avoid repeating
known-crowded formula shapes before Rust scoring penalizes them.

## Alpha Zoo

Frequent-Subtree Avoidance is batch-local: it only compares candidates within
the current search run. The Alpha Zoo is the paper's ("Navigating the Alpha
Jungle") complementary cross-run diversity control: a durable, persistent
population of previously-accepted factors that new candidates are also
checked against, regardless of which run produced them.

The Alpha Zoo snapshot is sourced from the `factor_registry` table, which
`persist_research_trace` writes to across every historical run. Unlike
`avoided-subtrees.json`, which only ever sees the reports passed into a single
`write_alpha_search_artifacts_with_state_and_runtime_feedback` call, the Alpha
Zoo snapshot reflects every `candidate`, `dry_run`, `approved`, or `production`
row ever recorded for a target.

Flow:

1. `persist_research_trace --export-alpha-zoo-snapshot <path> --export-alpha-zoo-target <target>`
   queries every `factor_registry` row, groups the accepted ones by root gene
   with `group_factor_registry_rows_into_alpha_zoo_snapshot`, and writes an
   `AlphaZooSnapshot` JSON file.
2. `factor_walk_forward_v2 --alpha-zoo-snapshot-json <path>` loads that file and
   passes it as `Some(&snapshot)` into
   `write_alpha_search_artifacts_with_state_and_runtime_feedback`. Omitting the
   flag passes `None`, which is a strict no-op: reward and node metrics are
   identical to a run with no Alpha Zoo evidence at all.
3. `reward()` subtracts `alpha_zoo_novelty_penalty(&report.expr, alpha_zoo)`
   from the same sum where `execution_penalty` and `runtime_pass_through_penalty`
   are subtracted, and `node_metric()` records `alpha_zoo_novelty` and
   `alpha_zoo_penalty` alongside the other per-candidate score fields.

The Alpha Zoo currently reuses the coarse root-operator-only `root_gene()`
fingerprint (the same one `avoided_subtrees` uses), with a higher crowding
threshold (`5`) than Frequent-Subtree Avoidance's batch-local threshold (`2`),
because the Alpha Zoo aggregates every historical run rather than a single
batch. This can be upgraded to the finer-grained `structural_signature()` once
that lands on `main`.

## Workflow Roles

- `factor-walk-forward-v2-hosted-artifact.yml` should be the default efficient
  search surface once a complete sampled snapshot artifact exists.
- Build or select a retained `research-snapshot.yml` artifact first, then
  dispatch `factor-walk-forward-v2-hosted-artifact.yml` directly with
  `snapshot_run_id` so the hosted artifact workflow performs the search.
- `pm5d-execution` settlement-probability searches should not require Deribit by
  default. Use `options_json.require_deribit=true` only when the hypothesis
  explicitly depends on the `pm5d-vol`/Deribit surface.
- Settlement-probability promotion defaults to
  `min_promotion_entry_fill_rate=0.30`, including global full-depth entry
  fillability. Lower capacity is research evidence only, because a signal that
  cannot be filled at the configured stake is likely already reflected in the
  Polymarket book.
- AutoFactor promotion also requires the selected factor's top bucket to pass
  `top_bucket_full_depth_entry_fill_rate >= 0.30` by default. This prevents a
  statistically strong factor from being promoted when its highest-score rows
  are mostly unfillable at the configured stake.
- For `tradeable_full_depth_settlement_pnl`, the search space may generate
  hard `full_depth_entry_fillable_gate` mutations that filter out rows where the
  current entry book cannot fill the configured stake. Treat these as execution
  gates, not alpha features: a gated predictive formula asks whether the
  predictive signal still works inside tradable capacity, and runtime promotion
  remains blocked unless the scorer can reproduce the same book-depth gate
  before order placement.
- One-day OOS smoke searches can use hourly windows, for example
  `train_window_hours=12`, `test_window_hours=12`, and `step_hours=12`, when
  only a clean 24h snapshot is available. Keep these marked as early
  walk-forward evidence, not final promotion-grade history.
- `autofactor-strategy-promotion.yml` should be used to re-evaluate an existing
  walk-forward artifact without rerunning search.
- `event-ml-rolling-evidence.yml` is the supervised event-ML lane, not the
  formula-search lane, though it may consume factor registries later.

## Required Search Artifact Bundle

Every complete CI/CD alpha-search run should upload:

- `search-space.json`: feature pool, constants, operators, targets, limits
- `llm-priors.json`: hypotheses, symbolic seeds, suggested modifications
- `candidate-expressions.json`: all generated `FactorExpr` candidates
- `rejected-expressions.json`: invalid or blocked expressions and reasons
- `tree-trace.json`: parent/child expansions, selected weak dimension, rewards
- `node-metrics.json`: multi-dimensional scores per node
- `mcts-expansion-plan.json`: UCB-style branch selection for the next search
  run, including selected weak dimension and proposed mutation type
- `avoided-subtrees.json`: repeated subtrees blocked or penalized
- `search-feedback.json`: backtest feedback used by MCTS
- `alpha-search-chain/chain-decision.json`: hosted-workflow continuation
  decision, including whether the next run was dispatched and why the chain
  stopped when it did not continue
- `factor-walk-forward-v2/report.txt`: existing walk-forward report
- `candidate-strategy-replay.json`: selected runtime-score historical runtime
  replay proof with `basis=runtime_market_update_replay`, required before
  dry-run handoff
- `autofactor-factor-registry.json`: evaluated factor rows and blockers
- `autofactor-strategy-handoff.json`: ready/blocked handoff manifest

If any of the search artifacts are missing, the run can still be diagnostic,
but it is not a complete alpha-search run.

For a quick review across multiple downloaded hosted runs, use:

```bash
python3 scripts/summarize_alpha_search_chain.py \
  /tmp/ploy-alpha-run-25683730944 \
  /tmp/ploy-alpha-run-25683858420 \
  --output-json /tmp/alpha-search-chain-summary.json \
  --output-md /tmp/alpha-search-chain-summary.md
```

The summary reports candidate counts, passed counts, best reward, selected MCTS
factor, handoff status, recommended action, and chain stop reason per run. It
is a review aid only; it does not replace the promotion evaluator or replay
parity gate.

## Closed-Loop Agent Review

After a hosted alpha-search run or chain stops, run the closed-loop classifier
before deciding what to do next:

```bash
python3 scripts/alpha_search_closed_loop_agent.py \
  /tmp/ploy-alpha-run-25683730944 \
  --output-json /tmp/alpha-closed-loop-decision.json \
  --output-md /tmp/alpha-closed-loop-decision.md \
  --output-prior-json /tmp/alpha-next-llm-prior.json
```

The classifier reads existing CI artifacts only. It does not call an LLM, does
not edit runtime config, and does not promote a strategy. Its output action is
one of:

- `ready_handoff`: use the existing ready-only AutoFactor handoff/config PR
  path.
- `continue_search`: dispatch the next bounded hosted MCTS iteration only when
  the classifier also emits `allow_dispatch=true`.
- `revise_prior`: review and pass the generated typed prior JSON into the next
  run through `options_json.alpha_search_llm_prior_json`.
- `fix_data`: repair missing/weak data surfaces before more search.
- `fix_runtime`: repair runtime scorer, mapping, or parity before promotion.
- `fix_workflow`: repair missing CI evidence artifacts, such as recorded
  replay parity, before interpreting promotion readiness.

Stagnation, empty selected-node plans, and high rejection ratios map to
`revise_prior` with `allow_dispatch=false`. That makes the workflow stop
automatic chaining, publish a bounded typed prior draft, and require a human or
LLM judgment step before another explicit research-CI dispatch.

Execution blockers also take priority over runtime mapping blockers. For
example, `one_event_decision_violation`, top-bucket slippage, or fillability
problems mean the candidate is still a search/prior problem; the classifier
returns `revise_prior` even if the same factor also lacks a runtime mapping.
When a runtime candidate replay artifact is present, the hosted workflow keeps
the full `candidate-strategy-replay.json` in the uploaded artifact bundle so
the classifier can inspect runtime diagnostics, not only promotion blockers.
If the score has many direct threshold passes but runtime entry signals still
collapse at executable edge, min-edge, side, or depth gates, the classifier
returns `revise_prior` with a runtime pass-through typed-prior draft. That
prevents the next search from repeating factors that look strong in aggregate
top-bucket diagnostics but cannot survive the executable runtime decision path.

This is the current closed-loop boundary: a failed or stagnant search now
produces a machine-readable next action and, when appropriate, a bounded typed
prior draft using only the existing allowed mutation types. It still cannot
guarantee profitability and cannot bypass walk-forward, promotion, replay
parity, dry-run, or live approval gates.

## Real LLM Expansion

The hosted artifact workflow can optionally replace the deterministic
closed-loop prior with a real model-proposed typed prior. This is off by
default. Enable it only on an explicit `workflow_dispatch` run by setting:

```json
{
  "enable_llm_expansion": true,
  "llm_expansion_provider": "anthropic",
  "llm_expansion_model": ""
}
```

The workflow also requires the repository secret
`PLOY_RESEARCH_LLM_API_KEY`. If the flag is false, the secret is missing, the
model call fails, optional artifact JSON is corrupt, or the model returns no
mutations, the step exits successfully and leaves the deterministic
`next-llm-prior.json` unchanged. A successful model response is still only a
typed prior draft: Rust must compile it through the existing allowed
`LlmMutationSpec` mutation types before any candidate is evaluated.

When the provider returns usage data, the script writes
`llm-expansion-usage.json` next to `next-llm-prior.json` in the alpha-search
artifact directory. Treat that as per-run token accounting, not promotion
evidence.

## FactorEvolve Research Manager V0

The FactorEvolve Research Manager starts as a deterministic typed planning
surface, not an LLM service. Its input contract includes latest run evidence,
factor registry summary, rejected factor patterns, market-data health, and a
daily research budget. Its output is a `ResearchManagerPlan` with a theme such
as `fix_data`, `fix_runtime`, `revise_prior`, or `continue_search`.

The v0 manager cannot mutate evaluator thresholds, train/test split policy,
target labels, cost/slippage assumptions, promotion gates, or deployment
configs. LLM participation is allowed only outside this boundary by producing a
typed prior JSON that the Rust DSL compiler validates before CI evaluates it.

The first CLI surface is:

```bash
rtk cargo run -p ploy-research --example factor_evolve_daily_plan -- \
  <input-json> <output-json>
```

This command is plan-only evidence. It does not run searches, create PRs,
deploy services, or resume dry-run/live strategies.

The CI entrypoint for the daily loop is
`.github/workflows/factor-evolve-daily-research.yml`. Its first version is an
orchestrator, not a strategy mutator:

```text
typed budget and latest evidence
  -> factor_evolve_daily_plan
  -> optional hosted artifact search dispatch
  -> daily plan artifact / optional tracking issue
```

`run_mode=plan_only` emits the Research Manager plan only. `run_mode=search`
requires a retained research snapshot and dispatches the hosted artifact
walk-forward path with handoff/config mutation disabled. Pass
`max_quote_age_secs` to match the retained snapshot compile setting; for the
current compact snapshots the daily workflow defaults this to `2` so the
hosted walk-forward validator does not reject the snapshot. `run_mode=promote_handoff`
records that a manual handoff review is required; it does not edit strategy
configs or touch runtime services.

## Event-Level Promotion Gate

PM5D settlement strategies are event-rooted binary options. A deployable
candidate must prove the top deployable bucket behaves like one event, one
decision, one trade lifecycle. The AutoFactor report therefore exposes:

- `top_bucket_unique_event_count`
- `top_bucket_max_event_decisions`
- `top_bucket_avg_entry_sweep_slip_bps`
- `top_bucket_avg_entry_sweep_levels`

`scripts/evaluate_autofactor_strategy_promotion.py` treats missing event
decision fields as `missing_one_event_decision_gate` and blocks promotion when
`top_bucket_max_event_decisions > 1`. This keeps repeated observation rows or
same-event UP/DOWN rows from being counted as independent deployable trades.
Diagnostic rows can still be useful for research, but a dry-run handoff must
pass this event-level gate.

The search layer also consumes these fields before promotion. `node-metrics.json`
records event uniqueness and top-bucket sweep metrics, `reward` penalizes
repeated event decisions, missing top-bucket event coverage, high average sweep
slippage, excessive sweep levels, and low top-bucket fillability, and
`mcts-expansion-plan.json` routes repeated-event branches to
`event_uniqueness` / `add_capacity_gate` instead of generic exploitation. This
keeps MCTS from repeatedly expanding branches that look strong only because
the same event contributed multiple diagnostic rows or because the entry was
not realistically executable.

For settlement targets, AutoFactor scoring itself must be event-level:
`settlement_executable_pnl`, `full_depth_settlement_executable_pnl`, and
`tradeable_full_depth_settlement_pnl` collapse observations to one scored
candidate decision per event before IC, bucket, and promotion metrics are
computed. Repricing targets can remain row-level diagnostics because their
question is short-horizon quote movement, not final event settlement.

## Completion Criteria

The method is fully defined only when CI exposes all of the following:

- LLM prior artifact with machine-checkable hypotheses and formulas
- declared feature pool, constant pool, and operator pool
- typed mutation schema from LLM suggestions to `FactorExpr`
- MCTS controller with selection, expansion, evaluation, and backpropagation
- multi-dimensional node scoring from CI backtest feedback
- frequent-subtree/root-gene avoidance
- full search artifact bundle
- event-level one-decision promotion gate
- existing walk-forward and promotion gates
- ready-only issue/config PR creation

Current implementation status:

- Implemented: deterministic seed-search artifact bundle from
  `factor_walk_forward_v2` via `--alpha-search-output-dir`.
- Implemented: bounded deterministic multi-depth mutations over existing domain
  seeds, including squashing, spread adjustment, near-strike interaction,
  capacity gating, and PM-lag gating. The current depth is capped at `2`, with
  a candidate cap to keep CI runs bounded.
- Implemented: workflow upload path for the artifact bundle through both
  Factor Walk-Forward V2 workflows.
- Implemented: MCTS control artifacts, `mcts-state.json` and
  `mcts-expansion-plan.json`. The state artifact stores explicit factor
  parent lineage, accumulates leaf visits, backpropagates leaf rewards through
  ancestor nodes across runs, and persists structural subtree frequencies
  across runs. The expansion plan ranks non-rejected current-run nodes with a
  UCB-style priority using that cumulative state.
  `mcts-state.json.backpropagation_truncated_count > 0` means reward
  propagation hit a defensive stop before reaching a root node. Inspect
  `parent_name` lineage for cycles first; if there is no cycle, check whether
  the search state graph has grown beyond the expected bounded chain size. CI
  warnings report only newly observed truncations while including the
  cumulative artifact count.
- Implemented: `factor_walk_forward_v2 --alpha-search-plan-json <path>` can
  consume a prior `mcts-expansion-plan.json` and generate extra `mcts_*`
  guided mutations for selected branches. The Factor Walk-Forward workflows
  expose this as `options_json.alpha_search_plan_json`.
- Implemented: `factor_walk_forward_v2 --alpha-search-state-json <path>` can
  consume a prior `mcts-state.json`; when a prior alpha-search artifact is
  downloaded via `options_json.alpha_search_plan_run_id`, the workflows pass
  its `mcts-state.json` automatically when present. This carries both MCTS node
  rewards and structural subtree frequency counts into the next run.
- Implemented: `factor_walk_forward_v2 --alpha-search-llm-prior-json <path>`
  accepts a typed LLM-prior JSON file with bounded mutation requests. The Rust
  layer compiles those requests into existing `FactorExpr` candidates only when
  the requested base factor, feature, mutation type, and constants are valid.
  Unsupported free-form code is ignored rather than evaluated.
- Implemented: the Factor Walk-Forward workflows can download a prior plan from
  a previous run with `options_json.alpha_search_plan_run_id`,
  `alpha_search_plan_artifact_name`, and `alpha_search_plan_target`.
- Implemented: the hosted artifact workflow can dispatch the next search
  iteration automatically with `options_json.chain_next_run=true` and bounded
  `chain_remaining`.
- Implemented: the hosted chain stops early when the current run already
  produces `autofactor-strategy-handoff.json status=ready` or when the current
  MCTS plan has no selected nodes.
- Implemented: hosted chained runs request `actions: write` and publish
  `alpha-search-chain/chain-decision.json` before dispatch, so CI artifacts
  record `continue`, `ready_handoff`, `no_selected_nodes`,
  `chain_remaining_exhausted`, or other stop reasons instead of relying only on
  workflow logs.
- Implemented: the hosted chain compares the current
  `search-feedback.json.best_reward` with the prior plan artifact's
  `search-feedback.json.best_reward` and stops with `reward_stagnation` when
  the configured `alpha_search_min_reward_improvement` threshold is not met.
- Implemented: `scripts/alpha_search_closed_loop_agent.py` classifies stopped
  or blocked chains into a next action and emits a bounded typed prior draft
  when the correct action is `revise_prior`.
- Implemented: chained dispatch is fail-closed on the classifier artifact. The
  hosted workflow requires `closed-loop-decision.json` and dispatches only when
  it contains `allow_dispatch=true`; older chain decisions alone cannot trigger
  the next run.
- Implemented: AutoFactor promotion now fail-closes on missing or repeated
  top-bucket event decisions, so dry-run handoff cannot count repeated rows
  from the same event as independent deployable trades.
- Implemented: alpha-search node metrics and MCTS reward now include
  event-level uniqueness and execution-capacity penalties, so repeated-event or
  high-slippage candidates are de-prioritized before the promotion gate.
- Implemented: alpha-search node metrics and MCTS reward now include
  structural novelty and diversity penalties from persisted subtree crowding, so
  `avoided-subtrees.json` reflects a reward input rather than a write-only
  diagnostic.
- Implemented: settlement AutoFactor targets score one candidate decision per
  event before bucket and promotion metrics, while repricing targets preserve
  row-level diagnostics.
- Implemented as artifact and input contract: `llm-priors.json` records the
  typed prior schema, and an operator- or LLM-produced prior file can now enter
  CI through `--alpha-search-llm-prior-json` / `options_json.alpha_search_llm_prior_json`.
- Implemented: deeper typed LLM-prior expansion at the existing safe compiler
  boundary. `remove_component` can now ablate a named existing input from a
  candidate AST, or unwrap a top-level robustness/gate component, while still
  compiling only into existing `FactorExpr` nodes.
- Implemented: closed-loop prior drafts carry `structural_avoid_signatures`
  from crowded subtree artifacts, giving the next prior-generation step an
  explicit list of formula shapes to avoid.
- Implemented: a durable, cross-run Alpha Zoo novelty penalty. `reward()` and
  `node_metric()` accept an optional `AlphaZooSnapshot` grouped from historical
  `factor_registry` rows by root gene; `persist_research_trace
  --export-alpha-zoo-snapshot` produces it, and `factor_walk_forward_v2
  --alpha-zoo-snapshot-json <path>` consumes it. Omitting the flag is a no-op.
- Implemented but opt-in: direct LLM API proposal inside hosted CI through
  `scripts/alpha_search_llm_propose.py` and
  `options_json.enable_llm_expansion=true`. It writes only a typed prior draft
  and fails soft back to the deterministic closed-loop prior.

The current system is enough to start systematizing alpha discovery in CI: every
walk-forward run can now expand interpretable bounded multi-depth mutations,
download or consume a prior MCTS expansion plan, and preserve search space,
candidates, rejected expressions, node metrics, tree trace, MCTS expansion plan,
subtree crowding, and feedback artifacts. Hosted artifact runs can also dispatch
bounded chained follow-up runs with ready-handoff, empty-plan, and reward
stagnation stop criteria.
