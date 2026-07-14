# PM5D Factor Stability Probe

Generated from snapshot-backed Factor Walk-Forward V2 run `25243415961`.

## Question

Can the current candidate factors prove stable profitability from train into
validation before any PM5D dry-run restoration?

## Inputs

- Workflow: `factor-walk-forward-v2.yml`
- Run: `25243415961`
- Source: `snapshot`
- Snapshot run: `25204438461`
- Snapshot hash: `fb338e1f202c3bda`
- Snapshot window: `2026-04-24T00:00:00Z -> 2026-05-02T00:00:00Z`
- Train window: `2026-04-24 -> 2026-04-29`
- Validation window: `2026-04-29 -> 2026-05-02`
- Symbols: `BTCUSDT,ETHUSDT,SOLUSDT,XRPUSDT,DOGEUSDT,BNBUSDT`
- Stake: `15`
- Train/test shape: `train_window_days=5`, `test_window_days=3`, `step_days=1`
- Minimum observations: `80`
- Factor filter:
  `side_model_prob,side_distance_over_sigma,cex_continuation_edge_gate,exit_bid_change_30s,entry_ask_change_30s,pm_reprice_speed_30s,obi_persistence_30s_side,cex_continuation_score_side`

## Data Health

- Source observations: `129839`
- V2 rows: `259678`
- Executable PnL rows: `50371`
- Entry fill rate: `19.40%`
- Exit fill rate: `17.97%`
- Full-depth entry fill rate: `46.84%`
- Full-depth exit fill rate: `37.43%`
- Snapshot data audit status: `critical`

## Raw Walk-Forward Read

Before applying the liquidity gate, the strongest validation PnL factors were:

| Factor | Validation PnL | Positive Window Ratio | Avg Fill Rate | Symbol Positive |
| --- | ---: | ---: | ---: | ---: |
| `exit_bid_change_30s` | `8914.4190` | `1.0000` | `0.6145` | `0.6667` |
| `cex_continuation_edge_gate` | `4416.7277` | `1.0000` | `0.6793` | `0.5000` |
| `entry_ask_change_30s` | `2390.5643` | `1.0000` | `0.7229` | `0.3333` |
| `pm_reprice_speed_30s` | `2390.5643` | `1.0000` | `0.7229` | `0.3333` |

The raw alpha-side factors were negative on validation:

| Factor | Direction | Train PnL | Validation PnL | Symbol Positive |
| --- | ---: | ---: | ---: | ---: |
| `side_model_prob` | `-1` | `88694.4939` | `-4832.1955` | `0.3333` |
| `side_distance_over_sigma` | `-1` | `88667.0470` | `-4867.1008` | `0.3333` |

This means raw single-factor results should not be promoted directly. They are
sensitive to execution and liquidity context.

## Liquidity-Gated Read

After applying the liquidity gate, the same train-to-validation probe changes
materially:

| Factor | Decision | Validation PnL | Positive Window Ratio | Fill Rate | Symbol Positive | Time Bucket Positive |
| --- | --- | ---: | ---: | ---: | ---: | ---: |
| `side_distance_over_sigma` | `watchlist` | `5997.9890` | `1.0000` | `1.0000` | `1.0000` | `1.0000` |
| `side_model_prob` | `watchlist` | `5990.4034` | `1.0000` | `1.0000` | `1.0000` | `1.0000` |
| `cex_continuation_edge_gate` | `watchlist` | `353.9439` | `1.0000` | `1.0000` | `0.5000` | `0.7500` |
| `entry_ask_change_30s` | `watchlist` | `58.3147` | `1.0000` | `1.0000` | `0.6667` | `0.5000` |
| `pm_reprice_speed_30s` | `watchlist` | `58.3147` | `1.0000` | `1.0000` | `0.6667` | `0.5000` |
| `exit_bid_change_30s` | `reject` | `-39.6352` | `0.0000` | `1.0000` | `0.5000` | `0.5000` |
| `cex_continuation_score_side` | `reject` | `-416.6829` | `0.0000` | `1.0000` | `0.1667` | `0.5000` |
| `obi_persistence_30s_side` | `reject` | `-1349.0392` | `0.0000` | `1.0000` | `0.1667` | `0.0000` |

