# PM5D Alpha Factor Reuse Plan - 2026-05-12

## Evidence Stage

`factor_attribution` -> `walk_forward`.

This plan is not dry-run or live approval. It turns prior PM5D factor evidence
into a typed alpha-search prior that can be evaluated by CI.

## Product Semantics

PM5D / PM15D Polymarket crypto events are binary-option-like markets. Each
event has an `UP` side and a `DOWN` side. At settlement, one side pays `1` and
the other pays `0`. The main deployable lane is therefore:

```text
estimated settlement probability - executable entry price - friction > 0
```

Promotion evidence must use official settlement, executable prices, fillability,
walk-forward/OOS windows, and replay/dry-run parity.

## Prior Factor Evidence To Reuse

| Lane | Factor family | Historical read | Decision |
| --- | --- | --- | --- |
| Settlement probability | `side_fair_prob` | Strong IC/ICIR for settlement outcome and executable settlement PnL, but naive `side_fair_edge = side_fair_prob - entry_ask - fee` was rejected. | Reuse as a probability signal, not as a direct edge-subtraction formula. |
| Liquidity-gated alpha | `side_model_prob`, `side_distance_over_sigma` | Raw lane was unstable, but liquidity-gated rows became positive across prior OOS windows with full fillability. | Reuse only behind conservative/full-depth settlement edge and capacity gates. |
| Execution quality | `entry_capacity_score`, `side_spread`, full-depth/conservative sweep prices | Required to prevent positive probability signals from becoming non-executable trades. | Hard gate or denominator/penalty, not optional decoration. |
| Near-strike geometry | `near_strike_score`, `side_distance_over_sigma` | Short-dated near-strike events are where small external moves matter most, but they can overfit. | Use as bounded interaction and require walk-forward stability. |
| Repricing backup | `spread_adjusted_external_move` | Strong repricing IC/ICIR, especially BTC/ETH/SOL; not the primary settlement lane. | Keep as backup branch, not settlement-promotion evidence. |

## Negative Constraints

- Do not promote naked `side_fair_edge`; previous settlement executable-PnL
  evidence rejected it.
- Do not promote raw `side_model_prob` or raw `side_distance_over_sigma`
  without liquidity/full-depth gates.
- Do not treat `exit_bid_change_30s`, `entry_ask_change_30s`,
  `pm_reprice_speed_30s`, or `obi_persistence_30s_side` as standalone
  settlement strategies. Prior liquidity-gated reads rejected or weakened them.
- Do not count many entry timestamps from the same event as independent
  deployable trades unless runtime implements the same multi-entry lifecycle.

## Typed Prior Artifact

Use:

```text
tasks/alpha_search_priors/pm5d_settlement_liquidity_prior_20260512.json
```

The file intentionally contains only supported mutation types:

- `add_near_strike_interaction`
- `add_capacity_gate`
- `add_feature_gate`
- `replace_denominator`
- `add_spread_penalty`
- `clip_or_squash`
- `change_time_window`

The Rust alpha-search compiler should ignore any mutation whose base factor or
feature is absent from the current snapshot.

## Recommended First CI Run

Run on a retained full research snapshot, preferably the latest BTC/ETH
settlement-probability snapshot with recorded parity available. Use
`full_depth_settlement_executable_pnl` as the allowed target.

Suggested hosted artifact inputs:

```json
{
  "train_window_days": 2,
  "test_window_days": 1,
  "step_days": 1,
  "lob_sample_secs": 30,
  "observation_sample_secs": 30,
  "max_quote_age_secs": 30,
  "top_n": 20,
  "min_observations": 20,
  "top_quantile": 0.2,
  "factor_name_filter": "",
  "data_quality_mode": "event_complete",
  "min_event_complete_events": 20,
  "min_event_complete_rows": 40,
  "alpha_search_llm_prior_json": "tasks/alpha_search_priors/pm5d_settlement_liquidity_prior_20260512.json",
  "alpha_search_plan_target": "full_depth_settlement_executable_pnl",
  "alpha_search_min_reward_improvement": 0.01,
  "chain_next_run": true,
  "chain_remaining": 2,
  "required_strategy_profile": "settlement_probability",
  "allowed_target": "full_depth_settlement_executable_pnl",
  "fail_if_blocked": false
}
```

## Success Criteria

Continue only if the run produces:

- non-empty alpha-search artifacts;
- `mcts-state.json` and `mcts-expansion-plan.json`;
- candidate factors with positive reward and no promotion blockers;
- walk-forward/OOS support, not just in-sample ranking;
- `autofactor-strategy-handoff.json status=ready` before any config PR.

Promotion remains blocked unless recorded replay/dry-run parity and fresh
dry-run executable evidence are clean.

## First Hosted Run Result

Run:

- Workflow: `factor-walk-forward-v2-hosted-artifact.yml`
- Run: `25707061616`
- Branch/ref: `research/pm5d-alpha-prior-reuse`
- Snapshot: `25642459432`
- Replay parity artifact: `recorded-replay-parity-25687392088`
- Symbols: `BTCUSDT,ETHUSDT`
- Window: `2026-04-24T00:00:00Z -> 2026-05-01T00:00:00Z`
- Target: `full_depth_settlement_executable_pnl`

Result:

- Status: workflow success.
- Alpha-search candidates: `193`.
- Passed candidates: `38`.
- Best reward: `4.81954070569323`.
- Best selected factor: `auto_settlement_conservative_settlement_edge`.
- Handoff: `ready`.
- Recommended action: `create_dry_run_handoff`.
- Chain decision: `ready_handoff`, so no follow-up chained run was dispatched.

Prior-specific read:

- The typed prior compiled into `llm_*` candidates, including near-strike,
  capacity, fair-probability, spread-penalty, full-depth, and repricing backup
  variants.
- The best typed-prior candidate was
  `llm_conservative_settlement_edge_near_strike` with reward
  `4.775414902351155`, matching the deterministic near-strike branch.
- The best overall factor stayed the simpler
  `auto_settlement_conservative_settlement_edge`.

Interpretation:

- Reusing the user's previous factors is valid: the prior entered CI and
  produced evaluated candidates.
- The prior did not beat the simpler conservative settlement edge on this
  BTC/ETH snapshot/window.
- The next strategy step should not be to add more formula complexity. It
  should be to deploy/collect/compare the ready conservative settlement dry-run
  candidate, then use fresh dry-run evidence to decide whether near-strike or
  capacity variants deserve a second promotion attempt.
