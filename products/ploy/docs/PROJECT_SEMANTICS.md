# Ploy Project Semantics

This file is the repository-level semantic contract for Ploy. Read it before
PM5D, event ML, factor research, replay, backtest, dry-run, live-prep, or
deployment-promotion work.

## What Ploy Is

Ploy is a multi-crate trading platform for prediction-market strategies, not a
single bot script. The current canonical runtime path is:

- `apps/new-ployd`: daemon and HTTP control-plane surface
- `apps/ployctl`: operator client for system, trading, and deployment control
- `apps/new-ploy-runner`: strategy runner using unified strategy configs
- `crates/ploy-trading`: canonical trading lifecycle
- `crates/ploy-strategy-bundles`: signal-to-intent strategy logic
- `crates/ploy-strategy-runtime`: strategy dispatch and runtime ownership
- `crates/ploy-research`: replay, factor research, backtest, and evidence
  generation

Archived or compatibility commands in older docs are not the default operating
surface unless a task explicitly targets legacy code.

## Trading Lifecycle

The platform lifecycle is:

```text
market data -> signal -> intent -> order -> ack -> fill -> position -> pnl -> risk
```

Research, replay, dry-run, and live work must say which lifecycle segment is
being tested. A result about signal quality is not evidence that execution,
fills, settlement accounting, or risk behavior is ready.

## PM5D Strategy Semantics

PM5D / five-minute crypto event work is event-rooted binary-options trading.
The strategy question is not only whether a short-horizon price move predicts a
quote move. It is which of these lanes is being evaluated:

- `settlement_probability`: estimate the probability that the event settles on
  a side, then compare that probability with executable entry cost.
- `repricing`: exploit short-horizon Polymarket quote movement before
  settlement.
- `runtime_parity`: prove replay, dry-run, and runtime use the same scorer,
  input sequence, and accounting semantics.
- `execution_quality`: prove quote freshness, depth, fillability, slippage,
  queue priority, and venue constraints are modeled conservatively.

Default promotion priority is settlement-probability evidence. Repricing
evidence can stay valuable as a diagnostic lane, but it must not silently become
the main dry-run or live promotion path.

Executable PM5D accounting is one event, one decision, one trade lifecycle
unless a task explicitly defines a multi-entry strategy. Entry-grid diagnostics
that evaluate many timestamps from the same event are research diagnostics, not
deployable trade counts.

## Evidence Stages

Use these stage names consistently in reports, issues, and handoff artifacts:

- `diagnostic`: explores signal shape or data behavior. Not deployable.
- `factor_attribution`: measures factor direction, stability, IC/ICIR, or
  bucket behavior. Not deployable by itself.
- `executable_replay`: uses executable prices, realistic fill assumptions,
  event-level accounting, and settlement or exit labels.
- `walk_forward`: separates train/validation/test windows and blocks leakage.
- `runtime_parity`: proves research and runtime scorers/configs produce the
  same decisions on the same input sequence.
- `dry_run_candidate`: has passed the required gates and can be tested with
  fixed small stake and kill switches.
- `live_candidate`: dry-run evidence plus operator, risk, balance, settlement,
  claim/redeem, and deployment guardrail evidence.

Do not collapse these stages. A positive diagnostic or factor result is a reason
to continue research, not a reason to deploy.

## Data Semantics

Reports must name the data surfaces used and the surfaces missing:

- Chainlink opening and close observations: contract reference and official
  settlement truth for governed PM5D crypto events.
- Binance spot or trade ticks: external predictive movement and repricing input.
- Binance L2 / LOB: external depth, pressure, and short-term repricing context.
- Polymarket quote ticks: top-of-book PM market state.
- Polymarket full CLOB depth: executable sweep, capacity, and conservative
  fillability.
- Official settlement: final binary outcome for settlement-probability
  accounting.
- Dry-run/runtime fills: observed execution behavior, not a substitute for
  official settlement labels.

Binance is never the PM5D opening reference, settlement oracle, or executable
venue. Polymarket CLOB data is never a substitute for Chainlink settlement
truth, and Chainlink observations are never a substitute for CLOB price, fees,
depth, or fillability.

FactorEvolve data surfaces use these fail-closed categories:

- `required_for_prediction`: needed to test the stated predictive hypothesis.
- `required_for_execution`: needed to model executable price, depth, slippage,
  capacity, or fillability.
- `optional_context`: useful context that is not required for the current
  hypothesis.
- `missing_blocks_promotion`: missing evidence that blocks promotion beyond the
  current diagnostic or research stage.

Current known FactorEvolve gaps include Binance futures OI, funding,
liquidation, basis / mark data, and OKX / Bybit LOB as first-class research
surfaces. These are not automatic blockers for every PM5D settlement factor, but
they are `missing_blocks_promotion` whenever the hypothesis depends on them.
Use [docs/runbooks/factor-evolve-data-surfaces.md](runbooks/factor-evolve-data-surfaces.md)
for the current data-surface gate.

If full-depth CLOB, official settlement, replay parity, or runtime scorer parity
is missing, mark promotion as blocked unless the task explicitly targets an
earlier diagnostic stage.

## Promotion Gates

A PM5D or event-ML strategy may move toward dry-run only when the evidence names
and passes the relevant gates:

- hypothesis and expected edge mechanism are explicit
- data window, symbols, snapshot/run ID, git ref, and artifacts are recorded
- data audit status and missing surfaces are listed
- executable price and fillability assumptions are conservative
- settlement or exit label matches the intended strategy lane
- train/test or walk-forward split prevents leakage
- historical executable replay is present before dry-run handoff
- replay/dry-run parity is present after dry-run evidence exists and before
  live promotion
- runtime scorer/config mapping is explicit
- risk, stake, kill switch, and daily loss limits are stated
- caveats and next decision are recorded

If any required gate is missing, the decision should be `continue`, `revise`, or
`reject`, not `promote to dry-run` or `promote to live`.

## Agent Operating Rule

Before running or interpreting research/backtest/promotion work, an agent must:

1. Read this file and the relevant runbook under `docs/runbooks/`.
   For CI/CD alpha-factor search, also read
   `docs/ALPHA_FACTOR_SEARCH_CICD.md`.
2. State the evidence stage being produced.
3. Write results using `tasks/research_evidence/TEMPLATE.md` or a stricter
   machine-readable artifact when a workflow already provides one.
4. Treat conflicts with this semantic contract as blockers or caveats.

Subagents working on Ploy research must receive this file path in their task
prompt. Memory can provide historical context, but this file is the project
source of truth for current semantics.