All positive rows are still `watchlist`, not `candidate`, because the run has
only one train-to-validation window. The workflow reason is
`too_few_windows_positive_pnl`.

## Interpretation

- The `80` gate is not per-symbol. It is a cross-symbol sample-power floor for
  the selected factor/hypothesis in a split.
- Per-symbol stability is checked separately through symbol-positive rates.
- Raw factor profitability is not stable enough to trust by itself.
- Liquidity-gated alpha factors are much more promising in this exact
  `5d train -> 3d validation` probe: `side_model_prob` and
  `side_distance_over_sigma` become strongly positive with `100%` fill,
  symbol-positive, and time-bucket-positive rates.
- This still does not authorize dry-run restoration because there is only one
  validation window and the source snapshot data audit remains `critical`.

## Decision

`continue-factor-stability-research`

Do not restore PM5D dry-run/live from this evidence. The next useful step is a
multi-window liquidity-gated factor stability run on longer or fresher snapshot
coverage, then a strict matrix/runtime-parity check if the same factors remain
positive across windows.

## Multi-Window Extension

Generated from snapshot-backed Factor Walk-Forward V2 run `25244553632`.
The first attempt, run `25244527617`, failed because `end_date=2026-05-02`
expanded to a requested window ending `2026-05-03T00:00:00Z`, outside the
snapshot end `2026-05-02T00:00:00Z`. That was an input boundary error, not a
strategy failure. The successful rerun used `end_date=2026-05-01`.

### Inputs

- Workflow: `factor-walk-forward-v2.yml`
- Run: `25244553632`
- Source: `snapshot`
- Snapshot run: `25204438461`
- Snapshot hash: `fb338e1f202c3bda`
- Snapshot window: `2026-04-24T00:00:00Z -> 2026-05-02T00:00:00Z`
- Rolling shape: `train_window_days=3`, `test_window_days=1`,
  `step_days=1`
- Windows: `5`
- Symbols: `BTCUSDT,ETHUSDT,SOLUSDT,XRPUSDT,DOGEUSDT,BNBUSDT`
- Stake: `15`
- Minimum observations: `80`
- Factor filter:
  `side_model_prob,side_distance_over_sigma,cex_continuation_edge_gate,exit_bid_change_30s,entry_ask_change_30s,pm_reprice_speed_30s,obi_persistence_30s_side,cex_continuation_score_side`

### Multi-Window Data Health

- Source observations: `129839`
- V2 rows: `259678`
- Executable PnL rows: `50371`
- Entry fill rate: `19.40%`
- Exit fill rate: `17.97%`
- Full-depth entry fill rate: `46.84%`
- Full-depth exit fill rate: `37.43%`
- Liquidity-gated rows: `22405`
- Liquidity-gate coverage: `8.63%`
- Liquidity-gate entry fill: `100.00%`
- Liquidity-gate roundtrip fill: `100.00%`
- Liquidity-gate rejection rate: `0.00%`
- Snapshot data audit status: `critical`

### Raw Rolling Read

Before the liquidity gate, the raw rolling read still had mixed stability:

| Factor | Windows | Positive Window Ratio | Total Test PnL | Min Window PnL | Avg Fill Rate |
| --- | ---: | ---: | ---: | ---: | ---: |
| `side_distance_over_sigma` | `5` | `0.8000` | `21195.4296` | `-9086.3107` | `0.5784` |
| `side_model_prob` | `5` | `0.8000` | `21111.2773` | `-9086.3107` | `0.5782` |
| `exit_bid_change_30s` | `5` | `1.0000` | `17131.4568` | `776.8428` | `0.6518` |
| `cex_continuation_edge_gate` | `5` | `1.0000` | `13967.0788` | `87.2273` | `0.7217` |
| `entry_ask_change_30s` | `5` | `1.0000` | `8254.6869` | `174.7645` | `0.7606` |
| `pm_reprice_speed_30s` | `5` | `1.0000` | `8254.6869` | `174.7645` | `0.7606` |
| `cex_continuation_score_side` | `5` | `0.0000` | `-12782.0402` | `-6391.3263` | `0.5398` |
| `obi_persistence_30s_side` | `5` | `0.0000` | `-103946.1858` | `-33786.1913` | `0.5827` |

