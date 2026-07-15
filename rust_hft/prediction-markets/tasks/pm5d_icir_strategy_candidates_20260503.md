# PM5D ICIR Strategy Candidates - 2026-05-03

Purpose: keep the strategy-discovery lane centered on IC/ICIR evidence before
promoting any PM5D strategy to dry-run or live.

## Source Artifacts

- BTC/ETH/SOL factor walk-forward: GitHub run `25262768330`
  - Snapshot run `25254380121`
  - Snapshot hash `762ae7751ad08a21`
  - Window `2026-04-21T00:00:00Z -> 2026-05-02T00:00:00Z`
  - Local artifact:
    `/tmp/ploy-factor-wf-25262768330/factor-walk-forward-v2-25262768330/factor-walk-forward-v2/report.txt`
- XRP/DOGE/BNB factor walk-forward: GitHub run `25262865237`
  - Snapshot run `25255158983`
  - Snapshot hash `6e858cf3c0a607f0`
  - Window `2026-04-21T00:00:00Z -> 2026-05-02T00:00:00Z`
  - Local artifact:
    `/tmp/ploy-factor-wf-25262865237/factor-walk-forward-v2-25262865237/factor-walk-forward-v2/report.txt`

Both snapshots are scoped PM5D execution evidence:

- `data_audit_status=critical`
- `include_deribit=false`
- Deribit IV / DVOL is not present in these runs.

Settlement-focused rerun after adding `side_fair_edge`:

- BTC/ETH/SOL settlement gate: GitHub run `25266543999`
  - Snapshot run `25254380121`
  - Head SHA `df29ac69fef9e04c06b0246f2a6cde8f8e22e5da`
  - Local artifact:
    `/tmp/ploy-settlement-gate-25266543999/factor-walk-forward-v2-25266543999/factor-walk-forward-v2/report.txt`
  - Health: `snapshot_data_audit_status=critical`, `source_obs=114008`,
    `v2_rows=228016`, `executable_pnl_rows=50680`, `deribit_rows=0`,
    `entry_fill_rate=22.23%`, `exit_fill_rate=20.97%`.
- XRP/DOGE/BNB settlement gate: GitHub run `25266544004`
  - Snapshot run `25255158983`
  - Head SHA `df29ac69fef9e04c06b0246f2a6cde8f8e22e5da`
  - Local artifact:
    `/tmp/ploy-settlement-gate-25266544004/factor-walk-forward-v2-25266544004/factor-walk-forward-v2/report.txt`
  - Health: `snapshot_data_audit_status=critical`, `source_obs=108296`,
    `v2_rows=216592`, `executable_pnl_rows=15828`, `deribit_rows=0`,
    `entry_fill_rate=7.31%`, `exit_fill_rate=6.50%`.

## Candidate Lanes

### 1. Repricing Momentum

Goal: trade short-horizon Polymarket repricing, not necessarily final outcome.

Primary factor:

- `spread_adjusted_external_move`

Evidence:

- BTC/ETH/SOL, `reprice_pnl_10s`
  - Spearman IC `0.093937`
  - ICIR `0.761164`
  - positive window ratio `0.8204`
  - top bucket avg executable label `0.257310`
- BTC/ETH/SOL, `reprice_pnl_30s`
  - Spearman IC `0.080748`
  - ICIR `0.872932`
  - positive window ratio `0.8389`
  - top bucket avg executable label `0.278328`
- XRP/DOGE/BNB, `reprice_pnl_10s`
  - Spearman IC `0.166884`
  - ICIR `1.544933`
  - positive window ratio `0.9146`
  - top bucket avg executable label `0.384413`
- XRP/DOGE/BNB, `reprice_pnl_30s`
  - Spearman IC `0.149044`
  - ICIR `2.012613`
  - positive window ratio `0.9851`
  - top bucket avg executable label `0.387410`

Status:

- Strongest current IC/ICIR repricing candidate.
- Runtime scorer exists as `repricing_momentum`.
- Snapshot optimize evidence is mixed:
  - BTC/ETH/SOL optimize run `25263475705` passed validation with 124 trades and `+$2032.78`.
  - XRP/DOGE/BNB optimize run `25263476561` stayed positive but failed closed with only 25 validation trades versus `min_trades=80`.

Decision:

