# BTCUSDT USD-M Real E2E Research Run — 2026-07-17

## Outcome

The real-data mission executed successfully through point-in-time
materialization and purged walk-forward evaluation. It did **not** satisfy the
platform E2E acceptance contract.

The single fixed baseline failed walk-forward. No candidate was kept, so the
300-row sealed holdout remained unopened and no Paper or Shadow handoff was
created. This is a research rejection, not a collector, download, build, or
runner failure.

| Gate | Result |
| --- | --- |
| Real raw manifest and checksums | Pass |
| Fixed content-addressed snapshot ID | Pass |
| Reproducible mission | Pass |
| Walk-forward metrics | Pass; candidate rejected |
| Separate in-sample metrics | Missing from the current bundle |
| Sealed holdout metrics | Correctly absent because walk-forward failed |
| Cost, latency, and slippage assumptions | Recorded below |
| Final evidence bundle | Pass; published and read back from OSS |
| Paper/Shadow output | Absent; no candidate reached handoff eligibility |
| Live execution remains closed | Pass |

## Execution identity

| Evidence | Value |
| --- | --- |
| Repository source revision | `6e3607ec11ed5470eb265480c06b0964f0653b21` |
| Data mission | `data-btcusdt-usdm-20260716t1036z-1300z-6e3607ec` |
| Research mission | `alpha-btcusdt-usdm-mcts-20260716t1036z-1300z-6e3607ec` |
| Engine / seed | MCTS / `7` |
| Runner mode | Local release binaries over downloaded real OSS objects |
| Result publication | Content-addressed OSS object with overwrite forbidden |
| Mission status | `BudgetExhausted` after one candidate and one expansion |

This run did not execute as an ACK Job. The local run used the same
`alpha-harness mission execute` contract as the ACK example Job, and the
resulting artifacts were published to and read back from the research OSS
bucket. Local execution is sufficient to prove the data/evaluator path; it is
not evidence that the ACK worker/image path is currently deployed.

## Fixed real raw data

The initially selected 12:00 UTC object was not independently replayable: it
contained diffs and a closing checkpoint but no opening BTCUSDT snapshot. The
materializer correctly rejected it with `diffs arrived before replay seed`.
The selection was expanded backward, before any strategy result was observed,
to the nearest session-start segment and every contiguous segment through the
original 12:00 hour. Ignoring the pre-seed diffs would have silently corrupted
the book and was not allowed.

All objects are under:

`oss://monday-lob-apne1-1045353359/lake/raw/venue=binance/market=usdm/dataset=usdm_perpetual_all/shard=all/date=2026-07-16/`

| Hour / object | Bytes | Events | Data SHA-256 | Collector manifest SHA-256 | `_SUCCESS` SHA-256 |
| --- | ---: | ---: | --- | --- | --- |
| `hour=10/part-1784198173618670109.jsonl.zst` | 233,729,218 | 2,457,614 | `c4b1fad5f5aeb74be075e6e9b7d16dbdadb18b38402634dbcfe266b2c0325857` | `9c4f11e6b57f0be0d61020a4c5dfba752d43bc9607319f3d14989be8d771145c` | `37a4f6903ea13ad3f8d85c5496ddbfb290f24655f6f01b16860da03846ee9dae` |
| `hour=11/part-1784199618466918109.jsonl.zst` | 641,146,276 | 6,700,493 | `c833b4783ffcb5fd354f3fa1442f7ad23f040989128af51461ddf7cd0bf5ec1b` | `ec7b90397d7cd4febf0c8ed906e668f49f1df75ae86f36f50c2e5d1738d7b1ad` | `6ec70a65b434496f327676614c16fb5b4657710a4cdef5c11120b6e063e0fa43` |
| `hour=12/part-1784203249742668574.jsonl.zst` | 680,561,693 | 6,744,792 | `4ee0791c0733fc68e637f10de6312dfcfe533a04160001e4e16557897d1f3af6` | `9429792cb3539e2db4087b7bc6d2e0dd7a99534bf25ba76455e04dc21c0954fb` | `800dc435c19df1fc450a817ad437e561312dd87e7124ce5e5481e5f20b4dcb67` |

The three collector manifests account for 15,902,899 events. The seed segment
contains one `session_start`, 573 opening snapshots, 2,456,467 diffs, and 573
closing checkpoints. The following two segments preserve the same replay
sequence and each closes with 573 checkpoints. None declares a sequence gap.

## Immutable snapshot evidence