The raw alpha-side factors were positive in aggregate but failed one of five
out-of-sample windows badly. That keeps the raw read below deployment quality.

### Liquidity-Gated Rolling Read

After the liquidity gate, the alpha-side factors became materially stronger
across all five out-of-sample windows:

| Factor | Decision | Windows | Positive Window Ratio | Total Test PnL | Min Window PnL | Fill Rate | Symbol Positive | Time Bucket Positive |
| --- | --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| `side_distance_over_sigma` | `watchlist` | `5` | `1.0000` | `21192.4930` | `875.4386` | `1.0000` | `1.0000` | `0.9500` |
| `side_model_prob` | `watchlist` | `5` | `1.0000` | `21192.4930` | `875.4386` | `1.0000` | `1.0000` | `0.9500` |
| `cex_continuation_edge_gate` | `watchlist` | `5` | `1.0000` | `1249.3031` | `18.5976` | `1.0000` | `0.7333` | `0.8000` |
| `exit_bid_change_30s` | `reject` | `5` | `0.4000` | `-6.3493` | `-126.4560` | `1.0000` | `0.5000` | `0.5000` |
| `entry_ask_change_30s` | `reject` | `5` | `0.6000` | `-32.1887` | `-107.5979` | `1.0000` | `0.5333` | `0.3833` |
| `pm_reprice_speed_30s` | `reject` | `5` | `0.6000` | `-32.1887` | `-107.5979` | `1.0000` | `0.5333` | `0.3833` |
| `cex_continuation_score_side` | `reject` | `5` | `0.2000` | `-104.5613` | `-395.8728` | `1.0000` | `0.4333` | `0.2500` |
| `obi_persistence_30s_side` | `reject` | `5` | `0.0000` | `-4153.5975` | `-1651.4985` | `1.0000` | `0.3000` | `0.3500` |

### Updated Interpretation

- Liquidity-gated `side_model_prob` and `side_distance_over_sigma` are now the
  strongest factor-stability lane on this snapshot: all five out-of-sample
  windows are positive, minimum window PnL is positive, fill is `100%`,
  symbol-positive is `100%`, and time-bucket-positive is `95%`.
- `cex_continuation_edge_gate` is directionally useful but much smaller and
  less uniform by symbol/time bucket.
- PM dynamics (`exit_bid_change_30s`, `entry_ask_change_30s`,
  `pm_reprice_speed_30s`) do not survive the liquidity-gated rolling check as
  independent deployment factors.
- OBI persistence and continuation score remain rejected in this shape.
- The evidence is stronger than the one-window probe, but still not
  dry-run-ready: the workflow decision remains `watchlist` because five windows
  are still below the promotion gate (`too_few_windows_positive_pnl`), and the
  snapshot data audit remains `critical`.

### Updated Decision

`continue-factor-stability-research`

Do not restore PM5D dry-run/live from this evidence. The next useful step is to
produce longer or fresher rolling evidence, or to run a strict runtime-parity
matrix over the liquidity-gated alpha lane, before any dry-run handoff.

## Long-Window Batch Extension

The six-symbol long-window snapshot attempt, run `25253729919`, failed as an
infrastructure/data-size blocker rather than strategy evidence. It requested
`2026-04-21 -> 2026-05-02` over all six symbols at 30s LOB/sample settings. The
snapshot compiler was killed with exit code `137` after logging
`lob snapshot rows: 196375`, which indicates the current `ploy-ci-1` memory
envelope cannot materialize that shape as one snapshot.