- Continue as BTC/ETH/SOL-first candidate.
- Do not promote all-six-symbol dry-run/live.
- Next gate is strict replay/runtime parity for BTC/ETH/SOL.

### 2. Volatility / Tradable Move

Goal: predict whether either side will move enough to create a tradable
repricing opportunity.

Primary current factor:

- `vol_gap`

Evidence on BTC/ETH/SOL:

- `abs_reprice_bid_change_5s`
  - Spearman IC `0.1620`
  - ICIR `1.3805`
  - positive window ratio `0.8333`
  - monotonic buckets: true
  - top bucket avg `0.1318`
- `abs_reprice_bid_change_10s`
  - Spearman IC `0.1621`
  - ICIR `1.3806`
  - positive window ratio `0.8333`
  - monotonic buckets: true
  - top bucket avg `0.1319`
- `abs_reprice_bid_change_30s`
  - Spearman IC `0.1565`
  - ICIR `1.1636`
  - positive window ratio `0.8333`
  - monotonic buckets: true
  - top bucket avg `0.1468`
- `abs_reprice_bid_change_60s`
  - Spearman IC `0.1769`
  - ICIR `1.2560`
  - positive window ratio `0.8333`
  - top bucket avg `0.1862`

Status:

- Good volatility/tradable-move candidate on BTC/ETH/SOL.
- Current run does not include Deribit IV, so this is not yet the full
  Deribit-IV volatility regime strategy.
- XRP/DOGE/BNB report did not show an equally clean alpha-side volatility
  candidate in the top target rows.

Decision:

- Treat as BTC/ETH/SOL volatility trigger candidate.
- It should not buy both YES and NO blindly.
- Runtime shape should be:
  `vol_trigger -> wait for direction confirmation -> buy the repricing side`.

### 3. Settlement / Hold-to-Expiry

Goal: hold when fair probability strongly predicts final settlement and the
entry is executable.

Primary factor:

- `side_fair_prob`

Evidence on BTC/ETH/SOL:

- `settlement_win`
  - Spearman IC `0.5453`
  - ICIR `5.4045`
  - positive window ratio `1.0000`
  - monotonic buckets: true
  - top bucket win rate `0.8906`
- `settlement_executable_pnl`
  - Spearman IC `0.4624`
  - ICIR `3.4101`
  - positive window ratio `1.0000`
  - monotonic buckets: true
  - top bucket avg PnL `1.1738`

Evidence on XRP/DOGE/BNB:

- `settlement_win`
  - Spearman IC `0.5737`
  - ICIR `4.8453`
  - positive window ratio `1.0000`
  - monotonic buckets: true
  - top bucket win rate `0.9040`
- `settlement_executable_pnl`
  - Spearman IC `0.5002`
  - ICIR `3.6109`
  - positive window ratio `1.0000`
  - monotonic buckets: true
  - top bucket avg PnL `0.0403`

Status:

- This is the cleanest high-IC/ICIR settlement lane.
- It is closer to a hold-to-expiry strategy than a 10s/30s repricing strategy.
- It still needs execution/risk filters because wide spreads and poor exits can
  dominate realized results.
- The direct `side_fair_edge = side_fair_prob - entry_ask - fee` hypothesis did
  not survive the settlement executable-PnL gate:
  - BTC/ETH/SOL AutoFactor `settlement_fair_edge -> settlement_executable_pnl`
    was rejected with Spearman IC `-0.156860`, ICIR `-1.441374`, positive
    window ratio `0.0593`, and top bucket avg label `-0.955000`.
  - XRP/DOGE/BNB AutoFactor `settlement_fair_edge -> settlement_executable_pnl`
    was rejected with Spearman IC `-0.042813`, ICIR `-0.226205`, positive
    window ratio `0.3874`, and top bucket avg label `-0.661597`.
- In the same rerun, raw `side_fair_prob` still ranked settlement outcomes and
  executable settlement PnL strongly:
  - BTC/ETH/SOL `side_fair_prob -> settlement_executable_pnl`: Spearman IC
    `0.4624`, ICIR `3.4101`, positive window ratio `1.0000`, top bucket avg
    PnL `1.1738`.
  - XRP/DOGE/BNB `side_fair_prob -> settlement_executable_pnl`: Spearman IC
    `0.5002`, ICIR `3.6109`, positive window ratio `1.0000`, top bucket avg
    PnL only `0.0403`.

