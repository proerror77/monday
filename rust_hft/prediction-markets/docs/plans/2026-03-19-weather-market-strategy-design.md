# Weather Market Strategy Design

Status: implemented for observe-only runtime
Issue: #66
Issue URL: https://github.com/proerror77/ploy/issues/66
Reference: https://x.com/BiteyeCN/status/2034203186970701859

## Objective

Add a standalone canonical strategy that can observe and score Polymarket-style
weather contracts for configurable stations and contract definitions. The goal
is to reproduce the full Biteye-style decision flow, not just a single maximum
temperature forecast.

## Confirmed constraints

- The strategy must be generic for configurable airport/station inputs and
  weather-contract definitions.
- The first delivery is observe-only: emit model state, probabilities, target
  bucket views, and entry suggestions without live order submission.
- Prefer public or low-cost weather data sources over paid commercial feeds.
- The design target includes settlement normalization, multi-source forecast
  fusion, intraday correction, warming/cooling classification, and peak-time
  anomaly handling.

## Planned strategy flow

1. Settlement normalization

Map each contract to the exact reporting station, observation source, units,
rounding, and bucket definitions used at settlement. This isolates market
contracts from user-facing weather-app values and avoids city-vs-station
confusion.

2. Multi-source baseline forecast

Collect forecast snapshots from multiple public or low-cost forecast sources
and merge them into a base maximum-temperature estimate. The merge should be
weather-type aware rather than a fixed average so the runtime can shift weight
between sources when cloud cover, wind, or other conditions imply a different
error profile.

3. Intraday real-time correction

As station observations arrive, update the base forecast with an intraday
correction layer. This stage should estimate the remaining heating potential
for the day and increase the influence of realized observations as local time
approaches the typical daily peak window.

4. Warming/cooling regime classification

Compute a daily regime signal that answers whether today is likely to finish
warmer, flatter, or cooler than the prior day and expose both class and
confidence. This becomes a directional prior that can reinforce or suppress the
baseline forecast and the intraday correction output.

5. Peak-time anomaly detection

Identify cases where the expected daily peak timing deviates from the normal
diurnal pattern, such as late-day or night-time peaks during rain or warm-air
advection. This step exists to catch market situations where the crowd is still
pricing a normal sunny-day temperature curve.

6. Market-vs-model recommendation output

Convert the fused model view into contract-bucket probabilities and compare
them with current market pricing. The observe-only runtime should emit entry
suggestions only when model divergence, confidence, and risk filters all align.

## Repo integration target

- Add a new canonical strategy module under `src/strategy/weather_market/`.
- Register the strategy in `src/strategy/mod.rs` and
  `src/strategy/manager.rs` / `src/strategy/manager/factory.rs`.
- Add a default config template under `config/strategies/`.
- Expose dry-run-friendly state metrics through `StrategyStateInfo.metrics`.
- Use `StrategyAction::Alert` and `StrategyAction::LogEvent` for recommendation
  output until live execution is explicitly enabled.

## Runtime contract in this PR

The first implementation lands an observe-only canonical strategy with the
following runtime surface:

- `phase = observing`
- `StrategyStateInfo.metrics` includes station, contract date, corrected max
  temperature, sigma, regime, peak anomaly, recommendation count, and best edge
- `StrategyEventType::Custom("weather_market_snapshot")` publishes the full
  model snapshot summary each evaluation cycle
- `StrategyEventType::SignalDetected` publishes recommendation events when a
  bucket clears confidence and edge thresholds
- `StrategyAction::Alert` emits a concise observe-only suggestion message with
  cooldown protection

No live order intent is emitted in this phase.

## Config surface in this PR

The canonical TOML is centered around three groups:

1. Station + settlement normalization
   - `station_id`, `station_name`, `contract_date`
   - `latitude`, `longitude`, `station_utc_offset_hours`
   - `settlement_unit`, `settlement_rounding`

2. Public data-source selection
   - `use_open_meteo`, `open_meteo_weight`
   - `use_nws_hourly`, `use_nws_observations`
   - `nws_station_id`, `nws_grid_office`, `nws_grid_x`, `nws_grid_y`

3. Market mapping + recommendation policy
   - `[[weather_market.buckets]]` with `label`, `token_id`, `min_temp`,
     `max_temp`, optional `market_slug`
   - `recommendation_min_edge`, `recommendation_min_confidence`
   - `tick_interval_ms`, `evaluation_cooldown_secs`, `alert_cooldown_secs`

This keeps contract-specific bucket mapping in config while keeping the model
logic reusable.

## Implemented public-source blend

This implementation intentionally prefers cheap/public sources:

- Open-Meteo forecast API for daily max and hourly temperature curve
- NOAA / weather.gov hourly forecast for US gridpoint peak tracking
- NOAA / weather.gov station observations for intraday actuals and observed max

The fusion layer is not a fixed average. Source weights are scaled by source
confidence so cloudy / precip-heavy days can suppress overconfident baselines.

## Intraday correction model in this PR

The first runtime slice uses a deterministic correction layer:

- base forecast starts from fused source max
- observed station max acts as a hard lower bound
- current temperature and local hour estimate remaining heating potential
- after the configured peak window, the model decays the forward component and
  relies more heavily on observed max

This is intentionally simple and auditable. It is a runtime inference shell,
not a trained nowcasting model.

## Phase split

This issue is explicitly split into two phases:

1. Phase A, in this PR
   - canonical observe-only runtime
   - public-source fetchers
   - settlement normalization
   - fused base forecast
   - intraday correction
   - regime + peak anomaly labels
   - bucket probability + recommendation output

2. Phase B, follow-up PR
   - historical replay / backtest data pipeline
   - archived-forecast ingestion and scoring
   - learned source weighting / bias correction
   - optional live-order enablement once runtime quality is validated

## Validation target

- Offline evaluation for forecast accuracy, bucket accuracy, and regime-label
  quality using historical station data plus archived forecast snapshots.
- Focused dry-run verification in the canonical strategy runtime to ensure the
  strategy emits stable state, signals, and recommendation output.
- Explicit documentation of any phase split if offline training and runtime
  inference do not land in the same implementation slice.

## Validation plan for the shipped slice

1. Unit tests
   - config parsing
   - bucket probability normalization
   - regime classification
   - peak anomaly classification
   - recommendation thresholding

2. Dry-run runtime verification
   - run the strategy with a real station / token mapping in foreground mode
   - confirm snapshot events and recommendation alerts appear without
     `SubmitIntent`
   - inspect `StrategyStateInfo.metrics` for corrected max, sigma, regime, and
     best edge fields

3. Offline follow-up
   - replay historical station observations against archived forecast snapshots
   - score MAE for max-temp estimate
   - score bucket Brier / log loss
   - score regime-label precision / recall on warming vs cooling days

## Open decisions

- Which public forecast sources should be first-class in the generic adapter
  layer for cross-region support.
- Whether the first trained correction model should be a simple bias table, a
  quantile model, or a station-specific residual regressor.