The runner service was restarted through Aliyun Cloud Assistant after the OOM
left `actions.runner.proerror77-ploy.ploy-ci-1.service` failed. No Rust build
was run manually on the host.

### BTC/ETH/SOL Batch

Generated from snapshot run `25254380121` and Factor Walk-Forward V2 run
`25255100665`.

#### Inputs

- Snapshot workflow: `research-snapshot.yml`
- Snapshot run: `25254380121`
- Walk-forward workflow: `factor-walk-forward-v2.yml`
- Walk-forward run: `25255100665`
- Snapshot hash: `762ae7751ad08a21`
- Snapshot window: `2026-04-21T00:00:00Z -> 2026-05-02T00:00:00Z`
- Review inputs: `start_date=2026-04-21`, `end_date=2026-05-01`
- Symbols: `BTCUSDT,ETHUSDT,SOLUSDT`
- Stake: `15`
- Rolling shape: `train_window_days=3`, `test_window_days=1`,
  `step_days=1`
- Minimum observations: `80`
- Factor filter:
  `side_model_prob,side_distance_over_sigma,cex_continuation_edge_gate,exit_bid_change_30s,entry_ask_change_30s,pm_reprice_speed_30s,obi_persistence_30s_side,cex_continuation_score_side`

#### Data Health

- Source observations: `114008`
- V2 rows: `228016`
- Executable PnL rows: `50680`
- Entry fill rate: `22.23%`
- Exit fill rate: `20.97%`
- Full-depth entry fill rate: `49.23%`
- Full-depth exit fill rate: `40.70%`
- PM book rows: `171794`
- Official settlement required: `true`
- Snapshot data audit status: `critical`

#### Raw Rolling Read

The raw alpha-side factors reached the required `8` windows and were positive
in aggregate, but still failed deployment quality because one OOS window was
large and negative:

| Factor | Windows | Positive Window Ratio | Total Test PnL | Min Window PnL | Avg Fill Rate |
| --- | ---: | ---: | ---: | ---: | ---: |
| `side_distance_over_sigma` | `8` | `0.8750` | `51477.7591` | `-37573.7623` | `0.6349` |
| `side_model_prob` | `8` | `0.8750` | `51373.4367` | `-37630.4270` | `0.6349` |

This keeps the raw read as research evidence only. A single severe bad window
is enough to reject direct dry-run handoff.

#### Liquidity-Gated Rolling Read

After the liquidity gate, the alpha lane became much cleaner but still did not
clear promotion:

| Factor | Decision | Windows | Positive Window Ratio | Total Test PnL | Min Window PnL | Fill Rate | Symbol Positive | Time Bucket Positive | Reason |
| --- | --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: | --- |
| `side_model_prob` | `watchlist` | `7` | `0.8571` | `34402.8533` | `-597.4725` | `1.0000` | `0.8571` | `0.8571` | `too_few_windows_positive_pnl` |
| `side_distance_over_sigma` | `watchlist` | `7` | `0.8571` | `34400.6509` | `-597.4725` | `1.0000` | `0.8571` | `0.8571` | `too_few_windows_positive_pnl` |
| `cex_continuation_edge_gate` | `watchlist` | `7` | `0.8571` | `1907.2512` | `-28.8733` | `1.0000` | `0.8095` | `0.7500` | `too_few_windows_positive_pnl` |

OBI persistence remained rejected. PM dynamics and continuation score were much
weaker than the alpha lane. Meta-label rules were negative in this run:
`liquidity_gate_only` total PnL `-10786.2307`, `cex_obi_confirmation`
`-3813.5170`, and `continuation_confirmation` `-155.2925`.

#### Batch Decision

`continue-factor-stability-research`

This is the strongest current evidence for the contrarian/liquidity-gated
alpha lane, but it is still not a deployable edge. It passes fillability after
the liquidity gate, but remains one effective liquidity-gated window short of
the default `8`-window gate and still has one negative OOS window. The required
next check is the second symbol batch
`XRPUSDT,DOGEUSDT,BNBUSDT`, followed by a cross-batch comparison. Do not restore
dry-run/live from the BTC/ETH/SOL batch alone.

