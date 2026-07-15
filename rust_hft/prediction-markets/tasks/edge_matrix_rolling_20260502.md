# PM5D Rolling Edge Matrix Diagnostic

Generated from current-main strategy matrix runs over snapshot `25204438461`
after the replay/dry-run parity matcher fix.

## Inputs

- Snapshot hash: `fb338e1f202c3bda`
- Symbols: `BTCUSDT,ETHUSDT,SOLUSDT,XRPUSDT,DOGEUSDT,BNBUSDT`
- Diagnostic min trades: `20`
- Strict deployability floor used by the analysis report: `80`

## Runs

| Run | Validation Window | Status | Run-Threshold Candidates | Strict Candidates | Top Row | Trades | PnL | Main Blocker |
| --- | --- | --- | ---: | ---: | --- | ---: | ---: | --- |
| `25242614746` | `2026-04-27` | success | 45 | 0 | `inverted_entry_only_pm_none_wide_ev0.05` | 77 | 3144.84 | sample power below 80 |
| `25242615468` | `2026-04-28` | success | 9 | 0 | `inverted_entry_only_pm_none_wide_ev0.05` | 27 | 844.04 | sample power below 80 |
| `25242616093` | `2026-04-29` | success | 5 | 0 | `inverted_entry_only_pm_none_wide_ev0.05` | 26 | 291.30 | sample power below 80 |
| `25242616810` | `2026-04-30` | success | 2 | 0 | `inverted_entry_only_pm_none_middle_ev0.05` | 21 | 425.35 | sample power below 80 |
| `25242617585` | `2026-05-01` | fail-closed | 0 | 0 | `inverted_entry_only_pm_none_wide_ev0.05` | 11 | -50.26 | sample power, negative PnL, calibration, day/symbol stability |

## Interpretation

- The first four single-day validation windows support the inverted-direction
  hypothesis as a research lane, especially with no PM-dynamics hard filter and
  entry-only executable fillability.
- The last validation day fails closed and is negative, so this is not stable
  enough to restore dry-run.
- Even the strongest one-day run reaches only `77` trades, below the strict
  `80`-trade floor. The current evidence is diagnostic, not deployable.
- The next useful research step is not to tune the old model selector. It is to
  build a narrower inverted/regime-calibration candidate and then test it on a
  longer or fresher snapshot with the strict `80`-trade gate restored.

## Decision

`diagnostic-only-continue-research`

No PM5D dry-run or live strategy should be restored from these results.