| Evidence | Value |
| --- | --- |
| Rows | 8,596 one-second BTCUSDT rows |
| Event-time bounds | `2026-07-16T10:36:41Z` to `2026-07-16T12:59:56Z` |
| Label availability bounds | `2026-07-16T10:36:46Z` to `2026-07-16T13:00:01Z` |
| Label horizon | 5 buckets / 5 seconds |
| Source revision | `3a4e97921374380a752f01ce398232a2dfac10e414e3387150a908e8fdbbffe4` |
| Feature artifact SHA-256 | `5586787378f18e86efa035768f5d32b7040bf8519aba55c87c1f8090b943bbd5` |
| Snapshot ID | `dataset-5586787378f18e86efa035768f5d32b7040bf8519aba55c87c1f8090b943bbd5` |
| Materialization SHA-256 | `541c1db8d839477a4ae530d96374f4ffe7a05a66c6bafbb619e8613ea38de760` |

Published objects:

- `oss://monday-lob-apne1-1045353359/lake/derived/venue=binance/market=usdm/dataset=lob_pit/symbol=BTCUSDT/snapshot=dataset-5586787378f18e86efa035768f5d32b7040bf8519aba55c87c1f8090b943bbd5/features-5586787378f18e86efa035768f5d32b7040bf8519aba55c87c1f8090b943bbd5.jsonl`
- `oss://monday-lob-apne1-1045353359/lake/derived/venue=binance/market=usdm/dataset=lob_pit/symbol=BTCUSDT/snapshot=dataset-5586787378f18e86efa035768f5d32b7040bf8519aba55c87c1f8090b943bbd5/materialization-541c1db8d839477a4ae530d96374f4ffe7a05a66c6bafbb619e8613ea38de760.json`

Both were written with `forbid-overwrite`, downloaded again, and compared
byte-for-byte. Their read-back SHA-256 values match the table above.

“Immutable” here is an application-level guarantee: content-addressed names,
no-clobber publication, and read-back verification. Bucket versioning and WORM
are not enabled, so this run does not claim storage-enforced immutability.

## Baseline and validation contract

The fixed baseline was the first live-compatible MCTS proposal:

`(book_imbalance + book_imbalance)`

It is a scaled top-of-book imbalance signal. The one-candidate budget makes the
run a baseline evaluation rather than a search over results. The current
live-only MCTS expansion surface only adds its secondary live field; simply
raising the same search budget would produce scaled variants and would not
address this rejection.

| Protocol setting | Value |
| --- | ---: |
| Initial train rows | 800 |
| Validation rows per fold | 240 |
| Folds | 3 |
| Purge / embargo | 5 / 5 rows |
| Sealed holdout | 300 rows |
| Fee | 2.0 bps |
| Funding | 0.0 bps |
| Latency haircut | 0.5 bps |
| Additional slippage | 0.0 bps; pinned revision `6e3607ec` exposed no separate field |
| Minimum trades per fold | 30 |
| Minimum mean net return per fold | 0.000001 |
| Minimum adjusted score | 2.0 |

At the pinned `6e3607ec` revision used for this run, the evaluator did not model
queue position, partial fills, market impact, capacity, or an explicit
spread-crossing/slippage term. Later revisions add explicit slippage and depth
capacity controls; they do not retroactively change this immutable result. The
result is therefore not deployment or Paper-readiness evidence, even if a later
candidate passes statistical gates.

## Walk-forward result

| Fold | Rows | Trades | Mean net return | Cumulative net return | Net Sharpe | Max drawdown |
| ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| 1 | 240 | 17 | -0.0000291472 | -0.00699532 | -0.228293 | 0.00732135 |
| 2 | 240 | 12 | -0.0000194372 | -0.00466494 | -0.165936 | 0.00466494 |
| 3 | 240 | 14 | 0.0000109843 | 0.00263623 | 0.084566 | 0.00235495 |

Aggregate validation metrics:

| Metric | Value |
| --- | ---: |
| Validation rows | 720 |
| Trades | 43 |
| Mean net return | -0.0000125334 |
| Cumulative net return | -0.00902403 |
| Net Sharpe | -0.103221 |
| Maximum drawdown | 0.00732135 |
| Time-series IC / RankIC | 0.333996 / 0.486661 |
| ICIR / RankICIR | 2.68256 / 4.59049 |
| Positive-IC fold ratio | 1.0 |
| Multiple-testing-adjusted score | -1.59909 |

The candidate failed because all three folds were below 30 trades, folds 1 and
2 did not establish positive cost-adjusted edge, and the adjusted score was
below 2.0. Positive predictive correlation did not translate into a passing
trading result after the configured costs.