### XRP/DOGE/BNB Batch

Generated from snapshot run `25255158983` and Factor Walk-Forward V2 run
`25255536366`.

#### Inputs

- Snapshot workflow: `research-snapshot.yml`
- Snapshot run: `25255158983`
- Walk-forward workflow: `factor-walk-forward-v2.yml`
- Walk-forward run: `25255536366`
- Snapshot hash: `6e858cf3c0a607f0`
- Snapshot window: `2026-04-21T00:00:00Z -> 2026-05-02T00:00:00Z`
- Review inputs: `start_date=2026-04-21`, `end_date=2026-05-01`
- Symbols: `XRPUSDT,DOGEUSDT,BNBUSDT`
- Stake: `15`
- Rolling shape: `train_window_days=3`, `test_window_days=1`,
  `step_days=1`
- Minimum observations: `80`

#### Data Health

- Source observations: `108296`
- V2 rows: `216592`
- Executable PnL rows: `15828`
- Entry fill rate: `7.31%`
- Exit fill rate: `6.50%`
- Full-depth entry fill rate: `45.22%`
- Full-depth exit fill rate: `36.27%`
- PM book rows: `141692`
- Official settlement required: `true`
- Snapshot data audit status: `critical`

#### Raw Rolling Read

The raw alpha lane failed on the second batch:

| Factor | Decision | Windows | Positive Window Ratio | Total Test PnL | Min Window PnL | Avg Fill Rate | Reason |
| --- | --- | ---: | ---: | ---: | ---: | ---: | --- |
| `side_distance_over_sigma` | `reject` | `8` | `0.7500` | `-17774.1481` | `-34578.5811` | `0.5310` | `nonpositive_executable_pnl` |
| `side_model_prob` | `reject` | `8` | `0.7500` | `-17805.3081` | `-34593.8434` | `0.5310` | `nonpositive_executable_pnl` |

This is an important negative result: the raw contrarian signal does not
generalize across the remaining symbols.

#### Liquidity-Gated Rolling Read

The liquidity-gated alpha lane stayed positive but small and still under the
window gate:

| Factor | Decision | Windows | Positive Window Ratio | Total Test PnL | Min Window PnL | Fill Rate | Symbol Positive | Time Bucket Positive | Reason |
| --- | --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: | --- |
| `side_distance_over_sigma` | `watchlist` | `7` | `1.0000` | `1240.0377` | `62.9193` | `1.0000` | `0.9524` | `0.9167` | `too_few_windows_positive_pnl` |
| `side_model_prob` | `watchlist` | `7` | `1.0000` | `1240.0377` | `62.9193` | `1.0000` | `0.9524` | `0.9167` | `too_few_windows_positive_pnl` |

All other tested factors were rejected in the liquidity-gated stability table.
The best non-alpha raw factor, `cex_continuation_edge_gate`, was only
watchlist with `8` windows, `50%` positive windows, total PnL `770.7205`, and
reason `positive_pnl_but_unstable_windows`.

### Cross-Batch Decision

`continue-factor-stability-research`

The scientific conclusion is narrower than "we found an edge":

- The raw contrarian alpha lane is not stable across all six symbols. It is
  strongly positive in BTC/ETH/SOL but negative in XRP/DOGE/BNB.
- The liquidity-gated alpha lane is positive in both symbol batches with
  `100%` fill and zero rejection, but both batches expose only `7` effective
  liquidity-gated OOS windows, below the default `8`-window promotion gate.
- The second batch is much smaller in PnL, so the apparent edge is concentrated
  in BTC/ETH/SOL. It is a promising watchlist lane, not a deployable strategy.

Do not restore dry-run/live from this evidence. The next research step should
either extend the per-batch snapshot by at least one effective liquidity-gated
window or fix snapshot compilation to handle the six-symbol long window without
OOM, then rerun the same side-neutral gates.
