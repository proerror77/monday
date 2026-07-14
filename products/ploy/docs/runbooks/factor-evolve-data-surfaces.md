# FactorEvolve Data Surface Gates

This runbook defines the data-surface contract for FactorEvolve research. It is
an evidence gate, not a collector implementation plan. Missing required
surfaces must be reported as caveats or blockers before a factor can move from
diagnostic search toward executable replay or dry-run handoff.

## Surface Taxonomy

- `required_for_prediction`: needed to make the stated alpha hypothesis
  testable without substituting market-implied information for prediction.
- `required_for_execution`: needed to price entry, sweep depth, slippage,
  capacity, and fillability conservatively.
- `optional_context`: useful for analysis or future model families, but not
  required for the current hypothesis.
- `missing_blocks_promotion`: absence blocks promotion beyond diagnostics until
  the surface is present or the hypothesis is explicitly rewritten to not need
  it.

## Current Surfaces

| Surface | Current status | Taxonomy | Promotion rule |
| --- | --- | --- | --- |
| Binance aggTrade / spot ticks | Present | `required_for_prediction` | Required for crypto event movement features. |
| Binance partial LOB snapshots | Present | `required_for_prediction` | Diagnostic only; not queue or passive-fill proof. |
| Binance futures local book | Contract added; collector replay not first-class | `required_for_prediction` | `missing_blocks_promotion` for hypotheses that depend on sequence-correct depth imbalance. |
| Polymarket quote ticks | Present | `required_for_execution` | Required for top-of-book state and quote freshness. |
| Polymarket full CLOB snapshots | Present and archived | `required_for_execution` | `missing_blocks_promotion` for executable-price or capacity claims. |
| Official settlement | Present for PM5D evidence | `required_for_prediction` | `missing_blocks_promotion` for settlement-probability strategies. |
| Dry-run/runtime fills | Present as runtime evidence | `required_for_execution` after dry-run | Required for recorded replay/dry-run parity, not a substitute for settlement labels. |
| Binance futures OI | Not first-class | `optional_context` | Blocks only hypotheses that explicitly depend on open-interest pressure. |
| Binance futures funding | Not first-class | `optional_context` | Blocks only funding/basis carry hypotheses. |
| Binance liquidation stream | Not first-class | `optional_context` | Blocks liquidation-shock hypotheses. |
| Binance basis / perp mark data | Not first-class | `optional_context` | Blocks basis or mark-price dislocation hypotheses. |
| OKX / Bybit LOB | Not first-class | `optional_context` | Blocks cross-exchange confirmation or venue-divergence hypotheses. |

## Fail-Closed Rules

1. A factor report must list every surface used and every surface required by
   the hypothesis but missing from the run.
2. If a required surface is missing, set `missing_blocks_promotion` and keep the
   decision at `continue`, `revise`, or `reject`.
3. Partial LOB snapshots can support diagnostics, but only a sequence-correct
   local book can support claims that depend on update ordering, queue pressure,
   or passive-fill feasibility.
4. Polymarket top-of-book quotes do not prove executable capacity. Promotion
   needs full CLOB depth, conservative sweep pricing, and fill-rate assumptions.
5. Runtime dry-run fills prove observed execution behavior only after a
   candidate exists. They do not replace historical executable replay or
   official settlement labels.

## Research Issue Checklist

Each FactorEvolve research issue should state:

- hypothesis and strategy lane
- `required_for_prediction` surfaces
- `required_for_execution` surfaces
- `optional_context` surfaces intentionally excluded
- `missing_blocks_promotion` surfaces and why they block or do not block this
  specific stage
- next evidence stage and promotion decision
