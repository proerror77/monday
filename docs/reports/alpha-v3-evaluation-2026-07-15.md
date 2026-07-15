# ACK Formula v3 Evaluation — 2026-07-15

## Outcome

The ACK research path now has a verified Formula v3 run. The successful Job
used the native `alpha-harness mission execute` entrypoint and produced eight
`purged-walk-forward-v3` evaluations. No candidate passed walk-forward, so the
sealed-holdout file is correctly empty.

The first attempt, `monday-alpha-bayes-btc-v3-20260715-00`, proved the immutable
v3 image could be pulled but failed before evaluation because the private Spot
worker could not reach a public OSS endpoint. The retry used the regional OSS
internal endpoint and completed successfully.

## Runtime and artifact evidence

| Evidence | Value |
| --- | --- |
| Successful Job | `monday-alpha-bayes-btc-v3-20260715-01` |
| Started / completed | `2026-07-15T00:10:20Z` / `2026-07-15T00:10:23Z` |
| Image source revision | `96cafd7aeb896f3514f9c51055d57c6322361681` |
| Image digest | `sha256:bdcab7ba9277d8111709972e49dd2d91ccaa24a83a209b2b073e8f883ee2d0d9` |
| Result object | `oss://monday-lob-apne1-1045353359/artifacts/alpha-results/96cafd7a/bayesian-btcusdt-usdm-v3-20260715-01/results.zip` |
| Bundle SHA-256 | `deb0b2481f4c2b7252cd5578b28e140c41eb42d3e70e165e6b2e24aad2148c34` |
| Bundle structure | 13 files; ZIP integrity passed |
| Evaluations | 8 walk-forward v3; 0 passed; 0 sealed |

The materialization manifest SHA-256 was independently verified as
`c63aa9b515f15ee0108b124212a13441ef106b2a8daf15a846acd94a61c2eb23`
before launch. Validation used three 240-row folds with five-row purge and
embargo windows, a 300-row sealed holdout, 2.0 bps fees, and 0.5 bps latency.

## Real walk-forward metrics

Candidates are ordered by adjusted score. Sharpe is the evaluator's
per-observation net Sharpe and is deliberately not annualized.

| Candidate | IC | RankIC | ICIR | RankICIR | Positive IC | Net Sharpe | Mean net return | Adjusted score | Trades |
| --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| `bayes-5` | 0.1828 | 0.1979 | 1.1918 | 1.1703 | 0.667 | -0.0094 | -0.00000291 | -2.1852 | 29 |
| `bayes-6` | 0.1635 | 0.1710 | 1.0184 | 0.9570 | 0.667 | -0.0328 | -0.00000565 | -2.5479 | 29 |
| `bayes-7` | 0.1957 | 0.2225 | 1.5034 | 1.6135 | 1.000 | -0.0411 | -0.00000654 | -2.6761 | 39 |
| `bayes-4` | 0.0290 | 0.0415 | 0.2013 | 0.2937 | 0.667 | -0.0514 | -0.00000745 | -2.8362 | 21 |
| `bayes-3` | 0.0611 | 0.0578 | 0.4059 | 0.3683 | 0.667 | -0.0606 | -0.00000858 | -2.9787 | 23 |
| `bayes-2` | 0.0372 | 0.0497 | 0.5352 | 0.5189 | 0.667 | -0.0919 | -0.00001299 | -3.4634 | 21 |
| `bayes-8` | 0.0487 | 0.0371 | 0.2499 | 0.1844 | 0.667 | -0.0944 | -0.00001270 | -3.5021 | 26 |
| `bayes-1` | 0.3259 | 0.3614 | 2.1683 | 2.1322 | 1.000 | -0.2100 | -0.00004499 | -5.2922 | 112 |

Failure evidence contained 3 ICIR failures, 3 RankICIR failures, 21 per-fold
minimum-trade failures, 16 per-fold net-edge failures, and 8 adjusted-score
failures. All eight candidates had negative aggregate net return and negative
adjusted score. The highest-IC candidate was also the worst after costs.

## Threshold calibration

No governed threshold is relaxed or tightened from this single run:

| Gate | Current value | Decision |
| --- | ---: | --- |
| Minimum IC / RankIC | 0.01 / 0.01 | Keep |
| Minimum ICIR / RankICIR | 0.5 / 0.5 | Keep |
| Minimum positive-IC ratio | 0.6 | Keep |
| Minimum trades per fold | 30 | Keep |
| Minimum mean net return per fold | 0.000001 | Keep |
| Minimum adjusted score | 2.0 | Keep |
| Maximum drawdown | 0.20 | Keep |

This is a calibration result, not a lack of calibration: relaxing prediction or
trade-count gates would not create positive cost-adjusted edge, while choosing
new thresholds after observing this evaluation run would add selection bias.
The next research change should improve the candidate/search and position
mapping, then run a new mission with fresh out-of-sample evidence. Sharpe remains
an evidence field rather than a gate until dataset frequency and serial-correlation
treatment are part of the evaluation contract.