The current mission bundle does not emit a separate training/in-sample metric
record. `kept-candidates.txt` and `sealed-evaluations.jsonl` are both empty, so
there are no holdout metrics. The holdout was not manually evaluated or
inspected after the rejection.

## Evidence bundle

| Evidence | Value |
| --- | --- |
| OSS object | `oss://monday-lob-apne1-1045353359/artifacts/alpha-results/mission=alpha-btcusdt-usdm-mcts-20260716t1036z-1300z-6e3607ec/results-396ad2678625a7ef4c48ecd2072a0a88308b68c84420bd236757cb48fa098ad5.zip` |
| Bundle SHA-256 | `396ad2678625a7ef4c48ecd2072a0a88308b68c84420bd236757cb48fa098ad5` |
| Bundle size | 583,917 bytes |
| Structure | 13 entries; ZIP integrity passed |
| Remote verification | Downloaded from OSS; byte comparison and SHA-256 passed |

The bundle contains the PIT feature artifact, materialization lineage,
feature manifest, DuckDB store and integrity key, mission/create/run/status
records, candidate evaluation, kept-candidate list, and sealed-evaluation file.

## Paper, Shadow, and Live boundary

No Paper or Shadow output exists for this mission. That is the expected
consequence of having no kept candidate and no sealed-holdout result; a runtime
handoff was neither constructed nor activated.

Focused verification on the source commit passed:

- `alpha-harness` exposes no order or trade command.
- The Binance-enabled runtime test proves an accepted Paper/Shadow handoff can
  reach both runtime adapters, but this mission did not produce such a handoff.
- The Shadow runtime test waits for real market input before producing
  loop-consumable evidence.
- LiveSmall Polymarket formula activation remains fail-closed.

Live execution was not enabled, no exchange credential was used, and no order
was submitted.

## Reproduction contract

After downloading each listed data object with its `.manifest.json` and
`._SUCCESS` sibling, the materialization command is:

```bash
rust_hft/target/release/lob-pit-materializer \
  --mission-id data-btcusdt-usdm-20260716t1036z-1300z-6e3607ec \
  --symbol BTCUSDT \
  --market usdm \
  --bucket-ms 1000 \
  --label-horizon-buckets 5 \
  --top-depth 5 \
  --segment part-1784198173618670109.jsonl.zst \
  --segment part-1784199618466918109.jsonl.zst \
  --segment part-1784203249742668574.jsonl.zst \
  --artifact-dir pit
```

The research command is:

```bash
rust_hft/target/release/alpha-harness mission execute \
  --work-dir work \
  --feature-url pit/5586787378f18e86efa035768f5d32b7040bf8519aba55c87c1f8090b943bbd5.jsonl \
  --materialization-url pit/541c1db8d839477a4ae530d96374f4ffe7a05a66c6bafbb619e8613ea38de760.materialization.json \
  --materialization-sha256 541c1db8d839477a4ae530d96374f4ffe7a05a66c6bafbb619e8613ea38de760 \
  --result-put-url published/results.zip \
  --data-mission-id data-btcusdt-usdm-20260716t1036z-1300z-6e3607ec \
  --mission-id alpha-btcusdt-usdm-mcts-20260716t1036z-1300z-6e3607ec \
  --engine mcts \
  --seed 7 \
  --feature-fields book_imbalance \
  --max-candidates 1 \
  --max-expansions 1 \
  --max-seconds 300 \
  --max-new-iterations 1 \
  --objective "Find a cost-aware, out-of-sample LOB factor for BTCUSDT USD-M" \
  --hypothesis-scope "Top-of-book imbalance at a five-second horizon" \
  --initial-train-rows 800 \
  --validation-rows 240 \
  --fold-count 3 \
  --purge-rows 5 \
  --embargo-rows 5 \
  --sealed-holdout-rows 300 \
  --fee-bps 2.0 \
  --funding-bps 0.0 \
  --latency-bps 0.5 \
  --label-horizon-buckets 5 \
  --observation-frequency-millis 1000
```

The maintained baseline is also encoded in
`deployment/aliyun/research/k8s/alpha-mission-job.example.yaml`. Its current
cost and capacity controls postdate this run; use the pinned command above to
reproduce the immutable result.

## Decision and next gate

The Crypto raw-data-to-evaluator path is real and reproducible, but the research
platform is not yet E2E-complete under the stated contract. The next code or
research change must improve the live-compatible candidate/position mapping and
add a separately reported training metric if that remains an acceptance field.
It must then run a new mission with fresh chronological evidence; the current
thresholds and sealed-holdout rule must not be relaxed in response to this run.

Polymarket remains out of scope until its reference collector produces a fresh,
passing health record. CPU quota investigation is also out of scope.