Decision:

- Promote to a settlement-gate research candidate, not directly to dry-run.
- Do not promote `side_fair_edge` or a naive
  `side_fair_prob - executable_price` formula.
- Required next gate: BTC/ETH/SOL-only selector/edge matrix around raw
  `side_fair_prob`, time-to-expiry, distance, entry ask, spread, depth, and
  realized executable settlement PnL.

## Current Ranking

1. Settlement hold-to-expiry: `side_fair_prob`
   - Highest and most stable IC/ICIR across both symbol batches.
2. Repricing momentum: `spread_adjusted_external_move`
   - Best executable short-horizon repricing candidate.
3. Volatility trigger: `vol_gap`
   - Good BTC/ETH/SOL tradable-move signal, but needs Deribit-IV enrichment.

## Fastest Path To A Tradable Candidate

The fastest path is not to build the full LOB + Deribit + AutoML stack first.
The fastest path is to promote one narrowly scoped candidate through the next
evidence gate while keeping the other lanes as backups.

Priority 1: settlement gate around `side_fair_prob`.

- Reason: it has the cleanest IC/ICIR evidence and maps directly to
  hold-to-expiry accounting.
- Required score: raw `side_fair_prob` plus explicit execution/risk filters.
- Rejected score: `side_fair_edge = side_fair_prob - entry_ask - fee`.
- Required target: `settlement_executable_pnl`.
- Required filters: time-to-expiry, distance-over-sigma, spread, top-book
  depth, and symbol.
- Promotion condition: positive OOS executable PnL, powered validation trade
  count, drawdown within risk budget, and no single-symbol/day concentration.
- Implementation status: the factor review and AutoFactor walk-forward runner
  now include `side_fair_edge` against `settlement_executable_pnl`, and the
  snapshot-backed rerun rejected that formula. The next step is a BTC/ETH/SOL
  settlement selector that treats price/liquidity as gates and sizing inputs
  rather than subtracting them into the alpha score.

Priority 2: BTC/ETH/SOL repricing momentum with `spread_adjusted_external_move`.

- Reason: it already has a runtime/replay scorer and BTC/ETH/SOL optimize
  evidence, so it is closest to a replay-parity handoff.
- Required next gate: strict replay/runtime parity on BTC/ETH/SOL only.
- Promotion condition: same scorer/config/MarketUpdate sequence, positive
  after-cost replay PnL, fill/exit evidence, and acceptable latency/slippage
  caveats.

Priority 3: volatility trigger with `vol_gap`.

- Reason: `vol_gap` has good IC/ICIR against tradable-move labels, but it is a
  trigger, not a side selector.
- Required action model: `vol_trigger -> direction confirmation -> buy side`.
- Required target: `tradable_move` and side-specific future bid reprice after
  direction confirmation.
- Promotion condition: the trigger improves opportunity selection without
  buying both YES and NO, and survives near-strike/liquidity buckets.

Decision rule:

- If the BTC/ETH/SOL `side_fair_prob` selector passes first, build the first
  small fixed-size dry-run candidate as hold-to-expiry only.
- If settlement is too sparse or drawdown-heavy, promote BTC/ETH/SOL repricing
  momentum to strict replay parity.
- If both fail, keep `vol_gap` as a pre-trade trigger and do not promote it
  until direction confirmation has its own executable IC/PnL evidence.

## Next Engineering Gates

1. Add or run a BTC/ETH/SOL settlement selector gate:
   - score: raw `side_fair_prob`
   - reject: naive `side_fair_edge = side_fair_prob - entry_ask - fee`
   - target: `settlement_executable_pnl`
   - regimes: time-to-expiry, distance-over-sigma, entry ask, spread,
     liquidity bucket, symbol.
2. Add or run a volatility-trigger gate:
   - score: `vol_gap` plus near-strike and liquidity state
   - target: `abs_reprice_bid_change_10s/30s` or tradable move
   - action: direction confirmation before entry.
3. Keep repricing momentum as BTC/ETH/SOL-first:
   - run replay/runtime parity before dry-run.

## Do Not Promote Yet

- No live trading.
- No all-six-symbol dry-run.
- No direct volatility straddle.
- No strategy handoff unless replay/runtime uses the same scorer and reports
  fill, exit, latency, slippage, adverse selection, drawdown, and missed
  opportunity.
